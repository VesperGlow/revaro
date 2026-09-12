//! EPUB container, OPF, navigation, cover and spine parsing.
//!
//! The pipeline mirrors Go's `parseEPUB`:
//!
//! 1. read `META-INF/container.xml` to find the OPF package document,
//! 2. parse the OPF for the title, manifest, spine and cover metadata,
//! 3. extract the EPUB3 navigation document (or fall back to NCX),
//! 4. extract the cover image,
//! 5. sanitize every readable spine item, in spine order.
//!
//! Steps 3 and 4 are where the Go original was non-deterministic: it iterated a
//! Go map while picking the navigation document and the cover, so two runs could
//! choose differently when a book declared several candidates. This port keeps
//! the manifest in declaration order (see [`build_manifest`]) and always picks
//! the first candidate in that order, which restores the documented
//! "same book, same artifacts" invariant.

use std::collections::HashMap;
use std::io::{Read, Seek};
use std::sync::LazyLock;

use quick_xml::Reader;
use quick_xml::escape::unescape;
use quick_xml::events::{BytesCData, BytesEnd, BytesStart, BytesText, Event};
use regex::Regex;
use zip::ZipArchive;

use markup5ever_rcdom::{Handle, NodeData};

use crate::archive::{zip_bytes, zip_text};
use crate::budget::{Budget, RenderBudget};
use crate::dom::{attr, children, parse_html, tag_name, text_content};
use crate::model::{Asset, Book, Chapter, Format};
use crate::path::{ext_of, fragment_of, normalize_path, resolve_path};
use crate::sanitize::ChapterRenderer;
use crate::{MAX_ARCHIVE_ENTRIES, MAX_RENDERED_HTML, ReaderError, TocEntry};

/// One `<meta>` element from the OPF metadata section.
#[derive(Debug, Default)]
struct OpfMeta {
    /// `name` attribute.
    name: String,
    /// `content` attribute.
    content: String,
}

/// A manifest `<item>` as declared in the OPF.
#[derive(Debug, Default)]
struct RawManifestItem {
    /// `id` attribute, used to resolve spine `idref`s and the `cover` meta.
    id: String,
    /// `href` attribute, resolved relative to the OPF path.
    href: String,
    /// `media-type` attribute.
    media_type: String,
    /// `properties` attribute (space-separated tokens).
    properties: String,
}

/// The subset of the OPF package document the reader needs.
#[derive(Debug, Default)]
struct OpfPackage {
    /// `<dc:title>` text, untrimmed.
    title: String,
    /// `<meta>` elements in document order.
    metas: Vec<OpfMeta>,
    /// Manifest items in declaration order.
    items: Vec<RawManifestItem>,
    /// `spine@toc`, the NCX manifest id.
    spine_toc: String,
    /// Spine `idref`s in reading order.
    spine: Vec<String>,
}

/// A manifest item after path resolution and duplicate-id folding.
#[derive(Debug)]
struct ManifestItem {
    /// Canonical archive path.
    path: String,
    /// Declared media type.
    media_type: String,
    /// Space-separated properties.
    properties: String,
}

/// Parse an EPUB stream into a [`Book`].
pub(crate) fn parse<R: Read + Seek>(
    source: R,
    asset_base_url: &str,
    etag: &str,
) -> Result<Book, ReaderError> {
    let mut archive =
        zip::ZipArchive::new(source).map_err(|error| ReaderError::InvalidZip(error.to_string()))?;
    if archive.len() > MAX_ARCHIVE_ENTRIES {
        return Err(ReaderError::TooManyEntries(MAX_ARCHIVE_ENTRIES));
    }
    let mut budget = Budget::default();
    let container = zip_text(&mut archive, "META-INF/container.xml", &mut budget)
        .map_err(|error| ReaderError::Container(error.to_string()))?;
    let opf_path = opf_path_from_container(&container)?;
    let opf_xml = zip_text(&mut archive, &opf_path, &mut budget)
        .map_err(|error| ReaderError::OpfRead(error.to_string()))?;
    let mut package = parse_opf(&opf_xml)?;
    if package.items.len() > MAX_ARCHIVE_ENTRIES || package.spine.len() > MAX_ARCHIVE_ENTRIES {
        return Err(ReaderError::TooManyManifestItems);
    }
    // The raw items are consumed by manifest building; the metadata and spine
    // sections stay on `package` for TOC and cover selection.
    let raw_items = std::mem::take(&mut package.items);
    let (items, index) = build_manifest(raw_items, &opf_path);

    let mut book = Book {
        format: Format::Epub,
        title: package.title.trim().to_string(),
        ..Book::default()
    };
    if book.title.is_empty() {
        book.title = "未命名书籍".to_string();
    }

    // The decompression budget is shared, so TOC and cover are charged before
    // the spine exactly as in Go; a book that just fits could otherwise shift
    // the entry that trips the cap.
    book.toc = extract_toc(&mut archive, &package, &items, &index, &mut budget);
    (book.cover, book.cover_ext) =
        extract_cover(&mut archive, &package, &items, &index, &mut budget);

    let asset_base = asset_base_url.trim_end_matches('/');
    let mut render = RenderBudget::new(MAX_RENDERED_HTML);
    let mut assets: Vec<Asset> = Vec::new();
    let mut asset_index: HashMap<String, usize> = HashMap::new();
    let mut chapters: Vec<Chapter> = Vec::new();
    for idref in &package.spine {
        let Some(&position) = index.get(idref) else {
            continue;
        };
        let item = &items[position];
        if !is_html_media(&item.media_type) {
            continue;
        }
        // A spine item that cannot be read is skipped: one broken chapter must
        // not lose the whole book.
        let Ok(chapter) = zip_text(&mut archive, &item.path, &mut budget) else {
            continue;
        };
        let html = {
            let mut renderer = ChapterRenderer {
                archive: &mut archive,
                budget: &mut budget,
                render: &mut render,
                assets: &mut assets,
                asset_index: &mut asset_index,
                asset_base,
                asset_version: etag,
                chapter: item.path.clone(),
                html: String::new(),
                pending: Vec::new(),
            };
            renderer.render(chapter.as_bytes());
            std::mem::take(&mut renderer.html)
        };
        if render.overflow {
            return Err(ReaderError::RenderedTooLarge(
                (MAX_RENDERED_HTML >> 20) as i64,
            ));
        }
        chapters.push(Chapter {
            html,
            source_path: item.path.clone(),
        });
    }
    if chapters.is_empty() {
        return Err(ReaderError::NoReadableContent);
    }
    book.chapters = chapters;
    book.assets = assets;
    Ok(book)
}

/// Fold raw manifest items into a path-resolved list plus an id index.
///
/// The list keeps **first-declaration order** (a repeated id overwrites the
/// earlier entry in place), and every later manifest scan walks this list rather
/// than a hash map. That is what makes navigation and cover selection
/// deterministic; the id index gives Go's "last declaration wins" lookup
/// semantics for spine references.
fn build_manifest(
    raw: Vec<RawManifestItem>,
    opf_path: &str,
) -> (Vec<ManifestItem>, HashMap<String, usize>) {
    let mut items: Vec<ManifestItem> = Vec::new();
    let mut index: HashMap<String, usize> = HashMap::new();
    for raw_item in raw {
        let item = ManifestItem {
            path: resolve_path(opf_path, &raw_item.href),
            media_type: raw_item.media_type,
            properties: raw_item.properties,
        };
        match index.get(&raw_item.id) {
            Some(&position) => items[position] = item,
            None => {
                index.insert(raw_item.id, items.len());
                items.push(item);
            }
        }
    }
    (items, index)
}

/// Read the OPF path out of `META-INF/container.xml`.
fn opf_path_from_container(xml: &str) -> Result<String, ReaderError> {
    let mut reader = Reader::from_str(xml);
    let mut stack: Vec<String> = Vec::new();
    loop {
        match reader.read_event() {
            Ok(Event::Start(event)) => {
                let local = start_local_name(&event);
                if local == "rootfile"
                    && stack.last().is_some_and(|parent| parent == "rootfiles")
                    && let Some(full_path) =
                        attribute(&event, "full-path").filter(|p| !p.is_empty())
                {
                    return Ok(normalize_path(&full_path));
                }
                stack.push(local);
            }
            Ok(Event::Empty(event)) => {
                let local = start_local_name(&event);
                if local == "rootfile"
                    && stack.last().is_some_and(|parent| parent == "rootfiles")
                    && let Some(full_path) =
                        attribute(&event, "full-path").filter(|p| !p.is_empty())
                {
                    return Ok(normalize_path(&full_path));
                }
            }
            Ok(Event::End(_)) => {
                stack.pop();
            }
            Ok(Event::Eof) => break,
            Err(_) => return Err(ReaderError::MissingRootfile),
            _ => {}
        }
    }
    Err(ReaderError::MissingRootfile)
}

/// Parse the OPF package document.
///
/// Only the fields the reader uses are extracted; unknown elements and
/// attributes are ignored, which is more forgiving than a strict schema check
/// but matches how Go's `encoding/xml` struct decoding behaved.
fn parse_opf(xml: &str) -> Result<OpfPackage, ReaderError> {
    let mut reader = Reader::from_str(xml);
    let mut stack: Vec<String> = Vec::new();
    let mut package = OpfPackage::default();
    let mut capturing_title = false;
    let mut open_meta: Option<OpfMeta> = None;
    loop {
        match reader.read_event() {
            Ok(Event::Start(event)) => {
                let local = start_local_name(&event);
                match local.as_str() {
                    "title" if in_section(&stack, "metadata") => {
                        // A repeated <title> overwrites, matching encoding/xml.
                        package.title.clear();
                        capturing_title = true;
                    }
                    "meta" if in_section(&stack, "metadata") => {
                        open_meta = Some(OpfMeta {
                            name: attribute(&event, "name").unwrap_or_default(),
                            content: attribute(&event, "content").unwrap_or_default(),
                        });
                    }
                    "item" if in_section(&stack, "manifest") => {
                        package.items.push(raw_manifest_item(&event));
                    }
                    "spine" => {
                        package.spine_toc = attribute(&event, "toc").unwrap_or_default();
                    }
                    "itemref" if in_section(&stack, "spine") => {
                        package
                            .spine
                            .push(attribute(&event, "idref").unwrap_or_default());
                    }
                    _ => {}
                }
                stack.push(local);
            }
            Ok(Event::Empty(event)) => {
                let local = start_local_name(&event);
                match local.as_str() {
                    "title" if in_section(&stack, "metadata") => package.title.clear(),
                    "meta" if in_section(&stack, "metadata") => {
                        package.metas.push(OpfMeta {
                            name: attribute(&event, "name").unwrap_or_default(),
                            content: attribute(&event, "content").unwrap_or_default(),
                        });
                    }
                    "item" if in_section(&stack, "manifest") => {
                        package.items.push(raw_manifest_item(&event));
                    }
                    "spine" => {
                        package.spine_toc = attribute(&event, "toc").unwrap_or_default();
                    }
                    "itemref" if in_section(&stack, "spine") => {
                        package
                            .spine
                            .push(attribute(&event, "idref").unwrap_or_default());
                    }
                    _ => {}
                }
            }
            Ok(Event::Text(text)) => {
                if capturing_title && let Some(decoded) = decode_text(&text) {
                    package.title.push_str(&decoded);
                }
            }
            Ok(Event::CData(cdata)) => {
                if capturing_title && let Some(decoded) = decode_cdata(&cdata) {
                    package.title.push_str(&decoded);
                }
            }
            Ok(Event::End(end)) => {
                let local = end_local_name(&end);
                if local == "title" {
                    capturing_title = false;
                }
                if local == "meta"
                    && let Some(meta) = open_meta.take()
                {
                    package.metas.push(meta);
                }
                stack.pop();
            }
            Ok(Event::Eof) => {
                // quick-xml reports a truncated document as a clean end of
                // stream; Go's `encoding/xml` returned "unexpected EOF" instead.
                if stack.is_empty() {
                    break;
                }
                return Err(ReaderError::OpfParse(
                    "unexpected end of the OPF document".to_string(),
                ));
            }
            Err(error) => return Err(ReaderError::OpfParse(error.to_string())),
            _ => {}
        }
    }
    Ok(package)
}

/// Build a raw manifest item from an `<item>` start or empty tag.
fn raw_manifest_item(event: &BytesStart) -> RawManifestItem {
    RawManifestItem {
        id: attribute(event, "id").unwrap_or_default(),
        href: attribute(event, "href").unwrap_or_default(),
        media_type: attribute(event, "media-type").unwrap_or_default(),
        properties: attribute(event, "properties").unwrap_or_default(),
    }
}

/// True when an open ancestor with local name `section` exists.
fn in_section(stack: &[String], section: &str) -> bool {
    stack.iter().any(|name| name == section)
}

/// Local name of a start tag, without the namespace prefix.
fn start_local_name(event: &BytesStart) -> String {
    String::from_utf8_lossy(event.local_name().as_ref()).into_owned()
}

/// Local name of an end tag, without the namespace prefix.
fn end_local_name(event: &BytesEnd) -> String {
    String::from_utf8_lossy(event.local_name().as_ref()).into_owned()
}

/// First attribute whose local name matches `key`, with entities decoded.
fn attribute(event: &BytesStart, key: &str) -> Option<String> {
    for attribute in event.attributes().flatten() {
        if attribute.key.local_name().as_ref() == key.as_bytes() {
            return attribute
                .unescape_value()
                .ok()
                .map(|value| value.into_owned());
        }
    }
    None
}

/// Decode and unescape an XML text event.
fn decode_text(text: &BytesText) -> Option<String> {
    let decoded = text.decode().ok()?;
    unescape(&decoded).ok().map(|value| value.into_owned())
}

/// Decode an XML CDATA event (no entity unescaping applies).
fn decode_cdata(cdata: &BytesCData) -> Option<String> {
    cdata.decode().ok().map(|value| value.into_owned())
}

/// True when a media type should be treated as an XHTML chapter.
fn is_html_media(media_type: &str) -> bool {
    let lower = media_type.to_lowercase();
    lower.contains("xhtml") || lower.contains("html") || lower.contains("xml")
}

/// True when a space-separated token list contains `property`.
///
/// The surrounding spaces stop `cover-image-extra` from matching `cover-image`.
fn properties_has(properties: &str, property: &str) -> bool {
    format!(" {properties} ").contains(&format!(" {property} "))
}

/// EPUB3 navigation document first, NCX fallback.
fn extract_toc<R: Read + Seek>(
    archive: &mut ZipArchive<R>,
    package: &OpfPackage,
    items: &[ManifestItem],
    index: &HashMap<String, usize>,
    budget: &mut Budget,
) -> Vec<TocEntry> {
    // Deterministic: the first nav item in manifest order that yields entries.
    for item in items {
        if !properties_has(&item.properties, "nav") {
            continue;
        }
        if let Ok(nav_html) = zip_text(archive, &item.path, budget) {
            let entries = parse_nav(nav_html.as_bytes(), &item.path);
            if !entries.is_empty() {
                return entries;
            }
        }
    }
    // Fall back to the NCX named by `spine@toc`, then to any NCX item — again in
    // manifest order rather than map order.
    let ncx = index
        .get(&package.spine_toc)
        .map(|&position| &items[position])
        .or_else(|| {
            items
                .iter()
                .find(|item| item.media_type.to_lowercase().contains("ncx"))
        });
    let Some(ncx) = ncx else {
        return Vec::new();
    };
    match zip_text(archive, &ncx.path, budget) {
        Ok(xml) => parse_ncx(xml.as_bytes(), &ncx.path),
        Err(_) => Vec::new(),
    }
}

/// Parse an EPUB3 nav document into TOC entries.
fn parse_nav(data: &[u8], nav_path: &str) -> Vec<TocEntry> {
    let Some(dom) = parse_html(data) else {
        return Vec::new();
    };
    let Some(nav) = find_toc_nav(&dom.document) else {
        return Vec::new();
    };
    let mut entries = Vec::new();
    collect_anchors(&nav, 0, nav_path, &mut entries);
    entries
}

/// Find the `<nav epub:type="toc">`, else the first `<nav>` in document order.
///
/// Go matched on the *value* of any attribute, not the name, and this port keeps
/// that: real-world nav documents spell the marker in several ways.
fn find_toc_nav(root: &Handle) -> Option<Handle> {
    fn walk(node: &Handle, first: &mut Option<Handle>, toc: &mut Option<Handle>) {
        if toc.is_some() {
            return;
        }
        if let Some(tag) = tag_name(node)
            && tag == "nav"
        {
            if first.is_none() {
                *first = Some(node.clone());
            }
            if has_attribute_value(node, "toc") {
                *toc = Some(node.clone());
                return;
            }
        }
        for child in children(node) {
            walk(&child, first, toc);
        }
    }
    let mut first = None;
    let mut toc = None;
    walk(root, &mut first, &mut toc);
    toc.or(first)
}

/// True when any attribute of `node` has exactly the value `value`.
fn has_attribute_value(node: &Handle, value: &str) -> bool {
    let NodeData::Element { attrs, .. } = &node.data else {
        return false;
    };
    attrs
        .borrow()
        .iter()
        .any(|attribute| &*attribute.value == value)
}

/// Collect `<a href>` entries, tracking list nesting for depth.
///
/// Anchors are leaves: the walk does not descend into them, so a label may
/// contain inline markup without producing nested entries.
fn collect_anchors(node: &Handle, list_depth: i32, nav_path: &str, out: &mut Vec<TocEntry>) {
    let mut depth = list_depth;
    if let Some(tag) = tag_name(node)
        && (tag == "ol" || tag == "ul")
    {
        depth += 1;
    }
    for child in children(node) {
        if let Some(tag) = tag_name(&child)
            && tag == "a"
        {
            let href = attr(&child, "href").unwrap_or_default();
            let label = text_content(&child).trim().to_string();
            if !label.is_empty() {
                out.push(TocEntry {
                    label,
                    path: resolve_path(nav_path, &href),
                    fragment: fragment_of(&href),
                    offset: 0,
                    depth: (depth - 1).max(0),
                });
            }
            continue;
        }
        collect_anchors(&child, depth, nav_path, out);
    }
}

/// Parse an NCX document into TOC entries.
///
/// The streaming parser emits each `navPoint` entry when its start tag is seen,
/// which reproduces Go's pre-order `walk` (parent before children) without
/// materialising a recursive struct.
fn parse_ncx(data: &[u8], ncx_path: &str) -> Vec<TocEntry> {
    let text = String::from_utf8_lossy(data);
    let mut reader = Reader::from_str(&text);
    let mut stack: Vec<String> = Vec::new();
    let mut frames: Vec<usize> = Vec::new();
    let mut entries: Vec<TocEntry> = Vec::new();
    let mut capturing_label: Option<usize> = None;
    loop {
        match reader.read_event() {
            Ok(Event::Start(event)) => {
                let local = start_local_name(&event);
                match local.as_str() {
                    "navPoint" if in_section(&stack, "navMap") => {
                        entries.push(TocEntry {
                            depth: frames.len() as i32,
                            ..TocEntry::default()
                        });
                        frames.push(entries.len() - 1);
                    }
                    "text" if in_section(&stack, "navLabel") => {
                        capturing_label = frames.last().copied();
                    }
                    "content" => {
                        if let Some(&index) = frames.last() {
                            set_ncx_target(&mut entries[index], &event, ncx_path);
                        }
                    }
                    _ => {}
                }
                stack.push(local);
            }
            Ok(Event::Empty(event)) => {
                if start_local_name(&event) == "content"
                    && let Some(&index) = frames.last()
                {
                    set_ncx_target(&mut entries[index], &event, ncx_path);
                }
            }
            Ok(Event::Text(event)) => {
                if let Some(index) = capturing_label
                    && let Some(decoded) = decode_text(&event)
                {
                    entries[index].label.push_str(&decoded);
                }
            }
            Ok(Event::CData(event)) => {
                if let Some(index) = capturing_label
                    && let Some(decoded) = decode_cdata(&event)
                {
                    entries[index].label.push_str(&decoded);
                }
            }
            Ok(Event::End(event)) => {
                let local = end_local_name(&event);
                if local == "text" {
                    capturing_label = None;
                }
                if local == "navPoint" {
                    frames.pop();
                }
                stack.pop();
            }
            Ok(Event::Eof) => break,
            Err(_) => return Vec::new(),
            _ => {}
        }
    }
    for entry in &mut entries {
        entry.label = entry.label.trim().to_string();
        if entry.label.is_empty() {
            entry.label = "未命名章节".to_string();
        }
    }
    entries
}

/// Write a `src` attribute onto an NCX entry.
fn set_ncx_target(entry: &mut TocEntry, event: &BytesStart, ncx_path: &str) {
    let Some(src) = attribute(event, "src") else {
        return;
    };
    entry.path = resolve_path(ncx_path, &src);
    entry.fragment = fragment_of(&src);
}

/// Cover-image filename heuristic, matching Go's `(?i)(^|[/_.\-])cover([/_.\-]|$)`.
static COVER_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)(^|[/_.\-])cover([/_.\-]|$)")
        .expect("the cover regex is a constant and must compile")
});

/// Extract the cover image and its extension.
fn extract_cover<R: Read + Seek>(
    archive: &mut ZipArchive<R>,
    package: &OpfPackage,
    items: &[ManifestItem],
    index: &HashMap<String, usize>,
    budget: &mut Budget,
) -> (Vec<u8>, String) {
    let Some(cover) = cover_item(package, items, index) else {
        return (Vec::new(), String::new());
    };
    match zip_bytes(archive, &cover.path, budget) {
        Ok(data) => {
            let ext = ext_of(&cover.path);
            (data, ext)
        }
        Err(_) => (Vec::new(), String::new()),
    }
}

/// Choose the cover manifest item.
///
/// Three strategies, in the order the EPUB spec and real-world files need them:
/// an explicit `<meta name="cover">`, then a `cover-image` property, then a
/// filename heuristic. Every scan walks `items` in declaration order, so the
/// result no longer depends on hash-map iteration order.
fn cover_item<'a>(
    package: &OpfPackage,
    items: &'a [ManifestItem],
    index: &HashMap<String, usize>,
) -> Option<&'a ManifestItem> {
    for meta in &package.metas {
        if meta.name == "cover"
            && let Some(&position) = index.get(&meta.content)
        {
            return Some(&items[position]);
        }
    }
    for item in items {
        if properties_has(&item.properties, "cover-image") {
            return Some(item);
        }
    }
    items.iter().find(|&item| {
        item.media_type.to_lowercase().starts_with("image/") && COVER_RE.is_match(&item.path)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn properties_tokens_are_matched_whole() {
        assert!(properties_has("nav", "nav"));
        assert!(properties_has("cover-image svg", "cover-image"));
        assert!(!properties_has("cover-image-extra", "cover-image"));
        assert!(!properties_has("", "nav"));
    }

    #[test]
    fn html_media_types_are_recognised() {
        assert!(is_html_media("application/xhtml+xml"));
        assert!(is_html_media("text/html"));
        assert!(is_html_media("application/xml"));
        assert!(!is_html_media("image/png"));
        // Go's "contains xml" test also accepts NCX, and this port keeps that
        // behaviour rather than silently changing which spine items render.
        assert!(is_html_media("application/x-dtbncx+xml"));
    }

    #[test]
    fn container_rootfile_is_required() {
        let good = r#"<?xml version="1.0"?><container><rootfiles>
            <rootfile full-path="OEBPS/content.opf" media-type="application/oebps-package+xml"/>
        </rootfiles></container>"#;
        assert_eq!(opf_path_from_container(good).unwrap(), "OEBPS/content.opf");
        assert!(matches!(
            opf_path_from_container("<container/>"),
            Err(ReaderError::MissingRootfile)
        ));
        assert!(matches!(
            opf_path_from_container("not xml"),
            Err(ReaderError::MissingRootfile)
        ));
    }

    #[test]
    fn opf_fields_are_parsed_in_declaration_order() {
        let xml = r#"<?xml version="1.0"?>
        <package xmlns="http://www.idpf.org/2007/opf">
          <metadata xmlns:dc="http://purl.org/dc/elements/1.1/">
            <dc:title> 书  名 </dc:title>
            <meta name="cover" content="c"/>
          </metadata>
          <manifest>
            <item id="nav" href="nav.xhtml" media-type="application/xhtml+xml" properties="nav"/>
            <item id="a" href="a.xhtml" media-type="application/xhtml+xml"/>
            <item id="b" href="b.xhtml" media-type="application/xhtml+xml"/>
          </manifest>
          <spine toc="ncx"><itemref idref="a"/><itemref idref="b"/></spine>
        </package>"#;
        let package = parse_opf(xml).unwrap();
        assert_eq!(package.title, " 书  名 ");
        assert_eq!(package.spine_toc, "ncx");
        assert_eq!(package.spine, vec!["a", "b"]);
        assert_eq!(package.metas.len(), 1);
        assert_eq!(package.metas[0].name, "cover");
        assert_eq!(package.metas[0].content, "c");
        let ids: Vec<&str> = package.items.iter().map(|item| item.id.as_str()).collect();
        assert_eq!(ids, vec!["nav", "a", "b"]);
    }

    #[test]
    fn malformed_opf_is_rejected() {
        assert!(matches!(
            parse_opf("<package><manifest>"),
            Err(ReaderError::OpfParse(_))
        ));
    }

    #[test]
    fn manifest_keeps_first_declaration_order_and_last_value() {
        let raw = vec![
            RawManifestItem {
                id: "a".into(),
                href: "first.xhtml".into(),
                media_type: "application/xhtml+xml".into(),
                properties: String::new(),
            },
            RawManifestItem {
                id: "b".into(),
                href: "b.xhtml".into(),
                media_type: "application/xhtml+xml".into(),
                properties: String::new(),
            },
            RawManifestItem {
                id: "a".into(),
                href: "second.xhtml".into(),
                media_type: "application/xhtml+xml".into(),
                properties: "nav".into(),
            },
        ];
        let (items, index) = build_manifest(raw, "OEBPS/content.opf");
        assert_eq!(items.len(), 2, "a repeated id must not add an entry");
        // Declaration order is preserved...
        assert_eq!(items[0].path, "OEBPS/second.xhtml");
        assert_eq!(items[1].path, "OEBPS/b.xhtml");
        // ...while lookups see the last declaration.
        assert_eq!(items[index["a"]].path, "OEBPS/second.xhtml");
    }
}

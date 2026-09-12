//! The HTML whitelist sanitiser.
//!
//! Chapter XHTML from an EPUB is parsed with a real HTML5 tree builder and then
//! re-emitted through a whitelist walk. Nothing from the source is copied
//! verbatim except text nodes (escaped) and attribute values on a small,
//! understood allowlist, so the result cannot execute script even if the source
//! was crafted to look like an allowed construct.
//!
//! Two porting fixes live here:
//!
//! * `data-source-path` and `data-frag-ids` are HTML-escaped. Go wrote them with
//!   `fmt.Fprintf("%q")`, which does not escape `<` or `"` in a way that is safe
//!   inside an attribute.
//! * Embedded images are deduplicated by archive path and published under
//!   `<asset_base>/<index>?v=<etag>`, with the intrinsic dimensions sniffed from
//!   the bytes so the client can reserve layout space.

use std::collections::HashMap;
use std::io::{Read, Seek};

use markup5ever_rcdom::{Handle, NodeData};
use zip::ZipArchive;

use crate::archive::zip_bytes;
use crate::budget::{Budget, RenderBudget};
use crate::dom::{
    attr, children, collect_elements, find_element, parse_html, tag_name, text_content,
};
use crate::image::image_dims;
use crate::model::Asset;
use crate::path::{asset_content_type, ext_of, query_escape, resolve_path};

/// Elements that must never reach the reader, whatever their attributes.
const DISALLOWED_TAGS: &[&str] = &[
    "script", "style", "iframe", "object", "embed", "form", "input", "button", "meta", "base",
    "link", "head", "title", "noscript",
];

/// Elements treated as standalone content blocks by the whitelist walk.
///
/// `img` and `svg` count as blocks so that an image-only paragraph survives
/// even though it has no text.
const BLOCK_TAGS: &[&str] = &[
    "p",
    "h1",
    "h2",
    "h3",
    "h4",
    "h5",
    "h6",
    "li",
    "blockquote",
    "pre",
    "figure",
    "img",
    "svg",
    "table",
    "hr",
];

/// HTML void elements, which are emitted as `<tag ...>` with no end tag.
const VOID_TAGS: &[&str] = &[
    "area", "base", "br", "col", "embed", "hr", "img", "input", "link", "meta", "source", "track",
    "wbr",
];

/// Per-chapter sanitizer state.
///
/// The archive, decompression budget, render budget and asset table are all
/// borrowed from the caller so they persist across spine items: assets are
/// deduplicated book-wide and the rendered-HTML cap is a whole-book cap.
pub(crate) struct ChapterRenderer<'a, R: Read + Seek> {
    /// Archive the images are read from.
    pub(crate) archive: &'a mut ZipArchive<R>,
    /// Archive-wide decompression budget.
    pub(crate) budget: &'a mut Budget,
    /// Whole-book rendered-HTML budget.
    pub(crate) render: &'a mut RenderBudget,
    /// Book-wide asset table.
    pub(crate) assets: &'a mut Vec<Asset>,
    /// Book-wide archive-path to asset-index map.
    pub(crate) asset_index: &'a mut HashMap<String, usize>,
    /// URL prefix rewritten images are published under.
    pub(crate) asset_base: &'a str,
    /// Version query value appended to asset URLs (usually the object ETag).
    pub(crate) asset_version: &'a str,
    /// Canonical path of the chapter being rendered.
    pub(crate) chapter: String,
    /// Sanitized output accumulated for this chapter.
    pub(crate) html: String,
    /// Element ids from dropped empty blocks, carried onto the next block.
    pub(crate) pending: Vec<String>,
}

impl<R: Read + Seek> ChapterRenderer<'_, R> {
    /// Sanitize one spine item's XHTML into [`Self::html`].
    ///
    /// Parse failures yield an empty chapter rather than an error, matching the
    /// Go original: one broken spine item must not lose the whole book.
    pub(crate) fn render(&mut self, source: &[u8]) {
        let Some(dom) = parse_html(source) else {
            return;
        };
        let Some(body) = find_element(&dom.document, "body") else {
            return;
        };
        self.walk(&body);
    }

    /// Recursively emit the children of `parent`.
    fn walk(&mut self, parent: &Handle) {
        for child in children(parent) {
            match &child.data {
                NodeData::Text { contents } => {
                    let text = contents.borrow();
                    if text.trim().is_empty() {
                        continue;
                    }
                    self.push("<p");
                    self.write_block_extra();
                    self.push(">");
                    let escaped = escape_html(&text);
                    self.push(&escaped);
                    self.push("</p>");
                }
                NodeData::Element { name, .. } => {
                    let tag: &str = &name.local;
                    if is_disallowed_tag(tag) {
                        continue;
                    }
                    let child_id = attr(&child, "id").unwrap_or_default();
                    if is_block_tag(tag) {
                        let keep = matches!(tag, "hr" | "img" | "svg")
                            || !text_content(&child).trim().is_empty()
                            || has_media(&child, true);
                        if keep {
                            self.emit_element(&child, true);
                        } else if !child_id.is_empty() {
                            self.pending.push(child_id);
                        }
                    } else if has_block_descendant(&child) {
                        if !child_id.is_empty() {
                            self.pending.push(child_id);
                        }
                        self.walk(&child);
                    } else if !text_content(&child).trim().is_empty() || has_media(&child, false) {
                        self.emit_element(&child, true);
                    } else if !child_id.is_empty() {
                        self.pending.push(child_id);
                    }
                }
                _ => {}
            }
        }
    }

    /// Emit one element and, when `with_extra` is set, its block bookkeeping.
    fn emit_element(&mut self, node: &Handle, with_extra: bool) {
        let NodeData::Element { name, attrs, .. } = &node.data else {
            return;
        };
        let tag: &str = &name.local;
        if is_disallowed_tag(tag) {
            return;
        }
        if tag == "svg" && self.emit_svg_as_image(node, with_extra) {
            return;
        }
        if tag == "img" {
            self.emit_img(node, with_extra);
            return;
        }
        if tag == "image" {
            if let Some(href) = attr(node, "href").filter(|href| !href.is_empty()) {
                let alt = attr(node, "alt").unwrap_or_default();
                self.write_img(&href, &alt, with_extra);
            }
            return;
        }
        self.push("<");
        self.push(tag);
        for attribute in attrs.borrow().iter() {
            let key: &str = &attribute.name.local;
            // Styling and behaviour are dropped wholesale; note the prefix test
            // covers every `on*` handler without having to enumerate them.
            if key == "style" || key == "align" || key.starts_with("on") {
                continue;
            }
            match key {
                // Images are re-emitted through the asset table, never proxied.
                "src" | "srcset" => continue,
                "href" => {
                    let clean = sanitize_href(&attribute.value);
                    if !clean.is_empty() {
                        self.push(" href=\"");
                        let escaped = escape_html(&clean);
                        self.push(&escaped);
                        self.push("\"");
                    }
                }
                _ => {
                    self.push(" ");
                    self.push(key);
                    self.push("=\"");
                    let escaped = escape_html(&attribute.value);
                    self.push(&escaped);
                    self.push("\"");
                }
            }
        }
        if with_extra {
            self.write_block_extra();
        }
        if VOID_TAGS.contains(&tag) {
            self.push(">");
            return;
        }
        self.push(">");
        for child in children(node) {
            match &child.data {
                NodeData::Element { .. } => self.emit_element(&child, false),
                NodeData::Text { contents } => {
                    // Text content can never re-introduce markup once escaped.
                    let text = contents.borrow();
                    let escaped = escape_html(&text);
                    self.push(&escaped);
                }
                _ => {}
            }
        }
        self.push("</");
        self.push(tag);
        self.push(">");
    }

    /// Emit an `<img>`, resolving its `src` against the chapter path.
    fn emit_img(&mut self, node: &Handle, with_extra: bool) {
        let Some(src) = attr(node, "src").filter(|src| !src.is_empty()) else {
            return;
        };
        let alt = attr(node, "alt").unwrap_or_default();
        self.write_img(&src, &alt, with_extra);
    }

    /// Rewrite an image reference into `<asset_base>/<index>?v=<etag>`.
    ///
    /// External URLs are dropped rather than proxied (the reader must not issue
    /// requests to arbitrary hosts) and so are SVG assets, which the whitelist
    /// cannot make safe.
    fn write_img(&mut self, src: &str, alt: &str, with_extra: bool) {
        if is_external_url(src) {
            return;
        }
        let zip_path = resolve_path(&self.chapter, src);
        if zip_path.is_empty() {
            return;
        }
        let content_type = asset_content_type(&ext_of(&zip_path));
        if content_type == "application/octet-stream" || content_type == "image/svg+xml" {
            return;
        }
        let index = match self.asset_index.get(&zip_path) {
            Some(&index) => index,
            None => {
                let Ok(data) = zip_bytes(self.archive, &zip_path, self.budget) else {
                    return;
                };
                let (width, height, _) = image_dims(&data);
                let index = self.assets.len();
                self.assets.push(Asset {
                    data,
                    content_type: content_type.to_string(),
                    width,
                    height,
                });
                self.asset_index.insert(zip_path, index);
                index
            }
        };
        let base = self.asset_base;
        self.push("<img src=\"");
        self.push(base);
        self.push("/");
        let number = index.to_string();
        self.push(&number);
        if !self.asset_version.is_empty() {
            self.push("?v=");
            let version = query_escape(self.asset_version);
            self.push(&version);
        }
        self.push("\"");
        let (width, height) = {
            let asset = &self.assets[index];
            (asset.width, asset.height)
        };
        if width > 0 && height > 0 {
            let dimensions = format!(" width=\"{width}\" height=\"{height}\"");
            self.push(&dimensions);
        }
        if !alt.is_empty() {
            self.push(" alt=\"");
            let escaped = escape_html(alt);
            self.push(&escaped);
            self.push("\"");
        }
        if with_extra {
            self.write_block_extra();
        }
        self.push(">");
    }

    /// Replace an SVG that wraps exactly one `<image>` with the same `<img>`.
    ///
    /// Calibre-style full-page covers are often a single SVG image; rewriting
    /// them keeps the cover visible once the SVG itself is refused.
    fn emit_svg_as_image(&mut self, node: &Handle, with_extra: bool) -> bool {
        let mut images: Vec<Handle> = Vec::new();
        collect_elements(node, "image", &mut images);
        let [image] = images.as_slice() else {
            return false;
        };
        let Some(href) = attr(image, "href").filter(|href| !href.is_empty()) else {
            return false;
        };
        let mut alt = attr(node, "aria-label").unwrap_or_default();
        if alt.is_empty() {
            alt = attr(image, "alt").unwrap_or_default();
        }
        self.write_img(&href, &alt, with_extra);
        true
    }

    /// Write the block markers that follow the opening tag.
    ///
    /// `data-source-path` is always written; `data-frag-ids` carries the ids of
    /// dropped empty blocks forward so anchors into them still resolve. Both are
    /// HTML-escaped — see the module docs.
    fn write_block_extra(&mut self) {
        self.push(" data-source-path=\"");
        let escaped = escape_html(&self.chapter);
        self.push(&escaped);
        self.push("\"");
        if self.pending.is_empty() {
            return;
        }
        let joined = self.pending.join(" ");
        self.pending.clear();
        self.push(" data-frag-ids=\"");
        let escaped = escape_html(&joined);
        self.push(&escaped);
        self.push("\"");
    }

    /// Append to the chapter, charging the whole-book render budget.
    fn push(&mut self, value: &str) {
        self.render.write(&mut self.html, value);
    }
}

/// True when any descendant is a content block.
fn has_block_descendant(node: &Handle) -> bool {
    for child in children(node) {
        if let Some(tag) = tag_name(&child)
            && is_block_tag(&tag)
        {
            return true;
        }
        if has_block_descendant(&child) {
            return true;
        }
    }
    false
}

/// True when any descendant is an image (`image` only when already inside SVG).
fn has_media(node: &Handle, include_image: bool) -> bool {
    for child in children(node) {
        let Some(tag) = tag_name(&child) else {
            continue;
        };
        if tag == "img" || tag == "svg" || (include_image && tag == "image") {
            return true;
        }
        if has_media(&child, include_image) {
            return true;
        }
    }
    false
}

/// True when the tag must be dropped.
fn is_disallowed_tag(tag: &str) -> bool {
    DISALLOWED_TAGS.contains(&tag)
}

/// True when the tag is a standalone content block.
fn is_block_tag(tag: &str) -> bool {
    BLOCK_TAGS.contains(&tag)
}

/// Drop URLs that could execute or exfiltrate when followed.
///
/// Only the scheme prefix is inspected, and leading whitespace is stripped
/// first so `" javascript:"` cannot slip through.
fn sanitize_href(value: &str) -> String {
    let lower = value
        .trim_start_matches([' ', '\t', '\r', '\n'])
        .to_lowercase();
    if lower.starts_with("javascript:")
        || lower.starts_with("vbscript:")
        || lower.starts_with("data:")
    {
        return String::new();
    }
    value.to_string()
}

/// True when `value` is an absolute URL the reader must not fetch.
fn is_external_url(value: &str) -> bool {
    let lower = value
        .trim_start_matches([' ', '\t', '\r', '\n'])
        .to_lowercase();
    lower.starts_with("data:")
        || lower.starts_with("blob:")
        || lower.starts_with("http:")
        || lower.starts_with("https:")
}

/// Escape text for HTML, using Go's `html.EscapeString` mapping.
///
/// The mapping is deliberately Go's (`&#34;`/`&#39;` rather than named
/// entities) so the cleaned output stays comparable with the original.
fn escape_html(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for character in value.chars() {
        match character {
            '&' => out.push_str("&amp;"),
            '\'' => out.push_str("&#39;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&#34;"),
            other => out.push(other),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn href_sanitizing_strips_dangerous_schemes() {
        assert_eq!(sanitize_href("javascript:alert(1)"), "");
        assert_eq!(sanitize_href("DATA:text/html,x"), "");
        assert_eq!(sanitize_href("vbscript:x"), "");
        assert_eq!(sanitize_href("  \tjavascript:alert(1)"), "");
        assert_eq!(sanitize_href("JaVaScRiPt:alert(1)"), "");
        assert_eq!(sanitize_href("https://example.com"), "https://example.com");
        assert_eq!(sanitize_href("ch1.xhtml#a"), "ch1.xhtml#a");
    }

    #[test]
    fn external_image_urls_are_recognised() {
        assert!(is_external_url("http://evil.test/x.png"));
        assert!(is_external_url("HTTPS://evil.test/x.png"));
        assert!(is_external_url("data:image/png;base64,AAAA"));
        assert!(is_external_url("blob:abc"));
        assert!(is_external_url("  https://evil.test/x.png"));
        assert!(!is_external_url("img/fig.png"));
        assert!(!is_external_url("../img/fig.png"));
    }

    #[test]
    fn escaping_matches_go_html_escape_string() {
        assert_eq!(
            escape_html(r#"a&b'c<d>e"f"#),
            "a&amp;b&#39;c&lt;d&gt;e&#34;f"
        );
        assert_eq!(escape_html("你好"), "你好");
    }

    #[test]
    fn tag_classification_is_a_whitelist() {
        for tag in ["script", "style", "iframe", "object", "form", "link"] {
            assert!(is_disallowed_tag(tag), "{tag} must be disallowed");
        }
        assert!(!is_disallowed_tag("p"));
        for tag in [
            "p", "h1", "li", "pre", "figure", "img", "svg", "table", "hr",
        ] {
            assert!(is_block_tag(tag), "{tag} must be a block");
        }
        assert!(!is_block_tag("span"));
        assert!(!is_block_tag("a"));
    }
}

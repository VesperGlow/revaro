//! Deterministic reading-flow generation for EPUB and plain-text books.
//!
//! The flow is the server's stable representation of a book. EPUB content is
//! parsed from the already-sanitised chapter fragments into top-level blocks;
//! TXT content is split only at line boundaries. Blocks receive a continuous
//! data-block number and chunks group complete blocks by approximate UTF-16
//! and HTML-byte budgets. The browser can therefore change its viewport and
//! font without asking the server to paginate again.
//!
//! Navigation is resolved before block HTML is serialised. Text targets retain
//! the real text node path and UTF-16 offset, while media targets receive a
//! stable data-rv-anchor attribute. This keeps directory jumps tied to the
//! content the browser actually lays out instead of to an injected empty
//! element whose column can differ from the target text.

use std::collections::{HashMap, HashSet};

use html5ever::serialize::{SerializeOpts, TraversalScope};
use markup5ever_rcdom::{Handle, NodeData, RcDom, SerializableHandle};
use revaro_core::reader::{ChunkMeta, FlowManifest, SpineMeta, TocTarget};

use crate::dom::{attr, children, parse_html, tag_name};
use crate::model::{Book, Chapter, Format};
use crate::path::normalize_path;
use crate::text::utf16_len;

/// Version of the generated flow format.
pub const FLOW_FORMAT_VERSION: i32 = 4;

/// Approximate UTF-16 size of one flow chunk.
pub const CHUNK_CHARS_TARGET: i64 = 7_000;

/// Approximate HTML size of one flow chunk.
pub const CHUNK_BYTES_TARGET: i64 = 96 << 10;

/// Approximate UTF-16 size of a TXT block.
pub const TXT_BLOCK_CHARS_TARGET: i64 = 2_000;

/// Maximum number of EPUB chapters or TXT sections in one flow.
pub const MAX_SPINES: usize = 1 << 14;

/// Maximum number of content blocks in one flow.
pub const MAX_BLOCKS: usize = 1 << 24;

/// Maximum chunk object size served by the backend.
pub const MAX_FLOW_OBJECT: usize = 8 << 20;

/// One generated flow chunk.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Chunk {
    /// Concatenated block HTML.
    pub html: String,
    /// Metadata describing the blocks in html.
    pub meta: ChunkMeta,
}

/// Complete output of one flow generation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Built {
    /// Manifest shared with the browser.
    pub manifest: FlowManifest,
    /// Chunk bodies in the same order as manifest.chunks.
    pub chunks: Vec<Chunk>,
}

/// Failures that can occur while constructing a flow.
#[derive(Debug, thiserror::Error)]
pub enum FlowError {
    /// The parsed book format is not one the flow generator understands.
    #[error("不支持的格式: {0}")]
    UnsupportedFormat(String),
    /// The book declares more chapters than the bounded flow model permits.
    #[error("EPUB 章节数过多")]
    TooManySpines,
    /// The book produces more content blocks than the bounded flow model permits.
    #[error("内容块数量超限")]
    TooManyBlocks,
    /// A serialised block was not a complete HTML element.
    #[error("非法块片段")]
    InvalidBlock,
    /// The HTML serializer failed while materialising a block.
    #[error("序列化阅读流失败: {0}")]
    Serialize(#[from] std::io::Error),
    /// The serializer returned bytes that were not UTF-8.
    #[error("阅读流不是有效的 UTF-8")]
    InvalidUtf8(#[from] std::string::FromUtf8Error),
}

#[derive(Debug, Clone)]
struct BlockBuild {
    node: Option<Handle>,
    html: String,
    chars: i64,
    ids: Vec<String>,
    nav_anchors: Vec<String>,
    spine_start: bool,
}

#[derive(Debug, Clone)]
struct SpineBuild {
    blocks: Vec<BlockBuild>,
    start_global: usize,
}

struct ChapterTree {
    // RcDom's Drop implementation clears descendant children while the root
    // is released. Keep the owning DOM alive alongside the handles that the
    // flow is still resolving and serialising.
    _dom: RcDom,
    nodes: Vec<Handle>,
    ids: Vec<Vec<String>>,
    source_path: String,
}

#[derive(Debug, Clone)]
struct Binding {
    id: String,
    media: bool,
    text_path: Vec<i32>,
    text_offset: i32,
}

#[derive(Debug, Clone)]
enum NavTarget {
    Media(Handle),
    Text { node: Handle, offset: i32 },
}

/// Build a deterministic flow from a parsed book.
pub fn build(book: &Book) -> Result<Built, FlowError> {
    match book.format {
        Format::Epub => build_epub(book),
        Format::Txt => build_txt(book),
    }
}

fn build_epub(book: &Book) -> Result<Built, FlowError> {
    if book.chapters.len() > MAX_SPINES {
        return Err(FlowError::TooManySpines);
    }

    let mut spines = Vec::with_capacity(book.chapters.len());
    let mut trees = Vec::with_capacity(book.chapters.len());
    let mut global = 0usize;

    for chapter in &book.chapters {
        let start_global = global;
        let (tree, blocks) = match parse_chapter(chapter) {
            Some(tree) => {
                let blocks = tree
                    .nodes
                    .iter()
                    .enumerate()
                    .map(|(index, node)| BlockBuild {
                        node: Some(node.clone()),
                        html: String::new(),
                        chars: text_chars16(node),
                        ids: tree.ids[index].clone(),
                        nav_anchors: Vec::new(),
                        spine_start: false,
                    })
                    .collect();
                (Some(tree), blocks)
            }
            None => {
                // A malformed sanitised fragment should not make the whole
                // book unreadable. Escape it as one ordinary paragraph and
                // count its original text, so progress totals remain honest.
                let escaped = escape_html(&chapter.html);
                (
                    None,
                    vec![BlockBuild {
                        node: None,
                        html: format!("<p>{escaped}</p>"),
                        chars: utf16_len(&chapter.html),
                        ids: Vec::new(),
                        nav_anchors: Vec::new(),
                        spine_start: false,
                    }],
                )
            }
        };

        if global.saturating_add(blocks.len()) > MAX_BLOCKS {
            return Err(FlowError::TooManyBlocks);
        }
        trees.push(tree);
        global += blocks.len();
        spines.push(SpineBuild {
            blocks,
            start_global,
        });
    }

    let entries = resolve_toc(book, &trees, &mut spines);
    assemble(spines, entries, "epub")
}

fn parse_chapter(chapter: &Chapter) -> Option<ChapterTree> {
    let dom = parse_html(chapter.html.as_bytes())?;
    let body = find_element(&dom, "body")?;
    let mut nodes = Vec::new();
    let mut ids = Vec::new();
    let mut source_path = String::new();

    for child in children(&body) {
        if !matches!(child.data, NodeData::Element { .. }) {
            continue;
        }
        strip_flow_attributes(&child);
        if source_path.is_empty() {
            source_path = attr(&child, "data-source-path").unwrap_or_default();
        }
        ids.push(collect_ids(&child));
        nodes.push(child);
    }

    // Chapter.source_path is the canonical archive path even when a broken or
    // empty sanitised fragment did not carry a marker of its own.
    if source_path.is_empty() {
        source_path = chapter.source_path.clone();
    }
    Some(ChapterTree {
        _dom: dom,
        nodes,
        ids,
        source_path,
    })
}

fn strip_flow_attributes(root: &Handle) {
    let mut pending = vec![root.clone()];
    while let Some(node) = pending.pop() {
        if let NodeData::Element { attrs, .. } = &node.data {
            attrs.borrow_mut().retain(|attribute| {
                let key = &*attribute.name.local;
                !matches!(key, "data-block" | "data-spine-start" | "data-rv-anchor")
            });
        }
        pending.extend(children(&node));
    }
}

fn find_element(dom: &RcDom, tag: &str) -> Option<Handle> {
    let mut pending = vec![dom.document.clone()];
    while let Some(node) = pending.pop() {
        if tag_name(&node).as_deref() == Some(tag) {
            return Some(node);
        }
        let mut descendants = children(&node);
        descendants.reverse();
        pending.extend(descendants);
    }
    None
}

fn collect_ids(root: &Handle) -> Vec<String> {
    let mut ids = Vec::new();
    let mut pending = vec![root.clone()];
    while let Some(node) = pending.pop() {
        if let NodeData::Element { attrs, .. } = &node.data {
            for attribute in attrs.borrow().iter() {
                let key = attribute.name.local.to_string();
                if key == "id" && !attribute.value.is_empty() {
                    ids.push(attribute.value.to_string());
                } else if key == "data-frag-ids" {
                    ids.extend(
                        attribute
                            .value
                            .split_whitespace()
                            .filter(|value| !value.is_empty())
                            .map(str::to_owned),
                    );
                }
            }
        }
        let mut descendants = children(&node);
        descendants.reverse();
        pending.extend(descendants);
    }
    ids
}

fn find_fragment_element(block: &Handle, fragment: &str) -> Option<Handle> {
    if fragment.is_empty() {
        return None;
    }
    let mut pending = vec![block.clone()];
    while let Some(node) = pending.pop() {
        if attr(&node, "id").as_deref() == Some(fragment) {
            return Some(node);
        }
        let mut descendants = children(&node);
        descendants.reverse();
        pending.extend(descendants);
    }
    None
}

fn is_media(node: &Handle) -> bool {
    matches!(tag_name(node).as_deref(), Some("img" | "svg" | "video"))
}

fn resolve_nav_target(start: &Handle) -> Option<NavTarget> {
    let mut pending = vec![start.clone()];
    while let Some(node) = pending.pop() {
        if is_media(&node) {
            return Some(NavTarget::Media(node));
        }
        if let NodeData::Text { contents } = &node.data {
            let text = contents.borrow();
            let offset = first_visible_offset(&text);
            drop(text);
            if let Some(offset) = offset {
                return Some(NavTarget::Text {
                    node: node.clone(),
                    offset,
                });
            }
        }
        let mut descendants = children(&node);
        descendants.reverse();
        pending.extend(descendants);
    }
    None
}

fn first_visible_offset(text: &str) -> Option<i32> {
    let mut offset = 0i64;
    for character in text.chars() {
        if !character.is_whitespace() {
            return i32::try_from(offset).ok();
        }
        offset += i64::from(character.len_utf16() as u32);
    }
    None
}

fn path_to_node(root: &Handle, target: &Handle) -> Option<Vec<i32>> {
    let mut pending = vec![(root.clone(), Vec::new())];
    while let Some((node, path)) = pending.pop() {
        if std::rc::Rc::ptr_eq(&node, target) {
            return Some(path);
        }
        let descendants = children(&node);
        for (index, child) in descendants.into_iter().enumerate().rev() {
            let mut child_path = path.clone();
            child_path.push(i32::try_from(index).ok()?);
            pending.push((child, child_path));
        }
    }
    None
}

fn text_chars16(root: &Handle) -> i64 {
    let mut total = 0i64;
    let mut pending = vec![root.clone()];
    while let Some(node) = pending.pop() {
        if let NodeData::Text { contents } = &node.data {
            total = total.saturating_add(utf16_len(&contents.borrow()));
        }
        pending.extend(children(&node));
    }
    total
}

fn append_media_anchor(node: &Handle, id: &str) {
    let NodeData::Element { attrs, .. } = &node.data else {
        return;
    };
    let name = html5ever::QualName::new(
        None,
        html5ever::ns!(),
        html5ever::LocalName::from("data-rv-anchor"),
    );
    attrs.borrow_mut().push(html5ever::Attribute {
        name,
        value: id.into(),
    });
}

fn resolve_toc(
    book: &Book,
    trees: &[Option<ChapterTree>],
    spines: &mut [SpineBuild],
) -> Vec<ResolvedToc> {
    let mut entries = Vec::with_capacity(book.toc.len());
    let mut bindings: HashMap<(usize, String), Option<Binding>> = HashMap::new();
    let mut nav_sequence = 0usize;

    for entry in &book.toc {
        let spine = chapter_index_for_path(entry.path.as_str(), trees);
        let block = fragment_block(spines, spine, entry.fragment.as_str());
        let resolved = if spine < trees.len()
            && block < spines.get(spine).map_or(0, |spine| spine.blocks.len())
        {
            let key = (spine, entry.fragment.clone());
            if let Some(existing) = bindings.get(&key) {
                existing.clone()
            } else {
                let result = spines
                    .get_mut(spine)
                    .and_then(|spine| spine.blocks.get_mut(block))
                    .and_then(|block| {
                        let root = block.node.as_ref()?;
                        let start = find_fragment_element(root, &entry.fragment)
                            .unwrap_or_else(|| root.clone());
                        let target = resolve_nav_target(&start)?;
                        let id = format!("rvn-{nav_sequence}");
                        nav_sequence += 1;
                        match target {
                            NavTarget::Media(node) => {
                                append_media_anchor(&node, &id);
                                block.nav_anchors.push(id.clone());
                                Some(Binding {
                                    id,
                                    media: true,
                                    text_path: Vec::new(),
                                    text_offset: 0,
                                })
                            }
                            NavTarget::Text { node, offset } => {
                                let path = path_to_node(root, &node)?;
                                // Text targets have no marker in the HTML, but
                                // they still participate in chunk placement.
                                // Keeping the target block near a chunk start
                                // gives the client the same bounded jump path
                                // as a media target.
                                block.nav_anchors.push(id.clone());
                                Some(Binding {
                                    id,
                                    media: false,
                                    text_path: path,
                                    text_offset: offset,
                                })
                            }
                        }
                    });
                bindings.insert(key, result.clone());
                result
            }
        } else {
            None
        };

        entries.push(ResolvedToc {
            label: entry.label.clone(),
            depth: entry.depth,
            spine,
            block,
            fragment: entry.fragment.clone(),
            source_path: entry.path.clone(),
            binding: resolved,
        });
    }
    entries
}

fn chapter_index_for_path(path: &str, trees: &[Option<ChapterTree>]) -> usize {
    if path.is_empty() {
        return 0;
    }
    let normalized = normalize_path(path);
    trees
        .iter()
        .position(|tree| {
            tree.as_ref()
                .is_some_and(|tree| tree.source_path == normalized)
        })
        .unwrap_or(0)
}

fn fragment_block(spines: &[SpineBuild], spine: usize, fragment: &str) -> usize {
    if fragment.is_empty() {
        return 0;
    }
    spines
        .get(spine)
        .and_then(|spine| {
            spine
                .blocks
                .iter()
                .position(|block| block.ids.iter().any(|id| id == fragment))
        })
        .unwrap_or(0)
}

#[derive(Debug, Clone)]
struct ResolvedToc {
    label: String,
    depth: i32,
    spine: usize,
    block: usize,
    fragment: String,
    source_path: String,
    binding: Option<Binding>,
}

fn build_txt(book: &Book) -> Result<Built, FlowError> {
    let (chapters, marks) = txt_chapters(&book.text, &book.toc);
    if chapters.len() > MAX_SPINES {
        return Err(FlowError::TooManySpines);
    }

    let mut spines = Vec::with_capacity(chapters.len());
    let mut global = 0usize;
    for chapter in chapters {
        let start_global = global;
        let mut blocks = txt_blocks(&chapter)
            .into_iter()
            .map(|part| BlockBuild {
                chars: utf16_len(&part),
                html: format!(r#"<div class="txt-blk">{}</div>"#, escape_html(&part)),
                node: None,
                ids: Vec::new(),
                nav_anchors: Vec::new(),
                spine_start: false,
            })
            .collect::<Vec<_>>();
        if blocks.is_empty() {
            blocks.push(BlockBuild {
                chars: 0,
                html: r#"<div class="txt-blk"></div>"#.to_owned(),
                node: None,
                ids: Vec::new(),
                nav_anchors: Vec::new(),
                spine_start: false,
            });
        }
        if global.saturating_add(blocks.len()) > MAX_BLOCKS {
            return Err(FlowError::TooManyBlocks);
        }
        global += blocks.len();
        spines.push(SpineBuild {
            blocks,
            start_global,
        });
    }

    let entries = marks
        .into_iter()
        .filter(|mark| mark.spine < spines.len())
        .map(|mark| ResolvedToc {
            label: mark.label,
            depth: mark.depth,
            spine: mark.spine,
            block: 0,
            fragment: String::new(),
            source_path: String::new(),
            binding: None,
        })
        .collect();
    assemble(spines, entries, "txt")
}

#[derive(Debug, Clone)]
struct TxtMark {
    label: String,
    depth: i32,
    spine: usize,
}

fn txt_chapters(text: &str, toc: &[revaro_core::reader::TocEntry]) -> (Vec<String>, Vec<TxtMark>) {
    let units = utf16_len(text);
    let mut cuts = Vec::new();
    let mut seen = HashSet::new();
    for entry in toc {
        if entry.offset <= 0 || entry.offset >= units {
            continue;
        }
        let byte_offset = utf16_offset_to_byte(text, entry.offset);
        if seen.insert(byte_offset) {
            cuts.push(byte_offset);
        }
    }
    cuts.sort_unstable();

    if cuts.is_empty() && units > 60_000 {
        return (split_long_txt(text), Vec::new());
    }

    let mut cut_bytes = Vec::with_capacity(cuts.len() + 2);
    cut_bytes.push(0usize);
    cut_bytes.extend(cuts.iter().copied());
    cut_bytes.push(text.len());
    cut_bytes.dedup();

    let mut chapters = Vec::new();
    for pair in cut_bytes.windows(2) {
        if pair[1] > pair[0] {
            chapters.push(text[pair[0]..pair[1]].to_owned());
        }
    }
    if chapters.is_empty() {
        chapters.push(text.to_owned());
    }

    let mut marks = Vec::new();
    for entry in toc {
        let spine = if entry.offset == 0 {
            Some(0)
        } else if entry.offset > 0 && entry.offset < units {
            let byte_offset = utf16_offset_to_byte(text, entry.offset);
            cut_bytes
                .iter()
                .position(|value| *value == byte_offset)
                .filter(|position| *position > 0)
        } else {
            None
        };
        if let Some(spine) = spine {
            marks.push(TxtMark {
                label: entry.label.clone(),
                depth: entry.depth,
                spine,
            });
        }
    }
    (chapters, marks)
}

fn split_long_txt(text: &str) -> Vec<String> {
    let mut cuts = Vec::new();
    let mut units_since_cut = 0i64;
    let mut last_line_end = 0usize;
    let mut last_cut_line_end = 0usize;
    for (index, character) in text.char_indices() {
        let end = index + character.len_utf8();
        if character == '\n' {
            last_line_end = end;
        }
        units_since_cut += i64::from(character.len_utf16() as u32);
        if units_since_cut >= 30_000
            && last_line_end > last_cut_line_end
            && last_line_end < text.len()
        {
            cuts.push(last_line_end);
            last_cut_line_end = last_line_end;
            units_since_cut = 0;
        }
    }
    cuts.push(text.len());
    let mut chapters = Vec::new();
    let mut start = 0usize;
    for end in cuts {
        if end > start {
            chapters.push(text[start..end].to_owned());
            start = end;
        }
    }
    if start < text.len() {
        chapters.push(text[start..].to_owned());
    }
    if chapters.is_empty() {
        chapters.push(text.to_owned());
    }
    chapters
}

fn txt_blocks(text: &str) -> Vec<String> {
    let mut blocks = Vec::new();
    let mut start = 0usize;
    let mut chars = 0i64;
    let mut line_chars = 0i64;

    for (index, character) in text.char_indices() {
        let end = index + character.len_utf8();
        line_chars += i64::from(character.len_utf16() as u32);
        if character == '\n' {
            chars += line_chars;
            line_chars = 0;
            let paragraph_end = end == text.len() || text.as_bytes().get(end) == Some(&b'\n');
            if (chars >= TXT_BLOCK_CHARS_TARGET || paragraph_end) && chars > 0 {
                blocks.push(text[start..end].to_owned());
                start = end;
                chars = 0;
            }
        }
    }
    if start < text.len() {
        blocks.push(text[start..].to_owned());
    }
    blocks
}

fn utf16_offset_to_byte(text: &str, offset: i64) -> usize {
    let mut seen = 0i64;
    for (index, character) in text.char_indices() {
        let width = i64::from(character.len_utf16() as u32);
        if seen + width > offset {
            return index;
        }
        seen += width;
    }
    text.len()
}

fn assemble(
    mut spines: Vec<SpineBuild>,
    entries: Vec<ResolvedToc>,
    format: &str,
) -> Result<Built, FlowError> {
    let mut manifest = FlowManifest {
        version: FLOW_FORMAT_VERSION,
        format: format.to_owned(),
        spines: Vec::with_capacity(spines.len()),
        ..FlowManifest::default()
    };
    let mut all_blocks = Vec::new();

    for (spine_index, spine) in spines.iter_mut().enumerate() {
        for (block_index, block) in spine.blocks.iter_mut().enumerate() {
            block.spine_start = spine_index > 0 && block_index == 0;
            let html = if let Some(node) = &block.node {
                serialize_node(node)?
            } else {
                block.html.clone()
            };
            block.html = inject_data_attributes(&html, all_blocks.len(), block.spine_start)?;
        }
        manifest.spines.push(SpineMeta {
            block_start: i32::try_from(spine.start_global).unwrap_or(i32::MAX),
            block_count: i32::try_from(spine.blocks.len()).unwrap_or(i32::MAX),
        });
        all_blocks.extend(spine.blocks.iter().cloned());
    }

    let (chunks, total_chars) = build_chunks(&all_blocks);
    manifest.chunks = chunks;
    manifest.total_chars = total_chars;
    manifest.toc = entries
        .into_iter()
        .filter_map(|entry| {
            let spine = manifest.spines.get(entry.spine)?;
            let block = spine
                .block_start
                .saturating_add(i32::try_from(entry.block).ok()?);
            let binding = entry.binding;
            Some(TocTarget {
                label: entry.label,
                depth: entry.depth,
                spine: i32::try_from(entry.spine).ok()?,
                block,
                nav_anchor: binding
                    .as_ref()
                    .filter(|binding| binding.media)
                    .map_or_else(String::new, |binding| binding.id.clone()),
                text_path: binding
                    .as_ref()
                    .filter(|binding| !binding.media)
                    .map_or_else(Vec::new, |binding| binding.text_path.clone()),
                text_offset: binding
                    .as_ref()
                    .filter(|binding| !binding.media)
                    .map_or(0, |binding| binding.text_offset),
                chunk: chunk_index_for_block(&manifest.chunks, block),
                source_path: entry.source_path,
                source_fragment: entry.fragment,
            })
        })
        .collect();

    let chunks = manifest
        .chunks
        .iter()
        .map(|meta| {
            let start = usize::try_from(meta.block_start).unwrap_or(0);
            let end = start.saturating_add(usize::try_from(meta.block_count).unwrap_or(0));
            let html = all_blocks
                .get(start..end)
                .unwrap_or_default()
                .iter()
                .map(|block| block.html.as_str())
                .collect();
            Chunk {
                html,
                meta: meta.clone(),
            }
        })
        .collect();
    Ok(Built { manifest, chunks })
}

fn serialize_node(node: &Handle) -> Result<String, FlowError> {
    let mut bytes = Vec::new();
    html5ever::serialize(
        &mut bytes,
        &SerializableHandle::from(node.clone()),
        SerializeOpts {
            scripting_enabled: false,
            traversal_scope: TraversalScope::IncludeNode,
            create_missing_parent: false,
        },
    )?;
    Ok(String::from_utf8(bytes)?)
}

fn inject_data_attributes(
    html: &str,
    block: usize,
    spine_start: bool,
) -> Result<String, FlowError> {
    let Some(end) = html.find('>') else {
        return Err(FlowError::InvalidBlock);
    };
    if html[..end].starts_with("</") || !html[..end].starts_with('<') {
        return Err(FlowError::InvalidBlock);
    }
    let mut result = String::with_capacity(html.len() + 40);
    result.push_str(&html[..end]);
    result.push_str(&format!(r#" data-block="{block}""#));
    if spine_start {
        result.push_str(" data-spine-start");
    }
    result.push_str(&html[end..]);
    Ok(result)
}

fn chunk_index_for_block(chunks: &[ChunkMeta], block: i32) -> i32 {
    chunks
        .iter()
        .position(|chunk| {
            block >= chunk.block_start
                && block < chunk.block_start.saturating_add(chunk.block_count)
        })
        .and_then(|index| i32::try_from(index).ok())
        .unwrap_or(0)
}

fn build_chunks(blocks: &[BlockBuild]) -> (Vec<ChunkMeta>, i64) {
    let mut chunks = Vec::new();
    let mut total_chars = 0i64;
    let mut start = 0usize;
    let mut chars = 0i64;
    let mut bytes = 0i64;

    let flush = |end: usize,
                 chunks: &mut Vec<ChunkMeta>,
                 start: &mut usize,
                 chars: &mut i64,
                 bytes: &mut i64,
                 total_chars: &mut i64| {
        if end <= *start {
            return;
        }
        chunks.push(ChunkMeta {
            index: i32::try_from(chunks.len()).unwrap_or(i32::MAX),
            block_start: i32::try_from(*start).unwrap_or(i32::MAX),
            block_count: i32::try_from(end - *start).unwrap_or(i32::MAX),
            chars: *chars,
            bytes: i32::try_from((*bytes).max(0)).unwrap_or(i32::MAX),
            url: String::new(),
        });
        *total_chars = total_chars.saturating_add(*chars);
        *start = end;
        *chars = 0;
        *bytes = 0;
    };

    for (index, block) in blocks.iter().enumerate() {
        if !block.nav_anchors.is_empty()
            && index > start
            && (chars >= CHUNK_CHARS_TARGET / 2 || bytes >= CHUNK_BYTES_TARGET / 2)
        {
            flush(
                index,
                &mut chunks,
                &mut start,
                &mut chars,
                &mut bytes,
                &mut total_chars,
            );
        }
        chars = chars.saturating_add(block.chars);
        bytes = bytes.saturating_add(i64::try_from(block.html.len()).unwrap_or(i64::MAX));
        if chars >= CHUNK_CHARS_TARGET || bytes >= CHUNK_BYTES_TARGET {
            flush(
                index + 1,
                &mut chunks,
                &mut start,
                &mut chars,
                &mut bytes,
                &mut total_chars,
            );
        }
    }
    flush(
        blocks.len(),
        &mut chunks,
        &mut start,
        &mut chars,
        &mut bytes,
        &mut total_chars,
    );
    if chunks.is_empty() {
        chunks.push(ChunkMeta {
            index: 0,
            block_start: 0,
            block_count: 0,
            chars: 0,
            bytes: 0,
            url: String::new(),
        });
    }
    (chunks, total_chars)
}

fn escape_html(value: &str) -> String {
    let mut escaped = String::with_capacity(value.len());
    for character in value.chars() {
        match character {
            '&' => escaped.push_str("&amp;"),
            '<' => escaped.push_str("&lt;"),
            '>' => escaped.push_str("&gt;"),
            '\'' => escaped.push_str("&#39;"),
            '"' => escaped.push_str("&#34;"),
            _ => escaped.push(character),
        }
    }
    escaped
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Book, Chapter, Format};
    use revaro_core::reader::TocEntry;

    fn txt_book(text: &str) -> Book {
        Book {
            format: Format::Txt,
            text: text.to_owned(),
            toc: vec![TocEntry {
                label: "第二章".into(),
                offset: utf16_len("第一章\n正文😀\n"),
                ..TocEntry::default()
            }],
            ..Book::default()
        }
    }

    #[test]
    fn txt_flow_preserves_text_and_utf16_totals() {
        let text = "第一章\n正文😀\n第二章\n继续\n";
        let built = build(&txt_book(text)).unwrap();
        assert_eq!(build(&txt_book(text)).unwrap(), built);
        assert_eq!(built.manifest.version, FLOW_FORMAT_VERSION);
        assert_eq!(built.manifest.total_chars, utf16_len(text));
        let html = built
            .chunks
            .iter()
            .map(|chunk| chunk.html.as_str())
            .collect::<String>();
        assert!(html.contains("第一章"));
        assert!(html.contains("data-block=\"0\""));
        assert_eq!(built.manifest.toc[0].spine, 1);
        assert_eq!(built.manifest.toc[0].block, 1);
    }

    #[test]
    fn txt_blocks_keep_long_lines_atomic_and_preserve_every_code_unit() {
        let text = format!(
            "{}\n{}\n{}\n",
            "😀".repeat(1_000),
            "a".repeat(2_500),
            "尾".repeat(1_000)
        );
        let blocks = txt_blocks(&text);
        assert_eq!(blocks.concat(), text);
        assert_eq!(blocks.len(), 3);
        assert!(blocks.iter().all(|block| block.ends_with('\n')));
        assert!(
            blocks
                .iter()
                .any(|block| utf16_len(block) > TXT_BLOCK_CHARS_TARGET)
        );
    }

    #[test]
    fn long_txt_without_toc_is_split_at_line_boundaries() {
        let text = "x\n".repeat(31_000);
        let (chapters, marks) = txt_chapters(&text, &[]);
        assert!(chapters.len() >= 3);
        assert!(marks.is_empty());
        assert_eq!(chapters.concat(), text);
        assert!(chapters.iter().all(|chapter| chapter.ends_with('\n')));
    }

    #[test]
    fn epub_text_targets_keep_real_dom_paths_without_markers() {
        let book = Book {
            format: Format::Epub,
            chapters: vec![Chapter {
                source_path: "OEBPS/ch1.xhtml".into(),
                html: r#"<p data-source-path="OEBPS/ch1.xhtml"><span>  Hello</span></p>"#.into(),
            }],
            toc: vec![TocEntry {
                label: "Start".into(),
                path: "OEBPS/ch1.xhtml".into(),
                ..TocEntry::default()
            }],
            ..Book::default()
        };
        let built = build(&book).unwrap();
        let target = &built.manifest.toc[0];
        assert_eq!(
            target.text_path,
            vec![0, 0],
            "manifest={:?} html={:?}",
            built.manifest,
            built.chunks[0].html
        );
        assert_eq!(target.text_offset, 2);
        assert!(target.nav_anchor.is_empty());
        assert!(!built.chunks[0].html.contains("data-rv-anchor"));
    }

    #[test]
    fn epub_media_targets_get_an_anchor_and_can_force_a_chunk_boundary() {
        let mut first = String::from("<p data-source-path=\"ch.xhtml\">");
        first.push_str(&"a".repeat((CHUNK_CHARS_TARGET / 2) as usize + 1));
        first.push_str("</p>");
        let book = Book {
            format: Format::Epub,
            chapters: vec![
                Chapter {
                    source_path: "ch.xhtml".into(),
                    html: first,
                },
                Chapter {
                    source_path: "ch2.xhtml".into(),
                    html: r#"<img data-source-path="ch2.xhtml" id="cover" src="/api/files/1/book/assets/0">"#.into(),
                },
            ],
            toc: vec![TocEntry {
                label: "Cover".into(),
                path: "ch2.xhtml".into(),
                fragment: "cover".into(),
                ..TocEntry::default()
            }],
            ..Book::default()
        };
        let built = build(&book).unwrap();
        let target = &built.manifest.toc[0];
        assert_eq!(target.nav_anchor, "rvn-0");
        assert!(
            built.chunks[target.chunk as usize]
                .html
                .contains(r#"data-rv-anchor="rvn-0""#)
        );
        assert!(built.manifest.spines[1].block_start > 0);
    }

    #[test]
    fn epub_text_targets_can_force_a_chunk_boundary_without_html_markers() {
        let first = format!("<p>{}</p>", "a".repeat(4_000));
        let book = Book {
            format: Format::Epub,
            chapters: vec![Chapter {
                source_path: "ch.xhtml".into(),
                html: format!(r#"{first}<p id="target">{}</p>"#, "b".repeat(4_000)),
            }],
            toc: vec![TocEntry {
                label: "Target".into(),
                path: "ch.xhtml".into(),
                fragment: "target".into(),
                ..TocEntry::default()
            }],
            ..Book::default()
        };
        let built = build(&book).unwrap();
        let target = &built.manifest.toc[0];
        assert_eq!(target.block, 1);
        assert_eq!(target.chunk, 1);
        assert_eq!(built.manifest.chunks.len(), 2);
        assert!(!built.chunks[1].html.contains("data-rv-anchor"));
    }

    #[test]
    fn source_flow_attributes_are_removed_before_generated_numbers_are_added() {
        let book = Book {
            format: Format::Epub,
            chapters: vec![Chapter {
                source_path: "ch.xhtml".into(),
                html: r#"<p data-block="999" data-spine-start data-rv-anchor="source">text</p>"#
                    .into(),
            }],
            ..Book::default()
        };
        let built = build(&book).unwrap();
        assert!(built.chunks[0].html.contains(r#"data-block="0""#));
        assert!(!built.chunks[0].html.contains(r#"data-block="999""#));
        assert!(!built.chunks[0].html.contains("data-rv-anchor"));
        assert!(!built.chunks[0].html.contains("data-spine-start"));
    }

    #[test]
    fn empty_books_still_have_an_addressable_txt_block() {
        let built = build(&txt_book("")).unwrap();
        assert_eq!(built.manifest.total_blocks(), 1);
        assert_eq!(built.manifest.chunks[0].block_count, 1);
        assert!(built.chunks[0].html.contains("data-block=\"0\""));
    }
}

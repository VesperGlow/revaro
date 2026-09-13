//! Target-independent rules for the EPUB/TXT reader.
//!
//! The browser component owns DOM measurement and CSS columns. The rules that
//! decide which flow chunk contains a position, how a virtual window keeps its
//! pagination origin, and how reader preferences are clamped are kept here so
//! they can be tested without a browser.

use std::cmp::Ordering;

use revaro_core::reader::{Anchor, FlowManifest};

/// Smallest reader font size accepted by the UI.
pub const FONT_MIN: i32 = 14;
/// Largest reader font size accepted by the UI.
pub const FONT_MAX: i32 = 32;
/// Supported line heights, kept deliberately finite so a preference cannot
/// cause an unbounded relayout loop through a malformed stored value.
pub const LINE_HEIGHTS: [f64; 3] = [1.4, 1.7, 2.0];

/// Default reader font size.
pub const DEFAULT_FONT_SIZE: i32 = 19;
/// Default reader line height.
pub const DEFAULT_LINE_HEIGHT: f64 = 1.7;

/// Margins used by the CSS-columns layout.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReaderMargins {
    /// Top margin in CSS pixels.
    pub top: i32,
    /// Bottom margin in CSS pixels.
    pub bottom: i32,
    /// Horizontal margin in CSS pixels.
    pub side: i32,
}

/// Compare two durable reading positions in the same order as the server.
#[must_use]
pub fn compare_anchor(left: &Anchor, right: &Anchor) -> Ordering {
    left.cmp(right)
}

/// Return the spine containing a global content block.
#[must_use]
pub fn spine_for_block(manifest: &FlowManifest, block: i32) -> i32 {
    manifest.spine_for_block(block)
}

/// Return the chunk containing a global content block, or `-1` if it is absent.
#[must_use]
pub fn chunk_for_block(manifest: &FlowManifest, block: i32) -> i32 {
    manifest.chunk_for_block(block).unwrap_or(-1)
}

/// Return the UTF-16 length before a chunk.
#[must_use]
pub fn chunk_prefix(manifest: &FlowManifest, chunk_index: i32) -> i64 {
    manifest.chars_before_chunk(chunk_index)
}

/// Map a global UTF-16 position to a chunk and an offset within that chunk.
///
/// The position is clamped to the book's declared range. Empty manifests and
/// malformed zero-length chunks still produce a safe `(0, 0)` result instead of
/// allowing a caller to index outside the chunk list.
#[must_use]
pub fn locate_char(manifest: &FlowManifest, character: i64) -> (i32, i64) {
    if manifest.chunks.is_empty() {
        return (0, 0);
    }

    let target = character.clamp(0, manifest.total_chars.max(0));
    let mut low = 0_usize;
    let mut high = manifest.chunks.len();
    while low < high {
        let middle = (low + high) / 2;
        let end = manifest.chunks.get(middle).map_or(0, |chunk| {
            chunk_prefix(manifest, middle as i32) + chunk.chars
        });
        if end > target {
            high = middle;
        } else {
            low = middle + 1;
        }
    }

    let chunk = low.min(manifest.chunks.len() - 1) as i32;
    let offset = (target - chunk_prefix(manifest, chunk)).max(0);
    (chunk, offset)
}

/// Return the last TOC entry whose block is at or before `block`.
#[must_use]
pub fn toc_active_index(manifest: &FlowManifest, block: i32) -> i32 {
    let mut active = -1;
    for (index, entry) in manifest.toc.iter().enumerate() {
        if entry.block <= block {
            active = index as i32;
        } else {
            break;
        }
    }
    active
}

/// Return the number of top-level content blocks in the book.
#[must_use]
pub fn total_blocks(manifest: &FlowManifest) -> i32 {
    manifest.total_blocks()
}

/// Return the chunk that contains the beginning of the current spine.
///
/// CSS columns need a stable origin. When a window moves within one spine, the
/// chunk containing that spine's first block remains in the DOM even if it is
/// not the chunk currently being read.
#[must_use]
pub fn spine_origin_chunk(manifest: &FlowManifest, block: i32) -> i32 {
    let spine = spine_for_block(manifest, block).max(0) as usize;
    let start = manifest
        .spines
        .get(spine)
        .map_or(0, |value| value.block_start);
    chunk_for_block(manifest, start).max(0)
}

/// Compute the stable inclusive chunk window around a block.
#[must_use]
pub fn stable_window_range(manifest: &FlowManifest, block: i32, ahead: i32) -> (i32, i32) {
    let last = manifest.chunks.len().saturating_sub(1) as i32;
    if manifest.chunks.is_empty() {
        return (0, 0);
    }

    let center = chunk_for_block(manifest, block);
    if center < 0 {
        return (0, ahead.max(0).min(last));
    }
    let first = spine_origin_chunk(manifest, block).clamp(0, last);
    let last = (center.saturating_add(ahead.max(0))).clamp(first, last);
    (first, last)
}

/// Clamp an untrusted or persisted font size to the reader range.
#[must_use]
pub fn clamp_font_size(value: i32) -> i32 {
    value.clamp(FONT_MIN, FONT_MAX)
}

/// Return a supported line height, falling back to the product default.
#[must_use]
pub fn valid_line_height(value: f64) -> f64 {
    if LINE_HEIGHTS
        .iter()
        .any(|candidate| value.is_finite() && (value - candidate).abs() < f64::EPSILON)
    {
        value
    } else {
        DEFAULT_LINE_HEIGHT
    }
}

/// Compute the reading margins without including the fixed toolbars in layout.
#[must_use]
pub fn compute_margins(width: f64, height: f64) -> ReaderMargins {
    let width = if width.is_finite() {
        width.max(0.0)
    } else {
        0.0
    };
    let height = if height.is_finite() {
        height.max(0.0)
    } else {
        0.0
    };
    let mut side = (width * 0.055).clamp(16.0, 44.0).round() as i32;
    if width - 2.0 * f64::from(side) > 720.0 {
        side = ((width - 720.0) / 2.0).round() as i32;
    }
    let mobile = width <= 850.0;
    let top = if mobile {
        (height * 0.025).clamp(16.0, 28.0).round() as i32
    } else {
        60
    };
    let bottom = if mobile {
        (height * 0.018).clamp(12.0, 22.0).round() as i32
    } else {
        24
    };
    ReaderMargins { top, bottom, side }
}

/// Whether two manifests describe the same layout semantics.
///
/// The generated HTML is deterministic for a given version, source fingerprint
/// and metadata sequence. Comparing those fields lets a reopened reader keep
/// its already-rendered chunks while still discarding a stale cached manifest.
#[must_use]
pub fn same_layout(left: &FlowManifest, right: &FlowManifest) -> bool {
    left.version == right.version
        && left.format == right.format
        && left.total_chars == right.total_chars
        && left.book_key == right.book_key
        && left.spines == right.spines
        && left.chunks.len() == right.chunks.len()
        && left.chunks.iter().zip(&right.chunks).all(|(a, b)| {
            a.block_start == b.block_start && a.block_count == b.block_count && a.chars == b.chars
        })
        && left.toc.len() == right.toc.len()
        && left.toc.iter().zip(&right.toc).all(|(a, b)| {
            a.label == b.label
                && a.depth == b.depth
                && a.block == b.block
                && a.nav_anchor == b.nav_anchor
                && a.text_path == b.text_path
                && a.text_offset == b.text_offset
                && a.source_fragment == b.source_fragment
                && a.source_path == b.source_path
        })
}

/// Validate a manifest before it is allowed to control DOM queries or URL
/// indices in the browser.
#[must_use]
pub fn validate_manifest(manifest: &FlowManifest) -> bool {
    if manifest.version <= 0
        || !matches!(manifest.format.as_str(), "epub" | "txt")
        || manifest.total_chars < 0
    {
        return false;
    }

    let mut total_blocks = 0_i64;
    for spine in &manifest.spines {
        if spine.block_count < 0 || i64::from(spine.block_start) != total_blocks {
            return false;
        }
        total_blocks = total_blocks.saturating_add(i64::from(spine.block_count));
    }
    if total_blocks > i64::from(i32::MAX) {
        return false;
    }

    let mut chunk_blocks = 0_i64;
    let mut chunk_chars = 0_i64;
    for (index, chunk) in manifest.chunks.iter().enumerate() {
        if i32::try_from(index).ok() != Some(chunk.index)
            || chunk.block_count <= 0
            || i64::from(chunk.block_start) != chunk_blocks
            || chunk.chars < 0
        {
            return false;
        }
        chunk_blocks = chunk_blocks.saturating_add(i64::from(chunk.block_count));
        chunk_chars = chunk_chars.saturating_add(chunk.chars);
    }
    if chunk_blocks != total_blocks || chunk_chars != manifest.total_chars {
        return false;
    }

    for entry in &manifest.toc {
        if entry.depth < 0
            || entry.block < 0
            || i64::from(entry.block) >= total_blocks
            || entry.text_path.len() > Anchor::MAX_PATH
            || entry.text_path.iter().any(|index| *index < 0)
            || manifest.chunk_for_block(entry.block) != Some(entry.chunk)
        {
            return false;
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use revaro_core::reader::{ChunkMeta, SpineMeta, TocTarget};

    fn manifest() -> FlowManifest {
        FlowManifest {
            version: 2,
            format: "txt".to_owned(),
            total_chars: 1_200,
            book_key: "book-key".to_owned(),
            spines: vec![
                SpineMeta {
                    block_start: 0,
                    block_count: 10,
                },
                SpineMeta {
                    block_start: 10,
                    block_count: 5,
                },
                SpineMeta {
                    block_start: 15,
                    block_count: 3,
                },
            ],
            chunks: vec![
                ChunkMeta {
                    index: 0,
                    block_start: 0,
                    block_count: 7,
                    chars: 499,
                    ..Default::default()
                },
                ChunkMeta {
                    index: 1,
                    block_start: 7,
                    block_count: 8,
                    chars: 301,
                    ..Default::default()
                },
                ChunkMeta {
                    index: 2,
                    block_start: 15,
                    block_count: 3,
                    chars: 400,
                    ..Default::default()
                },
            ],
            toc: vec![
                TocTarget {
                    label: "第一章".to_owned(),
                    block: 0,
                    ..Default::default()
                },
                TocTarget {
                    label: "第二章".to_owned(),
                    chunk: 1,
                    block: 10,
                    ..Default::default()
                },
                TocTarget {
                    label: "第二节".to_owned(),
                    chunk: 2,
                    block: 15,
                    ..Default::default()
                },
            ],
            ..Default::default()
        }
    }

    #[test]
    fn orders_anchors_by_spine_block_path_and_offset() {
        let first = Anchor {
            spine: 0,
            block: 4,
            path: vec![1, 2],
            offset: 3,
        };
        let second = Anchor {
            spine: 0,
            block: 4,
            path: vec![1, 3],
            offset: 0,
        };
        assert_eq!(compare_anchor(&first, &second), Ordering::Less);
        assert_eq!(compare_anchor(&first, &first), Ordering::Equal);
    }

    #[test]
    fn maps_blocks_and_chunks_with_spines_that_cross_chunks() {
        let value = manifest();
        assert_eq!(spine_for_block(&value, 9), 0);
        assert_eq!(spine_for_block(&value, 12), 1);
        assert_eq!(chunk_for_block(&value, 6), 0);
        assert_eq!(chunk_for_block(&value, 12), 1);
        assert_eq!(chunk_for_block(&value, 99), -1);
        assert_eq!(total_blocks(&value), 18);
    }

    #[test]
    fn locates_utf16_positions_at_boundaries_and_out_of_range() {
        let value = manifest();
        assert_eq!(chunk_prefix(&value, 0), 0);
        assert_eq!(chunk_prefix(&value, 1), 499);
        assert_eq!(locate_char(&value, 0), (0, 0));
        assert_eq!(locate_char(&value, 498), (0, 498));
        assert_eq!(locate_char(&value, 499), (1, 0));
        assert_eq!(locate_char(&value, 800), (2, 0));
        assert_eq!(locate_char(&value, -1), (0, 0));
        assert_eq!(locate_char(&value, 99_999), (2, 400));
    }

    #[test]
    fn highlights_the_last_toc_entry_before_the_current_block() {
        let value = manifest();
        assert_eq!(toc_active_index(&value, -1), -1);
        assert_eq!(toc_active_index(&value, 9), 0);
        assert_eq!(toc_active_index(&value, 10), 1);
        assert_eq!(toc_active_index(&value, 17), 2);
    }

    #[test]
    fn stable_window_keeps_the_spine_origin() {
        let value = manifest();
        assert_eq!(spine_origin_chunk(&value, 0), 0);
        assert_eq!(spine_origin_chunk(&value, 12), 1);
        assert_eq!(spine_origin_chunk(&value, 16), 2);
        assert_eq!(stable_window_range(&value, 12, 2), (1, 2));
        assert_eq!(stable_window_range(&value, 16, 5), (2, 2));
        assert_eq!(stable_window_range(&value, 5, 2), (0, 2));
        assert_eq!(stable_window_range(&value, 99, 2), (0, 2));
    }

    #[test]
    fn clamps_reader_preferences_and_rejects_unknown_line_heights() {
        assert_eq!(clamp_font_size(1), FONT_MIN);
        assert_eq!(clamp_font_size(19), 19);
        assert_eq!(clamp_font_size(100), FONT_MAX);
        assert_eq!(valid_line_height(1.4), 1.4);
        assert_eq!(valid_line_height(f64::NAN), DEFAULT_LINE_HEIGHT);
        assert_eq!(valid_line_height(1.5), DEFAULT_LINE_HEIGHT);
    }

    #[test]
    fn computes_mobile_and_desktop_margins() {
        assert_eq!(
            compute_margins(390.0, 844.0),
            ReaderMargins {
                top: 21,
                bottom: 15,
                side: 21
            }
        );
        assert_eq!(
            compute_margins(1_440.0, 900.0),
            ReaderMargins {
                top: 60,
                bottom: 24,
                side: 360
            }
        );
        assert_eq!(
            compute_margins(f64::NAN, f64::INFINITY),
            ReaderMargins {
                top: 16,
                bottom: 12,
                side: 16
            }
        );
    }

    #[test]
    fn detects_layout_changes_that_must_rebuild_the_window() {
        let first = manifest();
        let mut second = first.clone();
        assert!(same_layout(&first, &second));
        second.chunks[1].chars += 1;
        assert!(!same_layout(&first, &second));
        second = first.clone();
        second.toc[0].source_fragment = "changed".to_owned();
        assert!(!same_layout(&first, &second));
        second = first.clone();
        second.toc[0].label = "改名后的章节".to_owned();
        assert!(!same_layout(&first, &second));
        second = first;
        second.toc[0].depth = 1;
        assert!(!same_layout(&manifest(), &second));
    }

    #[test]
    fn rejects_manifests_with_inconsistent_ranges_or_toc_targets() {
        let value = manifest();
        assert!(validate_manifest(&value));

        let mut broken = value.clone();
        broken.chunks[1].block_start += 1;
        assert!(!validate_manifest(&broken));

        let mut broken = value.clone();
        broken.toc[0].chunk = 2;
        assert!(!validate_manifest(&broken));

        let mut broken = value;
        broken.total_chars += 1;
        assert!(!validate_manifest(&broken));
    }
}

//! The reader's continuous "reading flow" model, shared by the generator
//! (server), the storage layer and the paginating client.
//!
//! A *flow* is one book laid out as a single continuous stream: every spine of
//! an EPUB, or every chapter of a text file, is cleaned and concatenated.
//! Text and images share one layout context, so a page break is never a spine
//! break. The server slices the stream into *chunks* at natural DOM boundaries;
//! a chunk is not a page and is independent of viewport and font size. The
//! browser loads the chunks around the current position and performs the real
//! pagination with native CSS columns.
//!
//! A position (`Anchor`) addresses a text offset inside a globally numbered
//! content block, so it survives chunking, client-side pagination, font
//! changes and rotation.

use serde::{Deserialize, Deserializer, Serialize};

fn deserialize_nullable_vec<'de, D, T>(deserializer: D) -> Result<Vec<T>, D::Error>
where
    D: Deserializer<'de>,
    T: Deserialize<'de>,
{
    Ok(Option::<Vec<T>>::deserialize(deserializer)?.unwrap_or_default())
}

/// A stable reading position inside a book.
///
/// * `spine` — chapter index (EPUB spine item or text chapter).
/// * `block` — globally numbered top-level content block (`data-block` in the
///   chunk HTML, directly addressable by the client DOM).
/// * `path` — child-node index chain from the block element to the node holding
///   `offset`. An empty path means the block element itself.
/// * `offset` — UTF-16 code-unit offset inside the target text node. `-1` means
///   "the element boundary immediately before the last path element".
///
/// The legacy shape `{spine, path, offset}` (where `path[0]` *was* the
/// chapter-local block index) is migrated on deserialization, which is why this
/// type implements [`Deserialize`] by hand.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct Anchor {
    /// Chapter index.
    pub spine: i32,
    /// Globally numbered content block.
    pub block: i32,
    /// Child-node index chain; omitted when it addresses the block itself.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub path: Vec<i32>,
    /// UTF-16 offset inside the target text node.
    pub offset: i32,
}

/// The explicit JSON shape used to tell the current and legacy formats apart.
#[derive(Deserialize)]
struct RawAnchor {
    #[serde(default)]
    spine: i32,
    #[serde(default)]
    block: Option<i32>,
    #[serde(default)]
    path: Vec<i32>,
    #[serde(default = "default_offset")]
    offset: i32,
}

fn default_offset() -> i32 {
    -1
}

impl<'de> Deserialize<'de> for Anchor {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let raw = RawAnchor::deserialize(deserializer)?;
        match raw.block {
            Some(block) => Ok(Self {
                spine: raw.spine,
                block,
                path: raw.path,
                offset: raw.offset,
            }),
            None => {
                // Legacy `{spine, path, offset}`: the first path element was the
                // chapter-local block number. An empty path meant the chapter
                // root, which is block 0.
                let mut path = raw.path;
                let block = if path.is_empty() { 0 } else { path.remove(0) };
                Ok(Self {
                    spine: raw.spine,
                    block,
                    path,
                    offset: raw.offset,
                })
            }
        }
    }
}

impl Anchor {
    /// Largest chapter index an anchor may carry.
    pub const MAX_SPINE: i32 = 1 << 20;
    /// Largest block or offset value an anchor may carry.
    pub const MAX_BLOCK: i32 = 1 << 26;
    /// Longest child-node path an anchor may carry.
    pub const MAX_PATH: usize = 48;
    /// The sentinel offset meaning "element boundary before the target".
    pub const BOUNDARY_OFFSET: i32 = -1;

    /// Bound the anchor's ranges so corrupted or hostile progress data cannot
    /// drive the client into pathological work.
    #[must_use]
    pub fn is_valid(&self) -> bool {
        if self.spine < 0 || self.spine >= Self::MAX_SPINE {
            return false;
        }
        if self.block < 0 || self.block >= Self::MAX_BLOCK {
            return false;
        }
        if self.path.len() > Self::MAX_PATH {
            return false;
        }
        if self
            .path
            .iter()
            .any(|index| *index < 0 || *index >= Self::MAX_SPINE)
        {
            return false;
        }
        self.offset >= Self::BOUNDARY_OFFSET && self.offset < Self::MAX_BLOCK
    }
}

impl PartialOrd for Anchor {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for Anchor {
    /// Total order over positions in a book: spine, then block, then each path
    /// element, then offset. Used for sorting and binary search.
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.spine
            .cmp(&other.spine)
            .then_with(|| self.block.cmp(&other.block))
            .then_with(|| self.path.cmp(&other.path))
            .then_with(|| self.offset.cmp(&other.offset))
    }
}

/// One entry of a book's table of contents as the reader reports it.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TocEntry {
    /// Display label.
    pub label: String,
    /// Source path inside the EPUB, omitted when not applicable.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub path: String,
    /// Source fragment inside the EPUB, omitted when not applicable.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub fragment: String,
    /// Byte offset for plain-text books, omitted for EPUB.
    #[serde(default, skip_serializing_if = "is_zero_i64")]
    pub offset: i64,
    /// Nesting depth, `0` at the top level.
    #[serde(default)]
    pub depth: i32,
}

fn is_zero_i64(value: &i64) -> bool {
    *value == 0
}

/// The block range covered by one chapter.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SpineMeta {
    /// First block of the chapter.
    pub block_start: i32,
    /// Number of blocks in the chapter.
    pub block_count: i32,
}

/// One flow chunk.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChunkMeta {
    /// Chunk index in reading order.
    pub index: i32,
    /// First block contained in the chunk.
    pub block_start: i32,
    /// Number of blocks contained in the chunk.
    pub block_count: i32,
    /// UTF-16 code units of text in the chunk, used for progress scaling.
    pub chars: i64,
    /// Estimated HTML size in bytes, omitted when unknown.
    #[serde(default, skip_serializing_if = "is_zero_i32")]
    pub bytes: i32,
    /// URL the chunk can be fetched from, omitted when not addressable yet.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub url: String,
}

fn is_zero_i32(value: &i32) -> bool {
    *value == 0
}

fn missing_chunk() -> i32 {
    -1
}

/// A table-of-contents entry resolved onto the flow.
///
/// The resolved target is stable across client pagination: a text target is
/// addressed by the real text node's path plus a UTF-16 offset, and a media
/// target by a synthetic element id. Both fall back to the block start, and
/// finally to the raw source path, when the client cannot resolve them.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TocTarget {
    /// Display label.
    pub label: String,
    /// Nesting depth.
    #[serde(default)]
    pub depth: i32,
    /// Chapter the target belongs to.
    pub spine: i32,
    /// Block the target belongs to.
    pub block: i32,
    /// Synthetic element id for media targets, omitted otherwise.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub nav_anchor: String,
    /// Path to the target text node for text targets, omitted otherwise.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub text_path: Vec<i32>,
    /// UTF-16 offset of the first visible character, omitted when zero.
    #[serde(default, skip_serializing_if = "is_zero_i32")]
    pub text_offset: i32,
    /// Chunk containing the target; `-1` means an older manifest omitted it.
    /// `0` is meaningful for the first chunk.
    #[serde(default = "missing_chunk")]
    pub chunk: i32,
    /// Original EPUB path, kept for debugging and client fallback.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub source_path: String,
    /// Original EPUB fragment, kept for debugging and client fallback.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub source_fragment: String,
}

/// The manifest describing one book's reading flow.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct FlowManifest {
    /// Generator format version; bumping it invalidates cached artifacts.
    pub version: i32,
    /// `epub` or `txt`.
    pub format: String,
    /// Total UTF-16 code units of text in the book.
    pub total_chars: i64,
    /// Short content fingerprint of the book blob, used to isolate cached
    /// chunks on the client when a file id is reused for different content.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub book_key: String,
    /// One entry per chapter, in reading order.
    #[serde(default)]
    pub spines: Vec<SpineMeta>,
    /// One entry per chunk, in reading order.
    #[serde(default)]
    pub chunks: Vec<ChunkMeta>,
    /// Table of contents resolved onto the flow.
    #[serde(default, deserialize_with = "deserialize_nullable_vec")]
    pub toc: Vec<TocTarget>,
    /// Generation time, informational only.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub generated_at: String,
}

impl FlowManifest {
    /// Chapter index containing block `block`.
    ///
    /// Returns `0` when the book is empty or the block precedes every chapter.
    #[must_use]
    pub fn spine_for_block(&self, block: i32) -> i32 {
        let mut best: Option<usize> = None;
        for (index, spine) in self.spines.iter().enumerate() {
            if block >= spine.block_start {
                best = Some(index);
            }
        }
        match best {
            Some(index) => index as i32,
            None => 0,
        }
    }

    /// Chunk index containing block `block`, or `None` when no chunk does.
    #[must_use]
    pub fn chunk_for_block(&self, block: i32) -> Option<i32> {
        self.chunks
            .iter()
            .position(|chunk| {
                block >= chunk.block_start && block < chunk.block_start + chunk.block_count
            })
            .map(|index| index as i32)
    }

    /// UTF-16 code units of text before `chunk_index`.
    #[must_use]
    pub fn chars_before_chunk(&self, chunk_index: i32) -> i64 {
        let bound = chunk_index.max(0) as usize;
        self.chunks
            .iter()
            .take(bound)
            .map(|chunk| chunk.chars)
            .sum()
    }

    /// Total number of content blocks in the book.
    #[must_use]
    pub fn total_blocks(&self) -> i32 {
        self.spines.iter().map(|spine| spine.block_count).sum()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn anchor_round_trips_in_the_current_format() {
        let anchor = Anchor {
            spine: 1,
            block: 42,
            path: vec![0, 3],
            offset: 7,
        };
        let json = serde_json::to_value(&anchor).unwrap();
        assert_eq!(
            json,
            serde_json::json!({"spine": 1, "block": 42, "path": [0, 3], "offset": 7})
        );
        assert_eq!(serde_json::from_value::<Anchor>(json).unwrap(), anchor);
    }

    #[test]
    fn anchor_omits_an_empty_path() {
        let anchor = Anchor {
            spine: 0,
            block: 3,
            path: Vec::new(),
            offset: 0,
        };
        let json = serde_json::to_value(&anchor).unwrap();
        assert_eq!(
            json,
            serde_json::json!({"spine": 0, "block": 3, "offset": 0})
        );
    }

    #[test]
    fn legacy_anchors_are_migrated_on_read() {
        // Old shape: path[0] was the chapter-local block number.
        let legacy = serde_json::json!({"spine": 2, "path": [5, 1, 0], "offset": 12});
        let anchor: Anchor = serde_json::from_value(legacy).unwrap();
        assert_eq!(
            anchor,
            Anchor {
                spine: 2,
                block: 5,
                path: vec![1, 0],
                offset: 12
            }
        );
    }

    #[test]
    fn legacy_anchors_with_an_empty_path_start_at_block_zero() {
        let legacy = serde_json::json!({"spine": 1, "path": [], "offset": -1});
        let anchor: Anchor = serde_json::from_value(legacy).unwrap();
        assert_eq!(
            anchor,
            Anchor {
                spine: 1,
                block: 0,
                path: Vec::new(),
                offset: -1
            }
        );
    }

    #[test]
    fn a_missing_offset_defaults_to_the_boundary_sentinel() {
        let legacy = serde_json::json!({"spine": 0, "path": [4]});
        let anchor: Anchor = serde_json::from_value(legacy).unwrap();
        assert_eq!(anchor.offset, Anchor::BOUNDARY_OFFSET);
        assert_eq!(anchor.block, 4);
    }

    #[test]
    fn validity_bounds_every_field() {
        assert!(
            Anchor {
                spine: 0,
                block: 0,
                path: vec![0],
                offset: -1
            }
            .is_valid()
        );
        assert!(
            Anchor {
                spine: Anchor::MAX_SPINE - 1,
                block: Anchor::MAX_BLOCK - 1,
                path: Vec::new(),
                offset: Anchor::MAX_BLOCK - 1
            }
            .is_valid()
        );
        assert!(
            !Anchor {
                spine: -1,
                ..Anchor::default()
            }
            .is_valid()
        );
        assert!(
            !Anchor {
                spine: Anchor::MAX_SPINE,
                ..Anchor::default()
            }
            .is_valid()
        );
        assert!(
            !Anchor {
                block: -1,
                ..Anchor::default()
            }
            .is_valid()
        );
        assert!(
            !Anchor {
                block: Anchor::MAX_BLOCK,
                ..Anchor::default()
            }
            .is_valid()
        );
        assert!(
            !Anchor {
                path: vec![-1],
                ..Anchor::default()
            }
            .is_valid()
        );
        assert!(
            !Anchor {
                path: vec![Anchor::MAX_SPINE],
                ..Anchor::default()
            }
            .is_valid()
        );
        assert!(
            !Anchor {
                path: vec![0; Anchor::MAX_PATH + 1],
                ..Anchor::default()
            }
            .is_valid()
        );
        assert!(
            !Anchor {
                offset: -2,
                ..Anchor::default()
            }
            .is_valid()
        );
        assert!(
            !Anchor {
                offset: Anchor::MAX_BLOCK,
                ..Anchor::default()
            }
            .is_valid()
        );
    }

    #[test]
    fn anchors_order_by_spine_then_block_then_path_then_offset() {
        let base = Anchor {
            spine: 0,
            block: 0,
            path: vec![0],
            offset: 0,
        };
        let mut sorted = [
            Anchor {
                spine: 1,
                ..base.clone()
            },
            Anchor {
                block: 1,
                ..base.clone()
            },
            Anchor {
                offset: 1,
                ..base.clone()
            },
            Anchor {
                path: vec![0, 0],
                ..base.clone()
            },
            base.clone(),
        ];
        sorted.sort();
        assert_eq!(sorted[0], base);
        assert_eq!(
            sorted[1],
            Anchor {
                offset: 1,
                ..base.clone()
            }
        );
        assert_eq!(
            sorted[2],
            Anchor {
                path: vec![0, 0],
                ..base.clone()
            }
        );
        assert_eq!(
            sorted[3],
            Anchor {
                block: 1,
                ..base.clone()
            }
        );
        assert_eq!(sorted[4], Anchor { spine: 1, ..base });
    }

    fn manifest() -> FlowManifest {
        FlowManifest {
            version: 4,
            format: "epub".into(),
            total_chars: 300,
            book_key: "abc".into(),
            spines: vec![
                SpineMeta {
                    block_start: 0,
                    block_count: 2,
                },
                SpineMeta {
                    block_start: 2,
                    block_count: 3,
                },
            ],
            chunks: vec![
                ChunkMeta {
                    index: 0,
                    block_start: 0,
                    block_count: 2,
                    chars: 100,
                    bytes: 200,
                    url: "c/0".into(),
                },
                ChunkMeta {
                    index: 1,
                    block_start: 2,
                    block_count: 3,
                    chars: 200,
                    bytes: 0,
                    url: String::new(),
                },
            ],
            toc: vec![TocTarget {
                label: "One".into(),
                depth: 0,
                spine: 0,
                block: 0,
                chunk: 0,
                ..TocTarget::default()
            }],
            generated_at: String::new(),
        }
    }

    #[test]
    fn manifest_json_matches_the_historical_shape() {
        let json = serde_json::to_value(manifest()).unwrap();
        let chunk = &json["chunks"][1];
        // bytes/url are omitted when zero/empty.
        assert!(chunk.get("bytes").is_none());
        assert!(chunk.get("url").is_none());
        assert_eq!(
            json["spines"][1],
            serde_json::json!({"block_start": 2, "block_count": 3})
        );
        assert!(json.get("generated_at").is_none());
        // `chunk` on a TOC target is always emitted, even when zero.
        assert_eq!(json["toc"][0]["chunk"], 0);
    }

    #[test]
    fn block_lookups_work_across_chapters() {
        let manifest = manifest();
        assert_eq!(manifest.spine_for_block(0), 0);
        assert_eq!(manifest.spine_for_block(1), 0);
        assert_eq!(manifest.spine_for_block(2), 1);
        assert_eq!(manifest.spine_for_block(4), 1);
        // Past the end, the last chapter still owns the block.
        assert_eq!(manifest.spine_for_block(99), 1);
        assert_eq!(manifest.total_blocks(), 5);
    }

    #[test]
    fn chunk_lookups_and_char_accumulation() {
        let manifest = manifest();
        assert_eq!(manifest.chunk_for_block(0), Some(0));
        assert_eq!(manifest.chunk_for_block(1), Some(0));
        assert_eq!(manifest.chunk_for_block(2), Some(1));
        assert_eq!(manifest.chunk_for_block(4), Some(1));
        assert_eq!(manifest.chunk_for_block(5), None);
        assert_eq!(manifest.chars_before_chunk(0), 0);
        assert_eq!(manifest.chars_before_chunk(1), 100);
        assert_eq!(manifest.chars_before_chunk(99), 300);
    }

    #[test]
    fn toc_entries_omit_empty_source_information() {
        let entry = TocEntry {
            label: "Chapter".into(),
            path: String::new(),
            fragment: String::new(),
            offset: 0,
            depth: 1,
        };
        let json = serde_json::to_value(&entry).unwrap();
        assert_eq!(json, serde_json::json!({"label": "Chapter", "depth": 1}));
    }

    #[test]
    fn flow_manifest_treats_nullable_toc_as_empty() {
        let manifest: FlowManifest = serde_json::from_value(serde_json::json!({
            "version": 4,
            "format": "epub",
            "total_chars": 0,
            "spines": [],
            "chunks": [],
            "toc": null
        }))
        .unwrap();
        assert!(manifest.toc.is_empty());

        let invalid = serde_json::from_value::<FlowManifest>(serde_json::json!({
            "version": 4,
            "format": "epub",
            "total_chars": 0,
            "spines": [],
            "chunks": [],
            "toc": 42
        }));
        assert!(invalid.is_err());
    }
}

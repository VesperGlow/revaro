//! Book parsing and sanitisation for the Revaro reader.
//!
//! This crate is the parsing half of the Go `internal/reader` package: it turns
//! an uploaded `.epub` or `.txt` file into an immutable [`Book`]. The reading
//! flow generator is in [`flow`], and consumes these types directly without
//! re-reading the source file:
//!
//! * [`Book::chapters`] holds one already-sanitized XHTML fragment per spine
//!   item, in reading order, each carrying its `data-source-path` marker.
//! * [`Book::assets`] holds the deduplicated embedded images that the cleaned
//!   HTML refers to by index.
//! * [`Book::toc`] uses [`revaro_core::reader::TocEntry`], the shared model the
//!   flow builder resolves onto global block numbers.
//! * `flow::build` turns that parsed book into deterministic, cacheable HTML
//!   chunks and a shared flow manifest.
//!
//! ## Trust model
//!
//! An EPUB is user-supplied input. Two rules follow from that:
//!
//! 1. **The sanitiser is a whitelist.** Only a fixed set of tags and attributes
//!    is re-emitted; `<script>`, `<style>`, event handlers (`on*`), `style`,
//!    `align` and every `src`/`srcset` are dropped, and `href` survives only
//!    when it is not a `javascript:`, `vbscript:` or `data:` URL. There is no
//!    "known bad" blacklist to bypass.
//! 2. **Decompression is budgeted.** The archive is capped at
//!    [`MAX_EPUB`] compressed, [`MAX_ARCHIVE_ENTRIES`] entries,
//!    [`MAX_DECOMPRESSED_ENTRY`] per entry and [`MAX_DECOMPRESSED_TOTAL`] in
//!    total, and the cleaned HTML is capped at [`MAX_RENDERED_HTML`]. Both the
//!    declared and the actually-read sizes are checked, so a zip bomb is
//!    rejected instead of allocated.
//!
//! ## Determinism
//!
//! The Go original iterated Go maps while choosing the EPUB navigation document
//! and the cover image, so two parses of the same file could disagree. That
//! contradicts the reading-flow invariant that "the same book always produces
//! the same artifacts". Every choice here is made in OPF manifest declaration
//! order (see [`Book`]), and a regression test parses the same archive twice
//! and compares the output byte for byte.
//!
//! ## Attribute escaping
//!
//! Go emitted `data-source-path` and `data-frag-ids` with `fmt.Fprintf("%q")`,
//! which is *not* HTML escaping: a `"` in an archive path or element id could
//! break out of the attribute and inject markup. This port HTML-escapes both
//! values with the same mapping as Go's `html.EscapeString`.

#![forbid(unsafe_code)]

mod archive;
mod budget;
mod cache;
mod dom;
mod epub;
pub mod flow;
mod image;
mod model;
mod path;
mod sanitize;
mod text;

use std::io::{Read, Seek};

pub use cache::BookCache;
pub use model::{Asset, Book, Chapter, Format};
pub use path::{asset_content_type, normalize_path};
pub use revaro_core::reader::TocEntry;

/// Largest plain-text file the reader accepts, in bytes.
///
/// TXT is decoded entirely into memory, so the cap is much smaller than the
/// EPUB one.
pub const MAX_TXT: i64 = 16 << 20;

/// Largest EPUB source file the reader accepts, in bytes.
///
/// The cap is on the *compressed* archive; the decompressed budgets below are
/// what actually bound memory.
pub const MAX_EPUB: i64 = 128 << 20;

/// Total bytes the parser may decompress across all zip entries.
///
/// A 128 MiB archive can expand to many times its size, so parsing without this
/// cap would let a zip bomb exhaust memory.
pub const MAX_DECOMPRESSED_TOTAL: i64 = 256 << 20;

/// Largest single zip entry the parser will decompress, in bytes.
pub const MAX_DECOMPRESSED_ENTRY: i64 = 64 << 20;

/// Whole-book cap on the cleaned chapter HTML, in bytes.
///
/// The budget is shared across every spine item, matching the Go behaviour, so
/// many small chapters cannot add up past the cap.
pub const MAX_RENDERED_HTML: usize = 64 << 20;

/// Largest number of zip entries (and of manifest/spine entries) accepted.
pub const MAX_ARCHIVE_ENTRIES: usize = 10_000;

/// Failure modes of [`parse`].
///
/// Messages mirror the Go originals where the server surfaced them, so the HTTP
/// layer can keep returning the same text.
#[derive(Debug, thiserror::Error)]
pub enum ReaderError {
    /// The source exceeds [`MAX_EPUB`].
    #[error("EPUB 超过 {0} MiB 限制")]
    EpubTooLarge(i64),
    /// The source exceeds [`MAX_TXT`].
    #[error("文本文件超过 {0} MiB 限制，请下载后离线阅读")]
    TxtTooLarge(i64),
    /// The archive has no usable central directory.
    #[error("EPUB 不是有效的 zip: {0}")]
    InvalidZip(String),
    /// The archive (or the OPF manifest/spine) has more than
    /// [`MAX_ARCHIVE_ENTRIES`] entries.
    #[error("EPUB 条目数量超过 {0} 上限")]
    TooManyEntries(usize),
    /// `META-INF/container.xml` could not be read.
    #[error("读取 EPUB 容器失败: {0}")]
    Container(String),
    /// `container.xml` has no `rootfile` with a `full-path`.
    #[error("EPUB 缺少 container rootfile")]
    MissingRootfile,
    /// The OPF package document could not be read.
    #[error("读取 OPF 失败: {0}")]
    OpfRead(String),
    /// The OPF package document is not well-formed XML.
    #[error("解析 OPF 失败: {0}")]
    OpfParse(String),
    /// The OPF declares an implausible number of manifest or spine entries.
    #[error("EPUB 清单或书脊条目过多")]
    TooManyManifestItems,
    /// A named zip entry does not exist.
    #[error("EPUB 缺少文件：{0}")]
    MissingEntry(String),
    /// A single zip entry expands past [`MAX_DECOMPRESSED_ENTRY`].
    #[error("EPUB 条目超过 {0} MiB 上限")]
    EntryTooLarge(i64),
    /// The archive expands past [`MAX_DECOMPRESSED_TOTAL`].
    #[error("EPUB 解压后内容超过 {0} MiB 上限")]
    DecompressedTooLarge(i64),
    /// The cleaned HTML exceeds [`MAX_RENDERED_HTML`].
    #[error("EPUB 渲染正文超过 {0} MiB 上限")]
    RenderedTooLarge(i64),
    /// The spine produced no readable chapter at all.
    #[error("EPUB 中没有可阅读内容")]
    NoReadableContent,
    /// An I/O error while reading an entry.
    #[error("读取 EPUB 失败: {0}")]
    Io(#[from] std::io::Error),
}

/// Parse a book by file name.
///
/// `name` selects the format by extension exactly like Go's `path.Ext` did:
/// `.epub` (case-insensitive) is parsed as an EPUB, everything else as plain
/// text. `size` is the source length, checked against [`MAX_EPUB`] /
/// [`MAX_TXT`] *before* any allocation; the reader still enforces the real
/// limits while reading, so a wrong `size` is not a way in.
///
/// `asset_base_url` is the URL prefix rewritten images are published under (for
/// example `/api/files/<id>/book/assets`), and `etag` is appended as `?v=…` so
/// the asset URL changes whenever the book content does and can be cached
/// immutably.
///
/// The returned [`Book`] is immutable and cheap to share (wrap it in an
/// [`std::sync::Arc`] for the [`BookCache`]).
pub fn parse<R: Read + Seek>(
    name: &str,
    source: R,
    size: i64,
    asset_base_url: &str,
    etag: &str,
) -> Result<Book, ReaderError> {
    if is_epub_name(name) {
        if size > MAX_EPUB {
            return Err(ReaderError::EpubTooLarge(MAX_EPUB >> 20));
        }
        epub::parse(source, asset_base_url, etag)
    } else {
        if size > MAX_TXT {
            return Err(ReaderError::TxtTooLarge(MAX_TXT >> 20));
        }
        text::parse(source)
    }
}

/// True when a file name has the `.epub` extension.
///
/// Mirrors Go's `path.Ext` (the suffix after the last dot in the final path
/// segment), which unlike [`std::path::Path::extension`] also treats a leading
/// dot as the start of an extension.
fn is_epub_name(name: &str) -> bool {
    path::file_extension(name).eq_ignore_ascii_case(".epub")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    #[test]
    fn oversized_sources_are_refused_before_reading() {
        let error = parse(
            "book.epub",
            Cursor::new(Vec::new()),
            MAX_EPUB + 1,
            "/assets",
            "v",
        )
        .unwrap_err();
        assert!(matches!(error, ReaderError::EpubTooLarge(128)), "{error}");
        let error = parse("book.txt", Cursor::new(Vec::new()), MAX_TXT + 1, "", "").unwrap_err();
        assert!(matches!(error, ReaderError::TxtTooLarge(16)), "{error}");
    }

    #[test]
    fn extension_detection_matches_go_path_ext() {
        assert!(is_epub_name("book.epub"));
        assert!(is_epub_name("BOOK.EPUB"));
        assert!(is_epub_name("dir.v2/book.EPUB"));
        assert!(is_epub_name(".epub"));
        assert!(!is_epub_name("book.txt"));
        assert!(!is_epub_name("book"));
        assert!(!is_epub_name("book.epub.bak"));
        assert!(!is_epub_name("dir.epub/book"));
    }
}

//! The parsed, immutable content model of one file.

use revaro_core::reader::TocEntry;

/// The container format of a parsed book.
///
/// Kept as an enum rather than Go's bare `string` so the flow builder cannot
/// accidentally compare against a typo; [`Format::as_str`] recovers the wire
/// spelling used by the flow manifest.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Format {
    /// An EPUB, parsed into [`Book::chapters`].
    Epub,
    /// A plain-text file, parsed into [`Book::text`].
    #[default]
    Txt,
}

impl Format {
    /// The `epub` / `txt` spelling shared with the HTTP and flow contracts.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Format::Epub => "epub",
            Format::Txt => "txt",
        }
    }
}

/// One embedded image extracted from an EPUB.
///
/// Images are deduplicated by archive path, so two chapters referring to the
/// same file share one asset. The index in [`Book::assets`] is the number in
/// the rewritten `src` URL.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Asset {
    /// Raw image bytes, exactly as stored in the archive.
    pub data: Vec<u8>,
    /// MIME type derived from the archive path's extension.
    pub content_type: String,
    /// Intrinsic width in pixels, `0` when the format was not recognised.
    pub width: u32,
    /// Intrinsic height in pixels, `0` when the format was not recognised.
    pub height: u32,
}

/// One cleaned spine item.
///
/// The HTML is already sanitized: it is safe to insert into the reader DOM
/// without running any further filtering, and every block carries a
/// `data-source-path` attribute so the flow builder can match TOC entries
/// without re-parsing the source.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Chapter {
    /// Sanitized inner HTML of the source `<body>`.
    pub html: String,
    /// Canonical archive path of the spine item (`OEBPS/ch1.xhtml`), the same
    /// value written into `data-source-path`.
    pub source_path: String,
}

/// A parsed book. Immutable once returned from [`crate::parse`].
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Book {
    /// Source format.
    pub format: Format,
    /// EPUB `<dc:title>`, or `未命名书籍` when the OPF has none. Empty for TXT,
    /// where the caller already knows the file name.
    pub title: String,
    /// Cleaned spine items in reading order. Empty for TXT.
    pub chapters: Vec<Chapter>,
    /// Decoded full text. Empty for EPUB. Offsets in [`Book::toc`] are UTF-16
    /// code units into this string, matching JavaScript's `String#slice`.
    pub text: String,
    /// EPUB navigation (nav document or NCX) or TXT chapter headings.
    pub toc: Vec<TocEntry>,
    /// Raw cover image bytes, empty when the book has no cover.
    pub cover: Vec<u8>,
    /// Lower-case extension of the cover (`png`, `jpg`, …), empty with no cover.
    pub cover_ext: String,
    /// Embedded images, addressed by their index in this vector.
    pub assets: Vec<Asset>,
}

impl Book {
    /// Total in-memory size of the parsed content, in bytes.
    ///
    /// [`BookCache`](crate::BookCache) uses this for LRU accounting. It is a
    /// pure function of the fields (the Go original memoised it with a data
    /// race), so it is safe to call from any thread.
    #[must_use]
    pub fn byte_size(&self) -> i64 {
        let chapters: usize = self.chapters.iter().map(|chapter| chapter.html.len()).sum();
        let assets: usize = self.assets.iter().map(|asset| asset.data.len()).sum();
        let total = self
            .text
            .len()
            .saturating_add(self.cover.len())
            .saturating_add(chapters)
            .saturating_add(assets);
        i64::try_from(total).unwrap_or(i64::MAX)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn byte_size_counts_every_payload() {
        let book = Book {
            format: Format::Epub,
            title: "t".into(),
            chapters: vec![
                Chapter {
                    html: "abc".into(),
                    source_path: "a.xhtml".into(),
                },
                Chapter {
                    html: "de".into(),
                    source_path: "b.xhtml".into(),
                },
            ],
            text: String::new(),
            toc: Vec::new(),
            cover: vec![0; 4],
            cover_ext: "png".into(),
            assets: vec![Asset {
                data: vec![0; 7],
                content_type: "image/png".into(),
                width: 1,
                height: 1,
            }],
        };
        // Title and source paths are metadata, not content.
        assert_eq!(book.byte_size(), 3 + 2 + 4 + 7);
    }

    #[test]
    fn format_spellings_match_the_wire_contract() {
        assert_eq!(Format::Epub.as_str(), "epub");
        assert_eq!(Format::Txt.as_str(), "txt");
    }
}

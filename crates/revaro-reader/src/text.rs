//! Plain-text book parsing: encoding detection and chapter headings.
//!
//! TXT has no structure to speak of, so the reader does the minimum: decode the
//! bytes, then recognise `第…章` / `Chapter N` headings as table-of-contents
//! entries. Offsets are UTF-16 code units, not bytes, because the browser
//! slices the same string with JavaScript semantics.

use std::io::Read;
use std::sync::LazyLock;

use regex::Regex;

use crate::{Book, Format, MAX_TXT, ReaderError, TocEntry};

/// Chapter-heading detector, ported from Go's `txtChapterRe`.
///
/// Two deliberate differences from a naive translation:
///
/// * `\d` is spelled `[0-9]`, because Go's RE2 `\d` is ASCII-only while Rust's
///   `regex` defaults to Unicode digits.
/// * The ideographic space (`U+3000`) is written as `\u{3000}` so the indentation
///   rule also covers full-width text.
static CHAPTER_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?im)^[ \t\u{3000}]{0,4}(第[零〇一二三四五六七八九十百千万两0-9]+[章节卷部回篇][^\n]{0,40}|(?:chapter|part)[ \t]+[ivxlcdmIVXLCDM0-9]+[^\n]{0,40})[ \t]*$",
    )
    .expect("the chapter regex is a constant and must compile")
});

/// Maximum number of TXT TOC entries, matching the Go cap.
const MAX_TXT_TOC: usize = 500;

/// Parse a plain-text book.
///
/// Valid UTF-8 wins; otherwise the bytes are decoded as GBK, which is what most
/// Chinese-language TXT files on disk actually are. Invalid sequences decode to
/// `U+FFFD` rather than failing, matching Go's `simplifiedchinese.GBK` decoder.
pub(crate) fn parse<R: Read>(mut source: R) -> Result<Book, ReaderError> {
    let mut data = Vec::new();
    source
        .by_ref()
        .take(MAX_TXT as u64 + 1)
        .read_to_end(&mut data)?;
    if data.len() as i64 > MAX_TXT {
        return Err(ReaderError::TxtTooLarge(MAX_TXT >> 20));
    }
    let text = match std::str::from_utf8(&data) {
        Ok(text) => text.to_string(),
        Err(_) => encoding_rs::GBK.decode(&data).0.into_owned(),
    };
    let toc = extract_toc(&text);
    Ok(Book {
        format: Format::Txt,
        text,
        toc,
        ..Book::default()
    })
}

/// Collect chapter headings with UTF-16 offsets, capped at [`MAX_TXT_TOC`].
///
/// The offset reported for a heading is the number of UTF-16 code units before
/// it, so the client can `text.slice(offset)` its way to the chapter.
#[must_use]
pub(crate) fn extract_toc(text: &str) -> Vec<TocEntry> {
    let mut entries: Vec<TocEntry> = Vec::new();
    let mut cursor = 0usize;
    let mut utf16_offset = 0i64;
    for captures in CHAPTER_RE.captures_iter(text).take(MAX_TXT_TOC) {
        let Some(whole) = captures.get(0) else {
            continue;
        };
        let start = whole.start();
        // Headings arrive in order, so the offset advances incrementally rather
        // than rescanning the prefix for every match.
        utf16_offset += utf16_len(&text[cursor..start]);
        cursor = start;
        let Some(label) = captures.get(1) else {
            continue;
        };
        entries.push(TocEntry {
            label: label.as_str().trim().to_string(),
            offset: utf16_offset,
            ..TocEntry::default()
        });
    }
    entries
}

/// Length of `text` in UTF-16 code units (JavaScript `String#length`).
#[must_use]
pub(crate) fn utf16_len(text: &str) -> i64 {
    text.chars()
        .map(|character| i64::from(character.len_utf16() as u32))
        .sum()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    fn parse_text(text: &str) -> Book {
        parse(Cursor::new(text.as_bytes().to_vec())).unwrap()
    }

    #[test]
    fn parse_txt_toc_and_offsets() {
        let text = "第一章 开始\n正文一行\n第二章 继续\n";
        let book = parse_text(text);
        assert_eq!(book.format, Format::Txt);
        assert_eq!(book.text, text);
        assert_eq!(book.toc.len(), 2);
        assert_eq!(book.toc[0].label, "第一章 开始");
        assert_eq!(book.toc[0].offset, 0);
        // The second offset is the UTF-16 length of the preceding two lines.
        assert_eq!(book.toc[1].offset, utf16_len("第一章 开始\n正文一行\n"));
        // The TXT parser never fabricates EPUB fields.
        assert!(book.chapters.is_empty());
        assert!(book.cover.is_empty());
    }

    #[test]
    fn txt_gbk_is_decoded() {
        let text = "第一章 测试\n内容行";
        let (encoded, _, had_errors) = encoding_rs::GBK.encode(text);
        assert!(!had_errors);
        let book = parse(Cursor::new(encoded.into_owned())).unwrap();
        assert_eq!(book.text, text);
        assert_eq!(book.toc.len(), 1);
        assert_eq!(book.toc[0].label, "第一章 测试");
    }

    #[test]
    fn chapter_heading_variants_are_recognised() {
        for heading in [
            "第一章 开始",
            "第1节 引言",
            "  第三卷 风起",
            "Chapter 1 The Beginning",
            "PART IV",
            "第一回 楔子",
            "第一部 序",
        ] {
            let text = format!("{heading}\nbody\n");
            let book = parse_text(&text);
            assert_eq!(
                book.toc.len(),
                1,
                "heading {heading:?} produced {:?}",
                book.toc
            );
            assert_eq!(book.toc[0].label, heading.trim());
        }
        // A space between 第 and the numeral does not match, matching Go's RE2.
        assert!(parse_text("第 1 节 引言\nbody\n").toc.is_empty());
        // Nor does a heading without the 第…章 shape.
        assert!(parse_text("序章\nbody\n").toc.is_empty());
    }

    #[test]
    fn toc_offsets_count_surrogate_pairs_as_two_units() {
        // The emoji is one scalar but two UTF-16 code units.
        let text = "😀\n第一章 开始\n";
        let book = parse_text(text);
        assert_eq!(book.toc.len(), 1);
        assert_eq!(book.toc[0].offset, utf16_len("😀\n"));
        assert_eq!(book.toc[0].offset, 3);
    }

    #[test]
    fn toc_is_capped_at_five_hundred_entries() {
        let mut text = String::new();
        for index in 0..600 {
            text.push_str(&format!("第{index}章 标题\n正文\n"));
        }
        let book = parse_text(&text);
        assert_eq!(book.toc.len(), MAX_TXT_TOC);
    }

    #[test]
    fn oversized_txt_is_refused() {
        let data = vec![b'a'; MAX_TXT as usize + 1];
        let error = parse(Cursor::new(data)).unwrap_err();
        assert!(matches!(error, ReaderError::TxtTooLarge(16)), "{error}");
    }
}

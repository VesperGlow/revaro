//! End-to-end tests over in-memory EPUBs.
//!
//! Fixtures are built with the `zip` writer instead of committed binaries, so
//! every case is readable in the test that uses it and no binary can drift out
//! of sync with the parser.

use std::io::{Cursor, Write};

use revaro_reader::{Book, Format, ReaderError, parse};
use zip::write::SimpleFileOptions;
use zip::{CompressionMethod, ZipWriter};

/// Minimal in-memory EPUB writer.
struct EpubBuilder {
    writer: ZipWriter<Cursor<Vec<u8>>>,
}

impl EpubBuilder {
    fn new() -> Self {
        Self {
            writer: ZipWriter::new(Cursor::new(Vec::new())),
        }
    }

    /// Add one deflated entry, matching Go's `zip.Writer` defaults.
    fn add(&mut self, name: &str, data: &[u8]) {
        let options = SimpleFileOptions::default().compression_method(CompressionMethod::Deflated);
        self.writer.start_file(name, options).unwrap();
        self.writer.write_all(data).unwrap();
    }

    /// Add one UTF-8 text entry.
    fn add_str(&mut self, name: &str, data: &str) {
        self.add(name, data.as_bytes());
    }

    fn finish(self) -> Vec<u8> {
        self.writer.finish().unwrap().into_inner()
    }
}

/// Minimal PNG header with the given dimensions.
fn fake_png(width: u32, height: u32) -> Vec<u8> {
    let mut data = vec![0u8; 33];
    data[..8].copy_from_slice(b"\x89PNG\r\n\x1a\n");
    data[8..12].copy_from_slice(&13u32.to_be_bytes());
    data[12..16].copy_from_slice(b"IHDR");
    data[16..20].copy_from_slice(&width.to_be_bytes());
    data[20..24].copy_from_slice(&height.to_be_bytes());
    data
}

/// Parse an EPUB fixture the way the server does.
fn parse_epub(bytes: &[u8]) -> Result<Book, ReaderError> {
    parse(
        "book.epub",
        Cursor::new(bytes.to_vec()),
        bytes.len() as i64,
        "/api/files/f1/book/assets",
        "test-etag",
    )
}

/// The Go `buildTestEPUB` fixture: container, OPF, nav TOC, one chapter with a
/// script/dangerous link/embedded image, and a cover.
fn build_test_epub() -> Vec<u8> {
    let mut epub = EpubBuilder::new();
    epub.add_str("mimetype", "application/epub+zip");
    epub.add_str(
        "META-INF/container.xml",
        r#"<?xml version="1.0"?><container version="1.0" xmlns="urn:oasis:names:tc:opendocument:xmlns:container"><rootfiles><rootfile full-path="OEBPS/content.opf" media-type="application/oebps-package+xml"/></rootfiles></container>"#,
    );
    epub.add_str(
        "OEBPS/content.opf",
        r#"<?xml version="1.0"?><package xmlns="http://www.idpf.org/2007/opf" version="3.0" unique-identifier="id"><metadata xmlns:dc="http://purl.org/dc/elements/1.1/"><dc:title>测试书</dc:title><dc:creator>作者甲</dc:creator><meta name="cover" content="cover-img"/></metadata><manifest><item id="nav" href="nav.xhtml" media-type="application/xhtml+xml" properties="nav"/><item id="ch1" href="ch1.xhtml" media-type="application/xhtml+xml"/><item id="cover-img" href="img/cover.png" media-type="image/png"/><item id="fig" href="img/fig.png" media-type="image/png"/></manifest><spine><itemref idref="ch1"/></spine></package>"#,
    );
    epub.add_str(
        "OEBPS/nav.xhtml",
        r#"<html xmlns="http://www.w3.org/1999/xhtml"><body><nav epub:type="toc"><ol><li><a href="ch1.xhtml#sec1">第一章</a></li></ol></nav></body></html>"#,
    );
    epub.add_str(
        "OEBPS/ch1.xhtml",
        r#"<html xmlns="http://www.w3.org/1999/xhtml"><body><h1 id="sec1">第一章 开始</h1><p>你好世界</p><script>alert(1)</script><p><a href="javascript:alert(2)">坏链接</a> <img src="img/fig.png" alt="插图"/></p></body></html>"#,
    );
    epub.add("OEBPS/img/cover.png", &fake_png(300, 400));
    epub.add("OEBPS/img/fig.png", &fake_png(10, 20));
    epub.finish()
}

#[test]
fn epub_pipeline_produces_title_toc_chapters_assets_and_cover() {
    let fixture = build_test_epub();
    let book = parse_epub(&fixture).unwrap();
    assert_eq!(book.format, Format::Epub);
    assert_eq!(book.title, "测试书");

    assert_eq!(book.toc.len(), 1, "toc={:?}", book.toc);
    let entry = &book.toc[0];
    assert_eq!(entry.label, "第一章");
    assert_eq!(entry.path, "OEBPS/ch1.xhtml");
    assert_eq!(entry.fragment, "sec1");
    assert_eq!(entry.depth, 0);

    assert_eq!(book.chapters.len(), 1);
    assert_eq!(book.chapters[0].source_path, "OEBPS/ch1.xhtml");
    let html = &book.chapters[0].html;
    for want in [
        "你好世界",
        "第一章 开始",
        "data-source-path=\"OEBPS/ch1.xhtml\"",
        "alt=\"插图\"",
    ] {
        assert!(html.contains(want), "html missing {want:?}:\n{html}");
    }
    for forbidden in ["<script", "javascript:", "alert"] {
        assert!(
            !html.contains(forbidden),
            "html must not contain {forbidden:?}:\n{html}"
        );
    }
    assert!(
        html.contains("/api/files/f1/book/assets/0?v=test-etag\" width=\"10\" height=\"20\""),
        "img not rewritten with dimensions and version:\n{html}"
    );

    assert_eq!(book.assets.len(), 1);
    assert_eq!((book.assets[0].width, book.assets[0].height), (10, 20));
    assert_eq!(book.assets[0].content_type, "image/png");
    assert!(!book.cover.is_empty());
    assert_eq!(book.cover_ext, "png");
}

#[test]
fn epub_rejects_a_decompression_bomb() {
    // A single entry whose decompressed size is over the per-entry cap must be
    // refused, not truncated and not allocated.
    let mut epub = EpubBuilder::new();
    epub.add_str("mimetype", "application/epub+zip");
    epub.add_str(
        "META-INF/container.xml",
        r#"<?xml version="1.0"?><container version="1.0" xmlns="urn:oasis:names:tc:opendocument:xmlns:container"><rootfiles><rootfile full-path="OEBPS/content.opf" media-type="application/oebps-package+xml"/></rootfiles></container>"#,
    );
    epub.add_str(
        "OEBPS/content.opf",
        r#"<?xml version="1.0"?><package xmlns="http://www.idpf.org/2007/opf" version="3.0"><metadata xmlns:dc="http://purl.org/dc/elements/1.1/"><dc:title>炸弹</dc:title></metadata><manifest><item id="ch1" href="ch1.xhtml" media-type="application/xhtml+xml"/></manifest><spine><itemref idref="ch1"/></spine></package>"#,
    );
    // One byte past the 64 MiB per-entry cap.
    let bomb = vec![b'A'; (64 << 20) + 1];
    epub.add("OEBPS/ch1.xhtml", &bomb);
    let bytes = epub.finish();
    let error = parse_epub(&bytes).unwrap_err();
    assert!(matches!(error, ReaderError::NoReadableContent), "{error}");
}

#[test]
fn epub_renders_nothing_when_there_is_no_readable_spine() {
    let mut epub = EpubBuilder::new();
    epub.add_str("mimetype", "application/epub+zip");
    epub.add_str(
        "META-INF/container.xml",
        r#"<?xml version="1.0"?><container><rootfiles><rootfile full-path="content.opf"/></rootfiles></container>"#,
    );
    epub.add_str(
        "content.opf",
        r#"<package><metadata><title>空</title></metadata><manifest><item id="img" href="a.png" media-type="image/png"/></manifest><spine><itemref idref="img"/></spine></package>"#,
    );
    let bytes = epub.finish();
    let error = parse_epub(&bytes).unwrap_err();
    assert!(matches!(error, ReaderError::NoReadableContent), "{error}");
}

#[test]
fn unreadable_archives_are_rejected() {
    let error = parse_epub(b"this is not a zip").unwrap_err();
    assert!(matches!(error, ReaderError::InvalidZip(_)), "{error}");
}

/// EPUB with two nav documents and two `cover-image` candidates, used to pin
/// deterministic selection.
fn build_ambiguous_epub() -> Vec<u8> {
    let mut epub = EpubBuilder::new();
    epub.add_str("mimetype", "application/epub+zip");
    epub.add_str(
        "META-INF/container.xml",
        r#"<?xml version="1.0"?><container><rootfiles><rootfile full-path="OEBPS/content.opf"/></rootfiles></container>"#,
    );
    epub.add_str(
        "OEBPS/content.opf",
        r#"<package xmlns="http://www.idpf.org/2007/opf"><metadata><title>双目录</title></metadata><manifest><item id="nav1" href="nav1.xhtml" media-type="application/xhtml+xml" properties="nav"/><item id="nav2" href="nav2.xhtml" media-type="application/xhtml+xml" properties="nav"/><item id="ch1" href="ch1.xhtml" media-type="application/xhtml+xml"/><item id="ch2" href="ch2.xhtml" media-type="application/xhtml+xml"/><item id="cover1" href="img/c1.png" media-type="image/png" properties="cover-image"/><item id="cover2" href="img/c2.png" media-type="image/png" properties="cover-image"/></manifest><spine><itemref idref="ch1"/><itemref idref="ch2"/></spine></package>"#,
    );
    epub.add_str(
        "OEBPS/nav1.xhtml",
        r#"<html><body><nav epub:type="toc"><ol><li><a href="ch1.xhtml#a">第一章 A</a></li></ol></nav></body></html>"#,
    );
    epub.add_str(
        "OEBPS/nav2.xhtml",
        r#"<html><body><nav epub:type="toc"><ol><li><a href="ch2.xhtml#b">第二章 B</a></li></ol></nav></body></html>"#,
    );
    epub.add_str("OEBPS/ch1.xhtml", "<html><body><p>甲</p></body></html>");
    epub.add_str("OEBPS/ch2.xhtml", "<html><body><p>乙</p></body></html>");
    epub.add("OEBPS/img/c1.png", &fake_png(10, 10));
    epub.add("OEBPS/img/c2.png", &fake_png(20, 20));
    epub.finish()
}

#[test]
fn epub_parsing_is_byte_deterministic() {
    let fixture = build_ambiguous_epub();
    let first = parse_epub(&fixture).unwrap();
    let second = parse_epub(&fixture).unwrap();
    // Whole-model equality already covers the chapter HTML byte for byte.
    assert_eq!(first, second, "the same book must produce identical output");

    // The first nav document and the first cover-image candidate in manifest
    // declaration order win. A map-order implementation would be free to pick
    // the other one on either run.
    assert_eq!(first.toc.len(), 1);
    assert_eq!(first.toc[0].label, "第一章 A");
    assert_eq!(first.toc[0].path, "OEBPS/ch1.xhtml");
    assert_eq!(first.cover, fake_png(10, 10));
    assert_eq!(first.cover_ext, "png");
    assert_eq!(first.chapters.len(), 2);
}

#[test]
fn attribute_values_in_paths_and_ids_are_html_escaped() {
    // A quote in the manifest href resolves to a quote in the chapter path, and
    // a quote in an element id reaches `data-frag-ids`. Go's `%q` formatting
    // wrote both raw, which let them break out of the attribute.
    let mut epub = EpubBuilder::new();
    epub.add_str("mimetype", "application/epub+zip");
    epub.add_str(
        "META-INF/container.xml",
        r#"<?xml version="1.0"?><container><rootfiles><rootfile full-path="OEBPS/content.opf"/></rootfiles></container>"#,
    );
    epub.add_str(
        "OEBPS/content.opf",
        r#"<package><metadata><title>转义</title></metadata><manifest><item id="ch1" href="ch&quot;1&lt;x&gt;.xhtml" media-type="application/xhtml+xml"/></manifest><spine><itemref idref="ch1"/></spine></package>"#,
    );
    epub.add_str(
        "OEBPS/ch\"1<x>.xhtml",
        r#"<html><body><p id='a"b<c'></p><p>正文</p></body></html>"#,
    );
    let bytes = epub.finish();
    let book = parse_epub(&bytes).unwrap();
    let html = &book.chapters[0].html;
    assert_eq!(book.chapters[0].source_path, "OEBPS/ch\"1<x>.xhtml");
    assert!(
        html.contains("data-source-path=\"OEBPS/ch&#34;1&lt;x&gt;.xhtml\""),
        "source path must be HTML-escaped:\n{html}"
    );
    assert!(
        html.contains("data-frag-ids=\"a&#34;b&lt;c\""),
        "fragment ids must be HTML-escaped:\n{html}"
    );
    // Neither value may terminate its own attribute.
    assert!(!html.contains("data-source-path=\"OEBPS/ch\"1<x>.xhtml\""));
    assert!(!html.contains("data-frag-ids=\"a\"b<c\""));
}

#[test]
fn ncx_navigation_is_used_when_no_nav_document_exists() {
    let mut epub = EpubBuilder::new();
    epub.add_str("mimetype", "application/epub+zip");
    epub.add_str(
        "META-INF/container.xml",
        r#"<?xml version="1.0"?><container><rootfiles><rootfile full-path="OEBPS/content.opf"/></rootfiles></container>"#,
    );
    epub.add_str(
        "OEBPS/content.opf",
        r#"<package><metadata><title>NCX</title></metadata><manifest><item id="ncx" href="toc.ncx" media-type="application/x-dtbncx+xml"/><item id="ch1" href="ch1.xhtml" media-type="application/xhtml+xml"/></manifest><spine toc="ncx"><itemref idref="ch1"/></spine></package>"#,
    );
    epub.add_str(
        "OEBPS/toc.ncx",
        r#"<?xml version="1.0"?><ncx><navMap><navPoint id="n1"><navLabel><text>第一章</text></navLabel><content src="ch1.xhtml#sec1"/><navPoint id="n2"><navLabel><text>第一节</text></navLabel><content src="ch1.xhtml#sec2"/></navPoint></navPoint><navPoint id="n3"><navLabel><text></text></navLabel><content src="ch1.xhtml"/></navPoint></navMap></ncx>"#,
    );
    epub.add_str("OEBPS/ch1.xhtml", "<html><body><p>正文</p></body></html>");
    let bytes = epub.finish();
    let book = parse_epub(&bytes).unwrap();
    assert_eq!(book.toc.len(), 3, "toc={:?}", book.toc);
    assert_eq!(book.toc[0].label, "第一章");
    assert_eq!(book.toc[0].path, "OEBPS/ch1.xhtml");
    assert_eq!(book.toc[0].fragment, "sec1");
    assert_eq!(book.toc[0].depth, 0);
    // Nested navPoints are emitted after their parent, one level deeper.
    assert_eq!(book.toc[1].label, "第一节");
    assert_eq!(book.toc[1].fragment, "sec2");
    assert_eq!(book.toc[1].depth, 1);
    // An empty label falls back to the neutral placeholder.
    assert_eq!(book.toc[2].label, "未命名章节");
    assert_eq!(book.toc[2].depth, 0);
}

#[test]
fn images_are_rewritten_or_dropped_by_source() {
    let mut epub = EpubBuilder::new();
    epub.add_str("mimetype", "application/epub+zip");
    epub.add_str(
        "META-INF/container.xml",
        r#"<?xml version="1.0"?><container><rootfiles><rootfile full-path="OEBPS/content.opf"/></rootfiles></container>"#,
    );
    epub.add_str(
        "OEBPS/content.opf",
        r#"<package><metadata><title>图片</title></metadata><manifest><item id="ch1" href="ch1.xhtml" media-type="application/xhtml+xml"/><item id="fig" href="img/fig.png" media-type="image/png"/></manifest><spine><itemref idref="ch1"/></spine></package>"#,
    );
    epub.add_str(
        "OEBPS/ch1.xhtml",
        r#"<html><body><p><img src="https://evil.test/x.png" alt="外部"/><img src="img/fig.png" alt="内部"/><img src="data:image/png;base64,AAAA" alt="内联"/></p><svg aria-label="矢量图"><image xlink:href="img/fig.png"/></svg><p><img src="img/missing.png"/></p></body></html>"#,
    );
    epub.add("OEBPS/img/fig.png", &fake_png(5, 6));
    let bytes = epub.finish();
    let book = parse_epub(&bytes).unwrap();
    let html = &book.chapters[0].html;
    assert!(
        !html.contains("evil.test"),
        "external image must be dropped:\n{html}"
    );
    assert!(
        !html.contains("data:image"),
        "inline image must be dropped:\n{html}"
    );
    // The internal image is referenced twice (img and svg) but stored once.
    assert_eq!(book.assets.len(), 1, "assets={:?}", book.assets.len());
    assert_eq!(html.matches("/api/files/f1/book/assets/0").count(), 2);
    // The SVG was rewritten to an <img> carrying the SVG's aria-label.
    assert!(html.contains("alt=\"矢量图\""), "{html}");
    assert!(!html.contains("<svg"), "{html}");
}

#[test]
fn txt_books_are_parsed_without_an_archive() {
    let text = "第一章 开始\n正文\n";
    let book = parse(
        "notes.txt",
        Cursor::new(text.as_bytes().to_vec()),
        text.len() as i64,
        "",
        "",
    )
    .unwrap();
    assert_eq!(book.format, Format::Txt);
    assert_eq!(book.text, text);
    assert_eq!(book.toc.len(), 1);
}

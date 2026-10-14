//! The server's EPUB pipeline must produce manifests the reader accepts.

use std::io::{Cursor, Write};

use revaro_core::reader::{FlowManifest, TocEntry};
use revaro_reader::{Book, Chapter, Format, flow, parse};
use revaro_web::logic::reader::validate_manifest;
use zip::{ZipWriter, write::SimpleFileOptions};

fn assert_addressable(manifest: &FlowManifest) {
    assert!(validate_manifest(manifest), "{manifest:#?}");
    assert!(manifest.chunks.iter().all(|chunk| chunk.block_count > 0));
    for target in &manifest.toc {
        let spine = &manifest.spines[target.spine as usize];
        assert!(target.block >= spine.block_start);
        assert!(target.block < spine.block_start + spine.block_count);
        assert_eq!(manifest.chunk_for_block(target.block), Some(target.chunk));
        assert_eq!(manifest.spine_for_block(target.block), target.spine);
    }
}

fn xml_script_epub() -> Vec<u8> {
    let mut archive = ZipWriter::new(Cursor::new(Vec::new()));
    let entries = [
        (
            "META-INF/container.xml",
            r#"<container><rootfiles><rootfile full-path="item/content.opf"/></rootfiles></container>"#,
        ),
        (
            "item/content.opf",
            r#"<package><metadata><title>XML script regression</title></metadata><manifest><item id="nav" href="nav.xhtml" media-type="application/xhtml+xml" properties="nav"/><item id="cover" href="xhtml/cover.xhtml" media-type="application/xhtml+xml"/><item id="empty" href="xhtml/empty.xhtml" media-type="application/xhtml+xml"/><item id="text" href="xhtml/text.xhtml" media-type="application/xhtml+xml"/><item id="end" href="xhtml/end.xhtml" media-type="application/xhtml+xml"/></manifest><spine><itemref idref="cover"/><itemref idref="empty"/><itemref idref="text"/><itemref idref="end"/></spine></package>"#,
        ),
        (
            "item/nav.xhtml",
            r#"<?xml version="1.0"?><html xmlns="http://www.w3.org/1999/xhtml" xmlns:epub="http://www.idpf.org/2007/ops"><head><script type="text/javascript" src="kobo.js"/></head><body><nav epub:type="toc"><ol><li><a href="xhtml/cover.xhtml">Cover</a></li><li><a href="xhtml/empty.xhtml">Empty</a></li><li><a href="xhtml/text.xhtml#chapter">Text</a></li><li><a href="xhtml/end.xhtml">End</a></li></ol></nav></body></html>"#,
        ),
        (
            "item/xhtml/cover.xhtml",
            r#"<?xml version="1.0"?><html xmlns="http://www.w3.org/1999/xhtml"><head><script type="text/javascript" src="../../js/kobo.js"/><style type="text/css">.koboSpan { color: red; }</style></head><body><div><svg xmlns="http://www.w3.org/2000/svg" xmlns:xlink="http://www.w3.org/1999/xlink" viewBox="0 0 5 6"><image width="5" height="6" xlink:href="../image/cover.png"/></svg></div></body></html>"#,
        ),
        (
            "item/xhtml/empty.xhtml",
            r#"<html><head><script src="kobo.js"/></head><body><p id="empty"> </p></body></html>"#,
        ),
        (
            "item/xhtml/text.xhtml",
            r#"<html><head><script src="kobo.js"/></head><body><h1 id="chapter">正文😀</h1><p>下一段</p></body></html>"#,
        ),
        (
            "item/xhtml/end.xhtml",
            r#"<html><head><script src="kobo.js"/></head><body><script>discard()</script></body></html>"#,
        ),
    ];
    for (name, text) in entries {
        archive
            .start_file(name, SimpleFileOptions::default())
            .unwrap();
        archive.write_all(text.as_bytes()).unwrap();
    }
    let mut png = vec![0; 33];
    png[..8].copy_from_slice(b"\x89PNG\r\n\x1a\n");
    png[12..16].copy_from_slice(b"IHDR");
    png[16..20].copy_from_slice(&5u32.to_be_bytes());
    png[20..24].copy_from_slice(&6u32.to_be_bytes());
    archive
        .start_file("item/image/cover.png", SimpleFileOptions::default())
        .unwrap();
    archive.write_all(&png).unwrap();
    archive.finish().unwrap().into_inner()
}

#[test]
fn xhtml_self_closing_scripts_keep_svg_images_text_and_nav_targets() {
    let bytes = xml_script_epub();
    let book = parse(
        "regression.epub",
        Cursor::new(&bytes),
        bytes.len() as i64,
        "/assets",
        "v",
    )
    .unwrap();
    assert_eq!(
        book.toc.len(),
        4,
        "the nav document also contains an XML script"
    );
    assert_eq!(book.assets.len(), 1);
    assert!(book.chapters[0].html.contains(r#"src="/assets/0?v=v""#));
    assert!(!book.chapters[0].html.contains("<svg"));
    assert!(book.chapters[2].html.contains("正文😀"));
    assert!(book.chapters[1].html.is_empty());
    assert!(book.chapters[3].html.is_empty());

    let built = flow::build(&book).unwrap();
    assert_addressable(&built.manifest);
    assert_eq!(built.manifest.total_chars, 7);
    assert_eq!(
        built
            .manifest
            .spines
            .iter()
            .map(|s| s.block_count)
            .collect::<Vec<_>>(),
        vec![1, 1, 2, 1]
    );
    assert_eq!(
        built
            .manifest
            .toc
            .iter()
            .map(|t| t.block)
            .collect::<Vec<_>>(),
        vec![0, 1, 2, 4]
    );
    assert_eq!(built.manifest.toc[0].nav_anchor, "rvn-0");
    assert_eq!(built.manifest.toc[2].text_path, vec![0]);
    let html = built
        .chunks
        .iter()
        .map(|chunk| chunk.html.as_str())
        .collect::<String>();
    assert!(html.contains(r#"data-rv-anchor="rvn-0""#));
    assert!(!html.contains("script"));
    assert!(!html.contains("discard()"));
    for block in 0..5 {
        assert_eq!(html.matches(&format!(r#"data-block="{block}""#)).count(), 1);
    }
    assert_eq!(flow::build(&book).unwrap(), built);

    // SVG image-only books have zero text but still need a real TOC target.
    let image_only = Book {
        chapters: vec![book.chapters[0].clone()],
        toc: vec![book.toc[0].clone()],
        ..book
    };
    let built = flow::build(&image_only).unwrap();
    assert_addressable(&built.manifest);
    assert_eq!(built.manifest.total_chars, 0);
    assert_eq!(built.manifest.total_blocks(), 1);
    assert_eq!(built.manifest.toc[0].chunk, 0);
    assert!(built.chunks[0].html.contains(r#"data-rv-anchor="rvn-0""#));
}

#[test]
fn empty_spines_keep_their_own_blocks_and_toc_chunk_indices() {
    for content in ["".to_owned(), format!("<p>{}</p>", "字".repeat(14_001))] {
        let book = Book {
            format: Format::Epub,
            chapters: ["start.xhtml", "text.xhtml", "end.xhtml", "last.xhtml"]
                .into_iter()
                .map(|path| Chapter {
                    source_path: path.into(),
                    html: if path == "text.xhtml" {
                        content.clone()
                    } else {
                        String::new()
                    },
                })
                .collect(),
            toc: ["start.xhtml", "text.xhtml", "end.xhtml", "last.xhtml"]
                .into_iter()
                .map(|path| TocEntry {
                    path: path.into(),
                    ..TocEntry::default()
                })
                .collect(),
            ..Book::default()
        };
        let built = flow::build(&book).unwrap();
        assert_addressable(&built.manifest);
        assert_eq!(built.manifest.total_blocks(), 4);
        assert_eq!(
            built
                .manifest
                .toc
                .iter()
                .map(|t| t.block)
                .collect::<Vec<_>>(),
            vec![0, 1, 2, 3]
        );
        assert_eq!(
            built.manifest.total_chars,
            if content.is_empty() { 0 } else { 14_001 }
        );
        if !content.is_empty() {
            assert_eq!(built.manifest.toc[2].chunk, 1);
            assert_eq!(built.manifest.toc[3].chunk, 1);
        }
    }
}

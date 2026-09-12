//! EPUB-internal path handling and asset MIME inference.
//!
//! Every path that comes out of an archive is run through
//! [`normalize_path`], which collapses `.`/`..` without ever letting a path
//! escape the archive root. The flow builder matches TOC entries to chapter
//! source paths using the same canonical form, so there is exactly one spelling
//! of any given path.

/// Return the extension of a file name, including the leading dot, or `""`.
///
/// This mirrors Go's `path.Ext`, not [`std::path::Path::extension`]: a leading
/// dot counts as the start of an extension (`".epub"` → `".epub"`), and the
/// search stops at the last `/`.
#[must_use]
pub(crate) fn file_extension(name: &str) -> &str {
    let bytes = name.as_bytes();
    let mut index = bytes.len();
    while index > 0 {
        let byte = bytes[index - 1];
        if byte == b'/' {
            break;
        }
        if byte == b'.' {
            return &name[index - 1..];
        }
        index -= 1;
    }
    ""
}

/// Canonicalize an archive-internal path.
///
/// Empty segments and `.` are dropped, `..` pops the previous segment, and a
/// leading `/` is stripped, so the result is always relative to the archive
/// root. Pop past the root is ignored rather than escaping it: a crafted OPF
/// cannot address a file outside the book.
#[must_use]
pub fn normalize_path(path: &str) -> String {
    let trimmed = path.strip_prefix('/').unwrap_or(path);
    let mut parts: Vec<&str> = Vec::new();
    for part in trimmed.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            other => parts.push(other),
        }
    }
    parts.join("/")
}

/// Resolve `relative` against the directory of `base_file`.
///
/// The query string and fragment are dropped, the remaining path is
/// percent-decoded once, and the result is normalized. Absolute paths
/// (leading `/`) are treated as archive-root relative, matching the EPUB spec.
#[must_use]
pub(crate) fn resolve_path(base_file: &str, relative: &str) -> String {
    let head = match relative.find(['#', '?']) {
        Some(index) => &relative[..index],
        None => relative,
    };
    let clean = decode_path(head);
    if clean.is_empty() {
        return normalize_path(base_file);
    }
    let mut parts: Vec<String> = Vec::new();
    if !clean.starts_with('/') {
        let base = normalize_path(base_file);
        if !base.is_empty() {
            let mut segments: Vec<&str> = base.split('/').collect();
            segments.pop();
            parts.extend(segments.into_iter().map(str::to_string));
        }
    }
    parts.extend(clean.split('/').map(str::to_string));
    normalize_path(&parts.join("/"))
}

/// Return the percent-decoded fragment of an href, or `""`.
#[must_use]
pub(crate) fn fragment_of(href: &str) -> String {
    match href.find('#') {
        Some(index) => decode_path(&href[index + 1..]),
        None => String::new(),
    }
}

/// Percent-decode a path.
///
/// Malformed escapes make the whole value fall back to the raw input, exactly
/// like Go's `url.PathUnescape` error path. `+` is *not* decoded to a space:
/// that is form encoding, not path encoding.
fn decode_path(path: &str) -> String {
    let bytes = path.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' {
            let (Some(high), Some(low)) = (
                index
                    .checked_add(1)
                    .and_then(|i| bytes.get(i))
                    .and_then(|b| hex_value(*b)),
                index
                    .checked_add(2)
                    .and_then(|i| bytes.get(i))
                    .and_then(|b| hex_value(*b)),
            ) else {
                return path.to_string();
            };
            out.push(high * 16 + low);
            index += 3;
            continue;
        }
        out.push(bytes[index]);
        index += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Hex digit value, or `None` when the byte is not `[0-9A-Fa-f]`.
fn hex_value(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

/// Lower-case image extension for a path, falling back to `img`.
///
/// The fallback keeps the asset URL stable for exotic or missing extensions,
/// and the 5-character ASCII check stops absurd extensions from reaching the
/// MIME table or the `?v=` query.
#[must_use]
pub(crate) fn ext_of(path: &str) -> String {
    let ext = file_extension(path).to_lowercase();
    let ext = ext.strip_prefix('.').unwrap_or(&ext);
    if !ext.is_empty()
        && ext.len() <= 5
        && ext
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit())
    {
        return ext.to_string();
    }
    "img".to_string()
}

/// MIME type for an embedded asset extension.
///
/// Unknown extensions map to `application/octet-stream`, which the sanitiser
/// treats as "not an image" and drops; SVG is refused separately because the
/// sanitizer cannot make an SVG document safe.
#[must_use]
pub fn asset_content_type(ext: &str) -> &'static str {
    match ext {
        "jpg" | "jpeg" => "image/jpeg",
        "png" => "image/png",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "svg" => "image/svg+xml",
        "avif" => "image/avif",
        "bmp" => "image/bmp",
        _ => "application/octet-stream",
    }
}

/// Percent-encode a value for use in a query string, matching Go's
/// `url.QueryEscape`: unreserved bytes survive, spaces become `+`, and every
/// other byte is `%XX` with upper-case hex.
#[must_use]
pub(crate) fn query_escape(value: &str) -> String {
    const HEX: &[u8; 16] = b"0123456789ABCDEF";
    let mut out = String::with_capacity(value.len());
    for &byte in value.as_bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(byte as char);
            }
            b' ' => out.push('+'),
            _ => {
                out.push('%');
                out.push(HEX[(byte >> 4) as usize] as char);
                out.push(HEX[(byte & 0x0F) as usize] as char);
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_path_uses_one_epub_canonical_form() {
        for (input, want) in [
            (
                "/OEBPS/./chapters/../text/ch1.xhtml",
                "OEBPS/text/ch1.xhtml",
            ),
            ("../../OEBPS//text/ch1.xhtml", "OEBPS/text/ch1.xhtml"),
            ("./", ""),
            ("", ""),
            ("..", ""),
            ("a/b/../../c", "c"),
        ] {
            assert_eq!(normalize_path(input), want, "normalize_path({input:?})");
        }
    }

    #[test]
    fn resolve_path_is_relative_to_the_base_directory() {
        for (base, relative, want) in [
            ("OEBPS/content.opf", "ch1.xhtml", "OEBPS/ch1.xhtml"),
            ("OEBPS/content.opf", "../img/a.png", "img/a.png"),
            ("OEBPS/ch1.xhtml", "img/fig.png", "OEBPS/img/fig.png"),
            ("OEBPS/ch1.xhtml", "ch1.xhtml#frag", "OEBPS/ch1.xhtml"),
            ("OEBPS/nav.xhtml", "ch1.xhtml?q=1#f", "OEBPS/ch1.xhtml"),
            ("a/b/c.xhtml", "../../d.png", "d.png"),
            ("ch1.xhtml", "/absolute.png", "absolute.png"),
            ("OEBPS/content.opf", "", "OEBPS/content.opf"),
            (
                "OEBPS/content.opf",
                "ch%201%23x.xhtml",
                "OEBPS/ch 1#x.xhtml",
            ),
        ] {
            assert_eq!(
                resolve_path(base, relative),
                want,
                "resolve_path({base:?}, {relative:?})"
            );
        }
    }

    #[test]
    fn fragments_are_percent_decoded() {
        assert_eq!(fragment_of("ch1.xhtml#sec%201"), "sec 1");
        assert_eq!(fragment_of("ch1.xhtml"), "");
        assert_eq!(fragment_of("#a%2Fb"), "a/b");
        // A malformed escape falls back to the raw text.
        assert_eq!(fragment_of("#a%zz"), "a%zz");
    }

    #[test]
    fn extensions_are_lower_cased_and_bounded() {
        assert_eq!(ext_of("OEBPS/img/FIG.PNG"), "png");
        assert_eq!(ext_of("a.jpeg"), "jpeg");
        assert_eq!(ext_of("noext"), "img");
        assert_eq!(ext_of("a.verylongext"), "img");
        assert_eq!(ext_of("a.!"), "img");
        assert_eq!(ext_of("a."), "img");
    }

    #[test]
    fn content_types_cover_the_reader_set() {
        assert_eq!(asset_content_type("jpg"), "image/jpeg");
        assert_eq!(asset_content_type("jpeg"), "image/jpeg");
        assert_eq!(asset_content_type("png"), "image/png");
        assert_eq!(asset_content_type("gif"), "image/gif");
        assert_eq!(asset_content_type("webp"), "image/webp");
        assert_eq!(asset_content_type("svg"), "image/svg+xml");
        assert_eq!(asset_content_type("avif"), "image/avif");
        assert_eq!(asset_content_type("bmp"), "image/bmp");
        assert_eq!(asset_content_type("tiff"), "application/octet-stream");
    }

    #[test]
    fn query_escaping_matches_go_query_escape() {
        assert_eq!(query_escape("test-etag"), "test-etag");
        assert_eq!(query_escape("a b"), "a+b");
        assert_eq!(query_escape("a/b?c=d&e"), "a%2Fb%3Fc%3Dd%26e");
        assert_eq!(query_escape("版本"), "%E7%89%88%E6%9C%AC");
    }
}

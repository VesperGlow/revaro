//! Small, target-independent rules for restoring the file-browser route.

use revaro_core::classify::LibraryKind;

/// Resolve a browser pathname to a folder id.
///
/// Only the file-browser route is accepted here. A malformed or unrelated
/// path falls back to the virtual root so a stale bookmark cannot make the
/// client issue a request for an arbitrary nested path.
#[must_use]
pub fn folder_id(pathname: &str, root_id: &str) -> String {
    let Some(rest) = pathname.strip_prefix("/f/") else {
        return root_id.to_owned();
    };
    let trimmed = rest.trim_end_matches('/');
    if trimmed.is_empty() || trimmed.contains('/') || trimmed == "." || trimmed == ".." {
        return root_id.to_owned();
    }
    trimmed.to_owned()
}

/// Resolve a single-segment reader deep link.
#[must_use]
pub fn reader_id(pathname: &str) -> Option<String> {
    let rest = pathname.strip_prefix("/read/")?;
    let trimmed = rest.trim_end_matches('/');
    if trimmed.is_empty() || trimmed.contains('/') || trimmed == "." || trimmed == ".." {
        return None;
    }
    Some(trimmed.to_owned())
}

/// Build the canonical URL for a folder.
#[must_use]
pub fn folder_url(id: &str, root_id: &str) -> String {
    if id == root_id {
        "/".to_owned()
    } else {
        format!("/f/{id}")
    }
}

/// Resolve a media-library pathname and its optional folder filter.
#[must_use]
pub fn library_route(pathname: &str) -> Option<(LibraryKind, Option<String>)> {
    let rest = pathname.strip_prefix("/library/")?.trim_end_matches('/');
    let mut parts = rest.split('/');
    let kind = parts.next()?.parse().ok()?;
    let folder = match (parts.next(), parts.next(), parts.next()) {
        (None, None, None) => None,
        (Some("f"), Some(id), None) if !id.is_empty() && id != "." && id != ".." => {
            Some(id.to_owned())
        }
        _ => return None,
    };
    Some((kind, folder))
}

/// Build the canonical media-library URL.
#[must_use]
pub fn library_url(kind: LibraryKind, folder_id: Option<&str>) -> String {
    match folder_id {
        Some(folder_id) if !folder_id.is_empty() => {
            format!("/library/{}/f/{}", kind.as_str(), js_url_encode(folder_id))
        }
        _ => format!("/library/{}", kind.as_str()),
    }
}

fn js_url_encode(value: &str) -> String {
    // Folder ids are UUID-like in the product, so replacing the path
    // separators explicitly is enough while keeping this pure and wasm-free.
    value
        .replace('%', "%25")
        .replace('/', "%2F")
        .replace('?', "%3F")
}

#[cfg(test)]
mod tests {
    use super::*;

    const ROOT: &str = "root";

    #[test]
    fn restores_only_single_segment_folder_routes() {
        assert_eq!(folder_id("/", ROOT), ROOT);
        assert_eq!(folder_id("/f/folder", ROOT), "folder");
        assert_eq!(folder_id("/f/folder/", ROOT), "folder");
    }

    #[test]
    fn unrelated_or_traversal_paths_return_to_root() {
        for path in ["/read/book", "/f/", "/f/a/b", "/f/../secret", "/f/."] {
            assert_eq!(folder_id(path, ROOT), ROOT, "{path} must not be restored");
        }
    }

    #[test]
    fn restores_only_single_segment_reader_routes() {
        assert_eq!(reader_id("/read/book"), Some("book".to_owned()));
        assert_eq!(reader_id("/read/book/"), Some("book".to_owned()));
        for path in ["/", "/read/", "/read/a/b", "/read/..", "/read/."] {
            assert_eq!(reader_id(path), None, "{path} must not be restored");
        }
    }

    #[test]
    fn root_has_the_short_url() {
        assert_eq!(folder_url(ROOT, ROOT), "/");
        assert_eq!(folder_url("folder", ROOT), "/f/folder");
    }

    #[test]
    fn parses_library_routes_and_rejects_extra_segments() {
        assert_eq!(
            library_route("/library/image"),
            Some((LibraryKind::Image, None))
        );
        assert_eq!(
            library_route("/library/audio/f/folder"),
            Some((LibraryKind::Audio, Some("folder".to_owned())))
        );
        assert_eq!(library_route("/library/file/f/../secret"), None);
        assert_eq!(library_route("/library/nope"), None);
        assert_eq!(
            library_url(LibraryKind::Image, Some("folder")),
            "/library/image/f/folder"
        );
    }
}

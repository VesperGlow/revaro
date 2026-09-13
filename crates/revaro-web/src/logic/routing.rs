//! Small, target-independent rules for restoring the file-browser route.

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
}

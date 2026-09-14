//! Pure rules shared by the browser upload queue.
//!
//! The DOM-facing queue stays in the wasm component, while path handling and
//! progress arithmetic live here so they can be tested on every target. In
//! particular, a browser supplied relative path is treated as untrusted input
//! before it is used to create any server-side directory.

use std::collections::BTreeSet;

use revaro_core::limits;

/// Split and validate a browser `webkitRelativePath`.
///
/// Backslashes are accepted because some browsers and operating systems expose
/// them in directory selections. Absolute paths, empty components and dot
/// components are rejected so a folder upload can never escape its chosen
/// destination or silently change its structure.
pub fn relative_path_parts(raw: &str) -> Result<Vec<String>, &'static str> {
    let normalized = raw.replace('\\', "/");
    if normalized.is_empty() || normalized.starts_with('/') || normalized.ends_with('/') {
        return Err("文件夹中包含无效路径");
    }

    let parts: Vec<String> = normalized.split('/').map(ToOwned::to_owned).collect();
    if parts
        .iter()
        .any(|part| part.is_empty() || part == "." || part == "..")
    {
        return Err("文件夹中包含无效路径");
    }
    if parts
        .iter()
        .any(|part| revaro_core::validate::validate_name(part).is_err())
    {
        return Err("文件夹中包含无法保存的名称");
    }
    Ok(parts)
}

/// Return the directory paths required by a set of relative file paths.
///
/// Paths are sorted parent-first and then lexicographically. That makes the
/// directory creation order deterministic and means a later file can only be
/// queued after its complete parent chain exists.
pub fn directory_paths(paths: &[Vec<String>]) -> Vec<String> {
    let mut unique = BTreeSet::new();
    for parts in paths {
        for depth in 1..parts.len() {
            unique.insert(parts[..depth].join("/"));
        }
    }
    let mut paths: Vec<String> = unique.into_iter().collect();
    paths.sort_by_key(|path| (path.matches('/').count(), path.clone()));
    paths
}

/// Progress while bytes are being transferred, leaving the reference's
/// acknowledgement and commit headroom after the rounded file percentage.
#[must_use]
pub fn transfer_progress(done: i64, total: i64) -> u8 {
    if total <= 0 {
        return 98;
    }
    let done = done.clamp(0, total) as f64;
    let percent = ((done / total as f64) * 100.0).round().min(99.0);
    (percent * 0.98).floor().clamp(0.0, 98.0) as u8
}

/// The exact byte size expected for a numbered part.
#[must_use]
pub fn part_size(total: i64, size: i64, number: usize) -> Option<i64> {
    if number == 0 || size <= 0 {
        return None;
    }
    let count = limits::multipart_part_count(total, size).ok()?;
    if number > count {
        return None;
    }
    if number < count {
        Some(size)
    } else {
        Some(total - size * (count as i64 - 1))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn relative_paths_normalize_separators_and_keep_the_file_name() {
        assert_eq!(
            relative_path_parts(r"Books\fiction\story.epub").unwrap(),
            ["Books", "fiction", "story.epub"]
        );
    }

    #[test]
    fn relative_paths_reject_escape_and_malformed_components() {
        for path in [
            "../secret.txt",
            "folder/../../secret.txt",
            "/absolute.txt",
            "folder//file.txt",
            "folder/./file.txt",
            "folder/../file.txt",
            "folder/",
        ] {
            assert!(
                relative_path_parts(path).is_err(),
                "{path} should be rejected"
            );
        }
    }

    #[test]
    fn directory_paths_are_unique_and_parent_first() {
        let paths = vec![
            vec!["books".into(), "fiction".into(), "a.epub".into()],
            vec!["books".into(), "nonfiction".into(), "b.txt".into()],
            vec!["books".into(), "fiction".into(), "c.epub".into()],
        ];
        assert_eq!(
            directory_paths(&paths),
            ["books", "books/fiction", "books/nonfiction"]
        );
    }

    #[test]
    fn progress_is_bounded_and_reserves_commit_space() {
        assert_eq!(transfer_progress(0, 100), 0);
        assert_eq!(transfer_progress(50, 100), 49);
        assert_eq!(transfer_progress(1, 3), 32);
        assert_eq!(transfer_progress(1, 7), 13);
        assert_eq!(transfer_progress(100, 100), 97);
        assert_eq!(transfer_progress(1000, 100), 97);
        assert_eq!(transfer_progress(0, 0), 98);
    }

    #[test]
    fn multipart_part_sizes_match_the_server_boundary() {
        assert_eq!(part_size(33, 16, 1), Some(16));
        assert_eq!(part_size(33, 16, 2), Some(16));
        assert_eq!(part_size(33, 16, 3), Some(1));
        assert_eq!(part_size(33, 16, 4), None);
        assert_eq!(part_size(0, 16, 1), None);
    }
}

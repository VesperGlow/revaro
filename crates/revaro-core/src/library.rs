//! Library aggregation: the rules behind the sidebar's five buckets.
//!
//! Ported from `internal/server/library.go`. Two pieces of logic live here
//! because they are pure and easy to get subtly wrong:
//!
//! * **Counting.** `file` counts *every* ready file, and a file additionally
//!   counts in exactly one media bucket. The buckets are tested in a fixed
//!   order (book, image, video, audio) because a `.txt` that also looks like an
//!   image must land somewhere deterministic.
//! * **Folder paths.** Each library entry is annotated with the directory path
//!   it lives under, resolved from the root. The directory graph is a tree in
//!   practice but nothing in the schema enforces that, so resolution detects
//!   cycles instead of recursing forever on corrupt data.
//!
//! Keeping them in the shared crate means the browser can compute the same
//! counts from a cached listing without a second implementation.

use std::collections::{BTreeMap, HashMap, HashSet};

use crate::classify::{self, LibraryKind};
use crate::ids::ROOT_ID;
use crate::model::{File, FolderRef, LibraryCounts, LibraryItem};

/// Per-bucket entry counts.
///
/// Mirrors the Go `libraryTypeCounts`: `file` is the total of ready files, not
/// "everything that matched no other bucket". A video therefore increments both
/// `video` and `file`.
#[must_use]
pub fn bucket_counts(files: &[File]) -> LibraryCounts {
    let mut counts = LibraryCounts::default();
    for file in files {
        // Go's caller filtered to ready, non-deleted rows in SQL; this helper
        // accepts any slice, so it applies the same filter itself.
        if !file.is_ready_file() || file.is_trashed() {
            continue;
        }
        counts.file += 1;
        // First match wins, in the same order as the Go switch.
        for kind in [
            LibraryKind::Book,
            LibraryKind::Image,
            LibraryKind::Video,
            LibraryKind::Audio,
        ] {
            if classify::matches_library_kind(file, kind) {
                match kind {
                    LibraryKind::Book => counts.book += 1,
                    LibraryKind::Image => counts.image += 1,
                    LibraryKind::Video => counts.video += 1,
                    LibraryKind::Audio => counts.audio += 1,
                    LibraryKind::File => {}
                }
                break;
            }
        }
    }
    counts
}

/// Resolve the directory path of every directory, keyed by directory id.
///
/// Each path runs from just below the virtual root down to the directory
/// itself, so the root is never part of a path and a direct child of the root
/// has a one-element path. Directories that are unreachable (a missing parent,
/// or a cycle in corrupt data) get an empty path rather than an error: a broken
/// branch must not break the whole library view.
///
/// Results are cached per directory while resolving, matching the Go
/// implementation, so a deep tree is walked once rather than once per leaf.
#[must_use]
pub fn folder_paths(directories: &[File]) -> BTreeMap<String, Vec<FolderRef>> {
    // id -> (parent, name); later rows win, matching Go's map assignment.
    let mut index: HashMap<&str, (Option<&str>, &str)> = HashMap::new();
    for directory in directories {
        if directory.kind != crate::model::FileKind::Directory || directory.is_trashed() {
            continue;
        }
        index.insert(
            directory.id.as_str(),
            (directory.parent_id.as_deref(), directory.name.as_str()),
        );
    }

    let mut resolved: BTreeMap<String, Vec<FolderRef>> = BTreeMap::new();
    // Resolve in declaration order so the cache warms the same way as Go.
    for directory in directories {
        if !index.contains_key(directory.id.as_str()) {
            continue;
        }
        resolve_path(directory.id.as_str(), &index, &mut resolved);
    }
    resolved
}

/// Resolve one directory's path, caching every ancestor on the way back up.
fn resolve_path(
    start: &str,
    index: &HashMap<&str, (Option<&str>, &str)>,
    resolved: &mut BTreeMap<String, Vec<FolderRef>>,
) -> Vec<FolderRef> {
    // Walk up until we hit a cached ancestor, the root, or a broken link.
    let mut chain: Vec<&str> = Vec::new();
    let mut visited: HashSet<&str> = HashSet::new();
    let mut current = Some(start);
    let mut base: Vec<FolderRef> = Vec::new();
    while let Some(node) = current {
        if let Some(cached) = resolved.get(node) {
            base = cached.clone();
            break;
        }
        if node == ROOT_ID {
            break;
        }
        if !visited.insert(node) {
            // A cycle in the data; stop rather than loop forever.
            break;
        }
        let Some((parent, _)) = index.get(node) else {
            break;
        };
        chain.push(node);
        current = *parent;
    }

    let mut accumulated = base;
    for node in chain.into_iter().rev() {
        let Some((_, name)) = index.get(node) else {
            continue;
        };
        accumulated.push(FolderRef {
            id: node.to_owned(),
            name: (*name).to_owned(),
        });
        resolved.insert(node.to_owned(), accumulated.clone());
    }
    accumulated
}

/// Build one library entry: the file, where it lives, and how long it plays.
#[must_use]
pub fn item(
    file: &File,
    paths: &BTreeMap<String, Vec<FolderRef>>,
    durations: &HashMap<String, i64>,
) -> LibraryItem {
    let folder_path = file
        .parent_id
        .as_ref()
        .and_then(|parent| paths.get(parent).cloned())
        .unwrap_or_default();
    LibraryItem {
        file: file.clone(),
        folder_path,
        duration_ms: durations.get(&file.id).copied().unwrap_or(0),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Timestamp;
    use crate::model::{FileKind, FileStatus};

    fn directory(id: &str, parent: Option<&str>, name: &str) -> File {
        File {
            id: id.to_owned(),
            parent_id: parent.map(str::to_owned),
            name: name.to_owned(),
            kind: FileKind::Directory,
            status: FileStatus::Ready,
            ..File::default()
        }
    }

    fn file(id: &str, parent: Option<&str>, name: &str, mime: &str) -> File {
        File {
            id: id.to_owned(),
            parent_id: parent.map(str::to_owned),
            name: name.to_owned(),
            kind: FileKind::File,
            status: FileStatus::Ready,
            mime_type: mime.to_owned(),
            ..File::default()
        }
    }

    #[test]
    fn every_ready_file_counts_as_a_file_and_in_one_media_bucket() {
        let files = vec![
            file("1", None, "novel.epub", ""),
            file("2", None, "photo.png", "image/png"),
            file("3", None, "clip.mp4", "video/mp4"),
            file("4", None, "song.flac", "audio/flac"),
            file("5", None, "notes.txt", ""),
            file("6", None, "data.bin", "application/octet-stream"),
        ];
        let counts = bucket_counts(&files);
        // `file` is the total, not the leftovers: six ready files.
        assert_eq!(counts.file, 6);
        // `.epub` and `.txt` are both books, so two entries land in `book`.
        assert_eq!(counts.book, 2);
        assert_eq!(counts.image, 1);
        assert_eq!(counts.video, 1);
        assert_eq!(counts.audio, 1);
    }

    #[test]
    fn a_text_file_counts_as_a_book_not_a_file_only() {
        // `.txt` is both an editable document and a readable book; the Go switch
        // checks the book case first, so it must land in `book`.
        let files = vec![file("1", None, "chapter.txt", "text/plain; charset=utf-8")];
        let counts = bucket_counts(&files);
        assert_eq!(counts.book, 1);
        assert_eq!(counts.file, 1);
        assert_eq!(counts.image + counts.video + counts.audio, 0);
    }

    #[test]
    fn book_classification_wins_over_other_buckets() {
        // A `.txt` whose stored MIME type is an image would otherwise match two
        // buckets; the fixed order must make the result deterministic.
        let files = vec![file("1", None, "weird.txt", "image/png")];
        let counts = bucket_counts(&files);
        assert_eq!(counts.book, 1);
        assert_eq!(counts.image, 0);
    }

    #[test]
    fn non_ready_and_directory_rows_are_not_counted() {
        let mut pending = file("1", None, "photo.png", "image/png");
        pending.status = FileStatus::Pending;
        let mut trashed = file("2", None, "photo.png", "image/png");
        trashed.deleted_at = Some(Timestamp::epoch());
        let files = vec![pending, trashed, directory("d", None, "photos")];
        let counts = bucket_counts(&files);
        assert_eq!(counts, LibraryCounts::default());
    }

    #[test]
    fn folder_paths_run_from_the_root_down_to_the_directory() {
        let directories = vec![
            directory(ROOT_ID, None, ""),
            directory("movies", Some(ROOT_ID), "Movies"),
            directory("scifi", Some("movies"), "Sci-Fi"),
        ];
        let paths = folder_paths(&directories);
        // The root itself is never part of a path.
        assert!(!paths.contains_key(ROOT_ID));
        assert_eq!(paths["movies"].len(), 1);
        assert_eq!(paths["movies"][0].name, "Movies");
        assert_eq!(paths["scifi"].len(), 2);
        assert_eq!(paths["scifi"][0].id, "movies");
        assert_eq!(paths["scifi"][1].id, "scifi");
    }

    #[test]
    fn a_directory_whose_parent_is_missing_still_paths_to_itself() {
        // This looks wrong but is exactly what the Go implementation does: a
        // missing parent contributes an empty prefix rather than suppressing the
        // directory, so the entry is still reachable in the library view.
        let directories = vec![directory("orphan", Some("gone"), "Orphan")];
        let paths = folder_paths(&directories);
        assert_eq!(paths["orphan"].len(), 1);
        assert_eq!(paths["orphan"][0].id, "orphan");
    }

    #[test]
    fn a_cycle_terminates_instead_of_hanging() {
        // The schema does not forbid a cycle; corrupt data must not hang the
        // library view.
        let directories = vec![
            directory("a", Some("b"), "A"),
            directory("b", Some("a"), "B"),
        ];
        let paths = folder_paths(&directories);
        assert!(paths.contains_key("a"));
        assert!(paths.contains_key("b"));
        // Each path is bounded by the number of directories.
        assert!(paths["a"].len() <= 2);
        assert!(paths["b"].len() <= 2);
    }

    #[test]
    fn self_parenting_directory_terminates() {
        let directories = vec![directory("loop", Some("loop"), "Loop")];
        let paths = folder_paths(&directories);
        assert!(paths["loop"].len() <= 1);
    }

    #[test]
    fn trashed_directories_are_not_indexed() {
        let mut gone = directory("gone", Some(ROOT_ID), "Gone");
        gone.deleted_at = Some(Timestamp::epoch());
        let directories = vec![gone, directory("child", Some("gone"), "Child")];
        let paths = folder_paths(&directories);
        assert!(!paths.contains_key("gone"));
        // The child is still indexed; its trashed parent simply contributes no
        // prefix, matching the Go behaviour.
        assert_eq!(paths["child"].len(), 1);
        assert_eq!(paths["child"][0].id, "child");
    }

    #[test]
    fn items_carry_their_folder_path_and_duration() {
        let directories = vec![
            directory(ROOT_ID, None, ""),
            directory("movies", Some(ROOT_ID), "Movies"),
        ];
        let paths = folder_paths(&directories);
        let mut durations = HashMap::new();
        durations.insert("clip".to_owned(), 1234);

        let entry = item(
            &file("clip", Some("movies"), "clip.mp4", "video/mp4"),
            &paths,
            &durations,
        );
        assert_eq!(entry.folder_path.len(), 1);
        assert_eq!(entry.folder_path[0].name, "Movies");
        assert_eq!(entry.duration_ms, 1234);

        // A file directly in the root has an empty path, not a missing one.
        let root_entry = item(&file("x", Some(ROOT_ID), "x.bin", ""), &paths, &durations);
        assert!(root_entry.folder_path.is_empty());
        assert_eq!(root_entry.duration_ms, 0);
    }

    #[test]
    fn folder_paths_serialize_as_an_array_even_when_empty() {
        let entry = item(
            &file("x", Some(ROOT_ID), "x.bin", ""),
            &BTreeMap::new(),
            &HashMap::new(),
        );
        let json = serde_json::to_value(&entry).unwrap();
        assert_eq!(json["folder_path"], serde_json::json!([]));
        // duration_ms is omitted when zero, matching the historical tags.
        assert!(json.get("duration_ms").is_none());
    }
}

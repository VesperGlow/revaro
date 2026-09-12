//! Object keys inside the local object store.
//!
//! The on-disk layout is part of the product's compatibility surface and must
//! not change: `APP_DATA_DIR/objects/` holds
//!
//! ```text
//! blobs/<file-uuid>                       original file contents
//! thumbs/<aa>/<rest>.jpg                  generated thumbnails and audio covers
//! flows/<blob-key>/f<version>/manifest.json
//! flows/<blob-key>/f<version>/chunks/<n>.html
//! profile/avatar                          the administrator avatar
//! .multipart/<upload-id>/<sha256(key)>    in-progress multipart upload parts
//! ```
//!
//! These helpers are shared so the server, the media engine and the reader all
//! derive the same keys from the same inputs.

/// Key of the administrator avatar object.
pub const AVATAR_KEY: &str = "profile/avatar";

/// Prefix under which reader reading-flow artifacts live.
pub const FLOW_ROOT: &str = "flows/";

/// Prefix under which generated thumbnails live.
pub const THUMBNAIL_ROOT: &str = "thumbs/";

/// Prefix under which in-progress multipart uploads live.
pub const MULTIPART_ROOT: &str = ".multipart/";

/// Prefix under which original file contents live.
pub const BLOB_ROOT: &str = "blobs/";

/// Key of an original file's contents.
#[must_use]
pub fn blob_key(file_id: &str) -> String {
    format!("{BLOB_ROOT}{file_id}")
}

/// Recover the file id from a blob key, when the key is one.
#[must_use]
pub fn blob_id(key: &str) -> Option<&str> {
    let id = key.strip_prefix(BLOB_ROOT)?;
    if id.is_empty() || id.contains('/') {
        None
    } else {
        Some(id)
    }
}

/// Key of a thumbnail or generated audio cover.
///
/// Thumbnails are sharded by the first two characters so a large library does
/// not put tens of thousands of entries in a single directory. Returns `None`
/// for identifiers too short to shard, which cannot occur for real files.
#[must_use]
pub fn thumbnail_key(file_id: &str) -> Option<String> {
    if file_id.len() < 3 {
        return None;
    }
    let (prefix, rest) = file_id.split_at(2);
    Some(format!("{THUMBNAIL_ROOT}{prefix}/{rest}.jpg"))
}

/// Directory holding an in-progress multipart upload.
///
/// The upload id scopes the directory and the object key is hashed inside it,
/// so an upload can never write outside its own sandbox even if the key is
/// attacker-influenced.
#[must_use]
pub fn multipart_dir(upload_id: &str, object_key: &str) -> String {
    use std::fmt::Write as _;

    let digest = sha256_hex(object_key.as_bytes());
    let mut out = String::with_capacity(MULTIPART_ROOT.len() + upload_id.len() + digest.len() + 1);
    let _ = write!(out, "{MULTIPART_ROOT}{upload_id}/{digest}");
    out
}

/// Where the reader stores a reading flow's manifest.
#[must_use]
pub fn flow_manifest_key(book_object_key: &str, version: u32) -> String {
    format!("{}{}", flow_dir(book_object_key, version), "manifest.json")
}

/// Where the reader stores one reading flow chunk.
#[must_use]
pub fn flow_chunk_key(book_object_key: &str, version: u32, index: u32) -> String {
    format!("{}{index}.html", flow_chunk_dir(book_object_key, version))
}

/// The `flows/<book>/f<version>/` directory.
#[must_use]
pub fn flow_dir(book_object_key: &str, version: u32) -> String {
    format!("{FLOW_ROOT}{book_object_key}/f{version}/")
}

/// The `flows/<book>/f<version>/chunks/` directory.
#[must_use]
pub fn flow_chunk_dir(book_object_key: &str, version: u32) -> String {
    format!("{FLOW_ROOT}{book_object_key}/f{version}/chunks/")
}

/// Recover the book's blob key from a flow object key.
///
/// Flow keys look like `flows/blobs/<uuid>/f3/...`, so the first two path
/// segments after the prefix identify the book. Returns `None` for keys that do
/// not belong to a flow.
#[must_use]
pub fn flow_book_key(flow_key: &str) -> Option<String> {
    let rest = flow_key.strip_prefix(FLOW_ROOT)?;
    let mut segments = rest.split('/');
    let root = segments.next()?;
    let id = segments.next()?;
    if root.is_empty() || id.is_empty() {
        return None;
    }
    Some(format!("{root}/{id}"))
}

/// Short fingerprint used by the browser to namespace persisted flow chunks.
///
/// The object key is content-addressed by the upload path in the current
/// storage layout, and the first eight digest bytes preserve the historical
/// sixteen-character wire value without putting the full SHA-256 in every
/// manifest.
#[must_use]
pub fn flow_book_fingerprint(book_object_key: &str) -> String {
    let digest = crate::hash::sha256(book_object_key.as_bytes());
    let mut out = String::with_capacity(16);
    for byte in digest.into_iter().take(8) {
        out.push(char::from_digit(u32::from(byte >> 4), 16).unwrap_or('0'));
        out.push(char::from_digit(u32::from(byte & 0x0f), 16).unwrap_or('0'));
    }
    out
}

/// Lowercase hex SHA-256 of `bytes`.
///
/// Implemented here so the shared crate stays dependency-light and works on
/// wasm; it is *not* used for password hashing.
#[must_use]
pub fn sha256_hex(bytes: &[u8]) -> String {
    let digest = crate::hash::sha256(bytes);
    let mut out = String::with_capacity(64);
    for byte in digest {
        out.push(char::from_digit(u32::from(byte >> 4), 16).unwrap_or('0'));
        out.push(char::from_digit(u32::from(byte & 0x0f), 16).unwrap_or('0'));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blob_keys_round_trip() {
        let id = "0190f8f0-1c2b-7c3d-9e4f-5a6b7c8d9e0f";
        assert_eq!(blob_key(id), format!("blobs/{id}"));
        assert_eq!(blob_id(&blob_key(id)), Some(id));
        assert_eq!(blob_id("thumbs/ab/cd.jpg"), None);
        assert_eq!(blob_id("blobs/"), None);
        assert_eq!(blob_id("blobs/a/b"), None);
    }

    #[test]
    fn thumbnails_are_sharded_by_the_first_two_characters() {
        let id = "0190f8f0-1c2b-7c3d-9e4f-5a6b7c8d9e0f";
        assert_eq!(
            thumbnail_key(id).unwrap(),
            "thumbs/01/90f8f0-1c2b-7c3d-9e4f-5a6b7c8d9e0f.jpg"
        );
        assert_eq!(thumbnail_key("ab"), None);
    }

    #[test]
    fn multipart_directories_are_scoped_and_hashed() {
        let dir = multipart_dir("upload-1", "blobs/abc");
        assert!(dir.starts_with(".multipart/upload-1/"));
        assert_eq!(dir.len(), ".multipart/upload-1/".len() + 64);
        // The same key always maps to the same directory.
        assert_eq!(dir, multipart_dir("upload-1", "blobs/abc"));
        // A different upload id never collides.
        assert_ne!(dir, multipart_dir("upload-2", "blobs/abc"));
    }

    #[test]
    fn flow_keys_are_derivable_and_reversible() {
        let book = "blobs/0190f8f0-1c2b-7c3d-9e4f-5a6b7c8d9e0f";
        assert_eq!(
            flow_manifest_key(book, 4),
            format!("flows/{book}/f4/manifest.json")
        );
        assert_eq!(
            flow_chunk_key(book, 4, 7),
            format!("flows/{book}/f4/chunks/7.html")
        );
        assert_eq!(
            flow_book_key(&flow_manifest_key(book, 4)).as_deref(),
            Some(book)
        );
        assert_eq!(
            flow_book_key(&flow_chunk_key(book, 4, 7)).as_deref(),
            Some(book)
        );
        assert_eq!(flow_book_key("blobs/x"), None);
        assert_eq!(flow_book_key("flows/"), None);
    }

    #[test]
    fn sha256_matches_known_vectors() {
        assert_eq!(
            sha256_hex(b""),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
        assert_eq!(
            sha256_hex(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    #[test]
    fn flow_book_fingerprint_is_a_stable_short_digest() {
        assert_eq!(flow_book_fingerprint("blobs/abc"), "8a49932cc4d7d9b6");
        assert_eq!(flow_book_fingerprint("blobs/abc").len(), 16);
        assert_ne!(
            flow_book_fingerprint("blobs/abc"),
            flow_book_fingerprint("blobs/def")
        );
    }
}

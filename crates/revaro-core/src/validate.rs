//! Input validation and small parsing helpers shared by both ends.
//!
//! These rules used to be split between Go handlers and TypeScript guards, with
//! subtly different behaviour. They live here so a name rejected by the browser
//! is rejected by the server for exactly the same reason, and so the message the
//! user sees is produced in one place.

use crate::error::ApiError;
use crate::limits;

/// Characters allowed in an RFC 7230 token, which is what MIME types are made
/// of.
fn is_token_char(byte: u8) -> bool {
    byte.is_ascii_alphanumeric()
        || matches!(
            byte,
            b'!' | b'#'
                | b'$'
                | b'%'
                | b'&'
                | b'\''
                | b'*'
                | b'+'
                | b'-'
                | b'.'
                | b'^'
                | b'_'
                | b'`'
                | b'|'
                | b'~'
        )
}

/// Parse a `Content-Type` header value.
///
/// Returns the lowercased `type/subtype` and the raw parameter section, or
/// `None` when the value is not a syntactically valid media type. This mirrors
/// `mime.ParseMediaType`, which the Go server used both to validate uploaded
/// MIME types and to decide whether a stored type was safe to deliver.
#[must_use]
pub fn parse_media_type(value: &str) -> Option<(String, String)> {
    let (head, params) = match value.split_once(';') {
        Some((head, params)) => (head, params),
        None => (value, ""),
    };
    let head = head.trim();
    let (media_type, subtype) = head.split_once('/')?;
    if media_type.is_empty() || subtype.is_empty() {
        return None;
    }
    if !media_type.bytes().all(is_token_char) || !subtype.bytes().all(is_token_char) {
        return None;
    }
    Some((
        format!(
            "{}/{}",
            media_type.to_ascii_lowercase(),
            subtype.to_ascii_lowercase()
        ),
        params.trim().to_owned(),
    ))
}

/// Reject a MIME type the product cannot store.
///
/// # Errors
/// `400` when the value is empty, overly long, or not a media type.
pub fn validate_mime_type(value: &str) -> Result<(), ApiError> {
    if value.len() > limits::MAX_MIME_TYPE_LEN {
        return Err(ApiError::bad_request("mime type is too long"));
    }
    if parse_media_type(value).is_none() {
        return Err(ApiError::bad_request("mime type is invalid"));
    }
    Ok(())
}

/// Validate a single file or directory name.
///
/// Names may not be empty, `.`, `..`, padded with whitespace, longer than
/// 1024 bytes or 255 characters, contain a path separator, or contain control
/// characters.
///
/// # Errors
/// `400` with the first rule that fails.
pub fn validate_name(name: &str) -> Result<(), ApiError> {
    if name.is_empty() || name == "." || name == ".." {
        return Err(ApiError::bad_request("invalid name"));
    }
    if name.trim() != name {
        return Err(ApiError::bad_request(
            "name cannot start or end with whitespace",
        ));
    }
    if name.len() > limits::MAX_NAME_BYTES || name.chars().count() > limits::MAX_NAME_CHARS {
        return Err(ApiError::bad_request("name is too long"));
    }
    if name.contains('/') || name.contains('\\') {
        return Err(ApiError::bad_request("name cannot contain path separators"));
    }
    if name
        .chars()
        .any(|character| character < '\u{20}' || character == '\u{7f}')
    {
        return Err(ApiError::bad_request("name contains control characters"));
    }
    Ok(())
}

/// Validate the logical size declared for an upload.
///
/// # Errors
/// `400` when the size is negative or above the one-tebibyte ceiling.
pub fn validate_file_size(size: i64) -> Result<(), ApiError> {
    if !(0..=limits::MAX_LOGICAL_FILE_SIZE).contains(&size) {
        return Err(ApiError::bad_request("invalid file size"));
    }
    Ok(())
}

/// Validate a text document before it is created or updated.
///
/// Rust strings are always valid UTF-8, so the explicit encoding check the Go
/// server needed has no equivalent here: invalid bytes are rejected by the JSON
/// decoder before a handler ever sees them.
///
/// # Errors
/// `400` when the name, the extension or the size is unacceptable.
pub fn validate_document(name: &str, content: &str) -> Result<(), ApiError> {
    validate_name(name)?;
    if !crate::classify::is_editable_name(name) {
        return Err(ApiError::bad_request(
            "this file type cannot be edited as text",
        ));
    }
    if content.len() > limits::MAX_DOCUMENT_BYTES {
        return Err(ApiError::bad_request(
            "editable documents cannot exceed 1 MiB",
        ));
    }
    Ok(())
}

/// Validate the 1-based number of an upload part.
///
/// # Errors
/// `400` when the number is not an integer inside `1..=part_count`.
pub fn validate_part_number(number: i64, part_count: usize) -> Result<usize, ApiError> {
    if number < 1 || number > part_count as i64 {
        return Err(ApiError::bad_request("invalid multipart part number"));
    }
    Ok(number as usize)
}

/// Resolve a relative path into a normalized form with no `.` or `..`
/// components, or `None` when it escapes its root.
fn clean_relative_path(path: &str) -> Option<String> {
    let mut parts: Vec<&str> = Vec::new();
    for part in path.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                parts.pop()?;
            }
            other => parts.push(other),
        }
    }
    if parts.is_empty() {
        None
    } else {
        Some(parts.join("/"))
    }
}

/// Normalize and validate an entry path coming out of an archive.
///
/// This is the path-traversal defence for archive extraction: absolute paths,
/// Windows drive prefixes and `..` escapes are all refused, and each component
/// must be a legal file name. Backslashes are kept as characters here rather
/// than being treated as separators. That matches the historical Linux server
/// and avoids making a path safe on one platform while changing its meaning on
/// another; a backslash in a component is rejected by [`validate_name`].
///
/// The returned value uses `/` separators and contains no `.` or `..`
/// components, so callers can join it below a trusted extraction directory.
///
/// # Errors
/// `400` describing why the entry was refused.
pub fn normalize_archive_path(path: &str) -> Result<String, ApiError> {
    let trimmed = path.trim();
    if trimmed.is_empty()
        || trimmed.starts_with('/')
        || trimmed.starts_with('\\')
        || trimmed.contains('\0')
    {
        return Err(ApiError::bad_request(
            "archive contains an invalid absolute path",
        ));
    }
    let Some(clean) = clean_relative_path(trimmed) else {
        return Err(ApiError::bad_request(
            "archive contains a path outside its root",
        ));
    };
    if clean.len() >= 2 && clean.as_bytes()[1] == b':' {
        return Err(ApiError::bad_request(
            "archive contains a path outside its root",
        ));
    }
    for component in clean.split('/') {
        if let Err(error) = validate_name(component) {
            return Err(ApiError::bad_request(format!(
                "archive path {path:?} is not supported: {error}"
            )));
        }
    }
    Ok(clean)
}

/// Validate an entry path coming out of an archive before it is written into
/// the object store.
///
/// # Errors
/// `400` describing why the entry was refused.
pub fn validate_archive_path(path: &str) -> Result<(), ApiError> {
    normalize_archive_path(path).map(|_| ())
}

/// Validate a media playback position before it is stored.
///
/// The rules exist because the position is attacker-controlled and ends up in
/// SQLite as milliseconds: a negative, not-a-number or absurd value would
/// either fail the column's `CHECK` constraint with a `500` or store nonsense
/// that the player would then seek to. The bounds are the ones the Go server
/// enforced, including the tolerance that lets a client report a position a few
/// seconds past a slightly stale duration.
///
/// # Errors
/// `400` when any value is out of range.
pub fn validate_media_progress(position: f64, duration: f64) -> Result<(), ApiError> {
    /// A week, far beyond any real media file this product handles.
    const MAX_SECONDS: f64 = 7.0 * 24.0 * 60.0 * 60.0;
    /// How far past the recorded duration a position may be.
    const POSITION_TOLERANCE: f64 = 5.0;

    let in_range = |value: f64| value.is_finite() && (0.0..=MAX_SECONDS).contains(&value);
    if !in_range(position)
        || !in_range(duration)
        || (duration > 0.0 && position > duration + POSITION_TOLERANCE)
    {
        return Err(ApiError::bad_request("media progress values are invalid"));
    }
    Ok(())
}

/// Validate a batch request for upload part URLs.
///
/// Returns the numbers in request order. Duplicates are rejected because each
/// returned URL is single-use; handing out two URLs for one part would let a
/// client upload it twice and produce a wrong entity tag on the second attempt.
///
/// # Errors
/// `400` when the batch size is wrong, or a number is out of range or repeated.
pub fn validate_upload_part_batch(
    numbers: &[i32],
    part_count: usize,
) -> Result<Vec<i32>, ApiError> {
    if numbers.is_empty() || numbers.len() > limits::MAX_UPLOAD_PART_BATCH {
        return Err(ApiError::bad_request(format!(
            "request between 1 and {} upload parts",
            limits::MAX_UPLOAD_PART_BATCH
        )));
    }
    let mut seen = std::collections::HashSet::with_capacity(numbers.len());
    for number in numbers {
        let valid = *number >= 1 && (*number as usize) <= part_count && seen.insert(*number);
        if !valid {
            return Err(ApiError::bad_request("invalid multipart part number"));
        }
    }
    Ok(numbers.to_vec())
}

/// Validate the id list of a batch download request.
///
/// Only the shape is checked here; whether each id resolves to a readable file
/// needs the database and belongs to the server.
///
/// # Errors
/// `400` for an empty list, too many ids, a malformed id, or a duplicate.
pub fn validate_batch_download_ids(ids: &[String]) -> Result<(), ApiError> {
    if ids.is_empty() {
        return Err(ApiError::bad_request("at least one file id is required"));
    }
    if ids.len() > limits::MAX_BATCH_DOWNLOAD_FILES {
        return Err(ApiError::bad_request(format!(
            "a maximum of {} files can be downloaded at once",
            limits::MAX_BATCH_DOWNLOAD_FILES
        )));
    }
    let mut seen = std::collections::HashSet::with_capacity(ids.len());
    for id in ids {
        if !is_valid_batch_download_id(id) {
            return Err(ApiError::bad_request("invalid file id"));
        }
        if !seen.insert(id) {
            return Err(ApiError::bad_request("duplicate file id"));
        }
    }
    Ok(())
}

/// Validate the externally supplied identifier shape used by the historical
/// batch-download endpoint. File rows normally use UUIDs, but the old handler
/// deliberately left the final lookup to the database: a safe unknown value
/// therefore produced the same 404 as any other missing file. Keep that
/// distinction at the API boundary for clients which rely on the old error
/// partitioning.
fn is_valid_batch_download_id(id: &str) -> bool {
    if id.is_empty() || id == "." || id == ".." || id.len() > 128 || id.trim() != id {
        return false;
    }
    !id.chars().any(|character| {
        character < '\u{20}' || character == '\u{7f}' || character == '/' || character == '\\'
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_ordinary_names() {
        for name in ["photo.png", "三体.epub", "a b c.txt", ".bashrc", "a-b_c.d"] {
            assert!(validate_name(name).is_ok(), "{name:?} should be accepted");
        }
    }

    #[test]
    fn rejects_empty_dot_and_dotdot() {
        for name in ["", ".", ".."] {
            let error = validate_name(name).unwrap_err();
            assert_eq!(error.message, "invalid name");
        }
    }

    #[test]
    fn rejects_padded_whitespace() {
        for name in [" leading", "trailing ", "\tname", " padded "] {
            let error = validate_name(name).unwrap_err();
            assert_eq!(
                error.message, "name cannot start or end with whitespace",
                "{name:?}"
            );
        }
    }

    #[test]
    fn rejects_path_separators() {
        assert_eq!(
            validate_name("a/b").unwrap_err().message,
            "name cannot contain path separators"
        );
        assert_eq!(
            validate_name("a\\b").unwrap_err().message,
            "name cannot contain path separators"
        );
    }

    #[test]
    fn rejects_control_characters() {
        assert_eq!(
            validate_name("a\u{7f}b").unwrap_err().message,
            "name contains control characters"
        );
        assert_eq!(
            validate_name("a\u{1}b").unwrap_err().message,
            "name contains control characters"
        );
    }

    #[test]
    fn enforces_both_length_limits() {
        assert!(validate_name(&"a".repeat(limits::MAX_NAME_CHARS)).is_ok());
        assert_eq!(
            validate_name(&"a".repeat(limits::MAX_NAME_CHARS + 1))
                .unwrap_err()
                .message,
            "name is too long"
        );
        // 255 four-byte characters is 1020 bytes, under the byte cap; 257
        // characters trips the character cap first.
        assert!(validate_name(&"😀".repeat(255)).is_ok());
        assert_eq!(
            validate_name(&"😀".repeat(256)).unwrap_err().message,
            "name is too long"
        );
    }

    #[test]
    fn validates_mime_types_like_parse_media_type() {
        assert!(validate_mime_type("image/png").is_ok());
        assert!(validate_mime_type("text/plain; charset=utf-8").is_ok());
        assert!(validate_mime_type("application/epub+zip").is_ok());
        assert_eq!(
            validate_mime_type("").unwrap_err().message,
            "mime type is invalid"
        );
        assert_eq!(
            validate_mime_type("nope").unwrap_err().message,
            "mime type is invalid"
        );
        assert_eq!(
            validate_mime_type("a b/c").unwrap_err().message,
            "mime type is invalid"
        );
        assert_eq!(
            validate_mime_type(&format!("text/{}", "x".repeat(300)))
                .unwrap_err()
                .message,
            "mime type is too long"
        );
    }

    #[test]
    fn parses_media_types_into_lowercase() {
        assert_eq!(
            parse_media_type("TEXT/Plain; charset=UTF-8"),
            Some(("text/plain".to_owned(), "charset=UTF-8".to_owned()))
        );
    }

    #[test]
    fn validates_documents() {
        assert!(validate_document("notes.md", "hello").is_ok());
        assert_eq!(
            validate_document("photo.png", "x").unwrap_err().message,
            "this file type cannot be edited as text"
        );
        assert_eq!(
            validate_document("bad/name.md", "x").unwrap_err().message,
            "name cannot contain path separators"
        );
        let huge = "a".repeat(limits::MAX_DOCUMENT_BYTES + 1);
        assert_eq!(
            validate_document("notes.md", &huge).unwrap_err().message,
            "editable documents cannot exceed 1 MiB"
        );
        assert!(validate_document("notes.md", &"a".repeat(limits::MAX_DOCUMENT_BYTES)).is_ok());
    }

    #[test]
    fn accepts_ordinary_archive_paths() {
        for path in [
            "a.txt",
            "dir/a.txt",
            "./a.txt",
            "dir/./sub/a.txt",
            "dir/../a.txt",
        ] {
            assert!(
                validate_archive_path(path).is_ok(),
                "{path:?} should be accepted"
            );
        }
    }

    #[test]
    fn rejects_absolute_archive_paths() {
        for path in ["/etc/passwd", "\\windows\\system32", "", "  "] {
            let error = validate_archive_path(path).unwrap_err();
            assert_eq!(
                error.message, "archive contains an invalid absolute path",
                "{path:?}"
            );
        }
    }

    #[test]
    fn rejects_archive_paths_that_escape_their_root() {
        for path in ["../secret", "a/../../secret", "..", "C:/windows"] {
            let error = validate_archive_path(path).unwrap_err();
            assert_eq!(
                error.message, "archive contains a path outside its root",
                "{path:?}"
            );
        }
    }

    #[test]
    fn rejects_archive_entries_with_illegal_component_names() {
        let error = validate_archive_path("dir/ padded.txt").unwrap_err();
        assert!(
            error.message.starts_with("archive path "),
            "{}",
            error.message
        );
        assert!(error.message.contains("is not supported"));
    }

    #[test]
    fn rejects_backslashes_inside_archive_components() {
        let error = validate_archive_path("dir\\file.txt").unwrap_err();
        assert!(error.message.starts_with("archive path "));
        assert!(error.message.contains("path separators"));
    }

    #[test]
    fn validates_part_numbers() {
        assert_eq!(validate_part_number(1, 3).unwrap(), 1);
        assert_eq!(validate_part_number(3, 3).unwrap(), 3);
        assert_eq!(
            validate_part_number(0, 3).unwrap_err().message,
            "invalid multipart part number"
        );
        assert_eq!(
            validate_part_number(4, 3).unwrap_err().message,
            "invalid multipart part number"
        );
        assert_eq!(validate_part_number(-1, 3).unwrap_err().status, 400);
    }

    #[test]
    fn accepts_ordinary_playback_positions() {
        assert!(validate_media_progress(0.0, 0.0).is_ok());
        assert!(validate_media_progress(12.5, 100.0).is_ok());
        // A position slightly past a stale duration is tolerated.
        assert!(validate_media_progress(103.0, 100.0).is_ok());
        // A week exactly is still in range.
        assert!(validate_media_progress(7.0 * 24.0 * 3600.0, 0.0).is_ok());
    }

    #[test]
    fn rejects_impossible_playback_positions() {
        let error = || validate_media_progress(f64::NAN, 0.0).unwrap_err();
        assert_eq!(error().message, "media progress values are invalid");
        for (position, duration) in [
            (f64::NAN, 0.0),
            (f64::INFINITY, 0.0),
            (0.0, f64::NAN),
            (0.0, f64::NEG_INFINITY),
            (-0.1, 0.0),
            (0.0, -1.0),
            (7.0 * 24.0 * 3600.0 + 1.0, 0.0),
            (0.0, 7.0 * 24.0 * 3600.0 + 1.0),
            // More than the tolerance past a known duration.
            (106.0, 100.0),
        ] {
            let result = validate_media_progress(position, duration);
            assert!(result.is_err(), "{position}/{duration} should be rejected");
            assert_eq!(result.unwrap_err().status, 400);
        }
    }

    #[test]
    fn upload_part_batches_are_bounded_and_unique() {
        assert_eq!(
            validate_upload_part_batch(&[1, 2, 3], 3).unwrap(),
            vec![1, 2, 3]
        );
        assert_eq!(
            validate_upload_part_batch(&[], 3).unwrap_err().message,
            "request between 1 and 100 upload parts"
        );
        let too_many: Vec<i32> = (1..=101).collect();
        assert_eq!(
            validate_upload_part_batch(&too_many, 10_000)
                .unwrap_err()
                .message,
            "request between 1 and 100 upload parts"
        );
        // Out of range, zero, negative and duplicate numbers are all refused.
        for bad in [vec![0], vec![-1], vec![4], vec![1, 1]] {
            assert_eq!(
                validate_upload_part_batch(&bad, 3).unwrap_err().message,
                "invalid multipart part number",
                "{bad:?}"
            );
        }
        // Exactly 100 is allowed.
        let full: Vec<i32> = (1..=100).collect();
        assert!(validate_upload_part_batch(&full, 100).is_ok());
    }

    #[test]
    fn batch_download_id_lists_are_validated() {
        let id = "0190f8f0-1c2b-7c3d-9e4f-5a6b7c8d9e0f".to_owned();
        assert!(validate_batch_download_ids(std::slice::from_ref(&id)).is_ok());
        assert_eq!(
            validate_batch_download_ids(&[]).unwrap_err().message,
            "at least one file id is required"
        );
        assert_eq!(
            validate_batch_download_ids(&[id.clone(), id.clone()])
                .unwrap_err()
                .message,
            "duplicate file id"
        );
        assert!(validate_batch_download_ids(&["nope".to_owned()]).is_ok());
        assert!(validate_batch_download_ids(&["blobs/file".to_owned()]).is_err());
        let too_many: Vec<String> = (0..1001).map(|index| format!("{index:0>36}")).collect();
        assert!(
            validate_batch_download_ids(&too_many)
                .unwrap_err()
                .message
                .starts_with("a maximum of 1000 files")
        );
    }

    #[test]
    fn validates_file_sizes() {
        assert!(validate_file_size(0).is_ok());
        assert!(validate_file_size(limits::MAX_LOGICAL_FILE_SIZE).is_ok());
        assert_eq!(
            validate_file_size(-1).unwrap_err().message,
            "invalid file size"
        );
        assert_eq!(
            validate_file_size(limits::MAX_LOGICAL_FILE_SIZE + 1)
                .unwrap_err()
                .message,
            "invalid file size"
        );
    }
}

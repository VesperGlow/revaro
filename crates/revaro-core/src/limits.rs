//! Product limits shared by the server, the upload client and the reader.
//!
//! Every one of these used to be duplicated: a Go constant on the server and a
//! literal in the TypeScript client. Keeping them in one crate means the
//! browser can never disagree with the server about how a file is sliced,
//! whether it is editable, or what counts as too large.

/// Maximum decoded JSON request body.
pub const MAX_JSON_BODY_BYTES: usize = 7 << 20;

/// Maximum size of an editable text document.
pub const MAX_DOCUMENT_BYTES: usize = 1 << 20;

/// Maximum size of the administrator avatar upload.
pub const MAX_AVATAR_BYTES: usize = 2 << 20;

/// Maximum logical size of any single file.
pub const MAX_LOGICAL_FILE_SIZE: i64 = 1 << 40;

/// Maximum number of parts in one multipart upload.
pub const MAX_UPLOAD_PARTS: usize = 10_000;

/// Maximum number of parts acknowledged in one batch request.
pub const MAX_UPLOAD_PART_BATCH: usize = 100;

/// Files at or above this size use the multipart path.
pub const MULTIPART_UPLOAD_THRESHOLD: i64 = 16 << 20;

/// Preferred multipart part size.
pub const DEFAULT_MULTIPART_PART_SIZE: i64 = 16 << 20;

/// Maximum number of files in one prepared batch-download archive.
pub const MAX_BATCH_DOWNLOAD_FILES: usize = 1000;

/// Maximum length of a `Content-Type` header value accepted from a client.
pub const MAX_MIME_TYPE_LEN: usize = 255;

/// Maximum byte length of a file or directory name.
pub const MAX_NAME_BYTES: usize = 1024;

/// Maximum number of Unicode scalar values in a file or directory name.
pub const MAX_NAME_CHARS: usize = 255;

/// Maximum size of a plain-text book the reader will open.
pub const MAX_TXT_BYTES: i64 = 16 << 20;

/// Maximum size of an EPUB the reader will open.
pub const MAX_EPUB_BYTES: i64 = 128 << 20;

/// Maximum size of a single reader flow artifact served over HTTP.
pub const MAX_FLOW_OBJECT_BYTES: usize = 8 << 20;

/// True when a file of `size` bytes should use the multipart upload path.
#[must_use]
pub fn uses_multipart_upload(size: i64) -> bool {
    size >= MULTIPART_UPLOAD_THRESHOLD
}

/// The part size the server will assign to an upload of `size` bytes.
///
/// Mirrors the historical server rule: 16 MiB by default, grown and rounded up
/// to whole MiB for very large files so the 10,000-part ceiling can never be
/// exceeded and the browser slices on friendly boundaries.
#[must_use]
pub fn multipart_part_size(size: i64) -> i64 {
    let default = DEFAULT_MULTIPART_PART_SIZE;
    if size > default * MAX_UPLOAD_PARTS as i64 {
        ((size + 9999) / 10_000 + (1 << 20) - 1) / (1 << 20) * (1 << 20)
    } else {
        default
    }
}

/// Number of parts an upload of `size` bytes needs at `part_size`.
///
/// Returns an error message suitable for a `400` response when the parameters
/// cannot describe a valid upload.
#[must_use = "the part count must be handled"]
pub fn multipart_part_count(size: i64, part_size: i64) -> Result<usize, &'static str> {
    if size < 0 || part_size <= 0 {
        return Err("invalid multipart size");
    }
    if size == 0 {
        return Ok(0);
    }
    let parts = (size + part_size - 1) / part_size;
    if parts > MAX_UPLOAD_PARTS as i64 {
        return Err("multipart upload exceeds 10000 parts");
    }
    Ok(parts as usize)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn small_files_use_the_single_request_path() {
        assert!(!uses_multipart_upload(0));
        assert!(!uses_multipart_upload(MULTIPART_UPLOAD_THRESHOLD - 1));
        assert!(uses_multipart_upload(MULTIPART_UPLOAD_THRESHOLD));
    }

    #[test]
    fn part_size_stays_at_the_default_until_the_part_limit_bites() {
        assert_eq!(multipart_part_size(16 << 20), DEFAULT_MULTIPART_PART_SIZE);
        assert_eq!(
            multipart_part_size(DEFAULT_MULTIPART_PART_SIZE * MAX_UPLOAD_PARTS as i64),
            DEFAULT_MULTIPART_PART_SIZE
        );
    }

    #[test]
    fn part_size_grows_for_huge_files_and_never_exceeds_the_part_limit() {
        // One tebibyte, the largest logical file the product allows.
        let size = MAX_LOGICAL_FILE_SIZE;
        let part_size = multipart_part_size(size);
        assert!(part_size > DEFAULT_MULTIPART_PART_SIZE);
        assert_eq!(part_size % (1 << 20), 0, "part size must be whole MiB");
        let count = multipart_part_count(size, part_size).unwrap();
        assert!(count <= MAX_UPLOAD_PARTS, "{count} parts exceeds the limit");
    }

    #[test]
    fn part_counts_cover_the_edge_cases() {
        assert_eq!(multipart_part_count(0, 16), Ok(0));
        assert_eq!(multipart_part_count(16, 16), Ok(1));
        assert_eq!(multipart_part_count(17, 16), Ok(2));
        assert_eq!(multipart_part_count(-1, 16), Err("invalid multipart size"));
        assert_eq!(multipart_part_count(10, 0), Err("invalid multipart size"));
        assert_eq!(
            multipart_part_count(MAX_UPLOAD_PARTS as i64 + 1, 1),
            Err("multipart upload exceeds 10000 parts")
        );
    }
}

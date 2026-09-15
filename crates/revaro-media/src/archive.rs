//! Bounded archive inspection and extraction.
//!
//! The engine is deliberately independent of the server. It receives concrete
//! paths chosen by the caller, writes only below the caller supplied output
//! directory, and reports no database or HTTP state. The server owns task
//! persistence and imports the resulting regular files into the object store.

use std::fs::{self, OpenOptions};
use std::io::Write as _;
use std::path::{Component, Path, PathBuf};

use libarchive2::{FileType, ReadArchive};
use tokio_util::sync::CancellationToken;

/// Maximum number of archive entries inspected by one extraction.
pub const MAX_ARCHIVE_ENTRIES: usize = 100_000;
/// Absolute ceiling for the expanded bytes produced by one extraction.
pub const MAX_ARCHIVE_EXPANDED_BYTES: i64 = 64 << 30;
/// Maximum password length accepted by the archive task input endpoint.
pub const MAX_ARCHIVE_PASSWORD_BYTES: usize = 1024;
/// Chunk size used while copying archive data to disk.
const COPY_BUFFER_BYTES: usize = 256 << 10;

/// The phase reported while an archive is being checked or expanded.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArchivePhase {
    /// Archive headers and entry metadata are being inspected.
    Checking,
    /// Regular file data is being written.
    Extracting,
}

impl ArchivePhase {
    /// The task phase spelling used by the API and database.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Checking => "checking",
            Self::Extracting => "extracting",
        }
    }
}

/// Progress emitted by the blocking archive loop.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ArchiveProgress {
    /// Current engine phase.
    pub phase: ArchivePhase,
    /// Number of headers observed so far.
    pub entries: usize,
    /// Bytes written so far.
    pub expanded_bytes: i64,
}

/// Result of a successful extraction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ArchiveResult {
    /// Number of archive entries, including directories.
    pub entries: usize,
    /// Number of regular-file bytes written.
    pub expanded_bytes: i64,
}

/// Failures produced by the archive engine.
#[derive(Debug, thiserror::Error)]
pub enum ArchiveError {
    /// The archive could not be opened or its format could not be decoded.
    #[error("archive input error: {0}")]
    Input(String),
    /// The caller cancelled the operation.
    #[error("archive extraction cancelled")]
    Cancelled,
    /// The archive requires a password and none was supplied.
    #[error("archive password required")]
    PasswordRequired,
    /// A supplied archive password was rejected.
    #[error("archive password is incorrect")]
    WrongPassword,
    /// An entry path was rejected by the shared path policy.
    #[error("{0}")]
    UnsafePath(String),
    /// The archive contains too many headers.
    #[error("archive contains more than {MAX_ARCHIVE_ENTRIES} entries")]
    TooManyEntries,
    /// The expanded output exceeds the configured limit.
    #[error("archive exceeds expanded-size limit")]
    ExpandedTooLarge,
    /// Links are never materialized into the object store.
    #[error("archive links are not allowed")]
    LinksNotAllowed,
    /// Device files, sockets and other special entries are unsupported.
    #[error("archive contains unsupported special files")]
    UnsupportedSpecial,
    /// The archive did not contain any entry.
    #[error("archive is empty")]
    Empty,
    /// An entry did not expose a usable pathname.
    #[error("archive entry has no pathname")]
    MissingPath,
    /// The bytes read for an entry differed from its declared size.
    #[error("archive entry size mismatch for {0:?}")]
    SizeMismatch(String),
    /// The output directory or one of its files could not be written.
    #[error("archive output error: {0}")]
    Output(#[source] std::io::Error),
}

impl ArchiveError {
    /// Whether the error represents a password prompt or a rejected password.
    #[must_use]
    pub const fn is_password(&self) -> bool {
        matches!(self, Self::PasswordRequired | Self::WrongPassword)
    }
}

/// The in-process archive engine.
#[derive(Debug, Clone, Copy, Default)]
pub struct ArchiveEngine;

impl ArchiveEngine {
    /// Extract a supported archive into a fresh directory.
    ///
    /// The caller must provide a positive expanded-size limit. Every path is
    /// normalized with the shared validation policy before it is joined to
    /// `output`; links and special files are refused, and both declared and
    /// actual byte counts are bounded.
    ///
    /// # Errors
    /// Returns an [`ArchiveError`] when the input is invalid, unsafe, too
    /// large, password protected, or cancelled.
    pub fn extract<F>(
        &self,
        source: &Path,
        output: &Path,
        password: Option<&str>,
        expanded_limit: i64,
        cancel: CancellationToken,
        mut progress: F,
    ) -> Result<ArchiveResult, ArchiveError>
    where
        F: FnMut(ArchiveProgress),
    {
        if expanded_limit <= 0 {
            return Err(ArchiveError::ExpandedTooLarge);
        }
        if password.is_some_and(|value| value.len() > MAX_ARCHIVE_PASSWORD_BYTES) {
            return Err(ArchiveError::Input(
                "archive password is too long".to_owned(),
            ));
        }
        check_cancel(&cancel)?;
        prepare_output(output)?;

        let password_supplied = password.is_some_and(|value| !value.is_empty());
        let mut archive = match password {
            Some(value) if !value.is_empty() => ReadArchive::open_with_passphrase(source, value)
                .map_err(|error| classify_library_error(error.to_string(), password_supplied))?,
            _ => ReadArchive::open(source)
                .map_err(|error| classify_library_error(error.to_string(), false))?,
        };
        let mut entries = 0usize;
        let mut expanded_bytes = 0i64;
        let mut buffer = vec![0u8; COPY_BUFFER_BYTES];

        while let Some(metadata) = next_metadata(&mut archive, password_supplied)? {
            check_cancel(&cancel)?;
            entries = entries.checked_add(1).ok_or(ArchiveError::TooManyEntries)?;
            if entries > MAX_ARCHIVE_ENTRIES {
                return Err(ArchiveError::TooManyEntries);
            }
            progress(ArchiveProgress {
                phase: ArchivePhase::Checking,
                entries,
                expanded_bytes,
            });

            let relative = historical_archive_path(&metadata.pathname)?;
            let target = output.join(PathBuf::from(&relative));
            if metadata.is_link {
                return Err(ArchiveError::LinksNotAllowed);
            }
            match metadata.file_type {
                FileType::Directory => {
                    ensure_directory(&target)?;
                    archive.skip_data().map_err(|error| {
                        classify_library_error(error.to_string(), password_supplied)
                    })?;
                }
                FileType::RegularFile => {
                    if metadata.declared_size < 0
                        || metadata.declared_size > expanded_limit.saturating_sub(expanded_bytes)
                    {
                        return Err(ArchiveError::ExpandedTooLarge);
                    }
                    if let Some(parent) = target.parent() {
                        ensure_directory(parent)?;
                    }
                    let mut file = OpenOptions::new()
                        .create_new(true)
                        .write(true)
                        .open(&target)
                        .map_err(ArchiveError::Output)?;
                    let mut written = 0i64;
                    loop {
                        check_cancel(&cancel)?;
                        let read = archive.read_data(&mut buffer).map_err(|error| {
                            classify_library_error(error.to_string(), password_supplied)
                        })?;
                        if read == 0 {
                            break;
                        }
                        let read =
                            i64::try_from(read).map_err(|_| ArchiveError::ExpandedTooLarge)?;
                        written = written
                            .checked_add(read)
                            .ok_or(ArchiveError::ExpandedTooLarge)?;
                        let candidate = expanded_bytes
                            .checked_add(written)
                            .ok_or(ArchiveError::ExpandedTooLarge)?;
                        if candidate > expanded_limit {
                            return Err(ArchiveError::ExpandedTooLarge);
                        }
                        // `read` came from the buffer length, so this conversion
                        // is exact and cannot truncate the slice.
                        let read =
                            usize::try_from(read).map_err(|_| ArchiveError::ExpandedTooLarge)?;
                        file.write_all(&buffer[..read])
                            .map_err(ArchiveError::Output)?;
                        progress(ArchiveProgress {
                            phase: ArchivePhase::Extracting,
                            entries,
                            expanded_bytes: candidate,
                        });
                    }
                    if metadata.declared_size != 0 && written != metadata.declared_size {
                        return Err(ArchiveError::SizeMismatch(metadata.pathname));
                    }
                    file.flush().map_err(ArchiveError::Output)?;
                    expanded_bytes = expanded_bytes
                        .checked_add(written)
                        .ok_or(ArchiveError::ExpandedTooLarge)?;
                }
                _ => return Err(ArchiveError::UnsupportedSpecial),
            }
        }
        if entries == 0 {
            return Err(ArchiveError::Empty);
        }
        progress(ArchiveProgress {
            phase: ArchivePhase::Extracting,
            entries,
            expanded_bytes,
        });
        Ok(ArchiveResult {
            entries,
            expanded_bytes,
        })
    }
}

/// Reproduce the pre-migration data-plane path policy. It intentionally
/// rejects every non-normal component instead of cleaning `.` or `..`: the
/// old sidecar used `Path::components()` and only accepted ordinary names.
/// The server still performs a second validation while importing the output.
fn historical_archive_path(raw: &str) -> Result<String, ArchiveError> {
    let path = Path::new(raw);
    let mut clean = PathBuf::new();
    for component in path.components() {
        match component {
            Component::Normal(name) if !name.is_empty() => clean.push(name),
            _ => return Err(ArchiveError::Input(format!("unsafe archive path: {raw:?}"))),
        }
    }
    if clean.as_os_str().is_empty() {
        return Err(ArchiveError::Input("empty archive path".to_owned()));
    }
    clean
        .to_str()
        .map(str::to_owned)
        .ok_or_else(|| ArchiveError::Input("archive path is not valid UTF-8".to_owned()))
}

/// Calculate the output ceiling used by the historical server.
#[must_use]
pub fn expanded_limit(archive_size: i64) -> i64 {
    if archive_size > 0 && archive_size < MAX_ARCHIVE_EXPANDED_BYTES / 100 {
        archive_size.saturating_mul(100).max(4 << 30)
    } else {
        MAX_ARCHIVE_EXPANDED_BYTES
    }
}

#[derive(Debug)]
struct EntryMetadata {
    pathname: String,
    file_type: FileType,
    declared_size: i64,
    is_link: bool,
}

fn next_metadata(
    archive: &mut ReadArchive<'_>,
    password_supplied: bool,
) -> Result<Option<EntryMetadata>, ArchiveError> {
    let Some(entry) = archive
        .next_entry()
        .map_err(|error| classify_library_error(error.to_string(), password_supplied))?
    else {
        return Ok(None);
    };
    let metadata = EntryMetadata {
        pathname: entry.pathname().ok_or(ArchiveError::MissingPath)?,
        file_type: entry.file_type(),
        declared_size: entry.size(),
        is_link: entry.symlink().is_some() || entry.hardlink().is_some(),
    };
    Ok(Some(metadata))
}

fn prepare_output(output: &Path) -> Result<(), ArchiveError> {
    match fs::symlink_metadata(output) {
        Ok(metadata) if metadata.file_type().is_symlink() => {
            Err(ArchiveError::Output(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                "archive output is a symlink",
            )))
        }
        Ok(metadata) if !metadata.is_dir() => Err(ArchiveError::Output(std::io::Error::new(
            std::io::ErrorKind::AlreadyExists,
            "archive output is not a directory",
        ))),
        Ok(_) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            fs::create_dir_all(output).map_err(ArchiveError::Output)
        }
        Err(error) => Err(ArchiveError::Output(error)),
    }
}

fn ensure_directory(path: &Path) -> Result<(), ArchiveError> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() => {
            Err(ArchiveError::Output(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                "archive path is a symlink",
            )))
        }
        Ok(metadata) if metadata.is_dir() => Ok(()),
        Ok(_) => Err(ArchiveError::Output(std::io::Error::new(
            std::io::ErrorKind::AlreadyExists,
            "archive path is not a directory",
        ))),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            fs::create_dir_all(path).map_err(ArchiveError::Output)
        }
        Err(error) => Err(ArchiveError::Output(error)),
    }
}

fn check_cancel(cancel: &CancellationToken) -> Result<(), ArchiveError> {
    if cancel.is_cancelled() {
        Err(ArchiveError::Cancelled)
    } else {
        Ok(())
    }
}

fn classify_library_error(message: String, password_supplied: bool) -> ArchiveError {
    let lower = message.to_ascii_lowercase();
    if lower.contains("passphrase") || lower.contains("password") || lower.contains("encrypted") {
        if password_supplied {
            ArchiveError::WrongPassword
        } else {
            ArchiveError::PasswordRequired
        }
    } else {
        library_error(message)
    }
}

fn library_error(message: String) -> ArchiveError {
    ArchiveError::Input(message)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    use libarchive2::{ArchiveFormat, EntryMut, WriteArchive};
    use zip::ZipWriter;
    use zip::unstable::write::FileOptionsExt as _;
    use zip::write::SimpleFileOptions;

    fn temp_directory(label: &str) -> PathBuf {
        let path =
            std::env::temp_dir().join(format!("revaro-media-{label}-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&path).unwrap();
        path
    }

    fn zip_bytes(entries: &[(&str, &[u8])]) -> Vec<u8> {
        let mut writer = ZipWriter::new(Cursor::new(Vec::new()));
        for (name, data) in entries {
            writer
                .start_file(*name, SimpleFileOptions::default())
                .unwrap();
            writer.write_all(data).unwrap();
        }
        writer.finish().unwrap().into_inner()
    }

    fn encrypted_zip_bytes() -> Vec<u8> {
        let mut writer = ZipWriter::new(Cursor::new(Vec::new()));
        writer
            .start_file(
                "secret.txt",
                SimpleFileOptions::default().with_deprecated_encryption(b"secret"),
            )
            .unwrap();
        writer.write_all(b"encrypted archive").unwrap();
        writer.finish().unwrap().into_inner()
    }

    fn tar_bytes_with_entry(file_type: FileType, symlink: Option<&str>) -> Vec<u8> {
        let root = temp_directory("archive-entry");
        let source = root.join("source.tar");
        let mut archive = WriteArchive::new()
            .format(ArchiveFormat::TarPax)
            .open_file(&source)
            .unwrap();
        let mut entry = EntryMut::new();
        entry.set_pathname("entry").unwrap();
        entry.set_file_type(file_type);
        entry.set_size(0);
        entry.set_perm(0o644).unwrap();
        if let Some(target) = symlink {
            entry.set_symlink(target).unwrap();
        }
        archive.write_header(&entry).unwrap();
        archive.finish().unwrap();
        let bytes = fs::read(&source).unwrap();
        let _ = fs::remove_dir_all(root);
        bytes
    }

    #[test]
    fn extracts_a_real_zip_with_progress_and_bounds() {
        let root = temp_directory("archive");
        let source = root.join("source.zip");
        let output = root.join("output");
        fs::write(&source, zip_bytes(&[("dir/hello.txt", b"hello")])).unwrap();
        let mut progress = Vec::new();
        let result = ArchiveEngine
            .extract(
                &source,
                &output,
                None,
                1024,
                CancellationToken::new(),
                |value| progress.push(value),
            )
            .unwrap();
        assert_eq!(result.entries, 1);
        assert_eq!(result.expanded_bytes, 5);
        assert_eq!(fs::read(output.join("dir/hello.txt")).unwrap(), b"hello");
        assert!(
            progress
                .iter()
                .any(|item| item.phase == ArchivePhase::Checking)
        );
        assert!(
            progress
                .iter()
                .any(|item| item.phase == ArchivePhase::Extracting)
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn rejects_traversal_and_expansion_before_leaking_out_of_output() {
        let root = temp_directory("archive-safety");
        let source = root.join("source.zip");
        fs::write(&source, zip_bytes(&[("../escape.txt", b"no")])).unwrap();
        let error = ArchiveEngine
            .extract(
                &source,
                &root.join("output"),
                None,
                1024,
                CancellationToken::new(),
                |_| {},
            )
            .unwrap_err();
        assert!(
            matches!(error, ArchiveError::Input(message) if message == "unsafe archive path: \"../escape.txt\"")
        );
        assert!(!root.join("escape.txt").exists());

        fs::write(&source, zip_bytes(&[("large.txt", b"12345")])).unwrap();
        let error = ArchiveEngine
            .extract(
                &source,
                &root.join("small-output"),
                None,
                4,
                CancellationToken::new(),
                |_| {},
            )
            .unwrap_err();
        assert!(matches!(error, ArchiveError::ExpandedTooLarge));
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn cancellation_is_checked_before_opening_the_archive() {
        let root = temp_directory("archive-cancel");
        let source = root.join("source.zip");
        fs::write(&source, zip_bytes(&[("file.txt", b"data")])).unwrap();
        let cancel = CancellationToken::new();
        cancel.cancel();
        let error = ArchiveEngine
            .extract(&source, &root.join("output"), None, 1024, cancel, |_| {})
            .unwrap_err();
        assert!(matches!(error, ArchiveError::Cancelled));
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn password_protected_zip_requires_and_validates_a_password() {
        let root = temp_directory("archive-password");
        let source = root.join("source.zip");
        fs::write(&source, encrypted_zip_bytes()).unwrap();

        let error = ArchiveEngine
            .extract(
                &source,
                &root.join("without-password"),
                None,
                1024,
                CancellationToken::new(),
                |_| {},
            )
            .unwrap_err();
        assert!(matches!(error, ArchiveError::PasswordRequired));

        let error = ArchiveEngine
            .extract(
                &source,
                &root.join("wrong-password"),
                Some("wrong"),
                1024,
                CancellationToken::new(),
                |_| {},
            )
            .unwrap_err();
        assert!(matches!(error, ArchiveError::WrongPassword));

        let output = root.join("with-password");
        ArchiveEngine
            .extract(
                &source,
                &output,
                Some("secret"),
                1024,
                CancellationToken::new(),
                |_| {},
            )
            .unwrap();
        assert_eq!(
            fs::read(output.join("secret.txt")).unwrap(),
            b"encrypted archive"
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn refuses_links_and_special_archive_entries() {
        let root = temp_directory("archive-entry-safety");
        let source = root.join("source.tar");
        fs::write(
            &source,
            tar_bytes_with_entry(FileType::SymbolicLink, Some("outside")),
        )
        .unwrap();
        let error = ArchiveEngine
            .extract(
                &source,
                &root.join("link-output"),
                None,
                1024,
                CancellationToken::new(),
                |_| {},
            )
            .unwrap_err();
        assert!(matches!(error, ArchiveError::LinksNotAllowed));

        fs::write(&source, tar_bytes_with_entry(FileType::Fifo, None)).unwrap();
        let error = ArchiveEngine
            .extract(
                &source,
                &root.join("special-output"),
                None,
                1024,
                CancellationToken::new(),
                |_| {},
            )
            .unwrap_err();
        assert!(matches!(error, ArchiveError::UnsupportedSpecial));
        let _ = fs::remove_dir_all(root);
    }

    #[cfg(unix)]
    #[test]
    fn refuses_a_symlink_already_present_below_the_output_directory() {
        use std::os::unix::fs::symlink;

        let root = temp_directory("archive-output-link");
        let source = root.join("source.zip");
        fs::write(&source, zip_bytes(&[("dir/file.txt", b"data")])).unwrap();
        let output = root.join("output");
        let outside = root.join("outside");
        fs::create_dir_all(&output).unwrap();
        fs::create_dir_all(&outside).unwrap();
        symlink(&outside, output.join("dir")).unwrap();

        let error = ArchiveEngine
            .extract(
                &source,
                &output,
                None,
                1024,
                CancellationToken::new(),
                |_| {},
            )
            .unwrap_err();
        assert!(matches!(error, ArchiveError::Output(_)));
        assert!(!outside.join("file.txt").exists());
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn expanded_limit_preserves_absolute_ceiling() {
        assert_eq!(expanded_limit(1), 4 << 30);
        assert_eq!(expanded_limit((4 << 30) / 100), 4 << 30);
        assert_eq!(expanded_limit(1 << 30), MAX_ARCHIVE_EXPANDED_BYTES);
    }
}

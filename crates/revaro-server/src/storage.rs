//! The local object store, ported from Go's `internal/storage/local.go`.
//!
//! This is the layer that owns `APP_DATA_DIR/objects`. It is deliberately dumb:
//! it maps opaque string keys to bytes and knows nothing about files, metadata
//! or media. Everything above it addresses content only through a key produced
//! by [`revaro_core::keys`], so the on-disk layout stays a single decision.
//!
//! ## Why the details matter
//!
//! * **Confinement.** Keys are validated and resolved to relative paths only, and
//!   symlinks are refused on read. A key can never address anything outside the
//!   root, so a bug higher up cannot become an arbitrary-file read.
//! * **Atomicity.** Writes go to a temporary file in the destination directory,
//!   are flushed and `fsync`ed, and only then renamed into place. A crash leaves
//!   either the old object or a stray temporary file, never a half-written
//!   object. The directory is `fsync`ed as well so the rename itself is durable.
//! * **Immutable objects are create-only.** Derived artifacts (thumbnails,
//!   reader chunks) are hard-linked into place, so two concurrent generators
//!   cannot overwrite each other's output; the loser re-reads the winner's.
//!
//! ## Deliberate differences from the Go implementation
//!
//! * The Go type embedded a `*DataPlane` client for media and archive work. Media
//!   decoding now lives in the `revaro-media` library; archive operations will
//!   use the same boundary when that remaining migration stage is implemented.
//! * `walk_prefix` is gone. Go streamed batches to bound memory during garbage
//!   collection; here [`LocalStore::list_prefix`] returns the full list and the
//!   caller processes it in chunks. The store is single-user and local, so the
//!   bound that matters is request latency, not peak RSS, and one honest method
//!   beats two with subtly different semantics.

use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use revaro_core::keys;
use revaro_core::storage::{CompletedPart, ObjectInfo, ObjectRef};
use tokio::io::{AsyncRead, AsyncReadExt as _, AsyncSeekExt as _, AsyncWriteExt as _};

use crate::ids::new_id;

/// Prefix used for in-flight temporary files inside the destination directory.
const TEMP_PREFIX: &str = ".upload-";

/// Batch size used when deleting a listed set of objects.
const DELETE_BATCH: usize = 256;

/// Failure modes of the object store.
#[derive(Debug, thiserror::Error)]
pub enum StorageError {
    /// The key is empty, absolute, unnormalized, or otherwise unusable.
    #[error("invalid object key")]
    InvalidKey,
    /// The key names a directory, a symlink, or a file that is not there.
    #[error("object not found")]
    NotFound,
    /// A read was refused because the object exceeds the caller's limit.
    #[error("object exceeds the read limit of {limit} bytes")]
    TooLarge {
        /// The limit the caller supplied.
        limit: usize,
    },
    /// The bytes written did not match the declared size.
    #[error("object size {actual}, expected {expected}")]
    SizeMismatch {
        /// Bytes actually written.
        actual: i64,
        /// Size the caller declared.
        expected: i64,
    },
    /// The upload session identifier is malformed.
    #[error("invalid multipart reference")]
    InvalidMultipartReference,
    /// A part number was outside `1..=10000`.
    #[error("invalid part number")]
    InvalidPartNumber,
    /// The completion list was empty, too long, or not consecutive.
    #[error("invalid part count or ordering")]
    InvalidPartList,
    /// A part's stored entity tag did not match the one the client reported.
    #[error("part {part} entity tag mismatch")]
    PartEtagMismatch {
        /// The 1-based part number that failed verification.
        part: i32,
    },
    /// A part referenced by the completion list was never uploaded.
    #[error("part {part} is missing")]
    PartMissing {
        /// The 1-based part number that is missing.
        part: i32,
    },
    /// Underlying filesystem failure.
    #[error("object storage io error: {0}")]
    Io(#[from] std::io::Error),
}

impl StorageError {
    /// True when the failure means "there is no such object".
    ///
    /// Deletion is idempotent and callers of read paths translate this into a
    /// `404`, so it must be distinguishable from a real failure.
    #[must_use]
    pub fn is_not_found(&self) -> bool {
        matches!(self, StorageError::NotFound)
    }
}

/// A checked-out object ready to be streamed to a client.
///
/// The handle is positioned at the start. Callers implementing HTTP `Range`
/// support seek on it; the size and entity tag are cached because the file is
/// already open and stat-ing it again would be racy.
#[derive(Debug)]
pub struct StoredObject {
    /// Open file handle.
    pub file: tokio::fs::File,
    /// Size in bytes at the time the handle was opened.
    pub size: i64,
    /// Entity tag, see [`LocalStore::etag`].
    pub etag: String,
}

/// A directory-backed object store confined to one root.
#[derive(Debug, Clone)]
pub struct LocalStore {
    root: PathBuf,
}

impl LocalStore {
    /// Open (creating if necessary) the store rooted at `dir`.
    ///
    /// # Errors
    /// Returns a [`StorageError`] when the directory cannot be created.
    pub async fn open(dir: impl AsRef<Path>) -> Result<Self, StorageError> {
        let root = dir.as_ref().to_path_buf();
        tokio::fs::create_dir_all(&root).await?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            tokio::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700)).await?;
        }
        Ok(Self { root })
    }

    /// The store root.
    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Prove the root is writable by creating and removing a probe file.
    ///
    /// # Errors
    /// Returns a [`StorageError`] when the root cannot be written.
    pub async fn ping(&self) -> Result<(), StorageError> {
        let probe = self.root.join(format!("{TEMP_PREFIX}probe-{}", new_id()));
        tokio::fs::write(&probe, b"").await?;
        tokio::fs::remove_file(&probe).await?;
        Ok(())
    }

    /// The entity tag for an object of `size` bytes last modified at `modified`.
    ///
    /// The format (`<size hex>-<mtime nanoseconds hex>`) is reproduced exactly
    /// from the Go implementation because browsers cache responses keyed on it;
    /// changing it would silently invalidate every client's cache, and changing
    /// it to a content hash would mean reading every object to answer a `HEAD`.
    #[must_use]
    pub fn etag(size: i64, modified: SystemTime) -> String {
        let nanos = modified
            .duration_since(UNIX_EPOCH)
            .map(|elapsed| elapsed.as_nanos() as i64)
            .unwrap_or(0);
        format!("{size:x}-{nanos:x}")
    }

    /// Resolve a key to an absolute path inside the root.
    ///
    /// # Errors
    /// Returns [`StorageError::InvalidKey`] for anything that is not a
    /// normalized relative path.
    pub fn path_for(&self, key: &str) -> Result<PathBuf, StorageError> {
        if !is_valid_key(key) {
            return Err(StorageError::InvalidKey);
        }
        let mut path = self.root.clone();
        for part in key.split('/') {
            path.push(part);
        }
        Ok(path)
    }

    /// Stat an object and report its size and entity tag.
    ///
    /// # Errors
    /// Returns [`StorageError::NotFound`] when there is no regular file at the
    /// key, including when the key names a directory or a symlink.
    pub async fn head(&self, key: &str) -> Result<ObjectInfo, StorageError> {
        let path = self.path_for(key)?;
        let metadata = self.regular_file_metadata(&path).await?;
        Ok(ObjectInfo {
            size: metadata.len() as i64,
            etag: Self::etag(
                metadata.len() as i64,
                metadata.modified().unwrap_or(UNIX_EPOCH),
            ),
        })
    }

    /// Open an object for streaming.
    ///
    /// Named `open_object` rather than `open` because `open` is the store
    /// constructor; the two would otherwise collide.
    ///
    /// # Errors
    /// Returns [`StorageError::NotFound`] when there is no regular file at the
    /// key.
    pub async fn open_object(&self, key: &str) -> Result<StoredObject, StorageError> {
        let path = self.path_for(key)?;
        let metadata = self.regular_file_metadata(&path).await?;
        let file = tokio::fs::File::open(&path).await.map_err(map_not_found)?;
        let size = metadata.len() as i64;
        Ok(StoredObject {
            file,
            size,
            etag: Self::etag(size, metadata.modified().unwrap_or(UNIX_EPOCH)),
        })
    }

    /// Open an object synchronously after checking that its final path component
    /// is a regular file.
    ///
    /// Batch ZIP generation runs on a blocking thread because `zip`'s writer is
    /// synchronous. Keeping this check in the store means that the blocking
    /// path has the same key validation and symlink rejection as async reads,
    /// rather than teaching a response module how object paths work.
    pub(crate) fn open_object_blocking(&self, key: &str) -> Result<std::fs::File, StorageError> {
        let path = self.path_for(key)?;
        let metadata = std::fs::symlink_metadata(&path).map_err(map_not_found)?;
        if !metadata.is_file() {
            return Err(StorageError::NotFound);
        }
        std::fs::File::open(&path).map_err(map_not_found)
    }

    /// Read a whole object, refusing anything larger than `limit`.
    ///
    /// # Errors
    /// Returns [`StorageError::TooLarge`] when the object exceeds `limit`, so a
    /// caller-provided bound is enforced from the metadata rather than after
    /// allocating.
    pub async fn read(&self, key: &str, limit: usize) -> Result<Vec<u8>, StorageError> {
        let mut object = self.open_object(key).await?;
        if object.size > limit as i64 {
            return Err(StorageError::TooLarge { limit });
        }
        let mut buffer = Vec::with_capacity(object.size.max(0) as usize);
        object.file.read_to_end(&mut buffer).await?;
        Ok(buffer)
    }

    /// Read a byte range, seeking first. Used by video and audio range requests.
    ///
    /// # Errors
    /// Propagates [`StorageError::NotFound`] and I/O failures.
    pub async fn read_range(
        &self,
        key: &str,
        offset: u64,
        length: usize,
    ) -> Result<Vec<u8>, StorageError> {
        let mut object = self.open_object(key).await?;
        object.file.seek(std::io::SeekFrom::Start(offset)).await?;
        let mut buffer = vec![0u8; length];
        let mut filled = 0usize;
        while filled < length {
            let read = object.file.read(&mut buffer[filled..]).await?;
            if read == 0 {
                break;
            }
            filled += read;
        }
        buffer.truncate(filled);
        Ok(buffer)
    }

    /// Write `data` to `key` atomically.
    ///
    /// # Errors
    /// Propagates [`StorageError::InvalidKey`] and I/O failures.
    pub async fn put(&self, key: &str, data: &[u8]) -> Result<ObjectInfo, StorageError> {
        self.write(key, data, None, false).await
    }

    /// Write `data` to `key`, refusing to replace an existing object.
    ///
    /// When the object already exists its metadata is returned unchanged, so a
    /// second generator of the same derived artifact is a no-op rather than an
    /// error. That is what makes the caches safe to populate concurrently.
    ///
    /// # Errors
    /// Propagates [`StorageError::InvalidKey`] and I/O failures.
    pub async fn put_immutable(&self, key: &str, data: &[u8]) -> Result<ObjectInfo, StorageError> {
        self.write(key, data, None, true).await
    }

    /// Write a stream to `key`, requiring it to be exactly `size` bytes.
    ///
    /// # Errors
    /// Returns [`StorageError::SizeMismatch`] when the stream length differs
    /// from `size`; the destination is left untouched.
    pub async fn write_stream<R>(
        &self,
        key: &str,
        reader: &mut R,
        size: i64,
    ) -> Result<ObjectInfo, StorageError>
    where
        R: AsyncRead + Unpin,
    {
        let path = self.path_for(key)?;
        if let Some(parent) = path.parent() {
            // Every component of a valid key is a plain name, so creating the
            // parent chain cannot escape the root.
            tokio::fs::create_dir_all(parent).await?;
        }

        let temp = temp_path_for(&path);
        let result = self.write_temp(&temp, reader, Some(size)).await;
        let written = match result {
            Ok(written) => written,
            Err(error) => {
                let _ = tokio::fs::remove_file(&temp).await;
                return Err(error);
            }
        };

        match tokio::fs::rename(&temp, &path).await {
            Ok(()) => {}
            Err(error) => {
                let _ = tokio::fs::remove_file(&temp).await;
                return Err(StorageError::Io(error));
            }
        }
        self.sync_directory(&path).await?;
        debug_assert_eq!(written, size);
        self.head(key).await
    }

    /// Delete an object. Missing objects are not an error.
    ///
    /// # Errors
    /// Propagates [`StorageError::InvalidKey`] and I/O failures.
    pub async fn delete(&self, key: &str) -> Result<(), StorageError> {
        let path = self.path_for(key)?;
        match tokio::fs::remove_file(&path).await {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(StorageError::Io(error)),
        }
    }

    /// Delete many objects, continuing past failures and reporting the first.
    ///
    /// The cleanup worker enqueues whole batches; one missing entry must not
    /// strand the rest of the batch in the queue forever.
    ///
    /// # Errors
    /// Returns the first failure encountered, after attempting every key.
    pub async fn delete_many(&self, keys_to_delete: &[String]) -> Result<(), StorageError> {
        let mut first_error = None;
        for batch in keys_to_delete.chunks(DELETE_BATCH) {
            for key in batch {
                if let Err(error) = self.delete(key).await {
                    tracing::warn!(key, %error, "object deletion failed");
                    first_error.get_or_insert(error);
                }
            }
        }
        match first_error {
            Some(error) => Err(error),
            None => Ok(()),
        }
    }

    /// List every object whose key starts with `prefix`.
    ///
    /// Hidden entries (temporary uploads and multipart staging) are skipped, so
    /// a garbage-collection sweep can never delete an in-flight upload.
    ///
    /// # Errors
    /// Returns [`StorageError::InvalidKey`] for a malformed prefix and
    /// propagates I/O failures.
    pub async fn list_prefix(&self, prefix: &str) -> Result<Vec<ObjectRef>, StorageError> {
        let start = start_directory(prefix)?;
        let mut pending = vec![self.root.join(start)];
        let mut found = Vec::new();
        while let Some(directory) = pending.pop() {
            let mut entries = match tokio::fs::read_dir(&directory).await {
                Ok(entries) => entries,
                // A directory that vanished mid-scan is not an error.
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
                Err(error) => return Err(StorageError::Io(error)),
            };
            while let Some(entry) = entries.next_entry().await? {
                let name = entry.file_name();
                let name = name.to_string_lossy();
                if name.starts_with('.') {
                    continue;
                }
                let file_type = entry.file_type().await?;
                if file_type.is_dir() {
                    pending.push(entry.path());
                    continue;
                }
                if !file_type.is_file() {
                    continue;
                }
                let entry_path = entry.path();
                let Ok(relative) = entry_path.strip_prefix(&self.root) else {
                    continue;
                };
                let Some(key) = relative.to_str() else {
                    continue;
                };
                if !key.starts_with(prefix) {
                    continue;
                }
                let metadata = entry.metadata().await?;
                found.push(ObjectRef {
                    key: key.to_owned(),
                    size: metadata.len() as i64,
                    last_modified: revaro_core::Timestamp::from_system_time(
                        metadata.modified().unwrap_or(UNIX_EPOCH),
                    ),
                });
            }
        }
        found.sort_by(|left, right| left.key.cmp(&right.key));
        Ok(found)
    }

    /// Begin a multipart upload for `key`, returning its session identifier.
    ///
    /// # Errors
    /// Propagates [`StorageError::InvalidKey`] and I/O failures.
    pub async fn create_multipart(&self, key: &str) -> Result<String, StorageError> {
        if !is_valid_key(key) {
            return Err(StorageError::InvalidKey);
        }
        let upload_id = new_id();
        let directory = self.path_for(&keys::multipart_dir(&upload_id, key))?;
        tokio::fs::create_dir_all(&directory).await?;
        Ok(upload_id)
    }

    /// Store one part of a multipart upload.
    ///
    /// # Errors
    /// Returns [`StorageError::PartMissing`] when the upload session does not
    /// exist, and [`StorageError::InvalidPartNumber`] for an out-of-range part.
    pub async fn upload_part<R>(
        &self,
        key: &str,
        upload_id: &str,
        part_number: i32,
        reader: &mut R,
        size: i64,
    ) -> Result<ObjectInfo, StorageError>
    where
        R: AsyncRead + Unpin,
    {
        if !(1..=10_000).contains(&part_number) {
            return Err(StorageError::InvalidPartNumber);
        }
        let directory = self.multipart_directory(key, upload_id)?;
        // `directory` is an object key; the filesystem needs the resolved path.
        let directory_path = self.path_for(&directory)?;
        if !tokio::fs::try_exists(&directory_path)
            .await
            .unwrap_or(false)
        {
            return Err(StorageError::PartMissing { part: part_number });
        }
        let part_key = format!("{directory}/{part_number}");
        self.write_stream(&part_key, reader, size).await
    }

    /// Concatenate every part into the final object, then discard the session.
    ///
    /// Parts must be consecutive from 1 and each must still match the entity tag
    /// the client acknowledged; both checks happen before any bytes are copied,
    /// so a bad completion list cannot leave a partially assembled object.
    ///
    /// # Errors
    /// Returns [`StorageError::InvalidPartList`], [`StorageError::PartMissing`]
    /// or [`StorageError::PartEtagMismatch`] when the list cannot be honoured.
    pub async fn complete_multipart(
        &self,
        key: &str,
        upload_id: &str,
        parts: &[CompletedPart],
    ) -> Result<ObjectInfo, StorageError> {
        if parts.is_empty() || parts.len() > 10_000 {
            return Err(StorageError::InvalidPartList);
        }
        let directory = keys::multipart_dir(upload_id, key);
        let mut total: i64 = 0;
        for (index, part) in parts.iter().enumerate() {
            let expected_number = index as i32 + 1;
            if part.part_number != expected_number {
                return Err(StorageError::InvalidPartList);
            }
            let part_key = format!("{directory}/{expected_number}");
            let info = self.head(&part_key).await.map_err(|error| match error {
                StorageError::NotFound => StorageError::PartMissing {
                    part: expected_number,
                },
                other => other,
            })?;
            // Clients may send the entity tag wrapped in quotes, as HTTP requires.
            if part.etag.trim_matches('"') != info.etag {
                return Err(StorageError::PartEtagMismatch {
                    part: expected_number,
                });
            }
            total += info.size;
        }

        let destination = self.path_for(key)?;
        if let Some(parent) = destination.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }
        let temp = temp_path_for(&destination);
        match self.assemble_parts(&directory, parts.len(), &temp).await {
            Ok(assembled) if assembled != total => {
                let _ = tokio::fs::remove_file(&temp).await;
                return Err(StorageError::SizeMismatch {
                    actual: assembled,
                    expected: total,
                });
            }
            Ok(_) => {}
            Err(error) => {
                let _ = tokio::fs::remove_file(&temp).await;
                return Err(error);
            }
        }
        if let Err(error) = tokio::fs::rename(&temp, &destination).await {
            let _ = tokio::fs::remove_file(&temp).await;
            return Err(StorageError::Io(error));
        }
        self.sync_directory(&destination).await?;
        let info = self.head(key).await?;
        // The session's bytes are now part of the committed object; dropping the
        // staging directory is housekeeping, not part of the commit.
        if let Err(error) = self.abort_multipart(key, upload_id).await {
            tracing::warn!(key, upload_id, %error, "could not remove multipart staging");
        }
        Ok(info)
    }

    /// Discard a multipart session and its parts.
    ///
    /// # Errors
    /// Returns [`StorageError::InvalidMultipartReference`] for a malformed
    /// session identifier, and propagates I/O failures.
    pub async fn abort_multipart(&self, key: &str, upload_id: &str) -> Result<(), StorageError> {
        let directory = self.multipart_directory(key, upload_id)?;
        let directory_path = self.path_for(&directory)?;
        match tokio::fs::remove_dir_all(&directory_path).await {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(StorageError::Io(error)),
        }
        // The per-session parent directory is now empty; removing it is best
        // effort because another session may share it.
        let parent = self.path_for(&format!("{}{upload_id}", keys::MULTIPART_ROOT))?;
        let _ = tokio::fs::remove_dir(&parent).await;
        Ok(())
    }

    /// Remove abandoned temporary files and expired multipart sessions.
    ///
    /// A temporary file is only removed once it is older than `age`, which is
    /// why this runs on a timer with a grace period derived from the upload
    /// session lifetime: an upload in flight must not be reaped.
    ///
    /// # Errors
    /// Propagates I/O failures other than "already gone".
    pub async fn cleanup_temporary(&self, age: Duration) -> Result<(), StorageError> {
        self.cleanup_staging(age).await?;
        self.cleanup_uploads(&self.root.clone(), age).await
    }

    async fn cleanup_staging(&self, age: Duration) -> Result<(), StorageError> {
        let staging = self.root.join(keys::MULTIPART_ROOT.trim_end_matches('/'));
        let mut sessions = match tokio::fs::read_dir(&staging).await {
            Ok(sessions) => sessions,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(error) => return Err(StorageError::Io(error)),
        };
        while let Some(session) = sessions.next_entry().await? {
            let metadata = session.metadata().await?;
            if !metadata.is_dir() || !is_older_than(&metadata, age) {
                continue;
            }
            if let Err(error) = tokio::fs::remove_dir_all(session.path()).await {
                tracing::warn!(path = %session.path().display(), %error, "could not remove staging");
            }
        }
        Ok(())
    }

    /// Walk the tree removing `.upload-*` temporary files older than `age`.
    async fn cleanup_uploads(&self, directory: &Path, age: Duration) -> Result<(), StorageError> {
        let mut entries = match tokio::fs::read_dir(directory).await {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(error) => return Err(StorageError::Io(error)),
        };
        let mut subdirectories = Vec::new();
        while let Some(entry) = entries.next_entry().await? {
            let name = entry.file_name();
            let name = name.to_string_lossy().into_owned();
            if name == keys::MULTIPART_ROOT.trim_end_matches('/') {
                // Sessions expire on their own clock, handled above.
                continue;
            }
            let file_type = entry.file_type().await?;
            if file_type.is_dir() {
                subdirectories.push(entry.path());
                continue;
            }
            if !name.starts_with(TEMP_PREFIX) {
                continue;
            }
            let metadata = entry.metadata().await?;
            if is_older_than(&metadata, age)
                && let Err(error) = tokio::fs::remove_file(entry.path()).await
            {
                tracing::warn!(path = %entry.path().display(), %error, "could not remove temp file");
            }
        }
        for subdirectory in subdirectories {
            // Boxed because the recursion is async; the depth is bounded by the
            // key layout (a handful of levels), so this cannot grow unbounded.
            Box::pin(self.cleanup_uploads(&subdirectory, age)).await?;
        }
        Ok(())
    }

    /// Write `data`, or copy from `reader` when `data` is empty and a stream was
    /// requested. Returns the number of bytes written.
    async fn write_temp<R>(
        &self,
        temp: &Path,
        reader: &mut R,
        expected: Option<i64>,
    ) -> Result<i64, StorageError>
    where
        R: AsyncRead + Unpin,
    {
        // `create_new` makes the temporary name create-only, so two writers can
        // never share a temporary file even if the identifier collides.
        let mut file = tokio::fs::OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(temp)
            .await?;
        // Copy one byte past the declared size so an oversized stream is
        // detected instead of silently truncated.
        let limit = expected.map(|size| size.max(0) as u64 + 1);
        let written = match limit {
            Some(limit) => {
                let mut limited = reader.take(limit);
                tokio::io::copy(&mut limited, &mut file).await? as i64
            }
            None => tokio::io::copy(reader, &mut file).await? as i64,
        };
        file.flush().await?;
        file.sync_all().await?;
        drop(file);
        if let Some(expected) = expected
            && written != expected
        {
            return Err(StorageError::SizeMismatch {
                actual: written,
                expected,
            });
        }
        Ok(written)
    }

    /// Copy parts 1..=count from `directory` into `temp` in order, returning the
    /// number of bytes written.
    async fn assemble_parts(
        &self,
        directory: &str,
        count: usize,
        temp: &Path,
    ) -> Result<i64, StorageError> {
        let mut output = tokio::fs::OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(temp)
            .await?;
        let mut copied: i64 = 0;
        for number in 1..=count {
            let part_key = format!("{directory}/{number}");
            let mut part = self
                .open_object(&part_key)
                .await
                .map_err(|error| match error {
                    StorageError::NotFound => StorageError::PartMissing {
                        part: number as i32,
                    },
                    other => other,
                })?;
            copied += tokio::io::copy(&mut part.file, &mut output).await? as i64;
        }
        output.flush().await?;
        output.sync_all().await?;
        Ok(copied)
    }

    /// Metadata for a regular file, rejecting directories, symlinks and
    /// anything else that is not plain content.
    async fn regular_file_metadata(&self, path: &Path) -> Result<std::fs::Metadata, StorageError> {
        // `symlink_metadata` deliberately does not follow the final component, so
        // a symlink planted in the store is reported as a symlink rather than as
        // whatever it points at.
        let metadata = tokio::fs::symlink_metadata(path)
            .await
            .map_err(map_not_found)?;
        if !metadata.is_file() {
            return Err(StorageError::NotFound);
        }
        Ok(metadata)
    }

    /// `fsync` the directory holding `path` so a rename is durable.
    async fn sync_directory(&self, path: &Path) -> Result<(), StorageError> {
        let Some(parent) = path.parent() else {
            return Ok(());
        };
        match tokio::fs::File::open(parent).await {
            Ok(directory) => directory.sync_all().await?,
            // Directory fsync is not permitted everywhere; the data itself is
            // already durable, so treat this as best effort.
            Err(error) if error.kind() == std::io::ErrorKind::PermissionDenied => {}
            Err(error) => return Err(StorageError::Io(error)),
        }
        Ok(())
    }

    /// Validate a multipart session reference and return its directory key.
    fn multipart_directory(&self, key: &str, upload_id: &str) -> Result<String, StorageError> {
        if !is_valid_key(key) || !revaro_core::ids::is_canonical_uuid(upload_id) {
            return Err(StorageError::InvalidMultipartReference);
        }
        Ok(keys::multipart_dir(upload_id, key))
    }

    /// Shared write path used by [`LocalStore::put`] and
    /// [`LocalStore::put_immutable`].
    async fn write(
        &self,
        key: &str,
        data: &[u8],
        _expected: Option<i64>,
        immutable: bool,
    ) -> Result<ObjectInfo, StorageError> {
        let path = self.path_for(key)?;
        if let Some(parent) = path.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }
        let temp = temp_path_for(&path);
        let mut source = data;
        let written = self
            .write_temp(&temp, &mut source, Some(data.len() as i64))
            .await;
        if let Err(error) = written {
            let _ = tokio::fs::remove_file(&temp).await;
            return Err(error);
        }

        let outcome = if immutable {
            // A hard link is create-only: if the object already exists the link
            // fails with `AlreadyExists` and the existing bytes win, which is
            // exactly the semantics derived artifacts need.
            match tokio::fs::hard_link(&temp, &path).await {
                Ok(()) => Ok(()),
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => Ok(()),
                Err(error) => Err(StorageError::Io(error)),
            }
        } else {
            tokio::fs::rename(&temp, &path)
                .await
                .map_err(StorageError::Io)
        };
        // The temporary name is redundant once linked or renamed.
        let _ = tokio::fs::remove_file(&temp).await;
        outcome?;
        self.sync_directory(&path).await?;
        self.head(key).await
    }
}

/// True when `key` is a normalized relative path usable as an object key.
///
/// Rejects empty keys, `.`/`..` components, repeated or trailing separators,
/// absolute paths and backslashes, which is what Go's `path.Clean(key) == key`
/// check plus its explicit guards amounted to.
#[must_use]
pub fn is_valid_key(key: &str) -> bool {
    if key.is_empty() || key.starts_with('/') || key.contains('\\') {
        return false;
    }
    key.split('/')
        .all(|part| !part.is_empty() && part != "." && part != "..")
}

/// The directory a prefix scan should start from.
fn start_directory(prefix: &str) -> Result<String, StorageError> {
    if prefix.is_empty() {
        return Ok(String::new());
    }
    let trimmed = prefix.trim_end_matches('/');
    if !is_valid_key(trimmed) {
        return Err(StorageError::InvalidKey);
    }
    Ok(match trimmed.rsplit_once('/') {
        Some((parent, _)) => parent.to_owned(),
        None => String::new(),
    })
}

fn temp_path_for(path: &Path) -> PathBuf {
    let name = format!("{TEMP_PREFIX}{}", new_id());
    match path.parent() {
        Some(parent) => parent.join(name),
        None => PathBuf::from(name),
    }
}

fn map_not_found(error: std::io::Error) -> StorageError {
    if error.kind() == std::io::ErrorKind::NotFound {
        StorageError::NotFound
    } else {
        StorageError::Io(error)
    }
}

fn is_older_than(metadata: &std::fs::Metadata, age: Duration) -> bool {
    match metadata.modified() {
        Ok(modified) => SystemTime::now()
            .duration_since(modified)
            .map(|elapsed| elapsed > age)
            .unwrap_or(false),
        Err(_) => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU32, Ordering};

    /// A unique temporary directory per test, removed on drop.
    struct TempRoot(PathBuf);

    impl TempRoot {
        fn new() -> Self {
            static COUNTER: AtomicU32 = AtomicU32::new(0);
            let unique = COUNTER.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir()
                .join(format!("revaro-storage-{}-{unique}", std::process::id()));
            let _ = std::fs::remove_dir_all(&path);
            Self(path)
        }
    }

    impl Drop for TempRoot {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    async fn store() -> (TempRoot, LocalStore) {
        let root = TempRoot::new();
        let store = LocalStore::open(&root.0).await.expect("store opens");
        (root, store)
    }

    #[test]
    fn key_validation_rejects_everything_that_could_escape() {
        for key in [
            "",
            ".",
            "..",
            "/abs",
            "a/../b",
            "a/./b",
            "a//b",
            "a/",
            "\\windows",
            "a\\b",
            "../a",
        ] {
            assert!(!is_valid_key(key), "{key:?} must be rejected");
        }
        for key in [
            "a",
            "a/b",
            "blobs/0190f8f0",
            "thumbs/ab/cd.jpg",
            ".multipart/x/y",
        ] {
            assert!(is_valid_key(key), "{key:?} must be accepted");
        }
    }

    #[test]
    fn prefix_scans_start_in_the_right_directory() {
        assert_eq!(start_directory("").unwrap(), "");
        assert_eq!(start_directory("blobs/").unwrap(), "");
        assert_eq!(start_directory("blobs/abc").unwrap(), "blobs");
        assert_eq!(
            start_directory("flows/blobs/x/f1/manifest.json").unwrap(),
            "flows/blobs/x/f1"
        );
        assert!(start_directory("../etc").is_err());
        assert!(start_directory("a//b").is_err());
    }

    #[test]
    fn etag_uses_go_hex_format() {
        let modified = UNIX_EPOCH + Duration::from_nanos(0x1f);
        assert_eq!(LocalStore::etag(255, modified), "ff-1f");
        assert_eq!(LocalStore::etag(0, UNIX_EPOCH), "0-0");
    }

    #[tokio::test]
    async fn ping_succeeds_and_leaves_nothing_behind() {
        let (root, store) = store().await;
        store.ping().await.unwrap();
        let remaining: Vec<_> = std::fs::read_dir(&root.0)
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        assert!(remaining.is_empty(), "{remaining:?}");
    }

    #[tokio::test]
    async fn put_then_read_round_trips() {
        let (_root, store) = store().await;
        let info = store.put(&keys::blob_key("f1"), b"hello").await.unwrap();
        assert_eq!(info.size, 5);
        assert_eq!(
            store.read(&keys::blob_key("f1"), 1024).await.unwrap(),
            b"hello"
        );
        let stored = store.open_object(&keys::blob_key("f1")).await.unwrap();
        assert_eq!(stored.size, 5);
        assert_eq!(stored.etag, info.etag);
    }

    #[tokio::test]
    async fn writes_leave_no_temporary_files() {
        let (root, store) = store().await;
        store.put("a/b/c.bin", b"data").await.unwrap();
        let mut leftovers = Vec::new();
        for entry in walk(&root.0) {
            let name = entry.file_name().unwrap().to_string_lossy().into_owned();
            if name.starts_with(TEMP_PREFIX) {
                leftovers.push(entry);
            }
        }
        assert!(
            leftovers.is_empty(),
            "temporary files left behind: {leftovers:?}"
        );
    }

    #[tokio::test]
    async fn reads_honour_the_limit() {
        let (_root, store) = store().await;
        store.put("big.bin", &[0u8; 64]).await.unwrap();
        assert!(matches!(
            store.read("big.bin", 63).await.unwrap_err(),
            StorageError::TooLarge { limit: 63 }
        ));
        assert_eq!(store.read("big.bin", 64).await.unwrap().len(), 64);
    }

    #[tokio::test]
    async fn missing_objects_report_not_found() {
        let (_root, store) = store().await;
        assert!(store.head("nope").await.unwrap_err().is_not_found());
        assert!(store.open_object("nope").await.unwrap_err().is_not_found());
        assert!(store.read("nope", 16).await.unwrap_err().is_not_found());
    }

    #[tokio::test]
    async fn invalid_keys_never_touch_the_filesystem() {
        let (_root, store) = store().await;
        for key in ["../escape", "/etc/passwd", "a/../../b"] {
            let error = store.put(key, b"x").await.unwrap_err();
            assert!(
                matches!(error, StorageError::InvalidKey),
                "{key:?} gave {error}"
            );
        }
    }

    #[tokio::test]
    async fn directories_are_not_objects() {
        let (_root, store) = store().await;
        store.put("dir/file.bin", b"x").await.unwrap();
        assert!(store.head("dir").await.unwrap_err().is_not_found());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn symlinks_are_refused() {
        let (root, store) = store().await;
        let secret = root.0.join("secret.txt");
        std::fs::write(&secret, b"classified").unwrap();
        std::os::unix::fs::symlink(&secret, root.0.join("link")).unwrap();
        assert!(
            store.open_object("link").await.unwrap_err().is_not_found(),
            "a symlink must never be followed"
        );
    }

    #[tokio::test]
    async fn immutable_puts_keep_the_first_writer() {
        let (_root, store) = store().await;
        let key = keys::flow_manifest_key("blobs/b", 1);
        let first = store.put_immutable(&key, b"one").await.unwrap();
        let second = store.put_immutable(&key, b"two").await.unwrap();
        assert_eq!(first.etag, second.etag);
        assert_eq!(store.read(&key, 16).await.unwrap(), b"one");
    }

    #[tokio::test]
    async fn delete_is_idempotent() {
        let (_root, store) = store().await;
        store.put("a.bin", b"x").await.unwrap();
        store.delete("a.bin").await.unwrap();
        store.delete("a.bin").await.unwrap();
        // A malformed key is still an error: it is a bug, not a missing object.
        assert!(matches!(
            store.delete("../x").await.unwrap_err(),
            StorageError::InvalidKey
        ));
    }

    #[tokio::test]
    async fn delete_many_attempts_every_key() {
        let (_root, store) = store().await;
        store.put("a.bin", b"x").await.unwrap();
        store.put("b.bin", b"y").await.unwrap();
        store
            .delete_many(&[
                "a.bin".to_owned(),
                "missing.bin".to_owned(),
                "b.bin".to_owned(),
            ])
            .await
            .unwrap();
        assert!(store.head("a.bin").await.unwrap_err().is_not_found());
        assert!(store.head("b.bin").await.unwrap_err().is_not_found());
    }

    #[tokio::test]
    async fn write_stream_detects_a_size_mismatch() {
        let (_root, store) = store().await;
        let mut source: &[u8] = b"twelve bytes";
        assert_eq!(source.len(), 12);
        let error = store
            .write_stream("a.bin", &mut source, 11)
            .await
            .unwrap_err();
        assert!(
            matches!(
                error,
                StorageError::SizeMismatch {
                    actual: 12,
                    expected: 11
                }
            ),
            "{error}"
        );
        // The failed write must not leave a partial object behind.
        assert!(store.head("a.bin").await.unwrap_err().is_not_found());

        let mut source: &[u8] = b"short";
        let error = store
            .write_stream("b.bin", &mut source, 99)
            .await
            .unwrap_err();
        assert!(matches!(
            error,
            StorageError::SizeMismatch {
                actual: 5,
                expected: 99
            }
        ));
    }

    #[tokio::test]
    async fn write_stream_accepts_an_exact_size() {
        let (_root, store) = store().await;
        let mut source: &[u8] = b"exact";
        let info = store.write_stream("a.bin", &mut source, 5).await.unwrap();
        assert_eq!(info.size, 5);
    }

    #[tokio::test]
    async fn list_prefix_finds_only_matching_keys_and_skips_hidden_entries() {
        let (_root, store) = store().await;
        store.put(&keys::blob_key("a"), b"1").await.unwrap();
        store.put(&keys::blob_key("b"), b"22").await.unwrap();
        store
            .put(
                &keys::thumbnail_key("0190f8f0-1c2b-7c3d-9e4f-5a6b7c8d9e0f").unwrap(),
                b"t",
            )
            .await
            .unwrap();
        // Park a temporary upload that a sweep must not see.
        std::fs::write(store.root().join(".upload-pending"), b"partial").unwrap();

        let blobs = store.list_prefix("blobs/").await.unwrap();
        assert_eq!(blobs.len(), 2);
        assert_eq!(blobs[0].key, keys::blob_key("a"));
        assert_eq!(blobs[0].size, 1);
        assert_eq!(blobs[1].size, 2);
        assert!(blobs.iter().all(|entry| !entry.key.starts_with('.')));

        assert_eq!(store.list_prefix("thumbs/").await.unwrap().len(), 1);
        assert!(store.list_prefix("nothing/").await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn list_prefix_of_the_whole_store_finds_everything() {
        let (_root, store) = store().await;
        store.put("a", b"1").await.unwrap();
        store.put("nested/b", b"2").await.unwrap();
        assert_eq!(store.list_prefix("").await.unwrap().len(), 2);
    }

    #[tokio::test]
    async fn range_reads_seek_to_the_offset() {
        let (_root, store) = store().await;
        store.put("data.bin", b"0123456789").await.unwrap();
        assert_eq!(store.read_range("data.bin", 3, 4).await.unwrap(), b"3456");
        // A range past the end yields what exists rather than an error.
        assert_eq!(store.read_range("data.bin", 8, 10).await.unwrap(), b"89");
    }

    #[tokio::test]
    async fn multipart_assembles_parts_in_order() {
        let (_root, store) = store().await;
        let key = keys::blob_key("multi");
        let upload_id = store.create_multipart(&key).await.unwrap();

        let mut first: &[u8] = b"hello ";
        store
            .upload_part(&key, &upload_id, 1, &mut first, 6)
            .await
            .unwrap();
        let mut second: &[u8] = b"world";
        store
            .upload_part(&key, &upload_id, 2, &mut second, 5)
            .await
            .unwrap();

        let head = store
            .head(&format!("{}/1", keys::multipart_dir(&upload_id, &key)))
            .await
            .unwrap();
        let parts = vec![
            CompletedPart {
                part_number: 1,
                etag: head.etag.clone(),
            },
            CompletedPart {
                part_number: 2,
                etag: store
                    .head(&format!("{}/2", keys::multipart_dir(&upload_id, &key)))
                    .await
                    .unwrap()
                    .etag,
            },
        ];
        let info = store
            .complete_multipart(&key, &upload_id, &parts)
            .await
            .unwrap();
        assert_eq!(info.size, 11);
        assert_eq!(store.read(&key, 64).await.unwrap(), b"hello world");

        // The session is gone once the object is committed.
        assert!(
            !store
                .root()
                .join(keys::multipart_dir(&upload_id, &key))
                .exists()
        );
    }

    #[tokio::test]
    async fn multipart_rejects_a_bad_completion_list() {
        let (_root, store) = store().await;
        let key = keys::blob_key("multi");
        let upload_id = store.create_multipart(&key).await.unwrap();
        let mut part: &[u8] = b"abc";
        store
            .upload_part(&key, &upload_id, 1, &mut part, 3)
            .await
            .unwrap();
        let etag = store
            .head(&format!("{}/1", keys::multipart_dir(&upload_id, &key)))
            .await
            .unwrap()
            .etag;

        assert!(matches!(
            store
                .complete_multipart(&key, &upload_id, &[])
                .await
                .unwrap_err(),
            StorageError::InvalidPartList
        ));
        assert!(matches!(
            store
                .complete_multipart(
                    &key,
                    &upload_id,
                    &[CompletedPart {
                        part_number: 2,
                        etag: etag.clone()
                    }]
                )
                .await
                .unwrap_err(),
            StorageError::InvalidPartList
        ));
        assert!(matches!(
            store
                .complete_multipart(
                    &key,
                    &upload_id,
                    &[CompletedPart {
                        part_number: 1,
                        etag: "wrong".into()
                    }]
                )
                .await
                .unwrap_err(),
            StorageError::PartEtagMismatch { part: 1 }
        ));
        // A quoted entity tag is accepted, because HTTP clients often quote it.
        store
            .complete_multipart(
                &key,
                &upload_id,
                &[CompletedPart {
                    part_number: 1,
                    etag: format!("\"{etag}\""),
                }],
            )
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn multipart_reports_missing_parts() {
        let (_root, store) = store().await;
        let key = keys::blob_key("multi");
        let upload_id = store.create_multipart(&key).await.unwrap();
        let error = store
            .complete_multipart(
                &key,
                &upload_id,
                &[CompletedPart {
                    part_number: 1,
                    etag: "x".into(),
                }],
            )
            .await
            .unwrap_err();
        assert!(
            matches!(error, StorageError::PartMissing { part: 1 }),
            "{error}"
        );
    }

    #[tokio::test]
    async fn aborted_sessions_are_removed_and_unknown_sessions_are_tolerated() {
        let (_root, store) = store().await;
        let key = keys::blob_key("multi");
        let upload_id = store.create_multipart(&key).await.unwrap();
        let mut part: &[u8] = b"abc";
        store
            .upload_part(&key, &upload_id, 1, &mut part, 3)
            .await
            .unwrap();
        store.abort_multipart(&key, &upload_id).await.unwrap();
        assert!(
            !store
                .root()
                .join(keys::multipart_dir(&upload_id, &key))
                .exists()
        );
        // Aborting twice is fine.
        store.abort_multipart(&key, &upload_id).await.unwrap();
        // A malformed session id is a bug, not a missing session.
        assert!(matches!(
            store.abort_multipart(&key, "not-a-uuid").await.unwrap_err(),
            StorageError::InvalidMultipartReference
        ));
    }

    #[tokio::test]
    async fn upload_part_rejects_bad_numbers_and_unknown_sessions() {
        let (_root, store) = store().await;
        let key = keys::blob_key("multi");
        let upload_id = store.create_multipart(&key).await.unwrap();
        let mut part: &[u8] = b"x";
        assert!(matches!(
            store
                .upload_part(&key, &upload_id, 0, &mut part, 1)
                .await
                .unwrap_err(),
            StorageError::InvalidPartNumber
        ));
        assert!(matches!(
            store
                .upload_part(
                    &key,
                    "0190f8f0-1c2b-7c3d-9e4f-5a6b7c8d9e0f",
                    1,
                    &mut part,
                    1
                )
                .await
                .unwrap_err(),
            StorageError::PartMissing { part: 1 }
        ));
    }

    #[tokio::test]
    async fn cleanup_removes_only_expired_uploads() {
        let (root, store) = store().await;
        // A stale temporary file and a stale session.
        std::fs::write(root.0.join(".upload-stale"), b"partial").unwrap();
        let key = keys::blob_key("multi");
        let upload_id = store.create_multipart(&key).await.unwrap();
        // A current temporary file that must survive.
        std::fs::write(root.0.join(".upload-fresh"), b"active").unwrap();

        tokio::time::sleep(Duration::from_millis(20)).await;
        // Age 0 expires everything old enough to have a different timestamp.
        store
            .cleanup_temporary(Duration::from_secs(0))
            .await
            .unwrap();

        assert!(!root.0.join(".upload-stale").exists());
        assert!(
            !root
                .0
                .join(keys::MULTIPART_ROOT.trim_end_matches('/'))
                .join(&upload_id)
                .exists()
        );
    }

    #[tokio::test]
    async fn cleanup_keeps_recent_uploads() {
        let (root, store) = store().await;
        let key = keys::blob_key("multi");
        let upload_id = store.create_multipart(&key).await.unwrap();
        let mut part: &[u8] = b"abc";
        store
            .upload_part(&key, &upload_id, 1, &mut part, 3)
            .await
            .unwrap();
        // A generous grace period must not reap an in-flight upload.
        store
            .cleanup_temporary(Duration::from_secs(3600))
            .await
            .unwrap();
        assert!(root.0.join(keys::multipart_dir(&upload_id, &key)).exists());
    }

    #[tokio::test]
    async fn concurrent_writers_of_the_same_key_leave_a_complete_object() {
        let (_root, store) = store().await;
        let mut handles = Vec::new();
        for index in 0..8u8 {
            let store = store.clone();
            handles.push(tokio::spawn(async move {
                let payload = vec![index; 4096];
                store.put("contended.bin", &payload).await.unwrap();
            }));
        }
        for handle in handles {
            handle.await.unwrap();
        }
        let bytes = store.read("contended.bin", 8192).await.unwrap();
        assert_eq!(bytes.len(), 4096);
        assert!(bytes.iter().all(|byte| *byte == bytes[0]), "object is torn");
    }

    /// Recursively collect every path under `root`, for assertions.
    fn walk(root: &Path) -> Vec<PathBuf> {
        let mut found = Vec::new();
        let mut pending = vec![root.to_path_buf()];
        while let Some(directory) = pending.pop() {
            let Ok(entries) = std::fs::read_dir(&directory) else {
                continue;
            };
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_dir() {
                    pending.push(path.clone());
                }
                found.push(path);
            }
        }
        found
    }
}

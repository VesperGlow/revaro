//! The HTTP API contract.
//!
//! These are the exact request and response bodies of the Revaro HTTP surface.
//! The Axum backend binds them as extractors and responses; the Leptos client
//! deserializes the same types, so a field rename cannot silently break one side
//! without failing the other's build.
//!
//! Request types set `deny_unknown_fields` to preserve the server's historical
//! behaviour of rejecting unknown JSON members with `400 invalid JSON request`.

use serde::de::Deserializer;
use serde::{Deserialize, Serialize};

use crate::classify::LibraryKind;
use crate::model::{
    File, LibraryCounts, LibraryItem, Task, UploadMode, UploadPart, UploadStatus as UploadState,
};
use crate::reader::{Anchor, FlowManifest, TocEntry};
use crate::storage::CompletedPart;
use crate::time::Timestamp;

fn deserialize_vec_or_default<'de, D, T>(deserializer: D) -> Result<Vec<T>, D::Error>
where
    D: Deserializer<'de>,
    T: Deserialize<'de>,
{
    Ok(Option::<Vec<T>>::deserialize(deserializer)?.unwrap_or_default())
}

/// Older browser callers treated an unset TOTP flag as falsey. Keep explicit
/// `null` equivalent to an omitted field while rejecting other malformed
/// scalar values.
fn deserialize_nullable_bool<'de, D>(deserializer: D) -> Result<bool, D::Error>
where
    D: Deserializer<'de>,
{
    Ok(Option::<bool>::deserialize(deserializer)?.unwrap_or(false))
}

/// Older browser callers treated an unset recovery-code count as an empty
/// count. Keep explicit `null` equivalent to an omitted field while rejecting
/// other malformed scalar values.
fn deserialize_nullable_i64<'de, D>(deserializer: D) -> Result<i64, D::Error>
where
    D: Deserializer<'de>,
{
    Ok(Option::<i64>::deserialize(deserializer)?.unwrap_or_default())
}

/// Older account callers treated an absent or `null` setup string as empty
/// while still entering the setup stage. Keep response decoding tolerant while
/// rejecting other malformed scalar types.
fn deserialize_nullable_string<'de, D>(deserializer: D) -> Result<String, D::Error>
where
    D: Deserializer<'de>,
{
    Ok(Option::<String>::deserialize(deserializer)?.unwrap_or_default())
}

/// The historical upload caller treated every mode other than the literal
/// `single` as multipart. Keep that response fallback without relaxing the
/// strict `FromStr` parser used for persisted upload rows.
fn deserialize_upload_mode_response<'de, D>(deserializer: D) -> Result<UploadMode, D::Error>
where
    D: Deserializer<'de>,
{
    let value = serde_json::Value::deserialize(deserializer)?;
    Ok(value
        .as_str()
        .and_then(|raw| raw.parse().ok())
        .unwrap_or(UploadMode::Multipart))
}

/// The historical resume caller only branched on `completed`; every other
/// status continued the upload using its mode. Treat a newer or malformed
/// response status as pending while retaining strict storage parsing.
fn deserialize_upload_status_response<'de, D>(deserializer: D) -> Result<UploadState, D::Error>
where
    D: Deserializer<'de>,
{
    let value = serde_json::Value::deserialize(deserializer)?;
    Ok(value
        .as_str()
        .and_then(|raw| raw.parse().ok())
        .unwrap_or(UploadState::Pending))
}

/// Health probe response.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Health {
    /// `ok` for liveness, `ready` for readiness.
    pub status: String,
}

/// Authentication, session and second-factor payloads.
pub mod auth {
    use super::*;
    use crate::model::Profile;

    /// `POST /api/auth/login`
    #[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
    #[serde(deny_unknown_fields)]
    pub struct LoginRequest {
        /// Login name.
        pub username: String,
        /// Plain-text password, verified against the stored Argon2id hash.
        pub password: String,
        /// TOTP authenticator or recovery code, empty on the first step.
        #[serde(default)]
        pub second_factor: String,
    }

    /// Body of a successful login and of `GET /api/auth/me`.
    pub type Session = Profile;

    /// `PATCH /api/auth/credentials`
    #[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
    #[serde(deny_unknown_fields)]
    pub struct ChangeCredentialsRequest {
        /// Current password, re-verified before the change.
        pub current_password: String,
        /// New login name.
        pub username: String,
        /// New password.
        pub password: String,
    }

    /// `PATCH /api/auth/password`
    #[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
    #[serde(deny_unknown_fields)]
    pub struct ChangePasswordRequest {
        /// Current password.
        pub current_password: String,
        /// New password.
        pub password: String,
    }

    /// `PATCH /api/profile/username`
    #[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
    #[serde(deny_unknown_fields)]
    pub struct ChangeUsernameRequest {
        /// New login name.
        pub username: String,
    }

    /// `PUT /api/profile/avatar`
    #[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
    #[serde(deny_unknown_fields)]
    pub struct AvatarRequest {
        /// A `data:image/...;base64,...` URL.
        pub data_url: String,
    }

    /// Request bodies that only re-verify the current password.
    #[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
    #[serde(deny_unknown_fields)]
    pub struct PasswordRequest {
        /// Current password.
        pub current_password: String,
    }

    /// Request bodies that re-verify the current password and a TOTP code.
    #[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
    #[serde(deny_unknown_fields)]
    pub struct PasswordCodeRequest {
        /// Current password.
        pub current_password: String,
        /// TOTP authenticator or recovery code.
        pub code: String,
    }

    /// `GET /api/auth/totp`
    #[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
    pub struct TotpStatus {
        /// Whether TOTP is enabled.
        #[serde(default, deserialize_with = "crate::api::deserialize_nullable_bool")]
        pub enabled: bool,
        /// Number of unused recovery codes.
        #[serde(default, deserialize_with = "crate::api::deserialize_nullable_i64")]
        pub recovery_codes: i64,
    }

    /// `POST /api/auth/totp/setup`
    #[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
    pub struct TotpSetup {
        /// Base32 shared secret.
        #[serde(default, deserialize_with = "crate::api::deserialize_nullable_string")]
        pub secret: String,
        /// `otpauth://` provisioning URI.
        #[serde(default, deserialize_with = "crate::api::deserialize_nullable_string")]
        pub uri: String,
        /// PNG QR code as a data URL.
        #[serde(default, deserialize_with = "crate::api::deserialize_nullable_string")]
        pub qr_data_url: String,
    }

    /// `POST /api/auth/totp/enable` and `.../recovery-codes`
    #[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
    pub struct TotpRecovery {
        /// Always `true` on success.
        pub enabled: bool,
        /// The freshly generated recovery codes.
        pub recovery_codes: Vec<String>,
    }
}

/// File, directory and document payloads.
pub mod files {
    use super::*;

    /// `POST /api/directories`
    #[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
    #[serde(deny_unknown_fields)]
    pub struct CreateDirectoryRequest {
        /// Owning directory.
        pub parent_id: String,
        /// New directory name.
        pub name: String,
    }

    /// `POST /api/documents`
    #[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
    #[serde(deny_unknown_fields)]
    pub struct CreateDocumentRequest {
        /// Owning directory.
        pub parent_id: String,
        /// New file name.
        pub name: String,
        /// Initial UTF-8 contents.
        pub content: String,
    }

    /// `PATCH /api/files/{id}`; both fields are optional, absent means unchanged.
    #[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
    #[serde(deny_unknown_fields)]
    pub struct PatchFileRequest {
        /// New name.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub name: Option<String>,
        /// New parent directory.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub parent_id: Option<String>,
    }

    /// `POST /api/files/{id}/copy`
    #[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
    #[serde(deny_unknown_fields)]
    pub struct CopyFileRequest {
        /// Destination directory.
        pub parent_id: String,
    }

    /// `GET /api/files/{id}`
    #[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
    pub struct FileDetail {
        /// The requested file.
        pub file: File,
        /// Path from the root, outermost first.
        pub breadcrumbs: Vec<File>,
    }

    /// `GET /api/files/{id}/children`
    #[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
    pub struct Children {
        /// Directory entries, directories first then case-insensitive by name.
        pub items: Vec<File>,
        /// Total bytes of the ready files directly inside.
        pub total_bytes: i64,
        /// Number of ready files directly inside.
        pub file_count: i64,
    }

    /// The subset returned to callers that only need directory entries.
    ///
    /// The historical browser used this narrow shape in the directory picker,
    /// upload-folder conflict recovery and sidebar tree. Those callers did not
    /// read the aggregate counters, so an older successful response may omit
    /// them while still being usable.
    #[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
    pub struct ChildItems {
        /// Directory entries, directories first then case-insensitive by name.
        pub items: Vec<File>,
    }

    /// `GET /api/files/{id}/content`
    #[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
    pub struct DocumentContent {
        /// UTF-8 contents.
        pub content: String,
        /// ETag the client must send back when writing.
        #[serde(default, deserialize_with = "crate::api::deserialize_nullable_string")]
        pub etag: String,
        /// Last modification time.
        #[serde(default)]
        pub updated_at: Timestamp,
    }

    /// `PUT /api/files/{id}/content`
    #[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
    #[serde(deny_unknown_fields)]
    pub struct UpdateDocumentRequest {
        /// New contents.
        pub content: String,
        /// ETag from the last read; a mismatch is a `409`.
        #[serde(default)]
        pub etag: String,
    }

    /// `GET /api/trash`
    #[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
    pub struct Trash {
        /// Trashed root items, most recently deleted first.
        pub items: Vec<File>,
        /// Bytes held by trashed files.
        pub total_bytes: i64,
        /// Number of trashed files.
        pub file_count: i64,
    }

    /// `POST /api/files/batch-download/prepare`
    #[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
    #[serde(deny_unknown_fields)]
    pub struct BatchDownloadRequest {
        /// Files to include in the archive.
        pub ids: Vec<String>,
    }

    /// Response of `POST /api/files/batch-download/prepare`.
    #[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
    pub struct BatchDownloadTicket {
        /// Opaque, short-lived token for the streaming download URL.
        pub token: String,
    }
}

/// Upload payloads.
pub mod uploads {
    use super::*;

    /// `POST /api/uploads`
    #[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
    #[serde(deny_unknown_fields)]
    pub struct CreateUploadRequest {
        /// Destination directory.
        pub parent_id: String,
        /// File name.
        pub name: String,
        /// Declared total size in bytes.
        pub size: i64,
        /// Declared MIME type; empty becomes `application/octet-stream`.
        #[serde(default)]
        pub mime_type: String,
    }

    /// Response of `POST /api/uploads`.
    #[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
    pub struct CreateUpload {
        /// Upload session id.
        pub upload_id: String,
        /// Placeholder file row created up front.
        #[serde(default)]
        pub file_id: String,
        /// Single-request or multipart transfer.
        #[serde(deserialize_with = "deserialize_upload_mode_response")]
        pub mode: UploadMode,
        /// Target URL, empty for multipart uploads.
        #[serde(default)]
        pub url: String,
        /// Part size the client must slice with.
        pub part_size: i64,
        /// Number of parts; `0` for single-request uploads.
        pub part_count: usize,
        /// Session expiry.
        #[serde(default)]
        pub expires_at: Timestamp,
    }

    /// Response of `GET /api/uploads/{id}`.
    #[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
    pub struct UploadStatus {
        /// Upload session id.
        pub upload_id: String,
        /// Placeholder file row.
        #[serde(default)]
        pub file_id: String,
        /// Single-request or multipart transfer.
        #[serde(deserialize_with = "deserialize_upload_mode_response")]
        pub mode: UploadMode,
        /// Target URL, empty for multipart uploads.
        #[serde(default)]
        pub url: String,
        /// Part size the client must slice with.
        pub part_size: i64,
        /// Number of parts; `0` for single-request uploads.
        pub part_count: usize,
        /// Declared total size.
        #[serde(default)]
        pub expected_size: i64,
        /// Declared MIME type.
        #[serde(default)]
        pub mime_type: String,
        /// Session state.
        #[serde(default, deserialize_with = "deserialize_upload_status_response")]
        pub status: UploadStatusKind,
        /// Session expiry.
        #[serde(default)]
        pub expires_at: Timestamp,
        /// Parts acknowledged so far.
        #[serde(default)]
        pub parts: Vec<UploadPart>,
    }

    /// Alias kept so the field type reads naturally at the call site.
    pub use crate::model::UploadStatus as UploadStatusKind;

    /// `POST /api/uploads/{id}/parts`
    #[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
    #[serde(deny_unknown_fields)]
    pub struct UploadPartsRequest {
        /// Part numbers to fetch signed, single-use URLs for.
        pub part_numbers: Vec<i32>,
    }

    /// One part URL inside [`UploadPartsResponse`].
    #[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
    pub struct PartUrl {
        /// 1-based part number.
        pub part_number: i32,
        /// Relative URL to `PUT` the part bytes to.
        pub url: String,
    }

    /// Response of `POST /api/uploads/{id}/parts`.
    #[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
    pub struct UploadPartsResponse {
        /// Requested part URLs.
        pub parts: Vec<PartUrl>,
    }

    /// `PUT /api/uploads/{id}/parts/{part}`
    #[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
    #[serde(deny_unknown_fields)]
    pub struct RecordUploadPartRequest {
        /// Entity tag the client observed while uploading the bytes.
        pub etag: String,
        /// Part size in bytes.
        pub size: i64,
        /// Optional per-part integrity hash.
        #[serde(default, skip_serializing_if = "String::is_empty")]
        pub content_hash: String,
    }

    /// `POST /api/uploads/{id}/complete`
    #[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
    #[serde(deny_unknown_fields)]
    pub struct CompleteUploadRequest {
        /// Every part, in ascending order; incomplete lists are refused.
        pub parts: Vec<CompletedPart>,
    }
}

/// Library aggregation payloads.
pub mod library {
    use super::*;

    /// Response of `GET /api/library`.
    #[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
    pub struct Library {
        /// The requested bucket.
        #[serde(rename = "type")]
        pub kind: LibraryKind,
        /// Matching files across every directory.
        pub items: Vec<LibraryItem>,
        /// Counts for every bucket.
        pub counts: LibraryCounts,
    }

    /// Response of `GET /api/library/all`.
    #[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
    pub struct LibraryAll {
        /// Items grouped by bucket.
        pub items: LibraryBuckets,
        /// Counts for every bucket.
        pub counts: LibraryCounts,
    }

    /// The four media buckets the sidebar tree uses.
    #[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
    pub struct LibraryBuckets {
        /// Books.
        #[serde(default, deserialize_with = "crate::api::deserialize_vec_or_default")]
        pub book: Vec<LibraryItem>,
        /// Images.
        #[serde(default, deserialize_with = "crate::api::deserialize_vec_or_default")]
        pub image: Vec<LibraryItem>,
        /// Videos.
        #[serde(default, deserialize_with = "crate::api::deserialize_vec_or_default")]
        pub video: Vec<LibraryItem>,
        /// Audio files.
        #[serde(default, deserialize_with = "crate::api::deserialize_vec_or_default")]
        pub audio: Vec<LibraryItem>,
    }
}

/// Task-centre payloads.
pub mod tasks {
    use super::*;

    /// Response of `GET /api/tasks`.
    #[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
    pub struct TaskList {
        /// Active tasks plus recently finished ones, newest first.
        pub items: Vec<Task>,
    }

    /// `POST /api/tasks/{id}/input`
    #[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
    #[serde(deny_unknown_fields)]
    pub struct TaskInputRequest {
        /// Archive password supplied by the user.
        pub password: String,
    }
}

/// Archive extraction payloads.
pub mod archive {
    use super::*;

    /// Extraction lifecycle as reported to the task centre.
    #[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
    #[serde(rename_all = "snake_case")]
    pub enum JobStatus {
        /// Waiting for a worker.
        #[default]
        Queued,
        /// Fetching the source archive.
        Downloading,
        /// Validating entries.
        Checking,
        /// Writing entries.
        Extracting,
        /// Committing metadata.
        Importing,
        /// Waiting for the user to supply a password.
        WaitingPassword,
        /// Finished successfully.
        Done,
        /// Finished unsuccessfully.
        Failed,
        /// Cancelled by the user or by process shutdown.
        Cancelled,
    }

    /// Snapshot returned by `POST /api/files/{id}/extract` and by task input.
    #[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
    pub struct Job {
        /// Job identifier.
        pub id: String,
        /// Source archive file.
        pub file_id: String,
        /// Directory the output is written into.
        pub parent_id: String,
        /// Source archive name.
        pub name: String,
        /// Current phase.
        pub status: JobStatus,
        /// Completion percentage.
        pub progress: i32,
        /// Human-readable progress message.
        pub message: String,
        /// Created output directory, omitted until committed.
        #[serde(default, skip_serializing_if = "String::is_empty")]
        pub output_id: String,
        /// Created output directory name, omitted until committed.
        #[serde(default, skip_serializing_if = "String::is_empty")]
        pub output_name: String,
        /// Failure message, omitted while healthy.
        #[serde(default, skip_serializing_if = "String::is_empty")]
        pub error: String,
        /// Creation time.
        pub created_at: Timestamp,
        /// Last state change.
        pub updated_at: Timestamp,
    }
}

/// Book and reader payloads.
pub mod book {
    use super::*;

    /// `GET /api/files/{id}/book`
    #[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
    pub struct Info {
        /// `epub` or `txt`.
        #[serde(default, deserialize_with = "crate::api::deserialize_nullable_string")]
        pub format: String,
        /// Book title as parsed from the source.
        #[serde(default, deserialize_with = "crate::api::deserialize_nullable_string")]
        pub title: String,
        /// File name on disk.
        #[serde(default, deserialize_with = "crate::api::deserialize_nullable_string")]
        pub name: String,
        /// Whether an embedded cover exists.
        #[serde(default, deserialize_with = "crate::api::deserialize_nullable_bool")]
        pub cover: bool,
        /// Table of contents.
        #[serde(default, deserialize_with = "crate::api::deserialize_vec_or_default")]
        pub toc: Vec<TocEntry>,
    }

    /// `PUT /api/files/{id}/book/progress`
    #[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
    #[serde(deny_unknown_fields)]
    pub struct SaveProgressRequest {
        /// Reading position. An empty object clears the saved position.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub anchor: Option<Anchor>,
    }

    /// Response of `GET /api/files/{id}/book/progress`.
    ///
    /// The response deliberately accepts the same empty shape as the write
    /// request: old installations return `{}` when no usable progress exists.
    #[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
    pub struct Progress {
        /// Saved reading position, omitted when it is absent or invalid.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub anchor: Option<Anchor>,
    }

    /// Response of `GET /api/files/{id}/book/flow`.
    pub type Flow = FlowManifest;
}

/// Media payloads.
pub mod media {
    pub use crate::media::{AudioMedia, VideoMedia};

    /// Response of `POST /api/files/{id}/media/reanalyze`.
    pub use crate::media::ReanalyzeResult;
}

/// System status payloads.
pub mod system {
    use super::*;
    use std::collections::BTreeMap;

    /// A component that only reports health and a byte count.
    #[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
    pub struct Component {
        /// `ok` or `degraded`.
        #[serde(default, deserialize_with = "crate::api::deserialize_nullable_string")]
        pub status: String,
        /// Size in bytes, omitted when zero.
        #[serde(default, skip_serializing_if = "is_zero")]
        pub bytes: i64,
    }

    /// Storage usage as the status panel reports it.
    #[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
    pub struct Storage {
        /// `ok` or `degraded`.
        #[serde(default, deserialize_with = "crate::api::deserialize_nullable_string")]
        pub status: String,
        /// Bytes held by ready, non-deleted files, counting copies separately.
        pub bytes: i64,
        /// Bytes held by trashed files.
        pub trash_bytes: i64,
        /// Number of ready files.
        pub file_count: i64,
    }

    /// Cache usage as the status panel reports it.
    #[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
    pub struct Cache {
        /// `ok` or `degraded`.
        #[serde(default, deserialize_with = "crate::api::deserialize_nullable_string")]
        pub status: String,
        /// Bytes held in memory.
        pub memory_bytes: i64,
        /// Bytes held on disk.
        pub disk_bytes: i64,
        /// Number of in-memory entries.
        pub memory_entries: i64,
        /// Number of on-disk entries.
        pub disk_entries: i64,
        /// Per-class counters, omitted when no classes are registered.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub classes: Option<BTreeMap<String, CacheClass>>,
    }

    /// Counters for one cache class.
    #[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
    pub struct CacheClass {
        /// Lookups served from cache.
        pub hits: i64,
        /// Lookups that missed.
        pub misses: i64,
        /// Values produced by a loader.
        pub loads: i64,
        /// Loader failures.
        pub load_errors: i64,
        /// Entries evicted under pressure.
        pub evictions: i64,
        /// Bytes held in memory, omitted when the class is disk-only.
        #[serde(default, skip_serializing_if = "is_zero")]
        pub memory_bytes: i64,
        /// In-memory entries, omitted when the class is disk-only.
        #[serde(default, skip_serializing_if = "is_zero")]
        pub memory_entries: i64,
        /// Bytes held on disk, omitted when the class is memory-only.
        #[serde(default, skip_serializing_if = "is_zero")]
        pub disk_bytes: i64,
        /// On-disk entries, omitted when the class is memory-only.
        #[serde(default, skip_serializing_if = "is_zero")]
        pub disk_entries: i64,
    }

    /// Response of `GET /api/system/status`.
    #[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
    pub struct Status {
        /// Overall health.
        #[serde(default, deserialize_with = "crate::api::deserialize_nullable_string")]
        pub status: String,
        /// Database component.
        pub database: Component,
        /// Storage component.
        pub storage: Storage,
        /// Cache component.
        pub cache: Cache,
    }

    fn is_zero(value: &i64) -> bool {
        *value == 0
    }
}

/// Sharing payloads.
pub mod share {
    use crate::model::ShareStatus;

    /// Response of `GET|POST|DELETE /api/files/{id}/share`.
    pub type Status = ShareStatus;
}

/// Playback progress payloads.
pub mod progress {
    use crate::model::MediaProgress;

    /// Response of `GET|PUT /api/files/{id}/media/progress`.
    pub type Media = MediaProgress;
}

/// Convenience re-exports of the payload modules.
pub use archive::Job as ArchiveJob;
pub use book::{
    Flow, Info as BookInfo, Progress as BookProgress,
    SaveProgressRequest as SaveBookProgressRequest,
};
pub use files::{
    BatchDownloadRequest, BatchDownloadTicket, ChildItems, Children, CopyFileRequest,
    CreateDirectoryRequest, CreateDocumentRequest, DocumentContent, FileDetail, PatchFileRequest,
    Trash, UpdateDocumentRequest,
};
pub use library::{Library, LibraryAll, LibraryBuckets};
pub use tasks::{TaskInputRequest, TaskList};
pub use uploads::{
    CompleteUploadRequest, CreateUpload, CreateUploadRequest, PartUrl, RecordUploadPartRequest,
    UploadPartsRequest, UploadPartsResponse, UploadStatus,
};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn login_request_rejects_unknown_members() {
        let ok: Result<auth::LoginRequest, _> =
            serde_json::from_str(r#"{"username":"a","password":"b","second_factor":"123456"}"#);
        assert!(ok.is_ok());
        let err = serde_json::from_str::<auth::LoginRequest>(
            r#"{"username":"a","password":"b","extra":1}"#,
        );
        assert!(err.is_err());
    }

    #[test]
    fn login_request_defaults_second_factor() {
        let request: auth::LoginRequest =
            serde_json::from_str(r#"{"username":"a","password":"b"}"#).unwrap();
        assert_eq!(request.second_factor, "");
    }

    #[test]
    fn totp_status_treats_nullable_fields_as_disabled_and_empty() {
        let status: auth::TotpStatus = serde_json::from_value(serde_json::json!({
            "enabled": null,
            "recovery_codes": null
        }))
        .unwrap();
        assert!(!status.enabled);
        assert_eq!(status.recovery_codes, 0);
    }

    #[test]
    fn totp_setup_treats_nullable_metadata_as_empty() {
        let setup: auth::TotpSetup = serde_json::from_value(serde_json::json!({
            "secret": "JBSWY3DPEHPK3PXP",
            "uri": null,
            "qr_data_url": null
        }))
        .unwrap();
        assert_eq!(setup.secret, "JBSWY3DPEHPK3PXP");
        assert!(setup.uri.is_empty());
        assert!(setup.qr_data_url.is_empty());
    }

    #[test]
    fn book_progress_allows_an_empty_write_and_omits_an_empty_anchor() {
        let request: book::SaveProgressRequest = serde_json::from_str("{}").unwrap();
        assert_eq!(request.anchor, None);
        assert_eq!(
            serde_json::to_value(&request).unwrap(),
            serde_json::json!({})
        );

        let response = book::Progress::default();
        assert_eq!(
            serde_json::to_value(response).unwrap(),
            serde_json::json!({})
        );
    }

    #[test]
    fn book_progress_write_still_rejects_unknown_members() {
        let error = serde_json::from_str::<book::SaveProgressRequest>(
            r#"{"anchor":null,"unexpected":true}"#,
        );
        assert!(error.is_err());
    }

    #[test]
    fn document_content_treats_a_nullable_etag_as_empty() {
        let content: DocumentContent = serde_json::from_value(serde_json::json!({
            "content": "# body\n",
            "etag": null
        }))
        .unwrap();
        assert_eq!(content.content, "# body\n");
        assert!(content.etag.is_empty());

        let invalid = serde_json::from_value::<DocumentContent>(serde_json::json!({
            "content": "# body\n",
            "etag": 42
        }));
        assert!(invalid.is_err());
    }

    #[test]
    fn book_info_treats_nullable_metadata_as_defaults() {
        let info: book::Info = serde_json::from_value(serde_json::json!({
            "format": null,
            "title": null,
            "name": "book.epub",
            "cover": null,
            "toc": null
        }))
        .unwrap();
        assert!(info.format.is_empty());
        assert!(info.title.is_empty());
        assert_eq!(info.name, "book.epub");
        assert!(!info.cover);
        assert!(info.toc.is_empty());

        let invalid = serde_json::from_value::<book::Info>(serde_json::json!({
            "format": 1
        }));
        assert!(invalid.is_err());
    }

    #[test]
    fn patch_file_distinguishes_absent_from_explicit() {
        let patch: PatchFileRequest = serde_json::from_str(r#"{"name":"new.txt"}"#).unwrap();
        assert_eq!(patch.name.as_deref(), Some("new.txt"));
        assert_eq!(patch.parent_id, None);
        let json = serde_json::to_value(&patch).unwrap();
        assert_eq!(json, serde_json::json!({"name": "new.txt"}));
    }

    #[test]
    fn library_response_uses_the_type_key() {
        let body = Library {
            kind: LibraryKind::Video,
            items: Vec::new(),
            counts: LibraryCounts::default(),
        };
        let json = serde_json::to_value(&body).unwrap();
        assert_eq!(json["type"], "video");
        assert_eq!(json["items"], serde_json::json!([]));
        assert_eq!(json["counts"]["book"], 0);
    }

    #[test]
    fn library_all_groups_the_four_media_buckets() {
        let body = LibraryAll::default();
        let json = serde_json::to_value(&body).unwrap();
        for bucket in ["book", "image", "video", "audio"] {
            assert_eq!(json["items"][bucket], serde_json::json!([]), "{bucket}");
        }
    }

    #[test]
    fn library_all_treats_missing_or_null_media_buckets_as_empty() {
        let counts = serde_json::to_value(LibraryCounts::default()).unwrap();
        let missing: LibraryAll = serde_json::from_value(serde_json::json!({
            "items": {
                "book": [],
                "image": [],
                "video": []
            },
            "counts": counts
        }))
        .unwrap();
        assert!(missing.items.audio.is_empty());

        let null: LibraryAll = serde_json::from_value(serde_json::json!({
            "items": {
                "book": null,
                "image": [],
                "video": [],
                "audio": null
            },
            "counts": serde_json::to_value(LibraryCounts::default()).unwrap()
        }))
        .unwrap();
        assert!(null.items.book.is_empty());
        assert!(null.items.audio.is_empty());
    }

    #[test]
    fn create_upload_response_matches_the_client_contract() {
        let body = CreateUpload {
            upload_id: "u".into(),
            file_id: "f".into(),
            mode: UploadMode::Multipart,
            url: String::new(),
            part_size: 16 << 20,
            part_count: 3,
            expires_at: Timestamp::parse("2024-05-06T07:08:09Z").unwrap(),
        };
        let json = serde_json::to_value(&body).unwrap();
        assert_eq!(json["mode"], "multipart");
        assert_eq!(json["url"], "");
        assert_eq!(json["expires_at"], "2024-05-06T07:08:09Z");
    }

    #[test]
    fn create_upload_accepts_a_missing_optional_url() {
        let body: CreateUpload = serde_json::from_value(serde_json::json!({
            "upload_id": "u",
            "mode": "single",
            "part_size": 16,
            "part_count": 0
        }))
        .unwrap();
        assert!(body.url.is_empty());
        assert!(body.file_id.is_empty());
        assert_eq!(body.expires_at, Timestamp::default());
    }

    #[test]
    fn upload_responses_treat_unknown_modes_as_multipart_without_relaxing_storage_parsing() {
        let created: CreateUpload = serde_json::from_value(serde_json::json!({
            "upload_id": "u",
            "mode": "future_mode",
            "part_size": 16,
            "part_count": 2
        }))
        .unwrap();
        assert_eq!(created.mode, UploadMode::Multipart);

        let status: UploadStatus = serde_json::from_value(serde_json::json!({
            "upload_id": "u",
            "mode": null,
            "part_size": 16,
            "part_count": 2
        }))
        .unwrap();
        assert_eq!(status.mode, UploadMode::Multipart);
        assert!("future_mode".parse::<UploadMode>().is_err());
    }

    #[test]
    fn upload_resume_responses_treat_unknown_statuses_as_pending_without_relaxing_storage_parsing()
    {
        let status: UploadStatus = serde_json::from_value(serde_json::json!({
            "upload_id": "u",
            "mode": "single",
            "status": "future_status",
            "part_size": 16,
            "part_count": 1
        }))
        .unwrap();
        assert_eq!(status.status, crate::model::UploadStatus::Pending);

        let null: UploadStatus = serde_json::from_value(serde_json::json!({
            "upload_id": "u",
            "mode": "single",
            "status": null,
            "part_size": 16,
            "part_count": 1
        }))
        .unwrap();
        assert_eq!(null.status, crate::model::UploadStatus::Pending);
        assert!(
            "future_status"
                .parse::<crate::model::UploadStatus>()
                .is_err()
        );
    }

    #[test]
    fn system_status_shape() {
        let status = system::Status {
            status: "ok".into(),
            database: system::Component {
                status: "ok".into(),
                bytes: 4096,
            },
            storage: system::Storage {
                status: "ok".into(),
                bytes: 10,
                trash_bytes: 0,
                file_count: 1,
            },
            cache: system::Cache {
                status: "ok".into(),
                memory_bytes: 0,
                disk_bytes: 0,
                memory_entries: 0,
                disk_entries: 0,
                classes: None,
            },
        };
        let json = serde_json::to_value(&status).unwrap();
        assert_eq!(
            json["database"],
            serde_json::json!({"status": "ok", "bytes": 4096})
        );
        // Zero-valued storage counters are still emitted.
        assert_eq!(json["storage"]["trash_bytes"], 0);
        assert_eq!(json["storage"]["file_count"], 1);
        assert!(json["cache"].get("classes").is_none());
    }

    #[test]
    fn system_status_treats_nullable_statuses_as_empty() {
        let status: system::Status = serde_json::from_value(serde_json::json!({
            "status": null,
            "database": { "status": null, "bytes": 1 },
            "storage": { "status": null, "bytes": 2, "trash_bytes": 0, "file_count": 0 },
            "cache": {
                "status": null,
                "memory_bytes": 0,
                "disk_bytes": 0,
                "memory_entries": 0,
                "disk_entries": 0
            }
        }))
        .unwrap();
        assert!(status.status.is_empty());
        assert!(status.database.status.is_empty());
        assert!(status.storage.status.is_empty());
        assert!(status.cache.status.is_empty());
    }

    #[test]
    fn cache_class_omits_unused_dimensions() {
        let class = system::CacheClass {
            hits: 1,
            misses: 2,
            ..system::CacheClass::default()
        };
        let json = serde_json::to_value(&class).unwrap();
        assert_eq!(json["hits"], 1);
        assert!(json.get("memory_bytes").is_none());
        assert!(json.get("disk_entries").is_none());
    }

    #[test]
    fn archive_job_statuses_use_snake_case() {
        assert_eq!(
            serde_json::to_value(archive::JobStatus::WaitingPassword).unwrap(),
            "waiting_password"
        );
        assert_eq!(
            serde_json::to_value(archive::JobStatus::Done).unwrap(),
            "done"
        );
    }

    #[test]
    fn upload_status_serializes_the_parts_array() {
        let body = UploadStatus {
            upload_id: "u".into(),
            file_id: "f".into(),
            mode: UploadMode::Multipart,
            url: String::new(),
            part_size: 16,
            part_count: 1,
            expected_size: 16,
            mime_type: "image/png".into(),
            status: crate::model::UploadStatus::Pending,
            expires_at: Timestamp::parse("2024-05-06T07:08:09Z").unwrap(),
            parts: vec![UploadPart {
                part_number: 1,
                size: Some(16),
                etag: "e".into(),
                content_hash: Some(String::new()),
            }],
        };
        let json = serde_json::to_value(&body).unwrap();
        assert_eq!(json["status"], "pending");
        // content_hash is part of the contract even when empty.
        assert_eq!(
            json["parts"][0],
            serde_json::json!({"part_number": 1, "size": 16, "etag": "e", "content_hash": ""})
        );
        let sparse: UploadPart = serde_json::from_value(serde_json::json!({
            "part_number": 2,
            "etag": "saved"
        }))
        .unwrap();
        assert_eq!(sparse.size, None);
        assert_eq!(sparse.content_hash, None);
        assert_eq!(
            serde_json::to_value(sparse).unwrap(),
            serde_json::json!({"part_number": 2, "etag": "saved"})
        );
    }

    #[test]
    fn media_payloads_re_export_the_shared_media_types() {
        use crate::media::{AudioMedia, VideoMedia};

        let audio = AudioMedia::default();
        let video = VideoMedia::default();
        assert_eq!(
            serde_json::to_value(&audio).unwrap()["chapters"],
            serde_json::json!([])
        );
        assert_eq!(
            serde_json::to_value(&video).unwrap()["subtitles"],
            serde_json::json!([])
        );
    }
}

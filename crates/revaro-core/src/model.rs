//! The persisted domain model.
//!
//! Every type here maps to exactly one SQLite table (or to a projection of one)
//! and serializes to the same JSON the Go server produced. Field names, `null`
//! versus omitted semantics and enum spellings are part of the product's
//! compatibility surface: changing them would break existing databases and
//! already-deployed browsers, so they are pinned by the tests at the bottom of
//! this module.

use serde::{Deserialize, Serialize};

use crate::time::Timestamp;

/// Declare a `String`-backed enum with one canonical wire/database spelling.
///
/// The spelling is used identically in SQLite `CHECK` constraints, JSON bodies
/// and query parameters, so it is defined once here.
macro_rules! string_enum {
    (
        $(#[$meta:meta])*
        $name:ident { $($(#[$variant_meta:meta])* $variant:ident => $text:literal),+ $(,)? }
    ) => {
        $(#[$meta])*
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
        pub enum $name {
            $(
                $(#[$variant_meta])*
                #[doc = concat!("Wire and database spelling: `", $text, "`.")]
                $variant,
            )+
        }

        impl $name {
            /// Every variant, in declaration order.
            pub const ALL: &'static [$name] = &[$($name::$variant),+];

            /// The canonical wire and database spelling.
            #[must_use]
            pub const fn as_str(self) -> &'static str {
                match self {
                    $($name::$variant => $text),+
                }
            }
        }

        impl std::fmt::Display for $name {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str(self.as_str())
            }
        }

        impl std::str::FromStr for $name {
            type Err = crate::model::UnknownEnumValue;

            fn from_str(value: &str) -> Result<Self, Self::Err> {
                match value {
                    $($text => Ok($name::$variant),)+
                    other => Err(crate::model::UnknownEnumValue {
                        type_name: stringify!($name),
                        value: other.to_owned(),
                    }),
                }
            }
        }

        impl Serialize for $name {
            fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
                serializer.serialize_str(self.as_str())
            }
        }

        impl<'de> Deserialize<'de> for $name {
            fn deserialize<D: serde::Deserializer<'de>>(
                deserializer: D,
            ) -> Result<Self, D::Error> {
                let raw = String::deserialize(deserializer)?;
                raw.parse().map_err(serde::de::Error::custom)
            }
        }
    };
}

/// A value that is not a member of a string-backed enum.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{value:?} is not a valid {type_name}")]
pub struct UnknownEnumValue {
    /// The enum type that was being parsed.
    pub type_name: &'static str,
    /// The rejected value.
    pub value: String,
}

/// Whether a row in `files` is a regular file or a directory.
///
/// `Unknown` is a response-only value. The historical browser treated a
/// future kind as a generic, non-directory item, so decoding a children list
/// must not fail just because the server added a kind. Database parsing stays
/// strict through [`FromStr`], which accepts only the two persisted values.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum FileKind {
    /// A regular file.
    #[default]
    File,
    /// A directory.
    Directory,
    /// A kind introduced by a newer server response.
    Unknown,
}

impl FileKind {
    /// Every kind persisted by the database.
    pub const ALL: &'static [Self] = &[Self::File, Self::Directory];

    /// Canonical wire spelling.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::File => "file",
            Self::Directory => "directory",
            Self::Unknown => "unknown",
        }
    }
}

impl std::fmt::Display for FileKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

impl std::str::FromStr for FileKind {
    type Err = crate::model::UnknownEnumValue;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "file" => Ok(Self::File),
            "directory" => Ok(Self::Directory),
            other => Err(crate::model::UnknownEnumValue {
                type_name: "FileKind",
                value: other.to_owned(),
            }),
        }
    }
}

impl Serialize for FileKind {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for FileKind {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let raw = String::deserialize(deserializer)?;
        raw.parse().map_err(serde::de::Error::custom)
    }
}

string_enum! {
    /// Lifecycle of a row in `files`.
    #[derive(Default)]
    FileStatus {
        /// Metadata exists, bytes are still uploading.
        Pending => "pending",
        /// Bytes and metadata are committed.
        #[default]
        Ready => "ready",
        /// Permanent deletion is in progress.
        Deleting => "deleting",
        /// A previous attempt failed.
        Failed => "failed",
    }
}

string_enum! {
    /// How an upload transfers its bytes.
    #[derive(Default)]
    UploadMode {
        #[default]
        Single => "single",
        Multipart => "multipart",
    }
}

string_enum! {
    /// Lifecycle of a row in `uploads`.
    #[derive(Default)]
    UploadStatus {
        /// Accepting bytes.
        #[default]
        Pending => "pending",
        /// Bytes committed.
        Completed => "completed",
        /// Abandoned by the client.
        Aborted => "aborted",
        /// A previous attempt failed.
        Failed => "failed",
    }
}

/// Lifecycle of a row in `tasks`.
///
/// The Go browser decoded task responses into a structural TypeScript type, so
/// a newer server status remained in the task array and was simply omitted
/// from the known groups. Keep database parsing strict through [`FromStr`],
/// while accepting an unknown response value as `Unknown` for the browser.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum TaskStatus {
    /// Waiting for a worker.
    #[default]
    Queued,
    /// A worker is executing.
    Running,
    /// The task needs input (for example an archive password).
    WaitingInput,
    /// A failed task was retried.
    Retrying,
    /// Finished successfully.
    Completed,
    /// Finished unsuccessfully.
    Failed,
    /// Cancelled by the user.
    Cancelled,
    /// A status introduced by a newer server; ignored by known UI groups.
    Unknown,
}

impl TaskStatus {
    /// Every status understood by the product and persisted in SQLite.
    pub const ALL: &'static [Self] = &[
        Self::Queued,
        Self::Running,
        Self::WaitingInput,
        Self::Retrying,
        Self::Completed,
        Self::Failed,
        Self::Cancelled,
    ];

    /// The canonical wire and database spelling.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Queued => "queued",
            Self::Running => "running",
            Self::WaitingInput => "waiting_input",
            Self::Retrying => "retrying",
            Self::Completed => "completed",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
            Self::Unknown => "unknown",
        }
    }

    /// Default used when a legacy or newer task response omits its status.
    #[must_use]
    pub const fn unknown() -> Self {
        Self::Unknown
    }
}

impl std::fmt::Display for TaskStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

impl std::str::FromStr for TaskStatus {
    type Err = crate::model::UnknownEnumValue;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "queued" => Ok(Self::Queued),
            "running" => Ok(Self::Running),
            "waiting_input" => Ok(Self::WaitingInput),
            "retrying" => Ok(Self::Retrying),
            "completed" => Ok(Self::Completed),
            "failed" => Ok(Self::Failed),
            "cancelled" => Ok(Self::Cancelled),
            other => Err(crate::model::UnknownEnumValue {
                type_name: "TaskStatus",
                value: other.to_owned(),
            }),
        }
    }
}

impl serde::Serialize for TaskStatus {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> serde::Deserialize<'de> for TaskStatus {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = serde_json::Value::deserialize(deserializer)?;
        Ok(match value.as_str() {
            Some(raw) => raw.parse().unwrap_or(Self::Unknown),
            None => Self::Unknown,
        })
    }
}

impl TaskStatus {
    /// True when no further transition is expected.
    #[must_use]
    pub fn is_terminal(self) -> bool {
        matches!(
            self,
            TaskStatus::Completed | TaskStatus::Failed | TaskStatus::Cancelled
        )
    }
}

/// Task type names used by the task centre.
///
/// Stored as free-form text so a future task type does not require a schema
/// change; these are the values the product understands today.
pub mod task_type {
    /// A file upload.
    pub const UPLOAD: &str = "upload";
    /// An archive extraction.
    pub const ARCHIVE_EXTRACT: &str = "archive_extract";
    /// Subtitle preparation and extraction.
    pub const SUBTITLE: &str = "subtitle";
}

/// A row of the `files` table as the API exposes it.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct File {
    /// Primary key.
    pub id: String,
    /// Owning directory, `null` for the virtual root.
    pub parent_id: Option<String>,
    /// Display name.
    pub name: String,
    /// File or directory.
    #[serde(deserialize_with = "deserialize_file_kind")]
    pub kind: FileKind,
    /// Size in bytes; directories report `0`.
    pub size: i64,
    /// Stored content type, omitted when unset.
    #[serde(
        default,
        deserialize_with = "deserialize_nullable_string",
        skip_serializing_if = "String::is_empty"
    )]
    pub mime_type: String,
    /// Entity tag used for cache validation, omitted when unset.
    #[serde(
        default,
        deserialize_with = "deserialize_nullable_string",
        skip_serializing_if = "String::is_empty"
    )]
    pub etag: String,
    /// Integrity hash of the committed bytes, omitted when unset.
    #[serde(
        default,
        deserialize_with = "deserialize_nullable_string",
        skip_serializing_if = "String::is_empty"
    )]
    pub content_hash: String,
    /// Algorithm of [`File::content_hash`], omitted when unset.
    #[serde(
        default,
        deserialize_with = "deserialize_nullable_string",
        skip_serializing_if = "String::is_empty"
    )]
    pub hash_algorithm: String,
    /// Lifecycle state.
    #[serde(deserialize_with = "deserialize_file_status")]
    pub status: FileStatus,
    /// Creation time.
    #[serde(
        default = "missing_file_timestamp",
        deserialize_with = "deserialize_nullable_timestamp"
    )]
    pub created_at: Timestamp,
    /// Last metadata change.
    #[serde(
        default = "missing_file_timestamp",
        deserialize_with = "deserialize_nullable_timestamp"
    )]
    pub updated_at: Timestamp,
    /// Soft-deletion time, omitted while the row is live.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub deleted_at: Option<Timestamp>,
    /// Directory a trashed item should return to, omitted when unknown.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub restore_parent_id: Option<String>,
    /// Whether a thumbnail or generated cover exists.
    #[serde(
        default,
        deserialize_with = "deserialize_nullable_bool",
        skip_serializing_if = "is_false"
    )]
    pub has_cover: bool,
    /// Object-store key of the contents.
    ///
    /// Never leaves the server: the browser addresses files by id.
    #[serde(skip)]
    pub object_key: String,
}

/// Older browser responses typed optional string fields structurally. A
/// server may therefore send `null` for an unset value, which the Vue caller
/// treated exactly like an omitted field (`value || ''`). Keep response
/// decoding tolerant while retaining the concrete string representation used
/// by the Rust UI and storage layer.
fn deserialize_nullable_string<'de, D>(deserializer: D) -> Result<String, D::Error>
where
    D: serde::Deserializer<'de>,
{
    Ok(Option::<String>::deserialize(deserializer)?.unwrap_or_default())
}

/// Older task callers treated a missing or `null` numeric progress value as
/// zero through JavaScript's normal falsey/coercion rules. Keep that response
/// tolerance while rejecting other malformed scalar types.
fn deserialize_nullable_f64<'de, D>(deserializer: D) -> Result<f64, D::Error>
where
    D: serde::Deserializer<'de>,
{
    Ok(Option::<f64>::deserialize(deserializer)?.unwrap_or_default())
}

/// Older task callers treated a missing or `null` integer counter as zero.
fn deserialize_nullable_i64<'de, D>(deserializer: D) -> Result<i64, D::Error>
where
    D: serde::Deserializer<'de>,
{
    Ok(Option::<i64>::deserialize(deserializer)?.unwrap_or_default())
}

/// Older upload callers ignored resume parts whose number was omitted or
/// `null`. Keep that response tolerance while rejecting malformed scalar
/// values.
fn deserialize_nullable_i32<'de, D>(deserializer: D) -> Result<i32, D::Error>
where
    D: serde::Deserializer<'de>,
{
    Ok(Option::<i32>::deserialize(deserializer)?.unwrap_or_default())
}

/// Older task callers did not read creation/update timestamps at all. Treat
/// an omitted or explicit `null` timestamp as the wire default while still
/// rejecting malformed timestamp values.
fn deserialize_nullable_timestamp<'de, D>(deserializer: D) -> Result<Timestamp, D::Error>
where
    D: serde::Deserializer<'de>,
{
    Ok(Option::<Timestamp>::deserialize(deserializer)?.unwrap_or_default())
}

fn missing_file_timestamp() -> Timestamp {
    Timestamp::missing()
}

/// The historical `has_cover` check was `=== true`, so an explicit `null`
/// was the same as a missing/false value. Reject other malformed scalar types
/// rather than inventing a new truthiness rule.
fn deserialize_nullable_bool<'de, D>(deserializer: D) -> Result<bool, D::Error>
where
    D: serde::Deserializer<'de>,
{
    Ok(Option::<bool>::deserialize(deserializer)?.unwrap_or(false))
}

/// The historical browser treated every status other than `ready` as a muted
/// item and did not validate the string against the current server enum. Keep
/// that response tolerance while retaining strict `FromStr` parsing for
/// persisted database values.
fn deserialize_file_status<'de, D>(deserializer: D) -> Result<FileStatus, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let value = serde_json::Value::deserialize(deserializer)?;
    Ok(value
        .as_str()
        .and_then(|raw| raw.parse().ok())
        .unwrap_or(FileStatus::Failed))
}

/// Preserve an unknown response kind as a generic, non-directory item while
/// keeping persisted database values strict.
fn deserialize_file_kind<'de, D>(deserializer: D) -> Result<FileKind, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let value = serde_json::Value::deserialize(deserializer)?;
    Ok(value
        .as_str()
        .and_then(|raw| raw.parse().ok())
        .unwrap_or(FileKind::Unknown))
}

impl File {
    /// True when this row is a directory.
    #[must_use]
    pub fn is_directory(&self) -> bool {
        self.kind == FileKind::Directory
    }

    /// True when this row is a regular file whose bytes are committed.
    #[must_use]
    pub fn is_ready_file(&self) -> bool {
        self.kind == FileKind::File && self.status == FileStatus::Ready
    }

    /// True when this row is in the trash.
    #[must_use]
    pub fn is_trashed(&self) -> bool {
        self.deleted_at.is_some()
    }
}

/// One hop of a breadcrumb trail.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct FolderRef {
    /// Directory id.
    pub id: String,
    /// Directory name.
    pub name: String,
}

/// A library entry: a file plus where it lives and how long it plays.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct LibraryItem {
    /// The underlying file.
    #[serde(flatten)]
    pub file: File,
    /// Path from the root to the containing directory.
    #[serde(default)]
    pub folder_path: Vec<FolderRef>,
    /// Media duration in milliseconds, omitted when unknown or not media.
    #[serde(
        default,
        deserialize_with = "deserialize_nullable_i64",
        skip_serializing_if = "is_zero_i64"
    )]
    pub duration_ms: i64,
}

/// Per-bucket entry counts for the sidebar badges.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct LibraryCounts {
    /// Books.
    #[serde(default, deserialize_with = "deserialize_nullable_i64")]
    pub book: i64,
    /// Images.
    #[serde(default, deserialize_with = "deserialize_nullable_i64")]
    pub image: i64,
    /// Videos.
    #[serde(default, deserialize_with = "deserialize_nullable_i64")]
    pub video: i64,
    /// Audio files.
    #[serde(default, deserialize_with = "deserialize_nullable_i64")]
    pub audio: i64,
    /// Everything else.
    #[serde(default, deserialize_with = "deserialize_nullable_i64")]
    pub file: i64,
}

/// Playback position for one audio or video file.
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
pub struct MediaProgress {
    /// Resume position in seconds.
    #[serde(default, deserialize_with = "deserialize_nullable_f64")]
    pub position: f64,
    /// Known duration in seconds.
    #[serde(default, deserialize_with = "deserialize_nullable_f64")]
    pub duration: f64,
    /// When the position was last written, omitted when never saved.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub updated_at: Option<Timestamp>,
}

/// Reading position for one book.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct BookProgress {
    /// The stored reading anchor, omitted when unset.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub anchor: Option<crate::reader::Anchor>,
}

/// Public share state of a single file.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ShareStatus {
    /// Whether a share link exists.
    #[serde(default, deserialize_with = "deserialize_nullable_bool")]
    pub active: bool,
    /// The public URL, present only while active.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    /// When the share was created, present only while active.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub created_at: Option<Timestamp>,
}

/// One acknowledged part of a multipart upload.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct UploadPart {
    /// 1-based part number.
    #[serde(default, deserialize_with = "deserialize_nullable_i32")]
    pub part_number: i32,
    /// Part size in bytes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size: Option<i64>,
    /// Entity tag returned when the part was stored.
    #[serde(default, deserialize_with = "deserialize_nullable_string")]
    pub etag: String,
    /// Optional per-part integrity hash.
    ///
    /// Older successful resume responses omitted this field altogether.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content_hash: Option<String>,
}

/// A row of the `tasks` table as the API exposes it.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Task {
    /// Primary key.
    pub id: String,
    /// Task type, see [`task_type`].
    #[serde(rename = "type")]
    pub task_type: String,
    /// Lifecycle state.
    #[serde(default = "TaskStatus::unknown")]
    pub status: TaskStatus,
    /// Human-readable phase within the task.
    #[serde(default, deserialize_with = "deserialize_nullable_string")]
    pub phase: String,
    /// Completion percentage, `0.0..=100.0`.
    #[serde(default, deserialize_with = "deserialize_nullable_f64")]
    pub progress: f64,
    /// Current throughput in bytes per second.
    #[serde(default, deserialize_with = "deserialize_nullable_i64")]
    pub speed: i64,
    /// Estimated seconds remaining, omitted when unknown.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub eta_seconds: Option<i64>,
    /// How many times this task has been retried.
    #[serde(default, deserialize_with = "deserialize_nullable_i64")]
    pub retry_count: i64,
    /// Retry ceiling.
    #[serde(default, deserialize_with = "deserialize_nullable_i64")]
    pub max_retries: i64,
    /// Failure message, omitted while healthy.
    #[serde(
        default,
        deserialize_with = "deserialize_nullable_string",
        skip_serializing_if = "String::is_empty"
    )]
    pub error: String,
    /// Origin kind of the task, omitted when it has none.
    #[serde(
        default,
        deserialize_with = "deserialize_nullable_string",
        skip_serializing_if = "String::is_empty"
    )]
    pub source_type: String,
    /// Origin identifier, omitted when it has none.
    #[serde(
        default,
        deserialize_with = "deserialize_nullable_string",
        skip_serializing_if = "String::is_empty"
    )]
    pub source_id: String,
    /// Whether cancellation has been requested.
    #[serde(default, deserialize_with = "deserialize_nullable_bool")]
    pub cancel_requested: bool,
    /// Creation time.
    #[serde(default, deserialize_with = "deserialize_nullable_timestamp")]
    pub created_at: Timestamp,
    /// When a worker picked the task up, omitted before that.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub started_at: Option<Timestamp>,
    /// When the task reached a terminal state, omitted before that.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub finished_at: Option<Timestamp>,
    /// Last state change.
    #[serde(default, deserialize_with = "deserialize_nullable_timestamp")]
    pub updated_at: Timestamp,
    /// Display name derived from the task's files or type.
    #[serde(default, deserialize_with = "deserialize_nullable_string")]
    pub name: String,
}

/// The signed-in administrator's profile.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Profile {
    /// Login name.
    pub username: String,
    /// Whether an avatar image is stored.
    #[serde(default, deserialize_with = "deserialize_nullable_bool")]
    pub has_avatar: bool,
}

/// Aggregate size of the logical library.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct StorageStats {
    /// Sum of the sizes of every counted file.
    pub total_bytes: i64,
    /// Number of counted files.
    pub file_count: i64,
}

fn is_false(value: &bool) -> bool {
    !*value
}

fn is_zero_i64(value: &i64) -> bool {
    *value == 0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn enum_spellings_match_the_database_constraints() {
        assert_eq!(FileKind::File.as_str(), "file");
        assert_eq!(FileKind::Directory.as_str(), "directory");
        assert_eq!(FileStatus::Pending.as_str(), "pending");
        assert_eq!(FileStatus::Ready.as_str(), "ready");
        assert_eq!(FileStatus::Deleting.as_str(), "deleting");
        assert_eq!(FileStatus::Failed.as_str(), "failed");
        assert_eq!(UploadMode::Single.as_str(), "single");
        assert_eq!(UploadMode::Multipart.as_str(), "multipart");
        assert_eq!(UploadStatus::Aborted.as_str(), "aborted");
        assert_eq!(TaskStatus::WaitingInput.as_str(), "waiting_input");
        assert_eq!(TaskStatus::Cancelled.as_str(), "cancelled");
    }

    #[test]
    fn enum_parsing_rejects_unknown_spellings() {
        assert_eq!("ready".parse::<FileStatus>(), Ok(FileStatus::Ready));
        let error = "bogus".parse::<FileStatus>().unwrap_err();
        assert_eq!(error.type_name, "FileStatus");
        assert_eq!(error.value, "bogus");
    }

    #[test]
    fn task_response_unknown_statuses_are_ignored_but_database_parsing_stays_strict() {
        assert_eq!(
            serde_json::from_str::<TaskStatus>(r#""future_status""#).unwrap(),
            TaskStatus::Unknown
        );
        assert_eq!(
            serde_json::from_str::<TaskStatus>("null").unwrap(),
            TaskStatus::Unknown
        );
        assert!("future_status".parse::<TaskStatus>().is_err());
        assert_eq!(
            serde_json::to_string(&TaskStatus::Unknown).unwrap(),
            r#""unknown""#
        );
        let task: Task = serde_json::from_value(serde_json::json!({
            "id": "future-task",
            "type": "upload",
            "created_at": "2024-05-06T07:08:09Z",
            "updated_at": "2024-05-06T07:08:09Z"
        }))
        .unwrap();
        assert_eq!(task.status, TaskStatus::Unknown);
    }

    #[test]
    fn every_variant_round_trips() {
        for variant in FileStatus::ALL {
            assert_eq!(variant.as_str().parse::<FileStatus>(), Ok(*variant));
        }
        for variant in TaskStatus::ALL {
            assert_eq!(variant.as_str().parse::<TaskStatus>(), Ok(*variant));
        }
        for variant in UploadMode::ALL {
            assert_eq!(variant.as_str().parse::<UploadMode>(), Ok(*variant));
        }
    }

    #[test]
    fn file_json_matches_the_historical_shape() {
        let file = File {
            id: "0190f8f0-1c2b-7c3d-9e4f-5a6b7c8d9e0f".into(),
            parent_id: None,
            name: "photo.png".into(),
            kind: FileKind::File,
            size: 42,
            mime_type: "image/png".into(),
            etag: "abc".into(),
            content_hash: String::new(),
            hash_algorithm: String::new(),
            status: FileStatus::Ready,
            created_at: Timestamp::parse("2024-05-06T07:08:09Z").unwrap(),
            updated_at: Timestamp::parse("2024-05-06T07:08:09Z").unwrap(),
            deleted_at: None,
            restore_parent_id: None,
            has_cover: false,
            object_key: "blobs/secret".into(),
        };
        let json = serde_json::to_value(&file).unwrap();
        assert_eq!(
            json,
            serde_json::json!({
                "id": "0190f8f0-1c2b-7c3d-9e4f-5a6b7c8d9e0f",
                "parent_id": null,
                "name": "photo.png",
                "kind": "file",
                "size": 42,
                "mime_type": "image/png",
                "etag": "abc",
                "status": "ready",
                "created_at": "2024-05-06T07:08:09Z",
                "updated_at": "2024-05-06T07:08:09Z"
            })
        );
        // The object key must never reach the browser.
        assert!(!serde_json::to_string(&file).unwrap().contains("secret"));
    }

    #[test]
    fn file_responses_treat_unknown_statuses_as_non_ready() {
        let base = serde_json::json!({
            "id": "file",
            "parent_id": null,
            "name": "future.txt",
            "kind": "file",
            "size": 1,
            "created_at": "2024-05-06T07:08:09Z",
            "updated_at": "2024-05-06T07:08:09Z"
        });
        let mut unknown = base.clone();
        unknown["status"] = serde_json::json!("future_status");
        assert_eq!(
            serde_json::from_value::<File>(unknown).unwrap().status,
            FileStatus::Failed
        );

        let mut non_string = base;
        non_string["status"] = serde_json::Value::Null;
        assert_eq!(
            serde_json::from_value::<File>(non_string).unwrap().status,
            FileStatus::Failed
        );
    }

    #[test]
    fn file_responses_treat_unknown_kinds_as_generic_without_relaxing_database_parsing() {
        let file = serde_json::from_value::<File>(serde_json::json!({
            "id": "future-kind",
            "parent_id": null,
            "name": "future.bin",
            "kind": "future_kind",
            "size": 1,
            "status": "ready",
            "created_at": "2024-05-06T07:08:09Z",
            "updated_at": "2024-05-06T07:08:09Z"
        }))
        .unwrap();
        assert_eq!(file.kind, FileKind::Unknown);
        assert!(!file.is_directory());
        assert!(!file.is_ready_file());
        assert!("future_kind".parse::<FileKind>().is_err());
        assert!(serde_json::from_str::<FileKind>(r#""future_kind""#).is_err());
    }

    #[test]
    fn file_responses_treat_nullable_optional_fields_as_empty_without_affecting_required_data() {
        let file = serde_json::from_value::<File>(serde_json::json!({
            "id": "nullable-fields",
            "parent_id": null,
            "name": "nullable.txt",
            "kind": "file",
            "size": 1,
            "mime_type": null,
            "etag": null,
            "content_hash": null,
            "hash_algorithm": null,
            "status": "ready",
            "created_at": "2024-05-06T07:08:09Z",
            "updated_at": "2024-05-06T07:08:09Z",
            "has_cover": null
        }))
        .unwrap();
        assert_eq!(file.mime_type, "");
        assert_eq!(file.etag, "");
        assert_eq!(file.content_hash, "");
        assert_eq!(file.hash_algorithm, "");
        assert!(!file.has_cover);
    }

    #[test]
    fn file_responses_treat_unused_timestamps_as_defaults() {
        let mut missing = serde_json::json!({
            "id": "missing-time",
            "parent_id": null,
            "name": "missing-time.txt",
            "kind": "file",
            "size": 1,
            "status": "ready"
        });
        let file = serde_json::from_value::<File>(missing.clone()).unwrap();
        assert!(file.created_at.is_missing());
        assert!(file.updated_at.is_missing());

        missing["created_at"] = serde_json::Value::Null;
        missing["updated_at"] = serde_json::Value::Null;
        let nullable = serde_json::from_value::<File>(missing).unwrap();
        assert_eq!(nullable.created_at, Timestamp::default());
        assert_eq!(nullable.updated_at, Timestamp::default());

        let invalid = serde_json::from_value::<File>(serde_json::json!({
            "id": "invalid-time",
            "parent_id": null,
            "name": "invalid-time.txt",
            "kind": "file",
            "size": 1,
            "status": "ready",
            "created_at": 123
        }));
        assert!(invalid.is_err());
    }

    #[test]
    fn library_counts_treat_missing_or_null_values_as_zero() {
        let counts = serde_json::from_value::<LibraryCounts>(serde_json::json!({
            "book": null,
            "image": 2,
            "video": null,
            "file": 3
        }))
        .unwrap();
        assert_eq!(counts.book, 0);
        assert_eq!(counts.image, 2);
        assert_eq!(counts.video, 0);
        assert_eq!(counts.audio, 0);
        assert_eq!(counts.file, 3);

        let invalid = serde_json::from_value::<LibraryCounts>(serde_json::json!({
            "book": "2"
        }));
        assert!(invalid.is_err());
    }

    #[test]
    fn share_responses_treat_nullable_fields_as_an_inactive_share() {
        let share = serde_json::from_value::<ShareStatus>(serde_json::json!({
            "active": null,
            "url": null,
            "created_at": null
        }))
        .unwrap();
        assert!(!share.active);
        assert!(share.url.is_none());
        assert!(share.created_at.is_none());
    }

    #[test]
    fn profile_responses_treat_nullable_avatar_as_absent() {
        let profile = serde_json::from_value::<Profile>(serde_json::json!({
            "username": "admin",
            "has_avatar": null
        }))
        .unwrap();
        assert_eq!(profile.username, "admin");
        assert!(!profile.has_avatar);
    }

    #[test]
    fn library_items_flatten_their_file() {
        let item = LibraryItem {
            file: File {
                id: "id".into(),
                name: "clip.mp4".into(),
                kind: FileKind::File,
                status: FileStatus::Ready,
                ..File::default()
            },
            folder_path: vec![FolderRef {
                id: "root".into(),
                name: "Movies".into(),
            }],
            duration_ms: 1500,
        };
        let json = serde_json::to_value(&item).unwrap();
        assert_eq!(json["name"], "clip.mp4");
        assert_eq!(json["duration_ms"], 1500);
        assert_eq!(json["folder_path"][0]["name"], "Movies");
    }

    #[test]
    fn library_items_treat_nullable_duration_as_unknown() {
        let item = serde_json::from_value::<LibraryItem>(serde_json::json!({
            "id": "audio",
            "parent_id": null,
            "name": "track.mp3",
            "kind": "file",
            "size": 1,
            "status": "ready",
            "created_at": "2024-05-06T07:08:09Z",
            "updated_at": "2024-05-06T07:08:09Z",
            "folder_path": [],
            "duration_ms": null
        }))
        .unwrap();
        assert_eq!(item.duration_ms, 0);

        let invalid = serde_json::from_value::<LibraryItem>(serde_json::json!({
            "id": "audio",
            "parent_id": null,
            "name": "track.mp3",
            "kind": "file",
            "size": 1,
            "status": "ready",
            "created_at": "2024-05-06T07:08:09Z",
            "updated_at": "2024-05-06T07:08:09Z",
            "folder_path": [],
            "duration_ms": "125"
        }));
        assert!(invalid.is_err());
    }

    #[test]
    fn media_progress_treats_nullable_numbers_as_zero() {
        let progress = serde_json::from_value::<MediaProgress>(serde_json::json!({
            "position": null,
            "duration": null,
            "updated_at": null
        }))
        .unwrap();
        assert_eq!(progress.position, 0.0);
        assert_eq!(progress.duration, 0.0);
        assert!(progress.updated_at.is_none());

        let invalid = serde_json::from_value::<MediaProgress>(serde_json::json!({
            "position": "ten"
        }));
        assert!(invalid.is_err());
    }

    #[test]
    fn task_json_omits_empty_optional_fields() {
        let task = Task {
            id: "t".into(),
            task_type: task_type::UPLOAD.into(),
            status: TaskStatus::Queued,
            name: "upload".into(),
            created_at: Timestamp::parse("2024-05-06T07:08:09Z").unwrap(),
            updated_at: Timestamp::parse("2024-05-06T07:08:09Z").unwrap(),
            ..Task::default()
        };
        let json = serde_json::to_value(&task).unwrap();
        let object = json.as_object().unwrap();
        for absent in [
            "eta_seconds",
            "error",
            "source_type",
            "source_id",
            "started_at",
            "finished_at",
        ] {
            assert!(!object.contains_key(absent), "{absent} should be omitted");
        }
        assert_eq!(json["type"], "upload");
        assert_eq!(json["status"], "queued");
    }

    #[test]
    fn task_responses_treat_nullable_display_fields_as_defaults() {
        let task: Task = serde_json::from_value(serde_json::json!({
            "id": "task",
            "type": "upload",
            "status": "running",
            "phase": null,
            "progress": null,
            "speed": null,
            "retry_count": null,
            "max_retries": null,
            "error": null,
            "source_type": null,
            "source_id": null,
            "cancel_requested": null,
            "created_at": "2024-05-06T07:08:09Z",
            "updated_at": "2024-05-06T07:08:09Z",
            "name": null
        }))
        .unwrap();
        assert_eq!(task.phase, "");
        assert_eq!(task.progress, 0.0);
        assert_eq!(task.speed, 0);
        assert_eq!(task.retry_count, 0);
        assert_eq!(task.max_retries, 0);
        assert!(task.error.is_empty());
        assert!(task.source_type.is_empty());
        assert!(task.source_id.is_empty());
        assert!(!task.cancel_requested);
        assert!(task.name.is_empty());
    }

    #[test]
    fn task_responses_treat_unused_timestamps_as_defaults() {
        let task: Task = serde_json::from_value(serde_json::json!({
            "id": "task",
            "type": "upload",
            "status": "completed",
            "created_at": null
        }))
        .unwrap();
        assert_eq!(task.created_at, Timestamp::default());
        assert_eq!(task.updated_at, Timestamp::default());

        let invalid = serde_json::from_value::<Task>(serde_json::json!({
            "id": "task",
            "type": "upload",
            "created_at": 42
        }));
        assert!(invalid.is_err());
    }

    #[test]
    fn task_status_terminality() {
        assert!(TaskStatus::Completed.is_terminal());
        assert!(TaskStatus::Failed.is_terminal());
        assert!(TaskStatus::Cancelled.is_terminal());
        assert!(!TaskStatus::Running.is_terminal());
        assert!(!TaskStatus::WaitingInput.is_terminal());
    }
}

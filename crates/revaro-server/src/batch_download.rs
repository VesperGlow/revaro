//! Short-lived batch-download tickets and streaming ZIP responses.
//!
//! Preparation and delivery are separate on purpose. The prepare request does
//! all database validation before the browser starts a native download, while
//! the ticket keeps only server-side file metadata. The download URL therefore
//! never accepts object-store keys or paths from the client and can be consumed
//! exactly once.
//!
//! The ZIP is written on a blocking worker through zip's forward-only stream
//! writer. A bounded channel connects that worker to the HTTP body, so a slow
//! client applies backpressure without buffering an entire archive in memory.

use std::collections::{HashMap, HashSet};
use std::convert::Infallible;
use std::io::{self, Write};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use axum::extract::{FromRequest, Path as PathParam, Request, State};
use axum::response::Response;
use axum::routing::{get, post};
use axum::{Router, body::Body};
use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use bytes::Bytes;
use revaro_core::ApiError;
use revaro_core::api::BatchDownloadTicket;
use revaro_core::model::{File, FileKind, FileStatus};
use revaro_core::validate::validate_batch_download_ids;
use rusqlite::Connection;
use tokio::sync::mpsc;
use zip::write::SimpleFileOptions;
use zip::{CompressionMethod, ZipWriter};

use crate::auth::AuthUser;
use crate::auth_routes::JsonBody;
use crate::file_routes::lookup_file;
use crate::state::AppState;
use crate::storage::{LocalStore, StorageError};

/// Maximum number of files in one prepared archive.
pub use revaro_core::limits::MAX_BATCH_DOWNLOAD_FILES;
/// How long a prepared URL remains redeemable.
pub const BATCH_DOWNLOAD_TOKEN_TTL: Duration = Duration::from_secs(2 * 60);
/// Maximum number of outstanding prepared URLs.
pub const MAX_BATCH_DOWNLOAD_TOKENS: usize = 256;
/// Number of chunks retained while a ZIP is being sent.
const ZIP_CHANNEL_CAPACITY: usize = 8;

#[derive(Debug, Default, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct BatchDownloadInput {
    ids: Option<Vec<Option<String>>>,
}

/// A validated file and the safe name it will have inside the archive.
#[derive(Debug, Clone)]
pub(crate) struct BatchDownloadEntry {
    /// File metadata, including its server-only object key.
    pub file: File,
    /// A single path component, unique within this archive.
    pub name: String,
}

#[derive(Debug)]
struct StoredBatchDownloadTicket {
    user: String,
    entries: Vec<BatchDownloadEntry>,
    expires_at: Instant,
}

/// Errors raised while reserving a batch-download ticket.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum BatchDownloadError {
    /// The bounded in-memory ticket table has no room after expired entries are
    /// removed.
    #[error("batch download token store is full")]
    TokenStoreFull,
}

/// Process-local storage for short-lived, user-bound batch-download tickets.
#[derive(Debug)]
pub struct BatchDownloadRuntime {
    tickets: Mutex<HashMap<String, StoredBatchDownloadTicket>>,
}

impl BatchDownloadRuntime {
    /// Create an empty ticket table.
    #[must_use]
    pub fn new() -> Self {
        Self {
            tickets: Mutex::new(HashMap::new()),
        }
    }

    /// Reserve a one-time URL for one authenticated user.
    pub(crate) fn issue(
        &self,
        user: String,
        entries: Vec<BatchDownloadEntry>,
    ) -> Result<String, BatchDownloadError> {
        self.issue_at(user, entries, Instant::now())
    }

    fn issue_at(
        &self,
        user: String,
        entries: Vec<BatchDownloadEntry>,
        now: Instant,
    ) -> Result<String, BatchDownloadError> {
        let mut tickets = self.tickets.lock().unwrap_or_else(PoisonError::into_inner);
        tickets.retain(|_, ticket| ticket.expires_at > now);
        if tickets.len() >= MAX_BATCH_DOWNLOAD_TOKENS {
            return Err(BatchDownloadError::TokenStoreFull);
        }

        loop {
            let token = URL_SAFE_NO_PAD.encode(rand::random::<[u8; 32]>());
            if tickets.contains_key(&token) {
                continue;
            }
            tickets.insert(
                token.clone(),
                StoredBatchDownloadTicket {
                    user: user.clone(),
                    entries: entries.clone(),
                    expires_at: now + BATCH_DOWNLOAD_TOKEN_TTL,
                },
            );
            return Ok(token);
        }
    }

    /// Consume a URL if it belongs to user and has not expired.
    pub(crate) fn consume(&self, user: &str, token: &str) -> Option<Vec<BatchDownloadEntry>> {
        if !(32..=128).contains(&token.len()) {
            return None;
        }
        let now = Instant::now();
        let mut tickets = self.tickets.lock().unwrap_or_else(PoisonError::into_inner);
        tickets.retain(|_, ticket| ticket.expires_at > now);
        let belongs_to_user = tickets.get(token).is_some_and(|ticket| ticket.user == user);
        if !belongs_to_user {
            return None;
        }
        tickets.remove(token).map(|ticket| ticket.entries)
    }
}

impl Default for BatchDownloadRuntime {
    fn default() -> Self {
        Self::new()
    }
}

/// Routes under the authenticated /api subtree.
pub fn routes() -> Router<Arc<AppState>> {
    Router::new()
        .route("/files/batch-download/prepare", post(prepare))
        .route("/files/batch-download/{token}", get(download))
}

/// POST /api/files/batch-download/prepare
async fn prepare(
    State(state): State<Arc<AppState>>,
    user: AuthUser,
    request: Request,
) -> Result<axum::Json<BatchDownloadTicket>, ApiError> {
    let JsonBody(input) =
        JsonBody::<Option<BatchDownloadInput>>::from_request(request, &state).await?;
    let ids = input
        .and_then(|input| input.ids)
        .unwrap_or_default()
        .into_iter()
        .map(|id| id.unwrap_or_default())
        .collect::<Vec<_>>();
    validate_batch_download_ids(&ids)?;
    let entries = state
        .db
        .call_api(move |connection| resolve_entries(connection, &ids))
        .await?;

    let token = state
        .batch_download
        .issue(user.username, entries)
        .map_err(|error| {
            tracing::warn!(%error, "batch download ticket table is full");
            ApiError::unavailable("batch download preparation is busy; try again shortly")
        })?;
    Ok(axum::Json(BatchDownloadTicket { token }))
}

/// Resolve and validate every selected row before issuing a ticket.
fn resolve_entries(
    connection: &Connection,
    ids: &[String],
) -> Result<Vec<BatchDownloadEntry>, ApiError> {
    let mut used_names = HashSet::with_capacity(ids.len());
    let mut entries = Vec::with_capacity(ids.len());
    for id in ids {
        let file = lookup_file(connection, id).map_err(|error| {
            if error.is_not_found() {
                ApiError::not_found("file not found")
            } else {
                tracing::error!(%error, file_id = %id, "batch download metadata read failed");
                ApiError::internal("could not read file metadata")
            }
        })?;
        if file.kind != FileKind::File {
            return Err(ApiError::bad_request(
                "directories cannot be downloaded in a batch",
            ));
        }
        if file.status != FileStatus::Ready {
            return Err(ApiError::conflict("file is not ready for download"));
        }
        if file.object_key.is_empty() {
            tracing::error!(file_id = %file.id, "batch download file has no object key");
            return Err(ApiError::internal("file content is unavailable"));
        }

        let name = unique_zip_name(safe_zip_name(&file.name), &mut used_names);
        entries.push(BatchDownloadEntry { file, name });
    }
    Ok(entries)
}

/// GET /api/files/batch-download/{token}
async fn download(
    State(state): State<Arc<AppState>>,
    user: AuthUser,
    PathParam(token): PathParam<String>,
) -> Result<Response, ApiError> {
    let entries = state
        .batch_download
        .consume(&user.username, &token)
        .ok_or_else(|| ApiError::not_found("batch download token not found or expired"))?;

    // Reject a corrupt object key before committing the response headers. Normal
    // rows always contain blobs/<uuid>, but this check keeps a manually edited
    // database from turning the blocking worker into an arbitrary path reader.
    for entry in &entries {
        state
            .store
            .path_for(&entry.file.object_key)
            .map_err(|error| {
                tracing::error!(%error, file_id = %entry.file.id, "batch download object key is invalid");
                ApiError::internal("file content is unavailable")
            })?;
    }

    let (sender, receiver) = mpsc::channel(ZIP_CHANNEL_CAPACITY);
    let store = state.store.clone();
    tokio::task::spawn_blocking(move || {
        if let Err(error) = write_zip(&store, entries, sender) {
            tracing::warn!(%error, "batch download stream failed");
        }
    });

    let stream = futures_util::stream::unfold(receiver, |mut receiver| async move {
        receiver
            .recv()
            .await
            .map(|chunk| (Ok::<Bytes, Infallible>(chunk), receiver))
    });
    let mut response = Response::new(Body::from_stream(stream));
    response.headers_mut().insert(
        http::header::CONTENT_TYPE,
        "application/zip".parse().expect("valid header value"),
    );
    response.headers_mut().insert(
        http::header::CONTENT_DISPOSITION,
        "attachment; filename=\"revaro-download.zip\""
            .parse()
            .expect("valid header value"),
    );
    response.headers_mut().insert(
        http::header::CACHE_CONTROL,
        "no-store".parse().expect("valid header value"),
    );
    Ok(response)
}

/// Convert a legacy or corrupt display name into one ZIP path component.
fn safe_zip_name(name: &str) -> String {
    let candidate = name.replace('\\', "/");
    let candidate = candidate.rsplit('/').next().unwrap_or_default();
    let mut cleaned = String::with_capacity(candidate.len());
    for character in candidate.chars() {
        if character.is_control() {
            cleaned.push('_');
        } else {
            cleaned.push(character);
        }
    }
    if cleaned.is_empty() || cleaned == "." || cleaned == ".." {
        return "file".to_owned();
    }
    // A drive prefix is interpreted as a path by Windows ZIP extractors.
    if cleaned.len() >= 2
        && cleaned.as_bytes()[0].is_ascii_alphabetic()
        && cleaned.as_bytes()[1] == b':'
    {
        cleaned.insert(0, '_');
    }
    cleaned
}

/// Add a numeric suffix while preserving the final extension.
fn unique_zip_name(name: String, used: &mut HashSet<String>) -> String {
    let key = name.to_lowercase();
    if used.insert(key) {
        return name;
    }

    let (stem, extension) = match name.rfind('.') {
        Some(index) if index > 0 => (&name[..index], &name[index..]),
        _ => (name.as_str(), ""),
    };
    for index in 2.. {
        let candidate = format!("{stem} ({index}){extension}");
        if used.insert(candidate.to_lowercase()) {
            return candidate;
        }
    }
    unreachable!("a finite archive cannot exhaust usize suffixes")
}

#[derive(Debug, thiserror::Error)]
enum BatchZipError {
    /// The object disappeared or failed the store's regular-file check.
    #[error(transparent)]
    Storage(#[from] StorageError),
    /// Reading an object or sending a chunk failed.
    #[error(transparent)]
    Io(#[from] io::Error),
    /// ZIP framing or compression failed.
    #[error(transparent)]
    Zip(#[from] zip::result::ZipError),
}

/// A blocking Write sink backed by the response body channel.
struct ChunkWriter {
    sender: mpsc::Sender<Bytes>,
}

impl Write for ChunkWriter {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        if buffer.is_empty() {
            return Ok(0);
        }
        self.sender
            .blocking_send(Bytes::copy_from_slice(buffer))
            .map_err(|_| {
                io::Error::new(io::ErrorKind::BrokenPipe, "batch download client closed")
            })?;
        Ok(buffer.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// Generate one ZIP directly into the response stream.
fn write_zip(
    store: &LocalStore,
    entries: Vec<BatchDownloadEntry>,
    sender: mpsc::Sender<Bytes>,
) -> Result<(), BatchZipError> {
    let mut archive = ZipWriter::new_stream(ChunkWriter { sender });
    let options = SimpleFileOptions::default().compression_method(CompressionMethod::Deflated);
    for entry in entries {
        let mut source = store.open_object_blocking(&entry.file.object_key)?;
        archive.start_file(entry.name, options)?;
        io::copy(&mut source, &mut archive)?;
    }
    archive.finish()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::io::Cursor;
    use std::path::PathBuf;

    use axum::body::Body;
    use http::{Request, StatusCode};
    use http_body_util::BodyExt as _;
    use revaro_core::ids::ROOT_ID;
    use revaro_core::time::Timestamp;
    use tower::ServiceExt as _;
    use zip::ZipArchive;

    struct TempRoot(PathBuf);

    impl TempRoot {
        fn new() -> Self {
            let path = std::env::temp_dir().join(format!(
                "revaro-batch-download-{}-{}",
                std::process::id(),
                crate::ids::new_id()
            ));
            Self(path)
        }
    }

    impl Drop for TempRoot {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    struct TestContext {
        state: Arc<AppState>,
        _root: TempRoot,
    }

    async fn context() -> TestContext {
        let root = TempRoot::new();
        let config = crate::config::Config::from_lookup(&|name| match name {
            "APP_BASE_URL" => Some("http://localhost:8080".to_owned()),
            "APP_WEB_DIR" => Some("/nonexistent".to_owned()),
            _ => None,
        })
        .unwrap();
        let store = LocalStore::open(&root.0).await.unwrap();
        let database = crate::db::Database::open_in_memory().unwrap();
        let auth = crate::auth::AuthService::new(database.clone());
        TestContext {
            state: AppState::new(Arc::new(config), database, store, auth),
            _root: root,
        }
    }

    async fn session(state: &Arc<AppState>) -> String {
        let raw = "batch-download-test-session".to_owned();
        let hash = crate::auth::token_hash(&raw);
        state
            .db
            .call_api(move |connection| {
                connection
                    .execute(
                        "INSERT OR REPLACE INTO settings(key,value,updated_at) VALUES('admin_username','admin','2024-01-01T00:00:00Z')",
                        [],
                    )
                    .map_err(|error| ApiError::internal(error.to_string()))?;
                connection
                    .execute(
                        "INSERT OR REPLACE INTO sessions(id,token_hash,created_at,expires_at) VALUES('batch-session',?1,'2024-01-01T00:00:00Z','2999-01-01T00:00:00Z')",
                        [hash],
                    )
                    .map_err(|error| ApiError::internal(error.to_string()))?;
                Ok(())
            })
            .await
            .unwrap();
        raw
    }

    async fn request(
        state: &Arc<AppState>,
        method: http::Method,
        uri: &str,
        body: Option<serde_json::Value>,
        authenticated: bool,
    ) -> axum::response::Response {
        let mut builder = Request::builder().method(method.clone()).uri(uri);
        if method != http::Method::GET {
            builder = builder.header("origin", "http://localhost:8080");
        }
        if authenticated {
            builder = builder.header(
                "cookie",
                format!("{}={}", crate::auth::SESSION_COOKIE, session(state).await),
            );
        }
        let body = match body {
            Some(value) => {
                builder = builder.header("content-type", "application/json");
                Body::from(value.to_string())
            }
            None => Body::empty(),
        };
        crate::router::build(state.clone())
            .oneshot(builder.body(body).unwrap())
            .await
            .unwrap()
    }

    async fn add_directory(state: &Arc<AppState>, name: &str) -> String {
        let id = crate::ids::new_id();
        let name = name.to_owned();
        let id_for_db = id.clone();
        state
            .db
            .call_api(move |connection| {
                connection
                    .execute(
                        "INSERT INTO files(id,parent_id,name,kind,status,created_at,updated_at) VALUES(?1,?2,?3,'directory','ready',?4,?4)",
                        rusqlite::params![id_for_db, ROOT_ID, name, Timestamp::now().to_rfc3339()],
                    )
                    .map_err(|error| ApiError::internal(error.to_string()))?;
                Ok(())
            })
            .await
            .unwrap();
        id
    }

    async fn add_file(state: &Arc<AppState>, parent_id: &str, name: &str, bytes: &[u8]) -> String {
        let id = crate::ids::new_id();
        let object_key = revaro_core::keys::blob_key(&id);
        state.store.put(&object_key, bytes).await.unwrap();
        let now = Timestamp::now().to_rfc3339();
        let id_for_db = id.clone();
        let parent_id = parent_id.to_owned();
        let name = name.to_owned();
        let key_for_db = object_key.clone();
        let size = i64::try_from(bytes.len()).unwrap();
        state
            .db
            .call_api(move |connection| {
                connection
                    .execute(
                        "INSERT INTO files(id,parent_id,name,kind,object_key,size,mime_type,status,created_at,updated_at) VALUES(?1,?2,?3,'file',?4,?5,'application/octet-stream','ready',?6,?6)",
                        rusqlite::params![id_for_db, parent_id, name, key_for_db, size, now],
                    )
                    .map_err(|error| ApiError::internal(error.to_string()))?;
                Ok(())
            })
            .await
            .unwrap();
        id
    }

    async fn add_pending_file(state: &Arc<AppState>) -> String {
        let id = add_file(state, ROOT_ID, "pending.bin", b"pending").await;
        state
            .db
            .call_api({
                let id = id.clone();
                move |connection| {
                    connection
                        .execute("UPDATE files SET status='pending' WHERE id=?1", [&id])
                        .map_err(|error| ApiError::internal(error.to_string()))?;
                    Ok(())
                }
            })
            .await
            .unwrap();
        id
    }

    async fn json_body(response: axum::response::Response) -> (StatusCode, serde_json::Value) {
        let status = response.status();
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        (
            status,
            serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null),
        )
    }

    fn entry(name: &str) -> BatchDownloadEntry {
        BatchDownloadEntry {
            file: File::default(),
            name: name.to_owned(),
        }
    }

    #[test]
    fn tickets_are_user_bound_one_time_and_expire() {
        let runtime = BatchDownloadRuntime::new();
        let now = Instant::now();
        let token = runtime
            .issue_at("alice".to_owned(), vec![entry("one.txt")], now)
            .unwrap();
        assert_eq!(token.len(), 43);
        assert!(runtime.consume("bob", &token).is_none());
        assert!(runtime.consume("alice", &token).is_some());
        assert!(runtime.consume("alice", &token).is_none());

        let expired = runtime
            .issue_at(
                "alice".to_owned(),
                vec![entry("expired.txt")],
                now - BATCH_DOWNLOAD_TOKEN_TTL - Duration::from_secs(1),
            )
            .unwrap();
        assert!(runtime.consume("alice", &expired).is_none());
    }

    #[test]
    fn ticket_table_evicts_expired_entries_before_refusing_new_work() {
        let runtime = BatchDownloadRuntime::new();
        let old = Instant::now() - BATCH_DOWNLOAD_TOKEN_TTL - Duration::from_secs(1);
        for _ in 0..MAX_BATCH_DOWNLOAD_TOKENS {
            runtime
                .issue_at("alice".to_owned(), vec![entry("old")], old)
                .unwrap();
        }
        assert_eq!(
            runtime
                .issue_at("alice".to_owned(), vec![entry("new")], Instant::now())
                .unwrap()
                .len(),
            43
        );
    }

    #[tokio::test]
    async fn prepare_and_download_stream_a_safe_unique_zip() {
        let context = context().await;
        let state = &context.state;
        let first = add_file(state, ROOT_ID, "file.txt", b"root content").await;
        let directory = add_directory(state, "nested").await;
        let second = add_file(state, &directory, "file.txt", b"nested content").await;
        let traversal = add_file(state, ROOT_ID, "../evil.txt", b"safe content").await;

        let response = request(
            state,
            http::Method::POST,
            "/api/files/batch-download/prepare",
            Some(serde_json::json!({"ids": [first, second, traversal]})),
            true,
        )
        .await;
        let (status, body) = json_body(response).await;
        assert_eq!(status, StatusCode::OK);
        let token = body["token"].as_str().unwrap().to_owned();

        let response = request(
            state,
            http::Method::GET,
            &format!("/api/files/batch-download/{token}"),
            None,
            true,
        )
        .await;
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response.headers()[http::header::CONTENT_TYPE],
            "application/zip"
        );
        assert_eq!(
            response.headers()[http::header::CONTENT_DISPOSITION],
            "attachment; filename=\"revaro-download.zip\""
        );
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        let mut archive = ZipArchive::new(Cursor::new(bytes)).unwrap();
        assert_eq!(archive.len(), 3);
        let mut names = (0..archive.len())
            .map(|index| archive.by_index(index).unwrap().name().to_owned())
            .collect::<Vec<_>>();
        names.sort();
        assert_eq!(names, ["evil.txt", "file (2).txt", "file.txt"]);
        {
            let mut nested = archive.by_name("file (2).txt").unwrap();
            let mut nested_bytes = Vec::new();
            std::io::Read::read_to_end(&mut nested, &mut nested_bytes).unwrap();
            assert_eq!(nested_bytes, b"nested content");
        }
        assert!(archive.by_name("../evil.txt").is_err());
    }

    #[tokio::test]
    async fn selection_validation_and_authentication_keep_their_statuses() {
        let context = context().await;
        let state = &context.state;
        let ready = add_file(state, ROOT_ID, "ready.txt", b"ready").await;
        let directory = add_directory(state, "folder").await;
        let pending = add_pending_file(state).await;

        let unauthenticated = request(
            state,
            http::Method::POST,
            "/api/files/batch-download/prepare",
            Some(serde_json::json!({"ids": [ready]})),
            false,
        )
        .await;
        assert_eq!(unauthenticated.status(), StatusCode::UNAUTHORIZED);

        let cases = [
            (
                serde_json::json!({"ids": []}),
                StatusCode::BAD_REQUEST,
                "at least one file id is required",
            ),
            (
                serde_json::json!({"ids": [ready.clone(), ready]}),
                StatusCode::BAD_REQUEST,
                "duplicate file id",
            ),
            (
                serde_json::json!({"ids": [directory]}),
                StatusCode::BAD_REQUEST,
                "directories cannot be downloaded in a batch",
            ),
            (
                serde_json::json!({"ids": [pending]}),
                StatusCode::CONFLICT,
                "file is not ready for download",
            ),
            (
                serde_json::json!({"ids": [crate::ids::new_id()]}),
                StatusCode::NOT_FOUND,
                "file not found",
            ),
        ];
        for (body, expected_status, expected_message) in cases {
            let response = request(
                state,
                http::Method::POST,
                "/api/files/batch-download/prepare",
                Some(body),
                true,
            )
            .await;
            let (status, body) = json_body(response).await;
            assert_eq!(status, expected_status);
            assert_eq!(body["error"]["message"], expected_message);
        }
    }

    #[tokio::test]
    async fn streaming_download_requires_authentication() {
        let context = context().await;
        let state = &context.state;
        let file = add_file(state, ROOT_ID, "auth.txt", b"auth").await;
        let response = request(
            state,
            http::Method::POST,
            "/api/files/batch-download/prepare",
            Some(serde_json::json!({"ids": [file]})),
            true,
        )
        .await;
        let (_, body) = json_body(response).await;
        let token = body["token"].as_str().unwrap();

        let response = request(
            state,
            http::Method::GET,
            &format!("/api/files/batch-download/{token}"),
            None,
            false,
        )
        .await;
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn download_token_is_consumed_once() {
        let context = context().await;
        let state = &context.state;
        let file = add_file(state, ROOT_ID, "once.txt", b"once").await;
        let response = request(
            state,
            http::Method::POST,
            "/api/files/batch-download/prepare",
            Some(serde_json::json!({"ids": [file]})),
            true,
        )
        .await;
        let (_, body) = json_body(response).await;
        let token = body["token"].as_str().unwrap().to_owned();

        let first = request(
            state,
            http::Method::GET,
            &format!("/api/files/batch-download/{token}"),
            None,
            true,
        )
        .await;
        assert_eq!(first.status(), StatusCode::OK);
        let _ = first.into_body().collect().await.unwrap();

        let second = request(
            state,
            http::Method::GET,
            &format!("/api/files/batch-download/{token}"),
            None,
            true,
        )
        .await;
        let (status, body) = json_body(second).await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        assert_eq!(
            body["error"]["message"],
            "batch download token not found or expired"
        );
    }
}

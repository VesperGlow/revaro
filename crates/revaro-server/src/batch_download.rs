//! User-bound resumable ZIP tickets. Archives are generated once into the
//! rebuildable cache, then served by the same Range transport as any file.
//! Generation stays on a bounded blocking worker and survives client disconnects.

use std::collections::{HashMap, HashSet};
use std::io::{self, Write};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use axum::Router;
use axum::extract::{FromRequest, Path as PathParam, Request, State};
use axum::response::Response;
use axum::routing::{get, post};
use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use revaro_core::ApiError;
use revaro_core::api::BatchDownloadTicket;
use revaro_core::model::{File, FileKind, FileStatus};
use revaro_core::validate::validate_batch_download_ids;
use rusqlite::Connection;
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
pub const BATCH_DOWNLOAD_TOKEN_TTL: Duration = Duration::from_secs(24 * 3600);
/// Maximum number of outstanding prepared URLs.
pub const MAX_BATCH_DOWNLOAD_TOKENS: usize = 256;

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

    /// Reserve a resumable URL for one authenticated user.
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

    /// Resolve a URL if it belongs to user and has not expired.
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
        tickets.get_mut(token).map(|ticket| {
            ticket.expires_at = now + BATCH_DOWNLOAD_TOKEN_TTL;
            ticket.entries.clone()
        })
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
    let mut used_names = HashSet::new();
    let mut entries = Vec::new();
    let mut queue = std::collections::VecDeque::new();
    for id in ids {
        let file =
            lookup_file(connection, id).map_err(|_| ApiError::not_found("file not found"))?;
        if id == revaro_core::ids::ROOT_ID {
            return Err(ApiError::bad_request("select a folder below the root"));
        }
        let name = unique_zip_name(safe_zip_name(&file.name), &mut used_names);
        queue.push_back((file, name, 0_usize));
    }
    let mut count = 0;
    let mut total = 0_i64;
    while let Some((file, name, depth)) = queue.pop_front() {
        count += 1;
        if count > 5000 || depth > 128 {
            return Err(ApiError::payload_too_large(
                "archive directory tree exceeds limits",
            ));
        }
        if file.status != FileStatus::Ready {
            return Err(ApiError::conflict("file is not ready for download"));
        }
        if file.kind == FileKind::Directory {
            let mut q=connection.prepare(&format!("SELECT {} FROM files WHERE parent_id=?1 AND deleted_at IS NULL ORDER BY name,id LIMIT 5001",crate::file_routes::FILE_COLUMNS)).map_err(|_|ApiError::internal("database error"))?;
            let children = q
                .query_map([&file.id], crate::file_routes::scan_file)
                .map_err(|_| ApiError::internal("database error"))?;
            for child in children {
                let child = child.map_err(|_| ApiError::internal("database error"))?;
                let path = unique_zip_name(
                    format!("{name}/{}", safe_zip_name(&child.name)),
                    &mut used_names,
                );
                queue.push_back((child, path, depth + 1));
                if queue.len() + count > 5000 {
                    return Err(ApiError::payload_too_large(
                        "archive directory tree exceeds limits",
                    ));
                }
            }
            entries.push(BatchDownloadEntry {
                file,
                name: format!("{name}/"),
            });
        } else {
            if file.object_key.is_empty() {
                return Err(ApiError::internal("file content is unavailable"));
            }
            total = total.saturating_add(file.size);
            if entries
                .iter()
                .filter(|e| e.file.kind == FileKind::File)
                .count()
                >= MAX_BATCH_DOWNLOAD_FILES
                || total > (1_i64 << 40)
            {
                return Err(ApiError::payload_too_large(
                    "archive is limited to 1000 files and 1 TiB",
                ));
            }
            entries.push(BatchDownloadEntry { file, name });
        }
    }
    Ok(entries)
}

/// GET /api/files/batch-download/{token}
async fn download(
    State(state): State<Arc<AppState>>,
    user: AuthUser,
    PathParam(token): PathParam<String>,
    headers: http::HeaderMap,
) -> Result<Response, ApiError> {
    let _guard = state.uploads.lock(&format!("batch:{token}")).await;
    let entries = state
        .batch_download
        .consume(&user.username, &token)
        .ok_or_else(|| ApiError::not_found("batch download token not found or expired"))?;
    let store = LocalStore::open(state.config.caches_dir.join("batch-downloads"))
        .await
        .map_err(|_| ApiError::new(507, "archive cache unavailable"))?;
    let key = format!("{token}.zip");
    if store.head(&key).await.is_err() {
        let permit = state.zip_slots.clone().try_acquire_owned().map_err(|_| {
            ApiError::too_many_requests("ZIP preparation is busy; try again shortly")
        })?;
        let required = entries
            .iter()
            .map(|entry| entry.file.size.max(0) as u64)
            .sum::<u64>()
            .saturating_add(1 << 20);
        let root = state.config.caches_dir.join("batch-downloads");
        let reservation = state
            .uploads
            .reserve_space(required, state.config.upload_min_free_bytes as u64, || {
                fs2::available_space(&root)
            })
            .map_err(|_| ApiError::new(507, "archive cache space unavailable"))?
            .ok_or_else(|| {
                ApiError::new(
                    507,
                    "insufficient disk space to prepare a resumable archive",
                )
            })?;
        let temporary = store
            .path_for(&format!("{token}.tmp"))
            .map_err(|_| ApiError::internal("invalid archive key"))?;
        let destination = store
            .path_for(&key)
            .map_err(|_| ApiError::internal("invalid archive key"))?;
        let source = state.store.clone();
        tokio::task::spawn_blocking(move || {
            let _permit = permit;
            let _reservation = reservation;
            let _generation_guard = _guard;
            let result = (|| -> Result<(), BatchZipError> {
                let file = std::fs::File::create(&temporary)?;
                write_zip(&source, entries, file)?;
                std::fs::rename(&temporary, &destination)?;
                Ok(())
            })();
            if result.is_err() {
                let _ = std::fs::remove_file(&temporary);
            }
            result
        })
        .await
        .map_err(|_| ApiError::new(502, "archive preparation interrupted"))?
        .map_err(|error| {
            tracing::warn!(%error, "archive preparation failed");
            ApiError::new(502, "archive preparation failed")
        })?;
    }
    let object = crate::file_access::FileAccess::open(&store, &key)
        .await
        .map_err(|_| ApiError::new(502, "archive read failed"))?;
    crate::transfer::serve_reader(
        object.reader,
        object.size,
        &object.etag,
        "application/zip",
        "attachment; filename=\"revaro-download.zip\"",
        headers,
    )
    .await
}

/// Remove abandoned archive artifacts, while retaining all live tickets.
pub(crate) async fn cleanup_archives(state: &AppState) -> Result<(), String> {
    let live = {
        let mut tickets = state
            .batch_download
            .tickets
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        tickets.retain(|_, ticket| ticket.expires_at > Instant::now());
        tickets.keys().cloned().collect::<HashSet<_>>()
    };
    let directory = state.config.caches_dir.join("batch-downloads");
    let mut entries = match tokio::fs::read_dir(directory).await {
        Ok(entries) => entries,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error.to_string()),
    };
    while let Some(entry) = entries.next_entry().await.map_err(|e| e.to_string())? {
        let name = entry.file_name().to_string_lossy().into_owned();
        let stem = name.split('.').next().unwrap_or_default();
        if live.contains(stem) {
            continue;
        }
        let metadata = entry.metadata().await.map_err(|e| e.to_string())?;
        let old = metadata
            .modified()
            .ok()
            .and_then(|v| v.elapsed().ok())
            .is_some_and(|age| age > BATCH_DOWNLOAD_TOKEN_TTL);
        if metadata.is_file() && old {
            let _ = tokio::fs::remove_file(entry.path()).await;
        }
    }
    Ok(())
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

/// Generate a ZIP into a bounded disk artifact; no whole-archive memory buffer.
fn write_zip(
    store: &LocalStore,
    entries: Vec<BatchDownloadEntry>,
    sink: impl Write,
) -> Result<(), BatchZipError> {
    let mut archive = ZipWriter::new_stream(sink);
    let options = SimpleFileOptions::default().compression_method(CompressionMethod::Deflated);
    for entry in entries {
        if entry.file.kind == FileKind::Directory {
            archive.add_directory(entry.name, options)?;
            continue;
        }
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
            "APP_CACHES_DIR" => Some(root.0.join("caches").to_string_lossy().into_owned()),
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
    fn tickets_are_user_bound_resumable_and_expire() {
        let runtime = BatchDownloadRuntime::new();
        let now = Instant::now();
        let token = runtime
            .issue_at("alice".to_owned(), vec![entry("one.txt")], now)
            .unwrap();
        assert_eq!(token.len(), 43);
        assert!(runtime.consume("bob", &token).is_none());
        assert!(runtime.consume("alice", &token).is_some());
        assert!(runtime.consume("alice", &token).is_some());

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
    async fn download_token_can_resume() {
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
        assert_eq!(second.status(), StatusCode::OK);
        assert_eq!(second.headers()[http::header::ACCEPT_RANGES], "bytes");
        assert!(second.headers().contains_key(http::header::ETAG));
        let second_bytes = second.into_body().collect().await.unwrap().to_bytes();
        assert!(second_bytes.starts_with(b"PK"));
    }
}

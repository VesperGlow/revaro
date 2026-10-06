//! Uploads: creating a session, streaming bytes, and committing the file.
//!
//! Upload sessions retain two wire modes:
//!
//! * **single** — empty files and existing legacy sessions use one `PUT`.
//! * **multipart** — all new nonempty files use durable, checksummed chunks.
//!   Completion can use the authoritative acknowledgements; the legacy URL
//!   remains available for single-part sessions, along with the ACK routes.
//!
//! The bytes always go to [`LocalStore`]; this module only owns the session
//! bookkeeping in `uploads`/`upload_parts` and the commit transaction.
//!
//! ## Why the commit is a transaction
//!
//! A file becomes visible only once its bytes are durable *and* its `files` row
//! is `ready`. Doing those separately would leave a window where a `ready` row
//! points at absent or half-written bytes. The commit therefore flips the
//! `files` row and the `uploads` row in one transaction, and completing twice
//! returns the already-committed file instead of creating a second one.
//!
//! Single bodies are hashed while receiving, multipart bodies while assembling.
//! Durable commit phases allow publication to resume after a crash without
//! deleting staged parts early. Distinct parts can write concurrently; the
//! completion and abort paths exclusively lock the session.

use std::sync::Arc;

use axum::body::Body;
use axum::extract::{FromRequest, Path as PathParam, Request, State};
use axum::routing::{get, post, put};
use axum::{Json, Router};
use futures_util::StreamExt as _;
use http::StatusCode;
use revaro_core::ApiError;
use revaro_core::api::uploads::{
    CreateUpload, CreateUploadRequest, PartUrl, UploadPartsResponse,
    UploadStatus as UploadStatusResponse,
};
use revaro_core::keys;
use revaro_core::limits;
use revaro_core::model::{UploadMode, UploadPart, UploadStatus};
use revaro_core::storage::CompletedPart;
use revaro_core::time::Timestamp;
use revaro_core::validate;
use rusqlite::Connection;
use serde::Deserialize;
use tokio_util::io::StreamReader;

use crate::auth::extract::AuthUser;
use crate::auth_routes::JsonBody;
use crate::db::DbError;
use crate::state::AppState;
use crate::storage::StorageError;

/// Route table for the upload surface.
pub fn routes() -> Router<Arc<AppState>> {
    Router::new()
        .route("/uploads", post(create_upload))
        .route("/uploads/{id}", get(get_upload).delete(abort_upload))
        .route("/uploads/{id}/data", put(upload_content))
        .route("/uploads/{id}/data/{part}", put(upload_content_part))
        .route("/uploads/{id}/parts", post(upload_parts))
        .route("/uploads/{id}/parts/{part}", put(record_upload_part))
        .route("/uploads/{id}/complete", post(complete_upload))
}

/// The mutable state of an upload session.
struct UploadRecord {
    id: String,
    file_id: String,
    mode: UploadMode,
    object_key: String,
    multipart_id: Option<String>,
    part_size: i64,
    expected_size: i64,
    mime_type: String,
    status: UploadStatus,
    expires_at: Timestamp,
    commit_state: String,
    staged_etag: Option<String>,
    content_hash: Option<String>,
    request_fingerprint: Option<String>,
}

/// The Go decoder filled omitted upload fields with their zero values before
/// validation. Keep that wire behaviour instead of letting Axum turn a
/// missing required member into a framework-level 422 response.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct CreateUploadInput {
    parent_id: Option<String>,
    name: Option<String>,
    size: Option<i64>,
    mime_type: Option<String>,
    idempotency_key: Option<String>,
}

/// The Go decoder zero-filled omitted members before each upload endpoint
/// performed its own validation. Route-local inputs preserve that distinction
/// from malformed JSON, which is reported as the historical 400 envelope.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct UploadPartsInput {
    part_numbers: Option<Vec<Option<i32>>>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RecordUploadPartInput {
    etag: Option<String>,
    size: Option<i64>,
    content_hash: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct CompletePartInput {
    part_number: Option<i32>,
    etag: Option<String>,
    // The old Go completion decoder rejects these fields even though the old
    // resume response returns them. Rust accepts them so a resumed upload can
    // complete; the explicit old defect is covered by the parity test.
    size: Option<i64>,
    content_hash: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct CompleteUploadInput {
    parts: Option<Vec<Option<CompletePartInput>>>,
}

impl UploadRecord {
    fn is_multipart(&self) -> bool {
        self.mode == UploadMode::Multipart
    }

    /// True when the session may no longer be used. An unparseable expiry counts
    /// as expired, matching Go: a corrupt row must not become a permanent
    /// writable session.
    fn is_expired(&self, now: Timestamp) -> bool {
        self.expires_at.is_expired_at(now)
    }

    /// Bytes expected for part `number`, or `None` when the number is invalid.
    ///
    /// Only the final part may be short, so the expectation is exact: a client
    /// cannot silently upload a truncated middle part.
    fn expected_part_size(&self, number: i32) -> Option<i64> {
        let count = limits::multipart_part_count(self.expected_size, self.part_size).ok()? as i32;
        if number < 1 || number > count {
            return None;
        }
        if number < count {
            return Some(self.part_size);
        }
        Some(self.expected_size - self.part_size * (count as i64 - 1))
    }
}

fn load_upload(connection: &Connection, id: &str) -> Result<UploadRecord, DbError> {
    connection
        .query_row(
            "SELECT id,file_id,mode,object_key,multipart_id,part_size,expected_size,mime_type,status,expires_at,commit_state,staged_etag,content_hash,request_fingerprint \
FROM uploads WHERE id = ?1",
            [id],
            |row| {
                Ok(UploadRecord {
                    id: row.get(0)?,
                    file_id: row.get(1)?,
                    mode: row.get::<_, String>(2)?.parse().map_err(|_| {
                        rusqlite::Error::InvalidColumnType(2, "mode".into(), rusqlite::types::Type::Text)
                    })?,
                    object_key: row.get(3)?,
                    multipart_id: row.get(4)?,
                    part_size: row.get(5)?,
                    expected_size: row.get(6)?,
                    mime_type: row.get(7)?,
                    status: row.get::<_, String>(8)?.parse().map_err(|_| {
                        rusqlite::Error::InvalidColumnType(8, "status".into(), rusqlite::types::Type::Text)
                    })?,
                    expires_at: Timestamp::parse(&row.get::<_, String>(9)?).map_err(|_| {
                        rusqlite::Error::InvalidColumnType(9, "expires_at".into(), rusqlite::types::Type::Text)
                    })?,
                    commit_state: row.get(10)?,
                    staged_etag: row.get(11)?,
                    content_hash: row.get(12)?,
                    request_fingerprint: row.get(13)?,
                })
            },
        )
        .map_err(DbError::Query)
}

fn load_acknowledged_parts(
    connection: &Connection,
    upload_id: &str,
) -> Result<Vec<CompletedPart>, DbError> {
    let mut statement = connection
        .prepare(
            "SELECT part_number, etag, size, content_hash FROM upload_parts \
             WHERE upload_id = ?1 ORDER BY part_number",
        )
        .map_err(DbError::Query)?;
    let rows = statement
        .query_map([upload_id], |row| {
            Ok(CompletedPart {
                part_number: row.get(0)?,
                etag: row.get(1)?,
                size: Some(row.get(2)?),
                content_hash: row.get::<_, Option<String>>(3)?,
            })
        })
        .map_err(DbError::Query)?;
    rows.collect::<Result<Vec<_>, _>>().map_err(DbError::Query)
}

/// A live, non-expired session, or the `404` every upload endpoint shares.
fn require_pending(connection: &Connection, id: &str) -> Result<UploadRecord, ApiError> {
    let record = load_upload(connection, id).map_err(|error| {
        if error.is_not_found() {
            pending_missing()
        } else {
            database_error(error)
        }
    })?;
    if record.status != UploadStatus::Pending
        || record.is_expired(Timestamp::now())
        || record.commit_state != "receiving"
    {
        return Err(pending_missing());
    }
    Ok(record)
}

fn pending_missing() -> ApiError {
    ApiError::not_found("pending upload not found")
}

fn upload_missing() -> ApiError {
    ApiError::not_found("upload not found")
}

/// [`load_upload`] adapted to the error type `call_api` expects.
fn load_upload_api(connection: &Connection, id: &str) -> Result<UploadRecord, ApiError> {
    load_upload(connection, id).map_err(|error| {
        if error.is_not_found() {
            pending_missing()
        } else {
            database_error(error)
        }
    })
}

fn database_error(error: DbError) -> ApiError {
    tracing::error!(%error, "upload query failed");
    ApiError::internal("database error")
}

/// `POST /api/uploads`
async fn create_upload(
    State(state): State<Arc<AppState>>,
    _user: AuthUser,
    JsonBody(input): JsonBody<CreateUploadInput>,
) -> Result<(StatusCode, Json<CreateUpload>), ApiError> {
    let request = CreateUploadRequest {
        parent_id: input.parent_id.unwrap_or_default(),
        name: input.name.unwrap_or_default(),
        size: input.size.unwrap_or_default(),
        mime_type: input.mime_type.unwrap_or_default(),
        idempotency_key: input.idempotency_key.unwrap_or_default(),
    };
    validate::validate_name(&request.name)?;
    validate::validate_file_size(request.size)?;
    let mime_type = if request.mime_type.is_empty() {
        "application/octet-stream".to_owned()
    } else {
        request.mime_type
    };
    validate::validate_mime_type(&mime_type)?;

    if request.idempotency_key.len() > 128 || request.idempotency_key.chars().any(char::is_control)
    {
        return Err(ApiError::bad_request("invalid idempotency key"));
    }
    let fingerprint = keys::sha256_hex(
        serde_json::to_string(&(&request.parent_id, &request.name, request.size, &mime_type))
            .map_err(|_| ApiError::internal("upload fingerprint failed"))?
            .as_bytes(),
    );
    let _creation_guard = if request.idempotency_key.is_empty() {
        None
    } else {
        Some(
            state
                .uploads
                .lock(&format!("create:{}", request.idempotency_key))
                .await,
        )
    };
    if !request.idempotency_key.is_empty() {
        let key = request.idempotency_key.clone();
        let existing = state
            .db
            .call_api(move |connection| {
                use rusqlite::OptionalExtension as _;
                let id = connection
                    .query_row(
                        "SELECT id FROM uploads WHERE idempotency_key=?1",
                        [key],
                        |row| row.get::<_, String>(0),
                    )
                    .optional()
                    .map_err(|error| database_error(DbError::Query(error)))?;
                match id {
                    Some(id) => match load_upload_api(connection, &id) {
                        Ok(record) => Ok(Some(record)),
                        Err(error) if error.status == 404 => Ok(None),
                        Err(error) => Err(error),
                    },
                    None => Ok(None),
                }
            })
            .await?;
        if let Some(record) = existing {
            if record.request_fingerprint.as_deref() != Some(&fingerprint) {
                return Err(ApiError::conflict(
                    "idempotency key belongs to a different upload",
                ));
            }
            if record.status == UploadStatus::Completed
                || (record.status == UploadStatus::Pending
                    && (record.commit_state != "receiving" || !record.is_expired(Timestamp::now())))
            {
                return Ok((StatusCode::CREATED, Json(created_response(&record))));
            }
            match abort_pending_upload(&state, &record.id, true).await {
                Ok(()) => {}
                Err(error) if error.status == 404 => {
                    // Completion or the expiry worker may have won while we
                    // waited for the lifecycle lock. Reconcile before creating.
                    let id = record.id;
                    match state.db.call_api(move |c| load_upload_api(c, &id)).await {
                        Ok(record)
                            if matches!(
                                record.status,
                                UploadStatus::Pending | UploadStatus::Completed
                            ) =>
                        {
                            return Ok((StatusCode::CREATED, Json(created_response(&record))));
                        }
                        Ok(_) => return Err(pending_missing()),
                        Err(error) if error.status == 404 => {}
                        Err(error) => return Err(error),
                    }
                }
                Err(error) => return Err(error),
            }
        }
    }
    // Reclaim an expired placeholder immediately instead of making a new
    // upload wait for the periodic cleanup worker to free its name.
    let (parent, name) = (request.parent_id.clone(), request.name.clone());
    let expired = state
        .db
        .call_api(move |connection| {
            use rusqlite::OptionalExtension as _;
            connection
                .query_row(
                    "SELECT u.id FROM uploads u JOIN files f ON f.id=u.file_id \
                     WHERE f.parent_id=?1 AND f.name=?2 AND f.deleted_at IS NULL \
                     AND u.status='pending' AND u.commit_state='receiving' \
                     AND julianday(u.expires_at)<=julianday(?3)",
                    rusqlite::params![parent, name, Timestamp::now().to_rfc3339()],
                    |row| row.get::<_, String>(0),
                )
                .optional()
                .map_err(|error| database_error(DbError::Query(error)))
        })
        .await?;
    if let Some(id) = expired {
        match abort_pending_upload(&state, &id, true).await {
            Ok(()) => {}
            Err(error) if error.status == 404 => {}
            Err(error) => return Err(error),
        }
    }
    let _space = reserve_space(
        &state,
        request
            .size
            .saturating_mul(if limits::uses_multipart_upload(request.size) {
                2
            } else {
                1
            }),
    )
    .await?;

    let parent_id = request.parent_id.clone();
    let parent_valid = state
        .db
        .call_api(move |connection| {
            connection
                .query_row(
                    "SELECT EXISTS(SELECT 1 FROM files WHERE id = ?1 AND kind = 'directory' \
AND status = 'ready' AND deleted_at IS NULL)",
                    [&parent_id],
                    |row| row.get::<_, bool>(0),
                )
                .map_err(|error| database_error(DbError::Query(error)))
        })
        .await?;
    if !parent_valid {
        return Err(ApiError::bad_request("parent directory is invalid"));
    }

    let multipart = limits::uses_multipart_upload(request.size);
    let part_size = if multipart {
        limits::multipart_part_size(request.size)
    } else {
        request.size.max(1)
    };
    let part_count = if multipart {
        limits::multipart_part_count(request.size, part_size).map_err(ApiError::bad_request)?
    } else {
        0
    };

    let file_id = crate::ids::new_id();
    let upload_id = crate::ids::new_id();
    let object_key = keys::blob_key(&crate::ids::new_id());

    // The staging session is created before the metadata transaction and removed
    // again if the transaction fails, so a rejected upload never leaves an
    // orphaned multipart directory. Same ordering as Go.
    let multipart_id = if multipart {
        Some(
            state
                .store
                .create_multipart(&object_key)
                .await
                .map_err(|error| {
                    tracing::error!(%error, "multipart initialization failed");
                    ApiError::new(502, "object storage could not initialize the upload")
                })?,
        )
    } else {
        None
    };

    let url = format!("/api/uploads/{upload_id}/data");
    let expires_at =
        Timestamp::from_system_time(std::time::SystemTime::now() + state.config.upload_expires);

    let insert = {
        let (parent_id, name, size) = (
            request.parent_id.clone(),
            request.name.clone(),
            request.size,
        );
        let (file_id, upload_id) = (file_id.clone(), upload_id.clone());
        let (object_key, multipart_id, mime_type) =
            (object_key.clone(), multipart_id.clone(), mime_type.clone());
        let mode = if multipart {
            UploadMode::Multipart
        } else {
            UploadMode::Single
        };
        let idempotency_key =
            (!request.idempotency_key.is_empty()).then_some(request.idempotency_key.clone());
        state
            .db
            .call_api(move |connection| {
                let now = Timestamp::now().to_rfc3339();
                let transaction = connection
                    .transaction()
                    .map_err(|error| database_error(DbError::Query(error)))?;

                // `INSERT ... SELECT ... WHERE EXISTS` makes the parent check and
                // the insert one atomic statement, so a directory deleted
                // concurrently cannot slip between them.
                let inserted = transaction
                    .execute(
                        "INSERT INTO files(id,parent_id,name,kind,object_key,size,mime_type,status,\
created_at,updated_at) SELECT ?1,?2,?3,'file',?4,?5,?6,'pending',?7,?7 \
WHERE EXISTS(SELECT 1 FROM files WHERE id = ?2 AND kind = 'directory' \
AND status = 'ready' AND deleted_at IS NULL)",
                        rusqlite::params![
                            file_id, parent_id, name, object_key, size, mime_type, now
                        ],
                    )
                    .map_err(|error| conflict_or(DbError::Query(error)))?;
                if inserted != 1 {
                    return Err(ApiError::conflict(
                        "parent directory is no longer available",
                    ));
                }

                transaction
                    .execute(
                        "INSERT INTO uploads(id,file_id,mode,object_key,multipart_id,part_size,\
expected_size,mime_type,status,created_at,expires_at,idempotency_key,request_fingerprint) \
VALUES(?1,?2,?3,?4,?5,?6,?7,?8,'pending',?9,?10,?11,?12)",
                        rusqlite::params![
                            upload_id,
                            file_id,
                            mode.as_str(),
                            object_key,
                            multipart_id,
                            part_size,
                            size,
                            mime_type,
                            now,
                            expires_at.to_rfc3339(),
                            idempotency_key,
                            fingerprint,
                        ],
                    )
                    .map_err(|error| conflict_or(DbError::Query(error)))?;

                transaction
                    .commit()
                    .map_err(|error| database_error(DbError::Query(error)))?;
                Ok(())
            })
            .await
    };

    if let Err(error) = insert {
        if let Some(id) = &multipart_id {
            let _ = state.store.abort_multipart(&object_key, id).await;
        }
        return Err(error);
    }

    Ok((
        StatusCode::CREATED,
        Json(CreateUpload {
            upload_id,
            file_id,
            mode: if multipart {
                UploadMode::Multipart
            } else {
                UploadMode::Single
            },
            url,
            part_size,
            part_count,
            expires_at,
        }),
    ))
}

/// `GET /api/uploads/{id}`
async fn get_upload(
    State(state): State<Arc<AppState>>,
    _user: AuthUser,
    PathParam(id): PathParam<String>,
) -> Result<Json<UploadStatusResponse>, ApiError> {
    state
        .db
        .call_api(move |connection| {
            let record = load_upload(connection, &id).map_err(|error| {
                if error.is_not_found() {
                    upload_missing()
                } else {
                    database_error(error)
                }
            })?;
            if record.status == UploadStatus::Pending
                && record.commit_state == "receiving"
                && record.is_expired(Timestamp::now())
            {
                return Err(ApiError::new(410, "upload expired; create a new upload"));
            }
            let mut statement = connection
                .prepare(
                    "SELECT part_number,size,etag,COALESCE(content_hash,'') FROM upload_parts \
WHERE upload_id = ?1 ORDER BY part_number",
                )
                .map_err(|error| database_error(DbError::Query(error)))?;
            let rows = statement
                .query_map([&record.id], |row| {
                    Ok(UploadPart {
                        part_number: row.get(0)?,
                        size: Some(row.get(1)?),
                        etag: row.get(2)?,
                        content_hash: Some(row.get(3)?),
                    })
                })
                .map_err(|error| database_error(DbError::Query(error)))?;
            let mut parts = Vec::new();
            for row in rows {
                parts.push(row.map_err(|error| database_error(DbError::Query(error)))?);
            }
            drop(statement);

            let part_count =
                limits::multipart_part_count(record.expected_size, record.part_size).unwrap_or(0);
            let url = if record.mode == UploadMode::Single && record.status == UploadStatus::Pending
            {
                format!("/api/uploads/{}/data", record.id)
            } else {
                String::new()
            };
            Ok(UploadStatusResponse {
                upload_id: record.id,
                file_id: record.file_id,
                mode: record.mode,
                url,
                part_size: record.part_size,
                part_count,
                expected_size: record.expected_size,
                mime_type: record.mime_type,
                status: record.status,
                finalizing: record.commit_state != "receiving",
                data_received: record.staged_etag.is_some(),
                expires_at: record.expires_at,
                parts,
            })
        })
        .await
        .map(Json)
}

/// Adapt the request body into an [`tokio::io::AsyncRead`] so a large upload is
/// streamed to disk rather than buffered in memory.
fn body_reader(
    body: Body,
    idle: std::time::Duration,
    total: std::time::Duration,
) -> impl tokio::io::AsyncRead + Unpin {
    let deadline = tokio::time::Instant::now() + total;
    let stream = futures_util::stream::unfold(
        (body.into_data_stream(), false),
        move |(mut stream, done)| async move {
            if done {
                return None;
            }
            let until = deadline.min(tokio::time::Instant::now() + idle);
            match tokio::time::timeout_at(until, stream.next()).await {
                Ok(Some(Ok(bytes))) => Some((Ok::<_, std::io::Error>(bytes), (stream, false))),
                Ok(Some(Err(e))) => Some((Err(std::io::Error::other(e)), (stream, true))),
                Ok(None) => None,
                Err(_) => Some((
                    Err(std::io::Error::new(
                        std::io::ErrorKind::TimedOut,
                        "upload made no progress or exceeded its request timeout",
                    )),
                    (stream, true),
                )),
            }
        },
    );
    StreamReader::new(Box::pin(stream))
}
async fn reserve_space(
    state: &Arc<AppState>,
    bytes: i64,
) -> Result<crate::state::UploadSpaceGuard, ApiError> {
    let root = state.store.root().to_owned();
    let state = Arc::clone(state);
    tokio::task::spawn_blocking(move || {
        state.uploads.reserve_space(
            bytes.max(0) as u64,
            state.config.upload_min_free_bytes as u64,
            || fs2::available_space(root),
        )
    })
    .await
    .map_err(|_| ApiError::internal("disk space check failed"))?
    .map_err(|_| ApiError::internal("disk space check failed"))?
    .ok_or_else(|| ApiError::new(507, "insufficient disk space for upload"))
}

fn created_response(record: &UploadRecord) -> CreateUpload {
    CreateUpload {
        upload_id: record.id.clone(),
        file_id: record.file_id.clone(),
        mode: record.mode,
        url: format!("/api/uploads/{}/data", record.id),
        part_size: record.part_size,
        part_count: if record.is_multipart() {
            limits::multipart_part_count(record.expected_size, record.part_size).unwrap_or(0)
        } else {
            0
        },
        expires_at: record.expires_at,
    }
}

async fn io_slot(state: &AppState) -> Result<tokio::sync::OwnedSemaphorePermit, ApiError> {
    Arc::clone(&state.uploads.io_slots)
        .acquire_owned()
        .await
        .map_err(|_| ApiError::unavailable("upload service unavailable"))
}

async fn accepted_hash(state: &AppState, key: &str) -> Result<Option<String>, ApiError> {
    match state.store.head(key).await {
        Ok(_) => state
            .store
            .sha256_hex(key)
            .await
            .map(Some)
            .map_err(complete_error),
        Err(StorageError::NotFound) => Ok(None),
        Err(error) => Err(complete_error(error)),
    }
}

fn claimed_hash(request: &Request, known: Option<&str>) -> Result<Option<String>, ApiError> {
    let Some(raw) = request.headers().get("x-content-sha256") else {
        return Ok(None);
    };
    let hash = raw
        .to_str()
        .map_err(|_| ApiError::bad_request("invalid SHA-256"))?
        .to_ascii_lowercase();
    if hash.len() != 64 || !hash.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(ApiError::bad_request("invalid SHA-256"));
    }
    if known.is_some_and(|v| v != hash) {
        return Err(ApiError::conflict("upload retry contains different bytes"));
    }
    Ok(Some(hash))
}

fn content_length_mismatch(request: &Request, expected: i64) -> bool {
    request
        .headers()
        .get(http::header::CONTENT_LENGTH)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse::<i64>().ok())
        .is_some_and(|length| length >= 0 && length != expected)
}

/// `PUT /api/uploads/{id}/data` — a whole single-request upload.
async fn upload_content(
    State(state): State<Arc<AppState>>,
    _user: AuthUser,
    PathParam(id): PathParam<String>,
    request: Request,
) -> Result<http::Response<Body>, ApiError> {
    let _guard = state.uploads.lock_part(&id, 0).await;
    let record = state.db.call_api(move |c| require_pending(c, &id)).await?;
    if record.is_multipart() {
        // Compatibility for existing API clients sending a one-block file to /data.
        if limits::multipart_part_count(record.expected_size, record.part_size) == Ok(1) {
            return upload_content_part(State(state), _user, PathParam((record.id, 1)), request)
                .await;
        }
        return Err(ApiError::bad_request("use numbered chunks for this upload"));
    }
    let _slot = io_slot(&state).await?;
    if content_length_mismatch(&request, record.expected_size) {
        return Err(ApiError::bad_request("upload size mismatch"));
    }
    let known_hash = match record.content_hash.clone().filter(|hash| !hash.is_empty()) {
        Some(hash) => Some(hash),
        None => accepted_hash(&state, &record.object_key).await?,
    };
    let claimed = claimed_hash(&request, known_hash.as_deref())?;
    let _space = reserve_space(&state, record.expected_size).await?;
    let mut reader = body_reader(
        request.into_body(),
        state.config.upload_idle_timeout,
        state.config.upload_request_timeout,
    );
    let stored = state
        .store
        .write_stream_checked(
            &record.object_key,
            &mut reader,
            record.expected_size,
            known_hash.as_deref(),
            claimed.as_deref(),
        )
        .await
        .map_err(write_error)?;
    let (upload_id, etag, hash) = (
        record.id,
        stored.info.etag.clone(),
        stored.content_hash.clone(),
    );
    state
        .db
        .call_api(move |connection| {
            let changed = connection
                .execute(
                    "UPDATE uploads SET staged_etag=?1,content_hash=?2 \
                     WHERE id=?3 AND status='pending' AND commit_state='receiving'",
                    rusqlite::params![etag, hash, upload_id],
                )
                .map_err(|error| database_error(DbError::Query(error)))?;
            if changed != 1 {
                return Err(pending_missing());
            }
            Ok(())
        })
        .await?;
    Ok(etag_response(&stored.info.etag, &stored.content_hash))
}

/// A successful part PUT includes the durable database acknowledgement.
async fn upload_content_part(
    State(state): State<Arc<AppState>>,
    _user: AuthUser,
    PathParam((id, part)): PathParam<(String, i32)>,
    request: Request,
) -> Result<http::Response<Body>, ApiError> {
    let _guard = state.uploads.lock_part(&id, part).await;
    let _slot = io_slot(&state).await?;
    let record = state
        .db
        .call_api({
            let id = id.clone();
            move |c| require_pending(c, &id)
        })
        .await?;
    if !record.is_multipart() {
        return Err(ApiError::bad_request("single upload has no parts"));
    }
    let expected = record
        .expected_part_size(part)
        .ok_or_else(|| ApiError::bad_request("invalid part number"))?;
    if content_length_mismatch(&request, expected) {
        return Err(ApiError::bad_request("upload size mismatch"));
    }
    let multipart_id = record.multipart_id.as_deref().ok_or_else(pending_missing)?;
    let key = format!(
        "{}/{part}",
        keys::multipart_dir(multipart_id, &record.object_key)
    );
    let known_hash = state
        .db
        .call_api({
            let id = id.clone();
            move |c| {
                use rusqlite::OptionalExtension as _;
                c.query_row(
                    "SELECT content_hash FROM upload_parts WHERE upload_id=?1 AND part_number=?2",
                    rusqlite::params![id, part],
                    |r| r.get::<_, Option<String>>(0),
                )
                .optional()
                .map(|hash| hash.flatten().filter(|hash| !hash.is_empty()))
                .map_err(|e| database_error(DbError::Query(e)))
            }
        })
        .await?;
    let known_hash = match known_hash {
        Some(hash) => Some(hash),
        None => accepted_hash(&state, &key).await?,
    };
    let claimed = claimed_hash(&request, known_hash.as_deref())?;
    let _space = reserve_space(&state, expected).await?;
    let mut reader = body_reader(
        request.into_body(),
        state.config.upload_idle_timeout,
        state.config.upload_request_timeout,
    );
    let stored = state
        .store
        .write_stream_checked(
            &key,
            &mut reader,
            expected,
            known_hash.as_deref(),
            claimed.as_deref(),
        )
        .await
        .map_err(write_error)?;
    let etag = stored.info.etag.clone();
    let hash = stored.content_hash.clone();
    state
        .db
        .call_api(move |connection| {
            connection
                .execute(
                    "INSERT INTO upload_parts(upload_id,part_number,size,etag,content_hash,completed_at) \
                     VALUES(?1,?2,?3,?4,?5,?6) ON CONFLICT(upload_id,part_number) DO UPDATE \
                     SET size=excluded.size,etag=excluded.etag,content_hash=excluded.content_hash,completed_at=excluded.completed_at",
                    rusqlite::params![id, part, expected, etag, hash, Timestamp::now().to_rfc3339()],
                )
                .map_err(|error| database_error(DbError::Query(error)))?;
            Ok(())
        })
        .await?;
    Ok(etag_response(&stored.info.etag, &stored.content_hash))
}

/// `POST /api/uploads/{id}/parts` — legacy batch of authenticated local URLs.
async fn upload_parts(
    State(state): State<Arc<AppState>>,
    _user: AuthUser,
    PathParam(id): PathParam<String>,
    request: Request,
) -> Result<Json<UploadPartsResponse>, ApiError> {
    let record = state
        .db
        .call_api(move |connection| {
            let record = require_pending(connection, &id)?;
            if !record.is_multipart() {
                return Err(pending_missing());
            }
            Ok(record)
        })
        .await?;
    let JsonBody(request) = JsonBody::<UploadPartsInput>::from_request(request, &state).await?;
    let part_numbers = request
        .part_numbers
        .unwrap_or_default()
        .into_iter()
        .map(|part_number| part_number.unwrap_or_default())
        .collect::<Vec<_>>();
    let part_count =
        limits::multipart_part_count(record.expected_size, record.part_size).unwrap_or(0);
    let numbers = validate::validate_upload_part_batch(&part_numbers, part_count)?;
    let parts = numbers
        .into_iter()
        .map(|part_number| PartUrl {
            part_number,
            url: format!("/api/uploads/{}/data/{part_number}", record.id),
        })
        .collect();
    Ok(Json(UploadPartsResponse { parts }))
}

/// `PUT /api/uploads/{id}/parts/{part}` — acknowledge a stored part.
async fn record_upload_part(
    State(state): State<Arc<AppState>>,
    _user: AuthUser,
    PathParam((id, part)): PathParam<(String, i32)>,
    request: Request,
) -> Result<StatusCode, ApiError> {
    let _guard = state.uploads.lock_part(&id, part).await;
    let record = state
        .db
        .call_api({
            let id = id.clone();
            move |c| require_pending(c, &id)
        })
        .await?;
    if !record.is_multipart() {
        return Err(pending_missing());
    }
    let expected = record
        .expected_part_size(part)
        .ok_or_else(|| ApiError::bad_request("invalid multipart part number"))?;
    let JsonBody(input) = JsonBody::<RecordUploadPartInput>::from_request(request, &state).await?;
    let etag = input.etag.unwrap_or_default();
    if etag.trim().is_empty()
        || input.size.unwrap_or_default() != expected
        || input.content_hash.unwrap_or_default().len() > 128
    {
        return Err(ApiError::bad_request(
            "invalid uploaded part acknowledgement",
        ));
    }
    let key = format!(
        "{}/{part}",
        keys::multipart_dir(
            record.multipart_id.as_deref().ok_or_else(pending_missing)?,
            &record.object_key
        )
    );
    let stored = state.store.head(&key).await.map_err(complete_error)?;
    if stored.size != expected || stored.etag != etag.trim().trim_matches('"') {
        return Err(ApiError::conflict(
            "part acknowledgement does not match stored bytes",
        ));
    }
    // New PUTs have already acknowledged their bytes. A session written before
    // the upgrade can still acknowledge its stored part, using a server hash.
    let has_ack = state
        .db
        .call({
            let id = record.id.clone();
            move |connection| {
                connection
                    .query_row(
                        "SELECT EXISTS(SELECT 1 FROM upload_parts WHERE upload_id=?1 AND part_number=?2)",
                        rusqlite::params![id, part],
                        |row| row.get::<_, bool>(0),
                    )
                    .map_err(DbError::Query)
            }
        })
        .await
        .map_err(database_error)?;
    if !has_ack {
        let _slot = io_slot(&state).await?;
        let hash = state.store.sha256_hex(&key).await.map_err(complete_error)?;
        state
            .db
            .call_api(move |connection| {
                connection
                    .execute(
                        "INSERT INTO upload_parts(upload_id,part_number,etag,size,content_hash,completed_at) \
                         VALUES(?1,?2,?3,?4,?5,?6)",
                        rusqlite::params![record.id, part, stored.etag, expected, hash, Timestamp::now().to_rfc3339()],
                    )
                    .map_err(|error| database_error(DbError::Query(error)))?;
                Ok(())
            })
            .await?;
    }
    Ok(StatusCode::NO_CONTENT)
}

/// `POST /api/uploads/{id}/complete`
async fn complete_upload(
    State(state): State<Arc<AppState>>,
    _user: AuthUser,
    PathParam(id): PathParam<String>,
    request: Request,
) -> Result<Json<revaro_core::model::File>, ApiError> {
    let JsonBody(input) = JsonBody::<CompleteUploadInput>::from_request(request, &state).await?;
    let parts = input
        .parts
        .unwrap_or_default()
        .into_iter()
        .map(|part| {
            let part = part.unwrap_or_default();
            CompletedPart {
                part_number: part.part_number.unwrap_or_default(),
                etag: part.etag.unwrap_or_default(),
                size: part.size,
                content_hash: part.content_hash,
            }
        })
        .collect();
    finalize_upload(&state, &id, parts).await.map(Json)
}

/// Resume every publication phase from durable metadata. The file is visible
/// only after the final transaction; staging is retained throughout failures.
async fn finalize_upload(
    state: &Arc<AppState>,
    id: &str,
    mut parts: Vec<CompletedPart>,
) -> Result<revaro_core::model::File, ApiError> {
    let _guard = state.uploads.lock(id).await;
    let mut record = state
        .db
        .call_api({
            let id = id.to_owned();
            move |c| load_upload_api(c, &id)
        })
        .await?;
    if record.status == UploadStatus::Completed {
        cleanup_committed_staging(state, &record).await;
        return state
            .db
            .call_api({
                let id = record.file_id;
                move |c| crate::file_routes::lookup_file_any(c, &id).map_err(database_error)
            })
            .await;
    }
    if record.status != UploadStatus::Pending
        || (record.commit_state == "receiving" && record.is_expired(Timestamp::now()))
    {
        return Err(ApiError::new(410, "upload expired or unavailable"));
    }
    if record.commit_state == "receiving" {
        if record.is_multipart() {
            if parts.is_empty() {
                parts = state
                    .db
                    .call_api({
                        let id = record.id.clone();
                        move |c| load_acknowledged_parts(c, &id).map_err(database_error)
                    })
                    .await?;
            }
            let count = limits::multipart_part_count(record.expected_size, record.part_size)
                .map_err(ApiError::bad_request)?;
            if parts.len() != count {
                return Err(ApiError::bad_request(
                    "multipart completion list is incomplete",
                ));
            }
            parts.sort_by_key(|part| part.part_number);
            if parts
                .iter()
                .enumerate()
                .any(|(i, p)| p.part_number != i as i32 + 1 || p.etag.trim().is_empty())
            {
                return Err(ApiError::bad_request(
                    "multipart completion list is invalid",
                ));
            }
            let directory = keys::multipart_dir(
                record.multipart_id.as_deref().ok_or_else(pending_missing)?,
                &record.object_key,
            );
            for part in &mut parts {
                let info = state
                    .store
                    .head(&format!("{directory}/{}", part.part_number))
                    .await
                    .map_err(complete_error)?;
                if Some(info.size) != record.expected_part_size(part.part_number)
                    || info.etag != part.etag.trim_matches('"')
                {
                    return Err(complete_error(StorageError::PartEtagMismatch {
                        part: part.part_number,
                    }));
                }
                part.size = Some(info.size);
                part.etag = info.etag;
            }
        } else {
            if !parts.is_empty() {
                return Err(ApiError::bad_request(
                    "single upload must not include multipart parts",
                ));
            }
            let info = state
                .store
                .head(&record.object_key)
                .await
                .map_err(complete_error)?;
            if info.size != record.expected_size {
                return Err(ApiError::bad_request(
                    "uploaded object size does not match the declared size",
                ));
            }
        }
        let upload_id = record.id.clone();
        state
            .db
            .call_api(move |connection| {
                let transaction = connection
                    .transaction()
                    .map_err(|error| database_error(DbError::Query(error)))?;
                for part in parts {
                    transaction
                        .execute(
                            "INSERT INTO upload_parts(upload_id,part_number,size,etag,completed_at) \
                             VALUES(?1,?2,?3,?4,?5) ON CONFLICT(upload_id,part_number) DO NOTHING",
                            rusqlite::params![upload_id, part.part_number, part.size, part.etag, Timestamp::now().to_rfc3339()],
                        )
                        .map_err(|error| database_error(DbError::Query(error)))?;
                }
                let changed = transaction
                    .execute(
                        "UPDATE uploads SET commit_state='assembling' \
                         WHERE id=?1 AND status='pending' AND commit_state='receiving'",
                        [upload_id],
                    )
                    .map_err(|error| database_error(DbError::Query(error)))?;
                if changed != 1 {
                    return Err(pending_missing());
                }
                transaction
                    .commit()
                    .map_err(|error| database_error(DbError::Query(error)))?;
                Ok(())
            })
            .await?;
        record.commit_state = "assembling".to_owned();
    }
    if record.commit_state == "assembling" {
        let _slot = io_slot(state).await?;
        let stored = match state.store.head(&record.object_key).await {
            Ok(info) => {
                if info.size != record.expected_size {
                    return Err(ApiError::bad_request(
                        "uploaded object size does not match the declared size",
                    ));
                }
                let hash = if record.staged_etag.as_deref() == Some(&info.etag) {
                    record.content_hash.clone().filter(|hash| hash.len() == 64)
                } else {
                    None
                };
                let content_hash = match hash {
                    Some(hash) => hash,
                    None => {
                        let hash = state
                            .store
                            .sha256_hex(&record.object_key)
                            .await
                            .map_err(complete_error)?;
                        state
                            .store
                            .sync_object(&record.object_key)
                            .await
                            .map_err(complete_error)?;
                        hash
                    }
                };
                crate::storage::HashedObject { info, content_hash }
            }
            Err(StorageError::NotFound) if record.is_multipart() => {
                let _space = reserve_space(state, record.expected_size).await?;
                let parts = state
                    .db
                    .call_api({
                        let id = record.id.clone();
                        move |c| load_acknowledged_parts(c, &id).map_err(database_error)
                    })
                    .await?;
                state
                    .store
                    .complete_multipart(
                        &record.object_key,
                        record.multipart_id.as_deref().ok_or_else(pending_missing)?,
                        &parts,
                    )
                    .await
                    .map_err(complete_error)?
            }
            Err(error) => return Err(complete_error(error)),
        };
        if stored.info.size != record.expected_size {
            return Err(ApiError::bad_request(
                "uploaded object size does not match the declared size",
            ));
        }
        let (id, etag, hash) = (
            record.id.clone(),
            stored.info.etag.clone(),
            stored.content_hash.clone(),
        );
        state
            .db
            .call_api(move |connection| {
                let changed = connection
                    .execute(
                        "UPDATE uploads SET commit_state='staged',staged_etag=?1,content_hash=?2 \
                         WHERE id=?3 AND status='pending' AND commit_state='assembling'",
                        rusqlite::params![etag, hash, id],
                    )
                    .map_err(|error| database_error(DbError::Query(error)))?;
                if changed != 1 {
                    return Err(pending_missing());
                }
                Ok(())
            })
            .await?;
        record.commit_state = "staged".to_owned();
        record.staged_etag = Some(stored.info.etag);
        record.content_hash = Some(stored.content_hash);
    }
    let info = state
        .store
        .head(&record.object_key)
        .await
        .map_err(complete_error)?;
    if info.size != record.expected_size || record.staged_etag.as_deref() != Some(&info.etag) {
        return Err(ApiError::conflict("staged upload object changed"));
    }
    let (file_id, upload_id, etag, hash, size) = (
        record.file_id.clone(),
        record.id.clone(),
        info.etag,
        record
            .content_hash
            .clone()
            .ok_or_else(|| ApiError::internal("staged upload hash missing"))?,
        record.expected_size,
    );
    let file = state
        .db
        .call_api(move |connection| {
            let transaction = connection
                .transaction()
                .map_err(|error| database_error(DbError::Query(error)))?;
            let now = Timestamp::now().to_rfc3339();
            let changed = transaction
                .execute(
                    "UPDATE files SET status='ready',etag=?1,content_hash=?2,hash_algorithm='sha256',updated_at=?3 \
                     WHERE id=?4 AND status='pending' AND size=?5 AND deleted_at IS NULL",
                    rusqlite::params![etag, hash, now, file_id, size],
                )
                .map_err(|error| database_error(DbError::Query(error)))?;
            if changed != 1 {
                return Err(ApiError::conflict("upload could not be committed"));
            }
            let changed = transaction
                .execute(
                    "UPDATE uploads SET status='completed',completed_at=?1 \
                     WHERE id=?2 AND status='pending' AND commit_state='staged'",
                    rusqlite::params![now, upload_id],
                )
                .map_err(|error| database_error(DbError::Query(error)))?;
            if changed != 1 {
                return Err(ApiError::conflict("upload could not be committed"));
            }
            let file = crate::file_routes::lookup_file_any(&transaction, &file_id)
                .map_err(database_error)?;
            transaction
                .commit()
                .map_err(|error| database_error(DbError::Query(error)))?;
            Ok(file)
        })
        .await?;
    cleanup_committed_staging(state, &record).await;
    Ok(file)
}

async fn cleanup_committed_staging(state: &Arc<AppState>, record: &UploadRecord) {
    if let Some(id) = &record.multipart_id
        && let Err(error) = state.store.abort_multipart(&record.object_key, id).await
    {
        tracing::warn!(%error,upload=%record.id,"committed upload staging cleanup failed");
        return;
    }
    let id = record.id.clone();
    if let Err(error) = state
        .db
        .call(move |c| {
            c.execute(
                "UPDATE uploads SET staging_cleaned=1 WHERE id=?1 AND status='completed'",
                [id],
            )
            .map_err(DbError::Query)
        })
        .await
    {
        tracing::warn!(%error,"upload cleanup acknowledgement failed");
    }
}

pub(crate) async fn recover_upload_commits(state: &Arc<AppState>) -> Result<(), String> {
    let ids = state
        .db
        .call(|connection| {
            let mut query = connection
                .prepare(
                    "SELECT id FROM uploads WHERE (status='pending' AND commit_state<>'receiving') \
                     OR (status='completed' AND staging_cleaned=0) \
                     ORDER BY COALESCE(commit_attempted_at,created_at),id LIMIT 32",
                )
                .map_err(DbError::Query)?;
            query
                .query_map([], |row| row.get::<_, String>(0))
                .map_err(DbError::Query)?
                .collect::<Result<Vec<_>, _>>()
                .map_err(DbError::Query)
        })
        .await
        .map_err(|error| error.to_string())?;
    for id in ids {
        // Record before attempting, including attempts interrupted by timeout.
        // A broken early session must not monopolize every bounded batch.
        state
            .db
            .call({
                let id = id.clone();
                move |connection| {
                    connection
                        .execute(
                            "UPDATE uploads SET commit_attempted_at=?1 WHERE id=?2",
                            rusqlite::params![Timestamp::now().to_rfc3339(), id],
                        )
                        .map_err(DbError::Query)
                }
            })
            .await
            .map_err(|error| error.to_string())?;
        if let Err(error) = finalize_upload(state, &id, Vec::new()).await {
            tracing::warn!(upload=%id,%error,"upload commit recovery will retry");
        }
    }
    Ok(())
}

/// `DELETE /api/uploads/{id}`
async fn abort_upload(
    State(state): State<Arc<AppState>>,
    _user: AuthUser,
    PathParam(id): PathParam<String>,
) -> Result<StatusCode, ApiError> {
    let upload_id = id.clone();
    let record = state
        .db
        .call_api(move |connection| load_upload_api(connection, &upload_id))
        .await?;
    // A completion response can be lost after the metadata transaction wins.
    // Retrying the browser's cleanup request must not turn that successful
    // upload into a user-visible error or remove its committed object.
    if record.status == UploadStatus::Completed {
        return Ok(StatusCode::NO_CONTENT);
    }
    abort_pending_upload(&state, &id, false).await?;
    Ok(StatusCode::NO_CONTENT)
}

/// Mark a pending upload aborted, remove its invisible file row and clean its
/// staged bytes. The status transition is committed before storage cleanup so
/// a failed cleanup can be retried without reopening the upload session.
async fn abort_pending_upload(
    state: &Arc<AppState>,
    id: &str,
    expired_only: bool,
) -> Result<(), ApiError> {
    let _upload_guard = state.uploads.lock(id).await;
    let upload_id = id.to_owned();
    let record = state
        .db
        .call_api(move |connection| load_upload_api(connection, &upload_id))
        .await?;
    if !expired_only && record.status == UploadStatus::Completed {
        return Ok(());
    }
    if record.status != UploadStatus::Pending
        || (expired_only
            && (record.commit_state != "receiving" || !record.is_expired(Timestamp::now())))
    {
        return Err(pending_missing());
    }

    let file_id = record.file_id.clone();
    let upload_id = record.id.clone();
    state
        .db
        .call_api(move |connection| {
            let transaction = connection
                .transaction()
                .map_err(|error| database_error(DbError::Query(error)))?;
            let changed = transaction
                .execute(
                    "UPDATE uploads SET status = 'aborted' WHERE id = ?1 AND file_id = ?2 AND status = 'pending' \
                     AND EXISTS (SELECT 1 FROM files WHERE id = ?2 AND status = 'pending')",
                    rusqlite::params![upload_id, file_id],
                )
                .map_err(|error| database_error(DbError::Query(error)))?;
            if changed != 1 {
                return Err(pending_missing());
            }
            transaction
                .execute(
                    "DELETE FROM files WHERE id = ?1 AND status = 'pending'",
                    [&file_id],
                )
                .map_err(|error| database_error(DbError::Query(error)))?;
            transaction
                .commit()
                .map_err(|error| database_error(DbError::Query(error)))?;
            Ok(())
        })
        .await?;

    if let Some(multipart_id) = &record.multipart_id
        && let Err(error) = state
            .store
            .abort_multipart(&record.object_key, multipart_id)
            .await
    {
        tracing::warn!(%error, upload = %record.id, "could not remove multipart staging");
    }
    if let Err(error) = state.store.delete(&record.object_key).await {
        tracing::warn!(%error, upload = %record.id, "could not remove a partial object");
    }
    Ok(())
}

fn etag_response(etag: &str, hash: &str) -> http::Response<Body> {
    let mut response = http::Response::new(Body::empty());
    *response.status_mut() = StatusCode::NO_CONTENT;
    if let Ok(value) = etag.parse() {
        response.headers_mut().insert(http::header::ETAG, value);
    }
    response.headers_mut().insert(
        http::header::HeaderName::from_static("x-content-sha256"),
        hash.parse().expect("SHA-256 header"),
    );
    response
}

fn write_error(error: StorageError) -> ApiError {
    if matches!(error, StorageError::ContentMismatch) {
        return ApiError::conflict("upload retry contains different bytes");
    }
    if matches!(&error,StorageError::Io(e) if e.kind()==std::io::ErrorKind::TimedOut) {
        return ApiError::new(408, "upload timed out; reselect the file to continue");
    }
    if matches!(&error, StorageError::Io(e) if e.kind() == std::io::ErrorKind::StorageFull) {
        return ApiError::new(507, "insufficient disk space for upload");
    }
    tracing::error!(%error, "upload write failed");
    if matches!(error, StorageError::Io(_)) {
        return ApiError::internal("upload storage write failed");
    }
    ApiError::bad_request("file write failed or size mismatch")
}

fn complete_error(error: StorageError) -> ApiError {
    tracing::error!(%error, "upload completion failed");
    // The Go route only validates the shape of the completion list itself.
    // Missing staged parts, ETag mismatches, and other failures reported by
    // object storage all use its generic 502 response; keep the 400 messages
    // above for the route-level count/order/empty-ETag checks.
    ApiError::new(502, "object storage could not complete the upload")
}

fn conflict_or(error: DbError) -> ApiError {
    if error.is_constraint_violation() {
        ApiError::conflict("an item with that name already exists")
    } else {
        database_error(error)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lookup_and_pending_upload_errors_keep_distinct_messages() {
        assert_eq!(upload_missing().message, "upload not found");
        assert_eq!(pending_missing().message, "pending upload not found");
    }
    #[tokio::test]
    async fn idle_upload_times_out_and_removes_partial_bytes() {
        use tokio::io::AsyncReadExt;
        let body = Body::from_stream(futures_util::stream::pending::<
            Result<bytes::Bytes, std::io::Error>,
        >());
        let mut reader = body_reader(
            body,
            std::time::Duration::from_millis(10),
            std::time::Duration::from_secs(1),
        );
        let mut bytes = Vec::new();
        let error = reader.read_to_end(&mut bytes).await.unwrap_err();
        assert_eq!(error.kind(), std::io::ErrorKind::TimedOut);
        let root = std::env::temp_dir().join(format!("revaro-idle-test-{}", crate::ids::new_id()));
        let store = crate::storage::LocalStore::open(&root).await.unwrap();
        let body = Body::from_stream(futures_util::stream::pending::<
            Result<bytes::Bytes, std::io::Error>,
        >());
        let mut reader = body_reader(
            body,
            std::time::Duration::from_millis(10),
            std::time::Duration::from_secs(1),
        );
        let error = store
            .write_stream("blobs/timeout", &mut reader, 1)
            .await
            .unwrap_err();
        assert_eq!(write_error(error).status, 408);
        assert!(store.list_prefix("blobs").await.unwrap().is_empty());
        assert_eq!(std::fs::read_dir(root.join("blobs")).unwrap().count(), 0);
        let _ = std::fs::remove_dir_all(root);
    }
}

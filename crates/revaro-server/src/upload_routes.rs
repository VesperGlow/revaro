//! Uploads: creating a session, streaming bytes, and committing the file.
//!
//! Two transfer modes, chosen by size exactly as the Go server chose them:
//!
//! * **single** — one `PUT` carries the whole body (files below 16 MiB).
//! * **multipart** — the client fetches single-use URLs in batches, `PUT`s each
//!   part, acknowledges its entity tag, then completes with the full part list.
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
//! Completion hashes the committed object with a bounded streaming SHA-256, so
//! single and multipart uploads expose the same integrity metadata without
//! buffering a multi-gigabyte object in memory. Mutable operations for one
//! upload are serialized by [`crate::state::UploadRuntime`].

use std::sync::Arc;

use axum::body::Body;
use axum::extract::{Path as PathParam, State};
use axum::routing::{get, post, put};
use axum::{Json, Router};
use futures_util::TryStreamExt as _;
use http::StatusCode;
use revaro_core::ApiError;
use revaro_core::api::uploads::{
    CompleteUploadRequest, CreateUpload, CreateUploadRequest, PartUrl, RecordUploadPartRequest,
    UploadPartsRequest, UploadPartsResponse, UploadStatus as UploadStatusResponse,
};
use revaro_core::keys;
use revaro_core::limits;
use revaro_core::model::{UploadMode, UploadPart, UploadStatus};
use revaro_core::time::Timestamp;
use revaro_core::validate;
use rusqlite::Connection;
use tokio_util::io::StreamReader;

use crate::auth::extract::AuthUser;
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
            "SELECT id,file_id,mode,object_key,multipart_id,part_size,expected_size,mime_type,status,expires_at \
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
                })
            },
        )
        .map_err(DbError::Query)
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
    if record.status != UploadStatus::Pending || record.is_expired(Timestamp::now()) {
        return Err(pending_missing());
    }
    Ok(record)
}

fn pending_missing() -> ApiError {
    ApiError::not_found("pending upload not found")
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
    Json(request): Json<CreateUploadRequest>,
) -> Result<(StatusCode, Json<CreateUpload>), ApiError> {
    validate::validate_name(&request.name)?;
    validate::validate_file_size(request.size)?;
    let mime_type = if request.mime_type.is_empty() {
        "application/octet-stream".to_owned()
    } else {
        request.mime_type
    };
    validate::validate_mime_type(&mime_type)?;

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

    let url = if multipart {
        String::new()
    } else {
        format!("/api/uploads/{upload_id}/data")
    };
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
expected_size,mime_type,status,created_at,expires_at) \
VALUES(?1,?2,?3,?4,?5,?6,?7,?8,'pending',?9,?10)",
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

    // Uploads are visible in the same background-task centre as archive jobs.
    // Keep this separate from the metadata transaction: the reference server
    // treats a task-row failure as non-fatal to an otherwise valid upload.
    if let Err(error) = create_upload_task(&state, &upload_id, &file_id).await {
        tracing::error!(%error, upload = %upload_id, "could not create upload task");
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
                    pending_missing()
                } else {
                    database_error(error)
                }
            })?;
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
                        size: row.get(1)?,
                        etag: row.get(2)?,
                        content_hash: row.get(3)?,
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
                expires_at: record.expires_at,
                parts,
            })
        })
        .await
        .map(Json)
}

/// Adapt the request body into an [`tokio::io::AsyncRead`] so a large upload is
/// streamed to disk rather than buffered in memory.
fn body_reader(body: Body) -> impl tokio::io::AsyncRead + Unpin {
    let stream = body.into_data_stream().map_err(std::io::Error::other);
    StreamReader::new(stream)
}

/// `PUT /api/uploads/{id}/data` — a whole single-request upload.
async fn upload_content(
    State(state): State<Arc<AppState>>,
    _user: AuthUser,
    PathParam(id): PathParam<String>,
    request: axum::extract::Request,
) -> Result<http::Response<Body>, ApiError> {
    let _upload_guard = state.uploads.lock(&id).await;
    let record = state
        .db
        .call_api(move |connection| require_pending(connection, &id))
        .await?;
    if record.is_multipart() {
        return Err(ApiError::bad_request(
            "multipart uploads must send numbered parts",
        ));
    }

    let mut reader = body_reader(request.into_body());
    let stored = state
        .store
        .write_stream(&record.object_key, &mut reader, record.expected_size)
        .await
        .map_err(write_error)?;

    Ok(etag_response(&stored.etag))
}

/// `PUT /api/uploads/{id}/data/{part}` — one numbered part.
async fn upload_content_part(
    State(state): State<Arc<AppState>>,
    _user: AuthUser,
    PathParam((id, part)): PathParam<(String, i32)>,
    request: axum::extract::Request,
) -> Result<http::Response<Body>, ApiError> {
    let _upload_guard = state.uploads.lock(&id).await;
    let record = state
        .db
        .call_api(move |connection| require_pending(connection, &id))
        .await?;
    if !record.is_multipart() {
        return Err(ApiError::bad_request(
            "single upload must not include multipart parts",
        ));
    }
    let Some(expected) = record.expected_part_size(part) else {
        return Err(ApiError::bad_request("invalid multipart part number"));
    };
    let Some(multipart_id) = record.multipart_id.clone() else {
        return Err(pending_missing());
    };

    let mut reader = body_reader(request.into_body());
    let stored = state
        .store
        .upload_part(
            &record.object_key,
            &multipart_id,
            part,
            &mut reader,
            expected,
        )
        .await
        .map_err(write_error)?;
    Ok(etag_response(&stored.etag))
}

/// `POST /api/uploads/{id}/parts` — hand out single-use URLs for a batch.
async fn upload_parts(
    State(state): State<Arc<AppState>>,
    _user: AuthUser,
    PathParam(id): PathParam<String>,
    Json(request): Json<UploadPartsRequest>,
) -> Result<Json<UploadPartsResponse>, ApiError> {
    state
        .db
        .call_api(move |connection| {
            let record = require_pending(connection, &id)?;
            if !record.is_multipart() {
                return Err(pending_missing());
            }
            let part_count =
                limits::multipart_part_count(record.expected_size, record.part_size).unwrap_or(0);
            let numbers = validate::validate_upload_part_batch(&request.part_numbers, part_count)?;
            let parts = numbers
                .into_iter()
                .map(|part_number| PartUrl {
                    part_number,
                    url: format!("/api/uploads/{}/data/{part_number}", record.id),
                })
                .collect();
            Ok(UploadPartsResponse { parts })
        })
        .await
        .map(Json)
}

/// `PUT /api/uploads/{id}/parts/{part}` — acknowledge a stored part.
async fn record_upload_part(
    State(state): State<Arc<AppState>>,
    _user: AuthUser,
    PathParam((id, part)): PathParam<(String, i32)>,
    Json(request): Json<RecordUploadPartRequest>,
) -> Result<StatusCode, ApiError> {
    let _upload_guard = state.uploads.lock(&id).await;
    state
        .db
        .call_api(move |connection| {
            let record = require_pending(connection, &id)?;
            if !record.is_multipart() {
                return Err(pending_missing());
            }
            let Some(expected) = record.expected_part_size(part) else {
                return Err(ApiError::bad_request("invalid multipart part number"));
            };
            if request.size != expected {
                return Err(ApiError::bad_request("invalid uploaded part acknowledgement"));
            }
            connection
                .execute(
                    "INSERT INTO upload_parts(upload_id,part_number,size,etag,content_hash,completed_at) \
VALUES(?1,?2,?3,?4,?5,?6) ON CONFLICT(upload_id,part_number) DO UPDATE SET \
size=excluded.size,etag=excluded.etag,content_hash=excluded.content_hash,completed_at=excluded.completed_at",
                    rusqlite::params![
                        record.id,
                        part,
                        request.size,
                        request.etag,
                        request.content_hash,
                        Timestamp::now().to_rfc3339(),
                    ],
                )
                .map_err(|error| database_error(DbError::Query(error)))?;
            Ok(StatusCode::NO_CONTENT)
        })
        .await
}

/// `POST /api/uploads/{id}/complete`
async fn complete_upload(
    State(state): State<Arc<AppState>>,
    _user: AuthUser,
    PathParam(id): PathParam<String>,
    Json(request): Json<CompleteUploadRequest>,
) -> Result<Json<revaro_core::model::File>, ApiError> {
    let _upload_guard = state.uploads.lock(&id).await;
    let record = state
        .db
        .call_api(move |connection| load_upload_api(connection, &id))
        .await?;

    // Completing twice returns the committed file rather than minting a second
    // one: a client that retried after a lost response must not have to guess.
    if record.status == UploadStatus::Completed {
        return state
            .db
            .call_api({
                let file_id = record.file_id.clone();
                move |connection| {
                    crate::file_routes::lookup_file_for_commit(connection, &file_id)
                        .map_err(database_error)
                }
            })
            .await
            .map(Json);
    }
    if record.status != UploadStatus::Pending || record.is_expired(Timestamp::now()) {
        return Err(pending_missing());
    }

    let content_hash = if record.is_multipart() {
        let Some(multipart_id) = record.multipart_id.clone() else {
            return Err(pending_missing());
        };
        let parts: Vec<revaro_core::storage::CompletedPart> = request
            .parts
            .iter()
            .map(|part| revaro_core::storage::CompletedPart {
                part_number: part.part_number,
                etag: part.etag.clone(),
                ..Default::default()
            })
            .collect();
        let expected_parts = limits::multipart_part_count(record.expected_size, record.part_size)
            .map_err(ApiError::bad_request)?;
        if parts.len() != expected_parts {
            return Err(ApiError::bad_request(
                "multipart completion list is incomplete",
            ));
        }
        state
            .store
            .complete_multipart(&record.object_key, &multipart_id, &parts)
            .await
            .map_err(complete_error)?;
        update_upload_task(&state, &record.id, "running", "verifying", 99.0, "").await;
        match state.store.sha256_hex(&record.object_key).await {
            Ok(hash) => hash,
            Err(error) => {
                let api_error = complete_error(error);
                update_upload_task(
                    &state,
                    &record.id,
                    "failed",
                    "verifying",
                    99.0,
                    &api_error.message,
                )
                .await;
                return Err(api_error);
            }
        }
    } else {
        update_upload_task(&state, &record.id, "running", "verifying", 99.0, "").await;
        match state.store.sha256_hex(&record.object_key).await {
            Ok(hash) => hash,
            Err(error) => {
                let api_error = complete_error(error);
                update_upload_task(
                    &state,
                    &record.id,
                    "failed",
                    "verifying",
                    99.0,
                    &api_error.message,
                )
                .await;
                return Err(api_error);
            }
        }
    };

    let file_id = record.file_id.clone();
    let upload_id = record.id.clone();
    let expected_size = record.expected_size;
    let result = state
        .db
        .call_api(move |connection| {
            let now = Timestamp::now().to_rfc3339();
            let transaction = connection
                .transaction()
                .map_err(|error| database_error(DbError::Query(error)))?;
            let committed = transaction
                .execute(
                    "UPDATE files SET status = 'ready', content_hash = ?1, hash_algorithm = ?2, \
updated_at = ?3 WHERE id = ?4 AND status = 'pending' AND size = ?5 AND deleted_at IS NULL",
                    rusqlite::params![
                        content_hash,
                        if content_hash.is_empty() {
                            ""
                        } else {
                            "sha256"
                        },
                        now,
                        file_id,
                        expected_size,
                    ],
                )
                .map_err(|error| database_error(DbError::Query(error)))?;
            if committed != 1 {
                return Err(ApiError::conflict("upload could not be committed"));
            }
            transaction
                .execute(
                    "UPDATE uploads SET status = 'completed', content_hash = ?1, completed_at = ?2 \
WHERE id = ?3",
                    rusqlite::params![content_hash, now, upload_id],
                )
                .map_err(|error| database_error(DbError::Query(error)))?;
            // Read through the transaction, not `connection`: the transaction
            // holds the mutable borrow, and reading inside it also makes the
            // returned row the one this commit produced.
            let file = crate::file_routes::lookup_file_for_commit(&transaction, &file_id)
                .map_err(database_error)?;
            transaction
                .commit()
                .map_err(|error| database_error(DbError::Query(error)))?;
            Ok(file)
        })
        .await;
    match result {
        Ok(file) => {
            update_upload_task(&state, &record.id, "completed", "completed", 100.0, "").await;
            Ok(Json(file))
        }
        Err(error) => {
            update_upload_task(
                &state,
                &record.id,
                "retrying",
                "committing",
                99.0,
                "文件已上传，正在等待元数据重试",
            )
            .await;
            Err(error)
        }
    }
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

/// Abort a pending upload from the task centre runtime.
pub(crate) async fn cancel_upload_task(state: &Arc<AppState>, upload_id: &str) {
    if let Err(error) = abort_pending_upload(state, upload_id, false).await
        && error.status != 404
    {
        tracing::warn!(%error, upload = %upload_id, "could not cancel upload task");
    }
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
    if record.status != UploadStatus::Pending
        || (expired_only && !record.is_expired(Timestamp::now()))
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

    let phase = if expired_only { "expired" } else { "cancelled" };
    let message = if expired_only {
        "upload session expired"
    } else {
        ""
    };
    update_upload_task(state, &record.id, "cancelled", phase, 0.0, message).await;

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

/// Create the durable task row used by the reference client's TaskCenter.
///
/// The reference server does not emit a jobs event at creation time. The row
/// becomes visible to the client when the upload reaches a lifecycle update
/// (normally completion), so an in-flight byte transfer stays out of the task
/// centre just as it did before the Rust migration.
async fn create_upload_task(
    state: &Arc<AppState>,
    upload_id: &str,
    file_id: &str,
) -> Result<(), ApiError> {
    let task_id = crate::ids::new_id();
    let upload_id = upload_id.to_owned();
    let file_id = file_id.to_owned();
    state
        .db
        .call_api(move |connection| {
            let now = Timestamp::now().to_rfc3339();
            let transaction = connection
                .transaction()
                .map_err(|error| database_error(DbError::Query(error)))?;
            transaction
                .execute(
                    "INSERT INTO tasks(id,type,status,phase,source_type,source_id,created_at,updated_at) \
                     VALUES(?1,'upload','queued','uploading','upload',?2,?3,?3)",
                    rusqlite::params![task_id, upload_id, now],
                )
                .map_err(|error| database_error(DbError::Query(error)))?;
            transaction
                .execute(
                    "INSERT INTO task_files(task_id,file_id,role) VALUES(?1,?2,'input')",
                    rusqlite::params![task_id, file_id],
                )
                .map_err(|error| database_error(DbError::Query(error)))?;
            transaction
                .commit()
                .map_err(|error| database_error(DbError::Query(error)))?;
            Ok(())
        })
        .await?;
    Ok(())
}

/// Persist the same upload lifecycle fields used by the old task manager.
async fn update_upload_task(
    state: &Arc<AppState>,
    upload_id: &str,
    status: &str,
    phase: &str,
    progress: f64,
    task_error: &str,
) {
    let upload_id = upload_id.to_owned();
    let status = status.to_owned();
    let phase = phase.to_owned();
    let task_error = task_error.to_owned();
    let upload_for_log = upload_id.clone();
    let now = Timestamp::now().to_rfc3339();
    let result = state
        .db
        .call_api(move |connection| {
            connection
                .execute(
                    "UPDATE tasks SET status=?1,phase=?2,progress=?3,error=?4, \
                     started_at=CASE WHEN ?5='running' THEN COALESCE(started_at,?6) ELSE started_at END, \
                     finished_at=CASE WHEN ?7 IN ('completed','failed','cancelled') THEN COALESCE(finished_at,?8) ELSE NULL END, \
                     heartbeat_at=CASE WHEN ?9='running' THEN ?10 ELSE heartbeat_at END, \
                     updated_at=?11 \
                     WHERE source_type='upload' AND source_id=?12",
                    rusqlite::params![
                        status,
                        phase,
                        progress,
                        task_error,
                        status,
                        now,
                        status,
                        now,
                        status,
                        now,
                        now,
                        upload_id,
                    ],
                )
                .map_err(|error| database_error(DbError::Query(error)))
        })
        .await;
    if let Err(error) = result {
        tracing::warn!(%error, upload = %upload_for_log, "could not update upload task");
    }
    state.jobs.changed();
}

fn etag_response(etag: &str) -> http::Response<Body> {
    let mut response = http::Response::new(Body::empty());
    *response.status_mut() = StatusCode::NO_CONTENT;
    if let Ok(value) = etag.parse() {
        response.headers_mut().insert(http::header::ETAG, value);
    }
    response
}

fn write_error(error: StorageError) -> ApiError {
    match error {
        StorageError::SizeMismatch { .. } => {
            ApiError::bad_request("uploaded object size does not match the declared size")
        }
        StorageError::NotFound => pending_missing(),
        other => {
            tracing::error!(%other, "upload write failed");
            ApiError::new(502, "object storage write failed")
        }
    }
}

fn complete_error(error: StorageError) -> ApiError {
    match error {
        StorageError::InvalidPartList | StorageError::PartMissing { .. } => {
            ApiError::bad_request("multipart completion list is incomplete")
        }
        StorageError::PartEtagMismatch { .. } => {
            ApiError::bad_request("multipart completion list is invalid")
        }
        StorageError::SizeMismatch { .. } => {
            ApiError::bad_request("uploaded object size does not match the declared size")
        }
        other => {
            tracing::error!(%other, "upload completion failed");
            ApiError::new(502, "object storage write failed")
        }
    }
}

fn conflict_or(error: DbError) -> ApiError {
    if error.is_constraint_violation() {
        ApiError::conflict("an item with that name already exists")
    } else {
        database_error(error)
    }
}

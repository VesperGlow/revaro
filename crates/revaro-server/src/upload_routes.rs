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
//! ## Known gap
//!
//! Go hashed the committed object and stored `content_hash`. For single mode
//! (≤16 MiB) this port does the same by reading the object back after upload.
//! For multipart it leaves `content_hash` empty: hashing a multi-gigabyte object
//! requires an incremental hash computed while parts stream in, which the
//! `upload_parts.content_hash` column exists to support but which is not wired
//! up yet. Integrity is still enforced by per-part entity tags and the declared
//! total size.

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
            })
            .collect();
        state
            .store
            .complete_multipart(&record.object_key, &multipart_id, &parts)
            .await
            .map_err(complete_error)?;
        // See the module docs: hashing a multi-gigabyte object incrementally is
        // not wired up yet, so multipart commits record no content hash.
        String::new()
    } else {
        let bytes = state
            .store
            .read(&record.object_key, record.expected_size.max(1) as usize)
            .await
            .map_err(complete_error)?;
        keys::sha256_hex(&bytes)
    };

    let _object_key = record.object_key.clone();
    let file_id = record.file_id.clone();
    let upload_id = record.id.clone();
    let expected_size = record.expected_size;
    state
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
        .await
        .map(Json)
}

/// `DELETE /api/uploads/{id}`
async fn abort_upload(
    State(state): State<Arc<AppState>>,
    _user: AuthUser,
    PathParam(id): PathParam<String>,
) -> Result<StatusCode, ApiError> {
    let record = state
        .db
        .call_api(move |connection| load_upload_api(connection, &id))
        .await?;

    // A lost completion response can make the browser issue DELETE after the
    // transaction has already made the file visible. Keep the abort endpoint
    // idempotent without allowing that race to delete a committed object.
    if record.status == UploadStatus::Completed {
        return Ok(StatusCode::NO_CONTENT);
    }

    let file_id = record.file_id.clone();
    let upload_id = record.id.clone();
    let removed = state
        .db
        .call_api(move |connection| {
            let transaction = connection
                .transaction()
                .map_err(|error| database_error(DbError::Query(error)))?;
            let deleted_upload = transaction
                .execute(
                    "DELETE FROM uploads WHERE id = ?1 AND file_id = ?2 AND status <> 'completed' \
AND EXISTS (SELECT 1 FROM files WHERE id = ?2 AND status = 'pending')",
                    rusqlite::params![upload_id, file_id],
                )
                .map_err(|error| database_error(DbError::Query(error)))?;
            if deleted_upload != 1 {
                transaction
                    .rollback()
                    .map_err(|error| database_error(DbError::Query(error)))?;
                return Ok(false);
            }
            // The pending file row is invisible to every listing, so removing it
            // is what actually cancels the upload.
            let deleted_file = transaction
                .execute(
                    "DELETE FROM files WHERE id = ?1 AND status = 'pending'",
                    [&file_id],
                )
                .map_err(|error| database_error(DbError::Query(error)))?;
            if deleted_file != 1 {
                return Err(ApiError::conflict("upload state changed"));
            }
            transaction
                .commit()
                .map_err(|error| database_error(DbError::Query(error)))?;
            Ok(true)
        })
        .await?;
    if !removed {
        return Ok(StatusCode::NO_CONTENT);
    }

    // Claiming the pending rows before touching storage closes the race with
    // completion: a completion that is already committed fails the conditional
    // delete, while one that is still assembling can no longer publish a ready
    // row after this cleanup wins.
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

    Ok(StatusCode::NO_CONTENT)
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

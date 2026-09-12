//! File browsing, the storage summary and the library aggregation.
//!
//! This module owns the read side of `internal/server/server_files.go` plus all
//! of `library.go`. The mutating endpoints (create, rename, copy, delete) and
//! the trash are the next slice.
//!
//! ## One scanner, one column list
//!
//! Every query that returns a `files` row selects the same column list in the
//! same order and decodes it with [`scan_file`]. The Go server did this with a
//! shared `fileColumns` constant plus `scanFile`, and the discipline is worth
//! keeping: `files` has fifteen columns and a hand-written scan per query would
//! eventually disagree with the `SELECT` and mis-decode a row rather than fail.
//!
//! ## Aggregation lives in the shared crate
//!
//! The bucket counts and the folder-path resolution are pure functions and live
//! in `revaro_core::library`, because the browser needs the same rules to render
//! counts from a cached listing. This module only fetches rows and joins them.

use std::collections::HashMap;
use std::sync::Arc;

use axum::extract::{Path as PathParam, Query, State};
use axum::routing::get;
use axum::{Json, Router};
use revaro_core::ApiError;
use revaro_core::api::{Children, FileDetail, Library, LibraryAll, LibraryBuckets};
use revaro_core::classify::LibraryKind;
use revaro_core::keys;
use revaro_core::library as aggregation;
use revaro_core::model::{
    File, FileKind, FileStatus, FolderRef, LibraryCounts, LibraryItem, StorageStats,
};
use revaro_core::time::Timestamp;
use rusqlite::{Connection, Row};

use crate::auth::extract::AuthUser;
use crate::db::DbError;
use crate::state::AppState;

/// The canonical `files` column list.
///
/// `object_key` is coalesced because the column is nullable for directories but
/// the model stores it as a plain `String`; the rest are read as nullable and
/// defaulted, matching the Go scanner.
const FILE_COLUMNS: &str = "id,parent_id,name,kind,COALESCE(object_key,''),size,mime_type,etag,\
content_hash,hash_algorithm,status,created_at,updated_at,deleted_at,restore_parent_id";

/// The same list qualified with a table alias, for the recursive breadcrumb
/// query where `files` and the CTE both contribute columns.
const FILE_COLUMNS_QUALIFIED: &str = "f.id,f.parent_id,f.name,f.kind,COALESCE(f.object_key,''),\
f.size,f.mime_type,f.etag,f.content_hash,f.hash_algorithm,f.status,f.created_at,f.updated_at,\
f.deleted_at,f.restore_parent_id";

/// Route table for the read-only file surface.
pub fn routes() -> Router<Arc<AppState>> {
    Router::new()
        .route("/files/{id}", get(get_file))
        .route("/files/{id}/children", get(children))
        .route("/storage/stats", get(storage_stats))
        .route("/system/status", get(system_status))
        .route("/files/{id}/audio", get(audio_media_info))
        .route("/tasks", get(list_tasks))
        .route("/tasks/{id}", get(get_task))
        .route("/library", get(library))
        .route("/library/all", get(library_all))
        .route("/library/counts", get(library_counts))
        .route("/directories", axum::routing::post(create_directory))
        .route(
            "/files/{id}",
            axum::routing::patch(patch_file).delete(delete_file),
        )
        .route("/files/{id}/copy", axum::routing::post(copy_file))
        .route("/documents", axum::routing::post(create_document))
        .route(
            "/files/{id}/media/progress",
            get(media_progress).put(save_media_progress),
        )
        .route(
            "/files/{id}/share",
            get(get_share).post(create_share).delete(revoke_share),
        )
        .route("/trash", get(trash).delete(empty_trash))
        .route("/trash/{id}/restore", axum::routing::post(restore_trash))
        .route("/trash/{id}", axum::routing::delete(purge_trash))
        .route(
            "/files/{id}/content",
            get(get_document).put(update_document),
        )
}

/// Decode one `files` row selected with [`FILE_COLUMNS`].
///
/// A column that holds an unknown enum spelling or an unparseable timestamp is
/// surfaced as a decode error rather than silently defaulted: that would mean
/// the database was written by something other than this product, and quietly
/// reporting `ready` for a corrupt row is worse than failing the request.
pub fn scan_file(row: &Row<'_>) -> rusqlite::Result<File> {
    Ok(File {
        id: row.get(0)?,
        parent_id: row.get(1)?,
        name: row.get(2)?,
        kind: enum_column(row, 3, "kind")?,
        object_key: row.get(4)?,
        size: row.get(5)?,
        mime_type: row.get::<_, Option<String>>(6)?.unwrap_or_default(),
        etag: row.get::<_, Option<String>>(7)?.unwrap_or_default(),
        content_hash: row.get::<_, Option<String>>(8)?.unwrap_or_default(),
        hash_algorithm: row.get::<_, Option<String>>(9)?.unwrap_or_default(),
        status: enum_column(row, 10, "status")?,
        created_at: timestamp_column(row, 11)?,
        updated_at: timestamp_column(row, 12)?,
        deleted_at: optional_timestamp_column(row, 13)?,
        restore_parent_id: row.get(14)?,
        has_cover: false,
    })
}

fn enum_column<T: std::str::FromStr>(
    row: &Row<'_>,
    index: usize,
    column: &str,
) -> rusqlite::Result<T> {
    let raw: String = row.get(index)?;
    raw.parse().map_err(|_| {
        rusqlite::Error::InvalidColumnType(
            index,
            format!("{column}={raw:?}"),
            rusqlite::types::Type::Text,
        )
    })
}

fn timestamp_column(row: &Row<'_>, index: usize) -> rusqlite::Result<Timestamp> {
    let raw: String = row.get(index)?;
    Timestamp::parse(&raw).map_err(|_| {
        rusqlite::Error::InvalidColumnType(index, raw.clone(), rusqlite::types::Type::Text)
    })
}

fn optional_timestamp_column(row: &Row<'_>, index: usize) -> rusqlite::Result<Option<Timestamp>> {
    match row.get::<_, Option<String>>(index)? {
        None => Ok(None),
        Some(raw) if raw.is_empty() => Ok(None),
        Some(raw) => Timestamp::parse(&raw).map(Some).map_err(|_| {
            rusqlite::Error::InvalidColumnType(index, raw.clone(), rusqlite::types::Type::Text)
        }),
    }
}

/// A live (non-deleted) file by id.
fn lookup_file(connection: &rusqlite::Connection, id: &str) -> Result<File, DbError> {
    connection
        .query_row(
            &format!("SELECT {FILE_COLUMNS} FROM files WHERE id = ?1 AND deleted_at IS NULL"),
            [id],
            scan_file,
        )
        .map_err(DbError::Query)
}

/// Look up a file row for the upload commit path.
///
/// `pub(crate)` because the upload module needs the same decode but lives in its
/// own module; duplicating [`FILE_COLUMNS`] there would let the two drift.
pub(crate) fn lookup_file_for_commit(
    connection: &rusqlite::Connection,
    id: &str,
) -> Result<File, DbError> {
    connection
        .query_row(
            &format!("SELECT {FILE_COLUMNS} FROM files WHERE id = ?1"),
            [id],
            scan_file,
        )
        .map_err(DbError::Query)
}

/// A file by id, including soft-deleted rows.
///
/// Content delivery deliberately resolves trashed rows: an item stays readable
/// until it is restored or purged, which is what Go's `readableFile` did.
fn lookup_file_any(connection: &rusqlite::Connection, id: &str) -> Result<File, DbError> {
    connection
        .query_row(
            &format!("SELECT {FILE_COLUMNS} FROM files WHERE id = ?1"),
            [id],
            scan_file,
        )
        .map_err(DbError::Query)
}

/// `GET /api/files/{id}`
async fn get_file(
    State(state): State<Arc<AppState>>,
    _user: AuthUser,
    PathParam(id): PathParam<String>,
) -> Result<Json<FileDetail>, ApiError> {
    let (file, breadcrumbs) = state
        .db
        .call_api(move |connection| {
            let file = lookup_file(connection, &id)
                .map_err(|error| not_found_or(error, "file not found"))?;
            let breadcrumbs = breadcrumbs(connection, &file.id).map_err(database_error)?;
            Ok((file, breadcrumbs))
        })
        .await?;
    Ok(Json(FileDetail { file, breadcrumbs }))
}

/// Ancestors of `id`, from the root down to its parent.
///
/// The recursive CTE is the authoritative implementation, ported unchanged: it
/// is one round trip and it cannot loop, because it only ever walks to a parent
/// that is not deleted.
fn breadcrumbs(connection: &rusqlite::Connection, id: &str) -> Result<Vec<File>, DbError> {
    let sql = format!(
        "WITH RECURSIVE p(id,parent_id,name,kind,object_key,size,mime_type,etag,content_hash,\
hash_algorithm,status,created_at,updated_at,deleted_at,restore_parent_id,depth) AS (\
SELECT {FILE_COLUMNS},0 FROM files WHERE id = ?1 AND deleted_at IS NULL \
UNION ALL \
SELECT {FILE_COLUMNS_QUALIFIED},p.depth+1 FROM files f JOIN p ON f.id = p.parent_id \
WHERE f.deleted_at IS NULL) \
SELECT {FILE_COLUMNS} FROM p ORDER BY depth DESC"
    );
    let mut statement = connection.prepare(&sql).map_err(DbError::Query)?;
    let rows = statement
        .query_map([id], scan_file)
        .map_err(DbError::Query)?;
    let mut out = Vec::new();
    for row in rows {
        out.push(row.map_err(DbError::Query)?);
    }
    Ok(out)
}

/// `GET /api/files/{id}/children`
async fn children(
    State(state): State<Arc<AppState>>,
    _user: AuthUser,
    PathParam(id): PathParam<String>,
) -> Result<Json<Children>, ApiError> {
    state
        .db
        .call_api(move |connection| {
            let parent = lookup_file(connection, &id)
                .map_err(|error| not_found_or(error, "directory not found"))?;
            if parent.kind != FileKind::Directory {
                return Err(ApiError::not_found("directory not found"));
            }

            let sql = format!(
                "SELECT {FILE_COLUMNS} FROM files WHERE parent_id = ?1 AND deleted_at IS NULL \
ORDER BY kind DESC, name COLLATE NOCASE"
            );
            let mut statement = connection
                .prepare(&sql)
                .map_err(|error| database_error(DbError::Query(error)))?;
            let rows = statement
                .query_map([&parent.id], scan_file)
                .map_err(|error| database_error(DbError::Query(error)))?;
            let mut items = Vec::new();
            for row in rows {
                items.push(row.map_err(|error| database_error(DbError::Query(error)))?);
            }
            drop(statement);

            mark_audio_covers(connection, &parent.id, &mut items)?;

            // `directory_stats` is maintained by SQL triggers, so it is the
            // authoritative aggregate and no recount is needed. A missing row
            // means the database is inconsistent, and reporting zero bytes for
            // a directory that has content would be a silently wrong answer —
            // so this fails loudly, as Go did.
            let (total_bytes, file_count) = connection
                .query_row(
                    "SELECT total_bytes, file_count FROM directory_stats WHERE directory_id = ?1",
                    [&parent.id],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )
                .map_err(|error| {
                    tracing::error!(%error, directory = %parent.id, "directory usage is missing");
                    ApiError::internal("could not calculate directory usage")
                })?;

            Ok(Children {
                items,
                total_bytes,
                file_count,
            })
        })
        .await
        .map(Json)
}

/// Set `has_cover` on audio files that have a matching analysed cover.
///
/// The match is on `source_etag = files.etag`, so a stale analysis of replaced
/// content does not claim a cover the file no longer has.
fn mark_audio_covers(
    connection: &rusqlite::Connection,
    parent_id: &str,
    items: &mut [File],
) -> Result<(), ApiError> {
    let mut statement = connection
        .prepare(
            "SELECT m.file_id FROM media_metadata m JOIN files f ON f.id = m.file_id \
WHERE f.parent_id = ?1 AND f.deleted_at IS NULL AND m.source_etag = f.etag \
AND m.video_codec <> ''",
        )
        .map_err(|error| database_error(DbError::Query(error)))?;
    let rows = statement
        .query_map([parent_id], |row| row.get::<_, String>(0))
        .map_err(|error| database_error(DbError::Query(error)))?;
    let mut covered = Vec::new();
    for row in rows {
        covered.push(row.map_err(|error| database_error(DbError::Query(error)))?);
    }
    for item in items.iter_mut() {
        // Go guards this with `isAudioSource(f) && covered[f.ID]`: a video's
        // generated thumbnail is fetched through its own endpoint and must not
        // be advertised as an audio cover, because the browser keys its
        // album-art rendering on `has_cover`.
        if revaro_core::classify::is_audio(item) && covered.iter().any(|id| id == &item.id) {
            item.has_cover = true;
        }
    }
    Ok(())
}

/// `GET /api/storage/stats`
async fn storage_stats(
    State(state): State<Arc<AppState>>,
    _user: AuthUser,
) -> Result<Json<StorageStats>, ApiError> {
    state
        .db
        .call_api(|connection| {
            connection
                .query_row(
                    "SELECT COALESCE(SUM(size),0), COUNT(*) FROM files \
WHERE kind = 'file' AND status = 'ready' AND deleted_at IS NULL",
                    [],
                    |row| {
                        Ok(StorageStats {
                            total_bytes: row.get(0)?,
                            file_count: row.get(1)?,
                        })
                    },
                )
                .map_err(|error| database_error(DbError::Query(error)))
        })
        .await
        .map(Json)
}

/// Everything the library views need, loaded once.
struct LibraryData {
    files: Vec<File>,
    paths: std::collections::BTreeMap<String, Vec<FolderRef>>,
    durations: HashMap<String, i64>,
}

fn load_library(connection: &rusqlite::Connection) -> Result<LibraryData, DbError> {
    let directories = query_files(
        connection,
        "SELECT ".to_owned()
            + FILE_COLUMNS
            + " FROM files WHERE kind = 'directory' AND deleted_at IS NULL",
        [],
    )?;
    let paths = aggregation::folder_paths(&directories);

    let mut durations = HashMap::new();
    {
        let mut statement = connection
            .prepare(
                "SELECT m.file_id, m.duration_ms FROM media_metadata m JOIN files f ON f.id = m.file_id \
WHERE m.source_etag = f.etag",
            )
            .map_err(DbError::Query)?;
        let rows = statement
            .query_map([], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
            })
            .map_err(DbError::Query)?;
        for row in rows {
            let (id, duration) = row.map_err(DbError::Query)?;
            durations.insert(id, duration);
        }
    }

    let files = query_files(
        connection,
        "SELECT ".to_owned()
            + FILE_COLUMNS
            + " FROM files WHERE kind = 'file' AND status = 'ready' AND deleted_at IS NULL \
ORDER BY name COLLATE NOCASE",
        [],
    )?;
    Ok(LibraryData {
        files,
        paths,
        durations,
    })
}

fn query_files<P: rusqlite::Params>(
    connection: &rusqlite::Connection,
    sql: String,
    params: P,
) -> Result<Vec<File>, DbError> {
    let mut statement = connection.prepare(&sql).map_err(DbError::Query)?;
    let rows = statement
        .query_map(params, scan_file)
        .map_err(DbError::Query)?;
    let mut out = Vec::new();
    for row in rows {
        out.push(row.map_err(DbError::Query)?);
    }
    Ok(out)
}

impl LibraryData {
    fn items(&self, kind: LibraryKind) -> Vec<LibraryItem> {
        self.files
            .iter()
            .filter(|file| revaro_core::classify::matches_library_kind(file, kind))
            .map(|file| aggregation::item(file, &self.paths, &self.durations))
            .collect()
    }
}

/// Query string of `GET /api/library`.
#[derive(Debug, serde::Deserialize)]
struct LibraryQuery {
    #[serde(rename = "type")]
    kind: Option<String>,
}

/// `GET /api/library?type=…`
async fn library(
    State(state): State<Arc<AppState>>,
    _user: AuthUser,
    Query(query): Query<LibraryQuery>,
) -> Result<Json<Library>, ApiError> {
    let requested = query.kind.unwrap_or_else(|| "file".to_owned());
    let kind: LibraryKind = requested
        .parse()
        .map_err(|()| ApiError::bad_request("unknown library type"))?;

    let data = state
        .db
        .call_api(|connection| load_library(connection).map_err(database_error))
        .await?;
    let counts = aggregation::bucket_counts(&data.files);
    Ok(Json(Library {
        kind,
        items: data.items(kind),
        counts,
    }))
}

/// `GET /api/library/all`
async fn library_all(
    State(state): State<Arc<AppState>>,
    _user: AuthUser,
) -> Result<Json<LibraryAll>, ApiError> {
    let data = state
        .db
        .call_api(|connection| load_library(connection).map_err(database_error))
        .await?;
    let counts = aggregation::bucket_counts(&data.files);
    // `file` is deliberately absent: it is the plain browser's job, and the Go
    // handler grouped only the four media buckets.
    let items = LibraryBuckets {
        book: data.items(LibraryKind::Book),
        image: data.items(LibraryKind::Image),
        video: data.items(LibraryKind::Video),
        audio: data.items(LibraryKind::Audio),
    };
    Ok(Json(LibraryAll { items, counts }))
}

/// `GET /api/library/counts`
async fn library_counts(
    State(state): State<Arc<AppState>>,
    _user: AuthUser,
) -> Result<Json<LibraryCounts>, ApiError> {
    let data = state
        .db
        .call_api(|connection| load_library(connection).map_err(database_error))
        .await?;
    Ok(Json(aggregation::bucket_counts(&data.files)))
}

/// `POST /api/directories`
async fn create_directory(
    State(state): State<Arc<AppState>>,
    _user: AuthUser,
    Json(request): Json<revaro_core::api::CreateDirectoryRequest>,
) -> Result<(http::StatusCode, Json<File>), ApiError> {
    revaro_core::validate::validate_name(&request.name)?;
    let id = crate::ids::new_id();
    let created = state
        .db
        .call_api(move |connection| {
            let valid: bool = connection
                .query_row(
                    "SELECT EXISTS(SELECT 1 FROM files WHERE id = ?1 AND kind = 'directory' \
AND status = 'ready' AND deleted_at IS NULL)",
                    [&request.parent_id],
                    |row| row.get(0),
                )
                .map_err(|error| database_error(DbError::Query(error)))?;
            if !valid {
                return Err(ApiError::bad_request("parent directory is invalid"));
            }
            let now = Timestamp::now().to_rfc3339();
            connection
                .execute(
                    "INSERT INTO files(id,parent_id,name,kind,status,created_at,updated_at) \
VALUES(?1,?2,?3,'directory','ready',?4,?4)",
                    rusqlite::params![id, request.parent_id, request.name, now],
                )
                .map_err(|error| conflict_or(DbError::Query(error)))?;
            lookup_file(connection, &id).map_err(database_error)
        })
        .await?;
    Ok((http::StatusCode::CREATED, Json(created)))
}

/// `PATCH /api/files/{id}` — rename, move, or both.
async fn patch_file(
    State(state): State<Arc<AppState>>,
    _user: AuthUser,
    PathParam(id): PathParam<String>,
    Json(request): Json<revaro_core::api::PatchFileRequest>,
) -> Result<Json<File>, ApiError> {
    if revaro_core::ids::is_root(&id) {
        return Err(ApiError::bad_request("root cannot be modified"));
    }
    if let Some(name) = &request.name {
        revaro_core::validate::validate_name(name)?;
    }
    let updated = state
        .db
        .call_api(move |connection| {
            let existing = lookup_file(connection, &id)
                .map_err(|error| not_found_or(error, "file not found"))?;
            let name = request
                .name
                .clone()
                .unwrap_or_else(|| existing.name.clone());
            let parent = request
                .parent_id
                .clone()
                .or_else(|| existing.parent_id.clone());

            if let Some(parent_id) = &parent
                && parent_id != &existing.parent_id.clone().unwrap_or_default()
            {
                let valid: bool = connection
                    .query_row(
                        "SELECT EXISTS(SELECT 1 FROM files WHERE id = ?1 AND kind = 'directory' \
AND status = 'ready' AND deleted_at IS NULL)",
                        [parent_id],
                        |row| row.get(0),
                    )
                    .map_err(|error| database_error(DbError::Query(error)))?;
                if !valid {
                    return Err(ApiError::bad_request("target directory is invalid"));
                }
                if existing.kind == FileKind::Directory {
                    // The authoritative cycle check, ported from Go: does the
                    // destination live inside the directory being moved?
                    let cyclic: bool = connection
                        .query_row(
                            "WITH RECURSIVE d(id) AS (SELECT id FROM files WHERE id = ?1 \
UNION ALL SELECT f.id FROM files f JOIN d ON f.parent_id = d.id) \
SELECT EXISTS(SELECT 1 FROM d WHERE id = ?2)",
                            rusqlite::params![id, parent_id],
                            |row| row.get(0),
                        )
                        .map_err(|error| database_error(DbError::Query(error)))?;
                    if cyclic {
                        return Err(ApiError::bad_request(
                            "a directory cannot be moved into itself or its descendants",
                        ));
                    }
                }
            }

            connection
                .execute(
                    "UPDATE files SET name = ?1, parent_id = ?2, updated_at = ?3 WHERE id = ?4",
                    rusqlite::params![name, parent, Timestamp::now().to_rfc3339(), id],
                )
                .map_err(|error| conflict_or(DbError::Query(error)))?;
            lookup_file(connection, &id).map_err(database_error)
        })
        .await?;
    Ok(Json(updated))
}

/// `DELETE /api/files/{id}` — move into the trash, or drop unfinished rows.
///
/// A file that is still pending has no recoverable content, so Go removed it
/// outright rather than trashing it. This port keeps that behaviour but not the
/// pending-upload abort that precedes it in Go: aborting needs the upload
/// module, and until it lands the cascade delete removes the `uploads` row while
/// any multipart staging is reaped by the store's age-based cleanup.
async fn delete_file(
    State(state): State<Arc<AppState>>,
    _user: AuthUser,
    PathParam(id): PathParam<String>,
) -> Result<http::StatusCode, ApiError> {
    if revaro_core::ids::is_root(&id) {
        return Err(ApiError::bad_request("root cannot be deleted"));
    }
    state
        .db
        .call_api(move |connection| {
            let file = lookup_file(connection, &id).map_err(|error| not_found_or(error, "file not found"))?;

            if file.kind == FileKind::File && file.status != FileStatus::Ready {
                let changed = connection
                    .execute("DELETE FROM files WHERE id = ?1 AND status <> 'ready'", [&id])
                    .map_err(|error| database_error(DbError::Query(error)))?;
                if changed != 1 {
                    return Err(ApiError::conflict("file changed; refresh before deleting"));
                }
                return Ok(http::StatusCode::NO_CONTENT);
            }

            let now = Timestamp::now().to_rfc3339();
            let transaction = connection
                .transaction()
                .map_err(|error| database_error(DbError::Query(error)))?;
            // Marking the whole subtree and then detaching the root keeps every
            // trashed row reachable from the trash listing, while the original
            // parent is remembered so a restore can go back.
            transaction
                .execute(
                    "WITH RECURSIVE tree(id) AS (SELECT id FROM files WHERE id = ?1 \
AND deleted_at IS NULL UNION ALL SELECT f.id FROM files f JOIN tree t ON f.parent_id = t.id \
WHERE f.deleted_at IS NULL) \
UPDATE files SET deleted_at = ?2, trash_root_id = ?1 WHERE id IN tree",
                    rusqlite::params![id, now],
                )
                .map_err(|error| database_error(DbError::Query(error)))?;
            transaction
                .execute(
                    "UPDATE files SET restore_parent_id = parent_id, parent_id = NULL WHERE id = ?1",
                    [&id],
                )
                .map_err(|error| database_error(DbError::Query(error)))?;
            transaction
                .commit()
                .map_err(|error| database_error(DbError::Query(error)))?;
            Ok(http::StatusCode::NO_CONTENT)
        })
        .await
}

/// `GET /api/trash`
async fn trash(
    State(state): State<Arc<AppState>>,
    _user: AuthUser,
) -> Result<Json<revaro_core::api::Trash>, ApiError> {
    state
        .db
        .call_api(|connection| {
            let sql = format!(
                "SELECT {FILE_COLUMNS} FROM files WHERE deleted_at IS NOT NULL AND trash_root_id = id \
ORDER BY deleted_at DESC"
            );
            let items = query_files(connection, sql, []).map_err(database_error)?;
            let (total_bytes, file_count) = connection
                .query_row(
                    "SELECT COALESCE(SUM(size),0), COUNT(*) FROM files \
WHERE kind = 'file' AND status = 'ready' AND deleted_at IS NOT NULL",
                    [],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )
                .map_err(|error| database_error(DbError::Query(error)))?;
            Ok(revaro_core::api::Trash { items, total_bytes, file_count })
        })
        .await
        .map(Json)
}

/// `DELETE /api/trash` — empty it.
async fn empty_trash(
    State(state): State<Arc<AppState>>,
    _user: AuthUser,
) -> Result<http::StatusCode, ApiError> {
    state
        .db
        .call_api(|connection| {
            let transaction = connection
                .transaction()
                .map_err(|error| database_error(DbError::Query(error)))?;
            // Deferred foreign keys let the cascade order settle before the
            // constraints are checked at commit.
            transaction
                .execute_batch("PRAGMA defer_foreign_keys=ON")
                .map_err(|error| database_error(DbError::Query(error)))?;
            transaction
                .execute("DELETE FROM files WHERE deleted_at IS NOT NULL", [])
                .map_err(|error| database_error(DbError::Query(error)))?;
            transaction
                .commit()
                .map_err(|error| database_error(DbError::Query(error)))?;
            Ok(http::StatusCode::NO_CONTENT)
        })
        .await
}

/// `POST /api/trash/{id}/restore`
async fn restore_trash(
    State(state): State<Arc<AppState>>,
    _user: AuthUser,
    PathParam(id): PathParam<String>,
) -> Result<http::StatusCode, ApiError> {
    state
        .db
        .call_api(move |connection| {
            let file = connection
                .query_row(
                    &format!(
                        "SELECT {FILE_COLUMNS} FROM files \
WHERE id = ?1 AND deleted_at IS NOT NULL AND trash_root_id = id"
                    ),
                    [&id],
                    scan_file,
                )
                .map_err(|error| not_found_or(DbError::Query(error), "trash item not found"))?;

            // An item whose original directory is gone has to go back to the
            // root: Go treated the root as a valid fallback.
            let parent = match &file.restore_parent_id {
                Some(candidate) => {
                    let valid: bool = connection
                        .query_row(
                            "SELECT EXISTS(SELECT 1 FROM files WHERE id = ?1 AND kind = 'directory' \
AND status = 'ready' AND deleted_at IS NULL)",
                            [candidate],
                            |row| row.get(0),
                        )
                        .map_err(|error| database_error(DbError::Query(error)))?;
                    if valid { candidate.clone() } else { revaro_core::ids::ROOT_ID.to_owned() }
                }
                None => revaro_core::ids::ROOT_ID.to_owned(),
            };

            let transaction = connection
                .transaction()
                .map_err(|error| database_error(DbError::Query(error)))?;
            transaction
                .execute("UPDATE files SET parent_id = ?1 WHERE id = ?2", rusqlite::params![parent, id])
                .map_err(|error| database_error(DbError::Query(error)))?;
            // This is the statement the unique name index actually fires on: the
            // row only becomes visible to `files_unique_name` once `deleted_at`
            // is cleared. Mapping the conflict on the *first* update instead
            // turned every restore collision into a 500.
            transaction
                .execute(
                    "UPDATE files SET deleted_at = NULL, restore_parent_id = NULL, trash_root_id = NULL \
WHERE trash_root_id = ?1",
                    [&id],
                )
                .map_err(|error| restore_conflict_or(DbError::Query(error)))?;
            transaction
                .commit()
                .map_err(|error| database_error(DbError::Query(error)))?;
            Ok(http::StatusCode::NO_CONTENT)
        })
        .await
}

/// `DELETE /api/trash/{id}` — remove one item permanently.
async fn purge_trash(
    State(state): State<Arc<AppState>>,
    _user: AuthUser,
    PathParam(id): PathParam<String>,
) -> Result<http::StatusCode, ApiError> {
    state
        .db
        .call_api(move |connection| {
            let present: bool = connection
                .query_row(
                    "SELECT EXISTS(SELECT 1 FROM files WHERE id = ?1 AND deleted_at IS NOT NULL \
AND trash_root_id = id)",
                    [&id],
                    |row| row.get(0),
                )
                .map_err(|error| database_error(DbError::Query(error)))?;
            if !present {
                return Err(ApiError::not_found("trash item not found"));
            }
            let transaction = connection
                .transaction()
                .map_err(|error| database_error(DbError::Query(error)))?;
            transaction
                .execute_batch("PRAGMA defer_foreign_keys=ON")
                .map_err(|error| database_error(DbError::Query(error)))?;
            // Deleting the subtree fires the cleanup trigger, which queues every
            // blob for reclamation in the same transaction.
            transaction
                .execute(
                    "WITH RECURSIVE tree(id) AS (SELECT id FROM files WHERE id = ?1 \
UNION ALL SELECT f.id FROM files f JOIN tree t ON f.parent_id = t.id) \
DELETE FROM files WHERE id IN tree",
                    [&id],
                )
                .map_err(|error| database_error(DbError::Query(error)))?;
            transaction
                .commit()
                .map_err(|error| database_error(DbError::Query(error)))?;
            Ok(http::StatusCode::NO_CONTENT)
        })
        .await
}

/// The hash algorithm recorded alongside `content_hash`.
const CONTENT_HASH_ALGORITHM: &str = "sha256";

/// `GET /api/files/{id}/content`
async fn get_document(
    State(state): State<Arc<AppState>>,
    _user: AuthUser,
    PathParam(id): PathParam<String>,
) -> Result<Json<revaro_core::api::DocumentContent>, ApiError> {
    let file = state
        .db
        .call_api(move |connection| {
            let file = lookup_file_any(connection, &id)
                .map_err(|error| not_found_or(error, "ready file not found"))?;
            if file.kind != FileKind::File || file.status != FileStatus::Ready {
                return Err(ApiError::not_found("ready file not found"));
            }
            Ok(file)
        })
        .await?;

    // The name decides editability, not the stored MIME type: a `.md` uploaded
    // as octet-stream is still editable.
    if !revaro_core::classify::is_editable_name(&file.name) {
        return Err(ApiError::unsupported_media_type(
            "this file type cannot be edited as text",
        ));
    }
    if file.size > revaro_core::limits::MAX_DOCUMENT_BYTES as i64 {
        return Err(ApiError::payload_too_large(
            "editable documents cannot exceed 1 MiB",
        ));
    }

    let bytes = state
        .store
        .read(&file.object_key, revaro_core::limits::MAX_DOCUMENT_BYTES)
        .await
        .map_err(|error| match error {
            crate::storage::StorageError::TooLarge { .. } => {
                ApiError::payload_too_large("editable documents cannot exceed 1 MiB")
            }
            crate::storage::StorageError::NotFound => ApiError::not_found("ready file not found"),
            other => {
                tracing::error!(%other, "document read failed");
                ApiError::new(502, "object storage read failed")
            }
        })?;

    // Rust strings are UTF-8 by construction, so an invalid byte sequence only
    // reaches here from a file that is not actually text.
    let content = String::from_utf8(bytes)
        .map_err(|_| ApiError::unsupported_media_type("file is not valid UTF-8 text"))?;

    Ok(Json(revaro_core::api::DocumentContent {
        content,
        etag: file.etag,
        updated_at: file.updated_at,
    }))
}

/// `PUT /api/files/{id}/content`
async fn update_document(
    State(state): State<Arc<AppState>>,
    _user: AuthUser,
    PathParam(id): PathParam<String>,
    Json(request): Json<revaro_core::api::UpdateDocumentRequest>,
) -> Result<Json<File>, ApiError> {
    let file = state
        .db
        .call_api(move |connection| {
            let file = lookup_file(connection, &id)
                .map_err(|error| not_found_or(error, "ready file not found"))?;
            if file.kind != FileKind::File || file.status != FileStatus::Ready {
                return Err(ApiError::not_found("ready file not found"));
            }
            Ok(file)
        })
        .await?;

    revaro_core::validate::validate_document(&file.name, &request.content)?;

    // An empty ETag means "I did not read a version", so the check is skipped;
    // the conditional UPDATE below still protects against a lost update.
    if !request.etag.is_empty() && !file.etag.is_empty() && request.etag != file.etag {
        return Err(ApiError::conflict(
            "document changed elsewhere; reopen it before saving",
        ));
    }

    let bytes = request.content.into_bytes();
    let size = bytes.len() as i64;
    let object_key = revaro_core::keys::blob_key(&crate::ids::new_id());
    let mut source: &[u8] = &bytes;
    let stored = state
        .store
        .write_stream(&object_key, &mut source, size)
        .await
        .map_err(|error| {
            tracing::error!(%error, "document write failed");
            ApiError::new(502, "object storage write failed")
        })?;
    let content_hash = revaro_core::keys::sha256_hex(&bytes);
    let mime = revaro_core::classify::document_mime(&file.name).to_owned();

    // The previous object key is part of the WHERE clause, so two concurrent
    // savers cannot both win: the loser updates zero rows and gets a 409 instead
    // of silently overwriting the winner between a check and a write.
    let previous_key = file.object_key.clone();
    let file_id = file.id.clone();
    let update = {
        let object_key = object_key.clone();
        let content_hash = content_hash.clone();
        state
            .db
            .call_api(move |connection| {
                let now = Timestamp::now().to_rfc3339();
                let changed = connection
                    .execute(
                        "UPDATE files SET object_key = ?1, size = ?2, mime_type = ?3, etag = ?4, \
content_hash = ?5, hash_algorithm = ?6, updated_at = ?7 \
WHERE id = ?8 AND object_key = ?9 AND status = 'ready' AND deleted_at IS NULL",
                        rusqlite::params![
                            object_key,
                            size,
                            mime,
                            stored.etag,
                            content_hash,
                            CONTENT_HASH_ALGORITHM,
                            now,
                            file_id,
                            previous_key,
                        ],
                    )
                    .map_err(|error| database_error(DbError::Query(error)))?;
                if changed == 0 {
                    return Err(ApiError::conflict(
                        "document changed elsewhere; reopen it before saving",
                    ));
                }
                lookup_file(connection, &file_id).map_err(database_error)
            })
            .await
    };

    let updated = match update {
        Ok(updated) => updated,
        Err(error) => {
            // The metadata write failed, so the object we just stored is
            // unreachable; remove it rather than leaking a blob.
            if let Err(cleanup) = state.store.delete(&object_key).await {
                tracing::warn!(%cleanup, key = %object_key, "could not discard an orphaned document blob");
            }
            return Err(error);
        }
    };
    Ok(Json(updated))
}

/// A share token: 32 random bytes as unpadded base64url (43 characters).
///
/// The length is also enforced when the public link is redeemed, so a token is
/// never guessable and a malformed one is rejected before touching the database.
fn new_share_token() -> String {
    use base64::Engine as _;
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(rand::random::<[u8; 32]>())
}

fn share_url(base_url: &str, token: &str) -> String {
    format!("{base_url}/s/{token}")
}

/// `GET /api/files/{id}/share`
async fn get_share(
    State(state): State<Arc<AppState>>,
    _user: AuthUser,
    PathParam(id): PathParam<String>,
) -> Result<Json<revaro_core::api::share::Status>, ApiError> {
    let base_url = state.config.base_url.clone();
    state
        .db
        .call_api(move |connection| {
            // Joining the file row means a share for a file that has since been
            // deleted or reverted to pending is reported as inactive rather than
            // handing back a dead link.
            let found = connection
                .query_row(
                    "SELECT s.token, s.created_at FROM shares s JOIN files f ON f.id = s.file_id \
WHERE s.file_id = ?1 AND f.kind = 'file' AND f.status = 'ready' AND f.deleted_at IS NULL",
                    [&id],
                    |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
                )
                .map(Some)
                .or_else(|error| match error {
                    rusqlite::Error::QueryReturnedNoRows => Ok(None),
                    other => Err(database_error(DbError::Query(other))),
                })?;
            Ok(match found {
                None => revaro_core::api::share::Status {
                    active: false,
                    url: None,
                    created_at: None,
                },
                Some((token, created_at)) => revaro_core::api::share::Status {
                    active: true,
                    url: Some(share_url(&base_url, &token)),
                    created_at: Timestamp::parse(&created_at).ok(),
                },
            })
        })
        .await
        .map(Json)
}

/// `POST /api/files/{id}/share`
async fn create_share(
    State(state): State<Arc<AppState>>,
    _user: AuthUser,
    PathParam(id): PathParam<String>,
) -> Result<(http::StatusCode, Json<revaro_core::api::share::Status>), ApiError> {
    let token = new_share_token();
    let base_url = state.config.base_url.clone();
    let created_at = Timestamp::now();
    let created = created_at.to_rfc3339();
    let issued = token.clone();
    let issued_at = created.clone();
    state
        .db
        .call_api(move |connection| {
            let file = lookup_file(connection, &id)
                .map_err(|error| not_found_or(error, "ready file not found"))?;
            if file.kind != FileKind::File || file.status != FileStatus::Ready {
                return Err(ApiError::not_found("ready file not found"));
            }
            // Re-sharing rotates the token, so a leaked link can be revoked by
            // sharing again rather than only by deleting the share.
            connection
                .execute(
                    "INSERT INTO shares(file_id,token,created_at) VALUES(?1,?2,?3) \
ON CONFLICT(file_id) DO UPDATE SET token = excluded.token, created_at = excluded.created_at",
                    rusqlite::params![id, issued, issued_at],
                )
                .map_err(|error| {
                    tracing::error!(%error, "could not create a share link");
                    ApiError::internal("could not create share link")
                })?;
            Ok(())
        })
        .await?;
    Ok((
        http::StatusCode::CREATED,
        Json(revaro_core::api::share::Status {
            active: true,
            url: Some(share_url(&base_url, &token)),
            created_at: Some(created_at),
        }),
    ))
}

/// `DELETE /api/files/{id}/share`
async fn revoke_share(
    State(state): State<Arc<AppState>>,
    _user: AuthUser,
    PathParam(id): PathParam<String>,
) -> Result<http::StatusCode, ApiError> {
    state
        .db
        .call_api(move |connection| {
            connection
                .execute("DELETE FROM shares WHERE file_id = ?1", [&id])
                .map_err(|error| {
                    tracing::error!(%error, "could not revoke the share link");
                    ApiError::internal("could not revoke share link")
                })?;
            Ok(http::StatusCode::NO_CONTENT)
        })
        .await
}

/// Media rows that may carry a playback position.
///
/// The position is only meaningful for audio and video, and Go refused the
/// endpoints for anything else so a client cannot use this table as arbitrary
/// per-file key/value storage.
fn require_media_file(connection: &Connection, id: &str) -> Result<File, ApiError> {
    let file = lookup_file(connection, id)
        .map_err(|error| not_found_or(error, "ready media file not found"))?;
    if file.kind != FileKind::File
        || file.status != FileStatus::Ready
        || !(revaro_core::classify::is_audio(&file) || revaro_core::classify::is_video(&file))
    {
        return Err(ApiError::not_found("ready media file not found"));
    }
    Ok(file)
}

/// Milliseconds to whole seconds, the unit the player speaks.
fn progress_response(
    position_ms: i64,
    duration_ms: i64,
    updated_at: Option<Timestamp>,
) -> revaro_core::api::progress::Media {
    revaro_core::api::progress::Media {
        position: position_ms as f64 / 1000.0,
        duration: duration_ms as f64 / 1000.0,
        updated_at,
    }
}

/// The canonical `tasks` projection.
///
/// `name` is derived rather than stored: the first output file's name, else the
/// first input file's name, else the task type. Doing it in SQL keeps the task
/// centre to one query instead of a lookup per row.
const TASK_COLUMNS: &str = "tasks.id,tasks.type,tasks.status,tasks.phase,tasks.progress,\
tasks.speed,tasks.eta_seconds,tasks.retry_count,tasks.max_retries,tasks.error,tasks.source_type,\
tasks.source_id,tasks.cancel_requested,tasks.created_at,tasks.started_at,tasks.finished_at,\
tasks.updated_at,COALESCE((SELECT files.name FROM task_files JOIN files ON files.id=task_files.file_id \
WHERE task_files.task_id=tasks.id AND task_files.role='output' LIMIT 1),\
(SELECT files.name FROM task_files JOIN files ON files.id=task_files.file_id \
WHERE task_files.task_id=tasks.id AND task_files.role='input' LIMIT 1),tasks.type)";

/// Decode one row selected with [`TASK_COLUMNS`].
pub fn scan_task(row: &Row<'_>) -> rusqlite::Result<revaro_core::model::Task> {
    Ok(revaro_core::model::Task {
        id: row.get(0)?,
        task_type: row.get(1)?,
        status: enum_column(row, 2, "status")?,
        phase: row.get(3)?,
        progress: row.get(4)?,
        speed: row.get(5)?,
        eta_seconds: row.get(6)?,
        retry_count: row.get(7)?,
        max_retries: row.get(8)?,
        error: row.get(9)?,
        // Nullable in the schema, but the wire format is a plain string that is
        // omitted when empty, so NULL becomes "".
        source_type: row.get::<_, Option<String>>(10)?.unwrap_or_default(),
        source_id: row.get::<_, Option<String>>(11)?.unwrap_or_default(),
        cancel_requested: row.get(12)?,
        created_at: timestamp_column(row, 13)?,
        started_at: optional_timestamp_column(row, 14)?,
        finished_at: optional_timestamp_column(row, 15)?,
        updated_at: timestamp_column(row, 16)?,
        name: row.get(17)?,
    })
}

/// The task types the task centre shows.
///
/// Other types exist in the table for internal bookkeeping and are deliberately
/// not surfaced, matching Go's filter.
const VISIBLE_TASK_TYPES: &str = "('upload','archive_extract','subtitle')";

/// `GET /api/tasks`
async fn list_tasks(
    State(state): State<Arc<AppState>>,
    _user: AuthUser,
) -> Result<Json<revaro_core::api::TaskList>, ApiError> {
    state
        .db
        .call_api(|connection| {
            // Terminal successes are notification-like records: the durable row
            // is kept for history, but it stops being returned after half an
            // hour so the panel does not accumulate stale entries.
            let sql = format!(
                "SELECT {TASK_COLUMNS} FROM tasks WHERE tasks.type IN {VISIBLE_TASK_TYPES} \
AND (tasks.status NOT IN ('completed','cancelled') \
OR julianday(tasks.finished_at) >= julianday('now','-30 minutes')) \
ORDER BY tasks.created_at DESC LIMIT 500"
            );
            let mut statement = connection
                .prepare(&sql)
                .map_err(|error| database_error(DbError::Query(error)))?;
            let rows = statement
                .query_map([], scan_task)
                .map_err(|error| database_error(DbError::Query(error)))?;
            let mut items = Vec::new();
            for row in rows {
                items.push(row.map_err(|error| database_error(DbError::Query(error)))?);
            }
            Ok(revaro_core::api::TaskList { items })
        })
        .await
        .map(Json)
}

/// `GET /api/tasks/{id}`
async fn get_task(
    State(state): State<Arc<AppState>>,
    _user: AuthUser,
    PathParam(id): PathParam<String>,
) -> Result<Json<revaro_core::model::Task>, ApiError> {
    state
        .db
        .call_api(move |connection| {
            connection
                .query_row(
                    &format!("SELECT {TASK_COLUMNS} FROM tasks WHERE tasks.id = ?1"),
                    [&id],
                    scan_task,
                )
                .map_err(|error| not_found_or(DbError::Query(error), "task not found"))
        })
        .await
        .map(Json)
}

/// Stored media analysis for a file, when it is still current.
///
/// The `source_etag = files.etag` condition is what makes an analysis valid: a
/// file whose bytes were replaced keeps its row but must not be described by
/// stale metadata. Re-analysing is the media engine's job and is not ported, so
/// a missing or stale row is reported as unavailable rather than silently
/// returning yesterday's duration.
fn current_media_metadata(
    connection: &Connection,
    file: &File,
) -> Result<Option<(i64, String, String)>, ApiError> {
    connection
        .query_row(
            "SELECT duration_ms, chapters_json, video_codec FROM media_metadata \
WHERE file_id = ?1 AND source_etag = ?2",
            rusqlite::params![file.id, file.etag],
            |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                ))
            },
        )
        .map(Some)
        .or_else(|error| match error {
            rusqlite::Error::QueryReturnedNoRows => Ok(None),
            other => Err(database_error(DbError::Query(other))),
        })
}

/// `GET /api/files/{id}/audio`
async fn audio_media_info(
    State(state): State<Arc<AppState>>,
    _user: AuthUser,
    PathParam(id): PathParam<String>,
) -> Result<Json<revaro_core::api::media::AudioMedia>, ApiError> {
    state
        .db
        .call_api(move |connection| {
            let file = lookup_file_any(connection, &id)
                .map_err(|error| not_found_or(error, "ready audio file not found"))?;
            if !revaro_core::classify::is_audio(&file) {
                return Err(ApiError::not_found("ready audio file not found"));
            }
            let Some((duration_ms, chapters_json, video_codec)) =
                current_media_metadata(connection, &file)?
            else {
                return Err(ApiError::not_found("audio metadata is not available"));
            };

            // A corrupt chapters column must not fail the whole response: the
            // duration and cover are still useful without the chapter list.
            let chapters: Vec<revaro_core::media::MediaChapter> =
                serde_json::from_str(&chapters_json).unwrap_or_default();
            let chapters = chapters
                .into_iter()
                .enumerate()
                .map(|(index, chapter)| revaro_core::media::AudioChapter {
                    id: index as i32 + 1,
                    title: chapter.title,
                    start: chapter.start_ms as f64 / 1000.0,
                    end: chapter.end_ms as f64 / 1000.0,
                })
                .collect();

            let has_cover = !video_codec.is_empty();
            Ok(revaro_core::api::media::AudioMedia {
                duration: duration_ms as f64 / 1000.0,
                chapters,
                cover_url: if has_cover {
                    format!("/api/files/{}/thumbnail?v={}", file.id, file.etag)
                } else {
                    String::new()
                },
                has_cover,
            })
        })
        .await
        .map(Json)
}

/// `GET /api/system/status`
///
/// Mirrors Go's snapshot, including two deliberate details:
///
/// * the storage figures count **ready files regardless of trash**, so `bytes`
///   includes trashed bytes and `trash_bytes` is the subset that is trashed.
///   `GET /api/storage/stats` answers the different question "live bytes" — the
///   two are not interchangeable.
/// * a component that cannot be measured degrades the whole response, rather
///   than reporting a plausible zero. The cache layer is not ported yet, so the
///   cache component reports `degraded` exactly as Go does with a nil cache,
///   and the overall status is therefore `degraded` until it is wired up.
async fn system_status(
    State(state): State<Arc<AppState>>,
) -> Result<Json<revaro_core::api::system::Status>, ApiError> {
    let measured = state
        .db
        .call_api(|connection| {
            let mut status = revaro_core::api::system::Status {
                status: "ok".to_owned(),
                database: revaro_core::api::system::Component {
                    status: "ok".to_owned(),
                    bytes: 0,
                },
                storage: revaro_core::api::system::Storage {
                    status: "ok".to_owned(),
                    bytes: 0,
                    trash_bytes: 0,
                    file_count: 0,
                },
                cache: revaro_core::api::system::Cache {
                    // No cache layer exists in the Rust port yet; saying `ok`
                    // would claim a measurement that was never taken.
                    status: "degraded".to_owned(),
                    memory_bytes: 0,
                    disk_bytes: 0,
                    memory_entries: 0,
                    disk_entries: 0,
                    classes: None,
                },
            };
            // Go's `degrade` helper marks the component *and* the whole
            // response, so an unavailable cache degrades the overall status.
            status.status = "degraded".to_owned();

            let pages = connection.query_row("PRAGMA page_count", [], |row| row.get::<_, i64>(0));
            let page_size =
                connection.query_row("PRAGMA page_size", [], |row| row.get::<_, i64>(0));
            match (pages, page_size) {
                (Ok(pages), Ok(page_size)) => status.database.bytes = pages * page_size,
                _ => {
                    status.database.status = "degraded".to_owned();
                    status.status = "degraded".to_owned();
                }
            }

            let storage = connection.query_row(
                "SELECT COALESCE(SUM(size),0), \
COALESCE(SUM(CASE WHEN deleted_at IS NOT NULL THEN size ELSE 0 END),0), COUNT(*) \
FROM files WHERE kind = 'file' AND status = 'ready'",
                [],
                |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        row.get::<_, i64>(1)?,
                        row.get::<_, i64>(2)?,
                    ))
                },
            );
            match storage {
                Ok((bytes, trash_bytes, file_count)) => {
                    status.storage.bytes = bytes;
                    status.storage.trash_bytes = trash_bytes;
                    status.storage.file_count = file_count;
                }
                Err(error) => {
                    tracing::warn!(%error, "could not measure storage");
                    status.storage.status = "degraded".to_owned();
                    status.status = "degraded".to_owned();
                }
            }
            Ok(status)
        })
        .await?;

    // The store is probed outside the transaction because it is asynchronous
    // and independent of the database.
    let mut measured = measured;
    if state.store.ping().await.is_err() {
        measured.storage.status = "degraded".to_owned();
        measured.status = "degraded".to_owned();
    }
    Ok(Json(measured))
}

/// `GET /api/files/{id}/media/progress`
async fn media_progress(
    State(state): State<Arc<AppState>>,
    _user: AuthUser,
    PathParam(id): PathParam<String>,
) -> Result<Json<revaro_core::api::progress::Media>, ApiError> {
    state
        .db
        .call_api(move |connection| {
            require_media_file(connection, &id)?;
            // A file that has never been played reports zeroes rather than 404,
            // so the player can call this unconditionally on open.
            let row = connection
                .query_row(
                    "SELECT position_ms, duration_ms, updated_at FROM media_progress WHERE file_id = ?1",
                    [&id],
                    |row| {
                        Ok((
                            row.get::<_, i64>(0)?,
                            row.get::<_, i64>(1)?,
                            row.get::<_, String>(2)?,
                        ))
                    },
                )
                .map(Some)
                .or_else(|error| match error {
                    rusqlite::Error::QueryReturnedNoRows => Ok(None),
                    other => Err(database_error(DbError::Query(other))),
                })?;
            Ok(match row {
                None => progress_response(0, 0, None),
                Some((position, duration, updated_at)) => {
                    progress_response(position, duration, Timestamp::parse(&updated_at).ok())
                }
            })
        })
        .await
        .map(Json)
}

/// `PUT /api/files/{id}/media/progress`
async fn save_media_progress(
    State(state): State<Arc<AppState>>,
    _user: AuthUser,
    PathParam(id): PathParam<String>,
    Json(request): Json<revaro_core::api::progress::Media>,
) -> Result<Json<revaro_core::api::progress::Media>, ApiError> {
    // Rejects NaN, infinities, negatives, anything beyond a week, and a position
    // more than five seconds past a known duration. These values become integer
    // milliseconds under a CHECK constraint, so an out-of-range value would
    // otherwise surface as a 500 rather than a 400.
    revaro_core::validate::validate_media_progress(request.position, request.duration)?;
    let position_ms = (request.position * 1000.0).round() as i64;
    let duration_ms = (request.duration * 1000.0).round() as i64;

    state
        .db
        .call_api(move |connection| {
            require_media_file(connection, &id)?;
            let now = Timestamp::now().to_rfc3339();
            // A zero duration means "not known yet" and must not erase a
            // duration established by an earlier save.
            connection
                .execute(
                    "INSERT INTO media_progress(file_id,position_ms,duration_ms,updated_at) \
VALUES(?1,?2,?3,?4) ON CONFLICT(file_id) DO UPDATE SET \
position_ms = excluded.position_ms, \
duration_ms = CASE WHEN excluded.duration_ms > 0 THEN excluded.duration_ms \
ELSE media_progress.duration_ms END, \
updated_at = excluded.updated_at",
                    rusqlite::params![id, position_ms, duration_ms, now],
                )
                .map_err(|error| database_error(DbError::Query(error)))?;

            let stored = connection
                .query_row(
                    "SELECT position_ms, duration_ms, updated_at FROM media_progress WHERE file_id = ?1",
                    [&id],
                    |row| {
                        Ok((
                            row.get::<_, i64>(0)?,
                            row.get::<_, i64>(1)?,
                            row.get::<_, String>(2)?,
                        ))
                    },
                )
                .map_err(|error| database_error(DbError::Query(error)))?;
            Ok(progress_response(
                stored.0,
                stored.1,
                Timestamp::parse(&stored.2).ok(),
            ))
        })
        .await
        .map(Json)
}

/// `POST /api/documents` — create a new text document.
///
/// The bytes are written first and the row second, so a failed insert can
/// discard the freshly written object rather than leave an unreferenced blob.
/// The parent check is folded into the insert for the same reason as elsewhere:
/// a directory deleted concurrently must not be written into.
async fn create_document(
    State(state): State<Arc<AppState>>,
    _user: AuthUser,
    Json(request): Json<revaro_core::api::CreateDocumentRequest>,
) -> Result<(http::StatusCode, Json<File>), ApiError> {
    revaro_core::validate::validate_document(&request.name, &request.content)?;

    let file_id = crate::ids::new_id();
    let object_key = keys::blob_key(&crate::ids::new_id());
    let mime = revaro_core::classify::document_mime(&request.name).to_owned();
    let bytes = request.content.into_bytes();
    let size = bytes.len() as i64;

    let mut source: &[u8] = &bytes;
    let stored = state
        .store
        .write_stream(&object_key, &mut source, size)
        .await
        .map_err(|error| {
            tracing::error!(%error, "document write failed");
            ApiError::new(502, "object storage write failed")
        })?;
    let content_hash = keys::sha256_hex(&bytes);

    let insert = {
        let (parent_id, name) = (request.parent_id.clone(), request.name.clone());
        let (object_key, mime, content_hash) =
            (object_key.clone(), mime.clone(), content_hash.clone());
        state
            .db
            .call_api(move |connection| {
                let now = Timestamp::now().to_rfc3339();
                let inserted = connection
                    .execute(
                        "INSERT INTO files(id,parent_id,name,kind,object_key,size,mime_type,etag,\
content_hash,hash_algorithm,status,created_at,updated_at) \
SELECT ?1,?2,?3,'file',?4,?5,?6,?7,?8,?9,'ready',?10,?10 \
WHERE EXISTS(SELECT 1 FROM files WHERE id = ?2 AND kind = 'directory' AND status = 'ready' \
AND deleted_at IS NULL)",
                        rusqlite::params![
                            file_id,
                            parent_id,
                            name,
                            object_key,
                            size,
                            mime,
                            stored.etag,
                            content_hash,
                            CONTENT_HASH_ALGORITHM,
                            now,
                        ],
                    )
                    .map_err(|error| conflict_or(DbError::Query(error)))?;
                if inserted != 1 {
                    return Err(ApiError::conflict(
                        "parent directory is no longer available",
                    ));
                }
                lookup_file_for_commit(connection, &file_id).map_err(database_error)
            })
            .await
    };

    match insert {
        Ok(file) => Ok((http::StatusCode::CREATED, Json(file))),
        Err(error) => {
            // The row never landed, so the object is unreachable.
            if let Err(cleanup) = state.store.delete(&object_key).await {
                tracing::warn!(%cleanup, key = %object_key, "could not discard an orphaned document blob");
            }
            Err(error)
        }
    }
}

/// Pick a free name for a copy of `original` inside `parent_id`.
///
/// Mirrors Go exactly: the source name if it is free, otherwise
/// `"<stem> - 副本<ext>"`, then `"<stem> - 副本 N<ext>"` up to 9999. The suffix is
/// part of the product's Chinese UI and is reproduced verbatim.
fn available_copy_name(
    connection: &Connection,
    parent_id: &str,
    original: &str,
) -> Result<String, ApiError> {
    let taken = |name: &str| -> Result<bool, ApiError> {
        connection
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM files WHERE parent_id = ?1 AND name = ?2 \
AND deleted_at IS NULL)",
                rusqlite::params![parent_id, name],
                |row| row.get(0),
            )
            .map_err(|error| database_error(DbError::Query(error)))
    };

    if !taken(original)? {
        return Ok(original.to_owned());
    }

    let extension = revaro_core::classify::extension(original);
    let (stem, suffix) = if extension.is_empty() {
        (original, "")
    } else {
        (
            &original[..original.len() - extension.len() - 1],
            &original[original.len() - extension.len() - 1..],
        )
    };

    for index in 1..=9999 {
        let marker = if index > 1 {
            format!(" - 副本 {index}")
        } else {
            " - 副本".to_owned()
        };
        let candidate = format!("{stem}{marker}{suffix}");
        // A long original name can push the copy name past the limit; that is a
        // failure, not something to silently truncate.
        revaro_core::validate::validate_name(&candidate)
            .map_err(|_| ApiError::internal("copy name is too long"))?;
        if !taken(&candidate)? {
            return Ok(candidate);
        }
    }
    Err(ApiError::internal("too many copies with the same name"))
}

/// `POST /api/files/{id}/copy`
async fn copy_file(
    State(state): State<Arc<AppState>>,
    _user: AuthUser,
    PathParam(id): PathParam<String>,
    Json(request): Json<revaro_core::api::CopyFileRequest>,
) -> Result<(http::StatusCode, Json<File>), ApiError> {
    state
        .db
        .call_api(move |connection| {
            let source = lookup_file(connection, &id)
                .map_err(|error| not_found_or(error, "ready file not found"))?;
            if source.kind != FileKind::File || source.status != FileStatus::Ready {
                return Err(ApiError::not_found("ready file not found"));
            }
            let parent = lookup_file(connection, &request.parent_id)
                .map_err(|_| ApiError::bad_request("target directory is invalid"))?;
            if parent.kind != FileKind::Directory || parent.status != FileStatus::Ready {
                return Err(ApiError::bad_request("target directory is invalid"));
            }

            let transaction = connection
                .transaction()
                .map_err(|error| database_error(DbError::Query(error)))?;

            // Re-read inside the transaction so a concurrent purge cannot drop
            // the final reference to the blob between the check and the insert.
            let source = transaction
                .query_row(
                    &format!(
                        "SELECT {FILE_COLUMNS} FROM files WHERE id = ?1 AND kind = 'file' \
AND status = 'ready' AND deleted_at IS NULL"
                    ),
                    [&id],
                    scan_file,
                )
                .map_err(|_| ApiError::conflict("source file is no longer available"))?;

            let name = available_copy_name(&transaction, &request.parent_id, &source.name)?;
            let copy_id = crate::ids::new_id();
            let now = Timestamp::now().to_rfc3339();
            let inserted = transaction
                .execute(
                    "INSERT INTO files(id,parent_id,name,kind,object_key,size,mime_type,etag,\
content_hash,hash_algorithm,status,created_at,updated_at) \
SELECT ?1,?2,?3,'file',?4,?5,?6,?7,?8,?9,'ready',?10,?10 \
WHERE EXISTS(SELECT 1 FROM files WHERE id = ?2 AND kind = 'directory' AND status = 'ready' \
AND deleted_at IS NULL)",
                    rusqlite::params![
                        copy_id,
                        request.parent_id,
                        name,
                        source.object_key,
                        source.size,
                        source.mime_type,
                        source.etag,
                        source.content_hash,
                        source.hash_algorithm,
                        now,
                    ],
                )
                .map_err(|error| conflict_or(DbError::Query(error)))?;
            if inserted != 1 {
                return Err(ApiError::conflict("parent directory is no longer available"));
            }

            // Carry the analysis across so the copy does not have to be probed
            // again. A source with no metadata simply copies zero rows.
            transaction
                .execute(
                    "INSERT INTO media_metadata(file_id,duration_ms,container,video_codec,audio_codec,\
width,height,bitrate,chapters_json,analyzed_at,frame_rate,video_profile,video_level,subtitles_json,\
source_etag,probe_version) SELECT ?1,duration_ms,container,video_codec,audio_codec,width,height,\
bitrate,chapters_json,analyzed_at,frame_rate,video_profile,video_level,subtitles_json,source_etag,\
probe_version FROM media_metadata WHERE file_id = ?2",
                    rusqlite::params![copy_id, id],
                )
                .map_err(|error| database_error(DbError::Query(error)))?;

            let copied = lookup_file(&transaction, &copy_id).map_err(database_error)?;
            transaction
                .commit()
                .map_err(|error| database_error(DbError::Query(error)))?;
            Ok((http::StatusCode::CREATED, copied))
        })
        .await
        .map(|(status, file)| (status, Json(file)))
}

/// Restore reports its conflict with a different message: the name is taken at
/// the location the item is going *back* to, which is more actionable than the
/// generic duplicate-name message.
fn restore_conflict_or(error: DbError) -> ApiError {
    if error.is_constraint_violation() {
        ApiError::conflict("an item with that name already exists at the restore location")
    } else {
        database_error(error)
    }
}

/// Map a uniqueness violation onto `409` and anything else onto `500`.
fn conflict_or(error: DbError) -> ApiError {
    if error.is_constraint_violation() {
        ApiError::conflict("an item with that name already exists")
    } else {
        database_error(error)
    }
}

/// Map an internal failure onto the response the client expects.
fn database_error(error: DbError) -> ApiError {
    tracing::error!(%error, "file query failed");
    ApiError::internal("database error")
}

/// `404` when the row was simply absent, `500` when the query itself broke.
fn not_found_or(error: DbError, message: &'static str) -> ApiError {
    if error.is_not_found() {
        ApiError::not_found(message)
    } else {
        database_error(error)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;
    use crate::db::Database;
    use crate::storage::LocalStore;
    use axum::body::Body;
    use http::{Request, StatusCode};
    use http_body_util::BodyExt as _;
    use revaro_core::ids::ROOT_ID;
    use tower::ServiceExt as _;

    async fn state() -> Arc<AppState> {
        let config = Config::from_lookup(&|name| match name {
            "APP_BASE_URL" => Some("http://localhost:8080".to_owned()),
            "APP_WEB_DIR" => Some("/nonexistent".to_owned()),
            _ => None,
        })
        .unwrap();
        let root = std::env::temp_dir().join(format!(
            "revaro-files-store-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let store = LocalStore::open(&root).await.unwrap();
        let database = Database::open_in_memory().unwrap();
        let auth = crate::auth::AuthService::new(database.clone());
        AppState::new(Arc::new(config), database, store, auth)
    }

    async fn seed(state: &Arc<AppState>, sql: &'static str) {
        state
            .db
            .call(move |connection| {
                connection.execute_batch(sql).map_err(DbError::Query)?;
                Ok(())
            })
            .await
            .unwrap();
    }

    async fn call(state: &Arc<AppState>, uri: &str) -> (StatusCode, serde_json::Value) {
        // Reads need no origin header; `AuthUser` is satisfied by inserting a
        // session below.
        let request = Request::builder()
            .uri(uri)
            .header(
                "cookie",
                format!("{}={}", crate::auth::SESSION_COOKIE, token(state).await),
            )
            .body(Body::empty())
            .unwrap();
        let response = crate::router::build(state.clone())
            .oneshot(request)
            .await
            .unwrap();
        let status = response.status();
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        (
            status,
            serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null),
        )
    }

    /// Insert a session row and return its raw token.
    async fn token(state: &Arc<AppState>) -> String {
        let raw = "test-session-token".to_owned();
        // Use the production helper: the stored hash is base64url(sha256), not
        // hex, and reimplementing it here would silently break authentication.
        let hash = crate::auth::token_hash(&raw);
        state
            .db
            .call(move |connection| {
                connection
                    .execute_batch(&format!(
                        "INSERT OR REPLACE INTO settings(key,value,updated_at) VALUES('admin_username','admin','2024-01-01T00:00:00Z');\
INSERT OR REPLACE INTO sessions(id,token_hash,created_at,expires_at) VALUES('s1','{hash}','2024-01-01T00:00:00Z','2999-01-01T00:00:00Z');"
                    ))
                    .map_err(DbError::Query)?;
                Ok(())
            })
            .await
            .unwrap();
        raw
    }

    fn file_row(id: &str, parent: &str, name: &str, kind: &str, size: i64, mime: &str) -> String {
        format!(
            "INSERT INTO files(id,parent_id,name,kind,object_key,size,mime_type,status,created_at,updated_at) \
VALUES('{id}','{parent}','{name}','{kind}',{}, {size},'{mime}','ready','2024-01-01T00:00:00Z','2024-01-01T00:00:00Z');",
            if kind == "file" {
                format!("'blobs/{id}'")
            } else {
                "NULL".to_owned()
            }
        )
    }

    /// An authenticated write request: writes also need a same-origin `Origin`
    /// header because of the origin guard.
    async fn write(
        state: &Arc<AppState>,
        method: &str,
        uri: &str,
        body: Option<serde_json::Value>,
    ) -> (StatusCode, serde_json::Value) {
        let mut builder = Request::builder()
            .method(method)
            .uri(uri)
            .header("origin", "http://localhost:8080")
            .header(
                "cookie",
                format!("{}={}", crate::auth::SESSION_COOKIE, token(state).await),
            );
        let payload = match body {
            Some(value) => {
                builder = builder.header("content-type", "application/json");
                value.to_string()
            }
            None => String::new(),
        };
        let request = builder.body(Body::from(payload)).unwrap();
        let response = crate::router::build(state.clone())
            .oneshot(request)
            .await
            .unwrap();
        let status = response.status();
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        (
            status,
            serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null),
        )
    }

    #[tokio::test]
    async fn create_rename_move_delete_and_restore_round_trip() {
        let state = state().await;
        let root = ROOT_ID;

        // Create a directory, then a child directory inside it.
        let (status, parent) = write(
            &state,
            "POST",
            "/api/directories",
            Some(serde_json::json!({"parent_id": root, "name": "movies"})),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED);
        let parent_id = parent["id"].as_str().unwrap().to_owned();
        assert_eq!(parent["kind"], "directory");

        let (status, child) = write(
            &state,
            "POST",
            "/api/directories",
            Some(serde_json::json!({"parent_id": parent_id, "name": "scifi"})),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED);
        let child_id = child["id"].as_str().unwrap().to_owned();

        // A duplicate name in the same directory is a conflict, not a 500.
        let (status, body) = write(
            &state,
            "POST",
            "/api/directories",
            Some(serde_json::json!({"parent_id": root, "name": "movies"})),
        )
        .await;
        assert_eq!(status, StatusCode::CONFLICT);
        assert_eq!(
            body["error"]["message"],
            "an item with that name already exists"
        );

        // An invalid name is refused before touching the database.
        let (status, body) = write(
            &state,
            "POST",
            "/api/directories",
            Some(serde_json::json!({"parent_id": root, "name": "a/b"})),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(
            body["error"]["message"],
            "name cannot contain path separators"
        );

        // Renaming works, and the new name is what the listing shows.
        let (status, renamed) = write(
            &state,
            "PATCH",
            &format!("/api/files/{parent_id}"),
            Some(serde_json::json!({"name": "films"})),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(renamed["name"], "films");

        // Moving a directory into its own descendant must be refused.
        let (status, body) = write(
            &state,
            "PATCH",
            &format!("/api/files/{parent_id}"),
            Some(serde_json::json!({"parent_id": child_id})),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(
            body["error"]["message"],
            "a directory cannot be moved into itself or its descendants"
        );

        // The root is immutable.
        let (status, body) = write(
            &state,
            "PATCH",
            &format!("/api/files/{root}"),
            Some(serde_json::json!({"name": "x"})),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(body["error"]["message"], "root cannot be modified");

        // Soft delete moves the whole subtree into the trash.
        let (status, _) = write(&state, "DELETE", &format!("/api/files/{parent_id}"), None).await;
        assert_eq!(status, StatusCode::NO_CONTENT);

        let (status, trash) = call(&state, "/api/trash").await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(
            trash["items"].as_array().unwrap().len(),
            1,
            "only the trash root is listed"
        );
        assert_eq!(trash["items"][0]["name"], "films");
        assert_eq!(trash["file_count"], 0, "directories hold no bytes");

        // The subtree is detached from the root listing...
        let (_, children) = call(&state, &format!("/api/files/{root}/children")).await;
        assert!(children["items"].as_array().unwrap().is_empty());

        // ...and restore brings it back under its original parent (the root).
        let (status, _) = write(
            &state,
            "POST",
            &format!("/api/trash/{parent_id}/restore"),
            None,
        )
        .await;
        assert_eq!(status, StatusCode::NO_CONTENT);
        let (_, children) = call(&state, &format!("/api/files/{root}/children")).await;
        assert_eq!(children["items"].as_array().unwrap().len(), 1);
        assert_eq!(children["items"][0]["name"], "films");

        // Restoring something that is not in the trash is a 404.
        let (status, body) = write(
            &state,
            "POST",
            &format!("/api/trash/{parent_id}/restore"),
            None,
        )
        .await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        assert_eq!(body["error"]["message"], "trash item not found");
    }

    #[tokio::test]
    async fn tasks_are_listed_with_a_derived_name_and_hidden_when_stale() {
        let state = state().await;
        let sql: &'static str = "INSERT INTO files(id,parent_id,name,kind,object_key,size,mime_type,status,created_at,updated_at) \
VALUES('f1','00000000-0000-0000-0000-000000000000','holiday.mp4','file','blobs/f1',10,'video/mp4','ready','2024-01-01T00:00:00Z','2024-01-01T00:00:00Z'); \
INSERT INTO tasks(id,type,status,phase,progress,speed,eta_seconds,retry_count,max_retries,error,source_type,source_id,cancel_requested,created_at,started_at,finished_at,updated_at) VALUES \
('t1','upload','running','uploading',42.5,1024,30,0,3,'','upload','u1',0,'2024-01-02T00:00:00Z','2024-01-02T00:00:01Z',NULL,'2024-01-02T00:00:02Z'), \
('t2','upload','completed','done',100,0,NULL,0,3,'','upload','u2',0,'2024-01-01T00:00:00Z','2024-01-01T00:00:01Z','2024-01-01T00:00:02Z','2024-01-01T00:00:02Z'), \
('t3','internal','queued','x',0,0,NULL,0,3,'','x','y',0,'2024-01-03T00:00:00Z',NULL,NULL,'2024-01-03T00:00:00Z'); \
INSERT INTO task_files(task_id,file_id,role) VALUES('t1','f1','input');";
        seed(&state, sql).await;

        let (status, body) = call(&state, "/api/tasks").await;
        assert_eq!(status, StatusCode::OK);
        let items = body["items"].as_array().unwrap();
        // t2 finished long ago and is filtered out; t3 is not a task-centre type.
        assert_eq!(items.len(), 1, "{items:?}");
        assert_eq!(items[0]["id"], "t1");
        assert_eq!(items[0]["type"], "upload");
        assert_eq!(items[0]["status"], "running");
        assert_eq!(items[0]["progress"], 42.5);
        assert_eq!(items[0]["eta_seconds"], 30);
        assert_eq!(items[0]["cancel_requested"], false);
        // The name comes from the joined input file.
        assert_eq!(items[0]["name"], "holiday.mp4");
        // A non-running task omits its optional fields rather than sending nulls.
        assert!(items[0].get("finished_at").is_none());

        let (status, body) = call(&state, "/api/tasks/t1").await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["id"], "t1");
        assert_eq!(body["started_at"], "2024-01-02T00:00:01Z");

        let (status, body) = call(&state, "/api/tasks/nope").await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        assert_eq!(body["error"]["message"], "task not found");
    }

    #[tokio::test]
    async fn audio_info_reports_stored_chapters_and_rejects_stale_analysis() {
        let state = state().await;
        let chapters = r#"[{"title":"Intro","start_ms":0,"end_ms":1500},{"title":"Main","start_ms":1500,"end_ms":12345}]"#;
        let sql: &'static str = Box::leak(format!(
            "INSERT INTO files(id,parent_id,name,kind,object_key,size,mime_type,etag,status,created_at,updated_at) \
VALUES('a1','00000000-0000-0000-0000-000000000000','song.flac','file','blobs/a1',10,'audio/flac','e1','ready','2024-01-01T00:00:00Z','2024-01-01T00:00:00Z'), \
('a2','00000000-0000-0000-0000-000000000000','other.flac','file','blobs/a2',10,'audio/flac','e2','ready','2024-01-01T00:00:00Z','2024-01-01T00:00:00Z'), \
('a3','00000000-0000-0000-0000-000000000000','plain.mp3','file','blobs/a3',10,'audio/mpeg','e3','ready','2024-01-01T00:00:00Z','2024-01-01T00:00:00Z'); \
INSERT INTO media_metadata(file_id,duration_ms,container,video_codec,audio_codec,width,height,bitrate,chapters_json,analyzed_at,frame_rate,video_profile,video_level,subtitles_json,source_etag,probe_version) VALUES \
('a1',12345,'flac','mjpeg','flac',0,0,0,'{}', '2024-01-01T00:00:00Z','','',0,'[]','e1',2), \
('a2',9999,'flac','','flac',0,0,0,'[]','2024-01-01T00:00:00Z','','',0,'[]','STALE',2);",
            chapters
        ).into_boxed_str());
        seed(&state, sql).await;

        // Current analysis: duration in seconds, chapters numbered from 1, and a
        // cover URL because a video stream is present.
        let (status, body) = call(&state, "/api/files/a1/audio").await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["duration"], 12.345);
        assert_eq!(body["has_cover"], true);
        assert_eq!(body["cover_url"], "/api/files/a1/thumbnail?v=e1");
        assert_eq!(body["chapters"].as_array().unwrap().len(), 2);
        assert_eq!(body["chapters"][0]["id"], 1);
        assert_eq!(body["chapters"][1]["title"], "Main");
        assert_eq!(body["chapters"][1]["end"], 12.345);

        // Analysis recorded against different bytes is stale and must not be
        // served as if it described the current file.
        let (status, body) = call(&state, "/api/files/a2/audio").await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        assert_eq!(body["error"]["message"], "audio metadata is not available");

        // No analysis at all: same answer.
        let (status, _) = call(&state, "/api/files/a3/audio").await;
        assert_eq!(status, StatusCode::NOT_FOUND);

        // Not audio at all.
        let sql: &'static str = "INSERT INTO files(id,parent_id,name,kind,object_key,size,mime_type,status,created_at,updated_at) \
VALUES('x1','00000000-0000-0000-0000-000000000000','notes.bin','file','blobs/x1',1,'application/octet-stream','ready','2024-01-01T00:00:00Z','2024-01-01T00:00:00Z');";
        seed(&state, sql).await;
        let (status, body) = call(&state, "/api/files/x1/audio").await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        assert_eq!(body["error"]["message"], "ready audio file not found");
    }

    #[tokio::test]
    async fn system_status_counts_trashed_bytes_separately_from_live_bytes() {
        let state = state().await;
        let sql: &'static str = "INSERT INTO files(id,parent_id,name,kind,object_key,size,mime_type,status,created_at,updated_at) \
VALUES('a','00000000-0000-0000-0000-000000000000','a.bin','file','blobs/a',100,'application/octet-stream','ready','2024-01-01T00:00:00Z','2024-01-01T00:00:00Z'), \
('b','00000000-0000-0000-0000-000000000000','b.bin','file','blobs/b',60,'application/octet-stream','ready','2024-01-01T00:00:00Z','2024-01-01T00:00:00Z'); \
UPDATE files SET deleted_at='2024-01-01T00:00:00Z', trash_root_id='b' WHERE id='b';";
        seed(&state, sql).await;

        let (status, body) = call(&state, "/api/system/status").await;
        assert_eq!(status, StatusCode::OK);
        // Storage figures deliberately include trash, unlike /storage/stats.
        assert_eq!(body["storage"]["bytes"], 160);
        assert_eq!(body["storage"]["trash_bytes"], 60);
        assert_eq!(body["storage"]["file_count"], 2);
        assert!(body["database"]["bytes"].as_i64().unwrap() > 0);
        // The cache layer does not exist yet, so it reports degraded — claiming
        // "ok" would assert a measurement nobody took — and degrades the whole
        // response, exactly as Go does with a nil cache.
        assert_eq!(body["cache"]["status"], "degraded");
        assert_eq!(body["status"], "degraded");

        // The live-bytes endpoint answers the other question and must exclude
        // the trashed file.
        let (_, live) = call(&state, "/api/storage/stats").await;
        assert_eq!(live["total_bytes"], 100);
        assert_eq!(live["file_count"], 1);
    }

    #[tokio::test]
    async fn sharing_creates_rotates_and_revokes_a_link() {
        let state = state().await;
        let sql: &'static str = "INSERT INTO files(id,parent_id,name,kind,object_key,size,mime_type,status,created_at,updated_at) \
VALUES('f1','00000000-0000-0000-0000-000000000000','a.bin','file','blobs/f1',3,'application/octet-stream','ready','2024-01-01T00:00:00Z','2024-01-01T00:00:00Z'), \
('d1','00000000-0000-0000-0000-000000000000','dir','directory',NULL,0,'','ready','2024-01-01T00:00:00Z','2024-01-01T00:00:00Z');";
        seed(&state, sql).await;

        // Not shared yet: the client renders the "create link" affordance from
        // this rather than from a 404.
        let (status, body) = call(&state, "/api/files/f1/share").await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body, serde_json::json!({"active": false}));

        let (status, created) = write(&state, "POST", "/api/files/f1/share", None).await;
        assert_eq!(status, StatusCode::CREATED);
        assert_eq!(created["active"], true);
        let url = created["url"].as_str().unwrap().to_owned();
        assert!(url.starts_with("http://localhost:8080/s/"), "{url}");
        let token = url.rsplit('/').next().unwrap().to_owned();
        // 32 random bytes as unpadded base64url.
        assert_eq!(token.len(), 43);
        assert!(
            token
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
        );

        // Re-sharing rotates the token, so a leaked link can be retired.
        let (_, again) = write(&state, "POST", "/api/files/f1/share", None).await;
        assert_ne!(again["url"], created["url"]);
        assert_eq!(again["active"], true);

        // A directory cannot be shared.
        let (status, body) = write(&state, "POST", "/api/files/d1/share", None).await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        assert_eq!(body["error"]["message"], "ready file not found");

        // Revoking is idempotent and returns the file to "not shared".
        let (status, _) = write(&state, "DELETE", "/api/files/f1/share", None).await;
        assert_eq!(status, StatusCode::NO_CONTENT);
        let (_, body) = call(&state, "/api/files/f1/share").await;
        assert_eq!(body, serde_json::json!({"active": false}));
        let (status, _) = write(&state, "DELETE", "/api/files/f1/share", None).await;
        assert_eq!(status, StatusCode::NO_CONTENT);
    }

    #[tokio::test]
    async fn media_progress_round_trips_and_refuses_impossible_values() {
        let state = state().await;
        let sql: &'static str = "INSERT INTO files(id,parent_id,name,kind,object_key,size,mime_type,status,created_at,updated_at) \
VALUES('v1','00000000-0000-0000-0000-000000000000','clip.mp4','file','blobs/v1',10,'video/mp4','ready','2024-01-01T00:00:00Z','2024-01-01T00:00:00Z'), \
('x1','00000000-0000-0000-0000-000000000000','notes.bin','file','blobs/x1',10,'application/octet-stream','ready','2024-01-01T00:00:00Z','2024-01-01T00:00:00Z');";
        seed(&state, sql).await;

        // Never played: zeroes, not a 404, so the player can call it blindly.
        let (status, body) = call(&state, "/api/files/v1/media/progress").await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["position"], 0.0);
        assert_eq!(body["duration"], 0.0);
        assert!(body.get("updated_at").is_none());

        let (status, saved) = write(
            &state,
            "PUT",
            "/api/files/v1/media/progress",
            Some(serde_json::json!({"position": 12.5, "duration": 100.0})),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(saved["position"], 12.5);
        assert_eq!(saved["duration"], 100.0);
        assert!(saved["updated_at"].is_string());

        // A save that does not know the duration must not erase the known one.
        let (status, saved) = write(
            &state,
            "PUT",
            "/api/files/v1/media/progress",
            Some(serde_json::json!({"position": 30.0, "duration": 0.0})),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(saved["position"], 30.0);
        assert_eq!(
            saved["duration"], 100.0,
            "a zero duration means unknown, not reset"
        );

        // Out-of-range and non-finite values are 400, never a 500 from the
        // column's CHECK constraint.
        for payload in [
            serde_json::json!({"position": -1.0, "duration": 10.0}),
            serde_json::json!({"position": 10.0, "duration": -1.0}),
            serde_json::json!({"position": 10.0, "duration": 700000.0}),
            serde_json::json!({"position": 20.0, "duration": 10.0}),
        ] {
            let (status, body) =
                write(&state, "PUT", "/api/files/v1/media/progress", Some(payload)).await;
            assert_eq!(status, StatusCode::BAD_REQUEST);
            assert_eq!(
                body["error"]["message"],
                "media progress values are invalid"
            );
        }

        // A non-media file is refused outright.
        let (status, body) = call(&state, "/api/files/x1/media/progress").await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        assert_eq!(body["error"]["message"], "ready media file not found");
    }

    #[tokio::test]
    async fn creating_a_document_stores_bytes_and_commits_a_ready_row() {
        let state = state().await;
        let root = ROOT_ID;
        let (status, created) = write(
            &state,
            "POST",
            "/api/documents",
            Some(serde_json::json!({"parent_id": root, "name": "todo.md", "content": "# hi\n"})),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED);
        assert_eq!(created["name"], "todo.md");
        assert_eq!(created["status"], "ready");
        assert_eq!(created["size"], 5);
        assert_eq!(created["mime_type"], "text/markdown; charset=utf-8");
        assert_eq!(created["content_hash"].as_str().unwrap().len(), 64);
        assert_eq!(created["hash_algorithm"], "sha256");

        // The bytes really landed in the object store, and the file is readable
        // back through the document endpoint.
        let id = created["id"].as_str().unwrap().to_owned();
        let (status, body) = call(&state, &format!("/api/files/{id}/content")).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["content"], "# hi\n");

        // A non-editable extension is refused before anything is written.
        let (status, body) = write(
            &state,
            "POST",
            "/api/documents",
            Some(serde_json::json!({"parent_id": root, "name": "photo.png", "content": "x"})),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(
            body["error"]["message"],
            "this file type cannot be edited as text"
        );

        // A vanished parent is a conflict, and no orphan blob is left behind.
        let before = std::fs::read_dir(state.store.root().join("blobs"))
            .map(|d| d.count())
            .unwrap_or(0);
        let (status, _) = write(
            &state,
            "POST",
            "/api/documents",
            Some(serde_json::json!({
                "parent_id": "0190f8f0-1c2b-7c3d-9e4f-5a6b7c8d9e0f",
                "name": "lost.md",
                "content": "x"
            })),
        )
        .await;
        assert_eq!(status, StatusCode::CONFLICT);
        let after = std::fs::read_dir(state.store.root().join("blobs"))
            .map(|d| d.count())
            .unwrap_or(0);
        assert_eq!(before, after, "a failed create must not leak a blob");
    }

    #[tokio::test]
    async fn copy_reuses_the_blob_and_suffixes_the_name() {
        let state = state().await;
        let root = ROOT_ID;
        let sql: &'static str = "INSERT INTO files(id,parent_id,name,kind,object_key,size,mime_type,etag,status,created_at,updated_at) \
VALUES('f1','00000000-0000-0000-0000-000000000000','notes.md','file','blobs/f1',5,'text/markdown','e1','ready','2024-01-01T00:00:00Z','2024-01-01T00:00:00Z');";
        seed(&state, sql).await;

        // The source name is taken at the root, so the copy is suffixed with the
        // product's Chinese marker rather than failing.
        let (status, first) = write(
            &state,
            "POST",
            "/api/files/f1/copy",
            Some(serde_json::json!({"parent_id": root})),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED);
        assert_eq!(first["name"], "notes - 副本.md");
        // A copy shares the object, so no bytes are duplicated.
        assert!(
            first.get("object_key").is_none(),
            "the object key must never be exposed"
        );

        let (status, second) = write(
            &state,
            "POST",
            "/api/files/f1/copy",
            Some(serde_json::json!({"parent_id": root})),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED);
        assert_eq!(second["name"], "notes - 副本 2.md");

        // Into a directory where the name is free, the original name is kept.
        let (_, dir) = write(
            &state,
            "POST",
            "/api/directories",
            Some(serde_json::json!({"parent_id": root, "name": "sub"})),
        )
        .await;
        let dir_id = dir["id"].as_str().unwrap().to_owned();
        let (status, third) = write(
            &state,
            "POST",
            "/api/files/f1/copy",
            Some(serde_json::json!({"parent_id": dir_id})),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED);
        assert_eq!(third["name"], "notes.md");

        // Copying onto an invalid target is a 400, and a missing source a 404.
        let (status, body) = write(
            &state,
            "POST",
            "/api/files/f1/copy",
            Some(serde_json::json!({"parent_id": "0190f8f0-1c2b-7c3d-9e4f-5a6b7c8d9e0f"})),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(body["error"]["message"], "target directory is invalid");

        let (status, body) = write(
            &state,
            "POST",
            "/api/files/0190f8f0-1c2b-7c3d-9e4f-5a6b7c8d9e0f/copy",
            Some(serde_json::json!({"parent_id": root})),
        )
        .await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        assert_eq!(body["error"]["message"], "ready file not found");
    }

    #[tokio::test]
    async fn restoring_onto_a_taken_name_reports_the_restore_location() {
        let state = state().await;
        let root = ROOT_ID;

        let (_, first) = write(
            &state,
            "POST",
            "/api/directories",
            Some(serde_json::json!({"parent_id": root, "name": "shared"})),
        )
        .await;
        let first_id = first["id"].as_str().unwrap().to_owned();

        let (status, _) = write(&state, "DELETE", &format!("/api/files/{first_id}"), None).await;
        assert_eq!(status, StatusCode::NO_CONTENT);

        // Occupy the name at the original location.
        let (status, _) = write(
            &state,
            "POST",
            "/api/directories",
            Some(serde_json::json!({"parent_id": root, "name": "shared"})),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED);

        // Restoring can no longer go back to the root under that name. The
        // message names the restore location specifically, which is the Go
        // wording and more actionable than a bare duplicate-name error.
        let (status, body) = write(
            &state,
            "POST",
            &format!("/api/trash/{first_id}/restore"),
            None,
        )
        .await;
        assert_eq!(status, StatusCode::CONFLICT);
        assert_eq!(
            body["error"]["message"],
            "an item with that name already exists at the restore location"
        );
    }

    #[tokio::test]
    async fn purge_and_empty_remove_rows_permanently() {
        let state = state().await;
        let sql: &'static str = Box::leak(
            format!(
                "{}{}",
                file_row(
                    "f1",
                    ROOT_ID,
                    "a.bin",
                    "file",
                    3,
                    "application/octet-stream"
                ),
                file_row(
                    "f2",
                    ROOT_ID,
                    "b.bin",
                    "file",
                    4,
                    "application/octet-stream"
                )
            )
            .into_boxed_str(),
        );
        seed(&state, sql).await;

        // Two soft-deleted files, then purge one and empty the rest.
        for id in ["f1", "f2"] {
            let (status, _) = write(&state, "DELETE", &format!("/api/files/{id}"), None).await;
            assert_eq!(status, StatusCode::NO_CONTENT);
        }
        let (_, trash) = call(&state, "/api/trash").await;
        assert_eq!(trash["items"].as_array().unwrap().len(), 2);
        assert_eq!(trash["total_bytes"], 7);
        assert_eq!(trash["file_count"], 2);

        let (status, _) = write(&state, "DELETE", "/api/trash/f1", None).await;
        assert_eq!(status, StatusCode::NO_CONTENT);
        let (_, trash) = call(&state, "/api/trash").await;
        assert_eq!(trash["items"].as_array().unwrap().len(), 1);

        let (status, _) = write(&state, "DELETE", "/api/trash", None).await;
        assert_eq!(status, StatusCode::NO_CONTENT);
        let (_, trash) = call(&state, "/api/trash").await;
        assert!(trash["items"].as_array().unwrap().is_empty());

        // Purging something already gone is a 404.
        let (status, body) = write(&state, "DELETE", "/api/trash/f1", None).await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        assert_eq!(body["error"]["message"], "trash item not found");
    }

    #[tokio::test]
    async fn has_cover_is_reported_for_audio_only() {
        let state = state().await;
        // Both rows have analysed metadata with a video stream (which is how an
        // embedded cover is represented), so only the audio guard distinguishes
        // them.
        let sql: &'static str = "INSERT INTO files(id,parent_id,name,kind,object_key,size,mime_type,etag,status,created_at,updated_at) VALUES \
('a1','00000000-0000-0000-0000-000000000000','song.flac','file','blobs/a1',10,'audio/flac','e1','ready','2024-01-01T00:00:00Z','2024-01-01T00:00:00Z'), \
('v1','00000000-0000-0000-0000-000000000000','clip.mp4','file','blobs/v1',20,'video/mp4','e2','ready','2024-01-01T00:00:00Z','2024-01-01T00:00:00Z'); \
INSERT INTO media_metadata(file_id,duration_ms,container,video_codec,audio_codec,width,height,bitrate,chapters_json,analyzed_at,frame_rate,video_profile,video_level,subtitles_json,source_etag,probe_version) VALUES \
('a1',1000,'flac','mjpeg','flac',0,0,0,'[]','2024-01-01T00:00:00Z','','',0,'[]','e1',2), \
('v1',2000,'mp4','h264','aac',1920,1080,0,'[]','2024-01-01T00:00:00Z','','',0,'[]','e2',2);";
        seed(&state, sql).await;

        let (status, body) = call(&state, &format!("/api/files/{ROOT_ID}/children")).await;
        assert_eq!(status, StatusCode::OK);
        let flags: Vec<(&str, bool)> = body["items"]
            .as_array()
            .unwrap()
            .iter()
            .map(|item| {
                (
                    item["name"].as_str().unwrap(),
                    item["has_cover"].as_bool().unwrap_or(false),
                )
            })
            .collect();
        let audio = flags
            .iter()
            .find(|(name, _)| *name == "song.flac")
            .unwrap()
            .1;
        let video = flags
            .iter()
            .find(|(name, _)| *name == "clip.mp4")
            .unwrap()
            .1;
        assert!(
            audio,
            "an audio file with embedded art must report has_cover"
        );
        assert!(
            !video,
            "a video must not advertise an audio cover, matching Go"
        );
    }

    #[tokio::test]
    async fn documents_round_trip_and_reject_stale_writes() {
        let state = state().await;
        // A stored blob plus the row that points at it.
        // Seed the row with the store's real ETag: without it the optimistic
        // concurrency check has nothing to compare against and a stale save
        // would be indistinguishable from a first one.
        let stored = state.store.put("blobs/doc1", b"hello").await.unwrap();
        let sql: &'static str = Box::leak(
            format!(
                "INSERT INTO files(id,parent_id,name,kind,object_key,size,mime_type,etag,status,created_at,updated_at) \
VALUES('doc1','00000000-0000-0000-0000-000000000000','notes.md','file','blobs/doc1',5,'text/markdown','{}','ready','2024-01-01T00:00:00Z','2024-01-01T00:00:00Z');",
                stored.etag
            )
            .into_boxed_str(),
        );
        seed(&state, sql).await;

        let (status, body) = call(&state, "/api/files/doc1/content").await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["content"], "hello");
        let etag = body["etag"].as_str().unwrap().to_owned();

        // A save with the ETag we read succeeds and returns the updated row.
        let (status, updated) = write(
            &state,
            "PUT",
            "/api/files/doc1/content",
            Some(serde_json::json!({"content": "world!", "etag": etag})),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(updated["size"], 6);
        assert_eq!(updated["content_hash"].as_str().unwrap().len(), 64);

        let (_, body) = call(&state, "/api/files/doc1/content").await;
        assert_eq!(body["content"], "world!");

        // A save carrying the *old* ETag is refused rather than silently
        // overwriting the newer content.
        let (status, body) = write(
            &state,
            "PUT",
            "/api/files/doc1/content",
            Some(serde_json::json!({"content": "stale", "etag": etag})),
        )
        .await;
        assert_eq!(status, StatusCode::CONFLICT);
        assert_eq!(
            body["error"]["message"],
            "document changed elsewhere; reopen it before saving"
        );

        // A binary file is not editable text.
        let sql: &'static str = "INSERT INTO files(id,parent_id,name,kind,object_key,size,status,created_at,updated_at) \
VALUES('bin1','00000000-0000-0000-0000-000000000000','photo.png','file','blobs/bin1',3,'ready','2024-01-01T00:00:00Z','2024-01-01T00:00:00Z');";
        seed(&state, sql).await;
        let (status, body) = call(&state, "/api/files/bin1/content").await;
        assert_eq!(status, StatusCode::UNSUPPORTED_MEDIA_TYPE);
        assert_eq!(
            body["error"]["message"],
            "this file type cannot be edited as text"
        );
    }

    #[tokio::test]
    async fn storage_stats_totals_ready_files_only() {
        let state = state().await;
        let sql: &'static str = Box::leak(
            format!(
                "{}{}{}",
                file_row(
                    "a",
                    ROOT_ID,
                    "a.bin",
                    "file",
                    10,
                    "application/octet-stream"
                ),
                file_row(
                    "b",
                    ROOT_ID,
                    "b.bin",
                    "file",
                    32,
                    "application/octet-stream"
                ),
                file_row("d", ROOT_ID, "dir", "directory", 0, "")
            )
            .into_boxed_str(),
        );
        seed(&state, sql).await;
        let (status, body) = call(&state, "/api/storage/stats").await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["total_bytes"], 42);
        assert_eq!(body["file_count"], 2);
    }

    #[tokio::test]
    async fn children_orders_directories_first_and_reports_stats() {
        let state = state().await;
        let sql: &'static str = Box::leak(
            format!(
                "{}{}{}",
                file_row("d1", ROOT_ID, "zdir", "directory", 0, ""),
                file_row(
                    "f1",
                    ROOT_ID,
                    "afile.bin",
                    "file",
                    7,
                    "application/octet-stream"
                ),
                file_row(
                    "f2",
                    ROOT_ID,
                    "bfile.bin",
                    "file",
                    5,
                    "application/octet-stream"
                )
            )
            .into_boxed_str(),
        );
        seed(&state, sql).await;
        let (status, body) = call(&state, &format!("/api/files/{ROOT_ID}/children")).await;
        assert_eq!(status, StatusCode::OK);
        let names: Vec<&str> = body["items"]
            .as_array()
            .unwrap()
            .iter()
            .map(|item| item["name"].as_str().unwrap())
            .collect();
        // `ORDER BY kind DESC` puts *files* first, because 'file' sorts after
        // 'directory'. This is the Go ordering, not a conventional
        // directories-first listing — the browser re-sorts if it wants to.
        assert_eq!(names, vec!["afile.bin", "bfile.bin", "zdir"]);
        assert_eq!(body["total_bytes"], 12);
        assert_eq!(body["file_count"], 2);
    }

    #[tokio::test]
    async fn a_missing_file_is_a_404() {
        let state = state().await;
        let (status, body) = call(&state, "/api/files/0190f8f0-1c2b-7c3d-9e4f-5a6b7c8d9e0f").await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        assert_eq!(body["error"]["message"], "file not found");
    }

    #[tokio::test]
    async fn a_file_id_is_not_a_directory() {
        let state = state().await;
        let sql: &'static str = Box::leak(
            file_row(
                "f1",
                ROOT_ID,
                "a.bin",
                "file",
                1,
                "application/octet-stream",
            )
            .into_boxed_str(),
        );
        seed(&state, sql).await;
        let (status, body) = call(&state, "/api/files/f1/children").await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        assert_eq!(body["error"]["message"], "directory not found");
    }

    #[tokio::test]
    async fn breadcrumbs_run_from_the_root_down_to_the_file_itself() {
        let state = state().await;
        let sql: &'static str = Box::leak(
            format!(
                "{}{}",
                file_row("d1", ROOT_ID, "movies", "directory", 0, ""),
                file_row("f1", "d1", "clip.mp4", "file", 3, "video/mp4")
            )
            .into_boxed_str(),
        );
        seed(&state, sql).await;
        let (status, body) = call(&state, "/api/files/f1").await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["file"]["name"], "clip.mp4");
        // The recursive CTE starts at the requested row and walks *up*, then the
        // result is ordered by depth descending — so the trail runs from the
        // root down to and including the file itself. Faithful to the Go query.
        let crumbs: Vec<&str> = body["breadcrumbs"]
            .as_array()
            .unwrap()
            .iter()
            .map(|c| c["name"].as_str().unwrap())
            .collect();
        assert_eq!(crumbs, vec!["", "movies", "clip.mp4"]);
    }

    #[tokio::test]
    async fn library_filters_by_type_and_counts_files_as_the_total() {
        let state = state().await;
        let sql: &'static str = Box::leak(
            format!(
                "{}{}{}",
                file_row("v1", ROOT_ID, "clip.mp4", "file", 3, "video/mp4"),
                file_row("i1", ROOT_ID, "photo.png", "file", 4, "image/png"),
                file_row(
                    "t1",
                    ROOT_ID,
                    "book.epub",
                    "file",
                    5,
                    "application/epub+zip"
                )
            )
            .into_boxed_str(),
        );
        seed(&state, sql).await;

        let (status, body) = call(&state, "/api/library?type=video").await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["type"], "video");
        assert_eq!(body["items"].as_array().unwrap().len(), 1);
        assert_eq!(body["items"][0]["name"], "clip.mp4");
        // `file` counts every ready file, not the unclassified leftovers.
        assert_eq!(body["counts"]["file"], 3);
        assert_eq!(body["counts"]["video"], 1);
        assert_eq!(body["counts"]["image"], 1);
        assert_eq!(body["counts"]["book"], 1);

        let (status, body) = call(&state, "/api/library?type=nonsense").await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(body["error"]["message"], "unknown library type");

        // The default bucket is `file`.
        let (status, body) = call(&state, "/api/library").await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["type"], "file");
        assert_eq!(body["items"].as_array().unwrap().len(), 3);

        let (status, body) = call(&state, "/api/library/all").await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["items"]["video"].as_array().unwrap().len(), 1);
        assert_eq!(body["items"]["audio"].as_array().unwrap().len(), 0);
        assert!(body["items"].get("file").is_none());

        let (status, body) = call(&state, "/api/library/counts").await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["file"], 3);
    }
}

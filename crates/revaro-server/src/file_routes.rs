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
use revaro_core::library as aggregation;
use revaro_core::model::{
    File, FileKind, FileStatus, FolderRef, LibraryCounts, LibraryItem, StorageStats,
};
use revaro_core::time::Timestamp;
use rusqlite::Row;

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
        .route("/library", get(library))
        .route("/library/all", get(library_all))
        .route("/library/counts", get(library_counts))
        .route("/directories", axum::routing::post(create_directory))
        .route(
            "/files/{id}",
            axum::routing::patch(patch_file).delete(delete_file),
        )
        .route("/trash", get(trash).delete(empty_trash))
        .route("/trash/{id}/restore", axum::routing::post(restore_trash))
        .route("/trash/{id}", axum::routing::delete(purge_trash))
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
            // authoritative aggregate and no recount is needed here.
            let (total_bytes, file_count) = connection
                .query_row(
                    "SELECT COALESCE(total_bytes,0), COALESCE(file_count,0) FROM directory_stats \
WHERE directory_id = ?1",
                    [&parent.id],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )
                .unwrap_or((0, 0));

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
        if covered.iter().any(|id| id == &item.id) {
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
                .map_err(|error| conflict_or(DbError::Query(error)))?;
            transaction
                .execute(
                    "UPDATE files SET deleted_at = NULL, restore_parent_id = NULL, trash_root_id = NULL \
WHERE trash_root_id = ?1",
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

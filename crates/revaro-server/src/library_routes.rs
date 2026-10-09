//! Content-library queries and virtual collections over the existing files.
mod query;
mod stacks;

use std::sync::Arc;

use axum::{
    Json, Router,
    extract::{Path, Query, State},
    routing::get,
};
use revaro_core::{
    ApiError, Timestamp,
    library::{Collection, CreateCollection, ItemUpdate, LibraryItem, LibraryListing},
};
use rusqlite::OptionalExtension;

use crate::{
    auth::extract::AuthUser,
    auth_routes::JsonBody,
    file_routes::{FILE_COLUMNS, scan_file},
    state::AppState,
};

#[derive(Default, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct Options {
    kind: Option<String>,
    #[serde(default)]
    q: String,
    #[serde(default)]
    favorite: bool,
    collection: Option<String>,
    stack: Option<String>,
    #[serde(default)]
    group_stacks: bool,
    #[serde(default)]
    recent: bool,
    /// Home history uses explicit opens, independently of background progress saves.
    #[serde(default)]
    opened_only: bool,
    #[serde(default)]
    offset: i64,
    limit: Option<i64>,
}

pub fn routes() -> Router<Arc<AppState>> {
    Router::new()
        .merge(stacks::routes())
        .route("/library/items", get(list))
        .route("/library/items/{id}", axum::routing::patch(update))
        .route(
            "/library/collections",
            get(collections).post(create_collection),
        )
        .route(
            "/library/collections/{id}",
            axum::routing::delete(delete_collection),
        )
        .route(
            "/library/collections/{id}/items/{file_id}",
            axum::routing::put(add_member).delete(remove_member),
        )
}

fn db(error: rusqlite::Error) -> ApiError {
    tracing::error!(%error, "library database operation failed");
    ApiError::internal("database error")
}

fn valid_kind(kind: &str) -> bool {
    matches!(kind, "book" | "audio" | "image" | "video")
}

async fn list(
    State(state): State<Arc<AppState>>,
    _user: AuthUser,
    Query(o): Query<Options>,
) -> Result<Json<LibraryListing>, ApiError> {
    let limit = o.limit.unwrap_or(60);
    if !(1..=200).contains(&limit)
        || o.offset < 0
        || o.q.len() > 512
        || o.kind.as_deref().is_some_and(|k| !valid_kind(k))
    {
        return Err(ApiError::bad_request("invalid library query"));
    }
    if o.stack
        .as_ref()
        .is_some_and(|s| s.is_empty() || s.len() > 128)
        || ((o.group_stacks || o.stack.is_some()) && o.kind.as_deref() != Some("book"))
    {
        return Err(ApiError::bad_request("invalid stack query"));
    }
    let grouped = o.group_stacks && o.stack.is_none();
    let offset = o.offset;
    let (total, rows) = state
        .db
        .call_api(move |connection| query::load_listing(connection, o, limit, grouped))
        .await?;
    let mut items = Vec::<LibraryItem>::new();
    let mut previous_bucket = String::new();
    let mut progress_sum = 0.0;
    for query::Candidate {
        mut item,
        progress,
        bucket,
        stack,
    } in rows
    {
        if item.kind == "book" {
            item.reading_progress = book_reading_progress(&state, &item.file, progress).await;
        }
        if grouped && stack.is_some() && previous_bucket == bucket {
            let group = items.last_mut().expect("previous group exists");
            progress_sum += item.reading_progress.unwrap_or(0.0);
            let stack = group.stack.as_mut().expect("stack group exists");
            stack.files.push(item.file);
            group.reading_progress =
                (progress_sum > 0.0).then(|| progress_sum / stack.files.len() as f64);
        } else {
            previous_bucket = bucket;
            progress_sum = item.reading_progress.unwrap_or(0.0);
            if grouped && let Some((id, name)) = stack {
                item.stack = Some(revaro_core::stacks::Stack {
                    id,
                    name,
                    files: vec![item.file.clone()],
                });
            }
            items.push(item);
        }
    }
    Ok(Json(LibraryListing {
        items,
        total,
        offset,
        limit,
    }))
}

/// Cache series metadata in the existing classification row, including negative results.
pub(crate) async fn index_book_series(state: &Arc<AppState>) -> Result<(), ApiError> {
    let files = state.db.call_api(|c| {
        let mut query = c.prepare(&format!("SELECT f.* FROM (SELECT {FILE_COLUMNS} FROM files) f JOIN library_items l ON l.file_id=f.id WHERE l.kind='book' AND f.status='ready' AND f.deleted_at IS NULL AND (l.metadata_etag IS NULL OR l.metadata_etag<>COALESCE(f.etag,'')) AND NOT EXISTS(SELECT 1 FROM book_metadata_retries r WHERE r.file_id=f.id AND r.source_etag=COALESCE(f.etag,'') AND r.retry_at>?1) ORDER BY f.id LIMIT 16")).map_err(db)?;
        query.query_map([Timestamp::now().to_rfc3339()], scan_file).map_err(db)?.collect::<Result<Vec<_>,_>>().map_err(db)
    }).await?;
    let full_batch = files.len() == 16;
    for file in files {
        let _guard = state.reader.book_lock(&file.object_key).await;
        let id = file.id.clone();
        let etag = file.etag.clone();
        let current = state
            .db
            .call_api(move |c| {
                Ok(c.query_row(
                    "SELECT metadata_etag= ?2 FROM library_items WHERE file_id=?1",
                    rusqlite::params![id, etag],
                    |r| r.get::<_, Option<bool>>(0),
                )
                .optional()
                .map_err(db)?
                .flatten()
                .unwrap_or(false))
            })
            .await?;
        if current {
            continue;
        }
        let metadata = if revaro_core::classify::is_epub_name(&file.name)
            && file.size <= revaro_reader::MAX_EPUB
        {
            let permit = Arc::clone(&state.reader.work_slots)
                .acquire_owned()
                .await
                .map_err(|_| ApiError::unavailable("reader is shutting down"))?;
            match state
                .store
                .read(&file.object_key, revaro_reader::MAX_EPUB as usize)
                .await
            {
                Ok(bytes) => tokio::task::spawn_blocking(move || {
                    let _permit = permit;
                    revaro_reader::read_series(std::io::Cursor::new(bytes))
                })
                .await
                .ok()
                .and_then(Result::ok)
                .flatten(),
                Err(error) => {
                    tracing::warn!(%error, file_id=%file.id, "could not read series metadata");
                    let retry_id = file.id.clone();
                    let retry_etag = file.etag.clone();
                    let retry_at =
                        Timestamp::from_unix_millis(Timestamp::now().unix_millis() + 60_000)
                            .to_rfc3339();
                    state.db.call_api(move |c| {
                        c.execute("INSERT INTO book_metadata_retries(file_id,source_etag,retry_at) VALUES(?1,?2,?3) ON CONFLICT(file_id) DO UPDATE SET source_etag=excluded.source_etag,retry_at=excluded.retry_at", rusqlite::params![retry_id,retry_etag,retry_at]).map_err(db)?;
                        Ok(())
                    }).await?;
                    continue;
                }
            }
        } else {
            None
        };
        let (series, index) = metadata
            .map(|(name, index)| (Some(name), index))
            .unwrap_or_default();
        state.db.call_api(move |c| {
            c.execute("DELETE FROM book_metadata_retries WHERE file_id=?1", [&file.id]).map_err(db)?;
            c.execute("UPDATE library_items SET series=?2,series_index=?3,metadata_etag=?4 WHERE file_id=?1 AND kind='book' AND EXISTS(SELECT 1 FROM files WHERE id=?1 AND COALESCE(etag,'')=?4 AND name=?5 AND object_key=?6)", rusqlite::params![file.id,series,index,file.etag,file.name,file.object_key]).map_err(db)?;
            Ok(())
        }).await?;
    }
    if full_batch {
        state.maintenance.wake("book-series");
    }
    Ok(())
}

async fn book_reading_progress(
    state: &Arc<AppState>,
    file: &revaro_core::model::File,
    raw: Option<String>,
) -> Option<f64> {
    let raw = raw?;
    let mut progress: revaro_core::api::book::Progress = serde_json::from_str(&raw).ok()?;
    let anchor = progress.anchor.as_ref().filter(|a| a.is_valid())?;
    let percent = match progress
        .percent
        .filter(|p| p.is_finite() && (0.0..=100.0).contains(p))
    {
        Some(percent) => percent,
        None => {
            let percent =
                crate::reader_routes::legacy_progress_percent(state.clone(), file, anchor).await?;
            progress.percent = Some(percent);
            let value = serde_json::to_string(&progress).ok()?;
            let key = format!("book_progress/{}", file.id);
            // Keep its timestamp and never overwrite a newer reader save.
            state
                .db
                .call_api(move |c| {
                    c.execute(
                        "UPDATE settings SET value=?1 WHERE key=?2 AND value=?3",
                        rusqlite::params![value, key, raw],
                    )
                    .map_err(db)?;
                    Ok(())
                })
                .await
                .ok()?;
            percent
        }
    };
    (percent > 0.0).then_some(percent)
}

async fn update(
    State(state): State<Arc<AppState>>,
    _user: AuthUser,
    Path(id): Path<String>,
    JsonBody(o): JsonBody<ItemUpdate>,
) -> Result<http::StatusCode, ApiError> {
    state.db.call_api(move |c| {
        crate::file_routes::lookup_file(c,&id).map_err(|_| ApiError::not_found("file not found"))?;
        c.execute("INSERT INTO library_state(file_id,favorite,last_opened) VALUES(?1,COALESCE(?2,0),?3) ON CONFLICT(file_id) DO UPDATE SET favorite=COALESCE(?2,library_state.favorite),last_opened=COALESCE(?3,library_state.last_opened)",rusqlite::params![id,o.favorite,o.opened.then(|| Timestamp::now().to_rfc3339())]).map_err(db)?;
        Ok(http::StatusCode::NO_CONTENT)
    }).await
}

async fn collections(
    State(state): State<Arc<AppState>>,
    _user: AuthUser,
) -> Result<Json<Vec<Collection>>, ApiError> {
    state.db.call_api(move |c| {
        let mut s=c.prepare("SELECT id,name,kind,(SELECT COUNT(*) FROM library_collection_items i JOIN files f ON f.id=i.file_id JOIN library_items l ON l.file_id=f.id WHERE i.collection_id=library_collections.id AND f.status='ready' AND f.deleted_at IS NULL AND l.kind=library_collections.kind) FROM library_collections ORDER BY created_at,id").map_err(db)?;
        s.query_map([],|r|Ok(Collection{id:r.get(0)?,name:r.get(1)?,kind:r.get(2)?,item_count:r.get(3)?})).map_err(db)?.collect::<Result<Vec<_>,_>>().map_err(db)
    }).await.map(Json)
}

async fn create_collection(
    State(state): State<Arc<AppState>>,
    _user: AuthUser,
    JsonBody(o): JsonBody<CreateCollection>,
) -> Result<Json<Collection>, ApiError> {
    let name = o.name.trim().to_owned();
    if name.is_empty() || name.chars().count() > 80 || !valid_kind(&o.kind) {
        return Err(ApiError::bad_request("集合名称应为 1–80 个字符"));
    }
    state
        .db
        .call_api(move |c| {
            let id = crate::ids::new_id();
            c.execute(
                "INSERT INTO library_collections(id,name,kind,created_at) VALUES(?1,?2,?3,?4)",
                rusqlite::params![id, name, o.kind, Timestamp::now().to_rfc3339()],
            )
            .map_err(db)?;
            Ok(Collection {
                id,
                name,
                kind: o.kind,
                item_count: 0,
            })
        })
        .await
        .map(Json)
}

async fn delete_collection(
    State(state): State<Arc<AppState>>,
    _user: AuthUser,
    Path(id): Path<String>,
) -> Result<http::StatusCode, ApiError> {
    state
        .db
        .call_api(move |c| {
            if c.execute("DELETE FROM library_collections WHERE id=?1", [id])
                .map_err(db)?
                == 0
            {
                return Err(ApiError::not_found("collection not found"));
            }
            Ok(http::StatusCode::NO_CONTENT)
        })
        .await
}

async fn add_member(
    State(state): State<Arc<AppState>>,
    _user: AuthUser,
    Path((id, file_id)): Path<(String, String)>,
) -> Result<http::StatusCode, ApiError> {
    state.db.call_api(move |c| {
        let tx=c.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate).map_err(db)?;
        let compatible:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM library_collections c JOIN library_items l ON l.kind=c.kind JOIN files f ON f.id=l.file_id WHERE c.id=?1 AND f.id=?2 AND f.deleted_at IS NULL AND f.status='ready')",rusqlite::params![id,file_id],|r|r.get(0)).map_err(db)?;
        if !compatible {return Err(ApiError::bad_request("文件与集合类型不匹配，或文件已删除"));}
        tx.execute("INSERT OR IGNORE INTO library_collection_items(collection_id,file_id,position) VALUES(?1,?2,COALESCE((SELECT MAX(position)+1 FROM library_collection_items WHERE collection_id=?1),0))",rusqlite::params![id,file_id]).map_err(db)?;
        tx.commit().map_err(db)?;
        Ok(http::StatusCode::NO_CONTENT)
    }).await
}

async fn remove_member(
    State(state): State<Arc<AppState>>,
    _user: AuthUser,
    Path((id, file_id)): Path<(String, String)>,
) -> Result<http::StatusCode, ApiError> {
    state
        .db
        .call_api(move |c| {
            let exists = c
                .query_row(
                    "SELECT id FROM library_collections WHERE id=?1",
                    [&id],
                    |r| r.get::<_, String>(0),
                )
                .optional()
                .map_err(db)?;
            if exists.is_none() {
                return Err(ApiError::not_found("collection not found"));
            }
            c.execute(
                "DELETE FROM library_collection_items WHERE collection_id=?1 AND file_id=?2",
                rusqlite::params![id, file_id],
            )
            .map_err(db)?;
            Ok(http::StatusCode::NO_CONTENT)
        })
        .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use http::{Request, StatusCode};
    use http_body_util::BodyExt;
    use tower::ServiceExt;

    async fn state() -> Arc<AppState> {
        let config = crate::config::Config::from_lookup(&|name| match name {
            "APP_BASE_URL" => Some("http://localhost:8080".to_owned()),
            "APP_WEB_DIR" => Some("/nonexistent-web-dir".to_owned()),
            _ => None,
        })
        .unwrap();
        let root = std::env::temp_dir().join(format!("revaro-library-{}", crate::ids::new_id()));
        let store = crate::storage::LocalStore::open(root).await.unwrap();
        let database = crate::db::Database::open_in_memory().unwrap();
        let auth = crate::auth::AuthService::new(database.clone());
        let state = AppState::new(Arc::new(config), database, store, auth);
        let hash = crate::auth::token_hash("library-test");
        state.db.call(move |c| {
            c.execute("INSERT INTO settings(key,value,updated_at) VALUES('admin_username','admin','2026-01-01T00:00:00Z')",[])?;
            c.execute("INSERT INTO sessions(id,token_hash,created_at,expires_at) VALUES('library-session',?1,'2026-01-01T00:00:00Z','2999-01-01T00:00:00Z')",[hash])?;
            Ok(())
        }).await.unwrap();
        state
    }

    #[tokio::test]
    async fn series_indexing_is_bounded_and_unreadable_books_do_not_block_the_queue() {
        let state = state().await;
        seed(&state, "000-bad", "unreadable.epub").await;
        for index in 0..18 {
            seed(
                &state,
                &format!("txt-{index:02}"),
                &format!("book-{index}.txt"),
            )
            .await;
        }
        let (status, _) = request(&state, "GET", "/api/library/stack-suggestions", None).await;
        assert_eq!(status, StatusCode::OK);
        let indexed = |c: &mut rusqlite::Connection| {
            c.query_row(
                "SELECT COUNT(*) FROM library_items WHERE metadata_etag IS NOT NULL",
                [],
                |r| r.get::<_, i64>(0),
            )
            .map_err(crate::db::DbError::Query)
        };
        assert_eq!(state.db.call(indexed).await.unwrap(), 0);
        index_book_series(&state).await.unwrap();
        assert_eq!(state.db.call(indexed).await.unwrap(), 15);
        index_book_series(&state).await.unwrap();
        assert_eq!(state.db.call(indexed).await.unwrap(), 18);
        assert_eq!(
            state
                .db
                .call(|c| c
                    .query_row("SELECT COUNT(*) FROM book_metadata_retries", [], |r| r
                        .get::<_, i64>(0))
                    .map_err(crate::db::DbError::Query))
                .await
                .unwrap(),
            1
        );
        std::fs::remove_dir_all(state.store.root()).unwrap();
    }

    #[tokio::test]
    async fn idle_series_indexing_does_not_rescan_every_book_for_each_file() {
        let state = state().await;
        state.db.call(|c| {
            let tx = c.transaction()?;
            for index in 0..5000 {
                let id = format!("indexed-{index:05}");
                tx.execute("INSERT INTO files(id,parent_id,name,kind,object_key,size,etag,status,created_at,updated_at) VALUES(?1,?2,?3,'file',?4,12,?1,'ready','2026-01-01T00:00:00Z','2026-01-01T00:00:00Z')",rusqlite::params![id,revaro_core::ids::ROOT_ID,format!("{id}.txt"),format!("blobs/{id}")])?;
            }
            tx.execute("UPDATE library_items SET metadata_etag=file_id",[])?;
            tx.commit()?;
            Ok(())
        }).await.unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(2), index_book_series(&state))
            .await
            .expect("an idle indexing pass must finish without a quadratic scan")
            .unwrap();
        std::fs::remove_dir_all(state.store.root()).unwrap();
    }

    async fn seed(state: &Arc<AppState>, id: &str, name: &str) {
        let id = id.to_owned();
        let name = name.to_owned();
        state.db.call(move |c| {
            c.execute("INSERT INTO files(id,parent_id,name,kind,object_key,size,status,created_at,updated_at) VALUES(?1,?2,?3,'file',?4,12,'ready','2026-01-01T00:00:00Z','2026-01-01T00:00:00Z')",rusqlite::params![id,revaro_core::ids::ROOT_ID,name,format!("blobs/{id}")])?;
            Ok(())
        }).await.unwrap();
    }

    async fn request(
        state: &Arc<AppState>,
        method: &str,
        path: &str,
        body: Option<serde_json::Value>,
    ) -> (StatusCode, serde_json::Value) {
        let bytes = body
            .map(|b| serde_json::to_vec(&b).unwrap())
            .unwrap_or_default();
        let response = crate::router::build(state.clone())
            .oneshot(
                Request::builder()
                    .method(method)
                    .uri(path)
                    .header(
                        "cookie",
                        format!("{}=library-test", crate::auth::SESSION_COOKIE),
                    )
                    .header("origin", "http://localhost:8080")
                    .header("content-type", "application/json")
                    .body(Body::from(bytes))
                    .unwrap(),
            )
            .await
            .unwrap();
        let status = response.status();
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        (status, serde_json::from_slice(&bytes).unwrap_or_default())
    }

    #[tokio::test]
    async fn recent_items_use_the_latest_activity_with_nanosecond_precision() {
        let state = state().await;
        for (kind, extension) in [("book", "txt"), ("audio", "wav")] {
            for index in 0..6 {
                seed(
                    &state,
                    &format!("{kind}-{index}"),
                    &format!("{kind}-{index}.{extension}"),
                )
                .await;
            }
        }
        state.db.call(|c| {
            for kind in ["book", "audio"] {
                for (index, timestamp) in [
                    (0, "2026-01-01T00:00:00Z"),
                    (1, "2026-01-02T00:00:00Z"),
                    (3, "2026-01-04T00:00:00.100000001Z"),
                    (4, "2026-01-04 00:00:01"),
                ] {
                    c.execute("INSERT INTO library_state(file_id,last_opened) VALUES(?1,?2)",rusqlite::params![format!("{kind}-{index}"),timestamp])?;
                }
                for (index, timestamp) in [(0,"2026-01-03T00:00:00Z"),(2,"2026-01-04T00:00:00.1Z")] {
                    let id = format!("{kind}-{index}");
                    if kind == "book" {
                        c.execute("INSERT INTO settings(key,value,updated_at) VALUES(?1,?2,?3)",rusqlite::params![format!("book_progress/{id}"),r#"{"anchor":{"spine":0,"block":1,"offset":0},"percent":50.0}"#,timestamp])?;
                    } else {
                        c.execute("INSERT INTO media_progress(file_id,position_ms,duration_ms,updated_at) VALUES(?1,1000,10000,?2)",rusqlite::params![id,timestamp])?;
                    }
                }
            }
            Ok(())
        }).await.unwrap();
        for kind in ["book", "audio"] {
            let (status, listing) = request(
                &state,
                "GET",
                &format!("/api/library/items?kind={kind}&recent=true"),
                None,
            )
            .await;
            assert_eq!(status, StatusCode::OK);
            assert_eq!(listing["total"], 5);
            let items = listing["items"].as_array().unwrap();
            let ids = items
                .iter()
                .map(|item| item["file"]["id"].as_str().unwrap())
                .collect::<Vec<_>>();
            assert_eq!(ids, [4, 3, 2, 0, 1].map(|i| format!("{kind}-{i}")));
            assert_eq!(items[1]["last_opened"], "2026-01-04T00:00:00.100000001Z");
            assert_eq!(items[2]["last_opened"], "2026-01-04T00:00:00.1Z");
            assert_eq!(items[3]["last_opened"], "2026-01-03T00:00:00Z");
        }
    }

    #[tokio::test]
    async fn opened_history_mixes_kinds_and_ignores_unopened_files_and_progress_saves() {
        let state = state().await;
        for (id, name) in [
            ("book", "book.txt"),
            ("audio", "song.wav"),
            ("image", "photo.png"),
            ("video", "clip.webm"),
            ("unopened", "new.png"),
            ("favorite", "favorite.txt"),
            ("progress-only", "progress.wav"),
            ("document", "notes.md"),
            ("trashed", "trashed.png"),
        ] {
            seed(&state, id, name).await;
        }
        state.db.call(|c| {
            for (id, timestamp) in [
                ("audio", "2026-01-01T00:00:00Z"),
                ("image", "2026-01-02T00:00:00.1Z"),
                ("video", "2026-01-02T00:00:00.100000001Z"),
                ("book", "2026-01-03T00:00:00Z"),
                ("document", "2026-01-04T00:00:00Z"),
                ("trashed", "2026-01-05T00:00:00Z"),
            ] {
                c.execute("INSERT INTO library_state(file_id,last_opened) VALUES(?1,?2)", rusqlite::params![id,timestamp])?;
            }
            c.execute("INSERT INTO library_state(file_id,favorite) VALUES('favorite',1)", [])?;
            for id in ["audio", "progress-only"] {
                c.execute("INSERT INTO media_progress(file_id,position_ms,duration_ms,updated_at) VALUES(?1,1000,10000,'2026-01-09T00:00:00Z')", [id])?;
            }
            c.execute("UPDATE files SET deleted_at='2026-01-06T00:00:00Z' WHERE id='trashed'", [])?;
            c.execute("INSERT INTO files(id,parent_id,name,kind,object_key,size,status,created_at,updated_at) VALUES('folder',?1,'folder.png','directory',NULL,0,'ready','2026-01-01T00:00:00Z','2026-01-01T00:00:00Z')", [revaro_core::ids::ROOT_ID])?;
            c.execute("INSERT INTO library_state(file_id,last_opened) VALUES('folder','2026-01-10T00:00:00Z')", [])?;
            Ok(())
        }).await.unwrap();
        let path = "/api/library/items?recent=true&opened_only=true";
        let (status, listing) = request(&state, "GET", path, None).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(listing["total"], 4);
        let ids = |listing: &serde_json::Value| {
            listing["items"]
                .as_array()
                .unwrap()
                .iter()
                .map(|item| item["file"]["id"].as_str().unwrap().to_owned())
                .collect::<Vec<_>>()
        };
        assert_eq!(ids(&listing), ["book", "video", "image", "audio"]);
        assert_eq!(listing["items"][3]["last_opened"], "2026-01-01T00:00:00Z");
        let (status, page) =
            request(&state, "GET", &format!("{path}&limit=2&offset=2"), None).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(page["total"], 4);
        assert_eq!(ids(&page), ["image", "audio"]);
        let (status, _) = request(
            &state,
            "PATCH",
            "/api/library/items/audio",
            Some(serde_json::json!({"opened":true})),
        )
        .await;
        assert_eq!(status, StatusCode::NO_CONTENT);
        let (_, listing) = request(&state, "GET", path, None).await;
        assert_eq!(ids(&listing), ["audio", "book", "video", "image"]);
    }

    fn file_ids(items: &serde_json::Value) -> Vec<&str> {
        items
            .as_array()
            .unwrap()
            .iter()
            .map(|i| i["id"].as_str().unwrap())
            .collect()
    }

    #[tokio::test]
    async fn manual_stacks_paginate_as_units_preserve_filters_and_ignore_series_grouping() {
        let state = state().await;
        for (id, name) in [
            ("a", "third.txt"),
            ("b", "first.txt"),
            ("c", "second.txt"),
            ("d", "standalone.txt"),
        ] {
            seed(&state, id, name).await;
        }
        state.db.call(|c| {
            c.execute("UPDATE library_items SET series='Saga',series_index=CASE file_id WHEN 'a' THEN 10 WHEN 'b' THEN 1 ELSE 2 END,metadata_etag=(SELECT COALESCE(etag,'') FROM files WHERE id=file_id) WHERE file_id IN ('a','b','c')", [])?;
            c.execute("INSERT INTO settings(key,value,updated_at) VALUES('book_progress/b',?1,'2026-01-01T00:00:00Z')", [r#"{"anchor":{"spine":0,"block":1,"offset":0},"percent":60.0}"#])?;
            Ok(())
        }).await.unwrap();
        let (_, listing) = request(
            &state,
            "GET",
            "/api/library/items?kind=book&group_stacks=true",
            None,
        )
        .await;
        assert_eq!(listing["total"], 4);
        assert!(
            listing["items"]
                .as_array()
                .unwrap()
                .iter()
                .all(|i| i["stack"].is_null())
        );
        let (_, suggestions) = request(&state, "GET", "/api/library/stack-suggestions", None).await;
        assert_eq!(
            suggestions,
            serde_json::json!([{"name":"Saga","file_ids":["b","c","a"]}])
        );
        assert_eq!(
            request(&state, "GET", "/api/library/stacks", None).await.1,
            serde_json::json!([])
        );

        let (status, stack) = request(
            &state,
            "POST",
            "/api/library/stacks",
            Some(serde_json::json!({"name":"Manual", "file_ids":["c","a","b"]})),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        let id = stack["id"].as_str().unwrap();
        assert_eq!(file_ids(&stack["files"]), ["c", "a", "b"]);
        let (_, page) = request(
            &state,
            "GET",
            "/api/library/items?kind=book&group_stacks=true&limit=1",
            None,
        )
        .await;
        let (_, page2) = request(
            &state,
            "GET",
            "/api/library/items?kind=book&group_stacks=true&limit=1&offset=1",
            None,
        )
        .await;
        assert_eq!(page["total"], 2);
        assert_eq!(page["items"].as_array().unwrap().len(), 1);
        assert_eq!(page2["items"].as_array().unwrap().len(), 1);
        let group = [&page["items"][0], &page2["items"][0]]
            .into_iter()
            .find(|i| i["stack"]["id"] == id)
            .unwrap();
        assert_eq!(group["file"]["id"], "c");
        assert_eq!(file_ids(&group["stack"]["files"]), ["c", "a", "b"]);
        assert_eq!(group["reading_progress"], 20.0);
        let (_, detail) = request(
            &state,
            "GET",
            &format!("/api/library/items?kind=book&stack={id}"),
            None,
        )
        .await;
        assert_eq!(detail["total"], 3);
        assert_eq!(
            detail["items"]
                .as_array()
                .unwrap()
                .iter()
                .map(|i| i["file"]["id"].as_str().unwrap())
                .collect::<Vec<_>>(),
            ["c", "a", "b"]
        );
        assert!(
            detail["items"]
                .as_array()
                .unwrap()
                .iter()
                .all(|i| i["stack"].is_null())
        );
        let (_, search) = request(
            &state,
            "GET",
            "/api/library/items?kind=book&group_stacks=true&q=first",
            None,
        )
        .await;
        assert_eq!(search["total"], 1);
        assert_eq!(file_ids(&search["items"][0]["stack"]["files"]), ["b"]);
        request(
            &state,
            "PATCH",
            "/api/library/items/b",
            Some(serde_json::json!({"favorite":true})),
        )
        .await;
        let (_, favorites) = request(
            &state,
            "GET",
            "/api/library/items?kind=book&group_stacks=true&favorite=true",
            None,
        )
        .await;
        assert_eq!(file_ids(&favorites["items"][0]["stack"]["files"]), ["b"]);
        let (_, shelf) = request(
            &state,
            "POST",
            "/api/library/collections",
            Some(serde_json::json!({"name":"Shelf", "kind":"book"})),
        )
        .await;
        let shelf_id = shelf["id"].as_str().unwrap();
        request(
            &state,
            "PUT",
            &format!("/api/library/collections/{shelf_id}/items/a"),
            None,
        )
        .await;
        let (_, shelf_listing) = request(
            &state,
            "GET",
            &format!("/api/library/items?kind=book&group_stacks=true&collection={shelf_id}"),
            None,
        )
        .await;
        assert_eq!(
            file_ids(&shelf_listing["items"][0]["stack"]["files"]),
            ["a"]
        );
        let (_, shelf_detail) = request(
            &state,
            "GET",
            &format!("/api/library/items?kind=book&stack={id}&collection={shelf_id}"),
            None,
        )
        .await;
        assert_eq!(shelf_detail["total"], 1);
        assert_eq!(shelf_detail["items"][0]["file"]["id"], "a");
        assert_eq!(
            request(
                &state,
                "GET",
                "/api/library/items?kind=image&group_stacks=true",
                None
            )
            .await
            .0,
            StatusCode::BAD_REQUEST
        );
    }

    #[tokio::test]
    async fn stack_changes_are_atomic_and_dissolving_preserves_books_shelves_and_metadata() {
        let state = state().await;
        for (id, name) in [
            ("a", "a.txt"),
            ("b", "b.txt"),
            ("c", "c.txt"),
            ("image", "image.png"),
        ] {
            seed(&state, id, name).await;
        }
        state
            .store
            .put("blobs/a", b"original book content")
            .await
            .unwrap();
        let (_, shelf) = request(
            &state,
            "POST",
            "/api/library/collections",
            Some(serde_json::json!({"name":"Books","kind":"book"})),
        )
        .await;
        let shelf_id = shelf["id"].as_str().unwrap();
        request(
            &state,
            "PUT",
            &format!("/api/library/collections/{shelf_id}/items/a"),
            None,
        )
        .await;
        request(
            &state,
            "PATCH",
            "/api/library/items/a",
            Some(serde_json::json!({"favorite":true})),
        )
        .await;
        let before = request(&state, "GET", "/api/library/items?kind=book", None)
            .await
            .1;
        let shelves_before = request(&state, "GET", "/api/library/collections", None)
            .await
            .1;
        let (status, stack) = request(
            &state,
            "POST",
            "/api/library/stacks",
            Some(serde_json::json!({"name":"Original","file_ids":["a","b"]})),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        let id = stack["id"].as_str().unwrap();
        let path = format!("/api/library/stacks/{id}");
        for invalid in [
            serde_json::json!({"name":"Invalid","file_ids":["a","image"]}),
            serde_json::json!({"name":"Invalid","file_ids":["a","a"]}),
            serde_json::json!({"name":" ","file_ids":["a","b"]}),
            serde_json::json!({"name":"Invalid","file_ids":["a"]}),
            serde_json::json!({"name":"Invalid","file_ids":["a","missing"]}),
            serde_json::json!({"name":"Invalid","file_ids":["a","b"],"series":"Saga"}),
        ] {
            assert_eq!(
                request(&state, "POST", "/api/library/stacks", Some(invalid))
                    .await
                    .0,
                StatusCode::BAD_REQUEST
            );
        }
        assert_eq!(
            request(
                &state,
                "POST",
                &format!("{path}/items"),
                Some(serde_json::json!({"file_ids":["c","image"]}))
            )
            .await
            .0,
            StatusCode::BAD_REQUEST
        );
        let (_, all) = request(&state, "GET", "/api/library/stacks", None).await;
        assert_eq!(all.as_array().unwrap().len(), 1);
        assert_eq!(file_ids(&all[0]["files"]), ["a", "b"]);
        assert_eq!(
            request(
                &state,
                "PATCH",
                &path,
                Some(serde_json::json!({"name":"Renamed"}))
            )
            .await
            .0,
            StatusCode::NO_CONTENT
        );
        assert_eq!(
            request(
                &state,
                "POST",
                &format!("{path}/items"),
                Some(serde_json::json!({"file_ids":["c","a"]}))
            )
            .await
            .0,
            StatusCode::NO_CONTENT
        );
        assert_eq!(
            request(
                &state,
                "PUT",
                &format!("{path}/order"),
                Some(serde_json::json!({"file_ids":["b","a"]}))
            )
            .await
            .0,
            StatusCode::BAD_REQUEST
        );
        assert_eq!(
            request(
                &state,
                "PUT",
                &format!("{path}/order"),
                Some(serde_json::json!({"file_ids":["c","b","a"]}))
            )
            .await
            .0,
            StatusCode::NO_CONTENT
        );
        let (_, all) = request(&state, "GET", "/api/library/stacks", None).await;
        assert_eq!(all[0]["name"], "Renamed");
        assert_eq!(file_ids(&all[0]["files"]), ["c", "b", "a"]);
        request(
            &state,
            "DELETE",
            &format!("{path}/items"),
            Some(serde_json::json!({"file_ids":["b"]})),
        )
        .await;
        let (_, moved) = request(
            &state,
            "POST",
            "/api/library/stacks",
            Some(serde_json::json!({"name":"Moved","file_ids":["b","c"]})),
        )
        .await;
        let (_, all) = request(&state, "GET", "/api/library/stacks", None).await;
        let original = all
            .as_array()
            .unwrap()
            .iter()
            .find(|s| s["id"] == id)
            .unwrap();
        assert_eq!(file_ids(&original["files"]), ["a"]);
        assert_eq!(file_ids(&moved["files"]), ["b", "c"]);
        assert_eq!(
            request(&state, "DELETE", &path, None).await.0,
            StatusCode::NO_CONTENT
        );
        request(
            &state,
            "DELETE",
            &format!("/api/library/stacks/{}", moved["id"].as_str().unwrap()),
            None,
        )
        .await;
        assert_eq!(
            request(&state, "GET", "/api/library/items?kind=book", None)
                .await
                .1,
            before
        );
        assert_eq!(
            request(&state, "GET", "/api/library/collections", None)
                .await
                .1,
            shelves_before
        );
        assert_eq!(
            state.store.read("blobs/a", 1024).await.unwrap(),
            b"original book content"
        );
    }

    #[tokio::test]
    async fn unavailable_members_are_hidden_and_membership_survives_trash_restore() {
        let state = state().await;
        seed(&state, "a", "a.txt").await;
        seed(&state, "b", "b.txt").await;
        seed(&state, "c", "c.txt").await;
        let (_, stack) = request(
            &state,
            "POST",
            "/api/library/stacks",
            Some(serde_json::json!({"name":"Saved","file_ids":["a","b","c"]})),
        )
        .await;
        state
            .db
            .call(|c| {
                c.execute(
                    "UPDATE files SET deleted_at='2026-01-01T00:00:00Z' WHERE id='b'",
                    [],
                )?;
                Ok(())
            })
            .await
            .unwrap();
        let (_, all) = request(&state, "GET", "/api/library/stacks", None).await;
        assert_eq!(file_ids(&all[0]["files"]), ["a", "c"]);
        let (_, listing) = request(
            &state,
            "GET",
            "/api/library/items?kind=book&group_stacks=true",
            None,
        )
        .await;
        assert_eq!(file_ids(&listing["items"][0]["stack"]["files"]), ["a", "c"]);
        assert_eq!(
            request(
                &state,
                "POST",
                &format!(
                    "/api/library/stacks/{}/items",
                    stack["id"].as_str().unwrap()
                ),
                Some(serde_json::json!({"file_ids":["b"]}))
            )
            .await
            .0,
            StatusCode::BAD_REQUEST
        );
        assert_eq!(
            request(
                &state,
                "PUT",
                &format!(
                    "/api/library/stacks/{}/order",
                    stack["id"].as_str().unwrap()
                ),
                Some(serde_json::json!({"file_ids":["c","a"]}))
            )
            .await
            .0,
            StatusCode::NO_CONTENT
        );
        state
            .db
            .call(|c| {
                c.execute("UPDATE files SET deleted_at=NULL WHERE id='b'", [])?;
                Ok(())
            })
            .await
            .unwrap();
        assert_eq!(
            file_ids(&request(&state, "GET", "/api/library/stacks", None).await.1[0]["files"]),
            ["c", "b", "a"]
        );
    }

    #[tokio::test]
    async fn legacy_progress_is_recovered_without_changing_recent_order_or_newer_saves() {
        let state = state().await;
        seed(&state, "legacy", "legacy.txt").await;
        let text = "a long reading line😀\n".repeat(1200);
        state
            .store
            .put("blobs/legacy", text.as_bytes())
            .await
            .unwrap();
        state.db.call(|c| {
            c.execute("INSERT INTO settings(key,value,updated_at) VALUES('book_progress/legacy',?1,'2026-01-01T00:00:00Z')", [r#"{"anchor":{"spine":0,"block":1,"offset":0}}"#])?;
            Ok(())
        }).await.unwrap();
        let (status, listing) = request(&state, "GET", "/api/library/items?kind=book", None).await;
        assert_eq!(status, StatusCode::OK);
        assert!(listing["items"][0]["reading_progress"].as_f64().unwrap() > 0.0);
        state
            .db
            .call(|c| {
                let (value, updated): (String, String) = c.query_row(
                    "SELECT value,updated_at FROM settings WHERE key='book_progress/legacy'",
                    [],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )?;
                assert!(
                    serde_json::from_str::<serde_json::Value>(&value).unwrap()["percent"]
                        .as_f64()
                        .unwrap()
                        > 0.0
                );
                assert_eq!(updated, "2026-01-01T00:00:00Z");
                Ok(())
            })
            .await
            .unwrap();
        let (status, _) = request(
            &state,
            "PUT",
            "/api/files/legacy/book/progress",
            Some(serde_json::json!({"anchor":{"spine":0,"block":1,"offset":0},"percent":101})),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn classification_index_matches_supported_extensions_and_upload_commit() {
        let state = state().await;
        for (kind, extensions) in [
            ("book", revaro_core::classify::BOOK_EXTENSIONS),
            ("audio", revaro_core::classify::AUDIO_EXTENSIONS),
            ("image", revaro_core::classify::IMAGE_EXTENSIONS),
            ("video", revaro_core::classify::VIDEO_EXTENSIONS),
        ] {
            for extension in extensions {
                seed(
                    &state,
                    &format!("{kind}-{extension}"),
                    &format!("fixture.{}", extension.to_uppercase()),
                )
                .await;
            }
            let (status, list) = request(
                &state,
                "GET",
                &format!("/api/library/items?kind={kind}"),
                None,
            )
            .await;
            assert_eq!(status, StatusCode::OK);
            assert_eq!(list["total"], extensions.len());
        }
        seed(&state, "pending", "pending.png").await;
        state
            .db
            .call(|c| {
                c.execute("UPDATE files SET status='pending' WHERE id='pending'", [])?;
                Ok(())
            })
            .await
            .unwrap();
        assert!(
            request(
                &state,
                "GET",
                "/api/library/items?kind=image&q=pending",
                None
            )
            .await
            .1["items"]
                .as_array()
                .unwrap()
                .is_empty()
        );
        state
            .db
            .call(|c| {
                c.execute("UPDATE files SET status='ready' WHERE id='pending'", [])?;
                Ok(())
            })
            .await
            .unwrap();
        assert_eq!(
            request(
                &state,
                "GET",
                "/api/library/items?kind=image&q=pending",
                None
            )
            .await
            .1["total"],
            1
        );
        seed(&state, "unsupported", "photo.bmp").await;
        seed(&state, "dotfile", ".epub").await;
        assert_eq!(
            request(&state, "GET", "/api/library/items?q=.epub", None)
                .await
                .1["total"],
            1
        );
    }

    #[tokio::test]
    async fn collections_and_favorites_follow_the_file_lifecycle() {
        let state = state().await;
        seed(&state, "photo", "photo.png").await;
        seed(&state, "song", "song.mp3").await;
        let (status, collection) = request(
            &state,
            "POST",
            "/api/library/collections",
            Some(serde_json::json!({"kind":"image","name":"My album"})),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        let id = collection["id"].as_str().unwrap();
        assert_eq!(
            request(
                &state,
                "PUT",
                &format!("/api/library/collections/{id}/items/song"),
                None
            )
            .await
            .0,
            StatusCode::BAD_REQUEST
        );
        for _ in 0..2 {
            assert_eq!(
                request(
                    &state,
                    "PUT",
                    &format!("/api/library/collections/{id}/items/photo"),
                    None
                )
                .await
                .0,
                StatusCode::NO_CONTENT
            );
        }
        assert_eq!(
            request(
                &state,
                "PATCH",
                "/api/library/items/photo",
                Some(serde_json::json!({"favorite":true,"opened":true}))
            )
            .await
            .0,
            StatusCode::NO_CONTENT
        );
        assert_eq!(
            request(
                &state,
                "GET",
                &format!("/api/library/items?collection={id}&favorite=true"),
                None
            )
            .await
            .1["total"],
            1
        );
        state.db.call(|c|{c.execute("UPDATE files SET name='renamed.png',deleted_at='2026-02-01T00:00:00Z' WHERE id='photo'",[])?;Ok(())}).await.unwrap();
        assert_eq!(
            request(&state, "GET", "/api/library/items?favorite=true", None)
                .await
                .1["total"],
            0
        );
        state
            .db
            .call(|c| {
                c.execute("UPDATE files SET deleted_at=NULL WHERE id='photo'", [])?;
                Ok(())
            })
            .await
            .unwrap();
        assert_eq!(
            request(
                &state,
                "GET",
                &format!("/api/library/items?collection={id}"),
                None
            )
            .await
            .1["items"][0]["file"]["name"],
            "renamed.png"
        );
        assert_eq!(
            request(
                &state,
                "DELETE",
                &format!("/api/library/collections/{id}"),
                None
            )
            .await
            .0,
            StatusCode::NO_CONTENT
        );
        assert_eq!(
            request(&state, "GET", "/api/library/items?favorite=true", None)
                .await
                .1["total"],
            1
        );
        state
            .db
            .call(|c| {
                c.execute("DELETE FROM files WHERE id='photo'", [])?;
                let states: i64 = c.query_row(
                    "SELECT COUNT(*) FROM library_state WHERE file_id='photo'",
                    [],
                    |r| r.get(0),
                )?;
                assert_eq!(states, 0);
                Ok(())
            })
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn audio_uses_categories_and_rejects_stack_operations() {
        let state = state().await;
        seed(&state, "audio-1", "01.flac").await;
        seed(&state, "audio-2", "02.flac").await;
        let (status, _) = request(
            &state,
            "POST",
            "/api/library/stacks",
            Some(serde_json::json!({"name":"Audio", "file_ids":["audio-1","audio-2"]})),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        let (status, _) = request(
            &state,
            "GET",
            "/api/library/items?kind=audio&group_stacks=true",
            None,
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        let (status, collection) = request(
            &state,
            "POST",
            "/api/library/collections",
            Some(serde_json::json!({"name":"ASMR", "kind":"audio"})),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        let id = collection["id"].as_str().unwrap();
        for file in ["audio-1", "audio-2"] {
            assert_eq!(
                request(
                    &state,
                    "PUT",
                    &format!("/api/library/collections/{id}/items/{file}"),
                    None
                )
                .await
                .0,
                StatusCode::NO_CONTENT
            );
        }
        let (status, listing) = request(
            &state,
            "GET",
            &format!("/api/library/items?kind=audio&collection={id}"),
            None,
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(listing["total"], 2);
        assert!(
            listing["items"]
                .as_array()
                .unwrap()
                .iter()
                .all(|item| item["stack"].is_null())
        );
    }

    #[tokio::test]
    async fn library_search_is_paginated_and_requires_authentication() {
        let state = state().await;
        for i in 0..73 {
            seed(
                &state,
                &format!("photo-{i:03}"),
                &format!("photo-{i:03}.png"),
            )
            .await;
        }
        let first = request(&state, "GET", "/api/library/items?kind=image", None)
            .await
            .1;
        let last = request(
            &state,
            "GET",
            "/api/library/items?kind=image&offset=60",
            None,
        )
        .await
        .1;
        assert_eq!(first["total"], 73);
        assert_eq!(first["items"].as_array().unwrap().len(), 60);
        assert_eq!(last["items"].as_array().unwrap().len(), 13);
        assert_eq!(
            request(&state, "GET", "/api/library/items?kind=invalid", None)
                .await
                .0,
            StatusCode::BAD_REQUEST
        );
        let response = crate::router::build(state)
            .oneshot(
                Request::builder()
                    .uri("/api/library/items")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn indexed_pages_match_enriched_pages_with_equal_dates_and_open_history() {
        let state = state().await;
        for index in 0..73 {
            seed(
                &state,
                &format!("page-{index:03}"),
                &format!("page-{index:03}.png"),
            )
            .await;
        }
        state
            .db
            .call(|c| {
                for index in 0..30 {
                    let time = if index % 2 == 0 {
                        "2026-01-01 00:00:00"
                    } else {
                        "2026-01-01T00:00:00.1Z"
                    };
                    c.execute(
                        "INSERT INTO library_state(file_id,last_opened) VALUES(?1,?2)",
                        rusqlite::params![format!("page-{index:03}"), time],
                    )?;
                }
                Ok(())
            })
            .await
            .unwrap();
        for history in ["", "&recent=true&opened_only=true"] {
            for offset in [0, 13, 60, 80] {
                let path =
                    format!("/api/library/items?kind=image&limit=13&offset={offset}{history}");
                let indexed = request(&state, "GET", &path, None).await;
                let enriched = request(&state, "GET", &format!("{path}&q=page"), None).await;
                assert_eq!(indexed.0, StatusCode::OK);
                assert_eq!(indexed, enriched, "offset={offset} history={history}");
            }
        }
        std::fs::remove_dir_all(state.store.root()).unwrap();
    }
}

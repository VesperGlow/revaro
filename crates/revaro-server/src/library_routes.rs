//! Content-library queries and virtual collections over the existing files.
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
    series: Option<String>,
    #[serde(default)]
    group_series: bool,
    #[serde(default)]
    recent: bool,
    #[serde(default)]
    offset: i64,
    limit: Option<i64>,
}

pub fn routes() -> Router<Arc<AppState>> {
    Router::new()
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
    if o.series
        .as_ref()
        .is_some_and(|s| s.is_empty() || s.len() > 2048)
        || ((o.group_series || o.series.is_some()) && o.kind.as_deref() != Some("book"))
    {
        return Err(ApiError::bad_request("invalid series query"));
    }
    if o.kind.as_deref().is_none_or(|kind| kind == "book") {
        index_book_series(&state).await?;
    }
    let grouped = o.group_series && o.series.is_none();
    let offset = o.offset;
    let (total, rows) = state.db.call_api(move |c| {
        let predicate = "status='ready' AND deleted_at IS NULL AND id IN (SELECT file_id FROM library_items WHERE (?1 IS NULL OR kind=?1)) AND (instr(lower(name),lower(?2))>0 OR EXISTS(SELECT 1 FROM library_items WHERE file_id=files.id AND metadata_etag=COALESCE(files.etag,'') AND instr(lower(series),lower(?2))>0)) AND (?3=0 OR id IN (SELECT file_id FROM library_state WHERE favorite=1)) AND (?4 IS NULL OR id IN (SELECT file_id FROM library_collection_items WHERE collection_id=?4)) AND (?5=0 OR id IN (SELECT file_id FROM library_state WHERE last_opened IS NOT NULL) OR EXISTS(SELECT 1 FROM settings WHERE key='book_progress/'||files.id) OR id IN (SELECT file_id FROM media_progress)) AND (?6 IS NULL OR id IN (SELECT file_id FROM library_items WHERE series=?6))";
        let series = "(SELECT series FROM library_items WHERE file_id=files.id AND kind='book' AND metadata_etag=COALESCE(files.etag,''))";
        let bucket = format!("CASE WHEN ?7 AND {series} IS NOT NULL THEN 'series:'||{series} ELSE 'file:'||id END");
        let last = "COALESCE((SELECT last_opened FROM library_state WHERE file_id=files.id),(SELECT updated_at FROM settings WHERE key='book_progress/'||files.id),(SELECT updated_at FROM media_progress WHERE file_id=files.id))";
        let params = rusqlite::params![o.kind,o.q,o.favorite,o.collection,o.recent,o.series,grouped];
        let count = if grouped { format!("COUNT(DISTINCT ({bucket}))") } else { "COUNT(*)".to_owned() };
        let count_params = if grouped { params } else { &params[..6] };
        let total = c.query_row(&format!("SELECT {count} FROM files WHERE {predicate}"), count_params, |r| r.get::<_,i64>(0)).map_err(db)?;
        let (sort, aggregate, direction) = if o.series.is_some() {
            ("COALESCE((SELECT series_index FROM library_items WHERE file_id=files.id),1e308)".to_owned(), "MIN", "ASC")
        } else if o.collection.is_some() {
            ("(SELECT position FROM library_collection_items WHERE collection_id=?4 AND file_id=files.id)".to_owned(), "MIN", "ASC")
        } else if o.recent {
            (last.to_owned(), "MAX", "DESC")
        } else {
            ("created_at".to_owned(), "MAX", "DESC")
        };
        let fields = format!("{FILE_COLUMNS},(SELECT kind FROM library_items WHERE file_id=files.id) AS library_kind,COALESCE((SELECT favorite FROM library_state WHERE file_id=files.id),0) AS favorite,{last} AS last_opened,EXISTS(SELECT 1 FROM media_metadata m WHERE m.file_id=files.id AND m.source_etag=files.etag AND m.video_codec<>'') AS has_cover,(SELECT duration_ms FROM media_metadata m WHERE m.file_id=files.id AND m.source_etag=files.etag) AS duration_ms,{series} AS series,(SELECT series_index FROM library_items WHERE file_id=files.id AND metadata_etag=COALESCE(files.etag,'')) AS series_index,(SELECT value FROM settings WHERE key='book_progress/'||files.id) AS progress");
        // Page groups first, then fetch their members. Other listings keep ordinary file pagination.
        let sql = if grouped {
            format!("WITH candidates AS (SELECT {fields},{bucket} AS bucket,{sort} AS sort_key FROM files WHERE {predicate}), groups AS (SELECT bucket,{aggregate}(sort_key) AS sort_key FROM candidates GROUP BY bucket ORDER BY sort_key {direction},bucket LIMIT ?8 OFFSET ?9) SELECT candidates.* FROM candidates JOIN groups USING(bucket) ORDER BY groups.sort_key {direction},groups.bucket,candidates.series_index ASC NULLS LAST,candidates.name,candidates.id")
        } else {
            format!("SELECT {fields},'file:'||id AS bucket FROM files WHERE {predicate} ORDER BY {sort} {direction},CASE WHEN ?6 IS NOT NULL THEN name END,id LIMIT ?8 OFFSET ?9")
        };
        let mut query = c.prepare(&sql).map_err(db)?;
        let rows = query.query_map(rusqlite::params![o.kind,o.q,o.favorite,o.collection,o.recent,o.series,grouped,limit,offset], |row| {
            let mut file = scan_file(row)?;
            file.has_cover = row.get(18)?;
            Ok((LibraryItem {
                file, duration_ms: row.get(19)?, kind: row.get(15)?, favorite: row.get(16)?,
                last_opened: row.get::<_,Option<String>>(17)?.and_then(|s| Timestamp::parse(&s).ok()),
                reading_progress: None, series: row.get(20)?, series_index: row.get(21)?, series_files: Vec::new(),
            }, row.get::<_,Option<String>>(22)?, row.get::<_,String>(23)?))
        }).map_err(db)?.collect::<Result<Vec<_>,_>>().map_err(db)?;
        Ok((total, rows))
    }).await?;
    let mut items = Vec::<LibraryItem>::new();
    let mut previous_bucket = String::new();
    let mut progress_sum = 0.0;
    for (mut item, raw, bucket) in rows {
        if item.kind == "book" {
            item.reading_progress = book_reading_progress(&state, &item.file, raw).await;
        }
        if grouped && item.series.is_some() && previous_bucket == bucket {
            let group = items.last_mut().expect("previous group exists");
            progress_sum += item.reading_progress.unwrap_or(0.0);
            group.series_files.push(item.file);
            group.reading_progress =
                (progress_sum > 0.0).then(|| progress_sum / group.series_files.len() as f64);
        } else {
            previous_bucket = bucket;
            progress_sum = item.reading_progress.unwrap_or(0.0);
            if grouped && item.series.is_some() {
                item.series_files.push(item.file.clone());
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
async fn index_book_series(state: &Arc<AppState>) -> Result<(), ApiError> {
    let files = state.db.call_api(|c| {
        let mut query = c.prepare(&format!("SELECT {FILE_COLUMNS} FROM files WHERE status='ready' AND deleted_at IS NULL AND id IN (SELECT file_id FROM library_items WHERE kind='book' AND (metadata_etag IS NULL OR metadata_etag<>COALESCE(files.etag,'')))")).map_err(db)?;
        query.query_map([], scan_file).map_err(db)?.collect::<Result<Vec<_>,_>>().map_err(db)
    }).await?;
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
            let _permit = state
                .reader
                .work_slots
                .acquire()
                .await
                .map_err(|_| ApiError::unavailable("reader is shutting down"))?;
            match state
                .store
                .read(&file.object_key, revaro_reader::MAX_EPUB as usize)
                .await
            {
                Ok(bytes) => tokio::task::spawn_blocking(move || {
                    revaro_reader::read_series(std::io::Cursor::new(bytes))
                })
                .await
                .ok()
                .and_then(Result::ok)
                .flatten(),
                Err(error) => {
                    tracing::warn!(%error, file_id=%file.id, "could not read series metadata");
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
            c.execute("UPDATE library_items SET series=?2,series_index=?3,metadata_etag=?4 WHERE file_id=?1 AND kind='book' AND EXISTS(SELECT 1 FROM files WHERE id=?1 AND COALESCE(etag,'')=?4 AND name=?5 AND object_key=?6)", rusqlite::params![file.id,series,index,file.etag,file.name,file.object_key]).map_err(db)?;
            Ok(())
        }).await?;
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
    async fn series_pages_keep_members_together_and_details_sort_volumes() {
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
        let (status, page) = request(
            &state,
            "GET",
            "/api/library/items?kind=book&group_series=true&limit=1",
            None,
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(page["total"], 2);
        assert_eq!(page["items"].as_array().unwrap().len(), 1);
        let (_, page2) = request(
            &state,
            "GET",
            "/api/library/items?kind=book&group_series=true&limit=1&offset=1",
            None,
        )
        .await;
        let group = [&page["items"][0], &page2["items"][0]]
            .into_iter()
            .find(|i| i["series"] == "Saga")
            .unwrap();
        assert_eq!(group["file"]["id"], "b");
        assert_eq!(group["series_files"].as_array().unwrap().len(), 3);
        assert_eq!(group["reading_progress"], 20.0);
        let (_, detail) = request(
            &state,
            "GET",
            "/api/library/items?kind=book&series=Saga",
            None,
        )
        .await;
        assert_eq!(detail["total"], 3);
        let ids = detail["items"]
            .as_array()
            .unwrap()
            .iter()
            .map(|i| i["file"]["id"].as_str().unwrap())
            .collect::<Vec<_>>();
        assert_eq!(ids, ["b", "c", "a"]);
        assert!(
            detail["items"][0]["series_files"]
                .as_array()
                .unwrap()
                .is_empty()
        );
        assert!(detail["items"][1]["reading_progress"].is_null());
        let (_, search) = request(
            &state,
            "GET",
            "/api/library/items?kind=book&group_series=true&q=Saga",
            None,
        )
        .await;
        assert_eq!(search["total"], 1);
        assert_eq!(
            search["items"][0]["series_files"].as_array().unwrap().len(),
            3
        );
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
            "/api/library/items?kind=book&group_series=true&favorite=true",
            None,
        )
        .await;
        assert_eq!(
            favorites["items"][0]["series_files"]
                .as_array()
                .unwrap()
                .len(),
            1
        );
        state
            .db
            .call(|c| {
                c.execute(
                    "UPDATE files SET deleted_at='2026-01-02T00:00:00Z' WHERE id='c'",
                    [],
                )?;
                Ok(())
            })
            .await
            .unwrap();
        let (_, detail) = request(
            &state,
            "GET",
            "/api/library/items?kind=book&series=Saga",
            None,
        )
        .await;
        assert_eq!(detail["total"], 2);
        assert_eq!(
            request(
                &state,
                "GET",
                "/api/library/items?kind=image&group_series=true",
                None
            )
            .await
            .0,
            StatusCode::BAD_REQUEST
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
}

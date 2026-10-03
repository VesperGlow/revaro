//! Bounded directory listing and literal filename search.
use crate::{
    auth::extract::AuthUser,
    file_routes::{FILE_COLUMNS, lookup_file, scan_file},
    state::AppState,
};
use axum::{
    Json, Router,
    extract::{Query, State},
    routing::get,
};
use revaro_core::ids::ROOT_ID;
use revaro_core::{ApiError, features::Listing, model::FileKind};
use std::sync::Arc;
#[derive(Default, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct Options {
    parent_id: Option<String>,
    #[serde(default)]
    q: String,
    #[serde(default)]
    sort: String,
    #[serde(default)]
    descending: bool,
    #[serde(default)]
    offset: i64,
    limit: Option<i64>,
}
pub fn routes() -> Router<Arc<AppState>> {
    Router::new().route("/files", get(list))
}
fn db(e: rusqlite::Error) -> ApiError {
    tracing::error!(%e,"listing failed");
    ApiError::internal("database error")
}
async fn list(
    State(state): State<Arc<AppState>>,
    _user: AuthUser,
    Query(mut o): Query<Options>,
) -> Result<Json<Listing>, ApiError> {
    let limit = o.limit.unwrap_or(100);
    if !(1..=200).contains(&limit) || o.offset < 0 || o.q.len() > 512 {
        return Err(ApiError::bad_request("invalid listing bounds"));
    }
    let order = match o.sort.as_str() {
        "" | "name" => "name COLLATE NOCASE",
        "size" => "size",
        "updated" => "updated_at",
        _ => return Err(ApiError::bad_request("invalid sort")),
    };
    let direction = if o.descending { "DESC" } else { "ASC" };
    o.q = o.q.trim().to_owned();
    state.db.call_api(move |c| {
        let id = o.parent_id.as_deref().unwrap_or(ROOT_ID);
        let directory = lookup_file(c, id).map_err(|_|ApiError::not_found("directory not found"))?;
        if directory.kind != FileKind::Directory {
            return Err(ApiError::not_found("directory not found"));
        }
        let (total_bytes, file_count) = c.query_row(
            "SELECT total_bytes,file_count FROM directory_stats WHERE directory_id=?1",
            [id], |r| Ok((r.get(0)?, r.get(1)?)),
        ).map_err(db)?;
        // "My files" searches the whole drive. Other directories search only
        // their immediate children; no independent client-side scope exists.
        let parent = if id == ROOT_ID && !o.q.is_empty() { None } else { Some(id) };
        let predicate="deleted_at IS NULL AND parent_id IS NOT NULL AND (?1 IS NULL OR parent_id=?1) AND instr(lower(name),lower(?2))>0";
        let total=c.query_row(&format!("SELECT count(*) FROM files WHERE {predicate}"),rusqlite::params![parent,o.q],|r|r.get(0)).map_err(db)?;
        let mut s=c.prepare(&format!("SELECT {FILE_COLUMNS},EXISTS(SELECT 1 FROM media_metadata m WHERE m.file_id=files.id AND m.source_etag=files.etag AND m.video_codec<>'') FROM files WHERE {predicate} ORDER BY kind ASC,{order} {direction},id LIMIT ?3 OFFSET ?4")).map_err(db)?;
        let items=s.query_map(rusqlite::params![parent,o.q,limit,o.offset],|row|{let mut file=scan_file(row)?;file.has_cover=revaro_core::classify::is_audio(&file) && row.get::<_,bool>(15)?;Ok(file)}).map_err(db)?.collect::<Result<Vec<_>,_>>().map_err(db)?;
        Ok(Listing{items,total,offset:o.offset,limit,total_bytes,file_count})
    }).await.map(Json)
}

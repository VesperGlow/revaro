//! Immutable document snapshots, retained across saves and restoration.
use crate::{
    auth::extract::AuthUser, auth_routes::JsonBody, file_routes::lookup_file, state::AppState,
};
use axum::{
    Json, Router,
    extract::{Path, State},
    routing::get,
};
use revaro_core::{
    ApiError, Timestamp,
    features::{DocumentVersion, RestoreVersionRequest},
    model::{File, FileKind, FileStatus},
};
use rusqlite::{Connection, params};
use std::sync::Arc;
fn db(e: rusqlite::Error) -> ApiError {
    tracing::error!(%e,"version query failed");
    ApiError::internal("database error")
}
pub fn routes() -> Router<Arc<AppState>> {
    Router::new()
        .route("/files/{id}/versions", get(list))
        .route("/files/{id}/versions/{version}/content", get(content))
        .route(
            "/files/{id}/versions/{version}/restore",
            axum::routing::post(restore),
        )
}
fn require(c: &Connection, id: &str) -> Result<File, ApiError> {
    let f = lookup_file(c, id).map_err(|_| ApiError::not_found("document not found"))?;
    if f.kind != FileKind::File
        || f.status != FileStatus::Ready
        || !revaro_core::classify::is_editable_name(&f.name)
    {
        return Err(ApiError::not_found("document not found"));
    }
    Ok(f)
}
/// Called inside the same transaction as the save; pruning uses the cleanup trigger.
pub(crate) fn snapshot(c: &Connection, f: &File) -> Result<(), ApiError> {
    c.execute("INSERT INTO document_versions(id,file_id,object_key,size,etag,content_hash,mime_type,created_at) VALUES(?1,?2,?3,?4,?5,?6,?7,?8)",
        params![crate::ids::new_id(),f.id,f.object_key,f.size,f.etag,f.content_hash,f.mime_type,Timestamp::now().to_rfc3339()]).map_err(db)?;
    c.execute("DELETE FROM document_versions WHERE file_id=?1 AND id NOT IN (SELECT id FROM document_versions WHERE file_id=?1 ORDER BY created_at DESC,id DESC LIMIT 20)",[&f.id]).map_err(db)?;
    Ok(())
}
async fn list(
    State(s): State<Arc<AppState>>,
    _u: AuthUser,
    Path(id): Path<String>,
) -> Result<Json<Vec<DocumentVersion>>, ApiError> {
    s.db.call_api(move |c| {require(c,&id)?;
        let mut q=c.prepare("SELECT id,size,created_at FROM document_versions WHERE file_id=?1 ORDER BY created_at DESC,id DESC LIMIT 20").map_err(db)?;
        let rows=q.query_map([id],|r|Ok((r.get(0)?,r.get(1)?,r.get::<_,String>(2)?))).map_err(db)?;
        rows.map(|r|{let(id,size,t)=r.map_err(db)?;Ok(DocumentVersion{id,size,created_at:Timestamp::parse(&t).map_err(|_|ApiError::internal("invalid version timestamp"))?})}).collect::<Result<Vec<_>,ApiError>>()
    }).await.map(Json)
}
async fn content(
    State(s): State<Arc<AppState>>,
    _u: AuthUser,
    Path((id, v)): Path<(String, String)>,
) -> Result<Json<String>, ApiError> {
    let key =
        s.db.call_api(move |c| {
            require(c, &id)?;
            c.query_row(
                "SELECT object_key FROM document_versions WHERE file_id=?1 AND id=?2",
                params![id, v],
                |r| r.get::<_, String>(0),
            )
            .map_err(|_| ApiError::not_found("version not found"))
        })
        .await?;
    let b = s
        .store
        .read(&key, revaro_core::limits::MAX_DOCUMENT_BYTES)
        .await
        .map_err(|_| ApiError::new(502, "version read failed"))?;
    Ok(Json(String::from_utf8(b).map_err(|_| {
        ApiError::unsupported_media_type("version is not UTF-8 text")
    })?))
}
async fn restore(
    State(s): State<Arc<AppState>>,
    _u: AuthUser,
    Path((id, v)): Path<(String, String)>,
    JsonBody(input): JsonBody<RestoreVersionRequest>,
) -> Result<Json<File>, ApiError> {
    if input.etag.is_empty() {
        return Err(ApiError::bad_request("current document etag is required"));
    }
    // Validate that the snapshot blob is available before changing references.
    let key =
        s.db.call_api({
            let id = id.clone();
            let v = v.clone();
            move |c| {
                require(c, &id)?;
                c.query_row(
                    "SELECT object_key FROM document_versions WHERE file_id=?1 AND id=?2",
                    params![id, v],
                    |r| r.get::<_, String>(0),
                )
                .map_err(|_| ApiError::not_found("version not found"))
            }
        })
        .await?;
    s.store
        .head(&key)
        .await
        .map_err(|_| ApiError::new(502, "version object unavailable"))?;
    s.db.call_api(move|c|{
        let tx=c.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate).map_err(db)?;
        let f=require(&tx,&id)?;
        if f.etag!=input.etag {return Err(ApiError::conflict("document changed elsewhere; reopen it before restoring"));}
        let (key,size,etag,hash,mime)=tx.query_row("SELECT object_key,size,etag,content_hash,mime_type FROM document_versions WHERE file_id=?1 AND id=?2",params![id,v],|r|Ok((r.get::<_,String>(0)?,r.get::<_,i64>(1)?,r.get::<_,String>(2)?,r.get::<_,String>(3)?,r.get::<_,String>(4)?))).map_err(|_|ApiError::not_found("version not found"))?;
        snapshot(&tx,&f)?;
        // A new revision tag prevents a stale editor from winning after an undo/redo cycle.
        let etag=format!("{etag}-{}",crate::ids::new_id());
        tx.execute("UPDATE files SET object_key=?2,size=?3,etag=?4,content_hash=?5,mime_type=?6,updated_at=?7 WHERE id=?1",params![id,key,size,etag,hash,mime,Timestamp::now().to_rfc3339()]).map_err(db)?;
        let out=lookup_file(&tx,&id).map_err(|_|ApiError::not_found("document not found"))?;
        tx.commit().map_err(db)?;Ok(out)
    }).await.map(Json)
}

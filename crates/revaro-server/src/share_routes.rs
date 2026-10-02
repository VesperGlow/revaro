//! Expiring public links and authenticated link management.
use crate::{auth::extract::AuthUser, file_routes::lookup_file, state::AppState};
use axum::{
    Json, Router,
    body::Bytes,
    extract::{Path, State},
    routing::get,
};
use revaro_core::{
    ApiError, Timestamp,
    features::{ShareEntry, ShareRequest},
    model::{FileKind, FileStatus, ShareStatus},
};
use rusqlite::{OptionalExtension, params};
use std::sync::Arc;

pub fn routes() -> Router<Arc<AppState>> {
    Router::new()
        .route("/files/{id}/share", get(status).post(create).delete(revoke))
        .route("/shares", get(list))
}
fn db(error: rusqlite::Error) -> ApiError {
    tracing::error!(%error,"share query failed");
    ApiError::internal("database error")
}
fn decode(
    base: &str,
    token: String,
    created: String,
    expires: Option<String>,
) -> Result<ShareStatus, ApiError> {
    let expires_at = expires
        .map(|s| Timestamp::parse(&s))
        .transpose()
        .map_err(|_| ApiError::internal("invalid share timestamp"))?;
    Ok(ShareStatus {
        active: expires_at.is_none_or(|t| t > Timestamp::now()),
        url: Some(format!("{base}/s/{token}")),
        created_at: Some(
            Timestamp::parse(&created)
                .map_err(|_| ApiError::internal("invalid share timestamp"))?,
        ),
        expires_at,
    })
}
async fn status(
    State(state): State<Arc<AppState>>,
    _user: AuthUser,
    Path(id): Path<String>,
) -> Result<Json<ShareStatus>, ApiError> {
    let base = state.config.base_url.clone();
    state.db.call_api(move |c| {
        let row = c.query_row("SELECT s.token,s.created_at,s.expires_at FROM shares s JOIN files f ON f.id=s.file_id WHERE s.file_id=?1 AND f.deleted_at IS NULL AND f.status='ready'", [&id],
            |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?))).optional().map_err(db)?;
        row.map_or_else(|| Ok(ShareStatus::default()), |(t,c,e)| decode(&base,t,c,e))
    }).await.map(Json)
}
async fn create(
    State(state): State<Arc<AppState>>,
    _user: AuthUser,
    Path(id): Path<String>,
    bytes: Bytes,
) -> Result<(http::StatusCode, Json<ShareStatus>), ApiError> {
    let input: ShareRequest = if bytes.is_empty() {
        ShareRequest::default()
    } else {
        serde_json::from_slice::<Option<ShareRequest>>(&bytes)
            .map_err(|_| crate::error::invalid_json())?
            .unwrap_or_default()
    };
    let now = Timestamp::now();
    let expires_at = match input.expires_in_seconds {
        None => None,
        Some(seconds) if (1..=365 * 86400).contains(&seconds) => Some(Timestamp::from_unix_millis(
            now.unix_millis() + seconds * 1000,
        )),
        _ => {
            return Err(ApiError::bad_request(
                "share expiry must be between 1 second and 365 days",
            ));
        }
    };
    use base64::Engine as _;
    let token = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(rand::random::<[u8; 32]>());
    let status = ShareStatus {
        active: true,
        url: Some(format!("{}/s/{token}", state.config.base_url)),
        created_at: Some(now),
        expires_at,
    };
    state.db.call_api(move |c| {
        let tx = c.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate).map_err(db)?;
        let file = lookup_file(&tx,&id).map_err(|_| ApiError::not_found("ready file not found"))?;
        if file.kind != FileKind::File || file.status != FileStatus::Ready {return Err(ApiError::not_found("ready file not found"));}
        tx.execute("INSERT INTO shares(file_id,token,created_at,expires_at) VALUES(?1,?2,?3,?4) ON CONFLICT(file_id) DO UPDATE SET token=excluded.token,created_at=excluded.created_at,expires_at=excluded.expires_at",
            params![id,token,now.to_rfc3339(),expires_at.map(|t|t.to_rfc3339())]).map_err(db)?;
        tx.commit().map_err(db)?; Ok(())
    }).await?;
    Ok((http::StatusCode::CREATED, Json(status)))
}
async fn revoke(
    State(state): State<Arc<AppState>>,
    _user: AuthUser,
    Path(id): Path<String>,
) -> Result<http::StatusCode, ApiError> {
    state
        .db
        .call_api(move |c| {
            c.execute("DELETE FROM shares WHERE file_id=?1", [id])
                .map_err(db)?;
            Ok(())
        })
        .await?;
    Ok(http::StatusCode::NO_CONTENT)
}
#[derive(serde::Deserialize, Default)]
struct Page {
    #[serde(default)]
    offset: i64,
}
async fn list(
    State(state): State<Arc<AppState>>,
    _user: AuthUser,
    axum::extract::Query(page): axum::extract::Query<Page>,
) -> Result<Json<Vec<ShareEntry>>, ApiError> {
    if page.offset < 0 {
        return Err(ApiError::bad_request("invalid offset"));
    }
    let base = state.config.base_url.clone();
    state.db.call_api(move |c| {
        let mut q=c.prepare("SELECT f.id,f.name,s.token,s.created_at,s.expires_at FROM shares s JOIN files f ON f.id=s.file_id WHERE f.deleted_at IS NULL AND f.status='ready' ORDER BY s.created_at DESC,s.file_id LIMIT 200 OFFSET ?1").map_err(db)?;
        let rows=q.query_map([page.offset],|r| Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?))).map_err(db)?;
        rows.map(|r| {let(id,name,t,c,e)=r.map_err(db)?; Ok(ShareEntry {file_id:id,name,status:decode(&base,t,c,e)?})}).collect::<Result<Vec<_>,ApiError>>()
    }).await.map(Json)
}

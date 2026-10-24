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
    features::{ShareEntry, ShareRequest, valid_share_slug},
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
type GameSettings = (String, Option<String>, Option<String>, Option<String>);

fn decode(
    base: &str,
    token: String,
    created: String,
    expires: Option<String>,
    custom_slug: Option<String>,
    (kind, engine, entry, password_hash): GameSettings,
) -> Result<ShareStatus, ApiError> {
    let expires_at = expires
        .map(|s| Timestamp::parse(&s))
        .transpose()
        .map_err(|_| ApiError::internal("invalid share timestamp"))?;
    Ok(ShareStatus {
        active: expires_at.is_none_or(|t| t > Timestamp::now()),
        url: Some(format!(
            "{base}/{}/{token}",
            if kind == "galgame" { "g" } else { "s" }
        )),
        share_type: (kind == "galgame").then_some(kind),
        engine,
        entry,
        password_protected: password_hash.map(|_| true),
        custom_slug,
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
        let row = c.query_row("SELECT s.token,s.created_at,s.expires_at,s.custom_slug,s.share_type,s.engine,s.entry,s.password_hash FROM shares s CROSS JOIN files f ON f.id=s.file_id WHERE s.file_id=?1 AND f.deleted_at IS NULL AND f.status='ready'", [&id],
            |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,(r.get(4)?,r.get(5)?,r.get(6)?,r.get(7)?)))).optional().map_err(db)?;
        row.map_or_else(|| Ok(ShareStatus::default()), |(t,c,e,s,g)| decode(&base,t,c,e,s,g))
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
    let kind = input.share_type.as_deref().unwrap_or("file");
    if !matches!(kind, "file" | "galgame") {
        return Err(ApiError::bad_request("unsupported share type"));
    }
    let is_game = kind == "galgame";
    let (engine, entry) = if is_game {
        if input.engine.as_deref() != Some("kirikiri2") {
            return Err(ApiError::bad_request("only kirikiri2 is supported"));
        }
        let entry = input
            .entry
            .filter(|v| !v.is_empty())
            .ok_or_else(|| ApiError::bad_request("XP3 entry is required"))?;
        crate::galgame_routes::validate_entry(&entry)?;
        (Some("kirikiri2".to_owned()), Some(entry))
    } else {
        if input.engine.is_some() || input.entry.is_some() || input.password.is_some() {
            return Err(ApiError::bad_request(
                "game settings require a Galgame share",
            ));
        }
        (None, None)
    };
    let password_update = input.password.is_some();
    let password_hash = if let Some(password) = input.password.filter(|p| !p.is_empty()) {
        if password.len() > 256 {
            return Err(ApiError::bad_request("share password is too long"));
        }
        Some(
            crate::auth::with_kdf(move || {
                crate::auth::hash_password(&password, crate::auth::DEFAULT_PARAMS)
            })
            .await
            .map_err(|_| ApiError::internal("could not hash share password"))?,
        )
    } else {
        None
    };
    let custom_slug = input.custom_slug.filter(|slug| !slug.is_empty());
    if custom_slug
        .as_deref()
        .is_some_and(|slug| !valid_share_slug(slug))
    {
        return Err(ApiError::bad_request(
            "链接后缀须为 1–64 位英文字母、数字、短横线或下划线，且不能使用系统保留路径",
        ));
    }
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
    let token = custom_slug.clone().unwrap_or_else(|| {
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(rand::random::<[u8; 32]>())
    });
    let base = state.config.base_url.clone();
    let kind = kind.to_owned();
    let status = state.db.call_api(move |c| {
        let tx = c.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate).map_err(db)?;
        let file = lookup_file(&tx,&id).map_err(|_| ApiError::not_found("ready file not found"))?;
        if file.kind != FileKind::File || file.status != FileStatus::Ready {return Err(ApiError::not_found("ready file not found"));}
        if is_game && !file.name.to_ascii_lowercase().ends_with(".zip") {
            return Err(ApiError::bad_request("Galgame shares require a complete game ZIP"));
        }
        let previous: Option<String> = tx.query_row("SELECT password_hash FROM shares WHERE file_id=?1 AND share_type='galgame'", [&id], |r| r.get(0)).optional().map_err(db)?.flatten();
        let password_hash = if is_game && !password_update { previous } else { password_hash };
        let taken: bool = tx.query_row("SELECT EXISTS(SELECT 1 FROM shares WHERE token=?1 AND file_id<>?2)", params![token,id], |row| row.get(0)).map_err(db)?;
        if taken {
            return Err(ApiError::conflict("这个链接后缀已被使用，请换一个"));
        }
        tx.execute("INSERT INTO shares(file_id,token,created_at,expires_at,custom_slug,share_type,engine,entry,password_hash) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9) ON CONFLICT(file_id) DO UPDATE SET token=excluded.token,created_at=excluded.created_at,expires_at=excluded.expires_at,custom_slug=excluded.custom_slug,share_type=excluded.share_type,engine=excluded.engine,entry=excluded.entry,password_hash=excluded.password_hash",
            params![id,token,now.to_rfc3339(),expires_at.map(|t|t.to_rfc3339()),custom_slug,kind,engine,entry,password_hash]).map_err(db)?;
        tx.commit().map_err(db)?;
        decode(&base,token,now.to_rfc3339(),expires_at.map(|t|t.to_rfc3339()),custom_slug,(kind,engine,entry,password_hash))
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
        let mut q=c.prepare("SELECT f.id,f.name,s.token,s.created_at,s.expires_at,s.custom_slug,s.share_type,s.engine,s.entry,s.password_hash FROM shares s CROSS JOIN files f ON f.id=s.file_id WHERE f.deleted_at IS NULL AND f.status='ready' ORDER BY s.created_at DESC,s.file_id LIMIT 200 OFFSET ?1").map_err(db)?;
        let rows=q.query_map([page.offset],|r| Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?,r.get(5)?,(r.get(6)?,r.get(7)?,r.get(8)?,r.get(9)?)))).map_err(db)?;
        rows.map(|r| {let(id,name,t,c,e,s,g)=r.map_err(db)?; Ok(ShareEntry {file_id:id,name,status:decode(&base,t,c,e,s,g)?})}).collect::<Result<Vec<_>,ApiError>>()
    }).await.map(Json)
}

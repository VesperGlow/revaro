//! Authenticated operational statistics.
use crate::{auth::extract::AuthUser, state::AppState};
use axum::{Json, Router, extract::State, routing::get};
use revaro_core::ApiError;
use std::sync::Arc;
pub fn routes() -> Router<Arc<AppState>> {
    Router::new().route("/system/status", get(status))
}
async fn status(
    State(s): State<Arc<AppState>>,
    _u: AuthUser,
) -> Result<Json<serde_json::Value>, ApiError> {
    let root = s.store.root().to_owned();
    let space = tokio::task::spawn_blocking(move || fs2::available_space(root))
        .await
        .map_err(|_| ApiError::internal("disk probe failed"))?
        .map_err(|_| ApiError::internal("disk probe failed"))?;
    let tasks=s.db.call_api(|c| {
        let active:i64=c.query_row("SELECT count(*) FROM tasks WHERE status IN ('running','retrying','waiting_input')",[],|r|r.get(0)).map_err(|_|ApiError::internal("database error"))?;
        Ok(active)
    }).await?;
    Ok(Json(
        serde_json::json!({"disk_available_bytes":space,"cache":s.cache.stats(),"maintenance":s.maintenance.stats(),"active_tasks":tasks,
        "reader_available_slots":s.reader.work_slots.available_permits(),"zip_available_slots":s.zip_slots.available_permits(),"share_available_slots":s.share_slots.available_permits()}),
    ))
}

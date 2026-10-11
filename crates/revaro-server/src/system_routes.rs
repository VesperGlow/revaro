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
    let (total, free, space) = tokio::task::spawn_blocking(move || {
        Ok::<_, std::io::Error>((
            fs2::total_space(&root)?,
            fs2::free_space(&root)?,
            fs2::available_space(&root)?,
        ))
    })
    .await
    .map_err(|_| ApiError::internal("disk probe failed"))?
    .map_err(|_| ApiError::internal("disk probe failed"))?;
    Ok(Json(
        serde_json::json!({"disk_total_bytes":total,"disk_used_bytes":total.saturating_sub(free),"disk_available_bytes":space,"cache":s.cache.stats(),"memory_budget":s.memory,"maintenance":s.maintenance.stats(),
        "reader_available_slots":s.reader.work_slots.available_permits(),"zip_available_slots":s.zip_slots.available_permits(),"share_available_slots":s.share_slots.available_permits()}),
    ))
}

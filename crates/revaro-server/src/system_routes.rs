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
    let (total, free, space, memory) = tokio::task::spawn_blocking(move || {
        Ok::<_, std::io::Error>((
            fs2::total_space(&root)?,
            fs2::free_space(&root)?,
            fs2::available_space(&root)?,
            crate::memory::read_memory(),
        ))
    })
    .await
    .map_err(|_| ApiError::internal("disk probe failed"))?
    .map_err(|_| ApiError::internal("disk probe failed"))?;
    Ok(Json(
        serde_json::json!({"disk_total_bytes":total,"disk_used_bytes":total.saturating_sub(free),"disk_available_bytes":space,"memory":memory,"cache":s.cache.stats(),"memory_budget":s.memory,"maintenance":s.maintenance.stats(),
        "reader_available_slots":s.reader.work_slots.available_permits(),"zip_available_slots":s.zip_slots.available_permits(),"share_available_slots":s.share_slots.available_permits()}),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{
        body::Body,
        http::{Request, StatusCode},
    };
    use http_body_util::BodyExt;
    use revaro_core::system::SystemStatusSummary;
    use tower::ServiceExt;

    #[tokio::test]
    async fn status_reports_actual_cache_usage_and_dynamic_limit_separately_from_kernel_memory() {
        let root = std::env::temp_dir().join(format!("revaro-system-{}", crate::ids::new_id()));
        let config = crate::config::Config::from_lookup(&|name| match name {
            "APP_BASE_URL" => Some("http://localhost:8080".to_owned()),
            "APP_CACHES_DIR" => Some(root.join("caches").display().to_string()),
            _ => None,
        })
        .unwrap();
        let store = crate::storage::LocalStore::open(root.join("objects"))
            .await
            .unwrap();
        let db = crate::db::Database::open_in_memory().unwrap();
        let auth = crate::auth::AuthService::new(db.clone());
        let state = AppState::new(Arc::new(config), db, store, auth);
        let hash = crate::auth::token_hash("system-test");
        state.db.call(move |c| {
            c.execute("INSERT INTO settings(key,value,updated_at) VALUES('admin_username','admin','2026-01-01T00:00:00Z')", [])?;
            c.execute("INSERT INTO sessions(id,token_hash,created_at,expires_at) VALUES('system-session',?1,'2026-01-01T00:00:00Z','2999-01-01T00:00:00Z')", [hash])?;
            Ok(())
        }).await.unwrap();
        state.cache.set_memory_limit(4096);
        state
            .cache
            .put(
                crate::cache::READER_SOURCE,
                "test",
                b"payload",
                std::time::Duration::ZERO,
            )
            .await
            .unwrap();
        let router = routes().with_state(state.clone());
        let unauthorized = router
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/system/status")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(unauthorized.status(), StatusCode::UNAUTHORIZED);
        for (limit, used) in [(4096, 7), (0, 0)] {
            state.cache.set_memory_limit(limit);
            let response = router
                .clone()
                .oneshot(
                    Request::builder()
                        .uri("/system/status")
                        .header(
                            "cookie",
                            format!("{}=system-test", crate::auth::SESSION_COOKIE),
                        )
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::OK);
            let bytes = response.into_body().collect().await.unwrap().to_bytes();
            let status: SystemStatusSummary = serde_json::from_slice(&bytes).unwrap();
            assert_eq!(status.cache.memory_bytes, used);
            assert_eq!(status.cache.memory_limit, Some(limit as u64));
            assert_eq!(status.cache.disk_bytes, 7);
            if let (Some(total), Some(available)) = (
                status.memory.host_total_bytes,
                status.memory.host_available_bytes,
            ) {
                assert_eq!(status.memory.host_used_bytes, Some(total - available));
                assert!(
                    status
                        .memory
                        .available_bytes
                        .is_none_or(|value| value <= available)
                );
            }
            assert!(status.disk_total_bytes >= status.disk_used_bytes);
        }
        drop(router);
        drop(state);
        std::fs::remove_dir_all(root).unwrap();
    }
}

//! Reserve response capacity for interactive reads while large downloads run.
//! Both TCP and QUIC use this middleware, before bytes enter either protocol.

use std::sync::Arc;

use axum::{extract::State, middleware::Next, response::Response};
use tokio::sync::Semaphore;

use crate::state::AppState;

#[derive(Debug)]
pub struct DeliveryRuntime {
    slots: Arc<Semaphore>,
    background: Arc<Semaphore>,
    admitted: Arc<Semaphore>,
}

impl Default for DeliveryRuntime {
    fn default() -> Self {
        Self {
            slots: Arc::new(Semaphore::new(12)),
            background: Arc::new(Semaphore::new(4)),
            admitted: Arc::new(Semaphore::new(128)),
        }
    }
}

pub async fn schedule(
    State(state): State<Arc<AppState>>,
    request: axum::extract::Request,
    next: Next,
) -> Response {
    let path = request.uri().path();
    if !matches!(*request.method(), http::Method::GET | http::Method::HEAD)
        || !path.starts_with("/api/files/")
    {
        return next.run(request).await;
    }
    use axum::response::IntoResponse as _;
    let Ok(admitted) = Arc::clone(&state.delivery.admitted).try_acquire_owned() else {
        return revaro_core::ApiError::unavailable("file delivery is busy; retry shortly")
            .into_response();
    };
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(5);
    let hinted = request
        .headers()
        .get("priority")
        .and_then(|value| value.to_str().ok())
        .and_then(|value| {
            value
                .split(',')
                .find_map(|part| part.trim().strip_prefix("u=")?.parse::<u8>().ok())
        });
    let background = hinted.map_or_else(
        || {
            path.ends_with("/download")
                || path.ends_with("/thumbnail")
                || path.contains("/batch-download/")
        },
        |urgency| urgency >= 4,
    );
    // Acquire the smaller budget first so queued bulk reads cannot occupy
    // capacity reserved for the active player or reader.
    let background_permit = if background {
        match tokio::time::timeout_at(
            deadline,
            Arc::clone(&state.delivery.background).acquire_owned(),
        )
        .await
        {
            Ok(Ok(permit)) => Some(permit),
            _ => {
                return revaro_core::ApiError::unavailable("file delivery is busy; retry shortly")
                    .into_response();
            }
        }
    } else {
        None
    };
    let permit =
        match tokio::time::timeout_at(deadline, Arc::clone(&state.delivery.slots).acquire_owned())
            .await
        {
            Ok(Ok(permit)) => permit,
            _ => {
                return revaro_core::ApiError::unavailable("file delivery is busy; retry shortly")
                    .into_response();
            }
        };
    let response = next.run(request).await;
    let (parts, mut body) = response.into_parts();
    if let Some(background_permit) = background_permit {
        body = crate::transfer::hold_permit(body, background_permit);
    }
    body = crate::transfer::hold_permit(body, admitted);
    Response::from_parts(parts, crate::transfer::hold_permit(body, permit))
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{Router, body::Body, routing::get};
    use http_body_util::BodyExt as _;
    use tower::ServiceExt as _;

    #[tokio::test]
    async fn occupied_bulk_streams_leave_room_for_interactive_reads_and_cancel_cleanly() {
        let root =
            std::env::temp_dir().join(format!("revaro-delivery-test-{}", uuid::Uuid::new_v4()));
        let config = crate::config::Config::from_lookup(&|key| match key {
            "APP_CACHES_DIR" => Some(root.join("caches").display().to_string()),
            _ => None,
        })
        .unwrap();
        let db = crate::db::Database::open_in_memory().unwrap();
        let auth = crate::auth::AuthService::new(db.clone());
        let store = crate::storage::LocalStore::open(root.join("objects"))
            .await
            .unwrap();
        let state = AppState::new(Arc::new(config), db, store, auth);
        let app = Router::new()
            .route(
                "/api/files/bulk/download",
                get(|| async {
                    Body::from_stream(futures_util::stream::pending::<
                        Result<bytes::Bytes, std::io::Error>,
                    >())
                }),
            )
            .route(
                "/api/files/current/preview",
                get(|| async { "current media" }),
            )
            .layer(axum::middleware::from_fn_with_state(
                Arc::clone(&state),
                schedule,
            ));
        let request = |path: &str| {
            http::Request::builder()
                .uri(path)
                .body(Body::empty())
                .unwrap()
        };
        let mut responses = Vec::new();
        for _ in 0..4 {
            responses.push(
                app.clone()
                    .oneshot(request("/api/files/bulk/download"))
                    .await
                    .unwrap(),
            );
        }
        let next_app = app.clone();
        let mut queued = tokio::spawn(async move {
            next_app
                .oneshot(request("/api/files/bulk/download"))
                .await
                .unwrap()
        });
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(30), &mut queued)
                .await
                .is_err()
        );
        let full = Arc::clone(&state.delivery.admitted)
            .try_acquire_many_owned(123)
            .unwrap();
        let rejected = tokio::time::timeout(
            std::time::Duration::from_millis(100),
            app.clone().oneshot(request("/api/files/current/preview")),
        )
        .await
        .unwrap()
        .unwrap();
        assert_eq!(rejected.status(), http::StatusCode::SERVICE_UNAVAILABLE);
        drop(rejected);
        drop(full);
        let foreground = tokio::time::timeout(
            std::time::Duration::from_secs(1),
            app.oneshot(request("/api/files/current/preview")),
        )
        .await
        .unwrap()
        .unwrap();
        assert_eq!(
            foreground.into_body().collect().await.unwrap().to_bytes(),
            "current media"
        );
        drop(responses.pop());
        let admitted = tokio::time::timeout(std::time::Duration::from_secs(1), queued)
            .await
            .unwrap()
            .unwrap();
        drop(admitted);
        drop(responses);
        assert_eq!(state.delivery.slots.available_permits(), 12);
        assert_eq!(state.delivery.background.available_permits(), 4);
        assert_eq!(state.delivery.admitted.available_permits(), 128);
    }
}

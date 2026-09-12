//! The HTTP router.
//!
//! The shape mirrors the Go chi router so the client contract is unchanged:
//! `/healthz` and `/readyz` are public probes, `/api/*` always answers JSON
//! (including its own 404), and everything else is the single-page client.
//!
//! Feature routers are attached one migration stage at a time; until then every
//! unknown API path produces the same `api endpoint not found` body the Go
//! server produced, so the client's error handling is already correct.

use std::sync::Arc;

use axum::extract::State;
use axum::routing::get;
use axum::{Json, Router};
use revaro_core::ApiError;
use revaro_core::api::Health;

use crate::state::AppState;
use crate::{middleware, web};

/// Assemble the full application router.
pub fn build(state: Arc<AppState>) -> Router {
    Router::new()
        .route("/healthz", get(health))
        .route("/readyz", get(ready))
        // The public share table is deliberately outside `/api`: it is reached
        // with a URL-borne token and must not require a session.
        .merge(crate::file_routes::public_routes())
        .nest("/api", api())
        .fallback(web::serve)
        .with_state(state.clone())
        // Order matters, and matches the Go chain: security headers wrap the
        // origin guard, which wraps the routes.
        .layer(axum::middleware::from_fn_with_state(
            state.clone(),
            middleware::origin_guard,
        ))
        .layer(axum::middleware::from_fn_with_state(
            state,
            middleware::security_headers,
        ))
}

/// Liveness probe: the process is up.
async fn health() -> Json<Health> {
    Json(Health {
        status: "ok".to_owned(),
    })
}

/// Readiness probe: the database answers.
///
/// Object storage joins this check once the storage module is migrated; the Go
/// handler answered `503 object storage unavailable` in that case.
async fn ready(State(state): State<Arc<AppState>>) -> Result<Json<Health>, ApiError> {
    let database = state.db.clone();
    let reachable = tokio::task::spawn_blocking(move || database.ping())
        .await
        .map_err(|error| {
            tracing::error!(%error, "readiness check could not run");
            ApiError::unavailable("database unavailable")
        })?;
    match reachable {
        Ok(()) => Ok(Json(Health {
            status: "ready".to_owned(),
        })),
        Err(error) => {
            tracing::warn!(%error, "readiness check failed");
            Err(ApiError::unavailable("database unavailable"))
        }
    }
}

/// The authenticated API subtree.
fn api() -> Router<Arc<AppState>> {
    Router::new()
        .merge(crate::auth_routes::routes())
        .merge(crate::archive_routes::routes())
        .merge(crate::file_routes::routes())
        .merge(crate::media_routes::routes())
        .merge(crate::reader_routes::routes())
        .merge(crate::upload_routes::routes())
        .fallback(api_not_found)
}

/// Every unmatched `/api/*` path answers JSON rather than falling through to
/// the single-page client.
async fn api_not_found() -> ApiError {
    ApiError::not_found("api endpoint not found")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;
    use crate::db::Database;
    use crate::storage::LocalStore;
    use axum::body::Body;
    use http::{Request, StatusCode};
    use http_body_util::BodyExt as _;
    use tower::ServiceExt as _;

    /// Shared state for router tests: in-memory database, a scratch object
    /// store, and no web bundle.
    pub async fn test_state() -> Arc<AppState> {
        let config = Config::from_lookup(&|name| match name {
            "APP_BASE_URL" => Some("http://localhost:8080".to_owned()),
            "APP_WEB_DIR" => Some("/nonexistent-web-dir".to_owned()),
            _ => None,
        })
        .expect("test configuration is valid");
        let store_root = std::env::temp_dir().join(format!(
            "revaro-router-store-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let store = LocalStore::open(&store_root)
            .await
            .expect("object store opens");
        let database = Database::open_in_memory().expect("in-memory database");
        let auth = crate::auth::AuthService::new(database.clone());
        AppState::new(Arc::new(config), database, store, auth)
    }

    async fn get(app: Router, uri: &str, origin: Option<&str>) -> (StatusCode, serde_json::Value) {
        let mut request = Request::builder().method("GET").uri(uri);
        if let Some(origin) = origin {
            request = request.header("origin", origin);
        }
        let response = app
            .oneshot(request.body(Body::empty()).unwrap())
            .await
            .unwrap();
        let status = response.status();
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        let json = serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null);
        (status, json)
    }

    #[tokio::test]
    async fn healthz_reports_ok() {
        let (status, body) = get(build(test_state().await), "/healthz", None).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body, serde_json::json!({"status": "ok"}));
    }

    #[tokio::test]
    async fn readyz_reports_ready_when_the_database_answers() {
        let (status, body) = get(build(test_state().await), "/readyz", None).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body, serde_json::json!({"status": "ready"}));
    }

    #[tokio::test]
    async fn unknown_api_paths_answer_json() {
        let (status, body) = get(build(test_state().await), "/api/does-not-exist", None).await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        assert_eq!(
            body,
            serde_json::json!({"error": {"status": 404, "message": "api endpoint not found"}})
        );
    }

    #[tokio::test]
    async fn api_responses_are_not_cached_and_carry_security_headers() {
        let response = build(test_state().await)
            .oneshot(
                Request::builder()
                    .uri("/api/does-not-exist")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let headers = response.headers();
        assert_eq!(headers.get("cache-control").unwrap(), "no-store");
        assert_eq!(headers.get("x-content-type-options").unwrap(), "nosniff");
        assert_eq!(headers.get("x-frame-options").unwrap(), "DENY");
        assert!(headers.get("content-security-policy").is_some());
        // An HTTP base URL must not advertise HSTS.
        assert!(headers.get("strict-transport-security").is_none());
    }

    #[tokio::test]
    async fn write_requests_require_a_matching_origin() {
        let app = build(test_state().await);
        let request = Request::builder()
            .method("POST")
            .uri("/api/uploads")
            .body(Body::empty())
            .unwrap();
        let response = app.clone().oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::FORBIDDEN);

        let request = Request::builder()
            .method("POST")
            .uri("/api/uploads")
            .header("origin", "http://evil.example.com")
            .body(Body::empty())
            .unwrap();
        let response = app.clone().oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::FORBIDDEN);

        let request = Request::builder()
            .method("POST")
            .uri("/api/uploads")
            .header("origin", "http://localhost:8080")
            .body(Body::empty())
            .unwrap();
        let response = app.oneshot(request).await.unwrap();
        // The origin is accepted, so the guard lets the request through. Assert
        // the *absence* of 403 rather than a specific downstream status: this
        // route now exists and answers 401, and pinning a status here would
        // break every time a route is added.
        assert_ne!(
            response.status(),
            StatusCode::FORBIDDEN,
            "a same-origin write must pass the origin guard"
        );
    }

    #[tokio::test]
    async fn reads_do_not_require_an_origin() {
        let (status, _) = get(build(test_state().await), "/healthz", None).await;
        assert_eq!(status, StatusCode::OK);
    }

    #[tokio::test]
    async fn a_missing_bundle_reports_how_to_build_it() {
        let response = build(test_state().await)
            .oneshot(Request::builder().uri("/").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        let text = String::from_utf8(bytes.to_vec()).unwrap();
        assert!(text.contains("cargo xtask web-build"), "{text}");
    }
}

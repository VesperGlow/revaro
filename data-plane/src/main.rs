use std::{env, net::SocketAddr, sync::Arc, time::Duration};

use axum::{
    Json, Router,
    extract::{DefaultBodyLimit, Request, State},
    http::{HeaderValue, StatusCode, header::AUTHORIZATION},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::{get, post, put},
};
use serde::Serialize;
use subtle::ConstantTimeEq;
use tokio::{net::TcpListener, signal};
use tokio_util::sync::CancellationToken;
use tower_http::trace::TraceLayer;

const PROTOCOL_VERSION: u16 = 1;

mod archive;
mod backup;
mod error;
mod local;
mod media;

#[derive(Clone)]
struct AppState {
    bearer: Arc<[u8]>,
    local: local::LocalState,
    backup: Option<backup::BackupState>,
    media_light_slots: Arc<tokio::sync::Semaphore>,
    archive: archive::ArchiveState,
    archive_slots: Arc<tokio::sync::Semaphore>,
    shutdown: CancellationToken,
}

#[derive(Serialize)]
struct Health {
    status: &'static str,
    protocol: u16,
}

#[tokio::main(worker_threads = 2)]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt()
        .json()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();

    let addr: SocketAddr = env::var("REVARO_DATA_PLANE_ADDR")
        .unwrap_or_else(|_| "127.0.0.1:7081".into())
        .parse()?;
    if !addr.ip().is_loopback() {
        return Err("REVARO_DATA_PLANE_ADDR must be loopback".into());
    }
    let token = env::var("REVARO_DATA_PLANE_TOKEN")?;
    if token.len() < 32 {
        return Err("REVARO_DATA_PLANE_TOKEN must contain at least 32 bytes".into());
    }

    let shutdown = CancellationToken::new();
    let local = local::LocalState::from_env()?;
    let backup = backup::BackupState::from_env().await?;
    let state = AppState {
        bearer: Arc::from(format!("Bearer {token}").into_bytes()),
        local,
        backup,
        media_light_slots: Arc::new(tokio::sync::Semaphore::new(2)),
        archive: archive::ArchiveState::default(),
        archive_slots: Arc::new(tokio::sync::Semaphore::new(1)),
        shutdown: shutdown.clone(),
    };
    let app = Router::new()
        .route("/v1/health", get(health))
        .route("/v1/backup/object", put(backup::upload))
        .route(
            "/v1/backup/objects",
            get(backup::list).delete(backup::delete),
        )
        .route("/v1/archive/extract", post(archive::extract))
        .route("/v1/archive/{job_id}/cancel", post(archive::cancel))
        .route("/v1/archive/{job_id}/progress", get(archive::progress))
        .route("/v1/media/probe", post(media::probe))
        .route("/v1/media/thumbnail", post(media::thumbnail))
        .route("/v1/media/subtitle", post(media::subtitle))
        // Control messages are deliberately small. Streaming endpoints opt in
        // to their own byte limits and consume bodies incrementally.
        .layer(DefaultBodyLimit::max(1 << 20))
        .layer(middleware::from_fn_with_state(state.clone(), authorize))
        .layer(TraceLayer::new_for_http())
        .with_state(state.clone());

    let listener = TcpListener::bind(addr).await?;
    tracing::info!(%addr, protocol = PROTOCOL_VERSION, "data plane ready");

    let shutdown_for_signal = shutdown.clone();
    axum::serve(listener, app)
        .with_graceful_shutdown(async move {
            shutdown_signal().await;
            shutdown_for_signal.cancel();
        })
        .await?;
    Ok(())
}

async fn health() -> Json<Health> {
    Json(Health {
        status: "ok",
        protocol: PROTOCOL_VERSION,
    })
}

async fn authorize(State(state): State<AppState>, request: Request, next: Next) -> Response {
    let supplied = request
        .headers()
        .get(AUTHORIZATION)
        .map(HeaderValue::as_bytes)
        .unwrap_or_default();
    if supplied.len() != state.bearer.len() || !bool::from(supplied.ct_eq(state.bearer.as_ref())) {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    next.run(request).await
}

async fn shutdown_signal() {
    let ctrl_c = async {
        signal::ctrl_c().await.expect("install Ctrl-C handler");
    };
    #[cfg(unix)]
    let terminate = async {
        signal::unix::signal(signal::unix::SignalKind::terminate())
            .expect("install SIGTERM handler")
            .recv()
            .await;
    };
    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        () = ctrl_c => {},
        () = terminate => {},
    }
    // Leave a short window for in-flight bounded streams to observe shutdown.
    tokio::time::sleep(Duration::from_millis(50)).await;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn public_listener_is_rejected() {
        let addr: SocketAddr = "0.0.0.0:7081".parse().unwrap();
        assert!(!addr.ip().is_loopback());
    }
}

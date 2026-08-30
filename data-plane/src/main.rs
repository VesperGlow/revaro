use std::{
    collections::HashMap,
    env,
    net::SocketAddr,
    sync::{Arc, Mutex},
    time::Duration,
};

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
mod audio_fifo;
mod bt;
mod error;
mod media;
mod media_audio;
mod s3;

#[derive(Clone)]
struct AppState {
    bearer: Arc<[u8]>,
    s3: s3::S3State,
    media_light_slots: Arc<tokio::sync::Semaphore>,
    media_stream_slots: Arc<tokio::sync::Semaphore>,
    media_heavy_slots: Arc<tokio::sync::Semaphore>,
    hls_jobs: Arc<Mutex<HashMap<String, Arc<media::HlsJob>>>>,
    bt: bt::BtState,
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
    let s3 = s3::S3State::from_env().await?;
    let state = AppState {
        bearer: Arc::from(format!("Bearer {token}").into_bytes()),
        s3,
        // Keep one CPU-heavy encoder on 2C/4G, while allowing probes and other
        // short control-plane media work to remain responsive.
        media_light_slots: Arc::new(tokio::sync::Semaphore::new(2)),
        media_stream_slots: Arc::new(tokio::sync::Semaphore::new(2)),
        media_heavy_slots: Arc::new(tokio::sync::Semaphore::new(1)),
        hls_jobs: Arc::new(Mutex::new(HashMap::new())),
        bt: bt::BtState::from_env().await?,
        archive: archive::ArchiveState::default(),
        archive_slots: Arc::new(tokio::sync::Semaphore::new(1)),
        shutdown: shutdown.clone(),
    };
    let app = Router::new()
        .route("/v1/health", get(health))
        .route("/v1/s3/ping", get(s3::ping))
        .route(
            "/v1/s3/object",
            get(s3::get_range)
                .put(s3::put_stream)
                .delete(s3::delete_one),
        )
        .route("/v1/s3/blob", put(s3::put_blob))
        .route("/v1/s3/object/info", get(s3::head))
        .route("/v1/s3/objects", get(s3::list).delete(s3::delete_many))
        .route("/v1/s3/presign/put", post(s3::presign_put))
        .route("/v1/s3/presign/get", post(s3::presign_get))
        .route(
            "/v1/s3/multipart",
            post(s3::multipart_create)
                .put(s3::multipart_complete)
                .delete(s3::multipart_abort),
        )
        .route("/v1/s3/multipart/part", post(s3::multipart_part))
        .route("/v1/s3/multipart/upload", put(s3::multipart_upload))
        .route("/v1/archive/extract", post(archive::extract))
        .route("/v1/archive/{job_id}/cancel", post(archive::cancel))
        .route("/v1/archive/{job_id}/progress", get(archive::progress))
        .route("/v1/media/probe", post(media::probe))
        .route("/v1/media/thumbnail", post(media::thumbnail))
        .route("/v1/media/fmp4", post(media::fmp4))
        .route("/v1/media/hls", post(media::hls))
        .route(
            "/v1/media/hls/{job_id}",
            get(media::hls_status).delete(media::cancel_hls),
        )
        .route("/v1/media/audio/merge", post(media_audio::merge))
        .route("/v1/media/audio/decorate", post(media_audio::decorate))
        .route("/v1/media/subtitle", post(media::subtitle))
        .route("/v1/bt", post(bt::add))
        .route("/v1/bt/{id}", get(bt::details).delete(bt::delete))
        .route("/v1/bt/{id}/stats", get(bt::stats))
        .route("/v1/bt/{id}/selection", put(bt::select))
        .route("/v1/bt/{id}/start", post(bt::start))
        .route("/v1/bt/{id}/pause", post(bt::pause))
        .route("/v1/bt/{id}/import", post(bt::import))
        .route("/v1/bt/{id}/stream/{file_id}", get(bt::stream))
        // Control messages are deliberately small. Streaming endpoints opt in
        // to their own byte limits and consume bodies incrementally.
        .layer(DefaultBodyLimit::max(1 << 20))
        .layer(middleware::from_fn_with_state(state.clone(), authorize))
        .layer(TraceLayer::new_for_http())
        .with_state(state.clone());

    let listener = TcpListener::bind(addr).await?;
    tracing::info!(%addr, protocol = PROTOCOL_VERSION, "data plane ready");

    // Multipart cleanup can require many paginated S3 calls. It is
    // housekeeping, not a readiness prerequisite, so run it after bind with
    // both a deadline and shutdown cancellation.
    let cleanup_s3 = state.s3.clone();
    let cleanup_shutdown = shutdown.clone();
    tokio::spawn(async move {
        let stale_age = env::var("S3_MULTIPART_STALE_SECONDS")
            .ok()
            .and_then(|value| value.parse().ok())
            .map(Duration::from_secs)
            .unwrap_or(Duration::from_secs(24 * 60 * 60));
        let cleanup = tokio::time::timeout(
            Duration::from_secs(120),
            cleanup_s3.abort_stale_multipart_uploads(stale_age),
        );
        tokio::select! {
            _ = cleanup_shutdown.cancelled() => {},
            result = cleanup => match result {
                Ok(Ok(count)) if count > 0 => tracing::warn!(count, "aborted stale multipart uploads"),
                Ok(Ok(_)) => {},
                Ok(Err(error)) => tracing::warn!(?error, "could not clean stale multipart uploads"),
                Err(_) => tracing::warn!("stale multipart cleanup timed out"),
            }
        }
    });
    let shutdown_for_signal = shutdown.clone();
    let bt_for_signal = state.bt.clone();
    axum::serve(listener, app)
        .with_graceful_shutdown(async move {
            shutdown_signal().await;
            shutdown_for_signal.cancel();
            bt_for_signal.stop().await;
        })
        .await?;
    state.bt.stop().await;
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

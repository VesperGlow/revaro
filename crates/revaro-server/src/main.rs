//! The Revaro server entry point.
//!
//! Startup is intentionally boring: load configuration, initialise logging, bind
//! the listener, serve until a shutdown signal arrives. Everything with real
//! behaviour lives in the library so it can be tested without spawning a
//! process.

use std::process::ExitCode;
use std::sync::Arc;

use revaro_server::config::Config;
use revaro_server::router;
use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() -> ExitCode {
    init_tracing();

    let config = match Config::from_env() {
        Ok(config) => Arc::new(config),
        Err(error) => {
            tracing::error!(%error, "configuration invalid");
            return ExitCode::FAILURE;
        }
    };

    let addr = match config.listen_addr() {
        Ok(addr) => addr,
        Err(error) => {
            tracing::error!(%error, "configuration invalid");
            return ExitCode::FAILURE;
        }
    };

    let app = router::build(config.clone());
    let listener = match tokio::net::TcpListener::bind(addr).await {
        Ok(listener) => listener,
        Err(error) => {
            tracing::error!(%error, addr = %addr, "server listen failed");
            return ExitCode::FAILURE;
        }
    };

    tracing::info!(
        addr = %addr,
        data_dir = %config.data_dir.display(),
        web_dir = %config.web_dir.display(),
        base_url = %config.base_url,
        "server started"
    );

    match axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await
    {
        Ok(()) => {
            tracing::info!("server stopped");
            ExitCode::SUCCESS
        }
        Err(error) => {
            tracing::error!(%error, "server stopped unexpectedly");
            ExitCode::FAILURE
        }
    }
}

fn init_tracing() {
    let filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new("revaro_server=info,tower_http=warn"));
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_target(false)
        .init();
}

/// Resolve when the process is asked to stop.
async fn shutdown_signal() {
    let interrupt = async {
        let _ = tokio::signal::ctrl_c().await;
    };
    #[cfg(unix)]
    let terminate = async {
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(mut signal) => {
                signal.recv().await;
            }
            Err(error) => {
                tracing::warn!(%error, "could not install the SIGTERM handler");
                std::future::pending::<()>().await;
            }
        }
    };
    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        () = interrupt => tracing::info!("interrupt received, shutting down"),
        () = terminate => tracing::info!("termination signal received, shutting down"),
    }
}

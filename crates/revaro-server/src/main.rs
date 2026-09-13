//! The Revaro server entry point.
//!
//! Startup is intentionally boring: load configuration, initialise logging, bind
//! the listener, serve until a shutdown signal arrives. Everything with real
//! behaviour lives in the library so it can be tested without spawning a
//! process.

use std::process::ExitCode;
use std::sync::Arc;

use revaro_server::config::Config;
use revaro_server::db::Database;
use revaro_server::router;
use revaro_server::state::AppState;
use revaro_server::storage::LocalStore;
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

    let database = match open_database(&config) {
        Ok(database) => database,
        Err(error) => {
            tracing::error!(%error, "database startup failed");
            return ExitCode::FAILURE;
        }
    };
    tracing::info!(path = %database.path().display(), "database ready");

    if let Err(error) = prepare_work_directory(&config) {
        tracing::error!(%error, path = %config.work_dir.display(), "work directory startup check failed");
        return ExitCode::FAILURE;
    }

    let store = match LocalStore::open(config.objects_dir()).await {
        Ok(store) => store,
        Err(error) => {
            tracing::error!(%error, "local object storage startup failed");
            return ExitCode::FAILURE;
        }
    };
    if let Err(error) = store.ping().await {
        tracing::error!(%error, "local object storage check failed");
        return ExitCode::FAILURE;
    }
    tracing::info!(path = %store.root().display(), "local object storage ready");

    let auth = revaro_server::auth::AuthService::new(database.clone());
    let state = AppState::new(config.clone(), database, store, auth);
    match state
        .auth
        .initialize(&config.admin_username, &config.admin_password)
        .await
    {
        Ok(credentials) if credentials.created && credentials.generated => {
            tracing::warn!(
                username = %credentials.username,
                password = %credentials.password,
                "generated initial administrator credentials; sign in and change them"
            );
        }
        Ok(credentials) if credentials.created => {
            tracing::info!(username = %credentials.username, "administrator account created");
        }
        Ok(_) => {}
        Err(error) => {
            tracing::error!(%error, "administrator initialization failed");
            return ExitCode::FAILURE;
        }
    }
    let app = router::build(state.clone());
    let listener = match tokio::net::TcpListener::bind(addr).await {
        Ok(listener) => listener,
        Err(error) => {
            tracing::error!(%error, addr = %addr, "server listen failed");
            return ExitCode::FAILURE;
        }
    };

    // Recover archive tasks only after the listener is bound. A failed bind
    // must not leave background extraction workers running in a process that
    // is about to exit.
    revaro_server::archive_routes::recover(state.clone()).await;
    state
        .status
        .start(state.db.clone(), state.store.clone(), state.cache.clone());
    state.maintenance.start();

    tracing::info!(
        addr = %addr,
        data_dir = %config.data_dir.display(),
        web_dir = %config.web_dir.display(),
        base_url = %config.base_url,
        "server started"
    );

    let result = axum::serve(
        listener,
        app.into_make_service_with_connect_info::<std::net::SocketAddr>(),
    )
    .with_graceful_shutdown(shutdown_signal())
    .await;
    state.maintenance.close().await;
    state.status.shutdown();
    state.archive.shutdown();
    state.cache.close().await;
    match result {
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

/// Open and migrate the SQLite database.
fn open_database(config: &Config) -> Result<Database, revaro_server::db::DbError> {
    Database::open(config.database_path())
}

/// Create the work directory and prove it is writable.
///
/// The Go server refused to start when the work directory could not hold a
/// probe file, because archive extraction and reader caches would fail later
/// anyway — and failing at startup is far easier to diagnose.
fn prepare_work_directory(config: &Config) -> std::io::Result<()> {
    std::fs::create_dir_all(&config.work_dir)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(&config.work_dir, std::fs::Permissions::from_mode(0o700))?;
    }
    let probe = config.work_dir.join(".revaro-write-check");
    std::fs::write(&probe, b"")?;
    std::fs::remove_file(&probe)?;
    Ok(())
}

fn init_tracing() {
    // The filter must name the *binary* crate (`revaro`) as well as the library
    // crates: `tracing` targets are module paths, and an event emitted from
    // `main.rs` has target `revaro`, which a `revaro_server=info` directive
    // would filter out. Getting this wrong silences startup logging entirely.
    const DEFAULT_FILTER: &str =
        "revaro=info,revaro_server=info,revaro_media=info,revaro_reader=info,tower_http=warn";
    let filter =
        EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(DEFAULT_FILTER));
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

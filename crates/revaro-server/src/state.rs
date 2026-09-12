//! Shared application state.
//!
//! Axum carries exactly one state type, so this is where the process-wide
//! handles live. It is deliberately small: feature modules add their own
//! collaborators here as they are migrated, rather than threading a growing
//! argument list through every handler.

use std::sync::Arc;

use crate::config::Config;
use crate::db::Database;
use crate::storage::LocalStore;

/// Everything a handler can reach.
#[derive(Debug)]
pub struct AppState {
    /// Process configuration.
    pub config: Arc<Config>,
    /// The migrated SQLite database.
    pub db: Database,
    /// The local object store holding original bytes and derived artifacts.
    pub store: LocalStore,
}

impl AppState {
    /// Build the state for a running server.
    #[must_use]
    pub fn new(config: Arc<Config>, db: Database, store: LocalStore) -> Arc<Self> {
        Arc::new(Self { config, db, store })
    }
}

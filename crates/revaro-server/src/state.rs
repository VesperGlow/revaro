//! Shared application state.
//!
//! Axum carries exactly one state type, so this is where the process-wide
//! handles live. It is deliberately small: feature modules add their own
//! collaborators here as they are migrated, rather than threading a growing
//! argument list through every handler.

use std::sync::Arc;

use crate::config::Config;
use crate::db::Database;

/// Everything a handler can reach.
#[derive(Debug)]
pub struct AppState {
    /// Process configuration.
    pub config: Arc<Config>,
    /// The migrated SQLite database.
    pub db: Database,
}

impl AppState {
    /// Build the state for a running server.
    #[must_use]
    pub fn new(config: Arc<Config>, db: Database) -> Arc<Self> {
        Arc::new(Self { config, db })
    }
}

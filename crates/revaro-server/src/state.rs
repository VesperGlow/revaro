//! Shared application state.
//!
//! Axum carries exactly one state type, so this is where the process-wide
//! handles live. It is deliberately small: feature modules add their own
//! collaborators here as they are migrated, rather than threading a growing
//! argument list through every handler.

use std::sync::Arc;

use crate::auth::AuthService;
use crate::config::Config;
use crate::db::Database;
use crate::storage::LocalStore;

/// A change notification bus for the job/task event stream.
///
/// Subscribers only need to know *that* something changed, not what: the client
/// re-reads `GET /api/tasks` when it is told. That keeps the payload free of
/// task details, so a task cannot leak through the stream to a client that is
/// not allowed to read it.
///
/// Lagging receivers are dropped by the broadcast channel rather than blocking
/// the sender; a slow SSE client falls behind and resynchronises on its next
/// read, instead of stalling every handler that publishes.
#[derive(Debug, Clone)]
pub struct JobBus {
    sender: tokio::sync::broadcast::Sender<()>,
}

impl JobBus {
    /// Create the bus. `capacity` bounds how far a subscriber may lag.
    #[must_use]
    pub fn new(capacity: usize) -> Self {
        let (sender, _) = tokio::sync::broadcast::channel(capacity);
        Self { sender }
    }

    /// Announce that task state changed. Never fails: with no subscribers the
    /// signal is simply discarded.
    pub fn changed(&self) {
        let _ = self.sender.send(());
    }

    /// Subscribe to change notifications.
    #[must_use]
    pub fn subscribe(&self) -> tokio::sync::broadcast::Receiver<()> {
        self.sender.subscribe()
    }
}

impl Default for JobBus {
    fn default() -> Self {
        Self::new(256)
    }
}

/// Everything a handler can reach.
#[derive(Debug)]
pub struct AppState {
    /// Process configuration.
    pub config: Arc<Config>,
    /// The migrated SQLite database.
    pub db: Database,
    /// The local object store holding original bytes and derived artifacts.
    pub store: LocalStore,
    /// Administrator credentials, sessions and second factor.
    pub auth: AuthService,
    /// Task-change notifications for the event stream.
    pub jobs: JobBus,
}

impl AppState {
    /// Build the state for a running server.
    #[must_use]
    pub fn new(
        config: Arc<Config>,
        db: Database,
        store: LocalStore,
        auth: AuthService,
    ) -> Arc<Self> {
        Arc::new(Self {
            config,
            db,
            store,
            auth,
            jobs: JobBus::new(256),
        })
    }
}

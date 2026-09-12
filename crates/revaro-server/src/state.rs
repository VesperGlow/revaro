//! Shared application state.
//!
//! Axum carries exactly one state type, so this is where the process-wide
//! handles live. It is deliberately small: feature modules add their own
//! collaborators here as they are migrated, rather than threading a growing
//! argument list through every handler.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, PoisonError};

use crate::auth::AuthService;
use crate::config::Config;
use crate::db::Database;
use crate::storage::LocalStore;
use revaro_reader::BookCache;

/// Process-wide reader caches and per-book build locks.
///
/// The parsed-book cache bounds memory, while the keyed locks prevent two
/// simultaneous first opens from parsing and writing the same derived flow
/// twice. The lock maps intentionally contain only object-store keys, never
/// request-controlled filesystem paths.
#[derive(Debug)]
pub struct ReaderRuntime {
    /// LRU of parsed EPUB/TXT books.
    pub books: BookCache,
    book_locks: Mutex<HashMap<String, Arc<tokio::sync::Mutex<()>>>>,
    flow_locks: Mutex<HashMap<String, Arc<tokio::sync::Mutex<()>>>>,
}

impl ReaderRuntime {
    /// Create the bounded reader runtime used by the server.
    #[must_use]
    pub fn new() -> Self {
        Self {
            books: BookCache::new(4, 128 << 20),
            book_locks: Mutex::new(HashMap::new()),
            flow_locks: Mutex::new(HashMap::new()),
        }
    }

    /// Serialize cold loads for one object-store key.
    pub async fn book_lock(&self, key: &str) -> tokio::sync::OwnedMutexGuard<()> {
        let lock = self.lock_for(key, &self.book_locks);
        lock.lock_owned().await
    }

    /// Serialize flow generation for one object-store key.
    pub async fn flow_lock(&self, key: &str) -> tokio::sync::OwnedMutexGuard<()> {
        let lock = self.lock_for(key, &self.flow_locks);
        lock.lock_owned().await
    }

    fn lock_for(
        &self,
        key: &str,
        locks: &Mutex<HashMap<String, Arc<tokio::sync::Mutex<()>>>>,
    ) -> Arc<tokio::sync::Mutex<()>> {
        let mut locks = locks.lock().unwrap_or_else(PoisonError::into_inner);
        // The map owns one strong reference. Once no guard or waiter owns the
        // other references, the key is idle and can be removed on the next
        // lookup. This keeps a busy-key optimization from becoming a lifetime
        // sized map as uploads and deletions accumulate.
        locks.retain(|_, lock| Arc::strong_count(lock) > 1);
        locks
            .entry(key.to_owned())
            .or_insert_with(|| Arc::new(tokio::sync::Mutex::new(())))
            .clone()
    }
}

impl Default for ReaderRuntime {
    fn default() -> Self {
        Self::new()
    }
}

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
    /// Native media probing, thumbnail and subtitle coordination.
    pub media: crate::media_runtime::MediaRuntime,
    /// Native archive extraction and task lifecycle coordination.
    pub archive: crate::archive_runtime::ArchiveRuntime,
    /// Short-lived tickets for streaming batch downloads.
    pub batch_download: crate::batch_download::BatchDownloadRuntime,
    /// Process-wide system status snapshot and SSE subscribers.
    pub status: crate::status_routes::StatusRuntime,
    /// Task-change notifications for the event stream.
    pub jobs: JobBus,
    /// Parsed books and serialized reader-flow builders.
    pub reader: ReaderRuntime,
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
        let media_cache_capacity =
            usize::try_from(config.media_cache_capacity.max(0)).unwrap_or(usize::MAX);
        Arc::new(Self {
            config,
            db,
            store,
            auth,
            media: crate::media_runtime::MediaRuntime::with_cache_capacity(media_cache_capacity),
            archive: crate::archive_runtime::ArchiveRuntime::new(),
            batch_download: crate::batch_download::BatchDownloadRuntime::new(),
            status: crate::status_routes::StatusRuntime::new(),
            jobs: JobBus::new(256),
            reader: ReaderRuntime::new(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn reader_lock_maps_discard_idle_keys() {
        let runtime = ReaderRuntime::new();
        {
            let _guard = runtime.book_lock("first").await;
            assert_eq!(runtime.book_locks.lock().unwrap().len(), 1);
        }
        {
            let _guard = runtime.book_lock("second").await;
            let locks = runtime.book_locks.lock().unwrap();
            assert_eq!(locks.len(), 1);
            assert!(locks.contains_key("second"));
        }
    }
}

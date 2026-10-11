//! Shared application state.
//!
//! Axum carries exactly one state type, so this is where the process-wide
//! handles live. It is deliberately small: feature modules add their own
//! collaborators here as they are migrated, rather than threading a growing
//! argument list through every handler.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, PoisonError};

use crate::auth::AuthService;
use crate::cache::CacheManager;
use crate::config::Config;
use crate::db::Database;
use crate::storage::LocalStore;

/// Process-wide reader caches and per-book build locks.
///
/// The shared cache bounds memory, while the keyed locks prevent two
/// simultaneous first opens from parsing and writing the same derived flow
/// twice. The lock maps intentionally contain only object-store keys, never
/// request-controlled filesystem paths.
#[derive(Debug)]
pub struct ReaderRuntime {
    /// Bound peak memory and CPU of different books being parsed or built.
    pub work_slots: Arc<tokio::sync::Semaphore>,
    book_locks: Mutex<HashMap<String, Arc<tokio::sync::Mutex<()>>>>,
    flow_locks: Mutex<HashMap<String, Arc<tokio::sync::Mutex<()>>>>,
}

/// Lifecycle writers (completion, abort, expiry) exclude all part writers.
/// Part writers share the lifecycle lock and serialize only their own part.
#[derive(Debug)]
pub struct UploadRuntime {
    locks: Mutex<HashMap<String, Arc<tokio::sync::RwLock<()>>>>,
    part_locks: Mutex<UploadPartLocks>,
    pub io_slots: Arc<tokio::sync::Semaphore>,
    reserved_bytes: Arc<Mutex<u64>>,
}

type UploadPartLocks = HashMap<(String, i32), Arc<tokio::sync::Mutex<()>>>;

/// Held until both the streamed bytes and their acknowledgement are durable.
pub struct UploadPartGuard {
    _lifecycle: tokio::sync::OwnedRwLockReadGuard<()>,
    _part: tokio::sync::OwnedMutexGuard<()>,
}

/// Conservative admission budget for a currently active filesystem write.
/// The filesystem may also count bytes already written by another reservation;
/// counting those twice is safer than admitting writers past the reserve.
pub struct UploadSpaceGuard {
    reserved_bytes: Arc<Mutex<u64>>,
    bytes: u64,
}

impl Drop for UploadSpaceGuard {
    fn drop(&mut self) {
        let mut reserved = self
            .reserved_bytes
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        *reserved -= self.bytes;
    }
}

impl Default for UploadRuntime {
    fn default() -> Self {
        Self::with_concurrency(8)
    }
}

impl UploadRuntime {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    #[must_use]
    pub fn with_concurrency(concurrency: usize) -> Self {
        Self {
            locks: Mutex::new(HashMap::new()),
            part_locks: Mutex::new(HashMap::new()),
            io_slots: Arc::new(tokio::sync::Semaphore::new(concurrency)),
            reserved_bytes: Arc::new(Mutex::new(0)),
        }
    }

    fn lifecycle(&self, upload_id: &str) -> Arc<tokio::sync::RwLock<()>> {
        let mut locks = self.locks.lock().unwrap_or_else(PoisonError::into_inner);
        locks.retain(|_, lock| Arc::strong_count(lock) > 1);
        locks.entry(upload_id.to_owned()).or_default().clone()
    }

    pub async fn lock(&self, upload_id: &str) -> tokio::sync::OwnedRwLockWriteGuard<()> {
        self.lifecycle(upload_id).write_owned().await
    }

    pub async fn lock_part(&self, upload_id: &str, part: i32) -> UploadPartGuard {
        let lifecycle = self.lifecycle(upload_id).read_owned().await;
        let lock = {
            let mut locks = self
                .part_locks
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            locks.retain(|_, lock| Arc::strong_count(lock) > 1);
            locks
                .entry((upload_id.to_owned(), part))
                .or_default()
                .clone()
        };
        UploadPartGuard {
            _lifecycle: lifecycle,
            _part: lock.lock_owned().await,
        }
    }

    pub fn reserve_space(
        &self,
        bytes: u64,
        minimum: u64,
        available_space: impl FnOnce() -> std::io::Result<u64>,
    ) -> std::io::Result<Option<UploadSpaceGuard>> {
        let mut reserved = self
            .reserved_bytes
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        // Take the filesystem snapshot while holding the budget lock. Otherwise
        // a completed writer could release its reservation after the snapshot,
        // letting the next writer spend free bytes which were already consumed.
        let available = available_space()?;
        if available < bytes.saturating_add(*reserved).saturating_add(minimum) {
            return Ok(None);
        }
        *reserved += bytes;
        Ok(Some(UploadSpaceGuard {
            reserved_bytes: Arc::clone(&self.reserved_bytes),
            bytes,
        }))
    }
}

impl ReaderRuntime {
    /// Create the bounded reader runtime used by the server.
    #[must_use]
    pub fn new() -> Self {
        Self {
            work_slots: Arc::new(tokio::sync::Semaphore::new(2)),
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

/// Everything a handler can reach.
#[derive(Debug)]
pub struct AppState {
    /// Response budgets with reserved capacity for interactive readers.
    pub delivery: crate::delivery::DeliveryRuntime,
    /// Process configuration.
    pub config: Arc<Config>,
    pub memory: crate::memory::MemoryBudget,
    /// The migrated SQLite database.
    pub db: Database,
    /// The local object store holding original bytes and derived artifacts.
    pub store: LocalStore,
    /// Replaceable source for original-file HTTP reads, independent of writes
    /// and native parsing engines that currently require the local store.
    pub files: Arc<dyn crate::file_access::FileAccess>,
    /// Administrator credentials, sessions and second factor.
    pub auth: AuthService,
    /// Process-wide L1/L2 cache policy and statistics.
    pub cache: CacheManager,
    /// Native media probing and thumbnail coordination.
    pub media: crate::media_runtime::MediaRuntime,
    /// Short-lived tickets for streaming batch downloads.
    pub batch_download: crate::batch_download::BatchDownloadRuntime,
    /// Admission budgets held for the entire transfer or ZIP generation.
    pub share_slots: Arc<tokio::sync::Semaphore>,
    pub zip_slots: Arc<tokio::sync::Semaphore>,
    /// Parsed books and serialized reader-flow builders.
    pub reader: ReaderRuntime,
    /// Serialized upload lifecycle operations.
    pub uploads: UploadRuntime,
    /// Process-wide maintenance scheduler and durable cleanup passes.
    pub maintenance: crate::maintenance::MaintenanceRuntime,
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
        let files = Arc::new(store.clone());
        Self::with_file_access(config, db, store, auth, files)
    }

    /// Select an original-file read provider while retaining existing local
    /// upload, archive generation and parser lifecycles during migration.
    #[must_use]
    pub fn with_file_access(
        config: Arc<Config>,
        db: Database,
        store: LocalStore,
        auth: AuthService,
        files: Arc<dyn crate::file_access::FileAccess>,
    ) -> Arc<Self> {
        let reader = ReaderRuntime::new();
        let memory = crate::memory::MemoryBudget::detect(config.memory_budget);
        tracing::info!(
            requested_bytes = config.memory_budget,
            total_bytes = memory.total_bytes,
            cache_bytes = memory.cache_bytes,
            database_bytes = memory.database_bytes,
            "memory budget ready"
        );
        let cache = CacheManager::for_app(
            &config.caches_dir,
            memory.cache_bytes,
            config.media_cache_capacity,
        );
        let files = Arc::new(crate::file_access::CachedFileAccess::new(
            files,
            cache.clone(),
        ));
        let maintenance = crate::maintenance::MaintenanceRuntime::new();
        let uploads = UploadRuntime::with_concurrency(config.upload_concurrency);
        let state = Arc::new(Self {
            delivery: crate::delivery::DeliveryRuntime::default(),
            config,
            memory,
            db,
            files,
            store,
            auth,
            cache,
            media: crate::media_runtime::MediaRuntime::new(),
            batch_download: crate::batch_download::BatchDownloadRuntime::new(),
            share_slots: Arc::new(tokio::sync::Semaphore::new(8)),
            zip_slots: Arc::new(tokio::sync::Semaphore::new(2)),
            reader,
            uploads,
            maintenance,
        });
        state
            .maintenance
            .register_production(Arc::downgrade(&state));
        state
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

    #[tokio::test]
    async fn upload_locks_prune_idle_keys() {
        let runtime = UploadRuntime::new();
        {
            let _guard = runtime.lock("first").await;
            assert_eq!(runtime.locks.lock().unwrap().len(), 1);
        }
        {
            let _guard = runtime.lock("second").await;
            let locks = runtime.locks.lock().unwrap();
            assert_eq!(locks.len(), 1);
            assert!(locks.contains_key("second"));
        }
    }

    #[tokio::test]
    async fn concurrent_operations_for_one_upload_wait_for_each_other() {
        let runtime = Arc::new(UploadRuntime::new());
        let guard = runtime.lock("same-upload").await;
        let (finished, mut finished_rx) = tokio::sync::oneshot::channel();
        let waiting = Arc::clone(&runtime);
        let task = tokio::spawn(async move {
            let _guard = waiting.lock("same-upload").await;
            let _ = finished.send(());
        });

        tokio::task::yield_now().await;
        assert!(matches!(
            finished_rx.try_recv(),
            Err(tokio::sync::oneshot::error::TryRecvError::Empty)
        ));
        drop(guard);
        finished_rx.await.expect("waiting operation completes");
        task.await.expect("waiting task does not panic");
    }

    #[test]
    fn concurrent_upload_space_is_reserved_and_released() {
        let runtime = UploadRuntime::new();
        let first = runtime.reserve_space(60, 10, || Ok(100)).unwrap().unwrap();
        assert!(runtime.reserve_space(40, 10, || Ok(100)).unwrap().is_none());
        let second = runtime.reserve_space(30, 10, || Ok(100)).unwrap().unwrap();
        assert!(runtime.reserve_space(1, 10, || Ok(100)).unwrap().is_none());
        drop(first);
        assert!(runtime.reserve_space(60, 10, || Ok(100)).unwrap().is_some());
        drop(second);
        assert_eq!(*runtime.reserved_bytes.lock().unwrap(), 0);
    }
}

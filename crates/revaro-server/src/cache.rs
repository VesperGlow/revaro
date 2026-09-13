//! The process-wide cache manager.
//!
//! Revaro has several kinds of derived data with different lifetimes: parsed
//! books are objects owned by the reader crate, flow bytes are immutable
//! memory entries, source books are useful across restarts, and converted
//! subtitles expire. Keeping the policy here makes those choices visible in
//! one place and lets every managed class share the same memory and disk
//! budgets.
//!
//! The disk tier is deliberately a rebuildable cache. Each data file has a
//! sibling `.meta` file containing `class\0key\n<expires-unix-nanos>`; a
//! missing or malformed pair is discarded during reconciliation. Cache keys
//! never become path components, so user supplied names cannot escape the
//! cache directory.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::fmt::{self, Display, Formatter};
use std::future::Future;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use revaro_core::api::system;
use revaro_core::hash::Sha256;
use revaro_reader::BookCache;
use tokio::io::AsyncWriteExt as _;
use tokio::sync::{Mutex as AsyncMutex, watch};
use tokio_util::sync::CancellationToken;

/// Global managed memory budget, matching the production cache policy.
pub const MEMORY_LIMIT: i64 = 96 << 20;
/// Flow manifest cache class.
pub const READER_FLOW_MANIFEST: &str = "reader/flow-manifest";
/// Flow chunk cache class.
pub const READER_FLOW_CHUNK: &str = "reader/flow-chunk";
/// Original book source cache class.
pub const READER_SOURCE: &str = "reader/source";
/// External parsed-book cache class.
pub const READER_BOOKS: &str = "reader/books";
/// Converted subtitle cache class.
pub const MEDIA_SUBTITLE: &str = "media/subtitle";

/// The tier and eviction policy for one cache namespace.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CacheClass {
    /// Stable namespace used in statistics and disk metadata.
    pub name: String,
    /// Larger values survive global pressure longer.
    pub priority: i32,
    /// Soft byte quota for this class; zero means no class quota.
    pub soft_quota: i64,
    /// Whether values may be stored in the memory tier.
    pub memory: bool,
    /// Whether values may be stored in the disk tier.
    pub disk: bool,
    /// Optional per-entry bound. Oversized values are returned to the caller
    /// but are not admitted to either tier.
    pub max_entry: Option<i64>,
}

/// Why a loader failed. The cache does not know feature-specific error types,
/// but callers still need to preserve important HTTP distinctions such as a
/// missing reader object or an oversized subtitle.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CacheLoadKind {
    /// The source object is absent.
    NotFound,
    /// The source or derived result crossed an input/output bound.
    TooLarge,
    /// Any other loader failure.
    Other,
}

/// A loader failure that can safely cross a singleflight boundary.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CacheLoadError {
    kind: CacheLoadKind,
    message: String,
}

impl CacheLoadError {
    /// Construct a classified loader failure.
    #[must_use]
    pub fn new(kind: CacheLoadKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
        }
    }

    /// Construct an ordinary loader failure.
    pub fn other(message: impl Into<String>) -> Self {
        Self::new(CacheLoadKind::Other, message)
    }

    /// Construct a missing-source failure.
    pub fn not_found(message: impl Into<String>) -> Self {
        Self::new(CacheLoadKind::NotFound, message)
    }

    /// Construct a size-bound failure.
    pub fn too_large(message: impl Into<String>) -> Self {
        Self::new(CacheLoadKind::TooLarge, message)
    }

    /// The classification selected by the feature loader.
    #[must_use]
    pub fn kind(&self) -> CacheLoadKind {
        self.kind
    }

    /// The original feature-level message.
    #[must_use]
    pub fn message(&self) -> &str {
        &self.message
    }
}

impl Display for CacheLoadError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for CacheLoadError {}

/// Errors raised by cache policy or cache storage. A cache failure never
/// invalidates the source data; feature callers can fall back to their source
/// path after a disk error.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum CacheError {
    /// A feature used a class that was not registered during startup.
    #[error("cache class is not registered: {0}")]
    UnknownClass(String),
    /// A class name was registered more than once.
    #[error("cache class is already registered: {0}")]
    DuplicateClass(String),
    /// A class or key cannot be represented by the disk metadata format.
    #[error("cache class or key contains a forbidden control character")]
    InvalidKey,
    /// No new singleflight load may start after shutdown.
    #[error("cache is closed")]
    Closed,
    /// The shared source loader failed.
    #[error("cache loader failed: {0}")]
    Loader(CacheLoadError),
    /// The rebuildable disk tier could not be updated.
    #[error("cache storage error: {0}")]
    Storage(String),
}

impl CacheError {
    /// Whether this error means that a durable flow object is absent.
    #[must_use]
    pub fn is_loader_not_found(&self) -> bool {
        matches!(self, Self::Loader(error) if error.kind() == CacheLoadKind::NotFound)
    }

    /// Return the feature loader error, when this is a loader failure.
    #[must_use]
    pub fn loader_error(&self) -> Option<&CacheLoadError> {
        match self {
            Self::Loader(error) => Some(error),
            _ => None,
        }
    }
}

/// Cumulative counters and current usage for one namespace.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CacheClassStats {
    /// Number of lookups served by either tier.
    pub hits: i64,
    /// Number of lookups that reached a loader.
    pub misses: i64,
    /// Number of loader executions.
    pub loads: i64,
    /// Number of failed loader executions.
    pub load_errors: i64,
    /// Number of entries evicted by pressure or expiry.
    pub evictions: i64,
    /// Current memory bytes.
    pub memory_bytes: i64,
    /// Current memory entry count.
    pub memory_entries: i64,
    /// Current disk bytes.
    pub disk_bytes: i64,
    /// Current disk entry count.
    pub disk_entries: i64,
}

/// A consistent cache snapshot used by tests and the status endpoint.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CacheStats {
    /// Per-class snapshots, in deterministic name order.
    pub classes: BTreeMap<String, CacheClassStats>,
    /// Managed and external memory bytes.
    pub memory_bytes: i64,
    /// Managed and external disk bytes.
    pub disk_bytes: i64,
    /// Managed and external memory entries.
    pub memory_entries: i64,
    /// Managed and external disk entries.
    pub disk_entries: i64,
}

#[derive(Debug, Clone, Copy, Default)]
struct Counters {
    hits: i64,
    misses: i64,
    loads: i64,
    load_errors: i64,
    evictions: i64,
}

#[derive(Debug, Clone)]
struct ClassState {
    config: CacheClass,
    counters: Counters,
    memory_bytes: i64,
    memory_entries: i64,
    disk_bytes: i64,
    disk_entries: i64,
}

#[derive(Debug)]
struct MemoryEntry {
    class: String,
    key: String,
    data: Vec<u8>,
    expires: Option<SystemTime>,
    accessed: u64,
}

#[derive(Debug, Clone)]
struct DiskEntry {
    class: String,
    key: String,
    path: PathBuf,
    size: i64,
    modified: SystemTime,
    expires: Option<SystemTime>,
    accessed: u64,
}

type LoadResult = Result<Vec<u8>, CacheLoadError>;

#[derive(Debug)]
struct Flight {
    sender: watch::Sender<Option<Arc<LoadResult>>>,
}

#[derive(Debug, Default)]
struct State {
    classes: BTreeMap<String, ClassState>,
    memory: HashMap<String, MemoryEntry>,
    disk: HashMap<String, DiskEntry>,
    flights: HashMap<String, Arc<Flight>>,
    memory_bytes: i64,
    memory_entries: i64,
    disk_bytes: i64,
    disk_entries: i64,
    access_clock: u64,
    closed: bool,
}

#[derive(Debug)]
struct CacheInner {
    state: Mutex<State>,
    disk_io: AsyncMutex<()>,
    cache_dir: PathBuf,
    memory_limit: i64,
    disk_limit: i64,
    external_books: Option<Arc<BookCache>>,
    healthy: AtomicBool,
    shutdown: CancellationToken,
}

/// A process-wide, concurrent L1/L2 cache manager.
#[derive(Clone, Debug)]
pub struct CacheManager {
    inner: Arc<CacheInner>,
}

impl CacheManager {
    /// Create an empty manager for unit tests or an embedded caller.
    #[must_use]
    pub fn new(cache_dir: impl AsRef<Path>, memory_limit: i64, disk_limit: i64) -> Self {
        Self::new_with_external(
            cache_dir.as_ref().to_path_buf(),
            memory_limit,
            disk_limit,
            None,
        )
    }

    /// Create the production class registry and register the parsed-book LRU
    /// as an external memory provider.
    #[must_use]
    pub fn for_app(work_dir: impl AsRef<Path>, disk_limit: i64, books: Arc<BookCache>) -> Self {
        let manager = Self::new_with_external(
            work_dir.as_ref().join("cache"),
            MEMORY_LIMIT,
            disk_limit,
            Some(books),
        );
        let classes = [
            CacheClass {
                name: READER_FLOW_MANIFEST.to_owned(),
                priority: 90,
                soft_quota: 8 << 20,
                memory: true,
                disk: false,
                max_entry: Some(8 << 20),
            },
            CacheClass {
                name: READER_FLOW_CHUNK.to_owned(),
                priority: 70,
                soft_quota: 64 << 20,
                memory: true,
                disk: false,
                max_entry: Some(8 << 20),
            },
            CacheClass {
                name: READER_SOURCE.to_owned(),
                priority: 40,
                soft_quota: 512 << 20,
                memory: false,
                disk: true,
                max_entry: Some(64 << 20),
            },
            CacheClass {
                name: READER_BOOKS.to_owned(),
                priority: 60,
                soft_quota: 128 << 20,
                memory: true,
                disk: false,
                max_entry: None,
            },
            CacheClass {
                name: MEDIA_SUBTITLE.to_owned(),
                priority: 20,
                soft_quota: 64 << 20,
                memory: true,
                disk: true,
                max_entry: Some(32 << 20),
            },
        ];
        for class in classes {
            // These definitions are compile-time constants. A failure here
            // would indicate a programming error, while disk availability is
            // reported through `healthy` and remains a runtime concern.
            let _ = manager.register_class(class);
        }
        manager.reconcile_disk_startup();
        manager
    }

    fn new_with_external(
        cache_dir: PathBuf,
        memory_limit: i64,
        disk_limit: i64,
        external_books: Option<Arc<BookCache>>,
    ) -> Self {
        let healthy = prepare_cache_dir(&cache_dir).is_ok();
        Self {
            inner: Arc::new(CacheInner {
                state: Mutex::new(State::default()),
                disk_io: AsyncMutex::new(()),
                cache_dir,
                memory_limit,
                disk_limit,
                external_books,
                healthy: AtomicBool::new(healthy),
                shutdown: CancellationToken::new(),
            }),
        }
    }

    /// Register a class before it is used.
    pub fn register_class(&self, class: CacheClass) -> Result<(), CacheError> {
        validate_component(&class.name)?;
        let mut state = self.lock_state();
        if state.classes.contains_key(&class.name) {
            return Err(CacheError::DuplicateClass(class.name));
        }
        state.classes.insert(
            class.name.clone(),
            ClassState {
                config: class,
                counters: Counters::default(),
                memory_bytes: 0,
                memory_entries: 0,
                disk_bytes: 0,
                disk_entries: 0,
            },
        );
        Ok(())
    }

    /// Read the memory tier and count this as one cache lookup.
    pub fn get(&self, class: &str, key: &str) -> Result<Option<Vec<u8>>, CacheError> {
        let mut state = self.lock_state();
        let class_config = state
            .classes
            .get(class)
            .map(|class| class.config.clone())
            .ok_or_else(|| CacheError::UnknownClass(class.to_owned()))?;
        let qualified = qualified_key(class, key)?;
        if !class_config.memory {
            state.class_miss(class);
            return Ok(None);
        }
        let Some(entry) = state.memory.get(&qualified) else {
            state.class_miss(class);
            return Ok(None);
        };
        if expired(entry.expires) {
            let entry = state.memory.remove(&qualified).expect("entry was present");
            state.remove_memory_accounting(&entry);
            state.class_evict(class);
            state.class_miss(class);
            return Ok(None);
        }
        let data = entry.data.clone();
        let next = state.next_access();
        if let Some(entry) = state.memory.get_mut(&qualified) {
            entry.accessed = next;
        }
        state.class_hit(class);
        Ok(Some(data))
    }

    /// Report whether a non-expired entry exists in either configured tier.
    /// This reads only the reconciled disk index and never scans the directory.
    pub fn has(&self, class: &str, key: &str) -> Result<bool, CacheError> {
        let mut state = self.lock_state();
        let class_config = state
            .classes
            .get(class)
            .map(|class| class.config.clone())
            .ok_or_else(|| CacheError::UnknownClass(class.to_owned()))?;
        let qualified = qualified_key(class, key)?;
        if class_config.memory && state.memory.contains_key(&qualified) {
            if state
                .memory
                .get(&qualified)
                .is_some_and(|entry| !expired(entry.expires))
            {
                return Ok(true);
            }
            if let Some(entry) = state.memory.remove(&qualified) {
                state.remove_memory_accounting(&entry);
            }
        }
        if !class_config.disk {
            return Ok(false);
        }
        if state
            .disk
            .get(&qualified)
            .is_some_and(|entry| !expired(entry.expires))
        {
            return Ok(true);
        }
        if let Some(entry) = state.disk.remove(&qualified) {
            state.remove_disk_accounting(&entry);
        }
        Ok(false)
    }

    /// Load through memory, disk, and one detached source loader. A cancelled
    /// waiter stops waiting, while the shared loader continues for other
    /// callers and can still populate the cache.
    pub async fn load<F, Fut>(
        &self,
        class: &str,
        key: &str,
        ttl: Duration,
        loader: F,
    ) -> Result<Vec<u8>, CacheError>
    where
        F: FnOnce() -> Fut + Send + 'static,
        Fut: Future<Output = Result<Vec<u8>, CacheLoadError>> + Send + 'static,
    {
        let class_config = self.class_config(class, key)?;
        if class_config.memory {
            let mut state = self.lock_state();
            if let Some(data) = state.get_memory(class, key)? {
                state.class_hit(class);
                return Ok(data);
            }
        }
        if class_config.disk
            && let Some((data, expires)) = self.get_disk(class, key).await
        {
            let mut state = self.lock_state();
            state.class_hit(class);
            if class_config.memory {
                state.put_memory(class, key, data.clone(), expires);
                state.enforce_memory(self.inner.memory_limit);
            }
            return Ok(data);
        }

        let (flight, receiver, leader) = {
            let mut state = self.lock_state();
            state.class_miss(class);
            if state.closed {
                return Err(CacheError::Closed);
            }
            let qualified = qualified_key(class, key)?;
            if let Some(flight) = state.flights.get(&qualified) {
                (Arc::clone(flight), flight.sender.subscribe(), false)
            } else {
                let (sender, receiver) = watch::channel(None);
                let flight = Arc::new(Flight { sender });
                state.flights.insert(qualified, Arc::clone(&flight));
                (flight, receiver, true)
            }
        };

        if leader {
            let manager = self.clone();
            let class = class.to_owned();
            let key = key.to_owned();
            let shutdown = self.inner.shutdown.clone();
            tokio::spawn(async move {
                let mut loader_task = tokio::spawn(async move { loader().await });
                let result = tokio::select! {
                    result = &mut loader_task => match result {
                        Ok(result) => result,
                        Err(error) => Err(CacheLoadError::other(format!(
                            "cache loader task failed: {error}"
                        ))),
                    },
                    _ = shutdown.cancelled() => {
                        // Dropping a native media future runs its cancellation
                        // guard. Abort the nested task before notifying waiters.
                        loader_task.abort();
                        Err(CacheLoadError::other("cache is closed"))
                    },
                };
                manager.finish_load(class, key, ttl, flight, result).await;
            });
        }

        wait_for_flight(receiver).await
    }

    /// Write an entry to every tier allowed by its class. Disk failure is
    /// returned to direct callers but never turns a successful source load
    /// into a feature failure.
    pub async fn put(
        &self,
        class: &str,
        key: &str,
        data: &[u8],
        ttl: Duration,
    ) -> Result<(), CacheError> {
        let class_config = self.class_config(class, key)?;
        if class_config
            .max_entry
            .is_some_and(|limit| i64::try_from(data.len()).unwrap_or(i64::MAX) > limit)
        {
            return Ok(());
        }
        let expires = (ttl > Duration::ZERO).then(|| SystemTime::now() + ttl);
        {
            let mut state = self.lock_state();
            if class_config.memory {
                state.put_memory(class, key, data.to_vec(), expires);
                state.enforce_memory(self.inner.memory_limit);
            }
        }
        if class_config.disk {
            self.put_disk(class, key, data, expires).await?;
        }
        Ok(())
    }

    /// Remove one logical entry from both tiers.
    pub async fn delete(&self, class: &str, key: &str) -> Result<(), CacheError> {
        let class_config = self.class_config(class, key)?;
        let qualified = qualified_key(class, key)?;
        {
            let mut state = self.lock_state();
            if let Some(entry) = state.memory.remove(&qualified) {
                state.remove_memory_accounting(&entry);
            }
        }
        if !class_config.disk {
            return Ok(());
        }
        let _disk_guard = self.inner.disk_io.lock().await;
        let path = self.disk_path(class, key);
        remove_file_if_present(&path).map_err(|error| self.storage_error(error))?;
        remove_file_if_present(&meta_path(&path)).map_err(|error| self.storage_error(error))?;
        let mut state = self.lock_state();
        if state
            .disk
            .get(&qualified)
            .is_some_and(|entry| entry.path == path)
            && let Some(entry) = state.disk.remove(&qualified)
        {
            state.remove_disk_accounting(&entry);
        }
        Ok(())
    }

    /// Remove every entry whose logical key starts with `prefix`.
    pub async fn invalidate(&self, prefix: &str) -> Result<(), CacheError> {
        let keys = {
            let mut state = self.lock_state();
            let mut keys = Vec::new();
            let memory_keys: Vec<_> = state
                .memory
                .values()
                .filter(|entry| key_matches(prefix, &entry.class, &entry.key))
                .map(|entry| qualified_key_unchecked(&entry.class, &entry.key))
                .collect();
            for qualified in memory_keys {
                if let Some(entry) = state.memory.remove(&qualified) {
                    state.remove_memory_accounting(&entry);
                }
            }
            for entry in state.disk.values() {
                if key_matches(prefix, &entry.class, &entry.key) {
                    keys.push((entry.class.clone(), entry.key.clone()));
                }
            }
            keys
        };
        for (class, key) in keys {
            self.delete(&class, &key).await?;
        }
        Ok(())
    }

    /// Reconcile the disk index, enforce class and global budgets, and trim
    /// the external parsed-book cache to the remaining global memory budget.
    pub async fn prune(&self) -> Result<(), CacheError> {
        let disk_result = self.prune_disk().await;
        let external_bytes = if let Some(books) = &self.inner.external_books {
            let managed = self.lock_state().memory_bytes;
            let target = if self.inner.memory_limit > 0 {
                (self.inner.memory_limit - managed).max(0)
            } else {
                -1
            };
            books.trim_to(target);
            books.stats().0
        } else {
            0
        };
        if self.inner.memory_limit > 0 {
            let target = (self.inner.memory_limit - external_bytes).max(0);
            let mut state = self.lock_state();
            state.remove_expired_memory();
            state.evict_memory_to(target);
        } else {
            self.lock_state().remove_expired_memory();
        }
        disk_result
    }

    /// Stop accepting new loads and wait for every detached loader already in
    /// flight. Existing `put`, `delete` and `prune` operations remain usable so
    /// shutdown can finish durable cleanup.
    pub async fn close(&self) {
        let flights = {
            let mut state = self.lock_state();
            state.closed = true;
            state.flights.values().cloned().collect::<Vec<_>>()
        };
        self.inner.shutdown.cancel();
        for flight in flights {
            let receiver = flight.sender.subscribe();
            let _ = wait_for_flight(receiver).await;
        }
    }

    /// Whether the cache directory and its last reconciliation were healthy.
    #[must_use]
    pub fn is_healthy(&self) -> bool {
        self.inner.healthy.load(Ordering::Acquire)
    }

    /// Return counters and current tier usage without scanning the disk.
    #[must_use]
    pub fn stats(&self) -> CacheStats {
        let state = self.lock_state();
        let mut output = CacheStats::default();
        for (name, class) in &state.classes {
            output.classes.insert(
                name.clone(),
                CacheClassStats {
                    hits: class.counters.hits,
                    misses: class.counters.misses,
                    loads: class.counters.loads,
                    load_errors: class.counters.load_errors,
                    evictions: class.counters.evictions,
                    memory_bytes: class.memory_bytes,
                    memory_entries: class.memory_entries,
                    disk_bytes: class.disk_bytes,
                    disk_entries: class.disk_entries,
                },
            );
        }
        output.memory_bytes = state.memory_bytes;
        output.memory_entries = state.memory_entries;
        output.disk_bytes = state.disk_bytes;
        output.disk_entries = state.disk_entries;
        drop(state);

        if let Some(books) = &self.inner.external_books {
            let (bytes, entries) = books.stats();
            let class = output.classes.entry(READER_BOOKS.to_owned()).or_default();
            class.memory_bytes = bytes.max(0);
            class.memory_entries = i64::try_from(entries).unwrap_or(i64::MAX);
            output.memory_bytes = output.memory_bytes.saturating_add(bytes.max(0));
            output.memory_entries = output
                .memory_entries
                .saturating_add(i64::try_from(entries).unwrap_or(i64::MAX));
        }
        output
    }

    /// Convert the current counters to the public system-status shape.
    #[must_use]
    pub fn system_status(&self) -> system::Cache {
        let stats = self.stats();
        let classes: BTreeMap<_, _> = stats
            .classes
            .into_iter()
            .map(|(name, class)| {
                (
                    name,
                    system::CacheClass {
                        hits: class.hits,
                        misses: class.misses,
                        loads: class.loads,
                        load_errors: class.load_errors,
                        evictions: class.evictions,
                        memory_bytes: class.memory_bytes,
                        memory_entries: class.memory_entries,
                        disk_bytes: class.disk_bytes,
                        disk_entries: class.disk_entries,
                    },
                )
            })
            .collect();
        system::Cache {
            status: if self.is_healthy() {
                "ok".to_owned()
            } else {
                "degraded".to_owned()
            },
            memory_bytes: stats.memory_bytes,
            disk_bytes: stats.disk_bytes,
            memory_entries: stats.memory_entries,
            disk_entries: stats.disk_entries,
            classes: (!classes.is_empty()).then_some(classes),
        }
    }

    fn class_config(&self, class: &str, key: &str) -> Result<CacheClass, CacheError> {
        validate_key(key)?;
        let state = self.lock_state();
        state
            .classes
            .get(class)
            .map(|state| state.config.clone())
            .ok_or_else(|| CacheError::UnknownClass(class.to_owned()))
    }

    fn lock_state(&self) -> std::sync::MutexGuard<'_, State> {
        self.inner
            .state
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
    }

    fn storage_error(&self, error: std::io::Error) -> CacheError {
        self.inner.healthy.store(false, Ordering::Release);
        CacheError::Storage(error.to_string())
    }

    fn reconcile_disk_startup(&self) {
        if let Err(error) = self.reconcile_disk_sync() {
            tracing::warn!(%error, path = %self.inner.cache_dir.display(), "cache startup reconciliation failed");
        }
    }

    fn reconcile_disk_sync(&self) -> Result<(), CacheError> {
        let entries =
            std::fs::read_dir(&self.inner.cache_dir).map_err(|error| self.storage_error(error))?;
        let now = SystemTime::now();
        let old = {
            let state = self.lock_state();
            state.disk.clone()
        };
        let mut data_names = HashSet::new();
        let mut items = Vec::new();
        for entry in entries {
            let entry = entry.map_err(|error| self.storage_error(error))?;
            let name = entry.file_name().to_string_lossy().into_owned();
            if name.ends_with(".meta") {
                continue;
            }
            if name.starts_with(".cache-") {
                let _ = std::fs::remove_file(entry.path());
                continue;
            }
            let Some(digest) = name.strip_suffix(".cache") else {
                continue;
            };
            data_names.insert(name.clone());
            let path = entry.path();
            let metadata = std::fs::symlink_metadata(entry.path())
                .map_err(|error| self.storage_error(error))?;
            if !metadata.is_file()
                || metadata.file_type().is_symlink()
                || digest.len() != 16
                || !is_hex(digest)
            {
                discard_pair(&path);
                continue;
            }
            let Some((class, key, expires)) = read_disk_meta(&path) else {
                discard_pair(&path);
                continue;
            };
            let class_config = {
                let state = self.lock_state();
                state.classes.get(&class).map(|state| state.config.clone())
            };
            if class_config.as_ref().is_none_or(|class| {
                !class.disk
                    || disk_name(&class.name, &key) != digest
                    || class.max_entry.is_some_and(|limit| {
                        i64::try_from(metadata.len()).unwrap_or(i64::MAX) > limit
                    })
            }) {
                discard_pair(&path);
                continue;
            }
            if expires.is_some_and(|expires| now >= expires) {
                discard_pair(&path);
                continue;
            }
            let qualified = qualified_key_unchecked(&class, &key);
            let accessed = old
                .get(&qualified)
                .filter(|old| old.path == path)
                .map_or(0, |old| old.accessed);
            items.push(DiskEntry {
                class,
                key,
                path,
                size: i64::try_from(metadata.len()).unwrap_or(i64::MAX),
                modified: metadata.modified().unwrap_or(UNIX_EPOCH),
                expires,
                accessed,
            });
        }
        for entry in
            std::fs::read_dir(&self.inner.cache_dir).map_err(|error| self.storage_error(error))?
        {
            let entry = entry.map_err(|error| self.storage_error(error))?;
            let name = entry.file_name().to_string_lossy().into_owned();
            if let Some(data_name) = name.strip_suffix(".meta")
                && !data_names.contains(data_name)
            {
                let _ = std::fs::remove_file(entry.path());
            }
        }
        self.replace_disk(items);
        self.inner.healthy.store(true, Ordering::Release);
        Ok(())
    }

    async fn get_disk(&self, class: &str, key: &str) -> Option<(Vec<u8>, Option<SystemTime>)> {
        let _disk_guard = self.inner.disk_io.lock().await;
        let qualified = qualified_key_unchecked(class, key);
        let entry = {
            let state = self.lock_state();
            state.disk.get(&qualified).cloned()
        }?;
        if entry
            .expires
            .is_some_and(|expires| SystemTime::now() >= expires)
        {
            let _ = self.remove_disk_files_async(&entry, true).await;
            return None;
        }
        let metadata = match tokio::fs::symlink_metadata(&entry.path).await {
            Ok(metadata) if metadata.is_file() => metadata,
            Ok(_) => {
                let _ = self.remove_disk_files_async(&entry, true).await;
                return None;
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                let _ = self.remove_disk_files_async(&entry, true).await;
                return None;
            }
            Err(error) => {
                self.inner.healthy.store(false, Ordering::Release);
                tracing::warn!(%error, path = %entry.path.display(), "cache disk read stat failed");
                return None;
            }
        };
        let max_entry = {
            let state = self.lock_state();
            state
                .classes
                .get(class)
                .and_then(|class| class.config.max_entry)
        };
        if max_entry.is_some_and(|limit| i64::try_from(metadata.len()).unwrap_or(i64::MAX) > limit)
        {
            let _ = self.remove_disk_files_async(&entry, true).await;
            return None;
        }
        let data = match tokio::fs::read(&entry.path).await {
            Ok(data) => data,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                let _ = self.remove_disk_files_async(&entry, true).await;
                return None;
            }
            Err(error) => {
                self.inner.healthy.store(false, Ordering::Release);
                tracing::warn!(%error, path = %entry.path.display(), "cache disk read failed");
                return None;
            }
        };
        if data.len() as u64 != metadata.len() {
            let _ = self.remove_disk_files_async(&entry, true).await;
            return None;
        }
        let mut state = self.lock_state();
        let actual = i64::try_from(metadata.len()).unwrap_or(i64::MAX);
        let next_access = state.next_access();
        let old_size = state
            .disk
            .get(&qualified)
            .filter(|current| current.path == entry.path)
            .map(|current| current.size);
        if let Some(old_size) = old_size {
            if actual != old_size {
                state.adjust_disk_size(class, old_size, actual);
            }
            if let Some(current) = state.disk.get_mut(&qualified) {
                current.size = actual;
                current.accessed = next_access;
            }
        }
        Some((data, entry.expires))
    }

    async fn put_disk(
        &self,
        class: &str,
        key: &str,
        data: &[u8],
        expires: Option<SystemTime>,
    ) -> Result<(), CacheError> {
        let _disk_guard = self.inner.disk_io.lock().await;
        prepare_cache_dir(&self.inner.cache_dir).map_err(|error| self.storage_error(error))?;
        let path = self.disk_path(class, key);
        let temp = self
            .inner
            .cache_dir
            .join(format!(".cache-data-{}", crate::ids::new_id()));
        let meta_temp = self
            .inner
            .cache_dir
            .join(format!(".cache-meta-{}", crate::ids::new_id()));
        let write_result = async {
            let mut file = tokio::fs::OpenOptions::new()
                .create_new(true)
                .write(true)
                .open(&temp)
                .await?;
            file.write_all(data).await?;
            file.flush().await?;
            file.sync_all().await?;
            tokio::fs::set_permissions(&temp, permissions_0600()).await?;
            tokio::fs::rename(&temp, &path).await?;

            let expires = expires.map_or(0, system_time_nanos);
            let meta = format!("{}\0{}\n{expires}", class, key);
            let mut meta_file = tokio::fs::OpenOptions::new()
                .create_new(true)
                .write(true)
                .open(&meta_temp)
                .await?;
            meta_file.write_all(meta.as_bytes()).await?;
            meta_file.flush().await?;
            meta_file.sync_all().await?;
            tokio::fs::set_permissions(&meta_temp, permissions_0600()).await?;
            tokio::fs::rename(&meta_temp, meta_path(&path)).await?;
            Ok::<(), std::io::Error>(())
        }
        .await;
        let _ = tokio::fs::remove_file(&temp).await;
        let _ = tokio::fs::remove_file(&meta_temp).await;
        if let Err(error) = write_result {
            self.inner.healthy.store(false, Ordering::Release);
            return Err(self.storage_error(error));
        }
        let metadata = tokio::fs::metadata(&path)
            .await
            .map_err(|error| self.storage_error(error))?;
        let item = DiskEntry {
            class: class.to_owned(),
            key: key.to_owned(),
            path: path.clone(),
            size: i64::try_from(metadata.len()).unwrap_or(i64::MAX),
            modified: metadata.modified().unwrap_or(UNIX_EPOCH),
            expires,
            accessed: 0,
        };
        let mut state = self.lock_state();
        let qualified = qualified_key_unchecked(class, key);
        if let Some(old) = state.disk.remove(&qualified) {
            state.remove_disk_accounting(&old);
        }
        let mut item = item;
        item.accessed = state.next_access();
        state.add_disk_accounting(&item);
        state.disk.insert(qualified, item);
        Ok(())
    }

    async fn remove_disk_files_async(
        &self,
        entry: &DiskEntry,
        eviction: bool,
    ) -> Result<(), CacheError> {
        let data = tokio::fs::remove_file(&entry.path).await;
        if let Err(error) = data
            && error.kind() != std::io::ErrorKind::NotFound
        {
            return Err(self.storage_error(error));
        }
        let _ = tokio::fs::remove_file(meta_path(&entry.path)).await;
        let qualified = qualified_key_unchecked(&entry.class, &entry.key);
        let mut state = self.lock_state();
        if state
            .disk
            .get(&qualified)
            .is_some_and(|current| current.path == entry.path)
            && let Some(current) = state.disk.remove(&qualified)
        {
            state.remove_disk_accounting(&current);
            if eviction {
                state.class_evict(&current.class);
            }
        }
        Ok(())
    }

    async fn prune_disk(&self) -> Result<(), CacheError> {
        let _disk_guard = self.inner.disk_io.lock().await;
        let manager = self.clone();
        tokio::task::spawn_blocking(move || manager.prune_disk_sync())
            .await
            .map_err(|error| CacheError::Storage(format!("cache prune worker failed: {error}")))?
    }

    fn prune_disk_sync(&self) -> Result<(), CacheError> {
        self.reconcile_disk_sync()?;
        let classes = {
            let state = self.lock_state();
            state
                .classes
                .iter()
                .map(|(name, class)| (name.clone(), class.config.clone()))
                .collect::<BTreeMap<_, _>>()
        };
        let mut items = {
            let state = self.lock_state();
            state.disk.values().cloned().collect::<Vec<_>>()
        };
        let mut per_class = disk_usage_by_class(&items);
        for class in classes.values().filter(|class| class.disk) {
            while class.soft_quota > 0
                && per_class.get(&class.name).copied().unwrap_or(0) > class.soft_quota
            {
                let Some(victim) =
                    pick_disk_victim(&items, &classes, |item| item.class == class.name)
                else {
                    break;
                };
                remove_disk_sync(self, &victim)?;
                subtract_disk_item(&mut items, &mut per_class, &victim);
            }
        }
        if self.inner.disk_limit > 0 {
            let mut total: i64 = items.iter().map(|item| item.size).sum();
            while total > self.inner.disk_limit {
                let Some(victim) = pick_disk_victim(&items, &classes, |_| true) else {
                    break;
                };
                remove_disk_sync(self, &victim)?;
                total -= victim.size;
                subtract_disk_item(&mut items, &mut per_class, &victim);
            }
        }
        Ok(())
    }

    fn disk_path(&self, class: &str, key: &str) -> PathBuf {
        self.inner
            .cache_dir
            .join(format!("{}.cache", disk_name(class, key)))
    }

    fn replace_disk(&self, items: Vec<DiskEntry>) {
        let mut state = self.lock_state();
        state.disk.clear();
        state.disk_bytes = 0;
        state.disk_entries = 0;
        for class in state.classes.values_mut() {
            class.disk_bytes = 0;
            class.disk_entries = 0;
        }
        for item in items {
            state.add_disk_accounting(&item);
            state
                .disk
                .insert(qualified_key_unchecked(&item.class, &item.key), item);
        }
    }
}

impl State {
    fn next_access(&mut self) -> u64 {
        self.access_clock = self.access_clock.saturating_add(1);
        self.access_clock
    }

    fn class_miss(&mut self, class: &str) {
        if let Some(class) = self.classes.get_mut(class) {
            class.counters.misses = class.counters.misses.saturating_add(1);
        }
    }

    fn class_hit(&mut self, class: &str) {
        if let Some(class) = self.classes.get_mut(class) {
            class.counters.hits = class.counters.hits.saturating_add(1);
        }
    }

    fn class_load(&mut self, class: &str) {
        if let Some(class) = self.classes.get_mut(class) {
            class.counters.loads = class.counters.loads.saturating_add(1);
        }
    }

    fn class_load_error(&mut self, class: &str) {
        if let Some(class) = self.classes.get_mut(class) {
            class.counters.load_errors = class.counters.load_errors.saturating_add(1);
        }
    }

    fn class_evict(&mut self, class: &str) {
        if let Some(class) = self.classes.get_mut(class) {
            class.counters.evictions = class.counters.evictions.saturating_add(1);
        }
    }

    fn get_memory(&mut self, class: &str, key: &str) -> Result<Option<Vec<u8>>, CacheError> {
        let qualified = qualified_key(class, key)?;
        let Some(entry) = self.memory.get(&qualified) else {
            return Ok(None);
        };
        if expired(entry.expires) {
            let entry = self.memory.remove(&qualified).expect("entry was present");
            self.remove_memory_accounting(&entry);
            self.class_evict(class);
            return Ok(None);
        }
        let data = entry.data.clone();
        let next = self.next_access();
        if let Some(entry) = self.memory.get_mut(&qualified) {
            entry.accessed = next;
        }
        Ok(Some(data))
    }

    fn put_memory(&mut self, class: &str, key: &str, data: Vec<u8>, expires: Option<SystemTime>) {
        let qualified = qualified_key_unchecked(class, key);
        if let Some(previous) = self.memory.remove(&qualified) {
            self.remove_memory_accounting(&previous);
        }
        let entry = MemoryEntry {
            class: class.to_owned(),
            key: key.to_owned(),
            data,
            expires,
            accessed: self.next_access(),
        };
        self.add_memory_accounting(&entry);
        self.memory.insert(qualified, entry);
    }

    fn remove_expired_memory(&mut self) {
        let expired: Vec<_> = self
            .memory
            .iter()
            .filter(|(_, entry)| expired(entry.expires))
            .map(|(key, _)| key.clone())
            .collect();
        for qualified in expired {
            if let Some(entry) = self.memory.remove(&qualified) {
                self.remove_memory_accounting(&entry);
                self.class_evict(&entry.class);
            }
        }
    }

    fn enforce_memory(&mut self, limit: i64) {
        self.remove_expired_memory();
        let quotas: Vec<_> = self
            .classes
            .values()
            .filter(|class| class.config.memory && class.config.soft_quota > 0)
            .map(|class| (class.config.name.clone(), class.config.soft_quota))
            .collect();
        for (class, quota) in quotas {
            while self
                .classes
                .get(&class)
                .is_some_and(|class| class.memory_bytes > quota)
            {
                let Some(victim) = self.pick_memory_victim(|entry| entry.class == class) else {
                    break;
                };
                self.remove_memory_entry(&victim);
                self.class_evict(&class);
            }
        }
        if limit > 0 {
            self.evict_memory_to(limit);
        }
    }

    fn evict_memory_to(&mut self, limit: i64) {
        while self.memory_bytes > limit {
            let Some(victim) = self.pick_memory_victim(|_| true) else {
                break;
            };
            let class = victim.class.clone();
            self.remove_memory_entry(&victim);
            self.class_evict(&class);
        }
    }

    fn pick_memory_victim<F>(&self, accept: F) -> Option<MemoryEntry>
    where
        F: Fn(&MemoryEntry) -> bool,
    {
        self.memory
            .values()
            .filter(|entry| accept(entry))
            .min_by_key(|entry| {
                let priority = self
                    .classes
                    .get(&entry.class)
                    .map_or(i32::MIN, |class| class.config.priority);
                (priority, entry.accessed)
            })
            .map(|entry| MemoryEntry {
                class: entry.class.clone(),
                key: entry.key.clone(),
                data: Vec::new(),
                expires: entry.expires,
                accessed: entry.accessed,
            })
    }

    fn remove_memory_entry(&mut self, entry: &MemoryEntry) {
        let qualified = qualified_key_unchecked(&entry.class, &entry.key);
        if let Some(current) = self.memory.remove(&qualified) {
            self.remove_memory_accounting(&current);
        }
    }

    fn remove_memory_accounting(&mut self, entry: &MemoryEntry) {
        let size = i64::try_from(entry.data.len()).unwrap_or(i64::MAX);
        self.memory_bytes = self.memory_bytes.saturating_sub(size);
        self.memory_entries = self.memory_entries.saturating_sub(1);
        if let Some(class) = self.classes.get_mut(&entry.class) {
            class.memory_bytes = class.memory_bytes.saturating_sub(size);
            class.memory_entries = class.memory_entries.saturating_sub(1);
        }
    }

    fn add_memory_accounting(&mut self, entry: &MemoryEntry) {
        let size = i64::try_from(entry.data.len()).unwrap_or(i64::MAX);
        self.memory_bytes = self.memory_bytes.saturating_add(size);
        self.memory_entries = self.memory_entries.saturating_add(1);
        if let Some(class) = self.classes.get_mut(&entry.class) {
            class.memory_bytes = class.memory_bytes.saturating_add(size);
            class.memory_entries = class.memory_entries.saturating_add(1);
        }
    }

    fn add_disk_accounting(&mut self, entry: &DiskEntry) {
        self.disk_bytes = self.disk_bytes.saturating_add(entry.size);
        self.disk_entries = self.disk_entries.saturating_add(1);
        if let Some(class) = self.classes.get_mut(&entry.class) {
            class.disk_bytes = class.disk_bytes.saturating_add(entry.size);
            class.disk_entries = class.disk_entries.saturating_add(1);
        }
    }

    fn remove_disk_accounting(&mut self, entry: &DiskEntry) {
        self.disk_bytes = self.disk_bytes.saturating_sub(entry.size);
        self.disk_entries = self.disk_entries.saturating_sub(1);
        if let Some(class) = self.classes.get_mut(&entry.class) {
            class.disk_bytes = class.disk_bytes.saturating_sub(entry.size);
            class.disk_entries = class.disk_entries.saturating_sub(1);
        }
    }

    fn adjust_disk_size(&mut self, class: &str, old: i64, new: i64) {
        let delta = new.saturating_sub(old);
        self.disk_bytes = self.disk_bytes.saturating_add(delta);
        if let Some(class_state) = self.classes.get_mut(class) {
            class_state.disk_bytes = class_state.disk_bytes.saturating_add(delta);
        }
    }
}

async fn wait_for_flight(
    mut receiver: watch::Receiver<Option<Arc<LoadResult>>>,
) -> Result<Vec<u8>, CacheError> {
    loop {
        if let Some(result) = receiver.borrow().clone() {
            return result
                .as_ref()
                .clone()
                .map_err(|error| CacheError::Loader(error.clone()));
        }
        receiver.changed().await.map_err(|_| CacheError::Closed)?;
    }
}

impl CacheManager {
    async fn finish_load(
        &self,
        class: String,
        key: String,
        ttl: Duration,
        flight: Arc<Flight>,
        result: LoadResult,
    ) {
        let result = match result {
            Ok(data) => {
                if let Err(error) = self.put(&class, &key, &data, ttl).await {
                    tracing::warn!(%error, class, key, "cache result could not be stored");
                }
                Ok(data)
            }
            Err(error) => Err(error),
        };
        let shared = Arc::new(result);
        let qualified = qualified_key_unchecked(&class, &key);
        let mut state = self.lock_state();
        state.class_load(&class);
        if shared.is_err() {
            state.class_load_error(&class);
        }
        if state
            .flights
            .get(&qualified)
            .is_some_and(|current| Arc::ptr_eq(current, &flight))
        {
            state.flights.remove(&qualified);
        }
        let _ = flight.sender.send(Some(shared));
    }
}

fn validate_component(value: &str) -> Result<(), CacheError> {
    if value.is_empty() || value.contains('\0') || value.contains('\n') {
        return Err(CacheError::InvalidKey);
    }
    Ok(())
}

fn validate_key(value: &str) -> Result<(), CacheError> {
    if value.contains('\0') || value.contains('\n') {
        return Err(CacheError::InvalidKey);
    }
    Ok(())
}

fn qualified_key(class: &str, key: &str) -> Result<String, CacheError> {
    validate_component(class)?;
    validate_key(key)?;
    Ok(qualified_key_unchecked(class, key))
}

fn qualified_key_unchecked(class: &str, key: &str) -> String {
    format!("{class}\0{key}")
}

fn key_matches(prefix: &str, class: &str, key: &str) -> bool {
    key.starts_with(prefix) || qualified_key_unchecked(class, key).starts_with(prefix)
}

fn expired(deadline: Option<SystemTime>) -> bool {
    deadline.is_some_and(|deadline| SystemTime::now() >= deadline)
}

fn disk_name(class: &str, key: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(qualified_key_unchecked(class, key).as_bytes());
    hex::encode(&hasher.finalize()[..8])
}

fn disk_usage_by_class(items: &[DiskEntry]) -> HashMap<String, i64> {
    let mut usage = HashMap::new();
    for item in items {
        *usage.entry(item.class.clone()).or_insert(0) += item.size;
    }
    usage
}

fn pick_disk_victim<F>(
    items: &[DiskEntry],
    classes: &BTreeMap<String, CacheClass>,
    accept: F,
) -> Option<DiskEntry>
where
    F: Fn(&DiskEntry) -> bool,
{
    items
        .iter()
        .filter(|item| accept(item))
        .min_by_key(|item| {
            let priority = classes
                .get(&item.class)
                .map_or(i32::MIN, |class| class.priority);
            let accessed = if item.accessed == 0 {
                item.modified
            } else {
                UNIX_EPOCH + Duration::from_nanos(item.accessed)
            };
            (priority, accessed)
        })
        .cloned()
}

fn subtract_disk_item(
    items: &mut Vec<DiskEntry>,
    per_class: &mut HashMap<String, i64>,
    victim: &DiskEntry,
) {
    if let Some(index) = items.iter().position(|item| item.path == victim.path) {
        items.remove(index);
    }
    if let Some(usage) = per_class.get_mut(&victim.class) {
        *usage = usage.saturating_sub(victim.size);
    }
}

fn remove_disk_sync(manager: &CacheManager, entry: &DiskEntry) -> Result<(), CacheError> {
    remove_file_if_present(&entry.path).map_err(|error| manager.storage_error(error))?;
    let _ = std::fs::remove_file(meta_path(&entry.path));
    let qualified = qualified_key_unchecked(&entry.class, &entry.key);
    let mut state = manager.lock_state();
    if state
        .disk
        .get(&qualified)
        .is_some_and(|current| current.path == entry.path)
        && let Some(current) = state.disk.remove(&qualified)
    {
        state.remove_disk_accounting(&current);
        state.class_evict(&current.class);
    }
    Ok(())
}

fn read_disk_meta(path: &Path) -> Option<(String, String, Option<SystemTime>)> {
    let meta = meta_path(path);
    let metadata = std::fs::symlink_metadata(&meta).ok()?;
    if !metadata.is_file() || metadata.file_type().is_symlink() {
        return None;
    }
    let bytes = std::fs::read(meta).ok()?;
    let text = String::from_utf8(bytes).ok()?;
    let (identity, raw_expiry) = text.split_once('\n')?;
    let (class, key) = identity.split_once('\0')?;
    validate_component(class).ok()?;
    validate_key(key).ok()?;
    let nanos = raw_expiry.trim().parse::<i64>().ok()?;
    let expires = if nanos == 0 {
        None
    } else if nanos > 0 {
        Some(UNIX_EPOCH.checked_add(Duration::from_nanos(nanos as u64))?)
    } else {
        return None;
    };
    Some((class.to_owned(), key.to_owned(), expires))
}

fn discard_pair(path: &Path) {
    let _ = std::fs::remove_file(path);
    let _ = std::fs::remove_file(meta_path(path));
}

fn meta_path(path: &Path) -> PathBuf {
    PathBuf::from(format!("{}.meta", path.display()))
}

fn is_hex(value: &str) -> bool {
    value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn prepare_cache_dir(path: &Path) -> Result<(), std::io::Error> {
    if let Ok(metadata) = std::fs::symlink_metadata(path)
        && (!metadata.is_dir() || metadata.file_type().is_symlink())
    {
        return Err(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            "cache path is not a real directory",
        ));
    }
    std::fs::create_dir_all(path)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}

fn remove_file_if_present(path: &Path) -> Result<(), std::io::Error> {
    match std::fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

fn system_time_nanos(time: SystemTime) -> i64 {
    time.duration_since(UNIX_EPOCH)
        .map(|duration| i64::try_from(duration.as_nanos()).unwrap_or(i64::MAX))
        .unwrap_or(0)
}

#[cfg(unix)]
fn permissions_0600() -> std::fs::Permissions {
    use std::os::unix::fs::PermissionsExt as _;
    std::fs::Permissions::from_mode(0o600)
}

#[cfg(not(unix))]
fn permissions_0600() -> std::fs::Permissions {
    std::fs::Permissions::readonly(false)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use tokio::sync::oneshot;

    fn class(name: &str, priority: i32, soft_quota: i64, memory: bool, disk: bool) -> CacheClass {
        CacheClass {
            name: name.to_owned(),
            priority,
            soft_quota,
            memory,
            disk,
            max_entry: None,
        }
    }

    fn manager() -> CacheManager {
        let manager = CacheManager::new(
            std::env::temp_dir().join(format!("revaro-cache-{}", crate::ids::new_id())),
            1 << 20,
            1 << 20,
        );
        manager
            .register_class(class("tiered", 50, 1 << 20, true, true))
            .unwrap();
        manager
    }

    #[tokio::test]
    async fn load_falls_back_to_disk_and_backfills_memory() {
        let manager = manager();
        let calls = Arc::new(AtomicUsize::new(0));
        let calls_for_load = Arc::clone(&calls);
        let value = manager
            .load("tiered", "book", Duration::ZERO, move || async move {
                calls_for_load.fetch_add(1, Ordering::Relaxed);
                Ok::<_, CacheLoadError>(b"payload".to_vec())
            })
            .await
            .unwrap();
        assert_eq!(value, b"payload");
        assert_eq!(calls.load(Ordering::Relaxed), 1);
        assert_eq!(manager.get("tiered", "book").unwrap().unwrap(), b"payload");

        manager.delete("tiered", "book").await.unwrap();
        manager
            .put_disk_for_test("tiered", "book", b"payload", None)
            .await;
        let value = manager
            .load("tiered", "book", Duration::ZERO, || async {
                panic!("disk hit must not call the loader")
            })
            .await
            .unwrap();
        assert_eq!(value, b"payload");
        assert_eq!(manager.stats().classes["tiered"].hits, 2);
    }

    #[tokio::test]
    async fn disk_metadata_is_exact_and_orphans_are_removed_on_prune() {
        let directory = std::env::temp_dir().join(format!("revaro-cache-{}", crate::ids::new_id()));
        let manager = CacheManager::new(&directory, 0, 1 << 20);
        manager
            .register_class(class("disk", 10, 1 << 20, false, true))
            .unwrap();
        manager
            .put("disk", "key", b"value", Duration::ZERO)
            .await
            .unwrap();
        let path = manager.disk_path("disk", "key");
        assert_eq!(
            std::fs::read_to_string(meta_path(&path)).unwrap(),
            "disk\0key\n0"
        );
        let orphan = directory.join("orphan.cache");
        std::fs::write(&orphan, b"orphan").unwrap();
        manager.prune().await.unwrap();
        assert!(!orphan.exists());
        assert!(manager.has("disk", "key").unwrap());
    }

    #[tokio::test]
    async fn memory_pressure_evicts_low_priority_entries_first() {
        let manager = CacheManager::new(
            std::env::temp_dir().join(format!("revaro-cache-{}", crate::ids::new_id())),
            6,
            0,
        );
        manager
            .register_class(class("hot", 90, 0, true, false))
            .unwrap();
        manager
            .register_class(class("cold", 1, 0, true, false))
            .unwrap();
        manager
            .put("hot", "one", b"111", Duration::ZERO)
            .await
            .unwrap();
        manager
            .put("cold", "one", b"222", Duration::ZERO)
            .await
            .unwrap();
        manager
            .put("hot", "two", b"333", Duration::ZERO)
            .await
            .unwrap();
        assert!(manager.get("hot", "one").unwrap().is_some());
        assert!(manager.get("cold", "one").unwrap().is_none());
    }

    #[tokio::test]
    async fn one_loader_is_shared_and_survives_a_cancelled_waiter() {
        let manager = manager();
        let calls = Arc::new(AtomicUsize::new(0));
        let (ready, wait) = oneshot::channel();
        let calls_for_loader = Arc::clone(&calls);
        let first = manager.load("tiered", "same", Duration::ZERO, move || async move {
            calls_for_loader.fetch_add(1, Ordering::Relaxed);
            let _ = wait.await;
            Ok::<_, CacheLoadError>(b"shared".to_vec())
        });
        // The timeout only cancels this waiter; the detached loader still
        // owns the singleflight entry for the next caller.
        assert!(
            tokio::time::timeout(Duration::from_millis(20), first)
                .await
                .is_err()
        );
        let second = manager.load("tiered", "same", Duration::ZERO, || async {
            Ok::<_, CacheLoadError>(b"wrong".to_vec())
        });
        tokio::task::yield_now().await;
        let _ = ready.send(());
        assert_eq!(second.await.unwrap(), b"shared");
        assert_eq!(calls.load(Ordering::Relaxed), 1);
    }

    #[tokio::test]
    async fn ttl_entries_expire_from_both_tiers() {
        let manager = manager();
        manager
            .put("tiered", "temporary", b"v", Duration::from_millis(20))
            .await
            .unwrap();
        tokio::time::sleep(Duration::from_millis(40)).await;
        assert!(manager.get("tiered", "temporary").unwrap().is_none());
        assert!(!manager.has("tiered", "temporary").unwrap());
    }

    #[tokio::test]
    async fn a_new_manager_reconciles_durable_entries_after_registration() {
        let directory = std::env::temp_dir().join(format!("revaro-cache-{}", crate::ids::new_id()));
        let first = CacheManager::new(&directory, 0, 1 << 20);
        first
            .register_class(class("disk", 10, 1 << 20, false, true))
            .unwrap();
        first
            .put("disk", "persisted", b"value", Duration::ZERO)
            .await
            .unwrap();

        let second = CacheManager::new(&directory, 0, 1 << 20);
        second
            .register_class(class("disk", 10, 1 << 20, false, true))
            .unwrap();
        second.reconcile_disk_sync().unwrap();
        let value = second
            .load("disk", "persisted", Duration::ZERO, || async {
                panic!("a reconciled disk entry must not call the loader")
            })
            .await
            .unwrap();
        assert_eq!(value, b"value");
        assert_eq!(second.stats().classes["disk"].hits, 1);
    }

    #[tokio::test]
    async fn closing_the_manager_cancels_a_loader_and_releases_waiters() {
        let manager = manager();
        let (started, started_rx) = oneshot::channel();
        let waiter = {
            let manager = manager.clone();
            tokio::spawn(async move {
                manager
                    .load("tiered", "never", Duration::ZERO, move || async {
                        let _ = started.send(());
                        std::future::pending::<Result<Vec<u8>, CacheLoadError>>().await
                    })
                    .await
            })
        };
        tokio::time::timeout(Duration::from_secs(1), started_rx)
            .await
            .expect("loader must start before shutdown")
            .expect("loader start signal must arrive");
        tokio::time::timeout(Duration::from_secs(1), manager.close())
            .await
            .expect("close must not wait for an unbounded loader");
        let result = waiter.await.unwrap();
        assert!(matches!(result, Err(CacheError::Loader(_))));
    }

    #[tokio::test]
    async fn invalidate_removes_a_class_prefix_from_memory_and_disk() {
        let manager = manager();
        for key in ["prefix/one", "prefix/two", "other"] {
            manager
                .put("tiered", key, key.as_bytes(), Duration::ZERO)
                .await
                .unwrap();
        }
        manager.invalidate("tiered\0prefix/").await.unwrap();
        assert!(!manager.has("tiered", "prefix/one").unwrap());
        assert!(!manager.has("tiered", "prefix/two").unwrap());
        assert!(manager.has("tiered", "other").unwrap());
        assert_eq!(manager.stats().disk_entries, 1);
    }

    #[test]
    fn production_registry_reports_healthy_classes_for_a_writable_work_dir() {
        let work_dir = std::env::temp_dir().join(format!("revaro-cache-{}", crate::ids::new_id()));
        let manager =
            CacheManager::for_app(&work_dir, 1 << 20, Arc::new(BookCache::new(4, 128 << 20)));
        let status = manager.system_status();
        assert_eq!(status.status, "ok");
        let classes = status.classes.unwrap();
        assert!(classes.contains_key(READER_FLOW_MANIFEST));
        assert!(classes.contains_key(READER_FLOW_CHUNK));
        assert!(classes.contains_key(READER_SOURCE));
        assert!(classes.contains_key(READER_BOOKS));
        assert!(classes.contains_key(MEDIA_SUBTITLE));
    }

    impl CacheManager {
        async fn put_disk_for_test(
            &self,
            class: &str,
            key: &str,
            data: &[u8],
            expires: Option<SystemTime>,
        ) {
            self.put_disk(class, key, data, expires).await.unwrap();
        }
    }
}

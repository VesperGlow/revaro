//! Process-wide maintenance jobs.
//!
//! The durable tables and object store are the source of truth; this module
//! only supplies the timer and the bounded passes that reconcile them. Jobs
//! run in one task and deterministic name order, so a cleanup pass cannot race a
//! second pass from the same process. Each pass still takes one permit from
//! the shared maintenance I/O budget and is bounded by its own timeout.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::fmt;
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex, PoisonError, Weak};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use revaro_core::Timestamp;
use revaro_core::keys;
use tokio::sync::{Notify, Semaphore};
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

use crate::cache::{CacheManager, READER_FLOW_CHUNK, READER_FLOW_MANIFEST};
use crate::db::{Database, DbError};
use crate::state::AppState;

/// The timer resolution required by the original cleanup manager.
const TIMER_FLOOR: Duration = Duration::from_secs(1);
/// Maximum number of durable rows one pass claims.
const CLEANUP_BATCH: usize = 1_000;
/// The grace period before an unreferenced upload object is eligible for GC.
const GC_GRACE: Duration = Duration::from_secs(60 * 60);
/// One maintenance pass must not consume more than this many object keys in a
/// single garbage-collection batch. It is separate from the database row cap
/// because flow artifacts are expanded from one original object.
const GC_OBJECT_BATCH: usize = 1_000;
/// Flow artifacts use the same format version as the reader HTTP surface.
const FLOW_VERSION: u32 = revaro_reader::flow::FLOW_FORMAT_VERSION as u32;

type JobFuture = Pin<Box<dyn Future<Output = Result<(), String>> + Send>>;
type JobRunner = Arc<dyn Fn() -> JobFuture + Send + Sync>;

/// Failure returned while registering a maintenance job.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum RegistrationError {
    /// A job name is already registered.
    #[error("maintenance job {0:?} is already registered")]
    Duplicate(String),
    /// A zero interval would make a failed or successful job run forever.
    #[error("maintenance job {0:?} has a zero interval")]
    ZeroInterval(String),
    /// A zero timeout cannot execute any useful pass.
    #[error("maintenance job {0:?} has a zero timeout")]
    ZeroTimeout(String),
}

struct Job {
    interval: Duration,
    timeout: Duration,
    next: Instant,
    failures: u32,
    running: bool,
    wake_requested: bool,
    run: JobRunner,
}

struct Inner {
    jobs: Mutex<BTreeMap<String, Job>>,
    notify: Notify,
    shutdown: CancellationToken,
    io_slots: Arc<Semaphore>,
    started: std::sync::atomic::AtomicBool,
    task: Mutex<Option<JoinHandle<()>>>,
}

/// The process-wide sequential maintenance scheduler.
#[derive(Clone)]
pub struct MaintenanceRuntime {
    inner: Arc<Inner>,
}

impl fmt::Debug for MaintenanceRuntime {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let jobs = self
            .inner
            .jobs
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        formatter
            .debug_struct("MaintenanceRuntime")
            .field("jobs", &jobs.keys().collect::<Vec<_>>())
            .field(
                "started",
                &self
                    .inner
                    .started
                    .load(std::sync::atomic::Ordering::Acquire),
            )
            .finish()
    }
}

impl Default for MaintenanceRuntime {
    fn default() -> Self {
        Self::new()
    }
}

impl MaintenanceRuntime {
    /// Create an empty scheduler. Jobs may be registered before or after start.
    #[must_use]
    pub fn new() -> Self {
        Self {
            inner: Arc::new(Inner {
                jobs: Mutex::new(BTreeMap::new()),
                notify: Notify::new(),
                shutdown: CancellationToken::new(),
                io_slots: Arc::new(Semaphore::new(3)),
                started: std::sync::atomic::AtomicBool::new(false),
                task: Mutex::new(None),
            }),
        }
    }

    /// Register one job.
    ///
    /// `run_now` controls the first deadline only. Later runs use the interval,
    /// while failures use the bounded backoff described by the migration
    /// contract.
    pub fn register<F, Fut>(
        &self,
        name: impl Into<String>,
        interval: Duration,
        timeout: Duration,
        run_now: bool,
        run: F,
    ) -> Result<(), RegistrationError>
    where
        F: Fn() -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<(), String>> + Send + 'static,
    {
        let name = name.into();
        if interval.is_zero() {
            return Err(RegistrationError::ZeroInterval(name));
        }
        if timeout.is_zero() {
            return Err(RegistrationError::ZeroTimeout(name));
        }
        let now = Instant::now();
        let next = if run_now {
            now
        } else {
            now.checked_add(interval).unwrap_or(now)
        };
        let mut jobs = self
            .inner
            .jobs
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        if jobs.contains_key(&name) {
            return Err(RegistrationError::Duplicate(name));
        }
        jobs.insert(
            name,
            Job {
                interval,
                timeout,
                next,
                failures: 0,
                running: false,
                wake_requested: false,
                run: Arc::new(move || Box::pin(run())),
            },
        );
        drop(jobs);
        self.inner.notify.notify_one();
        Ok(())
    }

    /// Register the production jobs owned by the server.
    pub fn register_production(&self, state: Weak<AppState>) {
        let minute = Duration::from_secs(60);

        self.register(
            "cache",
            minute.saturating_mul(5),
            minute,
            false,
            state_job(state.clone(), |state| async move {
                state.cache.prune().await.map_err(|error| error.to_string())
            }),
        )
        .expect("production maintenance job names are unique");
        self.register(
            "uploads",
            minute.saturating_mul(15),
            minute.saturating_mul(5),
            true,
            state_job(state.clone(), |state| async move {
                cleanup_expired_uploads(&state).await
            }),
        )
        .expect("production maintenance job names are unique");
        self.register(
            "object-cleanup",
            minute.saturating_mul(15),
            minute.saturating_mul(5),
            true,
            state_job(state.clone(), |state| async move {
                cleanup_object_queue(&state).await
            }),
        )
        .expect("production maintenance job names are unique");
        self.register(
            "trash",
            minute.saturating_mul(15),
            minute.saturating_mul(10),
            true,
            state_job(
                state.clone(),
                |state| async move { cleanup_trash(&state).await },
            ),
        )
        .expect("production maintenance job names are unique");
        if let Some(interval) = state.upgrade().map(|state| state.config.gc_interval)
            && !interval.is_zero()
        {
            self.register(
                "orphan-objects",
                interval,
                minute.saturating_mul(10),
                true,
                state_job(state.clone(), |state| async move {
                    collect_garbage(&state).await
                }),
            )
            .expect("production maintenance job names are unique");
        }
        self.register(
            "temporary-uploads",
            minute.saturating_mul(60),
            minute.saturating_mul(5),
            true,
            state_job(state.clone(), |state| async move {
                state
                    .store
                    .cleanup_temporary(state.config.upload_expires)
                    .await
                    .map_err(|error| error.to_string())
            }),
        )
        .expect("production maintenance job names are unique");
        self.register(
            "auth",
            minute.saturating_mul(15),
            minute.saturating_mul(5),
            true,
            state_job(state, |state| async move {
                state
                    .auth
                    .cleanup_expired_sessions()
                    .await
                    .map(|_| ())
                    .map_err(|error| error.to_string())
            }),
        )
        .expect("production maintenance job names are unique");
    }

    /// Start the single scheduler task. Calling this twice is harmless.
    pub fn start(&self) {
        if self
            .inner
            .started
            .swap(true, std::sync::atomic::Ordering::AcqRel)
        {
            return;
        }
        let inner = Arc::clone(&self.inner);
        let task = tokio::spawn(async move { run_loop(inner).await });
        *self
            .inner
            .task
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = Some(task);
    }

    /// Wake one registered job immediately. The wake is coalesced while the
    /// job is already running and is consumed after that pass settles.
    pub fn wake(&self, name: &str) -> bool {
        let mut jobs = self
            .inner
            .jobs
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        let Some(job) = jobs.get_mut(name) else {
            return false;
        };
        if job.running {
            job.wake_requested = true;
        } else {
            job.next = Instant::now();
        }
        drop(jobs);
        self.inner.notify.notify_one();
        true
    }

    /// Cancel the scheduler and wait for an in-flight pass to drain.
    pub async fn close(&self) {
        self.inner.shutdown.cancel();
        self.inner.notify.notify_waiters();
        let task = self
            .inner
            .task
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take();
        if let Some(task) = task
            && let Err(error) = task.await
        {
            tracing::warn!(%error, "maintenance scheduler stopped abnormally");
        }
    }

    #[cfg(test)]
    fn job_state(&self, name: &str) -> Option<(u32, bool)> {
        let jobs = self
            .inner
            .jobs
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        jobs.get(name).map(|job| (job.failures, job.running))
    }
}

fn state_job<F, Fut>(
    state: Weak<AppState>,
    operation: F,
) -> impl Fn() -> JobFuture + Send + Sync + 'static
where
    F: Fn(Arc<AppState>) -> Fut + Send + Sync + Clone + 'static,
    Fut: Future<Output = Result<(), String>> + Send + 'static,
{
    move || {
        let state = state.clone();
        let operation = operation.clone();
        Box::pin(async move {
            let Some(state) = state.upgrade() else {
                return Ok(());
            };
            operation(state).await
        })
    }
}

async fn run_loop(inner: Arc<Inner>) {
    loop {
        if inner.shutdown.is_cancelled() {
            break;
        }

        let now = Instant::now();
        let due = due_jobs(&inner, now);
        if !due.is_empty() {
            for name in due {
                if inner.shutdown.is_cancelled() {
                    break;
                }
                run_one(&inner, &name).await;
            }
            continue;
        }

        let next = next_deadline(&inner);
        let notified = inner.notify.notified();
        tokio::pin!(notified);
        match next {
            Some(deadline) => {
                let delay = deadline
                    .saturating_duration_since(Instant::now())
                    .max(TIMER_FLOOR);
                tokio::select! {
                    _ = inner.shutdown.cancelled() => break,
                    _ = &mut notified => {}
                    _ = tokio::time::sleep(delay) => {}
                }
            }
            None => {
                tokio::select! {
                    _ = inner.shutdown.cancelled() => break,
                    _ = &mut notified => {}
                }
            }
        }
    }
}

fn due_jobs(inner: &Inner, now: Instant) -> Vec<String> {
    let jobs = inner.jobs.lock().unwrap_or_else(PoisonError::into_inner);
    jobs.iter()
        .filter(|(_, job)| !job.running && job.next <= now)
        .map(|(name, _)| name.clone())
        .collect()
}

fn next_deadline(inner: &Inner) -> Option<Instant> {
    let jobs = inner.jobs.lock().unwrap_or_else(PoisonError::into_inner);
    jobs.values()
        .filter(|job| !job.running)
        .map(|job| job.next)
        .min()
}

async fn run_one(inner: &Inner, name: &str) {
    let Some((runner, timeout)) = begin_job(inner, name) else {
        return;
    };
    let permit = tokio::select! {
        _ = inner.shutdown.cancelled() => {
            finish_job(inner, name, Err("maintenance scheduler is shutting down".to_owned()));
            return;
        }
        permit = Arc::clone(&inner.io_slots).acquire_owned() => match permit {
            Ok(permit) => permit,
            Err(_) => {
                finish_job(inner, name, Err("maintenance I/O budget is closed".to_owned()));
                return;
            }
        },
    };
    let result = match tokio::time::timeout(timeout, runner()).await {
        Ok(result) => result,
        Err(_) => Err(format!("maintenance pass timed out after {timeout:?}")),
    };
    drop(permit);
    finish_job(inner, name, result);
}

fn begin_job(inner: &Inner, name: &str) -> Option<(JobRunner, Duration)> {
    let mut jobs = inner.jobs.lock().unwrap_or_else(PoisonError::into_inner);
    let job = jobs.get_mut(name)?;
    if job.running || job.next > Instant::now() {
        return None;
    }
    job.running = true;
    job.next = Instant::now()
        .checked_add(job.interval)
        .unwrap_or_else(Instant::now);
    Some((Arc::clone(&job.run), job.timeout))
}

fn finish_job(inner: &Inner, name: &str, result: Result<(), String>) {
    let mut jobs = inner.jobs.lock().unwrap_or_else(PoisonError::into_inner);
    let Some(job) = jobs.get_mut(name) else {
        return;
    };
    job.running = false;
    let now = Instant::now();
    match result {
        Ok(()) => {
            if job.wake_requested {
                job.wake_requested = false;
                job.next = now;
            } else {
                job.next = now.checked_add(job.interval).unwrap_or(now);
            }
            job.failures = 0;
        }
        Err(error) => {
            // A wake requested by a pass that subsequently failed must not
            // defeat failure backoff. The next successful pass may request a
            // fresh immediate batch again.
            job.wake_requested = false;
            job.failures = job.failures.saturating_add(1);
            let minutes = 1_u64 << job.failures.min(6);
            let backoff = Duration::from_secs(minutes.saturating_mul(60));
            let candidate = now.checked_add(backoff).unwrap_or(now);
            if candidate < job.next {
                job.next = candidate;
            }
            tracing::warn!(job = name, failures = job.failures, %error, "maintenance job failed");
        }
    }
}

#[derive(Debug, Clone)]
struct ExpiredUpload {
    id: String,
    file_id: String,
    object_key: String,
    multipart_id: Option<String>,
}

#[derive(Debug, Default)]
struct References {
    /// Original objects and all known thumbnail generations still referenced
    /// by a file row, including rows in the trash and pending uploads.
    storage_keys: HashSet<String>,
    /// Original object keys that can own a reader flow directory.
    book_keys: HashSet<String>,
}

async fn load_expired_uploads(
    database: &Database,
    now: String,
) -> Result<Vec<ExpiredUpload>, String> {
    database
        .call(move |connection| {
            let mut statement = connection
                .prepare(
                    "SELECT id,file_id,object_key,multipart_id FROM uploads \
                     WHERE status='pending' AND julianday(expires_at) <= julianday(?1) \
                     ORDER BY expires_at LIMIT ?2",
                )
                .map_err(DbError::Query)?;
            let rows = statement
                .query_map(rusqlite::params![now, CLEANUP_BATCH as i64], |row| {
                    Ok(ExpiredUpload {
                        id: row.get(0)?,
                        file_id: row.get(1)?,
                        object_key: row.get(2)?,
                        multipart_id: row.get(3)?,
                    })
                })
                .map_err(DbError::Query)?;
            rows.collect::<Result<Vec<_>, _>>().map_err(DbError::Query)
        })
        .await
        .map_err(|error| error.to_string())
}

async fn cleanup_expired_uploads(state: &Arc<AppState>) -> Result<(), String> {
    let records = load_expired_uploads(&state.db, Timestamp::now().to_rfc3339()).await?;
    let mut removed = 0usize;
    for record in &records {
        let _guard = state.uploads.lock(&record.id).await;
        let id = record.id.clone();
        let file_id = record.file_id.clone();
        let now = Timestamp::now().to_rfc3339();
        let deleted = state
            .db
            .call(move |connection| {
                let transaction = connection.transaction().map_err(DbError::Query)?;
                let upload = transaction
                    .execute(
                        "DELETE FROM uploads WHERE id=?1 AND file_id=?2 AND status='pending' \
                         AND julianday(expires_at) <= julianday(?3) \
                         AND EXISTS(SELECT 1 FROM files WHERE id=?2 AND status='pending')",
                        rusqlite::params![id, file_id, now],
                    )
                    .map_err(DbError::Query)?;
                if upload == 0 {
                    transaction.rollback().map_err(DbError::Query)?;
                    return Ok(false);
                }
                let file = transaction
                    .execute(
                        "DELETE FROM files WHERE id=?1 AND status='pending'",
                        [&file_id],
                    )
                    .map_err(DbError::Query)?;
                if file != 1 {
                    transaction.rollback().map_err(DbError::Query)?;
                    return Ok(false);
                }
                transaction.commit().map_err(DbError::Query)?;
                Ok(true)
            })
            .await
            .map_err(|error| error.to_string())?;
        if !deleted {
            continue;
        }
        removed += 1;
        if let Some(multipart_id) = &record.multipart_id
            && let Err(error) = state
                .store
                .abort_multipart(&record.object_key, multipart_id)
                .await
        {
            tracing::warn!(%error, upload = %record.id, "expired multipart staging cleanup failed");
        }
        if let Err(error) = state.store.delete(&record.object_key).await {
            tracing::warn!(%error, upload = %record.id, "expired upload object cleanup failed");
        }
    }
    if removed > 0 {
        state.jobs.changed();
        state.maintenance.wake("object-cleanup");
    }
    if records.len() == CLEANUP_BATCH {
        state.maintenance.wake("uploads");
    }
    Ok(())
}

async fn cleanup_trash(state: &Arc<AppState>) -> Result<(), String> {
    // A zero retention is the documented opt-out. It is deliberately checked
    // before calculating a cutoff so a zero duration never means "delete now".
    if state.config.trash_retention.is_zero() {
        return Ok(());
    }
    let cutoff = Timestamp::from_system_time(
        SystemTime::now()
            .checked_sub(state.config.trash_retention)
            .unwrap_or(UNIX_EPOCH),
    )
    .to_rfc3339();
    let removed = state
        .db
        .call(move |connection| {
            let transaction = connection.transaction().map_err(DbError::Query)?;
            transaction
                .execute_batch("PRAGMA defer_foreign_keys=ON")
                .map_err(DbError::Query)?;
            let mut removed = 0usize;
            while removed < CLEANUP_BATCH {
                let changed = transaction
                    .execute(
                        "DELETE FROM files WHERE id=(SELECT id FROM files \
                         WHERE deleted_at IS NOT NULL AND julianday(deleted_at) <= julianday(?1) \
                         AND NOT EXISTS(SELECT 1 FROM files child WHERE child.parent_id=files.id) \
                         ORDER BY deleted_at,id LIMIT 1)",
                        [&cutoff],
                    )
                    .map_err(DbError::Query)?;
                if changed == 0 {
                    break;
                }
                removed += changed;
            }
            transaction.commit().map_err(DbError::Query)?;
            Ok(removed)
        })
        .await
        .map_err(|error| error.to_string())?;
    if removed > 0 {
        state.jobs.changed();
        state.maintenance.wake("object-cleanup");
    }
    if removed == CLEANUP_BATCH {
        state.maintenance.wake("trash");
        state.maintenance.wake("object-cleanup");
    }
    Ok(())
}

async fn referenced_objects(database: &Database) -> Result<References, String> {
    database
        .call(|connection| {
            let mut statement = connection
                .prepare(
                    "SELECT id,object_key FROM files WHERE object_key IS NOT NULL AND object_key<>''",
                )
                .map_err(DbError::Query)?;
            let rows = statement
                .query_map([], |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)))
                .map_err(DbError::Query)?;
            let mut references = References::default();
            for row in rows {
                let (file_id, object_key) = row.map_err(DbError::Query)?;
                references.book_keys.insert(object_key.clone());
                add_derived_keys(&mut references.storage_keys, &file_id, &object_key);
            }
            Ok(references)
        })
        .await
        .map_err(|error| error.to_string())
}

fn add_derived_keys(keys_set: &mut HashSet<String>, file_id: &str, object_key: &str) {
    keys_set.insert(object_key.to_owned());
    keys_set.insert(keys::thumbnail_v2_key(object_key));
    keys_set.insert(keys::image_thumbnail_key(object_key));
    keys_set.insert(keys::audio_thumbnail_key(object_key));
    keys_set.insert(keys::video_thumbnail_key(object_key));
    if let Some(legacy) = keys::thumbnail_key(file_id) {
        keys_set.insert(legacy);
    }
}

#[derive(Debug, Clone)]
struct CleanupRow {
    object_key: String,
    retry_count: i64,
    generation: i64,
}

async fn load_cleanup_rows(database: &Database) -> Result<Vec<CleanupRow>, String> {
    database
        .call(|connection| {
            let mut statement = connection
                .prepare(
                    "SELECT object_key,retry_count,generation FROM object_cleanup \
                     ORDER BY updated_at,object_key LIMIT ?1",
                )
                .map_err(DbError::Query)?;
            let rows = statement
                .query_map([CLEANUP_BATCH as i64], |row| {
                    Ok(CleanupRow {
                        object_key: row.get(0)?,
                        retry_count: row.get(1)?,
                        generation: row.get(2)?,
                    })
                })
                .map_err(DbError::Query)?;
            rows.collect::<Result<Vec<_>, _>>().map_err(DbError::Query)
        })
        .await
        .map_err(|error| error.to_string())
}

async fn cleanup_object_queue(state: &Arc<AppState>) -> Result<(), String> {
    let rows = load_cleanup_rows(&state.db).await?;
    let references = referenced_objects(&state.db).await?;
    let mut first_error = None;
    for row in &rows {
        if let Err(error) = cleanup_one_object(state, row, &references).await {
            tracing::warn!(
                object_key = %row.object_key,
                retry_count = row.retry_count,
                %error,
                "durable object cleanup will be retried"
            );
            mark_cleanup_retry(&state.db, row, &error).await?;
            first_error.get_or_insert(error);
        }
    }
    if rows.len() == CLEANUP_BATCH {
        state.maintenance.wake("object-cleanup");
    }
    if let Some(error) = first_error {
        return Err(error);
    }
    Ok(())
}

async fn cleanup_one_object(
    state: &Arc<AppState>,
    row: &CleanupRow,
    references: &References,
) -> Result<(), String> {
    let is_live = references.storage_keys.contains(&row.object_key)
        || object_is_referenced(&state.db, &row.object_key).await?;
    if !is_live {
        let _flow_guard;
        let _book_guard;
        if keys::blob_id(&row.object_key).is_some() {
            _flow_guard = Some(state.reader.flow_lock(&row.object_key).await);
            _book_guard = Some(state.reader.book_lock(&row.object_key).await);
        } else {
            _flow_guard = None;
            _book_guard = None;
        }
        let mut keys_to_delete = vec![row.object_key.clone()];
        if keys::blob_id(&row.object_key).is_some() {
            keys_to_delete.extend([
                keys::thumbnail_v2_key(&row.object_key),
                keys::image_thumbnail_key(&row.object_key),
                keys::audio_thumbnail_key(&row.object_key),
                keys::video_thumbnail_key(&row.object_key),
            ]);
            if let Some(file_id) = keys::blob_id(&row.object_key)
                && let Some(legacy) = keys::thumbnail_key(file_id)
            {
                keys_to_delete.push(legacy);
            }
            let flow_objects = state
                .store
                .list_prefix(&keys::flow_dir(&row.object_key, FLOW_VERSION))
                .await
                .map_err(|error| error.to_string())?;
            keys_to_delete.extend(flow_objects.into_iter().map(|object| object.key));
            invalidate_flow_cache(&state.cache, &row.object_key).await?;
        }
        state
            .store
            .delete_many(&keys_to_delete)
            .await
            .map_err(|error| error.to_string())?;
    }
    remove_cleanup_row(&state.db, row).await
}

async fn object_is_referenced(database: &Database, object_key: &str) -> Result<bool, String> {
    let object_key = object_key.to_owned();
    database
        .call(move |connection| {
            connection
                .query_row(
                    "SELECT EXISTS(SELECT 1 FROM files WHERE object_key=?1)",
                    [&object_key],
                    |row| row.get(0),
                )
                .map_err(DbError::Query)
        })
        .await
        .map_err(|error| error.to_string())
}

async fn mark_cleanup_retry(
    database: &Database,
    row: &CleanupRow,
    reason: &str,
) -> Result<(), String> {
    let key = row.object_key.clone();
    let generation = row.generation;
    let now = Timestamp::now().to_rfc3339();
    let reason = reason.to_owned();
    database
        .call(move |connection| {
            connection
                .execute(
                    "UPDATE object_cleanup SET retry_count=retry_count+1,updated_at=?1,reason=?2 \
                     WHERE object_key=?3 AND generation=?4",
                    rusqlite::params![now, reason, key, generation],
                )
                .map_err(DbError::Query)
        })
        .await
        .map_err(|error| error.to_string())?;
    Ok(())
}

async fn remove_cleanup_row(database: &Database, row: &CleanupRow) -> Result<(), String> {
    let key = row.object_key.clone();
    let generation = row.generation;
    database
        .call(move |connection| {
            connection
                .execute(
                    "DELETE FROM object_cleanup WHERE object_key=?1 AND generation=?2",
                    rusqlite::params![key, generation],
                )
                .map_err(DbError::Query)
        })
        .await
        .map_err(|error| error.to_string())?;
    Ok(())
}

async fn invalidate_flow_cache(cache: &CacheManager, book_key: &str) -> Result<(), String> {
    cache
        .invalidate(&format!(
            "{READER_FLOW_MANIFEST}\0manifest/{book_key}/f{FLOW_VERSION}"
        ))
        .await
        .map_err(|error| error.to_string())?;
    cache
        .invalidate(&format!(
            "{READER_FLOW_CHUNK}\0chunk/{book_key}/f{FLOW_VERSION}/"
        ))
        .await
        .map_err(|error| error.to_string())?;
    Ok(())
}

async fn collect_garbage(state: &Arc<AppState>) -> Result<(), String> {
    let references = referenced_objects(&state.db).await?;
    let cutoff = Timestamp::from_system_time(
        SystemTime::now()
            .checked_sub(state.config.upload_expires.saturating_add(GC_GRACE))
            .unwrap_or(UNIX_EPOCH),
    );
    collect_garbage_at(state, &references, cutoff).await
}

async fn collect_garbage_at(
    state: &Arc<AppState>,
    references: &References,
    cutoff: Timestamp,
) -> Result<(), String> {
    let mut candidates = Vec::new();
    for prefix in [keys::BLOB_ROOT, keys::THUMBNAIL_ROOT] {
        let objects = state
            .store
            .list_prefix(prefix)
            .await
            .map_err(|error| error.to_string())?;
        candidates.extend(objects.into_iter().filter(|object| {
            object.last_modified <= cutoff && !references.storage_keys.contains(&object.key)
        }));
    }
    candidates.sort_by(|left, right| {
        left.last_modified
            .cmp(&right.last_modified)
            .then_with(|| left.key.cmp(&right.key))
    });
    candidates.truncate(GC_OBJECT_BATCH);
    let keys_to_delete = candidates
        .iter()
        .map(|object| object.key.clone())
        .collect::<Vec<_>>();
    if !keys_to_delete.is_empty() {
        state
            .store
            .delete_many(&keys_to_delete)
            .await
            .map_err(|error| error.to_string())?;
    }
    collect_flow_cache(state, references).await?;
    if candidates.len() == GC_OBJECT_BATCH {
        state.maintenance.wake("orphan-objects");
    }
    Ok(())
}

async fn collect_flow_cache(state: &Arc<AppState>, references: &References) -> Result<(), String> {
    let objects = state
        .store
        .list_prefix(keys::FLOW_ROOT)
        .await
        .map_err(|error| error.to_string())?;
    let ttl_cutoff = Timestamp::from_system_time(
        SystemTime::now()
            .checked_sub(state.config.flow_cache_ttl)
            .unwrap_or(UNIX_EPOCH),
    );
    let mut to_delete = HashSet::new();
    let mut survivors = Vec::new();
    for object in objects {
        let Some(book_key) = keys::flow_book_key(&object.key) else {
            to_delete.insert(object.key);
            continue;
        };
        if !references.book_keys.contains(&book_key) || object.last_modified <= ttl_cutoff {
            to_delete.insert(object.key);
        } else {
            survivors.push(object);
        }
    }

    survivors.sort_by(|left, right| {
        left.last_modified
            .cmp(&right.last_modified)
            .then_with(|| left.key.cmp(&right.key))
    });
    let mut remaining = survivors.iter().map(|object| object.size).sum::<i64>();
    for object in survivors {
        if remaining > state.config.flow_cache_capacity {
            remaining = remaining.saturating_sub(object.size);
            to_delete.insert(object.key);
        }
    }

    delete_flow_objects(state, to_delete).await
}

async fn delete_flow_objects(
    state: &Arc<AppState>,
    keys_to_delete: HashSet<String>,
) -> Result<(), String> {
    let mut grouped: HashMap<Option<String>, Vec<String>> = HashMap::new();
    for key in keys_to_delete {
        grouped
            .entry(keys::flow_book_key(&key))
            .or_default()
            .push(key);
    }
    let mut first_error = None;
    for (book_key, keys_to_delete) in grouped {
        let _guard = if let Some(book_key) = &book_key {
            Some(state.reader.flow_lock(book_key).await)
        } else {
            None
        };
        if let Some(book_key) = &book_key
            && let Err(error) = invalidate_flow_cache(&state.cache, book_key).await
        {
            first_error.get_or_insert(error);
            continue;
        }
        if let Err(error) = state.store.delete_many(&keys_to_delete).await {
            first_error.get_or_insert(error.to_string());
        }
    }
    match first_error {
        Some(error) => Err(error),
        None => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use crate::auth::AuthService;
    use crate::config::Config;
    use crate::db::Database;
    use crate::storage::LocalStore;

    async fn test_state() -> Arc<AppState> {
        let root =
            std::env::temp_dir().join(format!("revaro-maintenance-{}", uuid::Uuid::new_v4()));
        let config = Config::from_lookup(&|name| match name {
            "APP_BASE_URL" => Some("http://localhost:8080".to_owned()),
            "APP_CACHES_DIR" => Some(root.join("caches").display().to_string()),
            "APP_WEB_DIR" => Some(root.join("web").display().to_string()),
            _ => None,
        })
        .expect("test configuration is valid");
        let database = Database::open_in_memory().expect("database opens");
        let store = LocalStore::open(root.join("objects"))
            .await
            .expect("store opens");
        let auth = AuthService::new(database.clone());
        AppState::new(Arc::new(config), database, store, auth)
    }

    #[tokio::test]
    async fn run_now_job_runs_and_close_waits_for_it() {
        let runtime = MaintenanceRuntime::new();
        let started = Arc::new(AtomicUsize::new(0));
        let released = Arc::new(tokio::sync::Notify::new());
        let started_for_job = Arc::clone(&started);
        let released_for_job = Arc::clone(&released);
        runtime
            .register(
                "test",
                Duration::from_secs(60),
                Duration::from_secs(5),
                true,
                move || {
                    let started = Arc::clone(&started_for_job);
                    let released = Arc::clone(&released_for_job);
                    async move {
                        started.fetch_add(1, Ordering::SeqCst);
                        released.notified().await;
                        Ok(())
                    }
                },
            )
            .unwrap();
        runtime.start();
        for _ in 0..20 {
            if started.load(Ordering::SeqCst) == 1 {
                break;
            }
            tokio::task::yield_now().await;
        }
        assert_eq!(started.load(Ordering::SeqCst), 1);
        let close = tokio::spawn({
            let runtime = runtime.clone();
            async move { runtime.close().await }
        });
        tokio::task::yield_now().await;
        assert!(!close.is_finished(), "close drains the active pass");
        released.notify_one();
        close.await.unwrap();
    }

    #[tokio::test]
    async fn wake_coalesces_during_a_running_pass() {
        let runtime = MaintenanceRuntime::new();
        let calls = Arc::new(AtomicUsize::new(0));
        let release = Arc::new(tokio::sync::Notify::new());
        let calls_for_job = Arc::clone(&calls);
        let release_for_job = Arc::clone(&release);
        runtime
            .register(
                "test",
                Duration::from_secs(60),
                Duration::from_secs(5),
                true,
                move || {
                    let calls = Arc::clone(&calls_for_job);
                    let release = Arc::clone(&release_for_job);
                    async move {
                        calls.fetch_add(1, Ordering::SeqCst);
                        release.notified().await;
                        Ok(())
                    }
                },
            )
            .unwrap();
        runtime.start();
        for _ in 0..20 {
            if calls.load(Ordering::SeqCst) == 1 {
                break;
            }
            tokio::task::yield_now().await;
        }
        assert!(runtime.wake("test"));
        release.notify_one();
        for _ in 0..40 {
            if calls.load(Ordering::SeqCst) == 2 {
                break;
            }
            tokio::task::yield_now().await;
        }
        assert_eq!(calls.load(Ordering::SeqCst), 2);
        assert_eq!(runtime.job_state("test"), Some((0, true)));
        release.notify_one();
        runtime.close().await;
    }

    #[tokio::test]
    async fn failures_back_off_until_an_explicit_wake() {
        let runtime = MaintenanceRuntime::new();
        let calls = Arc::new(AtomicUsize::new(0));
        let calls_for_job = Arc::clone(&calls);
        runtime
            .register(
                "failing",
                Duration::from_secs(60),
                Duration::from_secs(5),
                true,
                move || {
                    let calls = Arc::clone(&calls_for_job);
                    async move {
                        calls.fetch_add(1, Ordering::SeqCst);
                        Err("test failure".to_owned())
                    }
                },
            )
            .unwrap();
        runtime.start();
        for _ in 0..20 {
            if runtime
                .job_state("failing")
                .is_some_and(|(failures, _)| failures == 1)
            {
                break;
            }
            tokio::task::yield_now().await;
        }
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert_eq!(runtime.job_state("failing"), Some((1, false)));
        assert!(runtime.wake("failing"));
        for _ in 0..20 {
            if calls.load(Ordering::SeqCst) == 2 {
                break;
            }
            tokio::task::yield_now().await;
        }
        assert_eq!(calls.load(Ordering::SeqCst), 2);
        assert_eq!(runtime.job_state("failing"), Some((2, false)));
        runtime.close().await;
    }

    #[tokio::test]
    async fn expired_upload_is_removed_transactionally_and_object_is_deleted() {
        let state = test_state().await;
        let now = Timestamp::now().to_rfc3339();
        let old = Timestamp::epoch().to_rfc3339();
        state
            .db
            .call({
                let now = now.clone();
                move |connection| {
                    connection.execute(
                        "INSERT INTO files(id,parent_id,name,kind,object_key,size,status,created_at,updated_at) \
                         VALUES('expired',?1,'old.bin','file','blobs/expired',3,'pending',?2,?2)",
                        rusqlite::params![revaro_core::ids::ROOT_ID, now],
                    ).map_err(DbError::Query)?;
                    connection.execute(
                        "INSERT INTO uploads(id,file_id,mode,object_key,part_size,expected_size,mime_type,status,created_at,expires_at) \
                         VALUES('upload-expired','expired','single','blobs/expired',3,3,'application/octet-stream','pending',?1,?2)",
                        rusqlite::params![now, old],
                    ).map_err(DbError::Query)?;
                    Ok(())
                }
            })
            .await
            .unwrap();
        state.store.put("blobs/expired", b"old").await.unwrap();
        cleanup_expired_uploads(&state).await.unwrap();
        let remaining = state
            .db
            .call(|connection| {
                connection
                    .query_row(
                        "SELECT COUNT(*) FROM uploads WHERE id='upload-expired' OR file_id='expired'",
                        [],
                        |row| row.get::<_, i64>(0),
                    )
                    .map_err(DbError::Query)
            })
            .await
            .unwrap();
        assert_eq!(remaining, 0);
        assert!(state.store.head("blobs/expired").await.is_err());
    }

    #[tokio::test]
    async fn object_cleanup_keeps_referenced_objects_and_removes_derivatives() {
        let state = test_state().await;
        let key = "blobs/cleanup-file";
        let file_id = "cleanup-file";
        let now = Timestamp::now().to_rfc3339();
        state
            .db
            .call({
                let now = now.clone();
                move |connection| {
                    connection.execute(
                        "INSERT INTO files(id,parent_id,name,kind,object_key,size,status,created_at,updated_at) \
                         VALUES(?1,?2,'file.epub','file',?3,1,'ready',?4,?4)",
                        rusqlite::params![file_id, revaro_core::ids::ROOT_ID, key, now],
                    ).map_err(DbError::Query)?;
                    connection.execute(
                        "INSERT INTO object_cleanup(object_key,reason,created_at,updated_at) VALUES(?1,'test',?2,?2)",
                        rusqlite::params![key, now],
                    ).map_err(DbError::Query)?;
                    Ok(())
                }
            })
            .await
            .unwrap();
        state.store.put(key, b"x").await.unwrap();
        cleanup_object_queue(&state).await.unwrap();
        assert!(state.store.head(key).await.is_ok());
        let queued = state
            .db
            .call(|connection| {
                connection
                    .query_row("SELECT COUNT(*) FROM object_cleanup", [], |row| {
                        row.get::<_, i64>(0)
                    })
                    .map_err(DbError::Query)
            })
            .await
            .unwrap();
        assert_eq!(queued, 0);

        state
            .db
            .call(move |connection| {
                connection
                    .execute("DELETE FROM files WHERE id=?1", [file_id])
                    .map_err(DbError::Query)?;
                Ok(())
            })
            .await
            .unwrap();
        for derivative in [
            keys::thumbnail_v2_key(key),
            keys::image_thumbnail_key(key),
            keys::audio_thumbnail_key(key),
            keys::video_thumbnail_key(key),
        ] {
            state.store.put(&derivative, b"x").await.unwrap();
        }
        cleanup_object_queue(&state).await.unwrap();
        assert!(state.store.head(key).await.is_err());
        assert!(
            state
                .store
                .list_prefix(keys::THUMBNAIL_ROOT)
                .await
                .unwrap()
                .is_empty()
        );
    }

    #[tokio::test]
    async fn trash_cleanup_removes_old_leaves_before_their_parent() {
        let state = test_state().await;
        let old = Timestamp::epoch().to_rfc3339();
        let now = Timestamp::now().to_rfc3339();
        state
            .db
            .call({
                let old = old.clone();
                let now = now.clone();
                move |connection| {
                    connection
                        .execute(
                            "INSERT INTO files(id,parent_id,name,kind,status,created_at,updated_at,deleted_at,trash_root_id) \
                             VALUES('trash-dir',?1,'old','directory','ready',?2,?2,?3,'trash-dir')",
                            rusqlite::params![revaro_core::ids::ROOT_ID, now, old],
                        )
                        .map_err(DbError::Query)?;
                    connection
                        .execute(
                            "INSERT INTO files(id,parent_id,name,kind,object_key,size,status,created_at,updated_at,deleted_at,trash_root_id) \
                             VALUES('trash-file','trash-dir','old.bin','file','blobs/trash-file',1,'ready',?1,?1,?2,'trash-dir')",
                            rusqlite::params![now, old],
                        )
                        .map_err(DbError::Query)?;
                    Ok(())
                }
            })
            .await
            .unwrap();
        state.store.put("blobs/trash-file", b"x").await.unwrap();
        cleanup_trash(&state).await.unwrap();
        let files = state
            .db
            .call(|connection| {
                connection
                    .query_row(
                        "SELECT COUNT(*) FROM files WHERE id IN ('trash-dir','trash-file')",
                        [],
                        |row| row.get::<_, i64>(0),
                    )
                    .map_err(DbError::Query)
            })
            .await
            .unwrap();
        assert_eq!(files, 0);
        cleanup_object_queue(&state).await.unwrap();
        assert!(state.store.head("blobs/trash-file").await.is_err());
    }

    #[tokio::test]
    async fn orphan_gc_removes_unreferenced_blob_and_flow_objects() {
        let state = test_state().await;
        state.store.put("blobs/orphan", b"x").await.unwrap();
        state.store.put("thumbs/aa/orphan.jpg", b"x").await.unwrap();
        state
            .store
            .put("flows/blobs/missing/f4/chunks/0.html", b"<p>orphan</p>")
            .await
            .unwrap();
        collect_garbage_at(&state, &References::default(), Timestamp::now())
            .await
            .unwrap();
        assert!(state.store.head("blobs/orphan").await.is_err());
        assert!(state.store.head("thumbs/aa/orphan.jpg").await.is_err());
        assert!(
            state
                .store
                .head("flows/blobs/missing/f4/chunks/0.html")
                .await
                .is_err()
        );
    }
}

//! Process-wide lifecycle state for archive extraction jobs.
//!
//! The database is the durable source of truth, while this small runtime owns
//! cancellation tokens, the single archive worker slot and the transient job
//! snapshots needed by password input. Passwords are never stored in either
//! the snapshot or the runtime map.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use revaro_core::api::archive::{Job as ArchiveJob, JobStatus};
use revaro_core::time::Timestamp;
use revaro_media::ArchiveEngine;
use tokio::sync::{OwnedSemaphorePermit, Semaphore};
use tokio_util::sync::CancellationToken;

/// How long a password prompt remains actionable.
pub const PASSWORD_WAIT_TTL: Duration = Duration::from_secs(30 * 60);

/// Why an archive worker could not acquire the process-wide slot.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArchiveRuntimeError {
    /// The process is shutting down or the semaphore has been closed.
    ShuttingDown,
}

/// Runtime resources shared by archive routes and background workers.
#[derive(Debug, Clone)]
pub struct ArchiveRuntime {
    /// Native archive decoder and bounded extractor.
    pub engine: ArchiveEngine,
    /// Only one archive is expanded at a time, matching the old worker limit.
    slots: Arc<Semaphore>,
    jobs: Arc<Mutex<HashMap<String, Arc<ArchiveJobHandle>>>>,
    shutdown: CancellationToken,
}

/// The mutable state of one archive job.
#[derive(Debug)]
pub struct ArchiveJobHandle {
    job: Mutex<ArchiveJob>,
    cancel: Mutex<CancellationToken>,
    password_deadline: Mutex<Option<Instant>>,
}

impl ArchiveRuntime {
    /// Create an empty archive runtime.
    #[must_use]
    pub fn new() -> Self {
        Self {
            engine: ArchiveEngine,
            slots: Arc::new(Semaphore::new(1)),
            jobs: Arc::new(Mutex::new(HashMap::new())),
            shutdown: CancellationToken::new(),
        }
    }

    /// Register or replace a durable job snapshot in the process map.
    pub fn register(&self, job: ArchiveJob) -> Arc<ArchiveJobHandle> {
        let token = self.shutdown.child_token();
        let handle = Arc::new(ArchiveJobHandle {
            job: Mutex::new(job),
            cancel: Mutex::new(token),
            password_deadline: Mutex::new(None),
        });
        self.jobs
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(handle.id(), Arc::clone(&handle));
        handle
    }

    /// Find a live process job by its durable id.
    #[must_use]
    pub fn get(&self, id: &str) -> Option<Arc<ArchiveJobHandle>> {
        self.jobs
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(id)
            .cloned()
    }

    /// Remove a job from the process map after its worker has settled.
    pub fn remove(&self, id: &str) -> Option<Arc<ArchiveJobHandle>> {
        self.jobs
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(id)
    }

    /// Remove a job only when it is still backed by the expected worker.
    ///
    /// A retry can install a new handle under the same durable task id while a
    /// previous worker is finishing its cleanup. Pointer identity prevents the
    /// old worker from deleting the new attempt from the process map.
    pub fn remove_if(&self, id: &str, expected: &Arc<ArchiveJobHandle>) -> bool {
        let mut jobs = self.jobs.lock().unwrap_or_else(PoisonError::into_inner);
        if jobs
            .get(id)
            .is_some_and(|current| Arc::ptr_eq(current, expected))
        {
            jobs.remove(id);
            true
        } else {
            false
        }
    }

    /// Acquire the archive slot, also observing process shutdown.
    pub async fn acquire_slot(&self) -> Result<OwnedSemaphorePermit, ArchiveRuntimeError> {
        tokio::select! {
            _ = self.shutdown.cancelled() => Err(ArchiveRuntimeError::ShuttingDown),
            permit = Arc::clone(&self.slots).acquire_owned() => permit.map_err(|_| ArchiveRuntimeError::ShuttingDown),
        }
    }

    /// Make a fresh child token for a new attempt of an existing job.
    #[must_use]
    pub fn start_token(&self, id: &str) -> Option<CancellationToken> {
        let handle = self.get(id)?;
        let mut current = handle.cancel.lock().unwrap_or_else(PoisonError::into_inner);
        // A cancellation request can arrive after the task is registered but
        // before its Tokio future gets its first poll. Preserve that cancelled
        // token instead of replacing it with a fresh one and losing the user
        // request. Retries register a new handle, so they still receive a new
        // token.
        if current.is_cancelled() {
            return Some(current.clone());
        }
        let token = self.shutdown.child_token();
        *current = token.clone();
        Some(token)
    }

    /// Request cancellation of a live worker, if one exists.
    pub fn cancel(&self, id: &str) -> Option<Arc<ArchiveJobHandle>> {
        let handle = self.get(id)?;
        handle.cancel();
        Some(handle)
    }

    /// Cancel every live worker during graceful process shutdown.
    pub fn shutdown(&self) {
        self.shutdown.cancel();
        let jobs = self.jobs.lock().unwrap_or_else(PoisonError::into_inner);
        for handle in jobs.values() {
            handle.cancel();
        }
    }

    /// Update a snapshot and return its new value.
    pub fn update(
        &self,
        id: &str,
        status: JobStatus,
        progress: i32,
        message: impl Into<String>,
        error: impl Into<String>,
    ) -> Option<ArchiveJob> {
        self.get(id).map(|handle| {
            handle.update(status, progress, message.into(), error.into());
            handle.snapshot()
        })
    }

    /// Move a waiting job back to the checking phase exactly once.
    pub fn resume_for_password(&self, id: &str) -> Option<Arc<ArchiveJobHandle>> {
        let handle = self.get(id)?;
        if handle.resume_for_password() {
            Some(handle)
        } else {
            None
        }
    }

    /// Mark an expired password prompt as failed and return the snapshot.
    pub fn expire_password_wait(&self, id: &str) -> Option<ArchiveJob> {
        let handle = self.get(id)?;
        if handle.expire_password_wait() {
            Some(handle.snapshot())
        } else {
            None
        }
    }

    /// Mark a job cancelled when no worker is left to observe its token.
    pub fn mark_cancelled(&self, id: &str) -> Option<ArchiveJob> {
        self.get(id).map(|handle| {
            handle.cancel();
            handle.update(
                JobStatus::Cancelled,
                handle.snapshot().progress,
                "已取消".to_owned(),
                String::new(),
            );
            handle.snapshot()
        })
    }
}

impl Default for ArchiveRuntime {
    fn default() -> Self {
        Self::new()
    }
}

impl ArchiveJobHandle {
    /// The durable identifier of this job.
    #[must_use]
    pub fn id(&self) -> String {
        self.snapshot().id
    }

    /// The source file identifier.
    #[must_use]
    pub fn file_id(&self) -> String {
        self.snapshot().file_id
    }

    /// The destination directory identifier.
    #[must_use]
    pub fn parent_id(&self) -> String {
        self.snapshot().parent_id
    }

    /// Return a consistent API snapshot.
    #[must_use]
    pub fn snapshot(&self) -> ArchiveJob {
        self.job
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    /// Request cancellation of the current attempt.
    pub fn cancel(&self) {
        self.cancel
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .cancel();
    }

    /// Update status and monotonic progress.
    pub fn update(&self, status: JobStatus, progress: i32, message: String, error: String) {
        let mut job = self.job.lock().unwrap_or_else(PoisonError::into_inner);
        job.status = status;
        job.progress = job.progress.max(progress.clamp(0, 100));
        job.message = message;
        job.error = error;
        job.updated_at = Timestamp::now();
        if status != JobStatus::WaitingPassword {
            *self
                .password_deadline
                .lock()
                .unwrap_or_else(PoisonError::into_inner) = None;
        }
    }

    /// Set the password prompt and its deadline.
    pub fn wait_for_password(&self, message: String) {
        let error = message.clone();
        self.update(
            JobStatus::WaitingPassword,
            self.snapshot().progress,
            message,
            error,
        );
        *self
            .password_deadline
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = Some(Instant::now() + PASSWORD_WAIT_TTL);
    }

    fn resume_for_password(&self) -> bool {
        let mut job = self.job.lock().unwrap_or_else(PoisonError::into_inner);
        if job.status != JobStatus::WaitingPassword {
            return false;
        }
        let expired = self
            .password_deadline
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .is_some_and(|deadline| Instant::now() >= deadline);
        if expired {
            return false;
        }
        job.status = JobStatus::Checking;
        job.message = "正在验证密码".to_owned();
        job.error.clear();
        job.updated_at = Timestamp::now();
        *self
            .password_deadline
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = None;
        true
    }

    fn expire_password_wait(&self) -> bool {
        let mut job = self.job.lock().unwrap_or_else(PoisonError::into_inner);
        if job.status != JobStatus::WaitingPassword {
            return false;
        }
        let expired = self
            .password_deadline
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .is_some_and(|deadline| Instant::now() >= deadline);
        if !expired {
            return false;
        }
        let message = "解压失败：30 分钟内未输入密码，任务已超时".to_owned();
        job.status = JobStatus::Failed;
        job.message = message.clone();
        job.error = message;
        job.updated_at = Timestamp::now();
        drop(job);
        *self
            .password_deadline
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = None;
        true
    }

    /// Attach the directory created by a successful import.
    pub fn replace_output(&self, output_id: &str, output_name: &str) {
        let mut job = self.job.lock().unwrap_or_else(PoisonError::into_inner);
        job.output_id = output_id.to_owned();
        job.output_name = output_name.to_owned();
        job.updated_at = Timestamp::now();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn job(id: &str) -> ArchiveJob {
        let now = Timestamp::now();
        ArchiveJob {
            id: id.to_owned(),
            file_id: "file".to_owned(),
            parent_id: "parent".to_owned(),
            name: "archive.zip".to_owned(),
            status: JobStatus::Queued,
            progress: 0,
            message: "等待解压".to_owned(),
            output_id: String::new(),
            output_name: String::new(),
            error: String::new(),
            created_at: now,
            updated_at: now,
        }
    }

    #[test]
    fn a_cancel_before_the_worker_first_polls_is_preserved() {
        let runtime = ArchiveRuntime::new();
        let id = "archive-job";
        runtime.register(job(id));
        runtime.cancel(id);
        assert!(runtime.start_token(id).unwrap().is_cancelled());
    }

    #[test]
    fn expired_password_wait_cannot_be_resumed() {
        let runtime = ArchiveRuntime::new();
        let id = "archive-password-job";
        let handle = runtime.register(job(id));
        handle.wait_for_password("需要密码".to_owned());
        *handle
            .password_deadline
            .lock()
            .unwrap_or_else(PoisonError::into_inner) =
            Some(Instant::now() - Duration::from_secs(1));

        assert!(runtime.resume_for_password(id).is_none());
        assert_eq!(handle.snapshot().status, JobStatus::WaitingPassword);
        assert!(runtime.expire_password_wait(id).is_some());
        assert_eq!(handle.snapshot().status, JobStatus::Failed);
    }
}

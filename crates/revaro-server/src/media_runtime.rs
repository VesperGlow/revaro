//! Process-wide coordination for native media work.
//!
//! FFmpeg is deliberately kept behind a small amount of scheduling state. A
//! request can ask for metadata, a thumbnail and a subtitle at the same time;
//! bounded permits keep those blocking operations from consuming every worker
//! thread, while keyed locks make one source produce one durable result.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex, PoisonError};

use revaro_media::MediaEngine;

/// Runtime state shared by media routes and their short-lived background jobs.
#[derive(Debug)]
pub struct MediaRuntime {
    /// The native FFmpeg/image engine.
    pub engine: MediaEngine,
    /// Maximum simultaneous probe/video/subtitle operations.
    pub light_slots: Arc<tokio::sync::Semaphore>,
    /// Maximum simultaneous still-image/EPUB thumbnail decodes.
    pub image_slots: Arc<tokio::sync::Semaphore>,
    metadata_locks: Mutex<HashMap<String, Arc<tokio::sync::Mutex<()>>>>,
    thumbnail_locks: Mutex<HashMap<String, Arc<tokio::sync::Mutex<()>>>>,
    video_thumbnail_jobs: Mutex<HashSet<String>>,
    analysis_jobs: Mutex<HashSet<String>>,
}

impl MediaRuntime {
    /// Create the bounded media runtime used by a server process.
    #[must_use]
    pub fn new() -> Self {
        Self {
            engine: MediaEngine,
            light_slots: Arc::new(tokio::sync::Semaphore::new(2)),
            image_slots: Arc::new(tokio::sync::Semaphore::new(2)),
            metadata_locks: Mutex::new(HashMap::new()),
            thumbnail_locks: Mutex::new(HashMap::new()),
            video_thumbnail_jobs: Mutex::new(HashSet::new()),
            analysis_jobs: Mutex::new(HashSet::new()),
        }
    }

    /// Serialize probes for one file id and source version.
    pub async fn metadata_lock(&self, key: &str) -> tokio::sync::OwnedMutexGuard<()> {
        let lock = self.lock_for(key, &self.metadata_locks);
        lock.lock_owned().await
    }

    /// Serialize one derived thumbnail or cover build.
    pub async fn thumbnail_lock(&self, key: &str) -> tokio::sync::OwnedMutexGuard<()> {
        let lock = self.lock_for(key, &self.thumbnail_locks);
        lock.lock_owned().await
    }

    /// Claim a video thumbnail job. Returns false when an equivalent job is
    /// already queued or running.
    pub fn claim_video_thumbnail(&self, key: &str) -> bool {
        self.video_thumbnail_jobs
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(key.to_owned())
    }

    /// Release a video thumbnail job after it has settled.
    pub fn release_video_thumbnail(&self, key: &str) {
        self.video_thumbnail_jobs
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(key);
    }

    /// Claim a background metadata analysis for one file.
    pub fn claim_analysis(&self, key: &str) -> bool {
        self.analysis_jobs
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(key.to_owned())
    }

    /// Release a background metadata analysis after it settles.
    pub fn release_analysis(&self, key: &str) {
        self.analysis_jobs
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(key);
    }

    fn lock_for(
        &self,
        key: &str,
        locks: &Mutex<HashMap<String, Arc<tokio::sync::Mutex<()>>>>,
    ) -> Arc<tokio::sync::Mutex<()>> {
        let mut locks = locks.lock().unwrap_or_else(PoisonError::into_inner);
        // The map owns one reference. Active guards and waiters own the others,
        // so idle entries can be discarded without disturbing in-flight work.
        locks.retain(|_, lock| Arc::strong_count(lock) > 1);
        locks
            .entry(key.to_owned())
            .or_insert_with(|| Arc::new(tokio::sync::Mutex::new(())))
            .clone()
    }
}

impl Default for MediaRuntime {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn media_locks_prune_idle_keys_and_keep_jobs_deduplicated() {
        let runtime = MediaRuntime::new();
        {
            let _guard = runtime.metadata_lock("first").await;
            assert_eq!(runtime.metadata_locks.lock().unwrap().len(), 1);
        }
        let _guard = runtime.metadata_lock("second").await;
        assert_eq!(runtime.metadata_locks.lock().unwrap().len(), 1);
        assert!(runtime.claim_video_thumbnail("video"));
        assert!(!runtime.claim_video_thumbnail("video"));
        runtime.release_video_thumbnail("video");
        assert!(runtime.claim_video_thumbnail("video"));
    }
}

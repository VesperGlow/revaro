//! Process-wide coordination for native media work.
//!
//! FFmpeg is deliberately kept behind a small amount of scheduling state. A
//! request can ask for metadata, a thumbnail and a subtitle at the same time;
//! bounded permits keep those blocking operations from consuming every worker
//! thread, while keyed locks make one source produce one durable result.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

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
    subtitle_cache: Mutex<SubtitleCache>,
}

impl MediaRuntime {
    /// Create the bounded media runtime used by a server process.
    #[must_use]
    pub fn new() -> Self {
        Self::with_cache_capacity(64 << 20)
    }

    /// Create the runtime with the configured converted-subtitle cache size.
    #[must_use]
    pub fn with_cache_capacity(capacity: usize) -> Self {
        Self {
            engine: MediaEngine,
            light_slots: Arc::new(tokio::sync::Semaphore::new(2)),
            image_slots: Arc::new(tokio::sync::Semaphore::new(2)),
            metadata_locks: Mutex::new(HashMap::new()),
            thumbnail_locks: Mutex::new(HashMap::new()),
            video_thumbnail_jobs: Mutex::new(HashSet::new()),
            analysis_jobs: Mutex::new(HashSet::new()),
            subtitle_cache: Mutex::new(SubtitleCache::new(capacity)),
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

    /// Return a fresh converted subtitle from the bounded memory cache.
    pub fn subtitle_cache_get(&self, key: &str) -> Option<Vec<u8>> {
        self.subtitle_cache
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(key)
    }

    /// Insert one converted subtitle. Values larger than the cache capacity
    /// are intentionally left uncached rather than evicting every smaller
    /// subtitle in the process.
    pub fn subtitle_cache_put(&self, key: String, value: Vec<u8>, ttl: Duration) {
        self.subtitle_cache
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .put(key, value, ttl);
    }

    /// Drop all converted subtitles after a media source is re-analysed.
    pub fn clear_subtitle_cache(&self) {
        self.subtitle_cache
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clear();
    }

    /// Drop embedded subtitles belonging to one media source after a forced
    /// re-analysis. External subtitle values are keyed by their own file
    /// version and remain valid when only the video metadata changes.
    pub fn clear_subtitle_cache_for(&self, file_id: &str) {
        let mut cache = self
            .subtitle_cache
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        cache.remove_prefix(&format!("embedded-v2:{file_id}:"));
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

#[derive(Debug)]
struct SubtitleCache {
    capacity: usize,
    used: usize,
    entries: HashMap<String, SubtitleCacheEntry>,
}

#[derive(Debug)]
struct SubtitleCacheEntry {
    value: Vec<u8>,
    expires_at: Instant,
    used_at: Instant,
}

impl SubtitleCache {
    fn new(capacity: usize) -> Self {
        Self {
            capacity,
            used: 0,
            entries: HashMap::new(),
        }
    }

    fn get(&mut self, key: &str) -> Option<Vec<u8>> {
        self.remove_expired();
        let entry = self.entries.get_mut(key)?;
        entry.used_at = Instant::now();
        Some(entry.value.clone())
    }

    fn put(&mut self, key: String, value: Vec<u8>, ttl: Duration) {
        self.remove_expired();
        if value.len() > self.capacity {
            return;
        }
        if let Some(previous) = self.entries.remove(&key) {
            self.used = self.used.saturating_sub(previous.value.len());
        }
        while self.used.saturating_add(value.len()) > self.capacity {
            let Some(oldest) = self
                .entries
                .iter()
                .min_by_key(|(_, entry)| entry.used_at)
                .map(|(key, _)| key.clone())
            else {
                break;
            };
            if let Some(previous) = self.entries.remove(&oldest) {
                self.used = self.used.saturating_sub(previous.value.len());
            }
        }
        let now = Instant::now();
        self.used += value.len();
        self.entries.insert(
            key,
            SubtitleCacheEntry {
                value,
                expires_at: now + ttl,
                used_at: now,
            },
        );
    }

    fn clear(&mut self) {
        self.used = 0;
        self.entries.clear();
    }

    fn remove_prefix(&mut self, prefix: &str) {
        let keys: Vec<_> = self
            .entries
            .keys()
            .filter(|key| key.starts_with(prefix))
            .cloned()
            .collect();
        for key in keys {
            if let Some(entry) = self.entries.remove(&key) {
                self.used = self.used.saturating_sub(entry.value.len());
            }
        }
    }

    fn remove_expired(&mut self) {
        let now = Instant::now();
        self.entries.retain(|_, entry| {
            if entry.expires_at > now {
                true
            } else {
                self.used = self.used.saturating_sub(entry.value.len());
                false
            }
        });
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
        runtime.subtitle_cache_put(
            "subtitle".to_owned(),
            b"WEBVTT".to_vec(),
            Duration::from_secs(60),
        );
        runtime.subtitle_cache_put(
            "embedded-v2:file-a:etag:time:1".to_owned(),
            b"embedded".to_vec(),
            Duration::from_secs(60),
        );
        assert_eq!(runtime.subtitle_cache_get("subtitle").unwrap(), b"WEBVTT");
        runtime.clear_subtitle_cache_for("file-a");
        assert!(
            runtime
                .subtitle_cache_get("embedded-v2:file-a:etag:time:1")
                .is_none()
        );
        assert_eq!(runtime.subtitle_cache_get("subtitle").unwrap(), b"WEBVTT");
        runtime.clear_subtitle_cache();
        assert!(runtime.subtitle_cache_get("subtitle").is_none());
    }
}

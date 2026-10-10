//! Account-wide audio ownership and the durable queue observed by other devices.

use leptos::prelude::*;
use revaro_core::progress::{ListeningQueue, ListeningSession, ListeningWrite, ProgressWrite};

#[derive(Clone, Copy)]
pub(super) struct ListeningSync {
    pub server: RwSignal<Option<ListeningSession>>,
    writer: RwSignal<String>,
    sequence: RwSignal<u64>,
    pub fenced: RwSignal<bool>,
    pub stopping: RwSignal<bool>,
    pub activity: RwSignal<u64>,
}

impl ListeningSync {
    pub fn new() -> Self {
        Self {
            server: RwSignal::new(None),
            writer: RwSignal::new(super::playback::new_writer()),
            sequence: RwSignal::new(0),
            fenced: RwSignal::new(false),
            stopping: RwSignal::new(false),
            activity: RwSignal::new(0),
        }
    }
    pub fn intent(self) {
        self.activity.update(|value| *value += 1);
        self.stopping.set(false);
        if self.fenced.get_untracked() {
            self.writer.set(super::playback::new_writer());
            self.sequence.set(0);
            self.fenced.set(false);
        }
    }
    pub fn write(self, queue: ListeningQueue) -> Option<ListeningWrite> {
        if self.fenced.get_untracked() {
            return None;
        }
        let server = self.server.get_untracked()?;
        let sequence = self.sequence.get_untracked().saturating_add(1);
        self.sequence.set(sequence);
        Some(ListeningWrite {
            queue,
            sync: ProgressWrite {
                writer: self.writer.get_untracked(),
                sequence,
                base_revision: server.revision,
            },
        })
    }
    pub fn completion(
        self,
        write: &ListeningWrite,
    ) -> std::sync::Arc<dyn Fn(revaro_core::model::MediaProgress) + Send + Sync> {
        let writer = write.sync.writer.clone();
        let queue = write.queue.clone();
        let sequence = write.sync.sequence;
        std::sync::Arc::new(move |saved: revaro_core::model::MediaProgress| {
            if self.writer.try_get_untracked().as_deref() != Some(&writer) {
                return;
            }
            if let Some(revision) = saved.listening_revision
                && self
                    .server
                    .get_untracked()
                    .is_none_or(|old| old.revision < revision)
            {
                self.server.set(Some(ListeningSession {
                    queue: queue.clone(),
                    revision,
                    writer: Some(writer.clone()),
                    sequence,
                }));
            }
        })
    }
    pub fn receive(self, session: ListeningSession) -> bool {
        if self
            .server
            .get_untracked()
            .is_some_and(|old| old.revision >= session.revision)
        {
            return false;
        }
        let remote = session.writer.as_deref() != Some(&self.writer.get_untracked());
        self.server.set(Some(session));
        if remote {
            self.fenced.set(true);
        }
        remote
    }
}

//! Durable, idempotent progress writes survive player teardown and reload.

use crate::{api, browser};
use revaro_core::{model::MediaProgress, progress::SaveMediaProgress};
use serde::{Deserialize, Serialize};
use std::{cell::RefCell, collections::HashMap};
use wasm_bindgen::{JsCast, closure::Closure};

const PREFIX: &str = "revaro-media-outbox-v1:";
type Completion = std::sync::Arc<dyn Fn(Result<MediaProgress, api::RequestError>) + Send + Sync>;

#[derive(Clone, Serialize, Deserialize)]
struct Journal {
    file: String,
    request: SaveMediaProgress,
    #[serde(default)]
    queue_only: bool,
}
struct Entry {
    journal: Journal,
    callback: Option<Completion>,
    busy: bool,
    retry: Option<i32>,
    failures: u32,
}
thread_local! {
    static PENDING: RefCell<HashMap<String, Entry>> = RefCell::new(HashMap::new());
    static ACCOUNT: RefCell<String> = const { RefCell::new(String::new()) };
}

fn prefix() -> String {
    ACCOUNT.with(|account| format!("{PREFIX}{}:", account.borrow()))
}

pub(super) fn initialize(account: &str) {
    let changed = ACCOUNT.with(|value| {
        let changed = value.borrow().as_str() != account;
        *value.borrow_mut() = account.to_owned();
        changed
    });
    if changed {
        PENDING.with(|pending| {
            for entry in pending.borrow_mut().drain().map(|(_, entry)| entry) {
                if let Some(timer) = entry.retry
                    && let Some(window) = web_sys::window()
                {
                    window.clear_timeout_with_handle(timer);
                }
            }
        });
    }
    let Some(storage) = web_sys::window().and_then(|w| w.local_storage().ok().flatten()) else {
        return;
    };
    let prefix = prefix();
    let journals = (0..storage.length().unwrap_or(0))
        .filter_map(|i| storage.key(i).ok().flatten())
        .filter(|key| key.starts_with(&prefix))
        .filter_map(|key| storage.get_item(&key).ok().flatten())
        .filter_map(|raw| serde_json::from_str::<Journal>(&raw).ok())
        .filter(|journal| {
            revaro_core::progress::validate_write(&journal.request.sync).is_ok()
                && revaro_core::validate::validate_media_progress(
                    journal.request.position,
                    journal.request.duration,
                )
                .is_ok()
        })
        .collect::<Vec<_>>();
    for journal in journals {
        enqueue_journal(journal, None, true);
    }
}

pub(super) fn enqueue(
    file: String,
    request: SaveMediaProgress,
    callback: Option<Completion>,
    remote: bool,
) {
    enqueue_journal(
        Journal {
            file,
            request,
            queue_only: false,
        },
        callback,
        remote,
    );
}

pub(super) fn enqueue_listening(
    write: revaro_core::progress::ListeningWrite,
    callback: Option<Completion>,
) {
    let request = SaveMediaProgress {
        position: 0.0,
        duration: 0.0,
        completed: false,
        sync: write.sync.clone(),
        listening: Some(write),
    };
    enqueue_journal(
        Journal {
            file: String::new(),
            request,
            queue_only: true,
        },
        callback,
        true,
    );
}

fn enqueue_journal(journal: Journal, callback: Option<Completion>, remote: bool) {
    let key = format!("{}{}", prefix(), journal.request.sync.writer);
    if let Ok(raw) = serde_json::to_string(&journal) {
        browser::local_storage_set(&key, &raw);
    }
    PENDING.with(|pending| {
        let mut pending = pending.borrow_mut();
        if let Some(entry) = pending.get_mut(&key) {
            entry.journal = journal;
            entry.callback = callback;
        } else {
            pending.insert(
                key.clone(),
                Entry {
                    journal,
                    callback,
                    busy: false,
                    retry: None,
                    failures: 0,
                },
            );
        }
    });
    if remote {
        send(key);
    }
}

pub(super) fn pending_position(file: &str, remote: &MediaProgress) -> Option<f64> {
    PENDING.with(|pending| {
        pending
            .borrow()
            .values()
            .filter(|entry| entry.journal.file == file)
            .filter(|entry| {
                let sync = &entry.journal.request.sync;
                (remote.writer.as_deref() == Some(&sync.writer) && sync.sequence > remote.sequence)
                    || (remote.writer.as_deref() != Some(&sync.writer)
                        && sync.base_revision == remote.revision)
            })
            .max_by_key(|entry| entry.journal.request.sync.sequence)
            .map(|entry| {
                if entry.journal.request.completed {
                    0.0
                } else {
                    entry.journal.request.position
                }
            })
    })
}

pub(super) fn retry_all() {
    let keys = PENDING.with(|pending| pending.borrow().keys().cloned().collect::<Vec<_>>());
    for key in keys {
        send(key);
    }
}

pub(super) fn pending_listening(
    remote: &revaro_core::progress::ListeningSession,
) -> Option<revaro_core::progress::ListeningQueue> {
    PENDING.with(|pending| {
        pending
            .borrow()
            .values()
            .filter_map(|entry| entry.journal.request.listening.as_ref())
            .filter(|write| {
                revaro_core::progress::decide_write(&remote.ordering(), &write.sync)
                    == revaro_core::progress::WriteDecision::Accept
            })
            .max_by_key(|write| write.sync.sequence)
            .map(|write| write.queue.clone())
    })
}

fn send(key: String) {
    let start = PENDING.with(|pending| {
        let mut pending = pending.borrow_mut();
        let entry = pending.get_mut(&key)?;
        if entry.busy {
            return None;
        }
        if let Some(timer) = entry.retry.take()
            && let Some(window) = web_sys::window()
        {
            window.clear_timeout_with_handle(timer);
        }
        entry.busy = true;
        Some(())
    });
    if start.is_none() {
        return;
    }
    leptos::task::spawn_local(async move {
        loop {
            let Some((journal, callback)) = PENDING.with(|pending| {
                pending
                    .borrow()
                    .get(&key)
                    .map(|entry| (entry.journal.clone(), entry.callback.clone()))
            }) else {
                return;
            };
            let result = if journal.queue_only {
                if let Some(write) = &journal.request.listening {
                    api::save_listening_session_keepalive(write)
                        .await
                        .map(|session| MediaProgress {
                            sequence: session.sequence,
                            listening_revision: Some(session.revision),
                            ..Default::default()
                        })
                } else {
                    return;
                }
            } else {
                api::save_media_progress_keepalive(&journal.file, &journal.request).await
            };
            let terminal = result.is_ok()
                || result
                    .as_ref()
                    .is_err_and(|error| matches!(error.status, 400 | 403 | 404 | 409 | 410));
            let success = result.is_ok();
            let unauthorized = result.as_ref().is_err_and(|error| error.status == 401);
            let acknowledged = result.as_ref().ok().map(|saved| saved.sequence);
            if let Some(callback) = callback {
                callback(result);
            }
            let again = PENDING.with(|pending| {
                let mut pending = pending.borrow_mut();
                let Some(entry) = pending.get_mut(&key) else {
                    return false;
                };
                if success && entry.journal.request.sync.sequence > journal.request.sync.sequence {
                    return true;
                }
                if terminal {
                    pending.remove(&key);
                    if let Some(storage) =
                        web_sys::window().and_then(|w| w.local_storage().ok().flatten())
                    {
                        let removable = !success
                            || storage
                                .get_item(&key)
                                .ok()
                                .flatten()
                                .and_then(|raw| serde_json::from_str::<Journal>(&raw).ok())
                                .is_some_and(|current| {
                                    acknowledged.is_some_and(|sequence| {
                                        current.request.sync.sequence <= sequence
                                    })
                                });
                        if removable {
                            let _ = storage.remove_item(&key);
                        }
                    }
                } else {
                    entry.busy = false;
                    // Keep the journal across session expiry. Reauthentication
                    // restarts it; unauthenticated retries cannot make progress.
                    if unauthorized {
                        return false;
                    }
                    entry.failures = entry.failures.saturating_add(1);
                    let delay =
                        (1000_u32.saturating_mul(1 << entry.failures.min(5))).min(30_000) as i32;
                    if let Some(window) = web_sys::window() {
                        let retry_key = key.clone();
                        let callback = Closure::once(move || send(retry_key)).into_js_value();
                        entry.retry = window
                            .set_timeout_with_callback_and_timeout_and_arguments_0(
                                callback.unchecked_ref(),
                                delay,
                            )
                            .ok();
                    }
                }
                false
            });
            if !again {
                break;
            }
        }
    });
}

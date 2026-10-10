//! Shared progress lifecycle and timer policies for the three media players.

use leptos::prelude::*;
use revaro_core::model::MediaProgress;
use revaro_core::progress::{ProgressWrite, SaveMediaProgress};
use wasm_bindgen::{JsCast, closure::Closure};

use crate::{
    api, browser,
    logic::media::{PlaybackPosition, media_element_time},
};

#[derive(Clone, Copy)]
pub(super) struct PlaybackProgress {
    pub revision: RwSignal<u64>,
    pub loaded_file: RwSignal<Option<String>>,
    pending: RwSignal<Option<PlaybackPosition>>,
    pub ready: RwSignal<bool>,
    failed: RwSignal<bool>,
    loading: RwSignal<bool>,
    pub server: RwSignal<Option<MediaProgress>>,
    writer: RwSignal<String>,
    sequence: RwSignal<u64>,
    changed: RwSignal<bool>,
    pub fenced: RwSignal<bool>,
    completed: RwSignal<bool>,
    latest: RwSignal<Option<(String, f64, f64)>>,
    pub listening: RwSignal<Option<revaro_core::progress::ListeningWrite>>,
    pub on_saved: RwSignal<Option<std::sync::Arc<dyn Fn(MediaProgress) + Send + Sync>>>,
}

impl PlaybackProgress {
    pub fn new() -> Self {
        Self {
            revision: RwSignal::new(0),
            loaded_file: RwSignal::new(None),
            pending: RwSignal::new(None),
            ready: RwSignal::new(false),
            failed: RwSignal::new(false),
            loading: RwSignal::new(false),
            server: RwSignal::new(None),
            writer: RwSignal::new(new_writer()),
            sequence: RwSignal::new(0),
            changed: RwSignal::new(false),
            fenced: RwSignal::new(false),
            completed: RwSignal::new(false),
            latest: RwSignal::new(None),
            listening: RwSignal::new(None),
            on_saved: RwSignal::new(None),
        }
    }

    /// Close the write gate before a source change can emit old media events.
    pub fn reset(self, pending: Option<f64>) {
        self.ready.set(false);
        self.failed.set(false);
        self.loading.set(false);
        self.loaded_file.set(None);
        self.pending.set(pending.map(PlaybackPosition::Seek));
        self.server.set(None);
        self.writer.set(new_writer());
        self.sequence.set(0);
        self.changed.set(pending.is_some());
        self.fenced.set(false);
        self.completed.set(false);
        self.latest.set(None);
    }

    /// A user seek wins immediately over any outstanding history request.
    pub fn accept_seek(self) {
        self.pending.set(None);
        self.ready.set(true);
        self.failed.set(false);
        self.intent();
        self.changed.set(true);
        self.completed.set(false);
    }

    pub fn intent(self) {
        if self.fenced.get_untracked() {
            self.writer.set(new_writer());
            self.sequence.set(0);
            self.fenced.set(false);
        }
    }

    pub fn clock_changed(self) {
        if self.ready.get_untracked() && !self.fenced.get_untracked() {
            self.changed.set(true);
        }
    }

    pub fn ended(self) {
        self.completed.set(true);
        self.clock_changed();
    }

    /// Paused observers adopt newer remote progress without acquiring a writer.
    pub fn receive_remote(self, progress: MediaProgress) -> bool {
        let old = self.server.get_untracked();
        if old
            .as_ref()
            .is_some_and(|old| old.revision >= progress.revision)
        {
            return false;
        }
        if progress.writer.as_deref() == Some(&self.writer.get_untracked()) {
            self.server.set(Some(progress));
            return false;
        }
        self.server.set(Some(progress.clone()));
        self.changed.set(false);
        self.latest.set(None);
        self.fenced.set(true);
        self.ready.set(false);
        self.pending
            .set(Some(PlaybackPosition::Seek(if progress.completed {
                0.0
            } else {
                progress.position
            })));
        true
    }

    pub fn load(self, id: String, on_loaded: Callback<()>) {
        if self.loading.get_untracked() {
            return;
        }
        self.loading.set(true);
        self.failed.set(false);
        self.loaded_file.set(Some(id.clone()));
        if self.pending.get_untracked().is_some() {
            on_loaded.run(());
        }
        let revision = self.revision.get_untracked();
        leptos::task::spawn_local_scoped_with_cancellation(async move {
            let progress = api::fetch_media_progress(&id).await;
            if self.revision.try_get_untracked() != Some(revision)
                || self.loaded_file.try_get_untracked().flatten().as_deref() != Some(&id)
            {
                return;
            }
            self.loading.set(false);
            let Ok(progress) = progress else {
                // A failed read is not an empty history. Keep writes closed
                // until a successful retry or an intentional user seek.
                self.failed.set(true);
                return;
            };
            if self
                .server
                .get_untracked()
                .is_some_and(|old| old.revision > progress.revision)
            {
                return;
            }
            self.server.set(Some(progress.clone()));
            if self.ready.get_untracked() {
                flush_latest(self, true);
                return;
            }
            if self.pending.get_untracked().is_none() {
                let saved = super::progress_outbox::pending_position(&id, &progress).unwrap_or(
                    if progress.completed {
                        0.0
                    } else {
                        progress.position
                    },
                );
                self.pending.set(Some(
                    if progress.updated_at.is_some() || progress.revision > 0 {
                        PlaybackPosition::Seek(media_element_time(saved))
                    } else {
                        PlaybackPosition::Resume(media_element_time(saved))
                    },
                ));
            }
            on_loaded.run(());
        });
    }

    /// The caller supplies its metadata readiness.
    /// Authoritative zero is restored just like every other explicit position.
    pub fn restore(self, duration: f64, local_key: Option<&str>) -> Option<f64> {
        if self.ready.get_untracked() {
            return None;
        }
        let pending = self.pending.get_untracked()?;
        // Legacy local values are only a migration fallback for an empty
        // server history. In particular, remote zero is an authoritative seek.
        let local = self
            .server
            .get_untracked()
            .filter(|progress| progress.updated_at.is_none())
            .and_then(|_| {
                local_key
                    .and_then(browser::local_storage_get)
                    .and_then(|value| value.parse::<f64>().ok())
                    .filter(|value| value.is_finite() && *value > 0.0)
            });
        self.pending.set(None);
        self.ready.set(true);
        Some(pending.resolve(duration, local))
    }
}

#[component]
pub(super) fn ProgressRetry(
    playback: PlaybackProgress,
    file_id: Signal<String>,
    on_loaded: Callback<()>,
) -> impl IntoView {
    let owner = Owner::current().expect("progress retry belongs to a player");
    let retry =
        Callback::new(move |()| owner.with(|| playback.load(file_id.get_untracked(), on_loaded)));
    view! {
        <Show when=move || playback.failed.get() fallback=|| ()>
            <p class="audio-player-error" role="alert">
                "未能读取播放进度，原有进度已保留。"
                <button type="button" on:click=move |_| retry.run(())>"重试读取进度"</button>
            </p>
        </Show>
    }
}

#[derive(Clone, Copy)]
pub(super) enum ProgressDestination {
    Local,
    Remote,
    Keepalive,
}

pub(super) fn persist_progress(
    playback: PlaybackProgress,
    id: &str,
    position: f64,
    duration: f64,
    local_key: Option<&str>,
    destination: ProgressDestination,
) {
    if !position.is_finite()
        || !duration.is_finite()
        || position < 0.0
        || duration < 0.0
        || playback.fenced.get_untracked()
    {
        return;
    }
    if let Some(key) = local_key {
        browser::local_storage_set(key, &position.to_string());
    }
    playback
        .latest
        .set(Some((id.to_owned(), position, duration)));
    flush_latest(playback, !matches!(destination, ProgressDestination::Local));
}

pub(super) fn new_writer() -> String {
    format!(
        "{:x}-{:016x}-{:016x}",
        js_sys::Date::now() as u64,
        (js_sys::Math::random() * u64::MAX as f64) as u64,
        (js_sys::Math::random() * u64::MAX as f64) as u64
    )
}

fn flush_latest(playback: PlaybackProgress, remote: bool) {
    if !playback.changed.get_untracked() || playback.fenced.get_untracked() {
        return;
    }
    let Some(server) = playback.server.get_untracked() else {
        return;
    };
    let Some((id, position, duration)) = playback.latest.get_untracked() else {
        return;
    };
    let writer = playback.writer.get_untracked();
    let sequence = playback.sequence.get_untracked().saturating_add(1);
    playback.sequence.set(sequence);
    let request = SaveMediaProgress {
        position,
        duration,
        completed: playback.completed.get_untracked(),
        sync: ProgressWrite {
            writer: writer.clone(),
            sequence,
            base_revision: server.revision,
        },
        listening: playback.listening.get_untracked(),
    };
    let on_saved = playback.on_saved.get_untracked();
    super::progress_outbox::enqueue(
        id,
        request,
        Some(std::sync::Arc::new(
            move |result: Result<MediaProgress, api::RequestError>| {
                if playback.writer.try_get_untracked().as_deref() != Some(&writer) {
                    return;
                }
                match result {
                    Ok(saved) => {
                        if let Some(callback) = &on_saved {
                            callback(saved.clone());
                        }
                        if playback
                            .server
                            .get_untracked()
                            .is_none_or(|old| old.revision < saved.revision)
                        {
                            playback.server.set(Some(saved));
                        }
                        if playback.sequence.get_untracked() == sequence {
                            playback.changed.set(false);
                        }
                    }
                    Err(error) if error.status == 409 => {
                        playback.fenced.set(true);
                        playback.changed.set(false);
                    }
                    Err(_) => {}
                }
            },
        )),
        remote,
    );
}

/// Polling also covers devices without push transports and retries failed
/// history reads. Lifecycle events shorten handoff/reconnect latency.
pub(super) fn watch_progress(
    playback: PlaybackProgress,
    file_id: Signal<String>,
    on_loaded: Callback<()>,
    on_remote: Callback<()>,
) {
    let owner = Owner::current().expect("progress watcher belongs to a player");
    let busy = RwSignal::new(false);
    let poll = Callback::new(move |()| {
        if busy.get_untracked() {
            return;
        }
        let id = file_id.get_untracked();
        if id.is_empty() {
            return;
        }
        if playback.server.get_untracked().is_none() {
            owner.with(|| playback.load(id, on_loaded));
            return;
        }
        busy.set(true);
        let revision = playback.revision.get_untracked();
        leptos::task::spawn_local(async move {
            if let Ok(progress) = api::fetch_media_progress(&id).await
                && playback.revision.try_get_untracked() == Some(revision)
                && playback.receive_remote(progress)
            {
                on_remote.run(());
            }
            let _ = busy.try_set(false);
        });
    });
    let interval = Closure::<dyn FnMut()>::new(move || poll.run(()));
    let timer = web_sys::window().and_then(|window| {
        window
            .set_interval_with_callback_and_timeout_and_arguments_0(
                interval.as_ref().unchecked_ref(),
                3000,
            )
            .ok()
    });
    let interval = leptos::__reexports::send_wrapper::SendWrapper::new(interval);
    let mut focus = browser::on_window_capture("focus", move |_| poll.run(()));
    let mut pageshow = browser::on_window_capture("pageshow", move |_| poll.run(()));
    let mut online = browser::on_window_capture("online", move |_| {
        super::progress_outbox::retry_all();
        poll.run(());
    });
    let mut visibility = browser::on_window_capture("visibilitychange", move |_| poll.run(()));
    on_cleanup(move || {
        focus.release();
        pageshow.release();
        online.release();
        visibility.release();
        if let Some(timer) = timer
            && let Some(window) = web_sys::window()
        {
            window.clear_interval_with_handle(timer);
        }
        drop(interval);
    });
}

pub(super) fn stored_volume(key: &str, default: f64) -> f64 {
    browser::local_storage_get(key)
        .and_then(|value| value.parse::<f64>().ok())
        .filter(|value| value.is_finite())
        .map_or(default, |value| value.clamp(0.0, 1.0))
}

pub(super) fn clear_timer(signal: RwSignal<Option<i32>>) {
    if let Some(timer) = signal.get_untracked()
        && let Some(window) = web_sys::window()
    {
        window.clear_timeout_with_handle(timer);
    }
    signal.set(None);
}

pub(super) fn debounce<F>(signal: RwSignal<Option<i32>>, delay: i32, callback: F)
where
    F: Fn() + 'static,
{
    clear_timer(signal);
    schedule_timer(signal, delay, callback);
}

pub(super) fn throttle<F>(signal: RwSignal<Option<i32>>, delay: i32, callback: F)
where
    F: Fn() + 'static,
{
    if signal.get_untracked().is_none() {
        schedule_timer(signal, delay, callback);
    }
}

fn schedule_timer<F>(signal: RwSignal<Option<i32>>, delay: i32, callback: F)
where
    F: Fn() + 'static,
{
    if let Some(window) = web_sys::window() {
        let callback = Closure::once(move || {
            signal.set(None);
            callback();
        })
        .into_js_value();
        if let Ok(id) = window
            .set_timeout_with_callback_and_timeout_and_arguments_0(callback.unchecked_ref(), delay)
        {
            signal.set(Some(id));
        }
    }
}

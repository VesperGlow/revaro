//! Shared progress lifecycle and timer policies for the three media players.

use leptos::prelude::*;
use revaro_core::model::MediaProgress;
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
}

impl PlaybackProgress {
    pub fn new() -> Self {
        Self {
            revision: RwSignal::new(0),
            loaded_file: RwSignal::new(None),
            pending: RwSignal::new(None),
            ready: RwSignal::new(false),
            failed: RwSignal::new(false),
        }
    }

    /// Close the write gate before a source change can emit old media events.
    pub fn reset(self, pending: Option<f64>) {
        self.ready.set(false);
        self.failed.set(false);
        self.loaded_file.set(None);
        self.pending.set(pending.map(PlaybackPosition::Seek));
    }

    /// Restoring a listening session still applies the completion policy.
    pub fn reset_saved(self, position: f64) {
        self.reset(None);
        self.pending.set(Some(PlaybackPosition::Resume(position)));
    }

    /// A user seek wins immediately over any outstanding history request.
    pub fn accept_seek(self) {
        self.pending.set(None);
        self.ready.set(true);
        self.failed.set(false);
    }

    pub fn load(self, id: String, on_loaded: Callback<()>) {
        self.failed.set(false);
        self.loaded_file.set(Some(id.clone()));
        if self.pending.get_untracked().is_some() {
            on_loaded.run(());
            return;
        }
        let revision = self.revision.get_untracked();
        leptos::task::spawn_local_scoped_with_cancellation(async move {
            let progress = api::fetch_media_progress(&id).await;
            if self.revision.try_get_untracked() != Some(revision)
                || self.loaded_file.try_get_untracked().flatten().as_deref() != Some(&id)
                || self.ready.try_get_untracked() != Some(false)
            {
                return;
            }
            let Ok(progress) = progress else {
                // A failed read is not an empty history. Keep writes closed
                // until a successful retry or an intentional user seek.
                self.failed.set(true);
                return;
            };
            self.pending
                .set(Some(PlaybackPosition::Resume(media_element_time(
                    progress.position,
                ))));
            on_loaded.run(());
        });
    }

    /// The caller supplies its metadata readiness.
    /// Zero means there is no resume seek; previews leave their current clock alone.
    pub fn restore(self, duration: f64, local_key: Option<&str>) -> Option<f64> {
        if self.ready.get_untracked() {
            return None;
        }
        let pending = self.pending.get_untracked()?;
        let local = local_key
            .and_then(browser::local_storage_get)
            .and_then(|value| value.parse::<f64>().ok())
            .filter(|value| value.is_finite() && *value > 0.0);
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
    let retry = Callback::new(move |()| {
        owner.with(|| playback.load(file_id.get_untracked(), on_loaded.clone()))
    });
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
    id: &str,
    position: f64,
    duration: f64,
    local_key: Option<&str>,
    destination: ProgressDestination,
) {
    if let Some(key) = local_key {
        browser::local_storage_set(key, &position.floor().to_string());
    }
    let progress = MediaProgress {
        position,
        duration,
        updated_at: None,
    };
    match destination {
        ProgressDestination::Local => {}
        ProgressDestination::Remote => {
            let id = id.to_owned();
            leptos::task::spawn_local(async move {
                let _ = api::save_media_progress(&id, &progress).await;
            });
        }
        ProgressDestination::Keepalive => api::save_media_progress_keepalive(id, &progress),
    }
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

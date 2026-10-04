//! Shared progress lifecycle and timer policies for the three media players.

use leptos::prelude::*;
use revaro_core::model::MediaProgress;
use wasm_bindgen::{JsCast, closure::Closure};

use crate::{
    api, browser,
    logic::media::{authoritative_seek_target, media_element_time, resume_time},
};

#[derive(Clone, Copy)]
pub(super) struct PlaybackProgress {
    pub revision: RwSignal<u64>,
    pub loaded_file: RwSignal<Option<String>>,
    pub pending: RwSignal<Option<f64>>,
    pub ready: RwSignal<bool>,
}

impl PlaybackProgress {
    pub fn new() -> Self {
        Self {
            revision: RwSignal::new(0),
            loaded_file: RwSignal::new(None),
            pending: RwSignal::new(None),
            ready: RwSignal::new(false),
        }
    }

    /// Close the write gate before a source change can emit old media events.
    pub fn reset(self, pending: Option<f64>) {
        self.ready.set(false);
        self.loaded_file.set(None);
        self.pending.set(pending);
    }

    pub fn load(self, id: String, on_loaded: Callback<()>) {
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
            {
                return;
            }
            self.pending.set(Some(
                progress.map_or(0.0, |p| media_element_time(p.position)),
            ));
            on_loaded.run(());
        });
    }

    /// The caller supplies its metadata readiness and user-seek policy.
    /// Zero means there is no resume seek; previews leave their current clock alone.
    pub fn restore(
        self,
        duration: f64,
        current: f64,
        user_seeked: bool,
        local_key: Option<&str>,
    ) -> Option<f64> {
        if self.ready.get_untracked() {
            return None;
        }
        let server = self.pending.get_untracked()?;
        let saved = if server > 0.0 {
            server
        } else {
            local_key
                .and_then(browser::local_storage_get)
                .and_then(|value| value.parse::<f64>().ok())
                .filter(|value| value.is_finite() && *value > 0.0)
                .unwrap_or(0.0)
        };
        let target = authoritative_seek_target(current, saved, user_seeked);
        self.pending.set(None);
        self.ready.set(true);
        Some(resume_time(target, duration))
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
        let callback = Closure::once_into_js(move || {
            signal.set(None);
            callback();
        });
        if let Ok(id) = window
            .set_timeout_with_callback_and_timeout_and_arguments_0(callback.unchecked_ref(), delay)
        {
            signal.set(Some(id));
        }
    }
}

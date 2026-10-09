//! Bounded local recovery copies; only an explicit action replaces editor text.
use super::playback::{clear_timer, debounce};
use crate::browser;
use leptos::prelude::*;
use serde::{Deserialize, Serialize};

const PREFIX: &str = "revaro-editor-draft:";
const FLUSH_EVENT: &str = "revaro-save-editor-drafts";
#[derive(Clone, Serialize, Deserialize)]
struct Draft {
    name: String,
    content: String,
    saved_at: f64,
}

pub(super) fn remove(key: &str) {
    if let Some(storage) = web_sys::window().and_then(|w| w.local_storage().ok().flatten()) {
        let _ = storage.remove_item(key);
    }
}
pub(crate) fn clear_all() {
    if let Some(storage) = web_sys::window().and_then(|w| w.local_storage().ok().flatten()) {
        let keys = (0..storage.length().unwrap_or(0))
            .filter_map(|i| storage.key(i).ok().flatten())
            .filter(|key| key.starts_with(PREFIX))
            .collect::<Vec<_>>();
        for key in keys {
            let _ = storage.remove_item(&key);
        }
    }
}

pub(crate) fn flush_all() {
    if let Some(window) = web_sys::window()
        && let Ok(event) = web_sys::Event::new(FLUSH_EVENT)
    {
        let _ = window.dispatch_event(&event);
    }
}

#[component]
pub(super) fn DraftRecovery(
    draft_key: Signal<String>,
    name: RwSignal<String>,
    content: RwSignal<String>,
    dirty: RwSignal<bool>,
    busy: RwSignal<bool>,
    readonly: RwSignal<bool>,
    draft_pending: RwSignal<bool>,
) -> impl IntoView {
    let pending = RwSignal::new(None::<Draft>);
    let loaded = RwSignal::new(String::new());
    let failed = RwSignal::new(false);
    let timer = RwSignal::new(None::<i32>);
    Effect::new(move |_| draft_pending.set(pending.get().is_some()));
    Effect::new(move |_| {
        let key = draft_key.get();
        if busy.get() || readonly.get() || loaded.get_untracked() == key {
            return;
        }
        loaded.set(key.clone());
        pending.set(None);
        let draft = browser::local_storage_get(&key)
            .and_then(|raw| serde_json::from_str::<Draft>(&raw).ok())
            .filter(|d| {
                d.content.len() <= revaro_core::limits::MAX_DOCUMENT_BYTES && d.name.len() <= 1024
            });
        if let Some(draft) = draft {
            if draft.content != content.get_untracked() || draft.name != name.get_untracked() {
                pending.set(Some(draft));
            } else {
                remove(&key);
            }
        }
    });
    let persist = Callback::new(move |()| {
        if (busy.get_untracked() && !dirty.get_untracked())
            || readonly.get_untracked()
            || pending.get_untracked().is_some()
        {
            return;
        }
        let key = draft_key.get_untracked();
        if loaded.get_untracked() != key {
            return;
        }
        if !dirty.get_untracked() {
            remove(&key);
            return;
        }
        let Some(storage) = web_sys::window().and_then(|w| w.local_storage().ok().flatten()) else {
            failed.set(true);
            return;
        };
        let draft = Draft {
            name: name.get_untracked(),
            content: content.get_untracked(),
            saved_at: js_sys::Date::now(),
        };
        if draft.content.len() > revaro_core::limits::MAX_DOCUMENT_BYTES {
            failed.set(true);
            return;
        }
        // Keep at most four documents, evicting the oldest other draft first.
        let mut other = (0..storage.length().unwrap_or(0))
            .filter_map(|i| storage.key(i).ok().flatten())
            .filter(|k| k.starts_with(PREFIX) && *k != key)
            .map(|k| {
                let time = storage
                    .get_item(&k)
                    .ok()
                    .flatten()
                    .and_then(|s| serde_json::from_str::<Draft>(&s).ok())
                    .map_or(0.0, |d| d.saved_at);
                (k, time)
            })
            .collect::<Vec<_>>();
        other.sort_by(|a, b| a.1.total_cmp(&b.1));
        let evict = other.len().saturating_sub(3);
        for (key, _) in other.into_iter().take(evict) {
            let _ = storage.remove_item(&key);
        }
        failed.set(
            serde_json::to_string(&draft)
                .ok()
                .is_none_or(|value| storage.set_item(&key, &value).is_err()),
        );
    });
    Effect::new(move |_| {
        let _ = (
            name.get(),
            content.get(),
            dirty.get(),
            busy.get(),
            loaded.get(),
            pending.get(),
        );
        debounce(timer, 500, move || persist.run(()));
    });
    let mut pagehide = browser::on_pagehide(move |_| persist.run(()));
    let mut unload = browser::on_window_capture("beforeunload", move |_| persist.run(()));
    let mut session_end = browser::on_window_capture(FLUSH_EVENT, move |_| persist.run(()));
    on_cleanup(move || {
        clear_timer(timer);
        pagehide.release();
        unload.release();
        session_end.release();
    });
    view! {
        <Show when=move ||pending.get().is_some() fallback=|| ()>
            <div class="editor-draft-recovery" role="status">
                "发现未保存的本机草稿。"
                <button type="button" disabled=move ||busy.get() on:click=move |_|{
                    if let Some(draft)=pending.get_untracked(){name.set(draft.name);content.set(draft.content);dirty.set(true);pending.set(None);}
                }>"恢复草稿"</button>
                <button type="button" disabled=move ||busy.get() on:click=move |_|{remove(&draft_key.get_untracked());pending.set(None);}>"丢弃草稿"</button>
            </div>
        </Show>
        <Show when=move ||failed.get() fallback=|| ()><p role="alert">"本机草稿未能保存，请及时保存文档。"</p></Show>
    }
}

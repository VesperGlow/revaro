//! Small persisted view hints and bounded, document-local listing snapshots.
//! Persist state only; authenticated data is always revalidated on navigation.
use crate::browser;
use leptos::prelude::*;
use serde::{Deserialize, Serialize};

#[derive(Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct SavedView {
    pub query: String,
    pub favorite: bool,
    pub collection: String,
    pub stack: String,
    pub folder: String,
    pub sort: String,
    pub scroll: f64,
}

#[derive(Clone, Copy)]
pub struct ViewMemory {
    prefix: StoredValue<String>,
}
impl ViewMemory {
    pub fn new(username: &str) -> Self {
        Self {
            prefix: StoredValue::new(format!("revaro-view-v1:{username}:")),
        }
    }
    pub fn load(self, module: &str) -> SavedView {
        let mut saved = browser::local_storage_get(&format!("{}{module}", self.prefix.get_value()))
            .and_then(|raw| serde_json::from_str::<SavedView>(&raw).ok())
            .unwrap_or_default();
        if !saved.scroll.is_finite() || saved.scroll < 0.0 {
            saved.scroll = 0.0;
        }
        if ![
            "",
            "created",
            "name",
            "name_desc",
            "size",
            "size_desc",
            "updated",
            "updated_desc",
        ]
        .contains(&saved.sort.as_str())
        {
            saved.sort.clear();
        }
        if saved.query.len() > 512 {
            saved.query.clear();
        }
        saved
    }
    pub fn save(self, module: &str, saved: &SavedView) {
        if let Ok(raw) = serde_json::to_string(saved) {
            browser::local_storage_set(&format!("{}{module}", self.prefix.get_value()), &raw);
        }
    }
}

/// Every browser cache uses both a count and a byte bound, including stack pages.
pub struct Snapshots<T> {
    entries: Vec<(String, T, usize)>,
    bytes: usize,
    limit: usize,
}
impl<T: Clone + Serialize> Snapshots<T> {
    pub fn new() -> Self {
        Self {
            entries: Vec::new(),
            bytes: 0,
            limit: if browser::device_memory_gib().is_some_and(|m| m <= 4.0) {
                8 << 20
            } else {
                32 << 20
            },
        }
    }
    pub fn get(&mut self, key: &str) -> Option<T> {
        let index = self.entries.iter().position(|(k, _, _)| k == key)?;
        let entry = self.entries.remove(index);
        let value = entry.1.clone();
        self.entries.push(entry);
        Some(value)
    }
    pub fn put(&mut self, key: String, value: T) {
        let Ok(bytes) = serde_json::to_vec(&value).map(|v| v.len()) else {
            return;
        };
        if bytes > self.limit {
            return;
        }
        if let Some(index) = self.entries.iter().position(|(k, _, _)| *k == key) {
            self.bytes -= self.entries.remove(index).2;
        }
        while self.entries.len() >= 24 || self.bytes + bytes > self.limit {
            self.bytes -= self.entries.remove(0).2;
        }
        self.bytes += bytes;
        self.entries.push((key, value, bytes));
    }
}

/// Retry after each rendered page until the saved position is reachable.
/// One animation frame is owned at a time and released on unmount.
pub fn install_scroll_restore(
    pending: RwSignal<Option<f64>>,
    ready: Signal<bool>,
    revision: Signal<usize>,
    has_more: Signal<bool>,
    load_more: Callback<()>,
) {
    use wasm_bindgen::{JsCast, closure::Closure};
    let callback = Closure::<dyn FnMut(f64)>::new(move |_| {
        let Some(target) = pending.try_get_untracked().flatten() else {
            return;
        };
        if !ready.get_untracked() {
            return;
        }
        let Some(window) = web_sys::window() else {
            return;
        };
        let height = window
            .document()
            .and_then(|d| d.document_element())
            .map(|e| e.scroll_height() as f64)
            .unwrap_or_default();
        let viewport = window
            .inner_height()
            .ok()
            .and_then(|v| v.as_f64())
            .unwrap_or_default();
        if height - viewport + 2.0 < target && has_more.get_untracked() {
            load_more.run(());
            return;
        }
        window.scroll_to_with_x_and_y(0.0, target.min((height - viewport).max(0.0)));
        pending.set(None);
    });
    let callback = StoredValue::new_local(callback);
    let frame = StoredValue::new(None::<i32>);
    Effect::new(move |_| {
        let _ = (pending.get(), ready.get(), revision.get());
        if pending.get_untracked().is_none() || !ready.get_untracked() {
            return;
        }
        if let Some(window) = web_sys::window() {
            if let Some(id) = frame.get_value() {
                let _ = window.cancel_animation_frame(id);
            }
            let id = callback.with_value(|cb| {
                window
                    .request_animation_frame(cb.as_ref().unchecked_ref())
                    .ok()
            });
            frame.set_value(id);
        }
    });
    on_cleanup(move || {
        if let Some(window) = web_sys::window()
            && let Some(id) = frame.get_value()
        {
            let _ = window.cancel_animation_frame(id);
        }
    });
}

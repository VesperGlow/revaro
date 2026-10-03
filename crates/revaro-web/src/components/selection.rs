//! Global selection mode, shared by every content listing.
use std::collections::HashSet;

use leptos::prelude::*;
use revaro_core::model::File;
use wasm_bindgen::JsCast;

use super::selection_toolbar::SelectionActions;

const INTERACTIVE_ELEMENTS: &str = "button,a,input,textarea,select,label,summary,details,\
    [role=button],[role=checkbox],[role=menu],[role=menuitem],[role=toolbar],\
    [role=dialog],[contenteditable],.action-menu-panel,.directory-popover";

#[derive(Clone, Copy)]
pub struct SelectionManagement {
    pub busy: Signal<bool>,
    pub on_favorite: Callback<bool>,
    pub on_collection: Callback<crate::logic::library::LibraryPage>,
    pub on_remove: Callback<()>,
    pub can_remove: Signal<bool>,
}

#[derive(Clone, Copy)]
pub struct SelectionMode {
    pub enabled: RwSignal<bool>,
    pub ids: RwSignal<HashSet<String>>,
    pub library_items: RwSignal<Vec<File>>,
    pub actions: RwSignal<Option<SelectionActions>>,
    pub management: RwSignal<Option<SelectionManagement>>,
}

impl SelectionMode {
    pub fn new() -> Self {
        Self {
            enabled: RwSignal::new(false),
            ids: RwSignal::new(HashSet::new()),
            library_items: RwSignal::new(Vec::new()),
            actions: RwSignal::new(None),
            management: RwSignal::new(None),
        }
    }

    pub fn toggle(self, id: &str) {
        if !self.enabled.get_untracked() {
            return;
        }
        self.ids.update(|ids| {
            if !ids.insert(id.to_owned()) {
                ids.remove(id);
            }
        });
    }

    pub fn clear(self) {
        self.ids.set(HashSet::new());
    }

    pub fn exit(self) {
        self.enabled.set(false);
        self.clear();
    }

    pub fn toggle_mode(self) {
        if self.enabled.get_untracked() {
            self.exit();
        } else {
            self.clear();
            self.enabled.set(true);
        }
    }

    /// Only clicks on the listing's background dismiss selection mode.
    pub fn exit_from_blank(self, event: web_sys::MouseEvent) {
        if !self.enabled.get_untracked() {
            return;
        }
        let Some(target) = event
            .target()
            .and_then(|target| target.dyn_into::<web_sys::Element>().ok())
        else {
            return;
        };
        if matches!(target.closest(INTERACTIVE_ELEMENTS), Ok(None))
            && matches!(
                target.closest(".file-card,.library-card,.home-card"),
                Ok(None)
            )
        {
            self.exit();
        }
    }

    /// Card padding and music row numbers toggle selection, without intercepting controls.
    pub fn toggle_from_card_background(self, event: web_sys::MouseEvent, id: &str) {
        if !self.enabled.get_untracked() {
            return;
        }
        let Some(target) = event
            .target()
            .and_then(|target| target.dyn_into::<web_sys::Element>().ok())
        else {
            return;
        };
        if matches!(target.closest(INTERACTIVE_ELEMENTS), Ok(None)) {
            self.toggle(id);
        }
    }

    pub fn set_library_items(self, files: Vec<File>) {
        // A refresh prunes unavailable entries; appending a page keeps selection.
        let next = self
            .ids
            .get_untracked()
            .into_iter()
            .filter(|id| files.iter().any(|file| &file.id == id))
            .collect::<HashSet<_>>();
        if next != self.ids.get_untracked() {
            self.ids.set(next);
        }
        self.library_items.set(files);
    }

    pub fn selected_files(self) -> Vec<File> {
        let ids = self.ids.get_untracked();
        self.actions
            .get_untracked()
            .map(|actions| actions.items.get_untracked())
            .unwrap_or_default()
            .into_iter()
            .filter(|file| ids.contains(&file.id))
            .collect()
    }
}

#[component]
pub fn SelectionCheckbox(id: String, name: String, selection: SelectionMode) -> impl IntoView {
    let selected_id = id.clone();
    let selected = Memo::new(move |_| selection.ids.with(|ids| ids.contains(&selected_id)));
    let toggle = id;
    view! {
        <Show when=move || selection.enabled.get() fallback=|| ()>
            <label class="selection-checkbox"
                title=move || if selected.get() { "取消选择" } else { "选择项目" }
                on:click=move |event: web_sys::MouseEvent| event.stop_propagation()
                on:keydown=move |event: web_sys::KeyboardEvent| {
                    if matches!(event.key().as_str(), " " | "Enter") {
                        event.stop_propagation();
                    }
                }>
                // The card supplies the visual state; retain a native keyboard/screen reader control.
                <input type="checkbox" aria-label=format!("选择 {name}")
                    prop:checked=move || selected.get()
                    on:change={let toggle=toggle.clone();move |_| selection.toggle(&toggle)} />
            </label>
        </Show>
    }
}

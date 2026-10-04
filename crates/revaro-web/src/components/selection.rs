//! Global selection mode, shared by every content listing.
use std::collections::HashSet;
use std::{cell::RefCell, rc::Rc};

use leptos::prelude::*;
use revaro_core::model::File;
use wasm_bindgen::JsCast;
use wasm_bindgen::closure::Closure;

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
        self.toggle_group(&[id.to_owned()]);
    }

    pub fn toggle_group(self, group: &[String]) {
        if !self.enabled.get_untracked() {
            return;
        }
        self.ids.update(|ids| {
            let remove = group.iter().all(|id| ids.contains(id));
            for id in group {
                if remove {
                    ids.remove(id);
                } else {
                    ids.insert(id.clone());
                }
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
        self.toggle_group_from_card_background(event, &[id.to_owned()]);
    }

    pub fn toggle_group_from_card_background(self, event: web_sys::MouseEvent, ids: &[String]) {
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
            self.toggle_group(ids);
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
pub fn SelectionCheckbox(
    id: String,
    name: String,
    selection: SelectionMode,
    #[prop(optional)] group: Vec<String>,
) -> impl IntoView {
    let group = if group.is_empty() { vec![id] } else { group };
    let selected_group = group.clone();
    let selected = Memo::new(move |_| {
        selection
            .ids
            .with(|ids| selected_group.iter().all(|id| ids.contains(id)))
    });
    let toggle = group;
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
                    on:change={let toggle=toggle.clone();move |_| selection.toggle_group(&toggle)} />
            </label>
        </Show>
    }
}

#[derive(Default)]
struct PressState {
    pointer: Option<i32>,
    origin: (i32, i32),
    timer: Option<(i32, Closure<dyn FnMut()>)>,
    fired: bool,
}

impl PressState {
    fn cancel(&mut self) {
        if let Some((timer, _)) = self.timer.take()
            && let Some(window) = web_sys::window()
        {
            window.clear_timeout_with_handle(timer);
        }
        self.pointer = None;
    }
}

/// One delegated gesture for every listing. Pointer events cover mouse, touch and pen.
pub fn install_long_press(selection: SelectionMode) {
    let Some(window) = web_sys::window() else {
        return;
    };
    let state = Rc::new(RefCell::new(PressState::default()));
    let mut listeners = Vec::<(&'static str, Closure<dyn FnMut(web_sys::Event)>)>::new();
    for name in [
        "pointerdown",
        "pointermove",
        "pointerup",
        "pointercancel",
        "scroll",
        "blur",
        "click",
        "contextmenu",
        "dragstart",
    ] {
        let state = state.clone();
        let callback = Closure::<dyn FnMut(web_sys::Event)>::new(move |event: web_sys::Event| {
            if name == "click" {
                let mut press = state.borrow_mut();
                if press.fired {
                    press.fired = false;
                    if event
                        .dyn_ref::<web_sys::MouseEvent>()
                        .is_some_and(|e| e.detail() > 0)
                    {
                        event.prevent_default();
                        event.stop_immediate_propagation();
                    }
                }
                return;
            }
            if name == "contextmenu" {
                let press = state.borrow();
                if press.pointer.is_some() || press.fired {
                    event.prevent_default();
                }
                return;
            }
            // Capture also sees element blur when the pressed card receives focus.
            // Only leaving the browser window cancels the gesture.
            if name == "blur"
                && !event
                    .target()
                    .is_some_and(|target| target.is_instance_of::<web_sys::Window>())
            {
                return;
            }
            if matches!(name, "scroll" | "blur" | "dragstart") {
                state.borrow_mut().cancel();
                return;
            }
            let Some(pointer) = event.dyn_ref::<web_sys::PointerEvent>() else {
                return;
            };
            if name != "pointerdown" {
                let mut press = state.borrow_mut();
                if press.pointer != Some(pointer.pointer_id()) {
                    return;
                }
                if name != "pointermove"
                    || (pointer.client_x() - press.origin.0).abs() > 10
                    || (pointer.client_y() - press.origin.1).abs() > 10
                {
                    press.cancel();
                }
                return;
            }
            state.borrow_mut().cancel();
            state.borrow_mut().fired = false;
            if !pointer.is_primary()
                || pointer.button() != 0
                || !selection
                    .actions
                    .get_untracked()
                    .is_some_and(|a| a.visible.get_untracked())
            {
                return;
            }
            let Some(target) = pointer
                .target()
                .and_then(|t| t.dyn_into::<web_sys::Element>().ok())
            else {
                return;
            };
            let Ok(Some(card)) = target.closest("[data-selection-ids]") else {
                return;
            };
            if let Ok(Some(control)) = target.closest(INTERACTIVE_ELEMENTS)
                && !matches!(
                    control.matches(".file-card,.library-card-open,.home-item"),
                    Ok(true)
                )
            {
                return;
            }
            let Some(ids) = card
                .get_attribute("data-selection-ids")
                .and_then(|raw| serde_json::from_str::<Vec<String>>(&raw).ok())
                .filter(|ids| !ids.is_empty())
            else {
                return;
            };
            let timer_state = state.clone();
            let callback = Closure::<dyn FnMut()>::new(move || {
                if !card.is_connected()
                    || card.get_client_rects().length() == 0
                    || !selection
                        .actions
                        .get_untracked()
                        .is_some_and(|a| a.visible.get_untracked())
                {
                    return;
                }
                timer_state.borrow_mut().fired = true;
                selection.enabled.set(true);
                selection
                    .ids
                    .update(|selected| selected.extend(ids.iter().cloned()));
            });
            if let Some(window) = web_sys::window()
                && let Ok(timer) = window.set_timeout_with_callback_and_timeout_and_arguments_0(
                    callback.as_ref().unchecked_ref(),
                    500,
                )
            {
                let mut press = state.borrow_mut();
                press.pointer = Some(pointer.pointer_id());
                press.origin = (pointer.client_x(), pointer.client_y());
                press.timer = Some((timer, callback));
            }
        });
        let _ = window.add_event_listener_with_callback_and_bool(
            name,
            callback.as_ref().unchecked_ref(),
            true,
        );
        listeners.push((name, callback));
    }
    let cleanup = leptos::__reexports::send_wrapper::SendWrapper::new((window, listeners, state));
    on_cleanup(move || {
        let (window, listeners, state) = cleanup.take();
        state.borrow_mut().cancel();
        for (name, callback) in listeners {
            let _ = window.remove_event_listener_with_callback_and_bool(
                name,
                callback.as_ref().unchecked_ref(),
                true,
            );
        }
    });
}

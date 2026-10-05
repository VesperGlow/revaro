//! Inline Stack management and a delegated hold-to-reorder gesture.
use std::{cell::RefCell, collections::HashSet, rc::Rc};

use wasm_bindgen::closure::Closure;
use web_sys::{Element, HtmlElement, PointerEvent, TouchEvent};

use super::stacks::StackController;
use super::*;

#[derive(Clone, Copy)]
pub(super) struct StackGesture {
    pub dragging: RwSignal<Option<String>>,
    pub target: RwSignal<Option<String>>,
}

#[derive(Clone, Copy, PartialEq)]
enum Contact {
    Pointer(i32),
    Touch(i32),
}

#[derive(Default)]
struct Hold {
    contact: Option<Contact>,
    card: Option<Element>,
    source: String,
    origin: (i32, i32),
    anchor: (f64, f64),
    armed: bool,
    fired: bool,
    timer: Option<(i32, Closure<dyn FnMut()>)>,
    preview: Option<HtmlElement>,
}

impl Hold {
    fn cancel(&mut self, gesture: StackGesture) {
        if let Some((id, _)) = self.timer.take()
            && let Some(window) = web_sys::window()
        {
            window.clear_timeout_with_handle(id);
        }
        if let Some(preview) = self.preview.take() {
            preview.remove();
        }
        self.contact = None;
        self.card = None;
        self.armed = false;
        gesture.dragging.set(None);
        gesture.target.set(None);
    }
}

impl StackGesture {
    pub fn install(controller: StackController, selection: SelectionMode) -> Self {
        let gesture = Self {
            dragging: RwSignal::new(None),
            target: RwSignal::new(None),
        };
        let Some(window) = web_sys::window() else {
            return gesture;
        };
        let state = Rc::new(RefCell::new(Hold::default()));
        let scope_state = state.clone();
        Effect::new(move |_| {
            let _ = controller.selected.get();
            let _ = controller.dialog.get().is_some();
            scope_state.borrow_mut().cancel(gesture);
        });
        let mode_state = state.clone();
        Effect::new(move |_| {
            if !selection.enabled.get() {
                mode_state.borrow_mut().cancel(gesture);
            }
        });
        let mut listeners = Vec::new();
        for name in [
            "pointerdown",
            "pointermove",
            "pointerup",
            "pointercancel",
            "touchstart",
            "touchmove",
            "touchend",
            "touchcancel",
            "click",
            "contextmenu",
            "scroll",
            "blur",
        ] {
            let state = state.clone();
            let callback =
                Closure::<dyn FnMut(web_sys::Event)>::new(move |event: web_sys::Event| {
                    if name == "click" {
                        let mut hold = state.borrow_mut();
                        if hold.fired
                            && event
                                .dyn_ref::<web_sys::MouseEvent>()
                                .is_some_and(|e| e.detail() > 0)
                        {
                            hold.fired = false;
                            event.prevent_default();
                            event.stop_immediate_propagation();
                        }
                        return;
                    }
                    if name == "contextmenu" {
                        if state.borrow().contact.is_some() {
                            event.prevent_default();
                        }
                        return;
                    }
                    if name == "blur" {
                        if event
                            .target()
                            .is_some_and(|target| target.is_instance_of::<web_sys::Window>())
                        {
                            state.borrow_mut().cancel(gesture);
                        }
                        return;
                    }
                    if name == "scroll" {
                        if !state.borrow().armed {
                            state.borrow_mut().cancel(gesture);
                        }
                        return;
                    }
                    let start = matches!(name, "pointerdown" | "touchstart");
                    let end = matches!(name, "pointerup" | "touchend");
                    let cancel = matches!(name, "pointercancel" | "touchcancel");
                    // Touch events let us preserve scrolling until the hold fires, then prevent
                    // the browser's pan gesture without disabling normal card-area scrolling.
                    let (contact, point) = if let Some(touch) = event.dyn_ref::<TouchEvent>() {
                        if start && touch.touches().length() != 1 {
                            state.borrow_mut().cancel(gesture);
                            return;
                        }
                        let touches = touch.changed_touches();
                        let active = state.borrow().contact;
                        let Some(touch) = (0..touches.length())
                            .filter_map(|i| touches.item(i))
                            .find(|t| start || active == Some(Contact::Touch(t.identifier())))
                        else {
                            return;
                        };
                        (
                            Contact::Touch(touch.identifier()),
                            (touch.client_x(), touch.client_y()),
                        )
                    } else if let Some(pointer) = event.dyn_ref::<PointerEvent>() {
                        if pointer.pointer_type() == "touch"
                            || (start && (!pointer.is_primary() || pointer.button() != 0))
                        {
                            return;
                        }
                        (
                            Contact::Pointer(pointer.pointer_id()),
                            (pointer.client_x(), pointer.client_y()),
                        )
                    } else {
                        return;
                    };
                    if start {
                        state.borrow_mut().cancel(gesture);
                        state.borrow_mut().fired = false;
                        if controller.busy.get_untracked()
                            || controller.dialog.get_untracked().is_some()
                            || controller.selected.get_untracked().is_empty()
                            || !selection
                                .actions
                                .get_untracked()
                                .is_some_and(|a| a.visible.get_untracked())
                        {
                            return;
                        }
                        let Some(target) =
                            event.target().and_then(|t| t.dyn_into::<Element>().ok())
                        else {
                            return;
                        };
                        let Ok(Some(card)) = target.closest("[data-stack-book-id]") else {
                            return;
                        };
                        if let Ok(Some(control)) = target.closest("button,input,label,a,summary")
                            && !matches!(control.matches(".library-card-open"), Ok(true))
                        {
                            return;
                        }
                        let Some(source) = card.get_attribute("data-stack-book-id") else {
                            return;
                        };
                        let rect = card.get_bounding_client_rect();
                        let anchor = (point.0 as f64 - rect.left(), point.1 as f64 - rect.top());
                        let timer_state = state.clone();
                        let timer_source = source.clone();
                        let timer_card = card.clone();
                        let callback = Closure::<dyn FnMut()>::new(move || {
                            if !timer_card.is_connected()
                                || timer_card.get_client_rects().length() == 0
                                || controller.busy.get_untracked()
                            {
                                return;
                            }
                            let mut hold = timer_state.borrow_mut();
                            hold.armed = true;
                            hold.fired = true;
                            drop(hold);
                            selection.enabled.set(true);
                            selection.ids.update(|ids| {
                                ids.insert(timer_source.clone());
                            });
                        });
                        if let Some(window) = web_sys::window()
                            && let Ok(id) = window
                                .set_timeout_with_callback_and_timeout_and_arguments_0(
                                    callback.as_ref().unchecked_ref(),
                                    500,
                                )
                        {
                            let mut hold = state.borrow_mut();
                            hold.contact = Some(contact);
                            hold.card = Some(card);
                            hold.source = source;
                            hold.origin = point;
                            hold.anchor = anchor;
                            hold.timer = Some((id, callback));
                        }
                        return;
                    }
                    let mut hold = state.borrow_mut();
                    if hold.contact != Some(contact) {
                        return;
                    }
                    if end || cancel {
                        if hold.armed {
                            event.prevent_default();
                        }
                        let source = gesture.dragging.get_untracked();
                        let target = gesture.target.get_untracked();
                        hold.cancel(gesture);
                        drop(hold);
                        if !cancel && let (Some(source), Some(target)) = (source, target) {
                            controller.reorder.run((source, target));
                        }
                        return;
                    }
                    let dx = point.0 - hold.origin.0;
                    let dy = point.1 - hold.origin.1;
                    if !hold.armed {
                        if dx.abs() > 10 || dy.abs() > 10 {
                            hold.cancel(gesture);
                        }
                        return;
                    }
                    event.prevent_default();
                    if dx.abs() < 8 && dy.abs() < 8 && hold.preview.is_none() {
                        return;
                    }
                    let Some(document) = web_sys::window().and_then(|w| w.document()) else {
                        return;
                    };
                    if hold.preview.is_none()
                        && let Some(card) = hold.card.as_ref()
                        && let Ok(clone) = card.clone_node_with_deep(true)
                        && let Ok(preview) = clone.dyn_into::<HtmlElement>()
                    {
                        let rect = card.get_bounding_client_rect();
                        preview.set_class_name("library-card stack-drag-preview");
                        preview.set_attribute("aria-hidden", "true").ok();
                        for attr in [
                            "data-file-id",
                            "data-stack-book-id",
                            "data-selection-ids",
                            "draggable",
                        ] {
                            preview.remove_attribute(attr).ok();
                        }
                        let style = preview.style();
                        style
                            .set_property("width", &format!("{}px", rect.width()))
                            .ok();
                        style
                            .set_property(
                                "left",
                                &format!("{}px", hold.origin.0 as f64 - hold.anchor.0),
                            )
                            .ok();
                        style
                            .set_property(
                                "top",
                                &format!("{}px", hold.origin.1 as f64 - hold.anchor.1),
                            )
                            .ok();
                        if let Some(body) = document.body() {
                            body.append_child(&preview).ok();
                        }
                        hold.preview = Some(preview);
                        gesture.dragging.set(Some(hold.source.clone()));
                    }
                    if let Some(preview) = hold.preview.as_ref() {
                        preview
                            .style()
                            .set_property(
                                "transform",
                                &format!("translate({dx}px,{dy}px) rotate(2deg)"),
                            )
                            .ok();
                    }
                    let target = document
                        .element_from_point(point.0 as f32, point.1 as f32)
                        .and_then(|el| el.closest("[data-stack-book-id]").ok().flatten())
                        .and_then(|card| card.get_attribute("data-stack-book-id"))
                        .filter(|id| id != &hold.source);
                    gesture.target.set(target);
                    if let Some(window) = web_sys::window()
                        && let Some(height) = window.inner_height().ok().and_then(|h| h.as_f64())
                    {
                        let scroll = if point.1 < 72 {
                            -16.0
                        } else if point.1 as f64 > height - 72.0 {
                            16.0
                        } else {
                            0.0
                        };
                        if scroll != 0.0 {
                            window.scroll_by_with_x_and_y(0.0, scroll);
                        }
                    }
                });
            let options = web_sys::AddEventListenerOptions::new();
            options.set_capture(true);
            options.set_passive(false);
            window
                .add_event_listener_with_callback_and_add_event_listener_options(
                    name,
                    callback.as_ref().unchecked_ref(),
                    &options,
                )
                .ok();
            listeners.push((name, callback));
        }
        let cleanup =
            leptos::__reexports::send_wrapper::SendWrapper::new((window, listeners, state));
        on_cleanup(move || {
            let (window, listeners, state) = cleanup.take();
            state.borrow_mut().cancel(gesture);
            for (name, callback) in listeners {
                window
                    .remove_event_listener_with_callback_and_bool(
                        name,
                        callback.as_ref().unchecked_ref(),
                        true,
                    )
                    .ok();
            }
        });
        gesture
    }
}

#[component]
pub(super) fn StackManagementBar(
    controller: StackController,
    selection: SelectionMode,
) -> impl IntoView {
    view! {
        <Show when=move ||selection.enabled.get() fallback=|| ()>
            <div class="stack-management-bar" role="toolbar" aria-label="管理堆叠书籍" on:click=move |event|event.stop_propagation()>
                <button type="button" on:click=move |_|selection.exit()>"完成"</button>
                <span>{move ||format!("已选 {} 本",selection.ids.get().len())}</span>
                <small id="stack-order-hint">"长按拖动排序 · Alt + 方向键"</small>
                <button type="button" disabled=move ||controller.busy.get() on:click=move |_| {
                    let ids = selection.library_items.get_untracked().into_iter().map(|f|f.id).collect::<HashSet<_>>();
                    if selection.ids.get_untracked() == ids { selection.clear(); } else { selection.ids.set(ids); }
                }>{move ||if !selection.ids.get().is_empty() && selection.ids.get().len()==selection.library_items.get().len(){"取消全选"}else{"全选"}}</button>
                <button type="button" class="stack-remove-selected" disabled=move ||controller.busy.get() ||selection.ids.get().is_empty()
                    on:click=move |_|{if let Some(actions)=selection.stacks.get_untracked(){actions.on_remove.run(());}}>"移出堆叠"</button>
            </div>
            <span class="media-sr-only" role="status" aria-live="polite">{move ||controller.order_notice.get()}</span>
        </Show>
    }
}

//! Small browser interop helpers used by the shell.
//!
//! Nothing here is application logic; it is the handful of `web-sys` calls the
//! shell needs and that do not belong inline in a view: `localStorage` access and document-level event listeners.
//!
//! Only compiled for wasm. The pure replacements for the old TypeScript helpers
//! live in `crate::logic`.

use leptos::ev;
use leptos::prelude::*;
use wasm_bindgen::JsCast;
use wasm_bindgen::closure::Closure;

pub fn device_memory_gib() -> Option<f64> {
    let navigator = js_sys::Reflect::get(&js_sys::global(), &"navigator".into()).ok()?;
    js_sys::Reflect::get(&navigator, &"deviceMemory".into())
        .ok()?
        .as_f64()
        .filter(|memory| memory.is_finite() && *memory > 0.0)
}

/// Release both the event callback and native file buffer on completion or
/// cancellation. A forgotten load handler can keep its FileReader alive forever.
pub async fn read_file_data_url(file: &web_sys::File) -> Result<String, wasm_bindgen::JsValue> {
    struct Reading {
        reader: web_sys::FileReader,
        _completed: Closure<dyn FnMut(web_sys::Event)>,
    }
    impl Drop for Reading {
        fn drop(&mut self) {
            self.reader.set_onloadend(None);
            self.reader.abort();
        }
    }

    let reader = web_sys::FileReader::new()?;
    let (send, receive) = futures_channel::oneshot::channel();
    let completed = Closure::once(move |_: web_sys::Event| {
        let _ = send.send(());
    });
    reader.set_onloadend(Some(completed.as_ref().unchecked_ref()));
    let reading = Reading {
        reader,
        _completed: completed,
    };
    reading.reader.read_as_data_url(file.unchecked_ref())?;
    receive
        .await
        .map_err(|_| wasm_bindgen::JsValue::from_str("file read cancelled"))?;
    reading
        .reader
        .result()?
        .as_string()
        .ok_or_else(|| wasm_bindgen::JsValue::from_str("file read failed"))
}

/// Read a `localStorage` value, treating any storage failure as "absent".
///
/// Private-mode browsers throw on access; the Vue helpers wrapped every call in
/// `try/catch` for the same reason, and a missing preference must never break
/// startup.
#[must_use]
pub fn local_storage_get(key: &str) -> Option<String> {
    web_sys::window()?
        .local_storage()
        .ok()
        .flatten()?
        .get_item(key)
        .ok()
        .flatten()
}

/// Write a `localStorage` value, ignoring a throwing storage.
pub fn local_storage_set(key: &str, value: &str) {
    if let Some(storage) =
        web_sys::window().and_then(|window| window.local_storage().ok().flatten())
    {
        let _ = storage.set_item(key, value);
    }
}

/// Listen for a key press anywhere in the window.
///
/// `window` rather than `document` because window listeners also receive events
/// that bubble up from the document, which is all the shell's Escape handling
/// needs. The caller owns the returned handle and should release it through
/// [`OwnedListener::release`] in an `on_cleanup`.
pub fn on_keydown(callback: impl Fn(web_sys::KeyboardEvent) + 'static) -> OwnedListener {
    OwnedListener(Some(ListenerHandle::Window(window_event_listener(
        ev::keydown,
        callback,
    ))))
}

/// Listen for a key press during document capture.
///
/// The directory picker in the reference attaches its Escape handler to the
/// document with `capture: true`, so it stops the event before the focused
/// control or other bubbling handlers see it. Keep that phase available for
/// transient controls whose default-event semantics depend on it.
pub fn on_document_keydown_capture(
    callback: impl Fn(web_sys::KeyboardEvent) + 'static,
) -> OwnedListener {
    let Some(document) = web_sys::window().and_then(|window| window.document()) else {
        return OwnedListener(None);
    };
    let listener = Closure::<dyn FnMut(web_sys::KeyboardEvent)>::new(callback);
    let listener = listener.into_js_value();
    let _ = document.add_event_listener_with_callback_and_bool(
        "keydown",
        listener.unchecked_ref(),
        true,
    );
    let cleanup = leptos::__reexports::send_wrapper::SendWrapper::new((document, listener));
    OwnedListener(Some(ListenerHandle::Raw(Box::new(move || {
        let (document, listener) = cleanup.take();
        let _ = document.remove_event_listener_with_callback_and_bool(
            "keydown",
            listener.unchecked_ref(),
            true,
        );
    }))))
}

/// Dismiss layout-changing controls after the target has received its click.
/// Closing them on pointerdown can move navigation before pointerup is delivered.
pub fn on_click(callback: impl Fn(web_sys::MouseEvent) + 'static) -> OwnedListener {
    OwnedListener(Some(ListenerHandle::Window(window_event_listener(
        ev::click,
        callback,
    ))))
}

fn on_document_event(
    kind: &'static str,
    capture: bool,
    callback: impl Fn(web_sys::Event) + 'static,
) -> OwnedListener {
    let Some(document) = web_sys::window().and_then(|window| window.document()) else {
        return OwnedListener(None);
    };
    let listener = Closure::<dyn FnMut(web_sys::Event)>::new(callback).into_js_value();
    let _ =
        document.add_event_listener_with_callback_and_bool(kind, listener.unchecked_ref(), capture);
    let cleanup = leptos::__reexports::send_wrapper::SendWrapper::new((document, listener));
    OwnedListener(Some(ListenerHandle::Raw(Box::new(move || {
        let (document, listener) = cleanup.take();
        let _ = document.remove_event_listener_with_callback_and_bool(
            kind,
            listener.unchecked_ref(),
            capture,
        );
    }))))
}

/// Parent layers run before document-level child menus, regardless of mount order.
pub fn on_window_capture(
    kind: &'static str,
    callback: impl Fn(web_sys::Event) + 'static,
) -> OwnedListener {
    let Some(window) = web_sys::window() else {
        return OwnedListener(None);
    };
    let listener = Closure::<dyn FnMut(web_sys::Event)>::new(callback).into_js_value();
    let _ = window.add_event_listener_with_callback_and_bool(kind, listener.unchecked_ref(), true);
    let cleanup = leptos::__reexports::send_wrapper::SendWrapper::new((window, listener));
    OwnedListener(Some(ListenerHandle::Raw(Box::new(move || {
        let (window, listener) = cleanup.take();
        let _ = window.remove_event_listener_with_callback_and_bool(
            kind,
            listener.unchecked_ref(),
            true,
        );
    }))))
}

/// Composed paths include shadow roots; registered portal panels need no DOM ancestry.
pub fn event_inside(event: &web_sys::Event, contains: impl Fn(&web_sys::Node) -> bool) -> bool {
    event
        .composed_path()
        .iter()
        .filter_map(|target| target.dyn_into::<web_sys::Node>().ok())
        .any(|target| contains(&target))
        || event
            .target()
            .and_then(|target| target.dyn_into::<web_sys::Node>().ok())
            .is_some_and(|target| contains(&target))
}

/// Transient menus share outside dismissal, Escape and one active floating surface.
/// Nested sections belong to their containing surface; ordinary input stays open.
/// `close(true)` restores keyboard focus, while pointer dismissal preserves its target.
pub fn dismiss_popover(
    open: Signal<bool>,
    owner: impl Fn() -> Option<web_sys::Element> + Copy + Send + Sync + 'static,
    contains: impl Fn(&web_sys::Node) -> bool + 'static,
    close: Callback<bool>,
) {
    // Click capture sees stopped events after their pointer target is settled.
    let mut outside = on_document_event("click", true, move |event| {
        if open.get_untracked() && !event_inside(&event, &contains) {
            close.run(false);
        }
    });
    let mut escape = on_document_keydown_capture(move |event| {
        if open.get_untracked() && event.key() == "Escape" && !event.default_prevented() {
            event.prevent_default();
            event.stop_propagation();
            // Close the active nested section first and keep its parent visible,
            // so keyboard focus can return to the section's summary.
            if let Some(section) = owner().and_then(|node| {
                node.query_selector("details.embedded-menu[open]")
                    .ok()
                    .flatten()
            }) {
                let _ = section.remove_attribute("open");
                if let Some(trigger) = section
                    .first_element_child()
                    .and_then(|node| node.dyn_into::<web_sys::HtmlElement>().ok())
                {
                    let _ = trigger.focus();
                }
                return;
            }
            close.run(true);
        }
    });
    let mut exclusive = on_document_event("revaro:popover-open", false, move |event| {
        if open.get_untracked()
            && let Some(current) = owner()
            && let Some(other) = event
                .dyn_into::<web_sys::CustomEvent>()
                .ok()
                .and_then(|event| event.detail().dyn_into::<web_sys::Element>().ok())
            && !current.contains(Some(&other))
        {
            close.run(false);
        }
    });
    Effect::new(move |_| {
        if open.get()
            && let Some(owner) = owner()
            && let Some(document) = web_sys::window().and_then(|window| window.document())
        {
            let options = web_sys::CustomEventInit::new();
            options.set_detail(&owner);
            if let Ok(event) =
                web_sys::CustomEvent::new_with_event_init_dict("revaro:popover-open", &options)
            {
                let _ = document.dispatch_event(&event);
            }
        }
    });
    on_cleanup(move || {
        outside.release();
        escape.release();
        exclusive.release();
    });
}

/// A cancellable delay. Dropping the future clears the browser timer and releases
/// its Rust captures immediately, including when a scoped task is unmounted.
pub async fn delay(milliseconds: i32) {
    let Some(window) = web_sys::window() else {
        return;
    };
    let (send, receive) = futures_channel::oneshot::channel();
    let callback = Closure::once(move || {
        let _ = send.send(());
    });
    let Ok(id) = window.set_timeout_with_callback_and_timeout_and_arguments_0(
        callback.as_ref().unchecked_ref(),
        milliseconds,
    ) else {
        return;
    };
    struct Timer {
        window: web_sys::Window,
        id: i32,
        _callback: Closure<dyn FnMut()>,
    }
    impl Drop for Timer {
        fn drop(&mut self) {
            self.window.clear_timeout_with_handle(self.id);
        }
    }
    let _timer = Timer {
        window,
        id,
        _callback: callback,
    };
    let _ = receive.await;
}

/// Listen for viewport changes while a transient browser view is mounted.
pub fn on_resize(callback: impl Fn(leptos::ev::UiEvent) + 'static) -> OwnedListener {
    OwnedListener(Some(ListenerHandle::Window(window_event_listener(
        ev::resize,
        callback,
    ))))
}

/// Listen for viewport or scroll-container movement while a positioned
/// transient view is mounted.
///
/// Scroll events from element scroll containers do not bubble. The reference
/// registers this listener on `window` with capture enabled, which is why this
/// helper cannot use the ordinary bubbling `WindowListenerHandle` path.
pub fn on_scroll(callback: impl Fn(leptos::ev::Event) + 'static) -> OwnedListener {
    let Some(window) = web_sys::window() else {
        return OwnedListener(None);
    };
    let listener = Closure::<dyn FnMut(web_sys::Event)>::new(callback);
    let listener = listener.into_js_value();
    let _ =
        window.add_event_listener_with_callback_and_bool("scroll", listener.unchecked_ref(), true);
    let cleanup = leptos::__reexports::send_wrapper::SendWrapper::new((window, listener));
    OwnedListener(Some(ListenerHandle::Raw(Box::new(move || {
        let (window, listener) = cleanup.take();
        let _ = window.remove_event_listener_with_callback_and_bool(
            "scroll",
            listener.unchecked_ref(),
            true,
        );
    }))))
}

/// Position existing popovers below the actual trigger, centered horizontally.
/// Clamp only at viewport edges and flip above when there is more room there.
/// Resize and scroll listeners keep open panels attached to their trigger.
pub fn anchor_popover<A, P>(anchor: NodeRef<A>, panel: NodeRef<P>) -> Callback<()>
where
    A: leptos::html::ElementType,
    A::Output: JsCast + Clone,
    P: leptos::html::ElementType,
    P::Output: JsCast + Clone,
{
    let update = Callback::new(move |()| {
        let Some(anchor) = anchor
            .get_untracked()
            .map(|node| node.unchecked_into::<web_sys::Element>())
        else {
            return;
        };
        let Some(panel) = panel
            .get_untracked()
            .map(|node| node.unchecked_into::<web_sys::HtmlElement>())
        else {
            return;
        };
        let Some(window) = web_sys::window() else {
            return;
        };
        if anchor.get_client_rects().length() == 0 || panel.get_client_rects().length() == 0 {
            return;
        }
        let viewport_width = window
            .inner_width()
            .ok()
            .and_then(|v| v.as_f64())
            .unwrap_or(0.0);
        let viewport_height = window
            .inner_height()
            .ok()
            .and_then(|v| v.as_f64())
            .unwrap_or(0.0);
        let trigger = anchor.get_bounding_client_rect();
        let bounds = panel.get_bounding_client_rect();
        let margin = 8.0;
        let gap = 8.0;
        let below = (viewport_height - trigger.bottom() - gap - margin).max(0.0);
        let above = (trigger.top() - gap - margin).max(0.0);
        let opens_up = bounds.height() > below && above > below;
        let available = if opens_up { above } else { below };
        let height = bounds.height().min(available);
        let left = (trigger.left() + (trigger.width() - bounds.width()) / 2.0).clamp(
            margin,
            (viewport_width - margin - bounds.width()).max(margin),
        );
        let top = if opens_up {
            trigger.top() - gap - height
        } else {
            trigger.bottom() + gap
        };
        // Keep the existing DOM and stacking context, including media controls
        // inside transformed ancestors; convert viewport coordinates to the offset parent.
        let parent = panel.offset_parent();
        let parent_left = parent
            .as_ref()
            .map(|p| {
                p.get_bounding_client_rect().left() + f64::from(p.client_left())
                    - f64::from(p.scroll_left())
            })
            .unwrap_or(0.0);
        let parent_top = parent
            .as_ref()
            .map(|p| {
                p.get_bounding_client_rect().top() + f64::from(p.client_top())
                    - f64::from(p.scroll_top())
            })
            .unwrap_or(0.0);
        let style = panel.style();
        let _ = style.set_property("left", &format!("{}px", left - parent_left));
        let _ = style.set_property("top", &format!("{}px", top - parent_top));
        let _ = style.set_property("right", "auto");
        let _ = style.set_property("bottom", "auto");
        let _ = style.set_property("transform", "none");
        let _ = style.set_property("--popover-available-height", &format!("{available}px"));
    });
    // ResizeObserver runs during layout: schedule its correction in the next
    // frame so viewport-height clamping cannot trigger an observer feedback loop.
    let frame = StoredValue::new(None::<i32>);
    let frame_callback = StoredValue::new(leptos::__reexports::send_wrapper::SendWrapper::new(
        Closure::<dyn FnMut(f64)>::new(move |_| {
            frame.set_value(None);
            update.run(());
        }),
    ));
    let schedule = Callback::new(move |()| {
        if frame.get_value().is_none()
            && let Some(window) = web_sys::window()
        {
            frame_callback.with_value(|callback| {
                if let Ok(id) = window.request_animation_frame(callback.as_ref().unchecked_ref()) {
                    frame.set_value(Some(id));
                }
            });
        }
    });
    let observer = StoredValue::new(
        None::<
            leptos::__reexports::send_wrapper::SendWrapper<(
                web_sys::ResizeObserver,
                Closure<dyn FnMut()>,
            )>,
        >,
    );
    Effect::new(move |_| {
        let anchor_node = anchor.get();
        let panel_node = panel.get();
        observer.update_value(|old| {
            if let Some(old) = old.take() {
                old.0.disconnect();
            }
        });
        if let (Some(anchor), Some(panel)) = (anchor_node, panel_node) {
            let callback = Closure::<dyn FnMut()>::new(move || schedule.run(()));
            if let Ok(resize) = web_sys::ResizeObserver::new(callback.as_ref().unchecked_ref()) {
                resize.observe(anchor.unchecked_ref());
                resize.observe(panel.unchecked_ref());
                observer.set_value(Some(leptos::__reexports::send_wrapper::SendWrapper::new((
                    resize, callback,
                ))));
            }
            update.run(());
        }
    });
    let mut resize = on_resize(move |_| schedule.run(()));
    let mut scroll = on_scroll(move |_| schedule.run(()));
    on_cleanup(move || {
        resize.release();
        scroll.release();
        if let Some(id) = frame.get_value()
            && let Some(window) = web_sys::window()
        {
            window.cancel_animation_frame(id).ok();
        }
        observer.update_value(|old| {
            if let Some(old) = old.take() {
                old.0.disconnect();
            }
        });
    });
    update
}

/// Listen for changes to the document's fullscreen element.
pub fn on_fullscreenchange(callback: impl Fn(leptos::ev::Event) + 'static) -> OwnedListener {
    OwnedListener(Some(ListenerHandle::Window(window_event_listener(
        ev::fullscreenchange,
        callback,
    ))))
}

/// Listen for browser history navigation.
pub fn on_popstate(callback: impl Fn(web_sys::PopStateEvent) + 'static) -> OwnedListener {
    OwnedListener(Some(ListenerHandle::Window(window_event_listener(
        ev::popstate,
        callback,
    ))))
}

/// Flush playback state when navigating away or entering the back/forward cache.
pub fn on_pagehide(callback: impl Fn(web_sys::PageTransitionEvent) + 'static) -> OwnedListener {
    OwnedListener(Some(ListenerHandle::Window(window_event_listener(
        ev::pagehide,
        callback,
    ))))
}

/// A document/window listener that unregisters when dropped or released.
///
/// Leptos's raw [`WindowListenerHandle`] is a remove-only handle; wrapping it
/// lets a component keep it in a `StoredValue` and release it from
/// `on_cleanup` without leaking the closure for the page's lifetime.
enum ListenerHandle {
    Window(WindowListenerHandle),
    Raw(Box<dyn FnOnce() + Send + Sync>),
}

pub struct OwnedListener(Option<ListenerHandle>);

impl OwnedListener {
    /// Unregister the listener. Calling this twice is a no-op.
    pub fn release(&mut self) {
        if let Some(handle) = self.0.take() {
            match handle {
                ListenerHandle::Window(handle) => handle.remove(),
                ListenerHandle::Raw(handle) => handle(),
            }
        }
    }
}

impl Drop for OwnedListener {
    fn drop(&mut self) {
        self.release();
    }
}

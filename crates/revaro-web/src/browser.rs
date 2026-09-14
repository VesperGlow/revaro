//! Small browser interop helpers used by the shell.
//!
//! Nothing here is application logic; it is the handful of `web-sys` calls the
//! shell needs and that do not belong inline in a view: media-query signals,
//! `localStorage` access and document-level event listeners.
//!
//! Only compiled for wasm. The pure replacements for the old TypeScript helpers
//! live in `crate::logic`.

use leptos::ev;
use leptos::prelude::*;
use wasm_bindgen::JsCast;
use wasm_bindgen::closure::Closure;
use web_sys::MediaQueryListEvent;

/// A reactive `matchMedia(query).matches`.
///
/// `AppSidebar` and `AppTopbar` both switch layout at 850 px by reading
/// `window.matchMedia`, and both must react when the window crosses the
/// breakpoint rather than only at startup. The listener is installed once and
/// released with the component that owns the signal, matching the Vue
/// components' `onMounted`/`onBeforeUnmount` pair.
#[allow(dead_code)]
pub fn media_query_signal(query: &str) -> RwSignal<bool> {
    let signal = RwSignal::new(media_query_matches(query));
    let Some(window) = web_sys::window() else {
        return signal;
    };
    let Ok(Some(list)) = window.match_media(query) else {
        return signal;
    };
    let listener =
        Closure::<dyn FnMut(MediaQueryListEvent)>::new(move |event: MediaQueryListEvent| {
            signal.set(event.matches());
        });
    let _ = list.add_event_listener_with_callback("change", listener.as_ref().unchecked_ref());
    let cleanup = leptos::__reexports::send_wrapper::SendWrapper::new((list, listener));
    on_cleanup(move || {
        let (list, listener) = cleanup.take();
        let _ =
            list.remove_event_listener_with_callback("change", listener.as_ref().unchecked_ref());
    });
    signal
}

/// One-shot `matchMedia(query).matches`, false when the API is unavailable.
#[must_use]
#[allow(dead_code)]
pub fn media_query_matches(query: &str) -> bool {
    web_sys::window()
        .and_then(|window| window.match_media(query).ok().flatten())
        .is_some_and(|list| list.matches())
}

/// Read a `localStorage` value, treating any storage failure as "absent".
///
/// Private-mode browsers throw on access; the Vue helpers wrapped every call in
/// `try/catch` for the same reason, and a missing preference must never break
/// startup.
#[must_use]
#[allow(dead_code)]
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
#[allow(dead_code)]
pub fn local_storage_set(key: &str, value: &str) {
    if let Some(storage) =
        web_sys::window().and_then(|window| window.local_storage().ok().flatten())
    {
        let _ = storage.set_item(key, value);
    }
}

/// Remove a `localStorage` value, ignoring a throwing storage.
#[allow(dead_code)]
pub fn local_storage_remove(key: &str) {
    if let Some(storage) =
        web_sys::window().and_then(|window| window.local_storage().ok().flatten())
    {
        let _ = storage.remove_item(key);
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

/// Listen for a pointer press anywhere in the window.
pub fn on_pointerdown(callback: impl Fn(web_sys::PointerEvent) + 'static) -> OwnedListener {
    OwnedListener(Some(ListenerHandle::Window(window_event_listener(
        ev::pointerdown,
        callback,
    ))))
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
pub fn on_scroll(callback: impl Fn(leptos::ev::Event) + 'static) -> OwnedListener {
    OwnedListener(Some(ListenerHandle::Window(window_event_listener(
        ev::scroll,
        callback,
    ))))
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

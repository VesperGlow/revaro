//! The in-app image/media preview shell.
//!
//! A preview is mounted inside the authenticated browser instead of opening a
//! second tab. This keeps the session cookie, gallery navigation and playback
//! state in one document. Audio and video have their own player modules; this
//! module owns the shared command surface, focus lifecycle and image geometry.

use std::collections::HashMap;

use leptos::ev::{Event, MouseEvent, WheelEvent};
use leptos::prelude::*;
use revaro_core::classify;
use revaro_core::model::File;
use wasm_bindgen::JsCast;
use wasm_bindgen::closure::Closure;
use web_sys::{Element, HtmlDetailsElement, HtmlImageElement, KeyboardEvent, PointerEvent};

use crate::browser;
use crate::logic::format::format_size;
use crate::logic::image_geometry::{Point, Size, clamp_image_pan, fit_image, zoom_image_pan};

use super::audio::AudioPlayer;
use super::icons;
use super::video::VideoPlayer;

/// The static icon used by a small native details menu.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MenuIcon {
    /// An overflow action menu.
    More,
    /// Subtitle selection.
    Captions,
    /// Playback settings.
    Settings,
    /// Volume controls.
    Volume,
}

/// A disclosure menu that closes when focus moves outside it.
///
/// Native `<details>` preserves keyboard and screen-reader semantics. The
/// document listener only adds the outside-pointer behaviour that native
/// disclosure elements do not provide consistently across browsers.
#[component]
pub fn PreviewMenu(
    label: String,
    icon: MenuIcon,
    #[prop(optional)] volume: Option<RwSignal<f64>>,
    #[prop(optional)] muted: Option<RwSignal<bool>>,
    #[prop(optional)] on_toggle: Option<Callback<bool>>,
    children: Children,
) -> impl IntoView {
    let menu = NodeRef::<leptos::html::Details>::new();
    let outside_menu = menu;
    let mut outside = browser::on_pointerdown(move |event| {
        let Some(details) = outside_menu.get() else {
            return;
        };
        if !details.open() {
            return;
        }
        let inside = event
            .target()
            .and_then(|target| target.dyn_into::<web_sys::Node>().ok())
            .is_some_and(|target| details.contains(Some(&target)));
        if !inside {
            details.set_open(false);
        }
    });
    on_cleanup(move || outside.release());

    let menu_for_escape = menu;
    let on_toggle = on_toggle.clone();
    view! {
        <details
            node_ref=menu
            class="preview-menu"
            on:toggle=move |_| {
                if let Some(callback) = on_toggle.as_ref()
                    && let Some(details) = menu_for_escape.get()
                {
                    callback.run(details.open());
                }
            }
            on:keydown=move |event: KeyboardEvent| {
                if event.key() == "Escape" {
                    if let Some(details) = menu_for_escape.get() {
                        if details.open() {
                            event.prevent_default();
                            event.stop_propagation();
                            details.set_open(false);
                            let _ = details
                                .query_selector("summary")
                                .ok()
                                .flatten()
                                .and_then(|element| element.dyn_into::<web_sys::HtmlElement>().ok())
                                .map(|element| element.focus());
                        }
                    }
                }
            }
        >
            <summary
                aria-label=label.clone()
                title=label
            >
                {move || {
                    if icon == MenuIcon::Volume
                        && (muted.is_some_and(|value| value.get())
                            || volume.is_some_and(|value| value.get() <= 0.0))
                    {
                        icons::volume_x().into_any()
                    } else {
                        menu_icon(icon)
                    }
                }}
            </summary>
            <div
                class="preview-menu-panel"
                on:click=move |event: MouseEvent| {
                    let Some(target) = event
                        .target()
                        .and_then(|target| target.dyn_into::<Element>().ok())
                    else {
                        return;
                    };
                    if target.closest("[data-close-menu]").ok().flatten().is_some()
                        && let Some(details) = menu.get()
                    {
                        details.set_open(false);
                    }
                }
            >{children()}</div>
        </details>
    }
}

/// The preview for an image, audio file or video file.
#[component]
pub fn MediaPreview(
    selected: RwSignal<Option<File>>,
    items: RwSignal<Vec<File>>,
    on_close: Callback<()>,
    on_download: Callback<File>,
    on_move: Callback<File>,
    on_copy: Callback<File>,
) -> impl IntoView {
    let root = NodeRef::<leptos::html::Section>::new();
    let stage = NodeRef::<leptos::html::Div>::new();
    let chrome_visible = RwSignal::new(true);
    let thumbnails_open = RwSignal::new(false);
    let loading = RwSignal::new(true);
    let image_error = RwSignal::new(false);
    let natural = RwSignal::new(Size {
        width: 0.0,
        height: 0.0,
    });
    let stage_size = RwSignal::new(Size {
        width: 0.0,
        height: 0.0,
    });
    let zoom = RwSignal::new(1.0_f64);
    let pan = RwSignal::new(Point { x: 0.0, y: 0.0 });
    let pointers = RwSignal::new(HashMap::<i32, Point>::new());
    let drag = RwSignal::new(DragState::default());
    let click_timer = RwSignal::new(None::<i32>);
    let last_selected_id = StoredValue::new(String::new());

    let previous_focus = web_sys::window().and_then(|window| window.document()?.active_element());
    let previous_overflow = web_sys::window()
        .and_then(|window| window.document())
        .and_then(|document| document.body())
        .map(|body| {
            body.style()
                .get_property_value("overflow")
                .unwrap_or_default()
        });

    // Reset image-only state when the gallery changes. Audio/video children are
    // keyed by their file id and therefore get a fresh player at the same time.
    {
        let last_selected_id = last_selected_id;
        Effect::new(move |_| {
            let Some(file) = selected.get() else {
                return;
            };
            let previous_id = last_selected_id.get_value();
            if previous_id == file.id {
                return;
            }
            last_selected_id.set_value(file.id.clone());
            chrome_visible.set(true);
            loading.set(classify::is_image(&file));
            image_error.set(false);
            natural.set(Size {
                width: 0.0,
                height: 0.0,
            });
            zoom.set(1.0);
            pan.set(Point { x: 0.0, y: 0.0 });
            pointers.set(HashMap::new());
            drag.set(DragState::default());
            reveal_thumbnail(root);
            if classify::is_image(&file) && !previous_id.is_empty() {
                preload_adjacent(&file, &items.get_untracked());
            }
        });
    }

    // The stage is fixed to the viewport, but a resize/orientation change can
    // happen while the viewer is open. A window listener avoids retaining a
    // ResizeObserver and its closure beyond the viewer's lifetime.
    let stage_for_resize = stage;
    let resize_pan = pan;
    let resize_natural = natural;
    let resize_zoom = zoom;
    let resize_size = stage_size;
    let mut resize_listener = browser::on_resize(move |_| {
        update_stage_size(stage_for_resize, resize_size);
        clamp_pan(resize_pan, resize_natural, resize_size, resize_zoom);
    });

    let document = web_sys::window().and_then(|window| window.document());
    let body = document.as_ref().and_then(|document| document.body());
    if let Some(body) = &body {
        let _ = body.style().set_property("overflow", "hidden");
    }
    let root_for_mount = root;
    let mut mounted = false;
    // NodeRef is populated after the view is inserted. The zero-delay callback
    // gives the browser one layout turn before moving focus to the dialog.
    if let Some(window) = web_sys::window() {
        let callback = Closure::once_into_js(move || {
            if let Some(element) = root_for_mount.get() {
                let options = web_sys::FocusOptions::new();
                options.set_prevent_scroll(true);
                let _ = element.focus_with_options(&options);
            }
            update_stage_size(stage_for_resize, resize_size);
            clamp_pan(resize_pan, resize_natural, resize_size, resize_zoom);
        });
        let _ = window
            .set_timeout_with_callback_and_timeout_and_arguments_0(callback.unchecked_ref(), 0);
        mounted = true;
    }

    on_cleanup(move || {
        resize_listener.release();
        if let Some(timer) = click_timer.get_untracked()
            && let Some(window) = web_sys::window()
        {
            window.clear_timeout_with_handle(timer);
        }
        if let Some(body) = body {
            if let Some(previous) = previous_overflow {
                let _ = body.style().set_property("overflow", &previous);
            } else {
                let _ = body.style().remove_property("overflow");
            }
        }
        if mounted {
            if let Some(element) = previous_focus {
                if let Ok(element) = element.dyn_into::<web_sys::HtmlElement>() {
                    if element.is_connected() {
                        let options = web_sys::FocusOptions::new();
                        options.set_prevent_scroll(true);
                        let _ = element.focus_with_options(&options);
                    }
                }
            }
        }
    });

    let on_key = {
        let root = root;
        let selected = selected;
        let items = items;
        let on_change = Callback::new({
            let selected = selected;
            move |file: File| selected.set(Some(file))
        });
        move |event: KeyboardEvent| {
            if event.default_prevented() {
                return;
            }
            if event.key() == "Escape" {
                if web_sys::window()
                    .and_then(|window| window.document())
                    .and_then(|document| document.fullscreen_element())
                    .is_some()
                {
                    return;
                }
                if let Some(details) = root
                    .get()
                    .and_then(|element| element.query_selector("details[open]").ok().flatten())
                {
                    if let Ok(details) = details.dyn_into::<HtmlDetailsElement>() {
                        event.prevent_default();
                        event.stop_propagation();
                        details.set_open(false);
                        return;
                    }
                }
                event.prevent_default();
                event.stop_propagation();
                on_close.run(());
                return;
            }
            if event.key() == "Tab" {
                trap_focus(root, &event);
                chrome_visible.set(true);
                return;
            }
            let Some(file) = selected.get_untracked() else {
                return;
            };
            if !classify::is_image(&file) {
                return;
            }
            if event
                .target()
                .and_then(|target| target.dyn_into::<Element>().ok())
                .and_then(|target| target.closest("input, select, details[open]").ok())
                .flatten()
                .is_some()
            {
                return;
            }
            match event.key().as_str() {
                "ArrowLeft" | "ArrowRight" => {
                    event.prevent_default();
                    if zoom.get_untracked() <= 1.0 {
                        change_gallery(
                            selected,
                            items,
                            if event.key() == "ArrowLeft" { -1 } else { 1 },
                            on_change.clone(),
                        );
                    } else {
                        let direction = if event.key() == "ArrowLeft" {
                            64.0
                        } else {
                            -64.0
                        };
                        pan.update(|point| point.x += direction);
                        clamp_pan(pan, natural, stage_size, zoom);
                    }
                }
                "+" | "=" => {
                    event.prevent_default();
                    set_zoom(
                        zoom,
                        pan,
                        natural,
                        stage_size,
                        zoom.get_untracked() * 1.2,
                        Point { x: 0.0, y: 0.0 },
                        Point { x: 0.0, y: 0.0 },
                    );
                }
                "-" => {
                    event.prevent_default();
                    set_zoom(
                        zoom,
                        pan,
                        natural,
                        stage_size,
                        zoom.get_untracked() / 1.2,
                        Point { x: 0.0, y: 0.0 },
                        Point { x: 0.0, y: 0.0 },
                    );
                }
                "0" => fit_zoom(zoom, pan),
                "1" => {
                    set_zoom(
                        zoom,
                        pan,
                        natural,
                        stage_size,
                        actual_zoom(natural.get_untracked(), stage_size.get_untracked()),
                        Point { x: 0.0, y: 0.0 },
                        Point { x: 0.0, y: 0.0 },
                    );
                }
                _ => {}
            }
        }
    };

    let on_pointer_down = {
        let stage = stage;
        let selected = selected;
        move |event: PointerEvent| {
            let Some(file) = selected.get_untracked() else {
                return;
            };
            if !classify::is_image(&file)
                || image_error.get_untracked()
                || loading.get_untracked()
                || event.button() != 0
                || event
                    .target()
                    .and_then(|target| target.dyn_into::<Element>().ok())
                    .and_then(|target| target.closest("button").ok())
                    .flatten()
                    .is_some()
            {
                return;
            }
            let point = Point {
                x: f64::from(event.client_x()),
                y: f64::from(event.client_y()),
            };
            pointers.update(|active| {
                active.insert(event.pointer_id(), point);
            });
            let active_count = pointers.get_untracked().len();
            if active_count == 1 {
                drag.set(DragState {
                    active: true,
                    start_x: point.x,
                    start_y: point.y,
                    dx: 0.0,
                    dy: 0.0,
                    moved: false,
                    pinched: false,
                });
            } else if active_count > 1 {
                drag.update(|state| {
                    state.pinched = true;
                    state.moved = true;
                    state.dx = 0.0;
                    state.dy = 0.0;
                });
            }
            if let Some(stage) = stage
                .get()
                .map(|element| element.unchecked_into::<Element>())
            {
                let _ = stage.set_pointer_capture(event.pointer_id());
            }
        }
    };

    let on_pointer_move = {
        let stage = stage;
        move |event: PointerEvent| {
            let Some(before) = pointers.get_untracked().get(&event.pointer_id()).copied() else {
                return;
            };
            let old = pointers.get_untracked();
            pointers.update(|active| {
                active.insert(
                    event.pointer_id(),
                    Point {
                        x: f64::from(event.client_x()),
                        y: f64::from(event.client_y()),
                    },
                );
            });
            let current = pointers.get_untracked();
            if current.len() >= 2 {
                if let (Some((old_a, old_b)), Some((new_a, new_b))) =
                    (two_points(&old), two_points(&current))
                {
                    let old_distance = distance(old_a, old_b);
                    let new_distance = distance(new_a, new_b);
                    if old_distance > 0.0 {
                        let old_mid = midpoint(old_a, old_b);
                        let new_mid = midpoint(new_a, new_b);
                        set_zoom(
                            zoom,
                            pan,
                            natural,
                            stage_size,
                            zoom.get_untracked() * new_distance / old_distance,
                            local_point(stage, old_mid),
                            local_point(stage, new_mid),
                        );
                    }
                }
                drag.update(|state| {
                    state.pinched = true;
                    state.moved = true;
                    state.dx = 0.0;
                    state.dy = 0.0;
                });
                event.prevent_default();
                return;
            }
            let dx = f64::from(event.client_x()) - drag.get_untracked().start_x;
            let dy = f64::from(event.client_y()) - drag.get_untracked().start_y;
            drag.update(|state| {
                state.dx = dx;
                state.dy = dy;
                if (dx * dx + dy * dy).sqrt() > 5.0 {
                    state.moved = true;
                }
            });
            if zoom.get_untracked() > 1.0 {
                pan.update(|point| {
                    point.x += f64::from(event.client_x()) - before.x;
                    point.y += f64::from(event.client_y()) - before.y;
                });
                clamp_pan(pan, natural, stage_size, zoom);
                event.prevent_default();
            }
        }
    };

    let on_pointer_end = move |event: PointerEvent| {
        if !pointers.get_untracked().contains_key(&event.pointer_id()) {
            return;
        }
        let was_cancelled = event.type_() == "pointercancel";
        pointers.update(|active| {
            active.remove(&event.pointer_id());
        });
        if let Some(stage) = stage
            .get()
            .map(|element| element.unchecked_into::<Element>())
        {
            let _ = stage.release_pointer_capture(event.pointer_id());
        }
        let remaining = pointers.get_untracked();
        if !remaining.is_empty() {
            if let Some(point) = remaining.values().next().copied() {
                drag.update(|state| {
                    state.start_x = point.x;
                    state.start_y = point.y;
                    state.dx = 0.0;
                    state.dy = 0.0;
                });
            }
            return;
        }
        let state = drag.get_untracked();
        if was_cancelled {
            drag.update(|state| {
                state.pinched = true;
                state.moved = true;
            });
        } else if !state.pinched
            && zoom.get_untracked() <= 1.0
            && state.dx.abs() > 60.0
            && state.dx.abs() > state.dy.abs() * 1.25
        {
            change_gallery(
                selected,
                items,
                if state.dx < 0.0 { 1 } else { -1 },
                Callback::new(move |file: File| selected.set(Some(file))),
            );
        }
        // Keep the gesture flags through the synthetic click emitted after a
        // drag/pinch. The reference viewer ignores that click; the next
        // pointer-down starts a new gesture and clears the flags.
        drag.update(|state| {
            state.active = false;
            state.dx = 0.0;
            state.dy = 0.0;
        });
    };

    let on_stage_click = move |event: MouseEvent| {
        if !selected
            .get_untracked()
            .is_some_and(|file| classify::is_image(&file))
            || drag.get_untracked().moved
            || event
                .target()
                .and_then(|target| target.dyn_into::<Element>().ok())
                .and_then(|target| target.closest("button").ok())
                .flatten()
                .is_some()
        {
            return;
        }
        if let Some(timer) = click_timer.get_untracked()
            && let Some(window) = web_sys::window()
        {
            window.clear_timeout_with_handle(timer);
        }
        if event.detail() < 2 {
            let chrome_visible = chrome_visible;
            let timer = Closure::once_into_js(move || {
                click_timer.set(None);
                chrome_visible.update(|visible| *visible = !*visible);
            });
            if let Some(window) = web_sys::window()
                && let Ok(id) = window.set_timeout_with_callback_and_timeout_and_arguments_0(
                    timer.unchecked_ref(),
                    220,
                )
            {
                click_timer.set(Some(id));
            }
        }
    };

    let on_wheel = move |event: WheelEvent| {
        if !selected
            .get_untracked()
            .is_some_and(|file| classify::is_image(&file))
            || loading.get_untracked()
        {
            return;
        }
        event.prevent_default();
        let delta = event.delta_y()
            * match event.delta_mode() {
                1 => 16.0,
                2 => stage_size.get_untracked().height,
                _ => 1.0,
            };
        let delta = delta.clamp(-120.0, 120.0);
        let point = Point {
            x: f64::from(event.client_x()),
            y: f64::from(event.client_y()),
        };
        let local = local_point(stage, point);
        set_zoom(
            zoom,
            pan,
            natural,
            stage_size,
            zoom.get_untracked() * (-delta * 0.002).exp(),
            local,
            local,
        );
    };

    let set_selected = Callback::new({
        let selected = selected;
        move |file: File| selected.set(Some(file))
    });
    let close = on_close.clone();
    let download = on_download.clone();
    let move_item = on_move.clone();
    let copy_item = on_copy.clone();

    view! {
        <div class="modal-backdrop previewing" role="presentation">
            <section
                node_ref=root
                class="preview-modal"
                class:image-preview=move || selected.get().as_ref().is_some_and(classify::is_image)
                class:audio-preview=move || selected.get().as_ref().is_some_and(classify::is_audio)
                class:video-preview=move || selected.get().as_ref().is_some_and(classify::is_video)
                class:chrome-hidden=move || !chrome_visible.get()
                class:thumbnails-open=move || thumbnails_open.get()
                role="dialog"
                aria-modal="true"
                aria-label=move || selected.get().map(|file| file.name).unwrap_or_else(|| "媒体预览".to_owned())
                tabindex="-1"
                on:keydown=on_key
            >
                {move || {
                    selected.get().map(|file| {
                        if classify::is_video(&file) {
                            ().into_any()
                        } else {
                            let image_file = classify::is_image(&file);
                            view! {
                                <header
                                    class="preview-commandbar"
                                    inert=move || image_file && !chrome_visible.get()
                                >
                                    <div class="preview-file-meta">
                                        <strong title=file.name.clone()>{file.name.clone()}</strong>
                                    </div>
                                    <Show when=move || image_file fallback=|| ()>
                                        <span class="preview-count">
                                            {move || format!("{} / {}", gallery_position(selected, items), gallery_items(&items.get()).len())}
                                        </span>
                                    </Show>
                                    <div class="preview-file-actions">
                                        <PreviewMenu label="更多操作".to_owned() icon=MenuIcon::More>
                                            <button type="button" data-close-menu="true" on:click={
                                                let download = download.clone();
                                                let file = file.clone();
                                                move |_| download.run(file.clone())
                                            }>
                                                {icons::download()}<span>"下载"</span>
                                            </button>
                                            <button type="button" data-close-menu="true" on:click={
                                                let move_item = move_item.clone();
                                                let file = file.clone();
                                                move |_| move_item.run(file.clone())
                                            }>
                                                {icons::move_icon()}<span>"移动"</span>
                                            </button>
                                            <button type="button" data-close-menu="true" on:click={
                                                let copy_item = copy_item.clone();
                                                let file = file.clone();
                                                move |_| copy_item.run(file.clone())
                                            }>
                                                {icons::copy()}<span>"复制"</span>
                                            </button>
                                            <p class="media-detail">
                                                {icons::info()}
                                                {format_size(non_negative(file.size))}
                                                <Show when=move || natural.get().width.is_sign_positive() fallback=|| ()>
                                                    <span>{move || format!("{} × {}", natural.get().width as u64, natural.get().height as u64)}</span>
                                                </Show>
                                            </p>
                                        </PreviewMenu>
                                        <button class="media-icon-button preview-close" type="button" aria-label="关闭预览" title="关闭预览" on:click={
                                            let close = close.clone();
                                            move |_| close.run(())
                                        }>{icons::x()}</button>
                                    </div>
                                </header>
                            }.into_any()
                        }
                    }).unwrap_or_else(|| ().into_any())
                }}
                <div
                    node_ref=stage
                    class="preview-stage"
                    class:zoomed=move || zoom.get().gt(&1.0)
                    class:dragging=move || drag.get().active
                    on:click=on_stage_click
                    on:pointerdown=on_pointer_down
                    on:pointermove=on_pointer_move
                    on:pointerup=on_pointer_end
                    on:pointercancel=on_pointer_end
                    on:wheel=on_wheel
                >
                    {move || {
                        selected.get().map(|file| {
                            if classify::is_image(&file) {
                                view! {
                                    <img
                                        class="preview-image"
                                        src=format!("/api/files/{}/preview", file.id)
                                        alt=file.name.clone()
                                        draggable="false"
                                        style=move || image_style(
                                            natural.get(),
                                            stage_size.get(),
                                            pan.get(),
                                            zoom.get(),
                                            drag.get().dx,
                                            loading.get() || image_error.get(),
                                        )
                                        on:load=move |event: Event| image_loaded(event, natural, stage_size, loading, pan, zoom)
                                        on:error=move |_| { image_error.set(true); loading.set(false); }
                                        on:dblclick=move |event: MouseEvent| {
                                            event.stop_propagation();
                                            toggle_image_zoom(zoom, pan, natural, stage_size, click_timer);
                                        }
                                    />
                                    <Show when=move || image_error.get() fallback=move || view! {
                                        <Show when=move || loading.get() fallback=|| ()>
                                            <span class="preview-image-status" role="status">"正在加载图片…"</span>
                                        </Show>
                                    }>
                                        <p class="preview-image-status" role="alert">
                                            "图片暂时无法加载"
                                            <button type="button" on:click={
                                                let download = on_download.clone();
                                                let file = file.clone();
                                                move |_| download.run(file.clone())
                                            }>{"下载原图"}</button>
                                        </p>
                                    </Show>
                                    <Show when=move || gallery_items(&items.get()).len().gt(&1) fallback=|| ()>
                                        <button class="media-icon-button preview-nav preview-prev" type="button" aria-label="上一张" inert=move || !chrome_visible.get() on:click={
                                            let selected = selected;
                                            let items = items;
                                            let set_selected = set_selected.clone();
                                            move |event: MouseEvent| {
                                                event.stop_propagation();
                                                change_gallery(selected, items, -1, set_selected.clone());
                                            }
                                        }>{icons::chevron_left()}</button>
                                        <button class="media-icon-button preview-nav preview-next" type="button" aria-label="下一张" inert=move || !chrome_visible.get() on:click={
                                            let selected = selected;
                                            let items = items;
                                            let set_selected = set_selected.clone();
                                            move |event: MouseEvent| {
                                                event.stop_propagation();
                                                change_gallery(selected, items, 1, set_selected.clone());
                                            }
                                        }>{icons::chevron_right()}</button>
                                    </Show>
                                }.into_any()
                            } else if classify::is_audio(&file) {
                                view! {
                                    <AudioPlayer item=file />
                                }.into_any()
                            } else if classify::is_video(&file) {
                                view! {
                                    <VideoPlayer item=file on_close=on_close.clone() on_download=on_download.clone() on_move=on_move.clone() on_copy=on_copy.clone() />
                                }.into_any()
                            } else {
                                ().into_any()
                            }
                        }).unwrap_or_else(|| ().into_any())
                    }}
                </div>
                {move || {
                    if selected.get().as_ref().is_some_and(classify::is_image) {
                        view! {
                                <footer
                                    class="preview-image-footer"
                                    inert=move || !chrome_visible.get()
                                >
                                <div class="preview-image-tools" aria-label="图片工具">
                                    <button class="media-icon-button" type="button" aria-label="适应窗口" title="适应窗口" prop:disabled=move || loading.get() || image_error.get() on:click=move |_| fit_zoom(zoom, pan)>{icons::scan()}</button>
                                    <button class="media-icon-button" type="button" aria-label="缩小" title="缩小" prop:disabled=move || zoom.get() <= 1.0 || loading.get() || image_error.get() on:click=move |_| set_zoom(zoom, pan, natural, stage_size, zoom.get_untracked() / 1.2, Point { x: 0.0, y: 0.0 }, Point { x: 0.0, y: 0.0 })>{icons::zoom_out()}</button>
                                    <button class="preview-actual-size" type="button" aria-label="实际大小" title="实际大小（100%）" prop:disabled=move || loading.get() || image_error.get() on:click=move |_| set_zoom(zoom, pan, natural, stage_size, actual_zoom(natural.get_untracked(), stage_size.get_untracked()), Point { x: 0.0, y: 0.0 }, Point { x: 0.0, y: 0.0 })>
                                        {move || format!("{}%", zoom_percent(natural.get(), stage_size.get(), zoom.get()))}
                                    </button>
                                    <button class="media-icon-button" type="button" aria-label="放大" title="放大" prop:disabled={move || zoom.get() >= max_zoom(natural.get(), stage_size.get()) || loading.get() || image_error.get()} on:click=move |_| set_zoom(zoom, pan, natural, stage_size, zoom.get_untracked() * 1.2, Point { x: 0.0, y: 0.0 }, Point { x: 0.0, y: 0.0 })>{icons::zoom_in()}</button>
                                    <Show when=move || gallery_items(&items.get()).len().gt(&1) fallback=|| ()>
                                        <button class="media-icon-button" type="button" aria-label="缩略图" title="缩略图" aria-expanded=move || if thumbnails_open.get() { "true" } else { "false" } on:click=move |_| {
                                            let open = !thumbnails_open.get_untracked();
                                            thumbnails_open.set(open);
                                            if open {
                                                reveal_thumbnail(root);
                                            }
                                        }>{icons::gallery_horizontal_end()}</button>
                                    </Show>
                                </div>
                                <Show when=move || thumbnails_open.get() fallback=|| ()>
                                    <div class="preview-filmstrip">
                                        <For each=move || gallery_items(&items.get()) key=|file| file.id.clone() let:file>
                                            <FilmstripItem file=file selected=selected on_select=set_selected />
                                        </For>
                                    </div>
                                </Show>
                            </footer>
                        }.into_any()
                    } else {
                        ().into_any()
                    }
                }}
            </section>
        </div>
    }
}

#[component]
fn FilmstripItem(
    file: File,
    selected: RwSignal<Option<File>>,
    on_select: Callback<File>,
) -> impl IntoView {
    let file_id = file.id.clone();
    let file_name = file.name.clone();
    let fallback_file = file.clone();
    view! {
        <button
            type="button"
            aria-label=move || format!("查看 {}", file_name)
            aria-current=move || {
                if selected
                    .get()
                    .is_some_and(|current| current.id == file_id)
                {
                    Some("true")
                } else {
                    None
                }
            }
            on:click=move |_| on_select.run(file.clone())
        >
            <img
                src=thumbnail_url(&file)
                alt=file.name.clone()
                loading="lazy"
                draggable="false"
                on:error=move |event: leptos::ev::ErrorEvent| thumbnail_fallback(event, &fallback_file)
            />
        </button>
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq)]
struct DragState {
    active: bool,
    start_x: f64,
    start_y: f64,
    dx: f64,
    dy: f64,
    moved: bool,
    pinched: bool,
}

fn menu_icon(icon: MenuIcon) -> AnyView {
    match icon {
        MenuIcon::More => icons::more_horizontal().into_any(),
        MenuIcon::Captions => icons::captions().into_any(),
        MenuIcon::Settings => icons::settings_2().into_any(),
        MenuIcon::Volume => icons::volume_2().into_any(),
    }
}

fn gallery_items(items: &[File]) -> Vec<File> {
    items
        .iter()
        .filter(|file| classify::is_image(file))
        .cloned()
        .collect()
}

fn gallery_position(selected: RwSignal<Option<File>>, items: RwSignal<Vec<File>>) -> usize {
    let gallery = gallery_items(&items.get());
    selected
        .get()
        .and_then(|selected| gallery.iter().position(|file| file.id == selected.id))
        .map_or(0, |index| index + 1)
}

fn change_gallery(
    selected: RwSignal<Option<File>>,
    items: RwSignal<Vec<File>>,
    direction: i32,
    on_change: Callback<File>,
) {
    let gallery = gallery_items(&items.get_untracked());
    if gallery.len() <= 1 {
        return;
    }
    let Some(current) = selected.get_untracked() else {
        return;
    };
    let Some(index) = gallery.iter().position(|file| file.id == current.id) else {
        return;
    };
    let next = (index as i32 + direction).rem_euclid(gallery.len() as i32) as usize;
    on_change.run(gallery[next].clone());
}

fn non_negative(value: i64) -> u64 {
    u64::try_from(value).unwrap_or(0)
}

fn thumbnail_url(file: &File) -> String {
    format!(
        "/api/files/{}/thumbnail?v={}",
        file.id,
        js_sys::encode_uri_component(&file.etag)
            .as_string()
            .unwrap_or_default()
    )
}

fn actual_zoom(natural: Size, stage: Size) -> f64 {
    let fitted = fit_image(natural, stage);
    if fitted.width > 0.0 {
        natural.width / fitted.width
    } else {
        1.0
    }
}

fn max_zoom(natural: Size, stage: Size) -> f64 {
    8.0_f64.max(actual_zoom(natural, stage))
}

fn zoom_percent(natural: Size, stage: Size, zoom: f64) -> u64 {
    (zoom / actual_zoom(natural, stage) * 100.0)
        .round()
        .max(0.0) as u64
}

fn set_zoom(
    zoom: RwSignal<f64>,
    pan: RwSignal<Point>,
    natural: RwSignal<Size>,
    stage: RwSignal<Size>,
    value: f64,
    from: Point,
    to: Point,
) {
    let current = zoom.get_untracked().max(1.0);
    let next = value
        .max(1.0)
        .min(max_zoom(natural.get_untracked(), stage.get_untracked()));
    let ratio = next / current;
    pan.set(zoom_image_pan(pan.get_untracked(), from, to, ratio));
    zoom.set(next);
    clamp_pan(pan, natural, stage, zoom);
}

fn fit_zoom(zoom: RwSignal<f64>, pan: RwSignal<Point>) {
    zoom.set(1.0);
    pan.set(Point { x: 0.0, y: 0.0 });
}

fn clamp_pan(
    pan: RwSignal<Point>,
    natural: RwSignal<Size>,
    stage: RwSignal<Size>,
    zoom: RwSignal<f64>,
) {
    pan.set(clamp_image_pan(
        pan.get_untracked(),
        fit_image(natural.get_untracked(), stage.get_untracked()),
        stage.get_untracked(),
        zoom.get_untracked(),
    ));
}

fn update_stage_size(stage: NodeRef<leptos::html::Div>, size: RwSignal<Size>) {
    let Some(element) = stage.get() else {
        return;
    };
    let element = element.unchecked_into::<Element>();
    size.set(Size {
        width: f64::from(element.client_width().max(0)),
        height: f64::from(element.client_height().max(0)),
    });
}

fn image_style(
    natural: Size,
    stage: Size,
    pan: Point,
    zoom: f64,
    drag_x: f64,
    hidden: bool,
) -> String {
    let fitted = fit_image(natural, stage);
    let display = if hidden { "none" } else { "block" };
    // At fit zoom the reference applies the live horizontal drag offset while
    // the pointer is down. The offset is temporary: the completed gesture
    // either changes the gallery item or is cleared on pointer-up.
    let drag_x = if zoom <= 1.0 { drag_x } else { 0.0 };
    format!(
        "display:{display};width:{}px;height:{}px;transform:translate(-50%,-50%) translate3d({}px,{}px,0) scale({zoom});",
        fitted.width,
        fitted.height,
        pan.x + drag_x,
        pan.y
    )
}

fn local_point(stage: NodeRef<leptos::html::Div>, point: Point) -> Point {
    let Some(element) = stage.get() else {
        return point;
    };
    let element = element.unchecked_into::<Element>();
    let bounds = element.get_bounding_client_rect();
    Point {
        x: point.x - bounds.left() - bounds.width() / 2.0,
        y: point.y - bounds.top() - bounds.height() / 2.0,
    }
}

fn distance(a: Point, b: Point) -> f64 {
    (a.x - b.x).hypot(a.y - b.y)
}

fn midpoint(a: Point, b: Point) -> Point {
    Point {
        x: (a.x + b.x) / 2.0,
        y: (a.y + b.y) / 2.0,
    }
}

fn two_points(points: &HashMap<i32, Point>) -> Option<(Point, Point)> {
    let mut entries: Vec<_> = points.iter().collect();
    entries.sort_unstable_by_key(|(id, _)| **id);
    Some((*entries.first()?.1, *entries.get(1)?.1))
}

fn image_loaded(
    event: Event,
    natural: RwSignal<Size>,
    stage_size: RwSignal<Size>,
    loading: RwSignal<bool>,
    pan: RwSignal<Point>,
    zoom: RwSignal<f64>,
) {
    let Some(image) = event
        .target()
        .and_then(|target| target.dyn_into::<HtmlImageElement>().ok())
    else {
        return;
    };
    natural.set(Size {
        width: f64::from(image.natural_width()),
        height: f64::from(image.natural_height()),
    });
    loading.set(false);
    clamp_pan(pan, natural, stage_size, zoom);
}

fn toggle_image_zoom(
    zoom: RwSignal<f64>,
    pan: RwSignal<Point>,
    natural: RwSignal<Size>,
    stage: RwSignal<Size>,
    click_timer: RwSignal<Option<i32>>,
) {
    if let Some(timer) = click_timer.get_untracked()
        && let Some(window) = web_sys::window()
    {
        window.clear_timeout_with_handle(timer);
        click_timer.set(None);
    }
    let current = zoom.get_untracked();
    let actual = actual_zoom(natural.get_untracked(), stage.get_untracked());
    set_zoom(
        zoom,
        pan,
        natural,
        stage,
        if (current - actual).abs() < 0.01 {
            1.0
        } else {
            actual
        },
        Point { x: 0.0, y: 0.0 },
        Point { x: 0.0, y: 0.0 },
    );
}

fn thumbnail_fallback(event: leptos::ev::ErrorEvent, file: &File) {
    let Some(image) = event
        .target()
        .and_then(|target| target.dyn_into::<HtmlImageElement>().ok())
    else {
        return;
    };
    let source = format!("/api/files/{}/preview", file.id);
    let absolute = web_sys::window()
        .map(|window| window.location().origin())
        .and_then(|origin| origin.ok())
        .filter(|origin| !origin.is_empty())
        .map(|origin| format!("{origin}{source}"))
        .unwrap_or_else(|| source.clone());
    if image.src() != absolute {
        image.set_src(&absolute);
    }
}

fn reveal_thumbnail(root: NodeRef<leptos::html::Section>) {
    let Some(window) = web_sys::window() else {
        return;
    };
    let callback = Closure::once_into_js(move || {
        let Some(root) = root.get() else {
            return;
        };
        let Ok(Some(element)) = root.query_selector(".preview-filmstrip [aria-current=\"true\"]")
        else {
            return;
        };
        let options = web_sys::ScrollIntoViewOptions::new();
        options.set_block(web_sys::ScrollLogicalPosition::Nearest);
        options.set_inline(web_sys::ScrollLogicalPosition::Center);
        element.scroll_into_view_with_scroll_into_view_options(&options);
    });
    let _ =
        window.set_timeout_with_callback_and_timeout_and_arguments_0(callback.unchecked_ref(), 0);
}

fn preload_adjacent(selected: &File, items: &[File]) {
    let gallery = gallery_items(items);
    if gallery.len() <= 1 {
        return;
    }
    let Some(index) = gallery.iter().position(|file| file.id == selected.id) else {
        return;
    };
    for offset in [-1_i32, 1] {
        let index = (index as i32 + offset).rem_euclid(gallery.len() as i32) as usize;
        if let Ok(image) = HtmlImageElement::new() {
            image.set_src(&format!("/api/files/{}/preview", gallery[index].id));
        }
    }
}

fn trap_focus(root: NodeRef<leptos::html::Section>, event: &KeyboardEvent) {
    let Some(root) = root.get() else {
        return;
    };
    let scope = root
        .query_selector("[data-preview-sheet]")
        .ok()
        .flatten()
        .filter(|sheet| {
            web_sys::window()
                .and_then(|window| window.get_computed_style(sheet).ok().flatten())
                .is_some_and(|style| {
                    style.get_property_value("position").unwrap_or_default() != "static"
                })
        })
        .unwrap_or_else(|| root.clone().unchecked_into());
    let Ok(nodes) = scope.query_selector_all(
        "button:not([disabled]), summary, input:not([disabled]), select:not([disabled]), [tabindex=\"0\"]"
    ) else {
        return;
    };
    let mut focusable = Vec::new();
    for index in 0..nodes.length() {
        let Some(node) = nodes.item(index) else {
            continue;
        };
        let Ok(element) = node.dyn_into::<web_sys::HtmlElement>() else {
            continue;
        };
        let element_node: Element = element.clone().unchecked_into();
        let visible = element_node.get_client_rects().length() > 0
            && element_node
                .closest("[inert], [aria-hidden=\"true\"]")
                .ok()
                .flatten()
                .is_none()
            && web_sys::window()
                .and_then(|window| window.get_computed_style(&element_node).ok().flatten())
                .is_none_or(|style| {
                    style.get_property_value("visibility").unwrap_or_default() != "hidden"
                });
        if visible {
            focusable.push(element);
        }
    }
    let Some(first) = focusable.first() else {
        event.prevent_default();
        let _ = root.focus();
        return;
    };
    let Some(last) = focusable.last() else {
        return;
    };
    let active = web_sys::window()
        .and_then(|window| window.document())
        .and_then(|document| document.active_element());
    let first_element = first.clone().unchecked_into::<Element>();
    let last_element = last.clone().unchecked_into::<Element>();
    let active_is_first = active
        .as_ref()
        .is_some_and(|active| active.is_same_node(Some(&first_element)));
    let active_is_last = active
        .as_ref()
        .is_some_and(|active| active.is_same_node(Some(&last_element)));
    let active_in_scope = active
        .as_ref()
        .is_some_and(|active| scope.contains(Some(active.unchecked_ref::<web_sys::Node>())));
    let root_element: Element = root.clone().unchecked_into();
    let active_is_root = active
        .as_ref()
        .is_some_and(|active| active.is_same_node(Some(&root_element)));
    if (event.shift_key() && (active_is_first || active_is_root || !active_in_scope))
        || (!event.shift_key() && (active_is_last || active_is_root || !active_in_scope))
    {
        event.prevent_default();
        let _ = if event.shift_key() {
            last.focus()
        } else {
            first.focus()
        };
    }
}

//! EPUB/TXT reading view.
//!
//! The server produces sanitized flow chunks and stable reading anchors. This
//! module owns the browser-specific part of the reader: a bounded DOM window,
//! native CSS columns, page navigation, TOC targeting, preference persistence,
//! focus lifecycle and durable progress writes. Pagination rules that do not
//! need a browser live in crate::logic::reader.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet, VecDeque};
use std::rc::Rc;

use futures_channel::oneshot;
use futures_util::{StreamExt, stream};
use leptos::ev::{Event, MouseEvent, PointerEvent};
use leptos::prelude::*;
use revaro_core::api::book::SaveProgressRequest;
use revaro_core::classify;
use revaro_core::model::File;
use revaro_core::reader::{Anchor, FlowManifest, TocTarget};
use wasm_bindgen::JsCast;
use wasm_bindgen::JsValue;
use wasm_bindgen::closure::Closure;
use web_sys::{DomRect, Element, EventTarget, HtmlElement, HtmlInputElement, Node};

use crate::api;
use crate::browser;
use crate::logic::reader::{
    DEFAULT_FONT_SIZE, DEFAULT_LINE_HEIGHT, FONT_MAX, FONT_MIN, LINE_HEIGHTS, chunk_prefix,
    clamp_font_size, compute_margins, same_layout, spine_for_block, stable_window_range,
    toc_active_index, total_blocks, valid_line_height, validate_manifest,
};

use super::icons;
use super::reader_cache;

const AHEAD_MARGIN: i32 = 3;
const L1_CAPACITY: usize = 24;
const ANIMATION_MS: i32 = 260;
const PROGRESS_DELAY_MS: i32 = 1_200;
const WINDOW_SYNC_DELAY_MS: i32 = 200;
const RELAYOUT_DELAY_MS: i32 = 120;
const MAX_TEXT_WALK_DEPTH: usize = 64;

type DivRef = NodeRef<leptos::html::Div>;
type SectionRef = NodeRef<leptos::html::Section>;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ReaderStage {
    Loading,
    Reading,
    Error,
}

#[derive(Debug, Clone, Copy, PartialEq)]
struct ReaderPrefs {
    font_size: i32,
    line_height: f64,
    dark: bool,
}

#[derive(Debug, Clone, Copy, Default)]
struct ReaderMetrics {
    width: f64,
    height: f64,
    side: f64,
    top: f64,
    bottom: f64,
    pitch: f64,
    col_height: f64,
}

#[derive(Debug, Clone, Copy)]
struct SwipeState {
    pointer_id: i32,
    start_x: f64,
    start_y: f64,
    started_at: f64,
    dragging: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct ChunkLoadKey {
    generation: u64,
    index: i32,
}

#[derive(Debug, Default)]
struct ChunkCache {
    values: HashMap<i32, String>,
    order: VecDeque<i32>,
}

impl ChunkCache {
    fn get(&mut self, index: i32) -> Option<String> {
        let value = self.values.get(&index).cloned()?;
        self.touch(index);
        Some(value)
    }

    fn insert(&mut self, index: i32, value: String) {
        self.values.insert(index, value);
        self.touch(index);
        while self.order.len() > L1_CAPACITY {
            if let Some(oldest) = self.order.pop_front() {
                self.values.remove(&oldest);
            }
        }
    }

    fn touch(&mut self, index: i32) {
        self.order.retain(|value| *value != index);
        self.order.push_back(index);
    }

    fn clear(&mut self) {
        self.values.clear();
        self.order.clear();
    }
}

// DOM/window state is kept outside Leptos signals because pagination mutates
// styles and child nodes in place. The signals contain reader chrome state.
struct ReaderRuntime {
    manifest: Option<FlowManifest>,
    cache: ChunkCache,
    generation: u64,
    in_flight: HashSet<ChunkLoadKey>,
    waiters: HashMap<ChunkLoadKey, Vec<oneshot::Sender<Result<String, String>>>>,
    first_chunk: i32,
    last_chunk: i32,
    cols: i32,
    current_col: i32,
    metrics: ReaderMetrics,
    top_anchor: Option<Anchor>,
    pending_turns: i32,
    turn_busy: bool,
    syncing: bool,
    nav_depth: u32,
    closing: bool,
    progress_timer: Option<i32>,
    sync_timer: Option<i32>,
    relayout_timer: Option<i32>,
    resize_timer: Option<i32>,
}

impl Default for ReaderRuntime {
    fn default() -> Self {
        Self {
            manifest: None,
            cache: ChunkCache::default(),
            generation: 0,
            in_flight: HashSet::new(),
            waiters: HashMap::new(),
            first_chunk: 0,
            last_chunk: -1,
            cols: 1,
            current_col: 0,
            metrics: ReaderMetrics::default(),
            top_anchor: None,
            pending_turns: 0,
            turn_busy: false,
            syncing: false,
            nav_depth: 0,
            closing: false,
            progress_timer: None,
            sync_timer: None,
            relayout_timer: None,
            resize_timer: None,
        }
    }
}

struct NavGuard(Rc<RefCell<ReaderRuntime>>);

impl Drop for NavGuard {
    fn drop(&mut self) {
        let mut state = self.0.borrow_mut();
        state.nav_depth = state.nav_depth.saturating_sub(1);
    }
}

/// The authenticated reader overlay.
#[component]
pub fn ReaderView(
    file: File,
    on_close: Callback<()>,
    on_unauthorized: Callback<()>,
) -> impl IntoView {
    let root = SectionRef::new();
    let viewport = DivRef::new();
    let flow = DivRef::new();
    let runtime = Rc::new(RefCell::new(ReaderRuntime::default()));

    let stage = RwSignal::new(ReaderStage::Loading);
    let loading_text = RwSignal::new("正在读取书籍…".to_owned());
    let error_text = RwSignal::new(String::new());
    let title = RwSignal::new(classify::reader_display_title(&file.name));
    let manifest = RwSignal::new(None::<FlowManifest>);
    let toc_open = RwSignal::new(false);
    let font_open = RwSignal::new(false);
    let tools_visible = RwSignal::new(true);
    let toc_active = RwSignal::new(-1_i32);
    let percent = RwSignal::new(0.0_f64);
    let prefs = RwSignal::new(load_prefs());

    let previous_focus = web_sys::window()
        .and_then(|window| window.document())
        .and_then(|document| document.active_element());
    let document = web_sys::window().and_then(|window| window.document());
    let body = document.as_ref().and_then(|value| value.body());
    let previous_overflow = body.as_ref().map(|value| {
        value
            .style()
            .get_property_value("overflow")
            .unwrap_or_default()
    });
    if let Some(body) = &body {
        let _ = body.style().set_property("overflow", "hidden");
    }

    replace_reader_url(&file.id);
    let previous_title = document
        .as_ref()
        .map(|value| value.title())
        .unwrap_or_else(|| "revaro · 私人网盘".to_owned());
    if let Some(document) = &document {
        document.set_title(&format!("{} · revaro", file.name));
    }

    let file_id = file.id.clone();
    let close_view = {
        let on_close = on_close.clone();
        move |_| on_close.run(())
    };

    let mut key_listener = {
        let runtime = runtime.clone();
        let viewport = viewport;
        let flow = flow;
        let file_id = file_id.clone();
        let stage = stage;
        let toc_open = toc_open;
        let font_open = font_open;
        let close_view = on_close.clone();
        browser::on_keydown(move |event| {
            if event.default_prevented() {
                return;
            }
            if let Some(target) = event
                .target()
                .and_then(|value| value.dyn_into::<Element>().ok())
                && let Ok(Some(control)) =
                    target.closest("input, select, textarea, summary, button:not(.page-zone)")
                && event.key() != "Escape"
            {
                let _ = control;
                return;
            }
            if event.key() == "Escape" {
                if toc_open.get_untracked() {
                    event.prevent_default();
                    toc_open.set(false);
                    focus_element_by_id("toc-button");
                } else if font_open.get_untracked() {
                    event.prevent_default();
                    font_open.set(false);
                    focus_element_by_id("font-button");
                } else {
                    event.prevent_default();
                    close_view.run(());
                }
                return;
            }
            if stage.get_untracked() != ReaderStage::Reading {
                return;
            }
            let direction = match event.key().as_str() {
                "ArrowLeft" | "PageUp" => Some(-1),
                "ArrowRight" | "PageDown" | " " => Some(1),
                _ => None,
            };
            if let Some(direction) = direction {
                event.prevent_default();
                spawn_turn(
                    runtime.clone(),
                    viewport,
                    flow,
                    file_id.clone(),
                    stage,
                    toc_active,
                    percent,
                    direction,
                );
            }
        })
    };

    let mut resize_listener = {
        let runtime = runtime.clone();
        let viewport = viewport;
        let flow = flow;
        let manifest = manifest;
        let prefs = prefs;
        let stage = stage;
        browser::on_resize(move |_| {
            if manifest.get_untracked().is_none() || stage.get_untracked() != ReaderStage::Reading {
                return;
            }
            schedule_relayout(runtime.clone(), viewport, flow, manifest, prefs, stage);
        })
    };

    let lifecycle_listeners = install_progress_listeners(runtime.clone(), file_id.clone());

    let open_runtime = runtime.clone();
    let open_file_id = file_id.clone();
    let open_title = title;
    let open_manifest = manifest;
    let open_stage = stage;
    let open_loading_text = loading_text;
    let open_error_text = error_text;
    let open_on_unauthorized = on_unauthorized.clone();
    let open_prefs = prefs;
    leptos::task::spawn_local(async move {
        open_reader(
            open_runtime,
            open_file_id,
            open_title,
            open_manifest,
            open_stage,
            open_loading_text,
            open_error_text,
            open_on_unauthorized,
            viewport,
            flow,
            toc_active,
            percent,
            open_prefs,
        )
        .await;
    });

    if let Some(window) = web_sys::window() {
        let focus_root = root;
        let callback = Closure::once_into_js(move || {
            if let Some(root) = focus_root.get() {
                let options = web_sys::FocusOptions::new();
                options.set_prevent_scroll(true);
                let _ = root.focus_with_options(&options);
            }
        });
        let _ = window
            .set_timeout_with_callback_and_timeout_and_arguments_0(callback.unchecked_ref(), 0);
    }

    let cleanup_runtime = leptos::__reexports::send_wrapper::SendWrapper::new(runtime.clone());
    let cleanup_lifecycle =
        leptos::__reexports::send_wrapper::SendWrapper::new(lifecycle_listeners);
    let cleanup_file_id = file.id.clone();
    let cleanup_previous_title = previous_title.clone();
    on_cleanup(move || {
        let cleanup_runtime = cleanup_runtime.take();
        let cleanup_lifecycle = cleanup_lifecycle.take();
        flush_progress(cleanup_runtime.clone(), cleanup_file_id.clone());
        let mut runtime = cleanup_runtime.borrow_mut();
        runtime.closing = true;
        clear_runtime_timers(&mut runtime);
        drop(runtime);
        key_listener.release();
        resize_listener.release();
        drop(cleanup_lifecycle);
        if let Some(body) = body {
            if let Some(previous) = previous_overflow {
                let _ = body.style().set_property("overflow", &previous);
            } else {
                let _ = body.style().remove_property("overflow");
            }
        }
        if let Some(document) = web_sys::window().and_then(|window| window.document()) {
            document.set_title(&cleanup_previous_title);
        }
        if let Some(element) = previous_focus
            && let Ok(element) = element.dyn_into::<HtmlElement>()
        {
            if element.is_connected() {
                let options = web_sys::FocusOptions::new();
                options.set_prevent_scroll(true);
                let _ = element.focus_with_options(&options);
            }
        }
    });

    let close_toc = {
        let toc_open = toc_open;
        move |_| {
            toc_open.set(false);
        }
    };
    let open_toc = {
        let toc_open = toc_open;
        let font_open = font_open;
        move |_| {
            font_open.set(false);
            toc_open.set(true);
            focus_element_after_render("toc-close");
        }
    };
    let toggle_font = {
        let font_open = font_open;
        move |_| font_open.update(|value| *value = !*value)
    };
    let toggle_tools = {
        let tools_visible = tools_visible;
        let font_open = font_open;
        move |_| {
            tools_visible.update(|value| *value = !*value);
            if !tools_visible.get_untracked() {
                font_open.set(false);
            }
        }
    };
    let trap_keydown = {
        let root = root;
        move |event: web_sys::KeyboardEvent| {
            if event.key() == "Tab" {
                event.prevent_default();
                trap_focus(root, &event);
            }
        }
    };
    let toggle_theme = {
        let prefs = prefs;
        move |_| {
            prefs.update(|value| value.dark = !value.dark);
            save_prefs(prefs.get_untracked());
        }
    };
    let smaller_font =
        make_font_adjuster(runtime.clone(), viewport, flow, manifest, prefs, stage, -1);
    let larger_font =
        make_font_adjuster(runtime.clone(), viewport, flow, manifest, prefs, stage, 1);
    let font_input = {
        let runtime = runtime.clone();
        move |event: Event| {
            let Some(input) = event_target(&event) else {
                return;
            };
            let Ok(value) = input.value().parse::<i32>() else {
                return;
            };
            prefs.update(|prefs| prefs.font_size = clamp_font_size(value));
            save_prefs(prefs.get_untracked());
            schedule_relayout(runtime.clone(), viewport, flow, manifest, prefs, stage);
        }
    };
    let set_line_height = {
        let runtime = runtime.clone();
        move |value: f64| {
            prefs.update(|prefs| prefs.line_height = valid_line_height(value));
            save_prefs(prefs.get_untracked());
            schedule_relayout(runtime.clone(), viewport, flow, manifest, prefs, stage);
        }
    };
    let swipe = RwSignal::new(None::<SwipeState>);
    let suppress_zone_until = RwSignal::new(0.0_f64);
    let swipe_runtime = leptos::__reexports::send_wrapper::SendWrapper::new(runtime.clone());
    let on_pointer_down = {
        let swipe = swipe;
        let viewport = viewport;
        let stage = stage;
        let runtime = swipe_runtime.clone();
        move |event: PointerEvent| {
            if event.pointer_type() != "touch" && event.pointer_type() != "pen" {
                return;
            }
            if event.button() != 0 || stage.get_untracked() != ReaderStage::Reading {
                return;
            }
            if event
                .target()
                .and_then(|target| target.dyn_into::<Element>().ok())
                .and_then(|target| {
                    target
                        .closest("button:not(.page-zone), input, select, textarea")
                        .ok()
                })
                .flatten()
                .is_some()
            {
                return;
            }
            let state = runtime.borrow();
            if state.turn_busy || state.syncing {
                return;
            }
            swipe.set(Some(SwipeState {
                pointer_id: event.pointer_id(),
                start_x: f64::from(event.client_x()),
                start_y: f64::from(event.client_y()),
                started_at: js_sys::Date::now(),
                dragging: false,
            }));
            if let Some(viewport) = viewport
                .get()
                .map(|value| value.unchecked_into::<Element>())
            {
                let _ = viewport.set_pointer_capture(event.pointer_id());
            }
        }
    };
    let on_pointer_move = {
        let swipe = swipe;
        let flow = flow;
        let runtime = swipe_runtime.clone();
        move |event: PointerEvent| {
            let Some(mut state) = swipe.get_untracked() else {
                return;
            };
            if state.pointer_id != event.pointer_id() {
                return;
            }
            let dx = f64::from(event.client_x()) - state.start_x;
            let dy = f64::from(event.client_y()) - state.start_y;
            if !state.dragging {
                if dx.abs() < 8.0 {
                    return;
                }
                if dx.abs() < dy.abs() * 1.2 {
                    swipe.set(None);
                    return;
                }
                state.dragging = true;
                suppress_zone_until.set(js_sys::Date::now() + 600.0);
                set_promote(flow, true);
            }
            let (current, cols) = {
                let state = runtime.borrow();
                (state.current_col, state.cols)
            };
            let at_edge = (current == 0 && dx > 0.0) || (current >= cols - 1 && dx < 0.0);
            set_x_offset(&runtime, flow, if at_edge { dx / 3.0 } else { dx });
            swipe.set(Some(state));
            event.prevent_default();
        }
    };
    let on_pointer_end = {
        let swipe = swipe;
        let runtime = swipe_runtime.clone();
        let viewport = viewport;
        let flow = flow;
        let file_id = file.id.clone();
        let stage = stage;
        let toc_active = toc_active;
        let percent = percent;
        move |event: PointerEvent| {
            let Some(state) = swipe.get_untracked() else {
                return;
            };
            if state.pointer_id != event.pointer_id() {
                return;
            }
            swipe.set(None);
            if let Some(viewport) = viewport
                .get()
                .map(|value| value.unchecked_into::<Element>())
            {
                let _ = viewport.release_pointer_capture(event.pointer_id());
            }
            if event.type_() == "pointercancel" {
                set_promote(flow, false);
                set_x(&runtime, flow, false);
                return;
            }
            let dx = f64::from(event.client_x()) - state.start_x;
            let dy = f64::from(event.client_y()) - state.start_y;
            if !state.dragging && (dx.abs() < 45.0 || dx.abs() < dy.abs() * 1.5) {
                return;
            }
            if !state.dragging {
                suppress_zone_until.set(js_sys::Date::now() + 500.0);
            }
            let (current, cols, busy, pitch) = {
                let state = runtime.borrow();
                (
                    state.current_col,
                    state.cols,
                    state.turn_busy || state.syncing,
                    state.metrics.pitch,
                )
            };
            let flick = js_sys::Date::now() - state.started_at < 300.0 && dx.abs() > 30.0;
            if !flick && dx.abs() <= pitch * 0.25 {
                set_promote(flow, false);
                if busy {
                    set_x(&runtime, flow, false);
                    return;
                }
                let runtime = (*runtime).clone();
                spawn_snap_to_column(
                    runtime,
                    viewport,
                    flow,
                    file_id.clone(),
                    stage,
                    toc_active,
                    percent,
                    current,
                );
                return;
            }
            set_promote(flow, false);
            set_x(&runtime, flow, false);
            if busy {
                return;
            }
            let direction = if dx < 0.0 { 1 } else { -1 };
            let target = current + direction;
            if target < 0 || target >= cols {
                let can_extend = {
                    let state = runtime.borrow();
                    if direction > 0 {
                        state.manifest.as_ref().is_some_and(|manifest| {
                            state.last_chunk < manifest.chunks.len() as i32 - 1
                        })
                    } else {
                        state.first_chunk > 0
                    }
                };
                if !can_extend {
                    let runtime = (*runtime).clone();
                    spawn_snap_to_column(
                        runtime,
                        viewport,
                        flow,
                        file_id.clone(),
                        stage,
                        toc_active,
                        percent,
                        current,
                    );
                    return;
                }
            }
            spawn_turn(
                (*runtime).clone(),
                viewport,
                flow,
                file_id.clone(),
                stage,
                toc_active,
                percent,
                direction,
            );
        }
    };
    let previous = {
        let runtime = runtime.clone();
        let file_id = file.id.clone();
        let suppress_zone_until = suppress_zone_until;
        move |event: MouseEvent| {
            if js_sys::Date::now() < suppress_zone_until.get_untracked() {
                event.prevent_default();
                return;
            }
            spawn_turn(
                runtime.clone(),
                viewport,
                flow,
                file_id.clone(),
                stage,
                toc_active,
                percent,
                -1,
            );
        }
    };
    let next = {
        let runtime = runtime.clone();
        let file_id = file.id.clone();
        let suppress_zone_until = suppress_zone_until;
        move |event: MouseEvent| {
            if js_sys::Date::now() < suppress_zone_until.get_untracked() {
                event.prevent_default();
                return;
            }
            spawn_turn(
                runtime.clone(),
                viewport,
                flow,
                file_id.clone(),
                stage,
                toc_active,
                percent,
                1,
            );
        }
    };
    let jump_toc = leptos::__reexports::send_wrapper::SendWrapper::new({
        let runtime = runtime.clone();
        let file_id = file.id.clone();
        move |index: usize| {
            let Some(entry) = manifest
                .get_untracked()
                .and_then(|value| value.toc.get(index).cloned())
            else {
                return;
            };
            toc_open.set(false);
            spawn_toc_jump(
                runtime.clone(),
                viewport,
                flow,
                file_id.clone(),
                stage,
                toc_active,
                percent,
                entry,
            );
        }
    });

    view! {
        <section
            node_ref=root
            id="reader-view"
            class="reader-shell"
            class:dark=move || prefs.get().dark
            class:tools-hidden=move || !tools_visible.get()
            role="dialog"
            aria-modal="true"
            aria-label=move || title.get()
            tabindex="-1"
            on:keydown=trap_keydown
        >
            <header class="reader-bar">
                <button id="reader-back" class="reader-icon-btn" type="button" aria-label="返回" on:click=close_view.clone()>
                    {icons::chevron_left()}
                </button>
                <div class="reader-bar-title"><strong id="reader-title">{move || title.get()}</strong></div>
                <span
                    id="page-label"
                    class="reader-progress-ring"
                    role="img"
                    aria-label=move || format!("阅读进度 {}", progress_label(percent.get()))
                >
                    <svg viewBox="0 0 40 40" aria-hidden="true">
                        <circle class="reader-progress-ring-track" cx="20" cy="20" r="17.5" pathLength="100"></circle>
                        <circle
                            class="reader-progress-ring-value"
                            class:empty=move || percent.get() <= 0.0
                            cx="20"
                            cy="20"
                            r="17.5"
                            pathLength="100"
                            stroke-dasharray="100"
                            stroke-dashoffset=move || format!("{}", 100.0 - percent.get().clamp(0.0, 100.0))
                        ></circle>
                    </svg>
                    <b>{move || format!("{}", percent.get().round().clamp(0.0, 100.0))}</b>
                </span>
            </header>

            <div
                node_ref=viewport
                id="viewport"
                class="reader-viewport rf-viewport"
                on:pointerdown=on_pointer_down
                on:pointermove=on_pointer_move
                on:pointerup=on_pointer_end.clone()
                on:pointercancel=on_pointer_end
            >
                <div class="rf-pager">
                    <div node_ref=flow id="flow" class="rf-flow revaro-content"></div>
                </div>
                <Show when=move || stage.get() == ReaderStage::Loading fallback=|| ()>
                    <div id="loading" class="reader-loading">{move || loading_text.get()}</div>
                </Show>
                <Show when=move || stage.get() == ReaderStage::Error fallback=|| ()>
                    <div class="reader-loading">
                        {move || error_text.get()}
                        {" "}
                        <button class="font-step" type="button" on:click=close_view.clone()>"关闭"</button>
                    </div>
                </Show>
                <button id="prev-zone" class="page-zone prev-zone" type="button" aria-label="上一页" on:click=previous></button>
                <button id="center-zone" class="page-zone center-zone" type="button" aria-label="显示或隐藏工具栏" on:click=toggle_tools></button>
                <button id="next-zone" class="page-zone next-zone" type="button" aria-label="下一页" on:click=next></button>
            </div>

            <div id="toc-scrim" class="toc-scrim" class:hidden=move || !toc_open.get() on:click=close_toc></div>
            <aside
                id="toc-drawer"
                class="toc-drawer"
                class:open=move || toc_open.get()
                data-preview-sheet=move || toc_open.get().then_some("")
                aria-label="书籍目录"
                aria-hidden=move || if toc_open.get() { "false" } else { "true" }
            >
                <div class="toc-heading">
                    <h2>"目录"</h2>
                    <button id="toc-close" type="button" aria-label="关闭目录" on:click=close_toc.clone()>
                        {icons::x()}
                    </button>
                </div>
                <nav id="toc-list" class="toc-list">
                    {move || {
                        let entries = manifest.get().map_or_else(Vec::new, |value| value.toc);
                        if entries.is_empty() {
                            view! { <p class="toc-empty">"这本书没有可用目录。"</p> }.into_any()
                        } else {
                            entries
                                .into_iter()
                                .enumerate()
                                .map(|(index, entry)| {
                                    let label = entry.label.clone();
                                    let indent = entry.depth.clamp(0, 4) * 16;
                                    let jump = jump_toc.clone();
                                    view! {
                                        <button
                                            class="toc-item"
                                            class:active=move || toc_active.get() == index as i32
                                            style=format!("--toc-indent: {indent}px")
                                            type="button"
                                            on:click=move |_| jump(index)
                                        >
                                            {label}
                                        </button>
                                    }
                                })
                                .collect_view()
                                .into_any()
                        }
                    }}
                </nav>
            </aside>

            <div id="font-popover" class="font-popover" class:hidden=move || !font_open.get() aria-label="排版设置">
                <div class="reader-setting-row">
                    <span>"字号"</span>
                    <button id="font-smaller" class="font-step" type="button" aria-label="减小字号" on:click=smaller_font>
                        "A−"
                    </button>
                    <input
                        id="font-slider"
                        type="range"
                        min=FONT_MIN
                        max=FONT_MAX
                        step="1"
                        aria-label="阅读字号"
                        prop:value=move || prefs.get().font_size.to_string()
                        on:input=font_input
                    />
                    <button id="font-larger" class="font-step" type="button" aria-label="增大字号" on:click=larger_font>
                        "A+"
                    </button>
                </div>
                <div class="reader-setting-row v2-lineheight">
                    <span>"行距"</span>
                    {LINE_HEIGHTS
                        .into_iter()
                        .map(|value| {
                            let set_line_height = set_line_height.clone();
                            view! {
                                <button
                                    class="font-step"
                                    class:v2-active=move || (prefs.get().line_height - value).abs() < f64::EPSILON
                                    type="button"
                                    on:click=move |_| set_line_height(value)
                                >
                                    {line_height_label(value)}
                                </button>
                            }
                        })
                        .collect_view()}
                </div>
            </div>

            <footer class="reader-footer">
                <div class="reader-actions">
                    <button id="toc-button" class="reader-action-btn" type="button" aria-expanded=move || if toc_open.get() { "true" } else { "false" } on:click=open_toc>
                        {icons::list()}<span>"目录"</span>
                    </button>
                    <button id="font-button" class="reader-action-btn" type="button" aria-expanded=move || if font_open.get() { "true" } else { "false" } on:click=toggle_font>
                        {icons::type_icon()}<span>"排版"</span>
                    </button>
                    <button id="theme-button" class="reader-action-btn" type="button" aria-label=move || if prefs.get().dark { "切换浅色" } else { "切换深色" } on:click=toggle_theme>
                        {icons::sun_moon()}<span>"明暗"</span>
                    </button>
                </div>
            </footer>
        </section>
    }
}

fn load_prefs() -> ReaderPrefs {
    let fallback = ReaderPrefs {
        font_size: DEFAULT_FONT_SIZE,
        line_height: DEFAULT_LINE_HEIGHT,
        dark: false,
    };
    let Some(raw) = browser::local_storage_get("revaro-reader-prefs") else {
        return fallback;
    };
    let Ok(value) = serde_json::from_str::<serde_json::Value>(&raw) else {
        return fallback;
    };
    let font_size = value
        .get("fontSize")
        .and_then(serde_json::Value::as_i64)
        .and_then(|value| i32::try_from(value).ok())
        .map_or(fallback.font_size, clamp_font_size);
    let line_height = value
        .get("lineHeight")
        .and_then(serde_json::Value::as_f64)
        .map_or(fallback.line_height, valid_line_height);
    ReaderPrefs {
        font_size,
        line_height,
        dark: value.get("theme").and_then(serde_json::Value::as_str) == Some("dark"),
    }
}

fn save_prefs(prefs: ReaderPrefs) {
    let value = serde_json::json!({
        "fontSize": clamp_font_size(prefs.font_size),
        "lineHeight": valid_line_height(prefs.line_height),
        "theme": if prefs.dark { "dark" } else { "light" },
    });
    browser::local_storage_set("revaro-reader-prefs", &value.to_string());
}

fn event_target(event: &Event) -> Option<HtmlInputElement> {
    event
        .target()
        .and_then(|target| target.dyn_into::<HtmlInputElement>().ok())
}

fn replace_reader_url(file_id: &str) {
    let Some(window) = web_sys::window() else {
        return;
    };
    let Ok(history) = window.history() else {
        return;
    };
    let url = format!("/read/{file_id}");
    let _ = history.replace_state_with_url(&JsValue::NULL, "", Some(&url));
}

fn focus_element_by_id(id: &str) {
    let Some(document) = web_sys::window().and_then(|window| window.document()) else {
        return;
    };
    if let Some(element) = document
        .get_element_by_id(id)
        .and_then(|element| element.dyn_into::<HtmlElement>().ok())
    {
        let _ = element.focus();
    }
}

fn focus_element_after_render(id: &str) {
    let Some(window) = web_sys::window() else {
        return;
    };
    let id = id.to_owned();
    let callback = Closure::once_into_js(move || {
        let Some(document) = web_sys::window().and_then(|window| window.document()) else {
            return;
        };
        let Some(element) = document
            .get_element_by_id(&id)
            .and_then(|element| element.dyn_into::<HtmlElement>().ok())
        else {
            return;
        };
        let options = web_sys::FocusOptions::new();
        options.set_prevent_scroll(true);
        let _ = element.focus_with_options(&options);
    });
    let _ =
        window.set_timeout_with_callback_and_timeout_and_arguments_0(callback.unchecked_ref(), 0);
}

fn clear_timer(timer: &mut Option<i32>) {
    if let Some(timer) = timer.take()
        && let Some(window) = web_sys::window()
    {
        window.clear_timeout_with_handle(timer);
    }
}

fn clear_runtime_timers(runtime: &mut ReaderRuntime) {
    clear_timer(&mut runtime.progress_timer);
    clear_timer(&mut runtime.sync_timer);
    clear_timer(&mut runtime.relayout_timer);
    clear_timer(&mut runtime.resize_timer);
}

struct DomListener {
    target: EventTarget,
    name: String,
    callback: Closure<dyn FnMut(web_sys::Event)>,
}

impl Drop for DomListener {
    fn drop(&mut self) {
        let _ = self.target.remove_event_listener_with_callback(
            &self.name,
            self.callback.as_ref().unchecked_ref(),
        );
    }
}

fn install_dom_listener(
    target: &EventTarget,
    name: &str,
    callback: impl FnMut(web_sys::Event) + 'static,
) -> Option<DomListener> {
    let callback = Closure::new(callback);
    target
        .add_event_listener_with_callback(name, callback.as_ref().unchecked_ref())
        .ok()?;
    Some(DomListener {
        target: target.clone(),
        name: name.to_owned(),
        callback,
    })
}

fn install_progress_listeners(
    runtime: Rc<RefCell<ReaderRuntime>>,
    file_id: String,
) -> Vec<DomListener> {
    let mut listeners = Vec::new();
    let Some(window) = web_sys::window() else {
        return listeners;
    };
    let window_target: EventTarget = window.clone().unchecked_into();
    for name in ["pagehide", "beforeunload", "blur"] {
        let state = runtime.clone();
        let id = file_id.clone();
        if let Some(listener) = install_dom_listener(&window_target, name, move |_| {
            flush_progress(state.clone(), id.clone())
        }) {
            listeners.push(listener);
        }
    }
    if let Some(document) = window.document() {
        let document_target: EventTarget = document.unchecked_into();
        let state = runtime;
        if let Some(listener) =
            install_dom_listener(&document_target, "visibilitychange", move |_| {
                flush_progress(state.clone(), file_id.clone());
            })
        {
            listeners.push(listener);
        }
    }
    listeners
}

async fn open_reader(
    runtime: Rc<RefCell<ReaderRuntime>>,
    file_id: String,
    title: RwSignal<String>,
    manifest_signal: RwSignal<Option<FlowManifest>>,
    stage: RwSignal<ReaderStage>,
    loading_text: RwSignal<String>,
    error_text: RwSignal<String>,
    on_unauthorized: Callback<()>,
    viewport: DivRef,
    flow: DivRef,
    toc_active: RwSignal<i32>,
    percent: RwSignal<f64>,
    prefs: RwSignal<ReaderPrefs>,
) {
    let (book_result, progress_result) = futures_util::join!(
        api::fetch_book(&file_id),
        api::fetch_book_progress(&file_id)
    );

    if let Err(error) = &book_result
        && error.is_unauthorized()
    {
        on_unauthorized.run(());
        return;
    }
    if let Err(error) = &progress_result
        && error.is_unauthorized()
    {
        on_unauthorized.run(());
        return;
    }
    if let Ok(book) = book_result {
        let display = if !book.name.is_empty() {
            book.name
        } else {
            book.title
        };
        if !display.is_empty() {
            title.set(classify::reader_display_title(&display));
        }
    }
    let saved = progress_result.ok().and_then(|value| value.anchor);

    let cached = reader_cache::load_manifest(&file_id);
    let mut rendered_from_cache = false;
    if let Some(cached_manifest) = cached.clone() {
        loading_text.set("正在读取缓存书页…".to_owned());
        if setup_view(
            runtime.clone(),
            file_id.clone(),
            cached_manifest,
            saved.clone(),
            manifest_signal,
            stage,
            loading_text,
            viewport,
            flow,
            toc_active,
            percent,
            prefs.get_untracked(),
        )
        .await
        .is_ok()
        {
            rendered_from_cache = true;
        } else {
            reset_view(&runtime, flow, manifest_signal);
        }
    }

    // Keep the network phase wording from the reference reader. The flow
    // endpoint is an implementation detail; users see the same book-loading
    // state while its manifest is being fetched.
    loading_text.set("正在读取书籍…".to_owned());
    let network = api::fetch_book_flow(&file_id).await;
    match network {
        Ok(network_manifest) => {
            if !validate_manifest(&network_manifest) {
                if !rendered_from_cache {
                    stage.set(ReaderStage::Error);
                    error_text.set("阅读流清单无效".to_owned());
                }
                return;
            }
            reader_cache::store_manifest(&file_id, &network_manifest);
            let same = rendered_from_cache
                && runtime
                    .borrow()
                    .manifest
                    .as_ref()
                    .is_some_and(|current| same_layout(current, &network_manifest));
            if same {
                return;
            }
            if rendered_from_cache {
                reset_view(&runtime, flow, manifest_signal);
            }
            loading_text.set("正在排版…".to_owned());
            if let Err(error) = setup_view(
                runtime,
                file_id,
                network_manifest,
                saved,
                manifest_signal,
                stage,
                loading_text,
                viewport,
                flow,
                toc_active,
                percent,
                prefs.get_untracked(),
            )
            .await
            {
                stage.set(ReaderStage::Error);
                error_text.set(error);
            }
        }
        Err(error) if error.is_unauthorized() => on_unauthorized.run(()),
        Err(error) if !rendered_from_cache => {
            stage.set(ReaderStage::Error);
            error_text.set(error.message);
        }
        Err(_) => {
            // A cached, authenticated view remains useful while offline. The
            // next open will validate it against the no-cache manifest again.
        }
    }
}

async fn setup_view(
    runtime: Rc<RefCell<ReaderRuntime>>,
    file_id: String,
    value: FlowManifest,
    saved: Option<Anchor>,
    manifest_signal: RwSignal<Option<FlowManifest>>,
    stage: RwSignal<ReaderStage>,
    loading_text: RwSignal<String>,
    viewport: DivRef,
    flow: DivRef,
    toc_active: RwSignal<i32>,
    percent: RwSignal<f64>,
    prefs: ReaderPrefs,
) -> Result<(), String> {
    if !validate_manifest(&value) {
        return Err("阅读流清单无效".to_owned());
    }
    let total = total_blocks(&value);
    let first_block = value.spines.first().map_or(0, |spine| spine.block_start);
    let anchor = saved
        .filter(|anchor| anchor.is_valid() && anchor.block >= 0 && anchor.block < total.max(1))
        .unwrap_or(Anchor {
            spine: 0,
            block: first_block,
            path: Vec::new(),
            offset: Anchor::BOUNDARY_OFFSET,
        });
    let metrics = apply_metrics(viewport, flow, &value.format, prefs);
    {
        let mut state = runtime.borrow_mut();
        state.generation = state.generation.wrapping_add(1);
        state.manifest = Some(value.clone());
        state.metrics = metrics;
        state.top_anchor = Some(anchor.clone());
        state.current_col = 0;
        state.cols = 1;
    }
    manifest_signal.set(Some(value.clone()));
    loading_text.set("正在加载书页…".to_owned());
    let (first, last) = stable_window_range(&value, anchor.block, AHEAD_MARGIN);
    ensure_window(&runtime, &file_id, &value, flow, first, last).await?;
    measure_cols(&runtime, flow);
    stage.set(ReaderStage::Reading);
    let col = col_for_anchor(&runtime, flow, &anchor).unwrap_or(0);
    {
        let mut state = runtime.borrow_mut();
        state.current_col = col.clamp(0, state.cols.saturating_sub(1));
    }
    set_x(&runtime, flow, false);
    if let Some(captured) = capture_top_anchor(&runtime, viewport, flow) {
        runtime.borrow_mut().top_anchor = Some(captured);
    }
    refresh_ui(&runtime, flow, toc_active, percent);
    schedule_progress_save(runtime.clone(), file_id.clone());
    prefetch_chunks(runtime, file_id, value, anchor.block);
    Ok(())
}

fn reset_view(
    runtime: &Rc<RefCell<ReaderRuntime>>,
    flow: DivRef,
    manifest_signal: RwSignal<Option<FlowManifest>>,
) {
    if let Some(flow) = flow_element(flow) {
        flow.set_inner_html("");
    }
    let mut state = runtime.borrow_mut();
    state.generation = state.generation.wrapping_add(1);
    state.manifest = None;
    state.cache.clear();
    state.in_flight.clear();
    state.waiters.clear();
    state.first_chunk = 0;
    state.last_chunk = -1;
    state.cols = 1;
    state.current_col = 0;
    state.top_anchor = None;
    manifest_signal.set(None);
}

async fn load_chunk(
    runtime: Rc<RefCell<ReaderRuntime>>,
    file_id: String,
    manifest: FlowManifest,
    index: i32,
) -> Result<String, String> {
    let (key, receiver) = {
        let mut state = runtime.borrow_mut();
        if let Some(value) = state.cache.get(index) {
            return Ok(value);
        }
        let key = ChunkLoadKey {
            generation: state.generation,
            index,
        };
        if state.in_flight.insert(key.clone()) {
            (key, None)
        } else {
            let (sender, receiver) = oneshot::channel();
            state.waiters.entry(key.clone()).or_default().push(sender);
            (key, Some(receiver))
        }
    };
    if let Some(receiver) = receiver {
        return receiver
            .await
            .unwrap_or_else(|_| Err("内容片段加载已取消".to_owned()));
    }

    let cache_key = reader_cache::chunk_cache_key(&file_id, &manifest, index);
    let result = if let Some(value) = reader_cache::get_chunk(&cache_key).await {
        Ok(value)
    } else {
        api::fetch_book_chunk(&file_id, index)
            .await
            .map_err(|error| error.message)
    };
    let (waiters, should_persist) = {
        let mut state = runtime.borrow_mut();
        let accepts_result = state
            .manifest
            .as_ref()
            .is_some_and(|current| same_layout(current, &manifest));
        let accepts_current = accepts_result && state.generation == key.generation;
        if let Ok(value) = &result
            && accepts_current
        {
            state.cache.insert(index, value.clone());
        }
        state.in_flight.remove(&key);
        (
            state.waiters.remove(&key).unwrap_or_default(),
            accepts_current,
        )
    };
    for waiter in waiters {
        let _ = waiter.send(result.clone());
    }
    if should_persist && result.is_ok() {
        if let Ok(value) = &result {
            let value = value.clone();
            let cache_key = cache_key.clone();
            leptos::task::spawn_local(async move {
                reader_cache::put_chunk(&cache_key, &value).await;
            });
        }
    }
    result
}

async fn ensure_window(
    runtime: &Rc<RefCell<ReaderRuntime>>,
    file_id: &str,
    manifest: &FlowManifest,
    flow: DivRef,
    first: i32,
    last: i32,
) -> Result<bool, String> {
    let Some(flow) = flow_element(flow) else {
        return Ok(false);
    };
    let generation = runtime.borrow().generation;
    if manifest.chunks.is_empty() {
        let mut state = runtime.borrow_mut();
        state.first_chunk = 0;
        state.last_chunk = -1;
        return Ok(false);
    }
    let final_chunk = manifest.chunks.len().saturating_sub(1) as i32;
    let first = first.clamp(0, final_chunk);
    let last = last.clamp(first, final_chunk);

    let nodes = flow
        .query_selector_all(".rf-chunk")
        .map_err(|_| "阅读流 DOM 不可用".to_owned())?;
    let mut existing = HashSet::new();
    for index in 0..nodes.length() {
        let Some(node) = nodes.item(index) else {
            continue;
        };
        let Ok(element) = node.dyn_into::<Element>() else {
            continue;
        };
        let Some(chunk) = element
            .get_attribute("data-chunk")
            .and_then(|value| value.parse::<i32>().ok())
        else {
            let _ = element
                .parent_node()
                .map(|parent| parent.remove_child(&element));
            continue;
        };
        if chunk < first || chunk > last {
            let _ = element
                .parent_node()
                .map(|parent| parent.remove_child(&element));
        } else {
            existing.insert(chunk);
        }
    }

    let missing: Vec<i32> = (first..=last)
        .filter(|index| !existing.contains(index))
        .collect();
    if missing.is_empty() {
        let mut state = runtime.borrow_mut();
        let changed = state.first_chunk != first || state.last_chunk != last;
        state.first_chunk = first;
        state.last_chunk = last;
        return Ok(changed);
    }

    let state = runtime.clone();
    let id = file_id.to_owned();
    let value = manifest.clone();
    let concurrency = missing.len().min(6).max(1);
    let loaded: Vec<(i32, Result<String, String>)> = stream::iter(missing.iter().copied())
        .map(|index| {
            let state = state.clone();
            let id = id.clone();
            let value = value.clone();
            async move { (index, load_chunk(state, id, value, index).await) }
        })
        .buffer_unordered(concurrency)
        .collect()
        .await;
    if runtime.borrow().generation != generation {
        return Ok(false);
    }
    let mut html = HashMap::new();
    for (index, result) in loaded {
        html.insert(index, result?);
    }

    for index in missing {
        let Some(value) = html.remove(&index) else {
            continue;
        };
        let Some(document) = web_sys::window().and_then(|window| window.document()) else {
            return Err("浏览器文档不可用".to_owned());
        };
        let element = document
            .create_element("div")
            .map_err(|_| "无法创建阅读流节点".to_owned())?;
        element.set_class_name("rf-chunk");
        element
            .set_attribute("data-chunk", &index.to_string())
            .map_err(|_| "无法标记阅读流节点".to_owned())?;
        // The server's reader sanitizer is the trust boundary for this HTML;
        // the wrapper is created locally so arbitrary response markup cannot
        // escape the flow structure.
        element.set_inner_html(&value);
        let reference = flow.query_selector_all(".rf-chunk").ok().and_then(|nodes| {
            (0..nodes.length()).find_map(|position| {
                let node = nodes.item(position)?;
                let element = node.clone().dyn_into::<Element>().ok()?;
                let chunk = element.get_attribute("data-chunk")?.parse::<i32>().ok()?;
                (chunk > index).then_some(node)
            })
        });
        let node: Node = element.clone().unchecked_into();
        if let Some(reference) = reference {
            let _ = flow.insert_before(&node, Some(&reference));
        } else {
            let _ = flow.append_child(&node);
        }
    }

    let mut state = runtime.borrow_mut();
    state.first_chunk = first;
    state.last_chunk = last;
    Ok(true)
}

fn prefetch_chunks(
    runtime: Rc<RefCell<ReaderRuntime>>,
    file_id: String,
    manifest: FlowManifest,
    block: i32,
) {
    let Some(center) = manifest.chunk_for_block(block) else {
        return;
    };
    for distance in 1..=2 {
        for index in [center + distance, center - distance] {
            if index < 0 || index >= manifest.chunks.len() as i32 {
                continue;
            }
            let runtime = runtime.clone();
            let file_id = file_id.clone();
            let manifest = manifest.clone();
            leptos::task::spawn_local(async move {
                let _ = load_chunk(runtime, file_id, manifest, index).await;
            });
        }
    }
}

fn flow_element(flow: DivRef) -> Option<Element> {
    flow.get().map(|value| value.unchecked_into::<Element>())
}

fn flow_html_element(flow: DivRef) -> Option<HtmlElement> {
    flow.get()
        .map(|value| value.unchecked_into::<HtmlElement>())
}

fn viewport_size(viewport: DivRef) -> (f64, f64) {
    if let Some(viewport) = viewport
        .get()
        .map(|value| value.unchecked_into::<HtmlElement>())
    {
        let width = f64::from(viewport.client_width().max(0));
        let height = f64::from(viewport.client_height().max(0));
        if width > 0.0 && height > 0.0 {
            return (width, height);
        }
    }
    let Some(window) = web_sys::window() else {
        return (0.0, 0.0);
    };
    let width = window
        .inner_width()
        .ok()
        .and_then(|value| value.as_f64())
        .unwrap_or(0.0)
        .max(0.0);
    let height = window
        .inner_height()
        .ok()
        .and_then(|value| value.as_f64())
        .unwrap_or(0.0)
        .max(0.0);
    (width, height)
}

fn apply_metrics(
    viewport: DivRef,
    flow: DivRef,
    format: &str,
    prefs: ReaderPrefs,
) -> ReaderMetrics {
    let (width, height) = viewport_size(viewport);
    let margins = compute_margins(width, height);
    let metrics = ReaderMetrics {
        width,
        height,
        side: f64::from(margins.side),
        top: f64::from(margins.top),
        bottom: f64::from(margins.bottom),
        pitch: width,
        col_height: (height - f64::from(margins.top) - f64::from(margins.bottom)).max(1.0),
    };
    if let Some(flow) = flow_html_element(flow) {
        let style = flow.style();
        let _ = style.set_property("width", &format!("{}px", metrics.width));
        let _ = style.set_property("height", &format!("{}px", metrics.height));
        let _ = style.set_property("box-sizing", "border-box");
        let _ = style.set_property(
            "padding",
            &format!("{}px {}px {}px", metrics.top, metrics.side, metrics.bottom),
        );
        let _ = style.set_property(
            "column-width",
            &format!("{}px", (metrics.width - 2.0 * metrics.side).max(1.0)),
        );
        let _ = style.set_property("column-gap", &format!("{}px", 2.0 * metrics.side));
        let _ = style.set_property("column-fill", "auto");
        let _ = style.set_property(
            "--revaro-font-family",
            r#""Noto Serif SC", "Songti SC", Georgia, "Times New Roman", "STSong", SimSun, serif"#,
        );
        let _ = style.set_property(
            "--revaro-font-size",
            &format!("{}px", clamp_font_size(prefs.font_size)),
        );
        let _ = style.set_property(
            "--revaro-line-height",
            &valid_line_height(prefs.line_height).to_string(),
        );
        let _ = style.set_property("--revaro-col-height", &format!("{}px", metrics.col_height));
        if format.eq_ignore_ascii_case("txt") {
            let _ = flow.class_list().add_1("txt");
        } else {
            let _ = flow.class_list().remove_1("txt");
        }
    }
    metrics
}

fn measure_cols(runtime: &Rc<RefCell<ReaderRuntime>>, flow: DivRef) {
    let Some(flow) = flow_html_element(flow) else {
        return;
    };
    let mut state = runtime.borrow_mut();
    if state.metrics.pitch <= 0.0 {
        state.cols = 1;
        return;
    }
    let total = f64::from(flow.scroll_width()).max(state.metrics.pitch);
    state.cols = (total / state.metrics.pitch).round().max(1.0) as i32;
}

fn set_x(runtime: &Rc<RefCell<ReaderRuntime>>, flow: DivRef, animated: bool) {
    let Some(flow) = flow_html_element(flow) else {
        return;
    };
    let state = runtime.borrow();
    let x = -f64::from(state.current_col) * state.metrics.pitch;
    let style = flow.style();
    let _ = style.set_property(
        "transition",
        if animated {
            "transform 260ms cubic-bezier(.22,.72,.26,1)"
        } else {
            "none"
        },
    );
    let _ = style.set_property("transform", &format!("translateX({x}px)"));
}

fn set_x_offset(runtime: &Rc<RefCell<ReaderRuntime>>, flow: DivRef, offset: f64) {
    let Some(flow) = flow_html_element(flow) else {
        return;
    };
    let state = runtime.borrow();
    let x = -f64::from(state.current_col) * state.metrics.pitch + offset;
    let style = flow.style();
    let _ = style.set_property("transition", "none");
    let _ = style.set_property("transform", &format!("translateX({x}px)"));
}

fn set_promote(flow: DivRef, enabled: bool) {
    if let Some(flow) = flow_html_element(flow) {
        let _ = flow
            .style()
            .set_property("will-change", if enabled { "transform" } else { "auto" });
    }
}

fn col_from_rect(runtime: &Rc<RefCell<ReaderRuntime>>, flow: &Element, rect: &DomRect) -> i32 {
    let state = runtime.borrow();
    if state.metrics.pitch <= 0.0 {
        return 0;
    }
    let flow_rect = flow.get_bounding_client_rect();
    let value = ((rect.left() - flow_rect.left() - state.metrics.side + 1.0) / state.metrics.pitch)
        .floor() as i32;
    value.clamp(0, state.cols.saturating_sub(1))
}

fn col_for_anchor(
    runtime: &Rc<RefCell<ReaderRuntime>>,
    flow: DivRef,
    anchor: &Anchor,
) -> Option<i32> {
    let flow = flow_element(flow)?;
    let block = find_block(&flow, anchor.block)?;
    let node = resolve_path(&block, &anchor.path)?;
    if node.node_type() == Node::TEXT_NODE {
        if let Some(rect) = collapsed_range_rect(&node, anchor.offset.max(0)) {
            return Some(col_from_rect(runtime, &flow, &rect));
        }
    }
    let element = node.dyn_ref::<Element>().unwrap_or(&block);
    Some(col_from_rect(
        runtime,
        &flow,
        &element.get_bounding_client_rect(),
    ))
}

fn collapsed_range_rect(node: &Node, offset: i32) -> Option<DomRect> {
    let document = node.owner_document()?;
    let range = document.create_range().ok()?;
    let length = node
        .text_content()
        .map_or(0, |value| value.encode_utf16().count());
    let offset = offset.clamp(0, i32::try_from(length).unwrap_or(i32::MAX)) as u32;
    range.set_start(node, offset).ok()?;
    range.collapse();
    first_rect_of_range(&range).or_else(|| Some(range.get_bounding_client_rect()))
}

fn find_block(flow: &Element, block: i32) -> Option<Element> {
    let nodes = flow.query_selector_all("[data-block]").ok()?;
    for index in 0..nodes.length() {
        let element = nodes.item(index)?.dyn_into::<Element>().ok()?;
        if element
            .get_attribute("data-block")
            .and_then(|value| value.parse::<i32>().ok())
            == Some(block)
        {
            return Some(element);
        }
    }
    None
}

fn resolve_path(block: &Element, path: &[i32]) -> Option<Node> {
    let mut node: Node = block.clone().unchecked_into();
    for index in path {
        if *index < 0 {
            return None;
        }
        node = node.child_nodes().item(*index as u32)?;
    }
    Some(node)
}

fn child_index(parent: &Node, child: &Node) -> Option<i32> {
    let children = parent.child_nodes();
    for index in 0..children.length() {
        if children
            .item(index)
            .is_some_and(|value| value.is_same_node(Some(child)))
        {
            return i32::try_from(index).ok();
        }
    }
    None
}

fn anchor_from_node(manifest: &FlowManifest, block: &Element, node: &Node, offset: i32) -> Anchor {
    let block_id = block
        .get_attribute("data-block")
        .and_then(|value| value.parse::<i32>().ok())
        .unwrap_or(0);
    let block_node: Node = block.clone().unchecked_into();
    let mut path = Vec::new();
    let mut current = node.clone();
    while !current.is_same_node(Some(&block_node)) {
        let Some(parent) = current.parent_node() else {
            return Anchor {
                spine: spine_for_block(manifest, block_id),
                block: block_id,
                path: Vec::new(),
                offset: Anchor::BOUNDARY_OFFSET,
            };
        };
        let Some(index) = child_index(&parent, &current) else {
            break;
        };
        path.push(index);
        current = parent;
    }
    path.reverse();
    Anchor {
        spine: spine_for_block(manifest, block_id),
        block: block_id,
        path,
        offset: if node.node_type() == Node::TEXT_NODE {
            offset.max(0)
        } else {
            Anchor::BOUNDARY_OFFSET
        },
    }
}

fn rect_has_box(rect: &DomRect) -> bool {
    rect.width() > 0.0 || rect.height() > 0.0
}

fn rect_in_current_column(
    runtime: &Rc<RefCell<ReaderRuntime>>,
    flow: &Element,
    viewport: &Element,
    rect: &DomRect,
) -> bool {
    if !rect_has_box(rect) {
        return false;
    }
    let state = runtime.borrow();
    let flow_rect = flow.get_bounding_client_rect();
    let viewport_rect = viewport.get_bounding_client_rect();
    if state.metrics.pitch <= 0.0 {
        return false;
    }
    let column = ((rect.left() - flow_rect.left() - state.metrics.side + 1.0) / state.metrics.pitch)
        .floor() as i32;
    column == state.current_col
        && rect.right() > viewport_rect.left() + state.metrics.side
        && rect.left() < viewport_rect.right() - state.metrics.side
        && rect.bottom() > viewport_rect.top() + state.metrics.top
        && rect.top() < viewport_rect.bottom() - state.metrics.bottom
}

fn first_rect_of_range(range: &web_sys::Range) -> Option<DomRect> {
    range.get_client_rects().and_then(|rects| rects.item(0))
}

#[derive(Clone)]
struct VisualStart {
    rect: DomRect,
    node: Option<Node>,
    offset: i32,
}

fn first_text_visual(node: &Node, depth: usize) -> Option<VisualStart> {
    if depth > MAX_TEXT_WALK_DEPTH {
        return None;
    }
    if node.node_type() == Node::TEXT_NODE {
        let text = node.text_content().unwrap_or_default();
        let (character_index, _) = text
            .char_indices()
            .find(|(_, value)| !value.is_whitespace())?;
        let offset = text[..character_index].encode_utf16().count() as i32;
        let rect = collapsed_range_rect(node, offset)?;
        return rect_has_box(&rect).then(|| VisualStart {
            rect,
            node: Some(node.clone()),
            offset,
        });
    }
    let children = node.child_nodes();
    for index in 0..children.length() {
        let Some(child) = children.item(index) else {
            continue;
        };
        if let Some(start) = first_text_visual(&child, depth + 1) {
            return Some(start);
        }
    }
    None
}

fn first_media_visual(element: &Element) -> Option<VisualStart> {
    let nodes = element
        .query_selector_all("img,svg,video,canvas,iframe,embed,object,table")
        .ok()?;
    for index in 0..nodes.length() {
        let node = nodes.item(index)?;
        let element = node.dyn_into::<Element>().ok()?;
        let rect = element.get_bounding_client_rect();
        if rect_has_box(&rect) {
            return Some(VisualStart {
                rect,
                node: Some(element.unchecked_into()),
                offset: Anchor::BOUNDARY_OFFSET,
            });
        }
    }
    None
}

/// Find the first actual visual content of an element. A block's own first
/// fragment is not sufficient in CSS columns: a break-inside-avoid image can
/// move the content into a later column while the container fragment stays in
/// the previous one.
fn visual_start(element: &Element) -> Option<VisualStart> {
    let root: Node = element.clone().unchecked_into();
    let text = first_text_visual(&root, 0);
    let media = first_media_visual(element);
    let content = match (text, media) {
        (Some(text), Some(media)) => {
            let text_before_media =
                text.node
                    .as_ref()
                    .zip(media.node.as_ref())
                    .is_some_and(|(text, media)| {
                        text.compare_document_position(media) & Node::DOCUMENT_POSITION_FOLLOWING
                            != 0
                    });
            if text_before_media { text } else { media }
        }
        (Some(text), None) => text,
        (None, Some(media)) => media,
        (None, None) => {
            let rect = element
                .get_client_rects()
                .item(0)
                .unwrap_or_else(|| element.get_bounding_client_rect());
            if !rect_has_box(&rect) && rect.left() == 0.0 && rect.top() == 0.0 {
                return None;
            }
            VisualStart {
                rect,
                node: None,
                offset: Anchor::BOUNDARY_OFFSET,
            }
        }
    };
    Some(content)
}

fn anchor_at_text_fragment(
    runtime: &Rc<RefCell<ReaderRuntime>>,
    manifest: &FlowManifest,
    flow: &Element,
    viewport: &Element,
    block: &Element,
    node: &Node,
    first_offset: i32,
) -> Option<Anchor> {
    let length = node
        .text_content()
        .map_or(0, |value| value.encode_utf16().count());
    let length = i32::try_from(length).unwrap_or(i32::MAX);
    let current_col = runtime.borrow().current_col;
    let mut low = first_offset.clamp(0, length);
    let mut high = length;
    while low < high {
        let middle = low + (high - low) / 2;
        let Some(rect) = collapsed_range_rect(node, middle) else {
            low = middle.saturating_add(1);
            continue;
        };
        if col_from_rect(runtime, flow, &rect) >= current_col {
            high = middle;
        } else {
            low = middle.saturating_add(1);
        }
    }
    let start = low.saturating_sub(2).max(first_offset);
    let end = (low.saturating_add(2)).min(length);
    for offset in start..=end {
        if let Some(rect) = collapsed_range_rect(node, offset)
            && rect_in_current_column(runtime, flow, viewport, &rect)
        {
            return Some(anchor_from_node(manifest, block, node, offset));
        }
    }
    None
}

fn first_text_anchor(
    runtime: &Rc<RefCell<ReaderRuntime>>,
    manifest: &FlowManifest,
    flow: &Element,
    viewport: &Element,
    block: &Element,
    node: &Node,
    depth: usize,
) -> Option<Anchor> {
    if depth > MAX_TEXT_WALK_DEPTH {
        return None;
    }
    if node.node_type() == Node::TEXT_NODE {
        let text = node.text_content().unwrap_or_default();
        let Some((character_index, _)) = text
            .char_indices()
            .find(|(_, value)| !value.is_whitespace())
        else {
            return None;
        };
        let offset = text[..character_index].encode_utf16().count() as i32;
        return anchor_at_text_fragment(runtime, manifest, flow, viewport, block, node, offset);
    }

    let children = node.child_nodes();
    for index in 0..children.length() {
        let Some(child) = children.item(index) else {
            continue;
        };
        if let Some(anchor) =
            first_text_anchor(runtime, manifest, flow, viewport, block, &child, depth + 1)
        {
            return Some(anchor);
        }
    }
    None
}

fn anchor_from_visible_block(
    runtime: &Rc<RefCell<ReaderRuntime>>,
    manifest: &FlowManifest,
    flow: &Element,
    viewport: &Element,
    block: &Element,
) -> Option<Anchor> {
    let rects = block.get_client_rects();
    let visible = (0..rects.length()).any(|position| {
        rects
            .item(position)
            .is_some_and(|rect| rect_in_current_column(runtime, flow, viewport, &rect))
    });
    if !visible {
        return None;
    }

    if let Some(start) = visual_start(block)
        && rect_in_current_column(runtime, flow, viewport, &start.rect)
    {
        let block_node: Node = block.clone().unchecked_into();
        if let Some(node) = start.node {
            return Some(anchor_from_node(manifest, block, &node, start.offset));
        }
        return Some(anchor_from_node(
            manifest,
            block,
            &block_node,
            Anchor::BOUNDARY_OFFSET,
        ));
    }

    let text = first_text_anchor(
        runtime,
        manifest,
        flow,
        viewport,
        block,
        &{
            let node: Node = block.clone().unchecked_into();
            node
        },
        0,
    )
    .map(|anchor| {
        let node =
            resolve_path(block, &anchor.path).unwrap_or_else(|| block.clone().unchecked_into());
        (node, anchor)
    });
    let media = first_media_in_current_column(runtime, flow, viewport, block);
    match (text, media) {
        (Some((text_node, text_anchor)), Some((media_node, media_anchor))) => {
            if text_node.compare_document_position(&media_node) & Node::DOCUMENT_POSITION_FOLLOWING
                != 0
            {
                Some(text_anchor)
            } else {
                Some(media_anchor)
            }
        }
        (Some((_, anchor)), None) | (None, Some((_, anchor))) => Some(anchor),
        (None, None) => {
            let block_node: Node = block.clone().unchecked_into();
            Some(anchor_from_node(
                manifest,
                block,
                &block_node,
                Anchor::BOUNDARY_OFFSET,
            ))
        }
    }
}

fn first_media_in_current_column(
    runtime: &Rc<RefCell<ReaderRuntime>>,
    flow: &Element,
    viewport: &Element,
    block: &Element,
) -> Option<(Node, Anchor)> {
    let manifest = runtime.borrow().manifest.clone()?;
    let nodes = block
        .query_selector_all("img,svg,video,canvas,iframe,embed,object,table")
        .ok()?;
    for index in 0..nodes.length() {
        let node = nodes.item(index)?;
        let element = node.clone().dyn_into::<Element>().ok()?;
        let rect = element.get_bounding_client_rect();
        if rect_in_current_column(runtime, flow, viewport, &rect) {
            return Some((
                node.clone(),
                anchor_from_node(&manifest, block, &node, Anchor::BOUNDARY_OFFSET),
            ));
        }
    }
    None
}

fn block_at_point(x: f64, y: f64) -> Option<Element> {
    let document = web_sys::window()?.document()?;
    let elements = document.elements_from_point(x as f32, y as f32);
    for value in elements.iter() {
        let element = value.dyn_into::<Element>().ok()?;
        if element.has_attribute("data-block") {
            return Some(element);
        }
        if let Ok(Some(block)) = element.closest("[data-block]") {
            return Some(block);
        }
    }
    None
}

fn block_for_node(node: &Node) -> Option<Element> {
    let mut current = node.clone();
    loop {
        if let Some(element) = current.dyn_ref::<Element>()
            && element.has_attribute("data-block")
        {
            return Some(element.clone());
        }
        current = current.parent_node()?;
    }
}

fn content_origin(runtime: &Rc<RefCell<ReaderRuntime>>, viewport: &Element) -> (f64, f64) {
    let state = runtime.borrow();
    let rect = viewport.get_bounding_client_rect();
    (
        rect.left() + state.metrics.side + 2.0,
        rect.top() + state.metrics.top + 2.0,
    )
}

fn capture_top_anchor(
    runtime: &Rc<RefCell<ReaderRuntime>>,
    viewport: DivRef,
    flow: DivRef,
) -> Option<Anchor> {
    let viewport = viewport_element(viewport)?;
    let flow = flow_element(flow)?;
    let manifest = runtime.borrow().manifest.clone()?;
    let (x, y) = content_origin(runtime, &viewport);
    if let Some(document) = web_sys::window().and_then(|window| window.document())
        && let Some(caret) = document.caret_position_from_point(x as f32, y as f32)
        && let Some(node) = caret.offset_node()
        && node.node_type() == Node::TEXT_NODE
        && let Some(block) = block_for_node(&node)
    {
        let offset = i32::try_from(caret.offset()).unwrap_or(i32::MAX);
        let anchor = anchor_from_node(&manifest, &block, &node, offset);
        if collapsed_range_rect(&node, offset)
            .is_some_and(|rect| rect_in_current_column(runtime, &flow, &viewport, &rect))
        {
            return Some(anchor);
        }
        if let Some(block) = block_at_point(x, y)
            && let Some(anchor) =
                anchor_from_visible_block(runtime, &manifest, &flow, &viewport, &block)
        {
            return Some(anchor);
        }
    }
    if let Some(block) = block_at_point(x, y)
        && let Some(anchor) =
            anchor_from_visible_block(runtime, &manifest, &flow, &viewport, &block)
    {
        return Some(anchor);
    }
    let blocks = flow.query_selector_all("[data-block]").ok()?;
    for index in 0..blocks.length() {
        let Some(block) = blocks.item(index)?.dyn_into::<Element>().ok() else {
            continue;
        };
        if let Some(anchor) =
            anchor_from_visible_block(runtime, &manifest, &flow, &viewport, &block)
        {
            return Some(anchor);
        }
    }
    None
}

fn viewport_element(viewport: DivRef) -> Option<Element> {
    viewport
        .get()
        .map(|value| value.unchecked_into::<Element>())
}

fn refresh_ui(
    runtime: &Rc<RefCell<ReaderRuntime>>,
    flow: DivRef,
    toc_active: RwSignal<i32>,
    percent: RwSignal<f64>,
) {
    let state = runtime.borrow();
    let Some(manifest) = &state.manifest else {
        toc_active.set(-1);
        percent.set(0.0);
        return;
    };
    let Some(anchor) = &state.top_anchor else {
        toc_active.set(-1);
        percent.set(0.0);
        return;
    };
    toc_active.set(toc_active_index(manifest, anchor.block));
    percent.set(percent_for_anchor(manifest, flow, anchor));
}

fn percent_for_anchor(manifest: &FlowManifest, flow: DivRef, anchor: &Anchor) -> f64 {
    if manifest.total_chars <= 0 {
        return 0.0;
    }
    let Some(chunk) = manifest.chunk_for_block(anchor.block) else {
        return 0.0;
    };
    let mut chars = chunk_prefix(manifest, chunk);
    if let Some(flow) = flow_element(flow)
        && let Ok(chunks) = flow.query_selector_all(".rf-chunk")
    {
        for index in 0..chunks.length() {
            let Some(node) = chunks.item(index) else {
                continue;
            };
            let Ok(element) = node.dyn_into::<Element>() else {
                continue;
            };
            if element
                .get_attribute("data-chunk")
                .and_then(|value| value.parse::<i32>().ok())
                != Some(chunk)
            {
                continue;
            }
            let children = element.child_nodes();
            for child_index in 0..children.length() {
                let Some(child) = children.item(child_index) else {
                    continue;
                };
                let Ok(child) = child.dyn_into::<Element>() else {
                    continue;
                };
                let Some(block) = child
                    .get_attribute("data-block")
                    .and_then(|value| value.parse::<i32>().ok())
                else {
                    continue;
                };
                if block >= anchor.block {
                    break;
                }
                chars = chars.saturating_add(
                    child
                        .text_content()
                        .map_or(0, |value| value.encode_utf16().count()) as i64,
                );
            }
            break;
        }
    }
    (chars as f64 / manifest.total_chars as f64 * 100.0).clamp(0.0, 100.0)
}

fn progress_label(percent: f64) -> String {
    let value = percent.clamp(0.0, 100.0);
    if value.fract() == 0.0 {
        format!("{value:.0}%")
    } else {
        format!("{value:.1}%")
    }
}

fn line_height_label(value: f64) -> String {
    if value.fract() == 0.0 {
        format!("{value:.0}")
    } else {
        format!("{value:.1}")
    }
}

fn resolve_text_target(
    runtime: &Rc<RefCell<ReaderRuntime>>,
    flow: &Element,
    entry: &TocTarget,
) -> Option<(Anchor, DomRect)> {
    let manifest = runtime.borrow().manifest.clone()?;
    let block = find_block(flow, entry.block)?;
    let node = resolve_path(&block, &entry.text_path)?;
    if node.node_type() != Node::TEXT_NODE {
        return None;
    }
    let length = node
        .text_content()
        .map_or(0, |value| value.encode_utf16().count());
    let offset = entry
        .text_offset
        .clamp(0, i32::try_from(length).unwrap_or(i32::MAX));
    let rect = collapsed_range_rect(&node, offset)?;
    rect_has_box(&rect).then(|| (anchor_from_node(&manifest, &block, &node, offset), rect))
}

fn resolve_nav_target(
    runtime: &Rc<RefCell<ReaderRuntime>>,
    flow: &Element,
    entry: &TocTarget,
) -> Option<(Anchor, DomRect)> {
    if entry.nav_anchor.is_empty() {
        return None;
    }
    let nodes = flow.query_selector_all("[data-rv-anchor]").ok()?;
    let manifest = runtime.borrow().manifest.clone()?;
    for index in 0..nodes.length() {
        let element = nodes.item(index)?.dyn_into::<Element>().ok()?;
        if element.get_attribute("data-rv-anchor").as_deref() != Some(&entry.nav_anchor) {
            continue;
        }
        let block = element
            .closest("[data-block]")
            .ok()
            .flatten()
            .or_else(|| find_block(flow, entry.block))?;
        let rect = element.get_bounding_client_rect();
        if !rect_has_box(&rect) {
            continue;
        }
        let node: Node = element.clone().unchecked_into();
        return Some((anchor_from_node(&manifest, &block, &node, -1), rect));
    }
    None
}

fn decode_fragment(value: &str) -> String {
    js_sys::decode_uri_component(value)
        .ok()
        .and_then(|value| value.as_string())
        .unwrap_or_else(|| value.to_owned())
}

fn fragment_matches(element: &Element, fragment: &str, decoded: &str) -> bool {
    if element
        .get_attribute("id")
        .is_some_and(|value| value == fragment || value == decoded)
    {
        return true;
    }
    element.get_attribute("data-frag-ids").is_some_and(|value| {
        value
            .split_whitespace()
            .any(|part| part == fragment || part == decoded)
    })
}

fn resolve_fragment_target(
    runtime: &Rc<RefCell<ReaderRuntime>>,
    flow: &Element,
    entry: &TocTarget,
) -> Option<(Anchor, DomRect)> {
    if entry.source_fragment.is_empty() {
        return None;
    }
    let block = find_block(flow, entry.block)?;
    let decoded = decode_fragment(&entry.source_fragment);
    let candidates = block.query_selector_all("[id], [data-frag-ids]").ok()?;
    let manifest = runtime.borrow().manifest.clone()?;
    let block_node: Node = block.clone().unchecked_into();
    if fragment_matches(&block, &entry.source_fragment, &decoded) {
        let rect = block.get_bounding_client_rect();
        if rect_has_box(&rect) {
            return Some((anchor_from_node(&manifest, &block, &block_node, -1), rect));
        }
    }
    for index in 0..candidates.length() {
        let element = candidates.item(index)?.dyn_into::<Element>().ok()?;
        if !fragment_matches(&element, &entry.source_fragment, &decoded) {
            continue;
        }
        let rect = element.get_bounding_client_rect();
        if !rect_has_box(&rect) {
            continue;
        }
        let node: Node = element.clone().unchecked_into();
        return Some((anchor_from_node(&manifest, &block, &node, -1), rect));
    }
    None
}

fn spawn_toc_jump(
    runtime: Rc<RefCell<ReaderRuntime>>,
    viewport: DivRef,
    flow: DivRef,
    file_id: String,
    stage: RwSignal<ReaderStage>,
    toc_active: RwSignal<i32>,
    percent: RwSignal<f64>,
    entry: TocTarget,
) {
    leptos::task::spawn_local(async move {
        jump_to_toc(
            runtime, viewport, flow, file_id, stage, toc_active, percent, entry,
        )
        .await;
    });
}

async fn jump_to_toc(
    runtime: Rc<RefCell<ReaderRuntime>>,
    viewport: DivRef,
    flow: DivRef,
    file_id: String,
    stage: RwSignal<ReaderStage>,
    toc_active: RwSignal<i32>,
    percent: RwSignal<f64>,
    entry: TocTarget,
) {
    if stage.get_untracked() != ReaderStage::Reading {
        return;
    }
    let old_timer = runtime.borrow_mut().sync_timer.take();
    clear_timer_value(old_timer);
    let depth = runtime.borrow().nav_depth;
    runtime.borrow_mut().nav_depth = depth.saturating_add(1);
    let _nav_guard = NavGuard(runtime.clone());
    let Some(manifest) = runtime.borrow().manifest.clone() else {
        return;
    };
    let total = total_blocks(&manifest).max(1);
    let block = entry.block.clamp(0, total - 1);
    let (first, last) = stable_window_range(&manifest, block, AHEAD_MARGIN);
    if ensure_window(&runtime, &file_id, &manifest, flow, first, last)
        .await
        .is_err()
    {
        return;
    }
    measure_cols(&runtime, flow);
    let Some(flow_element) = flow_element(flow) else {
        return;
    };
    let fixed = TocTarget {
        block,
        ..entry.clone()
    };
    let target = resolve_text_target(&runtime, &flow_element, &fixed)
        .or_else(|| resolve_nav_target(&runtime, &flow_element, &fixed))
        .or_else(|| resolve_fragment_target(&runtime, &flow_element, &fixed));
    if let Some((anchor, rect)) = target {
        let column = col_from_rect(&runtime, &flow_element, &rect);
        move_to_column(&runtime, flow, column, false).await;
        runtime.borrow_mut().top_anchor = Some(anchor);
        refresh_ui(&runtime, flow, toc_active, percent);
        schedule_progress_save(runtime.clone(), file_id.clone());
        schedule_window_sync(runtime, viewport, flow, file_id, stage, toc_active, percent);
        return;
    }
    jump_to_block(
        runtime, viewport, flow, file_id, stage, toc_active, percent, block,
    )
    .await;
}

async fn jump_to_block(
    runtime: Rc<RefCell<ReaderRuntime>>,
    viewport: DivRef,
    flow: DivRef,
    file_id: String,
    stage: RwSignal<ReaderStage>,
    toc_active: RwSignal<i32>,
    percent: RwSignal<f64>,
    block: i32,
) {
    if stage.get_untracked() != ReaderStage::Reading {
        return;
    }
    let Some(manifest) = runtime.borrow().manifest.clone() else {
        return;
    };
    let total = total_blocks(&manifest).max(1);
    let block = block.clamp(0, total - 1);
    let (first, last) = stable_window_range(&manifest, block, AHEAD_MARGIN);
    if ensure_window(&runtime, &file_id, &manifest, flow, first, last)
        .await
        .is_err()
    {
        return;
    }
    measure_cols(&runtime, flow);
    let anchor = Anchor {
        spine: spine_for_block(&manifest, block),
        block,
        path: Vec::new(),
        offset: Anchor::BOUNDARY_OFFSET,
    };
    let column = col_for_anchor(&runtime, flow, &anchor).unwrap_or(0);
    move_to_column(&runtime, flow, column, false).await;
    runtime.borrow_mut().top_anchor = Some(anchor);
    refresh_ui(&runtime, flow, toc_active, percent);
    schedule_progress_save(runtime.clone(), file_id.clone());
    schedule_window_sync(runtime, viewport, flow, file_id, stage, toc_active, percent);
    let _ = viewport;
}

async fn move_to_column(
    runtime: &Rc<RefCell<ReaderRuntime>>,
    flow: DivRef,
    column: i32,
    animated: bool,
) {
    {
        let mut state = runtime.borrow_mut();
        state.current_col = column.clamp(0, state.cols.saturating_sub(1));
    }
    set_promote(flow, animated);
    set_x(runtime, flow, animated);
    if animated {
        pause(ANIMATION_MS).await;
        if let Some(flow) = flow_html_element(flow) {
            let _ = flow.style().set_property("transition", "none");
        }
        set_promote(flow, false);
    } else {
        set_promote(flow, false);
    }
}

fn spawn_snap_to_column(
    runtime: Rc<RefCell<ReaderRuntime>>,
    viewport: DivRef,
    flow: DivRef,
    file_id: String,
    stage: RwSignal<ReaderStage>,
    toc_active: RwSignal<i32>,
    percent: RwSignal<f64>,
    column: i32,
) {
    leptos::task::spawn_local(async move {
        move_to_column(&runtime, flow, column, true).await;
        if runtime.borrow().closing {
            return;
        }
        capture_and_refresh(&runtime, viewport, flow, toc_active, percent);
        schedule_progress_save(runtime.clone(), file_id.clone());
        schedule_window_sync(runtime, viewport, flow, file_id, stage, toc_active, percent);
    });
}

fn capture_and_refresh(
    runtime: &Rc<RefCell<ReaderRuntime>>,
    viewport: DivRef,
    flow: DivRef,
    toc_active: RwSignal<i32>,
    percent: RwSignal<f64>,
) {
    if let Some(anchor) = capture_top_anchor(runtime, viewport, flow) {
        runtime.borrow_mut().top_anchor = Some(anchor);
    }
    refresh_ui(runtime, flow, toc_active, percent);
}

fn schedule_progress_save(runtime: Rc<RefCell<ReaderRuntime>>, file_id: String) {
    let old_timer = {
        let mut state = runtime.borrow_mut();
        state.progress_timer.take()
    };
    clear_timer_value(old_timer);
    let Some(window) = web_sys::window() else {
        return;
    };
    let callback_state = runtime.clone();
    let callback_id = file_id;
    let callback = Closure::once_into_js(move || {
        callback_state.borrow_mut().progress_timer = None;
        save_current_progress(callback_state, callback_id);
    });
    if let Ok(timer) = window.set_timeout_with_callback_and_timeout_and_arguments_0(
        callback.unchecked_ref(),
        PROGRESS_DELAY_MS,
    ) {
        runtime.borrow_mut().progress_timer = Some(timer);
    }
}

fn clear_timer_value(timer: Option<i32>) {
    if let Some(timer) = timer
        && let Some(window) = web_sys::window()
    {
        window.clear_timeout_with_handle(timer);
    }
}

fn save_current_progress(runtime: Rc<RefCell<ReaderRuntime>>, file_id: String) {
    let anchor = {
        let state = runtime.borrow();
        if state.closing {
            return;
        }
        state.top_anchor.clone()
    };
    let Some(anchor) = anchor else {
        return;
    };
    leptos::task::spawn_local(async move {
        let _ = api::save_book_progress(
            &file_id,
            &SaveProgressRequest {
                anchor: Some(anchor),
            },
        )
        .await;
    });
}

fn flush_progress(runtime: Rc<RefCell<ReaderRuntime>>, file_id: String) {
    let anchor = runtime.borrow().top_anchor.clone();
    if let Some(anchor) = anchor {
        leptos::task::spawn_local(async move {
            let _ = api::save_book_progress(
                &file_id,
                &SaveProgressRequest {
                    anchor: Some(anchor),
                },
            )
            .await;
        });
    }
}

fn schedule_window_sync(
    runtime: Rc<RefCell<ReaderRuntime>>,
    viewport: DivRef,
    flow: DivRef,
    file_id: String,
    stage: RwSignal<ReaderStage>,
    toc_active: RwSignal<i32>,
    percent: RwSignal<f64>,
) {
    let old_timer = {
        let mut state = runtime.borrow_mut();
        state.sync_timer.take()
    };
    clear_timer_value(old_timer);
    let Some(window) = web_sys::window() else {
        return;
    };
    let callback_runtime = runtime.clone();
    let callback = Closure::once_into_js(move || {
        callback_runtime.borrow_mut().sync_timer = None;
        leptos::task::spawn_local(window_sync(
            callback_runtime,
            viewport,
            flow,
            file_id,
            stage,
            toc_active,
            percent,
        ));
    });
    if let Ok(timer) = window.set_timeout_with_callback_and_timeout_and_arguments_0(
        callback.unchecked_ref(),
        WINDOW_SYNC_DELAY_MS,
    ) {
        runtime.borrow_mut().sync_timer = Some(timer);
    }
}

async fn window_sync(
    runtime: Rc<RefCell<ReaderRuntime>>,
    viewport: DivRef,
    flow: DivRef,
    file_id: String,
    stage: RwSignal<ReaderStage>,
    toc_active: RwSignal<i32>,
    percent: RwSignal<f64>,
) {
    if stage.get_untracked() != ReaderStage::Reading {
        return;
    }
    {
        let mut state = runtime.borrow_mut();
        if state.closing || state.syncing || state.turn_busy || state.nav_depth > 0 {
            return;
        }
        state.syncing = true;
    }
    let result = async {
        let (manifest, anchor) = {
            let state = runtime.borrow();
            (state.manifest.clone(), state.top_anchor.clone())
        };
        let (Some(manifest), Some(anchor)) = (manifest, anchor) else {
            return Ok::<(), String>(());
        };
        let (first, last) = stable_window_range(&manifest, anchor.block, AHEAD_MARGIN);
        let changed = ensure_window(&runtime, &file_id, &manifest, flow, first, last).await?;
        if runtime.borrow().closing || runtime.borrow().nav_depth > 0 {
            return Ok(());
        }
        if changed {
            measure_cols(&runtime, flow);
            let column = col_for_anchor(&runtime, flow, &anchor).unwrap_or(0);
            move_to_column(&runtime, flow, column, false).await;
        }
        if runtime.borrow().closing || runtime.borrow().nav_depth > 0 {
            return Ok(());
        }
        refresh_ui(&runtime, flow, toc_active, percent);
        Ok(())
    }
    .await;
    if result.is_err() {
        // A background prefetch failure should not cover readable content with
        // an error panel. The next page turn retries the missing chunk.
    }
    let pending = {
        let mut state = runtime.borrow_mut();
        state.syncing = false;
        if state.closing || state.nav_depth > 0 {
            0
        } else {
            state.pending_turns
        }
    };
    if pending != 0 {
        let direction = if pending > 0 { 1 } else { -1 };
        runtime.borrow_mut().pending_turns -= direction;
        spawn_turn(
            runtime, viewport, flow, file_id, stage, toc_active, percent, direction,
        );
    }
}

fn spawn_turn(
    runtime: Rc<RefCell<ReaderRuntime>>,
    viewport: DivRef,
    flow: DivRef,
    file_id: String,
    stage: RwSignal<ReaderStage>,
    toc_active: RwSignal<i32>,
    percent: RwSignal<f64>,
    direction: i32,
) {
    leptos::task::spawn_local(async move {
        turn(
            runtime,
            viewport,
            flow,
            file_id,
            stage,
            toc_active,
            percent,
            direction.signum(),
        )
        .await;
    });
}

async fn turn(
    runtime: Rc<RefCell<ReaderRuntime>>,
    viewport: DivRef,
    flow: DivRef,
    file_id: String,
    stage: RwSignal<ReaderStage>,
    toc_active: RwSignal<i32>,
    percent: RwSignal<f64>,
    direction: i32,
) {
    if stage.get_untracked() != ReaderStage::Reading || direction == 0 {
        return;
    }
    {
        let mut state = runtime.borrow_mut();
        if state.closing {
            return;
        }
        if state.turn_busy || state.syncing {
            state.pending_turns += direction;
            return;
        }
        state.turn_busy = true;
    }

    let mut can_move = true;
    let current = runtime.borrow().current_col;
    let cols = runtime.borrow().cols;
    let target = current + direction;
    if target < 0 || target >= cols {
        let (manifest, first, last) = {
            let state = runtime.borrow();
            (state.manifest.clone(), state.first_chunk, state.last_chunk)
        };
        if let Some(manifest) = manifest {
            let final_chunk = manifest.chunks.len().saturating_sub(1) as i32;
            let range = if direction > 0 {
                if last >= final_chunk {
                    None
                } else {
                    Some((first, last + 1))
                }
            } else if first <= 0 {
                None
            } else {
                Some((first - 1, last))
            };
            if let Some((new_first, new_last)) = range {
                if ensure_window(&runtime, &file_id, &manifest, flow, new_first, new_last)
                    .await
                    .is_ok()
                {
                    measure_cols(&runtime, flow);
                    if let Some(anchor) = runtime.borrow().top_anchor.clone() {
                        let column = col_for_anchor(&runtime, flow, &anchor).unwrap_or(0);
                        runtime.borrow_mut().current_col = column;
                        set_x(&runtime, flow, false);
                    }
                } else {
                    can_move = false;
                }
            } else {
                can_move = false;
            }
        } else {
            can_move = false;
        }
    }

    if can_move {
        let current = runtime.borrow().current_col;
        let cols = runtime.borrow().cols;
        let next = current + direction;
        if next >= 0 && next < cols {
            move_to_column(&runtime, flow, next, true).await;
            if !runtime.borrow().closing {
                capture_and_refresh(&runtime, viewport, flow, toc_active, percent);
                schedule_progress_save(runtime.clone(), file_id.clone());
                schedule_window_sync(
                    runtime.clone(),
                    viewport,
                    flow,
                    file_id.clone(),
                    stage,
                    toc_active,
                    percent,
                );
            }
        }
    }

    let pending = {
        let mut state = runtime.borrow_mut();
        state.turn_busy = false;
        if state.closing {
            0
        } else {
            state.pending_turns
        }
    };
    if pending != 0 {
        let next = if pending > 0 { 1 } else { -1 };
        runtime.borrow_mut().pending_turns -= next;
        spawn_turn(
            runtime, viewport, flow, file_id, stage, toc_active, percent, next,
        );
    }
}

async fn pause(milliseconds: i32) {
    let Some(window) = web_sys::window() else {
        return;
    };
    let (sender, receiver) = oneshot::channel::<()>();
    let callback = Closure::once_into_js(move || {
        let _ = sender.send(());
    });
    if window
        .set_timeout_with_callback_and_timeout_and_arguments_0(
            callback.unchecked_ref(),
            milliseconds,
        )
        .is_ok()
    {
        let _ = receiver.await;
    }
}

fn schedule_relayout(
    runtime: Rc<RefCell<ReaderRuntime>>,
    viewport: DivRef,
    flow: DivRef,
    manifest_signal: RwSignal<Option<FlowManifest>>,
    prefs: RwSignal<ReaderPrefs>,
    stage: RwSignal<ReaderStage>,
) {
    let old_timer = {
        let mut state = runtime.borrow_mut();
        state.relayout_timer.take()
    };
    clear_timer_value(old_timer);
    let Some(window) = web_sys::window() else {
        return;
    };
    let callback_runtime = runtime.clone();
    let callback = Closure::once_into_js(move || {
        callback_runtime.borrow_mut().relayout_timer = None;
        relayout(
            callback_runtime,
            viewport,
            flow,
            manifest_signal,
            prefs,
            stage,
        );
    });
    if let Ok(timer) = window.set_timeout_with_callback_and_timeout_and_arguments_0(
        callback.unchecked_ref(),
        RELAYOUT_DELAY_MS,
    ) {
        runtime.borrow_mut().relayout_timer = Some(timer);
    }
}

fn relayout(
    runtime: Rc<RefCell<ReaderRuntime>>,
    viewport: DivRef,
    flow: DivRef,
    manifest_signal: RwSignal<Option<FlowManifest>>,
    prefs: RwSignal<ReaderPrefs>,
    stage: RwSignal<ReaderStage>,
) {
    if stage.get_untracked() != ReaderStage::Reading {
        return;
    }
    let Some(manifest) = manifest_signal.get_untracked() else {
        return;
    };
    let keep = runtime
        .borrow()
        .top_anchor
        .clone()
        .or_else(|| capture_top_anchor(&runtime, viewport, flow));
    let metrics = apply_metrics(viewport, flow, &manifest.format, prefs.get_untracked());
    runtime.borrow_mut().metrics = metrics;
    measure_cols(&runtime, flow);
    if let Some(anchor) = keep {
        let column = col_for_anchor(&runtime, flow, &anchor).unwrap_or(0);
        runtime.borrow_mut().current_col = column;
        runtime.borrow_mut().top_anchor = Some(anchor);
    } else {
        runtime.borrow_mut().current_col = 0;
    }
    set_x(&runtime, flow, false);
}

fn make_font_adjuster(
    runtime: Rc<RefCell<ReaderRuntime>>,
    viewport: DivRef,
    flow: DivRef,
    manifest: RwSignal<Option<FlowManifest>>,
    prefs: RwSignal<ReaderPrefs>,
    stage: RwSignal<ReaderStage>,
    delta: i32,
) -> impl FnMut(leptos::ev::MouseEvent) + 'static {
    move |_| {
        prefs.update(|value| value.font_size = clamp_font_size(value.font_size + delta));
        save_prefs(prefs.get_untracked());
        schedule_relayout(runtime.clone(), viewport, flow, manifest, prefs, stage);
    }
}

fn trap_focus(root: SectionRef, event: &web_sys::KeyboardEvent) {
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
        r#"button:not([disabled]), summary, input:not([disabled]), select:not([disabled]), [tabindex="0"]"#,
    ) else {
        return;
    };
    let mut focusable = Vec::new();
    for index in 0..nodes.length() {
        let Some(node) = nodes.item(index) else {
            continue;
        };
        let Ok(element) = node.dyn_into::<HtmlElement>() else {
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
    let first_element: Element = first.clone().unchecked_into();
    let last_element: Element = last.clone().unchecked_into();
    let active_is_first = active
        .as_ref()
        .is_some_and(|value| value.is_same_node(Some(&first_element)));
    let active_is_last = active
        .as_ref()
        .is_some_and(|value| value.is_same_node(Some(&last_element)));
    let active_in_scope = active
        .as_ref()
        .is_some_and(|value| scope.contains(Some(value.unchecked_ref::<web_sys::Node>())));
    let root_element: Element = root.clone().unchecked_into();
    let active_is_root = active
        .as_ref()
        .is_some_and(|value| value.is_same_node(Some(&root_element)));
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

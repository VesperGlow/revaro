//! EPUB/TXT reading view.
//!
//! The server produces sanitized flow chunks and stable reading anchors. This
//! module owns the browser-specific part of the reader: a bounded DOM window,
//! native CSS columns, page navigation, TOC targeting, preference persistence,
//! focus lifecycle and durable progress writes. Pagination rules that do not
//! need a browser live in crate::logic::reader.

mod anchors;
mod layout;
mod loading;
mod navigation;
mod paging;
mod persistence;

use anchors::*;
use layout::*;
use loading::*;
use navigation::*;
use paging::*;
use persistence::*;

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
    top_percent: f64,
    library_refresh: Option<RwSignal<u64>>,
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
            top_percent: 0.0,
            library_refresh: None,
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
    runtime.borrow_mut().library_refresh =
        use_context::<super::content_shell::ShellContext>().map(|c| c.refresh);

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
        let file_id = file_id.clone();
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
        browser::on_resize(move |_| {
            if manifest.get_untracked().is_none() || stage.get_untracked() != ReaderStage::Reading {
                return;
            }
            schedule_relayout(runtime.clone(), viewport, flow, manifest, prefs, stage);
        })
    };

    browser::dismiss_popover(
        font_open.into(),
        || {
            web_sys::window()
                .and_then(|window| window.document())
                .and_then(|document| document.get_element_by_id("font-popover"))
        },
        |target| {
            target.dyn_ref::<Element>().is_some_and(|target| {
                target
                    .closest("#font-popover, #font-button")
                    .ok()
                    .flatten()
                    .is_some()
            })
        },
        Callback::new(move |restore_focus| {
            font_open.set(false);
            if restore_focus {
                focus_element_by_id("font-button");
            }
        }),
    );
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
    // Opening can await metadata, a manifest and multiple chunks. None of
    // those continuations may touch the reader after its view is closed.
    leptos::task::spawn_local_scoped_with_cancellation(async move {
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
        let callback = Closure::once(move || {
            if let Some(root) = focus_root.get() {
                let options = web_sys::FocusOptions::new();
                options.set_prevent_scroll(true);
                let _ = root.focus_with_options(&options);
            }
        })
        .into_js_value();
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
        move |_| {
            toc_open.set(false);
        }
    };
    let open_toc = {
        move |_| {
            font_open.set(false);
            toc_open.set(true);
            focus_element_after_render("toc-close");
        }
    };
    let toggle_font = { move |_| font_open.update(|value| *value = !*value) };
    let toggle_tools = {
        move |_| {
            tools_visible.update(|value| *value = !*value);
            if !tools_visible.get_untracked() {
                font_open.set(false);
            }
        }
    };
    let trap_keydown = {
        move |event: web_sys::KeyboardEvent| {
            if event.key() == "Tab" {
                event.prevent_default();
                trap_focus(root, &event);
            }
        }
    };
    let toggle_theme = {
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
        let runtime = swipe_runtime.clone();
        let file_id = file.id.clone();
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
    let callback = Closure::once(move || {
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
    })
    .into_js_value();
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
            if name == "blur" {
                flush_progress(state.clone(), id.clone());
            } else {
                persistence::flush_progress_keepalive(&state, &id);
            }
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

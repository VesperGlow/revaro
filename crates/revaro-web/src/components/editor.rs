//! Built-in text/Markdown editor with an in-place recovery sidebar.

use leptos::ev::KeyboardEvent;
use leptos::prelude::*;
use wasm_bindgen::JsCast;
use wasm_bindgen::closure::Closure;

use super::version_history::HistoryPreview;
use crate::logic::{format::format_date, markdown::render_markdown};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EditorMode {
    Edit,
    Split,
    Preview,
}

#[derive(Clone, Default)]
struct EditorPosition {
    top: i32,
    left: i32,
    start: Option<u32>,
    end: Option<u32>,
    direction: Option<String>,
}

/// File loading, saving and ETag protection stay in the browser controller.
#[component]
pub fn DocumentEditor(
    file_id: RwSignal<String>,
    draft_key: Signal<String>,
    etag: RwSignal<String>,
    on_restored: Callback<()>,
    is_new: RwSignal<bool>,
    readonly: RwSignal<bool>,
    name: RwSignal<String>,
    content: RwSignal<String>,
    busy: RwSignal<bool>,
    error: RwSignal<String>,
    dirty: RwSignal<bool>,
    mode: RwSignal<EditorMode>,
    on_save: Callback<()>,
    on_close: Callback<()>,
) -> impl IntoView {
    let history_open = RwSignal::new(false);
    let history_list_active = RwSignal::new(false);
    let history_preview = RwSignal::new(None::<HistoryPreview>);
    let history_confirming = RwSignal::new(false);
    let draft_pending = RwSignal::new(false);
    let history_restoring = RwSignal::new(false);
    let text = NodeRef::<leptos::html::Textarea>::new();
    let markdown_view = NodeRef::<leptos::html::Article>::new();
    let historical_text = NodeRef::<leptos::html::Textarea>::new();
    let text_position = StoredValue::new(EditorPosition::default());
    let markdown_position = StoredValue::new((0_i32, 0_i32));
    let capture_position = Callback::new(move |()| {
        if let Some(text) = text
            .get_untracked()
            .filter(|text| text.get_client_rects().length() > 0)
        {
            text_position.set_value(EditorPosition {
                top: text.scroll_top(),
                left: text.scroll_left(),
                start: text.selection_start().ok().flatten(),
                end: text.selection_end().ok().flatten(),
                direction: text.selection_direction().ok().flatten(),
            });
        }
        if let Some(view) = markdown_view
            .get_untracked()
            .filter(|view| view.get_client_rects().length() > 0)
        {
            markdown_position.set_value((view.scroll_top(), view.scroll_left()));
        }
    });
    let restore_position = Callback::new(move |focus: bool| {
        after_layout(move || {
            if history_preview
                .try_get_untracked()
                .is_none_or(|preview| preview.is_some())
            {
                return;
            }
            let text_visible = text
                .get_untracked()
                .is_some_and(|text| text.get_client_rects().length() > 0);
            if let Some(text) = text
                .get_untracked()
                .filter(|text| text.get_client_rects().length() > 0)
            {
                let position = text_position.get_value();
                if let (Some(start), Some(end)) = (position.start, position.end) {
                    let _ = text.set_selection_range_with_direction(
                        start,
                        end,
                        position.direction.as_deref().unwrap_or("none"),
                    );
                }
                text.set_scroll_top(position.top);
                text.set_scroll_left(position.left);
                if focus {
                    let options = web_sys::FocusOptions::new();
                    options.set_prevent_scroll(true);
                    let _ = text.focus_with_options(&options);
                }
            }
            if let Some(view) = markdown_view
                .get_untracked()
                .filter(|view| view.get_client_rects().length() > 0)
            {
                let (top, left) = markdown_position.get_value();
                view.set_scroll_top(top);
                view.set_scroll_left(left);
                if focus && (mode.get_untracked() == EditorMode::Preview || !text_visible) {
                    let options = web_sys::FocusOptions::new();
                    options.set_prevent_scroll(true);
                    let _ = view.focus_with_options(&options);
                }
            }
        })
    });
    let return_current = Callback::new(move |()| {
        history_preview.set(None);
        history_list_active.set(false);
        restore_position.run(true);
    });
    let close_history = Callback::new(move |()| {
        if history_restoring.get_untracked() || history_confirming.get_untracked() {
            return;
        }
        if history_preview.with_untracked(|preview| preview.is_none()) {
            capture_position.run(());
        }
        history_open.set(false);
        return_current.run(());
    });
    let show_history_list = Callback::new(move |()| {
        if history_preview.with_untracked(|preview| preview.is_none()) {
            capture_position.run(());
        }
        history_open.set(true);
        history_list_active.set(true);
    });
    let toggle_history = Callback::new(move |()| {
        let mobile = web_sys::window()
            .and_then(|window| window.inner_width().ok())
            .and_then(|width| width.as_f64())
            .is_some_and(|width| width <= 850.0);
        if history_open.get_untracked() && (!mobile || history_list_active.get_untracked()) {
            close_history.run(());
        } else {
            show_history_list.run(());
        }
    });
    let view_version = Callback::new(move |preview: HistoryPreview| {
        if history_preview.with_untracked(|preview| preview.is_none()) {
            capture_position.run(());
        }
        history_preview.set(Some(preview));
        history_list_active.set(false);
        after_layout(move || {
            if history_preview
                .try_get_untracked()
                .is_none_or(|preview| preview.is_none())
            {
                return;
            }
            if let Some(text) = historical_text.get_untracked() {
                text.set_scroll_top(0);
                text.set_scroll_left(0);
                let options = web_sys::FocusOptions::new();
                options.set_prevent_scroll(true);
                let _ = text.focus_with_options(&options);
            }
        });
    });
    let switch_mode = Callback::new(move |next: EditorMode| {
        capture_position.run(());
        mode.set(next);
        restore_position.run(false);
    });
    let markdown = Signal::derive_local(move || {
        revaro_core::classify::is_editable_name(&name.get())
            && matches!(name.get().rsplit_once('.').map(|(_, extension)| extension),
                Some(extension) if extension.eq_ignore_ascii_case("md") || extension.eq_ignore_ascii_case("markdown"))
    });
    let bytes = Signal::derive_local(move || content.get().len());

    view! {
        <section class="document-editor" role="dialog" aria-modal="true" aria-labelledby="editor-title"
            on:keydown=move |event: KeyboardEvent| {
                if event.default_prevented() { return; }
                if (event.ctrl_key() || event.meta_key()) && event.key().eq_ignore_ascii_case("s") {
                    event.prevent_default();
                    if !readonly.get_untracked() && !busy.get_untracked() && !history_restoring.get_untracked()
                        && !history_confirming.get_untracked() && !draft_pending.get_untracked() && history_preview.with_untracked(|preview| preview.is_none())
                        && (is_new.get_untracked() || dirty.get_untracked()) {
                        on_save.run(());
                    }
                } else if event.key() == "Escape" && history_open.get_untracked() {
                    event.prevent_default(); event.stop_propagation();
                    if history_restoring.get_untracked() || history_confirming.get_untracked() { return; }
                    if history_preview.with_untracked(|preview| preview.is_some()) { return_current.run(()); } else { close_history.run(()); }
                }
            }>
            <header class="editor-header" inert=move || history_confirming.get()>
                <div class="editor-title">
                    <span aria-hidden="true">"▤"</span>
                    <div>
                        <Show when=move || is_new.get() fallback=move || view! { <strong id="editor-title" title=move || name.get()>{move || name.get()}</strong> }>
                            <input id="editor-title" aria-label="文档文件名" maxlength="1024"
                                prop:value=move || name.get() prop:disabled=move || busy.get() || history_restoring.get() || draft_pending.get()
                                on:input=move |event| name.set(event_target_value(&event)) />
                        </Show>
                        <small>{move || if is_new.get() { "保存在当前文件夹" } else if readonly.get() { "回收站只读预览" } else { "文本编辑器" }}</small>
                    </div>
                    <span class="editor-meta"><b>{move || format!("{} 字节", format_decimal(bytes.get()))}</b><span>" · UTF-8 · 最大 1 MiB"</span></span>
                </div>
                <Show when=move || markdown.get() && !readonly.get() fallback=|| ()>
                    <div class="editor-tabs" role="group" aria-label="编辑器视图">
                        <button class:active=move || mode.get() == EditorMode::Edit aria-pressed=move || (mode.get() == EditorMode::Edit).to_string() disabled=move || history_preview.with(|preview| preview.is_some()) type="button" on:click=move |_| switch_mode.run(EditorMode::Edit)>"编辑"</button>
                        <button class:active=move || mode.get() == EditorMode::Split aria-pressed=move || (mode.get() == EditorMode::Split).to_string() disabled=move || history_preview.with(|preview| preview.is_some()) type="button" on:click=move |_| switch_mode.run(EditorMode::Split)>"分栏"</button>
                        <button class:active=move || mode.get() == EditorMode::Preview aria-pressed=move || (mode.get() == EditorMode::Preview).to_string() disabled=move || history_preview.with(|preview| preview.is_some()) type="button" on:click=move |_| switch_mode.run(EditorMode::Preview)>"预览"</button>
                    </div>
                </Show>
                <div class="editor-actions">
                    <Show when=move || readonly.get() fallback=|| ()><span class="editor-header-message">"只读"</span></Show>
                    <Show when=move || !readonly.get() && !is_new.get() fallback=|| ()>
                        <button class="secondary editor-history-trigger" type="button" aria-controls="version-history" aria-expanded=move || history_open.get().to_string() prop:disabled=move || busy.get() || draft_pending.get() on:click=move |_| toggle_history.run(())>"版本历史"</button>
                    </Show>
                    <Show when=move || !readonly.get() fallback=|| ()>
                        <Show when=move || is_new.get() || dirty.get() fallback=|| ()><span class="unsaved-dot">"未保存"</span></Show>
                        <button class="primary" type="button" prop:disabled=move || busy.get() || history_restoring.get() || draft_pending.get() || history_preview.with(|preview| preview.is_some()) || (!is_new.get() && !dirty.get()) on:click=move |_| on_save.run(())>{move || if busy.get() { "保存中…" } else { "保存" }}</button>
                    </Show>
                    <button class="editor-close" type="button" prop:disabled=move || history_restoring.get() aria-label="关闭编辑器" on:click=move |_| on_close.run(())>"×"</button>
                </div>
            </header>
            <div class="editor-notices" inert=move || history_confirming.get()>
                <Show when=move || !error.get().is_empty() fallback=|| ()><p class="editor-notice-error" role="alert">{move || error.get()}</p></Show>
                <super::editor_draft::DraftRecovery draft_key=draft_key name=name content=content dirty=dirty busy=busy readonly=readonly draft_pending=draft_pending />
            </div>
            <div class="editor-body" class:history-open=move || history_open.get() class:history-list-active=move || history_list_active.get() inert=move || history_confirming.get()>
                <div class="editor-mobile-views" role="group" aria-label="编辑器内容">
                    <button class="secondary" type="button" aria-pressed=move || (!history_list_active.get()).to_string() on:click=move |_| { history_list_active.set(false); if history_preview.with_untracked(|preview| preview.is_none()) { restore_position.run(true); } }>{move || if history_preview.with(|preview| preview.is_some()) { "历史版本 · 只读" } else { "当前文档" }}</button>
                    <button class="secondary" type="button" aria-pressed=move || history_list_active.get().to_string() on:click=move |_| show_history_list.run(())>"历史记录"</button>
                </div>
                <main class="editor-main">
                    <div hidden=move || history_preview.with(|preview| preview.is_some()) class=move || format!("editor-workspace editor-current mode-{}{}", mode_class(mode.get()), if markdown.get() { " markdown" } else { "" })>
                        <textarea node_ref=text hidden=move || markdown.get() && mode.get() == EditorMode::Preview
                            readonly=move || readonly.get() || history_restoring.get() || history_confirming.get() || draft_pending.get() || (busy.get() && content.get().is_empty())
                            autofocus spellcheck="false" aria-label="文档内容" prop:value=move || content.get()
                            on:scroll=move |_| { if history_preview.with_untracked(|preview| preview.is_none()) { capture_position.run(()); } }
                            on:input=move |event| { content.set(event_target_value(&event)); dirty.set(true); }></textarea>
                        <article node_ref=markdown_view hidden=move || !markdown.get() || mode.get() == EditorMode::Edit
                            class="markdown-preview" aria-label="Markdown 预览" tabindex="0"
                            on:scroll=move |_| { if history_preview.with_untracked(|preview| preview.is_none()) { capture_position.run(()); } }
                            inner_html=move || if markdown.get() && mode.get() != EditorMode::Edit { render_markdown(&content.get()) } else { String::new() }></article>
                    </div>
                    <section class="editor-history-preview" hidden=move || history_preview.with(|preview| preview.is_none()) aria-label="历史版本预览">
                        <header><div><strong>"历史版本 · 只读"</strong><time>{move || history_preview.with(|preview| preview.as_ref().map(|preview| format_date(&preview.version.created_at.to_rfc3339())).unwrap_or_default())}</time></div><button class="secondary" type="button" on:click=move |_| return_current.run(())>"返回当前文档"</button></header>
                        <textarea node_ref=historical_text readonly spellcheck="false" aria-label="历史版本内容" prop:value=move || history_preview.with(|preview| preview.as_ref().map(|preview| preview.content.clone()).unwrap_or_default())></textarea>
                    </section>
                    <Show when=move || busy.get() && content.get().is_empty() fallback=|| ()>
                        <div class="state editor-loading" role="status"><div class="spinner"></div><p>"正在打开文档…"</p></div>
                    </Show>
                </main>
                <Show when=move || history_open.get() fallback=|| ()>
                    <super::version_history::VersionHistory file_id=file_id.get_untracked() etag=etag content=content dirty=dirty editor_busy=history_restoring confirming=history_confirming preview=history_preview on_preview=view_version on_restored=on_restored on_close=close_history />
                </Show>
            </div>
        </section>
    }
}

pub(super) fn after_layout(callback: impl FnOnce() + 'static) {
    if let Some(window) = web_sys::window() {
        let callback = Closure::once(callback).into_js_value();
        let _ = window.request_animation_frame(callback.unchecked_ref());
    }
}

fn mode_class(mode: EditorMode) -> &'static str {
    match mode {
        EditorMode::Edit => "edit",
        EditorMode::Split => "split",
        EditorMode::Preview => "preview",
    }
}

fn format_decimal(value: usize) -> String {
    let digits = value.to_string();
    let mut formatted = String::with_capacity(digits.len() + digits.len() / 3);
    for (index, digit) in digits.bytes().enumerate() {
        if index > 0 && (digits.len() - index).is_multiple_of(3) {
            formatted.push(',');
        }
        formatted.push(digit as char);
    }
    formatted
}

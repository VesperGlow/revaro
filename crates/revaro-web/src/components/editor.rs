//! Built-in text/Markdown editor.

use leptos::ev::KeyboardEvent;
use leptos::prelude::*;

use crate::logic::markdown::render_markdown;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EditorMode {
    Edit,
    Split,
    Preview,
}

/// The reference editor workspace. File loading and saving stay in the
/// browser controller; this component owns only the visual/editor state.
#[component]
pub fn DocumentEditor(
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
    let markdown = Signal::derive_local(move || {
        revaro_core::classify::is_editable_name(&name.get())
            && matches!(
                name.get().rsplit_once('.').map(|(_, extension)| extension),
                Some(extension) if extension.eq_ignore_ascii_case("md") || extension.eq_ignore_ascii_case("markdown")
            )
    });
    let bytes = Signal::derive_local(move || content.get().len());
    let close = on_close.clone();
    let save = on_save.clone();

    view! {
        <section class="document-editor" role="dialog" aria-modal="true" aria-labelledby="editor-title">
            <header class="editor-header">
                <div class="editor-title">
                    <span aria-hidden="true">"▤"</span>
                    <div>
                        <Show
                            when=move || is_new.get()
                            fallback=move || view! { <strong id="editor-title" title=move || name.get()>{move || name.get()}</strong> }
                        >
                            <input
                                aria-label="文档文件名"
                                maxlength="1024"
                                prop:value=move || name.get()
                                on:input=move |event| name.set(event_target_value(&event))
                            />
                        </Show>
                        <small>{move || if is_new.get() { "保存在当前文件夹" } else if readonly.get() { "回收站只读预览" } else { "文本编辑器" }}</small>
                    </div>
                    <span class="editor-meta"><b>{move || format!("{} 字节", format_decimal(bytes.get()))}</b><span>" · UTF-8 · 最大 1 MiB"</span></span>
                </div>
                <Show when=move || markdown.get() && !readonly.get() fallback=|| ()>
                    <div class="editor-tabs" role="group" aria-label="编辑器视图">
                        <button class:active=move || mode.get() == EditorMode::Edit type="button" on:click=move |_| mode.set(EditorMode::Edit)>"编辑"</button>
                        <button class:active=move || mode.get() == EditorMode::Split type="button" on:click=move |_| mode.set(EditorMode::Split)>"分栏"</button>
                        <button class:active=move || mode.get() == EditorMode::Preview type="button" on:click=move |_| mode.set(EditorMode::Preview)>"预览"</button>
                    </div>
                </Show>
                <div class="editor-actions">
                    <Show
                        when=move || !error.get().is_empty()
                        fallback=move || view! {
                            <Show when=move || readonly.get() fallback=move || view! {
                                <Show when=move || is_new.get() || dirty.get() fallback=|| ()><span class="unsaved-dot">"未保存"</span></Show>
                                <button class="primary" type="button" prop:disabled=move || busy.get() || (!is_new.get() && !dirty.get()) on:click=move |_| save.run(())>{move || if busy.get() { "保存中…" } else { "保存" }}</button>
                            }>
                                <span class="editor-header-message">"只读"</span>
                            </Show>
                        }
                    >
                        <span class="editor-header-message error">{move || error.get()}</span>
                    </Show>
                    <button class="editor-close" type="button" aria-label="关闭编辑器" on:click=move |_| close.run(())>"×"</button>
                </div>
            </header>
            <Show
                when=move || !(busy.get() && content.get().is_empty())
                fallback=|| view! { <div class="state editor-loading"><div class="spinner"></div><p>"正在打开文档…"</p></div> }
            >
                <div class=move || format!("editor-workspace mode-{}{}", mode_class(mode.get()), if markdown.get() { " markdown" } else { "" })>
                    <Show when=move || mode.get() != EditorMode::Preview fallback=|| ()>
                        <textarea
                            readonly=move || readonly.get()
                            autofocus
                            spellcheck="false"
                            aria-label="文档内容"
                            prop:value=move || content.get()
                            on:input=move |event| {
                                content.set(event_target_value(&event));
                                dirty.set(true);
                            }
                            on:keydown=move |event: KeyboardEvent| {
                                if (event.ctrl_key() || event.meta_key()) && event.key() == "s" {
                                    event.prevent_default();
                                    save.run(());
                                }
                            }
                        ></textarea>
                    </Show>
                    <Show when=move || markdown.get() && mode.get() != EditorMode::Edit fallback=|| ()>
                        <article class="markdown-preview" inner_html=move || render_markdown(&content.get())></article>
                    </Show>
                </div>
            </Show>
        </section>
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

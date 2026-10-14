//! Document recovery sidebar, scoped to an open editor session.
use super::dialogs::ActionDialog;
use super::icons;
use crate::api;
use crate::logic::format::{format_date, format_size};
use leptos::prelude::*;
use revaro_core::features::DocumentVersion;

#[derive(Clone)]
pub(super) struct HistoryPreview {
    pub version: DocumentVersion,
    pub content: String,
}

#[component]
pub(super) fn VersionHistory(
    file_id: String,
    etag: RwSignal<String>,
    content: RwSignal<String>,
    dirty: RwSignal<bool>,
    editor_busy: RwSignal<bool>,
    confirming: RwSignal<bool>,
    preview: RwSignal<Option<HistoryPreview>>,
    on_preview: Callback<HistoryPreview>,
    on_restored: Callback<()>,
    on_close: Callback<()>,
) -> impl IntoView {
    on_cleanup(move || {
        let _ = editor_busy.try_set(false);
        let _ = confirming.try_set(false);
    });
    let alive = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(true));
    let alive_cleanup = alive.clone();
    on_cleanup(move || alive_cleanup.store(false, std::sync::atomic::Ordering::Relaxed));
    let versions = RwSignal::new(Vec::<DocumentVersion>::new());
    let error = RwSignal::new(String::new());
    let busy = RwSignal::new(false);
    let pending = RwSignal::new(None::<DocumentVersion>);
    let restore_trigger = StoredValue::new(None::<NodeRef<leptos::html::Button>>);
    Effect::new(move |_| confirming.set(pending.get().is_some()));
    let close = Callback::new(move |()| {
        if !editor_busy.get_untracked() && pending.get_untracked().is_none() {
            on_close.run(());
        }
    });
    let load_id = file_id.clone();
    let load = Callback::new(move |()| {
        if busy.get_untracked() {
            return;
        }
        let id = load_id.clone();
        busy.set(true);
        error.set(String::new());
        leptos::task::spawn_local(async move {
            match api::fetch_versions(&id).await {
                Ok(result) => {
                    let _ = versions.try_set(result);
                }
                Err(e) => {
                    let _ = error.try_set(e.message);
                }
            }
            let _ = busy.try_set(false);
        });
    });
    let loaded_etag = RwSignal::new(None::<String>);
    Effect::new(move |_| {
        let current = etag.get();
        if !busy.get() && loaded_etag.get_untracked().as_ref() != Some(&current) {
            loaded_etag.set(Some(current));
            load.run(());
        }
    });
    let restore_id = file_id.clone();
    let alive_restore = alive.clone();
    let restore = Callback::new(move |_: String| {
        if busy.get_untracked() || dirty.get_untracked() {
            return;
        }
        let Some(version) = pending.get_untracked() else {
            return;
        };
        let alive = alive_restore.clone();
        let id = restore_id.clone();
        let current = etag.get_untracked();
        let submitted_content = content.get_untracked();
        busy.set(true);
        editor_busy.set(true);
        error.set(String::new());
        leptos::task::spawn_local(async move {
            let result = async {
                let file = api::restore_version(&id, &version.id, &current).await?;
                let doc = api::fetch_document(&id).await?;
                Ok::<_, api::RequestError>((file, doc))
            }
            .await;
            if !alive.load(std::sync::atomic::Ordering::Relaxed) {
                return;
            }
            match result {
                Ok((_file, doc)) => {
                    if content.get_untracked() != submitted_content {
                        etag.set(doc.etag);
                        dirty.set(true);
                        error.set("版本已恢复；等待期间的新修改已保留，请检查后保存。".to_owned());
                        pending.set(None);
                    } else if content.try_set(doc.content).is_none() {
                        etag.set(doc.etag);
                        dirty.set(false);
                        pending.set(None);
                        confirming.set(false);
                        editor_busy.set(false);
                        on_restored.run(());
                        on_close.run(());
                    }
                }
                Err(e) => {
                    let _ = error.try_set(e.message);
                }
            }
            let _ = busy.try_set(false);
            let _ = editor_busy.try_set(false);
        });
    });
    let alive_cancel = alive.clone();
    let cancel = Callback::new(move |()| {
        if !busy.get_untracked() {
            confirming.set(false);
            pending.set(None);
            error.set(String::new());
            let alive = alive_cancel.clone();
            super::editor::after_layout(move || {
                if alive.load(std::sync::atomic::Ordering::Relaxed)
                    && confirming.try_get_untracked() == Some(false)
                    && let Some(button) = restore_trigger
                        .get_value()
                        .and_then(|button| button.get_untracked())
                    && button.is_connected()
                {
                    let options = web_sys::FocusOptions::new();
                    options.set_prevent_scroll(true);
                    let _ = button.focus_with_options(&options);
                }
            });
        }
    });
    view! {
        <aside id="version-history" class="version-history" aria-labelledby="version-history-title">
            <header>
                <div><h2 id="version-history-title">"版本历史"</h2><p>"最近 20 次保存前的内容"</p></div>
                <button class="editor-close" type="button" aria-label="关闭版本历史" disabled=move || editor_busy.get() on:click=move |_| close.run(())>{icons::x()}</button>
            </header>
            <div class="history-list" aria-label="历史版本" aria-busy=move || busy.get().to_string()>
                <Show when=move || versions.get().is_empty() && error.get().is_empty() fallback=|| ()>
                    <div class="history-state" role="status"><Show when=move || busy.get() fallback=|| ()><div class="spinner"></div></Show><p>{move || if busy.get() { "正在加载版本…" } else { "暂无历史版本" }}</p></div>
                </Show>
                <For each=move || versions.get() key=|version| version.id.clone() children=move |version| {
                    let id = file_id.clone();
                    let view_version = version.clone();
                    let restore_version = version.clone();
                    let row_id = version.id.clone();
                    let timestamp = version.created_at.to_rfc3339();
                    let alive_view = alive.clone();
                    let restore_button = NodeRef::<leptos::html::Button>::new();
                    view! {
                        <div class="history-row" class:active=move || preview.with(|p| p.as_ref().is_some_and(|p| p.version.id == row_id))>
                            <div class="history-meta"><time datetime=timestamp.clone()>{format_date(&timestamp)}</time><small>{format_size(version.size.max(0) as u64)}</small></div>
                            <div class="history-actions">
                                <button class="secondary" type="button" disabled=move || busy.get() on:click=move |_| {
                                    let id = id.clone();
                                    let version = view_version.clone();
                                    let alive = alive_view.clone();
                                    busy.set(true);
                                    error.set(String::new());
                                    leptos::task::spawn_local(async move {
                                        let result = api::fetch_version_content(&id, &version.id).await;
                                        if !alive.load(std::sync::atomic::Ordering::Relaxed) { return; }
                                        match result {
                                            Ok(text) => on_preview.run(HistoryPreview { version, content: text }),
                                            Err(e) => { let _ = error.try_set(e.message); }
                                        }
                                        let _ = busy.try_set(false);
                                    });
                                }>"查看"</button>
                                <button node_ref=restore_button class="secondary" type="button" disabled=move || busy.get() || dirty.get() on:click=move |_| {
                                    restore_trigger.set_value(Some(restore_button));
                                    error.set(String::new());
                                    pending.set(Some(restore_version.clone()));
                                }>"恢复此版本"</button>
                            </div>
                        </div>
                    }
                }/>
            </div>
            <Show when=move || !error.get().is_empty() && pending.get().is_none() fallback=|| ()><div class="history-feedback" role="alert"><span>{move || error.get()}</span><Show when=move || versions.get().is_empty() fallback=|| ()><button class="secondary" type="button" disabled=move || busy.get() on:click=move |_| load.run(())>"重试"</button></Show></div></Show>
            <footer><p>{move || if dirty.get() { "请先保存当前修改，再恢复历史版本。" } else { "查看不会修改文档，恢复前会保留当前版本。" }}</p></footer>
        </aside>
        <Show when=move || pending.get().is_some() fallback=|| ()>
            <leptos::portal::Portal>
                <ActionDialog title="恢复历史版本？".to_owned()
                    message=pending.get_untracked().map(|v| format!("将文档恢复到 {} 的版本。当前内容会保存到版本历史中，可再次恢复。", format_date(&v.created_at.to_rfc3339()))).unwrap_or_default()
                    confirm_label="确认恢复".to_owned() danger=false input=false placeholder=None value=RwSignal::new(String::new()) busy=busy error=error
                    on_cancel=cancel on_confirm=restore />
            </leptos::portal::Portal>
        </Show>
    }
}

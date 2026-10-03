//! Compact topbar disclosures for public links and storage/runtime status.
use super::menu::{ActionMenu, MenuIcon};
use crate::api::{self, SHARE_PAGE_SIZE, SystemStatusSummary};
use crate::logic::format::{format_date, format_size};
use leptos::prelude::*;
use revaro_core::features::ShareEntry;

#[component]
pub fn PublicLinks(context: Signal<String>) -> impl IntoView {
    let shares = RwSignal::new(Vec::<ShareEntry>::new());
    let error = RwSignal::new(String::new());
    let offset = RwSignal::new(0_i64);
    let has_next = RwSignal::new(false);
    let busy = RwSignal::new(false);
    let loaded = RwSignal::new(false);
    let copied = RwSignal::new(None::<String>);
    let refresh = Callback::new(move |mut page: i64| {
        if busy.get_untracked() {
            return;
        }
        busy.set(true);
        error.set(String::new());
        copied.set(None);
        leptos::task::spawn_local(async move {
            loop {
                match api::fetch_shares(page).await {
                    Ok(result) if result.items.is_empty() && page > 0 => {
                        page = (page - SHARE_PAGE_SIZE).max(0)
                    }
                    Ok(result) => {
                        let _ = shares.try_set(result.items);
                        let _ = offset.try_set(page);
                        let _ = has_next.try_set(result.has_next);
                        let _ = loaded.try_set(true);
                        break;
                    }
                    Err(e) => {
                        let _ = error.try_set(e.message);
                        break;
                    }
                }
            }
            let _ = busy.try_set(false);
        });
    });
    let revoke = Callback::new(move |id: String| {
        if busy.get_untracked() {
            return;
        }
        busy.set(true);
        error.set(String::new());
        leptos::task::spawn_local(async move {
            match api::revoke_share(&id).await {
                Ok(()) => {
                    let _ = busy.try_set(false);
                    if let Some(page) = offset.try_get_untracked() {
                        refresh.run(page);
                    }
                }
                Err(e) => {
                    let _ = error.try_set(e.message);
                    let _ = busy.try_set(false);
                }
            }
        });
    });
    let copy = Callback::new(move |(id, url): (String, String)| {
        let Some(window) = web_sys::window() else {
            return;
        };
        let promise = window.navigator().clipboard().write_text(&url);
        leptos::task::spawn_local(async move {
            if wasm_bindgen_futures::JsFuture::from(promise).await.is_ok() {
                let _ = copied.try_set(Some(id));
            } else {
                let _ = error.try_set("复制失败，请选择链接后手动复制".to_owned());
            }
        });
    });
    view! {
        <ActionMenu label="公开链接".to_owned() icon=MenuIcon::Link context=context panel_class="topbar-popover public-links-popover"
            on_toggle=Callback::new(move |open| { if open { refresh.run(0); } })>
            <header class="popover-heading"><h2>"公开链接"</h2><button class="popover-text-action" type="button" disabled=move || busy.get() on:click=move |_| refresh.run(offset.get_untracked())>"刷新"</button></header>
            <Show when=move || !error.get().is_empty() fallback=|| ()><p class="form-error" role="alert">{move || error.get()}</p></Show>
            <Show when=move || !loaded.get() && busy.get() fallback=|| ()><p class="popover-empty" role="status">"正在读取公开链接…"</p></Show>
            <Show when=move || loaded.get() && !busy.get() && shares.get().is_empty() && error.get().is_empty() fallback=|| ()><p class="popover-empty">"暂无公开链接"</p></Show>
            <div class="public-links-list" aria-busy=move || busy.get().to_string()>
                <For each=move || shares.get() key=|s| (s.file_id.clone(), s.name.clone(), s.status.active, s.status.url.clone(), s.status.expires_at)
                    children=move |share| {
                        let id = share.file_id;
                        let copy_id = id.clone();
                        let copied_id = id.clone();
                        let url = share.status.url.unwrap_or_default();
                        let can_copy = share.status.active && !url.is_empty();
                        view! {
                            <article class="public-link-row">
                                <strong title=share.name.clone()>{share.name.clone()}</strong>
                                <p><span class:link-active=share.status.active>{if share.status.active { "有效" } else { "已到期" }}</span>" · "{share.status.expires_at.map(|t| format!("{} 到期", format_date(&t.to_rfc3339()))).unwrap_or_else(|| "永久有效".to_owned())}</p>
                                <input aria-label="公开链接地址" readonly value=url.clone() on:focus=|ev| { let _ = event_target::<web_sys::HtmlInputElement>(&ev).select(); } />
                                <div class="popover-row-actions">
                                    <button class="secondary" type="button" disabled=move || !can_copy || busy.get() on:click=move |_| copy.run((copy_id.clone(), url.clone()))>{move || if copied.get().as_ref() == Some(&copied_id) { "已复制" } else { "复制链接" }}</button>
                                    <button class="secondary" type="button" disabled=move || busy.get() on:click=move |_| revoke.run(id.clone())>"撤销链接"</button>
                                </div>
                            </article>
                        }
                    } />
            </div>
            <Show when=move || !shares.get().is_empty() && (offset.get() > 0 || has_next.get()) fallback=|| ()>
                <footer class="popover-row-actions">
                    <button class="secondary" type="button" disabled=move || busy.get() || offset.get() == 0 on:click=move |_| refresh.run((offset.get_untracked() - SHARE_PAGE_SIZE).max(0))>"上一页"</button>
                    <button class="secondary" type="button" disabled=move || busy.get() || !has_next.get() on:click=move |_| refresh.run(offset.get_untracked() + SHARE_PAGE_SIZE)>"下一页"</button>
                </footer>
            </Show>
        </ActionMenu>
    }
}

#[component]
pub fn SystemStatus(context: Signal<String>) -> impl IntoView {
    let status = RwSignal::new(None::<SystemStatusSummary>);
    let loading = RwSignal::new(false);
    let error = RwSignal::new(String::new());
    let notice = RwSignal::new(String::new());
    let refresh = Callback::new(move |()| {
        if loading.get_untracked() {
            return;
        }
        loading.set(true);
        error.set(String::new());
        leptos::task::spawn_local(async move {
            match api::fetch_system_status().await {
                Ok(summary) => {
                    let _ = status.try_set(Some(summary));
                }
                Err(e) => {
                    let _ = error.try_set(e.message);
                }
            }
            let _ = loading.try_set(false);
        });
    });
    let usage = Signal::derive(move || {
        status.get().filter(|s| s.disk_total_bytes > 0).map(|s| {
            (s.disk_used_bytes as f64 / s.disk_total_bytes as f64 * 100.0).clamp(0.0, 100.0)
        })
    });
    refresh.run(());
    view! {
        <ActionMenu label="系统状态".to_owned() icon=MenuIcon::SystemStatus usage=usage context=context panel_class="topbar-popover system-status-popover"
            on_toggle=Callback::new(move |open| { if open { refresh.run(()); } })>
            <header class="popover-heading"><h2>"系统状态"</h2><button class="popover-text-action" type="button" disabled=move || loading.get() on:click=move |_| refresh.run(())>{move || if loading.get() { "刷新中…" } else { "刷新状态" }}</button></header>
            <div class="system-metrics" role="status" aria-busy=move || loading.get().to_string()>
                {move || {
                    let summary = status.get();
                    let values = summary.as_ref().map(|s| [
                        format!("{} / {}", format_size(s.disk_used_bytes), format_size(s.disk_total_bytes)),
                        format_size(s.disk_available_bytes), format_size(s.cache.memory_bytes), format_size(s.cache.disk_bytes), s.active_tasks.to_string(),
                    ]);
                    ["已用 / 总存储空间", "可用空间", "内存缓存", "磁盘缓存", "活动任务"].into_iter().enumerate().map(|(index, label)| view! {
                        <div class="system-metric"><span>{label}</span><strong class:metric-skeleton=values.is_none()>{values.as_ref().map(|v| v[index].clone()).unwrap_or_else(|| "—".to_owned())}</strong></div>
                    }).collect_view()
                }}
            </div>
            <Show when=move || !error.get().is_empty() fallback=|| ()><p class="form-error" role="alert">{move || error.get()}</p></Show>
            <footer class="popover-row-actions">
                <button class="secondary" type="button" on:click=move |_| { super::reader_cache::clear_all(); notice.set("已清理本机阅读缓存".to_owned()); }>"清理阅读缓存"</button>
            </footer>
            <Show when=move || !notice.get().is_empty() fallback=|| ()><p class="popover-notice" role="status">{move || notice.get()}</p></Show>
        </ActionMenu>
    }
}

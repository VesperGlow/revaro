//! Compact topbar disclosures for public links and storage/runtime status.
use super::{
    icons,
    menu::{ActionMenu, MenuIcon},
};
use crate::api::{self, SHARE_PAGE_SIZE, SystemStatusSummary};
use crate::logic::{
    format::{format_date, format_size},
    system::{server_metrics, storage_usage},
};
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
        <ActionMenu label="公开链接".to_owned() icon=MenuIcon::Link context=context embedded=true panel_class="topbar-popover public-links-popover"
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
    let server = NodeRef::<leptos::html::Details>::new();
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
    let usage = Signal::derive(move || storage_usage(status.get().as_ref()));
    refresh.run(());
    view! {
        <ActionMenu label="系统状态".to_owned() icon=MenuIcon::SystemStatus usage=usage context=context panel_class="topbar-popover system-status-popover"
            on_toggle=Callback::new(move |open| {
                if open { refresh.run(()); }
                else if let Some(server) = server.get_untracked() { server.set_open(false); }
            })>
            <header class="popover-heading"><h2>"系统状态"</h2><button class="popover-text-action" type="button" disabled=move || loading.get() on:click=move |_| refresh.run(())>{move || if loading.get() { "刷新中…" } else { "刷新状态" }}</button></header>
            <section class="storage-status" aria-label="存储空间" aria-busy=move || loading.get().to_string()>
                <div class="storage-status-heading"><h3>"存储空间"</h3><strong>{move || usage.get().map(|value| format!("{value:.0}%")).unwrap_or_else(|| "—".to_owned())}</strong></div>
                <div class="storage-usage-track" role="progressbar" aria-label="存储空间使用率" aria-valuemin="0" aria-valuemax="100"
                    aria-valuenow=move || usage.get().map(|value| format!("{value:.1}"))
                    aria-valuetext=move || usage.get().map(|value| format!("已用 {value:.0}%")).unwrap_or_else(|| "暂无法读取存储空间".to_owned())>
                    <div class="storage-usage-fill" style:width=move || format!("{}%", usage.get().unwrap_or(0.0))></div>
                </div>
                <dl class="storage-capacities">
                    {move || {
                        let summary = status.get();
                        let pending = summary.is_none() && loading.get();
                        let values = summary.as_ref().map(|s| [format_size(s.disk_used_bytes), format_size(s.disk_total_bytes), format_size(s.disk_available_bytes)]);
                        ["已用", "总容量", "剩余"].into_iter().enumerate().map(|(index, label)| view! {
                            <div><dt>{label}</dt><dd class:metric-skeleton=pending>{values.as_ref().map(|values| values[index].clone()).unwrap_or_else(|| "—".to_owned())}</dd></div>
                        }).collect_view()
                    }}
                </dl>
            </section>
            <Show when=move || !error.get().is_empty() fallback=|| ()><p class="form-error" role="alert">{move || error.get()}</p></Show>
            <details class="server-status" node_ref=server>
                <summary><span>"服务器状态"</span>{icons::chevron_down()}</summary>
                <div class="server-status-body">
                    <div class="system-metrics" role="status" aria-busy=move || loading.get().to_string()>
                        {move || {
                            let summary = status.get();
                            server_metrics(summary.as_ref()).into_iter().map(|metric| {
                                let pending = metric.value.is_none() && loading.get();
                                let value = metric.value.unwrap_or_else(|| "—".to_owned());
                                view! {
                                    <div class="system-metric" title=metric.title><span>{metric.label}</span><strong class:metric-skeleton=pending>{value}</strong></div>
                                }
                            }).collect_view()
                        }}
                    </div>
                    <footer class="popover-row-actions">
                        <button class="secondary" type="button" on:click=move |_| { super::reader_cache::clear_all(); notice.set("已清理本机阅读缓存".to_owned()); }>"清理阅读缓存"</button>
                    </footer>
                    <Show when=move || !notice.get().is_empty() fallback=|| ()><p class="popover-notice" role="status">{move || notice.get()}</p></Show>
                </div>
            </details>
        </ActionMenu>
    }
}

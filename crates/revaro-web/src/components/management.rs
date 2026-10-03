//! Share overview and a compact runtime summary in account settings.
use crate::api::{self, SHARE_PAGE_SIZE, SystemStatusSummary};
use leptos::prelude::*;
use revaro_core::features::ShareEntry;

#[component]
pub fn Management() -> impl IntoView {
    let shares = RwSignal::new(Vec::<ShareEntry>::new());
    let error = RwSignal::new(String::new());
    let status = RwSignal::new(None::<SystemStatusSummary>);
    let status_loading = RwSignal::new(true);
    let status_failed = RwSignal::new(false);
    let offset = RwSignal::new(0_i64);
    let has_next = RwSignal::new(false);
    let busy = RwSignal::new(false);
    let refresh = Callback::new(move |mut page: i64| {
        if busy.try_get_untracked() != Some(false) {
            return;
        }
        busy.set(true);
        error.set(String::new());
        status_loading.set(true);
        status_failed.set(false);
        leptos::task::spawn_local(async move {
            let links_request = async {
                loop {
                    match api::fetch_shares(page).await {
                        Ok(result) if result.items.is_empty() && page > 0 => {
                            // Revoking the last link on a page returns to an
                            // available page rather than stranding the user.
                            page = (page - SHARE_PAGE_SIZE).max(0);
                        }
                        Ok(result) => {
                            let _ = shares.try_set(result.items);
                            let _ = offset.try_set(page);
                            let _ = has_next.try_set(result.has_next);
                            break;
                        }
                        Err(e) => {
                            let _ = error.try_set(e.message);
                            break;
                        }
                    }
                }
            };
            let status_request = async {
                match api::fetch_system_status().await {
                    Ok(summary) => {
                        let _ = status.try_set(Some(summary));
                    }
                    Err(_) => {
                        let _ = status_failed.try_set(true);
                    }
                }
                let _ = status_loading.try_set(false);
            };
            futures_util::join!(links_request, status_request);
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
                    if busy.try_set(false).is_none()
                        && let Some(page) = offset.try_get_untracked()
                    {
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
    refresh.run(0);
    view! {
        <section class="management" aria-label="分享管理与运行状态">
            <h3>"分享管理与运行状态"</h3>
            <div class="management-status" role="status" aria-busy=move || status_loading.get().to_string()>
                <Show when=move || !status_loading.get() fallback=move || view! {
                    <div class="status-metrics status-skeleton" aria-label="正在获取运行状态">
                        {(0..4).map(|_| view! { <span aria-hidden="true"><i></i><i></i></span> }).collect_view()}
                    </div>
                }>
                    <Show when=move || !status_failed.get() fallback=|| view! { <p class="status-result">"状态获取失败"</p> }>
                        {move || status.get().map(|summary| view! {
                            <div class="status-metrics status-result">
                                <span><small>"磁盘可用空间"</small><b>{format!("{:.1} GiB", summary.disk_available_bytes as f64 / 1073741824.0)}</b></span>
                                <span><small>"内存缓存"</small><b>{format!("{:.1} MiB", summary.cache.memory_bytes as f64 / 1048576.0)}</b></span>
                                <span><small>"磁盘缓存"</small><b>{format!("{:.1} MiB", summary.cache.disk_bytes as f64 / 1048576.0)}</b></span>
                                <span><small>"活动任务"</small><b>{summary.active_tasks}</b></span>
                            </div>
                        })}
                    </Show>
                </Show>
            </div>
            <div class="management-actions">
                <button class="secondary" type="button" disabled=move || busy.get()
                    on:click=move |_| refresh.run(offset.get_untracked())>"刷新状态"</button>
                <button class="secondary" type="button"
                    on:click=move |_| super::reader_cache::clear_all()>"清理阅读缓存"</button>
            </div>
            <Show when=move || !error.get().is_empty() fallback=|| ()>
                <p class="form-error" role="alert">{move || error.get()}</p>
            </Show>
            <h3>"公开链接"</h3>
            <Show when=move || shares.get().is_empty() fallback=|| ()>
                <p>"暂无公开链接"</p>
            </Show>
            <For each=move || shares.get()
                key=|s| (s.file_id.clone(), s.name.clone(), s.status.active, s.status.url.clone(), s.status.expires_at)
                children=move |share| {
                    let id = share.file_id;
                    view! {
                        <div class="history-row">
                            <div>
                                <strong>{share.name}</strong>
                                <p>{if share.status.active { "有效" } else { "已到期" }}" · "
                                    {share.status.expires_at.map(|t| t.to_rfc3339()).unwrap_or_else(|| "永久".to_owned())}</p>
                                <input aria-label="公开链接" readonly value=share.status.url.unwrap_or_default()/>
                            </div>
                            <button class="secondary" type="button" disabled=move || busy.get()
                                on:click=move |_| revoke.run(id.clone())>"撤销链接"</button>
                        </div>
                    }
                }
            />
            <Show when=move || !shares.get().is_empty() && (offset.get() > 0 || has_next.get()) fallback=|| ()>
                <div class="management-pagination">
                    <button class="secondary" type="button" disabled=move || busy.get() || offset.get() == 0
                        on:click=move |_| refresh.run((offset.get_untracked() - SHARE_PAGE_SIZE).max(0))>"上一页"</button>
                    <button class="secondary" type="button" disabled=move || busy.get() || !has_next.get()
                        on:click=move |_| refresh.run(offset.get_untracked() + SHARE_PAGE_SIZE)>"下一页"</button>
                </div>
            </Show>
        </section>
    }
}

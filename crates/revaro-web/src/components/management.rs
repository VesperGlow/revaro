//! Share overview and runtime health in account settings.
use crate::api;
use leptos::prelude::*;
use revaro_core::features::ShareEntry;
#[component]
pub fn Management() -> impl IntoView {
    let shares = RwSignal::new(Vec::<ShareEntry>::new());
    let error = RwSignal::new(String::new());
    let status = RwSignal::new(None::<serde_json::Value>);
    let offset = RwSignal::new(0_i64);
    let busy = RwSignal::new(false);
    let refresh = Callback::new(move |_: ()| {
        busy.set(true);
        error.set(String::new());
        let page = offset.get_untracked();
        leptos::task::spawn_local(async move {
            match api::fetch_shares(page).await {
                Ok(v) => {
                    let _ = shares.try_set(v);
                }
                Err(e) => {
                    let _ = error.try_set(e.message);
                }
            }
            match api::fetch_system_status().await {
                Ok(v) => {
                    let _ = status.try_set(Some(v));
                }
                Err(e) => {
                    let _ = error.try_set(e.message);
                }
            }
            let _ = busy.try_set(false);
        });
    });
    refresh.run(());
    view! { <details class="management"><summary>"分享管理与运行状态"</summary>
        <button type="button" prop:disabled=move||busy.get() on:click=move |_|refresh.run(())>"刷新状态"</button>
        <button type="button" on:click=move |_|super::reader_cache::clear_all()>"清理本机阅读缓存"</button>
        <Show when=move||!error.get().is_empty() fallback=||()><p class="form-error">{move||error.get()}</p></Show>
        {move||status.get().map(|s|view!{<p>{format!("磁盘可用 {:.1} GiB · 内存缓存 {:.1} MiB · 磁盘缓存 {:.1} MiB · 活动任务 {}",s["disk_available_bytes"].as_f64().unwrap_or_default()/1073741824.0,s["cache"]["memory_bytes"].as_f64().unwrap_or_default()/1048576.0,s["cache"]["disk_bytes"].as_f64().unwrap_or_default()/1048576.0,s["active_tasks"])}</p><details><summary>"缓存与维护详情"</summary><pre>{serde_json::to_string_pretty(&s).unwrap_or_default()}</pre></details>})}
        <h3>"公开链接"</h3>
        <Show when=move ||shares.get().is_empty() fallback=||()><p>"暂无公开链接"</p></Show>
        <For each=move||shares.get() key=|s|s.file_id.clone() children=move |s|{
            let id=s.file_id; view!{<div class="history-row"><div><strong>{s.name}</strong><p>{if s.status.active {"有效"}else{"已到期"}}" · "{s.status.expires_at.map(|t|t.to_rfc3339()).unwrap_or_else(||"永久".to_owned())}</p><input aria-label="公开链接" readonly value=s.status.url.unwrap_or_default()/></div><button type="button" prop:disabled=move||busy.get() on:click=move |_|{let id=id.clone();busy.set(true);leptos::task::spawn_local(async move{match api::revoke_share(&id).await{Ok(())=>{if busy.try_set(false).is_none(){refresh.run(());}},Err(e)=>{let _=error.try_set(e.message);let _=busy.try_set(false);}}});}>"撤销链接"</button></div>}
        }/>
        <button type="button" prop:disabled=move||busy.get()||offset.get()==0 on:click=move |_|{offset.update(|o|*o=(*o-200).max(0));refresh.run(());}>"上一页"</button>
        <button type="button" prop:disabled=move||busy.get()||shares.get().len()<200 on:click=move |_|{offset.update(|o|*o+=200);refresh.run(());}>"下一页"</button>
    </details> }
}

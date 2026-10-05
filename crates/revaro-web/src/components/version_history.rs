//! Document recovery panel, scoped to an open editor session.
use crate::api;
use crate::browser;
use leptos::prelude::*;
use revaro_core::features::DocumentVersion;
use wasm_bindgen::JsCast;
#[component]
pub fn VersionHistory(
    file_id: String,
    etag: RwSignal<String>,
    content: RwSignal<String>,
    dirty: RwSignal<bool>,
    on_restored: Callback<()>,
    on_close: Callback<()>,
) -> impl IntoView {
    let panel = NodeRef::<leptos::html::Section>::new();
    let mut outside = browser::on_click(move |event| {
        let Some(target) = event
            .target()
            .and_then(|target| target.dyn_into::<web_sys::Element>().ok())
        else {
            return;
        };
        if panel
            .get_untracked()
            .is_some_and(|panel| !panel.contains(Some(&target)))
            && target
                .closest("[aria-controls='version-history']")
                .ok()
                .flatten()
                .is_none()
        {
            on_close.run(());
        }
    });
    on_cleanup(move || outside.release());
    let alive = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(true));
    let alive_cleanup = alive.clone();
    on_cleanup(move || alive_cleanup.store(false, std::sync::atomic::Ordering::Relaxed));
    let versions = RwSignal::new(Vec::<DocumentVersion>::new());
    let error = RwSignal::new(String::new());
    let busy = RwSignal::new(true);
    let preview = RwSignal::new(None::<String>);
    let id = file_id.clone();
    leptos::task::spawn_local(async move {
        match api::fetch_versions(&id).await {
            Ok(v) => {
                let _ = versions.try_set(v);
            }
            Err(e) => {
                let _ = error.try_set(e.message);
            }
        }
        let _ = busy.try_set(false);
    });
    view! {
        <section node_ref=panel id="version-history" class="version-history" role="dialog" aria-label="版本历史">
            <header><strong>"版本历史（最近 20 次保存）"</strong><button type="button" aria-label="关闭版本历史" on:click=move |_|on_close.run(())>"关闭"</button></header>
            <p>"每次保存前保留原内容；恢复前也会保存当前版本。"</p>
            <Show when=move || !error.get().is_empty() fallback=|| ()><p class="form-error">{move ||error.get()}</p></Show>
            <Show when=move || versions.get().is_empty() fallback=|| ()><p>{move ||if busy.get(){"正在加载…"}else{"暂无历史版本"}}</p></Show>
            <For each=move ||versions.get() key=|v|v.id.clone() children=move |v| {
                let id_view=file_id.clone();let id_restore=file_id.clone();let alive_restore=alive.clone();let version_view=v.id.clone();let version_restore=v.id;
                view! {<div class="history-row"><span>{format!("{} · {} 字节",v.created_at.to_rfc3339(),v.size)}</span>
                    <button type="button" prop:disabled=move ||busy.get() on:click=move |_| {
                        let id=id_view.clone();let version=version_view.clone();busy.set(true);error.set(String::new());
                        leptos::task::spawn_local(async move {match api::fetch_version_content(&id,&version).await{Ok(text)=>{let _=preview.try_set(Some(text));},Err(e)=>{let _=error.try_set(e.message);}}let _=busy.try_set(false);});
                    }>"查看"</button>
                    <button type="button" prop:disabled=move ||busy.get() || dirty.get() on:click=move |_| {
                        let alive=alive_restore.clone();let id=id_restore.clone();let version=version_restore.clone();let current=etag.get_untracked();busy.set(true);error.set(String::new());
                        leptos::task::spawn_local(async move {
                            let result=async {let file=api::restore_version(&id,&version,&current).await?;let doc=api::fetch_document(&id).await?;Ok::<_,api::RequestError>((file,doc))}.await;
                            if !alive.load(std::sync::atomic::Ordering::Relaxed){return;}
                            match result {Ok((_file,doc))=>{if content.try_set(doc.content).is_none(){etag.set(doc.etag);dirty.set(false);on_restored.run(());on_close.run(());}},Err(e)=>{let _=error.try_set(e.message);}}
                            let _=busy.try_set(false);
                        });
                    }>"恢复此版本"</button>
                </div>}
            }/>
            <Show when=move ||dirty.get() fallback=|| ()><p>"请先保存当前修改，再恢复历史版本。"</p></Show>
            <Show when=move ||preview.get().is_some() fallback=|| ()><pre>{move ||preview.get().unwrap_or_default()}</pre></Show>
        </section>
    }
}

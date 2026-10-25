//! Public-share dialog for one live file.
//!
//! The link lifecycle deliberately stays visible in the same modal: the old
//! client let users inspect an existing link, copy it, regenerate it, or stop
//! sharing without leaving the file browser.

use leptos::prelude::*;
use revaro_core::model::File;

use super::dialogs::DialogBackdrop;
use super::menu::{ActionMenu, MenuIcon};
use crate::logic::format::format_date;

/// The reference share dialog's complete state and actions.
#[component]
pub fn ShareDialog(
    file: File,
    active: RwSignal<bool>,
    url: RwSignal<String>,
    created_at: RwSignal<String>,
    expires_at: RwSignal<String>,
    expiry: RwSignal<String>,
    custom_slug: RwSignal<String>,
    share_type: RwSignal<String>,
    engine: RwSignal<String>,
    entry: RwSignal<String>,
    key_config: RwSignal<String>,
    busy: RwSignal<bool>,
    error: RwSignal<String>,
    copied: RwSignal<bool>,
    on_close: Callback<()>,
    on_copy: Callback<()>,
    on_revoke: Callback<()>,
    on_create: Callback<bool>,
) -> impl IntoView {
    let is_zip = file.name.to_ascii_lowercase().ends_with(".zip");
    let file_id = file.id.clone();
    let entries = RwSignal::new(Vec::<String>::new());
    let entries_loaded = RwSignal::new(false);
    let entries_busy = RwSignal::new(false);
    let entries_error = RwSignal::new(String::new());
    let engines = RwSignal::new(Vec::<String>::new());
    let load_entries = Callback::new(move |(): ()| {
        if entries_busy.get_untracked() {
            return;
        }
        entries_loaded.set(true);
        entries_busy.set(true);
        entries_error.set(String::new());
        let file_id = file_id.clone();
        leptos::task::spawn_local(async move {
            let result = crate::api::fetch_game_entries(&file_id).await;
            if entries_loaded.try_get_untracked().is_none() {
                return;
            }
            match result {
                Ok(found) => {
                    if !found.engines.contains(&engine.get_untracked()) {
                        engine.set(
                            found
                                .engines
                                .first()
                                .cloned()
                                .unwrap_or_else(|| "kirikiri2".to_owned()),
                        );
                    }
                    engines.set(found.engines);
                    if !found.entries.contains(&entry.get_untracked()) {
                        entry.set(if found.entries.len() == 1 {
                            found.entries[0].clone()
                        } else {
                            String::new()
                        });
                    }
                    entries.set(found.entries);
                }
                Err(error) => entries_error.set(error.message),
            }
            entries_busy.set(false);
        });
    });
    let initial_load = load_entries.clone();
    Effect::new(move |_| {
        if share_type.get() == "galgame" && !entries_loaded.get_untracked() {
            initial_load.run(());
        }
    });
    let cannot_create = move || {
        busy.get()
            || (share_type.get() == "galgame"
                && (entries_busy.get()
                    || !entries_error.get().is_empty()
                    || engine.get().is_empty()
                    || !entries.get().contains(&entry.get())))
    };
    let file_name = file.name;
    let file_title = file_name.clone();
    let close_backdrop = on_close.clone();
    let close_header = on_close;
    let copy = on_copy;
    let revoke = on_revoke;
    let create = on_create;
    let base_url = web_sys::window()
        .and_then(|window| window.location().origin().ok())
        .unwrap_or_default();

    view! {
        <DialogBackdrop on_close=close_backdrop>
            <section class="modal share-modal" role="dialog" aria-modal="true" aria-labelledby="share-dialog-title">
                <header>
                    <div class="share-title">
                        <span>"↗"</span>
                        <div>
                            <h2 id="share-dialog-title">"分享文件"</h2>
                            <p title=file_title>{file_name}</p>
                        </div>
                    </div>
                    <button type="button" aria-label="关闭" on:click=move |_| close_header.run(())>"×"</button>
                </header>
                <label class="share-description">"分享类型 "
                    <select aria-label="分享类型" prop:value=move || share_type.get() prop:disabled=move || busy.get() on:change=move |e| share_type.set(event_target_value(&e))>
                        <option value="file">"普通文件分享"</option>
                        <option value="galgame" disabled=!is_zip>"Galgame 分享（完整 ZIP）"</option>
                    </select>
                </label>
                <Show when=move || share_type.get() == "galgame">
                    <label class="share-description">"游戏引擎 "
                        <select aria-label="游戏引擎" prop:value=move || engine.get() prop:disabled=move || busy.get() on:change=move |e| {
                            let value = event_target_value(&e);
                            engine.set(value.clone());
                            let valid = if value == "siglus" { matches!(entry.get_untracked().to_ascii_lowercase().as_str(), "scene.pck" | "scene.chs") } else { entry.get_untracked().to_ascii_lowercase().ends_with(".xp3") };
                            if !valid { entry.set(entries.get_untracked().iter().find(|path| if value == "siglus" { matches!(path.to_ascii_lowercase().as_str(), "scene.pck" | "scene.chs") } else { path.to_ascii_lowercase().ends_with(".xp3") }).cloned().unwrap_or_default()); }
                        }>
                            <option value="kirikiri2" prop:disabled=move || !engines.get().iter().any(|v| v == "kirikiri2")>"Kirikiri2"</option>
                            <option value="siglus" prop:disabled=move || !engines.get().iter().any(|v| v == "siglus")>"SiglusEngine"</option>
                        </select>
                    </label>
                    <div class="share-entry-picker">
                        <span>"入口文件"</span>
                        <ActionMenu label="入口文件".to_owned() icon=MenuIcon::Link
                            disabled=Signal::derive(move || busy.get() || entries_busy.get() || entries.get().is_empty())
                            text=Signal::derive(move || if entries_busy.get() { "正在读取 ZIP 目录…".to_owned() } else if entry.get().is_empty() { "请选择启动入口".to_owned() } else { entry.get() })
                            panel_class="share-entry-options".to_owned() viewport_bound=true>
                            <div role="listbox" aria-label="游戏入口列表">
                                {move || entries.get().into_iter().filter(|path| {
                                    if engine.get() == "siglus" {
                                        matches!(path.to_ascii_lowercase().as_str(), "scene.pck" | "scene.chs")
                                    } else {
                                        path.to_ascii_lowercase().ends_with(".xp3")
                                    }
                                }).map(|path| {
                                    let selected_path = path.clone();
                                    let value = path.clone();
                                    view! {
                                        <button type="button" role="option" data-close-menu="true"
                                            aria-selected=move || (entry.get() == selected_path).to_string()
                                            on:click=move |_| entry.set(value.clone())>{path}</button>
                                    }
                                }).collect_view()}
                            </div>
                        </ActionMenu>
                    </div>
                    <Show when=move || !entries_busy.get() && entries_loaded.get() && entries.get().is_empty() && entries_error.get().is_empty()>
                        <p class="form-error">"ZIP 中没有找到可识别的 Kirikiri2 或 SiglusEngine 游戏资源。"</p>
                    </Show>
                    <Show when=move || !entries_error.get().is_empty()>
                        <p class="form-error">{move || entries_error.get()} <button type="button" on:click=move |_| load_entries.run(())>"重新读取"</button></p>
                    </Show>
                    <Show when=move || engine.get() == "siglus">
                        <details class="share-description">
                            <summary>"游戏配置（可选）"</summary>
                            <label>"key.toml"
                                <textarea aria-label="Siglus key.toml" rows="4" maxlength="16384"
                                    placeholder="需要专用配置的游戏可粘贴 key.toml；ZIP 内已有则无需填写。"
                                    prop:value=move || key_config.get() prop:disabled=move || busy.get()
                                    on:input=move |e| key_config.set(event_target_value(&e)) />
                            </label>
                        </details>
                    </Show>
                    <p class="share-slug-help">"点击入口展开选项，选中后自动收起。其余资源会一同按需加载，存档保存在浏览器。"</p>
                </Show>
                <label class="share-slug-label" for="share-custom-slug">"自定义链接后缀（可选）"</label>
                <div class="share-slug-field">
                    <span>{move || format!("{base_url}/{}/", if share_type.get() == "galgame" { "g" } else { "s" })}</span>
                    <input
                        id="share-custom-slug"
                        type="text"
                        placeholder="自定义后缀"
                        maxlength="64"
                        autocomplete="off"
                        autocapitalize="none"
                        spellcheck="false"
                        aria-describedby="share-slug-help"
                        prop:value=move || custom_slug.get()
                        prop:disabled=move || busy.get()
                        on:input=move |event| custom_slug.set(event_target_value(&event))
                    />
                </div>
                <p id="share-slug-help" class="share-slug-help">"留空生成随机链接。支持 1–64 位英文字母、数字、- 和 _，区分大小写。"</p>
                <label class="share-description">"新链接有效期 "
                    <select aria-label="分享有效期" prop:value=move || expiry.get() prop:disabled=move || busy.get() on:change=move |e|expiry.set(event_target_value(&e))>
                        <option value="3600">"1 小时"</option><option value="86400">"1 天"</option><option value="604800">"7 天"</option><option value="2592000">"30 天"</option><option value="0">"永久"</option>
                    </select>
                </label>
                <p class="share-description">{move || if expires_at.get().is_empty() {String::new()} else {format!("当前链接到期时间：{}",format_date(&expires_at.get()))}}</p>
                <Show
                    when=move || !busy.get()
                    fallback=|| view! {
                        <div class="state small"><div class="spinner"></div><p>"正在准备分享…"</p></div>
                    }
                >
                    <Show
                        when=move || active.get()
                        fallback=move || view! {
                            <p class="share-description">"创建后，无需登录即可通过链接读取这个文件。你可以随时重新生成或停止分享。"</p>
                            <button class="primary share-create" type="button" prop:disabled=cannot_create on:click=move |_| create.run(false)>
                                "创建公开链接"
                            </button>
                            <Show when=move || !error.get().is_empty() fallback=|| ()>
                                <p class="form-error">{move || error.get()}</p>
                            </Show>
                        }
                    >
                        <p class="share-description">"任何拿到链接的人都能直接读取该文件。更换后缀、重新生成随机链接或停止分享后，旧链接立即失效。"</p>
                        <div class="share-link">
                            <input
                                type="text"
                                aria-label="分享链接"
                                readonly
                                prop:value=move || url.get()
                                on:focus=|event| {
                                    let input = event_target::<web_sys::HtmlInputElement>(&event);
                                    let _ = input.select();
                                }
                            />
                            <button class="primary" type="button" on:click=move |_| copy.run(())>
                                {move || if copied.get() { "已复制" } else { "复制链接" }}
                            </button>
                        </div>
                        <Show when=move || !created_at.get().is_empty() fallback=|| ()>
                            <p class="share-created">
                                {move || format!("公开链接 · 创建于 {}", format_date(&created_at.get()))}
                            </p>
                        </Show>
                        <Show when=move || !error.get().is_empty() fallback=|| ()>
                            <p class="form-error">{move || error.get()}</p>
                        </Show>
                        <footer class="share-footer">
                            <button class="danger-text" type="button" prop:disabled=move || busy.get() on:click=move |_| revoke.run(())>"停止分享"</button>
                            <button class="secondary" type="button" prop:disabled=cannot_create on:click=move |_| create.run(true)>{move || if custom_slug.get().is_empty() { "重新生成链接" } else { "保存链接设置" }}</button>
                        </footer>
                    </Show>
                </Show>
            </section>
        </DialogBackdrop>
    }
}

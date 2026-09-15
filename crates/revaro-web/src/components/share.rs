//! Public-share dialog for one live file.
//!
//! The link lifecycle deliberately stays visible in the same modal: the old
//! client let users inspect an existing link, copy it, regenerate it, or stop
//! sharing without leaving the file browser.

use leptos::ev::MouseEvent;
use leptos::prelude::*;
use revaro_core::model::File;

use crate::logic::format::format_date;

/// The reference share dialog's complete state and actions.
#[component]
pub fn ShareDialog(
    file: File,
    active: RwSignal<bool>,
    url: RwSignal<String>,
    created_at: RwSignal<String>,
    busy: RwSignal<bool>,
    error: RwSignal<String>,
    copied: RwSignal<bool>,
    on_close: Callback<()>,
    on_copy: Callback<()>,
    on_revoke: Callback<()>,
    on_create: Callback<bool>,
) -> impl IntoView {
    let file_name = file.name;
    let file_title = file_name.clone();
    let close_backdrop = on_close.clone();
    let close_header = on_close;
    let copy = on_copy;
    let revoke = on_revoke;
    let create = on_create;

    view! {
        <div
            class="modal-backdrop"
            role="presentation"
            on:click=move |event: MouseEvent| {
                // The reference modal remains dismissible while its initial
                // read or a create/revoke request is in flight. The request
                // may finish after the overlay has been removed; keeping the
                // close path independent of `busy` preserves that behavior.
                if event.target() == event.current_target() {
                    close_backdrop.run(());
                }
            }
        >
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
                            <button class="primary share-create" type="button" prop:disabled=move || busy.get() on:click=move |_| create.run(false)>
                                "创建公开链接"
                            </button>
                            <Show when=move || !error.get().is_empty() fallback=|| ()>
                                <p class="form-error">{move || error.get()}</p>
                            </Show>
                        }
                    >
                        <p class="share-description">"任何拿到链接的人都能直接读取该文件。重新生成或停止分享后，旧链接立即失效。"</p>
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
                            <button class="secondary" type="button" prop:disabled=move || busy.get() on:click=move |_| create.run(true)>"重新生成链接"</button>
                        </footer>
                    </Show>
                </Show>
            </section>
        </div>
    }
}

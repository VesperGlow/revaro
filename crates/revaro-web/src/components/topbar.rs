//! The authenticated top bar.
//!
//! This is intentionally a separate shell component. The reference top bar has
//! separate disclosure surfaces for tasks and the mobile account/tools menu;
//! keeping their ownership explicit prevents an account
//! click from accidentally becoming logout again.

use leptos::prelude::*;
use revaro_core::model::TaskStatus;
use wasm_bindgen::JsCast;
use wasm_bindgen::closure::Closure;

use crate::browser;

use super::icons;
use super::tasks::{TaskCenter, UiTaskController};
use crate::logic::task_status::is_active_task_status;

/// The desktop/mobile authenticated top bar.
#[component]
pub fn AppTopbar(
    username: RwSignal<String>,
    has_avatar: RwSignal<bool>,
    avatar_version: RwSignal<u64>,
    task_controller: UiTaskController,
    on_home: Callback<()>,
    on_trash: Callback<()>,
    on_account: Callback<()>,
    search_text: RwSignal<String>,
    search_query: RwSignal<String>,
    search_global: RwSignal<bool>,
    trash_mode: RwSignal<bool>,
    on_search: Callback<()>,
) -> impl IntoView {
    let mobile = browser::media_query_signal("(max-width: 850px)");
    let mobile_menu = NodeRef::<leptos::html::Details>::new();
    let search_open = RwSignal::new(false);
    let search_ready = RwSignal::new(false);
    let search_container = NodeRef::<leptos::html::Div>::new();
    let search_scope = NodeRef::<leptos::html::Div>::new();
    let scope_toggle = NodeRef::<leptos::html::Button>::new();
    let scope_open = RwSignal::new(false);
    let search_input = NodeRef::<leptos::html::Input>::new();
    let search_toggle = NodeRef::<leptos::html::Button>::new();
    Effect::new(move |_| {
        if trash_mode.get() {
            search_open.set(false);
        }
        if !search_open.get() {
            search_ready.set(false);
            scope_open.set(false);
        } else if search_open.get() && search_input.get().is_some() {
            // Wait for the 240 ms reveal transition so the input is visible
            // and focusable, then resolve the current node rather than a stale one.
            if let Some(window) = web_sys::window() {
                let callback = Closure::once_into_js(move || {
                    if search_open.try_get_untracked() == Some(true)
                        && let Some(input) = search_input.get_untracked()
                    {
                        search_ready.set(true);
                        let options = web_sys::FocusOptions::new();
                        options.set_prevent_scroll(true);
                        let _ = input.focus_with_options(&options);
                    }
                });
                let _ = window.set_timeout_with_callback_and_timeout_and_arguments_0(
                    callback.unchecked_ref(),
                    250,
                );
            }
        }
    });
    let task_signal = task_controller.task_signal();
    let active_task_count = Signal::derive_local(move || {
        task_signal
            .get()
            .into_iter()
            .filter(|task| is_active_task_status(task.status))
            .count()
    });
    let failed_task_count = Signal::derive_local(move || {
        task_signal
            .get()
            .into_iter()
            .filter(|task| task.status == TaskStatus::Failed)
            .count()
    });

    let menu_for_outside = mobile_menu;
    let mut outside = browser::on_pointerdown(move |event| {
        let Some(target) = event
            .target()
            .and_then(|target| target.dyn_into::<web_sys::Node>().ok())
        else {
            return;
        };
        if search_open.get_untracked()
            && !search_container
                .get()
                .is_some_and(|container| container.contains(Some(&target)))
        {
            search_open.set(false);
            scope_open.set(false);
        } else if scope_open.get_untracked()
            && !search_scope
                .get()
                .is_some_and(|scope| scope.contains(Some(&target)))
        {
            scope_open.set(false);
        }
        if let Some(details) = menu_for_outside.get()
            && details.open()
            && !details.contains(Some(&target))
        {
            details.set_open(false);
        }
    });
    let close_search = Callback::new(move |(): ()| {
        scope_open.set(false);
        search_ready.set(false);
        search_open.set(false);
        if let Some(button) = search_toggle.get() {
            let options = web_sys::FocusOptions::new();
            options.set_prevent_scroll(true);
            let _ = button.focus_with_options(&options);
        }
    });
    let menu_for_escape = mobile_menu;
    let mut escape = browser::on_keydown(move |event| {
        if event.key() != "Escape" {
            return;
        }
        if search_open.get_untracked() {
            event.prevent_default();
            close_search.run(());
            return;
        }
        let Some(details) = menu_for_escape.get() else {
            return;
        };
        if details.open() {
            details.set_open(false);
            if let Ok(Some(summary)) = details.query_selector("summary")
                && let Ok(summary) = summary.dyn_into::<web_sys::HtmlElement>()
            {
                let _ = summary.focus();
            }
        }
    });
    on_cleanup(move || {
        outside.release();
        escape.release();
    });

    let select_scope = Callback::new(move |global: bool| {
        search_global.set(global);
        scope_open.set(false);
        if let Some(button) = scope_toggle.get() {
            let _ = button.focus();
        }
    });

    let close_mobile_menu = Callback::new(move |(): ()| {
        if let Some(details) = mobile_menu.get() {
            details.set_open(false);
        }
    });
    let open_mobile_tasks = {
        let close_mobile_menu = close_mobile_menu.clone();
        let controller = task_controller.clone();
        Callback::new(move |(): ()| {
            close_mobile_menu.run(());
            controller.open_center();
        })
    };
    let open_mobile_trash = {
        let close_mobile_menu = close_mobile_menu.clone();
        let on_trash = on_trash.clone();
        Callback::new(move |(): ()| {
            close_mobile_menu.run(());
            on_trash.run(());
        })
    };
    let open_mobile_account = {
        let close_mobile_menu = close_mobile_menu.clone();
        let on_account = on_account.clone();
        Callback::new(move |(): ()| {
            close_mobile_menu.run(());
            on_account.run(());
        })
    };
    let mobile_task_controller = task_controller.clone();

    let initial = move || {
        username
            .get()
            .chars()
            .next()
            .map(|character| character.to_uppercase().collect::<String>())
            .unwrap_or_else(|| "R".to_owned())
    };
    let avatar_url = move || format!("/api/profile/avatar?v={}", avatar_version.get());
    let avatar_fallback = {
        let has_avatar = has_avatar;
        move |_| has_avatar.set(false)
    };
    let account_avatar = move || {
        view! {
            <span class="avatar-badge">
                <Show
                    when=move || has_avatar.get()
                    fallback=move || view! { <span>{initial()}</span> }
                >
                    <img
                        class="ui-image"
                        src=avatar_url
                        alt="个人头像"
                        draggable="false"
                        on:error=avatar_fallback
                    />
                </Show>
            </span>
        }
    };

    view! {
        <header class="topbar" class:searching=move || search_open.get()>
            <div class="topbar-left">
                <button
                    class="logo brand-button"
                    type="button"
                    title="回到我的文件"
                    aria-label="回到我的文件"
                    on:click=move |_| on_home.run(())
                >
                    <img class="brand-logo" src="/revaro-logo.svg" alt="" aria-hidden="true" />
                </button>
            </div>
            <div class="top-actions">
                <Show when=move || !trash_mode.get() fallback=|| ()>
                    <div node_ref=search_container class="topbar-search"
                        class:expanded=move || search_open.get()
                        class:ready=move || search_ready.get()
                        class:active=move || !search_query.get().is_empty()>
                        <form id="topbar-file-search" class="search-surface" role="search" aria-label="文件搜索"
                            on:submit=move |ev: leptos::ev::SubmitEvent| {
                                ev.prevent_default();
                                if search_open.get_untracked() {
                                    scope_open.set(false);
                                    on_search.run(());
                                }
                            }>
                            <button node_ref=search_toggle class="search-toggle" type="button"
                                title=move || if search_open.get() { "聚焦搜索" } else { "打开搜索" }
                                aria-label=move || if search_open.get() { "聚焦搜索" } else { "打开搜索" }
                                aria-expanded=move || search_open.get().to_string() aria-controls="topbar-search-fields"
                                on:click=move |_| {
                                    if search_open.get_untracked() {
                                        if let Some(input) = search_input.get() {
                                            let _ = input.focus();
                                        }
                                    } else {
                                        search_open.set(true);
                                    }
                                }>
                                {icons::search()}
                            </button>
                            <div id="topbar-search-fields" class="search-fields" aria-hidden=move || (!search_open.get()).to_string()>
                                <input node_ref=search_input type="search" aria-label="搜索文件名" placeholder="搜索文件…"
                                    prop:disabled=move || !search_open.get()
                                    prop:value=move || search_text.get()
                                    on:input=move |ev| search_text.set(event_target_value(&ev)) />
                                <div node_ref=search_scope class="search-scope">
                                    <button node_ref=scope_toggle class="search-scope-toggle" type="button"
                                        prop:disabled=move || !search_open.get()
                                        aria-haspopup="true" aria-expanded=move || scope_open.get().to_string()
                                        aria-controls="search-scope-options"
                                        on:click=move |_| scope_open.update(|open| *open = !*open)>
                                        <span>{move || if search_global.get() { "全部文件夹" } else { "当前文件夹" }}</span>
                                        {icons::chevron_down()}
                                    </button>
                                    <Show when=move || scope_open.get() && search_open.get() fallback=|| ()>
                                        <div id="search-scope-options" class="search-scope-options" role="group" aria-label="搜索范围">
                                            <button type="button"
                                                on:click=move |_| select_scope.run(!search_global.get_untracked())>
                                                <span>{move || if search_global.get() { "当前文件夹" } else { "全部文件夹" }}</span>
                                            </button>
                                        </div>
                                    </Show>
                                </div>
                                <button class="search-close" type="button" title="收起搜索" aria-label="收起搜索"
                                    prop:disabled=move || !search_open.get()
                                    on:click=move |_| close_search.run(())>
                                    {icons::close_square()}
                                </button>
                            </div>
                        </form>
                    </div>
                </Show>
                <Show
                    when=move || !mobile.get()
                    fallback=move || view! {
                        <details node_ref=mobile_menu class="mobile-account-menu">
                            <summary title="账户与工具" aria-label="打开账户与工具菜单">
                                {account_avatar()}
                                <Show when=move || { failed_task_count.get() > 0 } fallback=move || view! {
                                    <Show when=move || { active_task_count.get() > 0 } fallback=|| ()>
                                        <i class="task-menu-badge active" aria-label=move || format!("{} 个活动任务", active_task_count.get())></i>
                                    </Show>
                                }>
                                    <i class="task-menu-badge failed" aria-label="存在失败任务">"!"</i>
                                </Show>
                            </summary>
                            <section>
                                <button
                                    class="mobile-tool-item"
                                    type="button"
                                    on:click=move |_| open_mobile_tasks.run(())
                                >
                                    <span class="mobile-task-icon">{icons::activity()}</span>
                                    <b>"任务中心"</b>
                                    <Show when=move || { failed_task_count.get() > 0 } fallback=move || view! {
                                        <Show when=move || { active_task_count.get() > 0 } fallback=|| ()>
                                            <small>{move || format!("{} 项活动", active_task_count.get())}</small>
                                        </Show>
                                    }>
                                        <small class="failed">{move || format!("{} 项失败", failed_task_count.get())}</small>
                                    </Show>
                                </button>
                                <button
                                    class="mobile-trash"
                                    type="button"
                                    on:click=move |_| open_mobile_trash.run(())
                                >
                                    <span class="trash-button">{icons::trash()}</span>
                                    <b>"回收站"</b>
                                </button>
                                <hr />
                                <button
                                    class="mobile-tool-item"
                                    type="button"
                                    on:click=move |_| open_mobile_account.run(())
                                >
                                    <span class="mobile-task-icon">{icons::settings()}</span>
                                    <b>"账户设置"</b>
                                </button>
                            </section>
                        </details>
                        <TaskCenter controller=mobile_task_controller.clone() hide_trigger=true />
                    }
                >
                    <TaskCenter controller=task_controller.clone() hide_trigger=false />
                    <button
                        class="trash-button"
                        type="button"
                        title="回收站"
                        aria-label="打开回收站"
                        on:click=move |_| on_trash.run(())
                    >
                        {icons::trash()}
                    </button>
                    <button
                        class="account-button"
                        type="button"
                        title="打开账户设置"
                        aria-label="打开账户设置"
                        on:click=move |_| on_account.run(())
                    >
                        {account_avatar()}
                        <span class="account-copy">
                            <b>{move || username.get()}</b>
                            <small>"账户设置"</small>
                        </span>
                    </button>
                </Show>
            </div>
        </header>
    }
}

//! The authenticated top bar.
//!
//! This is intentionally a separate shell component. The reference top bar has
//! three different disclosure surfaces (tasks, system status and the mobile
//! account/tools menu); keeping their ownership explicit prevents an account
//! click from accidentally becoming logout again.

use leptos::prelude::*;
use revaro_core::model::TaskStatus;
use wasm_bindgen::JsCast;

use crate::browser;

use super::icons;
use super::system_status::SystemStatus;
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
) -> impl IntoView {
    let mobile = browser::media_query_signal("(max-width: 850px)");
    let mobile_menu = NodeRef::<leptos::html::Details>::new();
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
        let Some(details) = menu_for_outside.get() else {
            return;
        };
        if !details.open() {
            return;
        }
        let inside = event
            .target()
            .and_then(|target| target.dyn_into::<web_sys::Node>().ok())
            .is_some_and(|target| details.contains(Some(&target)));
        if !inside {
            details.set_open(false);
        }
    });
    let menu_for_escape = mobile_menu;
    let mut escape = browser::on_keydown(move |event| {
        if event.key() != "Escape" {
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
        <header class="topbar">
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
                <Show
                    when=move || !mobile.get()
                    fallback=move || view! {
                        <SystemStatus />
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
                    <SystemStatus />
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

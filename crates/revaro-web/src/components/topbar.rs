//! Shared application header and navigation, using the existing file tools.

use leptos::prelude::*;
use revaro_core::model::TaskStatus;
use wasm_bindgen::JsCast;
use wasm_bindgen::closure::Closure;

use crate::{browser, logic::library::LibraryPage};

use super::icons;
use super::menu::{ActionMenu, MenuIcon};
use super::tasks::{TaskCenter, UiTaskController};
use crate::logic::task_status::is_active_task_status;

/// Profile, task and file actions injected by the persistent file workspace.
#[derive(Clone)]
pub struct TopbarActions {
    pub username: RwSignal<String>,
    pub has_avatar: RwSignal<bool>,
    pub avatar_version: RwSignal<u64>,
    pub task_controller: UiTaskController,
    pub on_files: Callback<()>,
    pub on_upload_files: Callback<()>,
    pub on_upload_folder: Callback<()>,
    pub on_new_document: Callback<()>,
    pub on_create_folder: Callback<()>,
    pub on_trash: Callback<()>,
    pub on_account: Callback<()>,
    pub search_text: RwSignal<String>,
    pub trash_mode: RwSignal<bool>,
    pub on_search: Callback<()>,
}

/// Existing content-library search callbacks, injected without changing query semantics.
#[derive(Clone, Copy)]
pub struct TopbarSearch {
    pub text: RwSignal<String>,
    pub on_input: Callback<String>,
    pub on_submit: Callback<()>,
}

/// The single application header, mounted across every content page.
#[component]
pub fn AppTopbar(
    actions: TopbarActions,
    library_search: TopbarSearch,
    page: RwSignal<LibraryPage>,
    on_navigate: Callback<LibraryPage>,
) -> impl IntoView {
    let TopbarActions {
        username,
        has_avatar,
        avatar_version,
        task_controller,
        on_upload_files,
        on_upload_folder,
        on_new_document,
        on_create_folder,
        on_trash,
        on_account,
        search_text,
        trash_mode,
        on_search,
        ..
    } = actions;
    let open_trash = Callback::new(move |()| {
        on_navigate.run(LibraryPage::Files);
        on_trash.run(());
    });
    let file_actions_disabled =
        Signal::derive(move || page.get() == LibraryPage::Files && trash_mode.get());
    let menu_context = Signal::from(Memo::new(move |_| {
        format!("{}:{}", page.get().path(), trash_mode.get())
    }));
    let mobile = browser::media_query_signal("(max-width: 850px)");
    let mobile_menu = NodeRef::<leptos::html::Details>::new();
    let mobile_trigger = NodeRef::<leptos::html::Summary>::new();
    let mobile_panel = NodeRef::<leptos::html::Section>::new();
    let mobile_position = browser::anchor_popover(mobile_trigger, mobile_panel);
    let search_open = RwSignal::new(false);
    let search_container = NodeRef::<leptos::html::Div>::new();
    let search_input = NodeRef::<leptos::html::Input>::new();
    let search_toggle = NodeRef::<leptos::html::Button>::new();
    let search_measure = NodeRef::<leptos::html::Span>::new();
    let search_width = RwSignal::new(208.0_f64);
    let size_search = Callback::new(move |(): ()| {
        if let Some(measure) = search_measure.get_untracked() {
            let text = if page.get_untracked() == LibraryPage::Files {
                search_text.get_untracked()
            } else {
                library_search.text.get_untracked()
            };
            measure.set_text_content(Some(&text));
            // Padding, close/search buttons and a little space for the caret.
            search_width.set((measure.get_bounding_client_rect().width() + 108.0).max(208.0));
        }
    });
    Effect::new(move |_| {
        let _ = search_measure.get();
        let _ = page.get();
        if page.get() == LibraryPage::Files {
            let _ = search_text.get();
        } else {
            let _ = library_search.text.get();
        }
        size_search.run(());
    });
    let mut search_resize = browser::on_resize(move |_| size_search.run(()));
    on_cleanup(move || search_resize.release());

    let search_context = Memo::new(move |_| {
        (
            page.get(),
            page.get() == LibraryPage::Files && trash_mode.get(),
        )
    });
    Effect::new(move |_| {
        let _ = search_context.get();
        search_open.set(false);
    });
    Effect::new(move |_| {
        if search_open.get() && search_input.get().is_some() {
            // Wait for the 240 ms reveal transition so the input is visible
            // and focusable, then resolve the current node rather than a stale one.
            if let Some(window) = web_sys::window() {
                let callback = Closure::once_into_js(move || {
                    if search_open.try_get_untracked() == Some(true)
                        && let Some(input) = search_input.get_untracked()
                    {
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
    let mut outside = browser::on_click(move |event| {
        let Some(target) = event
            .target()
            .and_then(|target| target.dyn_into::<web_sys::Node>().ok())
        else {
            return;
        };
        // Close after the click is dispatched so shrinking the header cannot
        // move a navigation link between pointer-down and pointer-up.
        if search_open.get_untracked()
            && !search_container
                .get()
                .is_some_and(|container| container.contains(Some(&target)))
        {
            search_open.set(false);
        }
        if let Some(details) = menu_for_outside.get()
            && details.open()
            && !details.contains(Some(&target))
        {
            details.set_open(false);
        }
    });
    let close_search = Callback::new(move |(): ()| {
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
        let on_trash = open_trash;
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
                    title="回到首页"
                    aria-label="回到首页"
                    on:click=move |_| on_navigate.run(LibraryPage::Home)
                >
                    <img class="brand-logo" src="/revaro-logo.svg" alt="" aria-hidden="true" />
                </button>
            </div>
            <AppNavigation page=page on_navigate=on_navigate mobile=false />
            <div class="top-actions">
                <Show when=move || page.get() != LibraryPage::Home && !(page.get() == LibraryPage::Files && trash_mode.get()) fallback=|| ()>
                    <div node_ref=search_container class="topbar-search"
                        class:expanded=move || search_open.get()
                        style=move || format!("--search-width:{}px", search_width.get())>
                        <form class="search-surface" role="search" aria-label=move || match page.get() {
                            LibraryPage::Books => "书籍搜索", LibraryPage::Music => "歌曲搜索", LibraryPage::Gallery => "图片搜索", _ => "文件搜索",
                        } on:submit=move |ev: leptos::ev::SubmitEvent| {
                            ev.prevent_default();
                            if search_open.get_untracked() {
                                if page.get_untracked() == LibraryPage::Files { on_search.run(()); } else { library_search.on_submit.run(()); }
                            }
                        }>
                            <span node_ref=search_measure class="search-measure" aria-hidden="true"></span>
                            <div id="topbar-search-fields" class="search-fields" aria-hidden=move || (!search_open.get()).to_string()>
                                <input node_ref=search_input type="search" aria-label=move || match page.get() {
                                    LibraryPage::Books => "搜索书籍", LibraryPage::Music => "搜索歌曲", LibraryPage::Gallery => "搜索图片", _ => "搜索文件名",
                                } placeholder=move || match page.get() {
                                    LibraryPage::Books => "搜索书籍…", LibraryPage::Music => "搜索歌曲…", LibraryPage::Gallery => "搜索图片…", _ => "搜索文件…",
                                } prop:disabled=move || !search_open.get()
                                    prop:value=move || if page.get() == LibraryPage::Files { search_text.get() } else { library_search.text.get() }
                                    on:input=move |ev| {
                                        let text = event_target_value(&ev);
                                        if page.get_untracked() == LibraryPage::Files { search_text.set(text); } else { library_search.on_input.run(text); }
                                    } />
                                <button class="search-close" type="button" title="收起搜索" aria-label="收起搜索"
                                    prop:disabled=move || !search_open.get() on:click=move |_| close_search.run(())>
                                    {icons::close_square()}
                                </button>
                            </div>
                            <button node_ref=search_toggle class="search-toggle" type="button"
                                title=move || if search_open.get() { "聚焦搜索" } else { "打开搜索" }
                                aria-label=move || if search_open.get() { "聚焦搜索" } else { "打开搜索" }
                                aria-expanded=move || search_open.get().to_string() aria-controls="topbar-search-fields"
                                on:click=move |_| {
                                    if search_open.get_untracked() {
                                        if let Some(input) = search_input.get() { let _ = input.focus(); }
                                    } else { search_open.set(true); }
                                }>{icons::search()}</button>
                        </form>
                    </div>
                </Show>
                <ActionMenu label="新建".to_owned() icon=MenuIcon::Create disabled=file_actions_disabled context=menu_context>
                    <button type="button" data-close-menu="true" on:click=move |_| on_new_document.run(())>"新建文档"</button>
                    <button type="button" data-close-menu="true" on:click=move |_| on_create_folder.run(())>"新建文件夹"</button>
                </ActionMenu>
                <ActionMenu label="上传".to_owned() icon=MenuIcon::Upload disabled=file_actions_disabled context=menu_context>
                    <button type="button" data-close-menu="true" on:click=move |_| on_upload_files.run(())>"上传文件"</button>
                    <button type="button" data-close-menu="true" on:click=move |_| on_upload_folder.run(())>"上传文件夹"</button>
                </ActionMenu>
                <Show
                    when=move || !mobile.get()
                    fallback=move || view! {
                        <details node_ref=mobile_menu class="mobile-account-menu" on:toggle=move |_| mobile_position.run(())>
                            <summary node_ref=mobile_trigger title="账户与工具" aria-label="打开账户与工具菜单">
                                {account_avatar()}
                                <Show when=move || { failed_task_count.get() > 0 } fallback=move || view! {
                                    <Show when=move || { active_task_count.get() > 0 } fallback=|| ()>
                                        <i class="task-menu-badge active" aria-label=move || format!("{} 个活动任务", active_task_count.get())></i>
                                    </Show>
                                }>
                                    <i class="task-menu-badge failed" aria-label="存在失败任务">"!"</i>
                                </Show>
                            </summary>
                            <section node_ref=mobile_panel>
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
                        <TaskCenter controller=mobile_task_controller.clone() hide_trigger=true anchor=mobile_trigger />
                    }
                >
                    <TaskCenter controller=task_controller.clone() hide_trigger=false />
                    <button
                        class="trash-button"
                        type="button"
                        title="回收站"
                        aria-label="打开回收站"
                        on:click=move |_| open_trash.run(())
                    >
                        {icons::trash()}
                    </button>
                    <button
                        class="account-button"
                        type="button"
                        title=move || format!("账户设置 · {}", username.get())
                        aria-label="打开账户设置"
                        on:click=move |_| on_account.run(())
                    >
                        {account_avatar()}

                    </button>
                </Show>
            </div>
        </header>
    }
}

/// Desktop and mobile navigation share routes, active state and click handling.
#[component]
pub fn AppNavigation(
    page: RwSignal<LibraryPage>,
    on_navigate: Callback<LibraryPage>,
    mobile: bool,
) -> impl IntoView {
    view! {
        <nav class="app-navigation" class:mobile-navigation=mobile
            aria-label=if mobile { "移动端导航" } else { "主导航" }>
            <For each=|| LibraryPage::ALL key=|p| p.path() children=move |destination| view! {
                <a href=destination.path() class:active=move || page.get() == destination
                    title=destination.label() aria-label=destination.label()
                    aria-current=move || if page.get() == destination { Some("page") } else { None }
                    on:click=move |event: leptos::ev::MouseEvent| {
                        if event.button() != 0 || event.ctrl_key() || event.meta_key() || event.shift_key() || event.alt_key() {
                            return;
                        }
                        event.prevent_default();
                        on_navigate.run(destination);
                    }>{match destination {
                        LibraryPage::Home => icons::home().into_any(),
                        LibraryPage::Books => icons::book_open().into_any(),
                        LibraryPage::Music => icons::music_2().into_any(),
                        LibraryPage::Gallery => icons::image().into_any(),
                        LibraryPage::Files => icons::folder().into_any(),
                    }}</a>
            } />
        </nav>
    }
}

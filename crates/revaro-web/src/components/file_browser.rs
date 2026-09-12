//! The authenticated file browser and its basic lifecycle actions.
//!
//! This slice owns folder navigation, breadcrumbs, the grid/list choice,
//! selection, file and folder mutations, and the trash view. Specialised
//! readers and media controls remain separate stages; live file cards still
//! open through the authenticated preview/download endpoints.

use std::collections::HashSet;

use leptos::prelude::*;
use revaro_core::api::auth::Session;
use revaro_core::api::files::{Children, CreateDirectoryRequest, FileDetail, PatchFileRequest};
use revaro_core::classify;
use revaro_core::ids::ROOT_ID;
use revaro_core::model::{File, FileKind, FileStatus};
use wasm_bindgen::JsValue;

use crate::api;
use crate::logic::format::format_size;
use crate::logic::routing::{folder_id, folder_url};

use super::dialogs::ActionDialog;
use super::icons;
use super::selection_toolbar::SelectionToolbar;
use super::uploads::{UploadController, UploadSurface};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ViewMode {
    Grid,
    List,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum DialogState {
    CreateFolder,
    Rename { id: String },
    Delete,
    Restore,
    Purge,
    EmptyTrash,
}

/// The authenticated file browser.
#[component]
pub fn FileBrowser(session: Session, on_logout: Callback<()>) -> impl IntoView {
    let current_id = RwSignal::new(ROOT_ID.to_owned());
    let current = RwSignal::new(None::<File>);
    let breadcrumbs = RwSignal::new(Vec::<File>::new());
    let items = RwSignal::new(Vec::<File>::new());
    let total_bytes = RwSignal::new(0_i64);
    let file_count = RwSignal::new(0_i64);
    let loading = RwSignal::new(false);
    let error = RwSignal::new(String::new());
    let trash_mode = RwSignal::new(false);
    let view_mode = RwSignal::new(ViewMode::Grid);
    let sidebar_collapsed = RwSignal::new(false);
    let request_sequence = RwSignal::new(0_u64);
    let selected_ids = RwSignal::new(HashSet::<String>::new());
    let dialog = RwSignal::new(None::<DialogState>);
    let dialog_value = RwSignal::new(String::new());
    let dialog_busy = RwSignal::new(false);
    let dialog_error = RwSignal::new(String::new());
    let feedback = RwSignal::new(String::new());

    let load_folder = {
        let current_id = current_id;
        let current = current;
        let breadcrumbs = breadcrumbs;
        let items = items;
        let total_bytes = total_bytes;
        let file_count = file_count;
        let loading = loading;
        let error = error;
        let trash_mode = trash_mode;
        let request_sequence = request_sequence;
        let selected_ids = selected_ids;
        let feedback = feedback;
        let on_logout = on_logout.clone();
        Callback::new(move |id: String| {
            let sequence = request_sequence.get_untracked().wrapping_add(1);
            request_sequence.set(sequence);
            loading.set(true);
            error.set(String::new());
            feedback.set(String::new());
            selected_ids.set(HashSet::new());
            let requested_id = id;
            let logout = on_logout.clone();

            leptos::task::spawn_local(async move {
                let result = async {
                    let detail = api::fetch_file(&requested_id).await?;
                    let children = api::fetch_children(&requested_id).await?;
                    Ok::<(FileDetail, Children), api::RequestError>((detail, children))
                }
                .await;

                if request_sequence.get_untracked() != sequence {
                    return;
                }

                match result {
                    Ok((detail, children)) => {
                        current_id.set(requested_id.clone());
                        current.set(Some(detail.file));
                        breadcrumbs.set(detail.breadcrumbs);
                        items.set(children.items);
                        total_bytes.set(children.total_bytes);
                        file_count.set(children.file_count);
                        trash_mode.set(false);
                        replace_folder_url(&requested_id);
                        loading.set(false);
                    }
                    Err(request_error) if request_error.is_unauthorized() => {
                        loading.set(false);
                        logout.run(());
                    }
                    Err(request_error) => {
                        loading.set(false);
                        error.set(request_error.message);
                    }
                }
            });
        })
    };

    let load_trash = {
        let current = current;
        let breadcrumbs = breadcrumbs;
        let items = items;
        let total_bytes = total_bytes;
        let file_count = file_count;
        let loading = loading;
        let error = error;
        let trash_mode = trash_mode;
        let request_sequence = request_sequence;
        let selected_ids = selected_ids;
        let feedback = feedback;
        let on_logout = on_logout.clone();
        Callback::new(move |(): ()| {
            let sequence = request_sequence.get_untracked().wrapping_add(1);
            request_sequence.set(sequence);
            loading.set(true);
            error.set(String::new());
            feedback.set(String::new());
            selected_ids.set(HashSet::new());
            let logout = on_logout.clone();

            leptos::task::spawn_local(async move {
                match api::fetch_trash().await {
                    Ok(trash) if request_sequence.get_untracked() == sequence => {
                        current.set(None);
                        breadcrumbs.set(Vec::new());
                        items.set(trash.items);
                        total_bytes.set(trash.total_bytes);
                        file_count.set(trash.file_count);
                        trash_mode.set(true);
                        loading.set(false);
                    }
                    Err(request_error)
                        if request_sequence.get_untracked() == sequence
                            && request_error.is_unauthorized() =>
                    {
                        loading.set(false);
                        logout.run(());
                    }
                    Err(request_error) if request_sequence.get_untracked() == sequence => {
                        loading.set(false);
                        error.set(request_error.message);
                    }
                    Ok(_) | Err(_) => {}
                }
            });
        })
    };

    let file_input = NodeRef::<leptos::html::Input>::new();
    let folder_input = NodeRef::<leptos::html::Input>::new();
    let upload_refresh = {
        let current_id = current_id;
        let trash_mode = trash_mode;
        let load_folder = load_folder.clone();
        Callback::new(move |parent_id: String| {
            if !trash_mode.get_untracked() && current_id.get_untracked() == parent_id {
                load_folder.run(parent_id);
            }
        })
    };
    let upload_feedback = {
        let feedback = feedback;
        Callback::new(move |message: String| feedback.set(message))
    };
    let uploads = UploadController::new(
        current_id,
        current,
        trash_mode,
        file_input,
        folder_input,
        upload_refresh,
        upload_feedback,
        on_logout.clone(),
    );
    let uploads_for_cleanup = leptos::__reexports::send_wrapper::SendWrapper::new(uploads.clone());
    on_cleanup(move || uploads_for_cleanup.dispose());

    let clear_selection = {
        let selected_ids = selected_ids;
        Callback::new(move |(): ()| selected_ids.set(HashSet::new()))
    };
    let toggle_selection = {
        let selected_ids = selected_ids;
        Callback::new(move |item: File| {
            selected_ids.update(|selected| {
                if !selected.insert(item.id.clone()) {
                    selected.remove(&item.id);
                }
            });
        })
    };
    let select_all = {
        let items = items;
        let selected_ids = selected_ids;
        Callback::new(move |(): ()| {
            let entries = items.get_untracked();
            if selected_ids.get_untracked().len() == entries.len() {
                selected_ids.set(HashSet::new());
            } else {
                selected_ids.set(entries.into_iter().map(|item| item.id).collect());
            }
        })
    };

    let show_create_folder = {
        let dialog = dialog;
        let dialog_value = dialog_value;
        let dialog_error = dialog_error;
        Callback::new(move |(): ()| {
            dialog_value.set(String::new());
            dialog_error.set(String::new());
            dialog.set(Some(DialogState::CreateFolder));
        })
    };
    let show_rename = {
        let dialog = dialog;
        let dialog_value = dialog_value;
        let dialog_error = dialog_error;
        let items = items;
        let selected_ids = selected_ids;
        Callback::new(move |(): ()| {
            let Some(item) = items
                .get_untracked()
                .into_iter()
                .find(|item| selected_ids.get_untracked().contains(&item.id))
            else {
                return;
            };
            dialog_value.set(item.name);
            dialog_error.set(String::new());
            dialog.set(Some(DialogState::Rename { id: item.id }));
        })
    };
    let show_delete = {
        let dialog = dialog;
        let dialog_value = dialog_value;
        let dialog_error = dialog_error;
        let selected_ids = selected_ids;
        Callback::new(move |(): ()| {
            if selected_ids.get_untracked().is_empty() {
                return;
            }
            dialog_value.set(String::new());
            dialog_error.set(String::new());
            dialog.set(Some(DialogState::Delete));
        })
    };
    let show_restore = {
        let dialog = dialog;
        let dialog_value = dialog_value;
        let dialog_error = dialog_error;
        let selected_ids = selected_ids;
        Callback::new(move |(): ()| {
            if selected_ids.get_untracked().is_empty() {
                return;
            }
            dialog_value.set(String::new());
            dialog_error.set(String::new());
            dialog.set(Some(DialogState::Restore));
        })
    };
    let show_purge = {
        let dialog = dialog;
        let dialog_value = dialog_value;
        let dialog_error = dialog_error;
        let selected_ids = selected_ids;
        Callback::new(move |(): ()| {
            if selected_ids.get_untracked().is_empty() {
                return;
            }
            dialog_value.set(String::new());
            dialog_error.set(String::new());
            dialog.set(Some(DialogState::Purge));
        })
    };
    let show_empty_trash = {
        let dialog = dialog;
        let dialog_value = dialog_value;
        let dialog_error = dialog_error;
        let items = items;
        let trash_mode = trash_mode;
        Callback::new(move |(): ()| {
            if !trash_mode.get_untracked() || items.get_untracked().is_empty() {
                return;
            }
            dialog_value.set(String::new());
            dialog_error.set(String::new());
            dialog.set(Some(DialogState::EmptyTrash));
        })
    };

    let submit_dialog = {
        let dialog = dialog;
        let dialog_value = dialog_value;
        let dialog_busy = dialog_busy;
        let dialog_error = dialog_error;
        let selected_ids = selected_ids;
        let current_id = current_id;
        let trash_mode = trash_mode;
        let feedback = feedback;
        let load_folder = load_folder.clone();
        let load_trash = load_trash.clone();
        let on_logout = on_logout.clone();
        Callback::new(move |value: String| {
            let Some(state) = dialog.get_untracked() else {
                return;
            };
            if dialog_busy.get_untracked() {
                return;
            }
            dialog_busy.set(true);
            dialog_error.set(String::new());
            let mut ids: Vec<String> = selected_ids.get_untracked().into_iter().collect();
            ids.sort_unstable();
            let parent_id = current_id.get_untracked();
            let refresh_parent_id = parent_id.clone();
            let in_trash = trash_mode.get_untracked();
            let refresh_folder = load_folder.clone();
            let refresh_trash = load_trash.clone();
            let logout = on_logout.clone();

            leptos::task::spawn_local(async move {
                let result: Result<String, api::RequestError> = async {
                    match state {
                        DialogState::CreateFolder => {
                            let name = value.trim().to_owned();
                            if name.is_empty() {
                                Err(api::RequestError {
                                    status: 0,
                                    code: None,
                                    message: "文件夹名称不能为空".to_owned(),
                                })
                            } else {
                                api::create_directory(&CreateDirectoryRequest { parent_id, name })
                                    .await
                                    .map(|_| "文件夹已创建".to_owned())
                            }
                        }
                        DialogState::Rename { id } => {
                            let name = value.trim().to_owned();
                            if name.is_empty() {
                                Err(api::RequestError {
                                    status: 0,
                                    code: None,
                                    message: "名称不能为空".to_owned(),
                                })
                            } else {
                                api::patch_file(
                                    &id,
                                    &PatchFileRequest {
                                        name: Some(name),
                                        parent_id: None,
                                    },
                                )
                                .await
                                .map(|_| "名称已更新".to_owned())
                            }
                        }
                        DialogState::Delete => {
                            for id in ids {
                                api::delete_file(&id).await?;
                            }
                            Ok("已移入回收站".to_owned())
                        }
                        DialogState::Restore => {
                            for id in ids {
                                api::restore_trash(&id).await?;
                            }
                            Ok("已恢复所选项目".to_owned())
                        }
                        DialogState::Purge => {
                            for id in ids {
                                api::purge_trash(&id).await?;
                            }
                            Ok("已永久删除所选项目".to_owned())
                        }
                        DialogState::EmptyTrash => {
                            api::empty_trash().await?;
                            Ok("回收站已清空".to_owned())
                        }
                    }
                }
                .await;

                dialog_busy.set(false);
                match result {
                    Ok(message) => {
                        dialog.set(None);
                        dialog_value.set(String::new());
                        dialog_error.set(String::new());
                        selected_ids.set(HashSet::new());
                        if in_trash {
                            refresh_trash.run(());
                        } else {
                            refresh_folder.run(refresh_parent_id);
                        }
                        feedback.set(message);
                    }
                    Err(request_error) if request_error.is_unauthorized() => {
                        dialog.set(None);
                        logout.run(());
                    }
                    Err(request_error) => dialog_error.set(request_error.message),
                }
            });
        })
    };
    let close_dialog = {
        let dialog = dialog;
        let dialog_value = dialog_value;
        let dialog_error = dialog_error;
        let dialog_busy = dialog_busy;
        Callback::new(move |(): ()| {
            if !dialog_busy.get_untracked() {
                dialog.set(None);
                dialog_value.set(String::new());
                dialog_error.set(String::new());
            }
        })
    };

    let open_item = {
        let load_folder = load_folder.clone();
        let trash_mode = trash_mode;
        Callback::new(move |item: File| {
            if trash_mode.get_untracked() {
                return;
            }
            if item.kind == FileKind::Directory {
                load_folder.run(item.id);
            } else {
                open_file(&item);
            }
        })
    };

    let pathname = web_sys::window()
        .and_then(|window| window.location().pathname().ok())
        .unwrap_or_default();
    load_folder.run(folder_id(&pathname, ROOT_ID));

    let username = session.username.clone();
    let initial = username
        .chars()
        .next()
        .map(|character| character.to_uppercase().collect::<String>())
        .unwrap_or_else(|| "R".to_owned());
    let logout = {
        let on_logout = on_logout.clone();
        Callback::new(move |(): ()| {
            let on_logout = on_logout.clone();
            leptos::task::spawn_local(async move {
                api::logout().await;
                on_logout.run(());
            });
        })
    };

    let return_home = {
        let load_folder = load_folder.clone();
        Callback::new(move |(): ()| load_folder.run(ROOT_ID.to_owned()))
    };

    let uploads_for_view = leptos::__reexports::send_wrapper::SendWrapper::new(uploads.clone());
    let upload_from_top = uploads_for_view.clone();
    let upload_folder_from_top = uploads_for_view.clone();
    let shell_upload = uploads_for_view.clone();
    let shell_upload_leave = uploads_for_view.clone();
    let shell_upload_drop = uploads_for_view.clone();
    let upload_surface = uploads_for_view.clone();

    view! {
        <div
            class="app-shell"
            class:sidebar-collapsed=move || sidebar_collapsed.get()
            on:dragover=move |event: web_sys::DragEvent| shell_upload.on_drag_over(event)
            on:dragleave=move |event: web_sys::DragEvent| shell_upload_leave.on_drag_leave(event)
            on:drop=move |event: web_sys::DragEvent| shell_upload_drop.on_drop(event)
        >
            <header class="topbar">
                <div class="topbar-left">
                    <button
                        class="logo brand-button"
                        type="button"
                        title="回到我的文件"
                        aria-label="回到我的文件"
                        on:click={move |_| return_home.run(())}
                    >
                        <img class="brand-logo" src="/revaro-logo.svg" alt="" aria-hidden="true" />
                    </button>
                </div>
                <div class="top-actions">
                    <span class="connection" aria-label="服务已连接">
                        <i></i>
                        "服务已连接"
                    </span>
                    <details class="upload-menu">
                        <summary class="secondary" title="上传文件">"上传"</summary>
                        <div class="upload-menu-popover">
                            <button type="button" on:click=move |_| upload_from_top.choose_files()>
                                <span aria-hidden="true">"↑"</span>
                                <div>
                                    <b>"上传文件"</b>
                                    <small>"选择一个或多个文件"</small>
                                </div>
                            </button>
                            <button type="button" on:click=move |_| upload_folder_from_top.choose_folder()>
                                <span aria-hidden="true">"↥"</span>
                                <div>
                                    <b>"上传文件夹"</b>
                                    <small>"保留目录结构"</small>
                                </div>
                            </button>
                        </div>
                    </details>
                    <button
                        class="trash-button"
                        type="button"
                        title="回收站"
                        aria-label="打开回收站"
                        on:click={move |_| load_trash.run(())}
                    >
                        {icons::trash()}
                    </button>
                    <button
                        class="account-button"
                        type="button"
                        title="退出登录"
                        on:click={move |_| logout.run(())}
                    >
                        <span class="avatar-badge">{initial.clone()}</span>
                        <span class="account-copy">
                            <b>{username.clone()}</b>
                            <small>"退出登录"</small>
                        </span>
                    </button>
                </div>
            </header>

            <aside class="app-sidebar" class:collapsed=move || sidebar_collapsed.get()>
                <div class="sidebar-head">
                    <button
                        class="sidebar-collapse"
                        type="button"
                        title=move || if sidebar_collapsed.get() { "展开分类栏" } else { "收起分类栏" }
                        aria-label=move || if sidebar_collapsed.get() { "展开分类栏" } else { "收起分类栏" }
                        on:click=move |_| sidebar_collapsed.update(|collapsed| *collapsed = !*collapsed)
                    >
                        {move || if sidebar_collapsed.get() {
                            icons::panel_left_open().into_any()
                        } else {
                            icons::panel_left_close().into_any()
                        }}
                        <span class:hidden=move || sidebar_collapsed.get()>"文件浏览"</span>
                    </button>
                </div>
                <nav class="sidebar-nav" aria-label="主导航">
                    <section class="sidebar-category" class:active=move || !trash_mode.get()>
                        <button
                            class="category-main"
                            class:active=move || !trash_mode.get()
                            type="button"
                            title="我的文件"
                            aria-label="我的文件"
                            on:click={move |_| return_home.run(())}
                        >
                            <span class="category-icon">{icons::folder_closed()}</span>
                            <span class="category-label" class:hidden=move || sidebar_collapsed.get()>"我的文件"</span>
                        </button>
                    </section>
                </nav>
                <div class="sidebar-foot">
                    <button
                        class="category-main trash-entry"
                        class:active=move || trash_mode.get()
                        type="button"
                        title="回收站"
                        aria-label="回收站"
                        on:click={move |_| load_trash.run(())}
                    >
                        <span class="category-icon">{icons::trash()}</span>
                        <span class="category-label" class:hidden=move || sidebar_collapsed.get()>"回收站"</span>
                    </button>
                </div>
            </aside>

            <section class="content">
                <div class="content-head">
                    <div class="folder-heading">
                        <Show when=move || !trash_mode.get() fallback=|| ()>
                            <nav class="breadcrumbs" aria-label="当前路径">
                                {move || {
                                    let path = breadcrumbs.get();
                                    let last = path.len().saturating_sub(1);
                                    path.into_iter()
                                        .enumerate()
                                        .map(|(index, crumb)| {
                                            let id = crumb.id;
                                            let label = if crumb.name.is_empty() {
                                                "我的文件".to_owned()
                                            } else {
                                                crumb.name
                                            };
                                            let open = load_folder.clone();
                                            view! {
                                                <span class="breadcrumb-item">
                            <Show when=move || { index > 0 } fallback=|| ()>
                                                        <span class="breadcrumb-separator" aria-hidden="true">"/"</span>
                                                    </Show>
                                                    <button
                                                        type="button"
                                                        class:current=index == last
                                                        title=label.clone()
                                                        aria-current=if index == last { Some("page") } else { None }
                                                        on:click=move |_| open.run(id.clone())
                                                    >
                                                        {label.clone()}
                                                    </button>
                                                </span>
                                            }
                                        })
                                        .collect_view()
                                }}
                            </nav>
                        </Show>
                        <div class="title-row">
                            <h1>
                                {move || {
                                    if trash_mode.get() {
                                        "回收站".to_owned()
                                    } else {
                                        current
                                            .get()
                                            .map(|file| {
                                                if file.name.is_empty() {
                                                    "我的文件".to_owned()
                                                } else {
                                                    file.name
                                                }
                                            })
                                            .unwrap_or_else(|| "我的文件".to_owned())
                                    }
                                }}
                            </h1>
                        </div>
                        <p class="folder-meta">
                            <span>{move || format!("{} 个项目", items.get().len())}</span>
                            <i></i>
                            <Show
                                when=move || trash_mode.get()
                                fallback=move || view! {
                                    <span>{move || format!("共 {} 个文件", file_count.get().max(0))}</span>
                                    <i></i>
                                    <span>{move || format_size(non_negative(total_bytes.get()))}</span>
                                }
                            >
                                <span>{move || format_size(non_negative(total_bytes.get()))}</span>
                                <i></i>
                                <span>"已删除的文件将在 30 天后永久删除"</span>
                            </Show>
                        </p>
                    </div>
                    <div class="actions">
                        <Show when=move || trash_mode.get() fallback=move || view! {
                            <div class="view-switch file-view-switch" role="group" aria-label="文件视图切换">
                                <button
                                    type="button"
                                    class:active=move || view_mode.get() == ViewMode::Grid
                                    aria-pressed=move || view_mode.get() == ViewMode::Grid
                                    title="方块视图"
                                    on:click=move |_| view_mode.set(ViewMode::Grid)
                                >
                                    "方块"
                                </button>
                                <button
                                    type="button"
                                    class:active=move || view_mode.get() == ViewMode::List
                                    aria-pressed=move || view_mode.get() == ViewMode::List
                                    title="列表视图"
                                    on:click=move |_| view_mode.set(ViewMode::List)
                                >
                                    "列表"
                                </button>
                            </div>
                            <button class="secondary" type="button" on:click={move |_| show_create_folder.run(())}>
                                "新建文件夹"
                            </button>
                        }>
                            <button class="secondary" type="button" on:click={move |_| return_home.run(())}>
                                "返回我的文件"
                            </button>
                            <button
                                class="trash-empty-action"
                                type="button"
                                prop:disabled=move || items.get().is_empty()
                                on:click={move |_| show_empty_trash.run(())}
                            >
                                "清空回收站"
                            </button>
                        </Show>
                    </div>
                </div>

                <Show when=move || !selected_ids.get().is_empty() fallback=|| ()>
                    <SelectionToolbar
                        items=items
                        selected_ids=selected_ids
                        trash_mode=trash_mode
                        on_clear=clear_selection.clone()
                        on_select_all=select_all.clone()
                        on_rename=show_rename.clone()
                        on_delete=show_delete.clone()
                        on_restore=show_restore.clone()
                        on_purge=show_purge.clone()
                    />
                </Show>

                {move || {
                    if loading.get() {
                        view! {
                            <div class="state" aria-live="polite">
                                <div class="spinner"></div>
                                <p>"正在读取文件…"</p>
                            </div>
                        }
                        .into_any()
                    } else if !error.get().is_empty() {
                        let message = error.get();
                        let retry = if trash_mode.get() {
                            let load_trash = load_trash.clone();
                            Callback::new(move |(): ()| load_trash.run(()))
                        } else {
                            let load_folder = load_folder.clone();
                            let id = current_id.get_untracked();
                            Callback::new(move |(): ()| load_folder.run(id.clone()))
                        };
                        view! {
                            <div class="state" role="alert">
                                <div class="empty-icon" aria-hidden="true">"!"</div>
                                <h3>"读取失败"</h3>
                                <p>{message}</p>
                                <button class="secondary" type="button" on:click=move |_| retry.run(())>
                                    "重试"
                                </button>
                            </div>
                        }
                        .into_any()
                    } else if items.get().is_empty() {
                        let heading = if trash_mode.get() {
                            "回收站是空的"
                        } else {
                            "这里还没有文件"
                        };
                        let description = if trash_mode.get() {
                            "被删除的文件会显示在这里。"
                        } else {
                            "上传文件或创建文件夹后，它们会显示在这里。"
                        };
                        view! {
                            <div class="state">
                                <div class="empty-icon" aria-hidden="true">{icons::folder_closed()}</div>
                                <h3>{heading}</h3>
                                <p>{description}</p>
                            </div>
                        }
                        .into_any()
                    } else if view_mode.get() == ViewMode::Grid {
                        view! {
                            <div class="file-grid">
                                <For each=move || items.get() key=|item| item.id.clone() let:item>
                                    <FileTile
                                        item=item
                                        trash_mode=trash_mode
                                        selected_ids=selected_ids
                                        on_select=toggle_selection.clone()
                                        on_open=open_item.clone()
                                    />
                                </For>
                            </div>
                        }
                        .into_any()
                    } else {
                        view! {
                            <div class="file-rows">
                                <For each=move || items.get() key=|item| item.id.clone() let:item>
                                    <FileRow
                                        item=item
                                        trash_mode=trash_mode
                                        selected_ids=selected_ids
                                        on_select=toggle_selection.clone()
                                        on_open=open_item.clone()
                                    />
                                </For>
                            </div>
                        }
                        .into_any()
                    }
                }}
            </section>
            <Show when=move || !feedback.get().is_empty() fallback=|| ()>
                <div class="toast" role="status">{move || feedback.get()}</div>
            </Show>
            {move || {
                if let Some(state) = dialog.get() {
                    let (title, message, confirm_label, danger, input) =
                        dialog_config(&state, selected_ids.get().len());
                    view! {
                        <ActionDialog
                            title=title
                            message=message
                            confirm_label=confirm_label
                            danger=danger
                            input=input
                            value=dialog_value
                            busy=dialog_busy
                            error=dialog_error
                            on_cancel=close_dialog.clone()
                            on_confirm=submit_dialog.clone()
                        />
                    }
                    .into_any()
                } else {
                    ().into_any()
                }
            }}
            <UploadSurface controller=upload_surface />
        </div>
    }
}

fn dialog_config(
    state: &DialogState,
    selected_count: usize,
) -> (String, String, String, bool, bool) {
    match state {
        DialogState::CreateFolder => (
            "新建文件夹".to_owned(),
            "给当前文件夹起一个名称。".to_owned(),
            "创建".to_owned(),
            false,
            true,
        ),
        DialogState::Rename { .. } => (
            "重命名".to_owned(),
            "名称修改后会立即保存。".to_owned(),
            "保存".to_owned(),
            false,
            true,
        ),
        DialogState::Delete => (
            "移入回收站".to_owned(),
            format!("确定要把选中的 {selected_count} 项移入回收站吗？"),
            "移入回收站".to_owned(),
            true,
            false,
        ),
        DialogState::Restore => (
            "恢复项目".to_owned(),
            format!("选中的 {selected_count} 项会恢复到原来的位置。"),
            "恢复".to_owned(),
            false,
            false,
        ),
        DialogState::Purge => (
            "永久删除".to_owned(),
            format!("选中的 {selected_count} 项及其内容将被永久删除，无法撤销。"),
            "永久删除".to_owned(),
            true,
            false,
        ),
        DialogState::EmptyTrash => (
            "清空回收站".to_owned(),
            "回收站中的所有内容将被永久删除，无法撤销。".to_owned(),
            "清空回收站".to_owned(),
            true,
            false,
        ),
    }
}

#[component]
fn FileTile(
    item: File,
    trash_mode: RwSignal<bool>,
    selected_ids: RwSignal<HashSet<String>>,
    on_select: Callback<File>,
    on_open: Callback<File>,
) -> impl IntoView {
    let name = item.name.clone();
    let item_for_click = item.clone();
    let item_for_key = item.clone();
    let item_for_select_click = item.clone();
    let item_for_select_key = item.clone();
    let item_for_meta = item.clone();
    let item_id_for_class = item.id.clone();
    let item_id_for_title = item.id.clone();
    let item_id_for_label = item.id.clone();
    let item_id_for_pressed = item.id.clone();
    let item_id_for_active = item.id.clone();
    let class = tile_class(&item);
    let label = format!(
        "{}，{}",
        name,
        if item.kind == FileKind::Directory {
            "文件夹"
        } else {
            "文件"
        }
    );
    let on_select_click = on_select.clone();
    let on_open_click = on_open.clone();
    let on_open_key = on_open;

    view! {
        <article
            class=class
            class:selected=move || selected_ids.get().contains(&item_id_for_class)
            role="button"
            tabindex="0"
            aria-label=label
            on:click=move |_| {
                if !trash_mode.get_untracked() || item_for_click.kind == FileKind::File {
                    on_open_click.run(item_for_click.clone());
                }
            }
            on:keydown=move |event: web_sys::KeyboardEvent| {
                if event.key() == "Enter" && !trash_mode.get_untracked() {
                    event.prevent_default();
                    on_open_key.run(item_for_key.clone());
                } else if event.key() == " " {
                    event.prevent_default();
                    on_select.run(item_for_select_key.clone());
                }
            }
        >
            <button
                class="card-select"
                type="button"
                title=move || if selected_ids.get().contains(&item_id_for_title) { "取消选择" } else { "选择项目" }
                aria-label=move || if selected_ids.get().contains(&item_id_for_label) { "取消选择" } else { "选择项目" }
                aria-pressed=move || selected_ids.get().contains(&item_id_for_pressed)
                class:active=move || selected_ids.get().contains(&item_id_for_active)
                on:click=move |event: web_sys::MouseEvent| {
                    event.stop_propagation();
                    on_select_click.run(item_for_select_click.clone());
                }
                on:keydown=move |event: web_sys::KeyboardEvent| event.stop_propagation()
            >
                <svg viewBox="0 0 24 24" aria-hidden="true">
                    <path d="m5 12 4 4L19 6"></path>
                </svg>
            </button>
            <div class="card-preview" title=move || if item.kind == FileKind::Directory { "打开文件夹" } else { "打开文件" }>
                {file_preview(&item)}
            </div>
            <div class="card-info">
                <strong title=name.clone()>{name.clone()}</strong>
                <small>{move || display_meta(&item_for_meta, trash_mode.get())}</small>
            </div>
        </article>
    }
}

#[component]
fn FileRow(
    item: File,
    trash_mode: RwSignal<bool>,
    selected_ids: RwSignal<HashSet<String>>,
    on_select: Callback<File>,
    on_open: Callback<File>,
) -> impl IntoView {
    let name = item.name.clone();
    let item_for_click = item.clone();
    let item_for_key = item.clone();
    let item_for_select_click = item.clone();
    let item_for_select_key = item.clone();
    let item_for_meta = item.clone();
    let item_id_for_class = item.id.clone();
    let item_id_for_title = item.id.clone();
    let item_id_for_label = item.id.clone();
    let item_id_for_pressed = item.id.clone();
    let item_id_for_active = item.id.clone();
    let class = row_class(&item);
    let on_select_click = on_select.clone();
    let on_open_click = on_open.clone();
    let on_open_key = on_open;
    view! {
        <article
            class=class
            class:selected=move || selected_ids.get().contains(&item_id_for_class)
            role="button"
            tabindex="0"
            aria-label=name.clone()
            on:click=move |_| {
                if !trash_mode.get_untracked() || item_for_click.kind == FileKind::File {
                    on_open_click.run(item_for_click.clone());
                }
            }
            on:keydown=move |event: web_sys::KeyboardEvent| {
                if event.key() == "Enter" && !trash_mode.get_untracked() {
                    event.prevent_default();
                    on_open_key.run(item_for_key.clone());
                } else if event.key() == " " {
                    event.prevent_default();
                    on_select.run(item_for_select_key.clone());
                }
            }
        >
            <button
                class="row-select"
                type="button"
                title=move || if selected_ids.get().contains(&item_id_for_title) { "取消选择" } else { "选择项目" }
                aria-label=move || if selected_ids.get().contains(&item_id_for_label) { "取消选择" } else { "选择项目" }
                aria-pressed=move || selected_ids.get().contains(&item_id_for_pressed)
                class:active=move || selected_ids.get().contains(&item_id_for_active)
                on:click=move |event: web_sys::MouseEvent| {
                    event.stop_propagation();
                    on_select_click.run(item_for_select_click.clone());
                }
                on:keydown=move |event: web_sys::KeyboardEvent| event.stop_propagation()
            >
                <svg viewBox="0 0 24 24" aria-hidden="true">
                    <path d="m5 12 4 4L19 6"></path>
                </svg>
            </button>
            <div class="row-preview">{file_preview(&item)}</div>
            <div class="row-info">
                <strong title=name.clone()>{name.clone()}</strong>
                <small>{move || display_meta(&item_for_meta, trash_mode.get())}</small>
            </div>
        </article>
    }
}

fn tile_class(file: &File) -> String {
    let mut class = String::from("file-card fallback-tile");
    if file.kind == FileKind::Directory {
        class.push_str(" folder-tile");
    } else if classify::is_editable(file) {
        class.push_str(" document-tile");
    } else if classify::is_book(file) {
        class.push_str(" book-tile");
    } else if classify::is_audio(file) {
        class.push_str(" audio-tile");
    }
    if classify::is_image(file) || (classify::is_audio(file) && file.has_cover) {
        class.push_str(" preview-tile");
    }
    if file.status != FileStatus::Ready {
        class.push_str(" mutedrow");
    }
    class
}

fn row_class(file: &File) -> String {
    if file.status == FileStatus::Ready {
        "file-row".to_owned()
    } else {
        "file-row mutedrow".to_owned()
    }
}

fn display_meta(file: &File, trash_mode: bool) -> String {
    if file.kind == FileKind::Directory {
        return "文件夹".to_owned();
    }
    let size = format_size(non_negative(file.size));
    if trash_mode {
        format!("{size} · 已移入回收站")
    } else if file.status == FileStatus::Ready {
        size
    } else {
        format!("{size} · {}", file.status)
    }
}

fn non_negative(value: i64) -> u64 {
    u64::try_from(value).unwrap_or(0)
}

fn file_preview(file: &File) -> AnyView {
    if classify::is_image(file) || (classify::is_audio(file) && file.has_cover) {
        let src = format!("/api/files/{}/thumbnail", file.id);
        return view! {
            <img class="ui-image" src=src alt=file.name.clone() loading="lazy" draggable="false" />
        }
        .into_any();
    }
    file_icon(file)
}

fn file_icon(file: &File) -> AnyView {
    if file.kind == FileKind::Directory {
        view! {
            <svg class="file-type-icon folder-type-icon" viewBox="0 0 96 96" aria-hidden="true">
                <path class="folder-back" d="M10 23c0-4 3-7 7-7h21l10 11h31c4 0 7 3 7 7v9H10Z"></path>
                <path class="folder-front" d="M8 38c0-4 3-7 7-7h66c5 0 8 4 7 9l-7 35c-1 4-4 6-8 6H16c-4 0-7-3-7-7Z"></path>
                <path class="folder-highlight" d="M17 38h62l-1 6H16Z"></path>
            </svg>
        }
        .into_any()
    } else if classify::is_video(file) {
        view! {
            <span class="large-video" aria-hidden="true">
                <svg viewBox="0 0 24 24"><path d="m9 7 8 5-8 5Z"></path></svg>
            </span>
        }
        .into_any()
    } else if classify::is_audio(file) {
        view! {
            <span class="file-type-icon generic-type-icon" aria-hidden="true">{icons::music()}</span>
        }
        .into_any()
    } else if classify::is_book(file) {
        view! {
            <span class="file-type-icon book-type-icon" aria-hidden="true">{icons::book_open()}</span>
        }
        .into_any()
    } else if classify::is_editable(file) {
        view! {
            <span class="file-type-icon document-type-icon" aria-hidden="true">{icons::file_text()}</span>
        }
        .into_any()
    } else {
        view! {
            <span class="file-type-icon generic-type-icon" aria-hidden="true">{icons::file()}</span>
        }
        .into_any()
    }
}

fn open_file(file: &File) {
    let path = if classify::is_image(file) || classify::is_video(file) || classify::is_audio(file) {
        format!("/api/files/{}/preview", file.id)
    } else {
        format!("/api/files/{}/download", file.id)
    };
    if let Some(window) = web_sys::window() {
        let _ = window.open_with_url_and_target(&path, "_blank");
    }
}

fn replace_folder_url(id: &str) {
    let Some(window) = web_sys::window() else {
        return;
    };
    let url = folder_url(id, ROOT_ID);
    if let Ok(history) = window.history() {
        let _ = history.replace_state_with_url(&JsValue::NULL, "", Some(&url));
    }
}

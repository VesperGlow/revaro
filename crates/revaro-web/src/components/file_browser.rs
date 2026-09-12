//! The first usable authenticated browser screen.
//!
//! This slice owns folder navigation, breadcrumbs, the grid/list choice and a
//! read-only trash view. Mutating actions and specialised readers are separate
//! stages; a card therefore opens a file through the already authenticated
//! preview/download endpoints instead of presenting controls that have no
//! handler yet.

use leptos::prelude::*;
use revaro_core::api::auth::Session;
use revaro_core::api::files::{Children, FileDetail};
use revaro_core::classify;
use revaro_core::ids::ROOT_ID;
use revaro_core::model::{File, FileKind, FileStatus};
use wasm_bindgen::JsValue;

use crate::api;
use crate::logic::format::format_size;
use crate::logic::routing::{folder_id, folder_url};

use super::icons;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ViewMode {
    Grid,
    List,
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
        let on_logout = on_logout.clone();
        Callback::new(move |id: String| {
            let sequence = request_sequence.get_untracked().wrapping_add(1);
            request_sequence.set(sequence);
            loading.set(true);
            error.set(String::new());
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
        let on_logout = on_logout.clone();
        Callback::new(move |(): ()| {
            let sequence = request_sequence.get_untracked().wrapping_add(1);
            request_sequence.set(sequence);
            loading.set(true);
            error.set(String::new());
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

    view! {
        <div class="app-shell" class:sidebar-collapsed=move || sidebar_collapsed.get()>
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
                        }>
                            <button class="secondary" type="button" on:click={move |_| return_home.run(())}>
                                "返回我的文件"
                            </button>
                        </Show>
                    </div>
                </div>

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
                                        on_open=open_item.clone()
                                    />
                                </For>
                            </div>
                        }
                        .into_any()
                    }
                }}
            </section>
        </div>
    }
}

#[component]
fn FileTile(item: File, trash_mode: RwSignal<bool>, on_open: Callback<File>) -> impl IntoView {
    let name = item.name.clone();
    let item_for_click = item.clone();
    let item_for_meta = item.clone();
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
    let on_open_click = on_open.clone();
    let on_open_key = on_open;

    view! {
        <article
            class=class
            role="button"
            tabindex="0"
            aria-label=label
            on:click=move |_| {
                if !trash_mode.get_untracked() || item_for_click.kind == FileKind::File {
                    on_open_click.run(item_for_click.clone());
                }
            }
            on:keydown=move |event: web_sys::KeyboardEvent| {
                if event.key() == "Enter" || event.key() == " " {
                    event.prevent_default();
                    if !trash_mode.get_untracked() || item.kind == FileKind::File {
                        on_open_key.run(item.clone());
                    }
                }
            }
        >
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
fn FileRow(item: File, trash_mode: RwSignal<bool>, on_open: Callback<File>) -> impl IntoView {
    let name = item.name.clone();
    let item_for_click = item.clone();
    let item_for_meta = item.clone();
    let class = row_class(&item);
    let on_open_click = on_open.clone();
    let on_open_key = on_open;
    view! {
        <article
            class=class
            role="button"
            tabindex="0"
            aria-label=name.clone()
            on:click=move |_| {
                if !trash_mode.get_untracked() || item_for_click.kind == FileKind::File {
                    on_open_click.run(item_for_click.clone());
                }
            }
            on:keydown=move |event: web_sys::KeyboardEvent| {
                if event.key() == "Enter" || event.key() == " " {
                    event.prevent_default();
                    if !trash_mode.get_untracked() || item.kind == FileKind::File {
                        on_open_key.run(item.clone());
                    }
                }
            }
        >
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

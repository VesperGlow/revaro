//! Breadcrumbs and file-browser actions.

use leptos::prelude::*;
use revaro_core::model::File;
use wasm_bindgen::JsCast;
use wasm_bindgen::closure::Closure;

use crate::browser;
use crate::logic::format::format_size;

use super::file_browser::ViewMode;
use super::icons;

/// The reference file-browser header, including its two disclosure menus.
#[component]
pub fn FileBrowserHeader(
    breadcrumbs: RwSignal<Vec<File>>,
    current: RwSignal<Option<File>>,
    item_count: RwSignal<Vec<File>>,
    total_bytes: RwSignal<i64>,
    file_count: RwSignal<i64>,
    trash_mode: RwSignal<bool>,
    view_mode: RwSignal<ViewMode>,
    on_open_folder: Callback<String>,
    on_new_document: Callback<()>,
    on_create_folder: Callback<()>,
    on_upload_files: Callback<()>,
    on_upload_folder: Callback<()>,
    on_leave_trash: Callback<()>,
    on_empty_trash: Callback<()>,
) -> impl IntoView {
    let create_menu = NodeRef::<leptos::html::Details>::new();
    let upload_menu = NodeRef::<leptos::html::Details>::new();

    let create_for_outside = create_menu;
    let upload_for_outside = upload_menu;
    let mut outside = browser::on_pointerdown(move |event| {
        let target = event
            .target()
            .and_then(|target| target.dyn_into::<web_sys::Node>().ok());
        let Some(target) = target else {
            return;
        };
        if let Some(details) = create_for_outside.get()
            && details.open()
            && !details.contains(Some(&target))
        {
            details.set_open(false);
        }
        if let Some(details) = upload_for_outside.get()
            && details.open()
            && !details.contains(Some(&target))
        {
            details.set_open(false);
        }
    });
    let create_for_escape = create_menu;
    let upload_for_escape = upload_menu;
    let mut escape = browser::on_keydown(move |event| {
        if event.key() != "Escape" {
            return;
        }
        let mut closed = false;
        if let Some(details) = create_for_escape.get()
            && details.open()
        {
            details.set_open(false);
            closed = true;
        }
        if let Some(details) = upload_for_escape.get()
            && details.open()
        {
            details.set_open(false);
            closed = true;
        }
        if closed {
            event.prevent_default();
        }
    });
    on_cleanup(move || {
        outside.release();
        escape.release();
    });

    let path_items = Signal::derive_local(move || {
        let path = breadcrumbs.get();
        if path.is_empty() {
            current.get().into_iter().collect::<Vec<_>>()
        } else {
            path
        }
    });

    // The Vue header always reveals the current end of a long breadcrumb
    // after navigation. Keep that behavior on narrow screens and deep paths;
    // waiting one task lets the reactive path children finish rendering before
    // measuring the scroll width.
    let breadcrumb_nav = NodeRef::<leptos::html::Nav>::new();
    let breadcrumb_current = current;
    Effect::new(move |_| {
        let _ = breadcrumb_current.get();
        let Some(window) = web_sys::window() else {
            return;
        };
        let nav = breadcrumb_nav;
        let callback = Closure::once_into_js(move || {
            if let Some(nav) = nav.get() {
                nav.set_scroll_left(nav.scroll_width());
            }
        });
        let _ = window
            .set_timeout_with_callback_and_timeout_and_arguments_0(callback.unchecked_ref(), 0);
    });
    let close_create = Callback::new(move |(): ()| {
        if let Some(details) = create_menu.get() {
            details.set_open(false);
        }
    });
    let close_upload = Callback::new(move |(): ()| {
        if let Some(details) = upload_menu.get() {
            details.set_open(false);
        }
    });
    let create_document = {
        let close = close_create.clone();
        let action = on_new_document.clone();
        Callback::new(move |(): ()| {
            close.run(());
            action.run(());
        })
    };
    let create_folder = {
        let close = close_create;
        let action = on_create_folder.clone();
        Callback::new(move |(): ()| {
            close.run(());
            action.run(());
        })
    };
    let upload_files = {
        let close = close_upload.clone();
        let action = on_upload_files.clone();
        Callback::new(move |(): ()| {
            close.run(());
            action.run(());
        })
    };
    let upload_folder = {
        let close = close_upload;
        let action = on_upload_folder.clone();
        Callback::new(move |(): ()| {
            close.run(());
            action.run(());
        })
    };

    view! {
        <div class="content-head">
            <div class="folder-heading">
                <Show when=move || !trash_mode.get() && !path_items.get().is_empty() fallback=|| ()>
                    <nav node_ref=breadcrumb_nav class="breadcrumbs" aria-label="当前路径">
                        {move || {
                            let path = path_items.get();
                            let last = path.len().saturating_sub(1);
                            path.into_iter().enumerate().map(|(index, crumb)| {
                                let id = crumb.id;
                                let label = if crumb.name.is_empty() { "我的文件".to_owned() } else { crumb.name };
                                let open = on_open_folder.clone();
                                view! {
                                    <Show when=move || { index > 0 } fallback=|| ()>
                                        {icons::breadcrumb_separator()}
                                    </Show>
                                    <button
                                        type="button"
                                        class:current=index == last
                                        title=label.clone()
                                        aria-current=if index == last { Some("page") } else { None }
                                        on:click=move |_| open.run(id.clone())
                                    >{label.clone()}</button>
                                }
                            }).collect_view()
                        }}
                    </nav>
                </Show>
                <div class="title-row">
                    <h1 title=move || if trash_mode.get() { "回收站".to_owned() } else { current.get().map(|file| file.name).filter(|name| !name.is_empty()).unwrap_or_else(|| "我的文件".to_owned()) }>
                        {move || if trash_mode.get() { "回收站".to_owned() } else { current.get().map(|file| if file.name.is_empty() { "我的文件".to_owned() } else { file.name }).unwrap_or_else(|| "我的文件".to_owned()) }}
                    </h1>
                </div>
                <p class="folder-meta">
                    <span>{move || format!("{} 个项目", item_count.get().len())}</span><i></i>
                    <Show
                        when=move || trash_mode.get()
                        fallback=move || view! {
                            <span>{move || format!("共 {} 个文件", file_count.get().max(0))}</span><i></i>
                            <span>{move || format_size(non_negative(total_bytes.get()))}</span>
                        }
                    >
                        <span>{move || format_size(non_negative(total_bytes.get()))}</span><i></i>
                        <span>"已删除的文件将在 30 天后永久删除"</span>
                    </Show>
                </p>
            </div>
            <Show
                when=move || trash_mode.get()
                fallback=move || view! {
                    <div class="actions">
                        <div class="view-switch file-view-switch" role="group" aria-label="文件视图切换">
                            <button type="button" class:active=move || view_mode.get() == ViewMode::Grid aria-pressed=move || if view_mode.get() == ViewMode::Grid { "true" } else { "false" } title="方块视图" on:click=move |_| view_mode.set(ViewMode::Grid)>{icons::layout_grid()}<span>"方块"</span></button>
                            <button type="button" class:active=move || view_mode.get() == ViewMode::List aria-pressed=move || if view_mode.get() == ViewMode::List { "true" } else { "false" } title="列表视图" on:click=move |_| view_mode.set(ViewMode::List)>{icons::list()}<span>"列表"</span></button>
                        </div>
                        <div class="desktop-create-actions">
                            <button class="secondary" type="button" on:click=move |_| on_new_document.run(())>{icons::file_plus()}"新建文档"</button>
                            <button class="secondary" type="button" on:click=move |_| on_create_folder.run(())>{icons::folder_plus()}"新建文件夹"</button>
                        </div>
                        <details node_ref=create_menu class="create-menu">
                            <summary class="secondary">{icons::file_plus()}"新建"{icons::chevron_down()}</summary>
                            <div class="create-menu-popover">
                                <button type="button" on:click=move |_| create_document.run(())><span>{icons::file_plus()}</span><div><b>"新建文档"</b><small>"Markdown 或纯文本"</small></div></button>
                                <button type="button" on:click=move |_| create_folder.run(())><span>{icons::folder_plus()}</span><div><b>"新建文件夹"</b><small>"整理当前目录"</small></div></button>
                            </div>
                        </details>
                        <details node_ref=upload_menu class="upload-menu">
                            <summary class="primary upload-action">{icons::upload()}"上传"{icons::chevron_down()}</summary>
                            <div class="upload-menu-popover">
                                <button type="button" on:click=move |_| upload_files.run(())><span>{icons::upload()}</span><div><b>"上传文件"</b><small>"可一次选择多个文件"</small></div></button>
                                <button type="button" on:click=move |_| upload_folder.run(())><span>{icons::folder_up()}</span><div><b>"上传文件夹"</b><small>"保留完整目录结构"</small></div></button>
                            </div>
                        </details>
                    </div>
                }
            >
                <div class="actions"><button class="secondary" type="button" on:click=move |_| on_leave_trash.run(())>"返回我的文件"</button><button class="trash-empty-action" type="button" prop:disabled=move || item_count.get().is_empty() on:click=move |_| on_empty_trash.run(())>"清空回收站"</button></div>
            </Show>
        </div>
    }
}

fn non_negative(value: i64) -> u64 {
    u64::try_from(value).unwrap_or(0)
}

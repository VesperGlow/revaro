//! A live directory selector used by move and copy operations.
//!
//! The picker deliberately reads directory metadata through the same API as
//! the main browser. It never trusts a stale tree supplied by the caller, and
//! a request sequence prevents a slower folder response from replacing a
//! newer selection.

use std::collections::HashSet;

use leptos::prelude::*;
use revaro_core::ids::ROOT_ID;
use revaro_core::model::{File, FileKind};
use wasm_bindgen::JsCast;

use crate::api;
use crate::browser;

const POPOVER_MARGIN: f64 = 10.0;
const POPOVER_GAP: f64 = 6.0;
const POPOVER_HEIGHT: f64 = 340.0;

/// A directory picker embedded in a transfer dialog.
#[component]
pub fn DirectoryPicker(
    initial_id: String,
    excluded_ids: Vec<String>,
    disabled: RwSignal<bool>,
    on_change: Callback<String>,
    on_unauthorized: Callback<()>,
) -> impl IntoView {
    let root = NodeRef::<leptos::html::Div>::new();
    let trigger = NodeRef::<leptos::html::Button>::new();
    let panel = NodeRef::<leptos::html::Section>::new();
    let expanded = RwSignal::new(false);
    let panel_style = RwSignal::new(String::new());
    let current_id = RwSignal::new(initial_id.clone());
    let current = RwSignal::new(None::<File>);
    let breadcrumbs = RwSignal::new(Vec::<File>::new());
    let folders = RwSignal::new(Vec::<File>::new());
    let loading = RwSignal::new(false);
    let error = RwSignal::new(String::new());
    let request_sequence = RwSignal::new(0_u64);
    let excluded_ids: HashSet<String> = excluded_ids.into_iter().collect();

    let load_folder = {
        let current_id = current_id;
        let current = current;
        let breadcrumbs = breadcrumbs;
        let folders = folders;
        let loading = loading;
        let error = error;
        let request_sequence = request_sequence;
        let excluded_ids = excluded_ids.clone();
        let on_change = on_change.clone();
        let on_unauthorized = on_unauthorized.clone();
        Callback::new(move |id: String| {
            if excluded_ids.contains(&id) {
                return;
            }
            let notify = current_id.get_untracked() != id;
            let sequence = request_sequence.get_untracked().wrapping_add(1);
            request_sequence.set(sequence);
            loading.set(true);
            error.set(String::new());
            let excluded_ids = excluded_ids.clone();
            leptos::task::spawn_local(async move {
                let result = fetch_directory_data(&id, &excluded_ids).await;

                // The Vue picker returned to the virtual root when a stale
                // non-root target disappeared (for example after a move or
                // delete in another tab). Keep that recovery path instead of
                // leaving the transfer dialog permanently stuck on an error.
                let (loaded_id, result) = match result {
                    Err(request_error) if !request_error.is_unauthorized() && id != ROOT_ID => (
                        ROOT_ID.to_owned(),
                        fetch_directory_data(ROOT_ID, &excluded_ids).await,
                    ),
                    result => (id.clone(), result),
                };

                if request_sequence.get_untracked() != sequence {
                    return;
                }
                loading.set(false);
                match result {
                    Ok((file, path, children)) => {
                        current_id.set(loaded_id.clone());
                        current.set(Some(file));
                        breadcrumbs.set(path);
                        folders.set(children);
                        // The initial folder is only the starting point. Once
                        // the user chooses a folder, the transfer dialog owns
                        // the selected target through this callback.
                        if notify {
                            on_change.run(loaded_id);
                        }
                    }
                    Err(request_error) if request_error.is_unauthorized() => {
                        on_unauthorized.run(());
                    }
                    Err(request_error) => error.set(request_error.message),
                }
            });
        })
    };

    // Load the initial target before the popover is opened so its label and
    // child folders are ready when the dialog is first inspected.
    load_folder.run(initial_id);

    let update_position = {
        let trigger = trigger;
        let panel_style = panel_style;
        Callback::new(move |_: ()| {
            let Some(trigger) = trigger.get() else {
                return;
            };
            let Some(window) = web_sys::window() else {
                return;
            };
            let rect = trigger.get_bounding_client_rect();
            let viewport_width = window
                .inner_width()
                .ok()
                .and_then(|value| value.as_f64())
                .unwrap_or(1024.0);
            let viewport_height = window
                .inner_height()
                .ok()
                .and_then(|value| value.as_f64())
                .unwrap_or(768.0);
            let below = viewport_height - rect.bottom() - POPOVER_GAP - POPOVER_MARGIN;
            let above = rect.top() - POPOVER_GAP - POPOVER_MARGIN;
            let opens_up = below < POPOVER_HEIGHT.min(240.0) && above > below;
            let available = 150.0_f64.max(POPOVER_HEIGHT.min(if opens_up { above } else { below }));
            let width = rect.width().min(viewport_width - POPOVER_MARGIN * 2.0);
            let left =
                POPOVER_MARGIN.max((rect.left()).min(viewport_width - POPOVER_MARGIN - width));
            let top = if opens_up {
                "auto".to_owned()
            } else {
                format!("{}px", (rect.bottom() + POPOVER_GAP).round())
            };
            let bottom = if opens_up {
                format!("{}px", (viewport_height - rect.top() + POPOVER_GAP).round())
            } else {
                "auto".to_owned()
            };
            panel_style.set(format!(
                "position:fixed;left:{}px;width:{}px;max-height:{}px;top:{};bottom:{};",
                left.round(),
                width.round(),
                available.round(),
                top,
                bottom
            ));
        })
    };

    let mut outside_listener = {
        let root = root;
        let panel = panel;
        browser::on_pointerdown(move |event| {
            if !expanded.get_untracked() {
                return;
            }
            let inside = event
                .target()
                .and_then(|target| target.dyn_into::<web_sys::Node>().ok())
                .map(|target| {
                    root.get().is_some_and(|root| root.contains(Some(&target)))
                        || panel
                            .get()
                            .is_some_and(|panel| panel.contains(Some(&target)))
                })
                .unwrap_or(false);
            if !inside {
                expanded.set(false);
            }
        })
    };
    let mut escape_listener = browser::on_keydown(move |event| {
        if event.key() == "Escape" && expanded.get_untracked() {
            event.prevent_default();
            event.stop_propagation();
            expanded.set(false);
        }
    });
    let mut resize_listener = {
        let update_position = update_position;
        browser::on_resize(move |_| {
            if expanded.get_untracked() {
                update_position.run(());
            }
        })
    };
    let mut scroll_listener = {
        let update_position = update_position;
        browser::on_scroll(move |_| {
            if expanded.get_untracked() {
                update_position.run(());
            }
        })
    };
    Effect::new(move |_| {
        if disabled.get() {
            expanded.set(false);
        }
    });
    on_cleanup(move || {
        outside_listener.release();
        escape_listener.release();
        resize_listener.release();
        scroll_listener.release();
    });

    let update_on_toggle = update_position;
    let toggle = move |_| {
        if !disabled.get_untracked() {
            expanded.update(|open| *open = !*open);
            if expanded.get_untracked() {
                update_on_toggle.run(());
            }
        }
    };
    let path_label = move || directory_path(&current_id.get(), &current.get(), &breadcrumbs.get());
    let retry = {
        let load_folder = load_folder.clone();
        move |_| load_folder.run(current_id.get_untracked())
    };

    view! {
        <div node_ref=root class="directory-picker" class:expanded=move || expanded.get()>
            <button
                node_ref=trigger
                type="button"
                class="directory-trigger"
                aria-expanded=move || if expanded.get() { "true" } else { "false" }
                title=path_label
                prop:disabled=move || disabled.get()
                on:click=toggle
            >
                {folder_open_icon()}
                <span>{path_label}</span>
                {chevron_down_icon()}
            </button>
            <Show when=move || expanded.get() fallback=|| ()>
                <leptos::portal::Portal>
                    <section node_ref=panel class="directory-popover" style=move || panel_style.get() aria-label="选择目标目录">
                    <nav class="directory-breadcrumbs" aria-label="目录路径">
                        <button
                            type="button"
                            class:active=move || current_id.get() == ROOT_ID
                            title="我的文件"
                            on:click={let load_folder = load_folder.clone(); move |_| load_folder.run(ROOT_ID.to_owned())}
                        >
                            {home_icon()}<span>"我的文件"</span>
                        </button>
                        {move || {
                            directory_breadcrumbs(&current_id.get(), &current.get(), &breadcrumbs.get())
                                .into_iter()
                                .map(|crumb| {
                                    let id = crumb.id.clone();
                                    let active_id = id.clone();
                                    let label = crumb.name.clone();
                                    let load_folder = load_folder.clone();
                                    view! {
                                        <span class="directory-breadcrumb-entry">
                                            {chevron_right_icon()}
                                            <button
                                                type="button"
                                                class:active=move || current_id.get() == active_id
                                                title=label.clone()
                                                on:click=move |_| load_folder.run(id.clone())
                                            >{label.clone()}</button>
                                        </span>
                                    }
                                })
                                .collect_view()
                        }}
                    </nav>
                    <div class="directory-list" aria-live="polite">
                        {move || {
                            if loading.get() {
                                view! {
                                    <div class="directory-state"><span class="spinner"></span><p>"正在读取文件夹…"</p></div>
                                }
                                .into_any()
                            } else if !error.get().is_empty() {
                                view! {
                                    <div class="directory-state error" role="alert">
                                        <p>{move || error.get()}</p>
                                        <button type="button" on:click=retry>"重新加载"</button>
                                    </div>
                                }
                                .into_any()
                            } else if folders.get().is_empty() {
                                view! {
                                    <div class="directory-state">{folder_open_icon()}<p>"此文件夹中没有子文件夹"</p></div>
                                }
                                .into_any()
                            } else {
                                view! {
                                    <For each=move || folders.get() key=|folder| folder.id.clone() let:folder>
                                        <button
                                            type="button"
                                            title=folder.name.clone()
                                            on:click={let load_folder = load_folder.clone(); move |_| load_folder.run(folder.id.clone())}
                                        >
                                            {folder_icon()}<span>{folder.name.clone()}</span>{chevron_right_icon()}
                                        </button>
                                    </For>
                                }
                                .into_any()
                            }
                        }}
                    </div>
                    </section>
                </leptos::portal::Portal>
            </Show>
        </div>
    }
}

async fn fetch_directory_data(
    id: &str,
    excluded_ids: &HashSet<String>,
) -> Result<(File, Vec<File>, Vec<File>), api::RequestError> {
    let detail = api::fetch_file(id).await?;
    let children = api::fetch_children(id).await?;
    let folders = children
        .items
        .into_iter()
        .filter(|item| item.kind == FileKind::Directory && !excluded_ids.contains(&item.id))
        .collect::<Vec<_>>();
    Ok((detail.file, detail.breadcrumbs, folders))
}

fn directory_breadcrumbs(
    current_id: &str,
    current: &Option<File>,
    breadcrumbs: &[File],
) -> Vec<File> {
    let mut result: Vec<File> = breadcrumbs
        .iter()
        .filter(|item| item.id != ROOT_ID)
        .cloned()
        .collect();
    if let Some(current) = current
        && current.id != ROOT_ID
        && current.id == current_id
        && !result.iter().any(|item| item.id == current.id)
    {
        result.push(current.clone());
    }
    result
}

fn directory_path(current_id: &str, current: &Option<File>, breadcrumbs: &[File]) -> String {
    let mut names = vec!["我的文件".to_owned()];
    names.extend(
        directory_breadcrumbs(current_id, current, breadcrumbs)
            .into_iter()
            .map(|item| item.name),
    );
    names.join(" / ")
}

fn folder_open_icon() -> impl IntoView {
    view! {
        <svg xmlns="http://www.w3.org/2000/svg" width="24" height="24" viewBox="0 0 24 24"
            fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round"
            stroke-linejoin="round" aria-hidden="true">
            <path d="m6 14 1.5-2.9A2 2 0 0 1 9.24 10H20a2 2 0 0 1 1.94 2.5l-1.54 6a2 2 0 0 1-1.95 1.5H4a2 2 0 0 1-2-2V5a2 2 0 0 1 2-2h3.9a2 2 0 0 1 1.69.9l.81 1.2a2 2 0 0 0 1.67.9H18a2 2 0 0 1 2 2v2"></path>
        </svg>
    }
}

fn folder_icon() -> impl IntoView {
    view! {
        <svg xmlns="http://www.w3.org/2000/svg" width="24" height="24" viewBox="0 0 24 24"
            fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round"
            stroke-linejoin="round" aria-hidden="true">
            <path d="M20 20a2 2 0 0 0 2-2V8a2 2 0 0 0-2-2h-7.9a2 2 0 0 1-1.69-.9L9.6 3.9A2 2 0 0 0 7.93 3H4a2 2 0 0 0-2 2v13a2 2 0 0 0 2 2Z"></path>
        </svg>
    }
}

fn home_icon() -> impl IntoView {
    view! {
        <svg xmlns="http://www.w3.org/2000/svg" width="24" height="24" viewBox="0 0 24 24"
            fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round"
            stroke-linejoin="round" aria-hidden="true">
            <path d="M15 21v-8a1 1 0 0 0-1-1h-4a1 1 0 0 0-1 1v8"></path>
            <path d="M3 10a2 2 0 0 1 .709-1.528l7-6a2 2 0 0 1 2.582 0l7 6A2 2 0 0 1 21 10v9a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2z"></path>
        </svg>
    }
}

fn chevron_down_icon() -> impl IntoView {
    view! {
        <svg
            class="directory-chevron"
            viewBox="0 0 24 24"
            width="24"
            height="24"
            fill="none"
            stroke="currentColor"
            stroke-width="2"
            stroke-linecap="round"
            stroke-linejoin="round"
            aria-hidden="true"
        >
            <path d="m6 9 6 6 6-6"></path>
        </svg>
    }
}

fn chevron_right_icon() -> impl IntoView {
    view! {
        <svg
            class="directory-chevron-right"
            viewBox="0 0 24 24"
            width="24"
            height="24"
            fill="none"
            stroke="currentColor"
            stroke-width="2"
            stroke-linecap="round"
            stroke-linejoin="round"
            aria-hidden="true"
        >
            <path d="m9 18 6-6-6-6"></path>
        </svg>
    }
}

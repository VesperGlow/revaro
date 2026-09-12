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
    let expanded = RwSignal::new(false);
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
            let sequence = request_sequence.get_untracked().wrapping_add(1);
            request_sequence.set(sequence);
            loading.set(true);
            error.set(String::new());
            let excluded_ids = excluded_ids.clone();
            leptos::task::spawn_local(async move {
                let result = async {
                    let detail = api::fetch_file(&id).await?;
                    let children = api::fetch_children(&id).await?;
                    let folders = children
                        .items
                        .into_iter()
                        .filter(|item| {
                            item.kind == FileKind::Directory && !excluded_ids.contains(&item.id)
                        })
                        .collect::<Vec<_>>();
                    Ok::<(File, Vec<File>, Vec<File>), api::RequestError>((
                        detail.file,
                        detail.breadcrumbs,
                        folders,
                    ))
                }
                .await;

                if request_sequence.get_untracked() != sequence {
                    return;
                }
                loading.set(false);
                match result {
                    Ok((file, path, children)) => {
                        let notify = current_id.get_untracked() != id;
                        current_id.set(id.clone());
                        current.set(Some(file));
                        breadcrumbs.set(path);
                        folders.set(children);
                        // The initial folder is only the starting point. Once
                        // the user chooses a folder, the transfer dialog owns
                        // the selected target through this callback.
                        if notify {
                            on_change.run(id);
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

    let mut outside_listener = {
        let root = root;
        browser::on_pointerdown(move |event| {
            if !expanded.get_untracked() {
                return;
            }
            let inside = event
                .target()
                .and_then(|target| target.dyn_into::<web_sys::Node>().ok())
                .and_then(|target| root.get().map(|root| root.contains(Some(&target))))
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
    on_cleanup(move || {
        outside_listener.release();
        escape_listener.release();
    });

    let toggle = move |_| {
        if !disabled.get_untracked() {
            expanded.update(|open| *open = !*open);
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
                type="button"
                class="directory-trigger"
                aria-expanded=move || if expanded.get() { "true" } else { "false" }
                aria-label="选择目标目录"
                title=path_label
                prop:disabled=move || disabled.get()
                on:click=toggle
            >
                {folder_open_icon()}
                <span>{path_label}</span>
                {chevron_down_icon()}
            </button>
            <Show when=move || expanded.get() fallback=|| ()>
                <section class="directory-popover" aria-label="选择目标目录">
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
            </Show>
        </div>
    }
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
    view! { <svg viewBox="0 0 24 24" aria-hidden="true"><path d="M3 6.5A1.5 1.5 0 0 1 4.5 5h5l2 2h8A1.5 1.5 0 0 1 21 8.5v9A1.5 1.5 0 0 1 19.5 19h-15A1.5 1.5 0 0 1 3 17.5Z"></path><path d="M3.5 9h17"></path></svg> }
}

fn folder_icon() -> impl IntoView {
    view! { <svg viewBox="0 0 24 24" aria-hidden="true"><path d="M3 6.5A1.5 1.5 0 0 1 4.5 5h5l2 2h8A1.5 1.5 0 0 1 21 8.5v9A1.5 1.5 0 0 1 19.5 19h-15A1.5 1.5 0 0 1 3 17.5Z"></path></svg> }
}

fn home_icon() -> impl IntoView {
    view! { <svg viewBox="0 0 24 24" aria-hidden="true"><path d="m3 11 9-7 9 7v9H3Z"></path><path d="M9 20v-6h6v6"></path></svg> }
}

fn chevron_down_icon() -> impl IntoView {
    view! { <svg class="directory-chevron" viewBox="0 0 24 24" aria-hidden="true"><path d="m6 9 6 6 6-6"></path></svg> }
}

fn chevron_right_icon() -> impl IntoView {
    view! { <svg class="directory-chevron-right" viewBox="0 0 24 24" aria-hidden="true"><path d="m9 6 6 6-6 6"></path></svg> }
}

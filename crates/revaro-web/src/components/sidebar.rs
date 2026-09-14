//! The category/sidebar navigation.
//!
//! The sidebar is more than a link list: the reference implementation keeps a
//! five-bucket media index, a persisted accordion, a live ordinary-directory
//! tree, a desktop icon rail and a separate mobile drawer. This component keeps
//! those behaviours in one place while the browser owns the data snapshots.

use std::collections::HashMap;

use leptos::prelude::*;
use revaro_core::classify::LibraryKind;
use revaro_core::model::{File, FileKind, LibraryCounts};

use crate::api;
use crate::browser;
use revaro_core::ids::ROOT_ID;

use super::icons;
use super::library::LibraryFolderNode;

const EXPANDED_KEY: &str = "revaro:sidebar:expanded";

/// All data needed to render the sidebar's media path trees.
pub type LibraryTrees = HashMap<LibraryKind, LibraryFolderNode>;

/// The reference category sidebar.
#[component]
pub fn AppSidebar(
    section: RwSignal<LibraryKind>,
    collapsed: RwSignal<bool>,
    mobile_open: RwSignal<bool>,
    counts: RwSignal<LibraryCounts>,
    trees: RwSignal<LibraryTrees>,
    active_folder_id: RwSignal<Option<String>>,
    current_id: RwSignal<String>,
    reload_token: RwSignal<u64>,
    on_select_category: Callback<LibraryKind>,
    on_select_folder: Callback<Option<String>>,
    on_navigate_directory: Callback<String>,
    on_toggle_collapse: Callback<()>,
    on_toggle_mobile: Callback<()>,
    on_open_trash: Callback<()>,
) -> impl IntoView {
    let mobile = browser::media_query_signal("(max-width: 850px)");
    let rail = Signal::derive(move || collapsed.get() && !mobile.get());
    let expanded = RwSignal::new(read_expanded_category());
    let path_expansions = RwSignal::new(HashMap::<String, bool>::new());
    let previous_mobile = RwSignal::new(mobile.get_untracked());

    let responsive_expansion_effect = path_expansions;
    Effect::new(move |_| {
        let current_mobile = mobile.get();
        if current_mobile != previous_mobile.get_untracked() {
            clear_all_path_expansions(responsive_expansion_effect);
            previous_mobile.set(current_mobile);
        }
    });

    let expanded_effect = expanded;
    Effect::new(move |_| match expanded_effect.get() {
        Some(kind) => browser::local_storage_set(EXPANDED_KEY, kind.as_str()),
        None => browser::local_storage_remove(EXPANDED_KEY),
    });

    let mut escape_listener = browser::on_keydown(move |event| {
        if event.key() == "Escape" && mobile_open.get_untracked() {
            mobile_open.set(false);
        }
    });
    on_cleanup(move || escape_listener.release());

    let toggle_category = {
        let expanded = expanded;
        let path_expansions = path_expansions;
        Callback::new(move |kind: LibraryKind| {
            let previous = expanded.get_untracked();
            let next = if previous == Some(kind) {
                None
            } else {
                Some(kind)
            };
            if previous != next {
                if let Some(previous) = previous {
                    clear_path_expansions(path_expansions, previous);
                }
                clear_path_expansions(path_expansions, kind);
            }
            expanded.set(next);
        })
    };
    let close_mobile = Callback::new(move |(): ()| mobile_open.set(false));

    view! {
        <div
            class:open=move || mobile_open.get()
            class="sidebar-backdrop"
            aria-hidden="true"
            on:click=move |_| mobile_open.set(false)
        ></div>
        <Show when=move || mobile.get() fallback=|| ()>
            <button
                type="button"
                class:open=move || mobile_open.get()
                class="sidebar-handle"
                aria-expanded=move || if mobile_open.get() { "true" } else { "false" }
                aria-label=move || if mobile_open.get() { "收起分类栏" } else { "展开分类栏" }
                title=move || if mobile_open.get() { "收起分类栏" } else { "展开分类栏" }
                on:click=move |_| on_toggle_mobile.run(())
            >
                {move || if mobile_open.get() {
                    icons::chevron_left().into_any()
                } else {
                    icons::chevron_right().into_any()
                }}
            </button>
        </Show>

        <aside
            class:collapsed=move || rail.get()
            class:mobile-open=move || mobile_open.get()
            class="app-sidebar"
            aria-label="分类导航"
        >
            <div class="sidebar-head">
                <button
                    type="button"
                    class="sidebar-collapse"
                    title=move || if rail.get() { "展开分类栏" } else { "收起分类栏" }
                    aria-label=move || if rail.get() { "展开分类栏" } else { "收起分类栏" }
                    aria-expanded=move || if rail.get() { "false" } else { "true" }
                    on:click=move |_| {
                        if !mobile.get_untracked() {
                            clear_all_path_expansions(path_expansions);
                        }
                        on_toggle_collapse.run(());
                    }
                >
                    {move || if rail.get() {
                        icons::panel_left_open().into_any()
                    } else {
                        icons::panel_left_close().into_any()
                    }}
                    <Show when=move || !rail.get() fallback=|| ()>
                        <span>{"收起分类栏"}</span>
                    </Show>
                </button>
            </div>

            <nav class="sidebar-nav" aria-label="主导航">
                <For
                    each=move || LibraryKind::ALL.to_vec()
                    key=|kind| kind.as_str()
                    let:kind
                >
                    {render_category(
                        kind,
                        section,
                        rail,
                        mobile,
                        expanded,
                        counts,
                        trees,
                        active_folder_id,
                        current_id,
                        reload_token,
                        on_select_category,
                        on_select_folder,
                        on_navigate_directory,
                        toggle_category,
                        path_expansions,
                        close_mobile,
                    )}
                </For>
            </nav>

            <div class="sidebar-foot">
                <button
                    type="button"
                    class="category-main trash-entry"
                    title="回收站"
                    aria-label="回收站"
                    on:click=move |_| {
                        on_open_trash.run(());
                    }
                >
                    <span class="category-icon">{icons::trash()}</span>
                    <Show when=move || !rail.get() fallback=|| ()>
                        <span class="category-label">"回收站"</span>
                    </Show>
                </button>
            </div>
        </aside>
    }
}

fn render_category(
    kind: LibraryKind,
    section: RwSignal<LibraryKind>,
    rail: Signal<bool>,
    mobile: RwSignal<bool>,
    expanded: RwSignal<Option<LibraryKind>>,
    counts: RwSignal<LibraryCounts>,
    trees: RwSignal<LibraryTrees>,
    active_folder_id: RwSignal<Option<String>>,
    current_id: RwSignal<String>,
    reload_token: RwSignal<u64>,
    on_select_category: Callback<LibraryKind>,
    on_select_folder: Callback<Option<String>>,
    on_navigate_directory: Callback<String>,
    toggle_category: Callback<LibraryKind>,
    path_expansions: RwSignal<HashMap<String, bool>>,
    close_mobile: Callback<()>,
) -> AnyView {
    let label = category_label(kind);
    let hint = category_hint(kind);
    let icon = category_icon(kind);
    let select = on_select_category.clone();
    let close = close_mobile.clone();
    let select_folder = on_select_folder.clone();
    let navigate = on_navigate_directory.clone();
    let toggle = toggle_category.clone();
    let expand = expanded;
    view! {
        <section
            class:active=move || section.get() == kind && !rail.get()
            class="sidebar-category"
        >
            <div class="category-row">
                <button
                    type="button"
                    class:active=move || section.get() == kind
                    class="category-main"
                    data-category=kind.as_str()
                    title=move || if rail.get() { label.to_owned() } else { hint.to_owned() }
                    aria-current=move || if section.get() == kind { Some("page") } else { None::<&str> }
                    on:click=move |_| {
                        // The old sidebar expands the selected category's path
                        // tree as part of the row click. The separate chevron
                        // remains the only control that collapses it again.
                        if expand.get_untracked() != Some(kind) {
                            if let Some(previous) = expand.get_untracked() {
                                clear_path_expansions(path_expansions, previous);
                            }
                            clear_path_expansions(path_expansions, kind);
                        }
                        expand.set(Some(kind));
                        select.run(kind);
                        close.run(());
                    }
                >
                    <span class="category-icon">{icon}</span>
                    <Show when=move || !rail.get() fallback=|| ()>
                        <span class="category-label">{label}</span>
                    </Show>
                    <Show when=move || { !rail.get() && !mobile.get() && count_for(counts.get(), kind) > 0 } fallback=|| ()>
                        <span class="category-count">{move || count_for(counts.get(), kind)}</span>
                    </Show>
                </button>
                <Show when=move || !rail.get() && !mobile.get() fallback=|| ()>
                    <button
                        type="button"
                        class="category-expand"
                        aria-expanded=move || if expanded.get() == Some(kind) { "true" } else { "false" }
                        aria-label=move || format!("{}{}路径", if expanded.get() == Some(kind) { "收起" } else { "展开" }, label)
                        title=move || if expanded.get() == Some(kind) { "收起路径" } else { "展开路径" }
                        on:click=move |_| toggle.run(kind)
                    >
                        <span class:open=move || expanded.get() == Some(kind)>{icons::chevron_right()}</span>
                    </button>
                </Show>
            </div>
            {move || {
                if rail.get() || mobile.get() || expanded.get() != Some(kind) {
                    return ().into_any();
                }
                if kind == LibraryKind::File {
                    return view! {
                        <div class="category-paths">
                            <FileTree
                                current_id=current_id
                                reload_token=reload_token
                                on_navigate=navigate.clone()
                            />
                        </div>
                    }
                    .into_any();
                }
                if let Some(node) = trees.get().get(&kind).cloned() {
                    view! {
                        <div class="category-paths">
                            <PathTree
                                node=node
                                kind=kind
                                depth=0
                                active_folder_id=active_folder_id
                                expansions=path_expansions
                                on_select=select_folder.clone()
                            />
                        </div>
                    }
                    .into_any()
                } else {
                    view! {
                        <div class="category-paths">
                            <p class="path-loading">{format!("还没有{}内容", label)}</p>
                        </div>
                    }
                    .into_any()
                }
            }}
        </section>
    }
    .into_any()
}

#[component]
fn PathTree(
    node: LibraryFolderNode,
    kind: LibraryKind,
    depth: usize,
    active_folder_id: RwSignal<Option<String>>,
    expansions: RwSignal<HashMap<String, bool>>,
    on_select: Callback<Option<String>>,
) -> AnyView {
    let expansion_key = format!("{}:{}", kind.as_str(), node.id);
    let expanded = RwSignal::new(
        expansions
            .get_untracked()
            .get(&expansion_key)
            .copied()
            .unwrap_or(depth < 1),
    );
    let has_children = !node.children.is_empty();
    let node_id = node.id.clone();
    let node_name = node.name.clone();
    let node_path = node.path.clone();
    let node_title = if node_path.is_empty() {
        node_name.clone()
    } else {
        node_path
    };
    let select_id = node.id.clone();
    let node_children = RwSignal::new(node.children);
    let node_count = node.count;
    let key_for_toggle = expansion_key.clone();
    let toggle = Callback::new(move |(): ()| {
        expanded.update(|value| *value = !*value);
        let value = expanded.get_untracked();
        expansions.update(|states| {
            states.insert(key_for_toggle.clone(), value);
        });
    });
    let select = move |_| {
        if select_id == ROOT_ID {
            active_folder_id.set(None);
            on_select.run(None);
        } else {
            active_folder_id.set(Some(select_id.clone()));
            on_select.run(Some(select_id.clone()));
        }
    };

    view! {
        <div class="path-node">
            <div
                class:active=move || {
                    active_folder_id.get().as_deref() == Some(node_id.as_str())
                        || (active_folder_id.get().is_none() && node_id == ROOT_ID)
                }
                class="path-row"
                style=format!("--depth:{depth}")
            >
                <Show
                    when=move || has_children
                    fallback=|| view! { <span class="path-toggle spacer" aria-hidden="true"></span> }
                >
                    <button
                        type="button"
                        class="path-toggle"
                        aria-expanded=move || if expanded.get() { "true" } else { "false" }
                        aria-label=move || if expanded.get() { "收起子路径" } else { "展开子路径" }
                        on:click=move |_| toggle.run(())
                    >
                        <span class:open=move || expanded.get()>{icons::chevron_right()}</span>
                    </button>
                </Show>
                <button
                    type="button"
                    class="path-label"
                    title=node_title
                    on:click=select
                >
                    {icons::folder()}
                    <span>{node_name}</span>
                    <em>{node_count}</em>
                </button>
            </div>
            <Show when=move || expanded.get() && has_children fallback=|| ()>
                <div class="path-children">
                    <For
                        each=move || node_children.get()
                        key=|child| child.id.clone()
                        let:child
                    >
                        <PathTree
                            node=child
                            kind=kind
                            depth=depth + 1
                            active_folder_id=active_folder_id
                            expansions=expansions
                            on_select=on_select.clone()
                        />
                    </For>
                </div>
            </Show>
        </div>
    }
    .into_any()
}

fn clear_path_expansions(expansions: RwSignal<HashMap<String, bool>>, kind: LibraryKind) {
    let prefix = format!("{}:", kind.as_str());
    expansions.update(|states| {
        states.retain(|key, _| !key.starts_with(&prefix));
    });
}

fn clear_all_path_expansions(expansions: RwSignal<HashMap<String, bool>>) {
    expansions.set(HashMap::new());
}

#[component]
fn FileTree(
    current_id: RwSignal<String>,
    reload_token: RwSignal<u64>,
    on_navigate: Callback<String>,
) -> impl IntoView {
    let expanded = RwSignal::new(true);
    let children = RwSignal::new(None::<Vec<File>>);
    let loading = RwSignal::new(false);
    let load = {
        let children = children;
        let loading = loading;
        Callback::new(move |(): ()| {
            loading.set(true);
            leptos::task::spawn_local(async move {
                match api::fetch_children(ROOT_ID).await {
                    Ok(data) => children.set(Some(
                        data.items
                            .into_iter()
                            .filter(|item| item.kind == FileKind::Directory)
                            .collect(),
                    )),
                    Err(_) => children.set(Some(Vec::new())),
                }
                loading.set(false);
            });
        })
    };
    let initial_load = load.clone();
    Effect::new(move |_| {
        let _ = reload_token.get();
        children.set(None);
        if expanded.get_untracked() {
            initial_load.run(());
        }
    });

    view! {
        <div class="path-node file-tree-root">
            <div class:active=move || current_id.get() == ROOT_ID class="path-row">
                <button
                    type="button"
                    class="path-toggle"
                    aria-expanded=move || if expanded.get() { "true" } else { "false" }
                    aria-label=move || if expanded.get() { "收起子目录" } else { "展开子目录" }
                    on:click=move |_| {
                        expanded.update(|value| *value = !*value);
                        if expanded.get_untracked() && children.get_untracked().is_none() {
                            load.run(());
                        }
                    }
                >
                    <span class:open=move || expanded.get()>{icons::chevron_right()}</span>
                </button>
                <button
                    type="button"
                    class="path-label"
                    title="我的文件"
                    on:click=move |_| on_navigate.run(ROOT_ID.to_owned())
                >
                    {icons::folder()}
                    <span>"我的文件"</span>
                </button>
            </div>
            <Show when=move || expanded.get() fallback=|| ()>
                <div class="path-children">
                    <Show when=move || loading.get() fallback=move || view! {
                        <For
                            each=move || children.get().unwrap_or_default()
                            key=|item| item.id.clone()
                            let:item
                        >
                            <DirectoryNode
                                id=item.id
                                name=item.name
                                depth=1
                                current_id=current_id
                                reload_token=reload_token
                                on_navigate=on_navigate.clone()
                            />
                        </For>
                        <Show when=move || children.get().is_some_and(|items| items.is_empty()) fallback=|| ()>
                            <span class="path-loading">"还没有子文件夹"</span>
                        </Show>
                    }>
                        <span class="path-loading">"读取中…"</span>
                    </Show>
                </div>
            </Show>
        </div>
    }
}

#[component]
fn DirectoryNode(
    id: String,
    name: String,
    depth: usize,
    current_id: RwSignal<String>,
    reload_token: RwSignal<u64>,
    on_navigate: Callback<String>,
) -> impl IntoView {
    let expanded = RwSignal::new(false);
    let children = RwSignal::new(None::<Vec<File>>);
    let loading = RwSignal::new(false);
    let load = {
        let id = id.clone();
        let children = children;
        let loading = loading;
        Callback::new(move |(): ()| {
            loading.set(true);
            let id = id.clone();
            leptos::task::spawn_local(async move {
                match api::fetch_children(&id).await {
                    Ok(data) => children.set(Some(
                        data.items
                            .into_iter()
                            .filter(|item| item.kind == FileKind::Directory)
                            .collect(),
                    )),
                    Err(_) => children.set(Some(Vec::new())),
                }
                loading.set(false);
            });
        })
    };
    let reload = load.clone();
    Effect::new(move |_| {
        let _ = reload_token.get();
        children.set(None);
        if expanded.get_untracked() {
            reload.run(());
        }
    });
    let id_for_active = id.clone();
    let id_for_click = id.clone();
    let name_for_title = name.clone();
    let children_for_view = children;

    view! {
        <div class="path-node">
            <div class:active=move || current_id.get() == id_for_active class="path-row" style=format!("--depth:{depth}")>
                <button
                    type="button"
                    class="path-toggle"
                    aria-expanded=move || if expanded.get() { "true" } else { "false" }
                    aria-label=move || if expanded.get() { "收起子目录" } else { "展开子目录" }
                    on:click=move |_| {
                        expanded.update(|value| *value = !*value);
                        if expanded.get_untracked() && children_for_view.get_untracked().is_none() {
                            load.run(());
                        }
                    }
                >
                    <span class:open=move || expanded.get()>{icons::chevron_right()}</span>
                </button>
                <button
                    type="button"
                    class="path-label"
                    title=name_for_title.clone()
                    on:click=move |_| on_navigate.run(id_for_click.clone())
                >
                    {icons::folder()}
                    <span>{name.clone()}</span>
                </button>
            </div>
            <Show when=move || expanded.get() fallback=|| ()>
                <div class="path-children">
                    <Show when=move || loading.get() fallback=move || view! {
                        <For
                            each=move || children_for_view.get().unwrap_or_default()
                            key=|item| item.id.clone()
                            let:item
                        >
                            <DirectoryNode
                                id=item.id
                                name=item.name
                                depth=depth + 1
                                current_id=current_id
                                reload_token=reload_token
                                on_navigate=on_navigate.clone()
                            />
                        </For>
                    }>
                        <span class="path-loading">"读取中…"</span>
                    </Show>
                </div>
            </Show>
        </div>
    }
}

fn category_label(kind: LibraryKind) -> &'static str {
    match kind {
        LibraryKind::Book => "书架",
        LibraryKind::Image => "图片",
        LibraryKind::Video => "视频",
        LibraryKind::Audio => "音乐",
        LibraryKind::File => "文件",
    }
}

fn category_hint(kind: LibraryKind) -> &'static str {
    match kind {
        LibraryKind::Book => "以封面浏览 EPUB / TXT",
        LibraryKind::Image => "全部图片与图库分类",
        LibraryKind::Video => "全部视频与图库分类",
        LibraryKind::Audio => "方块或列表浏览音频",
        LibraryKind::File => "按目录管理全部文件",
    }
}

fn category_icon(kind: LibraryKind) -> AnyView {
    match kind {
        LibraryKind::Book => icons::book_open().into_any(),
        LibraryKind::Image => icons::image().into_any(),
        LibraryKind::Video => icons::film().into_any(),
        LibraryKind::Audio => icons::music().into_any(),
        LibraryKind::File => icons::folder_closed().into_any(),
    }
}

fn count_for(counts: LibraryCounts, kind: LibraryKind) -> i64 {
    match kind {
        LibraryKind::Book => counts.book,
        LibraryKind::Image => counts.image,
        LibraryKind::Video => counts.video,
        LibraryKind::Audio => counts.audio,
        LibraryKind::File => counts.file,
    }
}

fn read_expanded_category() -> Option<LibraryKind> {
    let raw = browser::local_storage_get(EXPANDED_KEY)?;
    raw.parse().ok()
}

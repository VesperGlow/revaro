//! Selection actions for the live folder and trash listings.

use leptos::prelude::*;
use revaro_core::classify;
use revaro_core::model::{File, FileKind};

use super::{
    icons,
    menu::{ActionMenu, MenuIcon},
    selection::SelectionMode,
};
use crate::logic::{feedback::Feedback, format::format_size, library::LibraryPage};

/// Callbacks supplied once by the persistent file workspace for every listing.
#[derive(Clone)]
pub struct SelectionActions {
    pub items: Signal<Vec<File>>,
    pub trash_mode: Signal<bool>,
    pub file_tools: Signal<bool>,
    pub visible: Signal<bool>,
    pub on_clear: Callback<()>,
    pub on_select_all: Callback<()>,
    pub on_rename: Callback<()>,
    pub on_move: Callback<()>,
    pub on_copy: Callback<()>,
    pub on_delete: Callback<()>,
    pub on_restore: Callback<()>,
    pub on_purge: Callback<()>,
    pub on_open: Callback<File>,
    pub on_read: Callback<File>,
    pub on_download: Callback<()>,
    pub on_share: Callback<File>,
    pub on_feedback: Callback<Feedback>,
}

/// The same batch toolbar serves the folder, trash and content-library views.
#[component]
pub fn BatchActionBar(selection: SelectionMode, actions: SelectionActions) -> impl IntoView {
    let mobile_actions = actions.clone();
    let selected_ids = selection.ids;
    let SelectionActions {
        items,
        trash_mode,
        file_tools,
        visible,
        on_clear,
        on_select_all,
        on_rename,
        on_move,
        on_copy,
        on_delete,
        on_restore,
        on_purge,
        on_open,
        on_read: _,
        on_download,
        on_share,
        on_feedback: _,
    } = actions;
    let selected_items = move || {
        let ids = selected_ids.get();
        items
            .get()
            .into_iter()
            .filter(|item| ids.contains(&item.id))
            .collect::<Vec<_>>()
    };
    let selected_count = move || {
        let ids = selected_ids.get();
        items
            .get()
            .iter()
            .filter(|item| ids.contains(&item.id))
            .count()
    };
    let selected_bytes = move || {
        selected_items()
            .iter()
            .filter(|item| item.kind == FileKind::File)
            .map(|item| u64::try_from(item.size).unwrap_or(0))
            .sum::<u64>()
    };
    let single_item = move || {
        let mut selected = selected_items().into_iter();
        let item = selected.next();
        item.filter(|_| selected.next().is_none())
    };
    let selected_file_count = move || selected_items().len();
    let all_selected = move || {
        let entries = items.get();
        let ids = selected_ids.get();
        entries.iter().filter(|item| ids.contains(&item.id)).count() == entries.len()
    };
    let rename = on_rename;
    let move_items = on_move;
    let delete = on_delete;
    let restore = on_restore;
    let purge = on_purge;
    let open = on_open;
    let download = on_download;
    let share = on_share;

    view! {
        <Show when=move || selection.enabled.get() && !selected_ids.get().is_empty() && visible.get() fallback=|| ()>
        <div class="selection-toolbar" role="toolbar" aria-label="所选项目操作">
            <button
                class="selection-close"
                type="button"
                title="退出选择模式"
                aria-label="取消"
                on:click=move |_| on_clear.run(())
            >
                "取消"
            </button>
            <span class="selection-summary">
                <b>{move || format!("已选择 {} 项", selected_count())}</b>
                <small>{move || format!("已选择 {}", format_size(selected_bytes()))}</small>
            </span>
            <div class="selection-action-groups">
            <div class="selection-actions">
                <button type="button" on:click=move |_| on_select_all.run(())>
                    {check_icon()}
                    <span>{move || if all_selected() { "取消全选" } else { "全选" }}</span>
                </button>
                <Show
                    when=move || !trash_mode.get()
                    fallback=move || view! {
                        <button type="button" on:click=move |_| restore.run(())>
                            {restore_icon()}
                            <span>"恢复"</span>
                        </button>
                        <button class="danger" type="button" on:click=move |_| purge.run(())>
                            {trash_icon()}
                            <span>"永久删除"</span>
                        </button>
                    }
                >
                    <Show
                        when=move || {
                            file_tools.get() && single_item().is_some_and(|item| {
                                item.kind == FileKind::Directory
                                    || classify::is_editable(&item)
                                    || classify::is_book(&item)
                                    || classify::is_image(&item)
                                    || classify::is_audio(&item)
                                    || classify::is_video(&item)
                            })
                        }
                        fallback=|| ()
                    >
                        {move || {
                            single_item().map_or_else(
                                || ().into_any(),
                                |item| {
                                    let label = if item.kind == FileKind::Directory {
                                        "打开"
                                    } else if classify::is_book(&item) {
                                        "阅读"
                                    } else if classify::is_editable(&item) {
                                        "编辑文本"
                                    } else if classify::is_image(&item) {
                                        "预览"
                                    } else {
                                        "播放"
                                    };
                                    view! {
                                        <button type="button" on:click=move |_| open.run(item.clone())>
                                            {open_icon(&item)}
                                            <span>{label}</span>
                                        </button>
                                    }
                                    .into_any()
                                },
                            )
                        }}
                    </Show>
                    <Show when=move ||file_tools.get() fallback=|| ()>
                        <button type="button" on:click=move |_|on_copy.run(())>
                            <span class="selection-mobile-icon" aria-hidden="true">{icons::copy()}</span>
                            <span>"复制到"</span>
                        </button>
                    </Show>
                    <Show when=move || { selected_file_count() > 0 } fallback=|| ()>
                        <button type="button" on:click=move |_| download.run(())>
                            {download_icon()}
                            <span>{move || format!("下载{}", if selected_file_count() > 1 { format!(" ({})", selected_file_count()) } else { String::new() })}</span>
                        </button>
                    </Show>
                    <Show when=move || file_tools.get() && single_item().is_some_and(|item| item.kind == FileKind::File) fallback=|| ()>
                        {move || {
                            single_item().map_or_else(
                                || ().into_any(),
                                |item| view! {
                                    <button type="button" on:click=move |_| share.run(item.clone())>
                                        {share_icon()}
                                        <span>"分享"</span>
                                    </button>
                                }.into_any(),
                            )
                        }}
                    </Show>
                    <Show when=move || file_tools.get() && selected_count() == 1 fallback=|| ()>
                        <button type="button" on:click=move |_| rename.run(())>
                            {rename_icon()}
                            <span>"重命名"</span>
                        </button>
                    </Show>
                    <button type="button" on:click=move |_| move_items.run(())>
                        {move_arrow_icon()}
                        <span>"移动"</span>
                    </button>
                    <button class="danger" type="button" on:click=move |_| delete.run(())>
                        {trash_icon()}
                        <span>"删除"</span>
                    </button>
                </Show>
            </div>
            <Show when=move || !trash_mode.get() && selection.management.get().is_some() && selected_items().iter().any(|file| file.kind == FileKind::File) fallback=|| ()>
                {move || selection.management.get().map(|management| {
                    let context = Signal::derive(move || {
                        let mut ids = selection.ids.get().into_iter().collect::<Vec<_>>();
                        ids.sort();
                        ids.join(",")
                    });
                    view! {
                        <div class="selection-management">
                            <button type="button" disabled=move || management.busy.get() on:click=move |_|management.on_favorite.run(true)>{icons::heart()}<span>"收藏"</span></button>
                            {move || selection.stacks.get().map(|stacks| view! {
                                <Show when=move ||selected_items().iter().any(classify::is_book) fallback=|| ()>
                                    <button type="button" disabled=move ||stacks.busy.get() on:click=move |_|stacks.on_stack.run(())>{icons::plus()}<span>"堆叠"</span></button>
                                    <Show when=move ||stacks.can_remove.get() fallback=|| ()>
                                        <button type="button" disabled=move ||stacks.busy.get() on:click=move |_|stacks.on_remove.run(())>
                                            <span class="selection-mobile-icon" aria-hidden="true">{icons::arrow_left()}</span>
                                            <span>"移出堆叠"</span>
                                        </button>
                                    </Show>
                                </Show>
                            })}
                            <ActionMenu label="更多管理操作".to_owned() icon=MenuIcon::More text=Signal::derive(|| "更多".to_owned()) context=context disabled=management.busy panel_class="selection-management-panel".to_owned()>
                                <button type="button" data-close-menu="true" disabled=move ||management.busy.get() on:click=move |_|management.on_favorite.run(false)>{icons::heart()}"取消收藏"</button>
                                {[LibraryPage::Books, LibraryPage::Music, LibraryPage::Gallery, LibraryPage::Videos].into_iter().map(|page| view! {
                                    <Show when=move ||selected_items().iter().any(|file| matches_collection(file, page)) fallback=|| ()>
                                        <button type="button" data-close-menu="true" disabled=move ||management.busy.get() on:click=move |_|management.on_collection.run(page)>{icons::plus()}{format!("加入{}", page.collection_label())}</button>
                                    </Show>
                                }).collect_view()}
                                <Show when=move ||management.can_remove.get() fallback=|| ()>
                                    <button type="button" data-close-menu="true" disabled=move ||management.busy.get() on:click=move |_|management.on_remove.run(())>{icons::folder()}"移出当前集合"</button>
                                </Show>
                            </ActionMenu>
                        </div>
                    }
                })}
            </Show>
            </div>
            <MobileBatchActions selection=selection actions=mobile_actions.clone() />
        </div>
        </Show>
    }
}

#[derive(Clone, Copy, Default)]
enum FavoriteMenuState {
    #[default]
    Loading,
    Ready(crate::logic::selection::FavoriteActions),
    Failed,
}

/// A fixed action row shares the existing callbacks with the desktop toolbar.
#[component]
fn MobileBatchActions(selection: SelectionMode, actions: SelectionActions) -> impl IntoView {
    use crate::logic::selection::{favorite_actions_for_selection, mobile_actions};
    let selected = Memo::new(move |_| {
        let ids = selection.ids.get();
        actions
            .items
            .get()
            .into_iter()
            .filter(|file| ids.contains(&file.id))
            .collect::<Vec<_>>()
    });
    let available = Memo::new(move |_| {
        mobile_actions(
            &selected.get(),
            actions.file_tools.get(),
            actions.trash_mode.get(),
        )
    });
    let single = move || selected.with(|files| (files.len() == 1).then(|| files[0].clone()));
    let context = Signal::derive(move || {
        let mut ids = selection.ids.get().into_iter().collect::<Vec<_>>();
        ids.sort();
        ids.join(",")
    });
    let favorites = RwSignal::new(FavoriteMenuState::Loading);
    let request_revision = RwSignal::new(0_u64);
    let load_favorites = Callback::new(move |()| {
        let mut ids = selected
            .get_untracked()
            .into_iter()
            .filter(|file| file.kind == FileKind::File)
            .map(|file| file.id)
            .collect::<Vec<_>>();
        ids.sort();
        ids.dedup();
        if ids.is_empty() || selection.management.get_untracked().is_none() {
            return;
        }
        request_revision.update(|value| *value += 1);
        let revision = request_revision.get_untracked();
        let scope = context.get_untracked();
        favorites.set(FavoriteMenuState::Loading);
        leptos::task::spawn_local_scoped_with_cancellation(async move {
            let mut states = Vec::new();
            for batch in ids.chunks(revaro_core::library::MAX_SELECTION_STATES) {
                if request_revision.try_get_untracked() != Some(revision)
                    || context.try_get_untracked().as_ref() != Some(&scope)
                {
                    return;
                }
                match crate::api::fetch_selection_state(batch).await {
                    Ok(batch) => states.extend(batch),
                    Err(_) => {
                        if request_revision.try_get_untracked() == Some(revision)
                            && context.try_get_untracked().as_ref() == Some(&scope)
                        {
                            favorites.set(FavoriteMenuState::Failed);
                        }
                        return;
                    }
                }
            }
            if request_revision.try_get_untracked() != Some(revision)
                || context.try_get_untracked().as_ref() != Some(&scope)
            {
                return;
            }
            // Missing/deleted targets must not silently appear as unfavorited.
            if let Some(actions) = favorite_actions_for_selection(&ids, &states) {
                favorites.set(FavoriteMenuState::Ready(actions));
            } else {
                favorites.set(FavoriteMenuState::Failed);
            }
        });
    });

    view! {
        <div class="selection-mobile-actions">
            <button type="button" on:click=move |_| actions.on_select_all.run(())>
                {check_icon()}
                <span>{move || if selected.get().len() == actions.items.get().len() { "取消全选" } else { "全选" }}</span>
            </button>
            <Show when=move || !actions.trash_mode.get() fallback=move || view! {
                <button type="button" on:click=move |_| actions.on_restore.run(())>{restore_icon()}<span>"恢复"</span></button>
                <button class="danger" type="button" on:click=move |_| actions.on_purge.run(())>{trash_icon()}<span>"永久删除"</span></button>
            }>
                <button type="button" disabled=move || !available.get().read title=move || if available.get().read { "阅读" } else { "请选择一本书籍" }
                    on:click=move |_| { if available.get_untracked().read && let Some(file) = single() { actions.on_read.run(file); } }>
                    {icons::book_open()}<span>"阅读"</span>
                </button>
                <button type="button" on:click=move |_| actions.on_download.run(())>{download_icon()}<span>"下载"</span></button>
                <button type="button" on:click=move |_| actions.on_move.run(())>{move_arrow_icon()}<span>"移动"</span></button>
                <button class="danger" type="button" on:click=move |_| actions.on_delete.run(())>{trash_icon()}<span>"删除"</span></button>
                <ActionMenu label="更多所选项目操作".to_owned() icon=MenuIcon::More text=Signal::derive(|| "更多".to_owned()) context=context
                    close_when_hidden=true viewport_bound=true panel_class="selection-mobile-panel".to_owned()
                    disabled=Signal::derive(move || selection.management.get().is_some_and(|management| management.busy.get())
                        || selection.stacks.get().is_some_and(|stacks| stacks.busy.get()))
                    on_toggle=Callback::new(move |open| { if open { load_favorites.run(()); } })>
                    <Show when=move || available.get().open_other.is_some() fallback=|| ()>
                        <button type="button" data-close-menu="true" on:click=move |_| { if let Some(file) = single() { selection.exit(); actions.on_open.run(file); } }>
                            {move || single().map(|file| {
                                if matches!(available.get().open_other, Some(crate::logic::selection::OpenAction::Editor)) {
                                    edit_icon().into_any()
                                } else {
                                    open_icon(&file)
                                }
                            })}<span>{move || available.get().open_other.map(|open| open.label())}</span>
                        </button>
                    </Show>
                    <Show when=move || available.get().copy fallback=|| ()>
                        <button type="button" data-close-menu="true" on:click=move |_| actions.on_copy.run(())>{icons::copy()}<span>"复制到"</span></button>
                    </Show>
                    <Show when=move || available.get().share fallback=|| ()>
                        <button type="button" data-close-menu="true" on:click=move |_| { if let Some(file) = single() { actions.on_share.run(file); } }>{share_icon()}<span>"分享"</span></button>
                    </Show>
                    <Show when=move || available.get().rename fallback=|| ()>
                        <button type="button" data-close-menu="true" on:click=move |_| actions.on_rename.run(())>{rename_icon()}<span>"重命名"</span></button>
                    </Show>
                    <Show when=move || selected.get().iter().any(|file| file.kind == FileKind::File) fallback=|| ()>
                        {move || selection.management.get().map(|management| view! {
                            <Show when=move || matches!(favorites.get(), FavoriteMenuState::Loading) fallback=|| ()>
                                <button type="button" disabled>{icons::heart()}<span>"正在读取收藏状态…"</span></button>
                            </Show>
                            <Show when=move || matches!(favorites.get(), FavoriteMenuState::Failed) fallback=|| ()>
                                <button type="button" on:click=move |_| load_favorites.run(())>{icons::rotate_ccw()}<span>"重试读取收藏状态"</span></button>
                            </Show>
                            <Show when=move || matches!(favorites.get(), FavoriteMenuState::Ready(state) if state.favorite) fallback=|| ()>
                                <button type="button" data-close-menu="true" disabled=move || management.busy.get() on:click=move |_| management.on_favorite.run(true)>{icons::heart()}<span>"收藏"</span></button>
                            </Show>
                            <Show when=move || matches!(favorites.get(), FavoriteMenuState::Ready(state) if state.unfavorite) fallback=|| ()>
                                <button type="button" data-close-menu="true" disabled=move || management.busy.get() on:click=move |_| management.on_favorite.run(false)>{icons::heart()}<span>"取消收藏"</span></button>
                            </Show>
                            {[LibraryPage::Books, LibraryPage::Music, LibraryPage::Gallery, LibraryPage::Videos].into_iter().map(|page| view! {
                                <Show when=move || selected.get().iter().any(|file| matches_collection(file, page)) fallback=|| ()>
                                    <button type="button" data-close-menu="true" disabled=move || management.busy.get() on:click=move |_| management.on_collection.run(page)>{icons::plus()}<span>{format!("加入{}", page.collection_label())}</span></button>
                                </Show>
                            }).collect_view()}
                            <Show when=move || management.can_remove.get() fallback=|| ()>
                                <button type="button" data-close-menu="true" disabled=move || management.busy.get() on:click=move |_| management.on_remove.run(())>{icons::folder()}<span>"移出当前集合"</span></button>
                            </Show>
                            {move || selection.stacks.get().map(|stacks| view! {
                                <Show when=move || selected.get().iter().any(classify::is_book) fallback=|| ()>
                                    <button type="button" data-close-menu="true" disabled=move || stacks.busy.get() on:click=move |_| stacks.on_stack.run(())>{icons::plus()}<span>"堆叠"</span></button>
                                    <Show when=move || stacks.can_remove.get() fallback=|| ()>
                                        <button type="button" data-close-menu="true" disabled=move || stacks.busy.get() on:click=move |_| stacks.on_remove.run(())>{icons::arrow_left()}<span>"移出堆叠"</span></button>
                                    </Show>
                                </Show>
                            })}
                        })}
                    </Show>
                </ActionMenu>
            </Show>
        </div>
    }
}

pub(super) fn matches_collection(file: &File, page: LibraryPage) -> bool {
    match page {
        LibraryPage::Books => classify::is_book(file),
        LibraryPage::Music => classify::is_audio(file),
        LibraryPage::Gallery => classify::is_image(file),
        LibraryPage::Videos => classify::is_video(file),
        _ => false,
    }
}

fn move_arrow_icon() -> impl IntoView {
    view! {
        <svg viewBox="0 0 24 24" aria-hidden="true">
            <path d="M5 12h14m-5-5 5 5-5 5"></path>
        </svg>
    }
}

fn check_icon() -> impl IntoView {
    view! {
        <svg viewBox="0 0 24 24" aria-hidden="true">
            <path d="M4 4h16v16H4z"></path>
            <path d="m8 12 3 3 5-6"></path>
        </svg>
    }
}

fn edit_icon() -> impl IntoView {
    view! {
        <svg viewBox="0 0 24 24" aria-hidden="true">
            <path d="m4 16-.8 4 4-.8L18.5 7.9l-3.2-3.2L4 16Z"></path>
        </svg>
    }
}

fn rename_icon() -> impl IntoView {
    view! {
        <svg viewBox="0 0 24 24" aria-hidden="true">
            <path d="M5 5h14M12 5v14M9 19h6"></path>
        </svg>
    }
}

fn open_icon(file: &File) -> AnyView {
    if file.kind == FileKind::Directory {
        view! {
            <svg viewBox="0 0 24 24" aria-hidden="true">
                <path d="M3 7h7l2 2h9v9H3z"></path>
            </svg>
        }
        .into_any()
    } else if classify::is_book(file) {
        view! {
            <svg viewBox="0 0 24 24" aria-hidden="true">
                <path d="M12 5c-1.7-1.4-4.2-2-8-2v14c3.8 0 6.3.6 8 2 1.7-1.4 4.2-2 8-2V3c-3.8 0-6.3.6-8 2Zm0 0v14"></path>
            </svg>
        }
        .into_any()
    } else if classify::is_editable(file) {
        edit_icon().into_any()
    } else if classify::is_image(file) {
        view! {
            <svg viewBox="0 0 24 24" aria-hidden="true">
                <path d="M2.5 12s3.5-6 9.5-6 9.5 6 9.5 6-3.5 6-9.5 6-9.5-6-9.5-6Z"></path>
            </svg>
        }
        .into_any()
    } else {
        view! {
            <svg viewBox="0 0 24 24" aria-hidden="true">
                <path d="M8 5v14l11-7Z"></path>
            </svg>
        }
        .into_any()
    }
}

fn download_icon() -> impl IntoView {
    view! {
        <svg viewBox="0 0 24 24" aria-hidden="true">
            <path d="M12 3v12m0 0 4-4m-4 4-4-4M5 20h14"></path>
        </svg>
    }
}

fn share_icon() -> impl IntoView {
    view! {
        <svg viewBox="0 0 24 24" aria-hidden="true">
            <circle cx="18" cy="5" r="2.5"></circle>
            <circle cx="6" cy="12" r="2.5"></circle>
            <circle cx="18" cy="19" r="2.5"></circle>
            <path d="m8.2 10.8 7.6-4.4M8.2 13.2l7.6 4.4"></path>
        </svg>
    }
}

fn restore_icon() -> impl IntoView {
    view! {
        <svg viewBox="0 0 24 24" aria-hidden="true">
            <path d="M3 12a9 9 0 1 0 3-6.7L3 8"></path>
            <path d="M3 3v5h5"></path>
        </svg>
    }
}

fn trash_icon() -> impl IntoView {
    view! {
        <svg viewBox="0 0 24 24" aria-hidden="true">
            <path d="M4 7h16M9 7V4h6v3m3 0-1 13H7L6 7m4 4v5m4-5v5"></path>
        </svg>
    }
}

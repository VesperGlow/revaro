//! Selection actions for the live folder and trash listings.

use std::collections::HashSet;

use leptos::prelude::*;
use revaro_core::classify;
use revaro_core::model::{File, FileKind};

use crate::logic::format::format_size;

/// A compact toolbar for the currently selected listing entries.
#[component]
pub fn SelectionToolbar(
    items: RwSignal<Vec<File>>,
    selected_ids: RwSignal<HashSet<String>>,
    trash_mode: RwSignal<bool>,
    on_clear: Callback<()>,
    on_select_all: Callback<()>,
    on_rename: Callback<()>,
    on_move: Callback<()>,
    on_delete: Callback<()>,
    on_restore: Callback<()>,
    on_purge: Callback<()>,
    on_open: Callback<File>,
    on_download: Callback<()>,
    on_share: Callback<File>,
) -> impl IntoView {
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
    let selected_file_count = move || {
        selected_items()
            .into_iter()
            .filter(|item| item.kind == FileKind::File)
            .count()
    };
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
        <div class="selection-toolbar" role="toolbar" aria-label="所选项目操作">
            <button
                class="selection-close"
                type="button"
                title="取消选择"
                aria-label="取消选择"
                on:click=move |_| on_clear.run(())
            >
                "×"
            </button>
            <span class="selection-summary">
                <b>{move || format!("{} 项", selected_count())}</b>
                <small>{move || format!("已选择 {}", format_size(selected_bytes()))}</small>
            </span>
            <div class="selection-actions">
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
                    <button type="button" on:click=move |_| on_select_all.run(())>
                        {check_icon()}
                        <span>{move || if all_selected() { "取消全选" } else { "全选" }}</span>
                    </button>
                    <Show
                        when=move || {
                            single_item().is_some_and(|item| {
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
                    <Show when=move || { selected_file_count() > 0 } fallback=|| ()>
                        <button type="button" on:click=move |_| download.run(())>
                            {download_icon()}
                            <span>{move || format!("下载{}", if selected_file_count() > 1 { format!(" ({})", selected_file_count()) } else { String::new() })}</span>
                        </button>
                    </Show>
                    <Show when=move || single_item().is_some_and(|item| item.kind == FileKind::File) fallback=|| ()>
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
                    <Show when=move || selected_count() == 1 fallback=|| ()>
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
        </div>
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

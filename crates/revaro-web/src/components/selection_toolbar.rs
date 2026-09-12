//! Selection actions for the live folder and trash listings.

use std::collections::HashSet;

use leptos::prelude::*;
use revaro_core::model::File;

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
    on_delete: Callback<()>,
    on_restore: Callback<()>,
    on_purge: Callback<()>,
) -> impl IntoView {
    let selected_count = move || selected_ids.get().len();
    let selected_bytes = move || {
        items
            .get()
            .into_iter()
            .filter(|item| selected_ids.get().contains(&item.id))
            .filter(|item| item.kind == revaro_core::model::FileKind::File)
            .map(|item| u64::try_from(item.size).unwrap_or(0))
            .sum::<u64>()
    };
    let all_selected = move || {
        let entries = items.get();
        !entries.is_empty() && selected_ids.get().len() == entries.len()
    };
    let rename = on_rename;
    let delete = on_delete;
    let restore = on_restore;
    let purge = on_purge;

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
                    <Show when=move || selected_count() == 1 fallback=|| ()>
                        <button type="button" on:click=move |_| rename.run(())>
                            {edit_icon()}
                            <span>"重命名"</span>
                        </button>
                    </Show>
                    <button class="danger" type="button" on:click=move |_| delete.run(())>
                        {trash_icon()}
                        <span>"删除"</span>
                    </button>
                </Show>
            </div>
        </div>
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

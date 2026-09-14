//! Media-library views and the pure grouping rules behind their navigation.
//!
//! The ordinary file browser and the four media buckets deliberately share
//! the same `File` DTO, but the reference UI has different grouping and action
//! surfaces for books, galleries and audio. This module restores those
//! bucket-specific entry points without duplicating the HTTP contract.

use std::cmp::Ordering;
use std::collections::HashMap;

use js_sys::Intl::Collator;
use js_sys::{Array, JsString, Object, Reflect};
use leptos::prelude::*;
use revaro_core::classify::LibraryKind;
use revaro_core::model::{File, FileKind, LibraryItem};
use wasm_bindgen::JsValue;

use crate::browser;
use crate::logic::format::format_size;

use super::file_browser::{file_preview, file_preview_with_state};
use super::icons;

/// A merged folder node used by the sidebar path accordion.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LibraryFolderNode {
    /// Folder id, with [`revaro_core::ids::ROOT_ID`] for the virtual root.
    pub id: String,
    /// Display name.
    pub name: String,
    /// Full path used as a tooltip.
    pub path: String,
    /// Number of media entries in this node's subtree.
    pub count: usize,
    /// Sorted child nodes.
    pub children: Vec<LibraryFolderNode>,
}

/// Build the same path tree the old `useLibrary` composable exposed.
#[must_use]
pub fn build_folder_tree(items: &[LibraryItem]) -> LibraryFolderNode {
    let mut root = LibraryFolderNode {
        id: revaro_core::ids::ROOT_ID.to_owned(),
        name: "我的文件".to_owned(),
        path: String::new(),
        count: 0,
        children: Vec::new(),
    };
    for item in items {
        root.count += 1;
        let mut node = &mut root;
        let mut path = String::new();
        for folder in &item.folder_path {
            path = if path.is_empty() {
                folder.name.clone()
            } else {
                format!("{} / {}", path, folder.name)
            };
            let index = node.children.iter().position(|child| child.id == folder.id);
            let index = match index {
                Some(index) => index,
                None => {
                    node.children.push(LibraryFolderNode {
                        id: folder.id.clone(),
                        name: folder.name.clone(),
                        path: path.clone(),
                        count: 0,
                        children: Vec::new(),
                    });
                    node.children.len() - 1
                }
            };
            node = &mut node.children[index];
            node.count += 1;
        }
    }
    let collator = chinese_collator(false);
    sort_folder_nodes(&mut root, &collator);
    root
}

fn sort_folder_nodes(node: &mut LibraryFolderNode, collator: &Collator) {
    node.children
        .sort_by(|left, right| collator_compare(collator, &left.name, &right.name));
    for child in &mut node.children {
        sort_folder_nodes(child, collator);
    }
}

/// The reference uses `localeCompare('zh-Hans-CN')` for all visible library
/// folder/album names. Keep that comparison in the browser instead of using
/// Rust's Unicode code-point order, which puts otherwise ordinary Chinese
/// names in a visibly different order.
fn chinese_collator(numeric: bool) -> Collator {
    let locales = Array::of1(&JsString::from("zh-Hans-CN"));
    let options = Object::new();
    let _ = Reflect::set(
        &options,
        &JsValue::from_str("numeric"),
        &JsValue::from_bool(numeric),
    );
    Collator::new(&locales, &options)
}

fn collator_compare(collator: &Collator, left: &str, right: &str) -> Ordering {
    let result = collator
        .compare()
        .call2(
            &JsValue::UNDEFINED,
            &JsValue::from_str(left),
            &JsValue::from_str(right),
        )
        .ok()
        .and_then(|value| value.as_f64())
        .unwrap_or_else(|| match left.cmp(right) {
            Ordering::Less => -1.0,
            Ordering::Equal => 0.0,
            Ordering::Greater => 1.0,
        });
    result.partial_cmp(&0.0).unwrap_or(Ordering::Equal)
}

/// Return the human-readable path of a library item.
#[must_use]
pub fn folder_label(item: &LibraryItem) -> String {
    if item.folder_path.is_empty() {
        "我的文件".to_owned()
    } else {
        let path = item
            .folder_path
            .iter()
            .map(|folder| folder.name.as_str())
            .filter(|name| !name.is_empty())
            .collect::<Vec<_>>();
        if path.is_empty() {
            "我的文件".to_owned()
        } else {
            path.join(" / ")
        }
    }
}

/// Filter a bucket by a selected sidebar path.
#[must_use]
pub fn filter_folder(items: &[LibraryItem], folder_id: Option<&str>) -> Vec<LibraryItem> {
    let Some(folder_id) = folder_id else {
        return items.to_vec();
    };
    items
        .iter()
        .filter(|item| item.folder_path.iter().any(|folder| folder.id == folder_id))
        .cloned()
        .collect()
}

/// Resolve the label shown in the library header for a selected path.
#[must_use]
pub fn filter_label(items: &[LibraryItem], folder_id: Option<&str>) -> String {
    let Some(folder_id) = folder_id else {
        return "全部位置".to_owned();
    };
    items
        .iter()
        .find_map(|item| {
            let index = item
                .folder_path
                .iter()
                .position(|folder| folder.id == folder_id)?;
            Some(
                item.folder_path[..=index]
                    .iter()
                    .map(|folder| folder.name.as_str())
                    .collect::<Vec<_>>()
                    .join(" / "),
            )
        })
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| "全部位置".to_owned())
}

/// A folder album in the image/video gallery.
#[derive(Debug, Clone, PartialEq, Eq)]
struct AlbumGroup {
    id: String,
    title: String,
    path: String,
    items: Vec<LibraryItem>,
}

fn group_albums(items: &[LibraryItem]) -> Vec<AlbumGroup> {
    let mut indexes = HashMap::<String, usize>::new();
    let mut groups = Vec::<AlbumGroup>::new();
    for item in items {
        let id = item
            .folder_path
            .last()
            .map(|folder| folder.id.clone())
            .unwrap_or_else(|| revaro_core::ids::ROOT_ID.to_owned());
        let title = item
            .folder_path
            .last()
            .map(|folder| folder.name.clone())
            .unwrap_or_else(|| "我的文件".to_owned());
        let path = folder_label(item);
        let index = match indexes.get(&id).copied() {
            Some(index) => index,
            None => {
                let index = groups.len();
                indexes.insert(id.clone(), index);
                groups.push(AlbumGroup {
                    id,
                    title,
                    path,
                    items: Vec::new(),
                });
                index
            }
        };
        groups[index].items.push(item.clone());
    }
    for group in &mut groups {
        group
            .items
            .sort_by(|left, right| right.file.updated_at.cmp(&left.file.updated_at));
    }
    let collator = chinese_collator(false);
    groups.sort_by(|left, right| collator_compare(&collator, &left.title, &right.title));
    groups
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct BookSeries {
    key: String,
    title: String,
    items: Vec<LibraryItem>,
}

fn book_series(items: &[LibraryItem]) -> Vec<BookSeries> {
    let mut indexes = HashMap::<String, usize>::new();
    let mut groups = Vec::<BookSeries>::new();
    for item in items {
        let (title, _) = book_series_info(&item.file.name);
        let key = series_key(&title, &item.file.name);
        let index = match indexes.get(&key).copied() {
            Some(index) => index,
            None => {
                let index = groups.len();
                indexes.insert(key.clone(), index);
                groups.push(BookSeries {
                    key,
                    title: title.clone(),
                    items: Vec::new(),
                });
                index
            }
        };
        groups[index].items.push(item.clone());
    }
    let collator = chinese_collator(true);
    for group in &mut groups {
        group.items.sort_by(|left, right| {
            let left_info = book_series_info(&left.file.name).1;
            let right_info = book_series_info(&right.file.name).1;
            match (left_info, right_info) {
                (Some(left), Some(right)) if left != right => left.cmp(&right),
                _ => collator_compare(&collator, &left.file.name, &right.file.name),
            }
        });
    }
    groups
}

fn series_key(title: &str, fallback: &str) -> String {
    let key = title
        .to_lowercase()
        .chars()
        .filter(|character| {
            !character.is_whitespace() && !matches!(character, '-' | '_' | '·' | ':' | '：')
        })
        .collect::<String>();
    if key.is_empty() {
        fallback
            .to_lowercase()
            .chars()
            .filter(|character| {
                !character.is_whitespace() && !matches!(character, '-' | '_' | '·' | ':' | '：')
            })
            .collect()
    } else {
        key
    }
}

fn book_series_info(name: &str) -> (String, Option<u32>) {
    let base = name.rsplit_once('.').map_or(name, |(base, _)| base).trim();
    let (title, volume) = strip_chapter_suffix(base);
    if let Some(volume) = volume {
        return (title, Some(volume));
    }
    (base.to_owned(), None)
}

fn strip_chapter_suffix(base: &str) -> (String, Option<u32>) {
    let trimmed = base.trim_end();
    if let Some(last) = trimmed.chars().last()
        && matches!(last, '卷' | '册' | '部' | '集' | '篇' | '季')
    {
        let before_unit = trimmed[..trimmed.len() - last.len_utf8()].trim_end();
        if let Some(marker) = before_unit.rfind('第') {
            let raw = &before_unit[marker + '第'.len_utf8()..];
            if let Some(volume) = parse_chinese_or_decimal(raw)
                && !before_unit[..marker].trim().is_empty()
            {
                return (trim_suffix_separators(&before_unit[..marker]), Some(volume));
            }
        }
    }

    if let Some((open, close)) = bracket_suffix(trimmed)
        && trimmed[open + 1..close]
            .chars()
            .all(|ch| ch.is_ascii_digit())
        && let Ok(volume) = trimmed[open + 1..close].parse::<u32>()
    {
        let title = trim_suffix_separators(&trimmed[..open]);
        if !title.is_empty() {
            return (title, Some(volume));
        }
    }

    for keyword in ["volume", "vol", "part", "book", "chapter"] {
        if let Some((title, number)) = word_number_suffix(trimmed, keyword) {
            return (title, Some(number));
        }
    }

    if let Some((title, number)) = v_number_suffix(trimmed) {
        return (title, Some(number));
    }
    if let Some((title, number)) = separator_number_suffix(trimmed) {
        return (title, Some(number));
    }
    (base.to_owned(), None)
}

fn bracket_suffix(value: &str) -> Option<(usize, usize)> {
    let close = value.len();
    let last = value.chars().last()?;
    let open_char = match last {
        ']' => '[',
        ')' => '(',
        '】' => '【',
        '）' => '（',
        _ => return None,
    };
    let open = value.rfind(open_char)?;
    Some((open, close - last.len_utf8()))
}

fn word_number_suffix(value: &str, keyword: &str) -> Option<(String, u32)> {
    let lower = value.to_ascii_lowercase();
    let end = lower.len();
    let mut number_start = end;
    while number_start > 0 && lower.as_bytes()[number_start - 1].is_ascii_digit() {
        number_start -= 1;
    }
    if number_start == end {
        return None;
    }
    let mut marker_end = number_start;
    while marker_end > 0 && lower.as_bytes()[marker_end - 1].is_ascii_whitespace() {
        marker_end -= 1;
    }
    if marker_end > 0 && lower.as_bytes()[marker_end - 1] == b'.' {
        marker_end -= 1;
        while marker_end > 0 && lower.as_bytes()[marker_end - 1].is_ascii_whitespace() {
            marker_end -= 1;
        }
    }
    let marker_start = marker_end.saturating_sub(keyword.len());
    if &lower[marker_start..marker_end] != keyword {
        return None;
    }
    let title = trim_suffix_separators(&value[..marker_start]);
    let number = value[number_start..].parse().ok()?;
    (!title.is_empty()).then_some((title, number))
}

fn v_number_suffix(value: &str) -> Option<(String, u32)> {
    let bytes = value.as_bytes();
    let mut number_start = bytes.len();
    while number_start > 0
        && bytes.len() - number_start < 3
        && bytes[number_start - 1].is_ascii_digit()
    {
        number_start -= 1;
    }
    if number_start == bytes.len() || bytes.len() - number_start > 3 || number_start == 0 {
        return None;
    }
    let marker = bytes[number_start - 1];
    if !matches!(marker, b'V' | b'v') {
        return None;
    }
    let title = trim_suffix_separators(&value[..number_start - 1]);
    let number = value[number_start..].parse().ok()?;
    (!title.is_empty()).then_some((title, number))
}

fn separator_number_suffix(value: &str) -> Option<(String, u32)> {
    let bytes = value.as_bytes();
    let mut number_start = bytes.len();
    while number_start > 0
        && bytes[number_start - 1].is_ascii_digit()
        && bytes.len() - number_start < 3
    {
        number_start -= 1;
    }
    if number_start == bytes.len() || bytes.len() - number_start > 3 {
        return None;
    }
    let title_end = number_start;
    while number_start > 0 && matches!(bytes[number_start - 1], b' ' | b'\t' | b'-' | b'_' | 0xc2) {
        number_start -= 1;
    }
    let title = trim_suffix_separators(&value[..title_end]);
    if title.len() == value.len() || title.is_empty() {
        return None;
    }
    let separator = value[title.len()..title_end]
        .chars()
        .any(|ch| ch.is_whitespace() || matches!(ch, '-' | '_' | '·'));
    if !separator {
        return None;
    }
    let number = value[title_end..].parse().ok()?;
    Some((title, number))
}

fn trim_suffix_separators(value: &str) -> String {
    value
        .trim_end_matches(|ch: char| ch.is_whitespace() || matches!(ch, '-' | '_' | '·'))
        .trim()
        .to_owned()
}

fn parse_chinese_or_decimal(value: &str) -> Option<u32> {
    let value = value.trim();
    if value.chars().all(|ch| ch.is_ascii_digit()) {
        return value.parse().ok();
    }
    let mut total = 0_u32;
    let mut current = 0_u32;
    for ch in value.chars() {
        let digit = match ch {
            '零' | '〇' => 0,
            '一' => 1,
            '二' | '两' => 2,
            '三' => 3,
            '四' => 4,
            '五' => 5,
            '六' => 6,
            '七' => 7,
            '八' => 8,
            '九' => 9,
            '十' => {
                total += (current.max(1)) * 10;
                current = 0;
                continue;
            }
            '百' => {
                total += (current.max(1)) * 100;
                current = 0;
                continue;
            }
            '千' => {
                total += (current.max(1)) * 1000;
                current = 0;
                continue;
            }
            _ if ch.is_whitespace() => continue,
            _ => return None,
        };
        current = digit;
    }
    Some(total + current)
}

/// The Rust equivalent of the old `LibraryView.vue`.
#[component]
pub fn LibraryView(
    kind: LibraryKind,
    items: RwSignal<Vec<LibraryItem>>,
    loading: RwSignal<bool>,
    error: RwSignal<String>,
    filter_label_signal: RwSignal<String>,
    on_open: Callback<File>,
    on_refresh: Callback<()>,
    on_upload: Callback<()>,
) -> impl IntoView {
    let gallery_key = if kind == LibraryKind::Image || kind == LibraryKind::Video {
        format!("revaro:library:gallery:{}", kind.as_str())
    } else {
        String::new()
    };
    let media_key = "revaro:library:media:audio".to_owned();
    let gallery_mode = RwSignal::new(if gallery_key.is_empty() {
        "all".to_owned()
    } else {
        browser::local_storage_get(&gallery_key).unwrap_or_else(|| "all".to_owned())
    });
    let media_mode =
        RwSignal::new(browser::local_storage_get(&media_key).unwrap_or_else(|| "grid".to_owned()));
    let title = category_title(kind);
    let eyebrow = category_eyebrow(kind);

    let set_gallery_mode = {
        let gallery_mode = gallery_mode;
        let gallery_key = gallery_key.clone();
        Callback::new(move |mode: String| {
            gallery_mode.set(mode.clone());
            if !gallery_key.is_empty() {
                browser::local_storage_set(&gallery_key, &mode);
            }
        })
    };
    let set_media_mode = {
        let media_mode = media_mode;
        Callback::new(move |mode: String| {
            media_mode.set(mode.clone());
            browser::local_storage_set(&media_key, &mode);
        })
    };
    let title_for_empty = title;
    let open_for_cards = on_open.clone();

    view! {
        <section class="library-view">
            <header class="library-head">
                <div class="library-heading">
                    <p class="eyebrow dark">{eyebrow}</p>
                    <h1>{title}</h1>
                    <p class="folder-meta">
                        <span>{move || filter_label_signal.get()}</span>
                        <i></i>
                        <span>{move || format!("{} 个项目", items.get().len())}</span>
                    </p>
                </div>
                <div class="actions">
                    <Show when=move || kind == LibraryKind::Audio fallback=|| ()>
                        <div class="view-switch" role="group" aria-label="音乐视图切换">
                            <button
                                type="button"
                                class:active=move || media_mode.get() == "grid"
                                aria-pressed=move || if media_mode.get() == "grid" { "true" } else { "false" }
                                on:click=move |_| set_media_mode.run("grid".to_owned())
                            >
                                {icons::layout_grid()}
                                "方块"
                            </button>
                            <button
                                type="button"
                                class:active=move || media_mode.get() == "list"
                                aria-pressed=move || if media_mode.get() == "list" { "true" } else { "false" }
                                on:click=move |_| set_media_mode.run("list".to_owned())
                            >
                                {icons::list()}
                                "列表"
                            </button>
                        </div>
                    </Show>
                    <button class="secondary" type="button" on:click=move |_| on_refresh.run(())>
                        {icons::refresh_cw()}
                        "刷新"
                    </button>
                    <button class="primary" type="button" on:click=move |_| on_upload.run(())>
                        {icons::upload()}
                        "上传"
                    </button>
                </div>
            </header>

            <Show
                when=move || !loading.get() && error.get().is_empty()
                fallback=move || view! {
                    <Show
                        when=move || loading.get()
                        fallback=move || view! {
                            <div class="state empty" role="alert">
                                <div class="empty-icon" aria-hidden="true">"!"</div>
                                <h3>"读取失败"</h3>
                                <p>{move || error.get()}</p>
                                <button class="secondary" type="button" on:click=move |_| on_refresh.run(())>"重试"</button>
                            </div>
                        }
                    >
                        <div class="state" aria-live="polite"><div class="spinner"></div><p>{format!("正在整理{}…", title)}</p></div>
                    </Show>
                }
            >
                <Show
                    when=move || !items.get().is_empty()
                    fallback=move || view! {
                        <div class="state empty">
                            <div class="empty-icon" aria-hidden="true">"⌁"</div>
                            <h3>{format!("这里还没有{}内容", title_for_empty)}</h3>
                            <p>"上传后会自动归类到这里。"</p>
                            <button class="primary" type="button" on:click=move |_| on_upload.run(())>"上传文件"</button>
                        </div>
                    }
                >
                    {render_library_content(kind, items, gallery_mode, media_mode, open_for_cards.clone(), set_gallery_mode.clone())}
                </Show>
            </Show>
        </section>
    }
}

fn render_library_content(
    kind: LibraryKind,
    items: RwSignal<Vec<LibraryItem>>,
    gallery_mode: RwSignal<String>,
    media_mode: RwSignal<String>,
    on_open: Callback<File>,
    set_gallery_mode: Callback<String>,
) -> AnyView {
    match kind {
        LibraryKind::Book => {
            let groups = Signal::derive_local(move || book_series(&items.get()));
            view! {
                <div class="book-shelf">
                    <For each=move || groups.get() key=|group| group.key.clone() let:group>
                        <BookSeriesCard group=group on_open=on_open.clone() />
                    </For>
                </div>
            }
            .into_any()
        }
        LibraryKind::Image | LibraryKind::Video => {
            let all = Signal::derive_local(move || {
                let mut values = items.get();
                values.sort_by(|left, right| right.file.updated_at.cmp(&left.file.updated_at));
                values
            });
            let albums = Signal::derive_local(move || group_albums(&items.get()));
            let all_open = on_open.clone();
            let album_open = on_open;
            view! {
                <Show
                    when=move || gallery_mode.get() == "albums"
                    fallback=move || view! {
                        <div class="file-grid">
                            <For each=move || all.get() key=|item| item.file.id.clone() let:item>
                                <LibraryCard item=item on_open=all_open.clone() />
                            </For>
                        </div>
                    }
                >
                    <div class="album-list">
                        <For each=move || albums.get() key=|album| album.id.clone() let:album>
                            <section class="album">
                                <header><div><h3>{album.title}</h3><p>{album.path}</p></div><span>{format!("{} 项", album.items.len())}</span></header>
                                <div class="file-grid">
                                    <For each=move || album.items.clone() key=|item| item.file.id.clone() let:item>
                                        <LibraryCard item=item on_open=album_open.clone() />
                                    </For>
                                </div>
                            </section>
                        </For>
                    </div>
                </Show>
                <div class="gallery-switch" role="group" aria-label="图库视图切换">
                    <button
                        type="button"
                        class:active=move || gallery_mode.get() == "all"
                        aria-pressed=move || if gallery_mode.get() == "all" { "true" } else { "false" }
                        on:click=move |_| set_gallery_mode.run("all".to_owned())
                    >
                        {icons::layout_grid()}
                        "全部视图"
                    </button>
                    <button
                        type="button"
                        class:active=move || gallery_mode.get() == "albums"
                        aria-pressed=move || if gallery_mode.get() == "albums" { "true" } else { "false" }
                        on:click=move |_| set_gallery_mode.run("albums".to_owned())
                    >
                        {icons::images()}
                        "图库分类"
                    </button>
                </div>
            }
            .into_any()
        }
        LibraryKind::Audio => {
            let on_open = on_open.clone();
            view! {
                <Show
                    when=move || media_mode.get() == "list"
                    fallback=move || view! {
                        <div class="file-grid">
                            <For each=move || items.get() key=|item| item.file.id.clone() let:item>
                                <LibraryCard item=item on_open=on_open.clone() />
                            </For>
                        </div>
                    }
                >
                    <div class="file-rows">
                        <For each=move || items.get() key=|item| item.file.id.clone() let:item>
                            <LibraryRow item=item on_open=on_open.clone() />
                        </For>
                    </div>
                </Show>
            }
            .into_any()
        }
        LibraryKind::File => ().into_any(),
    }
}

#[component]
fn BookSeriesCard(group: BookSeries, on_open: Callback<File>) -> impl IntoView {
    let is_series = group.items.len() > 1;
    let item = group.items.first().cloned();
    let items = group.items;
    let title = group.title;
    let count = items.len();
    view! {
        <Show
            when=move || is_series
            fallback=move || {
                item.clone().map_or_else(
                    || ().into_any(),
                    |item| view! { <BookSingleCard item=item on_open=on_open.clone() /> }.into_any(),
                )
            }
        >
            <article class="shelf-card series-card">
                <div class="series-stage">
                    <BookSeriesCovers items=items.clone() on_open=on_open.clone() />
                    <span class="series-badge">{format!("{} 本", count)}</span>
                </div>
                <div class="shelf-meta">
                    <strong title=title.clone()>{title.clone()}</strong>
                    <small>{format!("同系列 · {} 本", count)}</small>
                </div>
            </article>
        </Show>
    }
}

#[component]
fn BookSeriesCovers(items: Vec<LibraryItem>, on_open: Callback<File>) -> impl IntoView {
    let count = items.len();
    let first = items.first().cloned();
    let remaining = items.into_iter().skip(1).enumerate().collect::<Vec<_>>();
    view! {
        {first.map_or_else(
            || ().into_any(),
            |item| view! {
                <button
                    class="series-cover series-cover-main"
                    type="button"
                    style=main_cover_style()
                    title=format!("阅读 {}", item.file.name)
                    on:click={let on_open = on_open.clone(); move |_| on_open.run(item.file.clone())}
                >
                    {book_cover(&item.file, true)}
                </button>
            }.into_any(),
        )}
        <For each=move || remaining.clone() key=|(index, item)| format!("{}:{}", index, item.file.id) let:entry>
            <button
                class="series-cover series-cover-fan"
                type="button"
                style=fan_cover_style(entry.0, count)
                title=format!("阅读 {}", entry.1.file.name)
                on:click={let on_open = on_open.clone(); move |_| on_open.run(entry.1.file.clone())}
            >
                {book_cover(&entry.1.file, false)}
            </button>
        </For>
    }
}

#[component]
fn BookSingleCard(item: LibraryItem, on_open: Callback<File>) -> impl IntoView {
    let file = item.file;
    let title = file
        .name
        .rsplit_once('.')
        .map_or(file.name.as_str(), |(title, _)| title)
        .to_owned();
    let size = format_size(non_negative(file.size));
    let title_for_attr = title.clone();
    let title_for_text = title;
    let open = on_open.clone();
    let key_file = file.clone();
    view! {
        <article
            class="shelf-card"
            title=format!("阅读 {}", file.name)
            role="button"
            tabindex="0"
            on:click=move |_| open.run(file.clone())
            on:keydown=move |event: web_sys::KeyboardEvent| {
                if event.key() == "Enter" {
                    event.prevent_default();
                    on_open.run(key_file.clone());
                }
            }
        >
            <div class="shelf-cover">{book_cover(&file, true)}</div>
            <div class="shelf-meta"><strong title=title_for_attr>{title_for_text}</strong><small>{size}</small></div>
        </article>
    }
}

fn main_cover_style() -> String {
    "width:58%;z-index:3000".to_owned()
}

fn fan_cover_style(index: usize, count: usize) -> String {
    let fans = count.saturating_sub(1);
    let step = if fans == 0 { 0.0 } else { 42.0 / fans as f64 };
    let max_angle = if fans <= 1 {
        0.0
    } else {
        (7.0 / fans as f64).min(2.0)
    };
    let angle = if fans <= 1 {
        0.0
    } else {
        (index as f64 - (fans - 1) as f64 / 2.0) * (2.0 * max_angle / (fans - 1) as f64)
    };
    format!(
        "left:{:.3}%;width:58%;z-index:{};transform:rotate({angle:.3}deg) scale(1.06)",
        (index + 1) as f64 * step,
        2999_usize.saturating_sub(index)
    )
}

#[component]
fn LibraryCard(item: LibraryItem, on_open: Callback<File>) -> impl IntoView {
    let file = item.file.clone();
    let file_for_key = file.clone();
    let preview_available = RwSignal::new(initial_library_preview_available(&file));
    let class_file = file.clone();
    let label = file.name.clone();
    let label_for_title = label.clone();
    let preview_title = library_preview_title(&file);
    let open = on_open.clone();
    let open_file = file.clone();
    view! {
        <article
            class=move || library_tile_class(&class_file, preview_available.get())
            role="button"
            tabindex="0"
            aria-label=format!("{}，未选择", label)
            on:click=move |_| open.run(open_file.clone())
            on:keydown=move |event: web_sys::KeyboardEvent| {
                if event.key() == "Enter" {
                    event.prevent_default();
                    on_open.run(file_for_key.clone());
                } else if event.key() == " " {
                    event.prevent_default();
                }
            }
        >
            <div class="card-preview" title=preview_title>{file_preview_with_state(&item.file, Some(preview_available))}</div>
            <div class="card-info">
                <strong title=label_for_title>{label.clone()}</strong>
                <small>{format_size(non_negative(item.file.size))}</small>
            </div>
        </article>
    }
}

fn library_preview_title(file: &File) -> &'static str {
    if revaro_core::classify::is_book(file) {
        "阅读"
    } else if file.kind == FileKind::Directory {
        "打开文件夹"
    } else if revaro_core::classify::is_editable(file) {
        "编辑文档"
    } else if revaro_core::classify::is_image(file) {
        "预览图片"
    } else if revaro_core::classify::is_video(file) {
        "播放视频"
    } else if revaro_core::classify::is_audio(file) {
        "播放音频"
    } else {
        "文件"
    }
}

#[component]
fn LibraryRow(item: LibraryItem, on_open: Callback<File>) -> impl IntoView {
    let file = item.file.clone();
    let file_for_key = file.clone();
    let name = file.name.clone();
    let name_for_title = name.clone();
    view! {
        <article
            class="file-row"
            role="button"
            tabindex="0"
            aria-label=format!("{}，未选择", name)
            on:click=move |_| on_open.run(file.clone())
            on:keydown=move |event: web_sys::KeyboardEvent| {
                if event.key() == "Enter" {
                    event.prevent_default();
                    on_open.run(file_for_key.clone());
                } else if event.key() == " " {
                    event.prevent_default();
                }
            }
        >
            <div class="row-preview">{library_preview(&item.file)}</div>
            <div class="row-info"><strong title=name_for_title>{name.clone()}</strong><small>{folder_label(&item)}</small></div>
            <span class="row-duration">{format_duration(item.duration_ms)}</span>
        </article>
    }
}

fn library_preview(file: &File) -> AnyView {
    if revaro_core::classify::is_book(file) {
        book_cover(file, false)
    } else {
        file_preview(file)
    }
}

fn book_cover(file: &File, show_title: bool) -> AnyView {
    let title = file
        .name
        .rsplit_once('.')
        .map_or(file.name.as_str(), |(title, _)| title)
        .to_owned();
    let first_letter = title.chars().next().unwrap_or('书');
    let fallback_title = title.clone();
    let thumbnail = format!(
        "/api/files/{}/thumbnail?v={}",
        file.id,
        js_sys::encode_uri_component(&file.etag)
            .as_string()
            .unwrap_or_default()
    );
    let alt = file.name.clone();
    let broken = RwSignal::new(false);
    view! {
        <span class="book-cover">
            <Show
                when=move || !broken.get()
                fallback=move || {
                    let fallback_title = fallback_title.clone();
                    view! {
                        <span class="book-cover-fallback">
                            <b>{first_letter}</b>
                            {if show_title {
                                view! { <small>{fallback_title}</small> }.into_any()
                            } else {
                                ().into_any()
                            }}
                        </span>
                    }
                }
            >
                <img
                    class="ui-image"
                    src=thumbnail.clone()
                    alt=alt.clone()
                    loading="lazy"
                    draggable="false"
                    on:error=move |_| broken.set(true)
                />
            </Show>
        </span>
    }
    .into_any()
}

fn library_tile_class(file: &File, preview_available: bool) -> String {
    let mut class = String::from("file-card");
    if file.kind == FileKind::Directory {
        class.push_str(" folder-tile");
    } else if revaro_core::classify::is_book(file) {
        class.push_str(" book-tile");
    } else if revaro_core::classify::is_audio(file) {
        class.push_str(" audio-tile");
    }
    if preview_available {
        class.push_str(" preview-tile");
    } else {
        class.push_str(" fallback-tile");
    }
    class
}

fn initial_library_preview_available(file: &File) -> bool {
    revaro_core::classify::is_image(file)
        || (revaro_core::classify::is_audio(file) && file.has_cover)
        || revaro_core::classify::is_video(file)
        || revaro_core::classify::is_epub_name(&file.name)
}

fn category_title(kind: LibraryKind) -> &'static str {
    match kind {
        LibraryKind::Book => "书架",
        LibraryKind::Image => "图片",
        LibraryKind::Video => "视频",
        LibraryKind::Audio => "音乐",
        LibraryKind::File => "文件",
    }
}

fn category_eyebrow(kind: LibraryKind) -> &'static str {
    match kind {
        LibraryKind::Book => "BOOKSHELF",
        LibraryKind::Image => "PHOTOS",
        LibraryKind::Video => "VIDEOS",
        LibraryKind::Audio => "MUSIC",
        LibraryKind::File => "FILES",
    }
}

fn format_duration(duration_ms: i64) -> String {
    if duration_ms <= 0 {
        return "--:--".to_owned();
    }
    let total = (duration_ms as f64 / 1000.0).round() as u64;
    let hours = total / 3600;
    let minutes = (total % 3600) / 60;
    let seconds = total % 60;
    if hours > 0 {
        format!("{hours}:{minutes:02}:{seconds:02}")
    } else {
        format!("{minutes}:{seconds:02}")
    }
}

fn non_negative(value: i64) -> u64 {
    u64::try_from(value).unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use revaro_core::model::{File, FileKind, FileStatus};

    fn item(id: &str, folder: &[(&str, &str)]) -> LibraryItem {
        LibraryItem {
            file: File {
                id: id.to_owned(),
                name: format!("{id}.jpg"),
                kind: FileKind::File,
                status: FileStatus::Ready,
                ..File::default()
            },
            folder_path: folder
                .iter()
                .map(|(id, name)| revaro_core::model::FolderRef {
                    id: (*id).to_owned(),
                    name: (*name).to_owned(),
                })
                .collect(),
            duration_ms: 0,
        }
    }

    #[test]
    fn folder_tree_counts_each_item_in_every_ancestor() {
        let tree = build_folder_tree(&[
            item("one", &[("a", "相册")]),
            item("two", &[("a", "相册"), ("b", "旅行")]),
        ]);
        assert_eq!(tree.count, 2);
        assert_eq!(tree.children[0].count, 2);
        assert_eq!(tree.children[0].children[0].count, 1);
    }

    #[test]
    fn folder_filter_keeps_descendants_of_the_selected_folder() {
        let items = vec![
            item("one", &[("a", "相册")]),
            item("two", &[("a", "相册"), ("b", "旅行")]),
            item("three", &[]),
        ];
        assert_eq!(filter_folder(&items, Some("a")).len(), 2);
        assert_eq!(filter_label(&items, Some("b")), "相册 / 旅行");
        assert_eq!(filter_label(&items, None), "全部位置");
    }

    #[test]
    fn book_series_parses_reference_volume_suffixes() {
        assert_eq!(
            book_series_info("三体 第十卷.epub"),
            ("三体".to_owned(), Some(10))
        );
        assert_eq!(
            book_series_info("三体 Vol. 2.txt"),
            ("三体".to_owned(), Some(2))
        );
        assert_eq!(
            book_series_info("三体（3）.epub"),
            ("三体".to_owned(), Some(3))
        );
        assert_eq!(
            book_series_info("三体 V4.epub"),
            ("三体".to_owned(), Some(4))
        );
    }

    #[test]
    fn book_series_orders_numeric_volumes_before_name_fallback() {
        let make = |name: &str| LibraryItem {
            file: File {
                name: name.to_owned(),
                kind: FileKind::File,
                status: FileStatus::Ready,
                ..File::default()
            },
            ..LibraryItem::default()
        };
        let groups = book_series(&[
            make("三体 10.epub"),
            make("三体 2.epub"),
            make("三体 1.epub"),
        ]);
        assert_eq!(groups.len(), 1);
        assert_eq!(
            groups[0]
                .items
                .iter()
                .map(|item| item.file.name.as_str())
                .collect::<Vec<_>>(),
            vec!["三体 1.epub", "三体 2.epub", "三体 10.epub"]
        );
    }
}

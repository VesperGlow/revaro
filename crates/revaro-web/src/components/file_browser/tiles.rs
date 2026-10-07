//! File tiles, previews and thumbnail lifecycle.

use super::*;

#[component]
pub(super) fn FileTile(
    item: File,
    trash_mode: RwSignal<bool>,
    selectable: bool,
    selection: SelectionMode,
    on_open: Callback<File>,
) -> impl IntoView {
    let selected_ids = selection.ids;
    let name = item.name.clone();
    let name_for_aria = name.clone();
    let item_for_click = item.clone();
    let item_for_key = item.clone();
    let item_for_select_key = item.clone();
    let item_for_meta = item.clone();
    let item_for_title = item.clone();
    let item_for_cannot_open = item.clone();
    let item_for_preview = item.clone();
    let item_id_for_class = item.id.clone();
    let item_id_for_aria = item.id.clone();
    let preview_available = RwSignal::new(initial_preview_available(&item));
    let class_item = item.clone();
    let on_open_click = on_open.clone();
    let on_open_key = on_open;
    let select_control = if selectable {
        view! { <SelectionCheckbox id=item.id.clone() name=item.name.clone() selection=selection /> }.into_any()
    } else {
        ().into_any()
    };

    view! {
        <article
            class=move || tile_class(&class_item, preview_available.get())
            data-selection-ids=selectable.then(|| serde_json::to_string(&vec![item.id.clone()]).unwrap_or_default())
            class:selected=move || selected_ids.get().contains(&item_id_for_class)
            role="button"
            tabindex="0"
            aria-label=move || format!("{}，{}", name_for_aria, if selected_ids.get().contains(&item_id_for_aria) { "已选择" } else { "未选择" })
            on:click=move |_| {
                if selectable && selection.enabled.get_untracked() {
                    selection.toggle(&item_for_click.id);
                } else if !trash_mode.get_untracked() || item_for_click.kind == FileKind::File {
                    on_open_click.run(item_for_click.clone());
                }
            }
            on:keydown=move |event: web_sys::KeyboardEvent| {
                if matches!(event.key().as_str(), "Enter" | " ") {
                    event.prevent_default();
                    if selectable && selection.enabled.get_untracked() {
                        selection.toggle(&item_for_select_key.id);
                    } else if !trash_mode.get_untracked() || item_for_key.kind == FileKind::File {
                        on_open_key.run(item_for_key.clone());
                    }
                }
            }
            on:contextmenu=move |event: web_sys::MouseEvent| event.prevent_default()
        >
            {select_control}
            <FileCard name=name detail=Signal::derive(move || display_meta(&item_for_meta, trash_mode.get()))
                cannot_open=Signal::derive(move || {
                    trash_mode.get() && item_for_cannot_open.kind == FileKind::Directory
                })
                preview_title=Signal::derive(move || preview_title(&item_for_title, trash_mode.get()))
            >
                {file_preview_with_state(&item_for_preview, Some(preview_available))}
                {classify::is_book(&item_for_preview).then(||view! { <BookProgressBar file_id=item_for_preview.id.clone() /> })}
            </FileCard>
        </article>
    }
}

pub(super) fn tile_class(file: &File, preview_available: bool) -> String {
    let mut class = String::from("file-card");
    if file.kind == FileKind::Directory {
        class.push_str(" folder-tile");
    } else if classify::is_editable(file) {
        class.push_str(" document-tile");
    } else if is_epub_file(file) {
        class.push_str(" book-tile");
    } else if classify::is_audio(file) {
        class.push_str(" audio-tile");
    }
    if preview_available {
        class.push_str(" preview-tile");
    } else {
        class.push_str(" fallback-tile");
    }
    if file.status != FileStatus::Ready {
        class.push_str(" mutedrow");
    }
    class
}

pub(super) fn initial_preview_available(file: &File) -> bool {
    classify::is_image(file)
        || classify::is_video(file)
        || classify::is_audio(file)
        || is_epub_file(file)
}

pub(super) fn is_epub_file(file: &File) -> bool {
    file.kind == FileKind::File && classify::is_epub_name(&file.name)
}

pub(super) fn display_meta(file: &File, trash_mode: bool) -> String {
    if file.kind == FileKind::Directory {
        if trash_mode {
            return format!(
                "文件夹 · 删除于 {}",
                format_file_date(file.deleted_at.unwrap_or(file.updated_at))
            );
        }
        return "文件夹".to_owned();
    }
    let size = format_size(non_negative(file.size));
    if trash_mode {
        format!(
            "{size} · 删除于 {}",
            format_file_date(file.deleted_at.unwrap_or(file.updated_at))
        )
    } else {
        // FileGrid.vue's card fallback only renders the formatted size. The
        // reference deliberately keeps pending/failed lifecycle details in
        // the muted visual state instead of adding status text to the card.
        size
    }
}

pub(super) fn format_file_date(value: Timestamp) -> String {
    if value.is_missing() {
        "—".to_owned()
    } else {
        format_date(&value.to_rfc3339())
    }
}

pub(super) fn preview_title(file: &File, trash_mode: bool) -> String {
    if trash_mode && file.kind == FileKind::Directory {
        "恢复后可打开文件夹".to_owned()
    } else if classify::is_book(file) {
        "阅读".to_owned()
    } else if trash_mode && classify::is_editable(file) {
        "只读查看".to_owned()
    } else if file.kind == FileKind::Directory {
        "打开文件夹".to_owned()
    } else if classify::is_editable(file) {
        "编辑文档".to_owned()
    } else if classify::is_image(file) {
        "预览图片".to_owned()
    } else if classify::is_video(file) {
        "播放视频".to_owned()
    } else if classify::is_audio(file) {
        "播放音频".to_owned()
    } else {
        "文件".to_owned()
    }
}

pub(super) fn non_negative(value: i64) -> u64 {
    u64::try_from(value).unwrap_or(0)
}

pub(super) fn is_markdown_name(name: &str) -> bool {
    let extension = classify::extension(name);
    extension.eq_ignore_ascii_case("md") || extension.eq_ignore_ascii_case("markdown")
}

pub(super) fn file_preview_with_state(
    file: &File,
    preview_available: Option<RwSignal<bool>>,
) -> AnyView {
    view! { <FilePreview file=file.clone() preview_available=preview_available /> }.into_any()
}

/// Card/row thumbnails follow the old two-step fallback policy: images try a
/// generated thumbnail and then the original preview, while EPUB/audio covers
/// fall back directly to their type icon when the thumbnail is unavailable.
#[component]
pub(super) fn FilePreview(file: File, preview_available: Option<RwSignal<bool>>) -> impl IntoView {
    let is_image = classify::is_image(&file);
    let is_epub = is_epub_file(&file);
    // Artwork is generated on demand, so an unprobed file's has_cover=false
    // must not prevent the first thumbnail request.
    let is_audio = classify::is_audio(&file);
    let is_video = classify::is_video(&file);
    let thumbnail = thumbnail_url(&file);
    let preview = format!("/api/files/{}/preview", file.id);
    let preview_for_src = preview.clone();
    let thumbnail_for_src = thumbnail.clone();
    let fallback_to_preview = RwSignal::new(false);
    let broken = RwSignal::new(false);
    let preview_available_for_error = preview_available;
    let file_for_error = file.clone();
    let on_image_error = move |_| {
        if is_epub || is_audio || fallback_to_preview.get_untracked() {
            broken.set(true);
            if let Some(preview_available) = preview_available_for_error {
                preview_available.set(false);
            }
        } else {
            fallback_to_preview.set(true);
        }
    };

    view! {
        {move || {
            if is_video {
                view! { <VideoThumbnail file=file.clone() /> }.into_any()
            } else if (is_image || is_epub || is_audio) && !broken.get() {
                let file = file_for_error.clone();
                let preview = preview_for_src.clone();
                let thumbnail = thumbnail_for_src.clone();
                view! {
                    <img
                        class="ui-image"
                        src=move || {
                            if fallback_to_preview.get() {
                                preview.clone()
                            } else {
                                thumbnail.clone()
                            }
                        }
                        alt=file.name.clone()
                        loading="lazy"
                        draggable="false"
                        on:error=on_image_error
                    />
                }.into_any()
            } else {
                file_icon(&file)
            }
        }}
    }
}

#[component]
pub(super) fn VideoThumbnail(file: File) -> impl IntoView {
    const RETRY_DELAYS: [i32; 5] = [800, 1_600, 3_200, 6_400, 12_800];
    let attempt = RwSignal::new(0_usize);
    let loaded = RwSignal::new(false);
    let failed = RwSignal::new(false);
    let timer = RwSignal::new(None::<i32>);
    let id = StoredValue::new(file.id.clone());
    let etag = StoredValue::new(
        js_sys::encode_uri_component(&file.etag)
            .as_string()
            .unwrap_or_default(),
    );
    let on_error = move |_| {
        let current = attempt.get_untracked();
        if current >= RETRY_DELAYS.len() {
            failed.set(true);
            return;
        }
        if let Some(window) = web_sys::window() {
            if let Some(old) = timer.get_untracked() {
                window.clear_timeout_with_handle(old);
            }
            let callback = Closure::once_into_js(move || attempt.update(|value| *value += 1));
            if let Ok(id) = window.set_timeout_with_callback_and_timeout_and_arguments_0(
                callback.unchecked_ref(),
                RETRY_DELAYS[current],
            ) {
                timer.set(Some(id));
            }
        }
    };
    on_cleanup(move || {
        if let Some(timer) = timer.get_untracked()
            && let Some(window) = web_sys::window()
        {
            window.clear_timeout_with_handle(timer);
        }
    });

    view! {
        <div class="video-thumb">
            <span class="thumb-fallback" class:hidden=move || loaded.get() && !failed.get()>
                <span class="large-video" aria-hidden="true">
                    <svg viewBox="0 0 24 24"><path d="m9 7 8 5-8 5Z"></path></svg>
                </span>
            </span>
            <Show when=move || !failed.get() fallback=|| ()>
                <img
                    class="ui-image"
                    src=move || {
                        format!(
                            "/api/files/{}/thumbnail?v={}&retry={}",
                            id.get_value(),
                            etag.get_value(),
                            attempt.get()
                        )
                    }
                    alt=""
                    loading="lazy"
                    draggable="false"
                    on:load=move |_| loaded.set(true)
                    on:error=on_error
                />
            </Show>
        </div>
    }
}

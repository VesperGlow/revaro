//! Compact music rows backed by the persistent player and shared library actions.

use super::super::music_player::display_title;
use super::*;

#[component]
pub(super) fn SongRow(
    item: LibraryItem,
    index: usize,
    on_open: Callback<File>,
    on_collect: Callback<File>,
    on_favorite: Callback<(File, bool)>,
    busy: Signal<bool>,
) -> impl IntoView {
    let ShellContext {
        music, selection, ..
    } = expect_context::<ShellContext>();
    let file = item.file;
    let id = StoredValue::new(file.id.clone());
    let name = StoredValue::new(file.name.clone());
    let is_current = Signal::derive(move || {
        music
            .current()
            .is_some_and(|file| file.id == id.get_value())
    });
    let duration = RwSignal::new(
        item.duration_ms
            .filter(|value| *value > 0)
            .map(|value| value as f64 / 1000.0),
    );
    let cover = RwSignal::new(
        file.has_cover
            .then(|| super::super::resource_url::thumbnail_url(&file)),
    );
    let cover_failed = RwSignal::new(false);
    if duration.get_untracked().is_none() {
        let file_id = file.id.clone();
        leptos::task::spawn_local_scoped_with_cancellation(async move {
            if let Ok(metadata) = api::fetch_audio_media(&file_id).await {
                if metadata.duration.is_finite() && metadata.duration > 0.0 {
                    duration.set(Some(metadata.duration));
                }
                if metadata.has_cover && !metadata.cover_url.is_empty() {
                    cover.set(Some(metadata.cover_url));
                }
            }
        });
    }
    let play_file = file.clone();
    let open_file = file.clone();
    let favorite_file = file.clone();
    let collect_file = file.clone();
    let download_file = file.clone();
    let activate = Callback::new(move |()| {
        if is_current.get_untracked() {
            music.toggle();
        } else {
            on_open.run(play_file.clone());
        }
    });
    let detail = format!(
        "{} · 本地音乐",
        revaro_core::classify::extension(&file.name).to_uppercase()
    );

    view! {
        <article class="library-card song-row"
            data-file-id=file.id.clone()
            data-selection-ids=serde_json::to_string(&vec![file.id.clone()]).unwrap_or_default()
            class:selected=move || selection.ids.with(|ids| ids.contains(&id.get_value()))
            class:is-playing=move || is_current.get()
            on:click=move |event| selection.toggle_from_card_background(event, &id.get_value())>
            <SelectionCheckbox id=file.id.clone() name=file.name.clone() selection=selection />
            <span class="song-number-slot">
                <span class="song-number" aria-hidden="true">{format!("{:02}", index + 1)}</span>
                <button class="song-play" type="button"
                    disabled=move || selection.enabled.get()
                    aria-label=move || format!("{} {}", if is_current.get() && music.playing.get() { "暂停" } else { "播放" }, name.get_value())
                    on:click=move |_| activate.run(())>
                    {move || if is_current.get() && music.playing.get() { icons::pause().into_any() } else { icons::play().into_any() }}
                </button>
            </span>
            <button class="library-card-open" type="button" aria-label=format!("打开 {}", file.name)
                on:click=move |_| on_open.run(open_file.clone())>
                <FileCard name=display_title(&file.name) detail=Signal::derive(move || detail.clone())>
                    <div class="library-cover audio-cover song-cover">
                        <Show when=move || cover.get().is_some() && !cover_failed.get()
                            fallback=|| view! { <span class="song-cover-fallback" aria-hidden="true">{icons::music_2()}</span> }>
                            <img src=move || cover.get().unwrap_or_default() alt="" loading="lazy"
                                draggable="false" on:error=move |_| cover_failed.set(true) />
                        </Show>
                    </div>
                </FileCard>
            </button>
            <span class="song-duration" aria-label="曲目时长">
                {move || duration.get().map(format_media_time).unwrap_or_else(|| "—:—".to_owned())}
            </span>
            <ActionMenu label=format!("曲目更多操作：{}", file.name) icon=MenuIcon::More
                disabled=Signal::derive(move || busy.get() || selection.enabled.get())
                panel_class="song-menu-panel".to_owned()>
                <button type="button" data-close-menu="true" on:click=move |_| activate.run(())>
                    {icons::play()}<span>{move || if is_current.get() && music.playing.get() { "暂停播放" } else { "播放曲目" }}</span>
                </button>
                <button type="button" data-close-menu="true" on:click=move |_| on_favorite.run((favorite_file.clone(), !item.favorite))>
                    {icons::heart()}<span>{if item.favorite { "取消收藏" } else { "收藏曲目" }}</span>
                </button>
                <button type="button" data-close-menu="true" on:click=move |_| on_collect.run(collect_file.clone())>
                    {icons::plus()}"加入歌单"
                </button>
                <button type="button" data-close-menu="true" on:click=move |_| super::super::file_browser::download_file(&download_file)>
                    {icons::download()}"下载原文件"
                </button>
            </ActionMenu>
        </article>
    }
}

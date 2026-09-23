//! Chapter aware audio playback.
//!
//! The browser owns decoding and buffering through `HTMLAudioElement`. This
//! component adds the product behaviour around it: chapter metadata, resume
//! positions, a mobile chapter sheet, keyboard seeking and conservative
//! progress writes to the server.

use leptos::ev::{Event, PointerEvent};
use leptos::prelude::*;
use revaro_core::media::{AudioChapter, AudioMedia};
use revaro_core::model::{File, MediaProgress};
use wasm_bindgen::JsCast;
use wasm_bindgen::closure::Closure;
use web_sys::{
    HtmlAudioElement, HtmlInputElement, HtmlMediaElement, HtmlSelectElement, KeyboardEvent,
};

use crate::api;
use crate::browser;
use crate::logic::format::format_media_time;
use crate::logic::media::{active_chapter_index, clamp_percent};

use super::icons;
use super::media::{MenuIcon, PreviewMenu};

/// Full-screen audio player mounted inside [`super::media::MediaPreview`].
#[component]
pub fn AudioPlayer(item: File) -> impl IntoView {
    let player = NodeRef::<leptos::html::Div>::new();
    let audio = NodeRef::<leptos::html::Audio>::new();
    let media = RwSignal::new(None::<AudioMedia>);
    let panel_open = RwSignal::new(false);
    let cover_failed = RwSignal::new(false);
    let loading = RwSignal::new(true);
    let waiting = RwSignal::new(false);
    let playing = RwSignal::new(false);
    let current_time = RwSignal::new(0.0_f64);
    let native_duration = RwSignal::new(0.0_f64);
    let buffered = RwSignal::new(0.0_f64);
    let rate = RwSignal::new(1.0_f64);
    let volume = RwSignal::new(audio_volume());
    let muted = RwSignal::new(audio_muted());
    let error = RwSignal::new(String::new());
    let seek_preview = RwSignal::new(None::<f64>);
    let seek_hover = RwSignal::new(None::<SeekHover>);
    let server_position = RwSignal::new(0.0_f64);
    let progress_loaded = RwSignal::new(false);
    let restored_position = RwSignal::new(false);
    let save_timer = RwSignal::new(None::<i32>);
    let remote_save_timer = RwSignal::new(None::<i32>);
    let mounted = RwSignal::new(false);

    let source = format!("/api/files/{}/preview", item.id);
    let item_name = item.name.clone();
    let title = stem(&item.name);
    let position_key = format!("revaro-audio-position:{}", item.id);
    let item_id = item.id.clone();

    let duration = move || audio_duration(media.get(), native_duration.get());
    let chapters_factory = StoredValue::new({
        let item_name = item_name.clone();
        move || {
            media.get().map_or_else(
                || {
                    vec![AudioChapter {
                        id: 1,
                        title: stem(&item_name),
                        start: 0.0,
                        end: duration(),
                    }]
                },
                |value| {
                    if value.chapters.is_empty() {
                        vec![AudioChapter {
                            id: 1,
                            title: stem(&item_name),
                            start: 0.0,
                            end: duration(),
                        }]
                    } else {
                        value.chapters
                    }
                },
            )
        }
    });
    let chapters = move || chapters_factory.with_value(|factory| factory());
    let current_chapter_index_factory =
        StoredValue::new(move || active_chapter_index(&chapters(), current_time.get()));
    let current_chapter_index =
        move || current_chapter_index_factory.with_value(|factory| factory());
    let displayed_time = move || seek_preview.get().unwrap_or_else(|| current_time.get());
    let progress = move || {
        let total = duration();
        if total > 0.0 {
            clamp_percent(displayed_time() / total * 100.0)
        } else {
            0.0
        }
    };

    let restore_position = {
        let position_key = position_key.clone();
        move || {
            if !progress_loaded.get_untracked()
                || restored_position.get_untracked()
                || duration() <= 0.0
            {
                return;
            }
            restored_position.set(true);
            let saved = if server_position.get_untracked() > 0.0 {
                server_position.get_untracked()
            } else {
                browser::local_storage_get(&position_key)
                    .and_then(|value| value.parse::<f64>().ok())
                    .filter(|value| value.is_finite() && *value > 0.0)
                    .unwrap_or(0.0)
            };
            if saved > 0.0 && saved < duration() - 5.0 {
                seek_audio(audio, current_time, duration(), saved, false);
            }
        }
    };

    let save_progress = {
        let position_key = position_key.clone();
        let item_id = item_id.clone();
        move |remote: bool| {
            let position = current_time.get_untracked().max(0.0);
            if position <= 0.0 {
                return;
            }
            browser::local_storage_set(&position_key, &position.floor().to_string());
            if remote {
                let id = item_id.clone();
                let progress = MediaProgress {
                    position,
                    duration: duration(),
                    updated_at: None,
                };
                leptos::task::spawn_local(async move {
                    let _ = api::save_media_progress(&id, &progress).await;
                });
            }
        }
    };

    let schedule_local_save = {
        let save_progress = save_progress.clone();
        move || {
            clear_timer(save_timer);
            if let Some(window) = web_sys::window() {
                let callback = Closure::once_into_js({
                    let save_progress = save_progress.clone();
                    move || {
                        save_timer.set(None);
                        save_progress(false);
                    }
                });
                if let Ok(id) = window.set_timeout_with_callback_and_timeout_and_arguments_0(
                    callback.unchecked_ref(),
                    500,
                ) {
                    save_timer.set(Some(id));
                }
            }
        }
    };

    let schedule_remote_save = {
        let save_progress = save_progress.clone();
        move || {
            if remote_save_timer.get_untracked().is_some() {
                return;
            }
            if let Some(window) = web_sys::window() {
                let callback = Closure::once_into_js({
                    let save_progress = save_progress.clone();
                    move || {
                        remote_save_timer.set(None);
                        save_progress(true);
                    }
                });
                if let Ok(id) = window.set_timeout_with_callback_and_timeout_and_arguments_0(
                    callback.unchecked_ref(),
                    5_000,
                ) {
                    remote_save_timer.set(Some(id));
                }
            }
        }
    };

    let on_loaded_metadata = {
        let restore_position = restore_position.clone();
        move |_| {
            if let Some(element) = audio_element(audio) {
                native_duration.set(safe_duration(element.duration()));
                element.set_volume(volume.get_untracked());
                element.set_muted(muted.get_untracked());
                element.set_playback_rate(rate.get_untracked());
                loading.set(false);
                waiting.set(false);
            }
            update_buffer(audio, duration, buffered);
            restore_position();
        }
    };
    let on_time_update = {
        let schedule_local_save = schedule_local_save.clone();
        let schedule_remote_save = schedule_remote_save.clone();
        move |_| {
            if let Some(element) = audio_element(audio) {
                current_time.set(safe_time(element.current_time()));
            }
            update_buffer(audio, duration, buffered);
            schedule_local_save();
            schedule_remote_save();
        }
    };
    let on_pause = {
        let save_progress = save_progress.clone();
        move |_| {
            playing.set(false);
            clear_timer(remote_save_timer);
            save_progress(true);
        }
    };
    let on_play = move |_| playing.set(true);
    let on_waiting = move |_| waiting.set(true);
    let on_can_play = move |_| {
        loading.set(false);
        waiting.set(false);
    };
    let on_ended = {
        let on_pause = on_pause.clone();
        move |event: Event| on_pause(event)
    };
    let on_error = move |_| {
        loading.set(false);
        waiting.set(false);
        error.set("浏览器无法播放此原始格式，请下载后使用本地播放器打开".to_owned());
    };

    let toggle_playback = move || {
        let Some(element) = audio_element(audio) else {
            return;
        };
        if element.paused() {
            match element.play() {
                Ok(promise) => {
                    leptos::task::spawn_local(async move {
                        if wasm_bindgen_futures::JsFuture::from(promise).await.is_err() {
                            error.set("浏览器无法开始播放，请重试".to_owned());
                        }
                    });
                }
                Err(_) => error.set("浏览器无法开始播放，请重试".to_owned()),
            }
        } else {
            let _ = element.pause();
        }
    };
    let seek = {
        let restore_position = restore_position.clone();
        move |target: f64, play: bool| {
            seek_audio(audio, current_time, duration(), target, play);
            restore_position();
        }
    };
    let previous_chapter = {
        let seek = seek.clone();
        move || {
            let list = chapters();
            let index = current_chapter_index();
            let Some(chapter) = list.get(index) else {
                seek(0.0, false);
                return;
            };
            if current_time.get_untracked() - chapter.start > 3.0 {
                seek(chapter.start, false);
            } else {
                seek(
                    list.get(index.saturating_sub(1))
                        .map_or(0.0, |value| value.start),
                    false,
                );
            }
        }
    };
    let next_chapter = {
        let seek = seek.clone();
        move || {
            if let Some(chapter) = chapters().get(current_chapter_index() + 1) {
                seek(chapter.start, true);
            }
        }
    };
    let seek_callback = StoredValue::new(seek.clone());
    let previous_chapter_callback = StoredValue::new(previous_chapter);
    let _next_chapter_callback = StoredValue::new(next_chapter);

    let on_key = {
        let toggle_playback = toggle_playback.clone();
        let seek = seek.clone();
        let player = player;
        let close_panel = {
            let panel_open = panel_open;
            move || {
                panel_open.set(false);
                if let Some(element) = player.get() {
                    let _ = element
                        .unchecked_into::<web_sys::Element>()
                        .query_selector("[data-panel-trigger=\"chapters\"]")
                        .ok()
                        .flatten()
                        .and_then(|element| element.dyn_into::<web_sys::HtmlElement>().ok())
                        .map(|element| element.focus());
                }
            }
        };
        move |event: KeyboardEvent| {
            if event.default_prevented() {
                return;
            }
            if event.key() == "Escape" && panel_open.get_untracked() {
                event.prevent_default();
                event.stop_propagation();
                close_panel();
                return;
            }
            if event
                .target()
                .and_then(|target| target.dyn_into::<web_sys::Element>().ok())
                .and_then(|target| target.closest("button, input, select, summary").ok())
                .flatten()
                .is_some()
            {
                return;
            }
            match event.key().as_str() {
                " " => {
                    event.prevent_default();
                    toggle_playback();
                }
                "ArrowLeft" => {
                    event.prevent_default();
                    seek(current_time.get_untracked() - 15.0, false);
                }
                "ArrowRight" => {
                    event.prevent_default();
                    seek(current_time.get_untracked() + 30.0, false);
                }
                _ => {}
            }
        }
    };

    let open_panel = move |_| {
        panel_open.set(true);
        // The reference waits for the sheet to mount before moving focus to
        // its close button. A synchronous query races Leptos DOM insertion
        // and leaves keyboard users on the trigger.
        if let Some(window) = web_sys::window() {
            let callback = Closure::once_into_js(move || {
                if let Some(element) = player.get() {
                    let _ = element
                        .unchecked_into::<web_sys::Element>()
                        .query_selector(".audio-panel .media-icon-button")
                        .ok()
                        .flatten()
                        .and_then(|element| element.dyn_into::<web_sys::HtmlElement>().ok())
                        .map(|element| element.focus());
                }
            });
            let _ = window
                .set_timeout_with_callback_and_timeout_and_arguments_0(callback.unchecked_ref(), 0);
        }
    };
    let close_panel = move |_| {
        panel_open.set(false);
        if let Some(element) = player.get() {
            let _ = element
                .unchecked_into::<web_sys::Element>()
                .query_selector("[data-panel-trigger=\"chapters\"]")
                .ok()
                .flatten()
                .and_then(|element| element.dyn_into::<web_sys::HtmlElement>().ok())
                .map(|element| element.focus());
        }
    };
    let toggle_panel = move |event: leptos::ev::MouseEvent| {
        if panel_open.get_untracked() {
            close_panel(event);
        } else {
            open_panel(event);
        }
    };
    let revealed_chapter = StoredValue::new(None::<usize>);
    {
        let player = player;
        Effect::new(move |_| {
            let open = panel_open.get();
            let index = current_chapter_index();
            if !open {
                revealed_chapter.set_value(None);
                return;
            }
            if revealed_chapter.get_value() == Some(index) {
                return;
            }
            revealed_chapter.set_value(Some(index));
            let Some(window) = web_sys::window() else {
                return;
            };
            let callback = Closure::once_into_js(move || {
                let Some(player) = player.get() else {
                    return;
                };
                let selector = format!(".audio-panel [data-chapter-index=\"{index}\"]");
                let Ok(Some(element)) = player
                    .unchecked_into::<web_sys::Element>()
                    .query_selector(&selector)
                else {
                    return;
                };
                let options = web_sys::ScrollIntoViewOptions::new();
                options.set_block(web_sys::ScrollLogicalPosition::Nearest);
                element.scroll_into_view_with_scroll_into_view_options(&options);
            });
            let _ = window
                .set_timeout_with_callback_and_timeout_and_arguments_0(callback.unchecked_ref(), 0);
        });
    }
    let set_rate = move |event: Event| {
        let Some(element) = event
            .target()
            .and_then(|target| target.dyn_into::<HtmlSelectElement>().ok())
        else {
            return;
        };
        let value = element.value().parse::<f64>().unwrap_or(1.0);
        rate.set(value);
        if let Some(audio) = audio_element(audio) {
            audio.set_playback_rate(value);
        }
    };
    let set_volume = move |event: Event| {
        let Some(element) = event
            .target()
            .and_then(|target| target.dyn_into::<HtmlInputElement>().ok())
        else {
            return;
        };
        let value = element
            .value()
            .parse::<f64>()
            .unwrap_or(0.85)
            .clamp(0.0, 1.0);
        volume.set(value);
        muted.set(value == 0.0);
        browser::local_storage_set("revaro-audio-volume", &value.to_string());
        browser::local_storage_set("revaro-audio-muted", &(value == 0.0).to_string());
        if let Some(audio) = audio_element(audio) {
            audio.set_volume(value);
            audio.set_muted(value == 0.0);
        }
    };
    let toggle_mute = move |_| {
        let next = !muted.get_untracked();
        muted.set(next);
        browser::local_storage_set("revaro-audio-muted", &next.to_string());
        if let Some(audio) = audio_element(audio) {
            audio.set_muted(next);
        }
    };
    let preview_seek = move |event: Event| {
        if let Some(input) = event
            .target()
            .and_then(|target| target.dyn_into::<HtmlInputElement>().ok())
            && let Ok(value) = input.value().parse::<f64>()
        {
            seek_preview.set(Some(value));
        }
    };
    let commit_seek = {
        let seek = seek.clone();
        move |event: Event| {
            let target = event
                .target()
                .and_then(|target| target.dyn_into::<HtmlInputElement>().ok())
                .and_then(|input| input.value().parse::<f64>().ok());
            seek_preview.set(None);
            if let Some(target) = target {
                seek(target, playing.get_untracked());
            }
        }
    };
    let hover_seek = move |event: PointerEvent| {
        let Some(target) = event
            .current_target()
            .and_then(|target| target.dyn_into::<web_sys::Element>().ok())
        else {
            return;
        };
        let bounds = target.get_bounding_client_rect();
        if bounds.width() <= 0.0 {
            return;
        }
        let ratio =
            ((f64::from(event.client_x()) - bounds.left()) / bounds.width()).clamp(0.0, 1.0);
        seek_hover.set(Some(SeekHover {
            percent: ratio * 100.0,
            time: ratio * duration(),
        }));
    };
    let leave_seek = move |_| seek_hover.set(None);
    // Load durable progress and metadata independently. Either may arrive
    // first; `restore_position` only acts after both media duration and the
    // server response are available. These reads belong to the player: unlike
    // durable progress writes, they must stop when its reactive owner is gone.
    {
        let id = item.id.clone();
        let restore_position = restore_position.clone();
        leptos::task::spawn_local_scoped_with_cancellation(async move {
            if let Ok(progress) = api::fetch_media_progress(&id).await {
                server_position.set(safe_time(progress.position));
            }
            progress_loaded.set(true);
            restore_position();
        });
    }
    {
        let id = item.id.clone();
        leptos::task::spawn_local_scoped_with_cancellation(async move {
            if let Ok(value) = api::fetch_audio_media(&id).await {
                media.set(Some(value));
                restore_position();
            }
        });
    }

    // Start playback as soon as the element exists. A rejected autoplay call is
    // harmless and the visible play button remains available for that browser.
    {
        let source = source.clone();
        let player = player;
        Effect::new(move |_| {
            if mounted.get() {
                return;
            }
            let Some(element) = audio_element(audio) else {
                return;
            };
            mounted.set(true);
            element.set_src(&source);
            let _ = element
                .unchecked_ref::<web_sys::Element>()
                .set_attribute("playsinline", "");
            element.set_preload("metadata");
            element.set_autoplay(true);
            element.set_volume(volume.get_untracked());
            element.set_muted(muted.get_untracked());
            if let Some(element) = player.get() {
                let options = web_sys::FocusOptions::new();
                options.set_prevent_scroll(true);
                let _ = element.focus_with_options(&options);
            }
            element.load();
            play_ignoring_rejection(&element);
        });
    }

    let cleanup_save = save_progress.clone();
    let cleanup_item_id = item_id.clone();
    on_cleanup(move || {
        clear_timer(save_timer);
        clear_timer(remote_save_timer);
        cleanup_save(false);
        let position = current_time.get_untracked().max(0.0);
        if position > 0.0 {
            api::save_media_progress_keepalive(
                &cleanup_item_id,
                &MediaProgress {
                    position,
                    duration: audio_duration(
                        media.get_untracked(),
                        native_duration.get_untracked(),
                    ),
                    updated_at: None,
                },
            );
        }
        if let Some(element) = audio_element(audio) {
            let _ = element.pause();
            element.set_src("");
            element.load();
        }
    });

    let book_title = title.clone();
    let heading_title = title.clone();
    let cover_name = item.name.clone();
    view! {
        <div node_ref=player class="chapter-audio-player" class:panel-open=move || panel_open.get() tabindex="0" on:keydown=on_key>
            <main class="audio-main">
                <section class="audio-now-playing">
                    <div class="audio-cover">
                        {move || {
                            let cover = media.get().filter(|value| value.has_cover).map(|value| value.cover_url);
                            if cover.is_some() && !cover_failed.get() {
                                view! { <img src=cover.unwrap_or_default() alt=format!("{} 封面", cover_name) on:error=move |_| cover_failed.set(true) /> }.into_any()
                            } else {
                                view! { {icons::music_2()} }.into_any()
                            }
                        }}
                    </div>
                    <div class="audio-chapter-current">
                        <span>{move || if playing.get() { "正在播放" } else { "暂停中" }}</span>
                        <Show when=move || chapters().len().gt(&1) fallback=|| ()>
                            <p class="audio-book-title">{book_title.clone()}</p>
                        </Show>
                        <h1>{move || chapters().get(current_chapter_index()).map_or_else(|| heading_title.clone(), |chapter| if chapter.title.is_empty() { heading_title.clone() } else { chapter.title.clone() })}</h1>
                        <Show when=move || chapters().len().gt(&1) fallback=|| ()>
                            <small>{move || format!("第 {} / {} 章", current_chapter_index() + 1, chapters().len())}</small>
                        </Show>
                    </div>
                </section>
                <section class="audio-playback" aria-label="音频播放控制">
                    <AudioProgress
                        percent=progress
                        buffered=move || buffered.get()
                        markers=move || chapter_markers(&chapters(), duration())
                        duration=duration
                        current_time=displayed_time
                        hover=seek_hover
                        on_input=preview_seek
                        on_change=commit_seek
                        on_pointer_move=hover_seek
                        on_pointer_leave=leave_seek
                    />
                    <div class="audio-time"><span>{move || format_media_time(displayed_time())}</span><span>{move || format_media_time(duration())}</span></div>
                    <div class="audio-controls">
                        <button type="button" aria-label="后退15秒" title="后退 15 秒" prop:disabled={move || duration() <= 0.0} on:click=move |_| seek_callback.with_value(|seek| seek(current_time.get_untracked() - 15.0, false))>{icons::rotate_ccw()}<small>"15"</small></button>
                        <button class="audio-play" type="button" prop:disabled=move || loading.get() aria-label=move || if playing.get() { "暂停" } else { "播放" } on:click=move |_| toggle_playback()>
                            <Show when=move || loading.get() || waiting.get() fallback=move || if playing.get() { icons::pause().into_any() } else { icons::play().into_any() }>
                                <span class="audio-control-spinner"></span>
                            </Show>
                        </button>
                        <button type="button" aria-label="前进30秒" title="前进 30 秒" prop:disabled={move || duration() <= 0.0} on:click=move |_| seek_callback.with_value(|seek| seek(current_time.get_untracked() + 30.0, false))>{icons::rotate_cw()}<small>"30"</small></button>
                    </div>
                    <div class="audio-options">
                        <label class="audio-rate"><span class="media-sr-only">"播放速度"</span><select aria-label="播放速度" prop:value=move || rate.get().to_string() on:change=set_rate>
                            <option value="0.75">"0.75×"</option><option value="1">"1×"</option><option value="1.25">"1.25×"</option><option value="1.5">"1.5×"</option><option value="2">"2×"</option>
                        </select></label>
                        <button type="button" data-panel-trigger="chapters" aria-expanded=move || if panel_open.get() { "true" } else { "false" } on:click=toggle_panel>{icons::list()}<span>"章节"</span></button>
                        <PreviewMenu label="音量".to_owned() icon=MenuIcon::Volume volume=volume muted=muted>
                            <div class="audio-volume">
                                <button type="button" aria-label=move || if muted.get() { "取消静音" } else { "静音" } on:click=toggle_mute>
                                    {move || if muted.get() { icons::volume_x().into_any() } else { icons::volume_2().into_any() }}
                                </button>
                                <input type="range" min="0" max="1" step="0.01" aria-label="音量" prop:value=move || volume.get().to_string() on:input=set_volume />
                                <output>{move || format!("{}%", if muted.get() { 0 } else { (volume.get() * 100.0).round() as u64 })}</output>
                            </div>
                        </PreviewMenu>
                    </div>
                    <Show when=move || !error.get().is_empty() fallback=|| ()>
                        <p class="audio-player-error" role="alert">{move || error.get()}</p>
                    </Show>
                    <audio node_ref=audio src=source.clone() autoplay preload="metadata" on:loadedmetadata=on_loaded_metadata on:timeupdate=on_time_update on:progress=move |_| update_buffer(audio, duration, buffered) on:play=on_play on:pause=on_pause on:ended=on_ended on:waiting=on_waiting on:canplay=on_can_play on:error=on_error></audio>
                </section>
            </main>
            <Show when=move || panel_open.get() fallback=|| ()>
                <button class="audio-panel-scrim" type="button" aria-label="收起面板" on:click=close_panel></button>
                <aside class="audio-panel" data-preview-sheet aria-label="音频章节">
                    <header><div class="audio-panel-tabs"><span>"章节"</span></div><button class="media-icon-button" type="button" aria-label="收起面板" on:click=close_panel>{icons::x()}</button></header>
                    <div class="audio-chapter-navigation">
                        <button type="button" prop:disabled={move || duration() <= 0.0} on:click=move |_| previous_chapter_callback.with_value(|callback| callback())>{icons::skip_back()}<span>"上一章"</span></button>
                        <button type="button" prop:disabled={move || current_chapter_index() >= chapters().len().saturating_sub(1)} on:click=move |_| _next_chapter_callback.with_value(|callback| callback())><span>"下一章"</span>{icons::skip_forward()}</button>
                    </div>
                    <div class="audio-chapter-list">
                        <For each=move || indexed_chapters(chapters()) key=|(index, _)| *index let:entry>
                            <button type="button" data-chapter-index=entry.0.to_string() aria-current=move || if entry.0 == current_chapter_index() { Some("true") } else { None } on:click={
                                let seek_callback = seek_callback;
                                let chapter = entry.1.clone();
                                move |_| seek_callback.with_value(|seek| seek(chapter.start, true))
                            }>
                                <span class="audio-chapter-number">{format!("{:02}", entry.0 + 1)}</span>
                                <strong>{entry.1.title.clone()}</strong>
                                <small>{format_media_time(entry.1.start)}</small>
                            </button>
                        </For>
                    </div>
                </aside>
            </Show>
        </div>
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
struct SeekHover {
    percent: f64,
    time: f64,
}

#[component]
fn AudioProgress(
    percent: impl Fn() -> f64 + Copy + Send + 'static,
    buffered: impl Fn() -> f64 + Copy + Send + 'static,
    markers: impl Fn() -> Vec<f64> + Send + 'static,
    duration: impl Fn() -> f64 + Copy + Send + 'static,
    current_time: impl Fn() -> f64 + Copy + Send + 'static,
    hover: RwSignal<Option<SeekHover>>,
    on_input: impl Fn(Event) + Send + 'static,
    on_change: impl Fn(Event) + Send + 'static,
    on_pointer_move: impl Fn(PointerEvent) + Send + 'static,
    on_pointer_leave: impl Fn(PointerEvent) + Send + 'static,
) -> impl IntoView {
    // This component only groups the layered track. The input remains the
    // focusable/control authority, while the visible bars are inert decoration.
    view! {
        <div class="full-bleed-progress full-bleed-progress--audio">
            <div class="full-bleed-progress__track" aria-hidden="true">
                <span class="full-bleed-progress__buffer" style=move || format!("width:{}%;", clamp_percent(buffered()))></span>
                <span class="full-bleed-progress__played" style=move || format!("width:{}%;", clamp_percent(percent()))></span>
                <For each=move || markers() key=|marker| marker.to_bits() let:marker>
                    <i style=move || format!("left:{}%;", clamp_percent(marker))></i>
                </For>
            </div>
            <div class="full-bleed-progress__runway" aria-hidden="true">
                <span class="full-bleed-progress__thumb" style=move || format!("left:{}%;", clamp_percent(percent()))></span>
                <Show when=move || hover.get().is_some() fallback=|| ()>
                    <output class="full-bleed-progress__tooltip" style=move || {
                        let value = hover.get().unwrap_or(SeekHover { percent: 0.0, time: 0.0 });
                        format!("left:clamp(28px, {}%, calc(100% - 28px));", clamp_percent(value.percent))
                    }>{move || format_media_time(hover.get().map_or(0.0, |value| value.time))}</output>
                </Show>
            </div>
            <input
                class="full-bleed-progress__input"
                type="range"
                min="0"
                max=move || duration().max(0.0).to_string()
                step="0.1"
                prop:value=move || current_time().clamp(0.0, duration().max(0.0)).to_string()
                prop:disabled={move || duration() <= 0.0}
                aria-label="播放进度"
                on:input=on_input
                on:change=on_change
                on:pointermove=on_pointer_move
                on:pointerleave=on_pointer_leave
            />
        </div>
    }
}

fn audio_element(node: NodeRef<leptos::html::Audio>) -> Option<HtmlMediaElement> {
    node.get().map(|element| {
        element
            .unchecked_into::<HtmlAudioElement>()
            .unchecked_into()
    })
}

fn audio_duration(media: Option<AudioMedia>, native: f64) -> f64 {
    media
        .and_then(|value| {
            (value.duration.is_finite() && value.duration > 0.0).then_some(value.duration)
        })
        .unwrap_or(native.max(0.0))
}

fn indexed_chapters(chapters: Vec<AudioChapter>) -> Vec<(usize, AudioChapter)> {
    chapters.into_iter().enumerate().collect()
}

fn safe_duration(value: f64) -> f64 {
    if value.is_finite() && value > 0.0 {
        value
    } else {
        0.0
    }
}

fn safe_time(value: f64) -> f64 {
    if value.is_finite() && value >= 0.0 {
        value
    } else {
        0.0
    }
}

fn seek_audio(
    audio: NodeRef<leptos::html::Audio>,
    current_time: RwSignal<f64>,
    duration: f64,
    target: f64,
    play: bool,
) {
    if !target.is_finite() {
        return;
    }
    let target = target.max(0.0).min(if duration > 0.0 {
        duration
    } else {
        target.max(0.0)
    });
    if let Some(element) = audio_element(audio) {
        element.set_current_time(target);
        current_time.set(safe_time(element.current_time()));
        if play {
            play_ignoring_rejection(&element);
        }
    } else {
        current_time.set(target);
    }
}

fn play_ignoring_rejection(element: &HtmlMediaElement) {
    if let Ok(promise) = element.play() {
        wasm_bindgen_futures::spawn_local(async move {
            let _ = wasm_bindgen_futures::JsFuture::from(promise).await;
        });
    }
}

fn update_buffer(
    audio: NodeRef<leptos::html::Audio>,
    duration: impl Fn() -> f64,
    buffered: RwSignal<f64>,
) {
    let Some(element) = audio_element(audio) else {
        return;
    };
    let total = duration();
    let ranges = element.buffered();
    if total <= 0.0 || ranges.length() == 0 {
        buffered.set(0.0);
        return;
    }
    let end = ranges
        .end(ranges.length().saturating_sub(1))
        .ok()
        .filter(|value| value.is_finite())
        .unwrap_or(0.0);
    buffered.set(clamp_percent(end / total * 100.0));
}

fn chapter_markers(chapters: &[AudioChapter], duration: f64) -> Vec<f64> {
    if duration <= 0.0 {
        return Vec::new();
    }
    chapters
        .iter()
        .skip(1)
        .map(|chapter| clamp_percent(chapter.start / duration * 100.0))
        .collect()
}

fn stem(name: &str) -> String {
    name.rsplit_once('.')
        .map_or_else(|| name.to_owned(), |(stem, _)| stem.to_owned())
}

fn audio_volume() -> f64 {
    browser::local_storage_get("revaro-audio-volume")
        .and_then(|value| value.parse::<f64>().ok())
        .filter(|value| value.is_finite())
        .map_or(0.85, |value| value.clamp(0.0, 1.0))
}

fn audio_muted() -> bool {
    browser::local_storage_get("revaro-audio-muted").as_deref() == Some("true")
}

fn clear_timer(signal: RwSignal<Option<i32>>) {
    if let Some(timer) = signal.get_untracked()
        && let Some(window) = web_sys::window()
    {
        window.clear_timeout_with_handle(timer);
    }
    signal.set(None);
}

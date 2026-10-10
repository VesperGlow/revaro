//! Chapter aware audio playback.
//!
//! The browser owns decoding and buffering through `HTMLAudioElement`. This
//! component adds the product behaviour around it: chapter metadata, resume
//! positions, a mobile chapter sheet, keyboard seeking and conservative
//! progress writes to the server.

use leptos::ev::{Event, PointerEvent};
use leptos::prelude::*;
use revaro_core::media::{AudioChapter, AudioMedia};
use revaro_core::model::File;
use wasm_bindgen::JsCast;
use wasm_bindgen::closure::Closure;
use web_sys::{
    HtmlAudioElement, HtmlInputElement, HtmlMediaElement, HtmlSelectElement, KeyboardEvent,
};

use crate::api;
use crate::browser;
use crate::logic::format::format_media_time;
use crate::logic::media::{
    active_chapter_index, audio_duration, clamp_percent, media_element_time, stable_media_clock,
    timeline_chapters,
};

use super::audio_timeline::AudioSeekInput;
use super::icons;
use super::menu::{ActionMenu, MenuIcon};
use super::music_player::MusicController;
use super::playback::{
    PlaybackProgress, ProgressDestination, clear_timer, debounce, persist_progress, stored_volume,
    throttle,
};

/// A small lyric window leaves the detailed player's transport controls in place.
#[component]
pub(super) fn AudioLyricPreview(
    cues: Signal<Vec<revaro_core::media::VttCue>>,
    current_time: Signal<f64>,
) -> impl IntoView {
    let cues = Memo::new(move |_| cues.get());
    let lines = Memo::new(move |_| {
        cues.with(|cues| crate::logic::media::subtitle_context(cues, current_time.get()))
    });
    view! {
        <section class="dock-lyrics" aria-label="歌词与字幕">
            <Show when=move ||!cues.with(Vec::is_empty) fallback=||view! {<p class="dock-lyrics-empty">"暂无歌词/字幕"</p>}>
                <p class="dock-lyric-neighbor" aria-hidden="true">{move ||lines.with(|lines|lines.previous.clone())}</p>
                <div class="audio-subtitles" aria-label="音频字幕">{move ||lines.with(|lines|lines.current.clone())}</div>
                <p class="dock-lyric-neighbor" aria-hidden="true">{move ||lines.with(|lines|lines.next.clone())}</p>
            </Show>
        </section>
    }
}

/// The full player follows the subtitle clock without scrolling its controls.
#[component]
fn AudioTranscript(
    cues: Signal<Vec<revaro_core::media::VttCue>>,
    current_time: Signal<f64>,
    layout_changed: Signal<bool>,
    on_seek: Callback<f64>,
) -> impl IntoView {
    let viewport = NodeRef::<leptos::html::Div>::new();
    let cues = Memo::new(move |_| cues.get());
    let active = Memo::new(move |_| {
        cues.with(|cues| crate::logic::media::active_subtitle_indices(cues, current_time.get()))
    });
    let cursor = Memo::new(move |_| {
        if let Some(index) = active.with(|indices| indices.last().copied()) {
            return Some(index);
        }
        let time = media_element_time(current_time.get());
        cues.with(|cues| {
            if cues.is_empty() {
                None
            } else {
                Some(
                    cues.partition_point(|cue| cue.start <= time)
                        .saturating_sub(1),
                )
            }
        })
    });
    let following = RwSignal::new(true);
    let scroll_timer = RwSignal::new(None::<i32>);
    let reveal = Callback::new(move |()| {
        if !following.get_untracked() {
            return;
        }
        debounce(scroll_timer, 0, move || {
            let Some(viewport) = viewport.get() else {
                return;
            };
            let Some(index) = cursor.get_untracked() else {
                return;
            };
            let viewport: web_sys::HtmlElement = viewport.unchecked_into();
            let _ = viewport.style().set_property(
                "--transcript-inset",
                &format!(
                    "{}px",
                    (f64::from(viewport.client_height()) / 2.0 - 24.0).max(0.0)
                ),
            );
            let Ok(Some(line)) = viewport.query_selector(&format!("[data-cue-index=\"{index}\"]"))
            else {
                return;
            };
            let bounds = line.get_bounding_client_rect();
            let top = f64::from(viewport.scroll_top()) + bounds.top()
                - viewport.get_bounding_client_rect().top()
                - (f64::from(viewport.client_height()) - bounds.height()) / 2.0;
            viewport.scroll_to_with_x_and_y(0.0, top.max(0.0));
        });
    });
    Effect::new(move |_| {
        let _ = viewport.get();
        let _ = cursor.get();
        let _ = layout_changed.get();
        if following.get() {
            reveal.run(());
        } else {
            clear_timer(scroll_timer);
        }
    });
    let mut resize = browser::on_resize(move |_| reveal.run(()));
    on_cleanup(move || {
        resize.release();
        clear_timer(scroll_timer);
    });
    view! {
        <section class="audio-transcript" aria-label="滚动台词">
            <div node_ref=viewport class="audio-transcript-scroll" class:empty=move ||cues.with(|cues|cues.is_empty()) tabindex="0" aria-label="台词内容"
                on:wheel=move |_|following.set(false)
                on:pointerdown=move |_|following.set(false)
                on:keydown=move |event: KeyboardEvent| {
                    if matches!(event.key().as_str(), "ArrowUp" | "ArrowDown" | "PageUp" | "PageDown" | "Home" | "End") {
                        following.set(false);
                        event.stop_propagation();
                    }
                }>
                <Show when=move ||cues.with(|cues|cues.is_empty()) fallback=|| ()>
                    <p class="audio-transcript-empty">"暂无台词"</p>
                </Show>
                <div class="audio-transcript-lines">
                    <For each=move ||{cues.get().into_iter().enumerate().collect::<Vec<_>>()} key=|(index,_)|*index children=move |(index,cue)|view! {
                        <button type="button" class="audio-transcript-line" data-cue-index=index
                            class:active=move ||active.with(|indices|indices.contains(&index))
                            aria-current=move ||active.with(|indices|indices.contains(&index)).then_some("true")
                            title=format_media_time(cue.start)
                            on:click=move |_| {following.set(true);on_seek.run(cue.start);}>
                            {cue.text}
                        </button>
                    } />
                </div>
            </div>
            <Show when=move ||!following.get() && !cues.with(|cues|cues.is_empty()) fallback=|| ()>
                <button type="button" class="audio-transcript-follow" on:click=move |_|following.set(true)>"回到当前台词"</button>
            </Show>
        </section>
    }
}

/// Full-screen audio player mounted inside [`super::media::MediaPreview`].
#[component]
pub fn AudioPlayer(
    item: File,
    #[prop(optional)] controller: Option<MusicController>,
) -> impl IntoView {
    let player = NodeRef::<leptos::html::Div>::new();
    let audio = controller.map_or_else(NodeRef::new, |c| c.audio);
    let media = RwSignal::new(None::<AudioMedia>);
    let panel_open = RwSignal::new(false);
    let cover_failed = RwSignal::new(false);
    let loading = RwSignal::new(true);
    let waiting = controller.map_or_else(|| RwSignal::new(false), |c| c.waiting);
    let playing = controller.map_or_else(|| RwSignal::new(false), |c| c.playing);
    let current_time = controller.map_or_else(|| RwSignal::new(0.0_f64), |c| c.position);
    let native_duration = controller.map_or_else(|| RwSignal::new(0.0_f64), |c| c.duration);
    let media_ready = controller.map_or_else(|| RwSignal::new(false), |c| c.media_ready);
    let pending_seek = RwSignal::new(None::<f64>);
    let buffered = RwSignal::new(0.0_f64);
    let rate = controller.map_or_else(|| RwSignal::new(1.0_f64), |c| c.rate);
    let volume = controller.map_or_else(
        || RwSignal::new(stored_volume("revaro-audio-volume", 0.85)),
        |c| c.volume,
    );
    let muted = controller.map_or_else(|| RwSignal::new(audio_muted()), |c| c.muted);
    let error = controller.map_or_else(|| RwSignal::new(String::new()), |c| c.error);
    let seek_preview = RwSignal::new(None::<f64>);
    let seek_hover = RwSignal::new(None::<SeekHover>);
    let playback = PlaybackProgress::new();
    let save_timer = RwSignal::new(None::<i32>);
    let remote_save_timer = RwSignal::new(None::<i32>);
    let mounted = RwSignal::new(false);

    let source = format!("/api/files/{}/preview", item.id);
    let item_name = item.name.clone();
    let position_key = format!("revaro-audio-position:{}", item.id);
    let item_id = item.id.clone();

    let duration = move || {
        audio_duration(
            native_duration.get(),
            media.with(|media| media.as_ref().map_or(0.0, |media| media.duration)),
        )
    };
    let can_seek = move || media_ready.get() && duration() > 0.0;
    let chapters_factory = StoredValue::new({
        let item_name = item_name.clone();
        move || {
            media.get().map_or_else(
                || {
                    if controller.is_some() {
                        return Vec::new();
                    }
                    vec![AudioChapter {
                        id: 1,
                        title: stem(&item_name),
                        start: 0.0,
                        end: duration(),
                    }]
                },
                |value| {
                    if value.chapters.is_empty() && controller.is_none() {
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
            if controller.is_some() {
                return;
            }
            if duration() > 0.0
                && let Some(element) = audio_element(audio)
                && element.ready_state() >= 1
                && let Some(saved) = playback.restore(duration(), Some(&position_key))
            {
                seek_audio(audio, current_time, pending_seek, duration(), saved, false);
            }
        }
    };
    let save_progress = {
        let position_key = position_key.clone();
        let item_id = item_id.clone();
        move |remote: bool| {
            if let Some(controller) = controller {
                controller.save();
                return;
            }
            if playback.ready.get_untracked() {
                let position = pending_seek
                    .get_untracked()
                    .unwrap_or(current_time.get_untracked());
                persist_progress(
                    playback,
                    &item_id,
                    position,
                    duration(),
                    Some(&position_key),
                    if remote {
                        ProgressDestination::Remote
                    } else {
                        ProgressDestination::Local
                    },
                );
            }
        }
    };
    let schedule_local_save = {
        let save_progress = save_progress.clone();
        move || {
            let save = save_progress.clone();
            debounce(save_timer, 500, move || save(false));
        }
    };
    let schedule_remote_save = {
        let save_progress = save_progress.clone();
        move || {
            let save = save_progress.clone();
            throttle(remote_save_timer, 5_000, move || save(true));
        }
    };

    let on_loaded_metadata = {
        let restore_position = restore_position.clone();
        move |_| {
            if let Some(element) = audio_element(audio) {
                media_ready.set(element.ready_state() >= 1);
                let value = media_element_time(element.duration());
                if value > 0.0 {
                    native_duration.set(value);
                }
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
            if pending_seek.get_untracked().is_none()
                && let Some(element) = audio_element(audio)
            {
                if let Some(time) = stable_media_clock(
                    element.current_time(),
                    element.ready_state() >= 1,
                    current_time.get_untracked(),
                    None,
                ) {
                    current_time.set(time);
                    if !element.paused() {
                        playback.clock_changed();
                    }
                }
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
    let on_seeked = {
        let save_progress = save_progress.clone();
        move |_| {
            if let Some(element) = audio_element(audio)
                && !element.seeking()
                && stable_media_clock(
                    element.current_time(),
                    element.ready_state() >= 1,
                    current_time.get_untracked(),
                    pending_seek.get_untracked(),
                )
                .is_some()
            {
                pending_seek.set(None);
                current_time.set(media_element_time(element.current_time()));
                update_buffer(audio, duration, buffered);
                save_progress(true);
            }
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
        move |event: Event| {
            playback.ended();
            on_pause(event);
        }
    };
    let on_error = move |_| {
        loading.set(false);
        waiting.set(false);
        error.set("浏览器无法播放此原始格式，请下载后使用本地播放器打开".to_owned());
    };

    let toggle_playback = move || {
        if let Some(controller) = controller {
            controller.toggle();
            return;
        }
        let Some(element) = audio_element(audio) else {
            return;
        };
        if element.paused() {
            playback.intent();
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
        let save_progress = save_progress.clone();
        move |target: f64, play: bool| {
            if let Some(controller) = controller {
                controller.seek(target);
                if play {
                    controller.start();
                }
                return;
            }
            if target.is_finite()
                && let Some(element) = audio_element(audio)
                && element.ready_state() >= 1
            {
                seek_audio(audio, current_time, pending_seek, duration(), target, play);
                playback.accept_seek();
                save_progress(true);
            }
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
        let close_panel = {
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
            let callback = Closure::once(move || {
                if let Some(element) = player.get() {
                    let _ = element
                        .unchecked_into::<web_sys::Element>()
                        .query_selector(".audio-panel .chapter-panel-close")
                        .ok()
                        .flatten()
                        .and_then(|element| element.dyn_into::<web_sys::HtmlElement>().ok())
                        .map(|element| element.focus());
                }
            })
            .into_js_value();
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
            let callback = Closure::once(move || {
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
            })
            .into_js_value();
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
        browser::local_storage_set(
            if controller.is_some() {
                "revaro-music-volume"
            } else {
                "revaro-audio-volume"
            },
            &value.to_string(),
        );
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
    let preview_seek = Callback::new(move |value| seek_preview.set(Some(value)));
    let commit_seek = {
        let seek = seek.clone();
        Callback::new(move |target| {
            seek(target, playing.get_untracked());
            seek_preview.set(None);
        })
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
    let retry_progress = Callback::new({
        let restore_position = restore_position.clone();
        move |()| restore_position()
    });
    if controller.is_none() {
        let restore_position = restore_position.clone();
        playback.load(item.id.clone(), Callback::new(move |()| restore_position()));
    }
    if controller.is_none() {
        let restore = restore_position.clone();
        let remote_restore = restore_position.clone();
        super::playback::watch_progress(
            playback,
            Signal::derive({
                let id = item_id.clone();
                move || id.clone()
            }),
            Callback::new(move |()| restore()),
            Callback::new(move |()| {
                if let Some(element) = audio_element(audio) {
                    let _ = element.pause();
                }
                remote_restore();
                if let Some(remote) = playback.server.get_untracked()
                    && remote.position == 0.0
                {
                    seek_audio(audio, current_time, pending_seek, duration(), 0.0, false);
                }
            }),
        );
    }
    if controller.is_none() {
        let id = item.id.clone();
        leptos::task::spawn_local_scoped_with_cancellation(async move {
            if let Ok(value) = api::fetch_audio_media(&id).await {
                media.set(Some(value));
                restore_position();
            }
        });
    }
    if let Some(controller) = controller {
        Effect::new(move |_| {
            media.set(Some(controller.metadata.get()));
            loading.set(
                (!controller.media_ready.get() || controller.duration.get() <= 0.0)
                    && controller.error.get().is_empty(),
            );
        });
    }

    // Start playback as soon as the element exists. A rejected autoplay call is
    // harmless and the visible play button remains available for that browser.
    {
        let source = source.clone();
        Effect::new(move |_| {
            if mounted.get() {
                return;
            }
            let Some(element) = audio_element(audio) else {
                return;
            };
            mounted.set(true);
            if let Some(element) = player.get() {
                let options = web_sys::FocusOptions::new();
                options.set_prevent_scroll(true);
                let _ = element.focus_with_options(&options);
            }
            if controller.is_some() {
                return;
            }
            element.set_src(&source);
            let _ = element
                .unchecked_ref::<web_sys::Element>()
                .set_attribute("playsinline", "");
            element.set_preload("metadata");
            element.set_autoplay(true);
            element.set_volume(volume.get_untracked());
            element.set_muted(muted.get_untracked());
            element.load();
            play_ignoring_rejection(&element);
        });
    }

    let cleanup_save = save_progress.clone();
    let cleanup_item_id = item_id.clone();
    on_cleanup(move || {
        clear_timer(save_timer);
        clear_timer(remote_save_timer);
        if let Some(controller) = controller {
            controller.save();
            return;
        }
        cleanup_save(false);
        let position = pending_seek
            .get_untracked()
            .unwrap_or(current_time.get_untracked());
        if playback.ready.get_untracked() {
            persist_progress(
                playback,
                &cleanup_item_id,
                position,
                audio_duration(
                    native_duration.get_untracked(),
                    media.get_untracked().map_or(0.0, |media| media.duration),
                ),
                None,
                ProgressDestination::Keepalive,
            );
        }
        playback.reset(None);
        if let Some(element) = audio_element(audio) {
            let _ = element.pause();
            element.set_src("");
            element.load();
        }
    });

    let cover_name = item.name.clone();
    let track_title = stem(&item.name);
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
                    <div class="audio-track-info">
                        <h1>{track_title}</h1>
                        <p>{move || controller.and_then(|c| c.collection.get().map(|(_, name)| name)).unwrap_or_else(|| "正在播放你的音乐".to_owned())}</p>
                    </div>
                </section>
                <AudioTranscript
                    cues=Signal::derive(move ||media.get().map(|m|m.subtitles).unwrap_or_default())
                    current_time=current_time.into()
                    layout_changed=panel_open.into()
                    on_seek=Callback::new(move |time|seek_callback.with_value(|seek|seek(time,playing.get_untracked())))
                />
                <section class="audio-playback" aria-label="音频播放控制">
                    <AudioProgress
                        percent=progress
                        buffered=move || controller.map_or_else(|| vec![(0.0, buffered.get())], |c| c.buffered.get())
                        markers=move || timeline_chapters(&chapters(), duration())
                        duration=duration
                        enabled=can_seek
                        current_time=displayed_time
                        hover=seek_hover
                        on_input=preview_seek
                        on_change=commit_seek
                        on_cancel=Callback::new(move |()|seek_preview.set(None))
                        on_chapter_seek=Callback::new(move |time|seek_callback.with_value(|seek|seek(time,playing.get_untracked())))
                        on_pointer_move=hover_seek
                        on_pointer_leave=leave_seek
                    />
                    <div class="audio-time"><span>{move || format_media_time(displayed_time())}</span><span>{move || format_media_time(duration())}</span></div>
                    <div class="audio-controls">
                        <button type="button" aria-label="后退15秒" title="后退 15 秒" prop:disabled={move || !can_seek()} on:click=move |_| seek_callback.with_value(|seek| seek(current_time.get_untracked() - 15.0, false))>{icons::rotate_ccw()}<small>"15"</small></button>
                        <button class="audio-play" type="button" prop:disabled=move || loading.get() aria-label=move || if playing.get() { "暂停" } else { "播放" } on:click=move |_| toggle_playback()>
                            <Show when=move || loading.get() || waiting.get() fallback=move || if playing.get() { icons::pause().into_any() } else { icons::play().into_any() }>
                                <span class="audio-control-spinner"></span>
                            </Show>
                        </button>
                        <button type="button" aria-label="前进30秒" title="前进 30 秒" prop:disabled={move || !can_seek()} on:click=move |_| seek_callback.with_value(|seek| seek(current_time.get_untracked() + 30.0, false))>{icons::rotate_cw()}<small>"30"</small></button>
                    </div>
                    <div class="audio-options">
                        <label class="audio-rate"><span class="media-sr-only">"播放速度"</span><select aria-label="播放速度" prop:value=move || rate.get().to_string() on:change=set_rate>
                            <option value="0.75">"0.75×"</option><option value="1">"1×"</option><option value="1.25">"1.25×"</option><option value="1.5">"1.5×"</option><option value="2">"2×"</option>
                        </select></label>
                        {controller.map(|controller| view! { <super::music_player::PlaybackModeControl controller=controller /> })}
                        <button type="button" data-panel-trigger="chapters" aria-expanded=move || panel_open.get().to_string() on:click=toggle_panel>{icons::list()}<span>"章节"</span></button>
                        <ActionMenu label="音量".to_owned() icon=MenuIcon::Volume volume=volume muted=muted>
                            <div class="audio-volume">
                                <button type="button" aria-label=move || if muted.get() { "取消静音" } else { "静音" } on:click=toggle_mute>
                                    {move || if muted.get() { icons::volume_x().into_any() } else { icons::volume_2().into_any() }}
                                </button>
                                <input type="range" min="0" max="1" step="0.01" aria-label="音量" prop:value=move || volume.get().to_string() on:input=set_volume />
                                <output>{move || format!("{}%", if muted.get() { 0 } else { (volume.get() * 100.0).round() as u64 })}</output>
                            </div>
                        </ActionMenu>
                    </div>
                    <Show when=move || !error.get().is_empty() fallback=|| ()>
                        <p class="audio-player-error" role="alert">{move || error.get()}</p>
                    </Show>
                    {controller.is_none().then(|| view! { <super::playback::ProgressRetry playback=playback file_id=Signal::derive({let id=item_id.clone();move ||id.clone()}) on_loaded=retry_progress /> })}
                    {controller.is_none().then(|| view! { <audio node_ref=audio src=source.clone() autoplay preload="metadata" on:loadedmetadata=on_loaded_metadata.clone() on:durationchange=on_loaded_metadata on:timeupdate=on_time_update on:seeked=on_seeked on:progress=move |_| update_buffer(audio, duration, buffered) on:play=on_play on:pause=on_pause on:ended=on_ended on:waiting=on_waiting on:canplay=on_can_play on:error=on_error></audio> })}
                </section>
            </main>
            <Show when=move || panel_open.get() fallback=|| ()>
                <button class="audio-panel-scrim" type="button" aria-label="收起面板" on:click=close_panel></button>
                <aside class="audio-panel" data-preview-sheet aria-label="音频章节">
                    <header><strong>"章节"</strong><button class="chapter-panel-close" type="button" aria-label="关闭章节" title="关闭章节" on:click=close_panel>{icons::x()}</button></header>
                    <div class="audio-chapter-navigation">
                        <button type="button" prop:disabled={move || duration() <= 0.0 || chapters().is_empty()} on:click=move |_| previous_chapter_callback.with_value(|callback| callback())>{icons::skip_back()}<span>"上一章"</span></button>
                        <button type="button" prop:disabled={move || current_chapter_index() >= chapters().len().saturating_sub(1)} on:click=move |_| _next_chapter_callback.with_value(|callback| callback())><span>"下一章"</span>{icons::skip_forward()}</button>
                    </div>
                    <div class="audio-chapter-list">
                        <For each=move || indexed_chapters(chapters()) key=|(index, _)| *index let:entry>
                            <button type="button" data-chapter-index=entry.0.to_string() aria-current=move || if entry.0 == current_chapter_index() { Some("true") } else { None } on:click={
                                let chapter = entry.1.clone();
                                move |_| seek_callback.with_value(|seek| seek(chapter.start, true))
                            }>
                                <span class="audio-chapter-number">{format!("{:02}", entry.0 + 1)}</span>
                                <strong>{entry.1.title.clone()}</strong>
                                <small>{format_media_time(entry.1.start)}</small>
                            </button>
                        </For>
                    </div>
                    <Show when=move ||chapters().is_empty() fallback=|| ()><p class="audio-panel-empty">"暂无章节"</p></Show>
                </aside>
            </Show>
        </div>
    }
}

/// The historical player view controls the persistent element. Opening or
/// closing this dialog never reloads the source or interrupts playback.
#[component]
pub(super) fn AudioPlayerDialog(controller: MusicController) -> impl IntoView {
    let root = NodeRef::<leptos::html::Section>::new();
    let document = web_sys::window().and_then(|window| window.document());
    let previous_focus = document.as_ref().and_then(|d| d.active_element());
    let body = document.and_then(|d| d.body());
    let previous_overflow = body
        .as_ref()
        .and_then(|b| b.style().get_property_value("overflow").ok());
    if let Some(body) = &body {
        let _ = body.style().set_property("overflow", "hidden");
    }
    on_cleanup(move || {
        if let Some(body) = body {
            let _ = body
                .style()
                .set_property("overflow", previous_overflow.as_deref().unwrap_or_default());
        }
        if let Some(element) =
            previous_focus.and_then(|e| e.dyn_into::<web_sys::HtmlElement>().ok())
            && element.is_connected()
        {
            let options = web_sys::FocusOptions::new();
            options.set_prevent_scroll(true);
            let _ = element.focus_with_options(&options);
        }
    });
    view! {
        <super::dialogs::DialogBackdrop class="modal-backdrop previewing" on_close=Callback::new(move |()|controller.full_open.set(false))>
            <section node_ref=root class="preview-modal audio-preview music-player-dialog" role="dialog" aria-modal="true" aria-label="音频播放器" tabindex="-1"
                on:keydown=move |event: KeyboardEvent| {
                    if event.key()=="Escape" && !event.default_prevented() {
                        event.prevent_default();event.stop_propagation();controller.full_open.set(false);
                    } else if event.key()=="Tab" {
                        super::media::trap_focus(root,&event);
                    }
                }>
                <header class="preview-commandbar"><div class="preview-file-meta"><strong>{move ||controller.current().map(|f|stem(&f.name)).unwrap_or_default()}</strong></div>
                    <button type="button" class="media-icon-button preview-close" aria-label="收起音频播放器" on:click=move |_|controller.full_open.set(false)>{icons::chevron_down()}</button>
                </header>
                <div class="preview-stage">
                    <For each=move ||{controller.current().into_iter().collect::<Vec<_>>()} key=|file|file.id.clone() children=move |file|view! { <AudioPlayer item=file controller=controller /> } />
                </div>
            </section>
        </super::dialogs::DialogBackdrop>
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
struct SeekHover {
    percent: f64,
    time: f64,
}

#[component]
fn AudioProgress(
    percent: impl Fn() -> f64 + Copy + Send + Sync + 'static,
    buffered: impl Fn() -> Vec<(f64, f64)> + Copy + Send + Sync + 'static,
    markers: impl Fn() -> Vec<AudioChapter> + Send + Sync + 'static,
    duration: impl Fn() -> f64 + Copy + Send + Sync + 'static,
    enabled: impl Fn() -> bool + Copy + Send + Sync + 'static,
    current_time: impl Fn() -> f64 + Copy + Send + Sync + 'static,
    hover: RwSignal<Option<SeekHover>>,
    on_input: Callback<f64>,
    on_change: Callback<f64>,
    on_cancel: Callback<()>,
    on_chapter_seek: Callback<f64>,
    on_pointer_move: impl Fn(PointerEvent) + Send + Sync + 'static,
    on_pointer_leave: impl Fn(PointerEvent) + Send + Sync + 'static,
) -> impl IntoView {
    // This component only groups the layered track. The input remains the
    // focusable/control authority, while the visible bars are inert decoration.
    view! {
        <div class="full-bleed-progress full-bleed-progress--audio">
            <div class="full-bleed-progress__track" aria-hidden="true">
                <For each=buffered key=|(start, end)| (start.to_bits(), end.to_bits()) children=move |(start, end)| view! {
                    <span class="full-bleed-progress__buffer" style=format!("left:{}%;width:{}%;", clamp_percent(start), clamp_percent(end-start))></span>
                } />
                <span class="full-bleed-progress__played" style=move || format!("width:{}%;", clamp_percent(percent()))></span>
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
            <AudioSeekInput
                class="full-bleed-progress__input"
                duration=Signal::derive(duration)
                position=Signal::derive(current_time)
                enabled=Signal::derive(enabled)
                label="播放进度"
                on_preview=on_input
                on_commit=on_change
                on_cancel=on_cancel
                on_hover=Callback::new(on_pointer_move)
                on_leave=Callback::new(on_pointer_leave)
            />
            <For each=markers key=|chapter|chapter.start.to_bits() children=move |chapter|view! {
                <button type="button" class="full-bleed-progress__chapter-marker" title=chapter.title.clone()
                    aria-label=format!("跳转章节：{}",chapter.title) data-chapter-start=chapter.start.to_string()
                    prop:disabled=move ||!enabled()
                    style:left=move ||format!("{}%",clamp_percent(chapter.start/duration()*100.0))
                    on:click=move |_|on_chapter_seek.run(chapter.start)></button>
            } />
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

fn indexed_chapters(chapters: Vec<AudioChapter>) -> Vec<(usize, AudioChapter)> {
    chapters.into_iter().enumerate().collect()
}

fn seek_audio(
    audio: NodeRef<leptos::html::Audio>,
    current_time: RwSignal<f64>,
    pending_seek: RwSignal<Option<f64>>,
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
        if (element.current_time() - target).abs() > 0.001 {
            pending_seek.set(Some(target));
            element.set_current_time(target);
        } else {
            pending_seek.set(None);
        }
        current_time.set(target);
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

fn stem(name: &str) -> String {
    name.rsplit_once('.')
        .map_or_else(|| name.to_owned(), |(stem, _)| stem.to_owned())
}

fn audio_muted() -> bool {
    browser::local_storage_get("revaro-audio-muted").as_deref() == Some("true")
}

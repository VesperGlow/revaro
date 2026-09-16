//! Native video playback with subtitle tracks and resume progress.
//!
//! The server advertises subtitle URLs and serves the original bytes through a
//! Range aware endpoint. The component leaves decoding to the browser, while
//! keeping controls, the custom subtitle overlay and progress persistence in a
//! small lifecycle owned by this player.

use std::cell::RefCell;
use std::rc::Rc;

use js_sys::{Function, Object, Reflect};
use leptos::ev::{Event, MouseEvent, PointerEvent};
use leptos::prelude::*;
use revaro_core::media::VideoSubtitleTrack;
use revaro_core::model::{File, MediaProgress};
use wasm_bindgen::JsCast;
use wasm_bindgen::JsValue;
use wasm_bindgen::closure::Closure;
use web_sys::{
    Element, HtmlInputElement, HtmlMediaElement, HtmlSelectElement, HtmlTrackElement,
    HtmlVideoElement, KeyboardEvent, TextTrack, TextTrackMode, VttCue,
};

use crate::api;
use crate::browser;
use crate::logic::format::format_media_time;
use crate::logic::media::{
    authoritative_seek_target, contained_video_insets, initial_subtitle_index, media_element_time,
    should_hide_video_cursor, should_sync_media_clock, subtitle_line_is_secondary,
};

use super::icons;
use super::media::{MenuIcon, PreviewMenu};

fn play_video_ignoring_rejection(video: &HtmlMediaElement) {
    if let Ok(promise) = video.play() {
        wasm_bindgen_futures::spawn_local(async move {
            let _ = wasm_bindgen_futures::JsFuture::from(promise).await;
        });
    }
}

/// Full-screen video player mounted inside [`super::media::MediaPreview`].
#[component]
pub fn VideoPlayer(
    item: File,
    on_close: Callback<()>,
    on_download: Callback<File>,
    on_move: Callback<File>,
    on_copy: Callback<File>,
) -> impl IntoView {
    let shell = NodeRef::<leptos::html::Div>::new();
    let video = NodeRef::<leptos::html::Video>::new();
    let subtitle_element = NodeRef::<leptos::html::Track>::new();
    let subtitles = RwSignal::new(Vec::<VideoSubtitleTrack>::new());
    let active_subtitle = RwSignal::new(None::<usize>);
    let cue_track = RwSignal::new(None::<TextTrack>);
    let active_subtitle_lines = RwSignal::new(Vec::<String>::new());
    let subtitle_placement = RwSignal::new(SubtitlePlacement::Bottom);
    let subtitle_bottom = RwSignal::new(0.0_f64);
    let subtitle_inset = RwSignal::new(0.0_f64);
    let cue_listener: Rc<RefCell<Option<Closure<dyn FnMut(Event)>>>> = Rc::new(RefCell::new(None));
    let track_loaded = RwSignal::new(false);

    let starting = RwSignal::new(true);
    let buffering = RwSignal::new(false);
    let playing = RwSignal::new(false);
    let error = RwSignal::new(String::new());
    let current_time = RwSignal::new(0.0_f64);
    let duration = RwSignal::new(0.0_f64);
    let controls_visible = RwSignal::new(true);
    let controls_hovered = RwSignal::new(false);
    let initial_volume = video_volume();
    let volume = RwSignal::new(initial_volume);
    let last_audible_volume = RwSignal::new(if initial_volume > 0.0 {
        initial_volume
    } else {
        0.9
    });
    let muted = RwSignal::new(initial_volume == 0.0);
    let volume_feedback = RwSignal::new(false);
    let rate = RwSignal::new(video_rate());
    let pointer_type = RwSignal::new(String::from("mouse"));
    let click_timer = RwSignal::new(None::<i32>);
    let controls_timer = RwSignal::new(None::<i32>);
    let volume_timer = RwSignal::new(None::<i32>);
    let save_timer = RwSignal::new(None::<i32>);
    let remote_save_timer = RwSignal::new(None::<i32>);
    let pending_seek = RwSignal::new(None::<f64>);
    let fullscreen = RwSignal::new(false);
    let autoplay_pending = RwSignal::new(true);
    let server_position = RwSignal::new(0.0_f64);
    let progress_loaded = RwSignal::new(false);
    let restored_position = RwSignal::new(false);
    let user_seeked = RwSignal::new(false);

    let source = format!("/api/files/{}/preview", item.id);
    let poster = thumbnail_url(&item);
    let item_name = item.name.clone();
    let item_id = item.id.clone();

    // The reference focuses the player shell after mounting so keyboard
    // shortcuts (space, arrows, m, f) work immediately without requiring a
    // preliminary click on the video surface.
    if let Some(window) = web_sys::window() {
        let shell_for_focus = shell;
        let callback = Closure::once_into_js(move || {
            if let Some(element) = shell_for_focus.get() {
                let options = web_sys::FocusOptions::new();
                options.set_prevent_scroll(true);
                let _ = element.focus_with_options(&options);
            }
        });
        let _ = window
            .set_timeout_with_callback_and_timeout_and_arguments_0(callback.unchecked_ref(), 0);
    }

    let timeline_position = move || pending_seek.get().unwrap_or_else(|| current_time.get());
    let progress = move || {
        if duration.get() > 0.0 {
            (timeline_position() / duration.get() * 100.0).clamp(0.0, 100.0)
        } else {
            0.0
        }
    };
    let effective_volume = move || if muted.get() { 0.0 } else { volume.get() };
    let cursor_hidden = move || {
        should_hide_video_cursor(
            playing.get(),
            controls_visible.get(),
            starting.get(),
            buffering.get(),
            !error.get().is_empty(),
        )
    };

    let update_subtitle_bounds = move || {
        let Some(video) = video_element(video) else {
            return;
        };
        let (bottom, inset) = contained_video_insets(
            f64::from(video.client_width().max(0)),
            f64::from(video.client_height().max(0)),
            f64::from(video.video_width()),
            f64::from(video.video_height()),
        );
        subtitle_bottom.set(bottom);
        subtitle_inset.set(inset);
    };
    let mut resize_listener = {
        let update_subtitle_bounds = update_subtitle_bounds.clone();
        browser::on_resize(move |_| update_subtitle_bounds())
    };

    // A cuechange callback belongs to the selected TextTrack rather than the
    // video element. The callback is replaced when the selected track changes.
    let apply_subtitle = {
        let cue_listener = cue_listener.clone();
        move || {
            let Some(media) = video_media_element(video) else {
                return;
            };
            disable_tracks(&media);
            if active_subtitle.get_untracked().is_some()
                && let Some(track) = subtitle_element
                    .get()
                    .and_then(|element| element.unchecked_into::<HtmlTrackElement>().track())
            {
                track.set_mode(TextTrackMode::Hidden);
                install_cue_listener(
                    track,
                    cue_track,
                    active_subtitle_lines,
                    subtitle_placement,
                    &cue_listener,
                );
            } else {
                cue_track.set(None);
                active_subtitle_lines.set(Vec::new());
            }
        }
    };
    let on_track_load = move |_| track_loaded.set(true);
    // The Vue reference treats a track error as a diagnostic event only. Keep
    // the last cue overlay visible until the browser emits the next cuechange
    // or the user explicitly changes/disables the track; clearing it here
    // causes a visible flash that the reference does not produce.
    let on_track_error = move |_| {};
    {
        let apply_subtitle = apply_subtitle.clone();
        Effect::new(move |_| {
            active_subtitle.get();
            track_loaded.get();
            apply_subtitle();
        });
    }

    let restore_item_id = item_id.clone();
    let restore_position = move || {
        if !progress_loaded.get_untracked()
            || restored_position.get_untracked()
            || duration.get_untracked() <= 0.0
        {
            return;
        }
        restored_position.set(true);
        let saved = if server_position.get_untracked() > 0.0 {
            server_position.get_untracked()
        } else {
            browser::local_storage_get(&format!("revaro-video-position:{restore_item_id}"))
                .and_then(|value| value.parse::<f64>().ok())
                .filter(|value| value.is_finite() && *value > 0.0)
                .unwrap_or(0.0)
        };
        let target = authoritative_seek_target(
            current_time.get_untracked(),
            saved,
            user_seeked.get_untracked(),
        );
        if target > 0.0 && target < duration.get_untracked() - 5.0 {
            if let Some(video) = video_media_element(video) {
                video.set_current_time(target);
                current_time.set(target);
            }
        }
    };

    let save_progress = {
        let item_id = item_id.clone();
        move |remote: bool| {
            let position = current_time.get_untracked().max(0.0);
            if position <= 0.0 && !user_seeked.get_untracked() {
                return;
            }
            browser::local_storage_set(
                &format!("revaro-video-position:{item_id}"),
                &position.floor().to_string(),
            );
            if remote {
                let progress = MediaProgress {
                    position,
                    duration: duration.get_untracked(),
                    updated_at: None,
                };
                let id = item_id.clone();
                leptos::task::spawn_local(async move {
                    let _ = api::save_media_progress(&id, &progress).await;
                });
            }
        }
    };

    let on_loaded_metadata = {
        let restore_position = restore_position.clone();
        let apply_subtitle = apply_subtitle.clone();
        let update_subtitle_bounds = update_subtitle_bounds.clone();
        move |_| {
            if let Some(video) = video_element(video) {
                duration.set(safe_duration(video.duration()));
                video.set_volume(volume.get_untracked());
                video.set_muted(muted.get_untracked());
                video.set_playback_rate(rate.get_untracked());
                starting.set(false);
            }
            apply_subtitle();
            update_subtitle_bounds();
            restore_position();
        }
    };
    let on_time_update = {
        let save_progress = save_progress.clone();
        move |_| {
            if let Some(video) = video_media_element(video) {
                if should_sync_media_clock(starting.get_untracked(), video.paused()) {
                    current_time.set(media_element_time(video.current_time()));
                }
            }
            schedule_progress_timer(save_timer, 600, {
                let save_progress = save_progress.clone();
                move || save_progress(false)
            });
            schedule_remote_timer(remote_save_timer, 5_000, {
                let save_progress = save_progress.clone();
                move || save_progress(true)
            });
        }
    };
    let on_play = {
        let show = controls_visible;
        let timer = controls_timer;
        move |_| {
            playing.set(true);
            starting.set(false);
            buffering.set(false);
            show_video_controls(show, timer, playing, starting, buffering, error, false);
        }
    };
    let on_pause = {
        let save_progress = save_progress.clone();
        move |_| {
            playing.set(false);
            if !starting.get_untracked() {
                if let Some(video) = video_media_element(video) {
                    current_time.set(media_element_time(video.current_time()));
                }
                clear_timer(remote_save_timer);
                save_progress(true);
            }
            show_video_controls(
                controls_visible,
                controls_timer,
                playing,
                starting,
                buffering,
                error,
                true,
            );
        }
    };
    let on_waiting = move |_| {
        if !starting.get_untracked() {
            buffering.set(true);
            show_video_controls(
                controls_visible,
                controls_timer,
                playing,
                starting,
                buffering,
                error,
                true,
            );
        }
    };
    let on_can_play = move |_| {
        // Chromium can expose a readyState/duration pair after a second
        // `load()` without replaying `loadedmetadata` to a listener that was
        // attached during the initial source load. Keep the control bar's
        // duration in sync with the native element at the first playable
        // event, matching the reference's metadata display on retry/resume.
        if let Some(video) = video_element(video) {
            let native_duration = safe_duration(video.duration());
            if native_duration > 0.0 {
                duration.set(native_duration);
            }
        }
        buffering.set(false);
        starting.set(false);
        show_video_controls(
            controls_visible,
            controls_timer,
            playing,
            starting,
            buffering,
            error,
            false,
        );
    };
    let on_error = move |_| {
        starting.set(false);
        buffering.set(false);
        error.set("浏览器无法播放此原始格式，请下载后使用本地播放器打开".to_owned());
        show_video_controls(
            controls_visible,
            controls_timer,
            playing,
            starting,
            buffering,
            error,
            true,
        );
    };

    let toggle_playback = move || {
        let Some(video) = video_media_element(video) else {
            return;
        };
        if starting.get_untracked() {
            autoplay_pending.update(|value| *value = !*value);
            return;
        }
        if video.paused() {
            play_video_ignoring_rejection(&video);
        } else {
            let _ = video.pause();
        }
    };
    let seek_to = {
        move |target: f64| {
            if !target.is_finite() {
                return;
            }
            user_seeked.set(true);
            let limit = duration.get_untracked();
            let target = target
                .max(0.0)
                .min(if limit > 0.0 { limit } else { target.max(0.0) });
            if let Some(video) = video_media_element(video) {
                video.set_current_time(target);
                current_time.set(media_element_time(video.current_time()));
            } else {
                current_time.set(target);
            }
        }
    };
    let retry_playback = {
        let toggle_playback = toggle_playback.clone();
        move |_| {
            error.set(String::new());
            starting.set(false);
            if let Some(video) = video_media_element(video) {
                video.load();
            }
            toggle_playback();
        }
    };
    let preview_seek = move |event: Event| {
        if let Some(input) = event
            .target()
            .and_then(|target| target.dyn_into::<HtmlInputElement>().ok())
            && let Ok(value) = input.value().parse::<f64>()
        {
            pending_seek.set(Some(value));
            show_video_controls(
                controls_visible,
                controls_timer,
                playing,
                starting,
                buffering,
                error,
                true,
            );
        }
    };
    let commit_seek = {
        let seek_to = seek_to.clone();
        move |event: Event| {
            let value = event
                .target()
                .and_then(|target| target.dyn_into::<HtmlInputElement>().ok())
                .and_then(|input| input.value().parse::<f64>().ok());
            pending_seek.set(None);
            if let Some(value) = value {
                seek_to(value);
            }
            show_video_controls(
                controls_visible,
                controls_timer,
                playing,
                starting,
                buffering,
                error,
                false,
            );
        }
    };
    let cancel_seek = move |_| {
        pending_seek.set(None);
        show_video_controls(
            controls_visible,
            controls_timer,
            playing,
            starting,
            buffering,
            error,
            false,
        );
    };
    let shell_for_rate_focus = shell;
    let change_rate = move |event: Event| {
        let Some(select) = event
            .target()
            .and_then(|target| target.dyn_into::<HtmlSelectElement>().ok())
        else {
            return;
        };
        let value = select.value().parse::<f64>().unwrap_or(1.0);
        rate.set(value);
        browser::local_storage_set("revaro-video-rate", &value.to_string());
        if let Some(video) = video_media_element(video) {
            video.set_playback_rate(value);
        }
        // Leptos may patch the reactive select while handling `change`.
        // Restore its focus on the next layout turn so Escape still reaches
        // the open menu, matching the native Vue select behavior.
        if let Some(window) = web_sys::window() {
            let callback = Closure::once_into_js(move || {
                let Some(select) = shell_for_rate_focus
                    .get()
                    .and_then(|shell| shell.query_selector("details[open] select").ok().flatten())
                    .and_then(|element| element.dyn_into::<HtmlSelectElement>().ok())
                else {
                    return;
                };
                let _ = select.focus();
            });
            let _ = window
                .set_timeout_with_callback_and_timeout_and_arguments_0(callback.unchecked_ref(), 0);
        }
        show_video_controls(
            controls_visible,
            controls_timer,
            playing,
            starting,
            buffering,
            error,
            true,
        );
    };
    let change_volume = move |event: Event| {
        let Some(input) = event
            .target()
            .and_then(|target| target.dyn_into::<HtmlInputElement>().ok())
        else {
            return;
        };
        let value = input.value().parse::<f64>().unwrap_or(0.9).clamp(0.0, 1.0);
        volume.set(value);
        if value > 0.0 {
            last_audible_volume.set(value);
        }
        muted.set(value == 0.0);
        browser::local_storage_set("revaro-video-volume", &value.to_string());
        if let Some(video) = video_media_element(video) {
            video.set_volume(value);
            video.set_muted(value == 0.0);
        }
        volume_feedback.set(true);
        clear_timer(volume_timer);
        if let Some(window) = web_sys::window() {
            let callback = Closure::once_into_js(move || volume_feedback.set(false));
            if let Ok(id) = window.set_timeout_with_callback_and_timeout_and_arguments_0(
                callback.unchecked_ref(),
                900,
            ) {
                volume_timer.set(Some(id));
            }
        }
        show_video_controls(
            controls_visible,
            controls_timer,
            playing,
            starting,
            buffering,
            error,
            true,
        );
    };
    let toggle_mute = move || {
        let silent = muted.get_untracked() || volume.get_untracked() == 0.0;
        if silent {
            if volume.get_untracked() == 0.0 {
                volume.set(last_audible_volume.get_untracked());
            }
            muted.set(false);
        } else {
            muted.set(true);
        }
        if let Some(video) = video_media_element(video) {
            video.set_volume(volume.get_untracked());
            video.set_muted(muted.get_untracked());
        }
        show_video_controls(
            controls_visible,
            controls_timer,
            playing,
            starting,
            buffering,
            error,
            true,
        );
    };
    let toggle_fullscreen = move || {
        let Some(element) = shell
            .get()
            .map(|element| element.unchecked_into::<Element>())
        else {
            return;
        };
        let Some(document) = web_sys::window().and_then(|window| window.document()) else {
            return;
        };
        if document.fullscreen_element().is_some() {
            document.exit_fullscreen();
        } else {
            let Some(video) = video_element(video) else {
                return;
            };
            if !request_fullscreen(&element, &video) {
                return;
            }
        }
    };
    let mut fullscreen_listener = browser::on_fullscreenchange({
        let shell = shell;
        move |_| {
            let active = web_sys::window()
                .and_then(|window| window.document())
                .and_then(|document| document.fullscreen_element())
                .is_some_and(|element| {
                    shell
                        .get()
                        .is_some_and(|shell| element == shell.unchecked_into::<Element>())
                });
            fullscreen.set(active);
        }
    });

    let on_video_click = {
        let toggle_playback = toggle_playback.clone();
        move |_| {
            if pointer_type.get_untracked() != "mouse" {
                if controls_visible.get_untracked() && playing.get_untracked() {
                    controls_visible.set(false);
                } else {
                    show_video_controls(
                        controls_visible,
                        controls_timer,
                        playing,
                        starting,
                        buffering,
                        error,
                        false,
                    );
                }
                return;
            }
            clear_timer(click_timer);
            if let Some(window) = web_sys::window() {
                let callback = Closure::once_into_js(move || toggle_playback());
                if let Ok(id) = window.set_timeout_with_callback_and_timeout_and_arguments_0(
                    callback.unchecked_ref(),
                    200,
                ) {
                    click_timer.set(Some(id));
                }
            }
        }
    };
    let on_video_double_click = {
        let toggle_fullscreen = toggle_fullscreen.clone();
        move |event: MouseEvent| {
            if pointer_type.get_untracked() == "mouse" {
                event.prevent_default();
                clear_timer(click_timer);
                toggle_fullscreen();
            }
        }
    };
    let on_controls_pointer_enter = move |event: PointerEvent| {
        if event.pointer_type() == "mouse" {
            controls_hovered.set(true);
            show_video_controls(
                controls_visible,
                controls_timer,
                playing,
                starting,
                buffering,
                error,
                false,
            );
        }
    };
    let on_controls_pointer_leave = move |_: PointerEvent| {
        controls_hovered.set(false);
        show_video_controls(
            controls_visible,
            controls_timer,
            playing,
            starting,
            buffering,
            error,
            false,
        );
    };
    let on_pointer_down = move |event: PointerEvent| pointer_type.set(event.pointer_type());
    let on_pointer_move = move |event: PointerEvent| {
        if event.pointer_type() == "mouse" {
            show_video_controls(
                controls_visible,
                controls_timer,
                playing,
                starting,
                buffering,
                error,
                false,
            );
        }
    };
    let on_key = {
        let toggle_playback = toggle_playback.clone();
        let seek_to = seek_to.clone();
        let toggle_mute = toggle_mute.clone();
        let toggle_fullscreen = toggle_fullscreen.clone();
        move |event: KeyboardEvent| {
            show_video_controls(
                controls_visible,
                controls_timer,
                playing,
                starting,
                buffering,
                error,
                false,
            );
            if event.default_prevented()
                || event
                    .target()
                    .and_then(|target| target.dyn_into::<Element>().ok())
                    .and_then(|target| target.closest("input, select, button, summary").ok())
                    .flatten()
                    .is_some()
            {
                return;
            }
            match event.key().as_str() {
                " " | "k" => {
                    event.prevent_default();
                    toggle_playback();
                }
                "ArrowLeft" => {
                    event.prevent_default();
                    seek_to(current_time.get_untracked() - 5.0);
                }
                "ArrowRight" => {
                    event.prevent_default();
                    seek_to(current_time.get_untracked() + 5.0);
                }
                "m" => toggle_mute(),
                "f" => toggle_fullscreen(),
                _ => {}
            }
        }
    };

    // The reference VideoControls forwards PreviewMenu's native toggle event
    // back to showControls(). Closing a menu therefore restarts the same
    // auto-hide window as any other control interaction.
    let menu_interact = Callback::new(move |_: bool| {
        show_video_controls(
            controls_visible,
            controls_timer,
            playing,
            starting,
            buffering,
            error,
            false,
        );
    });

    // Discover metadata and progress independently, just as the audio player
    // does. A metadata failure leaves native video playback available.
    {
        let id = item.id.clone();
        leptos::task::spawn_local(async move {
            if let Ok(progress) = api::fetch_media_progress(&id).await {
                server_position.set(if progress.position.is_finite() {
                    progress.position.max(0.0)
                } else {
                    0.0
                });
            }
            progress_loaded.set(true);
            restore_position();
        });
    }
    {
        let id = item.id.clone();
        let video_for_start = video;
        leptos::task::spawn_local(async move {
            if let Ok(value) = api::fetch_video_media(&id).await {
                let defaults: Vec<_> = value
                    .subtitles
                    .iter()
                    .map(|track| (track.default, track.forced))
                    .collect();
                active_subtitle.set(initial_subtitle_index(&defaults));
                subtitles.set(value.subtitles);
            }

            // The Vue reference lets the element's initial `src` load first,
            // then explicitly starts it after subtitle discovery completes.
            // Keeping this second load in the same async lifecycle preserves
            // the reference retry/error timing for native media failures.
            if let Some(video) = video_element(video_for_start) {
                video.set_volume(volume.get_untracked());
                video.set_muted(muted.get_untracked());
                video.set_playback_rate(rate.get_untracked());
                video.load();
                play_video_ignoring_rejection(&video);
            }
        });
    }

    let cleanup_save = save_progress.clone();
    let cleanup_item_id = item_id.clone();
    on_cleanup(move || {
        clear_timer(click_timer);
        clear_timer(controls_timer);
        clear_timer(volume_timer);
        clear_timer(save_timer);
        clear_timer(remote_save_timer);
        resize_listener.release();
        fullscreen_listener.release();
        cleanup_save(false);
        let position = current_time.get_untracked().max(0.0);
        if position > 0.0 {
            api::save_media_progress_keepalive(
                &cleanup_item_id,
                &MediaProgress {
                    position,
                    duration: duration.get_untracked(),
                    updated_at: None,
                },
            );
        }
        if let Some(track) = cue_track.get_untracked() {
            track.set_oncuechange(None);
            track.set_mode(TextTrackMode::Disabled);
        }
        if let Some(video) = video_media_element(video) {
            let _ = video.pause();
            video.set_src("");
            video.load();
        }
    });

    let download_for_menu = on_download.clone();
    let move_for_menu = on_move.clone();
    let copy_for_menu = on_copy.clone();
    let subtitle_rows = move || {
        active_subtitle_lines
            .get()
            .into_iter()
            .enumerate()
            .collect::<Vec<_>>()
    };
    let subtitle_options = move || subtitles.get().into_iter().enumerate().collect::<Vec<_>>();

    view! {
        <div
            node_ref=shell
            class="video-player-shell"
            class:cursor-hidden=cursor_hidden
            tabindex="0"
            on:pointermove=on_pointer_move
            on:pointerdown=on_pointer_down
            on:keydown=on_key
        >
            <video
                node_ref=video
                src=source
                poster=poster
                crossorigin="anonymous"
                autoplay
                playsinline
                preload="metadata"
                on:click=on_video_click
                on:dblclick=on_video_double_click
                on:loadedmetadata=on_loaded_metadata
                on:timeupdate=on_time_update
                on:waiting=on_waiting
                on:stalled=on_waiting
                on:canplay=on_can_play
                on:playing=on_can_play
                on:play=on_play
                on:pause=on_pause.clone()
                on:ended=on_pause
                on:error=on_error
            >
                {move || active_subtitle.get().and_then(|index| subtitles.get().get(index).cloned()).map(|track| view! {
                    <track
                        node_ref=subtitle_element
                        kind="subtitles"
                        src=track.url
                        srclang=track.language
                        label=track.label
                        on:load=on_track_load
                        on:error=on_track_error
                    />
                })}
                "你的浏览器不支持这个视频格式。"
            </video>
            <Show when=move || !active_subtitle_lines.get().is_empty() fallback=|| ()>
                <div class="video-subtitle-overlay" class:top=move || subtitle_placement.get() == SubtitlePlacement::Top class:middle=move || subtitle_placement.get() == SubtitlePlacement::Middle style=move || format!("--subtitle-image-bottom:{}px;--subtitle-image-inset:{}px;", subtitle_bottom.get(), subtitle_inset.get()) aria-live="off">
                    <For each=subtitle_rows key=|(index, _)| *index let:line>
                        <span class:video-subtitle-secondary-line=move || subtitle_line_is_secondary(line.0)>{line.1.clone()}</span>
                    </For>
                </div>
            </Show>
            <div class="video-top-shade" class:visible=move || controls_visible.get() || !playing.get() inert=move || !controls_visible.get() && playing.get()>
                <div class="video-title-group">
                    <button class="video-back" type="button" aria-label="退出播放" on:click={
                        let on_close = on_close.clone();
                        move |_| on_close.run(())
                    }>{icons::chevron_left()}</button>
                    <strong title=item_name.clone()>{item_name.clone()}</strong>
                </div>
            </div>
            <Show when=move || !playing.get() && !starting.get() && error.get().is_empty() fallback=|| ()>
                <button class="video-center-play" type="button" aria-label="播放" on:click={move |_| toggle_playback()}>{icons::play()}</button>
            </Show>
            <Show when=move || starting.get() fallback=move || view! {
                <Show when=move || buffering.get() fallback=|| ()>
                    <div class="video-buffering"><span></span><strong>"正在缓冲"</strong></div>
                </Show>
            }>
                <div class="video-loading"><span></span><strong>"正在准备视频"</strong><small>"准备好后会自动开始播放"</small></div>
            </Show>
            <Show when=move || !error.get().is_empty() fallback=|| ()>
                <div class="video-error" role="alert"><p>{move || error.get()}</p><button type="button" on:click=retry_playback>"重新尝试"</button></div>
            </Show>
            <div
                class="video-controls"
                class:visible=move || controls_visible.get() || !playing.get()
                class:mouse-hover=move || controls_hovered.get()
                class:seek-pending=move || pending_seek.get().is_some()
                inert=move || !controls_visible.get() && playing.get()
                on:pointerenter=on_controls_pointer_enter
                on:pointerleave=on_controls_pointer_leave
            >
                <input class="video-seek" type="range" min="0" max=move || duration.get().max(1.0).to_string() step="0.25" prop:value=move || timeline_position().min(duration.get().max(1.0)).to_string() style=move || format!("--video-progress:{}%;", progress()) aria-label="视频进度" aria-valuetext=move || format_media_time(timeline_position()) prop:disabled={move || duration.get() <= 0.0} on:input=preview_seek on:change=commit_seek on:pointercancel=cancel_seek />
                <div class="video-control-row">
                    <button class="video-icon-button" type="button" aria-label=move || if playing.get() || starting.get() && autoplay_pending.get() { "暂停" } else { "播放" } on:click=move |_| toggle_playback()>
                        {move || if playing.get() || starting.get() && autoplay_pending.get() { icons::pause().into_any() } else { icons::play().into_any() }}
                    </button>
                    <span class="video-time">{move || format_media_time(timeline_position())}<span>{move || format!(" / {}", format_media_time(duration.get()))}</span></span>
                    <div class="video-desktop-volume">
                        <button class="video-icon-button" type="button" aria-label=move || if effective_volume() == 0.0 { "取消静音" } else { "静音" } on:click=move |_| toggle_mute()>
                            {move || volume_icon(effective_volume())}
                        </button>
                        <input class="video-volume" type="range" min="0" max="1" step="0.01" aria-label="音量" aria-valuetext=move || format!("{}%", (effective_volume() * 100.0).round() as i64) prop:value=move || effective_volume().to_string() on:input=change_volume />
                    </div>
                    <span class="video-control-spacer"></span>
                    <Show when=move || !subtitles.get().is_empty() fallback=|| ()>
                        <PreviewMenu label="字幕".to_owned() icon=MenuIcon::Captions on_toggle=menu_interact.clone()>
                            <label class="video-setting"><span>"字幕"</span><select aria-label="字幕轨道" prop:value=move || active_subtitle.get().map_or_else(|| "-1".to_owned(), |index| index.to_string()) on:change={move |event: Event| {
                                let Some(select) = event.target().and_then(|target| target.dyn_into::<HtmlSelectElement>().ok()) else { return; };
                                active_subtitle.set(select.value().parse::<usize>().ok().filter(|index| *index < subtitles.get_untracked().len()));
                            }}>
                                <option value="-1">"关闭字幕"</option>
                                <For each=subtitle_options key=|entry| entry.1.id.clone() let:entry>
                                    <option value=entry.0.to_string()>
                                        {entry.1.label.clone()}
                                    </option>
                                </For>
                            </select></label>
                        </PreviewMenu>
                    </Show>
                    <PreviewMenu label="播放设置".to_owned() icon=MenuIcon::Settings on_toggle=menu_interact>
                        <label class="video-setting"><span>"播放速度"</span><select aria-label="播放速度" prop:value=move || rate.get().to_string() on:change=change_rate>
                            <option value="0.5">"0.5×"</option><option value="0.75">"0.75×"</option><option value="1">"1×"</option><option value="1.25">"1.25×"</option><option value="1.5">"1.5×"</option><option value="2">"2×"</option>
                        </select></label>
                        <label class="video-setting video-mobile-volume"><span>"音量"</span><input type="range" min="0" max="1" step="0.01" aria-label="音量" prop:value=move || effective_volume().to_string() on:input=change_volume /></label>
                        <button type="button" data-close-menu="true" on:click={
                            let on_download = download_for_menu.clone(); let item = item.clone();
                            move |_| on_download.run(item.clone())
                        }>{icons::download()}<span>"下载"</span></button>
                        <button type="button" data-close-menu="true" on:click={
                            let on_move = move_for_menu.clone(); let item = item.clone();
                            move |_| on_move.run(item.clone())
                        }>{icons::move_icon()}<span>"移动"</span></button>
                        <button type="button" data-close-menu="true" on:click={
                            let on_copy = copy_for_menu.clone(); let item = item.clone();
                            move |_| on_copy.run(item.clone())
                        }>{icons::copy()}<span>"复制"</span></button>
                        <p class="media-detail">"原始文件播放"</p>
                    </PreviewMenu>
                    <button class="video-icon-button" type="button" aria-label=move || if fullscreen.get() { "退出全屏" } else { "全屏" } on:click=move |_| toggle_fullscreen()>
                        {move || if fullscreen.get() { icons::minimize().into_any() } else { icons::maximize().into_any() }}
                    </button>
                </div>
            </div>
        </div>
    }
}

/// Request the same hidden-navigation fullscreen mode as the reference
/// player, with the old WebKit video fallback for mobile Safari.
fn request_fullscreen(element: &Element, video: &HtmlVideoElement) -> bool {
    let options = Object::new();
    let _ = Reflect::set(
        &options,
        &JsValue::from_str("navigationUI"),
        &JsValue::from_str("hide"),
    );
    let request = Reflect::get(element.as_ref(), &JsValue::from_str("requestFullscreen"))
        .ok()
        .and_then(|value| value.dyn_into::<Function>().ok());
    if let Some(request) = request
        && request.call1(element.as_ref(), options.as_ref()).is_ok()
    {
        return true;
    }

    Reflect::get(video.as_ref(), &JsValue::from_str("webkitEnterFullscreen"))
        .ok()
        .and_then(|value| value.dyn_into::<Function>().ok())
        .is_some_and(|enter| enter.call0(video.as_ref()).is_ok())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SubtitlePlacement {
    Top,
    Middle,
    Bottom,
}

fn video_element(node: NodeRef<leptos::html::Video>) -> Option<HtmlVideoElement> {
    node.get().map(|element| element.unchecked_into())
}

fn video_media_element(node: NodeRef<leptos::html::Video>) -> Option<HtmlMediaElement> {
    video_element(node).map(|element| element.unchecked_into())
}

fn safe_duration(value: f64) -> f64 {
    if value.is_finite() && value > 0.0 {
        value
    } else {
        0.0
    }
}

fn volume_icon(value: f64) -> AnyView {
    if value <= 0.0 {
        icons::volume_x().into_any()
    } else if value < 0.5 {
        icons::volume_1().into_any()
    } else {
        icons::volume_2().into_any()
    }
}

fn video_volume() -> f64 {
    browser::local_storage_get("revaro-video-volume")
        .and_then(|value| value.parse::<f64>().ok())
        .filter(|value| value.is_finite())
        .map_or(0.9, |value| value.clamp(0.0, 1.0))
}

fn video_rate() -> f64 {
    let value = browser::local_storage_get("revaro-video-rate")
        .and_then(|value| value.parse::<f64>().ok())
        .unwrap_or(1.0);
    if [0.5, 0.75, 1.0, 1.25, 1.5, 2.0].contains(&value) {
        value
    } else {
        1.0
    }
}

fn thumbnail_url(file: &File) -> String {
    format!(
        "/api/files/{}/thumbnail?v={}",
        file.id,
        js_sys::encode_uri_component(&file.etag)
            .as_string()
            .unwrap_or_default()
    )
}

fn disable_tracks(media: &HtmlMediaElement) {
    if let Some(tracks) = media.text_tracks() {
        for index in 0..tracks.length() {
            if let Some(track) = tracks.get(index) {
                track.set_mode(TextTrackMode::Disabled);
            }
        }
    }
}

fn install_cue_listener(
    track: TextTrack,
    cue_track: RwSignal<Option<TextTrack>>,
    lines: RwSignal<Vec<String>>,
    placement: RwSignal<SubtitlePlacement>,
    listener: &Rc<RefCell<Option<Closure<dyn FnMut(Event)>>>>,
) {
    if let Some(old) = cue_track.get_untracked() {
        old.set_oncuechange(None);
    }
    cue_track.set(Some(track.clone()));
    if listener.borrow().is_none() {
        let callback = Closure::<dyn FnMut(Event)>::new(move |_| {
            update_active_cues(cue_track, lines, placement);
        });
        *listener.borrow_mut() = Some(callback);
    }
    if let Some(callback) = listener.borrow().as_ref() {
        track.set_oncuechange(Some(callback.as_ref().unchecked_ref()));
    }
    update_active_cues(cue_track, lines, placement);
}

fn update_active_cues(
    cue_track: RwSignal<Option<TextTrack>>,
    lines: RwSignal<Vec<String>>,
    placement: RwSignal<SubtitlePlacement>,
) {
    let Some(track) = cue_track.get_untracked() else {
        lines.set(Vec::new());
        return;
    };
    let Some(cues) = track.active_cues() else {
        lines.set(Vec::new());
        return;
    };
    let mut output = Vec::new();
    let mut first_line = 100.0;
    for index in 0..cues.length() {
        let Some(cue) = cues.get(index) else {
            continue;
        };
        if index == 0 && !cue.snap_to_lines() {
            first_line = cue.line().as_f64().unwrap_or(100.0);
        }
        output.extend(clean_cue_text(&cue));
    }
    placement.set(if first_line <= 25.0 {
        SubtitlePlacement::Top
    } else if first_line < 75.0 {
        SubtitlePlacement::Middle
    } else {
        SubtitlePlacement::Bottom
    });
    lines.set(output);
}

fn clean_cue_text(cue: &VttCue) -> Vec<String> {
    cue.text()
        .split('\n')
        .map(strip_cue_markup)
        .filter(|line| !line.is_empty())
        .collect()
}

fn strip_cue_markup(value: &str) -> String {
    // The reference runs each cue line through DOMParser and reads
    // `body.textContent`. That decodes the full HTML entity set (not just the
    // five common entities) and removes cue markup before trimming it.
    let decoded = web_sys::window()
        .and_then(|window| window.document())
        .and_then(|document| document.create_element("div").ok())
        .map(|container| {
            container.set_inner_html(value);
            container.text_content().unwrap_or_default()
        })
        .unwrap_or_else(|| value.to_owned());
    decoded.trim().to_owned()
}

fn clear_timer(signal: RwSignal<Option<i32>>) {
    if let Some(timer) = signal.get_untracked()
        && let Some(window) = web_sys::window()
    {
        window.clear_timeout_with_handle(timer);
    }
    signal.set(None);
}

fn show_video_controls(
    visible: RwSignal<bool>,
    timer: RwSignal<Option<i32>>,
    playing: RwSignal<bool>,
    starting: RwSignal<bool>,
    buffering: RwSignal<bool>,
    error: RwSignal<String>,
    persist: bool,
) {
    visible.set(true);
    clear_timer(timer);
    if !persist && playing.get_untracked() {
        if let Some(window) = web_sys::window() {
            let callback = Closure::once_into_js(move || {
                if !playing.get_untracked()
                    || starting.get_untracked()
                    || buffering.get_untracked()
                    || !error.get_untracked().is_empty()
                {
                    return;
                }
                let focus_or_menu_active = web_sys::window()
                    .and_then(|window| window.document())
                    .and_then(|document| {
                        document
                            .query_selector(".video-player-shell")
                            .ok()
                            .flatten()
                    })
                    .is_some_and(|shell| {
                        shell
                            .query_selector("details[open], :focus-visible, .video-controls.mouse-hover, .video-controls.seek-pending")
                            .ok()
                            .flatten()
                            .is_some()
                    });
                if focus_or_menu_active {
                    show_video_controls(visible, timer, playing, starting, buffering, error, false);
                    return;
                }
                visible.set(false);
            });
            if let Ok(id) = window.set_timeout_with_callback_and_timeout_and_arguments_0(
                callback.unchecked_ref(),
                2_800,
            ) {
                timer.set(Some(id));
            }
        }
    }
}

fn schedule_progress_timer<F>(signal: RwSignal<Option<i32>>, delay: i32, callback: F)
where
    F: Fn() + 'static,
{
    clear_timer(signal);
    if let Some(window) = web_sys::window() {
        let callback = Closure::once_into_js(move || {
            signal.set(None);
            callback();
        });
        if let Ok(id) = window
            .set_timeout_with_callback_and_timeout_and_arguments_0(callback.unchecked_ref(), delay)
        {
            signal.set(Some(id));
        }
    }
}

fn schedule_remote_timer<F>(signal: RwSignal<Option<i32>>, delay: i32, callback: F)
where
    F: Fn() + 'static,
{
    if signal.get_untracked().is_some() {
        return;
    }
    if let Some(window) = web_sys::window() {
        let callback = Closure::once_into_js(move || {
            signal.set(None);
            callback();
        });
        if let Ok(id) = window
            .set_timeout_with_callback_and_timeout_and_arguments_0(callback.unchecked_ref(), delay)
        {
            signal.set(Some(id));
        }
    }
}

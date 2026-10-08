//! One audio element for the lifetime of the authenticated application.
use leptos::prelude::*;
use revaro_core::media::AudioMedia;
use revaro_core::model::File;
use serde::{Deserialize, Serialize};
use wasm_bindgen::JsCast;
use wasm_bindgen::closure::Closure;

use super::audio_timeline::AudioSeekInput;
use super::icons;
use super::menu::{ActionMenu, MenuIcon};
use super::playback::{PlaybackProgress, ProgressDestination, persist_progress, stored_volume};
use super::resource_url::thumbnail_url;
use crate::{
    api, browser,
    logic::{
        format::format_media_time,
        library::{PlaybackMode, next_index},
        media::{
            active_chapter_index, audio_duration, clamp_percent, media_element_time,
            timeline_chapters,
        },
    },
};

#[derive(Serialize, Deserialize)]
struct ListeningSession {
    collection: Option<(String, String)>,
    tracks: Vec<String>,
    track: String,
    position: f64,
}

#[derive(Clone, Copy)]
pub struct MusicController {
    pub queue: RwSignal<Vec<File>>,
    pub index: RwSignal<usize>,
    pub audio: NodeRef<leptos::html::Audio>,
    pub playing: RwSignal<bool>,
    pub position: RwSignal<f64>,
    pub duration: RwSignal<f64>,
    pub media_ready: RwSignal<bool>,
    pub error: RwSignal<String>,
    pub mode: RwSignal<PlaybackMode>,
    pub collection: RwSignal<Option<(String, String)>>,
    pub metadata: RwSignal<AudioMedia>,
    pub buffered: RwSignal<Vec<(f64, f64)>>,
    pub full_open: RwSignal<bool>,
    pub panel_open: RwSignal<bool>,
    pub waiting: RwSignal<bool>,
    pub volume: RwSignal<f64>,
    pub muted: RwSignal<bool>,
    pub rate: RwSignal<f64>,
    session_key: StoredValue<String>,
    autoplay_requested: RwSignal<bool>,
    progress: PlaybackProgress,
    pending_seek: RwSignal<Option<f64>>,
}

impl MusicController {
    pub fn new(username: &str) -> Self {
        Self {
            queue: RwSignal::new(Vec::new()),
            index: RwSignal::new(0),
            audio: NodeRef::new(),
            playing: RwSignal::new(false),
            position: RwSignal::new(0.0),
            duration: RwSignal::new(0.0),
            media_ready: RwSignal::new(false),
            error: RwSignal::new(String::new()),
            mode: RwSignal::new(PlaybackMode::default()),
            collection: RwSignal::new(None),
            metadata: RwSignal::new(AudioMedia::default()),
            buffered: RwSignal::new(Vec::new()),
            full_open: RwSignal::new(false),
            panel_open: RwSignal::new(false),
            waiting: RwSignal::new(false),
            volume: RwSignal::new(stored_volume("revaro-music-volume", 0.8)),
            muted: RwSignal::new(false),
            rate: RwSignal::new(1.0),
            session_key: StoredValue::new(format!("revaro-listening:{username}")),
            autoplay_requested: RwSignal::new(false),
            progress: PlaybackProgress::new(),
            pending_seek: RwSignal::new(None),
        }
    }
    pub fn current(self) -> Option<File> {
        self.queue.get().get(self.index.get()).cloned()
    }
    fn element(self) -> Option<web_sys::HtmlAudioElement> {
        self.audio.get().map(|e| e.unchecked_into())
    }
    pub fn save(self) {
        if !self.progress.ready.get_untracked() {
            return;
        }
        if let Some(file) = self.current()
            && self.progress.loaded_file.get_untracked().as_deref() == Some(&file.id)
        {
            let position = self.pending_seek.get_untracked().unwrap_or_else(|| {
                self.element()
                    .map_or(self.position.get_untracked(), |audio| {
                        media_element_time(audio.current_time())
                    })
            });
            let duration = self.duration.get_untracked();
            persist_progress(
                &file.id,
                position,
                duration,
                Some(&format!("revaro-audio-position:{}", file.id)),
                ProgressDestination::Keepalive,
            );
            let session = ListeningSession {
                collection: self.collection.get_untracked(),
                tracks: self
                    .queue
                    .get_untracked()
                    .into_iter()
                    .map(|f| f.id)
                    .collect(),
                track: file.id,
                position,
            };
            if let Ok(value) = serde_json::to_string(&session) {
                let key = self.session_key.get_value();
                browser::local_storage_set(&key, &value);
                if let Some((id, _)) = session.collection {
                    browser::local_storage_set(&format!("{key}:collection:{id}"), &value);
                }
            }
        }
    }
    fn prepare(self, id: &str, restart: bool) {
        let resume = if restart {
            Some(0.0)
        } else if self.progress.ready.get_untracked()
            && self.progress.loaded_file.get_untracked().as_deref() == Some(id)
        {
            self.element()
                .map(|audio| media_element_time(audio.current_time()))
        } else {
            None
        };
        self.save();
        // Disable writes before changing the queue or audio source: load()
        // can dispatch pause/timeupdate events for the previous source.
        self.progress.reset(resume);
        self.pending_seek.set(None);
        self.media_ready.set(false);
        self.autoplay_requested.set(true);
        self.playing.set(false);
    }
    pub fn play(self, file: File, queue: Vec<File>) {
        self.play_collection(file, queue, None);
    }
    pub fn play_collection(
        self,
        file: File,
        queue: Vec<File>,
        collection: Option<(String, String)>,
    ) {
        if self.current().is_none() {
            self.panel_open.set(false);
        }
        self.prepare(&file.id, false);
        let mut queue = queue;
        if !queue.iter().any(|f| f.id == file.id) {
            queue.insert(0, file.clone());
        }
        let index = queue.iter().position(|f| f.id == file.id).unwrap_or(0);
        self.mode.update(|mode| *mode = mode.for_queue(queue.len()));
        self.queue.set(queue);
        self.index.set(index);
        self.collection.set(collection);
        self.progress.revision.update(|r| *r += 1);
    }
    pub fn select(self, index: usize, restart: bool) {
        let Some(file) = self.queue.get_untracked().get(index).cloned() else {
            return;
        };
        self.prepare(&file.id, restart);
        self.index.set(index);
        self.progress.revision.update(|r| *r += 1);
    }
    pub fn advance(self, direction: i32, ended: bool) {
        let len = self.queue.get_untracked().len();
        let index = self.index.get_untracked();
        let mode = self.mode.get_untracked().for_queue(len);
        let next = if ended && mode == PlaybackMode::RepeatOne {
            Some(index)
        } else if mode == PlaybackMode::Shuffle && len > 1 {
            Some((index + 1 + (js_sys::Math::random() * (len - 1) as f64).floor() as usize) % len)
        } else {
            next_index(index, len, direction, mode == PlaybackMode::RepeatAll)
        };
        if let Some(next) = next {
            self.select(next, ended);
        } else if ended {
            self.stop();
        }
    }
    /// End this session without erasing the track's saved listening progress.
    pub fn stop(self) {
        self.save();
        self.progress.reset(None);
        self.progress.revision.update(|revision| *revision += 1);
        self.pause();
        self.queue.set(Vec::new());
        self.index.set(0);
        self.collection.set(None);
        self.position.set(0.0);
        self.duration.set(0.0);
        self.media_ready.set(false);
        self.pending_seek.set(None);
        self.metadata.set(AudioMedia::default());
        self.buffered.set(Vec::new());
        self.error.set(String::new());
        self.waiting.set(false);
        self.playing.set(false);
        self.panel_open.set(false);
        self.full_open.set(false);
        self.mode.set(PlaybackMode::default());
        self.rate.set(1.0);
        self.muted.set(false);
        browser::local_storage_set(&self.session_key.get_value(), "");
        if let Some(audio) = self.element() {
            let _ = audio.remove_attribute("src");
            audio.load();
        }
    }
    fn restore_position(self) {
        if self.progress.ready.get_untracked() {
            return;
        }
        let Some(audio) = self.element() else {
            return;
        };
        if audio.ready_state() < 1 || self.progress.loaded_file.get_untracked().is_none() {
            return;
        }
        let duration = self.duration.get_untracked();
        if duration <= 0.0 {
            return;
        }
        let key = self
            .progress
            .loaded_file
            .get_untracked()
            .map(|id| format!("revaro-audio-position:{id}"));
        let Some(position) = self.progress.restore(duration, key.as_deref()) else {
            return;
        };
        self.pending_seek.set(Some(position));
        audio.set_current_time(position);
        self.position.set(position);
        self.update_buffer();
        if self.autoplay_requested.get_untracked() {
            self.start();
        }
    }
    fn sync_duration(self) {
        let native = self.element().map_or(0.0, |audio| {
            let ready = audio.ready_state() >= 1;
            self.media_ready.set(ready);
            if ready { audio.duration() } else { 0.0 }
        });
        let duration = audio_duration(native, self.metadata.with_untracked(|media| media.duration));
        if duration > 0.0 {
            self.duration.set(duration);
        }
        self.update_buffer();
        self.restore_position();
    }
    fn sync_position(self) {
        if !self.progress.ready.get_untracked() || self.pending_seek.get_untracked().is_some() {
            return;
        }
        if let Some(audio) = self.element() {
            self.position.set(media_element_time(audio.current_time()));
        }
    }
    pub fn can_seek(self) -> bool {
        self.media_ready.get() && self.duration.get() > 0.0
    }
    fn update_buffer(self) {
        let Some(audio) = self.element() else {
            return;
        };
        let total = self.duration.get_untracked();
        if total <= 0.0 {
            return;
        }
        let ranges = audio.buffered();
        self.buffered.set(
            (0..ranges.length())
                .filter_map(|i| {
                    Some((
                        clamp_percent(ranges.start(i).ok()? / total * 100.0),
                        clamp_percent(ranges.end(i).ok()? / total * 100.0),
                    ))
                })
                .collect(),
        );
    }
    pub fn seek(self, position: f64) {
        if !position.is_finite() || self.duration.get_untracked() <= 0.0 {
            return;
        }
        if let Some(audio) = self.element()
            && audio.ready_state() >= 1
        {
            let was_ready = self.progress.ready.get_untracked();
            let position = media_element_time(position).min(self.duration.get_untracked());
            self.pending_seek.set(Some(position));
            audio.set_current_time(position);
            self.progress.accept_seek();
            self.position.set(position);
            self.save();
            if !was_ready && self.autoplay_requested.get_untracked() {
                self.start();
            }
        }
    }
    pub fn pause(self) {
        self.autoplay_requested.set(false);
        if let Some(audio) = self.element() {
            let _ = audio.pause();
        }
    }
    pub fn toggle(self) {
        if let Some(audio) = self.element() {
            if audio.paused() {
                self.start();
            } else {
                self.pause();
            }
        }
    }
    pub fn start(self) {
        self.autoplay_requested.set(true);
        if !self.progress.ready.get_untracked() {
            return;
        }
        if let Some(audio) = self.element()
            && let Ok(promise) = audio.play()
        {
            let revision = self.progress.revision.get_untracked();
            leptos::task::spawn_local(async move {
                if wasm_bindgen_futures::JsFuture::from(promise).await.is_err()
                    && self.progress.revision.try_get_untracked() == Some(revision)
                    && self.autoplay_requested.try_get_untracked() == Some(true)
                {
                    self.error
                        .set("点击播放以开始，或检查浏览器是否支持此音频格式".to_owned());
                }
            });
        }
    }
}

/// The full-screen player uses a native keyboard picker for the shared mode.
#[component]
pub(super) fn PlaybackModeControl(controller: MusicController) -> impl IntoView {
    let multiple = move || controller.queue.with(|queue| queue.len() > 1);
    view! {
        <label class="playback-mode">
            <span class="media-sr-only">"播放模式"</span>
            <select aria-label="播放模式" title="播放模式" prop:value=move || controller.mode.get().value()
                on:change=move |event| {
                    controller.mode.set(PlaybackMode::from_value(&event_target_value(&event)).for_queue(controller.queue.with(|queue| queue.len())));
                }>
                <option value="sequential">{move || if multiple() { "顺序播放" } else { "播放一次" }}</option>
                <Show when=multiple fallback=|| ()>
                    <option value="shuffle">"随机播放"</option>
                    <option value="repeat-all">"列表循环"</option>
                </Show>
                <option value="repeat-one">"单曲循环"</option>
            </select>
        </label>
    }
}

/// The floating player's mode picker shares dismissal with its other submenus.
#[component]
fn PlaybackModeMenu(controller: MusicController) -> impl IntoView {
    let label = move |mode| match mode {
        PlaybackMode::Sequential if controller.queue.with(|queue| queue.len() <= 1) => "播放一次",
        PlaybackMode::Sequential => "顺序播放",
        PlaybackMode::Shuffle => "随机播放",
        PlaybackMode::RepeatAll => "列表循环",
        PlaybackMode::RepeatOne => "单曲循环",
    };
    view! {
        <div class="playback-mode" data-playback-mode=move ||controller.mode.get().value()>
            <ActionMenu label="播放模式".to_owned() icon=MenuIcon::More scope="music-dock" text=Signal::derive(move ||label(controller.mode.get()).to_owned())
                context=Signal::derive(move ||format!("{}:{}",controller.panel_open.get(),controller.full_open.get()))>
                <For each=move || {[PlaybackMode::Sequential,PlaybackMode::Shuffle,PlaybackMode::RepeatAll,PlaybackMode::RepeatOne].into_iter()
                    .filter(|mode|controller.queue.with(|queue|queue.len()>1) ||!matches!(mode,PlaybackMode::Shuffle|PlaybackMode::RepeatAll)).collect::<Vec<_>>()}
                    key=|mode|mode.value() children=move |mode|view! {
                        <button type="button" data-close-menu aria-pressed=move ||(controller.mode.get()==mode).to_string()
                            class:active=move ||controller.mode.get()==mode on:click=move |_|controller.mode.set(mode)>{move ||label(mode)}</button>
                    } />
            </ActionMenu>
        </div>
    }
}

#[component]
pub fn PersistentMusicPlayer(controller: MusicController) -> impl IntoView {
    let chapters_open = RwSignal::new(false);
    let pointer_interaction = RwSignal::new(false);
    let collapse_control = NodeRef::<leptos::html::Button>::new();
    let expand_control = NodeRef::<leptos::html::Button>::new();
    let panel = NodeRef::<leptos::html::Aside>::new();
    let panel_hidden =
        Signal::derive(move || !controller.panel_open.get() || controller.full_open.get());
    let orb_hidden =
        Signal::derive(move || controller.panel_open.get() || controller.full_open.get());
    let child_open = || {
        web_sys::window().and_then(|window|window.document())
        .is_some_and(|document|document.query_selector("[data-popover-scope=\"music-dock\"][open], [data-popover-scope=\"music-dock\"][data-popover-open=\"true\"]").ok().flatten().is_some())
    };
    // Wait for presentation attributes (including inert) before restoring focus.
    let focus_control = |control: NodeRef<leptos::html::Button>| {
        if let Some(window) = web_sys::window() {
            let callback = Closure::once_into_js(move || {
                if let Some(control) = control.get() {
                    let options = web_sys::FocusOptions::new();
                    options.set_prevent_scroll(true);
                    let _ = control.focus_with_options(&options);
                }
            });
            let _ = window
                .set_timeout_with_callback_and_timeout_and_arguments_0(callback.unchecked_ref(), 0);
        }
    };
    let close_panel = move |restore_focus| {
        controller.panel_open.set(false);
        if restore_focus {
            pointer_interaction.set(false);
            focus_control(expand_control);
        }
    };
    let inside = move |event: &web_sys::Event| {
        browser::event_inside(event, |target| {
            panel
                .get_untracked()
                .is_some_and(|panel| panel.contains(Some(target)))
                || target.dyn_ref::<web_sys::Element>().is_some_and(|element| {
                    element
                        .closest("[data-popover-scope=\"music-dock\"]")
                        .ok()
                        .flatten()
                        .is_some()
                })
        })
    };
    // Snapshot before focusout can close a child and before a range drag leaves the card.
    let pointer_origin = StoredValue::new((false, false));
    let mut pointer = browser::on_window_capture("pointerdown", move |event| {
        pointer_origin.set_value(if panel_hidden.get_untracked() {
            (false, false)
        } else {
            (child_open(), inside(&event))
        });
    });
    let mut outside = browser::on_window_capture("click", move |event| {
        let (from_child, from_card) = pointer_origin.get_value();
        pointer_origin.set_value((false, false));
        if !panel_hidden.get_untracked()
            && !from_child
            && !from_card
            && !child_open()
            && !inside(&event)
        {
            close_panel(false);
        }
    });
    let mut escape = browser::on_window_capture("keydown", move |event| {
        if !panel_hidden.get_untracked()
            && !child_open()
            && let Some(event) = event.dyn_ref::<web_sys::KeyboardEvent>()
            && event.key() == "Escape"
            && !event.default_prevented()
        {
            event.prevent_default();
            event.stop_propagation();
            close_panel(true);
        }
    });
    let mut dock_keyboard =
        browser::on_document_keydown_capture(move |_| pointer_interaction.set(false));
    on_cleanup(move || {
        dock_keyboard.release();
        pointer.release();
        outside.release();
        escape.release();
    });
    let set_panel_open = move |open, event: leptos::ev::MouseEvent| {
        pointer_interaction.set(event.detail() != 0);
        controller.panel_open.set(open);
        // Only presentation changes; the native audio and controller stay mounted.
        let control = if open {
            collapse_control
        } else {
            expand_control
        };
        focus_control(control);
    };
    let seek_preview = RwSignal::new(None::<f64>);
    let next_audio = NodeRef::<leptos::html::Audio>::new();
    let cover_failed = RwSignal::new(false);
    let last_save = RwSignal::new(0.0);
    let volume = controller.volume;
    Effect::new(move |_| {
        let revision = controller.progress.revision.get();
        let Some(audio) = controller.element() else {
            return;
        };
        let Some(file) = untrack(|| controller.current()) else {
            return;
        };
        controller.position.set(0.0);
        controller.duration.set(0.0);
        controller.error.set(String::new());
        controller.metadata.set(AudioMedia::default());
        controller.buffered.set(Vec::new());
        if !controller.panel_open.get_untracked() {
            chapters_open.set(false);
        }
        controller.waiting.set(false);
        seek_preview.set(None);
        cover_failed.set(false);
        controller.progress.loaded_file.set(Some(file.id.clone()));
        audio.set_src(&format!("/api/files/{}/preview", file.id));
        audio.set_volume(volume.get_untracked());
        audio.set_muted(controller.muted.get_untracked());
        audio.set_playback_rate(controller.rate.get_untracked());
        audio.load();
        last_save.set(js_sys::Date::now());
        controller.progress.load(
            file.id.clone(),
            Callback::new(move |()| controller.restore_position()),
        );
        let id = file.id;
        let metadata_id = id.clone();
        leptos::task::spawn_local_scoped_with_cancellation(async move {
            if let Ok(metadata) = api::fetch_audio_media(&metadata_id).await
                && controller.progress.revision.try_get_untracked() == Some(revision)
            {
                controller.metadata.set(metadata);
                controller.sync_duration();
            }
        });
        leptos::task::spawn_local_scoped_with_cancellation(async move {
            let _ = api::update_library_item(
                &id,
                &revaro_core::library::ItemUpdate {
                    favorite: None,
                    opened: true,
                },
            )
            .await;
        });
    });
    // A separate, silent element only asks the browser for the next track's
    // metadata. Seeking always changes the primary element's currentTime.
    Effect::new(move |_| {
        let next = controller
            .queue
            .get()
            .get(controller.index.get() + 1)
            .cloned();
        if let Some(element) = next_audio.get() {
            let audio: web_sys::HtmlAudioElement = element.unchecked_into();
            if let Some(file) = next {
                audio.set_src(&format!("/api/files/{}/preview", file.id));
            } else {
                let _ = audio.remove_attribute("src");
            }
            audio.load();
        }
    });
    // Refresh saved identities so removed files cannot return in the queue.
    if let Some(saved) = browser::local_storage_get(&controller.session_key.get_value())
        .and_then(|s| serde_json::from_str::<ListeningSession>(&s).ok())
    {
        leptos::task::spawn_local_scoped_with_cancellation(async move {
            let collection = if let Some((id, _)) = &saved.collection {
                api::fetch_collections().await.ok().and_then(|collections| {
                    collections
                        .into_iter()
                        .find(|c| c.id == *id && c.kind == "audio")
                        .map(|c| (c.id, c.name))
                })
            } else {
                None
            };
            let mut queue = Vec::new();
            for id in &saved.tracks {
                if let Ok(detail) = api::fetch_file(id).await
                    && revaro_core::classify::is_audio(&detail.file)
                {
                    queue.push(detail.file);
                }
            }
            if controller.progress.revision.get_untracked() != 0 || queue.is_empty() {
                return;
            }
            let index = queue.iter().position(|f| f.id == saved.track).unwrap_or(0);
            let position = if queue[index].id == saved.track {
                media_element_time(saved.position)
            } else {
                0.0
            };
            controller.progress.reset_saved(position);
            controller.collection.set(collection);
            controller.queue.set(queue);
            controller.index.set(index);
            controller.progress.revision.update(|r| *r += 1);
        });
    }
    let mut pagehide = browser::on_pagehide(move |_| controller.save());
    on_cleanup(move || {
        pagehide.release();
        controller.save();
        controller.progress.reset(None);
        controller.pause();
        if let Some(audio) = controller.element() {
            audio.set_src("");
            audio.load();
        }
    });
    view! {
        <audio node_ref=controller.audio preload="metadata"
            on:loadedmetadata=move |_|controller.sync_duration()
            on:durationchange=move |_|controller.sync_duration()
            on:timeupdate=move |_| {
                if !controller.progress.ready.get_untracked() {return;}
                controller.sync_position();
                let now=js_sys::Date::now();
                if now-last_save.get_untracked()>5000.0 {last_save.set(now);controller.save();}
            }
            on:play=move |_| {if controller.current().is_some() {controller.playing.set(true);controller.error.set(String::new());}}
            on:pause=move |_| {controller.playing.set(false);controller.save();}
            on:progress=move |_|controller.update_buffer()
            on:seeked=move |_| {
                if controller.element().is_some_and(|audio|audio.seeking()) {return;}
                controller.pending_seek.set(None);controller.sync_position();controller.update_buffer();controller.save();
            }
            on:waiting=move |_|controller.waiting.set(true)
            on:canplay=move |_| {controller.sync_duration();controller.waiting.set(false);}
            on:ended=move |_| controller.advance(1,true)
            on:error=move |_| {if controller.current().is_some() {controller.progress.ready.set(false);controller.playing.set(false);controller.error.set("此音频无法播放，可在文件管理中下载原文件".to_owned());}}
        ></audio>
        <audio node_ref=next_audio preload="metadata" aria-hidden="true"></audio>
        <Show when=move || controller.current().is_some() fallback=|| ()>
            <div class="music-orb-shell" class:is-hidden=move ||orb_hidden.get() class:pointer-interaction=move ||pointer_interaction.get()
                aria-hidden=move ||orb_hidden.get().to_string() inert=move ||orb_hidden.get()
                on:pointerdown=move |_|pointer_interaction.set(true)>
                <button node_ref=expand_control class="music-orb" type="button" aria-label="展开播放器" aria-controls="music-dock" aria-haspopup="dialog" aria-expanded="false"
                    title=move ||format!("{} · {}",controller.current().map(|file|display_title(&file.name)).unwrap_or_default(),if controller.playing.get(){"正在播放"}else{"已暂停"})
                    on:click=move |event|set_panel_open(true,event)>
                    <svg class="music-orb-ring" viewBox="0 0 100 100" aria-hidden="true">
                        <circle class="music-orb-track" cx="50" cy="50" r="46" fill="none" stroke-width="3.5"></circle>
                        <circle class="music-orb-progress" cx="50" cy="50" r="46" fill="none" stroke-width="3.5" pathLength="100" stroke-dasharray="100"
                            style:stroke-dashoffset=move ||(100.0-clamp_percent(controller.position.get()/controller.duration.get()*100.0)).to_string()></circle>
                    </svg>
                    <span class="music-orb-cover">
                        <Show when=move || !cover_failed.get() fallback=|| icons::music_2().into_any()>
                            <img src=move ||controller.current().map(|file|thumbnail_url(&file)).unwrap_or_default() alt="" on:error=move |_|cover_failed.set(true) />
                        </Show>
                    </span>
                    <span class="media-sr-only">{move ||format!("{}，{} / {}",controller.current().map(|file|display_title(&file.name)).unwrap_or_default(),format_media_time(controller.position.get()),format_media_time(controller.duration.get()))}</span>
                </button>
            </div>
            <aside node_ref=panel id="music-dock" class="music-dock" class:is-expanded=move ||controller.panel_open.get() && !controller.full_open.get() class:pointer-interaction=move ||pointer_interaction.get()
                role="dialog" aria-modal="false" aria-label="详细音频播放器" aria-hidden=move ||panel_hidden.get().to_string() inert=move ||panel_hidden.get()
                on:pointerdown=move |_|pointer_interaction.set(true)>
                <header class="dock-header">
                    <strong>{move ||if controller.playing.get(){"正在播放"}else{"已暂停"}}</strong>
                    <ActionMenu label="播放器更多操作".to_owned() icon=MenuIcon::More scope="music-dock" context=Signal::derive(move ||panel_hidden.get().to_string())>
                        <button type="button" data-close-menu on:click=move |_|controller.full_open.set(true)>{icons::maximize()}"打开全屏播放器"</button>
                        <button type="button" class="player-end" data-close-menu on:click=move |_|controller.stop()>"结束播放"</button>
                    </ActionMenu>
                    <button node_ref=collapse_control class="dock-collapse" type="button" aria-label="收起播放器" title="收起播放器" aria-controls="music-dock" aria-expanded="true"
                        on:click=move |event|set_panel_open(false,event)>{icons::chevron_down()}</button>
                </header>
                <div class="dock-body">
                    <div class="dock-track">
                        <button type="button" class="dock-cover" aria-label="打开音频播放器" on:click=move |_|controller.full_open.set(true)>
                            <Show when=move || !cover_failed.get() fallback=|| icons::music_2().into_any()>
                                <img src=move ||controller.current().map(|file|thumbnail_url(&file)).unwrap_or_default() alt="" on:error=move |_|cover_failed.set(true) />
                            </Show>
                        </button>
                        <div><strong>{move ||controller.current().map(|file|display_title(&file.name)).unwrap_or_default()}</strong>
                            <small>{move ||if controller.error.get().is_empty(){controller.collection.get().map(|(_,name)|format!("{name} · 第 {} / {} 轨",controller.index.get()+1,controller.queue.get().len())).unwrap_or_else(||"正在播放你的音乐".to_owned())}else{controller.error.get()}}</small>
                        </div>
                    </div>
                    <super::audio::AudioLyricPreview cues=Signal::derive(move ||controller.metadata.get().subtitles) current_time=controller.position.into() />
                    <div class="dock-progress"><span>{move ||format_media_time(seek_preview.get().unwrap_or(controller.position.get()))}</span>
                        <div class="dock-timeline">
                            <div class="dock-timeline-track" aria-hidden="true">
                                <For each=move ||controller.buffered.get() key=|(start,end)|(start.to_bits(),end.to_bits()) children=|(start,end)|view! { <span class="dock-buffer" style=format!("left:{start}%;width:{}%;",end-start)></span> } />
                                <span class="dock-played" style:width=move ||format!("{}%",clamp_percent(seek_preview.get().unwrap_or(controller.position.get())/controller.duration.get()*100.0))></span>
                            </div>
                            <AudioSeekInput label="音乐播放进度" duration=controller.duration.into()
                                enabled=Signal::derive(move ||controller.can_seek())
                                position=Signal::derive(move ||seek_preview.get().unwrap_or(controller.position.get()))
                                on_preview=Callback::new(move |value|seek_preview.set(Some(value)))
                                on_commit=Callback::new(move |value| {controller.seek(value);seek_preview.set(None);})
                                on_cancel=Callback::new(move |()|seek_preview.set(None)) />
                            <For each=move ||controller.metadata.with(|media|timeline_chapters(&media.chapters,controller.duration.get()))
                                key=|chapter|chapter.start.to_bits() children=move |chapter|view! {
                                <button type="button" class="dock-chapter-marker" title=chapter.title.clone()
                                    aria-label=format!("跳转章节：{}",chapter.title) data-chapter-start=chapter.start.to_string()
                                    prop:disabled=move ||!controller.can_seek()
                                    style:left=move ||format!("{}%",clamp_percent(chapter.start/controller.duration.get()*100.0))
                                    on:click=move |_|controller.seek(chapter.start)></button>
                            } />
                        </div>
                        <span>{move ||format_media_time(controller.duration.get())}</span>
                    </div>
                    <Show when=move ||chapters_open.get() fallback=|| ()>
                        <section class="music-queue music-chapters" aria-label="当前音轨章节"><header><strong>"章节"</strong><button class="chapter-panel-close" type="button" aria-label="关闭章节" title="关闭章节" on:click=move |_|chapters_open.set(false)>{icons::chevron_up()}</button></header>
                            <Show when=move ||controller.metadata.get().chapters.is_empty() fallback=|| ()><p>"暂无章节"</p></Show>
                            <For each=move ||{controller.metadata.get().chapters.into_iter().enumerate().collect::<Vec<_>>()} key=|(index,chapter)|(*index,chapter.start.to_bits()) children=move |(index,chapter)|view! {
                                <button type="button" data-chapter-index=index aria-current=move ||if controller.metadata.with(|media|active_chapter_index(&media.chapters,controller.position.get()))==index{Some("true")}else{None}
                                    class:active=move ||controller.metadata.with(|media|active_chapter_index(&media.chapters,controller.position.get()))==index on:click=move |_|controller.seek(chapter.start)>
                                    <span>{format!("{:02}",index+1)}</span><strong>{if chapter.title.is_empty(){format!("第 {} 章",index+1)}else{chapter.title}}</strong><small>{format_media_time(chapter.start)}</small>
                                </button>
                            } />
                        </section>
                    </Show>
                </div>
                <div class="dock-controls">
                    <button class="dock-prev" type="button" aria-label="上一首" prop:disabled=move ||controller.queue.with(|queue|queue.len()<=1) on:click=move |_|controller.advance(-1,false)>{icons::skip_back()}</button>
                    <button class="dock-play" type="button" aria-label=move ||if controller.playing.get(){"暂停音乐"}else{"播放音乐"} on:click=move |_|controller.toggle()>{move ||if controller.playing.get(){icons::pause().into_any()}else{icons::play().into_any()}}</button>
                    <button type="button" aria-label="下一首" prop:disabled=move ||controller.queue.with(|queue|queue.len()<=1) on:click=move |_|controller.advance(1,false)>{icons::skip_forward()}</button>
                </div>
                <footer class="dock-options">
                    <PlaybackModeMenu controller=controller />
                    <button type="button" aria-label="音轨章节" class:active=move ||chapters_open.get() aria-expanded=move ||chapters_open.get().to_string() on:click=move |_|chapters_open.update(|open|*open = !*open)>{icons::list()}"章节"</button>
                    <ActionMenu label="音乐音量设置".to_owned() icon=MenuIcon::Volume volume=volume muted=controller.muted scope="music-dock"
                        context=Signal::derive(move ||panel_hidden.get().to_string()) panel_class="dock-volume-popover">
                        <div class="dock-volume">
                            <button type="button" aria-label=move ||if controller.muted.get(){"取消静音"}else{"静音"} on:click=move |_|{
                                controller.muted.update(|muted|*muted = !*muted);
                                if let Some(audio)=controller.element() {audio.set_muted(controller.muted.get_untracked());}
                            }>{move ||if controller.muted.get(){icons::volume_x().into_any()}else{icons::volume_2().into_any()}}</button>
                            <input aria-label="音乐音量" type="range" min="0" max="1" step="0.05" prop:value=move ||volume.get().to_string() on:input=move |event|{
                                if let Ok(value)=event_target_value(&event).parse::<f64>() {let value=value.clamp(0.0,1.0);volume.set(value);controller.muted.set(value==0.0);browser::local_storage_set("revaro-music-volume",&value.to_string());if let Some(audio)=controller.element(){audio.set_volume(value);audio.set_muted(value==0.0);}}
                            } />
                            <output aria-label="当前音乐音量">{move ||format!("{}%",if controller.muted.get(){0}else{(volume.get()*100.0).round() as u64})}</output>
                        </div>
                    </ActionMenu>
                </footer>
            </aside>
        </Show>
        <Show when=move ||controller.full_open.get() && controller.current().is_some() fallback=|| ()>
            <super::audio::AudioPlayerDialog controller=controller />
        </Show>
    }
}

pub fn display_title(name: &str) -> String {
    name.rsplit_once('.')
        .map_or(name, |(stem, _)| stem)
        .to_owned()
}

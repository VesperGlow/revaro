//! One audio element for the lifetime of the authenticated application.
use leptos::prelude::*;
use revaro_core::media::AudioMedia;
use revaro_core::model::File;
use serde::{Deserialize, Serialize};
use wasm_bindgen::JsCast;

use super::icons;
use super::playback::{PlaybackProgress, ProgressDestination, persist_progress, stored_volume};
use super::resource_url::thumbnail_url;
use crate::{
    api, browser,
    logic::{
        format::format_media_time,
        library::next_index,
        media::{active_chapter_index, clamp_percent, media_element_time},
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
    pub error: RwSignal<String>,
    pub repeat: RwSignal<u8>,
    pub shuffle: RwSignal<bool>,
    pub collection: RwSignal<Option<(String, String)>>,
    pub metadata: RwSignal<AudioMedia>,
    pub buffered: RwSignal<Vec<(f64, f64)>>,
    pub full_open: RwSignal<bool>,
    pub waiting: RwSignal<bool>,
    pub volume: RwSignal<f64>,
    pub muted: RwSignal<bool>,
    pub rate: RwSignal<f64>,
    session_key: StoredValue<String>,
    autoplay_requested: RwSignal<bool>,
    progress: PlaybackProgress,
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
            error: RwSignal::new(String::new()),
            repeat: RwSignal::new(0),
            shuffle: RwSignal::new(false),
            collection: RwSignal::new(None),
            metadata: RwSignal::new(AudioMedia::default()),
            buffered: RwSignal::new(Vec::new()),
            full_open: RwSignal::new(false),
            waiting: RwSignal::new(false),
            volume: RwSignal::new(stored_volume("revaro-music-volume", 0.8)),
            muted: RwSignal::new(false),
            rate: RwSignal::new(1.0),
            session_key: StoredValue::new(format!("revaro-listening:{username}")),
            autoplay_requested: RwSignal::new(false),
            progress: PlaybackProgress::new(),
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
            let (position, duration) = self.element().map_or(
                (self.position.get_untracked(), self.duration.get_untracked()),
                |audio| {
                    (
                        media_element_time(audio.current_time()),
                        media_element_time(audio.duration()),
                    )
                },
            );
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
        self.prepare(&file.id, false);
        let mut queue = queue;
        if !queue.iter().any(|f| f.id == file.id) {
            queue.insert(0, file.clone());
        }
        let index = queue.iter().position(|f| f.id == file.id).unwrap_or(0);
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
        let next = if ended && self.repeat.get_untracked() == 2 {
            Some(index)
        } else if self.shuffle.get_untracked() && len > 1 {
            Some((index + 1 + (js_sys::Math::random() * (len - 1) as f64).floor() as usize) % len)
        } else {
            next_index(index, len, direction, self.repeat.get_untracked() == 1)
        };
        if let Some(next) = next {
            self.select(next, ended);
        } else if ended {
            self.save();
            self.playing.set(false);
        }
    }
    pub fn stop(self) {
        self.save();
        self.progress.reset(None);
        self.progress.revision.update(|r| *r += 1);
        self.pause();
        self.queue.set(Vec::new());
        self.collection.set(None);
        browser::local_storage_set(&self.session_key.get_value(), "");
        self.playing.set(false);
        self.full_open.set(false);
        if let Some(audio) = self.element() {
            audio.set_src("");
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
        let duration = media_element_time(audio.duration());
        let key = self
            .progress
            .loaded_file
            .get_untracked()
            .map(|id| format!("revaro-audio-position:{id}"));
        let Some(position) = self.progress.restore(duration, 0.0, false, key.as_deref()) else {
            return;
        };
        audio.set_current_time(position);
        self.position.set(position);
        self.duration.set(duration);
        self.update_buffer();
        if self.autoplay_requested.get_untracked() {
            self.start();
        }
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
        if !self.progress.ready.get_untracked() {
            return;
        }
        if let Some(audio) = self.element() {
            let position = media_element_time(position).min(self.duration.get_untracked());
            audio.set_current_time(position);
            self.position.set(position);
            self.save();
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
                {
                    self.error
                        .set("点击播放以开始，或检查浏览器是否支持此音频格式".to_owned());
                }
            });
        }
    }
}

#[component]
pub fn PersistentMusicPlayer(controller: MusicController) -> impl IntoView {
    let chapters_open = RwSignal::new(false);
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
            audio.set_src(
                &next
                    .map(|f| format!("/api/files/{}/preview", f.id))
                    .unwrap_or_default(),
            );
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
            controller.progress.reset(Some(position));
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
            on:loadedmetadata=move |_| {if let Some(audio)=controller.element() {controller.duration.set(media_element_time(audio.duration()));controller.restore_position();}}
            on:timeupdate=move |_| {
                if !controller.progress.ready.get_untracked() {return;}
                if let Some(audio)=controller.element() {controller.position.set(audio.current_time());}
                let now=js_sys::Date::now();
                if now-last_save.get_untracked()>5000.0 {last_save.set(now);controller.save();}
            }
            on:play=move |_| {controller.playing.set(true);controller.error.set(String::new());}
            on:pause=move |_| {controller.playing.set(false);controller.save();}
            on:progress=move |_|controller.update_buffer()
            on:seeked=move |_| {controller.update_buffer();controller.save();}
            on:waiting=move |_|controller.waiting.set(true)
            on:canplay=move |_|controller.waiting.set(false)
            on:ended=move |_| controller.advance(1,true)
            on:error=move |_| {controller.progress.ready.set(false);controller.playing.set(false);controller.error.set("此音频无法播放，可在文件管理中下载原文件".to_owned());}
        ></audio>
        <audio node_ref=next_audio preload="metadata" aria-hidden="true"></audio>
        <Show when=move || controller.current().is_some() fallback=|| ()>
            <aside class="music-dock" aria-label="全局音乐播放器" inert=move ||controller.full_open.get()>
                <super::audio::AudioSubtitles cues=Signal::derive(move ||controller.metadata.get().subtitles) current_time=controller.position.into() />
                <div class="dock-track">
                    <button type="button" class="dock-cover" aria-label="打开音频播放器" on:click=move |_| {chapters_open.set(false);controller.full_open.set(true);}>
                        <Show when=move || !cover_failed.get() fallback=|| icons::music_2().into_any()>
                            <img src=move || controller.current().map(|f|thumbnail_url(&f)).unwrap_or_default() alt="" on:error=move |_|cover_failed.set(true) />
                        </Show>
                    </button>
                    <div><strong>{move ||controller.current().map(|f|display_title(&f.name)).unwrap_or_default()}</strong><small>{move ||if controller.error.get().is_empty(){controller.collection.get().map(|(_,name)|format!("{name} · 第 {} / {} 轨",controller.index.get()+1,controller.queue.get().len())).unwrap_or_else(||"正在播放你的音乐".to_owned())}else{controller.error.get()}}</small></div>
                </div>
                <div class="dock-controls">
                    <button class="dock-prev" aria-label="上一首" on:click=move |_|controller.advance(-1,false)>{icons::chevron_left()}</button>
                    <button class="dock-play" aria-label=move ||if controller.playing.get(){"暂停音乐"}else{"播放音乐"} on:click=move |_|controller.toggle()>{move ||if controller.playing.get(){icons::pause().into_any()}else{icons::play().into_any()}}</button>
                    <button aria-label="下一首" on:click=move |_|controller.advance(1,false)>{icons::chevron_right()}</button>
                </div>
                <div class="dock-progress"><span>{move ||format_media_time(controller.position.get())}</span>
                    <div class="dock-timeline">
                    <div class="dock-timeline-track" aria-hidden="true">
                        <For each=move ||controller.buffered.get() key=|(start,end)|(start.to_bits(),end.to_bits()) children=|(start,end)|view! { <span class="dock-buffer" style=format!("left:{start}%;width:{}%;",end-start)></span> } />
                        <span class="dock-played" style:width=move ||format!("{}%",clamp_percent(seek_preview.get().unwrap_or(controller.position.get())/controller.duration.get()*100.0))></span>
                        <For each=move ||controller.metadata.get().chapters key=|c|c.start.to_bits() children=move |chapter|view! {<i class="dock-chapter-marker" title=chapter.title style:left=move ||format!("{}%",clamp_percent(chapter.start/controller.duration.get()*100.0))></i>} />
                    </div>
                    <input aria-label="音乐播放进度" type="range" min="0" max=move ||controller.duration.get().max(1.0).to_string() step="0.1"
                        prop:disabled=move || !controller.progress.ready.get()
                        prop:value=move ||seek_preview.get().unwrap_or(controller.position.get()).to_string()
                        on:input=move |ev| {if let Ok(value)=event_target_value(&ev).parse::<f64>() {seek_preview.set(Some(value));}}
                        on:change=move |ev| {if let Ok(value)=event_target_value(&ev).parse::<f64>() {controller.seek(value);}seek_preview.set(None);}
                        on:pointercancel=move |_|seek_preview.set(None) />
                    </div>
                    <span>{move ||format_media_time(controller.duration.get())}</span>
                </div>
                <div class="dock-options">
                    <button class:active=move ||controller.shuffle.get() aria-label="随机播放" aria-pressed=move ||controller.shuffle.get().to_string() on:click=move |_|controller.shuffle.update(|v|*v = !*v)>"随机"</button>
                    <button aria-label="循环模式" on:click=move |_|controller.repeat.update(|r|*r=(*r+1)%3)>{move ||match controller.repeat.get(){1=>"列表循环",2=>"单曲循环",_=>"顺序"}}</button>
                    <input aria-label="音乐音量" type="range" min="0" max="1" step="0.05" prop:value=move ||volume.get().to_string() on:input=move |ev|{if let Ok(value)=event_target_value(&ev).parse::<f64>() {volume.set(value);browser::local_storage_set("revaro-music-volume",&value.to_string());if let Some(audio)=controller.element(){audio.set_volume(value);}}} />
                    <button aria-label="音轨章节" class:active=move ||chapters_open.get() aria-expanded=move ||chapters_open.get().to_string() on:click=move |_|{chapters_open.update(|v|*v = !*v);}>"章节"</button>
                    <button aria-label="停止音乐" on:click=move |_|controller.stop()>"×"</button>
                </div>
            </aside>
            <Show when=move ||chapters_open.get() fallback=|| ()>
                <section class="music-queue music-chapters" aria-label="当前音轨章节"><header><strong>"章节"</strong><button aria-label="关闭章节" on:click=move |_|chapters_open.set(false)>"×"</button></header>
                    <Show when=move ||controller.metadata.get().chapters.is_empty() fallback=|| ()><p>"暂无章节"</p></Show>
                    <For each=move ||{controller.metadata.get().chapters.into_iter().enumerate().collect::<Vec<_>>()} key=|(i,c)|(*i,c.start.to_bits()) children=move |(i,chapter)|view!{
                        <button data-chapter-index=i aria-current=move ||if controller.metadata.with(|m|active_chapter_index(&m.chapters,controller.position.get()))==i{Some("true")}else{None}
                            class:active=move ||controller.metadata.with(|m|active_chapter_index(&m.chapters,controller.position.get()))==i on:click=move |_|controller.seek(chapter.start)>
                            <span>{format!("{:02}",i+1)}</span><strong>{if chapter.title.is_empty(){format!("第 {} 章",i+1)}else{chapter.title}}</strong><small>{format_media_time(chapter.start)}</small>
                        </button>
                    } />
                </section>
            </Show>
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

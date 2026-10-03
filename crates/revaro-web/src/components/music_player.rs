//! One audio element for the lifetime of the authenticated application.
use leptos::prelude::*;
use revaro_core::model::{File, MediaProgress};
use wasm_bindgen::JsCast;

use super::icons;
use crate::{
    api, browser,
    logic::{format::format_media_time, library::next_index},
};

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
    pub revision: RwSignal<u64>,
}

impl MusicController {
    pub fn new() -> Self {
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
            revision: RwSignal::new(0),
        }
    }
    pub fn current(self) -> Option<File> {
        self.queue.get().get(self.index.get()).cloned()
    }
    fn element(self) -> Option<web_sys::HtmlAudioElement> {
        self.audio.get().map(|e| e.unchecked_into())
    }
    pub fn save(self) {
        if let Some(file) = self.current() {
            api::save_media_progress_keepalive(
                &file.id,
                &MediaProgress {
                    position: self.position.get_untracked(),
                    duration: self.duration.get_untracked(),
                    updated_at: None,
                },
            );
        }
    }
    pub fn play(self, file: File, queue: Vec<File>) {
        self.save();
        let mut queue = queue;
        if !queue.iter().any(|f| f.id == file.id) {
            queue.insert(0, file.clone());
        }
        let index = queue.iter().position(|f| f.id == file.id).unwrap_or(0);
        self.queue.set(queue);
        self.index.set(index);
        self.revision.update(|r| *r += 1);
    }
    pub fn advance(self, direction: i32, ended: bool) {
        self.save();
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
            self.index.set(next);
            self.revision.update(|r| *r += 1);
        } else if ended {
            self.playing.set(false);
        }
    }
    pub fn pause(self) {
        if let Some(audio) = self.element() {
            let _ = audio.pause();
        }
    }
    pub fn toggle(self) {
        if let Some(audio) = self.element() {
            if audio.paused() {
                self.start();
            } else {
                let _ = audio.pause();
            }
        }
    }
    fn start(self) {
        if let Some(audio) = self.element()
            && let Ok(promise) = audio.play()
        {
            let revision = self.revision.get_untracked();
            leptos::task::spawn_local(async move {
                if wasm_bindgen_futures::JsFuture::from(promise).await.is_err()
                    && self.revision.try_get_untracked() == Some(revision)
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
    let queue_open = RwSignal::new(false);
    let cover_failed = RwSignal::new(false);
    let last_save = RwSignal::new(0.0);
    let volume = RwSignal::new(
        browser::local_storage_get("revaro-music-volume")
            .and_then(|s| s.parse::<f64>().ok())
            .filter(|v| v.is_finite())
            .unwrap_or(0.8)
            .clamp(0.0, 1.0),
    );
    Effect::new(move |_| {
        let _ = controller.revision.get();
        let Some(audio) = controller.element() else {
            return;
        };
        let Some(file) = controller.current() else {
            return;
        };
        controller.position.set(0.0);
        controller.duration.set(0.0);
        controller.error.set(String::new());
        cover_failed.set(false);
        audio.set_src(&format!("/api/files/{}/preview", file.id));
        audio.set_volume(volume.get_untracked());
        audio.load();
        controller.start();
        let id = file.id;
        leptos::task::spawn_local(async move {
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
    on_cleanup(move || {
        controller.save();
        controller.pause();
        if let Some(audio) = controller.element() {
            audio.set_src("");
            audio.load();
        }
    });
    view! {
        <audio node_ref=controller.audio preload="metadata"
            on:loadedmetadata=move |_| {if let Some(audio)=controller.element() {let d=audio.duration();controller.duration.set(if d.is_finite(){d.max(0.0)}else{0.0});}}
            on:timeupdate=move |_| {
                if let Some(audio)=controller.element() {controller.position.set(audio.current_time());}
                let now=js_sys::Date::now();
                if now-last_save.get_untracked()>5000.0 {last_save.set(now);controller.save();}
            }
            on:play=move |_| {controller.playing.set(true);controller.error.set(String::new());}
            on:pause=move |_| {controller.playing.set(false);controller.save();}
            on:ended=move |_| controller.advance(1,true)
            on:error=move |_| {controller.playing.set(false);controller.error.set("此音频无法播放，可在文件管理中下载原文件".to_owned());}
        ></audio>
        <Show when=move || controller.current().is_some() fallback=|| ()>
            <aside class="music-dock" aria-label="全局音乐播放器">
                <div class="dock-track">
                    <div class="dock-cover">
                        <Show when=move || !cover_failed.get() fallback=|| icons::music_2().into_any()>
                            <img src=move || controller.current().map(|f|format!("/api/files/{}/thumbnail?v={}",f.id,f.etag)).unwrap_or_default() alt="" on:error=move |_|cover_failed.set(true) />
                        </Show>
                    </div>
                    <div><strong>{move ||controller.current().map(|f|display_title(&f.name)).unwrap_or_default()}</strong><small>{move ||if controller.error.get().is_empty(){"正在播放你的音乐".to_owned()}else{controller.error.get()}}</small></div>
                </div>
                <div class="dock-controls">
                    <button class="dock-prev" aria-label="上一首" on:click=move |_|controller.advance(-1,false)>{icons::chevron_left()}</button>
                    <button class="dock-play" aria-label=move ||if controller.playing.get(){"暂停音乐"}else{"播放音乐"} on:click=move |_|controller.toggle()>{move ||if controller.playing.get(){icons::pause().into_any()}else{icons::play().into_any()}}</button>
                    <button aria-label="下一首" on:click=move |_|controller.advance(1,false)>{icons::chevron_right()}</button>
                </div>
                <div class="dock-progress"><span>{move ||format_media_time(controller.position.get())}</span>
                    <input aria-label="音乐播放进度" type="range" min="0" max=move ||controller.duration.get().max(1.0).to_string() step="0.1" prop:value=move ||controller.position.get().to_string()
                        on:input=move |ev| {if let Ok(value)=event_target_value(&ev).parse::<f64>() && let Some(audio)=controller.element(){audio.set_current_time(value);controller.position.set(value);}} />
                    <span>{move ||format_media_time(controller.duration.get())}</span>
                </div>
                <div class="dock-options">
                    <button class:active=move ||controller.shuffle.get() aria-label="随机播放" aria-pressed=move ||controller.shuffle.get().to_string() on:click=move |_|controller.shuffle.update(|v|*v = !*v)>"随机"</button>
                    <button aria-label="循环模式" on:click=move |_|controller.repeat.update(|r|*r=(*r+1)%3)>{move ||match controller.repeat.get(){1=>"列表循环",2=>"单曲循环",_=>"顺序"}}</button>
                    <input aria-label="音乐音量" type="range" min="0" max="1" step="0.05" prop:value=move ||volume.get().to_string() on:input=move |ev|{if let Ok(value)=event_target_value(&ev).parse::<f64>() {volume.set(value);browser::local_storage_set("revaro-music-volume",&value.to_string());if let Some(audio)=controller.element(){audio.set_volume(value);}}} />
                    <button aria-label="播放队列" class:active=move ||queue_open.get() on:click=move |_|queue_open.update(|v|*v = !*v)>"队列"<span>{move ||controller.queue.get().len()}</span></button>
                    <button aria-label="停止音乐" on:click=move |_|{controller.save();controller.pause();controller.queue.set(Vec::new());if let Some(audio)=controller.element(){audio.set_src("");audio.load();}}>"×"</button>
                </div>
            </aside>
            <Show when=move ||queue_open.get() fallback=|| ()>
                <section class="music-queue" aria-label="当前播放队列"><header><strong>"播放队列"</strong><button aria-label="关闭播放队列" on:click=move |_|queue_open.set(false)>"×"</button></header>
                    <For each=move || { controller.queue.get().into_iter().enumerate().collect::<Vec<_>>() } key=|(i,f)|(*i,f.id.clone()) children=move |(i,file)|view!{<button class:active=move ||controller.index.get()==i on:click=move |_|{controller.save();controller.index.set(i);controller.revision.update(|r|*r+=1);} ><span>{format!("{:02}",i+1)}</span><strong>{display_title(&file.name)}</strong></button>} />
                </section>
            </Show>
        </Show>
    }
}

pub fn display_title(name: &str) -> String {
    name.rsplit_once('.')
        .map_or(name, |(stem, _)| stem)
        .to_owned()
}

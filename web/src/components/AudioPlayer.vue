<script setup lang="ts">
import { computed, nextTick, onBeforeUnmount, onMounted, ref, watch } from 'vue'
import type HlsInstance from 'hls.js/light'
import type { DriveFile } from '../api'
import { api } from '../api'
import { previewURL } from '../fileTypes'
import type { AudioChapter, AudioHLSResponse, AudioMediaResponse, AudioSubtitle } from '../types'

const props=defineProps<{item:DriveFile}>()
const emit=defineEmits<{download:[item:DriveFile];move:[item:DriveFile];copy:[item:DriveFile]}>()
const audio=ref<HTMLAudioElement|null>(null)
const actionMenu=ref<HTMLDetailsElement|null>(null)
const chapterList=ref<HTMLElement|null>(null)
const subtitleList=ref<HTMLElement|null>(null)
const media=ref<AudioMediaResponse|null>(null)
const loading=ref(true)
const waiting=ref(false)
const playing=ref(false)
const currentTime=ref(0)
const nativeDuration=ref(0)
const buffered=ref(0)
const rate=ref(1)
const error=ref('')
const compatibilityMode=ref(false)
const compatibilityStarting=ref(false)
const hlsOffset=ref(0)
const seekPreview=ref<number|null>(null)
const seekHover=ref({visible:false,time:0,percent:0})
const chapterScrollbar=ref({visible:false,top:0,height:0})
const savedVolume=Number(localStorage.getItem('revaro-audio-volume')??0.85)
const volume=ref(Number.isFinite(savedVolume)?Math.max(0,Math.min(1,savedVolume)):0.85)
const muted=ref(localStorage.getItem('revaro-audio-muted')==='true')
let saveTimer=0
let remoteSaveTimer=0
let restoredPosition=false
let progressLoaded=false
let serverPosition=0
let autoplayRequested=true
let hls:HlsInstance|null=null
let hlsSessionId=''
let hlsGeneration=0
let chapterResizeObserver:ResizeObserver|null=null
let progressPromise:Promise<void>|null=null

// Keep the initial source stable so chapter metadata arriving later does not
// reload media that already started from the user's file click.
const source=previewURL(props.item)
const duration=computed(()=>media.value?.duration||nativeDuration.value||0)
const chapters=computed<AudioChapter[]>(()=>media.value?.chapters?.length?media.value.chapters:[{id:1,title:props.item.name.replace(/\.[^.]+$/,''),start:0,end:duration.value}])
const currentChapterIndex=computed(()=>{
  const index=chapters.value.findIndex((chapter,index)=>currentTime.value>=chapter.start&&(currentTime.value<chapter.end||index===chapters.value.length-1))
  return Math.max(0,index)
})
const currentChapter=computed(()=>chapters.value[currentChapterIndex.value])
const subtitles=computed<AudioSubtitle[]>(()=>media.value?.subtitles||[])
const subtitleFocusIndex=computed(()=>{
	if(!subtitles.value.length)return -1
	let focus=0
	for(let index=0;index<subtitles.value.length;index+=1){if(subtitles.value[index].start<=currentTime.value)focus=index;else break}
	return focus
})
const displayedTime=computed(()=>seekPreview.value??currentTime.value)
const progress=computed(()=>duration.value?Math.min(100,displayedTime.value/duration.value*100):0)
const positionKey=computed(()=>`revaro-audio-position:${props.item.id}`)

function savedPosition(){
  if(serverPosition>0)return serverPosition
  const saved=Number(localStorage.getItem(positionKey.value)||0)
  return Number.isFinite(saved)&&saved>0?saved:0
}
async function loadProgress(){
  try{const value=await api<{position:number}>(`/api/files/${props.item.id}/media/progress`);serverPosition=Number.isFinite(value.position)?value.position:0}
  catch{/* 本机进度仍可兜底 */}
  progressLoaded=true;restorePosition()
}
function restorePosition(){
  if(!progressLoaded||restoredPosition||compatibilityMode.value||!audio.value||!duration.value)return
  restoredPosition=true;const saved=savedPosition();if(saved>0&&saved<duration.value-5)seek(saved)
}
function persistProgress(remote=false){
  const position=Math.max(0,currentTime.value)
  if(position<=0)return
  localStorage.setItem(positionKey.value,String(Math.floor(position)))
  if(!remote)return
  void api(`/api/files/${props.item.id}/media/progress`,{method:'PUT',body:JSON.stringify({position,duration:duration.value})}).catch(()=>{})
}

function formatTime(seconds:number){
  if(!Number.isFinite(seconds)||seconds<0)return '0:00'
  const value=Math.floor(seconds);const hours=Math.floor(value/3600);const minutes=Math.floor(value%3600/60);const secs=value%60
  return hours?`${hours}:${String(minutes).padStart(2,'0')}:${String(secs).padStart(2,'0')}`:`${minutes}:${String(secs).padStart(2,'0')}`
}
function updateChapterScrollbar(){
  const el=chapterList.value
  if(!el)return
  const visible=el.scrollHeight>el.clientHeight+1
  const height=visible?Math.max(30,el.clientHeight*el.clientHeight/el.scrollHeight):0
  const travel=Math.max(0,el.clientHeight-height)
  const top=visible&&el.scrollHeight>el.clientHeight?el.scrollTop/(el.scrollHeight-el.clientHeight)*travel:0
  const current=chapterScrollbar.value
  if(current.visible!==visible||Math.abs(current.top-top)>.5||Math.abs(current.height-height)>.5)chapterScrollbar.value={visible,top,height}
}
async function togglePlayback(){
  if(!audio.value||compatibilityStarting.value)return
  if(audio.value.paused){autoplayRequested=true;try{await audio.value.play()}catch{if(!compatibilityStarting.value)error.value='浏览器无法开始播放，请重试'}}else{autoplayRequested=false;audio.value.pause()}
}
function seek(time:number,play=false){
  if(!audio.value||!Number.isFinite(time))return
  const target=Math.max(0,Math.min(time,duration.value||time))
  if(compatibilityMode.value){
    const local=target-hlsOffset.value
    const seekable=audio.value.seekable
    const seekableEnd=seekable.length?seekable.end(seekable.length-1):0
    if(local<0||local>seekableEnd+0.25){void startCompatibilityStream(target,play||playing.value);return}
    audio.value.currentTime=local;currentTime.value=target
  }else{
    audio.value.currentTime=target;currentTime.value=audio.value.currentTime
  }
  if(play)void audio.value.play().catch(()=>{})
}
function previewSeek(event:Event){seekPreview.value=Number((event.target as HTMLInputElement).value)}
function commitSeek(event:Event){const target=Number((event.target as HTMLInputElement).value);seekPreview.value=null;seek(target,playing.value)}
function updateSeekHover(event:PointerEvent){
  const bounds=(event.currentTarget as HTMLElement).getBoundingClientRect()
  const ratio=Math.max(0,Math.min(1,(event.clientX-bounds.left)/bounds.width))
  seekHover.value={visible:true,time:ratio*duration.value,percent:ratio*100}
}
function hideSeekHover(){seekHover.value.visible=false}
function previousChapter(){
  const chapter=currentChapter.value
  if(!chapter)return seek(0)
  if(currentTime.value-chapter.start>3)return seek(chapter.start)
  seek(chapters.value[Math.max(0,currentChapterIndex.value-1)]?.start||0)
}
function nextChapter(){
  const chapter=chapters.value[currentChapterIndex.value+1]
  if(chapter)seek(chapter.start,true)
}
function updateBuffer(){
  const el=audio.value
  if(!el||!duration.value||!el.buffered.length){buffered.value=0;return}
  const end=(compatibilityMode.value?hlsOffset.value:0)+el.buffered.end(el.buffered.length-1)
  buffered.value=Math.min(100,end/duration.value*100)
}
function onLoadedMetadata(){
  const el=audio.value
  if(!el)return
  nativeDuration.value=Number.isFinite(el.duration)?el.duration:0;loading.value=false;waiting.value=false;updateBuffer()
  if(compatibilityMode.value){currentTime.value=hlsOffset.value+el.currentTime;return}
  restorePosition()
}
function onTimeUpdate(){
  if(!audio.value)return
  currentTime.value=(compatibilityMode.value?hlsOffset.value:0)+audio.value.currentTime;updateBuffer()
  window.clearTimeout(saveTimer);saveTimer=window.setTimeout(()=>persistProgress(false),500)
  if(!remoteSaveTimer)remoteSaveTimer=window.setTimeout(()=>{remoteSaveTimer=0;persistProgress(true)},5000)
}
function onPause(){playing.value=false;window.clearTimeout(remoteSaveTimer);remoteSaveTimer=0;persistProgress(true)}
function setRate(event:Event){rate.value=Number((event.target as HTMLSelectElement).value);if(audio.value)audio.value.playbackRate=rate.value}
function applyVolume(){if(audio.value){audio.value.volume=volume.value;audio.value.muted=muted.value}}
function setVolume(event:Event){
  volume.value=Number((event.target as HTMLInputElement).value)
  muted.value=volume.value===0
  localStorage.setItem('revaro-audio-volume',String(volume.value));localStorage.setItem('revaro-audio-muted',String(muted.value));applyVolume()
}
function toggleMute(){muted.value=!muted.value;localStorage.setItem('revaro-audio-muted',String(muted.value));applyVolume()}
function closeActionMenuFromOutside(event:PointerEvent){
  const target=event.target
  if(actionMenu.value?.open&&target instanceof Node&&!actionMenu.value.contains(target))actionMenu.value.open=false
}
function closeActionMenuFromEscape(event:KeyboardEvent){
  if(event.key==='Escape'&&actionMenu.value?.open){actionMenu.value.open=false;actionMenu.value.querySelector<HTMLElement>('summary')?.focus()}
}

async function removeHLSSession(id:string){
  if(!id)return
  try{await api(`/api/audio/hls/${id}`,{method:'DELETE'})}catch{/* 闲置清理仍会兜底 */}
}
function resetLocalHLS(){
  hls?.destroy();hls=null
  if(audio.value){audio.value.pause();audio.value.removeAttribute('src');audio.value.load()}
}
async function startCompatibilityStream(start:number,autoplay=false){
  const generation=++hlsGeneration
  const previousSession=hlsSessionId
  hlsSessionId=''
  compatibilityStarting.value=true
  resetLocalHLS()
  compatibilityMode.value=false
  loading.value=true
  waiting.value=true
  error.value=''
  await removeHLSSession(previousSession)
  try{
    const response=await api<AudioHLSResponse>(`/api/files/${props.item.id}/audio/hls`,{method:'POST',body:JSON.stringify({start})})
    if(generation!==hlsGeneration){void removeHLSSession(response.session_id);return}
    const el=audio.value
    if(!el)throw new Error('播放器已经关闭')
    hlsSessionId=response.session_id
    hlsOffset.value=response.start
    currentTime.value=response.start
    compatibilityMode.value=true
    await nextTick()
    const {default:Hls}=await import('hls.js/light')
    if(Hls.isSupported()){
      const player=new Hls({enableWorker:true,lowLatencyMode:false})
      hls=player
      player.on(Hls.Events.MEDIA_ATTACHED,()=>player.loadSource(response.playlist_url))
      player.on(Hls.Events.MANIFEST_PARSED,()=>{
        compatibilityStarting.value=false;loading.value=false;waiting.value=false
        el.playbackRate=rate.value;applyVolume()
        if(autoplay)void el.play().catch(()=>{})
      })
      player.on(Hls.Events.ERROR,(_event,data)=>{
        if(!data.fatal)return
        compatibilityStarting.value=false;loading.value=false;waiting.value=false
        error.value='FFmpeg HLS 兼容流播放失败，请重试'
      })
      player.attachMedia(el)
    }else if(el.canPlayType('application/vnd.apple.mpegurl')){
      el.src=response.playlist_url;el.load();applyVolume()
      compatibilityStarting.value=false
      if(autoplay)void el.play().catch(()=>{})
    }else{
      throw new Error('当前浏览器不支持 HLS 播放')
    }
  }catch(caught){
    if(generation!==hlsGeneration)return
    const failedSession=hlsSessionId;hlsSessionId=''
    hls?.destroy();hls=null
    if(failedSession)void removeHLSSession(failedSession)
    compatibilityMode.value=false
    compatibilityStarting.value=false;loading.value=false;waiting.value=false
    error.value=caught instanceof Error?caught.message:'兼容流启动失败'
  }
}
async function onAudioError(){
  if(compatibilityMode.value||compatibilityStarting.value)return
  if(progressPromise)await progressPromise
  const saved=savedPosition()
  const start=currentTime.value>0?currentTime.value:(saved>0&&(!duration.value||saved<duration.value-5)?saved:0)
  void startCompatibilityStream(start,autoplayRequested||playing.value)
}

onMounted(()=>{
  // Run play immediately after mounting, while mobile browsers still treat it
  // as part of the click that opened the audio preview.
  progressPromise=loadProgress()
  applyVolume();audio.value?.load();void audio.value?.play().catch(()=>{})
  void api<AudioMediaResponse>(`/api/files/${props.item.id}/audio`).then(value=>{media.value=value;restorePosition()}).catch(()=>{/* 普通音频继续走原始 Range 预览 */})
  void nextTick().then(()=>{
    updateChapterScrollbar()
    if(chapterList.value&&'ResizeObserver' in window){chapterResizeObserver=new ResizeObserver(updateChapterScrollbar);chapterResizeObserver.observe(chapterList.value)}
  })
  document.addEventListener('pointerdown',closeActionMenuFromOutside)
  document.addEventListener('keydown',closeActionMenuFromEscape)
})
watch(()=>chapters.value.length,()=>void nextTick().then(updateChapterScrollbar))
watch(currentChapterIndex,index=>void nextTick().then(()=>{
  chapterList.value?.querySelector<HTMLElement>(`[data-chapter-index="${index}"]`)?.scrollIntoView({block:'nearest'})
  updateChapterScrollbar()
}))
watch(subtitleFocusIndex,index=>{
  if(index<0)return
  void nextTick().then(()=>subtitleList.value?.querySelector<HTMLElement>(`[data-subtitle-index="${index}"]`)?.scrollIntoView({behavior:'smooth',block:'center'}))
})
onBeforeUnmount(()=>{
  document.removeEventListener('pointerdown',closeActionMenuFromOutside);document.removeEventListener('keydown',closeActionMenuFromEscape)
  window.clearTimeout(saveTimer);window.clearTimeout(remoteSaveTimer);persistProgress(false);chapterResizeObserver?.disconnect();hlsGeneration++
  if(currentTime.value>0)void fetch(`/api/files/${props.item.id}/media/progress`,{method:'PUT',headers:{'Content-Type':'application/json'},body:JSON.stringify({position:currentTime.value,duration:duration.value}),credentials:'same-origin',keepalive:true})
  const session=hlsSessionId;hlsSessionId='';resetLocalHLS()
  if(session)void fetch(`/api/audio/hls/${session}`,{method:'DELETE',credentials:'same-origin',keepalive:true})
})
</script>

<template>
  <div class="chapter-audio-player">
    <main class="audio-main">
      <div class="audio-upper">
        <section class="audio-now-playing">
          <div class="audio-cover">
            <img v-if="media?.has_cover" :src="media.cover_url" :alt="`${item.name} 封面`">
            <span v-else>♫</span>
          </div>
          <div class="audio-chapter-current"><span>正在播放</span><strong>{{ item.name }}</strong><small>第 {{ currentChapterIndex+1 }} / {{ chapters.length }} 节</small></div>
        </section>
        <section class="audio-subtitle-panel">
          <header><strong>字幕</strong><span>{{ subtitles.length }} 条</span></header>
          <div class="audio-subtitle-stage">
            <div v-if="subtitles.length" ref="subtitleList" class="audio-subtitle-lines">
              <button v-for="(cue,index) in subtitles" :key="cue.id" :data-subtitle-index="index" :class="{active:index===subtitleFocusIndex,near:Math.abs(index-subtitleFocusIndex)===1,mid:Math.abs(index-subtitleFocusIndex)===2}" :aria-label="`${formatTime(cue.start)} ${cue.text}`" @click="seek(cue.start,true)"><span>{{ cue.text }}</span></button>
            </div>
            <div v-else class="audio-subtitle-empty"><b>CC</b><strong>没有内嵌字幕</strong><small>合并 M4A 时会自动识别每段同名的 VTT</small></div>
          </div>
        </section>
      </div>
      <section class="audio-playback">
        <div class="audio-track-wrap">
          <div class="audio-track-buffer" :style="{width:`${buffered}%`}"></div>
          <div class="audio-track-played" :style="{width:`${progress}%`}"></div>
          <i v-for="chapter in chapters.slice(1)" :key="chapter.id" :style="{left:`${duration?chapter.start/duration*100:0}%`}"></i>
          <span class="audio-track-thumb" :style="{left:`${progress}%`}"></span>
          <output v-if="seekHover.visible" class="audio-seek-tooltip" :style="{left:`${seekHover.percent}%`}">{{ formatTime(seekHover.time) }}</output>
          <input :value="displayedTime" type="range" min="0" :max="duration||0" step="0.1" aria-label="播放进度" @input="previewSeek" @change="commitSeek" @pointermove="updateSeekHover" @pointerleave="hideSeekHover">
        </div>
        <div class="audio-time"><span>{{ formatTime(displayedTime) }}</span><span>{{ formatTime(duration) }}</span><span>-{{ formatTime(Math.max(0,duration-displayedTime)) }}</span></div>
        <div class="audio-player-options audio-transport-row">
          <label class="audio-rate"><span>倍速</span><select :value="rate" @change="setRate"><option value="0.75">0.75×</option><option value="1">1.0×</option><option value="1.25">1.25×</option><option value="1.5">1.5×</option><option value="2">2.0×</option></select></label>
          <div class="audio-controls">
            <button title="上一节" aria-label="上一节" @click="previousChapter"><svg viewBox="0 0 24 24" aria-hidden="true"><path d="M6 5v14M19 6 9 12l10 6Z"/></svg></button>
            <button class="audio-play" :disabled="loading||compatibilityStarting" :title="playing?'暂停':'播放'" :aria-label="playing?'暂停':'播放'" @click="togglePlayback"><span v-if="loading||waiting||compatibilityStarting" class="audio-control-spinner"></span><svg v-else viewBox="0 0 24 24" aria-hidden="true"><path v-if="playing" d="M8 6v12M16 6v12"/><path v-else class="play-shape" d="m9 6 9 6-9 6Z"/></svg></button>
            <button title="下一节" aria-label="下一节" :disabled="currentChapterIndex>=chapters.length-1" @click="nextChapter"><svg viewBox="0 0 24 24" aria-hidden="true"><path d="M18 5v14M5 6l10 6-10 6Z"/></svg></button>
          </div>
          <div class="audio-option-end"><div class="audio-volume"><button type="button" :title="muted?'取消静音':'静音'" :aria-label="muted?'取消静音':'静音'" @click="toggleMute"><svg viewBox="0 0 24 24" aria-hidden="true"><path d="M4 9v6h4l5 4V5L8 9H4Z"/><path v-if="muted||volume===0" d="m17 9 5 6m0-6-5 6"/><path v-else-if="volume<.5" d="M17 9.5a4 4 0 0 1 0 5"/><path v-else d="M17 8a6 6 0 0 1 0 8m2.5-10.5a9 9 0 0 1 0 13"/></svg></button><input :value="volume" type="range" min="0" max="1" step="0.01" aria-label="音量" @input="setVolume"><span>{{ muted?0:Math.round(volume*100) }}%</span></div><details ref="actionMenu" class="audio-action-menu"><summary aria-label="更多操作"><svg viewBox="0 0 24 24"><path d="M5 7h14M5 12h14M5 17h14"/></svg></summary><div><button @click="actionMenu?.removeAttribute('open');emit('download',item)">下载</button><button @click="actionMenu?.removeAttribute('open');emit('move',item)">移动</button><button @click="actionMenu?.removeAttribute('open');emit('copy',item)">复制</button></div></details></div>
        </div>
        <span class="audio-stream-status" :class="{busy:compatibilityStarting||waiting}"><i></i>{{ compatibilityStarting?'正在启动 HLS 兼容流':waiting?'正在缓冲需要的片段':compatibilityMode?'HLS 兼容流':'原文件流式播放' }}</span>
        <p v-if="error" class="audio-player-error">{{ error }}</p>
        <audio ref="audio" :src="compatibilityMode?undefined:source" autoplay playsinline preload="metadata" @loadedmetadata="onLoadedMetadata" @timeupdate="onTimeUpdate" @progress="updateBuffer" @play="playing=true" @pause="onPause" @waiting="waiting=true" @canplay="waiting=false" @error="onAudioError"></audio>
      </section>
    </main>
    <aside class="audio-chapters">
      <header><div><strong>分节</strong><small>保留合并前的文件名</small></div><span>{{ chapters.length }} 节</span></header>
      <div class="audio-chapter-scroll-area">
        <div ref="chapterList" class="audio-chapter-list" @scroll.passive="updateChapterScrollbar">
          <button v-for="(chapter,index) in chapters" :key="chapter.id" :data-chapter-index="index" :class="{active:index===currentChapterIndex}" @click="seek(chapter.start,true)">
            <b>{{ index+1 }}</b><span><strong :title="chapter.title">{{ chapter.title }}</strong><small>{{ formatTime(chapter.start) }} · {{ formatTime(Math.max(0,chapter.end-chapter.start)) }}</small></span><i><span v-if="index===currentChapterIndex&&playing" class="chapter-equalizer"><b></b><b></b><b></b></span><svg v-else viewBox="0 0 24 24" aria-hidden="true"><path d="m9 7 8 5-8 5Z"/></svg></i>
          </button>
        </div>
        <span v-if="chapterScrollbar.visible" class="audio-chapter-scrollbar" :style="{height:`${chapterScrollbar.height}px`,transform:`translateY(${chapterScrollbar.top}px)`}" aria-hidden="true"></span>
      </div>
    </aside>
  </div>
</template>

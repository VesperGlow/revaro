<script setup lang="ts">
import { computed, nextTick, onBeforeUnmount, onMounted, ref, watch } from 'vue'
import type HlsInstance from 'hls.js/light'
import type { DriveFile } from '../api'
import { api } from '../api'
import { previewURL } from '../fileTypes'
import { formatMediaTime as formatTime } from '../format'
import type { AudioChapter, AudioHLSResponse, AudioMediaResponse, AudioSubtitle } from '../types'
import FullBleedProgress from './FullBleedProgress.vue'
import PreviewMenu from './PreviewMenu.vue'
import { Music2, Play, Pause, RotateCcw, RotateCw, List, Captions, Volume2, VolumeX, X, SkipBack, SkipForward, Info } from '@lucide/vue'

const props=defineProps<{item:DriveFile}>()
const audio=ref<HTMLAudioElement|null>(null)
const panelOpen=ref(false)
const panelTab=ref<'chapters'|'subtitles'>('chapters')
const playerEl=ref<HTMLElement|null>(null)
const coverFailed=ref(false)
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
const chapterMarkers=computed(()=>chapters.value.slice(1).map(chapter=>({id:chapter.id,percent:duration.value?chapter.start/duration.value*100:0})))
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

function revealCurrentChapter(){
  if(!panelOpen.value||panelTab.value!=='chapters')return
  void nextTick().then(()=>playerEl.value?.querySelector<HTMLElement>(`[data-chapter-index="${currentChapterIndex.value}"]`)?.scrollIntoView({block:'nearest'}))
}
function openPanel(tab:'chapters'|'subtitles',focus=true){
  panelTab.value=tab;panelOpen.value=true
  if(focus)void nextTick(()=>playerEl.value?.querySelector<HTMLElement>('.audio-panel .media-icon-button')?.focus())
  if(tab==='chapters')revealCurrentChapter()
  else revealSubtitle()
}
function closePanel(){
  panelOpen.value=false
  playerEl.value?.querySelector<HTMLElement>(`[data-panel-trigger="${panelTab.value}"]`)?.focus()
}
function onKey(event:KeyboardEvent){
  if(event.defaultPrevented)return
  if(event.key==='Escape'&&panelOpen.value&&!playerEl.value?.querySelector('details[open]')){
    event.preventDefault();event.stopPropagation();closePanel();return
  }
  if(event.target instanceof Element&&event.target.closest('button, input, select, summary'))return
  if(event.key===' '){event.preventDefault();void togglePlayback()}
  if(event.key==='ArrowLeft'){event.preventDefault();seek(currentTime.value-15)}
  if(event.key==='ArrowRight'){event.preventDefault();seek(currentTime.value+30)}
}
function revealSubtitle(){
  if(!panelOpen.value||panelTab.value!=='subtitles')return
  void nextTick().then(()=>subtitleList.value?.querySelector<HTMLElement>(`[data-subtitle-index="${subtitleFocusIndex.value}"]`)?.scrollIntoView({behavior:matchMedia('(prefers-reduced-motion: reduce)').matches?'instant':'smooth',block:'center'}))
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
function onEnded(){
  if(compatibilityMode.value&&currentTime.value<duration.value-1)void startCompatibilityStream(currentTime.value+.05,true)
}
function setRate(event:Event){rate.value=Number((event.target as HTMLSelectElement).value);if(audio.value)audio.value.playbackRate=rate.value}
function applyVolume(){if(audio.value){audio.value.volume=volume.value;audio.value.muted=muted.value}}
function setVolume(event:Event){
  volume.value=Number((event.target as HTMLInputElement).value)
  muted.value=volume.value===0
  localStorage.setItem('revaro-audio-volume',String(volume.value));localStorage.setItem('revaro-audio-muted',String(muted.value));applyVolume()
}
function toggleMute(){muted.value=!muted.value;localStorage.setItem('revaro-audio-muted',String(muted.value));applyVolume()}
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
        error.value='暂时无法播放，请重新打开试试'
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
  playerEl.value?.focus({preventScroll:true})
  // Run play immediately after mounting, while mobile browsers still treat it
  // as part of the click that opened the audio preview.
  progressPromise=loadProgress()
  applyVolume();audio.value?.load();void audio.value?.play().catch(()=>{})
  void api<AudioMediaResponse>(`/api/files/${props.item.id}/audio`).then(value=>{media.value=value;restorePosition();if(value.subtitles.length&&matchMedia('(min-width: 761px)').matches)openPanel('subtitles',false)}).catch(()=>{/* 普通音频继续走原始 Range 预览 */})
})
watch(currentChapterIndex,revealCurrentChapter)
watch(subtitleFocusIndex,revealSubtitle)
onBeforeUnmount(()=>{
  window.clearTimeout(saveTimer);window.clearTimeout(remoteSaveTimer);persistProgress(false);hlsGeneration++
  if(currentTime.value>0)void fetch(`/api/files/${props.item.id}/media/progress`,{method:'PUT',headers:{'Content-Type':'application/json'},body:JSON.stringify({position:currentTime.value,duration:duration.value}),credentials:'same-origin',keepalive:true})
  const session=hlsSessionId;hlsSessionId='';resetLocalHLS()
  if(session)void fetch(`/api/audio/hls/${session}`,{method:'DELETE',credentials:'same-origin',keepalive:true})
})
</script>

<template>
  <div ref="playerEl" class="chapter-audio-player" tabindex="0" :class="{'panel-open':panelOpen}" @keydown="onKey">
    <main class="audio-main">
      <section class="audio-now-playing">
        <div class="audio-cover">
          <img v-if="media?.has_cover&&!coverFailed" :src="media.cover_url" :alt="`${item.name} 封面`" @error="coverFailed=true">
          <Music2 v-else aria-hidden="true" />
        </div>
        <div class="audio-chapter-current">
          <span>{{ compatibilityStarting ? '正在准备播放…' : playing ? '正在播放' : '暂停中' }}</span>
          <p v-if="chapters.length>1" class="audio-book-title">{{ item.name.replace(/\.[^.]+$/,'') }}</p>
          <h1>{{ currentChapter?.title || item.name.replace(/\.[^.]+$/,'') }}</h1>
          <small v-if="chapters.length>1">第 {{ currentChapterIndex+1 }} / {{ chapters.length }} 章</small>
        </div>
      </section>
      <section class="audio-playback" aria-label="音频播放控制">
        <FullBleedProgress
          variant="audio" :percent="progress" :buffered-percent="buffered" :markers="chapterMarkers"
          :tooltip="seekHover.visible?{percent:seekHover.percent,text:formatTime(seekHover.time)}:undefined"
          :value="displayedTime" min="0" :max="duration||0" step="0.1" :disabled="!duration" aria-label="播放进度"
          @input="previewSeek" @change="commitSeek" @pointermove="updateSeekHover" @pointerleave="hideSeekHover"
        />
        <div class="audio-time"><span>{{ formatTime(displayedTime) }}</span><span>{{ formatTime(duration) }}</span></div>
        <div class="audio-controls">
          <button aria-label="后退15秒" title="后退 15 秒" :disabled="!duration" @click="seek(currentTime-15)"><RotateCcw aria-hidden="true" /><small>15</small></button>
          <button class="audio-play" :disabled="loading||compatibilityStarting" :aria-label="playing?'暂停':'播放'" @click="togglePlayback">
            <span v-if="loading||waiting||compatibilityStarting" class="audio-control-spinner"></span><Pause v-else-if="playing" aria-hidden="true" /><Play v-else aria-hidden="true" />
          </button>
          <button aria-label="前进30秒" title="前进 30 秒" :disabled="!duration" @click="seek(currentTime+30)"><RotateCw aria-hidden="true" /><small>30</small></button>
        </div>
        <div class="audio-options">
          <label class="audio-rate"><span class="media-sr-only">播放速度</span><select :value="rate" aria-label="播放速度" @change="setRate"><option v-for="speed in [0.75,1,1.25,1.5,2]" :key="speed" :value="speed">{{ speed }}×</option></select></label>
          <button data-panel-trigger="chapters" :aria-expanded="panelOpen&&panelTab==='chapters'" @click="panelOpen&&panelTab==='chapters'?closePanel():openPanel('chapters')"><List aria-hidden="true" /><span>章节</span></button>
          <button v-if="subtitles.length" data-panel-trigger="subtitles" :aria-expanded="panelOpen&&panelTab==='subtitles'" @click="panelOpen&&panelTab==='subtitles'?closePanel():openPanel('subtitles')"><Captions aria-hidden="true" /><span>字幕</span></button>
          <PreviewMenu label="音量">
            <template #trigger><VolumeX v-if="muted||volume===0" aria-hidden="true" /><Volume2 v-else aria-hidden="true" /></template>
            <div class="audio-volume"><button :aria-label="muted?'取消静音':'静音'" @click="toggleMute"><VolumeX v-if="muted" aria-hidden="true" /><Volume2 v-else aria-hidden="true" /></button><input :value="volume" type="range" min="0" max="1" step="0.01" aria-label="音量" @input="setVolume"><output>{{ muted?0:Math.round(volume*100) }}%</output></div>
          </PreviewMenu>
          <PreviewMenu v-if="compatibilityMode" label="播放详情"><template #trigger><Info aria-hidden="true" /></template><p class="media-detail">兼容播放 · HLS</p></PreviewMenu>
        </div>
        <p v-if="error" class="audio-player-error" role="alert">{{ error }}</p>
        <audio ref="audio" :src="compatibilityMode?undefined:source" autoplay playsinline preload="metadata" @loadedmetadata="onLoadedMetadata" @timeupdate="onTimeUpdate" @progress="updateBuffer" @play="playing=true" @pause="onPause" @ended="onEnded" @waiting="waiting=true" @canplay="waiting=false" @error="onAudioError"></audio>
      </section>
    </main>
    <button v-if="panelOpen" class="audio-panel-scrim" aria-label="收起面板" @click="closePanel"></button>
    <aside v-if="panelOpen" class="audio-panel" data-preview-sheet :aria-label="panelTab==='chapters'?'音频章节':'音频字幕'">
      <header><div class="audio-panel-tabs"><button :aria-pressed="panelTab==='chapters'" @click="openPanel('chapters')">章节</button><button v-if="subtitles.length" :aria-pressed="panelTab==='subtitles'" @click="openPanel('subtitles')">字幕</button></div><button class="media-icon-button" aria-label="收起面板" @click="closePanel"><X aria-hidden="true" /></button></header>
      <template v-if="panelTab==='chapters'">
        <div class="audio-chapter-navigation"><button :disabled="!duration" @click="previousChapter"><SkipBack aria-hidden="true" />上一章</button><button :disabled="currentChapterIndex>=chapters.length-1" @click="nextChapter">下一章<SkipForward aria-hidden="true" /></button></div>
        <div class="audio-chapter-list"><button v-for="(chapter,index) in chapters" :key="chapter.id" :data-chapter-index="index" :aria-current="index===currentChapterIndex?'true':undefined" @click="seek(chapter.start,true)"><span class="audio-chapter-number">{{ String(index+1).padStart(2,'0') }}</span><strong>{{ chapter.title }}</strong><small>{{ formatTime(chapter.start) }}</small></button></div>
      </template>
      <div v-else ref="subtitleList" class="audio-subtitle-lines"><button v-for="(cue,index) in subtitles" :key="cue.id" :data-subtitle-index="index" :class="{active:index===subtitleFocusIndex}" :aria-label="`${formatTime(cue.start)} ${cue.text}`" @click="seek(cue.start,true)">{{ cue.text }}</button></div>
    </aside>
  </div>
</template>

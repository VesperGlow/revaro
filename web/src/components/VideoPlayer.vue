<script setup lang="ts">
import { computed, nextTick, onBeforeUnmount, onMounted, ref, watch } from 'vue'
import type { DriveFile } from '../api'
import { api } from '../api'
import { previewURL, thumbSRC } from '../fileTypes'
import { formatMediaTime as formatTime } from '../format'
import type { VideoMediaResponse } from '../types'
import { createUnifiedVideoPlayer, initialSubtitleIndex, mediaElementTimelineTime, shouldContinueMediaClock, shouldHideVideoCursor, shouldSyncMediaClock, subtitleLineClass, type UnifiedVideoPlayer } from '../videoPlayer'
import { useVideoSubtitles } from '../composables/useVideoSubtitles'
import { useVideoProgress } from '../composables/useVideoProgress'
import VideoControls from './VideoControls.vue'
import VideoStatusOverlay from './VideoStatusOverlay.vue'

const props=defineProps<{item:DriveFile}>()
const emit=defineEmits<{close:[];download:[item:DriveFile];move:[item:DriveFile];copy:[item:DriveFile]}>()
const shell=ref<HTMLElement|null>(null)
const video=ref<HTMLVideoElement|null>(null)
const directMode=ref(true)
const directSource=ref(previewURL(props.item))
const starting=ref(false)
const buffering=ref(false)
const playing=ref(false)
const error=ref('')
const currentTime=ref(0)
const duration=ref(0)
const controlsVisible=ref(true)
const volume=ref(.9)
const muted=ref(false)
const volumeFeedback=ref(false)
const storedRate=Number(localStorage.getItem('revaro-video-rate')||1)
const rate=ref([0.5,0.75,1,1.25,1.5,2].includes(storedRate)?storedRate:1)
let controlsHovered=false
let pointerType='mouse'
let clickTimer=0
const pendingSeek=ref<number|null>(null)
const fullscreen=ref(false)
const autoplayPending=ref(true)
let player:UnifiedVideoPlayer|null=null
let saveTimer=0
let remoteSaveTimer=0
let controlsTimer=0
let volumeTimer=0
let clockFrame=0
let lastAudibleVolume=.9
let videoResizeObserver:ResizeObserver|null=null

const volumeKey='revaro-video-volume'
const poster=computed(()=>thumbSRC(props.item))
const timelinePosition=computed(()=>pendingSeek.value??currentTime.value)
const progress=computed(()=>duration.value?Math.min(100,timelinePosition.value/duration.value*100):0)
const effectiveVolume=computed(()=>muted.value?0:volume.value)
const volumePercent=computed(()=>Math.round(effectiveVolume.value*100))
const volumeState=computed<'muted'|'low'|'high'>(()=>effectiveVolume.value===0?'muted':effectiveVolume.value<.5?'low':'high')
const cursorHidden=computed(()=>shouldHideVideoCursor({playing:playing.value,controlsVisible:controlsVisible.value,starting:starting.value,buffering:buffering.value,error:error.value}))
const {subtitleElement,subtitles,activeSubtitle,activeSubtitleLines,subtitlePlacement,selectedSubtitle,selectedSubtitleURL,selectedSubtitleKey,subtitleStyle,updateSubtitleBounds,applySubtitle,disableSubtitleTracks,chooseSubtitle,onSubtitleLoad,onSubtitleError}=useVideoSubtitles({video,getPlayer:()=>player})
const {loadProgress,restoreDirectPosition,persistProgress,markUserSeeked}=useVideoProgress({itemId:props.item.id,video,currentTime,duration,directMode})

function resetPlayback(){
  player?.destroy();player=null
  if(video.value){video.value.pause();video.value.removeAttribute('src');video.value.load()}
}
function showControls(persist=false){
  controlsVisible.value=true;window.clearTimeout(controlsTimer)
  if(!persist&&playing.value)controlsTimer=window.setTimeout(()=>{
    if(controlsHovered||pendingSeek.value!==null||shell.value?.querySelector('details[open], :focus-visible')){showControls();return}
    if(!starting.value&&!buffering.value&&!error.value)controlsVisible.value=false
  },2800)
}

function setControlsHover(value:boolean){controlsHovered=value;showControls()}
function onVideoClick(){
  if(pointerType!=='mouse'){if(!controlsVisible.value)showControls();else if(playing.value)controlsVisible.value=false;return}
  window.clearTimeout(clickTimer);clickTimer=window.setTimeout(togglePlayback,200)
}
function onVideoDoubleClick(){if(pointerType==='mouse'){window.clearTimeout(clickTimer);void toggleFullscreen()}}
function changeRate(event:Event){rate.value=Number((event.target as HTMLSelectElement).value);if(video.value)video.value.playbackRate=rate.value;localStorage.setItem('revaro-video-rate',String(rate.value));showControls()}

function onLoadedMetadata(){
  const el=video.value;if(!el)return
  el.volume=volume.value;el.muted=muted.value;el.playbackRate=rate.value
  if(directMode.value){duration.value=Number.isFinite(el.duration)?el.duration:0;restoreDirectPosition()}
  applySubtitle()
  updateSubtitleBounds()
}
function syncPlaybackClock(){
  const el=video.value
  if(!el||!shouldSyncMediaClock(starting.value,el.paused))return false
  currentTime.value=mediaElementTimelineTime(el.currentTime,'direct',0)
  return true
}
function stopPlaybackClock(){window.cancelAnimationFrame(clockFrame);clockFrame=0}
function runPlaybackClock(){
  stopPlaybackClock()
  const tick=()=>{
    const el=video.value
    if(!shouldContinueMediaClock(Boolean(el))){clockFrame=0;return}
    if(!el)return
    // Keep sampling while the element exists; a later frame can resume the
    // clock even when the browser omits a play/timeupdate event.
    if(!el.paused)syncPlaybackClock()
    clockFrame=window.requestAnimationFrame(tick)
  }
  clockFrame=window.requestAnimationFrame(tick)
}
function onTimeUpdate(){
  if(!syncPlaybackClock())return
  window.clearTimeout(saveTimer);saveTimer=window.setTimeout(()=>persistProgress(false),600)
  if(!remoteSaveTimer)remoteSaveTimer=window.setTimeout(()=>{remoteSaveTimer=0;persistProgress(true)},5000)
}
function onWaiting(){
  if(starting.value)return
  buffering.value=true;showControls(true)
}
function onCanPlay(){buffering.value=false;showControls()}
function onEnded(){onPause()}
function onVideoError(){starting.value=false;buffering.value=false;error.value='浏览器无法播放此原始格式，请下载后使用本地播放器打开';showControls(true)}
function retryPlayback(){error.value='';video.value?.load();void video.value?.play().catch(()=>{})}
function seekTo(target:number){
 const el=video.value;if(!el||!Number.isFinite(target))return
 target=Math.max(0,Math.min(target,duration.value||target));markUserSeeked();el.currentTime=target;currentTime.value=target
}
function previewSeek(event:Event){const value=Number((event.target as HTMLInputElement).value);if(Number.isFinite(value))pendingSeek.value=value;showControls(true)}
function commitSeek(event:Event){const value=Number((event.target as HTMLInputElement).value);pendingSeek.value=null;seekTo(value);showControls()}
function togglePlayback(){
  const el=video.value;if(!el)return
  if(starting.value){autoplayPending.value=!autoplayPending.value;showControls(true);return}
  if(el.paused)void player?.play().catch(()=>{});else player?.pause()
}
function onPlay(){playing.value=true;buffering.value=false;runPlaybackClock();showControls()}
function onPause(){playing.value=false;if(starting.value)return;syncPlaybackClock();window.clearTimeout(remoteSaveTimer);remoteSaveTimer=0;persistProgress(true);showControls(true)}
function showVolumeFeedback(){volumeFeedback.value=true;window.clearTimeout(volumeTimer);volumeTimer=window.setTimeout(()=>volumeFeedback.value=false,900);showControls(true)}
function changeVolume(event:Event){const value=Math.max(0,Math.min(1,Number((event.target as HTMLInputElement).value)));volume.value=value;if(value>0)lastAudibleVolume=value;muted.value=value===0;localStorage.setItem(volumeKey,String(value));player?.setVolume(value,muted.value);showVolumeFeedback()}
function toggleMute(){if(muted.value||volume.value===0){if(volume.value===0)volume.value=lastAudibleVolume;muted.value=false}else muted.value=true;player?.setVolume(volume.value,muted.value);showVolumeFeedback()}
async function toggleFullscreen(){
  if(shell.value)await player?.requestFullscreen(shell.value)
}
function onFullscreenChange(){fullscreen.value=document.fullscreenElement===shell.value}
function onKey(event:KeyboardEvent){
  showControls()
  if(event.defaultPrevented||event.target instanceof Element&&event.target.closest('input, select, button, summary'))return
  if(event.key===' '||event.key==='k'){event.preventDefault();togglePlayback()}
  else if(event.key==='ArrowLeft'){event.preventDefault();seekTo(currentTime.value-5)}
  else if(event.key==='ArrowRight'){event.preventDefault();seekTo(currentTime.value+5)}
  else if(event.key==='m')toggleMute()
  else if(event.key==='f')void toggleFullscreen()
}

watch(selectedSubtitleURL,url=>{
  if(!url)return
  console.info('[revaro] subtitle url:',url)
  void nextTick().then(applySubtitle)
},{flush:'post'})

onMounted(async()=>{
  shell.value?.focus({preventScroll:true})
  document.addEventListener('fullscreenchange',onFullscreenChange)
  if(video.value&&typeof ResizeObserver!=='undefined'){videoResizeObserver=new ResizeObserver(updateSubtitleBounds);videoResizeObserver.observe(video.value)}
  const storedVolume=Number(localStorage.getItem(volumeKey));if(Number.isFinite(storedVolume)&&storedVolume>=0&&storedVolume<=1){volume.value=storedVolume;muted.value=storedVolume===0;if(storedVolume>0)lastAudibleVolume=storedVolume}
  void loadProgress()
  try{
    const media=await api<VideoMediaResponse>(`/api/files/${props.item.id}/video`)
    subtitles.value=media.subtitles||[];activeSubtitle.value=initialSubtitleIndex(subtitles.value)
    console.info('[revaro] subtitles discovered:',subtitles.value.length)
    console.info('[revaro] subtitle selected:',selectedSubtitle.value?.id||'off')
    await nextTick();applySubtitle()
  }catch(caught){console.error('[revaro] subtitle discovery failed:',caught)}
  const el=video.value
  if(directMode.value&&el){player=createUnifiedVideoPlayer('direct',el);player.setVolume(volume.value,muted.value);el.load();void player.play().catch(()=>{})}
})
onBeforeUnmount(()=>{
  window.clearTimeout(clickTimer);document.removeEventListener('fullscreenchange',onFullscreenChange);videoResizeObserver?.disconnect();videoResizeObserver=null;window.clearTimeout(saveTimer);window.clearTimeout(remoteSaveTimer);window.clearTimeout(controlsTimer);window.clearTimeout(volumeTimer);stopPlaybackClock();persistProgress(false);disableSubtitleTracks()
  if(currentTime.value>0)void fetch(`/api/files/${props.item.id}/media/progress`,{method:'PUT',headers:{'Content-Type':'application/json'},body:JSON.stringify({position:currentTime.value,duration:duration.value}),credentials:'same-origin',keepalive:true})
  resetPlayback()
})
</script>

<template>
  <div ref="shell" class="video-player-shell" :class="{'cursor-hidden':cursorHidden}" tabindex="0" @pointermove="$event.pointerType==='mouse'&&showControls()" @pointerdown="pointerType=$event.pointerType" @keydown.tab.capture="showControls()" @keydown="onKey">
    <video ref="video" :src="directSource||undefined" :poster="poster" crossorigin="anonymous" autoplay playsinline preload="metadata" @click="onVideoClick" @dblclick="onVideoDoubleClick" @loadedmetadata="onLoadedMetadata" @timeupdate="onTimeUpdate" @waiting="onWaiting" @stalled="onWaiting" @canplay="onCanPlay" @playing="onCanPlay" @play="onPlay" @pause="onPause" @ended="onEnded" @error="onVideoError">
      <track v-if="selectedSubtitle" ref="subtitleElement" :key="selectedSubtitleKey" kind="subtitles" :src="selectedSubtitleURL" :srclang="selectedSubtitle.language" :label="selectedSubtitle.label" @load="onSubtitleLoad" @error="onSubtitleError">
      你的浏览器不支持这个视频格式。
    </video>
    <div v-if="activeSubtitleLines.length" class="video-subtitle-overlay" :class="subtitlePlacement" :style="subtitleStyle" aria-live="off"><span v-for="(line,index) in activeSubtitleLines" :key="`${index}:${line}`" :class="subtitleLineClass(index)">{{ line }}</span></div>
    <VideoStatusOverlay :item-name="item.name" :controls-visible="controlsVisible" :playing="playing" :starting="starting" :error="error" :buffering="buffering" @close="emit('close')" @toggle-playback="togglePlayback" @retry="retryPlayback" />
    <VideoControls :visible="controlsVisible||!playing" :playing="playing" :starting="starting" :autoplay-pending="autoplayPending" :duration="duration" :timeline-position="timelinePosition" :progress="progress" :volume-state="volumeState" :volume-feedback="volumeFeedback" :volume-percent="volumePercent" :effective-volume="effectiveVolume" :subtitles="subtitles" :active-subtitle="activeSubtitle" :fullscreen="fullscreen" :rate="rate" playback-info="原始文件播放" :format-time="formatTime" @change-rate="changeRate" @hover="setControlsHover" @interact="showControls()" @cancel-seek="pendingSeek=null;showControls()" @toggle-playback="togglePlayback" @preview-seek="previewSeek" @commit-seek="commitSeek" @toggle-mute="toggleMute" @volume-start="volumeFeedback=true" @volume-end="showVolumeFeedback" @change-volume="changeVolume" @choose-subtitle="chooseSubtitle" @download="emit('download',item)" @move="emit('move',item)" @copy="emit('copy',item)" @toggle-fullscreen="toggleFullscreen" />
  </div>
</template>

<style src="../styles/video-player.css"></style>

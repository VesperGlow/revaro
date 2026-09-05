<script setup lang="ts">
import { computed, nextTick, onBeforeUnmount, onMounted, ref, watch } from 'vue'
import type HlsInstance from 'hls.js/light'
import type { DriveFile } from '../api'
import { api } from '../api'
import { previewURL, thumbSRC } from '../fileTypes'
import { formatMediaTime as formatTime } from '../format'
import type { VideoFMP4Metadata, VideoFMP4Response, VideoHLSResponse, VideoMediaResponse } from '../types'
import { attachFMP4Stream, authoritativeSeekTarget, createUnifiedVideoPlayer, initialSubtitleIndex, mediaElementTimelineTime, mseCompatibility, mseRecoveryAction, shouldContinueMediaClock, shouldHideVideoCursor, shouldSyncMediaClock, subtitleLineClass, type UnifiedVideoPlayer } from '../videoPlayer'
import { useVideoSubtitles } from '../composables/useVideoSubtitles'
import { useVideoProgress } from '../composables/useVideoProgress'
import VideoControls from './VideoControls.vue'
import VideoStatusOverlay from './VideoStatusOverlay.vue'
import { releaseFMP4Session, releaseHLSSession } from '../videoSession'

const props=defineProps<{item:DriveFile}>()
const emit=defineEmits<{close:[];download:[item:DriveFile];move:[item:DriveFile];copy:[item:DriveFile]}>()
const shell=ref<HTMLElement|null>(null)
const video=ref<HTMLVideoElement|null>(null)
const directMode=ref(/\.(mp4|m4v|webm|ogv|mov)$/i.test(props.item.name))
const optimizedMode=ref(false)
const mseMode=ref(false)
const directSource=ref(directMode.value?previewURL(props.item):'')
const starting=ref(false)
const buffering=ref(false)
const prepareKind=ref<'initial'|'seek'>('initial')
const prepareMode=ref<'mse'|'hls'>('mse')
const playing=ref(false)
const error=ref('')
const currentTime=ref(0)
const duration=ref(0)
const streamOffset=ref(0)
const videoCodec=ref('')
const audioCodec=ref('')
const transcoding=ref(false)
const audioTranscoding=ref(false)
const controlsVisible=ref(true)
const volume=ref(.9)
const muted=ref(false)
const volumeFeedback=ref(false)
const pendingSeek=ref<number|null>(null)
const fullscreen=ref(false)
const autoplayPending=ref(true)
let hls:HlsInstance|null=null
let hlsSessionId=''
let fmp4SessionId=''
let playbackGeneration=0
let player:UnifiedVideoPlayer|null=null
let saveTimer=0
let remoteSaveTimer=0
let controlsTimer=0
let volumeTimer=0
let clockFrame=0
let lastAudibleVolume=.9
let directFallbackStarted=false
let mseRecoveryStarted=false
let mseFreshFailures=0
let mseRecoveryAnchor=-1
let videoResizeObserver:ResizeObserver|null=null

const sessionKey=computed(()=>`revaro-video-hls-session:${props.item.id}`)
const fmp4SessionKey=computed(()=>`revaro-video-fmp4-session:${props.item.id}`)
const volumeKey='revaro-video-volume'
const poster=computed(()=>thumbSRC(props.item))
const compatibilityLabel=computed(()=>{
  const videoName=videoCodec.value.toUpperCase()||'视频'
  const audioName=audioCodec.value.toUpperCase()
  if(mseMode.value&&audioTranscoding.value)return `${videoName} + ${audioName} → AAC · MSE 视频原码`
  if(mseMode.value)return `${videoName}${audioName?` + ${audioName}`:''} · MSE 原码`
  return transcoding.value?`${videoName} → H.264 · HLS 兼容转码`:`${videoName} · HLS 兼容封装`
})
const timelinePosition=computed(()=>pendingSeek.value??currentTime.value)
const progress=computed(()=>duration.value?Math.min(100,timelinePosition.value/duration.value*100):0)
const effectiveVolume=computed(()=>muted.value?0:volume.value)
const volumePercent=computed(()=>Math.round(effectiveVolume.value*100))
const volumeState=computed<'muted'|'low'|'high'>(()=>effectiveVolume.value===0?'muted':effectiveVolume.value<.5?'low':'high')
const cursorHidden=computed(()=>shouldHideVideoCursor({playing:playing.value,controlsVisible:controlsVisible.value,starting:starting.value,buffering:buffering.value,error:error.value}))
const {subtitleElement,subtitles,activeSubtitle,activeSubtitleLines,subtitlePlacement,selectedSubtitle,selectedSubtitleURL,selectedSubtitleKey,subtitleStyle,updateSubtitleBounds,applySubtitle,disableSubtitleTracks,chooseSubtitle,onSubtitleLoad,onSubtitleError}=useVideoSubtitles({video,directMode,mseMode,prepareMode,streamOffset,getPlayer:()=>player})
const {savedPosition,loadProgress,restoreDirectPosition,persistProgress,markUserSeeked,didUserSeek}=useVideoProgress({itemId:props.item.id,video,currentTime,duration,directMode})

function resetPlayback(){
  player?.destroy();player=null;hls?.destroy();hls=null
  if(video.value){video.value.pause();video.value.removeAttribute('src');video.value.load()}
}
function showControls(persist=false){
  controlsVisible.value=true;window.clearTimeout(controlsTimer)
  if(!persist&&playing.value)controlsTimer=window.setTimeout(()=>controlsVisible.value=false,2400)
}

interface MSEStartOptions { fresh?:boolean;suspectSessionID?:string;recoveryReason?:string }
async function startMSEStream(start:number,autoplay=true,recovery:MSEStartOptions={}){
  if(typeof MediaSource==='undefined'){await startCompatibilityStream(start,autoplay,'浏览器没有 MediaSource Extensions');return}
  const generation=++playbackGeneration
  currentTime.value=start
  const previousFMP4=recovery.suspectSessionID||fmp4SessionId||sessionStorage.getItem(fmp4SessionKey.value)||''
  if(recovery.fresh){fmp4SessionId='';sessionStorage.removeItem(fmp4SessionKey.value)}
  const previousHLS=hlsSessionId;hlsSessionId=''
  prepareKind.value=fmp4SessionId||previousHLS||player?'seek':'initial';prepareMode.value='mse'
  starting.value=true;buffering.value=false;autoplayPending.value=autoplay;error.value='';directMode.value=false;mseMode.value=true;directSource.value='';showControls(true)
  if(prepareKind.value==='seek')video.value?.pause()
  resetPlayback();releaseHLSSession(previousHLS)
  await nextTick();applySubtitle();if(generation!==playbackGeneration)return
  try{
    const metadata=await api<VideoFMP4Metadata>(`/api/files/${props.item.id}/video/fmp4`,{},70000)
    if(generation!==playbackGeneration)return
    const compatibility=await mseCompatibility(metadata)
    console.info('[revaro] MSE video supported:',compatibility.videoSupported,'power efficient:',compatibility.powerEfficient??'unknown')
    console.info('[revaro] MSE audio supported:',compatibility.audioSupported,'AAC supported:',compatibility.aacAudioSupported)
    console.info('[revaro] MSE combined copy supported:',compatibility.combinedCopySupported,'combined AAC supported:',compatibility.combinedAACSupported)
    if(compatibility.mode==='hls'){
      console.warn('[revaro] selected mode: hls-transcode; fallback reason:',compatibility.fallbackReason)
      if(recovery.fresh)void releaseFMP4Session(previousFMP4,true,compatibility.fallbackReason)
      mseRecoveryStarted=false
      await startCompatibilityStream(start,autoplayPending.value,compatibility.fallbackReason)
      return
    }
    if(compatibility.mode==='error'){
      if(recovery.fresh)void releaseFMP4Session(previousFMP4,true,compatibility.fallbackReason)
      starting.value=false;buffering.value=false;mseRecoveryStarted=false;error.value=`MSE 音频输出不可用：${compatibility.fallbackReason}`
      console.error('[revaro] selected mode: mse-error; HEVC remains untouched; reason:',compatibility.fallbackReason);showControls(true)
      return
    }
    const fallbackReason=recovery.recoveryReason||compatibility.fallbackReason
    console.info('[revaro][mse] requesting session',{target:start,fresh_session:Boolean(recovery.fresh),previous_session_id:previousFMP4||'none',reason:fallbackReason||'none'})
    const response=await api<VideoFMP4Response>(`/api/files/${props.item.id}/video/fmp4`,{method:'POST',body:JSON.stringify({start,audio_mode:compatibility.mode,previous_session_id:previousFMP4,fresh_session:Boolean(recovery.fresh),fallback_reason:fallbackReason})},70000)
    if(generation!==playbackGeneration){void releaseFMP4Session(response.session_id);return}
    const el=video.value;if(!el)throw new Error('播放器已经关闭')
    fmp4SessionId=response.session_id;sessionStorage.setItem(fmp4SessionKey.value,response.session_id);streamOffset.value=response.start;currentTime.value=start;duration.value=response.duration
    videoCodec.value=response.video_codec;audioCodec.value=response.audio_codec||'';transcoding.value=false;audioTranscoding.value=response.audio_transcoding
    const attachment=await attachFMP4Stream({
      element:el,response,mimeType:compatibility.mimeType,target:currentTime.value,autoplay:false,
      onFatal:reason=>recoverFromMSE(reason),
      onFragment:()=>{buffering.value=false},
    })
    if(generation!==playbackGeneration){attachment.destroy();void releaseFMP4Session(response.session_id);return}
    player=createUnifiedVideoPlayer('mse',el,response.start,attachment.destroy,targetTime=>{
      buffering.value=true;showControls(true)
      void startMSEStream(targetTime,playing.value||autoplayPending.value)
      return true
    });player.setVolume(volume.value,muted.value)
    starting.value=false;buffering.value=false;mseRecoveryStarted=false;applySubtitle()
    console.info('[revaro] selected mode:',response.selected_mode,'video:',response.video_codec,'audio:',response.audio_codec||'none','output audio:',response.output_audio_codec||'none')
    if(autoplayPending.value)void player.play().catch(()=>{});showControls()
  }catch(caught){
    if(generation!==playbackGeneration)return
    const failedSession=fmp4SessionId;fmp4SessionId='';resetPlayback();void releaseFMP4Session(failedSession,Boolean(recovery.fresh),recovery.recoveryReason||'MSE attach failed')
    const reason=caught instanceof Error?caught.message:'MSE fMP4 启动失败'
    const confirmedBrowserFailure=/SourceBuffer|媒体解码|无法创建 MSE/i.test(reason)
    if(recovery.fresh||confirmedBrowserFailure){starting.value=false;mseRecoveryStarted=false;recoverFromMSE(`MSE attachment failed: ${reason}`);return}
    starting.value=false;buffering.value=false;mseMode.value=true;error.value=`MSE 原码流启动失败：${reason}`
    console.error('[revaro] selected mode: mse-error; no H.264 fallback; reason:',reason);showControls(true)
  }
}
function recoverFromMSE(reason:string){
  if(mseRecoveryStarted||starting.value)return
  mseRecoveryStarted=true
  const target=authoritativeSeekTarget(currentTime.value,savedPosition(),didUserSeek())
  if(mseRecoveryAnchor<0||Math.abs(target-mseRecoveryAnchor)>5){mseRecoveryAnchor=target;mseFreshFailures=0}
  const action=mseRecoveryAction(mseFreshFailures)
  const autoplay=playing.value||autoplayPending.value
  const suspect=fmp4SessionId||sessionStorage.getItem(fmp4SessionKey.value)||''
  sessionStorage.removeItem(fmp4SessionKey.value)
  console.warn('[revaro][mse] recovery selected',{action,target,reason,fresh_failures:mseFreshFailures,suspect_session:suspect||'none'})
  if(action==='fresh-mse'){
    mseFreshFailures+=1
    void startMSEStream(target,autoplay,{fresh:true,suspectSessionID:suspect,recoveryReason:reason})
    return
  }
  mseRecoveryStarted=false
  console.warn('[revaro][mse] fallback to HLS after fresh recovery failures',{target,reason,fresh_failures:mseFreshFailures})
  void startCompatibilityStream(target,autoplay,`MSE fresh recovery exhausted: ${reason}`,true)
}
async function startCompatibilityStream(start:number,autoplay=true,fallbackReason='MSE 不可用',discardFMP4=false){
  const generation=++playbackGeneration
  currentTime.value=start
  const previous=hlsSessionId
  const previousFMP4=fmp4SessionId;fmp4SessionId=''
  prepareKind.value=previous||previousFMP4||player?'seek':'initial';prepareMode.value='hls'
  starting.value=true;buffering.value=false;autoplayPending.value=autoplay;error.value='';directMode.value=false;mseMode.value=false;directSource.value='';showControls(true)
  if(prepareKind.value==='seek')video.value?.pause()
  resetPlayback();void releaseFMP4Session(previousFMP4,discardFMP4,fallbackReason);await nextTick();applySubtitle()
  if(generation!==playbackGeneration)return
  try{
    const previousSessionID=previous||sessionStorage.getItem(sessionKey.value)||''
    const response=await api<VideoHLSResponse>(`/api/files/${props.item.id}/video/hls`,{method:'POST',body:JSON.stringify({start,previous_session_id:previousSessionID,fallback_reason:fallbackReason})},110000)
    if(generation!==playbackGeneration)return
    const el=video.value
    if(!el)throw new Error('播放器已经关闭')
    const {default:Hls}=await import('hls.js/light')
    if(generation!==playbackGeneration)return
    resetPlayback();applySubtitle()
    hlsSessionId=response.session_id;sessionStorage.setItem(sessionKey.value,response.session_id);streamOffset.value=response.start;currentTime.value=start;duration.value=response.duration
    videoCodec.value=response.video_codec;audioCodec.value=response.audio_codec;transcoding.value=response.transcoding;audioTranscoding.value=false
    if(Hls.isSupported()){
      const hlsPlayer=new Hls({enableWorker:true,lowLatencyMode:false,startFragPrefetch:false,backBufferLength:30,maxBufferLength:30,maxMaxBufferLength:60,maxBufferSize:64*1024*1024,maxBufferHole:.5,highBufferWatchdogPeriod:2,manifestLoadingTimeOut:15000,fragLoadingTimeOut:25000})
      hls=hlsPlayer;player=createUnifiedVideoPlayer('hls',el,response.start,()=>{hlsPlayer.destroy();if(hls===hlsPlayer)hls=null})
      player.setVolume(volume.value,muted.value)
      const currentSession=()=>generation===playbackGeneration&&hls===hlsPlayer
      hlsPlayer.on(Hls.Events.MEDIA_ATTACHED,()=>{if(currentSession())hlsPlayer.loadSource(response.playlist_url)})
      hlsPlayer.on(Hls.Events.MANIFEST_PARSED,()=>{if(!currentSession())return;el.currentTime=Math.max(0,start-response.start);starting.value=false;buffering.value=true;applySubtitle();console.info('[revaro] video playback mode=hls fallback=',fallbackReason);if(autoplayPending.value)void player?.play().catch(()=>{});showControls()})
      hlsPlayer.on(Hls.Events.ERROR,(_event,data)=>{if(!currentSession())return;if(data.fatal){starting.value=false;buffering.value=false;error.value=`兼容视频流播放失败：${data.details||'未知错误'}`;showControls(true)}else if(String(data.details).includes('buffer'))buffering.value=true})
      hlsPlayer.attachMedia(el)
    }else if(el.canPlayType('application/vnd.apple.mpegurl')){
      const localStart=Math.max(0,start-response.start)
      if(localStart>0)el.addEventListener('loadedmetadata',()=>{if(generation===playbackGeneration)el.currentTime=localStart},{once:true})
      el.src=response.playlist_url;el.load();player=createUnifiedVideoPlayer('hls',el,response.start,()=>{el.pause();el.removeAttribute('src');el.load()});player.setVolume(volume.value,muted.value)
      starting.value=false;buffering.value=true;applySubtitle();console.info('[revaro] video playback mode=native-hls fallback=',fallbackReason);if(autoplayPending.value)void player.play().catch(()=>{});showControls()
    }else throw new Error('当前浏览器不支持 HLS 播放')
  }catch(caught){
    if(generation!==playbackGeneration)return
    resetPlayback()
    starting.value=false;buffering.value=false;error.value=caught instanceof Error?caught.message:'兼容视频流启动失败';showControls(true)
  }
}
function onLoadedMetadata(){
  const el=video.value;if(!el)return
  el.volume=volume.value;el.muted=muted.value
  if(directMode.value){duration.value=Number.isFinite(el.duration)?el.duration:0;restoreDirectPosition()}
  applySubtitle()
  updateSubtitleBounds()
}
function syncPlaybackClock(){
  const el=video.value
  if(!el||!shouldSyncMediaClock(starting.value,el.paused))return false
  currentTime.value=mediaElementTimelineTime(el.currentTime,directMode.value?'direct':mseMode.value?'mse':'hls',streamOffset.value)
  return true
}
function stopPlaybackClock(){window.cancelAnimationFrame(clockFrame);clockFrame=0}
function runPlaybackClock(){
  stopPlaybackClock()
  const tick=()=>{
    const el=video.value
    if(!shouldContinueMediaClock(Boolean(el))){clockFrame=0;return}
    if(!el)return
    // Keep the sampler alive across transient/stale pause events produced while
    // an MSE source is attached. A later presented frame is authoritative even
    // when the browser does not emit another play/timeupdate event.
    if(!el.paused)syncPlaybackClock()
    clockFrame=window.requestAnimationFrame(tick)
  }
  clockFrame=window.requestAnimationFrame(tick)
}
function onTimeUpdate(){
  if(!syncPlaybackClock())return
  if(mseFreshFailures&&mseRecoveryAnchor>=0&&Math.abs(currentTime.value-mseRecoveryAnchor)>10){
    console.info('[revaro][mse] recovery streak cleared after playback advanced',{from:mseRecoveryAnchor,to:currentTime.value})
    mseFreshFailures=0;mseRecoveryAnchor=-1
  }
  window.clearTimeout(saveTimer);saveTimer=window.setTimeout(()=>persistProgress(false),600)
  if(!remoteSaveTimer)remoteSaveTimer=window.setTimeout(()=>{remoteSaveTimer=0;persistProgress(true)},5000)
}
function onWaiting(){
  if(starting.value)return
  buffering.value=true;showControls(true)
}
function onCanPlay(){buffering.value=false}
function onEnded(){
  onPause()
  if(!directMode.value&&!mseMode.value&&currentTime.value<duration.value-1)void startCompatibilityStream(currentTime.value+.05,true,'继续有限兼容流')
}
function onVideoError(){
  if(starting.value)return
  if(optimizedMode.value){error.value='Web 播放文件无法解码';return}
  if(directMode.value&&!directFallbackStarted){directFallbackStarted=true;void startMSEStream(currentTime.value||savedPosition(),true);return}
  if(mseMode.value)recoverFromMSE('浏览器报告 MSE 媒体解码错误')
}
function retryPlayback(){if(mseMode.value)void startMSEStream(currentTime.value||savedPosition(),true);else void startCompatibilityStream(currentTime.value||savedPosition(),true,'用户重试 HLS')}
function seekTo(target:number){
  const el=video.value
  if(!el||!Number.isFinite(target))return
  target=Math.max(0,Math.min(target,duration.value||target))
  markUserSeeked();currentTime.value=target
  if(starting.value){
    if(mseMode.value)void startMSEStream(target,autoplayPending.value)
    else if(!directMode.value)void startCompatibilityStream(target,autoplayPending.value,'用户在流准备期间重新定位')
    return
  }
  if(player?.seek(target)){currentTime.value=target;return}
  if(mseMode.value){buffering.value=true;showControls(true);return}
  void startCompatibilityStream(target,playing.value,'目标位置不在 HLS 已缓冲范围')
}
function previewSeek(event:Event){const value=Number((event.target as HTMLInputElement).value);if(Number.isFinite(value))pendingSeek.value=value;showControls(true)}
function commitSeek(event:Event){const value=Number((event.target as HTMLInputElement).value);pendingSeek.value=null;seekTo(value)}
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
  if(event.target instanceof HTMLInputElement||event.target instanceof HTMLSelectElement)return
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
  document.addEventListener('fullscreenchange',onFullscreenChange)
  if(video.value&&typeof ResizeObserver!=='undefined'){videoResizeObserver=new ResizeObserver(updateSubtitleBounds);videoResizeObserver.observe(video.value)}
  const storedVolume=Number(localStorage.getItem(volumeKey));if(Number.isFinite(storedVolume)&&storedVolume>=0&&storedVolume<=1){volume.value=storedVolume;muted.value=storedVolume===0;if(storedVolume>0)lastAudibleVolume=storedVolume}
  const progressPromise=loadProgress()
  try{
    const media=await api<VideoMediaResponse>(`/api/files/${props.item.id}/video`)
    if(media.optimized&&media.playback_url){optimizedMode.value=true;directMode.value=true;directSource.value=media.playback_url}
    subtitles.value=media.subtitles||[];activeSubtitle.value=initialSubtitleIndex(subtitles.value)
    console.info('[revaro] subtitles discovered:',subtitles.value.length)
    console.info('[revaro] subtitle selected:',selectedSubtitle.value?.id||'off')
    await nextTick();applySubtitle()
  }catch(caught){console.error('[revaro] subtitle discovery failed:',caught)}
  const el=video.value
  if(directMode.value&&el){player=createUnifiedVideoPlayer('direct',el);player.setVolume(volume.value,muted.value);el.load();void player.play().catch(()=>{})}
  else{await progressPromise;void startMSEStream(savedPosition(),true)}
})
onBeforeUnmount(()=>{
  document.removeEventListener('fullscreenchange',onFullscreenChange);videoResizeObserver?.disconnect();videoResizeObserver=null;window.clearTimeout(saveTimer);window.clearTimeout(remoteSaveTimer);window.clearTimeout(controlsTimer);window.clearTimeout(volumeTimer);stopPlaybackClock();persistProgress(false);playbackGeneration++;disableSubtitleTracks()
  if(currentTime.value>0)void fetch(`/api/files/${props.item.id}/media/progress`,{method:'PUT',headers:{'Content-Type':'application/json'},body:JSON.stringify({position:currentTime.value,duration:duration.value}),credentials:'same-origin',keepalive:true})
  const hlsSession=hlsSessionId;hlsSessionId='';const fmp4Session=fmp4SessionId;fmp4SessionId='';resetPlayback()
  releaseHLSSession(hlsSession);if(fmp4Session)void fetch(`/api/video/fmp4/${fmp4Session}`,{method:'DELETE',credentials:'same-origin',keepalive:true})
})
</script>

<template>
  <div ref="shell" class="video-player-shell" :class="{'cursor-hidden':cursorHidden}" tabindex="0" @mousemove="showControls()" @mouseleave="playing&&(controlsVisible=false)" @keydown="onKey">
    <video ref="video" :src="directSource||undefined" :poster="poster" crossorigin="anonymous" autoplay playsinline preload="metadata" @click="togglePlayback" @dblclick="toggleFullscreen" @loadedmetadata="onLoadedMetadata" @timeupdate="onTimeUpdate" @waiting="onWaiting" @stalled="onWaiting" @canplay="onCanPlay" @playing="onCanPlay" @play="onPlay" @pause="onPause" @ended="onEnded" @error="onVideoError">
      <track v-if="selectedSubtitle" ref="subtitleElement" :key="selectedSubtitleKey" kind="subtitles" :src="selectedSubtitleURL" :srclang="selectedSubtitle.language" :label="selectedSubtitle.label" @load="onSubtitleLoad" @error="onSubtitleError">
      你的浏览器不支持这个视频格式。
    </video>
    <div v-if="activeSubtitleLines.length" class="video-subtitle-overlay" :class="[subtitlePlacement,{raised:controlsVisible||!playing}]" :style="subtitleStyle" aria-live="off"><span v-for="(line,index) in activeSubtitleLines" :key="`${index}:${line}`" :class="subtitleLineClass(index)">{{ line }}</span></div>
    <VideoStatusOverlay :item-name="item.name" :direct-mode="directMode" :compatibility-label="compatibilityLabel" :controls-visible="controlsVisible" :playing="playing" :starting="starting" :error="error" :prepare-kind="prepareKind" :prepare-mode="prepareMode" :timeline-position="timelinePosition" :buffering="buffering" :format-time="formatTime" @close="emit('close')" @toggle-playback="togglePlayback" @retry="retryPlayback" />
    <VideoControls :visible="controlsVisible||!playing" :playing="playing" :starting="starting" :autoplay-pending="autoplayPending" :duration="duration" :timeline-position="timelinePosition" :progress="progress" :volume-state="volumeState" :volume-feedback="volumeFeedback" :volume-percent="volumePercent" :effective-volume="effectiveVolume" :subtitles="subtitles" :active-subtitle="activeSubtitle" :fullscreen="fullscreen" :format-time="formatTime" @toggle-playback="togglePlayback" @preview-seek="previewSeek" @commit-seek="commitSeek" @toggle-mute="toggleMute" @volume-start="volumeFeedback=true" @volume-end="showVolumeFeedback" @change-volume="changeVolume" @choose-subtitle="chooseSubtitle" @download="emit('download',item)" @move="emit('move',item)" @copy="emit('copy',item)" @toggle-fullscreen="toggleFullscreen" />
  </div>
</template>

<style src="../styles/video-player.css"></style>

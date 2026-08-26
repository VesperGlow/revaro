<script setup lang="ts">
import { computed, nextTick, onBeforeUnmount, onMounted, ref, watch } from 'vue'
import type HlsInstance from 'hls.js/light'
import type { DriveFile } from '../api'
import { api } from '../api'
import { previewURL, thumbSRC } from '../fileTypes'
import type { VideoFMP4Metadata, VideoFMP4Response, VideoHLSResponse, VideoMediaResponse, VideoSubtitleTrack } from '../types'
import { attachFMP4Stream, createUnifiedVideoPlayer, mseCompatibility, mseRecoveryAction, setExclusiveSubtitleTrack, shouldHideVideoCursor, subtitleTrackKey, subtitleURLForPlayback, type UnifiedVideoPlayer, type VideoPlaybackMode } from '../videoPlayer'

const props=defineProps<{item:DriveFile}>()
const emit=defineEmits<{close:[];download:[item:DriveFile];move:[item:DriveFile];copy:[item:DriveFile]}>()
const shell=ref<HTMLElement|null>(null)
const video=ref<HTMLVideoElement|null>(null)
const subtitleElement=ref<HTMLTrackElement|null>(null)
const actionMenu=ref<HTMLDetailsElement|null>(null)
const subtitles=ref<VideoSubtitleTrack[]>([])
const activeSubtitle=ref(-1)
const directMode=ref(/\.(mp4|m4v|webm|ogv|mov)$/i.test(props.item.name))
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
let lastAudibleVolume=.9
let directFallbackStarted=false
let mseRecoveryStarted=false
let mseFreshFailures=0
let mseRecoveryAnchor=-1
let progressLoaded=false
let restoredPosition=false
let serverPosition=0

const positionKey=computed(()=>`revaro-video-position:${props.item.id}`)
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
const selectedSubtitle=computed(()=>activeSubtitle.value>=0?subtitles.value[activeSubtitle.value]:undefined)
const subtitlePlaybackMode=computed<VideoPlaybackMode>(()=>directMode.value?'direct':mseMode.value||prepareMode.value==='mse'?'mse':'hls')
const selectedSubtitleURL=computed(()=>{
  const track=selectedSubtitle.value
  if(!track)return ''
  return subtitleURLForPlayback(track.url,subtitlePlaybackMode.value,streamOffset.value)
})
const selectedSubtitleKey=computed(()=>selectedSubtitle.value?subtitleTrackKey(selectedSubtitle.value.id,subtitlePlaybackMode.value,streamOffset.value):'')
const cursorHidden=computed(()=>shouldHideVideoCursor({playing:playing.value,controlsVisible:controlsVisible.value,starting:starting.value,buffering:buffering.value,error:error.value}))

function formatTime(seconds:number){
  if(!Number.isFinite(seconds)||seconds<0)return '0:00'
  const value=Math.floor(seconds);const hours=Math.floor(value/3600);const minutes=Math.floor(value%3600/60);const secs=value%60
  return hours?`${hours}:${String(minutes).padStart(2,'0')}:${String(secs).padStart(2,'0')}`:`${minutes}:${String(secs).padStart(2,'0')}`
}
function savedPosition(){
  if(serverPosition>0)return serverPosition
  const value=Number(localStorage.getItem(positionKey.value)||0);return Number.isFinite(value)&&value>0?value:0
}
async function loadProgress(){
  try{const value=await api<{position:number}>(`/api/files/${props.item.id}/media/progress`);serverPosition=Number.isFinite(value.position)?value.position:0}
  catch{/* 本机进度仍可兜底 */}
  progressLoaded=true;restoreDirectPosition()
}
function restoreDirectPosition(){
  const el=video.value;if(!progressLoaded||restoredPosition||!directMode.value||!el||!duration.value)return
  restoredPosition=true;const saved=savedPosition();if(saved>0&&saved<duration.value-5){el.currentTime=saved;currentTime.value=saved}
}
function persistProgress(remote=false){
  const position=Math.max(0,currentTime.value)
  if(position<=0)return
  localStorage.setItem(positionKey.value,String(Math.floor(position)))
  if(!remote)return
  void api(`/api/files/${props.item.id}/media/progress`,{method:'PUT',body:JSON.stringify({position,duration:duration.value})}).catch(()=>{})
}
function applySubtitle(){
  const tracks=video.value?.textTracks
  if(!tracks)return
  const current=activeSubtitle.value>=0?subtitleElement.value?.track:null
  if(player)player.setSubtitle(current||null)
  else setExclusiveSubtitleTrack(tracks,current||null)
}
function disableSubtitleTracks(){
  const tracks=video.value?.textTracks
  if(tracks)setExclusiveSubtitleTrack(tracks,null)
}
async function chooseSubtitle(event:Event){
  activeSubtitle.value=Number((event.target as HTMLSelectElement).value)
  if(activeSubtitle.value<0){disableSubtitleTracks();console.info('[revaro] subtitle selected: off');return}
  await nextTick();applySubtitle()
  console.info('[revaro] subtitle selected:',selectedSubtitle.value?.id||'none')
}
function onSubtitleLoad(event:Event){
  if(event.currentTarget!==subtitleElement.value)return
  const track=(event.currentTarget as HTMLTrackElement).track
  applySubtitle();track.mode='showing'
  console.info('[revaro] subtitle track loaded')
  console.info('[revaro] subtitle cues:',track.cues?.length??0)
  console.info('[revaro] subtitle mode:',track.mode)
}
function onSubtitleError(event:Event){
  const element=event.currentTarget as HTMLTrackElement
  const url=selectedSubtitleURL.value
  console.error('[revaro] subtitle error','url:',url,'track readyState:',element.readyState)
  if(!url)return
  void fetch(url,{credentials:'same-origin',cache:'no-store'}).then(async response=>{
    console.error('[revaro] subtitle HTTP status:',response.status,'content-type:',response.headers.get('content-type')||'missing','track readyState:',element.readyState)
    await response.body?.cancel()
  }).catch(caught=>console.error('[revaro] subtitle diagnostic request failed:',caught))
}
function resetPlayback(){
  player?.destroy();player=null;hls?.destroy();hls=null
  if(video.value){video.value.pause();video.value.removeAttribute('src');video.value.load()}
}
function showControls(persist=false){
  controlsVisible.value=true;window.clearTimeout(controlsTimer)
  if(!persist&&playing.value)controlsTimer=window.setTimeout(()=>controlsVisible.value=false,2400)
}

async function releaseFMP4Session(id:string,discard=false,reason=''){
  if(!id)return
  const query=discard?`?discard=1&reason=${encodeURIComponent(reason)}`:''
  try{await api(`/api/video/fmp4/${id}${query}`,{method:'DELETE'})}catch{/* 服务端 TTL 仍会兜底 */}
}
function releaseHLSSession(id:string){
  if(id)void fetch(`/api/video/hls/${id}`,{method:'DELETE',credentials:'same-origin',keepalive:true})
}
interface MSEStartOptions { fresh?:boolean;suspectSessionID?:string;recoveryReason?:string }
async function startMSEStream(start:number,autoplay=true,recovery:MSEStartOptions={}){
  if(typeof MediaSource==='undefined'){await startCompatibilityStream(start,autoplay,'浏览器没有 MediaSource Extensions');return}
  const generation=++playbackGeneration
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
    player=createUnifiedVideoPlayer('mse',el,response.start,attachment.destroy,targetTime=>{buffering.value=true;showControls(true);return attachment.seek(targetTime)});player.setVolume(volume.value,muted.value)
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
  const target=currentTime.value||savedPosition()
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
      const hlsPlayer=new Hls({enableWorker:true,lowLatencyMode:false,startFragPrefetch:true,backBufferLength:60,maxBufferLength:90,maxMaxBufferLength:180,maxBufferSize:256*1024*1024,maxBufferHole:.5,highBufferWatchdogPeriod:2,manifestLoadingTimeOut:15000,fragLoadingTimeOut:25000})
      hls=hlsPlayer;player=createUnifiedVideoPlayer('hls',el,response.start,()=>{hlsPlayer.destroy();if(hls===hlsPlayer)hls=null})
      player.setVolume(volume.value,muted.value)
      hlsPlayer.on(Hls.Events.MEDIA_ATTACHED,()=>hlsPlayer.loadSource(response.playlist_url))
      hlsPlayer.on(Hls.Events.MANIFEST_PARSED,()=>{el.currentTime=Math.max(0,start-response.start);starting.value=false;buffering.value=true;applySubtitle();console.info('[revaro] video playback mode=hls fallback=',fallbackReason);if(autoplayPending.value)void player?.play().catch(()=>{});showControls()})
      hlsPlayer.on(Hls.Events.ERROR,(_event,data)=>{if(data.fatal){starting.value=false;buffering.value=false;error.value=`兼容视频流播放失败：${data.details||'未知错误'}`;showControls(true)}else if(String(data.details).includes('buffer'))buffering.value=true})
      hlsPlayer.attachMedia(el)
    }else if(el.canPlayType('application/vnd.apple.mpegurl')){
      const localStart=Math.max(0,start-response.start)
      if(localStart>0)el.addEventListener('loadedmetadata',()=>{el.currentTime=localStart},{once:true})
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
}
function onTimeUpdate(){
  const el=video.value;if(!el)return
  currentTime.value=(directMode.value?0:streamOffset.value)+el.currentTime
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
function onVideoError(){
  if(starting.value)return
  if(directMode.value&&!directFallbackStarted){directFallbackStarted=true;void startMSEStream(currentTime.value||savedPosition(),true);return}
  if(mseMode.value)recoverFromMSE('浏览器报告 MSE 媒体解码错误')
}
function seekTo(target:number){
  const el=video.value
  if(!el||!Number.isFinite(target))return
  target=Math.max(0,Math.min(target,duration.value||target))
  if(starting.value){currentTime.value=target;return}
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
function onPlay(){playing.value=true;buffering.value=false;showControls()}
function onPause(){playing.value=false;window.clearTimeout(remoteSaveTimer);remoteSaveTimer=0;persistProgress(true);showControls(true)}
function showVolumeFeedback(){volumeFeedback.value=true;window.clearTimeout(volumeTimer);volumeTimer=window.setTimeout(()=>volumeFeedback.value=false,900);showControls(true)}
function changeVolume(event:Event){const value=Math.max(0,Math.min(1,Number((event.target as HTMLInputElement).value)));volume.value=value;if(value>0)lastAudibleVolume=value;muted.value=value===0;localStorage.setItem(volumeKey,String(value));player?.setVolume(value,muted.value);showVolumeFeedback()}
function toggleMute(){if(muted.value||volume.value===0){if(volume.value===0)volume.value=lastAudibleVolume;muted.value=false}else muted.value=true;player?.setVolume(volume.value,muted.value);showVolumeFeedback()}
async function toggleFullscreen(){
  if(shell.value)await player?.requestFullscreen(shell.value)
}
function onFullscreenChange(){fullscreen.value=document.fullscreenElement===shell.value}
function closeActionMenuFromOutside(event:PointerEvent){const target=event.target;if(actionMenu.value?.open&&target instanceof Node&&!actionMenu.value.contains(target))actionMenu.value.open=false}
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
  document.addEventListener('pointerdown',closeActionMenuFromOutside)
  const storedVolume=Number(localStorage.getItem(volumeKey));if(Number.isFinite(storedVolume)&&storedVolume>=0&&storedVolume<=1){volume.value=storedVolume;muted.value=storedVolume===0;if(storedVolume>0)lastAudibleVolume=storedVolume}
  const progressPromise=loadProgress()
  try{
    const media=await api<VideoMediaResponse>(`/api/files/${props.item.id}/video`)
    subtitles.value=media.subtitles||[];activeSubtitle.value=subtitles.value.length?0:-1
    console.info('[revaro] subtitles discovered:',subtitles.value.length)
    console.info('[revaro] subtitle selected:',selectedSubtitle.value?.id||'off')
    await nextTick();applySubtitle()
  }catch(caught){console.error('[revaro] subtitle discovery failed:',caught)}
  const el=video.value
  if(directMode.value&&el){player=createUnifiedVideoPlayer('direct',el);player.setVolume(volume.value,muted.value);el.load();void player.play().catch(()=>{})}
  else{await progressPromise;void startMSEStream(savedPosition(),true)}
})
onBeforeUnmount(()=>{
  document.removeEventListener('fullscreenchange',onFullscreenChange);document.removeEventListener('pointerdown',closeActionMenuFromOutside);window.clearTimeout(saveTimer);window.clearTimeout(remoteSaveTimer);window.clearTimeout(controlsTimer);window.clearTimeout(volumeTimer);persistProgress(false);playbackGeneration++;disableSubtitleTracks()
  if(currentTime.value>0)void fetch(`/api/files/${props.item.id}/media/progress`,{method:'PUT',headers:{'Content-Type':'application/json'},body:JSON.stringify({position:currentTime.value,duration:duration.value}),credentials:'same-origin',keepalive:true})
  const hlsSession=hlsSessionId;hlsSessionId='';const fmp4Session=fmp4SessionId;fmp4SessionId='';resetPlayback()
  releaseHLSSession(hlsSession);if(fmp4Session)void fetch(`/api/video/fmp4/${fmp4Session}`,{method:'DELETE',credentials:'same-origin',keepalive:true})
})
</script>

<template>
  <div ref="shell" class="video-player-shell" :class="{'cursor-hidden':cursorHidden}" tabindex="0" @mousemove="showControls()" @mouseleave="playing&&(controlsVisible=false)" @keydown="onKey">
    <video ref="video" :src="directSource||undefined" :poster="poster" autoplay playsinline preload="metadata" @click="togglePlayback" @dblclick="toggleFullscreen" @loadedmetadata="onLoadedMetadata" @timeupdate="onTimeUpdate" @waiting="onWaiting" @stalled="onWaiting" @canplay="onCanPlay" @playing="onCanPlay" @play="onPlay" @pause="onPause" @ended="onPause" @error="onVideoError">
      <track v-if="selectedSubtitle" ref="subtitleElement" :key="selectedSubtitleKey" kind="subtitles" :src="selectedSubtitleURL" :srclang="selectedSubtitle.language" :label="selectedSubtitle.label" default @load="onSubtitleLoad" @error="onSubtitleError">
      你的浏览器不支持这个视频格式。
    </video>
    <div class="video-top-shade" :class="{visible:controlsVisible||!playing}"><div class="video-title-group"><button class="video-back" aria-label="退出播放" @click.stop="emit('close')"><svg viewBox="0 0 24 24"><path d="m15 5-7 7 7 7"/></svg></button><strong :title="item.name">{{ item.name }}</strong></div><span v-if="!directMode">{{ compatibilityLabel }}</span></div>
    <button v-if="!playing&&!starting&&!error" class="video-center-play" aria-label="播放" @click.stop="togglePlayback"><svg viewBox="0 0 24 24"><path d="m9 7 9 5-9 5Z"/></svg></button>
    <div v-if="starting" class="video-loading" :class="{compact:prepareKind==='seek'}"><span></span><strong>{{ prepareKind==='seek'?`正在定位到 ${formatTime(timelinePosition)}`:prepareMode==='mse'?'正在准备 MSE 原码流':'正在准备 HLS 兼容流' }}</strong><small v-if="prepareKind==='initial'">{{ prepareMode==='mse'?'正在重封装 fMP4，不会重新编码视频':'正在生成启动缓冲，首批分片完成后自动播放' }}</small></div>
    <div v-else-if="buffering" class="video-buffering"><span></span><strong>正在缓冲</strong></div>
    <p v-if="error" class="video-error">{{ error }} <button @click="mseMode?startMSEStream(currentTime||savedPosition(),true):startCompatibilityStream(currentTime||savedPosition(),true,'用户重试 HLS')">重试</button></p>
    <div class="video-controls" :class="{visible:controlsVisible||!playing}" @click.stop>
      <input class="video-seek" type="range" min="0" :max="Math.max(duration,1)" step=".25" :value="Math.min(timelinePosition,Math.max(duration,1))" :style="{'--video-progress':`${progress}%`}" :disabled="!duration" aria-label="视频进度" @input="previewSeek" @change="commitSeek">
      <div class="video-control-row">
        <button class="video-icon-button" :aria-label="playing||(starting&&autoplayPending)?'暂停':'播放'" @click="togglePlayback"><svg v-if="playing||(starting&&autoplayPending)" viewBox="0 0 24 24"><path d="M8 6v12M16 6v12"/></svg><svg v-else viewBox="0 0 24 24"><path d="m9 7 9 5-9 5Z"/></svg></button>
        <button class="video-icon-button video-mute" :class="volumeState" :aria-label="volumeState==='muted'?'取消静音':'静音'" @click="toggleMute"><svg viewBox="0 0 24 24"><path d="M5 10h3l4-3v10l-4-3H5Z"/><path v-if="volumeState==='low'" d="M15 9c1.5 1.5 1.5 4.5 0 6"/><template v-else-if="volumeState==='high'"><path d="M15 9c1.5 1.5 1.5 4.5 0 6"/><path d="M18 7c3 3 3 7 0 10"/></template><path v-else d="m16 10 4 4m0-4-4 4"/></svg></button>
        <label class="video-volume-control" :class="{active:volumeFeedback,muted:volumeState==='muted'}" :style="{'--volume-progress':`${volumePercent}%`}"><input class="video-volume" type="range" min="0" max="1" step=".01" :value="effectiveVolume" aria-label="音量" :aria-valuetext="`${volumePercent}%`" @pointerdown="volumeFeedback=true" @pointerup="showVolumeFeedback" @input="changeVolume"><output>{{ volumePercent }}%</output></label>
        <span class="video-time">{{ formatTime(timelinePosition) }} / {{ formatTime(duration) }}</span>
        <span class="video-control-spacer"></span>
        <label class="video-subtitles" :class="{disabled:!subtitles.length}"><span>CC</span><select :value="activeSubtitle" :disabled="!subtitles.length" aria-label="字幕" @change="chooseSubtitle"><option value="-1">关闭字幕</option><option v-for="(track,index) in subtitles" :key="track.id" :value="index">{{ track.label }}</option></select></label>
        <details ref="actionMenu" class="video-action-menu"><summary class="video-icon-button" aria-label="更多操作"><svg viewBox="0 0 24 24"><path d="M5 7h14M5 12h14M5 17h14"/></svg></summary><div><button @click="actionMenu?.removeAttribute('open');emit('download',item)">下载</button><button @click="actionMenu?.removeAttribute('open');emit('move',item)">移动</button><button @click="actionMenu?.removeAttribute('open');emit('copy',item)">复制</button></div></details>
        <button class="video-icon-button" :aria-label="fullscreen?'退出全屏':'全屏'" @click="toggleFullscreen"><svg viewBox="0 0 24 24"><path v-if="!fullscreen" d="M8 4H4v4M16 4h4v4M8 20H4v-4M16 20h4v-4"/><path v-else d="M4 8h4V4M20 8h-4V4M4 16h4v4M20 16h-4v4"/></svg></button>
      </div>
    </div>
  </div>
</template>

<style scoped>
.video-player-shell{position:relative;isolation:isolate;justify-self:center;width:min(1500px,100%);max-width:100%;height:min(820px,calc(100vh - 178px));height:min(820px,calc(100dvh - 178px));min-width:0;min-height:280px;overflow:hidden;border-radius:15px;background:#000;box-shadow:0 18px 46px #39517226;outline:none}
.video-player-shell:fullscreen{width:100vw;height:100vh;height:100dvh;border-radius:0}
.video-player-shell video{display:block;width:100%;height:100%;max-width:none;max-height:none;border-radius:0;background:#000;object-fit:contain;box-shadow:none;cursor:pointer}
.video-player-shell.cursor-hidden,.video-player-shell.cursor-hidden *{cursor:none!important}
.video-top-shade{position:absolute;z-index:9;inset:0 0 auto;display:flex;align-items:flex-start;justify-content:space-between;gap:18px;padding:max(18px,env(safe-area-inset-top,0px)) 22px 60px;background:linear-gradient(#000c,transparent);color:#fff;opacity:0;pointer-events:none;transition:opacity .2s}
.video-top-shade.visible{opacity:1}.video-title-group{display:flex;align-items:center;min-width:0;gap:8px}.video-top-shade strong{min-width:0;max-width:100%;overflow:hidden;font-size:17px;text-overflow:ellipsis;white-space:nowrap;text-shadow:0 1px 3px #000}.video-top-shade>span{flex:0 0 auto;padding:6px 9px;border:1px solid #ffffff3d;border-radius:999px;background:#0005;color:#d9e7f6;font-size:10px}
.video-back{display:grid;place-items:center;flex:0 0 auto;width:42px;height:42px;padding:0;border:0;border-radius:50%;background:#0b1220a8;color:#fff;pointer-events:auto}.video-back:hover{background:#ffffff24}.video-back svg{width:27px;height:27px;fill:none;stroke:currentColor;stroke-width:2.2;stroke-linecap:round;stroke-linejoin:round}
.video-center-play{position:absolute;z-index:6;top:50%;left:50%;display:grid;place-items:center;width:76px;height:54px;padding:0;border:0;border-radius:15px;background:#ff0033e8;color:#fff;box-shadow:0 8px 28px #0007;transform:translate(-50%,-50%);transition:transform .16s,background .16s}.video-center-play:hover{background:#ff0033;transform:translate(-50%,-50%) scale(1.06)}.video-center-play svg{width:33px;height:33px;fill:currentColor;stroke:none}
.video-controls{position:absolute;z-index:8;right:0;bottom:0;left:0;min-width:0;padding:62px 20px max(12px,env(safe-area-inset-bottom,0px));background:linear-gradient(transparent,#000e);color:#fff;opacity:0;pointer-events:none;transform:translateY(8px);transition:opacity .2s,transform .2s}.video-controls.visible{opacity:1;pointer-events:auto;transform:none}
.video-seek{display:block;width:100%;height:17px;margin:-6px 0 0;padding:6px 0;appearance:none;background:transparent;cursor:pointer}.video-seek::-webkit-slider-runnable-track{height:5px;border-radius:999px;background:linear-gradient(to right,#f03 var(--video-progress),#ffffff55 var(--video-progress))}.video-seek::-moz-range-track{height:5px;border-radius:999px;background:linear-gradient(to right,#f03 var(--video-progress),#ffffff55 var(--video-progress))}.video-seek::-webkit-slider-thumb{width:16px;height:16px;margin-top:-5.5px;appearance:none;border:0;border-radius:50%;background:#f03;box-shadow:0 0 0 3px #ff00333d}.video-seek::-moz-range-thumb{width:16px;height:16px;border:0;border-radius:50%;background:#f03;box-shadow:0 0 0 3px #ff00333d}
.video-control-row{display:flex;align-items:center;min-width:0;min-height:50px;gap:6px}.video-icon-button{display:grid;place-items:center;flex:0 0 auto;width:46px;height:46px;padding:0;border:0;border-radius:50%;background:transparent;color:#fff}.video-icon-button:hover,.video-icon-button:focus-visible{background:#ffffff1f;outline:none}.video-icon-button svg{width:28px;height:28px;fill:none;stroke:currentColor;stroke-width:2;stroke-linecap:round;stroke-linejoin:round}.video-icon-button svg path[d*="9 7"]{fill:currentColor;stroke:none}.video-mute.low{color:#dbeafe}.video-mute.muted{color:#fda4af}
.video-volume-control{position:relative;display:flex;align-items:center;flex:0 1 126px;width:126px;height:40px;min-width:82px;padding:0 9px;border-radius:10px;transition:background .15s}.video-volume-control:hover,.video-volume-control:focus-within,.video-volume-control.active{background:#ffffff17}.video-volume{width:100%;height:32px;margin:0;appearance:none;background:transparent;cursor:pointer;touch-action:pan-x}.video-volume::-webkit-slider-runnable-track{height:6px;border-radius:999px;background:linear-gradient(to right,#fff var(--volume-progress),#ffffff52 var(--volume-progress))}.video-volume::-moz-range-track{height:6px;border-radius:999px;background:linear-gradient(to right,#fff var(--volume-progress),#ffffff52 var(--volume-progress))}.video-volume::-webkit-slider-thumb{width:18px;height:18px;margin-top:-6px;appearance:none;border:2px solid #fff;border-radius:50%;background:#111827;box-shadow:0 2px 8px #0008;transition:transform .12s}.video-volume::-moz-range-thumb{width:18px;height:18px;border:2px solid #fff;border-radius:50%;background:#111827;box-shadow:0 2px 8px #0008}.video-volume:active::-webkit-slider-thumb,.video-volume:focus-visible::-webkit-slider-thumb{transform:scale(1.15)}.video-volume:focus-visible{outline:2px solid #93c5fd;outline-offset:3px}.video-volume-control.muted .video-volume::-webkit-slider-runnable-track{background:#ffffff42}.video-volume-control.muted .video-volume::-moz-range-track{background:#ffffff42}.video-volume-control output{position:absolute;left:50%;bottom:38px;min-width:44px;padding:5px 7px;border-radius:7px;background:#111827e8;color:#fff;font-size:10px;text-align:center;opacity:0;transform:translate(-50%,5px);transition:.14s;pointer-events:none}.video-volume-control:hover output,.video-volume-control:focus-within output,.video-volume-control.active output{opacity:1;transform:translate(-50%,0)}
.video-time{margin-left:5px;font:12px ui-monospace,SFMono-Regular,Consolas,monospace;white-space:nowrap;text-shadow:0 1px 2px #000}.video-control-spacer{min-width:12px;flex:1}.video-subtitles{position:relative;display:flex;align-items:center;flex:0 0 auto;margin-left:5px}.video-subtitles>span{display:grid;place-items:center;width:38px;height:31px;border:2px solid currentColor;border-radius:6px;font-size:10px;font-weight:850;letter-spacing:.04em}.video-subtitles select{position:absolute;right:0;bottom:40px;width:min(300px,70vw);padding:11px;border:1px solid #ffffff30;border-radius:9px;background:#171717ec;color:#fff;font-size:12px;opacity:0;pointer-events:none;transform:translateY(6px);transition:.15s}.video-subtitles:hover select,.video-subtitles select:focus{opacity:1;pointer-events:auto;transform:none}.video-subtitles.disabled{opacity:.45}
.video-loading{position:absolute;z-index:7;inset:0;display:grid;place-content:center;justify-items:center;padding:24px;background:#0009;color:#fff;text-align:center;pointer-events:none}.video-loading.compact{inset:auto auto 92px 50%;display:flex;align-items:center;padding:9px 13px;border:1px solid #ffffff2b;border-radius:999px;background:#111827d9;transform:translateX(-50%);gap:9px;white-space:nowrap}.video-loading span,.video-buffering span{width:42px;height:42px;border:3px solid #ffffff30;border-top-color:#fff;border-radius:50%;animation:video-spin .8s linear infinite}.video-loading.compact span,.video-buffering span{width:18px;height:18px;border-width:2px}.video-loading strong{margin-top:14px;font-size:15px}.video-loading.compact strong{margin:0;font-size:11px}.video-loading small{max-width:420px;margin-top:7px;color:#c8d2df;font-size:11px;line-height:1.6}.video-buffering{position:absolute;z-index:7;top:50%;left:50%;display:flex;align-items:center;padding:9px 13px;border-radius:999px;background:#111827c9;color:#fff;transform:translate(-50%,-50%);gap:9px;pointer-events:none}.video-buffering strong{font-size:11px}
.video-error{position:absolute;z-index:10;left:50%;bottom:86px;max-width:calc(100% - 30px);margin:0;padding:9px 12px;border:1px solid #fecaca;border-radius:10px;background:#fff1f2ed;color:#be123c;font-size:11px;transform:translateX(-50%)}.video-error button{margin-left:8px;border:0;background:transparent;color:#9f1239;font-weight:800}
.video-action-menu{position:relative;flex:0 0 auto}.video-action-menu summary{list-style:none}.video-action-menu summary::-webkit-details-marker{display:none}.video-action-menu>div{position:absolute;right:0;bottom:45px;display:grid;width:130px;overflow:hidden;border:1px solid #ffffff24;border-radius:11px;background:#171717f2;box-shadow:0 12px 32px #0007}.video-action-menu>div button{padding:10px 14px;border:0;background:transparent;color:#fff;text-align:left}.video-action-menu>div button:hover{background:#ffffff14}
@keyframes video-spin{to{transform:rotate(360deg)}}
@media(max-width:850px){.video-player-shell{width:100%;height:100%;min-height:0;border-radius:10px}.video-top-shade{padding:max(10px,env(safe-area-inset-top,0px)) 12px 48px}.video-title-group{gap:4px}.video-back{width:39px;height:39px}.video-back svg{width:24px;height:24px}.video-top-shade strong{font-size:13px}.video-top-shade>span{display:none}.video-controls{padding:46px 8px max(7px,env(safe-area-inset-bottom,0px))}.video-control-row{min-height:43px;gap:1px}.video-icon-button{width:39px;height:39px}.video-icon-button svg{width:23px;height:23px}.video-volume-control{flex-basis:clamp(76px,22vw,108px);width:clamp(76px,22vw,108px);min-width:70px;height:39px;padding:0 7px}.video-volume{height:38px}.video-volume::-webkit-slider-runnable-track{height:7px}.video-volume::-moz-range-track{height:7px}.video-volume::-webkit-slider-thumb{width:20px;height:20px;margin-top:-6.5px}.video-volume::-moz-range-thumb{width:20px;height:20px}.video-time{margin-left:2px;font-size:10px}.video-control-spacer{min-width:5px}.video-subtitles{margin-left:5px}.video-center-play{width:64px;height:47px}.video-subtitles select{inset:0;width:100%;height:100%;padding:0;opacity:0;pointer-events:auto;transform:none}.video-loading.compact{bottom:67px}.video-error{bottom:66px}}
@media(max-width:520px){.video-time{display:none}.video-control-spacer{flex:1}.video-volume-control{flex-basis:clamp(72px,24vw,98px);min-width:68px}.video-action-menu .video-icon-button,.video-control-row>.video-icon-button{width:38px}.video-loading.compact{max-width:calc(100% - 20px)}}
</style>

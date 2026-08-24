<script setup lang="ts">
import { computed, nextTick, onBeforeUnmount, onMounted, ref } from 'vue'
import type HlsInstance from 'hls.js/light'
import type { DriveFile } from '../api'
import { api } from '../api'
import { previewURL, thumbSRC } from '../fileTypes'
import type { VideoHLSResponse, VideoMediaResponse, VideoSubtitleTrack } from '../types'

const props=defineProps<{item:DriveFile}>()
const emit=defineEmits<{close:[];download:[item:DriveFile];move:[item:DriveFile];copy:[item:DriveFile]}>()
const shell=ref<HTMLElement|null>(null)
const video=ref<HTMLVideoElement|null>(null)
const actionMenu=ref<HTMLDetailsElement|null>(null)
const subtitles=ref<VideoSubtitleTrack[]>([])
const activeSubtitle=ref(-1)
const subtitleReady=ref(false)
const subtitleEpoch=ref(0)
const directMode=ref(/\.(mp4|m4v|webm|ogv|mov)$/i.test(props.item.name))
const directSource=ref(directMode.value?previewURL(props.item):'')
const starting=ref(false)
const playing=ref(false)
const error=ref('')
const currentTime=ref(0)
const duration=ref(0)
const hlsOffset=ref(0)
const videoCodec=ref('')
const audioCodec=ref('')
const transcoding=ref(false)
const controlsVisible=ref(true)
const volume=ref(.9)
const muted=ref(false)
const fullscreen=ref(false)
const autoplayPending=ref(true)
let hls:HlsInstance|null=null
let hlsSessionId=''
let hlsGeneration=0
let saveTimer=0
let remoteSaveTimer=0
let controlsTimer=0
let directFallbackStarted=false
let progressLoaded=false
let restoredPosition=false
let serverPosition=0

const positionKey=computed(()=>`revaro-video-position:${props.item.id}`)
const sessionKey=computed(()=>`revaro-video-hls-session:${props.item.id}`)
const poster=computed(()=>thumbSRC(props.item))
const compatibilityLabel=computed(()=>transcoding.value?`${videoCodec.value.toUpperCase()} → H.264 实时转码`:`${videoCodec.value.toUpperCase()||'视频'} HLS 封装`)
const progress=computed(()=>duration.value?Math.min(100,currentTime.value/duration.value*100):0)
const selectedSubtitle=computed(()=>activeSubtitle.value>=0?subtitles.value[activeSubtitle.value]:undefined)

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
  for(let index=0;index<tracks.length;index+=1)tracks[index].mode=selectedSubtitle.value?'showing':'disabled'
}
function refreshSubtitle(){subtitleEpoch.value+=1;void nextTick().then(applySubtitle)}
function chooseSubtitle(event:Event){activeSubtitle.value=Number((event.target as HTMLSelectElement).value);refreshSubtitle()}
function onSubtitleLoad(event:Event){
  const track=(event.currentTarget as HTMLTrackElement).track
  const offset=directMode.value?0:hlsOffset.value
  if(offset>0&&track.cues){
    for(const cue of Array.from(track.cues)){
      if(cue.endTime<=offset){track.removeCue(cue);continue}
      cue.startTime=Math.max(0,cue.startTime-offset)
      cue.endTime=Math.max(cue.startTime+.001,cue.endTime-offset)
    }
  }
  track.mode='showing'
}
function resetHLS(){hls?.destroy();hls=null;if(video.value){video.value.pause();video.value.removeAttribute('src');video.value.load()}}
function showControls(persist=false){
  controlsVisible.value=true;window.clearTimeout(controlsTimer)
  if(!persist&&playing.value)controlsTimer=window.setTimeout(()=>controlsVisible.value=false,2400)
}

async function startCompatibilityStream(start:number,autoplay=true){
  const generation=++hlsGeneration
  const previous=hlsSessionId
  starting.value=true;subtitleReady.value=false;autoplayPending.value=autoplay;error.value='';directMode.value=false;directSource.value='';showControls(true)
  resetHLS()
  try{
    const previousSessionID=previous||sessionStorage.getItem(sessionKey.value)||''
    const response=await api<VideoHLSResponse>(`/api/files/${props.item.id}/video/hls`,{method:'POST',body:JSON.stringify({start,previous_session_id:previousSessionID})},110000)
    if(generation!==hlsGeneration)return
    const el=video.value
    if(!el)throw new Error('播放器已经关闭')
    hlsSessionId=response.session_id;sessionStorage.setItem(sessionKey.value,response.session_id);hlsOffset.value=response.start;currentTime.value=response.start;duration.value=response.duration
    videoCodec.value=response.video_codec;audioCodec.value=response.audio_codec;transcoding.value=response.transcoding
    const {default:Hls}=await import('hls.js/light')
    if(Hls.isSupported()){
      const player=new Hls({enableWorker:true,lowLatencyMode:false,backBufferLength:90,maxBufferLength:90,maxMaxBufferLength:120,maxBufferSize:256*1024*1024})
      hls=player
      player.on(Hls.Events.MEDIA_ATTACHED,()=>player.loadSource(response.playlist_url))
      player.on(Hls.Events.MANIFEST_PARSED,()=>{el.currentTime=Math.max(0,start-response.start);starting.value=false;subtitleReady.value=true;refreshSubtitle();if(autoplayPending.value)void el.play().catch(()=>{});showControls()})
      player.on(Hls.Events.ERROR,(_event,data)=>{if(data.fatal){starting.value=false;subtitleReady.value=false;error.value='兼容视频流播放失败，请重试';showControls(true)}})
      player.attachMedia(el)
    }else if(el.canPlayType('application/vnd.apple.mpegurl')){
      const localStart=Math.max(0,start-response.start)
      if(localStart>0)el.addEventListener('loadedmetadata',()=>{el.currentTime=localStart},{once:true})
      el.src=response.playlist_url;el.load();starting.value=false;subtitleReady.value=true;refreshSubtitle();if(autoplayPending.value)void el.play().catch(()=>{});showControls()
    }else throw new Error('当前浏览器不支持 HLS 播放')
  }catch(caught){
    if(generation!==hlsGeneration)return
    resetHLS()
    starting.value=false;error.value=caught instanceof Error?caught.message:'兼容视频流启动失败';showControls(true)
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
  currentTime.value=(directMode.value?0:hlsOffset.value)+el.currentTime
  window.clearTimeout(saveTimer);saveTimer=window.setTimeout(()=>persistProgress(false),600)
  if(!remoteSaveTimer)remoteSaveTimer=window.setTimeout(()=>{remoteSaveTimer=0;persistProgress(true)},5000)
}
function onVideoError(){
  if(!directMode.value||directFallbackStarted||starting.value)return
  directFallbackStarted=true;void startCompatibilityStream(currentTime.value||savedPosition(),true)
}
function seekTo(target:number){
  const el=video.value
  if(!el||!Number.isFinite(target))return
  target=Math.max(0,Math.min(target,duration.value||target))
  if(directMode.value){el.currentTime=target;currentTime.value=target;return}
  if(starting.value){currentTime.value=target;void startCompatibilityStream(target,autoplayPending.value);return}
  const local=target-hlsOffset.value;const ranges=el.seekable
  let available=false
  for(let index=0;index<ranges.length;index+=1)if(local>=ranges.start(index)-.5&&local<=ranges.end(index)+.5){available=true;break}
  if(available){el.currentTime=Math.max(0,local);currentTime.value=target}
  else void startCompatibilityStream(target,playing.value)
}
function seek(event:Event){seekTo(Number((event.target as HTMLInputElement).value))}
function togglePlayback(){
  const el=video.value;if(!el)return
  if(starting.value){autoplayPending.value=!autoplayPending.value;showControls(true);return}
  if(el.paused)void el.play().catch(()=>{});else el.pause()
}
function onPlay(){playing.value=true;showControls()}
function onPause(){playing.value=false;window.clearTimeout(remoteSaveTimer);remoteSaveTimer=0;persistProgress(true);showControls(true)}
function changeVolume(event:Event){const value=Number((event.target as HTMLInputElement).value);volume.value=value;muted.value=value===0;if(video.value){video.value.volume=value;video.value.muted=muted.value}}
function toggleMute(){muted.value=!muted.value;if(video.value)video.value.muted=muted.value;showControls()}
async function toggleFullscreen(){
  if(!shell.value)return
  if(document.fullscreenElement)await document.exitFullscreen()
  else try{await shell.value.requestFullscreen({navigationUI:'hide'})}
  catch{
    const nativeVideo=video.value as (HTMLVideoElement&{webkitEnterFullscreen?:()=>void})|null
    nativeVideo?.webkitEnterFullscreen?.()
  }
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

onMounted(async()=>{
  document.addEventListener('fullscreenchange',onFullscreenChange)
  document.addEventListener('pointerdown',closeActionMenuFromOutside)
  const progressPromise=loadProgress()
  try{const media=await api<VideoMediaResponse>(`/api/files/${props.item.id}/video`);subtitles.value=media.subtitles||[];activeSubtitle.value=subtitles.value.length?0:-1;subtitleReady.value=directMode.value;refreshSubtitle()}catch{/* 视频仍可在没有字幕信息时播放 */}
  const el=video.value
  if(directMode.value){el?.load();void el?.play().catch(()=>{})}
  else{await progressPromise;void startCompatibilityStream(savedPosition(),true)}
})
onBeforeUnmount(()=>{
  document.removeEventListener('fullscreenchange',onFullscreenChange);document.removeEventListener('pointerdown',closeActionMenuFromOutside);window.clearTimeout(saveTimer);window.clearTimeout(remoteSaveTimer);window.clearTimeout(controlsTimer);persistProgress(false);hlsGeneration++
  if(currentTime.value>0)void fetch(`/api/files/${props.item.id}/media/progress`,{method:'PUT',headers:{'Content-Type':'application/json'},body:JSON.stringify({position:currentTime.value,duration:duration.value}),credentials:'same-origin',keepalive:true})
  const session=hlsSessionId;hlsSessionId='';resetHLS()
  if(session)void fetch(`/api/video/hls/${session}`,{method:'DELETE',credentials:'same-origin',keepalive:true})
})
</script>

<template>
  <div ref="shell" class="video-player-shell" tabindex="0" @mousemove="showControls()" @mouseleave="playing&&(controlsVisible=false)" @keydown="onKey">
    <video ref="video" :src="directSource||undefined" :poster="poster" autoplay playsinline preload="metadata" @click="togglePlayback" @dblclick="toggleFullscreen" @loadedmetadata="onLoadedMetadata" @timeupdate="onTimeUpdate" @play="onPlay" @pause="onPause" @ended="onPause" @error="onVideoError">
      <track v-if="subtitleReady&&selectedSubtitle" :key="`${selectedSubtitle.id}:${subtitleEpoch}`" kind="subtitles" :src="selectedSubtitle.url" :srclang="selectedSubtitle.language" :label="selectedSubtitle.label" default @load="onSubtitleLoad">
      你的浏览器不支持这个视频格式。
    </video>
    <div class="video-top-shade" :class="{visible:controlsVisible||!playing}"><div class="video-title-group"><button class="video-back" aria-label="退出播放" @click.stop="emit('close')"><svg viewBox="0 0 24 24"><path d="m15 5-7 7 7 7"/></svg></button><strong>{{ item.name }}</strong></div><span v-if="!directMode">{{ compatibilityLabel }}</span></div>
    <button v-if="!playing&&!starting&&!error" class="video-center-play" aria-label="播放" @click.stop="togglePlayback"><svg viewBox="0 0 24 24"><path d="m9 7 9 5-9 5Z"/></svg></button>
    <div v-if="starting" class="video-loading"><span></span><strong>正在准备兼容视频流</strong><small>首次播放 MKV / HEVC 时需要先生成几个分片</small></div>
    <p v-if="error" class="video-error">{{ error }} <button @click="startCompatibilityStream(currentTime||savedPosition(),true)">重试</button></p>
    <div class="video-controls" :class="{visible:controlsVisible||!playing}" @click.stop>
      <input class="video-seek" type="range" min="0" :max="Math.max(duration,1)" step=".25" :value="Math.min(currentTime,Math.max(duration,1))" :style="{'--video-progress':`${progress}%`}" :disabled="!duration" aria-label="视频进度" @change="seek">
      <div class="video-control-row">
        <button class="video-icon-button" :aria-label="playing||(starting&&autoplayPending)?'暂停':'播放'" @click="togglePlayback"><svg v-if="playing||(starting&&autoplayPending)" viewBox="0 0 24 24"><path d="M8 6v12M16 6v12"/></svg><svg v-else viewBox="0 0 24 24"><path d="m9 7 9 5-9 5Z"/></svg></button>
        <button class="video-icon-button" :aria-label="muted?'取消静音':'静音'" @click="toggleMute"><svg viewBox="0 0 24 24"><path d="M5 10h3l4-3v10l-4-3H5Z"/><path v-if="!muted" d="M15 9c1.5 1.5 1.5 4.5 0 6M18 7c3 3 3 7 0 10"/><path v-else d="m16 10 4 4m0-4-4 4"/></svg></button>
        <input class="video-volume" type="range" min="0" max="1" step=".05" :value="muted?0:volume" aria-label="音量" @input="changeVolume">
        <span class="video-time">{{ formatTime(currentTime) }} / {{ formatTime(duration) }}</span>
        <span class="video-control-spacer"></span>
        <label class="video-subtitles" :class="{disabled:!subtitles.length}"><span>CC</span><select :value="activeSubtitle" :disabled="!subtitles.length" aria-label="字幕" @change="chooseSubtitle"><option value="-1">关闭字幕</option><option v-for="(track,index) in subtitles" :key="track.id" :value="index">{{ track.label }}</option></select></label>
        <details ref="actionMenu" class="video-action-menu"><summary class="video-icon-button" aria-label="更多操作"><svg viewBox="0 0 24 24"><path d="M5 7h14M5 12h14M5 17h14"/></svg></summary><div><button @click="actionMenu?.removeAttribute('open');emit('download',item)">下载</button><button @click="actionMenu?.removeAttribute('open');emit('move',item)">移动</button><button @click="actionMenu?.removeAttribute('open');emit('copy',item)">复制</button></div></details>
        <button class="video-icon-button" :aria-label="fullscreen?'退出全屏':'全屏'" @click="toggleFullscreen"><svg viewBox="0 0 24 24"><path v-if="!fullscreen" d="M8 4H4v4M16 4h4v4M8 20H4v-4M16 20h4v-4"/><path v-else d="M4 8h4V4M20 8h-4V4M4 16h4v4M20 16h-4v4"/></svg></button>
      </div>
    </div>
  </div>
</template>

<style scoped>
.video-player-shell{position:relative;isolation:isolate;justify-self:center;width:min(1500px,100%);max-width:100%;height:min(820px,calc(100dvh - 178px));min-height:280px;overflow:hidden;border-radius:15px;background:#000;box-shadow:0 18px 46px #39517226;outline:none}.video-player-shell:fullscreen{width:100vw;height:100vh;border-radius:0}.video-player-shell video{display:block;width:100%;height:100%;max-width:none;max-height:none;border-radius:0;background:#000;object-fit:contain;box-shadow:none;cursor:pointer}.video-top-shade{position:absolute;z-index:9;inset:0 0 auto;display:flex;align-items:flex-start;justify-content:space-between;gap:18px;padding:18px 22px 60px;background:linear-gradient(#000c,transparent);color:#fff;opacity:0;pointer-events:none;transition:opacity .2s}.video-top-shade.visible{opacity:1}.video-title-group{display:flex;align-items:center;min-width:0;gap:8px}.video-top-shade strong{overflow:hidden;font-size:17px;text-overflow:ellipsis;white-space:nowrap;text-shadow:0 1px 3px #000}.video-top-shade>span{flex:0 0 auto;padding:6px 9px;border:1px solid #ffffff3d;border-radius:999px;background:#0005;color:#d9e7f6;font-size:10px}.video-back{display:grid;place-items:center;flex:0 0 auto;width:42px;height:42px;padding:0;border:0;border-radius:50%;background:#0b1220a8;color:#fff;pointer-events:auto}.video-back:hover{background:#ffffff24}.video-back svg{width:27px;height:27px;fill:none;stroke:currentColor;stroke-width:2.2;stroke-linecap:round;stroke-linejoin:round}.video-center-play{position:absolute;z-index:6;top:50%;left:50%;display:grid;place-items:center;width:76px;height:54px;padding:0;border:0;border-radius:15px;background:#ff0033e8;color:#fff;box-shadow:0 8px 28px #0007;transform:translate(-50%,-50%);transition:transform .16s,background .16s}.video-center-play:hover{background:#ff0033;transform:translate(-50%,-50%) scale(1.06)}.video-center-play svg{width:33px;height:33px;fill:currentColor;stroke:none}.video-controls{position:absolute;z-index:8;right:0;bottom:0;left:0;padding:62px 20px 12px;background:linear-gradient(transparent,#000e);color:#fff;opacity:0;pointer-events:none;transform:translateY(8px);transition:opacity .2s,transform .2s}.video-controls.visible{opacity:1;pointer-events:auto;transform:none}.video-seek{display:block;width:100%;height:5px;margin:0;appearance:none;border-radius:999px;background:linear-gradient(to right,#f03 var(--video-progress),#ffffff55 var(--video-progress));cursor:pointer;transition:height .12s}.video-seek:hover{height:7px}.video-seek::-webkit-slider-thumb{width:15px;height:15px;appearance:none;border:0;border-radius:50%;background:#f03}.video-seek::-moz-range-thumb{width:15px;height:15px;border:0;border-radius:50%;background:#f03}.video-control-row{display:flex;align-items:center;gap:7px;min-height:50px}.video-icon-button{display:grid;place-items:center;width:46px;height:46px;padding:0;border:0;border-radius:50%;background:transparent;color:#fff}.video-icon-button:hover{background:#ffffff1f}.video-icon-button svg{width:28px;height:28px;fill:none;stroke:currentColor;stroke-width:2;stroke-linecap:round;stroke-linejoin:round}.video-icon-button svg path[d*="9 7"]{fill:currentColor;stroke:none}.video-volume{width:0;height:5px;margin:0;appearance:none;border-radius:999px;background:#ffffff7a;accent-color:#fff;opacity:0;transition:width .18s,opacity .18s}.video-icon-button:hover+.video-volume,.video-volume:hover,.video-volume:focus{width:94px;opacity:1}.video-volume::-webkit-slider-thumb{width:13px;height:13px;appearance:none;border-radius:50%;background:#fff}.video-time{margin-left:5px;font:12px ui-monospace,SFMono-Regular,Consolas,monospace;white-space:nowrap;text-shadow:0 1px 2px #000}.video-control-spacer{flex:1}.video-subtitles{position:relative;display:flex;align-items:center}.video-subtitles>span{display:grid;place-items:center;width:38px;height:31px;border:2px solid currentColor;border-radius:6px;font-size:10px;font-weight:850;letter-spacing:.04em}.video-subtitles select{position:absolute;right:0;bottom:40px;width:min(300px,70vw);padding:11px;border:1px solid #ffffff30;border-radius:9px;background:#171717ec;color:#fff;font-size:12px;opacity:0;pointer-events:none;transform:translateY(6px);transition:.15s}.video-subtitles:hover select,.video-subtitles select:focus{opacity:1;pointer-events:auto;transform:none}.video-subtitles.disabled{opacity:.45}.video-loading{position:absolute;z-index:7;inset:0;display:grid;place-content:center;justify-items:center;padding:24px;background:#0009;color:#fff;text-align:center;pointer-events:none}.video-loading span{width:42px;height:42px;border:3px solid #ffffff30;border-top-color:#fff;border-radius:50%;animation:video-spin .8s linear infinite}.video-loading strong{margin-top:14px;font-size:15px}.video-loading small{max-width:420px;margin-top:7px;color:#c8d2df;font-size:11px;line-height:1.6}.video-error{position:absolute;z-index:10;left:50%;bottom:86px;max-width:calc(100% - 30px);margin:0;padding:9px 12px;border:1px solid #fecaca;border-radius:10px;background:#fff1f2ed;color:#be123c;font-size:11px;transform:translateX(-50%)}.video-error button{margin-left:8px;border:0;background:transparent;color:#9f1239;font-weight:800}@keyframes video-spin{to{transform:rotate(360deg)}}
.video-action-menu{position:relative}.video-action-menu summary{list-style:none}.video-action-menu summary::-webkit-details-marker{display:none}.video-action-menu>div{position:absolute;right:0;bottom:45px;display:grid;width:130px;overflow:hidden;border:1px solid #ffffff24;border-radius:11px;background:#171717f2;box-shadow:0 12px 32px #0007}.video-action-menu>div button{padding:10px 14px;border:0;background:transparent;color:#fff;text-align:left}.video-action-menu>div button:hover{background:#ffffff14}
@media(max-width:850px){.video-player-shell{width:100%;height:100%;min-height:0;border-radius:10px}.video-top-shade{padding:10px 12px 48px}.video-title-group{gap:4px}.video-back{width:39px;height:39px}.video-back svg{width:24px;height:24px}.video-top-shade strong{font-size:13px}.video-top-shade>span{display:none}.video-controls{padding:46px 9px max(6px,env(safe-area-inset-bottom,0px))}.video-control-row{gap:2px;min-height:43px}.video-icon-button{width:40px;height:40px}.video-icon-button svg{width:23px;height:23px}.video-volume{display:none}.video-time{font-size:10px}.video-center-play{width:64px;height:47px}.video-subtitles select{inset:0;width:100%;height:100%;padding:0;opacity:0;pointer-events:auto;transform:none}.video-error{bottom:66px}}
</style>

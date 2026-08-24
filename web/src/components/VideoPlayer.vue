<script setup lang="ts">
import { computed, nextTick, onBeforeUnmount, onMounted, ref } from 'vue'
import type HlsInstance from 'hls.js/light'
import type { DriveFile } from '../api'
import { api } from '../api'
import { previewURL } from '../fileTypes'
import type { VideoHLSResponse, VideoMediaResponse, VideoSubtitleTrack } from '../types'

const props=defineProps<{item:DriveFile}>()
const video=ref<HTMLVideoElement|null>(null)
const subtitles=ref<VideoSubtitleTrack[]>([])
const activeSubtitle=ref(-1)
const directMode=ref(/\.(mp4|m4v|webm|ogv|mov)$/i.test(props.item.name))
const directSource=ref(directMode.value?previewURL(props.item):'')
const loading=ref(true)
const starting=ref(false)
const playing=ref(false)
const error=ref('')
const currentTime=ref(0)
const duration=ref(0)
const hlsOffset=ref(0)
const videoCodec=ref('')
const audioCodec=ref('')
const transcoding=ref(false)
let hls:HlsInstance|null=null
let hlsSessionId=''
let hlsGeneration=0
let saveTimer=0
let directFallbackStarted=false

const positionKey=computed(()=>`revaro-video-position:${props.item.id}`)
const compatibilityLabel=computed(()=>transcoding.value?`${videoCodec.value.toUpperCase()} → H.264 实时转码`:`${videoCodec.value.toUpperCase()||'视频'} HLS 封装`)

function formatTime(seconds:number){
  if(!Number.isFinite(seconds)||seconds<0)return '0:00'
  const value=Math.floor(seconds);const hours=Math.floor(value/3600);const minutes=Math.floor(value%3600/60);const secs=value%60
  return hours?`${hours}:${String(minutes).padStart(2,'0')}:${String(secs).padStart(2,'0')}`:`${minutes}:${String(secs).padStart(2,'0')}`
}
function savedPosition(){const value=Number(localStorage.getItem(positionKey.value)||0);return Number.isFinite(value)&&value>0?value:0}
function applySubtitle(){
  const tracks=video.value?.textTracks
  if(!tracks)return
  for(let index=0;index<tracks.length;index+=1)tracks[index].mode=index===activeSubtitle.value?'showing':'disabled'
}
function chooseSubtitle(event:Event){activeSubtitle.value=Number((event.target as HTMLSelectElement).value);applySubtitle()}
async function removeHLSSession(id:string){if(id)try{await api(`/api/video/hls/${id}`,{method:'DELETE'})}catch{/* 服务端闲置清理兜底 */}}
function resetHLS(){hls?.destroy();hls=null;if(video.value){video.value.pause();video.value.removeAttribute('src');video.value.load()}}

async function startCompatibilityStream(start:number,autoplay=true){
  const generation=++hlsGeneration
  const previous=hlsSessionId;hlsSessionId=''
  starting.value=true;loading.value=true;error.value='';directMode.value=false;directSource.value=''
  resetHLS();await removeHLSSession(previous)
  try{
    const response=await api<VideoHLSResponse>(`/api/files/${props.item.id}/video/hls`,{method:'POST',body:JSON.stringify({start})},75000)
    if(generation!==hlsGeneration){void removeHLSSession(response.session_id);return}
    const el=video.value
    if(!el)throw new Error('播放器已经关闭')
    hlsSessionId=response.session_id;hlsOffset.value=response.start;currentTime.value=response.start;duration.value=response.duration
    videoCodec.value=response.video_codec;audioCodec.value=response.audio_codec;transcoding.value=response.transcoding
    const {default:Hls}=await import('hls.js/light')
    if(Hls.isSupported()){
      const player=new Hls({enableWorker:true,lowLatencyMode:false,backBufferLength:90})
      hls=player
      player.on(Hls.Events.MEDIA_ATTACHED,()=>player.loadSource(response.playlist_url))
      player.on(Hls.Events.MANIFEST_PARSED,()=>{starting.value=false;loading.value=false;applySubtitle();if(autoplay)void el.play().catch(()=>{})})
      player.on(Hls.Events.ERROR,(_event,data)=>{if(data.fatal){starting.value=false;loading.value=false;error.value='兼容视频流播放失败，请重试'}})
      player.attachMedia(el)
    }else if(el.canPlayType('application/vnd.apple.mpegurl')){
      el.src=response.playlist_url;el.load();starting.value=false;loading.value=false;applySubtitle();if(autoplay)void el.play().catch(()=>{})
    }else throw new Error('当前浏览器不支持 HLS 播放')
  }catch(caught){
    if(generation!==hlsGeneration)return
    const failed=hlsSessionId;hlsSessionId='';resetHLS();if(failed)void removeHLSSession(failed)
    starting.value=false;loading.value=false;error.value=caught instanceof Error?caught.message:'兼容视频流启动失败'
  }
}
function onLoadedMetadata(){
  const el=video.value;if(!el)return
  loading.value=false
  if(directMode.value){duration.value=Number.isFinite(el.duration)?el.duration:0;const saved=savedPosition();if(saved>0&&saved<duration.value-5)el.currentTime=saved}
  applySubtitle()
}
function onTimeUpdate(){
  const el=video.value;if(!el)return
  currentTime.value=(directMode.value?0:hlsOffset.value)+el.currentTime
  window.clearTimeout(saveTimer);saveTimer=window.setTimeout(()=>localStorage.setItem(positionKey.value,String(Math.floor(currentTime.value))),600)
}
function onVideoError(){
  if(!directMode.value||directFallbackStarted||starting.value)return
  directFallbackStarted=true;void startCompatibilityStream(currentTime.value||savedPosition(),true)
}
function seek(event:Event){
  const target=Number((event.target as HTMLInputElement).value);const el=video.value
  if(!el||!Number.isFinite(target))return
  if(directMode.value){el.currentTime=target;return}
  const local=target-hlsOffset.value;const ranges=el.seekable
  let available=false
  for(let index=0;index<ranges.length;index+=1)if(local>=ranges.start(index)-.5&&local<=ranges.end(index)+.5){available=true;break}
  if(available){el.currentTime=Math.max(0,local);currentTime.value=target}
  else void startCompatibilityStream(target,playing.value)
}

onMounted(async()=>{
  try{const media=await api<VideoMediaResponse>(`/api/files/${props.item.id}/video`);subtitles.value=media.subtitles||[];activeSubtitle.value=subtitles.value.length?0:-1;await nextTick();applySubtitle()}catch{/* 视频仍可在没有字幕信息时播放 */}
  const el=video.value
  if(directMode.value){el?.load();void el?.play().catch(()=>{})}
  else void startCompatibilityStream(savedPosition(),true)
})
onBeforeUnmount(()=>{
  window.clearTimeout(saveTimer);hlsGeneration++
  const session=hlsSessionId;hlsSessionId='';resetHLS()
  if(session)void fetch(`/api/video/hls/${session}`,{method:'DELETE',credentials:'same-origin',keepalive:true})
})
</script>

<template>
  <div class="video-player-shell">
    <div class="video-screen">
      <video ref="video" :src="directSource||undefined" controls autoplay playsinline preload="metadata" @loadedmetadata="onLoadedMetadata" @timeupdate="onTimeUpdate" @play="playing=true" @pause="playing=false" @error="onVideoError">
        <track v-for="(track,index) in subtitles" :key="track.id" kind="subtitles" :src="track.url" :srclang="track.language" :label="track.label" :default="index===activeSubtitle">
        你的浏览器不支持这个视频格式。
      </video>
      <div v-if="starting" class="video-loading"><span></span><strong>正在准备兼容视频流</strong><small>MKV / HEVC 首次启动需要等待 FFmpeg 生成首个分片</small></div>
      <p v-if="error" class="video-error">{{ error }} <button @click="startCompatibilityStream(currentTime||savedPosition(),true)">重试</button></p>
    </div>
    <div class="video-tools">
      <span class="video-time">{{ formatTime(currentTime) }} / {{ formatTime(duration) }}</span>
      <input class="video-full-seek" type="range" min="0" :max="Math.max(duration,1)" step="1" :value="Math.min(currentTime,Math.max(duration,1))" :disabled="!duration||starting" aria-label="视频进度" @change="seek">
      <span v-if="!directMode" class="compatibility-badge" :title="audioCodec?`音频：${audioCodec}`:''">{{ compatibilityLabel }}</span>
      <label class="subtitle-picker"><span>CC</span><select :value="activeSubtitle" :disabled="!subtitles.length" @change="chooseSubtitle"><option value="-1">关闭字幕</option><option v-for="(track,index) in subtitles" :key="track.id" :value="index">{{ track.label }}</option></select></label>
    </div>
  </div>
</template>

<style scoped>
.video-player-shell{display:grid;grid-template-rows:minmax(0,1fr) auto;justify-self:center;width:min(1500px,100%);max-width:100%;height:min(820px,calc(100dvh - 178px));min-height:280px;overflow:hidden;border:1px solid #dfe5ed;border-radius:16px;background:#05080e;box-shadow:0 18px 46px #39517226}.video-screen{position:relative;display:grid;place-items:center;min-width:0;min-height:0;overflow:hidden}.video-screen video{width:100%;height:100%;max-width:100%;max-height:100%;border-radius:0;background:#05080e;object-fit:contain;box-shadow:none}.video-loading{position:absolute;inset:0;display:grid;place-content:center;justify-items:center;padding:24px;background:#07101cdd;color:#e7eef8;text-align:center}.video-loading span{width:34px;height:34px;border:3px solid #ffffff26;border-top-color:#7dd3fc;border-radius:50%;animation:video-spin .8s linear infinite}.video-loading strong{margin-top:14px;font-size:14px}.video-loading small{max-width:420px;margin-top:7px;color:#9fb0c5;font-size:10px;line-height:1.6}.video-error{position:absolute;left:50%;bottom:18px;max-width:calc(100% - 30px);margin:0;padding:9px 12px;border:1px solid #fecaca;border-radius:10px;background:#fff1f2eb;color:#be123c;font-size:11px;transform:translateX(-50%)}.video-error button{margin-left:8px;border:0;background:transparent;color:#9f1239;font-weight:800}.video-tools{display:grid;grid-template-columns:auto minmax(100px,1fr) auto auto;align-items:center;gap:12px;min-height:54px;padding:8px 14px;border-top:1px solid #263244;background:#101827;color:#cbd5e1}.video-time{font:10px ui-monospace,SFMono-Regular,Consolas,monospace;white-space:nowrap}.video-full-seek{width:100%;accent-color:#38bdf8}.compatibility-badge{max-width:230px;overflow:hidden;padding:5px 8px;border:1px solid #334155;border-radius:999px;background:#1e293b;color:#a5c8f4;font-size:9px;text-overflow:ellipsis;white-space:nowrap}.subtitle-picker{display:flex;align-items:center;gap:6px}.subtitle-picker>span{display:grid;place-items:center;width:25px;height:19px;border:1px solid #64748b;border-radius:5px;color:#dce8f7;font-size:9px;font-weight:850}.subtitle-picker select{max-width:220px;padding:6px 8px;border:1px solid #334155;border-radius:8px;background:#172033;color:#e2e8f0;font-size:10px;outline:none}.subtitle-picker select:disabled{opacity:.45}@keyframes video-spin{to{transform:rotate(360deg)}}
@media(max-width:850px){.video-player-shell{height:100%;min-height:0;border-radius:10px}.video-tools{grid-template-columns:auto minmax(80px,1fr) auto;gap:7px;padding:7px 9px}.compatibility-badge{display:none}.subtitle-picker select{width:94px;max-width:94px}.video-time{font-size:8px}}
</style>

<script setup lang="ts">
import { computed, nextTick, onBeforeUnmount, onMounted, ref } from 'vue'
import type HlsInstance from 'hls.js/light'
import type { DriveFile } from '../api'
import { api } from '../api'
import { previewURL } from '../fileTypes'
import { formatSize } from '../format'
import type { AudioChapter, AudioHLSResponse, AudioMediaResponse } from '../types'

const props=defineProps<{item:DriveFile}>()
const audio=ref<HTMLAudioElement|null>(null)
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
let saveTimer=0
let restoredPosition=false
let hls:HlsInstance|null=null
let hlsSessionId=''
let hlsGeneration=0

const source=computed(()=>media.value?.stream_url||previewURL(props.item))
const duration=computed(()=>media.value?.duration||nativeDuration.value||0)
const chapters=computed<AudioChapter[]>(()=>media.value?.chapters?.length?media.value.chapters:[{id:1,title:props.item.name.replace(/\.[^.]+$/,''),start:0,end:duration.value}])
const currentChapterIndex=computed(()=>{
  const index=chapters.value.findIndex((chapter,index)=>currentTime.value>=chapter.start&&(currentTime.value<chapter.end||index===chapters.value.length-1))
  return Math.max(0,index)
})
const currentChapter=computed(()=>chapters.value[currentChapterIndex.value])
const progress=computed(()=>duration.value?Math.min(100,currentTime.value/duration.value*100):0)
const positionKey=computed(()=>`revaro-audio-position:${props.item.id}`)

function formatTime(seconds:number){
  if(!Number.isFinite(seconds)||seconds<0)return '0:00'
  const value=Math.floor(seconds);const hours=Math.floor(value/3600);const minutes=Math.floor(value%3600/60);const secs=value%60
  return hours?`${hours}:${String(minutes).padStart(2,'0')}:${String(secs).padStart(2,'0')}`:`${minutes}:${String(secs).padStart(2,'0')}`
}
async function togglePlayback(){
  if(!audio.value||compatibilityStarting.value)return
  if(audio.value.paused){try{await audio.value.play()}catch{if(!compatibilityStarting.value)error.value='浏览器无法开始播放，请重试'}}else audio.value.pause()
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
function seekFromSlider(event:Event){seek(Number((event.target as HTMLInputElement).value),playing.value)}
function seekRelative(delta:number){seek(currentTime.value+delta)}
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
  if(restoredPosition)return
  restoredPosition=true
  const saved=Number(localStorage.getItem(positionKey.value)||0)
  if(saved>0&&saved<duration.value-5)seek(saved)
}
function onTimeUpdate(){
  if(!audio.value)return
  currentTime.value=(compatibilityMode.value?hlsOffset.value:0)+audio.value.currentTime;updateBuffer()
  window.clearTimeout(saveTimer);saveTimer=window.setTimeout(()=>localStorage.setItem(positionKey.value,String(Math.floor(currentTime.value))),500)
}
function setRate(event:Event){rate.value=Number((event.target as HTMLSelectElement).value);if(audio.value)audio.value.playbackRate=rate.value}

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
        el.playbackRate=rate.value
        if(autoplay)void el.play().catch(()=>{})
      })
      player.on(Hls.Events.ERROR,(_event,data)=>{
        if(!data.fatal)return
        compatibilityStarting.value=false;loading.value=false;waiting.value=false
        error.value='FFmpeg HLS 兼容流播放失败，请重试'
      })
      player.attachMedia(el)
    }else if(el.canPlayType('application/vnd.apple.mpegurl')){
      el.src=response.playlist_url;el.load()
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
function onAudioError(){
  if(compatibilityMode.value||compatibilityStarting.value)return
  const saved=Number(localStorage.getItem(positionKey.value)||0)
  const start=currentTime.value>0?currentTime.value:(saved>0&&saved<duration.value-5?saved:0)
  void startCompatibilityStream(start,playing.value)
}

onMounted(async()=>{
  try{media.value=await api<AudioMediaResponse>(`/api/files/${props.item.id}/audio`)}catch{/* 普通音频继续走原始 Range 预览 */}
  await nextTick();audio.value?.load()
})
onBeforeUnmount(()=>{
  window.clearTimeout(saveTimer);hlsGeneration++
  const session=hlsSessionId;hlsSessionId='';resetLocalHLS()
  if(session)void fetch(`/api/audio/hls/${session}`,{method:'DELETE',credentials:'same-origin',keepalive:true})
})
</script>

<template>
  <div class="chapter-audio-player">
    <section class="audio-now-playing">
      <div class="audio-cover">
        <img v-if="media?.has_cover" :src="media.cover_url" :alt="`${item.name} 封面`">
        <span v-else>♫</span>
      </div>
      <div class="audio-title"><strong>{{ item.name }}</strong><small>{{ formatSize(item.size) }} · {{ compatibilityMode?'FFmpeg HLS 兼容流':'原文件 Range 流式播放' }}</small></div>
      <div class="audio-chapter-current"><span>正在播放</span><strong>{{ currentChapter?.title||item.name }}</strong><small>第 {{ currentChapterIndex+1 }} / {{ chapters.length }} 节</small></div>
      <div class="audio-track-wrap">
        <div class="audio-track-buffer" :style="{width:`${buffered}%`}"></div>
        <div class="audio-track-played" :style="{width:`${progress}%`}"></div>
        <i v-for="chapter in chapters.slice(1)" :key="chapter.id" :style="{left:`${duration?chapter.start/duration*100:0}%`}"></i>
        <input :value="currentTime" type="range" min="0" :max="duration||0" step="0.1" aria-label="播放进度" @change="seekFromSlider">
      </div>
      <div class="audio-time"><span>{{ formatTime(currentTime) }}</span><span>-{{ formatTime(Math.max(0,duration-currentTime)) }}</span></div>
      <div class="audio-controls">
        <button title="上一节" aria-label="上一节" @click="previousChapter">│◀</button>
        <button title="后退 15 秒" aria-label="后退 15 秒" @click="seekRelative(-15)">↶<small>15</small></button>
        <button class="audio-play" :disabled="loading||compatibilityStarting" :title="playing?'暂停':'播放'" :aria-label="playing?'暂停':'播放'" @click="togglePlayback">{{ waiting||compatibilityStarting?'…':playing?'Ⅱ':'▶' }}</button>
        <button title="前进 30 秒" aria-label="前进 30 秒" @click="seekRelative(30)">↷<small>30</small></button>
        <button title="下一节" aria-label="下一节" :disabled="currentChapterIndex>=chapters.length-1" @click="nextChapter">▶│</button>
      </div>
      <div class="audio-player-options"><label>倍速<select :value="rate" @change="setRate"><option value="0.75">0.75×</option><option value="1">1.0×</option><option value="1.25">1.25×</option><option value="1.5">1.5×</option><option value="2">2.0×</option></select></label><span v-if="compatibilityStarting">浏览器无法直放，正在启动兼容流…</span><span v-else-if="waiting">正在缓冲需要的片段…</span><span v-else>无需完整下载即可播放和跳转</span></div>
      <p v-if="error" class="audio-player-error">{{ error }}</p>
      <audio ref="audio" :src="compatibilityMode?undefined:source" preload="metadata" @loadedmetadata="onLoadedMetadata" @timeupdate="onTimeUpdate" @progress="updateBuffer" @play="playing=true" @pause="playing=false" @waiting="waiting=true" @canplay="waiting=false" @error="onAudioError"></audio>
    </section>
    <aside class="audio-chapters">
      <header><div><strong>分节</strong><small>保留合并前的文件名</small></div><span>{{ chapters.length }} 节</span></header>
      <div class="audio-chapter-list">
        <button v-for="(chapter,index) in chapters" :key="chapter.id" :class="{active:index===currentChapterIndex}" @click="seek(chapter.start,true)">
          <b>{{ index+1 }}</b><span><strong :title="chapter.title">{{ chapter.title }}</strong><small>{{ formatTime(chapter.start) }} · {{ formatTime(Math.max(0,chapter.end-chapter.start)) }}</small></span><i>{{ index===currentChapterIndex&&playing?'▮▮':'▶' }}</i>
        </button>
      </div>
    </aside>
  </div>
</template>

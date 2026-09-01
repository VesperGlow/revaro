<script setup lang="ts">
import { onBeforeUnmount, onMounted, ref } from 'vue'
import type { VideoSubtitleTrack } from '../types'

defineProps<{visible:boolean;playing:boolean;starting:boolean;autoplayPending:boolean;duration:number;timelinePosition:number;progress:number;volumeState:'muted'|'low'|'high';volumeFeedback:boolean;volumePercent:number;effectiveVolume:number;subtitles:VideoSubtitleTrack[];activeSubtitle:number;fullscreen:boolean;formatTime:(seconds:number)=>string}>()
const emit=defineEmits<{togglePlayback:[];previewSeek:[event:Event];commitSeek:[event:Event];toggleMute:[];volumeStart:[];volumeEnd:[];changeVolume:[event:Event];chooseSubtitle:[event:Event];download:[];move:[];copy:[];toggleFullscreen:[]}>()
const actionMenu=ref<HTMLDetailsElement|null>(null)
function closeActionMenuFromOutside(event:PointerEvent){const target=event.target;if(actionMenu.value?.open&&target instanceof Node&&!actionMenu.value.contains(target))actionMenu.value.open=false}
function action(name:'download'|'move'|'copy'){actionMenu.value?.removeAttribute('open');if(name==='download')emit('download');else if(name==='move')emit('move');else emit('copy')}
onMounted(()=>document.addEventListener('pointerdown',closeActionMenuFromOutside))
onBeforeUnmount(()=>document.removeEventListener('pointerdown',closeActionMenuFromOutside))
</script>

<template>
  <div class="video-controls" :class="{visible}" @click.stop>
    <input class="video-seek" type="range" min="0" :max="Math.max(duration,1)" step=".25" :value="Math.min(timelinePosition,Math.max(duration,1))" :style="{'--video-progress':`${progress}%`}" :disabled="!duration" aria-label="视频进度" @input="$emit('previewSeek',$event)" @change="$emit('commitSeek',$event)">
    <div class="video-control-row">
      <button class="video-icon-button" :aria-label="playing||(starting&&autoplayPending)?'暂停':'播放'" @click="$emit('togglePlayback')"><svg v-if="playing||(starting&&autoplayPending)" viewBox="0 0 24 24"><path d="M8 6v12M16 6v12"/></svg><svg v-else viewBox="0 0 24 24"><path d="m9 7 9 5-9 5Z"/></svg></button>
      <button class="video-icon-button video-mute" :class="volumeState" :aria-label="volumeState==='muted'?'取消静音':'静音'" @click="$emit('toggleMute')"><svg viewBox="0 0 24 24"><path d="M5 10h3l4-3v10l-4-3H5Z"/><path v-if="volumeState==='low'" d="M15 9c1.5 1.5 1.5 4.5 0 6"/><template v-else-if="volumeState==='high'"><path d="M15 9c1.5 1.5 1.5 4.5 0 6"/><path d="M18 7c3 3 3 7 0 10"/></template><path v-else d="m16 10 4 4m0-4-4 4"/></svg></button>
      <label class="video-volume-control" :class="{active:volumeFeedback,muted:volumeState==='muted'}" :style="{'--volume-progress':`${volumePercent}%`}"><input class="video-volume" type="range" min="0" max="1" step=".01" :value="effectiveVolume" aria-label="音量" :aria-valuetext="`${volumePercent}%`" @pointerdown="$emit('volumeStart')" @pointerup="$emit('volumeEnd')" @input="$emit('changeVolume',$event)"><output>{{ volumePercent }}%</output></label>
      <span class="video-time">{{ formatTime(timelinePosition) }} / {{ formatTime(duration) }}</span><span class="video-control-spacer"></span>
      <label class="video-subtitles" :class="{disabled:!subtitles.length}"><span>CC</span><select :value="activeSubtitle" :disabled="!subtitles.length" aria-label="字幕" @change="$emit('chooseSubtitle',$event)"><option value="-1">关闭字幕</option><option v-for="(track,index) in subtitles" :key="track.id" :value="index">{{ track.label }}</option></select></label>
      <details ref="actionMenu" class="video-action-menu"><summary class="video-icon-button" aria-label="更多操作"><svg viewBox="0 0 24 24"><path d="M5 7h14M5 12h14M5 17h14"/></svg></summary><div><button @click="action('download')">下载</button><button @click="action('move')">移动</button><button @click="action('copy')">复制</button></div></details>
      <button class="video-icon-button" :aria-label="fullscreen?'退出全屏':'全屏'" @click="$emit('toggleFullscreen')"><svg viewBox="0 0 24 24"><path v-if="!fullscreen" d="M8 4H4v4M16 4h4v4M8 20H4v-4M16 20h4v-4"/><path v-else d="M4 8h4V4M20 8h-4V4M4 16h4v4M20 16h-4v4"/></svg></button>
    </div>
  </div>
</template>

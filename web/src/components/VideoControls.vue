<script setup lang="ts">
import { Play, Pause, Volume2, Volume1, VolumeX, Captions, Settings2, Maximize, Minimize, Download, Move, Copy } from '@lucide/vue'
import type { VideoSubtitleTrack } from '../types'
import PreviewMenu from './PreviewMenu.vue'

defineProps<{visible:boolean;playing:boolean;starting:boolean;autoplayPending:boolean;duration:number;timelinePosition:number;progress:number;volumeState:'muted'|'low'|'high';volumeFeedback:boolean;volumePercent:number;effectiveVolume:number;subtitles:VideoSubtitleTrack[];activeSubtitle:number;fullscreen:boolean;rate:number;playbackInfo:string;formatTime:(seconds:number)=>string}>()
const emit=defineEmits<{togglePlayback:[];previewSeek:[event:Event];commitSeek:[event:Event];cancelSeek:[];toggleMute:[];volumeStart:[];volumeEnd:[];changeVolume:[event:Event];chooseSubtitle:[event:Event];changeRate:[event:Event];download:[];move:[];copy:[];toggleFullscreen:[];hover:[value:boolean];interact:[]}>()
</script>

<template>
  <div class="video-controls" :class="{visible}" :inert="!visible" @click.stop @pointerenter="$event.pointerType==='mouse'&&emit('hover',true)" @pointerleave="emit('hover',false)" @focusin="emit('interact')" @keydown="emit('interact')">
    <input class="video-seek" type="range" min="0" :max="Math.max(duration,1)" step=".25" :value="Math.min(timelinePosition,Math.max(duration,1))" :style="{'--video-progress':`${progress}%`}" :disabled="!duration" aria-label="视频进度" :aria-valuetext="formatTime(timelinePosition)" @input="emit('previewSeek',$event)" @change="emit('commitSeek',$event)" @pointercancel="emit('cancelSeek')">
    <div class="video-control-row">
      <button class="video-icon-button" :aria-label="playing||(starting&&autoplayPending)?'暂停':'播放'" @click="emit('togglePlayback')"><Pause v-if="playing||(starting&&autoplayPending)" aria-hidden="true" /><Play v-else aria-hidden="true" /></button>
      <span class="video-time">{{ formatTime(timelinePosition) }} <span>/ {{ formatTime(duration) }}</span></span>
      <div class="video-desktop-volume">
        <button class="video-icon-button" :aria-label="volumeState==='muted'?'取消静音':'静音'" @click="emit('toggleMute')"><VolumeX v-if="volumeState==='muted'" aria-hidden="true" /><Volume1 v-else-if="volumeState==='low'" aria-hidden="true" /><Volume2 v-else aria-hidden="true" /></button>
        <input class="video-volume" type="range" min="0" max="1" step=".01" :value="effectiveVolume" aria-label="音量" :aria-valuetext="`${volumePercent}%`" @pointerdown="emit('volumeStart')" @pointerup="emit('volumeEnd')" @input="emit('changeVolume',$event)">
      </div>
      <span class="video-control-spacer"></span>
      <PreviewMenu v-if="subtitles.length" label="字幕" @change="emit('interact')">
        <template #trigger><Captions aria-hidden="true" /></template>
        <label class="video-setting"><span>字幕</span><select :value="activeSubtitle" aria-label="字幕轨道" @change="emit('chooseSubtitle',$event)"><option value="-1">关闭字幕</option><option v-for="(track,index) in subtitles" :key="track.id" :value="index">{{ track.label }}</option></select></label>
      </PreviewMenu>
      <PreviewMenu label="播放设置" @change="emit('interact')">
        <template #trigger><Settings2 aria-hidden="true" /></template><template #default="{close}">
          <label class="video-setting"><span>播放速度</span><select :value="rate" aria-label="播放速度" @change="emit('changeRate',$event)"><option v-for="speed in [0.5,0.75,1,1.25,1.5,2]" :key="speed" :value="speed">{{ speed }}×</option></select></label>
          <label class="video-setting video-mobile-volume"><span>音量</span><input type="range" min="0" max="1" step=".01" :value="effectiveVolume" aria-label="音量" @input="emit('changeVolume',$event)"></label>
          <button @click="close();emit('download')"><Download aria-hidden="true" />下载</button><button @click="close();emit('move')"><Move aria-hidden="true" />移动</button><button @click="close();emit('copy')"><Copy aria-hidden="true" />复制</button>
          <p class="media-detail">{{ playbackInfo }}</p>
        </template>
      </PreviewMenu>
      <button class="video-icon-button" :aria-label="fullscreen?'退出全屏':'全屏'" @click="emit('toggleFullscreen')"><Minimize v-if="fullscreen" aria-hidden="true" /><Maximize v-else aria-hidden="true" /></button>
    </div>
  </div>
</template>

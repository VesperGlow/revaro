<script setup lang="ts">
defineProps<{itemName:string;directMode:boolean;compatibilityLabel:string;controlsVisible:boolean;playing:boolean;starting:boolean;error:string;prepareKind:'initial'|'seek';prepareMode:'mse'|'hls';timelinePosition:number;buffering:boolean;formatTime:(seconds:number)=>string}>()
defineEmits<{close:[];togglePlayback:[];retry:[]}>()
</script>

<template>
  <div class="video-top-shade" :class="{visible:controlsVisible||!playing}"><div class="video-title-group"><button class="video-back" aria-label="退出播放" @click.stop="$emit('close')"><svg viewBox="0 0 24 24"><path d="m15 5-7 7 7 7"/></svg></button><strong :title="itemName">{{ itemName }}</strong></div><span v-if="!directMode">{{ compatibilityLabel }}</span></div>
  <button v-if="!playing&&!starting&&!error" class="video-center-play" aria-label="播放" @click.stop="$emit('togglePlayback')"><svg viewBox="0 0 24 24"><path d="m9 7 9 5-9 5Z"/></svg></button>
  <div v-if="starting" class="video-loading" :class="{compact:prepareKind==='seek'}"><span></span><strong>{{ prepareKind==='seek'?`正在定位到 ${formatTime(timelinePosition)}`:'正在准备视频' }}</strong><small v-if="prepareKind==='initial'">{{ prepareMode==='mse'?'MSE 原码流 · fMP4 实时封装':'HLS 兼容流 · 正在生成启动缓冲' }}</small></div>
  <div v-else-if="buffering" class="video-buffering"><span></span><strong>正在缓冲</strong></div>
  <p v-if="error" class="video-error">{{ error }} <button @click="$emit('retry')">重试</button></p>
</template>

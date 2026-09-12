<script setup lang="ts">
import { ChevronLeft, Play } from '@lucide/vue'
defineProps<{itemName:string;controlsVisible:boolean;playing:boolean;starting:boolean;error:string;buffering:boolean}>()
defineEmits<{close:[];togglePlayback:[];retry:[]}>()
</script>

<template>
  <div class="video-top-shade" :class="{visible:controlsVisible||!playing}" :inert="!controlsVisible&&playing"><div class="video-title-group"><button class="video-back" aria-label="退出播放" @click.stop="$emit('close')"><ChevronLeft aria-hidden="true" /></button><strong :title="itemName">{{ itemName }}</strong></div></div>
  <button v-if="!playing&&!starting&&!error" class="video-center-play" aria-label="播放" @click.stop="$emit('togglePlayback')"><Play aria-hidden="true" /></button>
  <div v-if="starting" class="video-loading"><span></span><strong>正在准备视频</strong><small>准备好后会自动开始播放</small></div>
  <div v-else-if="buffering" class="video-buffering"><span></span><strong>正在缓冲</strong></div>
  <div v-if="error" class="video-error" role="alert"><p>{{ error }}</p><button @click="$emit('retry')">重新尝试</button></div>
</template>

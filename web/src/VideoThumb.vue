<script setup lang="ts">
import { computed, onBeforeUnmount, ref } from 'vue'
import type { DriveFile } from './api'

// The server generates video thumbnails in a bounded background queue. A
// cache miss returns quickly, so retry while keeping the existing icon visible.
const props = defineProps<{ file: DriveFile }>()
const emit = defineEmits<{ (e:'failed'):void }>()
const loaded = ref(false)
const failed = ref(false)
const attempt = ref(0)
const baseURL = computed(() => `/api/files/${props.file.id}/thumbnail?v=${encodeURIComponent(props.file.etag || '')}`)
const src = computed(() => `${baseURL.value}&retry=${attempt.value}`)
const retryDelays = [800, 1600, 3200, 6400, 12800]
let timer = 0

function onError() {
  if (attempt.value >= retryDelays.length) {
    failed.value = true
    emit('failed')
    return
  }
  timer = window.setTimeout(() => attempt.value++, retryDelays[attempt.value])
}

onBeforeUnmount(() => window.clearTimeout(timer))
</script>

<template>
  <div class="video-thumb">
    <span class="thumb-fallback" :class="{ hidden: loaded && !failed }"><slot /></span>
    <img v-if="!failed" class="ui-image" :src="src" alt="" loading="lazy" draggable="false" @load="loaded = true" @error="onError">
  </div>
</template>

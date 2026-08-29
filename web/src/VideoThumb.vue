<script setup lang="ts">
import { computed, onBeforeUnmount, onMounted, ref } from 'vue'
import type { DriveFile } from './api'

// 视频缩略图：直接渲染持久化 URL（服务端 Rust/libav 生成或前端上传，浏览器
// 长期缓存，命中时秒出）；加载期间显示占位符号、成功后盖住，不再出现
// "裂图"。404 才走前端抽帧兜底并上传持久化。
const props = defineProps<{ file: DriveFile }>()
const emit = defineEmits<{ (e:'failed'):void }>()
const rootEl = ref<HTMLElement|null>(null)
const captured = ref('') // 本次会话抽到的 dataURL
const failed = ref(false)
const erroring = ref(false)
const loaded = ref(false)

const sessionCache = new Map<string,string>()
const thumbURL = computed(() => `/api/files/${props.file.id}/thumbnail?v=${encodeURIComponent(props.file.etag || '')}`)
const src = computed(() => captured.value || thumbURL.value)

let disposed = false
let io: IntersectionObserver|null = null
let started = false

onMounted(() => {
  const cached = sessionCache.get(cacheKey())
  if (cached) { captured.value = cached; return }
  if (disposed) return
})
onBeforeUnmount(() => {
  disposed = true
  io?.disconnect()
  io = null
})

function cacheKey(){ return props.file.id + ':' + (props.file.etag || '') }

function onError(){
  if (captured.value || failed.value) return
  erroring.value = true
  beginCapture()
}

function beginCapture(){
  if (started || failed.value) return
  started = true
  if (disposed) return
  if (!('IntersectionObserver' in window)) { capture(); return }
  io = new IntersectionObserver(entries => {
    for (const entry of entries) {
      if (entry.isIntersecting) { io?.disconnect(); io = null; capture() }
    }
  }, { rootMargin: '240px' })
  io.observe(rootEl.value!)
}

async function capture(){
  if (disposed) return
  const url = await captureFrame()
  if (disposed) return
  if (!url) { failed.value = true; erroring.value = false; emit('failed'); return }
  sessionCache.set(cacheKey(), url)
  captured.value = url
  erroring.value = false
  persist(url)
}

function captureFrame(): Promise<string|null> {
  return new Promise(resolve => {
    const video = document.createElement('video')
    video.muted = true
    video.playsInline = true
    video.preload = 'metadata'
    video.crossOrigin = 'anonymous'
    video.src = `/api/files/${props.file.id}/preview`
    let finished = false
    const timer = window.setTimeout(() => finish(null), 10000)
    const cleanup = () => {
      video.removeAttribute('src')
      video.load()
    }
    const draw = () => {
      if (finished) return
      try {
        const canvas = document.createElement('canvas')
        const scale = Math.min(1, 480 / Math.max(1, video.videoWidth))
        canvas.width = Math.max(1, Math.round(video.videoWidth * scale))
        canvas.height = Math.max(1, Math.round(video.videoHeight * scale))
        canvas.getContext('2d')?.drawImage(video, 0, 0, canvas.width, canvas.height)
        finish(canvas.toDataURL('image/jpeg', 0.72))
      } catch { finish(null) }
    }
    const finish = (url: string|null) => {
      if (finished) return
      finished = true
      window.clearTimeout(timer)
      cleanup()
      resolve(url)
    }
    video.addEventListener('loadeddata', () => {
      if (finished) return
      if (video.duration > 0 && video.currentTime === 0) video.currentTime = Math.min(1, video.duration * 0.1)
      else draw()
    })
    video.addEventListener('seeked', () => draw(), { once: true })
    video.addEventListener('error', () => finish(null))
    video.load()
  })
}

async function persist(dataURL: string){
  try {
    const blob = await (await fetch(dataURL)).blob()
    const response = await fetch(`/api/files/${props.file.id}/thumbnail`, {
      method: 'PUT',
      headers: { 'Content-Type': 'image/jpeg' },
      body: blob,
      credentials: 'same-origin',
    })
    if (!response.ok) console.warn('视频缩略图持久化失败', response.status)
  } catch (error) {
    console.warn('视频缩略图持久化失败', error)
  }
}
</script>

<template>
  <div ref="rootEl" class="video-thumb">
    <span class="thumb-fallback" :class="{ hidden: loaded && !failed }"><slot /></span>
    <img v-if="!failed && !erroring" class="ui-image" :src="src" alt="" loading="lazy" draggable="false" @load="loaded = true" @error="onError">
  </div>
</template>

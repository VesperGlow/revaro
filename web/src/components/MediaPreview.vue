<script setup lang="ts">
import { computed, onBeforeUnmount, onMounted, reactive, ref } from 'vue'
import type { DriveFile } from '../api'
import { isAudio, isImage, isVideo, previewURL } from '../fileTypes'
import { formatSize } from '../format'
import AudioPlayer from './AudioPlayer.vue'

const props=defineProps<{selected:DriveFile;items:DriveFile[]}>()
const emit=defineEmits<{close:[];change:[item:DriveFile];download:[item:DriveFile];move:[item:DriveFile];copy:[item:DriveFile]}>()

const galleryItems=computed(()=>props.items.filter(item=>isImage(item)||isVideo(item)))
const galleryIndex=computed(()=>galleryItems.value.findIndex(item=>item.id===props.selected.id))
const hasGalleryNavigation=computed(()=>galleryIndex.value>=0&&galleryItems.value.length>1)
const swipe=reactive({active:false,pointerId:0,startX:0,startY:0,dx:0,dy:0})
const stageEl=ref<HTMLElement|null>(null)
let swipeStartedOnStage=false
const stageSwipeable=computed(()=>isImage(props.selected)&&hasGalleryNavigation.value)
const swipeStyle=computed(()=>({
  transform:`translateX(${swipe.active?swipe.dx:0}px)${swipe.active?' scale(.985)':''}`,
  transition:swipe.active?'none':'transform .24s cubic-bezier(.22,.8,.3,1)',
}))

function change(direction:-1|1){
  if(!hasGalleryNavigation.value)return
  const next=(galleryIndex.value+direction+galleryItems.value.length)%galleryItems.value.length
  emit('change',galleryItems.value[next])
}
function handleKey(event:KeyboardEvent){
  if(event.key==='ArrowLeft'||event.key==='ArrowRight'){
    event.preventDefault()
    change(event.key==='ArrowLeft'?-1:1)
  }
}
function onPointerDown(event:PointerEvent){
  if(event.target instanceof Element&&event.target.closest('.preview-nav'))return
  if(event.pointerType==='mouse'&&event.button!==0)return
  swipeStartedOnStage=event.target===stageEl.value
  if(!stageSwipeable.value||swipe.active)return
  swipe.active=true;swipe.pointerId=event.pointerId;swipe.startX=event.clientX;swipe.startY=event.clientY;swipe.dx=0;swipe.dy=0
  stageEl.value?.setPointerCapture(event.pointerId)
}
function onPointerMove(event:PointerEvent){
  if(!swipe.active||event.pointerId!==swipe.pointerId)return
  swipe.dx=event.clientX-swipe.startX;swipe.dy=event.clientY-swipe.startY
}
function onPointerEnd(event:PointerEvent){
  if(!swipe.active||event.pointerId!==swipe.pointerId)return
  const {dx,dy}=swipe
  swipe.active=false;swipe.dx=0;swipe.dy=0
  if(Math.abs(dx)>60&&Math.abs(dx)>Math.abs(dy)*1.25)change(dx<0?1:-1)
}
function onStageClick(event:MouseEvent){
  if(swipeStartedOnStage&&event.target===stageEl.value)emit('close')
  swipeStartedOnStage=false
}

onMounted(()=>window.addEventListener('keydown',handleKey))
onBeforeUnmount(()=>window.removeEventListener('keydown',handleKey))
</script>

<template>
  <section class="preview-modal" :class="{'audio-preview':isAudio(selected)}" @click.self="$emit('close')">
    <span v-if="galleryIndex>=0" class="preview-count-floating">{{ galleryIndex+1 }} / {{ galleryItems.length }}</span>
    <button class="preview-close" aria-label="关闭预览" @click="$emit('close')"><svg viewBox="0 0 24 24" aria-hidden="true"><path d="M6 6l12 12M18 6 6 18"/></svg></button>
    <div ref="stageEl" class="preview-stage" :class="{swipeable:stageSwipeable}" @click="onStageClick" @pointerdown="onPointerDown" @pointermove="onPointerMove" @pointerup="onPointerEnd" @pointercancel="onPointerEnd">
      <button v-if="hasGalleryNavigation" class="preview-nav preview-prev" aria-label="上一项" @click.stop="change(-1)"><svg viewBox="0 0 24 24" aria-hidden="true"><path d="m14.5 6-6 6 6 6"/></svg></button>
      <img v-if="isImage(selected)" :key="selected.id" :src="previewURL(selected)" :alt="selected.name" :style="swipeStyle">
      <video v-else-if="isVideo(selected)" :key="selected.id" :src="previewURL(selected)" controls autoplay playsinline preload="metadata">你的浏览器不支持这个视频格式。</video>
      <AudioPlayer v-else-if="isAudio(selected)" :key="selected.id" :item="selected" />
      <button v-if="hasGalleryNavigation" class="preview-nav preview-next" aria-label="下一项" @click.stop="change(1)"><svg viewBox="0 0 24 24" aria-hidden="true"><path d="m9.5 6 6 6-6 6"/></svg></button>
    </div>
    <footer class="preview-commandbar">
      <div class="preview-file-meta"><strong :title="selected.name">{{ selected.name }}</strong><small>{{ formatSize(selected.size) }} · {{ selected.mime_type||'媒体文件' }}</small></div>
      <div class="preview-file-actions">
        <button @click="$emit('download',selected)"><svg viewBox="0 0 24 24" aria-hidden="true"><path d="M12 3v12m0 0 4-4m-4 4-4-4M5 20h14"/></svg><span>下载</span></button>
        <button @click="$emit('move',selected)"><svg viewBox="0 0 24 24" aria-hidden="true"><path d="M3 7h7l2 2h9v10H3Z"/><path d="m14 13 2 2 2-2"/></svg><span>移动</span></button>
        <button @click="$emit('copy',selected)"><svg viewBox="0 0 24 24" aria-hidden="true"><rect x="8" y="8" width="11" height="11" rx="2"/><path d="M16 8V6a2 2 0 0 0-2-2H6a2 2 0 0 0-2 2v8a2 2 0 0 0 2 2h2"/></svg><span>复制</span></button>
      </div>
    </footer>
  </section>
</template>

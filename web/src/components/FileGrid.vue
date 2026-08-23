<script setup lang="ts">
import { onBeforeUnmount, reactive } from 'vue'
import type { DriveFile } from '../api'
import { isAudio, isBook, isEditable, isEpub, isImage, isVideo, previewURL, thumbSRC } from '../fileTypes'
import { formatDate, formatSize } from '../format'
import VideoThumb from '../VideoThumb.vue'

const props=defineProps<{items:DriveFile[];selectedIds:Set<string>;trashMode?:boolean}>()
const emit=defineEmits<{open:[item:DriveFile];select:[item:DriveFile]}>()

const thumbFallbackTried=reactive<Record<string,boolean>>({})
const imageBroken=reactive<Record<string,boolean>>({})
const coverBroken=reactive<Record<string,boolean>>({})
let holdTimer=0
let heldId=''
let heldResetTimer=0
let holdX=0
let holdY=0

function thumbFallback(event:Event,item:DriveFile){
  const image=event.target as HTMLImageElement
  if(isAudio(item)){imageBroken[item.id]=true;return}
  if(thumbFallbackTried[item.id]){imageBroken[item.id]=true;return}
  thumbFallbackTried[item.id]=true
  image.src=previewURL(item)
}

function canOpen(item:DriveFile){return !props.trashMode||item.kind==='file'}
function openItem(item:DriveFile){emit('open',item)}
function hasPreview(item:DriveFile){return ((isImage(item)||isAudio(item))&&!imageBroken[item.id])||isVideo(item)||(isEpub(item)&&!coverBroken[item.id])}
function isTouch(){return window.matchMedia('(hover: none), (pointer: coarse)').matches}
function cancelHold(){window.clearTimeout(holdTimer);holdTimer=0}
function startHold(item:DriveFile,event:PointerEvent){
  if(event.pointerType!=='touch'&&event.pointerType!=='pen')return
  cancelHold()
  window.clearTimeout(heldResetTimer)
  heldId=''
  holdX=event.clientX
  holdY=event.clientY
  holdTimer=window.setTimeout(()=>{
    heldId=item.id
    emit('select',item)
    if('vibrate' in navigator)navigator.vibrate(18)
  },480)
}
function moveHold(event:PointerEvent){if(Math.hypot(event.clientX-holdX,event.clientY-holdY)>12)cancelHold()}
function finishHold(){cancelHold();if(heldId)heldResetTimer=window.setTimeout(()=>heldId='',500)}
function activate(item:DriveFile){
  if(heldId===item.id){heldId='';return}
  if(isTouch()&&props.selectedIds.size){emit('select',item);return}
  openItem(item)
}
onBeforeUnmount(()=>{cancelHold();window.clearTimeout(heldResetTimer)})
</script>

<template>
  <div class="file-grid" :class="{'selection-mode':selectedIds.size>0}">
    <article v-for="item in items" :key="item.id" class="file-card" :class="{mutedrow:item.status!=='ready',selected:selectedIds.has(item.id),'preview-tile':hasPreview(item)}" role="button" tabindex="0" :aria-label="`${item.name}，${selectedIds.has(item.id)?'已选择':'未选择'}`" @click="activate(item)" @pointerdown="startHold(item,$event)" @pointerup="finishHold" @pointercancel="finishHold" @pointermove="moveHold" @contextmenu.prevent @keydown.enter.prevent="openItem(item)" @keydown.space.prevent="$emit('select',item)">
      <button class="card-select" :class="{active:selectedIds.has(item.id)}" :title="selectedIds.has(item.id)?'取消选择':'选择项目'" :aria-label="selectedIds.has(item.id)?'取消选择':'选择项目'" :aria-pressed="selectedIds.has(item.id)" @click.stop="$emit('select',item)" @keydown.stop>
        <svg viewBox="0 0 24 24" aria-hidden="true"><path d="m5 12 4 4L19 6"/></svg>
      </button>
      <div class="card-preview" :class="{'cannot-open':!canOpen(item)}" :title="trashMode&&item.kind==='directory'?'恢复后可打开文件夹':isBook(item)?'阅读':trashMode&&isEditable(item)?'只读查看':item.kind==='directory'?'打开文件夹':isEditable(item)?'编辑文档':isImage(item)?'预览图片':isVideo(item)?'播放视频':isAudio(item)?'播放音频':'文件'">
        <img v-if="(isImage(item)||isAudio(item))&&!imageBroken[item.id]" class="ui-image" :src="thumbSRC(item)" :alt="item.name" loading="lazy" draggable="false" @error="thumbFallback($event,item)">
        <VideoThumb v-else-if="isVideo(item)" :file="item"><span class="large-video"><svg viewBox="0 0 24 24" aria-hidden="true"><path d="m9 7 8 5-8 5Z"/></svg></span></VideoThumb>
        <img v-else-if="isEpub(item)&&!coverBroken[item.id]" class="ui-image" :src="thumbSRC(item)" :alt="item.name" loading="lazy" draggable="false" @error="coverBroken[item.id]=true">
        <svg v-else-if="isEpub(item)" class="file-type-icon book-type-icon" viewBox="0 0 96 96" aria-hidden="true"><path d="M48 26c-8-6-18-8-32-8v53c14 0 24 2 32 8 8-6 18-8 32-8V18c-14 0-24 2-32 8Z"/><path d="M48 26v53"/><path d="M23 31c7 0 13 1 18 4M23 43c7 0 13 1 18 4M73 31c-7 0-13 1-18 4M73 43c-7 0-13 1-18 4"/></svg>
        <svg v-else-if="item.kind==='directory'" class="file-type-icon folder-type-icon" viewBox="0 0 96 96" aria-hidden="true"><path d="M9 28h30l9 10h39v38H9Z"/><path d="M9 28v-8h27l8 8"/></svg>
        <svg v-else-if="isEditable(item)" class="file-type-icon document-type-icon" viewBox="0 0 96 96" aria-hidden="true"><path d="M24 12h34l16 16v56H24Z"/><path d="M58 12v16h16M34 45h30M34 56h30M34 67h21"/></svg>
        <svg v-else-if="isAudio(item)" class="file-type-icon audio-type-icon" viewBox="0 0 96 96" aria-hidden="true"><path d="M24 12h34l16 16v56H24Z"/><path d="M58 12v16h16M58 42v25"/><path d="m58 42-18 4v25"/><ellipse cx="34" cy="72" rx="8" ry="6"/><ellipse cx="52" cy="68" rx="8" ry="6"/></svg>
        <svg v-else class="file-type-icon generic-type-icon" viewBox="0 0 96 96" aria-hidden="true"><path d="M24 12h34l16 16v56H24Z"/><path d="M58 12v16h16"/></svg>
      </div>
      <div class="card-info"><strong :title="item.name">{{ item.name }}</strong><small v-if="trashMode">{{ item.kind==='directory'?'文件夹':formatSize(item.size) }} · 删除于 {{ formatDate(item.deleted_at||item.updated_at) }}</small><small v-else>{{ item.kind==='directory'?'文件夹':formatSize(item.size) }}</small></div>
    </article>
  </div>
</template>

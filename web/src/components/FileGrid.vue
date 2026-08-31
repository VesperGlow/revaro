<script setup lang="ts">
import { onBeforeUnmount, reactive } from 'vue'
import type { DriveFile } from '../api'
import { hasAudioCover, isAudio, isBook, isEditable, isEpub, isImage, isVideo, previewURL, thumbSRC } from '../fileTypes'
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
function hasPreview(item:DriveFile){return (isImage(item)&&!imageBroken[item.id])||(hasAudioCover(item)&&!imageBroken[item.id])||isVideo(item)||(isEpub(item)&&!coverBroken[item.id])}
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
    <article v-for="item in items" :key="item.id" class="file-card" :class="{mutedrow:item.status!=='ready',selected:selectedIds.has(item.id),'preview-tile':hasPreview(item),'fallback-tile':!hasPreview(item),'folder-tile':item.kind==='directory','document-tile':isEditable(item),'book-tile':isEpub(item),'audio-tile':isAudio(item)}" role="button" tabindex="0" :aria-label="`${item.name}，${selectedIds.has(item.id)?'已选择':'未选择'}`" @click="activate(item)" @pointerdown="startHold(item,$event)" @pointerup="finishHold" @pointercancel="finishHold" @pointermove="moveHold" @contextmenu.prevent @keydown.enter.prevent="openItem(item)" @keydown.space.prevent="$emit('select',item)">
      <button class="card-select" :class="{active:selectedIds.has(item.id)}" :title="selectedIds.has(item.id)?'取消选择':'选择项目'" :aria-label="selectedIds.has(item.id)?'取消选择':'选择项目'" :aria-pressed="selectedIds.has(item.id)" @click.stop="$emit('select',item)" @keydown.stop>
        <svg viewBox="0 0 24 24" aria-hidden="true"><path d="m5 12 4 4L19 6"/></svg>
      </button>
      <div class="card-preview" :class="{'cannot-open':!canOpen(item)}" :title="trashMode&&item.kind==='directory'?'恢复后可打开文件夹':isBook(item)?'阅读':trashMode&&isEditable(item)?'只读查看':item.kind==='directory'?'打开文件夹':isEditable(item)?'编辑文档':isImage(item)?'预览图片':isVideo(item)?'播放视频':isAudio(item)?'播放音频':'文件'">
        <img v-if="(isImage(item)||hasAudioCover(item))&&!imageBroken[item.id]" class="ui-image" :src="thumbSRC(item)" :alt="item.name" loading="lazy" draggable="false" @error="thumbFallback($event,item)">
        <VideoThumb v-else-if="isVideo(item)" :file="item"><span class="large-video"><svg viewBox="0 0 24 24" aria-hidden="true"><path d="m9 7 8 5-8 5Z"/></svg></span></VideoThumb>
        <img v-else-if="isEpub(item)&&!coverBroken[item.id]" class="ui-image" :src="thumbSRC(item)" :alt="item.name" loading="lazy" draggable="false" @error="coverBroken[item.id]=true">
        <svg v-else-if="isEpub(item)" class="file-type-icon book-type-icon" viewBox="0 0 96 96" aria-hidden="true"><path class="icon-base" d="M48 24c-9-6-20-8-34-8v57c14 0 25 2 34 8 9-6 20-8 34-8V16c-14 0-25 2-34 8Z"/><path class="icon-detail" d="M48 24v57M23 31c7 0 13 1 18 4M23 44c7 0 13 1 18 4M73 31c-7 0-13 1-18 4M73 44c-7 0-13 1-18 4"/></svg>
        <svg v-else-if="item.kind==='directory'" class="file-type-icon folder-type-icon" viewBox="0 0 96 96" aria-hidden="true"><path class="folder-back" d="M10 23c0-4 3-7 7-7h21l10 11h31c4 0 7 3 7 7v9H10Z"/><path class="folder-front" d="M8 38c0-4 3-7 7-7h66c5 0 8 4 7 9l-7 35c-1 4-4 6-8 6H16c-4 0-7-3-7-7Z"/><path class="folder-highlight" d="M17 38h62l-1 6H16Z"/></svg>
        <svg v-else-if="isEditable(item)" class="file-type-icon document-type-icon" viewBox="0 0 96 96" aria-hidden="true"><path class="icon-base" d="M22 10h38l17 17v58H22Z"/><path class="icon-fold" d="M60 10v17h17Z"/><path class="icon-detail" d="M34 45h31M34 57h31M34 69h22"/></svg>
        <svg v-else-if="isAudio(item)" class="file-type-icon audio-type-icon" viewBox="0 0 96 96" aria-hidden="true"><path class="icon-base" d="M22 10h38l17 17v58H22Z"/><path class="icon-fold" d="M60 10v17h17Z"/><path class="icon-detail audio-note" d="M62 42v27m0-27-20 5v27"/><ellipse class="icon-accent" cx="35" cy="75" rx="9" ry="7"/><ellipse class="icon-accent" cx="55" cy="70" rx="9" ry="7"/></svg>
        <svg v-else class="file-type-icon generic-type-icon" viewBox="0 0 96 96" aria-hidden="true"><path class="icon-base" d="M22 10h38l17 17v58H22Z"/><path class="icon-fold" d="M60 10v17h17Z"/><circle class="icon-accent" cx="49" cy="58" r="5"/></svg>
      </div>
      <div class="card-info"><strong :title="item.name">{{ item.name }}</strong><small v-if="trashMode">{{ item.kind==='directory'?'文件夹':formatSize(item.size) }} · 删除于 {{ formatDate(item.deleted_at||item.updated_at) }}</small><small v-else>{{ item.kind==='directory'?'文件夹':formatSize(item.size) }}</small></div>
    </article>
  </div>
</template>

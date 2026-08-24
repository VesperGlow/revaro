<script setup lang="ts">
import { computed, onBeforeUnmount, onMounted, reactive, ref, watch } from 'vue'
import type { DriveFile } from '../api'
import { isAudio, isImage, isVideo, previewURL } from '../fileTypes'
import { formatSize } from '../format'
import AudioPlayer from './AudioPlayer.vue'
import VideoPlayer from './VideoPlayer.vue'

const props=defineProps<{selected:DriveFile;items:DriveFile[]}>()
const emit=defineEmits<{close:[];change:[item:DriveFile];download:[item:DriveFile];move:[item:DriveFile];copy:[item:DriveFile]}>()

const galleryItems=computed(()=>props.items.filter(isImage))
const galleryIndex=computed(()=>galleryItems.value.findIndex(item=>item.id===props.selected.id))
const hasGalleryNavigation=computed(()=>galleryIndex.value>=0&&galleryItems.value.length>1)
const swipe=reactive({active:false,pointerId:0,startX:0,startY:0,dx:0,dy:0})
const zoom=ref(1)
const zoomNotice=ref(false)
const pan=reactive({x:0,y:0})
const stageEl=ref<HTMLElement|null>(null)
const pointers=new Map<number,{x:number;y:number}>()
let startPanX=0
let startPanY=0
let pinchDistance=0
let pinchZoom=1
let gestureMoved=false
let swipeStartedOnStage=false
let zoomNoticeTimer=0
const stageSwipeable=computed(()=>isImage(props.selected))
const swipeStyle=computed(()=>({
  transform:`translate3d(${pan.x+(zoom.value===1&&swipe.active?swipe.dx:0)}px,${pan.y}px,0) scale(${zoom.value})`,
  transition:swipe.active?'none':'transform .24s cubic-bezier(.22,.8,.3,1)',
  cursor:zoom.value>1?(swipe.active?'grabbing':'grab'):'zoom-in',
}))

function change(direction:-1|1){
  if(!hasGalleryNavigation.value)return
  const next=(galleryIndex.value+direction+galleryItems.value.length)%galleryItems.value.length
  emit('change',galleryItems.value[next])
}
function handleKey(event:KeyboardEvent){
  if(isImage(props.selected)&&(event.key==='ArrowLeft'||event.key==='ArrowRight')&&zoom.value===1){
    event.preventDefault()
    change(event.key==='ArrowLeft'?-1:1)
  }
}
function onPointerDown(event:PointerEvent){
  if(event.target instanceof Element&&event.target.closest('.preview-nav'))return
  if(event.pointerType==='mouse'&&event.button!==0)return
  swipeStartedOnStage=event.target===stageEl.value
  if(!stageSwipeable.value||pointers.has(event.pointerId))return
  pointers.set(event.pointerId,{x:event.clientX,y:event.clientY})
  if(pointers.size===1){swipe.active=true;swipe.pointerId=event.pointerId;swipe.startX=event.clientX;swipe.startY=event.clientY;swipe.dx=0;swipe.dy=0;startPanX=pan.x;startPanY=pan.y;gestureMoved=false}
  else if(pointers.size===2){const [a,b]=[...pointers.values()];pinchDistance=Math.hypot(a.x-b.x,a.y-b.y);pinchZoom=zoom.value;swipe.dx=0;swipe.dy=0}
  stageEl.value?.setPointerCapture(event.pointerId)
}
function onPointerMove(event:PointerEvent){
  if(!swipe.active||!pointers.has(event.pointerId))return
  pointers.set(event.pointerId,{x:event.clientX,y:event.clientY})
  if(pointers.size>=2){
    const [a,b]=[...pointers.values()];const distance=Math.hypot(a.x-b.x,a.y-b.y)
    if(pinchDistance>0)setZoom(pinchZoom*distance/pinchDistance)
    gestureMoved=true;swipe.dx=0;swipe.dy=0;return
  }
  swipe.dx=event.clientX-swipe.startX;swipe.dy=event.clientY-swipe.startY
  if(Math.abs(swipe.dx)>3||Math.abs(swipe.dy)>3)gestureMoved=true
  if(zoom.value>1){pan.x=startPanX+swipe.dx;pan.y=startPanY+swipe.dy;clampPan()}
}
function onPointerEnd(event:PointerEvent){
  if(!swipe.active||!pointers.has(event.pointerId))return
  pointers.delete(event.pointerId)
  if(pointers.size){const [remaining]=[...pointers.entries()];swipe.pointerId=remaining[0];swipe.startX=remaining[1].x;swipe.startY=remaining[1].y;startPanX=pan.x;startPanY=pan.y;return}
  const {dx,dy}=swipe
  swipe.active=false;swipe.dx=0;swipe.dy=0
  if(zoom.value===1&&Math.abs(dx)>60&&Math.abs(dx)>Math.abs(dy)*1.25)change(dx<0?1:-1)
}
function onStageClick(event:MouseEvent){
  if(!gestureMoved&&swipeStartedOnStage&&event.target===stageEl.value)emit('close')
  swipeStartedOnStage=false
}
function clampPan(){
  const stage=stageEl.value;if(!stage||zoom.value<=1){if(zoom.value<=1){pan.x=0;pan.y=0};return}
  const maxX=stage.clientWidth*(zoom.value-1)/2
  const maxY=stage.clientHeight*(zoom.value-1)/2
  pan.x=Math.max(-maxX,Math.min(maxX,pan.x));pan.y=Math.max(-maxY,Math.min(maxY,pan.y))
}
function setZoom(value:number,showNotice=true){const next=Math.max(1,Math.min(5,Math.round(value*100)/100));if(next===zoom.value)return;zoom.value=next;if(zoom.value===1){pan.x=0;pan.y=0}else clampPan();if(showNotice){zoomNotice.value=true;window.clearTimeout(zoomNoticeTimer);zoomNoticeTimer=window.setTimeout(()=>zoomNotice.value=false,900)}}
function onWheel(event:WheelEvent){if(!isImage(props.selected))return;event.preventDefault();setZoom(zoom.value+(event.deltaY<0?.35:-.35))}
function toggleZoom(){setZoom(zoom.value>1?1:2)}

watch(()=>props.selected.id,()=>{setZoom(1,false);zoomNotice.value=false;pointers.clear();swipe.active=false})

onMounted(()=>window.addEventListener('keydown',handleKey))
onBeforeUnmount(()=>{window.removeEventListener('keydown',handleKey);window.clearTimeout(zoomNoticeTimer)})
</script>

<template>
  <section class="preview-modal" :class="{'audio-preview':isAudio(selected),'video-preview':isVideo(selected),'image-preview':isImage(selected)}" @click.self="$emit('close')">
    <header v-if="isImage(selected)" class="preview-commandbar preview-image-bar">
      <div class="preview-command-content">
        <div class="preview-file-meta"><strong :title="selected.name">{{ selected.name }}</strong><small>{{ formatSize(selected.size) }} · {{ selected.mime_type||'图片' }}</small></div>
        <div class="preview-file-actions">
          <button @click="$emit('download',selected)"><svg viewBox="0 0 24 24" aria-hidden="true"><path d="M12 3v12m0 0 4-4m-4 4-4-4M5 20h14"/></svg><span>下载</span></button>
          <button @click="$emit('move',selected)"><svg viewBox="0 0 24 24" aria-hidden="true"><path d="M3 7h7l2 2h9v10H3Z"/><path d="m14 13 2 2 2-2"/></svg><span>移动</span></button>
          <button @click="$emit('copy',selected)"><svg viewBox="0 0 24 24" aria-hidden="true"><rect x="8" y="8" width="11" height="11" rx="2"/><path d="M16 8V6a2 2 0 0 0-2-2H6a2 2 0 0 0-2 2v8a2 2 0 0 0 2 2h2"/></svg><span>复制</span></button>
        </div>
      </div>
    </header>
    <span v-if="galleryIndex>=0" class="preview-count-floating">{{ galleryIndex+1 }} / {{ galleryItems.length }}</span>
    <button v-if="!isVideo(selected)" class="preview-close" aria-label="关闭预览" @click="$emit('close')"><svg viewBox="0 0 24 24" aria-hidden="true"><path d="M6 6l12 12M18 6 6 18"/></svg></button>
    <div ref="stageEl" class="preview-stage" :class="{swipeable:stageSwipeable,zoomed:zoom>1}" @click="onStageClick" @pointerdown="onPointerDown" @pointermove="onPointerMove" @pointerup="onPointerEnd" @pointercancel="onPointerEnd" @wheel="onWheel">
      <button v-if="hasGalleryNavigation&&isImage(selected)" class="preview-nav preview-prev" aria-label="上一项" @click.stop="change(-1)"><svg viewBox="0 0 24 24" aria-hidden="true"><path d="m14.5 6-6 6 6 6"/></svg></button>
      <img v-if="isImage(selected)" :key="selected.id" :src="previewURL(selected)" :alt="selected.name" :style="swipeStyle" draggable="false" @dblclick.stop="toggleZoom">
      <VideoPlayer v-else-if="isVideo(selected)" :key="selected.id" :item="selected" @close="emit('close')" @download="emit('download',$event)" @move="emit('move',$event)" @copy="emit('copy',$event)" />
      <AudioPlayer v-else-if="isAudio(selected)" :key="selected.id" :item="selected" @download="emit('download',$event)" @move="emit('move',$event)" @copy="emit('copy',$event)" />
      <button v-if="hasGalleryNavigation&&isImage(selected)" class="preview-nav preview-next" aria-label="下一项" @click.stop="change(1)"><svg viewBox="0 0 24 24" aria-hidden="true"><path d="m9.5 6 6 6-6 6"/></svg></button>
      <output v-if="isImage(selected)&&zoomNotice" class="preview-zoom-notice">{{ Math.round(zoom*100) }}%</output>
    </div>
  </section>
</template>

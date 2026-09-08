<script setup lang="ts">
import { computed, defineAsyncComponent, nextTick, onBeforeUnmount, onMounted, reactive, ref, watch } from 'vue'
import { ChevronLeft, ChevronRight, X, Ellipsis, ZoomIn, ZoomOut, Scan, GalleryHorizontalEnd, Download, Move, Copy, Info } from '@lucide/vue'
import type { DriveFile } from '../api'
import { isAudio, isImage, isVideo, previewURL, thumbSRC } from '../fileTypes'
import { formatSize } from '../format'
import { clampImagePan, fitImage, zoomImagePan, type Point } from '../imageGeometry'
import { usePreviewDialog } from '../composables/usePreviewDialog'
import PreviewMenu from './PreviewMenu.vue'
const AudioPlayer=defineAsyncComponent(()=>import('./AudioPlayer.vue'))
const VideoPlayer=defineAsyncComponent(()=>import('./VideoPlayer.vue'))
const props=defineProps<{selected:DriveFile;items:DriveFile[]}>()
const emit=defineEmits<{close:[];change:[item:DriveFile];download:[item:DriveFile];move:[item:DriveFile];copy:[item:DriveFile]}>()
const root=ref<HTMLElement|null>(null)
const stageEl=ref<HTMLElement|null>(null)
usePreviewDialog(root,()=>emit('close'))
const galleryItems=computed(()=>props.items.filter(isImage))
const galleryIndex=computed(()=>galleryItems.value.findIndex(item=>item.id===props.selected.id))
const hasGalleryNavigation=computed(()=>galleryItems.value.length>1)
const chromeVisible=ref(true)
const thumbnailsOpen=ref(false)
const loading=ref(true)
const imageError=ref(false)
const natural=reactive({width:0,height:0})
const stageSize=reactive({width:0,height:0})
const fitted=computed(()=>fitImage(natural,stageSize))
const zoom=ref(1)
const actualZoom=computed(()=>natural.width&&fitted.value.width?natural.width/fitted.value.width:1)
const maxZoom=computed(()=>Math.max(8,actualZoom.value))
const zoomPercent=computed(()=>Math.round(zoom.value/actualZoom.value*100))
const pan=reactive({x:0,y:0})
const pointers=new Map<number,Point>()
const drag=reactive({active:false,startX:0,startY:0,dx:0,dy:0})
let gestureMoved=false
let pinched=false
let clickTimer=0
let observer:ResizeObserver|undefined
const imageStyle=computed(()=>({width:`${fitted.value.width}px`,height:`${fitted.value.height}px`,transform:`translate(-50%,-50%) translate3d(${pan.x+(zoom.value===1?drag.dx:0)}px,${pan.y}px,0) scale(${zoom.value})`}))
function clampPan(){Object.assign(pan,clampImagePan(pan,fitted.value,stageSize,zoom.value))}
function local(point:Point):Point{const bounds=stageEl.value?.getBoundingClientRect();return bounds?{x:point.x-bounds.left-bounds.width/2,y:point.y-bounds.top-bounds.height/2}:point}
function setZoom(value:number,from:Point={x:0,y:0},to:Point=from){
  const next=Math.max(1,Math.min(maxZoom.value,value))
  Object.assign(pan,zoomImagePan(pan,from,to,next/zoom.value));zoom.value=next;clampPan()
}
function fit(){zoom.value=1;pan.x=0;pan.y=0}
function toggleZoom(){window.clearTimeout(clickTimer);setZoom(Math.abs(zoom.value-actualZoom.value)<.01?1:actualZoom.value)}
function change(direction:-1|1){
  if(!hasGalleryNavigation.value)return
  const index=(galleryIndex.value+direction+galleryItems.value.length)%galleryItems.value.length
  emit('change',galleryItems.value[index])
}
function onKey(event:KeyboardEvent){
  if(!isImage(props.selected))return
  if(event.key==='Tab')chromeVisible.value=true
  if(event.target instanceof Element&&event.target.closest('input, select, details[open]'))return
  if(event.key==='ArrowLeft'||event.key==='ArrowRight'){
    event.preventDefault()
    if(zoom.value===1)change(event.key==='ArrowLeft'?-1:1)
    else{pan.x+=event.key==='ArrowLeft'?64:-64;clampPan()}
  }else if(event.key==='+'||event.key==='='){event.preventDefault();setZoom(zoom.value*1.2)}
  else if(event.key==='-'){event.preventDefault();setZoom(zoom.value/1.2)}
  else if(event.key==='0')fit()
  else if(event.key==='1')setZoom(actualZoom.value)
}
function onPointerDown(event:PointerEvent){
  if(!isImage(props.selected)||imageError.value||loading.value||event.button!==0||event.target instanceof Element&&event.target.closest('button'))return
  pointers.set(event.pointerId,{x:event.clientX,y:event.clientY})
  if(pointers.size===1){drag.active=true;drag.startX=event.clientX;drag.startY=event.clientY;drag.dx=0;drag.dy=0;gestureMoved=false;pinched=false}
  if(pointers.size>1){pinched=true;gestureMoved=true;drag.dx=0;drag.dy=0}
  stageEl.value?.setPointerCapture(event.pointerId)
}
function onPointerMove(event:PointerEvent){
  const before=pointers.get(event.pointerId)
  if(!before)return
  const old=[...pointers.values()]
  pointers.set(event.pointerId,{x:event.clientX,y:event.clientY})
  if(pointers.size>=2){
    const [a,b]=old,[c,d]=[...pointers.values()]
    const distance=Math.hypot(a.x-b.x,a.y-b.y)
    if(distance>0)setZoom(zoom.value*Math.hypot(c.x-d.x,c.y-d.y)/distance,local({x:(a.x+b.x)/2,y:(a.y+b.y)/2}),local({x:(c.x+d.x)/2,y:(c.y+d.y)/2}))
    return
  }
  drag.dx=event.clientX-drag.startX;drag.dy=event.clientY-drag.startY
  if(Math.hypot(drag.dx,drag.dy)>5)gestureMoved=true
  if(zoom.value>1){pan.x+=event.clientX-before.x;pan.y+=event.clientY-before.y;clampPan()}
}
function onPointerEnd(event:PointerEvent){
  if(!pointers.has(event.pointerId))return
  pointers.delete(event.pointerId)
  if(event.type==='pointercancel'){pinched=true;gestureMoved=true}
  if(pointers.size){const point=[...pointers.values()][0];drag.startX=point.x;drag.startY=point.y;drag.dx=0;drag.dy=0;return}
  if(!pinched&&zoom.value===1&&Math.abs(drag.dx)>60&&Math.abs(drag.dx)>Math.abs(drag.dy)*1.25)change(drag.dx<0?1:-1)
  drag.active=false;drag.dx=0;drag.dy=0
}
function onStageClick(event:MouseEvent){
  if(!isImage(props.selected)||gestureMoved||event.target instanceof Element&&event.target.closest('button'))return
  window.clearTimeout(clickTimer)
  if(event.detail<2)clickTimer=window.setTimeout(()=>chromeVisible.value=!chromeVisible.value,220)
}
function onWheel(event:WheelEvent){
  if(!isImage(props.selected)||loading.value)return
  event.preventDefault()
  const delta=event.deltaY*(event.deltaMode===1?16:event.deltaMode===2?stageSize.height:1)
  setZoom(zoom.value*Math.exp(-Math.max(-120,Math.min(120,delta))*.002),local({x:event.clientX,y:event.clientY}))
}
function onImageLoad(event:Event){const image=event.target as HTMLImageElement;natural.width=image.naturalWidth;natural.height=image.naturalHeight;loading.value=false;clampPan()}
function thumbnailFallback(event:Event,item:DriveFile){const image=event.target as HTMLImageElement;const source=new URL(previewURL(item),location.origin).href;if(image.src!==source)image.src=source}
function revealThumbnail(){void nextTick(()=>root.value?.querySelector<HTMLElement>('.preview-filmstrip [aria-current="true"]')?.scrollIntoView({block:'nearest',inline:'center'}))}
watch(thumbnailsOpen,revealThumbnail)
watch(()=>props.selected.id,()=>{
  fit();natural.width=0;natural.height=0;loading.value=true;imageError.value=false;pointers.clear();drag.active=false;drag.dx=0;window.clearTimeout(clickTimer);revealThumbnail()
  if(isImage(props.selected)&&galleryItems.value.length>1)for(const offset of [-1,1]){const item=galleryItems.value[(galleryIndex.value+offset+galleryItems.value.length)%galleryItems.value.length];const image=new Image();image.src=previewURL(item)}
})
onMounted(()=>{observer=new ResizeObserver(()=>{if(stageEl.value){stageSize.width=stageEl.value.clientWidth;stageSize.height=stageEl.value.clientHeight;clampPan()}});if(stageEl.value)observer.observe(stageEl.value)})
onBeforeUnmount(()=>{observer?.disconnect();window.clearTimeout(clickTimer)})
</script>

<template>
  <section ref="root" class="preview-modal" role="dialog" aria-modal="true" :aria-label="selected.name" tabindex="-1" :class="{'audio-preview':isAudio(selected),'video-preview':isVideo(selected),'image-preview':isImage(selected),'chrome-hidden':!chromeVisible,'thumbnails-open':thumbnailsOpen}" @keydown="onKey" @keydown.tab.capture="chromeVisible=true">
    <header v-if="!isVideo(selected)" class="preview-commandbar" :inert="isImage(selected)&&!chromeVisible">
      <div class="preview-file-meta"><strong :title="selected.name">{{ selected.name }}</strong></div>
      <span v-if="isImage(selected)" class="preview-count">{{ galleryIndex+1 }} / {{ galleryItems.length }}</span>
      <div class="preview-file-actions">
        <PreviewMenu label="更多操作">
          <template #trigger><Ellipsis aria-hidden="true" /></template><template #default="{close}">
            <button @click="close();emit('download',selected)"><Download aria-hidden="true" />下载</button><button @click="close();emit('move',selected)"><Move aria-hidden="true" />移动</button><button @click="close();emit('copy',selected)"><Copy aria-hidden="true" />复制</button>
            <p class="media-detail"><Info aria-hidden="true" />{{ formatSize(selected.size) }}<span v-if="natural.width">{{ natural.width }} × {{ natural.height }}</span></p>
          </template>
        </PreviewMenu>
        <button class="media-icon-button preview-close" aria-label="关闭预览" title="关闭预览" @click="emit('close')"><X aria-hidden="true" /></button>
      </div>
    </header>
    <div ref="stageEl" class="preview-stage" :class="{zoomed:zoom>1,dragging:drag.active}" @click="onStageClick" @pointerdown="onPointerDown" @pointermove="onPointerMove" @pointerup="onPointerEnd" @pointercancel="onPointerEnd" @wheel="onWheel">
      <template v-if="isImage(selected)">
        <img v-show="!loading&&!imageError" :key="selected.id" class="preview-image" :src="previewURL(selected)" :alt="selected.name" :style="imageStyle" draggable="false" @load="onImageLoad" @error="imageError=true;loading=false" @dblclick.stop="toggleZoom">
        <p v-if="imageError" class="preview-image-status" role="alert">图片暂时无法加载<button @click="emit('download',selected)">下载原图</button></p>
        <span v-else-if="loading" class="preview-image-status" role="status">正在加载图片…</span>
        <button v-if="hasGalleryNavigation" class="media-icon-button preview-nav preview-prev" :inert="!chromeVisible" aria-label="上一张" @click.stop="change(-1)"><ChevronLeft aria-hidden="true" /></button>
        <button v-if="hasGalleryNavigation" class="media-icon-button preview-nav preview-next" :inert="!chromeVisible" aria-label="下一张" @click.stop="change(1)"><ChevronRight aria-hidden="true" /></button>
      </template>
      <VideoPlayer v-else-if="isVideo(selected)" :key="selected.id" :item="selected" @close="emit('close')" @download="emit('download',$event)" @move="emit('move',$event)" @copy="emit('copy',$event)" />
      <AudioPlayer v-else-if="isAudio(selected)" :key="selected.id" :item="selected" />
    </div>
    <footer v-if="isImage(selected)" class="preview-image-footer" :inert="!chromeVisible">
      <div class="preview-image-tools" aria-label="图片工具">
        <button class="media-icon-button" aria-label="适应窗口" title="适应窗口" :disabled="loading||imageError" @click="fit"><Scan aria-hidden="true" /></button>
        <button class="media-icon-button" aria-label="缩小" title="缩小" :disabled="zoom<=1||loading||imageError" @click="setZoom(zoom/1.2)"><ZoomOut aria-hidden="true" /></button>
        <button class="preview-actual-size" aria-label="实际大小" title="实际大小（100%）" :disabled="loading||imageError" @click="setZoom(actualZoom)">{{ zoomPercent }}%</button>
        <button class="media-icon-button" aria-label="放大" title="放大" :disabled="zoom>=maxZoom||loading||imageError" @click="setZoom(zoom*1.2)"><ZoomIn aria-hidden="true" /></button>
        <button v-if="hasGalleryNavigation" class="media-icon-button" aria-label="缩略图" title="缩略图" :aria-expanded="thumbnailsOpen" @click="thumbnailsOpen=!thumbnailsOpen"><GalleryHorizontalEnd aria-hidden="true" /></button>
      </div>
      <div v-if="thumbnailsOpen" class="preview-filmstrip"><button v-for="item in galleryItems" :key="item.id" :aria-label="`查看 ${item.name}`" :aria-current="item.id===selected.id?'true':undefined" @click="emit('change',item)"><img :src="thumbSRC(item)" :alt="item.name" loading="lazy" draggable="false" @error="thumbnailFallback($event,item)"></button></div>
    </footer>
  </section>
</template>

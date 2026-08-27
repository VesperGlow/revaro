<script setup lang="ts">
import { computed, onBeforeUnmount, onMounted, ref } from 'vue'
import { RotateCcw, Upload, X } from 'lucide-vue-next'
import { formatSize } from '../format'
import type { UploadTask } from '../types'

const props=defineProps<{uploads:UploadTask[]}>()
defineEmits<{clear:[];cancel:[task:UploadTask];retry:[task:UploadTask]}>()

const activeUploads=computed(()=>props.uploads.filter(task=>task.status==='queued'||task.status==='uploading'))
const activeCount=computed(()=>activeUploads.value.length)
const clearableCount=computed(()=>props.uploads.filter(task=>task.status==='done'||task.status==='cancelled').length)
const progress=computed(()=>{
  const total=activeUploads.value.reduce((sum,task)=>sum+Math.max(1,task.file.size),0)
  if(!total)return 0
  return Math.round(activeUploads.value.reduce((sum,task)=>sum+Math.max(1,task.file.size)*task.progress/100,0)/total*100)
})
const circumference=100.53
const dashOffset=computed(()=>circumference*(1-progress.value/100))
const uploadStatus=(task:UploadTask)=>task.status==='queued'?'等待中':task.status==='uploading'?'正在上传':task.status==='done'?'已完成':task.status==='cancelled'?'已取消':task.error
const center=ref<HTMLDetailsElement|null>(null)
function closeFromOutside(event:PointerEvent){
  const target=event.target
  if(center.value?.open&&target instanceof Node&&!center.value.contains(target))center.value.open=false
}
function closeFromEscape(event:KeyboardEvent){
  if(event.key!=='Escape'||!center.value?.open)return
  center.value.open=false
  center.value.querySelector<HTMLElement>('summary')?.focus()
}
onMounted(()=>{document.addEventListener('pointerdown',closeFromOutside);document.addEventListener('keydown',closeFromEscape)})
onBeforeUnmount(()=>{document.removeEventListener('pointerdown',closeFromOutside);document.removeEventListener('keydown',closeFromEscape)})
</script>

<template>
  <details ref="center" class="transfer-center">
    <summary :title="activeCount?`${activeCount} 个上传任务进行中`:'上传进度'" aria-label="打开上传进度">
      <svg class="transfer-ring" viewBox="0 0 40 40" aria-hidden="true">
        <circle class="ring-track" cx="20" cy="20" r="16"/>
        <circle v-if="activeCount" class="ring-value" cx="20" cy="20" r="16" :style="{strokeDashoffset:dashOffset}"/>
        <g class="ring-upload">
          <path d="M20 27V13m0 0-4 4m4-4 4 4"/>
        </g>
      </svg>
      <span v-if="activeCount" class="transfer-count">{{ activeCount }}</span>
    </summary>
    <section class="transfer-popover">
      <header><div><strong>上传进度</strong><small>{{ activeCount?`${activeCount} 项进行中`:'最近上传' }}</small></div><button v-if="clearableCount" @click.prevent="$emit('clear')">清除已完成</button></header>
      <div v-if="!uploads.length" class="transfer-empty"><svg viewBox="0 0 40 40" aria-hidden="true"><path d="M20 28V12m0 0-4 4m4-4 4 4"/></svg><p>还没有上传任务</p></div>
      <div v-else class="transfer-list">
        <article v-for="task in uploads" :key="task.id">
          <span class="task-direction upload"><Upload aria-hidden="true" /></span><div><strong :title="task.relativePath||task.file.name">{{ task.relativePath||task.file.name }}</strong><small>{{ formatSize(task.file.size) }} · {{ uploadStatus(task) }}</small><i><b :class="task.status" :style="{width:`${task.progress}%`}"></b></i></div><em>{{ task.progress }}%</em>
          <button v-if="task.status==='queued'||task.status==='uploading'" title="取消上传" aria-label="取消上传" @click="$emit('cancel',task)"><X /></button><button v-else-if="task.status==='failed'" title="重试上传" aria-label="重试上传" @click="$emit('retry',task)"><RotateCcw /></button>
        </article>
      </div>
    </section>
  </details>
</template>

<style scoped>
.transfer-center{position:relative}.transfer-center summary{position:relative;display:grid;place-items:center;width:44px;height:44px;border-radius:50%;cursor:pointer;list-style:none}.transfer-center summary::-webkit-details-marker{display:none}.transfer-center summary:hover{background:#f1f5f9}
.transfer-ring{width:40px;height:40px}.ring-track,.ring-value{fill:none;stroke-width:2.6}.ring-track{stroke:#dce6ef}.ring-value{stroke:#22c55e;stroke-linecap:round;stroke-dasharray:100.53;transform:rotate(-90deg);transform-box:fill-box;transform-origin:center;transition:stroke-dashoffset .25s}.ring-upload{fill:none;stroke:#3d5f7e;stroke-width:1.9;stroke-linecap:round;stroke-linejoin:round}
.transfer-count{position:absolute;right:-2px;top:-1px;display:grid;place-items:center;min-width:17px;height:17px;padding:0 4px;border:2px solid #fff;border-radius:9px;background:#2563eb;color:#fff;font-size:9px;font-weight:800}
.transfer-popover{position:absolute;z-index:45;top:52px;right:-12px;width:min(410px,calc(100vw - 24px));max-height:58vh;overflow:hidden;border:1px solid #dfe6ee;border-radius:17px;background:#fff;box-shadow:0 24px 70px #0f172a2e}.transfer-popover:before{content:"";position:absolute;right:26px;top:-7px;width:12px;height:12px;border-left:1px solid #dfe6ee;border-top:1px solid #dfe6ee;background:#fff;transform:rotate(45deg)}
header{display:flex;align-items:center;justify-content:space-between;min-height:58px;padding:0 17px;border-bottom:1px solid #edf1f5}header div{display:flex;flex-direction:column;gap:3px}header strong{font-size:14px}header small{color:#94a3b8;font-size:10px}header button{border:0;background:transparent;color:#64748b;font-size:11px}
.transfer-list{max-height:calc(58vh - 58px);overflow:auto}.transfer-list article{display:grid;grid-template-columns:34px minmax(0,1fr) auto auto;align-items:center;gap:10px;padding:13px 15px;border-bottom:1px solid #eef2f6}.task-direction{display:grid;place-items:center;width:32px;height:32px;border-radius:10px;font-weight:850}.task-direction.upload{background:#eaf2ff;color:#2563eb}.transfer-list article>div{display:flex;min-width:0;flex-direction:column;gap:4px}.transfer-list strong,.transfer-list small{overflow:hidden;text-overflow:ellipsis;white-space:nowrap}.transfer-list strong{font-size:12px}.transfer-list small{color:#8795a8;font-size:10px}.transfer-list i{height:3px;overflow:hidden;border-radius:3px;background:#e8edf3}.transfer-list i b{display:block;height:100%;background:#3b82f6}.transfer-list i b.done{background:#22c55e}.transfer-list i b.failed{background:#ef4444}.transfer-list em{color:#64748b;font-size:10px;font-style:normal}.transfer-list article>button{border:0;background:transparent;color:#64748b;font-size:11px}
.transfer-empty{display:grid;place-items:center;padding:34px;color:#94a3b8}.transfer-empty svg{width:40px;height:40px;fill:none;stroke:#8fa6c2;stroke-width:1.8;stroke-linecap:round;stroke-linejoin:round}.transfer-empty p{margin:8px 0 0;font-size:12px}
@media(max-width:850px){.transfer-popover{position:fixed;top:66px;right:10px}.transfer-center summary{width:40px;height:40px}}
</style>

<script setup lang="ts">
import { computed, onBeforeUnmount, onMounted, ref } from 'vue'
import type { AudioMergeResponse } from '../types'

const props=defineProps<{jobs:AudioMergeResponse[]}>()
defineEmits<{cancel:[job:AudioMergeResponse];clear:[]}>()

const terminal=(status:AudioMergeResponse['status'])=>status==='done'||status==='failed'||status==='cancelled'
const activeJobs=computed(()=>props.jobs.filter(job=>!terminal(job.status)))
const clearable=computed(()=>props.jobs.some(job=>terminal(job.status)))
const progress=computed(()=>activeJobs.value.length?Math.round(activeJobs.value.reduce((sum,job)=>sum+job.progress,0)/activeJobs.value.length):0)
const circumference=100.53
const dashOffset=computed(()=>circumference*(1-progress.value/100))
const center=ref<HTMLDetailsElement|null>(null)
const statusText=(job:AudioMergeResponse)=>job.status==='uploading'?'正在上传素材':job.status==='queued'?'队列中':job.status==='preparing'?'准备源文件':job.status==='merging'?'正在合并':job.status==='saving'?'正在保存':job.status==='cancelling'?'正在取消':job.status==='done'?'已完成':job.status==='cancelled'?'已取消':job.error||'失败'

function closeFromOutside(event:PointerEvent){const target=event.target;if(center.value?.open&&target instanceof Node&&!center.value.contains(target))center.value.open=false}
function closeFromEscape(event:KeyboardEvent){if(event.key==='Escape'&&center.value?.open){center.value.open=false;center.value.querySelector<HTMLElement>('summary')?.focus()}}
onMounted(()=>{document.addEventListener('pointerdown',closeFromOutside);document.addEventListener('keydown',closeFromEscape)})
onBeforeUnmount(()=>{document.removeEventListener('pointerdown',closeFromOutside);document.removeEventListener('keydown',closeFromEscape)})
</script>

<template>
  <details ref="center" class="merge-center">
    <summary :title="activeJobs.length?`${activeJobs.length} 个音频合并任务进行中`:'音频合并任务'" aria-label="打开音频合并任务">
      <svg class="merge-ring" viewBox="0 0 40 40" aria-hidden="true"><circle class="ring-track" cx="20" cy="20" r="16"/><circle v-if="activeJobs.length" class="ring-value" cx="20" cy="20" r="16" :style="{strokeDashoffset:dashOffset}"/><path d="M11 21v-2m4 6V15m5 13V12m5 13V15m4 6v-2"/></svg>
      <span v-if="activeJobs.length" class="merge-count">{{ activeJobs.length }}</span>
    </summary>
    <section class="merge-popover">
      <header><div><strong>音频合并</strong><small>{{ activeJobs.length?`${activeJobs.length} 项后台运行`:'最近任务' }}</small></div><button v-if="clearable" @click.prevent="$emit('clear')">清除已完成</button></header>
      <div v-if="!jobs.length" class="merge-empty"><span>♬</span><p>还没有合并任务</p></div>
      <div v-else class="merge-list">
        <article v-for="job in jobs" :key="job.id">
          <span class="task-icon">≋</span><div><strong :title="job.output_name">{{ job.output_name }}</strong><small>{{ job.input_count }} 节 · {{ job.output_format.toUpperCase() }} · {{ statusText(job) }}</small><i><b :class="job.status" :style="{width:`${job.progress}%`}"></b></i></div><em>{{ job.progress }}%</em>
          <button v-if="!terminal(job.status)" title="取消合并" aria-label="取消合并" @click="$emit('cancel',job)">×</button>
        </article>
      </div>
    </section>
  </details>
</template>

<style scoped>
.merge-center{position:relative}.merge-center summary{position:relative;display:grid;place-items:center;width:44px;height:44px;border-radius:50%;cursor:pointer;list-style:none}.merge-center summary::-webkit-details-marker{display:none}.merge-center summary:hover{background:#f1f5f9}.merge-ring{width:40px;height:40px;fill:none;stroke:#3d5f7e;stroke-width:1.8;stroke-linecap:round;stroke-linejoin:round}.ring-track,.ring-value{stroke-width:2.6}.ring-track{stroke:#dce6ef}.ring-value{stroke:#8b5cf6;stroke-dasharray:100.53;transform:rotate(-90deg);transform-box:fill-box;transform-origin:center}.merge-ring>path{stroke-width:1.7}.merge-count{position:absolute;right:-2px;top:-1px;display:grid;place-items:center;min-width:17px;height:17px;padding:0 4px;border:2px solid #fff;border-radius:9px;background:#7c3aed;color:#fff;font-size:9px;font-weight:800}
.merge-popover{position:absolute;z-index:45;top:52px;right:-12px;width:min(430px,calc(100vw - 24px));max-height:58vh;overflow:hidden;border:1px solid #dfe6ee;border-radius:17px;background:#fff;box-shadow:0 24px 70px #0f172a2e}.merge-popover:before{content:"";position:absolute;right:26px;top:-7px;width:12px;height:12px;border-left:1px solid #dfe6ee;border-top:1px solid #dfe6ee;background:#fff;transform:rotate(45deg)}header{display:flex;align-items:center;justify-content:space-between;min-height:58px;padding:0 17px;border-bottom:1px solid #edf1f5}header div{display:flex;flex-direction:column;gap:3px}header strong{font-size:14px}header small{color:#94a3b8;font-size:10px}header button{border:0;background:transparent;color:#64748b;font-size:11px}.merge-list{max-height:calc(58vh - 58px);overflow:auto}.merge-list article{display:grid;grid-template-columns:34px minmax(0,1fr) auto auto;align-items:center;gap:10px;padding:13px 15px;border-bottom:1px solid #eef2f6}.task-icon{display:grid;place-items:center;width:32px;height:32px;border-radius:10px;background:#f0eaff;color:#7c3aed;font-weight:850}.merge-list article>div{display:flex;min-width:0;flex-direction:column;gap:4px}.merge-list strong,.merge-list small{overflow:hidden;text-overflow:ellipsis;white-space:nowrap}.merge-list strong{font-size:12px}.merge-list small{color:#8795a8;font-size:10px}.merge-list i{height:3px;overflow:hidden;border-radius:3px;background:#e8edf3}.merge-list i b{display:block;height:100%;background:#8b5cf6}.merge-list i b.done{background:#22c55e}.merge-list i b.failed{background:#ef4444}.merge-list i b.cancelled{background:#94a3b8}.merge-list em{color:#64748b;font-size:10px;font-style:normal}.merge-list article>button{border:0;background:transparent;color:#64748b;font-size:16px}.merge-empty{display:grid;place-items:center;padding:34px;color:#94a3b8}.merge-empty span{font-size:34px}.merge-empty p{margin:8px 0 0;font-size:12px}@media(max-width:850px){.merge-popover{position:fixed;top:66px;right:10px}.merge-center summary{width:40px;height:40px}}
</style>

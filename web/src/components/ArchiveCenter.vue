<script setup lang="ts">
import { computed, onBeforeUnmount, onMounted, reactive, ref } from 'vue'
import type { ArchiveJob } from '../types'

const props=defineProps<{jobs:ArchiveJob[]}>()
const emit=defineEmits<{clear:[];password:[job:ArchiveJob,password:string]}>()

const terminal=(status:ArchiveJob['status'])=>status==='done'||status==='failed'
const activeJobs=computed(()=>props.jobs.filter(job=>!terminal(job.status)&&job.status!=='needs_password'))
const passwordJobs=computed(()=>props.jobs.filter(job=>job.status==='needs_password'))
const clearable=computed(()=>props.jobs.some(job=>terminal(job.status)))
const progress=computed(()=>activeJobs.value.length?Math.round(activeJobs.value.reduce((sum,job)=>sum+job.progress,0)/activeJobs.value.length):0)
const circumference=100.53
const dashOffset=computed(()=>circumference*(1-progress.value/100))
const center=ref<HTMLDetailsElement|null>(null)
const passwords=reactive<Record<string,string>>({})
const statusText=(job:ArchiveJob)=>job.status==='queued'?'等待解压':job.status==='downloading'?'正在读取压缩包':job.status==='checking'?'正在检查内容':job.status==='extracting'?'正在解压':job.status==='importing'?'正在保存到网盘':job.status==='needs_password'?(job.error||'压缩包已加密，请输入密码'):job.status==='done'?'已完成':job.error||'解压失败：服务端未返回具体原因'
const detailText=(job:ArchiveJob)=>job.status==='failed'?statusText(job):(job.message||statusText(job))
function submitPassword(job:ArchiveJob){const password=passwords[job.id]||'';if(!password)return;emit('password',job,password);passwords[job.id]=''}

function closeFromOutside(event:PointerEvent){const target=event.target;if(center.value?.open&&target instanceof Node&&!center.value.contains(target))center.value.open=false}
function closeFromEscape(event:KeyboardEvent){if(event.key==='Escape'&&center.value?.open){center.value.open=false;center.value.querySelector<HTMLElement>('summary')?.focus()}}
onMounted(()=>{document.addEventListener('pointerdown',closeFromOutside);document.addEventListener('keydown',closeFromEscape)})
onBeforeUnmount(()=>{document.removeEventListener('pointerdown',closeFromOutside);document.removeEventListener('keydown',closeFromEscape)})
</script>

<template>
  <details ref="center" class="archive-center">
    <summary :title="passwordJobs.length?`${passwordJobs.length} 个压缩包等待密码`:activeJobs.length?`${activeJobs.length} 个在线解压任务进行中`:'在线解压任务'" aria-label="打开在线解压中心">
      <svg class="archive-ring" viewBox="0 0 40 40" aria-hidden="true"><circle class="ring-track" cx="20" cy="20" r="16"/><circle v-if="activeJobs.length" class="ring-value" cx="20" cy="20" r="16" :style="{strokeDashoffset:dashOffset}"/><path d="M13 11h14v7H13zM13 22h14v7H13zM20 11v18m-3-14h3m-3 7h3"/></svg>
      <span v-if="activeJobs.length||passwordJobs.length" class="archive-count" :class="{password:passwordJobs.length}">{{ passwordJobs.length||activeJobs.length }}</span>
    </summary>
    <section class="archive-popover">
      <header><div><strong>解压中心</strong><small>{{ activeJobs.length?`${activeJobs.length} 项后台运行`:'最近任务' }}</small></div><button v-if="clearable" @click.prevent="$emit('clear')">清除已完成</button></header>
      <div class="archive-list">
        <article v-for="job in jobs" :key="job.id">
          <span class="task-icon">▦</span><div><strong :title="job.name">{{ job.name }}</strong><small :class="{failed:job.status==='failed'||job.status==='needs_password'}" :title="detailText(job)">{{ detailText(job) }}</small><form v-if="job.status==='needs_password'" @submit.prevent="submitPassword(job)"><input v-model="passwords[job.id]" type="password" autocomplete="current-password" maxlength="1024" placeholder="输入压缩包密码" :aria-label="`${job.name} 的解压密码`"><button :disabled="!passwords[job.id]">继续解压</button></form><i><b :class="job.status" :style="{width:`${job.progress}%`}"></b></i></div><em>{{ job.status==='needs_password'?'待密码':`${job.progress}%` }}</em>
        </article>
      </div>
    </section>
  </details>
</template>

<style scoped>
.archive-center{position:relative}.archive-center summary{position:relative;display:grid;place-items:center;width:44px;height:44px;border-radius:50%;cursor:pointer;list-style:none}.archive-center summary::-webkit-details-marker{display:none}.archive-center summary:hover{background:#f1f5f9}.archive-ring{width:40px;height:40px;fill:none;stroke:#3d5f7e;stroke-width:1.7;stroke-linecap:round;stroke-linejoin:round}.ring-track,.ring-value{stroke-width:2.6}.ring-track{stroke:#dce6ef}.ring-value{stroke:#f59e0b;stroke-dasharray:100.53;transform:rotate(-90deg);transform-box:fill-box;transform-origin:center}.archive-ring>path{stroke-width:1.55}.archive-count{position:absolute;right:-2px;top:-1px;display:grid;place-items:center;min-width:17px;height:17px;padding:0 4px;border:2px solid #fff;border-radius:9px;background:#f59e0b;color:#fff;font-size:9px;font-weight:800}
.archive-popover{position:absolute;z-index:45;top:52px;right:-12px;width:min(430px,calc(100vw - 24px));max-height:58vh;overflow:hidden;border:1px solid #dfe6ee;border-radius:17px;background:#fff;box-shadow:0 24px 70px #0f172a2e}.archive-popover:before{content:"";position:absolute;right:26px;top:-7px;width:12px;height:12px;border-left:1px solid #dfe6ee;border-top:1px solid #dfe6ee;background:#fff;transform:rotate(45deg)}header{display:flex;align-items:center;justify-content:space-between;min-height:58px;padding:0 17px;border-bottom:1px solid #edf1f5}header div{display:flex;flex-direction:column;gap:3px}header strong{font-size:14px}header small{color:#94a3b8;font-size:10px}header button{border:0;background:transparent;color:#64748b;font-size:11px}.archive-list{max-height:calc(58vh - 58px);overflow:auto}.archive-list article{display:grid;grid-template-columns:34px minmax(0,1fr) auto;align-items:center;gap:10px;padding:13px 15px;border-bottom:1px solid #eef2f6}.task-icon{display:grid;place-items:center;width:32px;height:32px;border-radius:10px;background:#fff7e6;color:#d97706;font-weight:850}.archive-list article>div{display:flex;min-width:0;flex-direction:column;gap:4px}.archive-list strong,.archive-list small{overflow:hidden;text-overflow:ellipsis;white-space:nowrap}.archive-list strong{font-size:12px}.archive-list small{color:#8795a8;font-size:10px}.archive-list i{height:3px;overflow:hidden;border-radius:3px;background:#e8edf3}.archive-list i b{display:block;height:100%;background:#f59e0b}.archive-list i b.done{background:#22c55e}.archive-list i b.failed{background:#ef4444}.archive-list em{color:#64748b;font-size:10px;font-style:normal}@media(max-width:850px){.archive-popover{position:fixed;top:66px;right:10px}.archive-center summary{width:40px;height:40px}}
.archive-list small.failed{display:-webkit-box;overflow:hidden;color:#dc2626;line-height:1.45;white-space:normal;-webkit-box-orient:vertical;-webkit-line-clamp:3}
.archive-count.password{background:#dc2626}.archive-list form{display:flex;gap:6px}.archive-list form input{min-width:0;flex:1;height:30px;padding:0 9px;border:1px solid #d8e0e9;border-radius:8px;background:#fff;font-size:11px}.archive-list form button{flex:0 0 auto;padding:0 10px;border:0;border-radius:8px;background:#d97706;color:#fff;font-size:10px;font-weight:700}.archive-list form button:disabled{opacity:.45}.archive-list i b.needs_password{background:#dc2626}
</style>

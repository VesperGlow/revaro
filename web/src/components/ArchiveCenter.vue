<script setup lang="ts">
import { computed, nextTick, onBeforeUnmount, onMounted, ref } from 'vue'
import { Archive, X } from 'lucide-vue-next'
import type { ArchiveJob } from '../types'

const props=defineProps<{jobs:ArchiveJob[]}>()
const emit=defineEmits<{clear:[];password:[job:ArchiveJob,password:string]}>()

const terminal=(status:ArchiveJob['status'])=>status==='done'||status==='failed'
const activeJobs=computed(()=>props.jobs.filter(job=>!terminal(job.status)&&job.status!=='waiting_password'))
const passwordJobs=computed(()=>props.jobs.filter(job=>job.status==='waiting_password'))
const clearable=computed(()=>props.jobs.some(job=>terminal(job.status)))
const progress=computed(()=>activeJobs.value.length?Math.round(activeJobs.value.reduce((sum,job)=>sum+job.progress,0)/activeJobs.value.length):0)
const circumference=100.53
const dashOffset=computed(()=>circumference*(1-progress.value/100))
const center=ref<HTMLDetailsElement|null>(null)
const passwordJob=ref<ArchiveJob|null>(null)
const passwordValue=ref('')
const passwordInput=ref<HTMLInputElement|null>(null)
const statusText=(job:ArchiveJob)=>job.status==='queued'?'等待解压':job.status==='downloading'?'正在读取压缩包':job.status==='checking'?'正在检测加密与检查内容':job.status==='extracting'?'正在解压':job.status==='importing'?'正在保存到网盘':job.status==='waiting_password'?(job.error||'需要密码'):job.status==='done'?'已完成':job.error||'解压失败：服务端未返回具体原因'
const detailText=(job:ArchiveJob)=>job.status==='failed'?statusText(job):(job.message||statusText(job))
async function openPassword(job:ArchiveJob){passwordJob.value=job;passwordValue.value='';await nextTick();passwordInput.value?.focus()}
function closePassword(){passwordJob.value=null;passwordValue.value=''}
function submitPassword(){const job=passwordJob.value;const password=passwordValue.value;if(!job||!password)return;emit('password',job,password);closePassword()}

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
          <span class="task-icon"><Archive aria-hidden="true" /></span><div><strong :title="job.name">{{ job.name }}</strong><small :class="{failed:job.status==='failed'||job.status==='waiting_password'}" :title="detailText(job)">{{ detailText(job) }}</small><button v-if="job.status==='waiting_password'" class="password-button" @click="openPassword(job)">输入密码</button><i><b :class="job.status" :style="{width:`${job.progress}%`}"></b></i></div><em>{{ job.status==='waiting_password'?'待密码':`${job.progress}%` }}</em>
        </article>
      </div>
    </section>
  </details>
  <Teleport to="body"><div v-if="passwordJob" class="archive-password-backdrop" @click.self="closePassword"><form class="archive-password-dialog" @submit.prevent="submitPassword"><header><div><small>ENCRYPTED ARCHIVE</small><strong>输入解压密码</strong></div><button type="button" aria-label="关闭" @click="closePassword"><X /></button></header><p :title="passwordJob.name">{{ passwordJob.name }}</p><label>密码<input ref="passwordInput" v-model="passwordValue" type="password" autocomplete="off" maxlength="1024" placeholder="仅用于本次解压任务" required></label><small>密码只会用于当前任务，不会保存。如果密码错误，可以重新输入。</small><footer><button type="button" @click="closePassword">取消</button><button class="confirm" :disabled="!passwordValue">继续解压</button></footer></form></div></Teleport>
</template>

<style scoped>
.archive-center{position:relative}.archive-center summary{position:relative;display:grid;place-items:center;width:44px;height:44px;border-radius:50%;cursor:pointer;list-style:none}.archive-center summary::-webkit-details-marker{display:none}.archive-center summary:hover{background:#f1f5f9}.archive-ring{width:40px;height:40px;fill:none;stroke:#3d5f7e;stroke-width:1.7;stroke-linecap:round;stroke-linejoin:round}.ring-track,.ring-value{stroke-width:2.6}.ring-track{stroke:#dce6ef}.ring-value{stroke:#f59e0b;stroke-dasharray:100.53;transform:rotate(-90deg);transform-box:fill-box;transform-origin:center}.archive-ring>path{stroke-width:1.55}.archive-count{position:absolute;right:-2px;top:-1px;display:grid;place-items:center;min-width:17px;height:17px;padding:0 4px;border:2px solid #fff;border-radius:9px;background:#f59e0b;color:#fff;font-size:9px;font-weight:800}
.archive-popover{position:absolute;z-index:45;top:52px;right:-12px;width:min(430px,calc(100vw - 24px));max-height:58vh;overflow:hidden;border:1px solid #dfe6ee;border-radius:17px;background:#fff;box-shadow:0 24px 70px #0f172a2e}.archive-popover:before{content:"";position:absolute;right:26px;top:-7px;width:12px;height:12px;border-left:1px solid #dfe6ee;border-top:1px solid #dfe6ee;background:#fff;transform:rotate(45deg)}header{display:flex;align-items:center;justify-content:space-between;min-height:58px;padding:0 17px;border-bottom:1px solid #edf1f5}header div{display:flex;flex-direction:column;gap:3px}header strong{font-size:14px}header small{color:#94a3b8;font-size:10px}header button{border:0;background:transparent;color:#64748b;font-size:11px}.archive-list{max-height:calc(58vh - 58px);overflow:auto}.archive-list article{display:grid;grid-template-columns:34px minmax(0,1fr) auto;align-items:center;gap:10px;padding:13px 15px;border-bottom:1px solid #eef2f6}.task-icon{display:grid;place-items:center;width:32px;height:32px;border-radius:10px;background:#fff7e6;color:#d97706;font-weight:850}.archive-list article>div{display:flex;min-width:0;flex-direction:column;gap:4px}.archive-list strong,.archive-list small{overflow:hidden;text-overflow:ellipsis;white-space:nowrap}.archive-list strong{font-size:12px}.archive-list small{color:#8795a8;font-size:10px}.archive-list i{height:3px;overflow:hidden;border-radius:3px;background:#e8edf3}.archive-list i b{display:block;height:100%;background:#f59e0b}.archive-list i b.done{background:#22c55e}.archive-list i b.failed{background:#ef4444}.archive-list em{color:#64748b;font-size:10px;font-style:normal}@media(max-width:850px){.archive-popover{position:fixed;top:66px;right:10px}.archive-center summary{width:40px;height:40px}}
.archive-list small.failed{display:-webkit-box;overflow:hidden;color:#dc2626;line-height:1.45;white-space:normal;-webkit-box-orient:vertical;-webkit-line-clamp:3}
.archive-count.password{background:#dc2626}.password-button{align-self:flex-start;min-height:29px;padding:0 11px;border:0;border-radius:8px;background:#dc2626;color:#fff;font-size:10px;font-weight:750}.password-button:hover{background:#b91c1c}.archive-list i b.waiting_password{background:#dc2626}
.archive-password-backdrop{position:fixed;z-index:100;inset:0;display:grid;place-items:center;padding:max(18px,env(safe-area-inset-top,0px)) max(18px,env(safe-area-inset-right,0px)) max(18px,env(safe-area-inset-bottom,0px)) max(18px,env(safe-area-inset-left,0px));background:#0f172a70;backdrop-filter:blur(5px)}.archive-password-dialog{width:min(420px,100%);padding:22px;border-radius:17px;background:#fff;box-shadow:0 28px 80px #0f172a52}.archive-password-dialog header{min-height:0;padding:0 0 16px}.archive-password-dialog header div{gap:5px}.archive-password-dialog header small{color:#dc2626;font-size:9px;font-weight:850;letter-spacing:.12em}.archive-password-dialog header strong{font-size:19px}.archive-password-dialog header button{width:36px;height:36px;border-radius:10px;background:#f1f5f9;font-size:22px}.archive-password-dialog>p{overflow:hidden;margin:0 0 17px;padding:10px 12px;border-radius:9px;background:#f8fafc;color:#64748b;font-size:11px;text-overflow:ellipsis;white-space:nowrap}.archive-password-dialog label{display:flex;color:#475569;font-size:12px;font-weight:700;flex-direction:column;gap:7px}.archive-password-dialog input{width:100%;min-height:44px;padding:0 12px;border:1px solid #d7dee8;border-radius:10px;outline:none}.archive-password-dialog input:focus{border-color:#f87171;box-shadow:0 0 0 3px #f8717124}.archive-password-dialog>small{display:block;margin-top:9px;color:#94a3b8;font-size:10px;line-height:1.55}.archive-password-dialog footer{display:flex;justify-content:flex-end;gap:9px;margin-top:21px}.archive-password-dialog footer button{min-height:40px;padding:0 15px;border:1px solid #dce2e9;border-radius:10px;background:#fff;color:#475569;font-weight:700}.archive-password-dialog footer .confirm{border-color:#dc2626;background:#dc2626;color:#fff}.archive-password-dialog footer .confirm:disabled{opacity:.45}
@media(max-width:850px){.archive-popover{top:calc(66px + env(safe-area-inset-top,0px));right:max(10px,env(safe-area-inset-right,0px));left:auto}}
</style>

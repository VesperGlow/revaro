<script setup lang="ts">
import { computed, onBeforeUnmount, onMounted, ref } from 'vue'
import { Activity, KeyRound, Pause, Play, Plus, RotateCcw, Square } from 'lucide-vue-next'
import { api } from '../api'
import { formatSize } from '../format'
import type { BackgroundTask } from '../types'
import DownloadCreateDialog from './DownloadCreateDialog.vue'

const props=defineProps<{tasks:BackgroundTask[];parentId:string}>()
const emit=defineEmits<{changed:[];cancel:[task:BackgroundTask];retry:[task:BackgroundTask]}>()
const center=ref<HTMLDetailsElement|null>(null)
const download=ref<InstanceType<typeof DownloadCreateDialog>|null>(null)
const passwordTask=ref<BackgroundTask|null>(null)
const password=ref('')
const error=ref('')
const active=computed(()=>props.tasks.filter(t=>!['completed','failed','cancelled'].includes(t.status)))
const terminal=computed(()=>props.tasks.filter(t=>['completed','failed','cancelled'].includes(t.status)))
const progress=computed(()=>active.value.length?Math.round(active.value.reduce((n,t)=>n+t.progress,0)/active.value.length):0)
const labels:Record<string,string>={upload:'上传',bt:'BT 下载',url_download:'直链下载',archive_extract:'解压',audio_merge:'音频合并',video_hls:'视频 HLS',audio_hls:'音频 HLS',video_fmp4:'视频转换',subtitle:'字幕处理'}
const status=(task:BackgroundTask)=>task.status==='waiting_input'?(task.type==='archive_extract'?'等待输入密码':'等待输入'):task.status==='retrying'?'等待重试':task.status==='queued'?'排队中':task.status==='running'?task.phase:task.status==='completed'?'已完成':task.status==='cancelled'?'已取消':task.error||'失败'
function closeCenter(){if(center.value)center.value.open=false}
function openDownload(){closeCenter();download.value?.openCreate()}
function openTask(task:BackgroundTask){if(task.type==='bt'&&['metadata','waiting'].includes(task.phase)){closeCenter();download.value?.openById(task.source_id||task.id)}else if(task.status==='waiting_input'&&task.type==='archive_extract'){closeCenter();passwordTask.value=task;password.value='';error.value=''}}
async function submitPassword(){if(!passwordTask.value||!password.value)return;try{await api(`/api/tasks/${passwordTask.value.id}/input`,{method:'POST',body:JSON.stringify({password:password.value})});passwordTask.value=null;password.value='';emit('changed')}catch(e){error.value=(e as Error).message}}
async function clearFinished(){await Promise.all(terminal.value.map(task=>api(`/api/tasks/${task.id}`,{method:'DELETE'}).catch(()=>undefined)));emit('changed')}
async function downloadAction(task:BackgroundTask,action:'pause'|'resume'){try{await api(`/api/downloads/${task.source_id}/${action}`,{method:'POST'});emit('changed')}catch(e){error.value=(e as Error).message}}
function closeFromOutside(event:PointerEvent){const target=event.target;if(center.value?.open&&target instanceof Node&&!center.value.contains(target))closeCenter()}
function closeFromEscape(event:KeyboardEvent){if(event.key!=='Escape')return;if(passwordTask.value){passwordTask.value=null;return}if(center.value?.open){closeCenter();center.value.querySelector<HTMLElement>('summary')?.focus()}}
onMounted(()=>{document.addEventListener('pointerdown',closeFromOutside);document.addEventListener('keydown',closeFromEscape)})
onBeforeUnmount(()=>{document.removeEventListener('pointerdown',closeFromOutside);document.removeEventListener('keydown',closeFromEscape)})
</script>

<template>
  <details ref="center" class="task-center">
    <summary title="任务中心"><Activity/><span v-if="active.length">{{ active.length }}</span></summary>
    <section class="task-panel">
      <header><div><strong>任务中心</strong><small>{{ active.length?`${active.length} 项运行中 · ${progress}%`:'最近任务' }}</small></div><span><button v-if="terminal.length" @click.prevent.stop="clearFinished">清除完成</button><button @click.prevent.stop="openDownload"><Plus/>新建下载</button></span></header>
      <p v-if="!tasks.length" class="empty">还没有后台任务</p>
      <div v-else class="task-list">
        <article v-for="task in tasks" :key="task.id" @click="openTask(task)">
          <span class="kind">{{ labels[task.type]||task.type }}</span><div><strong :title="task.name">{{ task.name||labels[task.type]||task.id }}</strong><small>{{ status(task) }}<template v-if="task.speed"> · {{ formatSize(task.speed) }}/s</template></small><i><b :class="task.status" :style="{width:`${task.progress}%`}"></b></i></div><em>{{ Math.round(task.progress) }}%</em>
          <span class="actions"><button v-if="['bt','url_download'].includes(task.type)&&['queued','downloading'].includes(task.phase)" title="暂停" @click.stop="downloadAction(task,'pause')"><Pause/></button><button v-else-if="['bt','url_download'].includes(task.type)&&task.phase==='paused'" title="继续" @click.stop="downloadAction(task,'resume')"><Play/></button><button v-if="!['completed','failed','cancelled'].includes(task.status)" title="取消" @click.stop="$emit('cancel',task)"><Square/></button><button v-if="task.status==='failed'&&task.retry_count<task.max_retries" title="重试" @click.stop="$emit('retry',task)"><RotateCcw/></button><button v-if="task.status==='waiting_input'&&task.type==='archive_extract'" title="输入密码" @click.stop="openTask(task)"><KeyRound/></button></span>
        </article>
      </div>
      <p v-if="error" class="error">{{ error }}</p>
    </section>
  </details>
  <DownloadCreateDialog ref="download" :parent-id="parentId" @changed="$emit('changed')" />
  <Teleport to="body"><div v-if="passwordTask" class="input-backdrop" @pointerdown.self="passwordTask=null"><form class="input-dialog" @pointerdown.stop @submit.prevent="submitPassword"><strong>输入压缩包密码</strong><small>{{ passwordTask.name }}</small><input v-model="password" type="password" maxlength="1024" autofocus><p v-if="error">{{ error }}</p><footer><button type="button" @click="passwordTask=null">取消</button><button :disabled="!password">继续任务</button></footer></form></div></Teleport>
</template>

<style scoped>
.task-center{position:relative}.task-center>summary{position:relative;display:grid;place-items:center;width:44px;height:44px;border-radius:50%;cursor:pointer;list-style:none}.task-center>summary::-webkit-details-marker{display:none}.task-center>summary:hover{background:#f1f5f9}.task-center>summary svg{width:21px;color:#3d5f7e}.task-center>summary span{position:absolute;right:-2px;top:-1px;display:grid;place-items:center;min-width:17px;height:17px;padding:0 4px;border:2px solid #fff;border-radius:9px;background:#2563eb;color:#fff;font-size:9px;font-weight:800}.task-panel{position:absolute;z-index:45;top:52px;right:-12px;width:min(470px,calc(100vw - 24px));max-height:65vh;overflow:hidden;border:1px solid #dfe6ee;border-radius:17px;background:#fff;box-shadow:0 24px 70px #0f172a2e}.task-panel header{display:flex;align-items:center;justify-content:space-between;min-height:58px;padding:0 17px;border-bottom:1px solid #edf1f5}.task-panel header div{display:flex;flex-direction:column}.task-panel header>span{display:flex;gap:5px}.task-panel header small{color:#94a3b8;font-size:10px}.task-panel header button{display:flex;align-items:center;gap:4px;border:0;border-radius:9px;background:#e0f2fe;color:#0369a1;padding:7px 10px;font-weight:700}.task-panel header svg{width:14px}.task-list{max-height:calc(65vh - 58px);overflow:auto}.task-list article{display:grid;grid-template-columns:70px minmax(0,1fr) auto auto;align-items:center;gap:9px;padding:12px 14px;border-bottom:1px solid #eef2f6;cursor:default}.kind{color:#2563eb;font-size:10px;font-weight:750}.task-list article>div{display:flex;min-width:0;flex-direction:column;gap:4px}.task-list strong,.task-list small{overflow:hidden;text-overflow:ellipsis;white-space:nowrap}.task-list strong{font-size:12px}.task-list small{color:#8795a8;font-size:10px}.task-list i{height:3px;overflow:hidden;border-radius:3px;background:#e8edf3}.task-list i b{display:block;height:100%;background:#3b82f6}.task-list i b.completed{background:#22c55e}.task-list i b.failed{background:#ef4444}.task-list em{color:#64748b;font-size:10px;font-style:normal}.actions{display:flex}.actions button{border:0;background:transparent;color:#64748b}.actions svg{width:15px}.empty,.error{padding:25px;text-align:center;color:#94a3b8}.error{color:#b91c1c}.input-backdrop{position:fixed;z-index:140;inset:0;display:grid;place-items:center;background:#0f172a80}.input-dialog{display:flex;width:min(400px,calc(100vw - 30px));flex-direction:column;gap:10px;padding:22px;border-radius:18px;background:#fff}.input-dialog small{color:#64748b}.input-dialog input{padding:11px;border:1px solid #d8e1ea;border-radius:9px}.input-dialog p{color:#b91c1c}.input-dialog footer{display:flex;justify-content:flex-end;gap:8px}.input-dialog button{padding:8px 12px;border-radius:9px;border:1px solid #d8e1ea;background:#fff}.input-dialog button:last-child{border:0;background:#1677b8;color:#fff}@media(max-width:850px){.task-panel{position:fixed;top:66px;right:10px}.task-list article{grid-template-columns:62px minmax(0,1fr) auto}}
</style>

<script setup lang="ts">
import { computed, onBeforeUnmount, onMounted, ref, watch } from 'vue'
import { ChevronRight, CirclePlay, Download, FolderOpen, Pause, Play, Plus, RotateCcw, Trash2, X } from 'lucide-vue-next'
import { api } from '../api'
import type { DriveFile } from '../api'
import { formatSize } from '../format'
import type { DownloadJob } from '../types'
import AppDialog from './AppDialog.vue'
import DirectoryPickerModal from './DirectoryPickerModal.vue'

const ROOT='00000000-0000-0000-0000-000000000000'

const props=defineProps<{jobs:DownloadJob[];parentId:string}>()
const emit=defineEmits<{changed:[]}>()

const terminal=(job:DownloadJob)=>job.status==='done'||job.status==='failed'||job.status==='cancelled'
const activeJobs=computed(()=>props.jobs.filter(job=>!terminal(job)))
// Import is a multi-phase operation. imported_size is only authoritative once
// the data plane has returned its verified upload result, so map the observable
// phases onto the final quarter instead of falsely dropping a fully downloaded
// torrent back to 0%.
const phaseCompleted=(job:DownloadJob)=>{
  if(job.status!=='importing')return job.completed_size
  const total=Math.max(1,job.selected_size)
  if(job.ingest_state==='probing')return Math.round(total*.76)
  if(job.ingest_state==='processing')return Math.round(total*.85)
  if(job.ingest_state==='uploading')return Math.max(Math.round(total*.95),job.imported_size)
  if(job.ingest_state==='completed')return total
  return Math.max(job.completed_size,job.imported_size)
}
const overallProgress=computed(()=>{
  const total=activeJobs.value.reduce((sum,job)=>sum+Math.max(1,job.selected_size),0)
  return total?Math.round(activeJobs.value.reduce((sum,job)=>sum+Math.min(phaseCompleted(job),Math.max(1,job.selected_size)),0)/total*100):0
})
const circumference=100.53
const dashOffset=computed(()=>circumference*(1-overallProgress.value/100))
const center=ref<HTMLDetailsElement|null>(null)
const modalOpen=ref(false)
const mode=ref<'magnet'|'torrent'|'url'>('magnet')
const magnet=ref('')
const directURL=ref('')
const torrentFile=ref<File|null>(null)
const detail=ref<DownloadJob|null>(null)
const selected=ref<Set<number>>(new Set())
const selectionJobId=ref('')
const busy=ref(false)
const error=ref('')
const streamingIndex=ref<number|null>(null)
const destinationId=ref(props.parentId)
const destinationName=ref('当前文件夹')
const pickerFor=ref<null|'create'|'retry'>(null)
const retryJob=ref<DownloadJob|null>(null)
const retryDestinationId=ref(ROOT)
const retryDestinationName=ref('我的文件')
const deleteJob=ref<DownloadJob|null>(null)
let metadataTimer=0

const selectedFiles=computed(()=>detail.value?.files?.filter(file=>selected.value.has(file.index))||[])
const selectedSize=computed(()=>selectedFiles.value.reduce((sum,file)=>sum+file.size,0))
const streamFile=computed(()=>detail.value?.files?.find(file=>file.index===streamingIndex.value)||null)
const streamURL=computed(()=>detail.value&&streamFile.value?`/api/downloads/${encodeURIComponent(detail.value.id)}/files/${streamFile.value.index}/stream`:"")
const playable=(path:string)=>/\.(mp4|m4v|webm|mkv|mov|avi|mp3|m4a|aac|ogg|opus|wav|flac)$/i.test(path)
const audioOnly=(path:string)=>/\.(mp3|m4a|aac|ogg|opus|wav|flac)$/i.test(path)
const progress=(job:DownloadJob)=>job.status==='done'?100:job.selected_size?Math.min(100,Math.round(phaseCompleted(job)/job.selected_size*100)):0
const statusText=(job:DownloadJob)=>job.status==='metadata'?'正在获取元数据':job.status==='waiting'?'等待选择文件':job.status==='queued'?'准备下载':job.status==='downloading'?(job.source_type==='url'?`${formatSize(job.completed_size)}${job.selected_size?` / ${formatSize(job.selected_size)}`:''}${job.download_speed?` · ${formatSize(job.download_speed)}/s`:''}`:`${formatSize(job.download_speed)}/s · ${job.peers} 个节点`):job.status==='paused'?'已暂停':job.status==='importing'?(job.ingest_state==='probing'?'正在分析媒体':job.ingest_state==='processing'?'正在准备 Web 播放文件':job.ingest_state==='uploading'?'正在上传并验证播放文件':`正在导入${job.current_file?` ${job.current_file.split('/').pop()}`:''} · ${formatSize(job.imported_size)} / ${formatSize(job.selected_size)}${job.import_speed?` · ${formatSize(job.import_speed)}/s`:''}`):job.status==='done'?'已完成':job.status==='cancelled'?'已取消':job.error||'失败'

function closeFromOutside(event:PointerEvent){const target=event.target;if(center.value?.open&&target instanceof Node&&!center.value.contains(target))center.value.open=false}
function closeFromEscape(event:KeyboardEvent){if(event.key==='Escape'){if(modalOpen.value)closeModal();else if(center.value?.open){center.value.open=false;center.value.querySelector<HTMLElement>('summary')?.focus()}}}
function resetForm(){mode.value='magnet';magnet.value='';directURL.value='';torrentFile.value=null;detail.value=null;streamingIndex.value=null;selected.value=new Set();selectionJobId.value='';error.value='';busy.value=false;destinationId.value=props.parentId;destinationName.value=props.parentId===ROOT?'我的文件':'当前文件夹'}
function openCreate(){resetForm();modalOpen.value=true;if(center.value)center.value.open=false}
function closeModal(){if(busy.value)return;streamingIndex.value=null;modalOpen.value=false;window.clearTimeout(metadataTimer)}
function pickTorrent(event:Event){torrentFile.value=(event.target as HTMLInputElement).files?.[0]||null;error.value=''}
function fileBase64(file:File){return new Promise<string>((resolve,reject)=>{const reader=new FileReader();reader.onerror=()=>reject(new Error('无法读取 .torrent 文件'));reader.onload=()=>{const bytes=new Uint8Array(reader.result as ArrayBuffer);let binary='';for(let i=0;i<bytes.length;i+=32768)binary+=String.fromCharCode(...bytes.subarray(i,i+32768));resolve(btoa(binary))};reader.readAsArrayBuffer(file)})}
function initializeSelection(job:DownloadJob){if(job.status!=='waiting'||selectionJobId.value===job.id)return;selected.value=new Set((job.files||[]).map(file=>file.index));selectionJobId.value=job.id}
async function loadDetail(id:string){
  try{const job=await api<DownloadJob>(`/api/downloads/${id}`);detail.value=job;initializeSelection(job);emit('changed');if(job.status==='metadata')metadataTimer=window.setTimeout(()=>void loadDetail(id),800)}
  catch(e){error.value=(e as Error).message}
}
async function createTask(){
  error.value=''
  if(mode.value==='magnet'&&!magnet.value.trim()){error.value='请粘贴磁力链接';return}
  if(mode.value==='torrent'&&!torrentFile.value){error.value='请选择 .torrent 文件';return}
  if(mode.value==='url'&&!/^https?:\/\//i.test(directURL.value.trim())){error.value='请输入完整的 HTTP 或 HTTPS 下载链接';return}
  busy.value=true
  try{
    const body:Record<string,string>={parent_id:destinationId.value}
    if(mode.value==='magnet')body.magnet=magnet.value.trim()
    else if(mode.value==='torrent')body.torrent_base64=await fileBase64(torrentFile.value!)
    else body.url=directURL.value.trim()
    const job=await api<DownloadJob>('/api/downloads',{method:'POST',body:JSON.stringify(body)})
    detail.value=job;initializeSelection(job);emit('changed')
    if(job.source_type==='url')modalOpen.value=false
    else if(job.status==='metadata')metadataTimer=window.setTimeout(()=>void loadDetail(job.id),500)
  }catch(e){error.value=(e as Error).message}
  finally{busy.value=false}
}
async function openJob(job:DownloadJob){modalOpen.value=true;detail.value=null;error.value='';busy.value=true;selectionJobId.value='';try{await loadDetail(job.id)}finally{busy.value=false};if(center.value)center.value.open=false}
async function startTask(){if(!detail.value||!selected.value.size)return;busy.value=true;error.value='';try{await api(`/api/downloads/${detail.value.id}/start`,{method:'POST',body:JSON.stringify({file_indices:[...selected.value]})});modalOpen.value=false;emit('changed')}catch(e){error.value=(e as Error).message}finally{busy.value=false}}
async function taskAction(job:DownloadJob,action:'pause'|'resume'){try{await api(`/api/downloads/${job.id}/${action}`,{method:'POST'});emit('changed')}catch(e){error.value=(e as Error).message}}
async function openRetry(job:DownloadJob){retryJob.value=job;retryDestinationId.value=job.parent_id;retryDestinationName.value=job.parent_id===ROOT?'我的文件':'原保存目录';error.value='';if(center.value)center.value.open=false;try{const meta=await api<{file:DriveFile}>(`/api/files/${job.parent_id}`);if(retryJob.value?.id===job.id)retryDestinationName.value=meta.file.name}catch{/* 提交重试时由服务端校验已删除的原目录 */}}
async function submitRetry(){if(!retryJob.value)return;busy.value=true;error.value='';try{await api(`/api/downloads/${retryJob.value.id}/resume`,{method:'POST',body:JSON.stringify({parent_id:retryDestinationId.value})});retryJob.value=null;emit('changed')}catch(e){error.value=(e as Error).message}finally{busy.value=false}}
async function removeTask(){const job=deleteJob.value;if(!job)return;busy.value=true;try{await api(`/api/downloads/${job.id}`,{method:'DELETE'},30*60*1000);deleteJob.value=null;emit('changed')}catch(e){error.value=(e as Error).message}finally{busy.value=false}}
function folderSelected(id:string,name:string){if(pickerFor.value==='retry'){retryDestinationId.value=id;retryDestinationName.value=name}else{destinationId.value=id;destinationName.value=name}pickerFor.value=null}
function toggleAll(){const files=detail.value?.files||[];selected.value=selected.value.size===files.length?new Set():new Set(files.map(file=>file.index))}
function toggleFile(index:number){const next=new Set(selected.value);if(next.has(index))next.delete(index);else next.add(index);selected.value=next}

onMounted(()=>{document.addEventListener('pointerdown',closeFromOutside);document.addEventListener('keydown',closeFromEscape)})
onBeforeUnmount(()=>{document.removeEventListener('pointerdown',closeFromOutside);document.removeEventListener('keydown',closeFromEscape);window.clearTimeout(metadataTimer)})
watch(()=>props.jobs.length,(length,previous)=>{if(!length&&previous&&center.value)center.value.open=false})
</script>

<template>
  <details ref="center" class="download-center">
    <summary :title="activeJobs.length?`${activeJobs.length} 个离线下载任务`:'离线下载'" aria-label="打开离线下载">
      <svg class="download-ring" viewBox="0 0 40 40" aria-hidden="true"><circle class="ring-track" cx="20" cy="20" r="16"/><circle v-if="activeJobs.length" class="ring-value" cx="20" cy="20" r="16" :style="{strokeDashoffset:dashOffset}"/><path d="M20 11v15m0 0-5-5m5 5 5-5M12 30h16"/></svg>
      <span v-if="activeJobs.length" class="download-count">{{ activeJobs.length }}</span>
    </summary>
    <section class="download-popover">
      <header><div><strong>离线下载</strong><small>{{ activeJobs.length?`${activeJobs.length} 项后台运行`:'磁力、BT 与直链' }}</small></div><button class="add" @click.prevent="openCreate"><Plus />新建</button></header>
      <div v-if="!jobs.length" class="download-empty"><Download aria-hidden="true" /><p>还没有离线下载</p><button @click="openCreate">添加磁力链接</button></div>
      <div v-else class="download-list">
        <article v-for="job in jobs" :key="job.id">
          <button class="task-main" @click="job.source_type!=='url'&&['waiting','queued','downloading','paused','importing'].includes(job.status)&&openJob(job)"><span class="task-icon"><Download /></span><span class="task-copy"><strong :title="job.name">{{ job.name||'获取种子元数据…' }}</strong><small>{{ job.selected_size||job.completed_size?`${formatSize(job.selected_size||job.completed_size)} · `:'' }}{{ statusText(job) }}</small><i><b :class="job.status" :style="{width:`${progress(job)}%`}"></b></i></span><em>{{ job.selected_size||job.status==='done'?`${progress(job)}%`:'—' }}</em></button>
          <span class="task-actions"><button v-if="job.status==='downloading'||job.status==='queued'" title="暂停" aria-label="暂停" @click="taskAction(job,'pause')"><Pause /></button><button v-else-if="job.status==='paused'" title="继续" aria-label="继续" @click="taskAction(job,'resume')"><Play /></button><button v-else-if="job.status==='failed'&&job.source_type!=='url'" title="重试任务" aria-label="重试任务" @click="openRetry(job)"><RotateCcw /></button><button v-else-if="job.status==='waiting'" title="选择文件" @click="openJob(job)">选择</button><button title="删除任务" aria-label="删除任务" @click="deleteJob=job"><Trash2 /></button></span>
        </article>
      </div>
      <p v-if="error&&!modalOpen&&!retryJob" class="popover-error">{{ error }}</p>
    </section>
  </details>

  <Teleport to="body">
    <div v-if="modalOpen" class="download-backdrop" @mousedown.self="closeModal">
      <section class="download-dialog" role="dialog" aria-modal="true" aria-label="新建离线下载">
        <header><div><strong>{{ detail?'选择下载文件':'新建离线下载' }}</strong><small>保存到网盘目录 · 完成后不会做种</small></div><button aria-label="关闭" @click="closeModal"><X /></button></header>
        <template v-if="!detail">
          <div class="source-tabs"><button :class="{active:mode==='magnet'}" @click="mode='magnet'">磁力链接</button><button :class="{active:mode==='torrent'}" @click="mode='torrent'">.torrent 文件</button><button :class="{active:mode==='url'}" @click="mode='url'">直链下载</button></div>
          <label v-if="mode==='magnet'" class="source-field"><span>磁力链接</span><textarea v-model="magnet" rows="5" maxlength="16384" placeholder="magnet:?xt=urn:btih:…"></textarea></label>
          <label v-else-if="mode==='torrent'" class="torrent-picker"><input type="file" accept=".torrent,application/x-bittorrent" @change="pickTorrent"><span>{{ torrentFile?.name||'选择 .torrent 文件' }}</span><small>最大 4 MiB</small></label>
          <label v-else class="source-field"><span>HTTP / HTTPS 下载链接</span><textarea v-model="directURL" rows="4" maxlength="16384" placeholder="https://example.com/video.mkv"></textarea></label>
          <div class="destination-field"><span>保存到</span><button type="button" :title="destinationName" @click="pickerFor='create'"><FolderOpen /><b>{{ destinationName }}</b><ChevronRight /></button><small>下载完成后会按种子目录结构导入这里</small></div>
          <p class="privacy-note">{{ mode==='url'?'由服务器流式下载并写入对象存储；内网、本机地址和不安全重定向会被拦截。':'只连接公网节点、Tracker 与 WebSeed；内网和本机地址会被拦截。' }}</p>
          <p v-if="error" class="download-error">{{ error }}</p>
          <footer><button class="secondary" @click="closeModal">取消</button><button class="primary" :disabled="busy" @click="createTask">{{ busy?'正在创建…':mode==='url'?'开始下载':'解析种子' }}</button></footer>
        </template>
        <template v-else-if="detail.status==='metadata'">
          <div class="metadata-wait"><span class="download-spinner"></span><strong>正在获取种子元数据</strong><p>磁力链接需要先从公网节点取得文件列表。</p></div>
        </template>
        <template v-else-if="detail.status==='waiting'">
          <div class="torrent-summary"><div><strong>{{ detail.name }}</strong><small>{{ detail.files?.length||0 }} 个文件 · 已选 {{ formatSize(selectedSize) }}</small></div><button @click="toggleAll">{{ selected.size===(detail.files?.length||0)?'全不选':'全选' }}</button></div>
          <div class="torrent-files"><label v-for="file in detail.files" :key="file.index"><input type="checkbox" :checked="selected.has(file.index)" @change="toggleFile(file.index)"><span :title="file.path">{{ file.path }}</span><small>{{ formatSize(file.size) }}</small></label></div>
          <p v-if="error" class="download-error">{{ error }}</p>
          <footer><button class="secondary" @click="closeModal">稍后选择</button><button class="primary" :disabled="busy||!selected.size" @click="startTask">{{ busy?'正在启动…':`下载 ${selected.size} 个文件` }}</button></footer>
        </template>
        <template v-else-if="detail.source_type!=='url'&&['queued','downloading','paused','importing'].includes(detail.status)">
          <div class="torrent-summary"><div><strong>{{ detail.name }}</strong><small>{{ statusText(detail) }} · 拖动进度条可优先下载目标分片</small></div></div>
          <div class="torrent-files stream-files"><div v-for="file in detail.files?.filter(item=>item.selected)" :key="file.index"><span :title="file.path">{{ file.path }}</span><small>{{ formatSize(file.size) }}</small><button v-if="playable(file.path)" title="边下边播" @click="streamingIndex=file.index"><CirclePlay />播放</button></div></div>
          <div v-if="streamFile" class="torrent-player"><strong :title="streamFile.path">{{ streamFile.path }}</strong><audio v-if="audioOnly(streamFile.path)" :key="streamURL" :src="streamURL" controls autoplay preload="metadata"></audio><video v-else :key="streamURL" :src="streamURL" controls autoplay preload="metadata"></video></div>
          <p v-if="error" class="download-error">{{ error }}</p>
          <footer><button class="primary" @click="closeModal">关闭</button></footer>
        </template>
        <template v-else>
          <div class="metadata-wait"><strong>{{ detail.status==='failed'?'任务失败':'任务已经开始' }}</strong><p>{{ detail.error||statusText(detail) }}</p></div><footer><button class="primary" @click="closeModal">关闭</button></footer>
        </template>
      </section>
    </div>
    <section v-if="retryJob" class="download-backdrop" @mousedown.self="!busy&&(retryJob=null)"><div class="retry-dialog" role="dialog" aria-modal="true" aria-labelledby="retry-title"><header><div><strong id="retry-title">重试下载任务</strong><small>沿用原任务参数，可重新选择保存位置</small></div><button aria-label="关闭" :disabled="busy" @click="retryJob=null"><X /></button></header><div class="retry-content"><dl><div><dt>任务名称</dt><dd :title="retryJob.name">{{ retryJob.name||'未命名任务' }}</dd></div><div><dt>失败原因</dt><dd class="failure" :title="retryJob.error">{{ retryJob.error||'服务端未返回失败原因' }}</dd></div></dl><div class="destination-field"><span>保存到</span><button type="button" :title="retryDestinationName" @click="pickerFor='retry'"><FolderOpen /><b>{{ retryDestinationName }}</b><ChevronRight /></button><small>默认继承原任务保存目录</small></div><p v-if="error" class="download-error">{{ error }}</p></div><footer><button class="secondary" :disabled="busy" @click="retryJob=null">取消</button><button class="primary" :disabled="busy" @click="submitRetry">{{ busy?'正在重试…':'重试任务' }}</button></footer></div></section>
    <DirectoryPickerModal v-if="pickerFor" :initial-id="pickerFor==='retry'?retryDestinationId:destinationId" :title="pickerFor==='retry'?'选择重试保存目录':'选择保存目录'" description="当前目录可以直接作为目标" @cancel="pickerFor=null" @select="folderSelected" />
    <AppDialog v-if="deleteJob" title="删除离线下载任务？" :message="`“${deleteJob.name||'获取元数据中'}”\n${deleteJob.status==='done'?'网盘中已导入的文件不会被删除。':'已下载的临时分片会一并清理。'}`" confirm-label="删除任务" cancel-label="取消" tone="danger" :input="false" value="" @confirm="removeTask" @cancel="deleteJob=null" />
  </Teleport>
</template>

<style scoped>
.download-center{position:relative}.download-center summary{position:relative;display:grid;place-items:center;width:44px;height:44px;border-radius:50%;cursor:pointer;list-style:none}.download-center summary::-webkit-details-marker{display:none}.download-center summary:hover{background:#f1f5f9}.download-ring{width:40px;height:40px;fill:none;stroke:#3d5f7e;stroke-width:1.8;stroke-linecap:round;stroke-linejoin:round}.ring-track,.ring-value{stroke-width:2.6}.ring-track{stroke:#dce6ef}.ring-value{stroke:#0ea5e9;stroke-dasharray:100.53;transform:rotate(-90deg);transform-box:fill-box;transform-origin:center}.download-ring>path{stroke-width:1.7}.download-count{position:absolute;right:-2px;top:-1px;display:grid;place-items:center;min-width:17px;height:17px;padding:0 4px;border:2px solid #fff;border-radius:9px;background:#0284c7;color:#fff;font-size:9px;font-weight:800}
.download-popover{position:absolute;z-index:45;top:52px;right:-12px;width:min(450px,calc(100vw - 24px));max-height:62vh;overflow:hidden;border:1px solid #dfe6ee;border-radius:17px;background:#fff;box-shadow:0 24px 70px #0f172a2e}.download-popover:before{content:"";position:absolute;right:26px;top:-7px;width:12px;height:12px;border-left:1px solid #dfe6ee;border-top:1px solid #dfe6ee;background:#fff;transform:rotate(45deg)}.download-popover>header,.download-dialog>header{display:flex;align-items:center;justify-content:space-between;min-height:58px;padding:0 17px;border-bottom:1px solid #edf1f5}.download-popover header div,.download-dialog header div{display:flex;flex-direction:column;gap:3px}.download-popover header strong,.download-dialog header strong{font-size:14px}.download-popover header small,.download-dialog header small{color:#94a3b8;font-size:10px}.download-popover header .add{border:0;border-radius:9px;background:#e0f2fe;color:#0369a1;padding:7px 10px;font-size:11px;font-weight:750}
.download-list{max-height:calc(62vh - 58px);overflow:auto}.download-list article{display:grid;grid-template-columns:minmax(0,1fr) auto;align-items:center;border-bottom:1px solid #eef2f6}.task-main{display:grid;grid-template-columns:34px minmax(0,1fr) auto;align-items:center;gap:10px;min-width:0;padding:13px 8px 13px 15px;border:0;background:transparent;text-align:left}.task-main:not(:disabled){cursor:pointer}.task-icon{display:grid;place-items:center;width:32px;height:32px;border-radius:10px;background:#e0f2fe;color:#0284c7;font-weight:850}.task-copy{display:flex;min-width:0;flex-direction:column;gap:4px}.task-copy strong,.task-copy small{overflow:hidden;text-overflow:ellipsis;white-space:nowrap}.task-copy strong{font-size:12px}.task-copy small{color:#8795a8;font-size:10px}.task-copy i{height:3px;overflow:hidden;border-radius:3px;background:#e8edf3}.task-copy i b{display:block;height:100%;background:#0ea5e9}.task-copy i b.done{background:#22c55e}.task-copy i b.failed{background:#ef4444}.task-main em{color:#64748b;font-size:10px;font-style:normal}.task-actions{display:flex;align-items:center;padding-right:10px}.task-actions button{min-width:27px;border:0;background:transparent;color:#64748b;font-size:11px}.download-empty{display:grid;place-items:center;padding:30px;color:#94a3b8}.download-empty span{font-size:38px}.download-empty p{margin:4px 0 12px;font-size:12px}.download-empty button{border:0;border-radius:9px;background:#e0f2fe;color:#0369a1;padding:8px 11px;font-weight:700}
.popover-error{margin:0;padding:10px 15px;border-top:1px solid #fecaca;background:#fff1f2;color:#b91c1c;font-size:11px}
.download-backdrop{position:fixed;z-index:120;inset:0;display:grid;place-items:center;padding:18px;background:#0f172a80;backdrop-filter:blur(5px)}.download-dialog{width:min(640px,100%);max-height:min(760px,calc(100vh - 36px));overflow:hidden;border:1px solid #dfe6ee;border-radius:20px;background:#fff;box-shadow:0 30px 90px #02061755}.download-dialog>header{padding:0 20px}.download-dialog>header button{border:0;background:transparent;color:#64748b;font-size:24px}.source-tabs{display:flex;margin:20px 20px 12px;padding:4px;border-radius:12px;background:#f1f5f9}.source-tabs button{flex:1;padding:9px;border:0;border-radius:9px;background:transparent;color:#64748b;font-weight:700}.source-tabs button.active{background:#fff;color:#0369a1;box-shadow:0 2px 10px #0f172a14}.source-field{display:flex;flex-direction:column;gap:7px;margin:0 20px}.source-field span{font-size:12px;font-weight:750}.source-field textarea{resize:vertical;min-height:110px;padding:12px;border:1px solid #d8e1ea;border-radius:11px;font:12px/1.5 ui-monospace,SFMono-Regular,monospace}.torrent-picker{display:grid;place-items:center;margin:20px;padding:28px;border:1px dashed #a8bfd2;border-radius:14px;background:#f8fbfd;color:#0369a1;cursor:pointer}.torrent-picker input{position:absolute;opacity:0;pointer-events:none}.torrent-picker span{font-size:13px;font-weight:750}.torrent-picker small{margin-top:4px;color:#94a3b8}.privacy-note,.download-error{margin:12px 20px;font-size:11px}.privacy-note{color:#64748b}.download-error{color:#dc2626}.download-dialog footer{display:flex;justify-content:flex-end;gap:9px;padding:16px 20px;border-top:1px solid #edf1f5}.download-dialog footer button{padding:9px 14px;border-radius:10px}.download-dialog footer .secondary{border:1px solid #d8e1ea;background:#fff}.download-dialog footer .primary{border:0;background:#1677b8;color:#fff;font-weight:750}.download-dialog footer .primary:disabled{opacity:.55}.metadata-wait{display:grid;place-items:center;padding:48px 24px}.metadata-wait strong{margin-top:12px}.metadata-wait p{margin:7px 0 0;color:#64748b;font-size:12px}.download-spinner{width:30px;height:30px;border:3px solid #dbeafe;border-top-color:#0284c7;border-radius:50%;animation:download-spin .8s linear infinite}@keyframes download-spin{to{transform:rotate(360deg)}}.torrent-summary{display:flex;align-items:center;justify-content:space-between;padding:16px 20px;border-bottom:1px solid #edf1f5}.torrent-summary div{display:flex;min-width:0;flex-direction:column;gap:4px}.torrent-summary strong{overflow:hidden;text-overflow:ellipsis;white-space:nowrap;font-size:13px}.torrent-summary small{color:#64748b;font-size:11px}.torrent-summary button{border:0;background:transparent;color:#0369a1;font-size:11px}.torrent-files{max-height:min(440px,calc(100vh - 260px));overflow:auto}.torrent-files label{display:grid;grid-template-columns:auto minmax(0,1fr) auto;align-items:center;gap:10px;padding:10px 20px;border-bottom:1px solid #f0f3f6;cursor:pointer}.torrent-files span{overflow:hidden;text-overflow:ellipsis;white-space:nowrap;font-size:11px}.torrent-files small{color:#94a3b8;font-size:10px}
.destination-field{display:grid;grid-template-columns:auto minmax(0,1fr);align-items:center;gap:6px 12px;margin:14px 20px 0}.destination-field>span{font-size:12px;font-weight:750}.destination-field>button{display:grid;grid-template-columns:22px minmax(0,1fr) 16px;align-items:center;gap:8px;min-width:0;padding:10px 12px;border:1px solid #d8e1ea;border-radius:10px;background:#fff;color:#334155;text-align:left}.destination-field>button:hover{border-color:#38a3d7;box-shadow:0 0 0 3px #38a3d71a}.destination-field>button svg{width:18px;color:#d99b25}.destination-field>button svg:last-child{width:15px;color:#94a3b8}.destination-field>button b{min-width:0;overflow:hidden;text-overflow:ellipsis;white-space:nowrap}.destination-field small{grid-column:2;color:#94a3b8;font-size:10px}
.retry-dialog{width:min(520px,100%);overflow:hidden;border:1px solid #dfe6ee;border-radius:20px;background:#fff;box-shadow:0 30px 90px #02061755}.retry-dialog>header{display:flex;align-items:center;justify-content:space-between;min-height:68px;padding:0 20px;border-bottom:1px solid #edf1f5}.retry-dialog>header div{display:flex;min-width:0;flex-direction:column;gap:4px}.retry-dialog>header small{color:#94a3b8;font-size:10px}.retry-dialog>header button{border:0;background:transparent;color:#64748b}.retry-dialog>header svg{width:20px}.retry-content{padding:18px 0}.retry-content dl{display:grid;gap:12px;margin:0 20px}.retry-content dl div{display:grid;grid-template-columns:72px minmax(0,1fr);gap:10px}.retry-content dt{color:#94a3b8;font-size:11px}.retry-content dd{min-width:0;margin:0;overflow:hidden;color:#334155;font-size:12px;text-overflow:ellipsis;white-space:nowrap}.retry-content dd.failure{color:#b91c1c;white-space:normal;overflow-wrap:anywhere}.retry-dialog>footer{display:flex;justify-content:flex-end;gap:9px;padding:16px 20px;border-top:1px solid #edf1f5}.retry-dialog>footer button{min-height:39px;padding:0 15px;border-radius:10px}.retry-dialog>footer .secondary{border:1px solid #d8e1ea;background:#fff}.retry-dialog>footer .primary{border:0;background:#1677b8;color:#fff;font-weight:750}
.stream-files>div{display:grid;grid-template-columns:minmax(0,1fr) auto auto;align-items:center;gap:10px;padding:10px 20px;border-bottom:1px solid #f0f3f6}.stream-files button{display:flex;align-items:center;gap:4px;border:0;border-radius:8px;background:#e0f2fe;color:#0369a1;padding:6px 9px;font-size:11px;font-weight:700}.stream-files button svg{width:15px}.torrent-player{display:flex;flex-direction:column;gap:9px;padding:14px 20px;background:#0f172a;color:#fff}.torrent-player strong{overflow:hidden;text-overflow:ellipsis;white-space:nowrap;font-size:11px}.torrent-player video{width:100%;max-height:320px;background:#000}.torrent-player audio{width:100%}
@media(max-width:850px){.download-popover{position:fixed;top:66px;right:10px}.download-center summary{width:40px;height:40px}.download-backdrop{align-items:end;padding:0}.download-dialog,.retry-dialog{max-height:88vh;border-radius:20px 20px 0 0}.retry-dialog{width:100%}.torrent-files{max-height:46vh}}
.download-backdrop{height:100vh;height:100dvh;padding:max(18px,env(safe-area-inset-top,0px)) max(18px,env(safe-area-inset-right,0px)) max(18px,env(safe-area-inset-bottom,0px)) max(18px,env(safe-area-inset-left,0px))}.download-dialog{max-height:min(760px,calc(100vh - 36px));max-height:min(760px,calc(100dvh - 36px))}.torrent-files{max-height:min(440px,calc(100vh - 260px));max-height:min(440px,calc(100dvh - 260px))}@media(max-width:850px){.download-popover{top:calc(66px + env(safe-area-inset-top,0px));right:max(10px,env(safe-area-inset-right,0px))}.download-backdrop{padding:0 0 env(safe-area-inset-bottom,0px)}.download-dialog{max-height:88vh;max-height:88dvh}.torrent-files{max-height:46vh;max-height:46dvh}}
</style>

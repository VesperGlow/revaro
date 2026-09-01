<script setup lang="ts">
import { computed, onBeforeUnmount, onMounted, ref } from 'vue'
import { ChevronRight, FolderOpen, X } from 'lucide-vue-next'
import { api } from '../api'
import { formatSize } from '../format'
import type { DownloadJob } from '../types'
import DirectoryPickerModal from './DirectoryPickerModal.vue'

const ROOT='00000000-0000-0000-0000-000000000000'
const props=defineProps<{parentId:string}>()
const emit=defineEmits<{changed:[]}>()
const open=ref(false)
const mode=ref<'magnet'|'torrent'|'url'>('magnet')
const magnet=ref('')
const directURL=ref('')
const torrentFile=ref<File|null>(null)
const detail=ref<DownloadJob|null>(null)
const selected=ref<Set<number>>(new Set())
const busy=ref(false)
const error=ref('')
const destinationId=ref(props.parentId)
const destinationName=ref('当前文件夹')
const pickerOpen=ref(false)
let metadataTimer=0

const selectedFiles=computed(()=>detail.value?.files?.filter(file=>selected.value.has(file.index))||[])
const selectedSize=computed(()=>selectedFiles.value.reduce((sum,file)=>sum+file.size,0))
function reset(){mode.value='magnet';magnet.value='';directURL.value='';torrentFile.value=null;detail.value=null;selected.value=new Set();error.value='';busy.value=false;destinationId.value=props.parentId;destinationName.value=props.parentId===ROOT?'我的文件':'当前文件夹'}
function openCreate(){reset();open.value=true}
function openById(id:string){open.value=true;detail.value=null;error.value='';busy.value=true;void loadDetail(id).finally(()=>busy.value=false)}
defineExpose({openCreate,openById})
function close(){if(busy.value)return;open.value=false;pickerOpen.value=false;window.clearTimeout(metadataTimer)}
function onEscape(event:KeyboardEvent){if(event.key!=='Escape')return;if(pickerOpen.value)return;if(open.value)close()}
function pickTorrent(event:Event){torrentFile.value=(event.target as HTMLInputElement).files?.[0]||null;error.value=''}
function fileBase64(file:File){return new Promise<string>((resolve,reject)=>{const reader=new FileReader();reader.onerror=()=>reject(new Error('无法读取 .torrent 文件'));reader.onload=()=>{const bytes=new Uint8Array(reader.result as ArrayBuffer);let binary='';for(let i=0;i<bytes.length;i+=32768)binary+=String.fromCharCode(...bytes.subarray(i,i+32768));resolve(btoa(binary))};reader.readAsArrayBuffer(file)})}
function initializeSelection(job:DownloadJob){if(job.status==='waiting')selected.value=new Set((job.files||[]).map(file=>file.index))}
async function loadDetail(id:string){try{const job=await api<DownloadJob>(`/api/downloads/${id}`);detail.value=job;initializeSelection(job);emit('changed');if(job.status==='metadata')metadataTimer=window.setTimeout(()=>void loadDetail(id),800)}catch(e){error.value=(e as Error).message}}
async function createTask(){error.value='';if(mode.value==='magnet'&&!magnet.value.trim()){error.value='请粘贴磁力链接';return}if(mode.value==='torrent'&&!torrentFile.value){error.value='请选择 .torrent 文件';return}if(mode.value==='url'&&!/^https?:\/\//i.test(directURL.value.trim())){error.value='请输入完整的 HTTP 或 HTTPS 下载链接';return}busy.value=true;try{const body:Record<string,string>={parent_id:destinationId.value};if(mode.value==='magnet')body.magnet=magnet.value.trim();else if(mode.value==='torrent')body.torrent_base64=await fileBase64(torrentFile.value!);else body.url=directURL.value.trim();const job=await api<DownloadJob>('/api/downloads',{method:'POST',body:JSON.stringify(body)});emit('changed');if(job.source_type==='url'){open.value=false;return}detail.value=job;initializeSelection(job);if(job.status==='metadata')metadataTimer=window.setTimeout(()=>void loadDetail(job.id),500)}catch(e){error.value=(e as Error).message}finally{busy.value=false}}
async function startTask(){if(!detail.value||!selected.value.size)return;busy.value=true;error.value='';try{await api(`/api/downloads/${detail.value.id}/start`,{method:'POST',body:JSON.stringify({file_indices:[...selected.value]})});open.value=false;emit('changed')}catch(e){error.value=(e as Error).message}finally{busy.value=false}}
function toggleAll(){const files=detail.value?.files||[];selected.value=selected.value.size===files.length?new Set():new Set(files.map(file=>file.index))}
function toggleFile(index:number){const next=new Set(selected.value);if(next.has(index))next.delete(index);else next.add(index);selected.value=next}
function folderSelected(id:string,name:string){destinationId.value=id;destinationName.value=name;pickerOpen.value=false}
onMounted(()=>document.addEventListener('keydown',onEscape))
onBeforeUnmount(()=>{document.removeEventListener('keydown',onEscape);window.clearTimeout(metadataTimer)})
</script>

<template>
  <Teleport to="body">
    <div v-if="open" class="download-backdrop" @pointerdown.self="close">
      <section class="download-dialog" role="dialog" aria-modal="true" aria-label="新建下载" @pointerdown.stop>
        <header><div><strong>{{ detail?'选择下载文件':'新建下载' }}</strong><small>磁力、BT 与 HTTP/HTTPS 直链</small></div><button aria-label="关闭" @click="close"><X/></button></header>
        <template v-if="!detail">
          <div class="source-tabs"><button :class="{active:mode==='magnet'}" @click="mode='magnet'">磁力链接</button><button :class="{active:mode==='torrent'}" @click="mode='torrent'">.torrent 文件</button><button :class="{active:mode==='url'}" @click="mode='url'">直链下载</button></div>
          <label v-if="mode==='magnet'" class="source-field"><span>磁力链接</span><textarea v-model="magnet" rows="5" maxlength="16384" placeholder="magnet:?xt=urn:btih:…"></textarea></label>
          <label v-else-if="mode==='torrent'" class="torrent-picker"><input type="file" accept=".torrent,application/x-bittorrent" @change="pickTorrent"><span>{{ torrentFile?.name||'选择 .torrent 文件' }}</span><small>最大 4 MiB</small></label>
          <label v-else class="source-field"><span>HTTP / HTTPS 下载链接</span><textarea v-model="directURL" rows="4" maxlength="16384" placeholder="https://example.com/video.mkv"></textarea></label>
          <div class="destination-field"><span>保存到</span><button type="button" :title="destinationName" @click="pickerOpen=true"><FolderOpen/><b>{{ destinationName }}</b><ChevronRight/></button><small>任务创建后将在统一任务中心显示</small></div>
          <p class="privacy-note">{{ mode==='url'?'由服务器流式下载并写入对象存储；内网、本机地址和不安全重定向会被拦截。':'只连接公网节点、Tracker 与 WebSeed；内网和本机地址会被拦截。' }}</p>
          <p v-if="error" class="download-error">{{ error }}</p><footer><button class="secondary" @click="close">取消</button><button class="primary" :disabled="busy" @click="createTask">{{ busy?'正在创建…':mode==='url'?'开始下载':'解析种子' }}</button></footer>
        </template>
        <template v-else-if="detail.status==='metadata'"><div class="metadata-wait"><span class="download-spinner"></span><strong>正在获取种子元数据</strong><p>关闭后可继续在任务中心查看。</p></div><footer><button class="primary" @click="close">后台继续</button></footer></template>
        <template v-else-if="detail.status==='waiting'">
          <div class="torrent-summary"><div><strong>{{ detail.name }}</strong><small>{{ detail.files?.length||0 }} 个文件 · 已选 {{ formatSize(selectedSize) }}</small></div><button @click="toggleAll">{{ selected.size===(detail.files?.length||0)?'全不选':'全选' }}</button></div>
          <div class="torrent-files"><label v-for="file in detail.files" :key="file.index"><input type="checkbox" :checked="selected.has(file.index)" @change="toggleFile(file.index)"><span :title="file.path">{{ file.path }}</span><small>{{ formatSize(file.size) }}</small></label></div>
          <p v-if="error" class="download-error">{{ error }}</p><footer><button class="secondary" @click="close">稍后选择</button><button class="primary" :disabled="busy||!selected.size" @click="startTask">{{ busy?'正在启动…':`下载 ${selected.size} 个文件` }}</button></footer>
        </template>
        <template v-else><div class="metadata-wait"><strong>任务已进入任务中心</strong></div><footer><button class="primary" @click="close">关闭</button></footer></template>
      </section>
    </div>
    <DirectoryPickerModal v-if="pickerOpen" :initial-id="destinationId" title="选择保存目录" description="当前目录可以直接作为目标" @cancel="pickerOpen=false" @select="folderSelected"/>
  </Teleport>
</template>

<style scoped>
.download-backdrop{position:fixed;z-index:120;inset:0;display:grid;place-items:center;padding:max(18px,env(safe-area-inset-top,0px)) max(18px,env(safe-area-inset-right,0px)) max(18px,env(safe-area-inset-bottom,0px)) max(18px,env(safe-area-inset-left,0px));background:#0f172a80;backdrop-filter:blur(5px)}.download-dialog{width:min(640px,100%);max-height:min(760px,calc(100dvh - 36px));overflow:hidden;border:1px solid #dfe6ee;border-radius:20px;background:#fff;box-shadow:0 30px 90px #02061755}.download-dialog>header{display:flex;align-items:center;justify-content:space-between;min-height:58px;padding:0 20px;border-bottom:1px solid #edf1f5}.download-dialog header div{display:flex;flex-direction:column;gap:3px}.download-dialog header small{color:#94a3b8;font-size:10px}.download-dialog header button{border:0;background:transparent;color:#64748b}.download-dialog header svg{width:20px}.source-tabs{display:flex;margin:20px 20px 12px;padding:4px;border-radius:12px;background:#f1f5f9}.source-tabs button{flex:1;padding:9px;border:0;border-radius:9px;background:transparent;color:#64748b;font-weight:700}.source-tabs button.active{background:#fff;color:#0369a1;box-shadow:0 2px 10px #0f172a14}.source-field{display:flex;flex-direction:column;gap:7px;margin:0 20px}.source-field span{font-size:12px;font-weight:750}.source-field textarea{resize:vertical;min-height:110px;padding:12px;border:1px solid #d8e1ea;border-radius:11px;font:12px/1.5 ui-monospace,SFMono-Regular,monospace}.torrent-picker{display:grid;place-items:center;margin:20px;padding:28px;border:1px dashed #a8bfd2;border-radius:14px;background:#f8fbfd;color:#0369a1;cursor:pointer}.torrent-picker input{position:absolute;opacity:0;pointer-events:none}.torrent-picker span{font-size:13px;font-weight:750}.torrent-picker small{margin-top:4px;color:#94a3b8}.destination-field{display:grid;grid-template-columns:auto minmax(0,1fr);align-items:center;gap:6px 12px;margin:14px 20px 0}.destination-field>span{font-size:12px;font-weight:750}.destination-field>button{display:grid;grid-template-columns:22px minmax(0,1fr) 16px;align-items:center;gap:8px;min-width:0;padding:10px 12px;border:1px solid #d8e1ea;border-radius:10px;background:#fff;text-align:left}.destination-field>button svg{width:18px;color:#d99b25}.destination-field>button svg:last-child{width:15px;color:#94a3b8}.destination-field>button b{overflow:hidden;text-overflow:ellipsis;white-space:nowrap}.destination-field small{grid-column:2;color:#94a3b8;font-size:10px}.privacy-note,.download-error{margin:12px 20px;font-size:11px}.privacy-note{color:#64748b}.download-error{color:#dc2626}.download-dialog footer{display:flex;justify-content:flex-end;gap:9px;padding:16px 20px;border-top:1px solid #edf1f5}.download-dialog footer button{padding:9px 14px;border-radius:10px}.secondary{border:1px solid #d8e1ea;background:#fff}.primary{border:0;background:#1677b8;color:#fff;font-weight:750}.primary:disabled{opacity:.55}.metadata-wait{display:grid;place-items:center;padding:48px 24px}.metadata-wait p{color:#64748b;font-size:12px}.download-spinner{width:30px;height:30px;border:3px solid #dbeafe;border-top-color:#0284c7;border-radius:50%;animation:spin .8s linear infinite}@keyframes spin{to{transform:rotate(360deg)}}.torrent-summary{display:flex;align-items:center;justify-content:space-between;padding:16px 20px;border-bottom:1px solid #edf1f5}.torrent-summary div{display:flex;min-width:0;flex-direction:column;gap:4px}.torrent-summary strong{overflow:hidden;text-overflow:ellipsis;white-space:nowrap}.torrent-summary small{color:#64748b;font-size:11px}.torrent-summary button{border:0;background:transparent;color:#0369a1}.torrent-files{max-height:min(440px,calc(100dvh - 260px));overflow:auto}.torrent-files label{display:grid;grid-template-columns:auto minmax(0,1fr) auto;gap:10px;padding:10px 20px;border-bottom:1px solid #f0f3f6}.torrent-files span{overflow:hidden;text-overflow:ellipsis;white-space:nowrap;font-size:11px}.torrent-files small{color:#94a3b8;font-size:10px}@media(max-width:850px){.download-backdrop{align-items:end;padding:0 0 env(safe-area-inset-bottom,0px)}.download-dialog{max-height:88dvh;border-radius:20px 20px 0 0}.torrent-files{max-height:46dvh}}
</style>

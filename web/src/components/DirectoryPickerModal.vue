<script setup lang="ts">
import { computed, onMounted, ref, watch } from 'vue'
import { ChevronLeft, ChevronRight, Folder, FolderOpen, Home, X } from 'lucide-vue-next'
import { api } from '../api'
import type { DriveFile } from '../api'

const ROOT='00000000-0000-0000-0000-000000000000'
const props=withDefaults(defineProps<{initialId?:string;title?:string;description?:string;excludedIds?:string[];busy?:boolean}>(),{initialId:ROOT,title:'选择文件夹',description:'选择一个保存位置',excludedIds:()=>[],busy:false})
const emit=defineEmits<{cancel:[];select:[folderId:string,folderName:string]}>()
const currentId=ref(props.initialId||ROOT),current=ref<DriveFile|null>(null),breadcrumbs=ref<DriveFile[]>([]),folders=ref<DriveFile[]>([])
const loading=ref(false),error=ref('');let requestSeq=0
const excluded=computed(()=>new Set(props.excludedIds)),currentName=computed(()=>currentId.value===ROOT?'我的文件':current.value?.name||'当前文件夹'),parentId=computed(()=>current.value?.parent_id||null)

async function openFolder(id:string){
  if(excluded.value.has(id))return
  const seq=++requestSeq;loading.value=true;error.value=''
  try{
    const [meta,list]=await Promise.all([api<{file:DriveFile;breadcrumbs:DriveFile[]}>(`/api/files/${id}`),api<{items:DriveFile[]}>(`/api/files/${id}/children`)])
    if(seq!==requestSeq)return
    currentId.value=id;current.value=meta.file;breadcrumbs.value=meta.breadcrumbs;folders.value=list.items.filter(item=>item.kind==='directory'&&!excluded.value.has(item.id))
  }catch(e){if(seq!==requestSeq)return;if(id!==ROOT){void openFolder(ROOT);return}error.value=(e as Error).message}
  finally{if(seq===requestSeq)loading.value=false}
}
function selectCurrent(){if(!loading.value&&!props.busy)emit('select',currentId.value,currentName.value)}
function onKeydown(event:KeyboardEvent){if(event.key==='Escape'&&!props.busy)emit('cancel')}
watch(()=>props.initialId,id=>void openFolder(id||ROOT));onMounted(()=>void openFolder(props.initialId||ROOT))
</script>

<template>
  <div class="directory-picker-backdrop" role="presentation" @mousedown.self="!busy&&$emit('cancel')" @keydown="onKeydown">
    <section class="directory-picker" role="dialog" aria-modal="true" aria-labelledby="directory-picker-title">
      <header><div><p class="eyebrow dark">DIRECTORY</p><h2 id="directory-picker-title">{{ title }}</h2><p>{{ description }}</p></div><button type="button" aria-label="关闭" :disabled="busy" @click="$emit('cancel')"><X /></button></header>
      <nav class="directory-breadcrumbs" aria-label="目录路径">
        <button type="button" :class="{active:currentId===ROOT}" title="我的文件" @click="openFolder(ROOT)"><Home /><span>我的文件</span></button>
        <template v-for="crumb in breadcrumbs.filter(item=>item.id!==ROOT)" :key="crumb.id"><ChevronRight aria-hidden="true" /><button type="button" :class="{active:crumb.id===currentId}" :title="crumb.name" @click="openFolder(crumb.id)"><span>{{ crumb.name }}</span></button></template>
      </nav>
      <div class="directory-current"><button type="button" :disabled="!parentId||loading" @click="parentId&&openFolder(parentId)"><ChevronLeft />返回上级</button><span :title="currentName"><FolderOpen />{{ currentName }}</span></div>
      <div class="directory-list" aria-live="polite">
        <div v-if="loading" class="directory-state"><span class="spinner"></span><p>正在读取文件夹…</p></div>
        <div v-else-if="error" class="directory-state error"><p>{{ error }}</p><button type="button" @click="openFolder(currentId)">重新加载</button></div>
        <button v-for="folder in folders" v-else :key="folder.id" type="button" :title="folder.name" @click="openFolder(folder.id)"><Folder /><span>{{ folder.name }}</span><ChevronRight /></button>
        <div v-if="!loading&&!error&&!folders.length" class="directory-state"><FolderOpen /><p>此文件夹中没有子文件夹</p></div>
      </div>
      <footer><button type="button" class="secondary" :disabled="busy" @click="$emit('cancel')">取消</button><button type="button" class="primary" :disabled="loading||busy||!!error" @click="selectCurrent">{{ busy?'正在处理…':'选择此文件夹' }}</button></footer>
    </section>
  </div>
</template>

<style scoped>
.directory-picker-backdrop{position:fixed;z-index:140;inset:0;display:grid;place-items:center;padding:20px;background:#0f172a73;backdrop-filter:blur(5px)}.directory-picker{display:flex;flex-direction:column;width:min(560px,100%);height:min(650px,calc(100dvh - 40px));overflow:hidden;border:1px solid #e4eaf1;border-radius:19px;background:#fff;box-shadow:0 30px 90px #0f172a4d;color:#172033}
header{display:flex;align-items:flex-start;justify-content:space-between;padding:22px 24px 18px;border-bottom:1px solid #edf1f5}header h2{margin:1px 0 5px;font-size:20px}header p:last-child{margin:0;color:#718096;font-size:12px}header>button{display:grid;place-items:center;width:36px;height:36px;border:0;border-radius:10px;background:transparent;color:#64748b}header>button:hover{background:#f1f5f9}header svg{width:20px}
.directory-breadcrumbs{display:flex;align-items:center;gap:3px;min-height:48px;padding:7px 16px;overflow-x:auto;border-bottom:1px solid #edf1f5;scrollbar-width:thin}.directory-breadcrumbs>svg{flex:0 0 auto;width:14px;color:#a0aec0}.directory-breadcrumbs button{display:flex;align-items:center;gap:6px;flex:0 0 auto;max-width:190px;min-height:32px;padding:0 9px;border:0;border-radius:8px;background:transparent;color:#64748b}.directory-breadcrumbs button:hover,.directory-breadcrumbs button.active{background:#eff6ff;color:#2563eb}.directory-breadcrumbs button span{overflow:hidden;text-overflow:ellipsis;white-space:nowrap}.directory-breadcrumbs button svg{width:15px}
.directory-current{display:flex;align-items:center;justify-content:space-between;gap:12px;padding:12px 18px;background:#f8fafc}.directory-current button{display:flex;align-items:center;gap:5px;flex:0 0 auto;min-height:34px;padding:0 10px;border:1px solid #dce3eb;border-radius:9px;background:#fff;color:#475569}.directory-current button:disabled{opacity:.45}.directory-current button svg{width:16px}.directory-current span{display:flex;align-items:center;justify-content:flex-end;gap:7px;min-width:0;color:#334155;font-size:13px;font-weight:750;overflow:hidden;text-overflow:ellipsis;white-space:nowrap}.directory-current span svg{flex:0 0 auto;width:18px;color:#d99b25}
.directory-list{flex:1;min-height:0;overflow:auto;padding:8px}.directory-list>button{display:grid;grid-template-columns:34px minmax(0,1fr) 18px;align-items:center;width:100%;min-height:50px;padding:5px 12px;border:0;border-radius:10px;background:#fff;color:#334155;text-align:left}.directory-list>button:hover{background:#eff6ff;color:#1d4ed8}.directory-list>button svg:first-child{width:22px;color:#d99b25;fill:#f8c96b}.directory-list>button svg:last-child{width:16px;color:#94a3b8}.directory-list>button span{min-width:0;overflow:hidden;text-overflow:ellipsis;white-space:nowrap}.directory-state{display:flex;flex-direction:column;align-items:center;justify-content:center;gap:10px;height:100%;min-height:180px;color:#94a3b8;text-align:center}.directory-state>svg{width:35px}.directory-state p{margin:0}.directory-state.error{color:#b91c1c}.directory-state button{padding:8px 12px;border:0;border-radius:8px;background:#fee2e2;color:#991b1b}
footer{display:flex;justify-content:flex-end;gap:10px;padding:16px 20px;border-top:1px solid #edf1f5;background:#fff}footer button{min-height:40px;padding:0 17px;border-radius:10px;font-weight:750}@media(max-width:600px){.directory-picker-backdrop{align-items:end;padding:0}.directory-picker{width:100%;height:min(78dvh,680px);border-radius:20px 20px 0 0}header{padding:18px 18px 15px}.directory-breadcrumbs{padding-inline:10px}.directory-current{padding-inline:12px}.directory-list{padding:6px}footer{padding:12px 14px calc(12px + env(safe-area-inset-bottom))}footer button{flex:1}.directory-breadcrumbs button{max-width:145px}}
</style>

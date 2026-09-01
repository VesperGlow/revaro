<script setup lang="ts">
import { computed, onBeforeUnmount, onMounted, ref, watch } from 'vue'
import { ChevronDown, ChevronLeft, ChevronRight, Folder, FolderOpen, Home } from 'lucide-vue-next'
import { api } from '../api'
import type { DriveFile } from '../api'

const ROOT='00000000-0000-0000-0000-000000000000'
const props=withDefaults(defineProps<{modelValue:string;excludedIds?:string[];disabled?:boolean}>(),{excludedIds:()=>[],disabled:false})
const emit=defineEmits<{change:[folderId:string,folderName:string]}>()
const root=ref<HTMLElement|null>(null),expanded=ref(false),currentId=ref(props.modelValue||ROOT)
const current=ref<DriveFile|null>(null),breadcrumbs=ref<DriveFile[]>([]),folders=ref<DriveFile[]>([])
const loading=ref(false),error=ref('');let requestSeq=0
const excluded=computed(()=>new Set(props.excludedIds))
const currentName=computed(()=>currentId.value===ROOT?'我的文件':current.value?.name||'当前文件夹')
const parentId=computed(()=>current.value?.parent_id||null)
const pathName=computed(()=>{
  const names=['我的文件',...breadcrumbs.value.filter(item=>item.id!==ROOT&&item.id!==currentId.value).map(item=>item.name)]
  if(currentId.value!==ROOT)names.push(currentName.value)
  return names.join(' / ')
})

async function openFolder(id:string,notify=true){
  if(excluded.value.has(id))return
  const seq=++requestSeq;loading.value=true;error.value=''
  try{
    const [meta,list]=await Promise.all([api<{file:DriveFile;breadcrumbs:DriveFile[]}>(`/api/files/${id}`),api<{items:DriveFile[]}>(`/api/files/${id}/children`)])
    if(seq!==requestSeq)return
    currentId.value=id;current.value=meta.file;breadcrumbs.value=meta.breadcrumbs;folders.value=list.items.filter(item=>item.kind==='directory'&&!excluded.value.has(item.id))
    if(notify)emit('change',id,pathName.value)
  }catch(e){if(seq!==requestSeq)return;if(id!==ROOT){void openFolder(ROOT,notify);return}error.value=(e as Error).message}
  finally{if(seq===requestSeq)loading.value=false}
}
function toggle(){if(!props.disabled)expanded.value=!expanded.value}
function closeFromOutside(event:PointerEvent){const target=event.target;if(expanded.value&&target instanceof Node&&!root.value?.contains(target))expanded.value=false}
function closeFromEscape(event:KeyboardEvent){if(event.key==='Escape'&&expanded.value){expanded.value=false;event.stopPropagation()}}
watch(()=>props.modelValue,id=>{if(id&&id!==currentId.value)void openFolder(id,false)})
onMounted(()=>{void openFolder(props.modelValue||ROOT,false);document.addEventListener('pointerdown',closeFromOutside);document.addEventListener('keydown',closeFromEscape,true)})
onBeforeUnmount(()=>{requestSeq++;document.removeEventListener('pointerdown',closeFromOutside);document.removeEventListener('keydown',closeFromEscape,true)})
</script>

<template>
  <div ref="root" class="directory-picker" :class="{expanded,disabled}">
    <button type="button" class="directory-trigger" :aria-expanded="expanded" :disabled="disabled" :title="pathName" @click="toggle"><FolderOpen/><span>{{ pathName }}</span><ChevronDown class="directory-chevron"/></button>
    <div class="directory-collapse">
      <div class="directory-panel">
        <nav class="directory-breadcrumbs" aria-label="目录路径">
          <button type="button" :class="{active:currentId===ROOT}" title="我的文件" @click="openFolder(ROOT)"><Home/><span>我的文件</span></button>
          <template v-for="crumb in breadcrumbs.filter(item=>item.id!==ROOT)" :key="crumb.id"><ChevronRight aria-hidden="true"/><button type="button" :class="{active:crumb.id===currentId}" :title="crumb.name" @click="openFolder(crumb.id)"><span>{{ crumb.name }}</span></button></template>
        </nav>
        <div class="directory-current"><button type="button" :disabled="!parentId||loading" @click="parentId&&openFolder(parentId)"><ChevronLeft/>返回上级</button><span :title="currentName"><FolderOpen/>{{ currentName }}</span></div>
        <div class="directory-list" aria-live="polite">
          <div v-if="loading" class="directory-state"><span class="spinner"></span><p>正在读取文件夹…</p></div>
          <div v-else-if="error" class="directory-state error"><p>{{ error }}</p><button type="button" @click="openFolder(currentId,false)">重新加载</button></div>
          <button v-for="folder in folders" v-else :key="folder.id" type="button" :title="folder.name" @click="openFolder(folder.id)"><Folder/><span>{{ folder.name }}</span><ChevronRight/></button>
          <div v-if="!loading&&!error&&!folders.length" class="directory-state"><FolderOpen/><p>此文件夹中没有子文件夹</p></div>
        </div>
      </div>
    </div>
  </div>
</template>

<style scoped>
.directory-picker{min-width:0}.directory-trigger{display:grid;grid-template-columns:22px minmax(0,1fr) 18px;align-items:center;width:100%;min-height:42px;padding:9px 12px;border:1px solid #d8e1ea;border-radius:10px;background:#fff;color:#334155;text-align:left}.directory-trigger:hover,.directory-picker.expanded .directory-trigger{border-color:#93b8d3;background:#f8fbfd}.directory-trigger>svg:first-child{width:18px;color:#d99b25}.directory-trigger span{overflow:hidden;text-overflow:ellipsis;white-space:nowrap;font-weight:700}.directory-chevron{width:16px;color:#94a3b8;transition:transform .18s ease}.expanded .directory-chevron{transform:rotate(180deg)}.directory-collapse{display:grid;grid-template-rows:0fr;opacity:0;transition:grid-template-rows .18s ease,opacity .15s ease}.expanded .directory-collapse{grid-template-rows:1fr;opacity:1}.directory-panel{min-height:0;overflow:hidden;border:0 solid #dfe6ee;border-radius:0 0 12px 12px;background:#fff}.expanded .directory-panel{margin-top:6px;border-width:1px}.directory-breadcrumbs{display:flex;align-items:center;gap:3px;min-height:43px;padding:5px 9px;overflow-x:auto;border-bottom:1px solid #edf1f5;scrollbar-width:thin}.directory-breadcrumbs>svg{flex:0 0 auto;width:13px;color:#a0aec0}.directory-breadcrumbs button{display:flex;align-items:center;gap:5px;flex:0 0 auto;max-width:155px;min-height:30px;padding:0 8px;border:0;border-radius:8px;background:transparent;color:#64748b}.directory-breadcrumbs button:hover,.directory-breadcrumbs button.active{background:#eff6ff;color:#2563eb}.directory-breadcrumbs button span{overflow:hidden;text-overflow:ellipsis;white-space:nowrap}.directory-breadcrumbs button svg{width:14px}.directory-current{display:flex;align-items:center;justify-content:space-between;gap:10px;padding:8px 10px;background:#f8fafc}.directory-current button{display:flex;align-items:center;gap:4px;flex:0 0 auto;min-height:32px;padding:0 9px;border:1px solid #dce3eb;border-radius:8px;background:#fff;color:#475569}.directory-current button:disabled{opacity:.45}.directory-current button svg{width:15px}.directory-current span{display:flex;align-items:center;justify-content:flex-end;gap:6px;min-width:0;overflow:hidden;color:#2563eb;font-size:12px;font-weight:750;text-overflow:ellipsis;white-space:nowrap}.directory-current span svg{flex:0 0 auto;width:17px;color:#d99b25}.directory-list{max-height:min(260px,36dvh);overflow:auto;overscroll-behavior:contain;padding:6px;touch-action:pan-y}.directory-list>button{display:grid;grid-template-columns:31px minmax(0,1fr) 17px;align-items:center;width:100%;min-height:44px;padding:4px 10px;border:0;border-radius:9px;background:#fff;color:#334155;text-align:left}.directory-list>button:hover{background:#eff6ff;color:#1d4ed8}.directory-list>button svg:first-child{width:20px;color:#d99b25;fill:#f8c96b}.directory-list>button svg:last-child{width:15px;color:#94a3b8}.directory-list>button span{overflow:hidden;text-overflow:ellipsis;white-space:nowrap}.directory-state{display:flex;min-height:100px;align-items:center;justify-content:center;gap:8px;color:#94a3b8;text-align:center}.directory-state>svg{width:25px}.directory-state p{margin:0}.directory-state.error{color:#b91c1c}.directory-state button{padding:7px 10px;border:0;border-radius:8px;background:#fee2e2;color:#991b1b}.disabled{opacity:.65}@media(max-width:600px){.directory-list{max-height:min(230px,31dvh)}.directory-breadcrumbs button{max-width:125px}.directory-current{padding:7px 8px}}
</style>

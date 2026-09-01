<script setup lang="ts">
import { computed, nextTick, onBeforeUnmount, onMounted, ref, watch } from 'vue'
import { ChevronDown, ChevronRight, Folder, FolderOpen, Home } from 'lucide-vue-next'
import { api } from '../api'
import type { DriveFile } from '../api'

const ROOT='00000000-0000-0000-0000-000000000000'
const props=withDefaults(defineProps<{modelValue:string;excludedIds?:string[];disabled?:boolean}>(),{excludedIds:()=>[],disabled:false})
const emit=defineEmits<{change:[folderId:string,folderName:string]}>()
const root=ref<HTMLElement|null>(null),trigger=ref<HTMLElement|null>(null),panel=ref<HTMLElement|null>(null)
const expanded=ref(false),currentId=ref(props.modelValue||ROOT)
const current=ref<DriveFile|null>(null),breadcrumbs=ref<DriveFile[]>([]),folders=ref<DriveFile[]>([])
const loading=ref(false),error=ref(''),panelStyle=ref<Record<string,string>>({});let requestSeq=0
const excluded=computed(()=>new Set(props.excludedIds))
const breadcrumbItems=computed(()=>{
  const items=breadcrumbs.value.filter(item=>item.id!==ROOT)
  if(current.value&&currentId.value!==ROOT&&!items.some(item=>item.id===currentId.value))items.push(current.value)
  return items
})
const pathName=computed(()=>['我的文件',...breadcrumbItems.value.map(item=>item.name)].join(' / '))

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
function updatePosition(){
  if(!expanded.value||!trigger.value)return
  const rect=trigger.value.getBoundingClientRect(),margin=10,gap=6,preferredHeight=340
  const below=window.innerHeight-rect.bottom-gap-margin,above=rect.top-gap-margin
  const opensUp=below<Math.min(240,preferredHeight)&&above>below
  const available=Math.max(150,Math.min(preferredHeight,opensUp?above:below))
  const width=Math.min(rect.width,window.innerWidth-margin*2)
  const left=Math.max(margin,Math.min(rect.left,window.innerWidth-margin-width))
  panelStyle.value={position:'fixed',left:`${Math.round(left)}px`,width:`${Math.round(width)}px`,maxHeight:`${Math.round(available)}px`,top:opensUp?'auto':`${Math.round(rect.bottom+gap)}px`,bottom:opensUp?`${Math.round(window.innerHeight-rect.top+gap)}px`:'auto'}
}
function toggle(){if(!props.disabled){expanded.value=!expanded.value;if(expanded.value)void nextTick(updatePosition)}}
function closeFromOutside(event:PointerEvent){const target=event.target;if(expanded.value&&target instanceof Node&&!root.value?.contains(target)&&!panel.value?.contains(target))expanded.value=false}
function closeFromEscape(event:KeyboardEvent){if(event.key==='Escape'&&expanded.value){expanded.value=false;event.stopPropagation()}}
function reposition(){if(expanded.value)updatePosition()}
watch(()=>props.modelValue,id=>{if(id&&id!==currentId.value)void openFolder(id,false)})
watch(()=>props.disabled,disabled=>{if(disabled)expanded.value=false})
onMounted(()=>{void openFolder(props.modelValue||ROOT,false);document.addEventListener('pointerdown',closeFromOutside);document.addEventListener('keydown',closeFromEscape,true);window.addEventListener('resize',reposition);window.addEventListener('scroll',reposition,true)})
onBeforeUnmount(()=>{requestSeq++;document.removeEventListener('pointerdown',closeFromOutside);document.removeEventListener('keydown',closeFromEscape,true);window.removeEventListener('resize',reposition);window.removeEventListener('scroll',reposition,true)})
</script>

<template>
  <div ref="root" class="directory-picker" :class="{expanded,disabled}">
    <button ref="trigger" type="button" class="directory-trigger" :aria-expanded="expanded" :disabled="disabled" :title="pathName" @click="toggle"><FolderOpen/><span>{{ pathName }}</span><ChevronDown class="directory-chevron"/></button>
    <Teleport to="body">
      <Transition name="directory-flyout">
        <section v-if="expanded" ref="panel" class="directory-popover" :style="panelStyle" aria-label="选择目标目录" @pointerdown.stop>
          <nav class="directory-breadcrumbs" aria-label="目录路径">
            <button type="button" :class="{active:currentId===ROOT}" title="我的文件" @click="openFolder(ROOT)"><Home/><span>我的文件</span></button>
            <template v-for="crumb in breadcrumbItems" :key="crumb.id"><ChevronRight aria-hidden="true"/><button type="button" :class="{active:crumb.id===currentId}" :title="crumb.name" @click="openFolder(crumb.id)"><span>{{ crumb.name }}</span></button></template>
          </nav>
          <div class="directory-list" aria-live="polite">
            <div v-if="loading" class="directory-state"><span class="spinner"></span><p>正在读取文件夹…</p></div>
            <div v-else-if="error" class="directory-state error"><p>{{ error }}</p><button type="button" @click="openFolder(currentId,false)">重新加载</button></div>
            <button v-for="folder in folders" v-else :key="folder.id" type="button" :title="folder.name" @click="openFolder(folder.id)"><Folder/><span>{{ folder.name }}</span><ChevronRight/></button>
            <div v-if="!loading&&!error&&!folders.length" class="directory-state"><FolderOpen/><p>此文件夹中没有子文件夹</p></div>
          </div>
        </section>
      </Transition>
    </Teleport>
  </div>
</template>

<style scoped>
.directory-picker{min-width:0}.directory-trigger{display:grid;grid-template-columns:22px minmax(0,1fr) 18px;align-items:center;width:100%;min-height:42px;padding:9px 12px;border:1px solid #d8e1ea;border-radius:10px;background:#fff;color:#334155;text-align:left}.directory-trigger:hover,.directory-picker.expanded .directory-trigger{border-color:#93b8d3;background:#f8fbfd}.directory-trigger>svg:first-child{width:18px;color:#d99b25}.directory-trigger span{overflow:hidden;text-overflow:ellipsis;white-space:nowrap;font-weight:700}.directory-chevron{width:16px;color:#94a3b8;transition:transform .16s ease}.expanded .directory-chevron{transform:rotate(180deg)}.disabled{opacity:.65}
.directory-popover{z-index:155;display:flex;min-height:120px;overflow:hidden;flex-direction:column;border:1px solid #dfe6ee;border-radius:12px;background:#fff;box-shadow:0 18px 50px #0f172a38;color:#172033}.directory-breadcrumbs{display:flex;align-items:center;gap:3px;flex:0 0 auto;min-height:43px;padding:5px 9px;overflow-x:auto;border-bottom:1px solid #edf1f5;scrollbar-width:thin}.directory-breadcrumbs>svg{flex:0 0 auto;width:13px;color:#a0aec0}.directory-breadcrumbs button{display:flex;align-items:center;gap:5px;flex:0 0 auto;max-width:155px;min-height:30px;padding:0 8px;border:0;border-radius:8px;background:transparent;color:#64748b}.directory-breadcrumbs button:hover,.directory-breadcrumbs button.active{background:#eff6ff;color:#2563eb}.directory-breadcrumbs button span{overflow:hidden;text-overflow:ellipsis;white-space:nowrap}.directory-breadcrumbs button svg{width:14px}.directory-list{min-height:0;overflow:auto;overscroll-behavior:contain;padding:6px;touch-action:pan-y}.directory-list>button{display:grid;grid-template-columns:31px minmax(0,1fr) 17px;align-items:center;width:100%;min-height:44px;padding:4px 10px;border:0;border-radius:9px;background:#fff;color:#334155;text-align:left}.directory-list>button:hover{background:#eff6ff;color:#1d4ed8}.directory-list>button svg:first-child{width:20px;color:#d99b25;fill:#f8c96b}.directory-list>button svg:last-child{width:15px;color:#94a3b8}.directory-list>button span{overflow:hidden;text-overflow:ellipsis;white-space:nowrap}.directory-state{display:flex;min-height:100px;align-items:center;justify-content:center;gap:8px;color:#94a3b8;text-align:center}.directory-state>svg{width:25px}.directory-state p{margin:0}.directory-state.error{color:#b91c1c}.directory-state button{padding:7px 10px;border:0;border-radius:8px;background:#fee2e2;color:#991b1b}.directory-flyout-enter-active,.directory-flyout-leave-active{transition:opacity .14s ease,transform .14s ease}.directory-flyout-enter-from,.directory-flyout-leave-to{opacity:0;transform:translateY(-4px)}@media(max-width:600px){.directory-breadcrumbs button{max-width:125px}.directory-list>button{min-height:46px}}
</style>

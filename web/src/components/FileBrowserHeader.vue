<script setup lang="ts">
import { computed, nextTick, onBeforeUnmount, onMounted, ref, watch } from 'vue'
import { ChevronDown, ChevronRight, FilePlus2, FolderPlus, FolderUp, Music2, Upload } from 'lucide-vue-next'
import type { DriveFile } from '../api'
import { formatSize } from '../format'

const props=defineProps<{
  breadcrumbs:DriveFile[]
  current:DriveFile|null
  itemCount:number
  totalBytes:number
  fileCount:number
  trashMode:boolean
}>()

const emit=defineEmits<{
  openFolder:[id:string]
  newDocument:[]
  createFolder:[]
  uploadFiles:[]
  uploadFolder:[]
  localAudioMerge:[]
  leaveTrash:[]
  emptyTrash:[]
}>()

const createMenu=ref<HTMLDetailsElement|null>(null)
const uploadMenu=ref<HTMLDetailsElement|null>(null)
const breadcrumbNav=ref<HTMLElement|null>(null)
const pathItems=computed(()=>props.breadcrumbs.length?props.breadcrumbs:(props.current?[props.current]:[]))

function revealCurrentPath(){nextTick(()=>breadcrumbNav.value?.scrollTo({left:breadcrumbNav.value.scrollWidth,behavior:'smooth'}))}
watch(()=>props.current?.id,revealCurrentPath)
onMounted(revealCurrentPath)

function runCreate(action:'document'|'folder'){
  createMenu.value?.removeAttribute('open')
  if(action==='document')emit('newDocument')
  else emit('createFolder')
}
function runUpload(action:'files'|'folder'|'localMerge'){
  uploadMenu.value?.removeAttribute('open')
  if(action==='files')emit('uploadFiles')
  else if(action==='folder')emit('uploadFolder')
  else emit('localAudioMerge')
}
function closeMenus(event:PointerEvent){
  const target=event.target as Node|null
  if(target&&!createMenu.value?.contains(target))createMenu.value?.removeAttribute('open')
  if(target&&!uploadMenu.value?.contains(target))uploadMenu.value?.removeAttribute('open')
}
function closeMenusWithEscape(event:KeyboardEvent){
  if(event.key!=='Escape')return
  createMenu.value?.removeAttribute('open');uploadMenu.value?.removeAttribute('open')
}
onMounted(()=>{window.addEventListener('pointerdown',closeMenus);window.addEventListener('keydown',closeMenusWithEscape)})
onBeforeUnmount(()=>{window.removeEventListener('pointerdown',closeMenus);window.removeEventListener('keydown',closeMenusWithEscape)})
</script>

<template>
  <div class="content-head">
    <div class="folder-heading">
      <nav v-if="!trashMode&&pathItems.length" ref="breadcrumbNav" class="breadcrumbs" aria-label="当前路径">
        <template v-for="(crumb,index) in pathItems" :key="crumb.id">
          <ChevronRight v-if="index" class="breadcrumb-separator" aria-hidden="true" />
          <button type="button" :class="{current:index===pathItems.length-1}" :title="crumb.name || '我的文件'" :aria-current="index===pathItems.length-1?'page':undefined" @click="$emit('openFolder',crumb.id)">
            {{ crumb.name || '我的文件' }}
          </button>
        </template>
      </nav>
      <div class="title-row">
        <h1 :title="trashMode?'回收站':current?.name || '我的文件'">{{ trashMode?'回收站':current?.name || '我的文件' }}</h1>
      </div>
      <p class="folder-meta">
        <span>{{ itemCount }} 个项目</span><i></i><template v-if="trashMode"><span>{{ formatSize(totalBytes) }}</span><i></i><span>已删除的文件将在 30 天后永久删除</span></template><template v-else>
          <span>共 {{ fileCount }} 个文件</span><i></i>
          <span>{{ formatSize(totalBytes) }}</span>
        </template>
      </p>
    </div>
    <div v-if="trashMode" class="actions"><button class="secondary" @click="$emit('leaveTrash')">返回我的文件</button><button class="trash-empty-action" :disabled="!itemCount" @click="$emit('emptyTrash')">清空回收站</button></div>
    <div v-else class="actions">
      <div class="desktop-create-actions">
        <button class="secondary" @click="$emit('newDocument')"><FilePlus2 aria-hidden="true" />新建文档</button>
        <button class="secondary" @click="$emit('createFolder')"><FolderPlus aria-hidden="true" />新建文件夹</button>
      </div>
      <details ref="createMenu" class="create-menu">
        <summary class="secondary"><FilePlus2 aria-hidden="true" />新建<ChevronDown class="menu-chevron" aria-hidden="true" /></summary>
        <div class="create-menu-popover">
          <button @click="runCreate('document')"><span><FilePlus2 /></span><div><b>新建文档</b><small>Markdown 或纯文本</small></div></button>
          <button @click="runCreate('folder')"><span><FolderPlus /></span><div><b>新建文件夹</b><small>整理当前目录</small></div></button>
        </div>
      </details>
      <details ref="uploadMenu" class="upload-menu">
        <summary class="primary upload-action"><Upload aria-hidden="true" />上传<ChevronDown class="upload-chevron" aria-hidden="true" /></summary>
        <div class="upload-menu-popover">
          <button @click="runUpload('files')"><span><Upload /></span><div><b>上传文件</b><small>可一次选择多个文件</small></div></button>
          <button @click="runUpload('folder')"><span><FolderUp /></span><div><b>上传文件夹</b><small>保留完整目录结构</small></div></button>
          <button @click="runUpload('localMerge')"><span><Music2 /></span><div><b>从本地目录合并</b><small>WAV + VTT + 封面，输出 ALAC M4A</small></div></button>
        </div>
      </details>
    </div>
  </div>
</template>

<style scoped>
.trash-empty-action{min-height:40px;padding:0 16px;border:1px solid #fecaca;border-radius:10px;background:#fff5f5;color:#dc2626;font-weight:750}.trash-empty-action:hover:not(:disabled){background:#fee2e2}.trash-empty-action:disabled{opacity:.45}
.breadcrumbs{display:flex;align-items:center;max-width:min(760px,70vw);margin:-4px 0 7px;padding:3px 0;overflow-x:auto;overscroll-behavior-x:contain;scrollbar-width:none;white-space:nowrap;-webkit-overflow-scrolling:touch}.breadcrumbs::-webkit-scrollbar{display:none}.breadcrumbs button{display:inline-flex;min-width:max-content;min-height:36px;align-items:center;padding:0 10px;border:0;border-radius:9px;background:transparent;color:#53657b;font-size:14px;font-weight:650;line-height:1;transition:background-color .15s ease,color .15s ease,box-shadow .15s ease}.breadcrumbs button:hover{background:#eef5ff;color:#1d64bd}.breadcrumbs button:active{background:#dfeeff;color:#174f96}.breadcrumbs button:focus-visible{outline:0;box-shadow:0 0 0 3px #bfdbfe}.breadcrumbs button.current{background:#edf5ff;color:#1764bd;font-weight:750}.breadcrumb-separator{width:15px;height:15px;flex:0 0 auto;color:#a9b7c8;stroke-width:1.8}@media(max-width:850px){.breadcrumbs{width:calc(100vw - 28px);max-width:100%;margin-bottom:6px;padding:2px 0 4px;mask-image:linear-gradient(to right,transparent 0,#000 10px,#000 calc(100% - 16px),transparent 100%)}.breadcrumbs button{min-height:40px;padding:0 11px;font-size:14px}.breadcrumbs button:first-of-type{margin-left:4px}.breadcrumbs button:last-of-type{margin-right:12px}.breadcrumb-separator{width:14px;height:14px}}
</style>

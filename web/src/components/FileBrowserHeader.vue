<script setup lang="ts">
import { computed, onBeforeUnmount, onMounted, ref } from 'vue'
import type { DriveFile } from '../api'
import { formatSize } from '../format'

const props=defineProps<{
  breadcrumbs:DriveFile[]
  current:DriveFile|null
  canGoUp:boolean
  itemCount:number
  totalBytes:number
  fileCount:number
  viewMode:'list'|'grid'
  trashMode:boolean
}>()

const emit=defineEmits<{
  openFolder:[id:string]
  up:[]
  setView:[mode:'list'|'grid']
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
const parentBreadcrumbs=computed(()=>props.breadcrumbs.slice(0,-1))

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
      <nav v-if="!trashMode&&parentBreadcrumbs.length" class="breadcrumbs" aria-label="路径">
        <button v-for="crumb in parentBreadcrumbs" :key="crumb.id" :title="crumb.name || '我的文件'" @click="$emit('openFolder',crumb.id)">
          {{ crumb.name || '我的文件' }}<span>/</span>
        </button>
      </nav>
      <div class="title-row">
        <button v-if="!trashMode&&canGoUp" class="up-button" title="返回上一级" aria-label="返回上一级" @click="$emit('up')">
          <svg viewBox="0 0 24 24" aria-hidden="true"><path d="M12 19V6m0 0-5 5m5-5 5 5"/></svg>
        </button>
        <h1 :title="trashMode?'回收站':current?.name || '我的文件'">{{ trashMode?'回收站':current?.name || '我的文件' }}</h1>
        <div class="view-switch" role="group" aria-label="文件显示方式">
          <button :class="{active:viewMode==='list'}" title="列表视图" aria-label="列表视图" @click="$emit('setView','list')">
            <svg viewBox="0 0 24 24" aria-hidden="true"><path d="M8 6h12M8 12h12M8 18h12M4 6h.01M4 12h.01M4 18h.01"/></svg>
          </button>
          <button :class="{active:viewMode==='grid'}" title="大图标视图" aria-label="大图标视图" @click="$emit('setView','grid')">
            <svg viewBox="0 0 24 24" aria-hidden="true"><rect x="4" y="4" width="6" height="6" rx="1"/><rect x="14" y="4" width="6" height="6" rx="1"/><rect x="4" y="14" width="6" height="6" rx="1"/><rect x="14" y="14" width="6" height="6" rx="1"/></svg>
          </button>
        </div>
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
        <button class="secondary" @click="$emit('newDocument')">＋ 新建文档</button>
        <button class="secondary" @click="$emit('createFolder')">＋ 新建文件夹</button>
        <button class="secondary merge-local-action" title="选择电脑上的音频目录，合并为无损 ALAC M4A" @click="$emit('localAudioMerge')">♬ 从本地目录合并</button>
      </div>
      <details ref="createMenu" class="create-menu">
        <summary class="secondary">＋ 新建</summary>
        <div class="create-menu-popover">
          <button @click="runCreate('document')"><span>▤</span><div><b>新建文档</b><small>Markdown 或纯文本</small></div></button>
          <button @click="runCreate('folder')"><span>▰</span><div><b>新建文件夹</b><small>整理当前目录</small></div></button>
        </div>
      </details>
      <details ref="uploadMenu" class="upload-menu">
        <summary class="primary upload-action">上传 <svg class="upload-chevron" viewBox="0 0 16 16" aria-hidden="true"><path d="m4 6 4 4 4-4"/></svg></summary>
        <div class="upload-menu-popover">
          <button @click="runUpload('files')"><span>↥</span><div><b>上传文件</b><small>可一次选择多个文件</small></div></button>
          <button @click="runUpload('folder')"><span>▰</span><div><b>上传文件夹</b><small>保留完整目录结构</small></div></button>
          <button @click="runUpload('localMerge')"><span>♬</span><div><b>从本地目录合并</b><small>WAV + VTT + 封面，输出 ALAC M4A</small></div></button>
        </div>
      </details>
    </div>
  </div>
</template>

<style scoped>
.trash-empty-action{min-height:40px;padding:0 16px;border:1px solid #fecaca;border-radius:10px;background:#fff5f5;color:#dc2626;font-weight:750}.trash-empty-action:hover:not(:disabled){background:#fee2e2}.trash-empty-action:disabled{opacity:.45}
</style>

<script setup lang="ts">
import { onMounted, ref, watch } from 'vue'
import { ChevronRight, Folder } from '@lucide/vue'
import { api } from '../api'
import type { DriveFile } from '../api'
import { ROOT_FOLDER_ID } from '../library'

const props=defineProps<{currentId:string;reloadToken:number}>()
const emit=defineEmits<{navigate:[id:string]}>()

const expanded=ref(true)
const children=ref<DriveFile[]|null>(null)
const loading=ref(false)

async function load(){
  loading.value=true
  try{
    const data=await api<{items:DriveFile[]}>(`/api/files/${ROOT_FOLDER_ID}/children`)
    children.value=data.items.filter(item=>item.kind==='directory')
  }catch{children.value=[]}
  finally{loading.value=false}
}
function toggle(){
  expanded.value=!expanded.value
  if(expanded.value&&children.value===null)void load()
}
onMounted(()=>{if(expanded.value)void load()})
watch(()=>props.reloadToken,()=>{children.value=null;if(expanded.value)void load()})
</script>

<template>
  <div class="path-node file-tree-root">
    <div class="path-row" :class="{active:currentId===ROOT_FOLDER_ID}">
      <button type="button" class="path-toggle" :aria-expanded="expanded" :aria-label="expanded?'收起子目录':'展开子目录'" @click.stop="toggle"><ChevronRight :class="{open:expanded}" aria-hidden="true" /></button>
      <button type="button" class="path-label" title="我的文件" @click="emit('navigate',ROOT_FOLDER_ID)"><Folder aria-hidden="true" /><span>我的文件</span></button>
    </div>
    <div v-if="expanded" class="path-children">
      <span v-if="loading" class="path-loading">读取中…</span>
      <SidebarDirectoryNode v-for="child in children||[]" v-else :key="child.id" :id="child.id" :name="child.name" :depth="1" :current-id="currentId" :reload-token="reloadToken" :auto-expand="false" @navigate="emit('navigate',$event)" />
      <span v-if="children&&!children.length&&!loading" class="path-loading">还没有子文件夹</span>
    </div>
  </div>
</template>

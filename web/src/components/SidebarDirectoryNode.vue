<script setup lang="ts">
import { ref, watch } from 'vue'
import { ChevronRight, Folder } from '@lucide/vue'
import { api } from '../api'
import type { DriveFile } from '../api'

const props=defineProps<{id:string;name:string;depth:number;currentId:string;reloadToken:number;autoExpand?:boolean}>()
const emit=defineEmits<{navigate:[id:string]}>()

const expanded=ref(false)
const children=ref<DriveFile[]|null>(null)
const loading=ref(false)

async function load(){
  loading.value=true
  try{
    const data=await api<{items:DriveFile[]}>(`/api/files/${props.id}/children`)
    children.value=data.items.filter(item=>item.kind==='directory')
  }catch{children.value=[]}
  finally{loading.value=false}
}
function toggle(){
  expanded.value=!expanded.value
  if(expanded.value&&children.value===null)void load()
}
function navigate(){emit('navigate',props.id)}
watch(()=>props.reloadToken,()=>{children.value=null;if(expanded.value)void load()})
watch(()=>props.autoExpand,value=>{if(value&&!expanded.value){expanded.value=true;if(children.value===null)void load()}},{immediate:true})
</script>

<template>
  <div class="path-node">
    <div class="path-row" :class="{active:currentId===id}" :style="{'--depth':depth}">
      <button type="button" class="path-toggle" :aria-expanded="expanded" :aria-label="expanded?'收起子目录':'展开子目录'" @click.stop="toggle"><ChevronRight :class="{open:expanded}" aria-hidden="true" /></button>
      <button type="button" class="path-label" :title="name" @click="navigate"><Folder aria-hidden="true" /><span>{{ name }}</span></button>
    </div>
    <div v-if="expanded" class="path-children">
      <span v-if="loading" class="path-loading">读取中…</span>
      <SidebarDirectoryNode v-for="child in children||[]" v-else :key="child.id" :id="child.id" :name="child.name" :depth="depth+1" :current-id="currentId" :reload-token="reloadToken" :auto-expand="false" @navigate="emit('navigate',$event)" />
    </div>
  </div>
</template>

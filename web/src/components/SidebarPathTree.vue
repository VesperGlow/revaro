<script setup lang="ts">
import { ref } from 'vue'
import { ChevronRight, Folder } from '@lucide/vue'
import { ROOT_FOLDER_ID, type LibraryFolderNode } from '../library'

const props=defineProps<{node:LibraryFolderNode;activeFolderId:string|null;depth:number}>()
const emit=defineEmits<{select:[id:string|null]}>()

const expanded=ref(props.depth<1)
function toggle(){expanded.value=!expanded.value}
function choose(){emit('select',props.node.id===ROOT_FOLDER_ID?null:props.node.id)}
</script>

<template>
  <div class="path-node">
    <div class="path-row" :class="{active:activeFolderId===node.id||(!activeFolderId&&node.id===ROOT_FOLDER_ID)}" :style="{'--depth':depth}">
      <button v-if="node.children.length" type="button" class="path-toggle" :aria-expanded="expanded" :aria-label="expanded?'收起子路径':'展开子路径'" @click.stop="toggle"><ChevronRight :class="{open:expanded}" aria-hidden="true" /></button>
      <span v-else class="path-toggle spacer" aria-hidden="true"></span>
      <button type="button" class="path-label" :title="node.path||node.name" @click="choose"><Folder aria-hidden="true" /><span>{{ node.name }}</span><em>{{ node.count }}</em></button>
    </div>
    <div v-if="expanded&&node.children.length" class="path-children">
      <SidebarPathTree v-for="child in node.children" :key="child.id" :node="child" :active-folder-id="activeFolderId" :depth="depth+1" @select="emit('select',$event)" />
    </div>
  </div>
</template>

<script setup lang="ts">
import type { DriveFile } from '../api'
import type { FolderOption } from '../types'

defineProps<{mode:'move'|'copy';targets:DriveFile[];folders:FolderOption[];busy:boolean}>()
defineEmits<{close:[];select:[folderId:string]}>()
</script>

<template>
  <section class="modal folder-modal"><header><div><p class="eyebrow dark">{{ mode==='copy'?'COPY':'MOVE' }}</p><h2>{{ mode==='copy'?'复制到':'移动到' }}</h2><p class="move-target" :title="targets.length===1?targets[0]?.name:undefined">{{ targets.length===1?`「${targets[0]?.name}」`:`${targets.length} 项` }}</p></div><button @click="$emit('close')">×</button></header><div v-if="busy" class="state small"><div class="spinner"></div></div><div v-else class="folder-list"><button v-for="folder in folders" :key="folder.id" :style="{paddingLeft:`${18+folder.depth*22}px`}" @click="$emit('select',folder.id)"><span>▰</span>{{ folder.name }}</button></div></section>
</template>

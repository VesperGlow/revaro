<script setup lang="ts">
import type { DriveFile } from '../api'
import FileCard from './FileCard.vue'

defineProps<{items:DriveFile[];selectedIds:Set<string>;trashMode?:boolean}>()
const emit=defineEmits<{open:[item:DriveFile];select:[item:DriveFile]}>()
</script>

<template>
  <div class="file-grid" :class="{'selection-mode':selectedIds.size>0}">
    <FileCard v-for="item in items" :key="item.id" :item="item" :selected="selectedIds.has(item.id)" :selection-mode="selectedIds.size>0" :trash-mode="trashMode" @open="emit('open',$event)" @select="emit('select',$event)" />
  </div>
</template>

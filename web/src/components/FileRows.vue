<script setup lang="ts">
import type { DriveFile } from '../api'
import { formatDate, formatSize } from '../format'
import { folderLabel, formatDuration, type LibraryItem } from '../library'
import FileCard from './FileCard.vue'

defineProps<{items:DriveFile[];selectedIds?:Set<string>;selectable?:boolean;mode?:'default'|'audio'}>()
const emit=defineEmits<{open:[item:DriveFile];select:[item:DriveFile]}>()

function subtitle(item:DriveFile,mode:'default'|'audio'){
  if(mode==='audio')return folderLabel(item as LibraryItem)
  if(item.kind==='directory')return '文件夹'
  return `${formatSize(item.size)} · ${formatDate(item.updated_at)}`
}
function duration(item:DriveFile){return formatDuration((item as LibraryItem).duration_ms)}
</script>

<template>
  <div class="file-rows" :class="{'selection-mode':(selectedIds?.size||0)>0}">
    <FileCard v-for="item in items" :key="item.id" :item="item" layout="list" :selected="selectedIds?.has(item.id)" :selectable="selectable" :selection-mode="(selectedIds?.size||0)>0" :subtitle="subtitle(item,mode||'default')" @open="emit('open',$event)" @select="emit('select',$event)">
      <template v-if="mode==='audio'" #trailing><span class="row-duration">{{ duration(item) }}</span></template>
    </FileCard>
  </div>
</template>

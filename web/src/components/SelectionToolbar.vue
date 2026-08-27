<script setup lang="ts">
import type { DriveFile } from '../api'
import { isArchive, isBook, isEditable, isImage, isMedia } from '../fileTypes'
import { formatSize } from '../format'

defineProps<{
  selectedItems:DriveFile[]
  selectedBytes:number
  selectedFiles:DriveFile[]
  singleSelected:DriveFile|null
  itemCount:number
  trashMode:boolean
  canMergeAudio:boolean
}>()

defineEmits<{
  clear:[];restore:[];purge:[];selectAll:[];open:[item:DriveFile];mergeAudio:[]
  extract:[item:DriveFile];download:[];share:[item:DriveFile];rename:[item:DriveFile];move:[];remove:[]
}>()
</script>

<template>
  <div class="selection-toolbar" role="toolbar" aria-label="所选项目操作">
    <button class="selection-close" title="取消选择" aria-label="取消选择" @click="$emit('clear')">×</button><span class="selection-summary"><b>{{ selectedItems.length }} 项</b><small>已选择 {{ formatSize(selectedBytes) }}</small></span>
    <div v-if="trashMode" class="selection-actions"><button @click="$emit('restore')"><svg viewBox="0 0 24 24" aria-hidden="true"><path d="M3 12a9 9 0 1 0 3-6.7L3 8"/><path d="M3 3v5h5"/></svg><span>恢复</span></button><button class="danger" @click="$emit('purge')"><svg viewBox="0 0 24 24" aria-hidden="true"><path d="M4 7h16M9 7V4h6v3m3 0-1 13H7L6 7m4 4v5m4-5v5"/></svg><span>永久删除</span></button></div>
    <div v-else class="selection-actions">
      <button @click="$emit('selectAll')"><svg viewBox="0 0 24 24" aria-hidden="true"><path d="M4 4h16v16H4z"/><path d="m8 12 3 3 5-6"/></svg><span>{{ selectedItems.length===itemCount?'取消全选':'全选' }}</span></button>
      <button v-if="singleSelected&&(singleSelected.kind==='directory'||isEditable(singleSelected)||isMedia(singleSelected)||isBook(singleSelected))" @click="$emit('open',singleSelected)"><svg viewBox="0 0 24 24" aria-hidden="true"><path v-if="singleSelected.kind==='directory'" d="M3 7h7l2 2h9v9H3z"/><path v-else-if="isEditable(singleSelected)&&!isBook(singleSelected)" d="m4 16-.8 4 4-.8L18.5 7.9l-3.2-3.2L4 16Z"/><path v-else-if="isBook(singleSelected)" d="M12 5c-1.7-1.4-4.2-2-8-2v14c3.8 0 6.3.6 8 2 1.7-1.4 4.2-2 8-2V3c-3.8 0-6.3.6-8 2Zm0 0v14"/><path v-else-if="isImage(singleSelected)" d="M2.5 12s3.5-6 9.5-6 9.5 6 9.5 6-3.5 6-9.5 6-9.5-6-9.5-6Z"/><path v-else d="M8 5v14l11-7Z"/></svg><span>{{ singleSelected.kind==='directory'?'打开':isBook(singleSelected)?'阅读':isEditable(singleSelected)?'编辑文本':isImage(singleSelected)?'预览':'播放' }}</span></button>
      <button v-if="canMergeAudio" @click="$emit('mergeAudio')"><svg viewBox="0 0 24 24" aria-hidden="true"><path d="M4 9v6M8 6v12M12 3v18M16 7v10M20 10v4"/></svg><span>合并音频</span></button>
      <button v-if="singleSelected&&isArchive(singleSelected)" @click="$emit('extract',singleSelected)"><svg viewBox="0 0 24 24" aria-hidden="true"><path d="M5 4h14v6H5zM5 14h14v6H5zM12 4v16M9 8h3m-3 4h3m-3 4h3"/></svg><span>在线解压</span></button>
      <button v-if="selectedFiles.length" @click="$emit('download')"><svg viewBox="0 0 24 24" aria-hidden="true"><path d="M12 3v12m0 0 4-4m-4 4-4-4M5 20h14"/></svg><span>下载{{ selectedFiles.length>1?` (${selectedFiles.length})`:'' }}</span></button>
      <button v-if="singleSelected?.kind==='file'" @click="$emit('share',singleSelected)"><svg viewBox="0 0 24 24" aria-hidden="true"><circle cx="18" cy="5" r="2.5"/><circle cx="6" cy="12" r="2.5"/><circle cx="18" cy="19" r="2.5"/><path d="m8.2 10.8 7.6-4.4M8.2 13.2l7.6 4.4"/></svg><span>分享</span></button>
      <button v-if="singleSelected" @click="$emit('rename',singleSelected)"><svg viewBox="0 0 24 24" aria-hidden="true"><path d="M5 5h14M12 5v14M9 19h6"/></svg><span>重命名</span></button>
      <button @click="$emit('move')"><svg viewBox="0 0 24 24" aria-hidden="true"><path d="M5 12h14m-5-5 5 5-5 5"/></svg><span>移动</span></button>
      <button class="danger" @click="$emit('remove')"><svg viewBox="0 0 24 24" aria-hidden="true"><path d="M4 7h16M9 7V4h6v3m3 0-1 13H7L6 7m4 4v5m4-5v5"/></svg><span>删除</span></button>
    </div>
  </div>
</template>

<script setup lang="ts">
import { reactive } from 'vue'
import { File, FileText, Folder, MoreHorizontal, Music2, Play } from 'lucide-vue-next'
import type { DriveFile } from '../api'
import { isArchive, isAudio, isBook, isEditable, isEpub, isImage, isMedia, isVideo, previewURL, thumbSRC } from '../fileTypes'
import { formatDate, formatSize } from '../format'
import VideoThumb from '../VideoThumb.vue'

defineProps<{items:DriveFile[];selectedIds:Set<string>;trashMode?:boolean}>()
defineEmits<{
  open:[item:DriveFile]
  select:[item:DriveFile]
  selectAll:[]
  edit:[item:DriveFile]
  preview:[item:DriveFile]
  read:[item:DriveFile]
  download:[item:DriveFile]
  extract:[item:DriveFile]
  share:[item:DriveFile]
  rename:[item:DriveFile]
  move:[item:DriveFile]
  remove:[item:DriveFile]
  restore:[item:DriveFile]
  purge:[item:DriveFile]
}>()

const thumbFallbackTried=reactive<Record<string,boolean>>({})
const imageBroken=reactive<Record<string,boolean>>({})
const coverBroken=reactive<Record<string,boolean>>({})

function thumbFallback(event:Event,item:DriveFile){
  const image=event.target as HTMLImageElement
  if(isAudio(item)){imageBroken[item.id]=true;return}
  if(thumbFallbackTried[item.id]){imageBroken[item.id]=true;return}
  thumbFallbackTried[item.id]=true
  image.src=previewURL(item)
}
</script>

<template>
  <div class="file-table">
    <div class="table-head"><span class="table-name-heading"><button class="table-select-all" :class="{active:selectedIds.size===items.length}" :title="selectedIds.size===items.length?'取消全选':'全选'" :aria-label="selectedIds.size===items.length?'取消全选':'全选'" :aria-pressed="selectedIds.size===items.length" @click="$emit('selectAll')"><svg viewBox="0 0 24 24" aria-hidden="true"><path v-if="selectedIds.size===items.length" d="m5 12 4 4L19 6"/><path v-else-if="selectedIds.size" d="M6 12h12"/></svg></button>名称</span><span>大小</span><span>修改时间</span><span>操作</span></div>
    <div v-for="item in items" :key="item.id" class="file-row" :class="{mutedrow:item.status!=='ready',selected:selectedIds.has(item.id)}" @dblclick="(!trashMode||item.kind==='file')&&$emit('open',item)">
      <div class="file-name">
        <button class="row-select" :class="{active:selectedIds.has(item.id)}" :title="selectedIds.has(item.id)?'取消选择':'选择项目'" :aria-label="selectedIds.has(item.id)?'取消选择':'选择项目'" :aria-pressed="selectedIds.has(item.id)" @click.stop="$emit('select',item)"><svg viewBox="0 0 24 24" aria-hidden="true"><path d="m5 12 4 4L19 6"/></svg></button>
        <button class="file-icon" :class="{directory:item.kind==='directory',image:isImage(item),document:isEditable(item),video:isVideo(item),audio:isAudio(item)}" :disabled="trashMode&&item.kind==='directory'" :title="trashMode&&item.kind==='directory'?'恢复后可打开文件夹':isBook(item)?'阅读':trashMode&&isEditable(item)?'只读查看':isEditable(item)?'编辑文档':isImage(item)?'预览图片':isVideo(item)?'播放视频':isAudio(item)?'播放音频':item.kind==='directory'?'打开文件夹':'文件'" @click="(!trashMode||item.kind==='file')&&$emit('open',item)">
          <Folder v-if="item.kind==='directory'" class="folder-glyph" aria-hidden="true" />
          <img v-else-if="(isImage(item)||isAudio(item))&&!imageBroken[item.id]" class="ui-image" :src="thumbSRC(item)" :alt="item.name" loading="lazy" draggable="false" @error="thumbFallback($event,item)">
          <VideoThumb v-else-if="isVideo(item)" :file="item"><Play aria-hidden="true" /></VideoThumb>
          <img v-else-if="isEpub(item)&&!coverBroken[item.id]" class="ui-image" :src="thumbSRC(item)" :alt="item.name" loading="lazy" draggable="false" @error="coverBroken[item.id]=true">
          <FileText v-else-if="isEpub(item)||isEditable(item)" aria-hidden="true" /><Music2 v-else-if="isAudio(item)" aria-hidden="true" /><File v-else aria-hidden="true" />
        </button>
        <div><strong :title="item.name">{{ item.name }}</strong><small v-if="trashMode">删除于 {{ formatDate(item.deleted_at||item.updated_at) }}</small><small v-else-if="item.status!=='ready'">{{ item.status }}</small><small v-else>{{ item.kind==='directory'?'文件夹':item.mime_type||'文件' }}</small></div>
      </div>
      <span>{{ item.kind==='directory'?'—':formatSize(item.size) }}</span><span>{{ formatDate(item.updated_at) }}</span>
      <div class="row-actions" :class="{'trash-actions':trashMode}">
        <template v-if="trashMode"><button v-if="isBook(item)||isEditable(item)||isMedia(item)" :title="isBook(item)?'阅读':isImage(item)?'预览':isMedia(item)?'播放':'只读查看'" :aria-label="isBook(item)?'阅读':isImage(item)?'预览':isMedia(item)?'播放':'只读查看'" @click="$emit('open',item)"><svg viewBox="0 0 24 24" aria-hidden="true"><path v-if="isBook(item)" d="M12 5c-1.7-1.4-4.2-2-8-2v14c3.8 0 6.3.6 8 2 1.7-1.4 4.2-2 8-2V3c-3.8 0-6.3.6-8 2Zm0 0v14"/><template v-else-if="isImage(item)"><path d="M2.5 12s3.5-6 9.5-6 9.5 6 9.5 6-3.5 6-9.5 6-9.5-6-9.5-6Z"/><circle cx="12" cy="12" r="2.6"/></template><path v-else-if="isMedia(item)" d="M8 5v14l11-7Z"/><path v-else d="M5 5h14M12 5v14M9 19h6"/></svg></button><button title="恢复" aria-label="恢复" class="restore-action" @click="$emit('restore',item)"><svg viewBox="0 0 24 24" aria-hidden="true"><path d="M3 12a9 9 0 1 0 3-6.7L3 8"/><path d="M3 3v5h5"/></svg></button><button title="永久删除" aria-label="永久删除" class="danger" @click="$emit('purge',item)"><svg viewBox="0 0 24 24" aria-hidden="true"><path d="M4 7h16M9 7V4h6v3m3 0-1 13H7L6 7m4 4v5m4-5v5"/></svg></button></template>
        <template v-else>
          <button v-if="isEditable(item)" title="编辑" aria-label="编辑" @click="$emit('edit',item)"><svg viewBox="0 0 24 24" aria-hidden="true"><path d="m4 16-.8 4 4-.8L18.5 7.9l-3.2-3.2L4 16Z"/><path d="m13.8 6.2 3.2 3.2"/></svg></button>
          <button v-if="isMedia(item)" :title="isImage(item)?'预览':'播放'" :aria-label="isImage(item)?'预览':'播放'" @click="$emit('preview',item)"><svg viewBox="0 0 24 24" aria-hidden="true"><template v-if="isImage(item)"><path d="M2.5 12s3.5-6 9.5-6 9.5 6 9.5 6-3.5 6-9.5 6-9.5-6-9.5-6Z"/><circle cx="12" cy="12" r="2.6"/></template><path v-else d="M8 5v14l11-7Z"/></svg></button>
          <button v-if="isBook(item)" title="阅读" aria-label="阅读" @click="$emit('read',item)"><svg viewBox="0 0 24 24" aria-hidden="true"><path d="M12 5c-1.7-1.4-4.2-2-8-2v14c3.8 0 6.3.6 8 2 1.7-1.4 4.2-2 8-2V3c-3.8 0-6.3.6-8 2Zm0 0v14"/></svg></button>
          <button v-if="item.kind==='file'" title="下载" aria-label="下载" @click="$emit('download',item)"><svg viewBox="0 0 24 24" aria-hidden="true"><path d="M12 3v12m0 0 4-4m-4 4-4-4M5 20h14"/></svg></button>
          <button v-if="isArchive(item)" title="在线解压" aria-label="在线解压" @click="$emit('extract',item)"><svg viewBox="0 0 24 24" aria-hidden="true"><path d="M5 4h14v6H5zM5 14h14v6H5zM12 4v16M9 8h3m-3 4h3m-3 4h3"/></svg></button>
          <button v-if="item.kind==='file'" title="分享" aria-label="分享" @click="$emit('share',item)"><svg viewBox="0 0 24 24" aria-hidden="true"><circle cx="18" cy="5" r="2.5"/><circle cx="6" cy="12" r="2.5"/><circle cx="18" cy="19" r="2.5"/><path d="m8.2 10.8 7.6-4.4M8.2 13.2l7.6 4.4"/></svg></button>
          <button title="重命名" aria-label="重命名" @click="$emit('rename',item)"><svg viewBox="0 0 24 24" aria-hidden="true"><path d="M5 5h14M12 5v14M9 19h6"/></svg></button>
          <button title="移动" aria-label="移动" @click="$emit('move',item)"><svg viewBox="0 0 24 24" aria-hidden="true"><path d="M5 12h14m-5-5 5 5-5 5"/></svg></button>
          <button title="删除" aria-label="删除" class="danger" @click="$emit('remove',item)"><svg viewBox="0 0 24 24" aria-hidden="true"><path d="M4 7h16M9 7V4h6v3m3 0-1 13H7L6 7m4 4v5m4-5v5"/></svg></button>
        </template>
      </div>
      <details class="row-menu">
        <summary aria-label="更多操作" title="更多操作"><MoreHorizontal aria-hidden="true" /></summary>
        <div class="row-menu-popover">
          <template v-if="trashMode"><button v-if="isBook(item)||isEditable(item)||isMedia(item)" @click="$emit('open',item)">{{ isBook(item)?'阅读':isImage(item)?'预览':isMedia(item)?'播放':'只读查看' }}</button><button @click="$emit('restore',item)">恢复</button><button class="danger" @click="$emit('purge',item)">永久删除</button></template>
          <template v-else>
            <button v-if="isEditable(item)" @click="$emit('edit',item)">编辑</button>
            <button v-if="isMedia(item)" @click="$emit('preview',item)">{{ isImage(item)?'预览':'播放' }}</button>
            <button v-if="isBook(item)" @click="$emit('read',item)">阅读</button>
            <button v-if="item.kind==='file'" @click="$emit('download',item)">下载</button>
            <button v-if="isArchive(item)" @click="$emit('extract',item)">在线解压</button>
            <button v-if="item.kind==='file'" @click="$emit('share',item)">分享</button>
            <button @click="$emit('rename',item)">重命名</button>
            <button @click="$emit('move',item)">移动</button>
            <button class="danger" @click="$emit('remove',item)">删除</button>
          </template>
        </div>
      </details>
    </div>
  </div>
</template>

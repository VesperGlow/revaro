<script setup lang="ts">
import { computed } from 'vue'
import { Images, LayoutGrid, List, RefreshCw, Upload } from '@lucide/vue'
import type { DriveFile } from '../api'
import { libraryViewStorageKey, type LibraryItem, type LibraryType } from '../library'
import { usePersistentMode } from '../composables/useLocalView'
import BookShelf from './BookShelf.vue'
import FileCard from './FileCard.vue'
import FileRows from './FileRows.vue'
import GalleryGrid from './GalleryGrid.vue'

const props=defineProps<{type:Exclude<LibraryType,'file'>;items:LibraryItem[];loading:boolean;error:string;filterLabel:string}>()
const emit=defineEmits<{open:[item:DriveFile];refresh:[];upload:[]}>()

const titles:Record<string,string>={book:'书架',image:'图片',video:'视频',audio:'音乐'}
const title=computed(()=>titles[props.type]||'媒体库')
const galleryMode=usePersistentMode(libraryViewStorageKey(props.type==='book'?'image':props.type,'gallery'),'all',['all','albums'] as const)
const mediaMode=usePersistentMode(libraryViewStorageKey('audio','media'),'grid',['grid','list'] as const)
const isGallery=computed(()=>props.type==='image'||props.type==='video')
</script>

<template>
  <section class="library-view">
    <header class="library-head">
      <div class="library-heading">
        <p class="eyebrow dark">{{ type==='book'?'BOOKSHELF':type==='image'?'PHOTOS':type==='video'?'VIDEOS':'MUSIC' }}</p>
        <h1>{{ title }}</h1>
        <p class="folder-meta"><span>{{ filterLabel }}</span><i></i><span>{{ items.length }} 个项目</span></p>
      </div>
      <div class="actions">
        <div v-if="type==='audio'" class="view-switch" role="group" aria-label="音乐视图切换">
          <button type="button" :class="{active:mediaMode==='grid'}" :aria-pressed="mediaMode==='grid'" @click="mediaMode='grid'"><LayoutGrid aria-hidden="true" />方块</button>
          <button type="button" :class="{active:mediaMode==='list'}" :aria-pressed="mediaMode==='list'" @click="mediaMode='list'"><List aria-hidden="true" />列表</button>
        </div>
        <button type="button" class="secondary" @click="emit('refresh')"><RefreshCw aria-hidden="true" />刷新</button>
        <button type="button" class="primary" @click="emit('upload')"><Upload aria-hidden="true" />上传</button>
      </div>
    </header>

    <div v-if="loading" class="state"><div class="spinner"></div><p>正在整理{{ title }}…</p></div>
    <div v-else-if="error" class="state empty"><div class="empty-icon">!</div><h3>读取失败</h3><p>{{ error }}</p><button class="secondary" @click="emit('refresh')">重试</button></div>
    <div v-else-if="!items.length" class="state empty"><div class="empty-icon">⌁</div><h3>这里还没有{{ title }}内容</h3><p>上传后会自动归类到这里。</p><button class="primary" @click="emit('upload')">上传文件</button></div>

    <template v-else>
      <BookShelf v-if="type==='book'" :items="items" @open="emit('open',$event)" />
      <template v-else-if="isGallery">
        <GalleryGrid :items="items" :mode="galleryMode" @open="emit('open',$event)" />
        <div class="gallery-switch" role="group" aria-label="图库视图切换">
          <button type="button" :class="{active:galleryMode==='all'}" :aria-pressed="galleryMode==='all'" @click="galleryMode='all'"><LayoutGrid aria-hidden="true" />全部视图</button>
          <button type="button" :class="{active:galleryMode==='albums'}" :aria-pressed="galleryMode==='albums'" @click="galleryMode='albums'"><Images aria-hidden="true" />图库分类</button>
        </div>
      </template>
      <FileRows v-else-if="mediaMode==='list'" :items="items" mode="audio" @open="emit('open',$event)" />
      <div v-else class="file-grid"><FileCard v-for="item in items" :key="item.id" :item="item" :selectable="false" @open="emit('open',$event)" /></div>
    </template>
  </section>
</template>

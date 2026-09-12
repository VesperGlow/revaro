<script setup lang="ts">
import { computed } from 'vue'
import type { DriveFile } from '../api'
import { groupAlbums, sortByUpdatedDesc, type GalleryMode, type LibraryItem } from '../library'
import FileCard from './FileCard.vue'

const props=defineProps<{items:LibraryItem[];mode:GalleryMode}>()
const emit=defineEmits<{open:[item:DriveFile]}>()

const sorted=computed(()=>sortByUpdatedDesc(props.items))
const albums=computed(()=>groupAlbums(props.items).map(album=>({...album,items:sortByUpdatedDesc(album.items)})))
</script>

<template>
  <div v-if="mode==='albums'" class="album-list">
    <section v-for="album in albums" :key="album.id" class="album">
      <header><div><h3>{{ album.title }}</h3><p>{{ album.path }}</p></div><span>{{ album.items.length }} 项</span></header>
      <div class="file-grid">
        <FileCard v-for="item in album.items" :key="item.id" :item="item" :selectable="false" @open="emit('open',$event)" />
      </div>
    </section>
  </div>
  <div v-else class="file-grid">
    <FileCard v-for="item in sorted" :key="item.id" :item="item" :selectable="false" @open="emit('open',$event)" />
  </div>
</template>

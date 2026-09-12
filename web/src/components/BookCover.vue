<script setup lang="ts">
import { ref } from 'vue'
import type { DriveFile } from '../api'
import { thumbSRC } from '../fileTypes'

const props=defineProps<{item:DriveFile;showTitle?:boolean}>()
const broken=ref(false)
const title=props.item.name.replace(/\.[^.]+$/,'')
</script>

<template>
  <span class="book-cover">
    <img v-if="!broken" class="ui-image" :src="thumbSRC(item)" :alt="item.name" loading="lazy" draggable="false" @error="broken=true">
    <span v-else class="book-cover-fallback"><b>{{ title.slice(0,1) }}</b><small v-if="showTitle">{{ title }}</small></span>
  </span>
</template>

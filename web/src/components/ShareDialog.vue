<script setup lang="ts">
import type { DriveFile } from '../api'
import { formatDate } from '../format'

defineProps<{file:DriveFile|null;active:boolean;url:string;createdAt:string;busy:boolean;error:string;copied:boolean}>()
defineEmits<{close:[];copy:[];revoke:[];create:[regenerate:boolean]}>()
</script>

<template>
  <section class="modal share-modal">
    <header><div class="share-title"><span>↗</span><div><h2>分享文件</h2><p :title="file?.name">{{ file?.name }}</p></div></div><button @click="$emit('close')">×</button></header>
    <div v-if="busy" class="state small"><div class="spinner"></div><p>正在准备分享…</p></div>
    <template v-else-if="active">
      <p class="share-description">任何拿到链接的人都能直接读取该文件。重新生成或停止分享后，旧链接立即失效。</p>
      <div class="share-link"><input :value="url" aria-label="分享链接" readonly @focus="($event.target as HTMLInputElement).select()"><button type="button" class="primary" @click="$emit('copy')">{{ copied?'已复制':'复制链接' }}</button></div>
      <p v-if="createdAt" class="share-created">公开链接 · 创建于 {{ formatDate(createdAt) }}</p><p v-if="error" class="form-error">{{ error }}</p>
      <footer class="share-footer"><button class="danger-text" :disabled="busy" @click="$emit('revoke')">停止分享</button><button class="secondary" :disabled="busy" @click="$emit('create',true)">重新生成链接</button></footer>
    </template>
    <template v-else><p class="share-description">创建后，无需登录即可通过链接读取这个文件。你可以随时重新生成或停止分享。</p><button class="primary share-create" :disabled="busy" @click="$emit('create',false)">创建公开链接</button><p v-if="error" class="form-error">{{ error }}</p></template>
  </section>
</template>

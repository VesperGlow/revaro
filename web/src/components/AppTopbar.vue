<script setup lang="ts">
import type { AudioMergeResponse, DownloadJob, UploadTask } from '../types'
import AudioMergeCenter from './AudioMergeCenter.vue'
import DownloadCenter from './DownloadCenter.vue'
import TransferCenter from './TransferCenter.vue'

defineProps<{
  user:string
  hasAvatar:boolean
  avatarUrl:string
  uploads:UploadTask[]
  audioMerges:AudioMergeResponse[]
	downloads:DownloadJob[]
	downloadParentId:string
}>()

defineEmits<{
  home:[]
  trash:[]
  account:[]
  logout:[]
  avatarError:[]
  clearUploads:[]
  cancelUpload:[task:UploadTask]
  retryUpload:[task:UploadTask]
  cancelAudioMerge:[job:AudioMergeResponse]
  clearAudioMerges:[]
	downloadsChanged:[]
}>()
</script>

<template>
  <header class="topbar">
    <button class="logo brand-button" title="回到我的文件" @click="$emit('home')">
      <span>revaro</span>
    </button>
    <div class="top-actions">
      <TransferCenter :uploads="uploads" @clear="$emit('clearUploads')" @cancel="$emit('cancelUpload',$event)" @retry="$emit('retryUpload',$event)" />
      <DownloadCenter :jobs="downloads" :parent-id="downloadParentId" @changed="$emit('downloadsChanged')" />
      <AudioMergeCenter v-if="audioMerges.length" :jobs="audioMerges" @cancel="$emit('cancelAudioMerge',$event)" @clear="$emit('clearAudioMerges')" />
      <button class="trash-button" title="回收站" aria-label="打开回收站" @click="$emit('trash')">
        <svg viewBox="0 0 24 24" aria-hidden="true"><path d="M4 7h16M9 7V4h6v3m3 0-1 13H7L6 7m4 4v5m4-5v5"/></svg>
      </button>
      <button class="account-button" title="打开账户设置" @click="$emit('account')">
        <span class="avatar-badge">
          <img v-if="hasAvatar" class="ui-image" :src="avatarUrl" alt="个人头像" draggable="false" @error="$emit('avatarError')">
          <template v-else>{{ user.slice(0,1).toUpperCase() }}</template>
        </span>
        <span class="account-copy"><b>{{ user }}</b><small>账户设置</small></span>
      </button>
      <button class="top-logout" @click="$emit('logout')">退出</button>
    </div>
  </header>
</template>

<style scoped>
.trash-button{display:grid;place-items:center;width:44px;height:44px;padding:0;border:0;border-radius:50%;background:transparent;color:#64748b}.trash-button:hover{background:#f1f5f9;color:#334155}.trash-button svg{width:20px;height:20px;fill:none;stroke:currentColor;stroke-width:1.7;stroke-linecap:round;stroke-linejoin:round}
@media(max-width:850px){.trash-button{width:40px;height:40px}.top-actions{gap:8px}}
@media(max-width:380px){.brand-button{display:none}.top-actions{gap:2px}.top-logout{font-size:11px}}
</style>

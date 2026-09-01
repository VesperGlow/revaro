<script setup lang="ts">
import { ref } from 'vue'
import type { DriveFile } from '../api'
import DirectoryPicker from './DirectoryPicker.vue'

const props=defineProps<{mode:'move'|'copy';targets:DriveFile[];busy:boolean;initialId?:string}>()
const emit=defineEmits<{close:[];select:[folderId:string]}>()
const targetText=props.targets.length===1?`“${props.targets[0]?.name}”`:`${props.targets.length} 项`
const targetId=ref(props.initialId||'00000000-0000-0000-0000-000000000000')
</script>

<template>
  <section class="modal move-copy-dialog">
    <header><div><p class="eyebrow dark">DIRECTORY</p><h2>{{ mode==='copy'?'复制到':'移动到' }}</h2><p>为 {{ targetText }} 选择目标文件夹</p></div><button type="button" aria-label="关闭" :disabled="busy" @click="emit('close')">×</button></header>
    <div class="move-copy-body"><DirectoryPicker :model-value="targetId" :excluded-ids="mode==='move'?targets.map(item=>item.id):[]" :disabled="busy" @change="folderId=>targetId=folderId"/></div>
    <footer><button type="button" class="secondary" :disabled="busy" @click="emit('close')">取消</button><button type="button" class="primary" :disabled="busy" @click="emit('select',targetId)">{{ busy?'正在处理…':mode==='copy'?'复制':'移动' }}</button></footer>
  </section>
</template>

<style scoped>
.move-copy-dialog{width:min(560px,calc(100vw - 28px))}.move-copy-dialog header p:last-child{margin:4px 0 0;color:#718096;font-size:12px}.move-copy-body{padding:18px 20px}@media(max-width:600px){.move-copy-dialog{width:calc(100vw - 20px)}.move-copy-body{padding:14px}}
</style>

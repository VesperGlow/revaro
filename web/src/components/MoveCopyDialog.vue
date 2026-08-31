<script setup lang="ts">
import type { DriveFile } from '../api'
import DirectoryPickerModal from './DirectoryPickerModal.vue'

const props=defineProps<{mode:'move'|'copy';targets:DriveFile[];busy:boolean;initialId?:string}>()
defineEmits<{close:[];select:[folderId:string]}>()
const targetText=props.targets.length===1?`“${props.targets[0]?.name}”`:`${props.targets.length} 项`
</script>

<template><DirectoryPickerModal :title="mode==='copy'?'复制到':'移动到'" :description="`为 ${targetText} 选择目标文件夹`" :initial-id="initialId" :excluded-ids="mode==='move'?targets.map(item=>item.id):[]" :busy="busy" @cancel="$emit('close')" @select="folderId=>$emit('select',folderId)" /></template>

<script setup lang="ts">
import { onBeforeUnmount, onMounted, ref } from 'vue'
import { RefreshCw, ServerCog } from 'lucide-vue-next'
import { api } from '../api'
import { formatSize } from '../format'
import type { SystemStatus } from '../types'

withDefaults(defineProps<{hideTrigger?:boolean}>(),{hideTrigger:false})
const panel=ref<HTMLDetailsElement|null>(null)
const status=ref<SystemStatus|null>(null)
const loading=ref(false)
const error=ref('')
async function refresh(){loading.value=true;error.value='';try{status.value=await api<SystemStatus>('/api/system/status',{},5000)}catch(e){error.value=(e as Error).message}finally{loading.value=false}}
function openPanel(){if(panel.value){panel.value.open=true;void refresh()}}
function closePanel(){if(panel.value)panel.value.open=false}
function toggle(){if(panel.value?.open&&!status.value&&!loading.value)void refresh()}
function outside(event:PointerEvent){const target=event.target;if(panel.value?.open&&target instanceof Node&&!panel.value.contains(target))closePanel()}
function escape(event:KeyboardEvent){if(event.key==='Escape'&&panel.value?.open){closePanel();panel.value.querySelector<HTMLElement>('summary')?.focus()}}
onMounted(()=>{document.addEventListener('pointerdown',outside);document.addEventListener('keydown',escape)})
onBeforeUnmount(()=>{document.removeEventListener('pointerdown',outside);document.removeEventListener('keydown',escape)})
defineExpose({openPanel,closePanel})
</script>

<template>
  <details ref="panel" class="system-status" @toggle="toggle">
    <summary v-show="!hideTrigger" title="系统状态" aria-label="打开系统状态"><ServerCog aria-hidden="true" /></summary>
    <section class="status-panel">
      <header><div><strong>系统状态</strong><small :class="status?.status">{{ status?.status==='degraded'?'部分服务异常':'运行概览' }}</small></div><button :disabled="loading" @click.prevent.stop="refresh"><RefreshCw :class="{spin:loading}" />刷新</button></header>
      <p v-if="error" class="status-error">{{ error }}</p>
      <p v-else-if="loading&&!status" class="status-empty">正在检查…</p>
      <div v-else-if="status" class="status-grid">
        <article><span :class="status.database.status"></span><div><b>数据库</b><small>{{ formatSize(status.database.bytes) }}</small></div></article>
        <article><span :class="status.storage.status"></span><div><b>S3 / 数据平面</b><small>{{ status.storage.status==='ok'?'可用':'异常' }}</small></div></article>
        <article><span :class="status.cache.status"></span><div><b>缓存</b><small>内存 {{ formatSize(status.cache.memory_bytes) }} · 磁盘 {{ formatSize(status.cache.disk_bytes) }}</small></div></article>
        <article><span :class="status.tasks.status"></span><div><b>任务</b><small>运行 {{ status.tasks.running }} · 排队 {{ status.tasks.queued }} · 等待 {{ status.tasks.waiting }} · 失败 {{ status.tasks.failed }}</small></div></article>
        <article><span :class="status.media_sessions.status"></span><div><b>媒体会话</b><small>音频 HLS {{ status.media_sessions.audio_hls }} · 视频 HLS {{ status.media_sessions.video_hls }} · fMP4 {{ status.media_sessions.fmp4 }}</small></div></article>
        <article><span :class="status.object_cleanup.status"></span><div><b>清理队列</b><small>{{ status.object_cleanup.pending }} 个对象待清理</small></div></article>
        <article><span :class="status.bt.status"></span><div><b>BT</b><small>{{ status.bt.enabled?(status.bt.available?'已启用 · 可用':'已启用 · 不可用'):'未启用' }}</small></div></article>
      </div>
    </section>
  </details>
</template>

<style scoped>
.system-status{position:relative}.system-status>summary{display:grid;place-items:center;width:44px;height:44px;border-radius:50%;cursor:pointer;list-style:none;color:#3d5f7e}.system-status>summary::-webkit-details-marker{display:none}.system-status>summary:hover{background:#f1f5f9}.system-status>summary svg{width:21px}.status-panel{position:absolute;z-index:45;top:52px;right:-12px;width:min(440px,calc(100vw - 24px));overflow:hidden;border:1px solid #dfe6ee;border-radius:17px;background:#fff;box-shadow:0 24px 70px #0f172a2e}.status-panel header{display:flex;align-items:center;justify-content:space-between;min-height:58px;padding:0 17px;border-bottom:1px solid #edf1f5}.status-panel header div{display:flex;flex-direction:column}.status-panel header small{color:#94a3b8;font-size:10px}.status-panel header small.degraded,.status-error{color:#b45309}.status-panel header button{display:flex;align-items:center;gap:5px;padding:7px 10px;border:0;border-radius:9px;background:#e0f2fe;color:#0369a1;font-weight:700}.status-panel header button:disabled{opacity:.6}.status-panel header svg{width:14px}.spin{animation:spin .8s linear infinite}.status-grid{display:grid;grid-template-columns:1fr 1fr;padding:8px}.status-grid article{display:flex;min-width:0;align-items:center;gap:10px;padding:12px 9px}.status-grid article>span{width:8px;height:8px;flex:0 0 auto;border-radius:50%;background:#22c55e}.status-grid article>span.degraded{background:#f59e0b}.status-grid article>div{display:flex;min-width:0;flex-direction:column}.status-grid b{font-size:12px}.status-grid small{overflow:hidden;color:#7b8a9d;font-size:10px;text-overflow:ellipsis;white-space:nowrap}.status-empty,.status-error{padding:28px;text-align:center}.status-empty{color:#94a3b8}@keyframes spin{to{transform:rotate(360deg)}}@media(max-width:850px){.status-panel{position:fixed;top:66px;right:10px}.status-grid{grid-template-columns:1fr}}
</style>

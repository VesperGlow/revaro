<script setup lang="ts">
import { onBeforeUnmount, onMounted, ref } from 'vue'
import { formatSize } from '../format'
import type { SystemStatus } from '../types'

withDefaults(defineProps<{hideTrigger?:boolean}>(),{hideTrigger:false})
const panel=ref<HTMLDetailsElement|null>(null)
const status=ref<SystemStatus|null>(null)
const error=ref('')
let source:EventSource|null=null
let reconnect=0
let retryDelay=1000
let stopped=false
function connect(){
  if(stopped||source)return
  source=new EventSource('/api/system/status/stream')
  source.addEventListener('status',event=>{try{status.value=JSON.parse((event as MessageEvent).data) as SystemStatus;error.value=''}catch{error.value='状态数据不可用'}})
  source.onopen=()=>{retryDelay=1000;window.clearTimeout(reconnect)}
  source.onerror=()=>{source?.close();source=null;window.clearTimeout(reconnect);reconnect=window.setTimeout(connect,retryDelay);retryDelay=Math.min(retryDelay*2,30_000)}
}
function openPanel(){if(panel.value)panel.value.open=true}
function closePanel(){if(panel.value)panel.value.open=false}
function outside(event:PointerEvent){const target=event.target;if(panel.value?.open&&target instanceof Node&&!panel.value.contains(target))closePanel()}
function escape(event:KeyboardEvent){if(event.key==='Escape'&&panel.value?.open){closePanel();panel.value.querySelector<HTMLElement>('summary')?.focus()}}
onMounted(()=>{connect();document.addEventListener('pointerdown',outside);document.addEventListener('keydown',escape)})
onBeforeUnmount(()=>{stopped=true;source?.close();window.clearTimeout(reconnect);document.removeEventListener('pointerdown',outside);document.removeEventListener('keydown',escape)})
defineExpose({openPanel,closePanel})
</script>

<template>
  <details ref="panel" class="system-status" :class="status?.status||'pending'">
    <summary v-show="!hideTrigger" title="系统状态" aria-label="打开系统状态"><i aria-hidden="true"></i></summary>
    <section class="status-panel">
      <div class="panel-ear"><i aria-hidden="true"></i></div>
      <header><strong>系统状态</strong><small :class="status?.status">{{ status?.status==='degraded'?'部分服务异常':'运行概览' }}</small></header>
      <p v-if="error" class="status-error">{{ error }}</p>
      <p v-else-if="!status" class="status-empty">正在获取状态…</p>
      <div v-else class="status-grid">
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
.system-status{position:relative;--orb:#2478d4;--pulse:4s}.system-status.degraded{--orb:#dc3545;--pulse:1.25s}.system-status.pending{--orb:#94a3b8}.system-status>summary{display:grid;place-items:center;width:44px;height:44px;cursor:pointer;list-style:none}.system-status>summary::-webkit-details-marker{display:none}.system-status i{display:block;width:15px;height:15px;border:3px solid #fff;border-radius:50%;background:var(--orb);box-shadow:0 0 0 1px color-mix(in srgb,var(--orb) 55%,transparent),0 0 0 5px color-mix(in srgb,var(--orb) 12%,transparent);animation:breathe var(--pulse) ease-in-out infinite}.status-panel{position:absolute;z-index:45;top:56px;right:-12px;width:min(440px,calc(100vw - 24px));overflow:visible;border:1px solid #dfe6ee;border-radius:17px;background:#fff;box-shadow:0 24px 70px #0f172a2e}.panel-ear{position:absolute;top:-18px;right:13px;display:grid;width:42px;height:25px;place-items:center;border:1px solid #dfe6ee;border-bottom:0;border-radius:20px 20px 0 0;background:#fff}.panel-ear:after{position:absolute;right:-2px;bottom:-2px;left:-2px;height:5px;background:#fff;content:''}.panel-ear i{position:relative;z-index:1}.status-panel header{display:flex;min-height:60px;flex-direction:column;align-items:center;justify-content:center;border-bottom:1px solid #edf1f5}.status-panel header small{color:#94a3b8;font-size:10px}.status-panel header small.degraded,.status-error{color:#dc2626}.status-grid{display:grid;grid-template-columns:1fr 1fr;padding:8px}.status-grid article{display:flex;min-width:0;align-items:center;gap:10px;padding:12px 9px}.status-grid article>span{width:8px;height:8px;flex:0 0 auto;border-radius:50%;background:#22c55e}.status-grid article>span.degraded{background:#ef4444}.status-grid article>div{display:flex;min-width:0;flex-direction:column}.status-grid b{font-size:12px}.status-grid small{overflow:hidden;color:#7b8a9d;font-size:10px;text-overflow:ellipsis;white-space:nowrap}.status-empty,.status-error{padding:28px;text-align:center}.status-empty{color:#94a3b8}@keyframes breathe{0%,100%{transform:scale(.9);box-shadow:0 0 0 1px color-mix(in srgb,var(--orb) 55%,transparent),0 0 0 4px color-mix(in srgb,var(--orb) 10%,transparent)}50%{transform:scale(1.08);box-shadow:0 0 0 1px color-mix(in srgb,var(--orb) 65%,transparent),0 0 0 9px color-mix(in srgb,var(--orb) 2%,transparent)}}@media(prefers-reduced-motion:reduce){.system-status i{animation:none}}@media(max-width:850px){.status-panel{position:fixed;top:82px;right:max(10px,env(safe-area-inset-right,0px));width:calc(100vw - 20px - env(safe-area-inset-left,0px) - env(safe-area-inset-right,0px))}.panel-ear{right:14px}.status-grid{grid-template-columns:1fr}}
</style>

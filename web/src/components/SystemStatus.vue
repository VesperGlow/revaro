<script setup lang="ts">
import { onBeforeUnmount, onMounted, ref } from 'vue'
import { Cloud, Database, HardDrive, ListTodo, Magnet, Radio, Trash2 } from 'lucide-vue-next'
import { formatSize } from '../format'
import type { SystemStatus } from '../types'
import ServiceCard from './ServiceCard.vue'
import StatusBadge, { type StatusTone } from './StatusBadge.vue'

withDefaults(defineProps<{hideTrigger?:boolean}>(),{hideTrigger:false})
const panel=ref<HTMLDetailsElement|null>(null)
const status=ref<SystemStatus|null>(null)
const error=ref('')
let source:EventSource|null=null
let reconnect=0
let retryDelay=1000
let stopped=false
function tone(value:string):StatusTone{return value==='critical'?'danger':value==='degraded'?'warning':'success'}
function stateLabel(value:string){return value==='critical'?'异常':value==='degraded'?'需注意':'正常'}
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
      <header>
        <div><strong>系统状态</strong><small>核心服务与后台活动概览</small></div>
        <StatusBadge :tone="!status?'neutral':status.status==='degraded'?'warning':'success'" size="md">{{ !status?'连接中':status.status==='degraded'?'部分服务异常':'所有服务正常' }}</StatusBadge>
      </header>
      <p v-if="error" class="status-error">{{ error }}</p>
      <p v-else-if="!status" class="status-empty">正在获取状态…</p>
      <div v-else class="status-grid">
        <ServiceCard title="数据库" :detail="`数据占用 ${formatSize(status.database.bytes)}`" :badge="stateLabel(status.database.status)" :tone="tone(status.database.status)"><template #icon><Database /></template></ServiceCard>
        <ServiceCard title="S3 / 数据平面" detail="对象存储连接状态" :badge="status.storage.status==='ok'?'可用':'异常'" :tone="tone(status.storage.status)"><template #icon><Cloud /></template></ServiceCard>
        <ServiceCard title="缓存" :detail="`内存 ${formatSize(status.cache.memory_bytes)} · 磁盘 ${formatSize(status.cache.disk_bytes)}`" :badge="stateLabel(status.cache.status)" :tone="tone(status.cache.status)"><template #icon><HardDrive /></template></ServiceCard>
        <ServiceCard title="任务" :detail="`排队 ${status.tasks.queued} · 等待 ${status.tasks.waiting} · 失败 ${status.tasks.failed}`" :badge="`${status.tasks.running} 运行中`" :tone="tone(status.tasks.status)"><template #icon><ListTodo /></template></ServiceCard>
        <ServiceCard title="清理队列" :detail="`${status.object_cleanup.pending} 个对象待清理`" :badge="stateLabel(status.object_cleanup.status)" :tone="tone(status.object_cleanup.status)"><template #icon><Trash2 /></template></ServiceCard>
        <ServiceCard title="媒体会话" :detail="`音频 HLS ${status.media_sessions.audio_hls} · 视频 HLS ${status.media_sessions.video_hls} · fMP4 ${status.media_sessions.fmp4}`" :badge="stateLabel(status.media_sessions.status)" :tone="tone(status.media_sessions.status)"><template #icon><Radio /></template></ServiceCard>
        <ServiceCard title="BT" :detail="status.bt.enabled?'下载服务已启用':'下载服务未启用'" :badge="status.bt.enabled?(status.bt.available?'可用':'不可用'):'未启用'" :tone="status.bt.enabled?tone(status.bt.status):'neutral'"><template #icon><Magnet /></template></ServiceCard>
      </div>
    </section>
  </details>
</template>

<style scoped>
.system-status{position:relative;--orb:#2478d4;--pulse:3.8s;--halo-a:color-mix(in srgb,var(--orb) 28%,transparent);--halo-b:color-mix(in srgb,var(--orb) 12%,transparent)}.system-status.degraded{--orb:#dc2638;--pulse:.95s;--halo-a:#dc263866;--halo-b:#dc26382e}.system-status.pending{--orb:#94a3b8}.system-status>summary{position:relative;z-index:47;display:grid;width:44px;height:44px;place-items:center;cursor:pointer;list-style:none}.system-status>summary::-webkit-details-marker{display:none}.system-status>summary i{display:block;width:16px;height:16px;border:3px solid #fff;border-radius:50%;background:var(--orb);animation:breathe var(--pulse) ease-in-out infinite}.status-panel{position:absolute;z-index:45;top:42px;left:50%;width:min(520px,calc(100vw - 24px));overflow:visible;transform:translateX(-50%);border:1px solid #dce6f1;border-radius:18px;background:#fbfdff;box-shadow:0 24px 70px #0f172a26}.status-panel:before{position:absolute;z-index:-1;top:-17px;left:50%;width:46px;height:30px;transform:translateX(-50%);border:1px solid #dce6f1;border-bottom:0;border-radius:25px 25px 0 0;background:#fbfdff;content:''}.status-panel:after{position:absolute;top:0;left:calc(50% - 24px);width:48px;height:8px;background:#fbfdff;content:''}.status-panel header{display:flex;min-height:76px;align-items:center;justify-content:space-between;gap:18px;padding:14px 18px;border-bottom:1px solid #e8eef5}.status-panel header>div{display:flex;min-width:0;flex-direction:column;gap:3px}.status-panel header strong{color:#172033;font-size:16px;letter-spacing:-.01em}.status-panel header small{color:#8291a5;font-size:11px}.status-grid{display:grid;grid-template-columns:repeat(2,minmax(0,1fr));gap:9px;padding:12px}.status-empty,.status-error{padding:36px;text-align:center}.status-empty{color:#94a3b8}.status-error{color:#dc2626}@keyframes breathe{0%,100%{transform:scale(.82);box-shadow:0 0 0 1px color-mix(in srgb,var(--orb) 70%,transparent),0 0 0 5px var(--halo-a),0 0 0 9px var(--halo-b)}50%{transform:scale(1.16);box-shadow:0 0 0 2px color-mix(in srgb,var(--orb) 75%,transparent),0 0 0 10px color-mix(in srgb,var(--halo-a) 60%,transparent),0 0 0 17px transparent}}@media(prefers-reduced-motion:reduce){.system-status>summary i{animation:none}}@media(min-width:851px){.status-panel{position:fixed;top:64px;right:16px;left:auto;transform:none}.status-panel:before,.status-panel:after{display:none}}@media(max-width:850px){.status-panel{position:fixed;top:64px;left:50%;width:calc(100vw - 20px - env(safe-area-inset-left,0px) - env(safe-area-inset-right,0px));max-height:calc(100dvh - 76px);overflow-y:auto}.system-status[open] .status-panel:before,.system-status[open] .status-panel:after{display:none}}@media(max-width:430px){.status-panel header{min-height:70px;padding:12px 14px}.status-panel header small{font-size:10px}.status-grid{gap:7px;padding:9px}}
</style>

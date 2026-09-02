<script setup lang="ts">
import type { StatusTone } from './StatusBadge.vue'
import StatusBadge from './StatusBadge.vue'
withDefaults(defineProps<{title:string;detail:string;badge:string;tone?:StatusTone}>(),{tone:'success'})
</script>

<template><article class="service-card" :class="`tone-${tone}`"><div class="service-card-head"><span class="service-icon" aria-hidden="true"><slot name="icon" /></span><StatusBadge :tone="tone">{{ badge }}</StatusBadge></div><div class="service-copy"><b>{{ title }}</b><small>{{ detail }}</small></div></article></template>

<style scoped>
.service-card{display:flex;min-width:0;min-height:112px;flex-direction:column;justify-content:space-between;padding:13px;border:1px solid #e5ecf4;border-radius:13px;background:#fff;box-shadow:0 2px 8px #1e3a5f08}.service-card-head{display:flex;align-items:center;justify-content:space-between;gap:8px}.service-icon{display:grid;width:34px;height:34px;place-items:center;border-radius:10px;background:#eaf3ff;color:#2474c8}.service-icon :deep(svg){width:18px;height:18px;stroke-width:1.9}.service-copy{display:flex;min-width:0;flex-direction:column;gap:3px}.service-copy b{color:#26364b;font-size:13px}.service-copy small{color:#718198;font-size:10.5px;line-height:1.45}.tone-warning .service-icon{background:#fff3e3;color:#b56208}.tone-danger .service-icon{background:#ffebed;color:#c6283b}.tone-neutral .service-icon{background:#f1f5f9;color:#64748b}@media(max-width:430px){.service-card{min-height:108px;padding:11px}.service-icon{width:31px;height:31px;border-radius:9px}.service-icon :deep(svg){width:17px;height:17px}.service-copy small{font-size:10px}}
</style>

<script setup lang="ts">
export type StatusTone='neutral'|'info'|'success'|'warning'|'danger'
withDefaults(defineProps<{tone?:StatusTone;size?:'sm'|'md'}>(),{tone:'neutral',size:'sm'})
</script>

<template><span class="status-badge" :class="[`tone-${tone}`,`size-${size}`]"><slot /></span></template>

<style scoped>
.status-badge{display:inline-flex;max-width:100%;align-items:center;justify-content:center;flex:0 0 auto;border:1px solid transparent;border-radius:999px;font-weight:750;line-height:1;white-space:nowrap}.size-sm{min-height:22px;padding:0 8px;font-size:10px}.size-md{min-height:27px;padding:0 10px;font-size:11px}.tone-neutral{border-color:#e2e8f0;background:#f4f7fa;color:#64748b}.tone-info{border-color:#dbeafe;background:#edf5ff;color:#1d64bd}.tone-success{border-color:#d4efdf;background:#eaf8f0;color:#167447}.tone-warning{border-color:#fde7c5;background:#fff4e5;color:#a95808}.tone-danger{border-color:#ffd5da;background:#ffedef;color:#bd2638}
</style>

<script setup lang="ts">
import { onBeforeUnmount, onMounted, ref } from 'vue'

defineProps<{ label: string }>()
const emit = defineEmits<{ change: [open: boolean] }>()
const menu = ref<HTMLDetailsElement | null>(null)
function close(restoreFocus = false) {
  if (!menu.value?.open) return
  menu.value.open = false
  if (restoreFocus) menu.value.querySelector('summary')?.focus()
}
function outside(event: PointerEvent) {
  if (event.target instanceof Node && !menu.value?.contains(event.target)) close()
}
function onKey(event: KeyboardEvent) {
  if (event.key !== 'Escape' || !menu.value?.open) return
  event.preventDefault()
  event.stopPropagation()
  close(true)
}
onMounted(() => document.addEventListener('pointerdown', outside))
onBeforeUnmount(() => document.removeEventListener('pointerdown', outside))
</script>

<template>
  <details ref="menu" class="preview-menu" @toggle="emit('change', !!menu?.open)" @keydown="onKey">
    <summary :aria-label="label" :title="label"><slot name="trigger"></slot></summary>
    <div class="preview-menu-panel"><slot :close="close"></slot></div>
  </details>
</template>

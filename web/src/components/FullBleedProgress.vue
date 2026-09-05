<script setup lang="ts">
import { computed } from 'vue'

defineOptions({ inheritAttrs: false })

interface ProgressMarker {
  id: string | number
  percent: number
}

interface ProgressTooltip {
  percent: number
  text: string
}

const props = withDefaults(defineProps<{
  percent: number
  bufferedPercent?: number
  markers?: ProgressMarker[]
  tooltip?: ProgressTooltip
  variant?: 'reader' | 'audio'
}>(), {
  bufferedPercent: 0,
  markers: () => [],
  tooltip: undefined,
  variant: 'reader',
})

const clampPercent = (value: number) => `${Math.max(0, Math.min(100, Number.isFinite(value) ? value : 0))}%`
const playedWidth = computed(() => clampPercent(props.percent))
const bufferedWidth = computed(() => clampPercent(props.bufferedPercent))
</script>

<template>
  <div class="full-bleed-progress" :class="`full-bleed-progress--${variant}`">
    <div class="full-bleed-progress__track" aria-hidden="true">
      <span class="full-bleed-progress__buffer" :style="{ width: bufferedWidth }"></span>
      <span class="full-bleed-progress__played" :style="{ width: playedWidth }"></span>
      <i v-for="marker in markers" :key="marker.id" :style="{ left: clampPercent(marker.percent) }"></i>
    </div>
    <div class="full-bleed-progress__runway" aria-hidden="true">
      <span class="full-bleed-progress__thumb" :style="{ left: playedWidth }"></span>
      <output
        v-if="tooltip"
        class="full-bleed-progress__tooltip"
        :style="{ left: `clamp(28px, ${clampPercent(tooltip.percent)}, calc(100% - 28px))` }"
      >{{ tooltip.text }}</output>
    </div>
    <input v-bind="$attrs" class="full-bleed-progress__input" type="range">
  </div>
</template>

<style scoped>
.full-bleed-progress {
  --progress-center: 0px;
  --progress-height: 4px;
  --progress-thumb-size: 18px;
  --progress-edge-start: max(calc(var(--progress-thumb-size) / 2), env(safe-area-inset-left, 0px));
  --progress-edge-end: max(calc(var(--progress-thumb-size) / 2), env(safe-area-inset-right, 0px));
  position: relative;
  z-index: 2;
  width: 100%;
  height: 22px;
  min-width: 0;
  overflow: visible;
  touch-action: pan-y;
}

.full-bleed-progress__track {
  position: absolute;
  top: var(--progress-center);
  right: 0;
  left: 0;
  height: var(--progress-height);
  overflow: hidden;
  background: var(--progress-track, var(--line));
  pointer-events: none;
  transform: translateY(-50%);
}

.full-bleed-progress__buffer,
.full-bleed-progress__played {
  position: absolute;
  inset: 0 auto 0 0;
  pointer-events: none;
}

.full-bleed-progress__buffer {
  background: var(--progress-buffer, transparent);
}

.full-bleed-progress__played {
  background: var(--progress-played, var(--accent));
}

.full-bleed-progress__track > i {
  position: absolute;
  z-index: 1;
  top: 50%;
  width: 2px;
  height: calc(var(--progress-height) + 4px);
  background: var(--progress-marker, #fff);
  box-shadow: 0 0 0 1px #7990ad52;
  pointer-events: none;
  transform: translate(-50%, -50%);
}

.full-bleed-progress__runway {
  position: absolute;
  top: var(--progress-center);
  right: var(--progress-edge-end);
  left: var(--progress-edge-start);
  height: 0;
  pointer-events: none;
}

.full-bleed-progress__thumb {
  position: absolute;
  z-index: 2;
  top: 0;
  width: var(--progress-thumb-size);
  height: var(--progress-thumb-size);
  border: 2px solid var(--progress-thumb-border, #fff);
  border-radius: 50%;
  background: var(--progress-thumb, var(--accent));
  box-shadow: 0 1px 4px #00000040;
  transform: translate(-50%, -50%);
}

.full-bleed-progress__tooltip {
  position: absolute;
  z-index: 4;
  bottom: 14px;
  min-width: 48px;
  padding: 5px 8px;
  border-radius: 7px;
  background: #1f2937;
  color: #fff;
  font: 10px ui-monospace, SFMono-Regular, Consolas, monospace;
  text-align: center;
  white-space: nowrap;
  transform: translateX(-50%);
  box-shadow: 0 5px 16px #0f172a2e;
}

.full-bleed-progress__tooltip::after {
  position: absolute;
  top: 100%;
  left: 50%;
  border: 4px solid transparent;
  border-top-color: #1f2937;
  content: "";
  transform: translateX(-50%);
}

.full-bleed-progress__input {
  -webkit-appearance: none;
  appearance: none;
  box-sizing: border-box;
  position: absolute;
  z-index: 5;
  top: calc(var(--progress-center) - 22px);
  right: 0;
  left: 0;
  width: 100%;
  height: 44px;
  margin: 0;
  padding-right: var(--progress-edge-end);
  padding-left: var(--progress-edge-start);
  border: 0;
  background: transparent;
  cursor: grab;
  opacity: 0;
  touch-action: pan-y;
}

.full-bleed-progress__input:active {
  cursor: grabbing;
}

.full-bleed-progress__input:focus-visible {
  opacity: 1;
  outline: 2px solid var(--accent);
  outline-offset: -2px;
}

.full-bleed-progress__input::-webkit-slider-runnable-track {
  height: 44px;
  background: transparent;
}

.full-bleed-progress__input::-webkit-slider-thumb {
  -webkit-appearance: none;
  appearance: none;
  width: var(--progress-thumb-size);
  height: 44px;
  border: 0;
  background: transparent;
}

.full-bleed-progress__input::-moz-range-track {
  height: 44px;
  border: 0;
  background: transparent;
}

.full-bleed-progress__input::-moz-range-thumb {
  width: var(--progress-thumb-size);
  height: 44px;
  border: 0;
  background: transparent;
}

.full-bleed-progress__input:disabled {
  cursor: default;
}
</style>

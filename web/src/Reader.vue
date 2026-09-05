<script setup lang="ts">
import type { DriveFile } from './api'
import { useReaderFlow } from './composables/useReaderFlow'

const props = defineProps<{ file: DriveFile }>()
const emit = defineEmits<{ (e: 'close'): void }>()

const {
  FONT_MAX, FONT_MIN, LINE_HEIGHTS, adjustFont, clamp, closeToc, errorText,
  flowEl, fontOpen, isDark, jumpToc, loadingText, next, onFontInput,
  openToc, pageLabel, percentNow, prefs, previous, setLineHeight,
  stage, title, toc, tocActive, tocOpen, toggleTheme, toggleTools, toolsVisible,
  viewportEl, zoneGuard,
} = useReaderFlow(props.file)

</script>

<template>
  <section id="reader-view" class="reader-shell" :class="{ dark: isDark, 'tools-hidden': !toolsVisible }">
    <header class="reader-bar">
      <button id="reader-back" class="reader-icon-btn" aria-label="返回" @click="emit('close')">
        <svg class="reader-back-icon" viewBox="0 0 24 24" aria-hidden="true">
          <path d="M19 12H5m7-7-7 7 7 7" />
        </svg>
      </button>
      <div class="reader-bar-title"><strong id="reader-title">{{ title }}</strong></div>
      <span id="page-label" class="reader-progress-ring" role="img" :aria-label="`阅读进度 ${pageLabel}`">
        <svg viewBox="0 0 40 40" aria-hidden="true">
          <circle class="reader-progress-ring-track" cx="20" cy="20" r="17.5" pathLength="100" />
          <circle
            class="reader-progress-ring-value"
            :class="{ empty: percentNow <= 0 }"
            cx="20"
            cy="20"
            r="17.5"
            pathLength="100"
            stroke-dasharray="100"
            :stroke-dashoffset="100 - clamp(percentNow, 0, 100)"
          />
        </svg>
        <b>{{ Math.round(clamp(percentNow, 0, 100)) }}</b>
      </span>
    </header>
    <main id="viewport" ref="viewportEl" class="reader-viewport rf-viewport">
      <div class="rf-pager">
        <div id="flow" ref="flowEl" class="rf-flow revaro-content"></div>
      </div>
      <div id="loading" class="reader-loading" v-if="stage === 'loading'">{{ loadingText }}</div>
      <div class="reader-loading" v-else-if="stage === 'error'">
        {{ errorText }}
        <button class="font-step" style="margin-top: 12px" @click="emit('close')">关闭</button>
      </div>
      <button id="prev-zone" class="page-zone prev-zone" aria-label="上一页" @click="zoneGuard(previous)()"></button>
      <button id="center-zone" class="page-zone center-zone" aria-label="显示或隐藏工具栏" @click="zoneGuard(toggleTools)()"></button>
      <button id="next-zone" class="page-zone next-zone" aria-label="下一页" @click="zoneGuard(next)()"></button>
    </main>
    <div id="toc-scrim" class="toc-scrim" :class="{ hidden: !tocOpen }" @click="closeToc"></div>
    <aside id="toc-drawer" class="toc-drawer" aria-label="书籍目录" :aria-hidden="!tocOpen" :class="{ open: tocOpen }">
      <div class="toc-heading"><div><small>CONTENTS</small><h2>目录</h2></div><button id="toc-close" aria-label="关闭目录" @click="closeToc">×</button></div>
      <nav id="toc-list" class="toc-list">
        <p v-if="!toc.length" class="toc-empty">这本书没有可用目录。</p>
        <button
          v-for="(entry, index) in toc"
          :key="index"
          class="toc-item"
          :class="{ active: tocActive === index }"
          :style="{ '--toc-indent': `${Math.min(4, entry.depth || 0) * 16}px` }"
          @click="jumpToc(index)"
        >
          {{ entry.label }}
        </button>
      </nav>
    </aside>
    <div id="font-popover" class="font-popover" :class="{ hidden: !fontOpen }">
      <span>字号</span>
      <button id="font-smaller" class="font-step" aria-label="减小字号" @click="adjustFont(-1)">A−</button>
      <input id="font-slider" type="range" :min="FONT_MIN" :max="FONT_MAX" step="1" :value="prefs.fontSize" aria-label="阅读字号" @input="onFontInput">
      <button id="font-larger" class="font-step" aria-label="增大字号" @click="adjustFont(1)">A+</button>
      <span class="v2-lineheight">
        <span>行距</span>
        <button
          v-for="lh in LINE_HEIGHTS"
          :key="lh"
          class="font-step"
          :class="{ 'v2-active': prefs.lineHeight === lh }"
          @click="setLineHeight(lh)"
        >{{ lh }}</button>
      </span>
    </div>
    <footer class="reader-footer">
      <div class="reader-actions">
        <button id="toc-button" class="reader-action-btn" :aria-expanded="tocOpen" @click="openToc"><b>☰</b><span>目录</span></button>
        <button id="font-button" class="reader-action-btn" @click="fontOpen = !fontOpen"><b>A</b><span>排版</span></button>
        <button id="theme-button" class="reader-action-btn" @click="toggleTheme"><b>◐</b><span>明暗</span></button>
      </div>
    </footer>
  </section>
</template>

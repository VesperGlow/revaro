<script setup lang="ts">
// 服务端固定分页阅读器（v2）：客户端严格保持三页窗口（上一页/当前页/下一页），
// 不做任何客户端分页。翻页热路径零网络、零重排：前后页提前预取进缓存，
// 点击只做合成层 transform。字号/行距/旋转等布局变化走「旧 layout 继续读，
// 新 profile 后台生成，完成后按 readingAnchor 无缝切换」。
import { computed, onBeforeUnmount, onMounted, reactive, ref, shallowRef } from 'vue'
import type { DriveFile } from './api'
import type { BookProgress, LayoutManifest, LayoutProfile, ReaderPrefs } from './reader/types'
import { fetchPageByURL, fetchProgress, saveProgress, submitProfile, waitForReadable, LayoutSuperseded } from './reader/api'
import { pageForAnchor, anchorForPage, clampPage, tocActiveIndex } from './reader/manifest'
import { PageCache, InFlight } from './reader/cache'
import { buildProfile, loadPrefs, savePrefs, FONT_MIN, FONT_MAX, LINE_HEIGHTS } from './reader/prefs'

const props = defineProps<{ file: DriveFile }>()
const emit = defineEmits<{ (e: 'close'): void }>()

const kind = computed(() => (/\.epub$/i.test(props.file.name) ? 'epub' : 'txt'))
const prefs = reactive<ReaderPrefs>(loadPrefs())

const viewportEl = ref<HTMLElement | null>(null)
const trackEl = ref<HTMLElement | null>(null)
const slotEls = ref<(HTMLElement | null)[]>([])
const slotRefs = [1, 2, 3].map(i => (el: unknown) => {
  slotEls.value[i - 1] = (el as HTMLElement) || null
})

const stage = ref<'loading' | 'reading' | 'error'>('loading')
const loadingText = ref('正在打开书页…')
const errorText = ref('')
const manifest = shallowRef<LayoutManifest | null>(null)
const current = ref(0)
const profileID = ref('')
const title = ref(props.file.name)
const tocOpen = ref(false)
const fontOpen = ref(false)
const toolsVisible = ref(true)
const tocActive = ref(-1)
const relayout = reactive({ pending: false, pages: 0, spinesDone: 0, spinesTotal: 0 })

const pageCount = computed(() => manifest.value?.page_count ?? 1)
const pageLabel = computed(() => {
  if (!manifest.value) return '— / — · 0%'
  const pct = pageCount.value <= 1 ? 100 : Math.round((current.value / (pageCount.value - 1)) * 100)
  return `${current.value + 1} / ${pageCount.value} · ${pct}%`
})
const toc = computed(() => manifest.value?.toc ?? [])

// ---- 三页窗口（非响应式的槽位簿记；DOM 由 syncSlots 统一写入） ----
const cache = new PageCache(12)
const inFlight = new InFlight()
let rolePages: number[] = [] // [左, 中, 右] 三槽当前的绝对页码
let domPages: number[] = [] // 三槽当前已渲染的页码
let turnBusy = false
let pendingTurns = 0 // 动画期间的连点累加排队：结束后续翻
let currentAnim: Animation | null = null
let suppressZoneClickUntil = 0
let lastDir: -1 | 1 = 1 // 最近一次翻页方向：预取按阅读方向不对称分配
let currentAnchor: { spine: number; path: number[]; offset: number } | null = null
let manifestTimer: ReturnType<typeof setTimeout> | null = null
let manifestGen = 0 // 当前布局的代数（重排/关闭后旧轮询自动失效）
let relayoutVersion = 0
let progressTimer: ReturnType<typeof setTimeout> | null = null
let relayoutTimer: ReturnType<typeof setTimeout> | null = null
let resizeTimer: ReturnType<typeof setTimeout> | null = null
let lastViewport = { w: 0, h: 0 }

function pageW(): number {
  return manifest.value?.profile.viewport_w ?? 0
}

function setTrackX(x: number) {
  const el = trackEl.value
  if (!el) return
  el.style.transition = 'none'
  el.style.transform = `translateX(${x}px)`
}

function animateTrack(from: number, to: number): Promise<void> {
  currentAnim?.cancel()
  return new Promise(resolve => {
    const el = trackEl.value
    if (!el) {
      resolve()
      return
    }
    setTrackX(from)
    currentAnim = el.animate(
      [{ transform: `translateX(${from}px)` }, { transform: `translateX(${to}px)` }],
      { duration: 260, easing: 'cubic-bezier(.22,.72,.26,1)' },
    )
    currentAnim.onfinish = () => {
      currentAnim = null
      resolve()
    }
  })
}

// pageURLFor 返回某全局页码的页面 URL；页码超出当前快照（渐进式分页
// 尚未生成）返回 null。
function pageURLFor(p: number): string | null {
  const m = manifest.value
  if (!m || p < 0 || p >= m.pages.length) return null
  return m.pages[p].url ?? null
}

// syncSlots 让三槽内容与 rolePages 一致；只写内容变化/屏外槽，不触发布局。
function syncSlots() {
  if (!manifest.value) return
  rolePages = [current.value - 1, current.value, current.value + 1]
  for (let i = 0; i < 3; i++) {
    const el = slotEls.value[i]
    if (!el) continue
    el.style.transform = `translateX(${(i - 1) * pageW()}px)`
    const p = rolePages[i]
    if (p === domPages[i]) continue
    domPages[i] = p
    const url = pageURLFor(p)
    el.innerHTML = url ? cache.get(url) ?? '' : ''
  }
}

function ensurePage(p: number): Promise<string> {
  const url = pageURLFor(p)
  if (!url) return Promise.resolve('') // 快照外：渐进式分页尚未生成该页
  const hit = cache.get(url)
  if (hit !== undefined) return Promise.resolve(hit)
  return inFlight.run(url, async () => {
    const html = await fetchPageByURL(url)
    cache.set(url, html)
    return html
  })
}

// 按阅读方向智能预取：前行时 [p+1, p+2, p+3, p-1]，后退时镜像。
// 只碰当前快照内已生成的页；快照增长后由 manifest 轮询补触发。
function prefetchAround(p: number, dir: -1 | 1 = 1) {
  const order = dir >= 0 ? [1, 2, 3, -1] : [-1, -2, -3, 1]
  for (const delta of order) {
    const q = p + delta
    if (q < 0 || q >= pageCount.value) continue
    const url = pageURLFor(q)
    if (!url || cache.has(url)) continue
    void ensurePage(q).then(() => syncSlots())
  }
}

async function turn(dir: -1 | 1, fromX = 0) {
  if (!manifest.value || stage.value !== 'reading') return
  lastDir = dir
  if (turnBusy) {
    pendingTurns += dir // 动画中连点：累加排队，动画结束后逐个续翻
    return
  }
  const target = current.value + dir
  if (target < 0 || target >= pageCount.value) return
  turnBusy = true
  try {
    await ensurePage(target)
    // 远侧槽（动画结束后轮换为新远侧）此刻在屏外，立即填入 target+dir 页
    const far = dir === 1 ? 0 : 2
    const farPage = target + dir
    if (farPage >= 0 && farPage < pageCount.value) {
      const html = await ensurePage(farPage)
      if (domPages[far] !== farPage) {
        domPages[far] = farPage
        const el = slotEls.value[far]
        if (el) el.innerHTML = html
      }
    }
    await animateTrack(fromX, -dir * pageW())
    current.value = target
    syncSlots()
    setTrackX(0)
    prefetchAround(target, dir)
    afterPageChange()
  } finally {
    turnBusy = false
    if (pendingTurns !== 0) {
      const next = pendingTurns > 0 ? 1 : -1
      pendingTurns -= next
      void turn(next)
    }
  }
}

async function seek(page: number) {
  if (!manifest.value || stage.value !== 'reading') return
  const target = clampPage(manifest.value, page)
  // 不做 target===current 的提前返回：滑块 input 会先更新 current 作预览，
  // 同页重渲染是幂等的（缓存命中 + 槽位内容不变），提前返回反而会漏掉
  // 「input 预览过但未渲染」的路径。
  turnBusy = true
  currentAnim?.cancel()
  try {
    await ensurePage(target) // 跳页允许一次网络等待；连续翻页不经过这里
    current.value = target
    syncSlots()
    setTrackX(0)
    prefetchAround(target, lastDir)
    afterPageChange()
  } finally {
    turnBusy = false
  }
}

function afterPageChange(save = true) {
  const m = manifest.value
  if (m) {
    currentAnchor = anchorForPage(m, current.value)
    tocActive.value = tocActiveIndex(m, current.value)
  }
  if (save) queueProgressSave()
  // 渐进式分页：当前快照尚未完整时，周期性拉取新快照补全后续页
  if (m && !m.complete && !manifestTimer) scheduleManifestPoll()
}

// ---- 进度（readingAnchor + profile，防抖保存） ----
function queueProgressSave() {
  if (progressTimer) clearTimeout(progressTimer)
  progressTimer = setTimeout(() => {
    progressTimer = null
    void saveNow()
  }, 1200)
}

async function saveNow() {
  const m = manifest.value
  if (!m) return
  const progress: BookProgress = {
    anchor: anchorForPage(m, current.value),
    profile: profileID.value,
  }
  try {
    await saveProgress(props.file.id, progress)
  } catch (error) {
    console.error('保存阅读进度失败', error)
  }
}

function flushProgress() {
  if (progressTimer) {
    clearTimeout(progressTimer)
    progressTimer = null
  }
  void saveNow()
}

// ---- 打开书籍 ----
async function open() {
  stage.value = 'loading'
  loadingText.value = '正在读取书籍…'
  const gen = ++manifestGen
  try {
    const progress = await fetchProgress(props.file.id).catch((): BookProgress => ({}))
    const profile = profileFromViewport()
    loadingText.value = '正在排版…'
    const status = await submitProfile(props.file.id, profile, progress.anchor ?? null)
    profileID.value = status.profile_id
    const startAnchor = progress.anchor ?? null
    // 渐进式：目标章完成即可读，不等全书生成完
    const m = await waitForReadable(props.file.id, status.profile_id, startAnchor, {
      onProgress: s => {
        const parts: string[] = ['正在排版…']
        if (s.spines_done != null && s.spines_total) parts.push(`${s.spines_done}/${s.spines_total} 章`)
        if (s.pages) parts.push(`${s.pages} 页`)
        loadingText.value = parts.join(' ')
      },
      pageForAnchor: (manifest, anchor) => pageForAnchor(manifest, anchor),
    })
    if (gen !== manifestGen) return // 组件已卸载或布局已重开
    manifest.value = m
    applyTrackSize()
    const start = resolveStartPage(m, progress)
    await ensurePage(start)
    current.value = start
    currentAnchor = anchorForPage(m, start)
    syncSlots()
    setTrackX(0)
    prefetchAround(start, 1)
    stage.value = 'reading'
    afterPageChange(false)
  } catch (error) {
    if (gen !== manifestGen) return
    stage.value = 'error'
    errorText.value = (error as Error).message || '打开失败'
  }
}

// scheduleManifestPoll 渐进式分页的快照轮询：当前快照不完整时周期性
// 拉取新快照（页码随前缀和增长而漂移），按当前 anchor 重映射页面。
function scheduleManifestPoll() {
  if (manifestTimer) return
  manifestTimer = setTimeout(() => void pollManifest(), 800)
}

async function pollManifest() {
  manifestTimer = null
  const m = manifest.value
  const gen = manifestGen
  if (!m || m.complete || stage.value !== 'reading') return
  try {
    const next = await fetchManifestSnapshot()
    if (gen !== manifestGen || !manifest.value) return
    handleManifestUpdate(next)
  } catch {
    /* 快照尚未发布：稍后再试 */
  }
  if (gen === manifestGen && manifest.value && !manifest.value.complete) scheduleManifestPoll()
}

async function fetchManifestSnapshot(): Promise<LayoutManifest> {
  const { fetchManifest } = await import('./reader/api')
  return fetchManifest(props.file.id, profileID.value)
}

function handleManifestUpdate(next: LayoutManifest) {
  const m = manifest.value
  if (!m) return
  const anchor = currentAnchor ?? anchorForPage(m, current.value)
  manifest.value = next
  const target = anchor ? clampPage(next, pageForAnchor(next, anchor)) : current.value
  current.value = target
  currentAnchor = anchor
  // URL 键缓存跨快照命中：页面对象不变，只是全局页码重映射
  syncSlots()
  prefetchAround(target, lastDir)
  tocActive.value = tocActiveIndex(next, target)
  if (next.complete) {
    if (manifestTimer) {
      clearTimeout(manifestTimer)
      manifestTimer = null
    }
    if (relayout.pending) relayout.pending = false
  }
}

function profileFromViewport(): LayoutProfile {
  const el = viewportEl.value
  const w = el?.clientWidth || window.innerWidth
  const h = el?.clientHeight || window.innerHeight
  lastViewport = { w, h }
  return buildProfile(w, h, prefs)
}

function applyTrackSize() {
  const m = manifest.value
  if (!m || !trackEl.value) return
  trackEl.value.style.width = `${m.profile.viewport_w}px`
  trackEl.value.style.height = `${m.profile.viewport_h}px`
}

function resolveStartPage(m: LayoutManifest, progress: BookProgress): number {
  // 恢复只靠 readingAnchor（跨 layout 稳定）；无锚点时从第一页开始
  if (progress.anchor) return clampPage(m, pageForAnchor(m, progress.anchor))
  return 0
}

// ---- 布局切换：旧 layout 继续读，新 profile 后台生成，完成后 anchor 无缝切换 ----
function scheduleRelayout() {
  if (!manifest.value || stage.value !== 'reading') return
  if (relayoutTimer) clearTimeout(relayoutTimer)
  relayoutTimer = setTimeout(() => void relayoutNow(), 600)
}

async function relayoutNow() {
  if (!manifest.value) return
  const version = ++relayoutVersion
  const anchor = currentAnchor ?? anchorForPage(manifest.value, current.value)
  relayout.pending = true
  try {
    const status = await submitProfile(props.file.id, profileFromViewport(), anchor)
    if (version !== relayoutVersion) return
    // 渐进式：新 profile 也是目标章先可读，就绪后按 anchor 无缝切换
    const m = await waitForReadable(props.file.id, status.profile_id, anchor, {
      aborted: () => version !== relayoutVersion,
      pageForAnchor: (manifest, a) => pageForAnchor(manifest, a),
      onProgress: s => {
        if (version === relayoutVersion) {
          relayout.pages = s.pages ?? 0
          relayout.spinesDone = s.spines_done ?? 0
          relayout.spinesTotal = s.spines_total ?? 0
        }
      },
    })
    if (version !== relayoutVersion) return
    manifest.value = m
    profileID.value = status.profile_id
    applyTrackSize()
    // 页缓存按 profile 隔离：切换 layout 时旧页面内容全部作废，强制重取
    cache.clear()
    domPages = [-2, -2, -2]
    const target = anchor ? clampPage(m, pageForAnchor(m, anchor)) : 0
    await ensurePage(target)
    current.value = target
    currentAnchor = anchor
    syncSlots()
    setTrackX(0)
    prefetchAround(target, lastDir)
    afterPageChange()
    // 新 profile 若只是部分生成，继续后台轮询直到 complete
    if (!m.complete) scheduleManifestPoll()
  } catch (error) {
    if (version === relayoutVersion && !(error instanceof LayoutSuperseded)) {
      console.error('重排失败，继续使用旧排版', error)
    }
  } finally {
    if (version === relayoutVersion) {
      // 已切到新 profile 且仍未生成完 → 指示器保持到 complete；
      // 失败/被取代（manifest 仍是旧 profile）→ 收起指示器，继续用旧排版。
      const m = manifest.value
      relayout.pending = Boolean(m && m.profile_id === profileID.value && !m.complete)
    }
  }
}

function onFontInput() {
  prefs.fontSize = Math.min(FONT_MAX, Math.max(FONT_MIN, Math.round(prefs.fontSize)))
  savePrefs(prefs)
  scheduleRelayout()
}

function adjustFont(delta: number) {
  prefs.fontSize = Math.min(FONT_MAX, Math.max(FONT_MIN, prefs.fontSize + delta))
  savePrefs(prefs)
  scheduleRelayout()
}

function setLineHeight(value: number) {
  prefs.lineHeight = value
  savePrefs(prefs)
  scheduleRelayout()
}

function onResize() {
  if (!manifest.value) return
  const el = viewportEl.value
  const w = el?.clientWidth ?? 0
  const h = el?.clientHeight ?? 0
  if (w === lastViewport.w && h === lastViewport.h) return
  if (resizeTimer) clearTimeout(resizeTimer)
  resizeTimer = setTimeout(() => scheduleRelayout(), 300)
}

// ---- 交互：热区/键盘/滑动 ----
function toggleTools() {
  toolsVisible.value = !toolsVisible.value
  if (!toolsVisible.value) fontOpen.value = false
}

function toggleTheme() {
  prefs.theme = prefs.theme === 'dark' ? 'light' : 'dark'
  savePrefs(prefs)
}

function openToc() {
  tocOpen.value = true
}

function closeToc() {
  tocOpen.value = false
}

function onKey(event: KeyboardEvent) {
  if (stage.value !== 'reading') return
  if (['ArrowLeft', 'ArrowRight', 'PageUp', 'PageDown', ' '].includes(event.key)) event.preventDefault()
  if (['ArrowLeft', 'PageUp'].includes(event.key)) void turn(-1)
  if (['ArrowRight', 'PageDown', ' '].includes(event.key)) void turn(1)
  if (event.key === 'Escape' && tocOpen.value) closeToc()
}

function onSliderInput(event: Event) {
  const value = Number((event.target as HTMLInputElement).value)
  if (!manifest.value || !Number.isFinite(value)) return
  current.value = clampPage(manifest.value, value)
  // 滑过已缓存的页（预取窗口内）直接预览；未缓存的等 change 事件 seek 拉取
  const url = pageURLFor(current.value)
  if (url && cache.has(url)) {
    syncSlots()
    setTrackX(0)
    tocActive.value = tocActiveIndex(manifest.value, current.value)
  }
}

function onSliderChange() {
  void seek(current.value)
}

function onFontSliderInput(event: Event) {
  prefs.fontSize = Number((event.target as HTMLInputElement).value)
  onFontInput()
}

function bindSwipe(el: HTMLElement | null) {
  if (!el) return
  let startX = 0
  let startY = 0
  let startTime = 0
  let tracking = false
  let dragging = false

  const release = (dx: number) => {
    const flick = Date.now() - startTime < 300 && Math.abs(dx) > 30
    const shouldTurn = flick || Math.abs(dx) > pageW() * 0.25
    if (shouldTurn) {
      if (turnBusy) {
        void animateTrack(dx, 0)
        return
      }
      const target = current.value + (dx < 0 ? 1 : -1)
      if (target >= 0 && target < pageCount.value) void turn(dx < 0 ? 1 : -1, dx)
      else void animateTrack(dx, 0)
    } else {
      void animateTrack(dx, 0)
    }
  }

  el.addEventListener(
    'touchstart',
    event => {
      tracking = event.touches.length === 1 && stage.value === 'reading' && !turnBusy
      dragging = false
      if (tracking) {
        startX = event.touches[0].clientX
        startY = event.touches[0].clientY
        startTime = Date.now()
      }
    },
    { passive: true },
  )
  el.addEventListener(
    'touchmove',
    event => {
      if (!tracking) return
      if (event.touches.length !== 1) {
        tracking = false
        if (dragging) setTrackX(0)
        return
      }
      const dx = event.touches[0].clientX - startX
      const dy = event.touches[0].clientY - startY
      if (!dragging) {
        if (Math.abs(dx) < 8) return
        if (Math.abs(dx) < Math.abs(dy) * 1.2) {
          tracking = false
          return
        }
        dragging = true
        suppressZoneClickUntil = Date.now() + 600
      }
      const atEdge = (current.value === 0 && dx > 0) || (current.value >= pageCount.value - 1 && dx < 0)
      setTrackX(atEdge ? dx / 3 : dx)
    },
    { passive: true },
  )
  el.addEventListener(
    'touchend',
    event => {
      if (!tracking) return
      tracking = false
      const touch = event.changedTouches[0]
      const dx = touch.clientX - startX
      const dy = touch.clientY - startY
      if (!dragging) {
        if (Math.abs(dx) < 45 || Math.abs(dx) < Math.abs(dy) * 1.5) return
        suppressZoneClickUntil = Date.now() + 500
      }
      release(dx)
    },
    { passive: true },
  )
  el.addEventListener(
    'touchcancel',
    () => {
      if (!tracking) return
      tracking = false
      if (dragging) setTrackX(0)
    },
    { passive: true },
  )
}

function zoneGuard(action: () => void) {
  return () => {
    if (Date.now() < suppressZoneClickUntil) return
    action()
  }
}

// ---- 生命周期 ----
function ensureReaderCSS() {
  if (document.getElementById('revaro-reader-css')) return
  const link = document.createElement('link')
  link.id = 'revaro-reader-css'
  link.rel = 'stylesheet'
  link.href = '/api/reader.css'
  document.head.appendChild(link)
}

function onVisibilityChange() {
  if (document.visibilityState === 'hidden') flushProgress()
}

onMounted(() => {
  ensureReaderCSS()
  document.title = `${props.file.name} · revaro`
  window.addEventListener('keydown', onKey)
  window.addEventListener('resize', onResize)
  window.visualViewport?.addEventListener('resize', onResize)
  document.addEventListener('visibilitychange', onVisibilityChange)
  window.addEventListener('pagehide', flushProgress)
  window.addEventListener('beforeunload', flushProgress)
  window.addEventListener('blur', flushProgress)
  bindSwipe(viewportEl.value)
  void open()
})

onBeforeUnmount(() => {
  manifestGen++ // 让所有在途轮询/生成等待失效
  if (manifestTimer) {
    clearTimeout(manifestTimer)
    manifestTimer = null
  }
  flushProgress()
  window.removeEventListener('keydown', onKey)
  window.removeEventListener('resize', onResize)
  window.visualViewport?.removeEventListener('resize', onResize)
  document.removeEventListener('visibilitychange', onVisibilityChange)
  window.removeEventListener('pagehide', flushProgress)
  window.removeEventListener('beforeunload', flushProgress)
  window.removeEventListener('blur', flushProgress)
  document.title = 'revaro · 私人网盘'
})
</script>

<template>
  <section id="reader-view" class="reader-shell" :class="{ dark: prefs.theme === 'dark', 'tools-hidden': !toolsVisible }">
    <header class="reader-bar">
      <button id="reader-back" class="reader-icon-btn" aria-label="返回" @click="emit('close')">
        <svg viewBox="0 0 24 24" width="20" height="20" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><path d="m15 18-6-6 6-6" /></svg>
      </button>
      <div class="reader-bar-title"><strong id="reader-title">{{ title }}</strong><small id="reader-kind">{{ kind.toUpperCase() }}</small></div>
      <button class="reader-icon-btn" id="toc-button" aria-label="打开目录" :aria-expanded="tocOpen" @click="openToc">☰</button>
    </header>
    <main id="viewport" ref="viewportEl" class="reader-viewport v2-viewport">
      <div id="loading" class="reader-loading" v-if="stage === 'loading'">{{ loadingText }}</div>
      <div class="reader-loading" v-else-if="stage === 'error'">
        {{ errorText }}
        <button class="font-step" style="margin-top: 12px" @click="emit('close')">关闭</button>
      </div>
      <div class="v2-window">
        <div class="v2-track" ref="trackEl">
          <div class="v2-slot" v-for="i in 3" :key="i" :ref="slotRefs[i - 1]"></div>
        </div>
      </div>
      <div class="v2-status" v-if="relayout.pending">
        正在生成新排版…
        <template v-if="relayout.spinesTotal">{{ relayout.spinesDone }}/{{ relayout.spinesTotal }} 章</template>
        <template v-if="relayout.pages"> · {{ relayout.pages }} 页</template>
      </div>
      <button id="prev-zone" class="page-zone prev-zone" aria-label="上一页" @click="zoneGuard(() => turn(-1))()"></button>
      <button id="center-zone" class="page-zone center-zone" aria-label="显示或隐藏工具栏" @click="zoneGuard(toggleTools)()"></button>
      <button id="next-zone" class="page-zone next-zone" aria-label="下一页" @click="zoneGuard(() => turn(1))()"></button>
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
          @click="closeToc(); seek(entry.page)"
        >
          {{ entry.label }}
        </button>
      </nav>
    </aside>
    <div id="font-popover" class="font-popover" :class="{ hidden: !fontOpen }">
      <span>字号</span>
      <button id="font-smaller" class="font-step" aria-label="减小字号" @click="adjustFont(-1)">A−</button>
      <input id="font-slider" type="range" :min="FONT_MIN" :max="FONT_MAX" step="1" :value="prefs.fontSize" aria-label="阅读字号" @input="onFontSliderInput">
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
      <div class="reader-seek">
        <span id="page-label">{{ pageLabel }}</span>
        <input id="page-slider" type="range" min="0" :max="Math.max(0, pageCount - 1)" step="1" :value="current" aria-label="页面进度" @input="onSliderInput" @change="onSliderChange">
      </div>
      <div class="reader-actions">
        <button id="toc-button-2" class="reader-action-btn" @click="openToc"><b>☰</b><span>目录</span></button>
        <button id="font-button" class="reader-action-btn" @click="fontOpen = !fontOpen"><b>A</b><span>排版</span></button>
        <button id="theme-button" class="reader-action-btn" @click="toggleTheme"><b>◐</b><span>明暗</span></button>
      </div>
    </footer>
  </section>
</template>

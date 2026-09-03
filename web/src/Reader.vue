<script setup lang="ts">
// 连续 reading flow 阅读器：服务端只产出「标准化阅读流 chunk」，客户端把
// 当前位置附近的少量 chunk 装进一个连续多栏 DOM（图片与文字同一排版上下文），
// 用浏览器原生 CSS columns 分页，翻页只做合成层 transform。
// 字号/横竖屏/行距变化只在客户端重新排版；进度始终是 readingAnchor。
import { computed, onBeforeUnmount, onMounted, reactive, ref } from 'vue'
import type { DriveFile } from './api'
import type { BookProgress, FlowManifest, FlowTocEntry, ReadingAnchor, ReaderPrefs } from './reader/types'
import { fetchBookInfo, fetchChunk, fetchFlow, fetchProgress, saveProgress } from './reader/api'
import { chunkForBlock, locateChar, spineForBlock, stableWindowRange, tocActiveIndex, totalBlocks } from './reader/flow'
import { PageCache, InFlight } from './reader/cache'
import { ClientCacheManager } from './reader/clientCache'
import { clamp, computeMargins, FONT_MAX, FONT_MIN, LINE_HEIGHTS, loadPrefs, savePrefs } from './reader/prefs'

const props = defineProps<{ file: DriveFile }>()
const emit = defineEmits<{ (e: 'close'): void }>()

const kind = computed(() => (/\.epub$/i.test(props.file.name) ? 'epub' : 'txt'))
const prefs = reactive<ReaderPrefs>(loadPrefs())
const isDark = computed(() => prefs.theme === 'dark')

const viewportEl = ref<HTMLElement | null>(null)
const flowEl = ref<HTMLElement | null>(null)

const stage = ref<'loading' | 'reading' | 'error'>('loading')
const loadingText = ref('正在打开书页…')
const errorText = ref('')
const title = ref(props.file.name)
const manifest = ref<FlowManifest | null>(null)
const tocOpen = ref(false)
const fontOpen = ref(false)
const toolsVisible = ref(true)
const tocActive = ref(-1)
const percentNow = ref(0)

// ---- 排版参数 ----
const FONT_STACK = '"Noto Serif SC", "Songti SC", Georgia, "Times New Roman", "STSong", SimSun, serif'

// ---- 引擎内部状态（非响应式） ----
// 实际 DOM 窗口 = [当前 spine 稳定分页边界 chunk, 当前 chunk + AHEAD]：
// spine 起点是稳定分页边界（data-spine-start 强制 break-before:column），
// 阅读 spine 内必须保留其排版前缀，否则 CSS columns 以任意 chunk 为原点，
// 窗口增删会改变 page boundary（相位漂移）。PageCache（L1）+ 持久
// ClientCacheManager（L2）让窗口滑动与重开零网络。
const AHEAD_MARGIN = 3 // 阅读方向前预取的 chunk 数
let metrics = { width: 0, height: 0, side: 0, top: 0, bottom: 0, pitch: 0, colHeight: 0 }
let c0 = 0 // 已加载窗口首 chunk
let c1 = -1 // 已加载窗口末 chunk
let cols = 1 // 当前已排版（已加载窗口内）的栏数
let currentCol = 0
let lastX = 0
let topAnchor: ReadingAnchor | null = null // 当前页顶部的 readingAnchor
let promoting = false // will-change:transform 是否开启（仅拖动/动画期间）
const chunkHTML = new PageCache(24)
const chunkFlight = new InFlight()
const persistentCache = new ClientCacheManager()
let turnBusy = false
let pendingTurns = 0
let syncing = false // windowSync（窗口重起源）在途：turn 不得按陈旧 cols/currentCol 排版
let navDepth = 0 // 程序化 TOC 导航在途计数：windowSync 期间不得按旧 topAnchor 抢先重排
let currentAnim: Animation | null = null
let suppressZoneClickUntil = 0
let gen = 0 // 打开代数：卸载后旧异步任务自动失效
let progressTimer: ReturnType<typeof setTimeout> | null = null
let syncTimer: ReturnType<typeof setTimeout> | null = null
let fontTimer: ReturnType<typeof setTimeout> | null = null
let resizeTimer: ReturnType<typeof setTimeout> | null = null
let lastViewport = { w: 0, h: 0 }
let closing = false

function pageLabelText(): string {
  return `${percentNow.value.toFixed(percentNow.value % 1 ? 1 : 0)}%`
}

const pageLabel = ref('0%')
const toc = computed(() => manifest.value?.toc ?? [])

// ---- 基础工具 ----

function totalBlockCount(): number {
  const m = manifest.value
  return m ? totalBlocks(m) : 0
}

function lastChunkIndex(): number {
  const m = manifest.value
  return m && m.chunks.length ? m.chunks.length - 1 : -1
}

function viewportSize(): { w: number; h: number } {
  const el = viewportEl.value
  const w = el?.clientWidth || window.innerWidth
  const h = el?.clientHeight || window.innerHeight
  return { w, h }
}

// applyMetrics 按视口与偏好设置多栏排版参数并更新几何数据。
function applyMetrics() {
  const flow = flowEl.value
  if (!flow) return
  const { w, h } = viewportSize()
  lastViewport = { w, h }
  const margins = computeMargins(w, h)
  metrics = {
    width: w,
    height: h,
    side: margins.side,
    top: margins.top,
    bottom: margins.bottom,
    pitch: w,
    colHeight: h - margins.top - margins.bottom,
  }
  const s = flow.style
  s.width = `${w}px`
  s.height = `${h}px`
  s.boxSizing = 'border-box'
  s.padding = `${margins.top}px ${margins.side}px ${margins.bottom}px`
  s.columnWidth = `${Math.max(1, w - 2 * margins.side)}px`
  s.columnGap = `${2 * margins.side}px`
  s.columnFill = 'auto'
  flow.classList.toggle('txt', manifest.value?.format === 'txt')
  flow.style.setProperty('--revaro-font-family', FONT_STACK)
  flow.style.setProperty('--revaro-font-size', `${prefs.fontSize}px`)
  flow.style.setProperty('--revaro-line-height', String(prefs.lineHeight))
  flow.style.setProperty('--revaro-col-height', `${metrics.colHeight}px`)
  flow.style.transform = 'translateX(0px)'
}

// measureCols 统计已加载窗口的栏数（每栏 = 一屏宽）。
function measureCols(): number {
  const flow = flowEl.value
  if (!flow || metrics.pitch <= 0) return 1
  // 多栏内容总宽 = scrollWidth；栏距恒等于 2×侧边距 → 栏距 + 栏宽 = 屏宽
  const total = Math.max(metrics.pitch, flow.scrollWidth)
  cols = Math.max(1, Math.round(total / metrics.pitch))
  return cols
}

function setX(x: number) {
  const flow = flowEl.value
  if (!flow) return
  flow.style.transition = 'none'
  const rx = Math.round(x)
  flow.style.transform = `translateX(${rx}px)`
  lastX = rx
}

// setPromote 只在拖动/翻页动画期间开启合成层提升，结束后释放：
// 常驻 will-change 会让大图旁的整层文字持续离屏合成，移动端出现发糊。
function setPromote(on: boolean) {
  const flow = flowEl.value
  if (!flow) return
  if (on === promoting) return
  promoting = on
  flow.style.willChange = on ? 'transform' : 'auto'
}

function animateX(from: number, to: number): Promise<void> {
  const flow = flowEl.value
  if (!flow) return Promise.resolve()
  currentAnim?.cancel()
  setPromote(true)
  setX(from)
  return new Promise(resolve => {
    currentAnim = flow.animate(
      [{ transform: `translateX(${Math.round(from)}px)` }, { transform: `translateX(${Math.round(to)}px)` }],
      { duration: 260, easing: 'cubic-bezier(.22,.72,.26,1)' },
    )
    const done = () => {
      currentAnim = null
      setX(to)
      setPromote(false)
      resolve()
    }
    currentAnim.onfinish = done
    currentAnim.oncancel = done
  })
}

// ---- DOM 定位 ----

function blockElFor(block: number): HTMLElement | null {
  const flow = flowEl.value
  if (!flow) return null
  return flow.querySelector<HTMLElement>(`[data-block="${block}"]`)
}

function childIndexOf(parent: Node, child: Node): number {
  for (let i = 0; i < parent.childNodes.length; i++) if (parent.childNodes[i] === child) return i
  return -1
}

// anchorFromNode 把块内真实 DOM 位置（visualStart 命中的文本/元素）转成
// 精确 readingAnchor：文本 → path + 文本偏移；元素 → path + 元素边界(-1)；
// 无命中 → 块起点。目录跳转必须提交该锚点而不是块起点，否则 windowSync/
// relayout 会按块首栏重新对齐，把页面拉回目标前的内容。
function anchorFromNode(blockEl: HTMLElement, node: Node | null, offset: number): ReadingAnchor | null {
  const base = makeAnchorFromBlockEl(blockEl, [], -1)
  if (!base || !node) return base
  const path: number[] = []
  let cur: Node | null = node
  while (cur && cur !== blockEl) {
    const parent: Node | null = cur.parentNode
    if (!parent) return base
    path.unshift(childIndexOf(parent, cur))
    cur = parent
  }
  if (cur !== blockEl) return base
  return { ...base, path, offset: node.nodeType === Node.TEXT_NODE ? Math.max(0, offset) : -1 }
}

function colFromRect(rect: DOMRect | undefined): number {
  const flow = flowEl.value
  if (!flow || !rect) return 0
  const flowRect = flow.getBoundingClientRect()
  const rel = rect.left - flowRect.left
  if (metrics.pitch <= 0) return 0
  return clamp(Math.floor((rel - metrics.side + 1) / metrics.pitch), 0, Math.max(0, cols - 1))
}

function firstRectOf(el: HTMLElement): DOMRect | null {
  const rects = el.getClientRects()
  if (rects.length) return rects[0] as DOMRect
  return null
}

function firstRectOfRange(range: Range): DOMRect | null {
  const rects = range.getClientRects()
  if (rects.length) return rects[0] as DOMRect
  return null
}

// rectHasBox 判断 rect 是否存在实际布局盒（全零 = 无盒子，如隐藏子树、
// 未加载或无尺寸的媒体），这样的候选不能用来算栏。
function rectHasBox(rect: DOMRect | null): boolean {
  return !!rect && (rect.width > 0 || rect.height > 0)
}

// visualStart 在元素内定位「首个实际可见内容」：取第一个非空文本（视觉
// 空白如 NBSP/U+3000 也跳过）与第一个实际可见媒体/表格后代中文档序在先
// 者，都没有才回退到元素自身盒。CSS columns 下块级容器的首 fragment 可能
// 留在上一栏（break-inside:avoid 的整页图被整体推入下一栏），因此定位
// 不能用容器自身 getClientRects()[0]，必须用实际内容的 rect。返回 rect
// 与命中节点（node+offset 用于还原精确 readingAnchor）；连自身都没有
// 布局盒时返回 null。
function visualStart(el: HTMLElement): { rect: DOMRect; node: Node | null; offset: number } | null {
  const doc = el.ownerDocument
  let text: { rect: DOMRect; node: Text; offset: number } | null = null
  const walker = doc.createTreeWalker(el, NodeFilter.SHOW_TEXT)
  for (let n = walker.nextNode(); n; n = walker.nextNode()) {
    const at = (n as Text).data.search(/\S/)
    if (at < 0) continue
    const range = doc.createRange()
    range.setStart(n, at)
    range.collapse(true)
    const rect = firstRectOfRange(range) ?? range.getBoundingClientRect()
    if (!rectHasBox(rect)) continue
    text = { rect, node: n as Text, offset: at }
    break
  }
  let media: { rect: DOMRect; node: Node; offset: number } | null = null
  for (const d of el.querySelectorAll<HTMLElement>('img,svg,video,canvas,iframe,embed,object,table')) {
    const rect = d.getBoundingClientRect()
    if (!rectHasBox(rect)) continue
    media = { rect, node: d, offset: -1 }
    break
  }
  if (text && media) {
    // 图在前、文字在后（整页图+图注）时视觉起点是图；文字在前取文本。
    // 文本在媒体子树内（表格）时 compareDocumentPosition 不含 FOLLOWING，
    // 同样取媒体自身。
    return text.node.compareDocumentPosition(media.node) & Node.DOCUMENT_POSITION_FOLLOWING ? text : media
  }
  const hit = text ?? media
  if (hit) return hit
  const self = firstRectOf(el) ?? el.getBoundingClientRect()
  if (!rectHasBox(self) && self.left === 0 && self.top === 0) return null
  return { rect: self, node: null, offset: -1 }
}

function visualStartRect(el: HTMLElement): DOMRect | null {
  return visualStart(el)?.rect ?? null
}

// anchorNode 把 anchor 解析回已加载 DOM 中的位置（块元素 + 目标节点）。
function anchorNode(anchor: ReadingAnchor): { blockEl: HTMLElement; node: Node } | null {
  const blockEl = blockElFor(anchor.block)
  if (!blockEl) return null
  let node: Node | null = blockEl
  for (const idx of anchor.path) {
    node = node?.childNodes[idx] ?? null
    if (!node) return null
  }
  return { blockEl, node }
}

// colForAnchor 返回 anchor 所在的栏（只考虑已加载窗口）。文本锚点用 caret
// Range；元素锚点（含块起点）用 visualStartRect——块容器首 fragment 可能
// 留在上一栏（整页图 break-inside:avoid 被推入下一栏），直接量容器会偏一页。
function colForAnchor(anchor: ReadingAnchor | null): number {
  if (!anchor) return 0
  const t = anchorNode(anchor)
  if (!t) return 0
  if (t.node.nodeType === Node.TEXT_NODE) {
    const len = (t.node as Text).data.length
    const range = t.blockEl.ownerDocument.createRange()
    range.setStart(t.node, clamp(anchor.offset < 0 ? 0 : anchor.offset, 0, len))
    range.collapse(true)
    const r = firstRectOfRange(range)
    if (r) return colFromRect(r)
  }
  const el = t.node.nodeType === Node.ELEMENT_NODE ? (t.node as HTMLElement) : t.blockEl
  return colFromRect(visualStartRect(el) ?? firstRectOf(el) ?? el.getBoundingClientRect())
}

// decodeFrag 对 EPUB URL fragment 做安全 percent decode（兼容非 ASCII /
// 空格 / 转义字符）；畸形序列解码失败时返回原值。
function decodeFrag(fragment: string): string {
  try {
    return decodeURIComponent(fragment)
  } catch {
    return fragment
  }
}

// fragMatchEl 按属性值比较元素自身 id / data-frag-ids 是否命中目标
// fragment。不把 fragment 拼进 CSS selector（特殊字符会让 selector 失效），
// 只遍历候选元素后按属性值比较。
function fragMatchEl(el: HTMLElement, want: string[]): HTMLElement | null {
  const id = el.getAttribute('id')
  if (id && want.includes(id)) return el
  const fragIds = el.getAttribute('data-frag-ids')
  if (fragIds) {
    for (const part of fragIds.split(/\s+/)) {
      if (part && want.includes(part)) return el
    }
  }
  return null
}

// fragmentTargetEl 在块内查找 fragment 对应的真实元素：优先元素自身 id，
// 其次 data-frag-ids（清洗器把被丢弃锚点的 id 顺延记到后续块上）。
// 比较同时用原始与 percent-decode 后的 fragment，兼容二次转义的目录。
function fragmentTargetEl(blockEl: HTMLElement, fragment: string): HTMLElement | null {
  const want = [fragment]
  const decoded = decodeFrag(fragment)
  if (decoded !== fragment) want.push(decoded)
  const self = fragMatchEl(blockEl, want)
  if (self) return self
  for (const el of blockEl.querySelectorAll<HTMLElement>('[id],[data-frag-ids]')) {
    if (fragMatchEl(el, want)) return el
  }
  return null
}

// blockAtPoint 返回点 (x,y) 处的顶层内容块（沿 DOM 向上找 data-block）。
function blockAtPoint(x: number, y: number): HTMLElement | null {
  const nodes = document.elementsFromPoint(x, y)
  for (const node of nodes) {
    const el = node as HTMLElement
    let cur: HTMLElement | null = el
    while (cur) {
      if (cur.hasAttribute?.('data-block')) return cur
      cur = cur.parentElement
    }
  }
  return null
}

function makeAnchorFromBlockEl(blockEl: HTMLElement, path: number[], offset: number): ReadingAnchor | null {
  const m = manifest.value
  if (!m) return null
  const block = Number(blockEl.dataset.block)
  if (!Number.isInteger(block)) return null
  return { spine: spineForBlock(m, block), block, path, offset }
}

// contentOrigin 返回当前页（第 currentCol 栏）内容区左上角的视口坐标。
function contentOrigin(): { x: number; y: number } {
  const vp = viewportEl.value?.getBoundingClientRect()
  if (!vp) return { x: window.innerWidth / 2, y: metrics.top + 2 }
  return { x: vp.left + metrics.side + 2, y: vp.top + metrics.top + 2 }
}

// captureTopAnchor 读取当前屏顶部的文本位置作为 readingAnchor。
function captureTopAnchor(): ReadingAnchor | null {
  const flow = flowEl.value
  const m = manifest.value
  if (!flow || !m) return null
  const { x, y } = contentOrigin()
  let range: Range | null = null
  try {
    if (document.caretRangeFromPoint) range = document.caretRangeFromPoint(x, y)
  } catch {
    range = null
  }
  const container = range?.startContainer
  if (container && container.nodeType === Node.TEXT_NODE) {
    const start = range!.startOffset
    const path: number[] = []
    let node: Node | null = container
    let blockEl: HTMLElement | null = null
    while (node) {
      if ((node as HTMLElement).hasAttribute?.('data-block')) {
        blockEl = node as HTMLElement
        break
      }
      const parent: Node | null = node.parentNode
      if (!parent) break
      path.unshift(childIndexOf(parent, node))
      node = parent
    }
    if (blockEl) {
      const anchor = makeAnchorFromBlockEl(blockEl, path, start)
      if (anchor) {
        const rect = firstRectOfRange(range!)
        // caret 若落在别的栏（如整页图顶部），退化到块起点锚点
        if (!rect || colFromRect(rect) !== currentCol) {
          const pt = blockAtPoint(x, y)
          if (pt) return makeAnchorFromBlockEl(pt, [], -1)
        }
        return anchor
      }
    }
  }
  const pt = blockAtPoint(x, y)
  if (pt) return makeAnchorFromBlockEl(pt, [], -1)
  return null
}

// anchorAtTopOfCurrentCol 兜底：取当前页顶部可见块。
function anchorAtTopOfCurrentCol(): ReadingAnchor | null {
  const m = manifest.value
  if (!m) return null
  const { x, y } = contentOrigin()
  const pt = blockAtPoint(x, y)
  return pt ? makeAnchorFromBlockEl(pt, [], -1) : null
}

// ---- 窗口管理（增量 DOM 更新） ----

// chunkText 的三级读取路径：内存 L1 → 持久 L2 → 网络（成功回填两级）。
// chunk 内容随（书内容指纹 × flow 版本 × chunk 序号）固定，L2 键安全。
async function chunkText(index: number): Promise<string> {
  const hit = chunkHTML.get(index)
  if (hit !== undefined) return hit
  return chunkFlight.run(index, async () => {
    const m = manifest.value
    const bookKey = m?.book_key ?? ''
    const version = m?.version ?? 0
    if (bookKey) {
      const cached = await persistentCache.getChunk(bookKey, version, index)
      if (cached !== null) {
        chunkHTML.set(index, cached)
        return cached
      }
    }
    const html = await fetchChunk(props.file.id, index)
    chunkHTML.set(index, html)
    if (bookKey) void persistentCache.putChunk(bookKey, version, index, html)
    return html
  })
}

function chunkElement(index: number): HTMLElement | null {
  const flow = flowEl.value
  return flow ? (flow.querySelector(`.rf-chunk[data-chunk="${index}"]`) as HTMLElement | null) : null
}

// ensureWindow 把窗口变为 [newC0, newC1]：只 append 新 chunk、remove 已不在
// 窗口内的旧 chunk，保留的 chunk 子树原样不动（避免清空重建触发整棵多栏
// CSS columns reflow）。chunk HTML 走两级缓存，重入窗口不重新请求。
async function ensureWindow(newC0: number, newC1: number): Promise<void> {
  const flow = flowEl.value
  const m = manifest.value
  if (!flow || !m) return
  const last = Math.max(0, m.chunks.length - 1)
  newC0 = clamp(newC0, 0, last)
  newC1 = clamp(newC1, newC0, last)
  if (c1 >= 0 && newC0 === c0 && newC1 === c1) return
  const existing = new Map<number, HTMLElement>()
  for (const child of Array.from(flow.children) as HTMLElement[]) {
    const idx = Number((child as HTMLElement).dataset.chunk)
    if (Number.isInteger(idx)) existing.set(idx, child as HTMLElement)
  }
  if (existing.size) {
    const minCur = Math.min(...existing.keys())
    const maxCur = Math.max(...existing.keys())
    // 移除窗口外（只发生在头部/尾部，逐节点 remove）
    for (let i = minCur; i < newC0; i++) existing.get(i)?.remove()
    for (let i = newC1 + 1; i <= maxCur; i++) existing.get(i)?.remove()
  }
  // 补齐缺失 chunk：插到下一个已存在的 chunk 前，保持阅读顺序
  for (let i = newC0; i <= newC1; i++) {
    if (existing.has(i)) continue
    const html = await chunkText(i)
    if (flowEl.value !== flow) return // 窗口/组件已变化
    const section = document.createElement('div')
    section.className = 'rf-chunk'
    section.dataset.chunk = String(i)
    section.innerHTML = html
    let ref: HTMLElement | null = null
    for (let j = i + 1; j <= newC1; j++) {
      const next = flow.querySelector<HTMLElement>(`.rf-chunk[data-chunk="${j}"]`)
      if (next) {
        ref = next
        break
      }
    }
    flow.insertBefore(section, ref)
    existing.set(i, section)
  }
  c0 = newC0
  c1 = newC1
}

// ensureWindowForBlock 把窗口收敛到 block 所属 spine 的稳定分页窗口：
// [spine 起始块所在 chunk, block 所在 chunk + AHEAD]。spine 排版前缀永远
// 保留，只有进入下一 spine 后才允许释放上一 spine 的 chunk。
async function ensureWindowForBlock(block: number): Promise<boolean> {
  const m = manifest.value
  if (!m) return false
  const [lo, hi] = stableWindowRange(m, block, AHEAD_MARGIN)
  if (c1 >= 0 && lo === c0 && hi === c1) return false
  await ensureWindow(lo, hi)
  return true
}

// alignToAnchor 不带动画地回到 topAnchor 所在栏（窗口滑动后位置不丢）。
async function alignToAnchor(): Promise<void> {
  if (!manifest.value || stage.value !== 'reading') return
  const anchor = topAnchor ?? anchorAtTopOfCurrentCol()
  const col = anchor ? colForAnchor(anchor) : currentCol
  currentCol = clamp(col, 0, Math.max(0, cols - 1))
  setX(-currentCol * metrics.pitch)
  captureTop()
}

// refreshTocUi 按当前 topAnchor 刷新目录高亮与进度显示。
function refreshTocUi() {
  const m = manifest.value
  if (!m) return
  const block = topAnchor ? topAnchor.block : -1
  tocActive.value = tocActiveIndex(m, block)
  percentNow.value = topAnchor ? percentOfAnchor(topAnchor) : 0
  pageLabel.value = pageLabelText()
}

// captureTop 记录当前页顶部 anchor 并刷新 UI。
function captureTop() {
  const captured = captureTopAnchor() ?? anchorAtTopOfCurrentCol()
  if (captured) topAnchor = captured
  refreshTocUi()
}

// commitNavAnchor 程序化导航落地后提交导航目标为兜底锚点。goToCol 里的
// captureTop 从屏幕重读位置；失败（栏顶 margin 死区、遮挡等）时 topAnchor
// 仍是跳转前的旧值——不提交目标的话，后续 windowSync 会按旧 anchor 重建
// 窗口并把页面拉回跳转前位置。captureTop 成功时 topAnchor 已是新对象。
function commitNavAnchor(target: ReadingAnchor, prev: ReadingAnchor | null) {
  if (topAnchor === prev) {
    topAnchor = target
    refreshTocUi()
  }
}

// percentOfAnchor 计算 anchor 前的文本占全书比例（块级精度）。
function percentOfAnchor(anchor: ReadingAnchor): number {
  const m = manifest.value
  if (!m || m.total_chars <= 0) return 0
  const ci = chunkForBlock(m, anchor.block)
  if (ci < 0) return 0
  let chars = 0
  for (let i = 0; i < ci; i++) chars += m.chunks[i].chars
  const el = chunkElement(ci)
  if (el) {
    for (const child of Array.from(el.children) as HTMLElement[]) {
      const b = Number(child.dataset.block)
      if (b >= anchor.block) break
      chars += child.textContent?.length ?? 0
    }
  }
  return (chars / m.total_chars) * 100
}

// ---- 翻页 ----

async function goToCol(col: number, animate: boolean): Promise<void> {
  if (!manifest.value || stage.value !== 'reading') return
  currentCol = clamp(col, 0, Math.max(0, cols - 1))
  const to = -currentCol * metrics.pitch
  if (animate) {
    await animateX(lastX, to)
  } else {
    setX(to)
  }
  captureTop()
  scheduleWindowSync()
}

// rebaseForCenter 把窗口物理平移一个 chunk（增量增删）并按顶部 anchor
// 对齐。只在翻页触到窗口物理边缘时使用：这是「原始」窗口扩张——向后越界
// 时允许越过当前 spine 的稳定边界（把上一 spine 的尾部 chunk 拉回来），
// data-spine-start 的强制分栏保证当前 spine 内的 page boundary 不因此
// 改变（绝对栏号变化由 alignToAnchor 重算吸收）。
async function rebaseForCenter(lo: number, hi: number): Promise<boolean> {
  const m = manifest.value
  if (!m) return false
  const last = Math.max(0, m.chunks.length - 1)
  const newLo = clamp(lo, 0, last)
  const newHi = clamp(hi, newLo, last)
  if (c1 >= 0 && newLo === c0 && newHi === c1) return false
  await ensureWindow(newLo, newHi)
  measureCols()
  await alignToAnchor()
  return true
}

async function turn(dir: -1 | 1) {
  const m = manifest.value
  if (!m || stage.value !== 'reading') return
  if (turnBusy || syncing) {
    // 翻页动画或窗口重起源在途：排队重放。重起源期间窗口 DOM/栏数正在
    // 变化，此刻按 cols/currentCol 计算落点会读到陈旧值（跳页）。
    pendingTurns += dir
    return
  }
  turnBusy = true
  try {
    let target = currentCol + dir
    if (target < 0 || target >= cols) {
      // 窗口物理边缘：向该方向原始扩张一个 chunk（增量 append/remove）
      if (dir < 0) {
        if (c0 <= 0) return
        await rebaseForCenter(c0 - 1, c1)
      } else {
        if (c1 >= lastChunkIndex()) return
        await rebaseForCenter(c0, c1 + 1)
      }
      target = currentCol + dir
      if (target < 0 || target >= cols) return
    }
    await goToCol(target, true)
    queueProgressSave()
  } finally {
    turnBusy = false
    if (pendingTurns !== 0) {
      const next = pendingTurns > 0 ? 1 : -1
      pendingTurns -= next
      void turn(next)
    }
  }
}

function previous() {
  void turn(-1)
}
function next() {
  void turn(1)
}

// seekToAnchor 跳到任意 anchor（跨 chunk：窗口收敛到目标 spine 的稳定
// 分页窗口）。
async function seekToAnchor(anchor: ReadingAnchor) {
  const m = manifest.value
  if (!m || stage.value !== 'reading') return
  const total = totalBlockCount()
  const block = clamp(anchor.block, 0, Math.max(0, total - 1))
  const fixed: ReadingAnchor = { spine: anchor.spine, block, path: anchor.path, offset: anchor.offset }
  const prev = topAnchor
  await ensureWindowForBlock(block)
  measureCols()
  await goToCol(colForAnchor(fixed), false)
  commitNavAnchor(fixed, prev)
  queueProgressSave()
}

// jumpToBlock 跳到某顶层内容块（目录/进度条）。
async function jumpToBlock(block: number) {
  const m = manifest.value
  if (!m || stage.value !== 'reading') return
  const total = totalBlockCount()
  const b = clamp(block, 0, Math.max(0, total - 1))
  await seekToAnchor({ spine: spineForBlock(m, b), block: b, path: [], offset: -1 })
}

// navAnchorEl 在已加载窗口内按属性值精确匹配 data-rv-anchor 绑定节点
// （服务端 synthetic id 字符集安全，但仍不拼进 CSS selector）。
function navAnchorEl(anchorID: string): HTMLElement | null {
  const flow = flowEl.value
  if (!flow || !anchorID) return null
  for (const el of flow.querySelectorAll<HTMLElement>('[data-rv-anchor]')) {
    if (el.getAttribute('data-rv-anchor') === anchorID) return el
  }
  return null
}

// anchorElRect 返回 NavAnchor 绑定节点的实际布局 rect：内联标记取首个
// 有效 line-box fragment（= 目标文本起始位置），媒体/块元素取自身盒。
function anchorElRect(el: HTMLElement): DOMRect | null {
  for (const r of el.getClientRects()) {
    if (r.width > 0 || r.height > 0) return r as DOMRect
  }
  return el.getBoundingClientRect()
}

// jumpToTocEntry 目录跳转（Stable NavAnchor，两套定位语义分离）：
//   1. 主路径（文本目标）——manifest 携带实际文本节点的稳定 DOM path +
//      UTF-16 偏移（text_path/text_offset）。客户端加载目标 chunk 后解析
//      真实 Text node，用 collapsed caret Range 的 rect 计算栏。空 inline
//      标记在 column/page break 处可能停留在上一栏而真实文本已进入下一栏，
//      因此文本定位一律以真实文本节点为准。
//   2. 主路径（媒体目标）——data-rv-anchor 绑定在 img/svg/video 元素上，
//      按真实元素 rect 定栏，并从绑定节点生成精确 readingAnchor 提交。
//   3. 回退——locator 解析失败（节点缺失/偏移越界/无布局盒）时用
//      sourceFragment 在块内做 fragment 精确定位（visualStart 找首个可见内容）。
//   4. 兜底——块起点（jumpToBlock，seekToAnchor 内同样提交锚点）。
// 导航正确性不依赖目录抽屉关闭动画：开始时先失效遗留的 pending
// windowSync，并以 navDepth 屏蔽在途 windowSync 抢先按旧 topAnchor
// 重排窗口（否则跳转会先成功、随后被拉回跳转前位置）；落地后由
// goToCol 重新调度 sync。
async function jumpToTocEntry(entry: FlowTocEntry) {
  const m = manifest.value
  if (!m || stage.value !== 'reading') return
  if (syncTimer) {
    clearTimeout(syncTimer)
    syncTimer = null
  }
  navDepth++
  try {
    const total = totalBlockCount()
    const b = clamp(entry.block, 0, Math.max(0, total - 1))
    const ci = entry.chunk ?? chunkForBlock(m, b)
    const prev = topAnchor
    await ensureWindowForBlock(b)
    measureCols()
    // 1) 文本目标：真实 Text node 的 collapsed caret rect 定栏
    const textHit = resolveTextLocator(entry)
    if (textHit) {
      const { blockEl, node, offset, anchor } = textHit
      const range = blockEl.ownerDocument.createRange()
      range.setStart(node, offset)
      range.collapse(true)
      const rect = firstRectOfRange(range)
      if (rect && rectHasBox(rect)) {
        await goToCol(colFromRect(rect), false)
        commitNavAnchor(anchor, prev)
        queueProgressSave()
        return
      }
    }
    // 2) 媒体目标：服务端 NavAnchor 绑定元素的真实 rect
    const hit = navAnchorEl(entry.nav_anchor ?? '')
    if (hit) {
      const blockEl = hit.closest<HTMLElement>('[data-block]')
      const rect = anchorElRect(hit)
      if (blockEl && rect) {
        const anchor = anchorFromNode(blockEl, hit, -1)
        await goToCol(colFromRect(rect), false)
        if (anchor) commitNavAnchor(anchor, prev)
        queueProgressSave()
        return
      }
    }
    // 3) 回退：source_fragment 块内精确定位
    if (entry.source_fragment) {
      const blockEl = blockElFor(b)
      const target = blockEl ? fragmentTargetEl(blockEl, entry.source_fragment) : null
      const vs = target ? visualStart(target) : null
      if (blockEl && vs) {
        const anchor = anchorFromNode(blockEl, vs.node, vs.offset)
        await goToCol(colFromRect(vs.rect), false)
        if (anchor) commitNavAnchor(anchor, prev)
        queueProgressSave()
        return
      }
    }
    // 4) 兜底：块起点
    await jumpToBlock(b)
  } finally {
    navDepth--
  }
}

// resolveTextLocator 把 manifest 的 text_path/text_offset 解析为已加载 DOM
// 内的真实文本位置。路径不可达、节点不是文本、偏移越界时返回 null（调用
// 方按回退链处理）。偏移按 UTF-16 码元语义与服务端对齐。
function resolveTextLocator(entry: FlowTocEntry): { blockEl: HTMLElement; node: Text; offset: number; anchor: ReadingAnchor } | null {
  const m = manifest.value
  if (!m || !entry.text_path) return null
  const block = entry.block
  const blockEl = blockElFor(block)
  if (!blockEl) return null
  let node: Node | null = blockEl
  for (const idx of entry.text_path) {
    node = node?.childNodes[idx] ?? null
    if (!node) return null
  }
  if (!node || node.nodeType !== Node.TEXT_NODE) return null
  const offset = clamp(entry.text_offset ?? 0, 0, (node as Text).data.length)
  return {
    blockEl,
    node: node as Text,
    offset,
    anchor: { spine: spineForBlock(m, block), block, path: entry.text_path, offset },
  }
}

// seekFraction 按全局文本比例跳页（进度条拖动）。
async function seekFraction(fraction: number) {
  const m = manifest.value
  if (!m || !m.total_chars || stage.value !== 'reading') return
  const target = Math.round(clamp(fraction, 0, 1) * m.total_chars)
  const { chunk, offset } = locateChar(m, target)
  const startBlock = m.chunks[chunk]?.block_start ?? 0
  await ensureWindowForBlock(startBlock)
  measureCols()
  const el = chunkElement(chunk)
  let block = startBlock
  if (el) {
    block = blockContainingOffset(el, offset, startBlock)
  }
  await jumpToBlock(block)
}

function onSeekInput(event: Event) {
  const value = Number((event.target as HTMLInputElement).value)
  if (!Number.isFinite(value)) return
  void seekFraction(value / 1000)
}

// blockContainingOffset 在 chunk DOM 内按文本长度找到包含该字符偏移的块。
function blockContainingOffset(chunkEl: HTMLElement, offset: number, firstBlock: number): number {
  let acc = 0
  let current = firstBlock
  for (const child of Array.from(chunkEl.children) as HTMLElement[]) {
    const b = Number(child.dataset.block)
    if (Number.isInteger(b)) current = b
    const len = child.textContent?.length ?? 0
    if (acc + len >= offset) return current
    acc += len
  }
  return current
}

// ---- 窗口保持（每页变化后的余量预取） ----

function scheduleWindowSync() {
  if (syncTimer) clearTimeout(syncTimer)
  syncTimer = setTimeout(() => void windowSync(), 200)
}

async function windowSync() {
  syncTimer = null
  const m = manifest.value
  if (closing || !m || stage.value !== 'reading' || c1 < 0) return
  // 程序化 TOC 导航在途：跳转落地时会按导航目标提交 anchor 并重新调度
  // sync，这里不得按（可能仍是跳转前的）topAnchor 抢先重建窗口/对齐。
  if (navDepth > 0) return
  // 翻页在途：跳过本轮（该翻页落地时会重新调度 sync），避免并发重建。
  if (turnBusy || syncing) return
  if (!topAnchor) return
  syncing = true
  try {
    // 收敛到当前 spine 的稳定分页窗口：只 append 新 chunk / remove 出窗的
    // 旧 spine chunk，不做整窗口清空重建；无变化则完全不动（热路径零开销）。
    const changed = await ensureWindowForBlock(topAnchor.block)
    if (navDepth > 0 || closing) return
    if (changed) {
      measureCols()
      await alignToAnchor()
    }
  } finally {
    syncing = false
    // 重起源期间排队的翻页输入在此重放（turn 内部也会排队，两边共同
    // 消费 pendingTurns，直至清空）。
    if (!closing && stage.value === 'reading' && pendingTurns !== 0) {
      const next = pendingTurns > 0 ? 1 : -1
      pendingTurns -= next
      void turn(next)
    }
  }
}

// ---- 交互 ----

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
function jumpToc(entryIndex: number) {
  const entry = toc.value[entryIndex]
  closeToc()
  if (entry) void jumpToTocEntry(entry)
}

function onKey(event: KeyboardEvent) {
  if (stage.value !== 'reading') return
  if (['ArrowLeft', 'ArrowRight', 'PageUp', 'PageDown', ' '].includes(event.key)) event.preventDefault()
  if (['ArrowLeft', 'PageUp'].includes(event.key)) previous()
  if (['ArrowRight', 'PageDown', ' '].includes(event.key)) next()
  if (event.key === 'Escape' && tocOpen.value) closeToc()
}

function zoneGuard(action: () => void) {
  return () => {
    if (Date.now() < suppressZoneClickUntil) return
    action()
  }
}

// ---- 排版变化（纯客户端重排，不请求服务端） ----

async function relayout() {
  const m = manifest.value
  if (!m || stage.value !== 'reading') return
  const keep = topAnchor ?? captureTopAnchor() ?? anchorAtTopOfCurrentCol()
  applyMetrics()
  measureCols()
  if (keep) {
    topAnchor = keep
    await goToCol(colForAnchor(keep), false)
  } else {
    setX(0)
    currentCol = 0
    captureTop()
  }
}

function scheduleFontRelayout() {
  if (fontTimer) clearTimeout(fontTimer)
  fontTimer = setTimeout(() => {
    fontTimer = null
    void relayout()
  }, 120)
}

function onFontInput() {
  prefs.fontSize = clamp(Math.round(prefs.fontSize), FONT_MIN, FONT_MAX)
  savePrefs(prefs)
  scheduleFontRelayout()
}
function adjustFont(delta: number) {
  prefs.fontSize = clamp(prefs.fontSize + delta, FONT_MIN, FONT_MAX)
  savePrefs(prefs)
  scheduleFontRelayout()
}
function setLineHeight(value: number) {
  prefs.lineHeight = value
  savePrefs(prefs)
  scheduleFontRelayout()
}

function onResize() {
  const m = manifest.value
  if (!m) return
  const { w, h } = viewportSize()
  if (w === lastViewport.w && h === lastViewport.h) return
  if (resizeTimer) clearTimeout(resizeTimer)
  resizeTimer = setTimeout(() => void relayout(), 250)
}

// ---- 进度 ----

function queueProgressSave() {
  if (progressTimer) clearTimeout(progressTimer)
  progressTimer = setTimeout(() => {
    progressTimer = null
    void saveNow()
  }, 1200)
}

async function saveNow() {
  if (closing || !manifest.value) return
  const anchor = topAnchor ?? anchorAtTopOfCurrentCol()
  if (!anchor) return
  try {
    await saveProgress(props.file.id, { anchor })
  } catch (error) {
    console.error('保存阅读进度失败', error)
  }
}

function flushProgress() {
  if (progressTimer) {
    clearTimeout(progressTimer)
    progressTimer = null
  }
  if (!manifest.value) return
  const anchor = topAnchor ?? anchorAtTopOfCurrentCol()
  if (anchor) {
    void saveProgress(props.file.id, { anchor }).catch(() => {})
  }
}

// ---- 打开书籍 ----

// setupView 用指定 manifest 排版并定位到 anchor（open 的主体；L2 快开与
// 网络校验两条路径共用）。
let renderedManifest: FlowManifest | null = null // 当前视图所用 manifest 原始引用

async function setupView(myGen: number, flow: FlowManifest, saved: BookProgress['anchor']): Promise<ReadingAnchor | null> {
  renderedManifest = flow
  manifest.value = flow
  applyMetrics()

  const total = totalBlockCount()
  let anchor: ReadingAnchor | null = null
  if (saved && Number.isInteger(saved.block) && saved.block >= 0 && saved.block < Math.max(1, total)) {
    anchor = { spine: saved.spine, block: saved.block, path: saved.path ?? [], offset: saved.offset ?? -1 }
  }
  if (!anchor) {
    const first = flow.spines[0]?.block_start ?? 0
    anchor = { spine: 0, block: first, path: [], offset: -1 }
  }
  loadingText.value = '正在排版…'
  await ensureWindowForBlock(anchor.block)
  if (myGen !== gen) return null
  measureCols()
  stage.value = 'reading'
  currentCol = clamp(colForAnchor(anchor), 0, Math.max(0, cols - 1))
  setX(-currentCol * metrics.pitch)
  topAnchor = anchor
  captureTop()
  prefetchSurrounding(anchor.block)
  queueProgressSave()
  return anchor
}

// resetFlow 丢弃当前 flow 的全部客户端状态（L1、窗口 DOM、位置）：L2
// manifest 与服务端 flow 不一致需要在新排版语义上重建时使用。持久 L2 的
// chunk 由 putManifest 按版本/指纹清除。
function resetFlow() {
  renderedManifest = null
  chunkHTML.clear()
  chunkFlight.clear()
  flowEl.value?.replaceChildren()
  c0 = 0
  c1 = -1
  cols = 1
  currentCol = 0
  lastX = 0
  topAnchor = null
}

// sameLayout 判断两个 manifest 的排版语义是否一致（flow 生成是确定性纯
// 函数：版本与 chunk/spine/toc 元数据一致 ⇒ 全部 chunk 逐字节一致）。
function sameLayout(a: FlowManifest, b: FlowManifest): boolean {
  return (
    a.version === b.version &&
    a.format === b.format &&
    a.total_chars === b.total_chars &&
    a.book_key === b.book_key &&
    a.chunks.length === b.chunks.length &&
    a.toc.length === b.toc.length &&
    a.chunks.every((c, i) => c.block_start === b.chunks[i].block_start && c.block_count === b.chunks[i].block_count && c.chars === b.chunks[i].chars) &&
    a.spines.length === b.spines.length &&
    a.spines.every((s, i) => s.block_start === b.spines[i].block_start && s.block_count === b.spines[i].block_count) &&
    a.toc.every((t, i) => t.block === b.toc[i].block && t.nav_anchor === b.toc[i].nav_anchor && t.source_fragment === b.toc[i].source_fragment)
  )
}

async function open() {
  const id = props.file.id
  stage.value = 'loading'
  loadingText.value = '正在读取书籍…'
  const myGen = ++gen
  closing = false
  try {
    const info = await fetchBookInfo(id).catch(() => null)
    if (info?.title) title.value = info.title
    const progress = await fetchProgress(id).catch((): BookProgress => ({}))

    // L2 manifest 命中 → 本地立即排版显示，不等网络 manifest
    const cached = await persistentCache.getManifest(id).catch(() => null)
    let renderedFromCache = false
    if (cached && myGen === gen) {
      try {
        renderedFromCache = (await setupView(myGen, cached, progress.anchor)) !== null
      } catch {
        if (myGen !== gen) return
        stage.value = 'loading' // L2 数据不可用：回落到网络路径
      }
    }

    // 网络 manifest（no-cache）：主路径 + L2 校验。与 L2 排版语义一致时
    // 保持现状（零 chunk 重取）；不一致（服务端 flow 更新/换书）才重建。
    const flow = await fetchFlow(id)
    if (myGen !== gen) return
    void persistentCache.putManifest(id, flow).catch(() => {})
    if (cached && renderedFromCache && renderedManifest === cached) {
      if (sameLayout(cached, flow)) return
      // L2 与服务端 flow 不一致：丢弃本地排版，按当前阅读位置在新 flow 上重建
      const resume = topAnchor
      loadingText.value = '正在排版…'
      resetFlow()
      await setupView(myGen, flow, resume ?? progress.anchor ?? undefined)
      return
    }
    if (cached && !renderedFromCache) resetFlow() // 失败的 L2 排版残留
    if (!(await setupView(myGen, flow, progress.anchor))) return
  } catch (error) {
    if (myGen !== gen) return
    stage.value = 'error'
    errorText.value = (error as Error).message || '打开失败'
  }
}

function prefetchSurrounding(block: number) {
  const m = manifest.value
  if (!m) return
  const ci = chunkForBlock(m, block)
  if (ci < 0) return
  for (let d = 1; d <= 2; d++) {
    for (const j of [ci + d, ci - d]) {
      if (j >= 0 && j < m.chunks.length) void chunkText(j)
    }
  }
}

// ---- 滑动 ----

function bindSwipe(el: HTMLElement | null) {
  if (!el) return
  let startX = 0
  let startY = 0
  let startTime = 0
  let tracking = false
  let dragging = false

  const release = (dx: number) => {
    const flick = Date.now() - startTime < 300 && Math.abs(dx) > 30
    const shouldTurn = flick || Math.abs(dx) > metrics.pitch * 0.25
    if (!shouldTurn) {
      void goToCol(currentCol, true)
      return
    }
    if (turnBusy) {
      void goToCol(currentCol, true)
      return
    }
    const target = currentCol + (dx < 0 ? 1 : -1)
    const mayReach = target >= 0 && target < cols
    const mayExtend = dx < 0 ? target >= cols && c1 < lastChunkIndex() : target < 0 && c0 > 0
    if (mayReach || mayExtend) void turn(dx < 0 ? 1 : -1)
    else void goToCol(currentCol, true)
  }

  el.addEventListener(
    'touchstart',
    event => {
      tracking = event.touches.length === 1 && stage.value === 'reading'
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
        if (dragging) {
          dragging = false
          setX(-currentCol * metrics.pitch)
          setPromote(false)
        }
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
        setPromote(true) // 跟手拖动期间临时提升合成层
        suppressZoneClickUntil = Date.now() + 600
      }
      const atEdge = (currentCol === 0 && dx > 0) || (currentCol >= cols - 1 && dx < 0)
      setX(-currentCol * metrics.pitch + (atEdge ? dx / 3 : dx))
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
      dragging = false
      release(dx) // 松手后由 goToCol(animate) 收尾：动画结束即释放 will-change
    },
    { passive: true },
  )
  el.addEventListener(
    'touchcancel',
    () => {
      if (!tracking) return
      tracking = false
      if (dragging) {
        dragging = false
        setX(-currentCol * metrics.pitch)
        setPromote(false)
      }
    },
    { passive: true },
  )
}

// ---- 生命周期 ----

function onVisibilityChange() {
  if (document.visibilityState === 'hidden') flushProgress()
}

onMounted(() => {
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
  closing = true
  gen++
  for (const timer of [syncTimer, fontTimer, resizeTimer, progressTimer]) {
    if (timer) clearTimeout(timer)
  }
  currentAnim?.cancel()
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
  <section id="reader-view" class="reader-shell" :class="{ dark: isDark, 'tools-hidden': !toolsVisible }">
    <header class="reader-bar">
      <button id="reader-back" class="reader-icon-btn" aria-label="返回" @click="emit('close')">
        <svg viewBox="0 0 24 24" width="20" height="20" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><path d="m15 18-6-6 6-6" /></svg>
      </button>
      <div class="reader-bar-title"><strong id="reader-title">{{ title }}</strong><small id="reader-kind">{{ kind.toUpperCase() }}</small></div>
      <button class="reader-icon-btn" id="toc-button" aria-label="打开目录" :aria-expanded="tocOpen" @click="openToc">☰</button>
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
      <div class="reader-seek">
        <span id="page-label">{{ pageLabel }}</span>
        <input id="page-slider" type="range" min="0" max="1000" step="1" :value="Math.round(clamp(percentNow, 0, 100) * 10)" aria-label="阅读进度" @input="onSeekInput">
      </div>
      <div class="reader-actions">
        <button id="toc-button-2" class="reader-action-btn" @click="openToc"><b>☰</b><span>目录</span></button>
        <button id="font-button" class="reader-action-btn" @click="fontOpen = !fontOpen"><b>A</b><span>排版</span></button>
        <button id="theme-button" class="reader-action-btn" @click="toggleTheme"><b>◐</b><span>明暗</span></button>
      </div>
    </footer>
  </section>
</template>

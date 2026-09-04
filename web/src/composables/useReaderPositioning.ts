import type { Ref } from 'vue'
import type { FlowManifest, ReadingAnchor } from '../reader/types'
import { spineForBlock } from '../reader/flow'
import { clamp } from '../reader/prefs'

export interface ReaderLayoutMetrics {
  width: number
  height: number
  side: number
  top: number
  bottom: number
  pitch: number
  colHeight: number
}

interface ReaderPositioningState {
  metrics: () => ReaderLayoutMetrics
  currentCol: () => number
  cols: () => number
}

// useReaderPositioning owns the conversion between stable reading anchors and
// the browser's live multi-column DOM. Navigation and progress intentionally
// share these primitives so their distinct semantics cannot drift apart.
export function useReaderPositioning(
  viewportEl: Ref<HTMLElement | null>,
  flowEl: Ref<HTMLElement | null>,
  manifest: Ref<FlowManifest | null>,
  state: ReaderPositioningState,
) {
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
  const metrics = state.metrics()
  if (metrics.pitch <= 0) return 0
  return clamp(Math.floor((rel - metrics.side + 1) / metrics.pitch), 0, Math.max(0, state.cols() - 1))
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
  const metrics = state.metrics()
  const vp = viewportEl.value?.getBoundingClientRect()
  if (!vp) return { x: window.innerWidth / 2, y: metrics.top + 2 }
  return { x: vp.left + metrics.side + 2, y: vp.top + metrics.top + 2 }
}

function contentBounds(): { left: number; right: number; top: number; bottom: number } {
  const metrics = state.metrics()
  const vp = viewportEl.value?.getBoundingClientRect()
  if (!vp) {
    return {
      left: metrics.side,
      right: metrics.width - metrics.side,
      top: metrics.top,
      bottom: metrics.height - metrics.bottom,
    }
  }
  return {
    left: vp.left + metrics.side,
    right: vp.right - metrics.side,
    top: vp.top + metrics.top,
    bottom: vp.bottom - metrics.bottom,
  }
}

function rectInCurrentCol(rect: DOMRect, bounds: { left: number; right: number; top: number; bottom: number }): boolean {
  return (
    rectHasBox(rect) &&
    colFromRect(rect) === state.currentCol() &&
    rect.right > bounds.left &&
    rect.left < bounds.right &&
    rect.bottom > bounds.top &&
    rect.top < bounds.bottom
  )
}

function collapsedRectAt(node: Text, offset: number): DOMRect | null {
  const range = node.ownerDocument.createRange()
  range.setStart(node, clamp(offset, 0, node.data.length))
  range.collapse(true)
  return firstRectOfRange(range)
}

// anchorAtTextFragment 把一个跨栏文本节点映射回当前栏的首个字符。Range
// 的 fragment rect 能告诉我们该文本节点确实出现在当前栏，但 rect 本身不
// 带 offset；利用正常阅读流中 offset→column 的单调关系二分首个当前栏
// caret，避免通过被翻页热区遮挡的 caretRangeFromPoint 取值。
function anchorAtTextFragment(
  blockEl: HTMLElement,
  node: Text,
  firstOffset: number,
  bounds: { left: number; right: number; top: number; bottom: number },
): ReadingAnchor | null {
  let lo = clamp(firstOffset, 0, node.data.length)
  let hi = node.data.length
  while (lo < hi) {
    const mid = (lo + hi) >> 1
    const rect = collapsedRectAt(node, mid)
    if (rect && colFromRect(rect) >= state.currentCol()) hi = mid
    else lo = mid + 1
  }
  const start = Math.max(firstOffset, lo - 2)
  const end = Math.min(node.data.length, lo + 2)
  for (let offset = start; offset <= end; offset++) {
    const rect = collapsedRectAt(node, offset)
    if (rect && rectInCurrentCol(rect, bounds)) return anchorFromNode(blockEl, node, offset)
  }
  return null
}

// anchorFromVisibleBlock 从一个已确认出现在当前栏的块中找当前栏最早的
// 实际内容。段首缩进会让块的左上角/命中点处于空白区，所以这里同时考虑
// 文本 Range 的 line fragment 与媒体元素；Range 命中后保留真实 DOM path 和
// offset，跨栏块不会退回到块首栏。
function anchorFromVisibleBlock(
  blockEl: HTMLElement,
  bounds: { left: number; right: number; top: number; bottom: number },
): ReadingAnchor | null {
  const visibleRects = Array.from(blockEl.getClientRects()).filter(rect => rectInCurrentCol(rect as DOMRect, bounds)) as DOMRect[]
  if (!visibleRects.length) return null

  const start = visualStart(blockEl)
  if (start && rectInCurrentCol(start.rect, bounds)) {
    const anchor = anchorFromNode(blockEl, start.node, start.offset)
    if (anchor) return anchor
  }

  const candidates: Array<{ node: Node; anchor: ReadingAnchor }> = []
  const doc = blockEl.ownerDocument
  const walker = doc.createTreeWalker(blockEl, NodeFilter.SHOW_TEXT)
  for (let node = walker.nextNode(); node; node = walker.nextNode()) {
    const text = node as Text
    const at = text.data.search(/\S/)
    if (at < 0) continue
    const range = doc.createRange()
    range.setStart(text, at)
    range.setEnd(text, text.data.length)
    for (const rect of Array.from(range.getClientRects()) as DOMRect[]) {
      if (!rectInCurrentCol(rect, bounds)) continue
      const anchor = anchorAtTextFragment(blockEl, text, at, bounds)
      if (anchor) candidates.push({ node: text, anchor })
      if (candidates.length) break
    }
    if (candidates.length) break
  }

  for (const media of blockEl.querySelectorAll<HTMLElement>('img,svg,video,canvas,iframe,embed,object,table')) {
    const rect = media.getBoundingClientRect()
    if (rectInCurrentCol(rect, bounds)) candidates.push({ node: media, anchor: anchorFromNode(blockEl, media, -1)! })
  }
  if (candidates.length) {
    let first = candidates[0]
    for (const candidate of candidates.slice(1)) {
      if (first.node.compareDocumentPosition(candidate.node) & Node.DOCUMENT_POSITION_FOLLOWING) break
      first = candidate
    }
    return first.anchor
  }

  // 没有可还原的文本/媒体位置时，仍返回当前栏内块起点；这只覆盖没有
  // 可寻址子节点的内容块，不会把一个能命中的跨栏文本降级为块首。
  return makeAnchorFromBlockEl(blockEl, [], -1)
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
        if (!rect || colFromRect(rect) !== state.currentCol()) {
          const pt = blockAtPoint(x, y)
          if (pt) return anchorFromVisibleBlock(pt, contentBounds())
        }
        return anchor
      }
    }
  }
  const pt = blockAtPoint(x, y)
  if (pt) return anchorFromVisibleBlock(pt, contentBounds())
  return anchorAtTopOfCurrentCol()
}

// anchorAtTopOfCurrentCol 兜底：取当前页顶部可见块。
function anchorAtTopOfCurrentCol(): ReadingAnchor | null {
  const m = manifest.value
  const flow = flowEl.value
  if (!m || !flow) return null
  const bounds = contentBounds()
  const { x, y } = contentOrigin()
  const pt = blockAtPoint(x, y)
  const direct = pt ? anchorFromVisibleBlock(pt, bounds) : null
  if (direct) return direct
  for (const blockEl of flow.querySelectorAll<HTMLElement>('[data-block]')) {
    const anchor = anchorFromVisibleBlock(blockEl, bounds)
    if (anchor) return anchor
  }
  return null
}

return {
  anchorAtTopOfCurrentCol,
  anchorFromNode,
  blockElFor,
  captureTopAnchor,
  colForAnchor,
  colFromRect,
  firstRectOfRange,
  fragmentTargetEl,
  rectHasBox,
  visualStart,
}
}

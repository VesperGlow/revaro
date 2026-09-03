// 连续 reading flow 阅读器浏览器级验证（route-mock，不依赖后端）：
//   1. 开书只拉当前位置附近的 chunk 窗口；翻页热路径零网络；
//   2. chunk 绝不重复请求、窗口 DOM 有界（滑动加载）；
//   3. 字号调整纯客户端重排（无 chunk/manifest 请求）；
//   4. readingAnchor 进度：目录跳转写块锚点，重开按锚点定位到目标 chunk。
import { test, expect, type Page, type Route } from '@playwright/test'

const BOOK_ID = 'book-1'
const ROOT = '00000000-0000-0000-0000-000000000000'
const BLOCKS_PER_CHUNK = 20
const CHUNK_COUNT = 12
const TOTAL_BLOCKS = BLOCKS_PER_CHUNK * CHUNK_COUNT

interface FlowFixture {
  progress?: { anchor?: unknown } | null
  // 覆盖默认目录（服务端 Stable NavAnchor 模型：nav_anchor 绑定 chunk
  // HTML 中的 data-rv-anchor 节点，chunk 为绑定节点所在 chunk，
  // source_fragment/source_path 保留原始 href 供回退）
  toc?: {
    label: string
    depth: number
    spine: number
    block: number
    chunk?: number
    nav_anchor?: string
    source_fragment?: string
    source_path?: string
  }[]
  // 覆盖默认 chunk HTML 生成器
  chunkHTML?: (chunk: number) => string
}

function blockHTML(chunk: number, b: number): string {
  const block = chunk * BLOCKS_PER_CHUNK + b
  return `<p data-block="${block}">c${chunk}b${b} 这是第 ${block} 段正文，用于填充阅读流并驱动客户端分页，句子足够长以便换行排版。</p>`
}

function chunkHTML(chunk: number): string {
  const parts: string[] = []
  for (let b = 0; b < BLOCKS_PER_CHUNK; b++) {
    parts.push(blockHTML(chunk, b))
  }
  return parts.join('\n')
}

let flowRequests: Record<number, number>
let nonChunkRequests: string[]

async function mockAPI(page: Page, fixture: FlowFixture = {}) {
  flowRequests = {}
  nonChunkRequests = []
  const totalChars = TOTAL_BLOCKS * 40
  const manifest = {
    version: 1,
    format: 'epub',
    total_chars: totalChars,
    spines: [{ block_start: 0, block_count: TOTAL_BLOCKS }],
    chunks: Array.from({ length: CHUNK_COUNT }, (_, i) => ({
      index: i,
      block_start: i * BLOCKS_PER_CHUNK,
      block_count: BLOCKS_PER_CHUNK,
      chars: BLOCKS_PER_CHUNK * 40,
    })),
    toc: fixture.toc ?? [
      { label: '开头', depth: 0, spine: 0, block: 0 },
      { label: '中段', depth: 0, spine: 0, block: 5 * BLOCKS_PER_CHUNK + 3 },
      { label: '结尾', depth: 0, spine: 0, block: TOTAL_BLOCKS - 1 },
    ],
  }
  await page.route('**/*', async (route: Route) => {
    const url = new URL(route.request().url())
    const path = url.pathname
    const method = route.request().method()
    if (!path.startsWith('/api/')) return route.continue()
    if (path === '/api/auth/me') return route.fulfill({ status: 200, contentType: 'application/json', body: JSON.stringify({ username: 'admin', has_avatar: false }) })
    if (path === '/api/tasks') return route.fulfill({ status: 200, contentType: 'application/json', body: JSON.stringify({ items: [] }) })
    if (path === '/api/events' || path === '/api/system/status/stream') {
      return route.fulfill({ status: 200, contentType: 'text/event-stream', body: '' })
    }
    if (path === `/api/files/${ROOT}/children`) {
      return route.fulfill({
        status: 200,
        contentType: 'application/json',
        body: JSON.stringify({
          items: [{ id: BOOK_ID, parent_id: ROOT, name: 'book.epub', kind: 'file', size: 1000, mime_type: 'application/epub+zip', status: 'ready', created_at: '', updated_at: '' }],
          total_bytes: 1000,
          file_count: 1,
        }),
      })
    }
    if (path === `/api/files/${ROOT}`) {
      return route.fulfill({ status: 200, contentType: 'application/json', body: JSON.stringify({ file: { id: ROOT, parent_id: null, name: '我的文件', kind: 'directory', size: 0, status: 'ready' }, breadcrumbs: [] }) })
    }
    if (path === `/api/files/${BOOK_ID}/thumbnail`) return route.fulfill({ status: 404, body: '' })
    if (path === `/api/files/${BOOK_ID}`) {
      return route.fulfill({ status: 200, contentType: 'application/json', body: JSON.stringify({ file: { id: BOOK_ID, parent_id: ROOT, name: 'book.epub', kind: 'file', size: 1000, mime_type: 'application/epub+zip', status: 'ready', created_at: '', updated_at: '' }, breadcrumbs: [] }) })
    }
    if (path === `/api/files/${BOOK_ID}/book`) {
      return route.fulfill({ status: 200, contentType: 'application/json', body: JSON.stringify({ format: 'epub', title: '测试书', name: 'book.epub', cover: false, toc: [] }) })
    }
    if (path === `/api/files/${BOOK_ID}/book/progress`) {
      if (method === 'PUT') {
        ;(page as Page & { __savedProgress: unknown }).__savedProgress = route.request().postDataJSON()
        return route.fulfill({ status: 204, body: '' })
      }
      const anchor = fixture.progress?.anchor
      return route.fulfill({ status: 200, contentType: 'application/json', body: JSON.stringify(anchor ? { anchor } : {}) })
    }
    if (path === `/api/files/${BOOK_ID}/book/flow` && method === 'GET') {
      nonChunkRequests.push('flow')
      return route.fulfill({ status: 200, contentType: 'application/json', body: JSON.stringify(manifest) })
    }
    const chunkMatch = path.match(new RegExp(`^/api/files/${BOOK_ID}/book/flow/chunks/(\\d+)$`))
    if (chunkMatch && method === 'GET') {
      const index = Number(chunkMatch[1])
      flowRequests[index] = (flowRequests[index] ?? 0) + 1
      return route.fulfill({ status: 200, contentType: 'text/html; charset=utf-8', body: (fixture.chunkHTML ?? chunkHTML)(index) })
    }
    return route.fulfill({ status: 200, contentType: 'application/json', body: '{}' })
  })
}

async function openReader(page: Page, fixture: FlowFixture = {}) {
  await mockAPI(page, fixture)
  await page.goto('/')
  await page.getByText('book.epub').first().click()
  await expect(page.locator('#reader-view')).toBeVisible()
  await expect(page.locator('#loading')).toBeHidden({ timeout: 20_000 })
  await expect(page.locator('#flow .rf-chunk').first()).toBeVisible({ timeout: 20_000 })
  await expect(page.locator('#page-label')).not.toBeEmpty()
}

async function clickNext(page: Page, times: number, gapMs = 340) {
  for (let i = 0; i < times; i++) {
    await page.locator('#next-zone').click()
    await page.waitForTimeout(gapMs)
  }
}

test('窗口化预取：开书只拉附近 chunk，翻页热路径零网络且绝不重复请求', async ({ page }) => {
  await openReader(page)
  // 打开：中心 chunk 0 → 窗口 [0, 3]（共 4 个 chunk，身后 2 + 前方 3 上限 6）
  const initial = { ...flowRequests }
  expect(Object.keys(initial).sort((a, b) => Number(a) - Number(b))).toEqual(['0', '1', '2', '3'])
  for (const n of Object.values(initial)) expect(n).toBe(1)

  // 连点翻页（动画 260ms，逐个等待）：chunk 只增不减、绝不重复；
  // 窗口 DOM 有界。
  const before = Object.keys(flowRequests).length
  await clickNext(page, 24)
  const after = { ...flowRequests }
  for (const n of Object.values(after)) expect(n).toBe(1)
  expect(Object.keys(after).length).toBeGreaterThanOrEqual(before)
  expect(Object.keys(after).length).toBeLessThanOrEqual(CHUNK_COUNT)
  // 窗口 DOM 有界（增量滑动窗口上限 6 个 chunk）
  await expect
    .poll(() => page.locator('#flow .rf-chunk').count(), { timeout: 5000 })
    .toBeLessThanOrEqual(6)
  // 进度条/标签仍有效
  await expect(page.locator('#page-label')).toContainText('%')
})

test('字号/行距调整纯客户端重排：零 chunk 请求', async ({ page }) => {
  await openReader(page)
  await page.waitForTimeout(600)
  const chunkBefore = Object.keys(flowRequests).length
  const flowBefore = nonChunkRequests.length

  await page.locator('#font-button').click()
  for (let i = 0; i < 4; i++) await page.locator('#font-larger').click()
  await page.waitForTimeout(700) // 防抖 + 重排
  for (let i = 0; i < 3; i++) await page.locator('#font-smaller').click()
  await page.waitForTimeout(700)

  expect(Object.keys(flowRequests).length).toBe(chunkBefore)
  expect(nonChunkRequests.length).toBe(flowBefore)
  await expect(page.locator('#page-label')).toContainText('%')
  // 行距按钮同样即时生效
  await page.locator('.v2-lineheight .font-step').nth(1).click()
  await page.waitForTimeout(600)
  expect(Object.keys(flowRequests).length).toBe(chunkBefore)
})

test('目录跳转写 readingAnchor 进度，重开按锚点定位', async ({ page }) => {
  await openReader(page)

  // 目录跳转：进入中段目标块
  await page.locator('#toc-button').click()
  await page.locator('.toc-item', { hasText: '中段' }).click()
  await expect
    .poll(() => (page as Page & { __savedProgress?: unknown }).__savedProgress ?? null, { timeout: 8000 })
    .not.toBeNull()
  const saved = (page as Page & { __savedProgress: { anchor: { block?: number; spine?: number; path?: number[]; offset?: number } } }).__savedProgress
  expect(saved.anchor).toBeTruthy()
  // 进度 = 当前页顶部锚点：目标块可能落在栏中部，顶部锚点是其前方不远处
  // 的同一章内容（仍处于目标 chunk 邻域，绝不在全书开头）。
  const savedBlock = saved.anchor.block as number
  expect(savedBlock).toBeGreaterThanOrEqual(4 * BLOCKS_PER_CHUNK)
  expect(savedBlock).toBeLessThan(7 * BLOCKS_PER_CHUNK)
  expect(saved.anchor.spine).toBe(0)

  // 重开：进度 GET 返回保存的 anchor → 只拉该 chunk 附近的窗口，不拉开头 chunk
  const resumeAnchor = { spine: 0, block: 8 * BLOCKS_PER_CHUNK + 5, path: [], offset: -1 }
  await page.unroute('**/*')
  await openReader(page, { progress: { anchor: resumeAnchor } })
  const got = { ...flowRequests }
  // chunk 8 中心 → 窗口含 6..10（或更大），绝不能只从头开始
  expect(got[8]).toBe(1)
  expect(got[0] ?? 0).toBe(0)
  const requested = Object.keys(got).map(Number)
  expect(requested.every(i => i >= 5)).toBe(true)
})

test('后退翻页回到开头不崩溃，进度仍为开头锚点', async ({ page }) => {
  await openReader(page)
  await clickNext(page, 6)
  for (let i = 0; i < 10; i++) {
    await page.locator('#prev-zone').click()
    await page.waitForTimeout(320)
  }
  await expect(page.locator('#flow .rf-chunk').first()).toBeVisible()
  await expect(page.locator('#page-label')).toContainText('%')
  await expect(page.locator('#reader-view')).toBeVisible()
})

// ---- 目录 fragment 精确跳转 ----

// 填充文本：约 2300 字 ≈ 桌面视口 3 栏，保证相邻目标不在同一栏。
const FRAG_GAP = '两个目录目标之间的填充文字，用来把内容撑开到不同的分栏，确保间隔足够远。'.repeat(60)

// giantBlockHTML 生成一个跨多栏的顶层块，块内多个 id 元素（可加前置填充，
// 使第一个目标不在块起点）；服务端会在每个目标的首个有效文本位置前绑定
// NavAnchor 标记（rvn-<i>），这里直接模拟绑定结果。
function giantBlockHTML(block: number, ids: string[], leadGap = ''): string {
  const inner = ids
    .map((id, i) => `<span class="toc-anchor" data-rv-anchor="rvn-${i}"></span><span id="${id}">【${id}】</span>`)
    .join(FRAG_GAP)
  return `<div data-block="${block}" data-source-path="OEBPS/frag.xhtml">${leadGap}${inner}${FRAG_GAP}</div>`
}

const FRAG_BLOCK = BLOCKS_PER_CHUNK // chunk 1 的第一个块

function fragmentChunkHTML(chunk: number): string {
  if (chunk !== 1) return chunkHTML(chunk)
  const parts = [giantBlockHTML(FRAG_BLOCK, ['目标一', '目标二', '目标三', '章节 二'])]
  for (let b = 1; b < BLOCKS_PER_CHUNK; b++) parts.push(blockHTML(chunk, b))
  return parts.join('\n')
}

function farFragmentChunkHTML(chunk: number): string {
  if (chunk !== 8) return chunkHTML(chunk)
  const farBlock = 8 * BLOCKS_PER_CHUNK + 3
  const parts: string[] = []
  for (let b = 0; b < 3; b++) parts.push(blockHTML(chunk, b))
  parts.push(giantBlockHTML(farBlock, ['远目标'], FRAG_GAP))
  for (let b = 4; b < BLOCKS_PER_CHUNK; b++) parts.push(blockHTML(chunk, b))
  return parts.join('\n')
}

const FRAG_TOC = [
  { label: '目标一', depth: 0, spine: 0, block: FRAG_BLOCK, chunk: 1, nav_anchor: 'rvn-0', source_fragment: '目标一', source_path: 'OEBPS/frag.xhtml' },
  { label: '目标二', depth: 0, spine: 0, block: FRAG_BLOCK, chunk: 1, nav_anchor: 'rvn-1', source_fragment: '目标二' },
  // 双重转义场景：服务端只解一次码，回退路径需补一次安全 decode
  { label: '编码目标', depth: 0, spine: 0, block: FRAG_BLOCK, chunk: 1, nav_anchor: 'rvn-2', source_fragment: '%E7%9B%AE%E6%A0%87%E4%B8%89' },
  // 非 ASCII + 空格：按属性值直接命中（不能进 selector）
  { label: '空格目标', depth: 0, spine: 0, block: FRAG_BLOCK, chunk: 1, nav_anchor: 'rvn-3', source_fragment: '章节 二' },
  // NavAnchor 绑定节点不在 DOM（旧缓存/被裁剪）→ 回退 source_fragment，
  // 仍找不到 → 回退块起点
  { label: '丢失目标', depth: 0, spine: 0, block: FRAG_BLOCK, chunk: 1, nav_anchor: 'rvn-99', source_fragment: 'not-there' },
  // 无 NavAnchor/无 source_fragment（TXT 或极端回退）→ 块起点
  { label: '无片段', depth: 0, spine: 0, block: FRAG_BLOCK },
]

const FAR_TOC = [{ label: '远目标', depth: 0, spine: 0, block: 8 * BLOCKS_PER_CHUNK + 3, chunk: 8, nav_anchor: 'rvn-0', source_fragment: '远目标' }]

// sectionChunkHTML 模仿真实 EPUB 章节：h1 章首（父级目录目标，服务端把
// NavAnchor 绑定到 h1 首行文本前的标记）+ h4#sigil + 段落。h1 强制从新
// 栏开始并保留 margin-top，用于验证目录跳转后不回弹。
function sectionChunkHTML(chunk: number): string {
  const parts: string[] = []
  for (let b = 0; b < BLOCKS_PER_CHUNK; b++) {
    const block = chunk * BLOCKS_PER_CHUNK + b
    if (b === 0) {
      parts.push(
        `<h1 data-block="${block}" style="break-before: column"><span class="toc-anchor" data-rv-anchor="rvn-doc${chunk}"></span>文档${chunk} 边际海岸的度假之夜之类的很长很长的章节标题文字</h1>`,
      )
    } else if (b === 1) {
      parts.push(`<h4 data-block="${block}" id="sigil_toc_id_${chunk}">${chunk}</h4>`)
    } else {
      parts.push(`<p data-block="${block}">c${chunk}b${b} 这是第 ${block} 段正文，用于填充阅读流并驱动客户端分页，句子足够长以便换行排版。</p>`)
    }
  }
  return parts.join('\n')
}

// currentColumn 读取当前翻到的 CSS column 序号（transform / 栏距）。
async function currentColumn(page: Page): Promise<number> {
  return page.evaluate(() => {
    const flow = document.getElementById('flow') as HTMLElement
    const matrix = new DOMMatrixReadOnly(getComputedStyle(flow).transform)
    return Math.round(-matrix.m41 / flow.clientWidth)
  })
}

// expectFragmentVisible 断言 id 元素的起始 rect 已落在当前可见栏内。
async function expectFragmentVisible(page: Page, id: string) {
  const box = await page.locator('#viewport').boundingBox()
  if (!box) throw new Error('viewport 不可见')
  await expect
    .poll(
      () =>
        page.evaluate(
          ({ id, minX, maxX }) => {
            const el = document.getElementById(id)
            if (!el) return false
            const rect = el.getClientRects()[0]
            return !!rect && rect.left >= minX && rect.left < maxX
          },
          { id, minX: box.x, maxX: box.x + box.width },
        ),
      { timeout: 8000 },
    )
    .toBe(true)
}

test('目录 fragment 精确跳转：同块两个目录项落到不同栏，编码/空格片段可定位，丢失回退块起点', async ({ page }) => {
  await openReader(page, { toc: FRAG_TOC, chunkHTML: fragmentChunkHTML })

  const openTocAndClick = async (label: string) => {
    await page.locator('#toc-button').click()
    await page.locator('.toc-item', { hasText: label }).click()
  }

  await openTocAndClick('目标一')
  await expectFragmentVisible(page, '目标一')
  const colA = await currentColumn(page)

  await openTocAndClick('目标二')
  await expectFragmentVisible(page, '目标二')
  const colB = await currentColumn(page)
  expect(colB).toBeGreaterThan(colA + 1)

  // percent-encoded fragment：客户端安全解码后命中 id=目标三
  await openTocAndClick('编码目标')
  await expectFragmentVisible(page, '目标三')

  // 非 ASCII + 空格 fragment：属性值直接比较命中
  await openTocAndClick('空格目标')
  await expectFragmentVisible(page, '章节 二')

  // fragment 未命中 → 回退块起点（= 块内第一个目标所在栏）
  await openTocAndClick('丢失目标')
  await expectFragmentVisible(page, '目标一')
  const colMissing = await currentColumn(page)

  // 无 fragment 的条目保持旧行为：同样落在块起点
  await openTocAndClick('无片段')
  await expectFragmentVisible(page, '目标一')
  const colNoFrag = await currentColumn(page)

  expect(colMissing).toBe(colNoFrag)
  expect(colNoFrag).toBe(colA)
})

// expectBlockVisible 断言指定 data-block 的起始 rect 落在当前可见栏内。
async function expectBlockVisible(page: Page, block: number) {
  const box = await page.locator('#viewport').boundingBox()
  if (!box) throw new Error('viewport 不可见')
  await expect
    .poll(
      () =>
        page.evaluate(
          ({ block, minX, maxX }) => {
            const el = document.querySelector(`[data-block="${block}"]`)
            if (!el) return false
            const rect = el.getClientRects()[0]
            return !!rect && rect.left >= minX && rect.left < maxX
          },
          { block, minX: box.x, maxX: box.x + box.width },
        ),
      { timeout: 8000 },
    )
    .toBe(true)
}

test('父级目录（无 fragment）跳转不回弹：延迟 windowSync 后仍停留在目标章节', async ({ page }) => {
  // 每个 chunk 一节，模仿真实 EPUB：h1 章首（父级目录目标，服务端把
  // NavAnchor 绑定到 h1 首行文本前的标记）+ h4#sigil + 段落。h1 强制从
  // 新栏开始并保留 margin-top —— 栏顶采样点落在 margin 死区，captureTop
  // 的屏幕读取在该栏可能返回 null（回弹的确定性条件：topAnchor 保留跳转
  // 前旧值 → windowSync 按旧值重建窗口并拉回旧页）。
  const parentToc = Array.from({ length: CHUNK_COUNT }, (_, k) => ({
    label: `文档${k}`,
    depth: 0,
    spine: 0,
    block: k * BLOCKS_PER_CHUNK,
    chunk: k,
    nav_anchor: `rvn-doc${k}`,
  }))
  await openReader(page, { toc: parentToc, chunkHTML: sectionChunkHTML })
  const target = 6 * BLOCKS_PER_CHUNK // 文档6：chunk 6 章首；跳转前窗口为 [0..3]

  await page.locator('#toc-button').click()
  await page.locator('.toc-item', { hasText: '文档6' }).click()

  // 跳转落地：目标章节起点进入当前栏
  await expectBlockVisible(page, target)
  // 延迟 windowSync（goToCol 调度的 200ms 防抖）+ 进度保存之后，
  // 位置必须保持在目标章节，不得恢复到跳转前的位置。
  await page.waitForTimeout(1500)
  await expectBlockVisible(page, target)
  const chunks = await page.evaluate(() =>
    Array.from(document.querySelectorAll('#flow .rf-chunk')).map(c => Number((c as HTMLElement).dataset.chunk)),
  )
  expect(chunks).toContain(6)
  expect(chunks).not.toContain(0)

  // 进度锚点提交在目标章节（导航目标兜底生效），而不是回写跳转前位置
  await expect
    .poll(() => (page as Page & { __savedProgress?: { anchor?: { block?: number } } }).__savedProgress ?? null, { timeout: 8000 })
    .toBeTruthy()
  const savedBlock = (page as Page & { __savedProgress: { anchor: { block?: number } } }).__savedProgress.anchor?.block ?? -1
  expect(savedBlock).toBeGreaterThanOrEqual(6 * BLOCKS_PER_CHUNK)
  expect(savedBlock).toBeLessThan(7 * BLOCKS_PER_CHUNK)
})

test('目录 fragment 在未加载 chunk：先加载目标 chunk 再精确定位', async ({ page }) => {
  await openReader(page, { toc: FAR_TOC, chunkHTML: farFragmentChunkHTML })
  // 初始窗口只拉 chunk 0..3，不含目标 chunk 8
  expect(flowRequests[8] ?? 0).toBe(0)

  await page.locator('#toc-button').click()
  await page.locator('.toc-item', { hasText: '远目标' }).click()

  // 目标 chunk 被加载，且远目标元素精确落到当前栏（不是块起点——
  // 目标前有约一栏的前置填充，回退块起点时它不会出现在当前栏）
  await expect.poll(() => flowRequests[8] ?? 0, { timeout: 8000 }).toBe(1)
  await expectFragmentVisible(page, '远目标')

  // 进度锚点写入目标块邻域（顶部锚点取自目标栏顶部的文本位置）
  await expect
    .poll(() => (page as Page & { __savedProgress?: { anchor?: { block?: number } } }).__savedProgress ?? null, { timeout: 8000 })
    .toBeTruthy()
  const savedBlock = (page as Page & { __savedProgress: { anchor?: { block?: number } } }).__savedProgress.anchor?.block ?? -1
  expect(savedBlock).toBeGreaterThanOrEqual(8 * BLOCKS_PER_CHUNK)
  expect(savedBlock).toBeLessThan(9 * BLOCKS_PER_CHUNK)
})

// ---- 整页图章节标题：容器首 fragment 在上一栏 ----

// 1×1 像素图，靠 width/height 属性撑出整页布局盒（真实 EPUB 清洗后
// 服务端会写入固有尺寸属性，加载前后盒子不变）。
const PIXEL = 'data:image/gif;base64,R0lGODlhAQABAIAAAP///wAAACH5BAEAAAAALAAAAAABAAEAAAICRAEAOw=='

// imagePageChunkHTML 复刻真实 EPUB 的目标形态（p-005.xhtml#id-a002）：
// 连续整页插画（break-inside:avoid）后跟 <p id="id-a002">，块内只有一行
// 空白内容 + 一张整页标题图。栏高 636px（1280×720 视口），逐栏推挤：
//   栏0 填充600 → 栏1 插画一620（剩16）→ 栏2 插画二（楠）500（剩136，
//   目标块的空白行 ≈32px 挤得下）→ 标题图636 放不下，整体推入栏3。
// 服务端把 NavAnchor 直接绑在标题图媒体元素上（data-rv-anchor="rvn-9"），
// 旧逻辑按容器 getClientRects()[0] 算栏会跳到前一张“楠”插画页；前导
// 空白行用 NBSP（\s 空白、引擎不塌缩、必然成行），两种引擎确定性复现。
function imagePageChunkHTML(chunk: number): string {
  if (chunk !== 1) return chunkHTML(chunk)
  const base = chunk * BLOCKS_PER_CHUNK
  const parts = [
    `<div data-block="${base}" style="height:600px">前置填充块，用来把后续整页图推入各自的栏。</div>`,
    `<p data-block="${base + 1}"><img src="${PIXEL}" width="720" height="620" alt="插画一"></p>`,
    `<p data-block="${base + 2}" id="id-nan"><img id="nan-img" src="${PIXEL}" width="720" height="500" alt="插画二（楠）"></p>`,
    `<p data-block="${base + 3}" id="id-a002">&nbsp;
    <img id="title-img" data-rv-anchor="rvn-9" src="${PIXEL}" width="720" height="636" alt="章节标题图"></p>`,
  ]
  for (let b = 4; b < BLOCKS_PER_CHUNK; b++) parts.push(blockHTML(chunk, b))
  return parts.join('\n')
}

// chunk 0 首段绑定「无 fragment → spine 首个真实可见内容」的书首 NavAnchor
function bookStartChunkHTML(chunk: number): string {
  if (chunk !== 0) return imagePageChunkHTML(chunk)
  return chunkHTML(chunk).replace(
    '<p data-block="0">',
    '<p data-block="0"><span class="toc-anchor" data-rv-anchor="rvn-10"></span>',
  )
}

const IMAGE_TOC = [
  {
    label: '烛林',
    depth: 0,
    spine: 0,
    block: 1 * BLOCKS_PER_CHUNK + 3,
    chunk: 1,
    nav_anchor: 'rvn-9',
    source_fragment: 'id-a002',
    source_path: 'OEBPS/Text/p-005.xhtml',
  },
  { label: '书首', depth: 0, spine: 0, block: 0, chunk: 0, nav_anchor: 'rvn-10' },
]

// elInViewport 判断元素主盒是否与当前可见视口横向相交（整页图是原子盒，
// 不会跨栏，getBoundingClientRect 即其唯一布局盒）。
async function elInViewport(page: Page, selector: string): Promise<boolean> {
  const box = await page.locator('#viewport').boundingBox()
  if (!box) throw new Error('viewport 不可见')
  return page.evaluate(
    ({ selector, minX, maxX }) => {
      const el = document.querySelector(selector)
      if (!el) return false
      const rect = el.getBoundingClientRect()
      return rect.width > 0 && rect.right > minX && rect.left < maxX
    },
    { selector, minX: box.x, maxX: box.x + box.width },
  )
}

test('整页图章节标题跳转：服务端 NavAnchor 绑在媒体元素上，容器首 fragment 在上一栏也能正确定位', async ({ page }) => {
  await openReader(page, { toc: IMAGE_TOC, chunkHTML: bookStartChunkHTML })

  // NavAnchor 精确跳转：必须落在章节标题图（绑定媒体元素所在栏），
  // 而不是前一张“楠”插画
  await page.locator('#toc-button').click()
  await page.locator('.toc-item', { hasText: '烛林' }).click()
  await expect.poll(() => elInViewport(page, '#title-img'), { timeout: 8000 }).toBe(true)
  expect(await elInViewport(page, '#nan-img')).toBe(false)

  // 落地后的 windowSync（200ms 防抖）与进度保存不得把页面拉回上一栏
  await page.waitForTimeout(1500)
  expect(await elInViewport(page, '#title-img')).toBe(true)

  // 字号重排（relayout 按 topAnchor 重对齐）后仍停在标题图页
  await page.locator('#font-button').click()
  await page.locator('#font-larger').click()
  await page.waitForTimeout(700)
  expect(await elInViewport(page, '#title-img')).toBe(true)
  await page.locator('#font-smaller').click()
  await page.waitForTimeout(700)
  expect(await elInViewport(page, '#title-img')).toBe(true)

  // 进度锚点提交在标题图块（captureTop 兜底 / 导航目标一致）
  const savedBlock = (page as Page & { __savedProgress?: { anchor?: { block?: number } } }).__savedProgress?.anchor?.block ?? -1
  expect(savedBlock).toBe(1 * BLOCKS_PER_CHUNK + 3)
})

test('无 fragment 目录按服务端 NavAnchor 落到 spine 首个真实可见内容（书首）', async ({ page }) => {
  await openReader(page, { toc: IMAGE_TOC, chunkHTML: bookStartChunkHTML })

  await page.locator('#toc-button').click()
  await page.locator('.toc-item', { hasText: '书首' }).click()
  // 绑定节点在 chunk 0 首段文本前 → 落回书首，而不是烛林标题图
  await expectBlockVisible(page, 0)
  expect(await elInViewport(page, '#title-img')).toBe(false)
})

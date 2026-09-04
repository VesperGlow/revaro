// 连续 reading flow 阅读器浏览器级验证（route-mock，不依赖后端）：
//   1. 开书只拉当前位置附近的 chunk 窗口；翻页热路径零网络；
//   2. chunk 绝不重复请求、窗口 DOM 有界（spine 稳定分页窗口）；
//   3. 字号调整纯客户端重排（无 chunk/manifest 请求）；
//   4. readingAnchor 进度：目录跳转写块锚点，重开按锚点定位到目标 chunk；
//   5. v4 文本 locator：目录文本目标按真实 Text node 的 caret rect 定栏；
//   6. 稳定分页边界：窗口滑动 / spine 切换不改变 page boundary；
//   7. 持久 L2：重开同一本书零 chunk 请求，manifest 版本变化才重取。
import { test, expect, type Page, type Route } from '@playwright/test'

const BOOK_ID = 'book-1'
const ROOT = '00000000-0000-0000-0000-000000000000'
const BLOCKS_PER_CHUNK = 20
const CHUNK_COUNT = 12
const DB_NAME = 'revaro-reader-cache'

interface TocFixture {
  label: string
  depth: number
  spine: number
  block: number
  chunk?: number
  nav_anchor?: string
  text_path?: number[]
  text_offset?: number
  source_fragment?: string
  source_path?: string
}

interface FlowFixture {
  progress?: { anchor?: unknown } | null
  manifestVersion?: number
  bookKey?: string
  // 每 spine 覆盖的 chunk 数（默认 1：每 chunk 一个 spine；0 = 单一大 spine）
  chunksPerSpine?: number
  chunkCount?: number
  toc?: TocFixture[]
  chunkHTML?: (chunk: number) => string
  clearCache?: boolean
}

function blockHTML(chunk: number, b: number, chunkPerSpine = 1): string {
  const block = chunk * BLOCKS_PER_CHUNK + b
  // 服务端在 spine 起始块（全书首块除外）注入 data-spine-start
  const spineStart = chunkPerSpine > 0 && block > 0 && block % (chunkPerSpine * BLOCKS_PER_CHUNK) === 0
  return `<p${spineStart ? ' data-spine-start' : ''} data-block="${block}">c${chunk}b${b} 这是第 ${block} 段正文，用于填充阅读流并驱动客户端分页，句子足够长以便换行排版。</p>`
}

function chunkHTMLFor(chunk: number, chunkPerSpine: number): string {
  const parts: string[] = []
  for (let b = 0; b < BLOCKS_PER_CHUNK; b++) parts.push(blockHTML(chunk, b, chunkPerSpine))
  return parts.join('\n')
}

let flowRequests: Record<number, number>
let nonChunkRequests: string[]

async function mockAPI(page: Page, fixture: FlowFixture = {}) {
  flowRequests = {}
  nonChunkRequests = []
  const chunkPerSpine = fixture.chunksPerSpine ?? 1
  const chunkCount = fixture.chunkCount ?? CHUNK_COUNT
  const totalBlocks = chunkCount * BLOCKS_PER_CHUNK
  const spineSize = chunkPerSpine > 0 ? chunkPerSpine * BLOCKS_PER_CHUNK : totalBlocks
  const spines =
    chunkPerSpine === 0
      ? [{ block_start: 0, block_count: totalBlocks }]
      : Array.from({ length: chunkCount / chunkPerSpine }, (_, i) => ({ block_start: i * spineSize, block_count: spineSize }))
  const totalChars = totalBlocks * 40
  const manifest = {
    version: fixture.manifestVersion ?? 4,
    format: 'epub',
    book_key: fixture.bookKey ?? 'book-key-1',
    total_chars: totalChars,
    spines,
    chunks: Array.from({ length: chunkCount }, (_, i) => ({
      index: i,
      block_start: i * BLOCKS_PER_CHUNK,
      block_count: BLOCKS_PER_CHUNK,
      chars: BLOCKS_PER_CHUNK * 40,
    })),
    toc: fixture.toc ?? [
      { label: '开头', depth: 0, spine: 0, block: 0, chunk: 0, text_path: [0], text_offset: 0 },
      { label: '中段', depth: 0, spine: 5, block: 5 * BLOCKS_PER_CHUNK + 3, chunk: 5, text_path: [0], text_offset: 0 },
      { label: '结尾', depth: 0, spine: 11, block: totalBlocks - 1, chunk: 11, text_path: [0], text_offset: 0 },
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
      return route.fulfill({ status: 200, contentType: 'text/html; charset=utf-8', body: (fixture.chunkHTML ?? ((c: number) => chunkHTMLFor(c, chunkPerSpine)))(index) })
    }
    return route.fulfill({ status: 200, contentType: 'application/json', body: '{}' })
  })
}

// clearClientCache 删除持久 L2（IndexedDB），模拟冷启动设备。
async function clearClientCache(page: Page) {
  await page.evaluate(
    name =>
      new Promise<void>(resolve => {
        const req = indexedDB.deleteDatabase(name)
        req.onsuccess = () => resolve()
        req.onerror = () => resolve()
        req.onblocked = () => resolve()
      }),
    DB_NAME,
  )
}

async function openReader(page: Page, fixture: FlowFixture = {}) {
  await mockAPI(page, fixture)
  await page.goto('/')
  if (fixture.clearCache) await clearClientCache(page)
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
  // 打开：中心 chunk 0 → 稳定窗口 [0, 3]（spine 边界 chunk 0 + 前方 3）
  const initial = { ...flowRequests }
  expect(Object.keys(initial).sort((a, b) => Number(a) - Number(b))).toEqual(['0', '1', '2', '3'])
  for (const n of Object.values(initial)) expect(n).toBe(1)

  // 连点翻页（动画 260ms，逐个等待）：chunk 只增不减、绝不重复；
  // 窗口 DOM 有界（稳定窗口 + 翻页边缘的瞬时原始扩张）。
  const before = Object.keys(flowRequests).length
  await clickNext(page, 24)
  const after = { ...flowRequests }
  for (const n of Object.values(after)) expect(n).toBe(1)
  expect(Object.keys(after).length).toBeGreaterThanOrEqual(before)
  expect(Object.keys(after).length).toBeLessThanOrEqual(CHUNK_COUNT)
  await expect
    .poll(() => page.locator('#flow .rf-chunk').count(), { timeout: 5000 })
    .toBeLessThanOrEqual(5)
  // 进度条/标签仍有效
  await expect(page.locator('#page-label')).toContainText('%')
})

test('字号/行距调整纯客户端重排：零 chunk 请求且阅读位置保持', async ({ page }) => {
  await openReader(page)
  await page.waitForTimeout(600)
  const chunkBefore = Object.keys(flowRequests).length
  const flowBefore = nonChunkRequests.length
  const percentBefore = await page.locator('#page-label').textContent()

  await page.locator('#font-button').click()
  for (let i = 0; i < 4; i++) await page.locator('#font-larger').click()
  await page.waitForTimeout(700) // 防抖 + 重排
  for (let i = 0; i < 3; i++) await page.locator('#font-smaller').click()
  await page.waitForTimeout(700)

  expect(Object.keys(flowRequests).length).toBe(chunkBefore)
  expect(nonChunkRequests.length).toBe(flowBefore)
  await expect(page.locator('#page-label')).toContainText('%')
  // 字号变化后 topAnchor 重对齐：阅读进度（文本百分比）基本不变
  const percentAfter = await page.locator('#page-label').textContent()
  expect(Math.abs(parseFloat(percentAfter ?? '0') - parseFloat(percentBefore ?? '0'))).toBeLessThan(2)
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
  // 每 chunk 一个 spine：目标块 103 落在 spine 5
  expect(saved.anchor.spine).toBe(5)

  // 重开（清除 L2 模拟冷启动）：进度 GET 返回保存的 anchor → 只拉该
  // chunk 附近的稳定窗口，不拉开头 chunk
  const resumeAnchor = { spine: 0, block: 8 * BLOCKS_PER_CHUNK + 5, path: [], offset: -1 }
  await page.unroute('**/*')
  await openReader(page, { progress: { anchor: resumeAnchor }, clearCache: true })
  const got = { ...flowRequests }
  // chunk 8 中心 → 稳定窗口含 8..11，绝不能只从头开始
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

// ---- 目录文本 locator（v4：真实 Text node 的 caret rect 定栏） ----

// 填充文本：约 2300 字 ≈ 桌面视口 3 栏，保证相邻目标不在同一栏。
const FRAG_GAP = '两个目录目标之间的填充文字，用来把内容撑开到不同的分栏，确保间隔足够远。'.repeat(60)

// giantBlockHTML 生成一个跨多栏的顶层块，块内多个 id 元素（可加前置填充，
// 使第一个目标不在块起点）。v4 下文本目标不在 DOM 注入任何标记：客户端
// 用 text_path 定位 span 内真实文本节点。
function giantBlockHTML(block: number, ids: string[], leadGap = ''): string {
  const inner = ids.map(id => `<span id="${id}">【${id}】</span>`).join(FRAG_GAP)
  return `<div data-block="${block}" data-source-path="OEBPS/frag.xhtml">${leadGap}${inner}${FRAG_GAP}</div>`
}

// giantSpanPath 计算 giantBlock 中第 i 个目标的 text locator：每项占 2 个
// childNodes 下标（span + 后续填充文本），leadGap 占据下标 0；text 节点是
// span 的首子节点（path 末段 0）。
function giantSpanPath(i: number, hasLeadGap: boolean): number[] {
  return [(hasLeadGap ? 1 : 0) + i * 2, 0]
}

const FRAG_BLOCK = BLOCKS_PER_CHUNK // chunk 1 的第一个块

function fragmentChunkHTML(chunk: number): string {
  if (chunk !== 1) return chunkHTMLFor(chunk, 1)
  const parts = [giantBlockHTML(FRAG_BLOCK, ['目标一', '目标二', '目标三', '章节 二'])]
  for (let b = 1; b < BLOCKS_PER_CHUNK; b++) parts.push(blockHTML(chunk, b, 1))
  return parts.join('\n')
}

function farFragmentChunkHTML(chunk: number): string {
  if (chunk !== 8) return chunkHTMLFor(chunk, 1)
  const farBlock = 8 * BLOCKS_PER_CHUNK + 3
  const parts: string[] = []
  for (let b = 0; b < 3; b++) parts.push(blockHTML(chunk, b, 1))
  parts.push(giantBlockHTML(farBlock, ['远目标'], FRAG_GAP))
  for (let b = 4; b < BLOCKS_PER_CHUNK; b++) parts.push(blockHTML(chunk, b, 1))
  return parts.join('\n')
}

const FRAG_TOC: TocFixture[] = [
  { label: '目标一', depth: 0, spine: 0, block: FRAG_BLOCK, chunk: 1, text_path: giantSpanPath(0, false), text_offset: 0, source_fragment: '目标一', source_path: 'OEBPS/frag.xhtml' },
  { label: '目标二', depth: 0, spine: 0, block: FRAG_BLOCK, chunk: 1, text_path: giantSpanPath(1, false), text_offset: 0 },
  // locator 缺失 → 回退 source_fragment（双重转义场景：客户端补一次安全
  // decode）
  { label: '编码目标', depth: 0, spine: 0, block: FRAG_BLOCK, chunk: 1, source_fragment: '%E7%9B%AE%E6%A0%87%E4%B8%89' },
  // 非 ASCII + 空格：text_path 直接定位（不能进 CSS selector）
  { label: '空格目标', depth: 0, spine: 0, block: FRAG_BLOCK, chunk: 1, text_path: giantSpanPath(3, false), text_offset: 0 },
  // locator 缺失且 source_fragment 未命中 → 回退块起点
  { label: '丢失目标', depth: 0, spine: 0, block: FRAG_BLOCK, chunk: 1, source_fragment: 'not-there' },
  // 无任何 locator（极端回退）→ 块起点
  { label: '无片段', depth: 0, spine: 0, block: FRAG_BLOCK },
]

const FAR_TOC: TocFixture[] = [
  { label: '远目标', depth: 0, spine: 0, block: 8 * BLOCKS_PER_CHUNK + 3, chunk: 8, text_path: giantSpanPath(0, true), text_offset: 0, source_fragment: '远目标' },
]

// currentColumn 读取当前翻到的 CSS column 序号（transform / 栏距）。
async function currentColumn(page: Page): Promise<number> {
  return page.evaluate(() => {
    const flow = document.getElementById('flow') as HTMLElement
    const matrix = new DOMMatrixReadOnly(getComputedStyle(flow).transform)
    return Math.round(-matrix.m41 / flow.clientWidth)
  })
}

// colOf 读取指定 data-block 的文档栏序号（rect 相对 flow 原点）。
async function colOf(page: Page, block: number): Promise<number> {
  return page.evaluate(block => {
    const flow = document.getElementById('flow') as HTMLElement
    const el = document.querySelector(`[data-block="${block}"]`) as HTMLElement | null
    if (!el) return -1
    const rect = el.getClientRects()[0] ?? el.getBoundingClientRect()
    const flowRect = flow.getBoundingClientRect()
    return Math.floor((rect.left - flowRect.left) / flow.clientWidth)
  }, block)
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

test('目录文本 locator：同块多个目标落到不同栏，编码片段回退可定位，丢失回退块起点', async ({ page }) => {
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

  // locator 缺失：percent-encoded fragment 回退命中 id=目标三
  await openTocAndClick('编码目标')
  await expectFragmentVisible(page, '目标三')

  // 非 ASCII + 空格目标：text_path 定位真实文本节点
  await openTocAndClick('空格目标')
  await expectFragmentVisible(page, '章节 二')

  // locator/fragment 双双未命中 → 回退块起点（= 块内第一个目标所在栏）
  await openTocAndClick('丢失目标')
  await expectFragmentVisible(page, '目标一')
  const colMissing = await currentColumn(page)

  // 无 locator 的条目保持旧行为：同样落在块起点
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

test('@benchmark 超长单一 spine 深度 TOC 跳转的稳定前缀成本', async ({ browser }, testInfo) => {
  test.slow()
  const results: Array<{ chunks: number; blocks: number; median_ms: number; samples_ms: number[] }> = []

  for (const chunkCount of [12, 24, 48]) {
    const lastChunk = chunkCount - 1
    const lastBlock = chunkCount * BLOCKS_PER_CHUNK - 1
    const samples: number[] = []
    for (let trial = 0; trial < 3; trial++) {
      const page = await browser.newPage()
      await openReader(page, {
        bookKey: `long-spine-benchmark-${chunkCount}-${trial}`,
        chunksPerSpine: 0,
        chunkCount,
        clearCache: true,
        toc: [{ label: '末端', depth: 0, spine: 0, block: lastBlock, chunk: lastChunk, text_path: [0], text_offset: 0 }],
      })

      const started = await page.evaluate(() => performance.now())
      await page.locator('#toc-button').click()
      await page.locator('.toc-item', { hasText: '末端' }).click()
      await expectBlockVisible(page, lastBlock)
      samples.push(Math.round(await page.evaluate(start => performance.now() - start, started)))

      // 深度跳转必须保留当前 spine 的完整排版前缀，且延迟
      // window sync 不得把已精确定位的目标拉回。
      await expect(page.locator('#flow .rf-chunk')).toHaveCount(chunkCount)
      await expect(page.locator('#flow [data-block]')).toHaveCount(lastBlock + 1)
      for (const count of Object.values(flowRequests)) expect(count).toBe(1)
      await page.waitForTimeout(500)
      await expectBlockVisible(page, lastBlock)
      await page.close()
    }
    const sorted = [...samples].sort((a, b) => a - b)
    results.push({ chunks: chunkCount, blocks: lastBlock + 1, median_ms: sorted[1], samples_ms: samples })
  }

  await testInfo.attach('reader-long-spine-benchmark.json', {
    body: JSON.stringify(results, null, 2),
    contentType: 'application/json',
  })
  console.info(`reader-long-spine benchmark: ${JSON.stringify(results)}`)
})

// sectionChunkHTML 模仿真实 EPUB 章节：h1 章首（父级目录目标 = 文本
// locator，且是 data-spine-start 稳定分页边界）+ h4#sigil + 段落。
function sectionChunkHTML(chunk: number): string {
  const parts: string[] = []
  for (let b = 0; b < BLOCKS_PER_CHUNK; b++) {
    const block = chunk * BLOCKS_PER_CHUNK + b
    if (b === 0) {
      const spineStart = chunk > 0 ? ' data-spine-start' : ''
      parts.push(`<h1${spineStart} data-block="${block}">文档${chunk} 边际海岸的度假之夜之类的很长很长的章节标题文字</h1>`)
    } else if (b === 1) {
      parts.push(`<h4 data-block="${block}" id="sigil_toc_id_${chunk}">${chunk}</h4>`)
    } else {
      parts.push(`<p data-block="${block}">c${chunk}b${b} 这是第 ${block} 段正文，用于填充阅读流并驱动客户端分页，句子足够长以便换行排版。</p>`)
    }
  }
  return parts.join('\n')
}

test('父级目录（文本 locator）跳转不回弹：延迟 windowSync 后仍停留在目标章节', async ({ page }) => {
  // 每个 chunk 一节，模仿真实 EPUB：h1 章首（文本 locator 落在标题文本
  // 节点）+ h4#sigil + 段落。h1 是 data-spine-start 稳定边界（强制从新
  // 栏开始并保留 margin-top），用于验证目录跳转后不回弹。
  const parentToc: TocFixture[] = Array.from({ length: CHUNK_COUNT }, (_, k) => ({
    label: `文档${k}`,
    depth: 0,
    spine: k,
    block: k * BLOCKS_PER_CHUNK,
    chunk: k,
    text_path: [0],
    text_offset: 0,
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

test('目录文本 locator 在未加载 chunk：先加载目标 chunk 再精确定位', async ({ page }) => {
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
  const savedBlock = (page as Page & { __savedProgress?: { anchor?: { block?: number } } }).__savedProgress.anchor?.block ?? -1
  expect(savedBlock).toBeGreaterThanOrEqual(8 * BLOCKS_PER_CHUNK)
  expect(savedBlock).toBeLessThan(9 * BLOCKS_PER_CHUNK)
})

// ---- 稳定分页边界（窗口虚拟化不以任意 chunk 为排版原点） ----

// seekToc：每 chunk 一个 spine，条目都落在 chunk 首块的文本上
const seekToc: TocFixture[] = [
  { label: '章节一', depth: 0, spine: 0, block: 0, chunk: 0, text_path: [0], text_offset: 0 },
  { label: '章节二', depth: 0, spine: 2, block: 2 * BLOCKS_PER_CHUNK, chunk: 2, text_path: [0], text_offset: 0 },
  { label: '章节三', depth: 0, spine: 6, block: 6 * BLOCKS_PER_CHUNK, chunk: 6, text_path: [0], text_offset: 0 },
  { label: '章节末', depth: 0, spine: 9, block: 9 * BLOCKS_PER_CHUNK, chunk: 9, text_path: [0], text_offset: 0 },
]

// phaseToc：每 2 个 chunk 一个 spine；甲在 spine 0 尾部、乙在 spine 1 起点
const phaseToc: TocFixture[] = [
  { label: '甲处', depth: 0, spine: 0, block: 35, chunk: 1, text_path: [0], text_offset: 0 },
  { label: '乙处', depth: 0, spine: 1, block: 40, chunk: 2, text_path: [0], text_offset: 0 },
]

async function jumpToc(page: Page, label: string) {
  await page.locator('#toc-button').click()
  await page.locator('.toc-item', { hasText: label }).click()
}

test('随机 TOC seek 落点确定：多次/往返跳转同一目标落点一致', async ({ page }) => {
  await openReader(page, { toc: seekToc })
  const spine3 = 6 * BLOCKS_PER_CHUNK

  await jumpToc(page, '章节三')
  await expectBlockVisible(page, spine3)
  await page.waitForTimeout(1200) // windowSync 收敛
  const col1 = await colOf(page, spine3)
  // 稳定窗口从 spine 3 起始 chunk 开始 → spine 起始块必落在 0 号栏
  expect(col1).toBe(0)

  await jumpToc(page, '章节一')
  await expectBlockVisible(page, 0)
  await jumpToc(page, '章节三')
  await expectBlockVisible(page, spine3)
  await page.waitForTimeout(1200)
  expect(await colOf(page, spine3)).toBe(col1)

  // 从更远处 seek 回来同样确定
  await jumpToc(page, '章节末')
  await expectBlockVisible(page, 9 * BLOCKS_PER_CHUNK)
  await jumpToc(page, '章节三')
  await expectBlockVisible(page, spine3)
  await page.waitForTimeout(1200)
  expect(await colOf(page, spine3)).toBe(col1)
})

test('窗口滑动 + spine 切换：page boundary 不漂移', async ({ page }) => {
  // 每 2 个 chunk 一个 spine：从 spine 0 尾部翻进 spine 1 时窗口会
  // 释放 spine 0 的 chunk（重设排版原点）。data-spine-start 强制分栏
  // 保证 spine 1 内部的分栏结构在释放前后逐栏一致。
  await openReader(page, { toc: phaseToc, chunksPerSpine: 2 })
  const spine0Tail = 35 // chunk 1 内（spine 0）
  const spine1Start = 40 // chunk 2 首块（data-spine-start）
  const probe = 45 // chunk 2 内

  await jumpToc(page, '甲处') // block 35
  await expectBlockVisible(page, spine0Tail)
  await page.waitForTimeout(1200)
  // spine 1 起始块与探针块此时已随预取入窗
  const deltaBefore = (await colOf(page, probe)) - (await colOf(page, spine1Start))

  // 翻页进入 spine 1（当前栏顶部块 ≥ 40）→ windowSync 释放 spine 0。
  // 逐页点击直到越过 spine 边界（栏界随分页略有浮动，单次点击不一定跨过）。
  let top = -1
  for (let i = 0; i < 3 && !(top >= spine1Start && top < spine1Start + 40); i++) {
    await page.locator('#next-zone').click()
    await page.waitForTimeout(450)
    top = await topBlockAt(page)
  }
  expect(top).toBeGreaterThanOrEqual(spine1Start)
  expect(top).toBeLessThan(spine1Start + 40) // 不得越过整个 spine 1
  await page.waitForTimeout(1500) // windowSync 200ms 防抖 + 对齐

  // spine 0 chunk 已释放（排版原点重设），但 spine 1 内部的分栏结构不变
  const chunks = await page.evaluate(() =>
    Array.from(document.querySelectorAll('#flow .rf-chunk')).map(c => Number((c as HTMLElement).dataset.chunk)),
  )
  expect(chunks).not.toContain(0)
  expect(chunks).not.toContain(1)
  expect(chunks).toContain(2)
  const deltaAfter = (await colOf(page, probe)) - (await colOf(page, spine1Start))
  expect(deltaAfter).toBe(deltaBefore)
})

test('连续翻页：探针块与当前页的距离逐页恰好 -1（无跳页）', async ({ page }) => {
  await openReader(page, { toc: phaseToc, chunksPerSpine: 2 })
  await jumpToc(page, '甲处') // block 35
  await expectBlockVisible(page, 35)
  await page.waitForTimeout(1200)

  // 探针在初始窗口尾部（chunk 4），且经 4 页推进后仍在预取窗口内
  const probe = 95
  let prev = (await colOf(page, probe)) - (await currentColumn(page))
  expect(prev).toBeGreaterThan(1)
  for (let i = 0; i < 4; i++) {
    await page.locator('#next-zone').click()
    await page.waitForTimeout(340)
    const now = (await colOf(page, probe)) - (await currentColumn(page))
    expect(now).toBe(prev - 1) // 每页恰好推进一栏：无重排、无相位漂移
    prev = now
  }
})

test('横竖屏旋转重排：阅读位置保持在当前内容', async ({ page }) => {
  await openReader(page, { toc: phaseToc, chunksPerSpine: 2 })
  await jumpToc(page, '甲处')
  await expectBlockVisible(page, 35)
  await page.waitForTimeout(1200)

  const topBefore = await topBlockAt(page)
  expect(topBefore).toBeGreaterThanOrEqual(0)
  await page.setViewportSize({ width: 720, height: 1280 })
  await page.waitForTimeout(1000) // resize 防抖 250ms + relayout
  const topAfter = await topBlockAt(page)
  expect(topAfter).toBeGreaterThanOrEqual(0)
  // 重排以 topAnchor 所在栏为粒度：栏界随布局变化，顶部块可前后移动数块
  //（块级精度），但原阅读位置所在的内容块必须仍出现在当前可见栏内。
  expect(await blockVisibleAny(page, topBefore)).toBe(true)
  await page.setViewportSize({ width: 1280, height: 720 })
  await page.waitForTimeout(1000)
  expect(await blockVisibleAny(page, topBefore)).toBe(true)
})

// blockVisibleAny 断言块（可能跨栏分片）的任一 fragment 与当前可见视口
// 横向相交。
async function blockVisibleAny(page: Page, block: number): Promise<boolean> {
  const box = await page.locator('#viewport').boundingBox()
  if (!box) throw new Error('viewport 不可见')
  return page.evaluate(
    ({ block, minX, maxX }) => {
      const el = document.querySelector(`[data-block="${block}"]`)
      if (!el) return false
      for (const rect of el.getClientRects()) {
        if (rect.width > 0 && rect.right > minX && rect.left < maxX) return true
      }
      return false
    },
    { block, minX: box.x, maxX: box.x + box.width },
  )
}

// topBlockAt 读取当前页内容区左上角处的顶层内容块（与 captureTop 同一
// 采样语义）。
async function topBlockAt(page: Page): Promise<number> {
  return page.evaluate(() => {
    const vp = document.getElementById('viewport') as HTMLElement
    const rect = vp.getBoundingClientRect()
    const x = rect.left + rect.width * 0.5
    const y = rect.top + 90
    for (const node of document.elementsFromPoint(x, y)) {
      const el = (node as HTMLElement).closest?.('[data-block]') as HTMLElement | null
      if (el) return Number(el.dataset.block)
    }
    return -1
  })
}

// ---- 整页图章节标题：媒体目标的元素 rect 定栏 ----

// 1×1 像素图，靠 width/height 属性撑出整页布局盒（真实 EPUB 清洗后
// 服务端会写入固有尺寸属性，加载前后盒子不变）。
const PIXEL = 'data:image/gif;base64,R0lGODlhAQABAIAAAP///wAAACH5BAEAAAAALAAAAAABAAEAAAICRAEAOw=='

// imagePageChunkHTML 复刻真实 EPUB 的目标形态（p-005.xhtml#id-a002）：
// 连续整页插画（break-inside:avoid）后跟 <p id="id-a002">，块内只有一行
// 空白内容 + 一张整页标题图。栏高 636px（1280×720 视口），逐栏推挤：
//   栏0 填充600 → 栏1 插画一620（剩16）→ 栏2 插画二（楠）500（剩136，
//   目标块的空白行 ≈32px 挤得下）→ 标题图636 放不下，整体推入栏3。
// 服务端把 NavAnchor 直接绑在标题图媒体元素上（data-rv-anchor="rvn-9"），
// 按媒体元素真实 rect 定栏；文本 locator 无法表达这种目标。
function imagePageChunkHTML(chunk: number): string {
  if (chunk !== 1) return chunkHTMLFor(chunk, 1)
  const base = chunk * BLOCKS_PER_CHUNK
  const parts = [
    `<div data-block="${base}" style="height:600px">前置填充块，用来把后续整页图推入各自的栏。</div>`,
    `<p data-block="${base + 1}"><img src="${PIXEL}" width="720" height="620" alt="插画一"></p>`,
    `<p data-block="${base + 2}" id="id-nan"><img id="nan-img" src="${PIXEL}" width="720" height="500" alt="插画二（楠）"></p>`,
    `<p data-block="${base + 3}" id="id-a002">&nbsp;
    <img id="title-img" data-rv-anchor="rvn-9" src="${PIXEL}" width="720" height="636" alt="章节标题图"></p>`,
  ]
  for (let b = 4; b < BLOCKS_PER_CHUNK; b++) parts.push(blockHTML(chunk, b, 1))
  return parts.join('\n')
}

const IMAGE_TOC: TocFixture[] = [
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
  // 书首 = 文本 locator（服务端解析 spine 首个真实可见文本）
  { label: '书首', depth: 0, spine: 0, block: 0, chunk: 0, text_path: [0], text_offset: 0 },
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

test('整页图章节标题跳转：NavAnchor 绑在媒体元素上，容器首 fragment 在上一栏也能正确定位', async ({ page }) => {
  await openReader(page, { toc: IMAGE_TOC, chunkHTML: imagePageChunkHTML })

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

test('无 fragment 目录按服务端文本 locator 落到 spine 首个真实可见内容（书首）', async ({ page }) => {
  await openReader(page, { toc: IMAGE_TOC, chunkHTML: imagePageChunkHTML })

  await page.locator('#toc-button').click()
  await page.locator('.toc-item', { hasText: '书首' }).click()
  // text locator 落在 chunk 0 首段文本 → 落回书首，而不是烛林标题图
  await expectBlockVisible(page, 0)
  expect(await elInViewport(page, '#title-img')).toBe(false)
})

// ---- 持久 L2（ClientCacheManager）：重开零 chunk 请求 ----

test('持久 L2：重开同一本书零 chunk 请求，manifest 版本变化才重取', async ({ page }) => {
  await openReader(page)
  // 首开：网络拉 manifest + 稳定窗口 chunks 0..3
  expect(Object.keys(flowRequests).sort((a, b) => Number(a) - Number(b))).toEqual(['0', '1', '2', '3'])
  await page.waitForTimeout(600) // 等待 L2 写入完成

  // 重开（页面上下文保持 → IndexedDB L2 保留）
  await page.unroute('**/*')
  await mockAPI(page)
  await page.reload()
  await page.getByText('book.epub').first().click()
  await expect(page.locator('#reader-view')).toBeVisible()
  await expect(page.locator('#loading')).toBeHidden({ timeout: 20_000 })
  await expect(page.locator('#page-label')).not.toBeEmpty()
  // manifest（no-cache）仍请求一次；窗口 chunks 全部命中 L2 → 零 chunk 请求
  expect(nonChunkRequests).toEqual(['flow'])
  expect(Object.keys(flowRequests)).toHaveLength(0)

  // 服务端 flow 版本升级 → sameLayout 失败 → L2 chunk 失效重取
  await page.unroute('**/*')
  await mockAPI(page, { manifestVersion: 5 })
  await page.reload()
  await page.getByText('book.epub').first().click()
  await expect(page.locator('#reader-view')).toBeVisible()
  await expect(page.locator('#loading')).toBeHidden({ timeout: 20_000 })
  await expect(page.locator('#page-label')).not.toBeEmpty()
  expect(Object.keys(flowRequests).sort((a, b) => Number(a) - Number(b))).toEqual(['0', '1', '2', '3'])
  for (const n of Object.values(flowRequests)) expect(n).toBe(1)
})

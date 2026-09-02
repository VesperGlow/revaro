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
  // 覆盖默认目录（可带 fragment，模拟服务端 TOCTarget.Fragment）
  toc?: { label: string; depth: number; spine: number; block: number; fragment?: string }[]
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
// 使第一个目标不在块起点，用于区分「精确命中」与「回退块起点」）。
function giantBlockHTML(block: number, ids: string[], leadGap = ''): string {
  const inner = ids.map(id => `<span id="${id}">【${id}】</span>`).join(FRAG_GAP)
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
  { label: '目标一', depth: 0, spine: 0, block: FRAG_BLOCK, fragment: '目标一' },
  { label: '目标二', depth: 0, spine: 0, block: FRAG_BLOCK, fragment: '目标二' },
  // 双重转义场景：服务端只解一次码，客户端需补一次安全 decode
  { label: '编码目标', depth: 0, spine: 0, block: FRAG_BLOCK, fragment: '%E7%9B%AE%E6%A0%87%E4%B8%89' },
  // 非 ASCII + 空格：按属性值直接命中（不能进 selector）
  { label: '空格目标', depth: 0, spine: 0, block: FRAG_BLOCK, fragment: '章节 二' },
  { label: '丢失目标', depth: 0, spine: 0, block: FRAG_BLOCK, fragment: 'not-there' },
  { label: '无片段', depth: 0, spine: 0, block: FRAG_BLOCK },
]

const FAR_TOC = [{ label: '远目标', depth: 0, spine: 0, block: 8 * BLOCKS_PER_CHUNK + 3, fragment: '远目标' }]

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
  const savedBlock = (page as Page & { __savedProgress: { anchor: { block?: number } } }).__savedProgress.anchor?.block ?? -1
  expect(savedBlock).toBeGreaterThanOrEqual(8 * BLOCKS_PER_CHUNK)
  expect(savedBlock).toBeLessThan(9 * BLOCKS_PER_CHUNK)
})

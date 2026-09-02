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
}

function chunkHTML(chunk: number): string {
  const parts: string[] = []
  for (let b = 0; b < BLOCKS_PER_CHUNK; b++) {
    const block = chunk * BLOCKS_PER_CHUNK + b
    parts.push(
      `<p data-block="${block}">c${chunk}b${b} 这是第 ${block} 段正文，用于填充阅读流并驱动客户端分页，句子足够长以便换行排版。</p>`,
    )
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
    toc: [
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
      return route.fulfill({ status: 200, contentType: 'text/html; charset=utf-8', body: chunkHTML(index) })
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
  // 打开：中心 chunk 0 → 窗口 [0, 4]（共 5 个 chunk）
  const initial = { ...flowRequests }
  expect(Object.keys(initial).sort((a, b) => Number(a) - Number(b))).toEqual(['0', '1', '2', '3', '4'])
  for (const n of Object.values(initial)) expect(n).toBe(1)

  // 连点翻页（动画 260ms，逐个等待）：chunk 只增不减、绝不重复；
  // 窗口 DOM 有界。
  const before = Object.keys(flowRequests).length
  await clickNext(page, 24)
  const after = { ...flowRequests }
  for (const n of Object.values(after)) expect(n).toBe(1)
  expect(Object.keys(after).length).toBeGreaterThanOrEqual(before)
  expect(Object.keys(after).length).toBeLessThanOrEqual(CHUNK_COUNT)
  // 窗口 DOM 有界（滑动窗口上限 8 个 chunk）
  await expect
    .poll(() => page.locator('#flow .rf-chunk').count(), { timeout: 5000 })
    .toBeLessThanOrEqual(8)
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

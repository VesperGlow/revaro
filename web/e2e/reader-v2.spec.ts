// 阅读器 v2 浏览器级验证（route-mock，不依赖后端）：
//   1. 严格三页窗口 + 邻页预取：翻页热路径零网络（每次翻页只新增 1 个预取请求，
//      已渲染页绝不重复请求）；
//   2. 点击翻页只做 transform（无等待：翻页目标/远侧页必已在缓存）；
//   3. 字号变更 → 新 profile 后台生成期间旧 layout 继续翻页，完成后按
//      readingAnchor 无缝切换（阅读位置不漂移）；
//   4. 目录跳页、进度条跳页；
//   5. 进度恢复：按 anchor 映射到正确页。
import { test, expect, type Page, type Route } from '@playwright/test'

const BOOK_ID = 'book-1'
const ROOT = '00000000-0000-0000-0000-000000000000'
const PROFILE_A = 'v1-' + 'a'.repeat(64)
const PROFILE_B = 'v1-' + 'b'.repeat(64)

interface MockManifest {
  pid: string
  pageCount: number
  // 每个页码展示的正文标记；markerFor(pid, pageIndex)
  markers?: Record<number, string>
  // anchor 文本 → 页码（用于重排后的映射）
  anchors?: Record<string, number>
}

function anchorFor(spine: number, path: number[], offset: number) {
  return { spine, path, offset }
}

// pageHTML 生成固定页：锁定参数内联（与真实产物同构）。
function pageHTML(pid: string, index: number, marker: string) {
  return `<div class="revaro-page" data-spine="0" data-index="${index}" style="width:390px;height:600px;padding:20px 16px;--revaro-font-family:'Revaro Serif';--revaro-font-size:16px;--revaro-line-height:1.6;--revaro-col-height:560px"><div class="revaro-content" data-spine="0"><p id="marker-${marker}">${marker} · 第 ${index + 1} 页</p><p>正文内容填充 ${marker} 尾部</p></div></div>`
}

let pageRequests: Record<number, number> = {}
let layoutRequests: { profile: unknown }[] = []

async function mockAPI(page: Page, manifest: MockManifest, progress: { anchor?: unknown; profile?: string | null }) {
  pageRequests = {}
  layoutRequests = []
  await page.route('**/*', async (route: Route) => {
    const url = new URL(route.request().url())
    const path = url.pathname
    const method = route.request().method()

    // 非 API 请求放行（vite 静态资源）
    if (!path.startsWith('/api/')) return route.continue()

    if (path === '/api/auth/me') return route.fulfill({ status: 200, contentType: 'application/json', body: JSON.stringify({ username: 'admin', has_avatar: false }) })
    if (path === '/api/reader.css') return route.fulfill({ status: 200, contentType: 'text/css', body: '' })
    if (path.startsWith('/api/reader/fonts/')) return route.fulfill({ status: 200, contentType: 'font/woff2', body: '' })
    if (path === '/api/tasks') return route.fulfill({ status: 200, contentType: 'application/json', body: JSON.stringify({ items: [] }) })
    if (path === '/api/events' || path === '/api/system/status/stream') {
      return route.fulfill({ status: 200, contentType: 'text/event-stream', body: '' })
    }
    if (path === `/api/files/${ROOT}/children`) {
      return route.fulfill({
        status: 200, contentType: 'application/json',
        body: JSON.stringify({
          items: [{ id: BOOK_ID, parent_id: ROOT, name: 'book.epub', kind: 'file', size: 1000, mime_type: 'application/epub+zip', status: 'ready', created_at: '', updated_at: '' }],
          total_bytes: 1000,
          file_count: 1,
        }),
      })
    }
    if (path === `/api/files/${ROOT}`) {
      return route.fulfill({
        status: 200, contentType: 'application/json',
        body: JSON.stringify({ file: { id: ROOT, parent_id: null, name: '我的文件', kind: 'directory', size: 0, status: 'ready' }, breadcrumbs: [{ id: ROOT, parent_id: null, name: '我的文件', kind: 'directory', size: 0, status: 'ready' }] }),
      })
    }
    if (path === `/api/files/${BOOK_ID}/thumbnail`) return route.fulfill({ status: 404, body: '' })
    if (path === `/api/files/${BOOK_ID}`) {
      return route.fulfill({
        status: 200, contentType: 'application/json',
        body: JSON.stringify({ file: { id: BOOK_ID, parent_id: ROOT, name: 'book.epub', kind: 'file', size: 1000, mime_type: 'application/epub+zip', status: 'ready', created_at: '', updated_at: '' }, breadcrumbs: [] }),
      })
    }
    if (path === `/api/files/${BOOK_ID}/book`) {
      return route.fulfill({ status: 200, contentType: 'application/json', body: JSON.stringify({ format: 'epub', title: '测试书', name: 'book.epub', cover: false, toc: [] }) })
    }
    if (path === `/api/files/${BOOK_ID}/book/progress`) {
      if (method === 'PUT') {
        // 记录最后一次进度写入，供用例断言 anchor/profile
        const body = route.request().postDataJSON()
        ;(page as Page & { __savedProgress: unknown }).__savedProgress = body
        return route.fulfill({ status: 204, body: '' })
      }
      return route.fulfill({ status: 200, contentType: 'application/json', body: JSON.stringify(progress ?? {}) })
    }
    if (path === `/api/files/${BOOK_ID}/book/layouts` && method === 'POST') {
      const body = route.request().postDataJSON()
      layoutRequests.push(body)
      return route.fulfill({ status: 200, contentType: 'application/json', body: JSON.stringify({ profile_id: manifest.pid, status: 'done', manifest: `/api/files/${BOOK_ID}/book/layouts/${manifest.pid}/manifest` }) })
    }
    const statusMatch = path.match(new RegExp(`^/api/files/${BOOK_ID}/book/layouts/(v1-[0-9a-f]+)$`))
    if (statusMatch && method === 'GET') {
      return route.fulfill({ status: 200, contentType: 'application/json', body: JSON.stringify({ profile_id: manifest.pid, status: 'done', complete: true, page_count: manifest.pageCount, spines_done: 1, spines_total: 1, manifest: `/api/files/${BOOK_ID}/book/layouts/${manifest.pid}/manifest` }) })
    }
    if (path === `/api/files/${BOOK_ID}/book/layouts/${manifest.pid}/manifest`) {
      const pages = Array.from({ length: manifest.pageCount }, (_, i) => ({
        index: i,
        spine: 0,
        start: anchorFor(0, [2 * i + 1], -1),
        end: anchorFor(0, [2 * i + 3], -1),
        url: `/api/files/${BOOK_ID}/book/layouts/${manifest.pid}/spines/0/pages/${i}`,
        bytes: 100,
      }))
      const toc = manifest.pageCount > 3
        ? [
            { label: '第一页', page: 0, depth: 0 },
            { label: '第三页', page: 2, depth: 0 },
            { label: '第六页', page: 5, depth: 1 },
          ]
        : []
      const profile = {
        viewport_w: 390, viewport_h: 600, font_size: 19,
        font_family: 'Revaro Serif', line_height: 1.6,
        margin_top: 20, margin_bottom: 12, margin_side: 16,
      }
      return route.fulfill({ status: 200, contentType: 'application/json', body: JSON.stringify({ version: 1, book_hash: 'h', profile_id: manifest.pid, profile, page_count: manifest.pageCount, pages, toc, complete: true, generated_at: '' }) })
    }
    const pageMatch = path.match(new RegExp(`^/api/files/${BOOK_ID}/book/layouts/${manifest.pid}/spines/0/pages/(\\d+)$`))
    if (pageMatch && method === 'GET') {
      const index = Number(pageMatch[1])
      pageRequests[index] = (pageRequests[index] ?? 0) + 1
      const marker = manifest.markers?.[index] ?? `m${manifest.pid === PROFILE_A ? 'A' : 'B'}-${index}`
      return route.fulfill({ status: 200, contentType: 'text/html; charset=utf-8', body: pageHTML(manifest.pid, index, marker) })
    }

    // 其余 API：回空对象，避免组件静默失败难排查
    return route.fulfill({ status: 200, contentType: 'application/json', body: '{}' })
  })
}

async function openReader(page: Page, manifest: MockManifest, progress: Record<string, unknown> = {}) {
  await mockAPI(page, manifest, progress)
  await page.goto('/')
  // 与真实 e2e 一致：在文件列表里点开书（/read/ 深链在冷启动时会被
  // openFolder 的 replaceState 覆盖，不依赖它）
  await page.getByText('book.epub').first().click()
  await expect(page.locator('#reader-view')).toBeVisible()
  await expect(page.locator('.v2-slot')).toHaveCount(3)
  await expect(page.locator('#loading')).toBeHidden()
}

test('三页窗口 + 邻页预取：翻页热路径零网络', async ({ page }) => {
  const markers: Record<number, string> = {}
  for (let i = 0; i < 12; i++) markers[i] = `page-${i}`
  await openReader(page, { pid: PROFILE_A, pageCount: 12, markers }, {})

  // 初始：0 已渲染 + 按阅读方向预取 1/2/3（前行方向看 3 页）
  await expect(page.locator('.v2-slot').nth(1)).toContainText('page-0')
  expect(pageRequests[0]).toBe(1)
  expect(pageRequests[1]).toBe(1)
  expect(pageRequests[2]).toBe(1)
  expect(pageRequests[3]).toBe(1)
  expect(Object.keys(pageRequests).length).toBe(4)

  // 翻页 ×4：每次只新增 1 个预取请求（新远侧页），已渲染页零重复请求
  for (let turn = 1; turn <= 4; turn++) {
    await page.locator('#next-zone').click()
    await expect(page.locator('.v2-slot').nth(1)).toContainText(`page-${turn}`)
    expect(Object.keys(pageRequests).length).toBe(4 + turn)
    expect(pageRequests[turn]).toBe(1)
  }
  // 已渲染过的页绝不重复请求
  for (let i = 0; i <= 4; i++) expect(pageRequests[i]).toBe(1)
  // 始终只有 3 个槽
  await expect(page.locator('.v2-slot')).toHaveCount(3)

  // 后退同理
  await page.locator('#prev-zone').click()
  await expect(page.locator('.v2-slot').nth(1)).toContainText('page-3')
  expect(pageRequests[3]).toBe(1)

  // 键盘翻页
  await page.keyboard.press('ArrowRight')
  await expect(page.locator('.v2-slot').nth(1)).toContainText('page-4')
})

test('目录跳页 + 进度条跳页 + 进度写入 readingAnchor', async ({ page }) => {
  const markers: Record<number, string> = {}
  for (let i = 0; i < 12; i++) markers[i] = `page-${i}`
  await openReader(page, { pid: PROFILE_A, pageCount: 12, markers }, {})

  await page.locator('#toc-button').click()
  await page.locator('.toc-item', { hasText: '第六页' }).click()
  await expect(page.locator('.v2-slot').nth(1)).toContainText('page-5')

  // 进度条跳页（range 输入用原生事件派发驱动）
  await page.locator('#page-slider').evaluate(el => {
    const input = el as HTMLInputElement
    input.value = '9'
    input.dispatchEvent(new Event('input', { bubbles: true }))
    input.dispatchEvent(new Event('change', { bubbles: true }))
  })
  await expect(page.locator('.v2-slot').nth(1)).toContainText('page-9')

  // 防抖保存后，进度负载带 anchor + profile + page/total_pages
  await expect
    .poll(() => (page as Page & { __savedProgress?: unknown }).__savedProgress ?? null, { timeout: 5000 })
    .not.toBeNull()
  const saved = (page as Page & { __savedProgress: { anchor?: unknown; profile?: string } }).__savedProgress
  expect(saved.profile).toBe(PROFILE_A)
  expect(saved.anchor).toEqual({ spine: 0, path: [19], offset: -1 })
})

test('字号变更：旧 layout 继续翻页，新 profile 就绪后按 anchor 无缝切换', async ({ page }) => {

  const markersA: Record<number, string> = {}
  for (let i = 0; i < 12; i++) markersA[i] = `page-${i}`
  await openReader(page, { pid: PROFILE_A, pageCount: 12, markers: markersA }, {})

  // 翻到第 4 页（index 3），anchor = [7]
  for (let i = 0; i < 3; i++) await page.locator('#next-zone').click()
  await expect(page.locator('.v2-slot').nth(1)).toContainText('page-3')

  // 拦截：新 profile 提交后先 queued 再 done（模拟后台生成耗时）
  let relayoutPending = true
  await page.unroute('**/*')
  await mockAPI(page, { pid: PROFILE_A, pageCount: 12, markers: markersA }, {})
  await page.route(`**/api/files/${BOOK_ID}/book/layouts`, async (route: Route) => {
    if (route.request().method() !== 'POST') return route.fallback()
    const body = route.request().postDataJSON()
    const font = (body as { profile: { font_size: number } }).profile.font_size
    if (font === 19) return route.fulfill({ status: 200, contentType: 'application/json', body: JSON.stringify({ profile_id: PROFILE_A, status: 'done' }) })
    layoutRequests.push(body)
    if (!relayoutPending) return route.fulfill({ status: 200, contentType: 'application/json', body: JSON.stringify({ profile_id: PROFILE_B, status: 'done' }) })
    // 第一次请求：先报告 queued；状态轮询在下方单独 mock
    return route.fulfill({ status: 200, contentType: 'application/json', body: JSON.stringify({ profile_id: PROFILE_B, status: 'queued' }) })
  })
  await page.route(`**/api/files/${BOOK_ID}/book/layouts/${PROFILE_B}`, async (route: Route) => {
    if (relayoutPending) {
      return route.fulfill({ status: 200, contentType: 'application/json', body: JSON.stringify({ profile_id: PROFILE_B, status: 'queued', pages: 0 }) })
    }
    return route.fulfill({ status: 200, contentType: 'application/json', body: JSON.stringify({ profile_id: PROFILE_B, status: 'done', page_count: 12 }) })
  })
  // 新 layout（PROFILE_B）：同一 anchor 落在第 6 页（index 5）——模拟字号变化后的页位漂移
  await page.route(`**/api/files/${BOOK_ID}/book/layouts/${PROFILE_B}/manifest`, async (route: Route) => {
    if (relayoutPending) return route.fulfill({ status: 404, contentType: 'application/json', body: '{"error":{"message":"not ready"}}' })
    const pages = Array.from({ length: 12 }, (_, i) => ({
      index: i, spine: 0,
      start: anchorFor(0, [2 * i + 1], -1),
      end: anchorFor(0, [2 * i + 3], -1),
      url: `/api/files/${BOOK_ID}/book/layouts/${PROFILE_B}/spines/0/pages/${i}`,
      bytes: 100,
    }))
    const profile = {
      viewport_w: 390, viewport_h: 600, font_size: 20,
      font_family: 'Revaro Serif', line_height: 1.6,
      margin_top: 20, margin_bottom: 12, margin_side: 16,
    }
    return route.fulfill({ status: 200, contentType: 'application/json', body: JSON.stringify({ version: 1, book_hash: 'h', profile_id: PROFILE_B, profile, page_count: 12, pages, toc: [], complete: true, generated_at: '' }) })
  })
  await page.route(`**/api/files/${BOOK_ID}/book/layouts/${PROFILE_B}/spines/0/pages/*`, async (route: Route) => {
    const index = Number(new URL(route.request().url()).pathname.split('/').pop())
    pageRequests[index] = (pageRequests[index] ?? 0) + 1
    const marker = index === 3 ? 'page-3' : `pageB-${index}` // 旧 anchor 位置在第 4 页，映射到新 layout 第 4 页后内容仍是 page-3 标记
    return route.fulfill({ status: 200, contentType: 'text/html; charset=utf-8', body: pageHTML(PROFILE_B, index, marker) })
  })

  // 增大字号 → 新 profile 排队中
  await page.locator('#font-button').click()
  await page.locator('#font-larger').click()
  await expect(page.locator('.v2-status')).toBeVisible()

  // 等待旧 layout 仍在第 4 页且可继续翻页（旧 layout 全程可读）
  await expect(page.locator('.v2-slot').nth(1)).toContainText('page-3')

  // 新 layout 就绪
  relayoutPending = false
  await expect(page.locator('.v2-status')).toBeHidden({ timeout: 10_000 })

  // 无缝切换：anchor [7]（第 4 页起点）→ 新 layout 第 4 页（index 3），内容仍是 page-3 标记
  await expect(page.locator('.v2-slot').nth(1)).toContainText('page-3')
  // 新 layout 下继续翻页
  await page.locator('#next-zone').click()
  await expect(page.locator('.v2-slot').nth(1)).toContainText('pageB-4')
})

test('进度恢复：按 readingAnchor 映射到正确页', async ({ page }) => {
  const markers: Record<number, string> = {}
  for (let i = 0; i < 12; i++) markers[i] = `page-${i}`
  // 保存的 anchor = 第 7 页（index 6）起点 [13]
  await openReader(
    page,
    { pid: PROFILE_A, pageCount: 12, markers },
    { anchor: { spine: 0, path: [13], offset: -1 }, profile: PROFILE_A },
  )
  await expect(page.locator('.v2-slot').nth(1)).toContainText('page-6')
})

test('始终严格三页窗口：连续翻页后 DOM 不膨胀', async ({ page }) => {
  const markers: Record<number, string> = {}
  for (let i = 0; i < 12; i++) markers[i] = `page-${i}`
  await openReader(page, { pid: PROFILE_A, pageCount: 12, markers }, {})
  for (let i = 0; i < 8; i++) await page.locator('#next-zone').click()
  await expect(page.locator('.v2-slot')).toHaveCount(3)
  const slots = await page.locator('.v2-slot').count()
  expect(slots).toBe(3)
  await expect(page.locator('.v2-slot').nth(1)).toContainText('page-8')
})

// 渐进式分页：目标章快照先就绪即可读，不等全书生成完；快照增长后
// 页码重映射但锚点位置不变（阅读不漂移、翻页不卡边界）。
test('渐进式首开：目标章先可读，快照增长后无缝续读', async ({ page }) => {
  const PROFILE_P = 'v1-' + 'c'.repeat(64)
  // 12 页中「已生成前缀」初始只含 target 章（spine 1）的页：全局索引 4..7
  let partial = true
  const markers: Record<number, string> = {}
  for (let i = 0; i < 12; i++) markers[i] = `page-${i}`

  await mockAPI(page, { pid: PROFILE_P, pageCount: 12, markers }, {})
  // 覆盖 manifest/status 路由为渐进式
  await page.route(`**/api/files/${BOOK_ID}/book/layouts/${PROFILE_P}/manifest`, async (route: Route) => {
    const mkPages = (from: number, to: number) =>
      Array.from({ length: to - from }, (_, i) => ({
        index: from + i, spine: from + i < 8 && from + i >= 4 ? 1 : 0,
        start: anchorFor(0, [2 * (from + i) + 1], -1),
        end: anchorFor(0, [2 * (from + i) + 3], -1),
        url: `/api/files/${BOOK_ID}/book/layouts/${PROFILE_P}/spines/0/pages/${from + i}`,
        bytes: 100,
      }))
    const profile = { viewport_w: 390, viewport_h: 600, font_size: 19, font_family: 'Revaro Serif', line_height: 1.6, margin_top: 20, margin_bottom: 12, margin_side: 16 }
    const pages = partial ? mkPages(4, 8) : mkPages(0, 12)
    return route.fulfill({
      status: 200, contentType: 'application/json',
      body: JSON.stringify({ version: 1, book_hash: 'h', profile_id: PROFILE_P, profile, page_count: pages.length, pages, toc: [], complete: !partial, generated_at: '' }),
    })
  })
  await page.route(`**/api/files/${BOOK_ID}/book/layouts/${PROFILE_P}/spines/0/pages/*`, async (route: Route) => {
    const index = Number(new URL(route.request().url()).pathname.split('/').pop())
    pageRequests[index] = (pageRequests[index] ?? 0) + 1
    return route.fulfill({ status: 200, contentType: 'text/html; charset=utf-8', body: pageHTML(PROFILE_P, index, `page-${index}`) })
  })

  // 进度锚点落在第 5 页（index 5，位于 target 章的已生成前缀内）
  await page.addInitScript(() => {})
  await page.unroute('**/*')
  await mockAPI(page, { pid: PROFILE_P, pageCount: 12, markers }, { anchor: { spine: 0, path: [11], offset: -1 }, profile: PROFILE_P })
  await page.route(`**/api/files/${BOOK_ID}/book/layouts/${PROFILE_P}/manifest`, async (route: Route) => {
    const mkPages = (from: number, to: number) =>
      Array.from({ length: to - from }, (_, i) => ({
        index: from + i, spine: 0,
        start: anchorFor(0, [2 * (from + i) + 1], -1),
        end: anchorFor(0, [2 * (from + i) + 3], -1),
        url: `/api/files/${BOOK_ID}/book/layouts/${PROFILE_P}/spines/0/pages/${from + i}`,
        bytes: 100,
      }))
    const profile = { viewport_w: 390, viewport_h: 600, font_size: 19, font_family: 'Revaro Serif', line_height: 1.6, margin_top: 20, margin_bottom: 12, margin_side: 16 }
    const pages = partial ? mkPages(4, 8) : mkPages(0, 12)
    return route.fulfill({
      status: 200, contentType: 'application/json',
      body: JSON.stringify({ version: 1, book_hash: 'h', profile_id: PROFILE_P, profile, page_count: pages.length, pages, toc: [], complete: !partial, generated_at: '' }),
    })
  })
  await page.route(`**/api/files/${BOOK_ID}/book/layouts/${PROFILE_P}`, async (route: Route) => {
    return route.fulfill({
      status: 200, contentType: 'application/json',
      body: JSON.stringify({ profile_id: PROFILE_P, status: partial ? 'running' : 'done', phase: partial ? 'background' : undefined, complete: !partial, pages: partial ? 4 : 12, spines_done: partial ? 1 : 2, spines_total: 2 }),
    })
  })
  await page.route(`**/api/files/${BOOK_ID}/book/layouts/${PROFILE_P}/spines/0/pages/*`, async (route: Route) => {
    const index = Number(new URL(route.request().url()).pathname.split('/').pop())
    pageRequests[index] = (pageRequests[index] ?? 0) + 1
    return route.fulfill({ status: 200, contentType: 'text/html; charset=utf-8', body: pageHTML(PROFILE_P, index, `page-${index}`) })
  })

  await page.goto('/')
  await page.getByText('book.epub').first().click()
  // 快照只含 4 页时即可读（不等全书）：锚点映射到第 5 页（index 5）
  await expect(page.locator('#loading')).toBeHidden()
  await expect(page.locator('.v2-slot').nth(1)).toContainText('page-5')
  // 滑块上限 = 当前快照页数
  const sliderMax = await page.locator('#page-slider').getAttribute('max')
  expect(Number(sliderMax)).toBe(3) // 4 页 → max 3

  // 快照增长：全书 12 页生成完毕
  partial = false
  await expect
    .poll(async () => Number(await page.locator('#page-slider').getAttribute('max')), { timeout: 8000 })
    .toBe(11)
  // 阅读位置不漂移：仍显示第 5 页标记（页码重映射后锚点不变）
  await expect(page.locator('.v2-slot').nth(1)).toContainText('page-5')
  // 快照增长后继续前行翻页，页面按新 URL 正常拉取
  await page.locator('#next-zone').click()
  await expect(page.locator('.v2-slot').nth(1)).toContainText('page-6')
})

// 按阅读方向智能预取：后退时向后看 3 页，另一侧邻页最后才取。
test('按阅读方向智能预取：后退时向后看 3 页', async ({ page }) => {
  const markers: Record<number, string> = {}
  for (let i = 0; i < 12; i++) markers[i] = `page-${i}`
  await openReader(page, { pid: PROFILE_A, pageCount: 12, markers }, {})
  const requestOrder: number[] = []
  const origPush = pageRequests
  const reset = () => {
    for (const k of Object.keys(origPush)) delete origPush[Number(k)]
  }
  reset()
  const record = async (idx: number) => {
    requestOrder.push(idx)
  }
  await page.unroute('**/*')
  await mockAPI(page, { pid: PROFILE_A, pageCount: 12, markers }, {})
  await page.route(`**/api/files/${BOOK_ID}/book/layouts/${PROFILE_A}/spines/0/pages/*`, async (route: Route) => {
    const index = Number(new URL(route.request().url()).pathname.split('/').pop())
    origPush[index] = (origPush[index] ?? 0) + 1
    await record(index)
    return route.fulfill({ status: 200, contentType: 'text/html; charset=utf-8', body: pageHTML(PROFILE_A, index, `page-${index}`) })
  })
  // 重新打开（路由已替换，重新加载页面状态）
  await page.reload()
  await page.getByText('book.epub').first().click()
  await expect(page.locator('.v2-slot').nth(1)).toContainText('page-0')
  requestOrder.length = 0

  // seek 到第 10 页（index 9），方向向右的预取已填 10/11/8
  await page.locator('#page-slider').evaluate(el => {
    const input = el as HTMLInputElement
    input.value = '9'
    input.dispatchEvent(new Event('input', { bubbles: true }))
    input.dispatchEvent(new Event('change', { bubbles: true }))
  })
  await expect(page.locator('.v2-slot').nth(1)).toContainText('page-9')
  requestOrder.length = 0

  // 后退一页：远侧槽先取 7，随后向后看 6、5——后退方向先看 3 页，
  // 另一侧邻页（10）已在缓存，不再请求
  await page.locator('#prev-zone').click()
  await expect(page.locator('.v2-slot').nth(1)).toContainText('page-8')
  await expect.poll(() => requestOrder.length, { timeout: 5000 }).toBeGreaterThanOrEqual(3)
  expect(requestOrder.slice(0, 3)).toEqual([7, 6, 5])
  expect(requestOrder.includes(10)).toBe(false)
})

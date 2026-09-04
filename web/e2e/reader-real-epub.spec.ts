// 真实 EPUB 连续 reading-flow 回归：fixture 目录只提供内容，不把书名、目录
// 名称或页码写进用例。用例从书首连续翻页，直到 windowSync 第一次改变 DOM
// 窗口/排版几何，验证同步前后的阅读位置没有倒退。
import { expect, test, type Page } from '@playwright/test'
import { join } from 'node:path'
import { existsSync, readdirSync, statSync } from 'node:fs'

const ROOT = '00000000-0000-0000-0000-000000000000'
const VIEWPORT = { width: 1280, height: 800 }
const PREFS = { fontSize: 19, lineHeight: 1.7, theme: 'light' }
const fixtureDirectory = process.env.REVARO_EPUB_FIXTURE_DIR || (existsSync('/config/donwload') ? '/config/donwload' : '/config/Downloads')

interface FixtureFile {
  path: string
  name: string
  size: number
}

interface DriveFile {
  id: string
  name: string
  size: number
  kind: string
  status: string
}

interface ReaderSnapshot {
  label: string
  currentCol: number
  cols: number
  scrollWidth: number
  transform: string
  window: number[]
  topAnchor: {
    block: number | null
    path: number[] | null
    offset: number | null
    rangeColumn: number | null
    rect: RectSnapshot | null
  }
  visibleBlocks: Array<{ block: number; rects: RectSnapshot[] }>
  images: Array<{ block: number; rect: RectSnapshot }>
}

interface RectSnapshot {
  left: number
  right: number
  top: number
  bottom: number
  width: number
  height: number
  column: number
}

function fixtureFiles(): FixtureFile[] {
  try {
    return readdirSync(fixtureDirectory)
      .filter(name => /\.epub$/i.test(name))
      .map(name => {
        const path = join(fixtureDirectory, name)
        return { path, name, size: statSync(path).size }
      })
      .sort((a, b) => a.name.localeCompare(b.name))
  } catch {
    return []
  }
}

async function login(page: Page) {
  await page.goto('/', { waitUntil: 'domcontentloaded' })
  await page.getByLabel('用户名').fill('admin')
  await page.getByLabel('密码').fill('revaro-e2e-password')
  await page.getByRole('button', { name: '进入我的网盘' }).click()
  await page.getByRole('heading', { name: '我的文件' }).waitFor()
}

async function children(page: Page): Promise<DriveFile[]> {
  return page.evaluate(async root => {
    const response = await fetch(`/api/files/${root}/children`)
    if (!response.ok) throw new Error(`读取 fixture 文件列表失败: ${response.status}`)
    const payload = (await response.json()) as { items?: DriveFile[] }
    return payload.items ?? []
  }, ROOT)
}

async function prepareFixtures(page: Page, files: FixtureFile[]): Promise<DriveFile[]> {
  const ready = (await children(page)).filter(item => item.kind === 'file' && item.status === 'ready')
  const missing = files.filter(file => !ready.some(item => item.name === file.name && item.size === file.size))
  if (missing.length) await page.locator('input[type=file]').first().setInputFiles(missing.map(file => file.path))
  await expect
    .poll(async () => {
      const current = await children(page)
      return files.every(file => current.some(item => item.kind === 'file' && item.status === 'ready' && item.name === file.name && item.size === file.size))
    }, { timeout: 180_000 })
    .toBe(true)
  const current = await children(page)
  return files.map(file => {
    const item = current.find(candidate => candidate.kind === 'file' && candidate.status === 'ready' && candidate.name === file.name && candidate.size === file.size)
    if (!item) throw new Error(`fixture 未进入 ready: ${file.name}`)
    return item
  })
}

async function resetProgress(page: Page, fileID: string) {
  const result = await page.evaluate(async id => {
    const response = await fetch(`/api/files/${id}/book/progress`, {
      method: 'PUT',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ anchor: null }),
    })
    return { ok: response.ok, status: response.status }
  }, fileID)
  if (!result.ok) throw new Error(`重置阅读进度失败: ${result.status}`)
}

async function openBook(page: Page, book: DriveFile) {
  await resetProgress(page, book.id)
  await page.locator('.file-card').filter({ hasText: book.name }).first().click()
  await page.locator('#reader-view').waitFor()
  await page.locator('#loading').waitFor({ state: 'hidden', timeout: 180_000 })
  await page.locator('#flow .rf-chunk').first().waitFor({ timeout: 30_000 })
}

async function snapshot(page: Page, label: string): Promise<ReaderSnapshot> {
  return page.evaluate(label => {
    const rect = value => ({
      left: value.left,
      right: value.right,
      top: value.top,
      bottom: value.bottom,
      width: value.width,
      height: value.height,
    })
    const flow = document.getElementById('flow')
    const viewport = document.getElementById('viewport')
    if (!flow || !viewport) throw new Error('阅读流 DOM 不存在')
    const flowRect = flow.getBoundingClientRect()
    const viewportRect = viewport.getBoundingClientRect()
    const style = getComputedStyle(flow)
    const side = Number.parseFloat(style.paddingLeft) || 0
    const pitch = flow.clientWidth || 1
    const column = value => Math.floor((value.left - flowRect.left - side + 1) / pitch)
    const withColumn = value => ({ ...rect(value), column: column(value) })
    let matrix = null
    try { matrix = new DOMMatrixReadOnly(style.transform) } catch { /* no transform */ }
    const blocks = Array.from(flow.querySelectorAll('[data-block]')).map(element => {
      const rects = Array.from(element.getClientRects()).map(withColumn)
      return { block: Number(element.getAttribute('data-block')), rects }
    })
    const visibleBlocks = blocks.filter(item => item.rects.some(value => value.right > viewportRect.left && value.left < viewportRect.right && value.bottom > viewportRect.top && value.top < viewportRect.bottom))

    const originX = viewportRect.left + side + 2
    const originY = viewportRect.top + (Number.parseFloat(style.paddingTop) || 0) + 2
    let topAnchor = { block: null, path: null, offset: null, rangeColumn: null, rect: null }
    try {
      const range = document.caretRangeFromPoint?.(originX, originY)
      if (range) {
        const rangeRect = range.getClientRects()[0] || range.getBoundingClientRect()
        const path = []
        let node = range.startContainer
        let blockElement = null
        while (node) {
          if (node.nodeType === Node.ELEMENT_NODE && node.hasAttribute('data-block')) { blockElement = node; break }
          const parent = node.parentNode
          if (!parent) break
          path.unshift(Array.prototype.indexOf.call(parent.childNodes, node))
          node = parent
        }
        topAnchor = {
          block: blockElement ? Number(blockElement.getAttribute('data-block')) : null,
          path,
          offset: range.startOffset,
          rangeColumn: rangeRect ? column(rangeRect) : null,
          rect: rangeRect ? withColumn(rangeRect) : null,
        }
      }
    } catch { /* content may begin below the top margin */ }

    const images = Array.from(flow.querySelectorAll('img')).map(element => {
      const value = element.getBoundingClientRect()
      return { block: Number(element.closest('[data-block]')?.getAttribute('data-block') ?? -1), rect: withColumn(value) }
    }).filter(item => item.rect.right > viewportRect.left && item.rect.left < viewportRect.right && item.rect.bottom > viewportRect.top && item.rect.top < viewportRect.bottom)
    return {
      label,
      currentCol: matrix ? Math.round(-matrix.m41 / pitch) : 0,
      cols: Math.max(1, Math.round(Math.max(pitch, flow.scrollWidth) / pitch)),
      scrollWidth: flow.scrollWidth,
      transform: style.transform,
      window: Array.from(flow.children, child => Number(child.getAttribute('data-chunk'))),
      topAnchor,
      visibleBlocks: visibleBlocks.slice(0, 20),
      images,
    }
  }, label)
}

test('三本真实 EPUB：windowSync 后连续翻页不回退到上一页', async ({ page }, testInfo) => {
  test.setTimeout(15 * 60_000)
  const files = fixtureFiles()
  test.skip(files.length === 0, `未找到真实 EPUB fixture: ${fixtureDirectory}`)
  expect(files).toHaveLength(3)
  await page.setViewportSize(VIEWPORT)
  await page.addInitScript(prefs => localStorage.setItem('revaro-reader-prefs', JSON.stringify(prefs)), PREFS)
  await login(page)
  const books = await prepareFixtures(page, files)
  const evidence: Array<{ book: string; turn: number; before: ReaderSnapshot; afterAnimation: ReaderSnapshot; afterSync: ReaderSnapshot }> = []

  for (const [bookIndex, book] of books.entries()) {
    await openBook(page, book)
    const manifest = await page.evaluate(async id => (await (await fetch(`/api/files/${id}/book/flow`)).json()) as { chunks: Array<unknown> }, book.id)
    let observedWindowSync = false
    // The bound is derived from the generated chunk count; the loop itself
    // stops at the first real window/layout change, not at a fixture-specific page.
    const limit = Math.max(32, manifest.chunks.length * 4)
    for (let turn = 1; turn <= limit && !observedWindowSync; turn++) {
      const before = await snapshot(page, `${bookIndex}-next-${turn}-before`)
      await page.locator('#next-zone').click()
      await page.waitForTimeout(350)
      const afterAnimation = await snapshot(page, `${bookIndex}-next-${turn}-after-animation`)
      await page.waitForTimeout(900)
      const afterSync = await snapshot(page, `${bookIndex}-next-${turn}-after-window-sync`)
      evidence.push({ book: book.name, turn, before, afterAnimation, afterSync })

      const windowChanged = afterSync.window.join(',') !== before.window.join(',')
      const layoutChanged = windowChanged || afterSync.cols !== before.cols || afterSync.scrollWidth !== before.scrollWidth
      if (layoutChanged) {
        observedWindowSync = true
        const animationTop = afterAnimation.visibleBlocks[0]?.block ?? -1
        const syncedTop = afterSync.visibleBlocks[0]?.block ?? -1
        if (syncedTop < animationTop - 3) {
          await testInfo.attach(`reader-drift-${bookIndex}`, {
            body: Buffer.from(JSON.stringify(evidence.filter(item => item.book === book.name), null, 2)),
            contentType: 'application/json',
          })
          await page.screenshot({ path: testInfo.outputPath(`reader-drift-${bookIndex}.png`) })
        }
        // This assertion intentionally fails on the current implementation:
        // animationTop is the page reached by the turn, while syncedTop is the
        // previous page after stale topAnchor re-alignment.
        expect(syncedTop, `${book.name}: windowSync 后内容倒退`).toBeGreaterThanOrEqual(animationTop - 3)
      }
    }
    expect(observedWindowSync, `${book.name}: 未观察到窗口/排版同步`).toBe(true)
    await page.locator('#reader-back').click()
    await page.getByRole('heading', { name: '我的文件' }).waitFor()
  }

  await testInfo.attach('reader-real-epub-evidence', {
    body: Buffer.from(JSON.stringify(evidence, null, 2)),
    contentType: 'application/json',
  })
})

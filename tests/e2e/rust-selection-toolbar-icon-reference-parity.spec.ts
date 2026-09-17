import { expect, test, type Page } from '@playwright/test'
import { listingEntries, useReferenceListView } from './helpers'

const ROOT = '00000000-0000-0000-0000-000000000000'
const STAMP = '2026-01-01T00:00:00Z'
const textFile = {
  id: 'compat-selection-text',
  parent_id: ROOT,
  name: '阅读笔记.txt',
  kind: 'file',
  size: 128,
  status: 'ready',
  created_at: STAMP,
  updated_at: STAMP,
  mime_type: 'text/plain',
  etag: 'compat-selection-text-etag',
}

async function mockSelection(page: Page) {
  await page.route('**/api/**', async route => {
    const path = new URL(route.request().url()).pathname
    const json = (value: unknown) => route.fulfill({ json: value })

    if (path === '/api/auth/me') return json({ username: 'admin', has_avatar: false })
    if (path === '/api/events' || path === '/api/system/status/stream') {
      return route.fulfill({ contentType: 'text/event-stream', body: '' })
    }
    if (path === '/api/tasks') return json({ items: [] })
    if (path === '/api/library/all') {
      return json({ items: { book: [], image: [], video: [], audio: [] }, counts: { book: 1, image: 0, video: 0, audio: 0, file: 1 } })
    }
    if (path === '/api/library/counts') return json({ book: 1, image: 0, video: 0, audio: 0, file: 1 })
    if (path === `/api/files/${ROOT}`) {
      return json({
        file: {
          id: ROOT,
          parent_id: null,
          name: '我的文件',
          kind: 'directory',
          size: 0,
          status: 'ready',
          created_at: STAMP,
          updated_at: STAMP,
          mime_type: '',
        },
        breadcrumbs: [],
      })
    }
    if (path === `/api/files/${ROOT}/children`) return json({ items: [textFile], total_bytes: textFile.size, file_count: 1 })
    return json({ items: [] })
  })
}

async function openTextSelection(page: Page, baseUrl: string) {
  await page.goto(`${baseUrl}/`)
  await expect(page.getByRole('heading', { name: '我的文件', exact: true })).toBeVisible()
  const useListView = await useReferenceListView(page)
  const row = listingEntries(page, useListView).filter({ hasText: textFile.name })
  await expect(row).toBeVisible()
  await row.getByRole('button', { name: '选择项目' }).click()
  const open = page.getByRole('toolbar', { name: '所选项目操作' }).getByRole('button', { name: '阅读', exact: true })
  await expect(open).toBeVisible()
  return open
}

async function iconGeometry(button: Page['locator']) {
  return button.locator('svg').evaluateAll(svgs => svgs.map(svg => Array.from(svg.querySelectorAll('path,circle,ellipse,rect,line,polyline,polygon')).map(element => ({
    tag: element.tagName.toLowerCase(),
    attributes: Object.fromEntries(Array.from(element.attributes)
      .filter(attribute => ['d', 'points', 'x', 'y', 'width', 'height', 'rx', 'ry', 'cx', 'cy', 'r', 'x1', 'x2', 'y1', 'y2', 'fill'].includes(attribute.name))
      .sort((left, right) => left.name.localeCompare(right.name))
      .map(attribute => [attribute.name, attribute.value])),
  }))))
}

test('文本既可编辑又可阅读时，选择工具栏保持 reference 的书本图标', async ({ browser }) => {
  const oldUrl = process.env.E2E_REFERENCE_URL || 'http://127.0.0.1:18080'
  const newUrl = process.env.E2E_NEW_URL || 'http://127.0.0.1:18084'
  const oldContext = await browser.newContext({ viewport: { width: 1280, height: 800 } })
  const newContext = await browser.newContext({ viewport: { width: 1280, height: 800 } })
  const oldPage = await oldContext.newPage()
  const newPage = await newContext.newPage()

  try {
    await Promise.all([mockSelection(oldPage), mockSelection(newPage)])
    const [oldOpen, newOpen] = await Promise.all([
      openTextSelection(oldPage, oldUrl),
      openTextSelection(newPage, newUrl),
    ])
    expect(await iconGeometry(newOpen), 'Rust .txt 阅读按钮图标应保持旧版书本几何').toEqual(await iconGeometry(oldOpen))
  } finally {
    await oldContext.close()
    await newContext.close()
  }
})

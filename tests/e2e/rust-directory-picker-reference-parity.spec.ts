import { expect, test, type Page } from '@playwright/test'

const ROOT = '00000000-0000-0000-0000-000000000000'
const STAMP = '2026-01-01T00:00:00Z'

const root = {
  id: ROOT,
  parent_id: null,
  name: '我的文件',
  kind: 'directory',
  size: 0,
  status: 'ready',
  created_at: STAMP,
  updated_at: STAMP,
  mime_type: '',
}
const source = {
  id: 'compat-source',
  parent_id: ROOT,
  name: '待移动.txt',
  kind: 'file',
  size: 12,
  status: 'ready',
  created_at: STAMP,
  updated_at: STAMP,
  mime_type: 'text/plain',
  etag: 'compat-source-etag',
}
const destination = {
  id: 'compat-destination',
  parent_id: ROOT,
  name: '目标文件夹',
  kind: 'directory',
  size: 0,
  status: 'ready',
  created_at: STAMP,
  updated_at: STAMP,
  mime_type: '',
}

async function mockPicker(page: Page) {
  await page.route('**/api/**', async route => {
    const path = new URL(route.request().url()).pathname
    const json = (value: unknown) => route.fulfill({ json: value })

    if (path === '/api/auth/me') return json({ username: 'admin', has_avatar: false })
    if (path === '/api/events' || path === '/api/system/status/stream') {
      return route.fulfill({ contentType: 'text/event-stream', body: '' })
    }
    if (path === '/api/tasks') return json({ items: [] })
    if (path === '/api/library/all') {
      return json({ items: { book: [], image: [], video: [], audio: [] }, counts: { book: 0, image: 0, video: 0, audio: 0, file: 1 } })
    }
    if (path === '/api/library/counts') return json({ book: 0, image: 0, video: 0, audio: 0, file: 1 })
    if (path === `/api/files/${ROOT}`) return json({ file: root, breadcrumbs: [] })
    if (path === `/api/files/${ROOT}/children`) return json({ items: [source, destination], total_bytes: source.size, file_count: 1 })
    if (path === '/api/files/compat-destination') return json({ file: destination, breadcrumbs: [root] })
    if (path === '/api/files/compat-destination/children') return json({ items: [], total_bytes: 0, file_count: 0 })
    return json({ items: [] })
  })
}

async function openPicker(page: Page, baseUrl: string) {
  await page.goto(`${baseUrl}/`)
  await expect(page.getByRole('heading', { name: '我的文件', exact: true })).toBeVisible()
  await page.getByRole('button', { name: '列表', exact: true }).click()
  const row = page.locator('.file-row').filter({ hasText: '待移动.txt' })
  await expect(row).toBeVisible()
  await row.getByRole('button', { name: '选择项目' }).click()
  await page.getByRole('toolbar', { name: '所选项目操作' }).getByRole('button', { name: '移动', exact: true }).click()
  await expect(page.locator('.directory-trigger')).toBeVisible()
}

async function iconGeometry(page: Page, selector: string) {
  return page.locator(selector).evaluateAll(svgs => svgs.map(svg => Array.from(
    svg.querySelectorAll('path,rect,circle,ellipse,line,polyline,polygon'),
  ).map(element => ({
    tag: element.tagName.toLowerCase(),
    attributes: Object.fromEntries(Array.from(element.attributes)
      .filter(attribute => ['d', 'points', 'x', 'y', 'width', 'height', 'rx', 'ry', 'cx', 'cy', 'r', 'x1', 'x2', 'y1', 'y2', 'fill'].includes(attribute.name))
      .sort((left, right) => left.name.localeCompare(right.name))
      .map(attribute => [attribute.name, attribute.value])),
  }))))
}

async function compareIcons(oldPage: Page, newPage: Page, selector: string, label: string) {
  const oldGeometry = await iconGeometry(oldPage, selector)
  const newGeometry = await iconGeometry(newPage, selector)
  expect(newGeometry, `${label} 的 Rust SVG 几何应保持 reference`).toEqual(oldGeometry)
}

async function installEscapeProbe(page: Page) {
  await page.evaluate(() => {
    ;(window as Window & { __pickerEscapeDefaultPrevented?: boolean }).__pickerEscapeDefaultPrevented = undefined
    window.addEventListener('keydown', event => {
      if (event.key === 'Escape') {
        window.setTimeout(() => {
          ;(window as Window & { __pickerEscapeDefaultPrevented?: boolean }).__pickerEscapeDefaultPrevented = event.defaultPrevented
        }, 0)
      }
    }, { capture: true, once: true })
  })
}

test('移动/复制目录选择器的路径图标和展开关闭行为保持 reference', async ({ browser }) => {
  const oldUrl = process.env.E2E_REFERENCE_URL || 'http://127.0.0.1:18080'
  const newUrl = process.env.E2E_NEW_URL || 'http://127.0.0.1:18083'
  const oldContext = await browser.newContext({ viewport: { width: 1280, height: 900 } })
  const newContext = await browser.newContext({ viewport: { width: 1280, height: 900 } })
  const oldPage = await oldContext.newPage()
  const newPage = await newContext.newPage()

  try {
    await Promise.all([mockPicker(oldPage), mockPicker(newPage)])
    await Promise.all([openPicker(oldPage, oldUrl), openPicker(newPage, newUrl)])

    await compareIcons(oldPage, newPage, '.directory-trigger > svg', '目录选择器触发器')
    await oldPage.locator('.directory-trigger').click()
    await newPage.locator('.directory-trigger').click()
    await expect(oldPage.getByRole('region', { name: '选择目标目录' })).toBeVisible()
    await expect(newPage.getByRole('region', { name: '选择目标目录' })).toBeVisible()
    await compareIcons(oldPage, newPage, '.directory-breadcrumbs svg', '目录选择器根路径')
    await compareIcons(oldPage, newPage, '.directory-list > button svg', '目录选择器子目录')

    await oldPage.getByRole('region', { name: '选择目标目录' }).getByRole('button', { name: '目标文件夹', exact: true }).click()
    await newPage.getByRole('region', { name: '选择目标目录' }).getByRole('button', { name: '目标文件夹', exact: true }).click()
    await expect(oldPage.locator('.directory-trigger')).toHaveAttribute('title', /目标文件夹/)
    await expect(newPage.locator('.directory-trigger')).toHaveAttribute('title', /目标文件夹/)
    await compareIcons(oldPage, newPage, '.directory-breadcrumbs svg', '目录选择器深层路径')
    await compareIcons(oldPage, newPage, '.directory-state svg', '目录选择器空目录')

    await Promise.all([installEscapeProbe(oldPage), installEscapeProbe(newPage)])
    await oldPage.keyboard.press('Escape')
    await newPage.keyboard.press('Escape')
    await expect(oldPage.getByRole('region', { name: '选择目标目录' })).toHaveCount(0)
    await expect(newPage.getByRole('region', { name: '选择目标目录' })).toHaveCount(0)
    await expect.poll(() => oldPage.evaluate(() => (window as Window & { __pickerEscapeDefaultPrevented?: boolean }).__pickerEscapeDefaultPrevented)).toBe(false)
    await expect.poll(() => newPage.evaluate(() => (window as Window & { __pickerEscapeDefaultPrevented?: boolean }).__pickerEscapeDefaultPrevented)).toBe(false)
  } finally {
    await oldContext.close()
    await newContext.close()
  }
})

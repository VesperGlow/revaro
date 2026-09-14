import { expect, test, type Page } from '@playwright/test'

const ROOT = '00000000-0000-0000-0000-000000000000'
const STAMP = '2026-01-01T00:00:00Z'

const base = (value: Record<string, unknown>) => ({
  parent_id: ROOT,
  kind: 'file',
  size: 4096,
  status: 'ready',
  created_at: STAMP,
  updated_at: STAMP,
  mime_type: 'application/octet-stream',
  etag: `toolbar-${String(value.id ?? 'item')}`,
  has_cover: false,
  ...value,
})

const items = [
  base({ id: 'folder', name: '资料夹', kind: 'directory', mime_type: '' }),
  base({ id: 'text', name: '笔记.txt', mime_type: 'text/plain' }),
  base({ id: 'book', name: '书籍.epub', mime_type: 'application/epub+zip' }),
  base({ id: 'image', name: '图片.png', mime_type: 'image/png' }),
  base({ id: 'archive', name: '归档.zip', mime_type: 'application/zip' }),
  base({ id: 'unknown', name: '未知.pdf', mime_type: 'application/pdf' }),
]

const trashItem = base({
  id: 'trash-text',
  name: '已删除.txt',
  mime_type: 'text/plain',
  deleted_at: '2026-01-02T00:00:00Z',
})

async function mockToolbar(page: Page) {
  await page.route('**/api/**', async route => {
    const path = new URL(route.request().url()).pathname
    const json = (value: unknown) => route.fulfill({ json: value })
    if (path === '/api/auth/me') return json({ username: 'admin', has_avatar: false })
    if (path === '/api/events' || path === '/api/system/status/stream') {
      return route.fulfill({ contentType: 'text/event-stream', body: '' })
    }
    if (path === '/api/tasks') return json({ items: [] })
    if (path === '/api/library/all') {
      return json({ items: { book: [], image: [], video: [], audio: [] }, counts: { book: 0, image: 0, video: 0, audio: 0, file: items.length } })
    }
    if (path === '/api/library/counts') return json({ book: 0, image: 0, video: 0, audio: 0, file: items.length })
    if (path === `/api/files/${ROOT}`) {
      return json({
        file: { id: ROOT, parent_id: null, name: '我的文件', kind: 'directory', size: 0, status: 'ready', created_at: STAMP, updated_at: STAMP, mime_type: '' },
        breadcrumbs: [],
      })
    }
    if (path === `/api/files/${ROOT}/children`) return json({ items, total_bytes: 20_480, file_count: items.length - 1 })
    if (path === '/api/trash') return json({ items: [trashItem], total_bytes: trashItem.size, file_count: 1 })
    if (path.endsWith('/thumbnail')) {
      return route.fulfill({ contentType: 'image/svg+xml', body: '<svg xmlns="http://www.w3.org/2000/svg" width="40" height="40"><rect width="40" height="40" fill="#789"/></svg>' })
    }
    return json({ items: [] })
  })
}

async function openBrowser(page: Page, baseUrl: string) {
  await page.goto(`${baseUrl}/?selection-toolbar-parity=${Date.now()}`)
  await expect(page.getByRole('heading', { name: '我的文件', exact: true })).toBeVisible()
  await page.getByTitle('列表视图').click()
  await expect(page.locator('.file-row')).toHaveCount(items.length)
}

async function toolbarMetrics(page: Page) {
  return page.getByRole('toolbar', { name: '所选项目操作' }).evaluate(toolbar => {
    const style = getComputedStyle(toolbar)
    const rect = toolbar.getBoundingClientRect()
    return {
      summary: toolbar.querySelector('.selection-summary')?.textContent?.replace(/\s+/g, ' ').trim(),
      buttons: Array.from(toolbar.querySelectorAll('.selection-actions button')).map(button => ({
        text: button.textContent?.replace(/\s+/g, ' ').trim(),
        className: button.className,
        title: button.getAttribute('title'),
        disabled: (button as HTMLButtonElement).disabled,
        paths: Array.from(button.querySelectorAll('path')).map(path => path.getAttribute('d')),
      })),
      layout: {
        display: style.display,
        gap: style.gap,
        minHeight: style.minHeight,
        padding: style.padding,
        width: Math.round(rect.width * 100) / 100,
        height: Math.round(rect.height * 100) / 100,
      },
    }
  })
}

async function select(page: Page, name: string) {
  await page.locator('.file-row').filter({ hasText: name }).getByRole('button', { name: '选择项目' }).click()
  await expect(page.getByRole('toolbar', { name: '所选项目操作' })).toBeVisible()
}

test('列表选择工具栏按文件类型保持 reference 的完整按钮分流', async ({ browser }) => {
  const oldUrl = process.env.E2E_REFERENCE_URL || 'http://127.0.0.1:18080'
  const newUrl = process.env.E2E_NEW_URL || 'http://127.0.0.1:18083'
  const oldContext = await browser.newContext({ viewport: { width: 1440, height: 900 } })
  const newContext = await browser.newContext({ viewport: { width: 1440, height: 900 } })
  const oldPage = await oldContext.newPage()
  const newPage = await newContext.newPage()

  try {
    await Promise.all([mockToolbar(oldPage), mockToolbar(newPage)])
    await Promise.all([openBrowser(oldPage, oldUrl), openBrowser(newPage, newUrl)])

    for (const name of items.map(item => item.name)) {
      await Promise.all([select(oldPage, name), select(newPage, name)])
      expect(await toolbarMetrics(newPage), `Rust ${name} 选择工具栏与 reference 不一致`)
        .toEqual(await toolbarMetrics(oldPage))
      await Promise.all([
        oldPage.locator('.selection-close').click(),
        newPage.locator('.selection-close').click(),
      ])
      await expect(oldPage.getByRole('toolbar', { name: '所选项目操作' })).toHaveCount(0)
      await expect(newPage.getByRole('toolbar', { name: '所选项目操作' })).toHaveCount(0)
    }

    await select(oldPage, '笔记.txt')
    await select(newPage, '笔记.txt')
    await Promise.all([
      oldPage.locator('.file-row').filter({ hasText: '图片.png' }).getByRole('button', { name: '选择项目' }).click(),
      newPage.locator('.file-row').filter({ hasText: '图片.png' }).getByRole('button', { name: '选择项目' }).click(),
    ])
    expect(await toolbarMetrics(newPage), 'Rust 多选工具栏与 reference 不一致')
      .toEqual(await toolbarMetrics(oldPage))
  } finally {
    await oldContext.close()
    await newContext.close()
  }
})

test('移动端和回收站选择工具栏保持 reference 的布局与操作分支', async ({ browser }) => {
  const oldUrl = process.env.E2E_REFERENCE_URL || 'http://127.0.0.1:18080'
  const newUrl = process.env.E2E_NEW_URL || 'http://127.0.0.1:18083'
  const oldContext = await browser.newContext({ viewport: { width: 390, height: 844 }, isMobile: true, hasTouch: true })
  const newContext = await browser.newContext({ viewport: { width: 390, height: 844 }, isMobile: true, hasTouch: true })
  const oldPage = await oldContext.newPage()
  const newPage = await newContext.newPage()

  try {
    await Promise.all([mockToolbar(oldPage), mockToolbar(newPage)])
    await Promise.all([openBrowser(oldPage, oldUrl), openBrowser(newPage, newUrl)])
    await Promise.all([select(oldPage, '笔记.txt'), select(newPage, '笔记.txt')])
    expect(await toolbarMetrics(newPage), 'Rust 移动端选择工具栏与 reference 不一致')
      .toEqual(await toolbarMetrics(oldPage))

    await Promise.all([
      oldPage.locator('.selection-close').click(),
      newPage.locator('.selection-close').click(),
    ])
    await Promise.all([
      oldPage.locator('.mobile-account-menu > summary').click(),
      newPage.locator('.mobile-account-menu > summary').click(),
    ])
    await Promise.all([
      oldPage.locator('.mobile-account-menu').getByRole('button', { name: '回收站', exact: true }).click(),
      newPage.locator('.mobile-account-menu').getByRole('button', { name: '回收站', exact: true }).click(),
    ])
    await expect(oldPage.getByRole('heading', { name: '回收站', exact: true })).toBeVisible()
    await expect(newPage.getByRole('heading', { name: '回收站', exact: true })).toBeVisible()
    await Promise.all([select(oldPage, trashItem.name), select(newPage, trashItem.name)])
    expect(await toolbarMetrics(newPage), 'Rust 回收站选择工具栏与 reference 不一致')
      .toEqual(await toolbarMetrics(oldPage))
  } finally {
    await oldContext.close()
    await newContext.close()
  }
})

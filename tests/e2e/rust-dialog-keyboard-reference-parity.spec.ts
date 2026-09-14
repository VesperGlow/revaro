import { expect, test, type Page } from '@playwright/test'

const ROOT = '00000000-0000-0000-0000-000000000000'
const STAMP = '2026-01-01T00:00:00Z'

async function mockDialog(page: Page) {
  await page.route('**/api/**', async route => {
    const path = new URL(route.request().url()).pathname
    const json = (value: unknown) => route.fulfill({ json: value })

    if (path === '/api/auth/me') return json({ username: 'admin', has_avatar: false })
    if (path === '/api/events' || path === '/api/system/status/stream') {
      return route.fulfill({ contentType: 'text/event-stream', body: '' })
    }
    if (path === '/api/tasks') return json({ items: [] })
    if (path === '/api/library/all') {
      return json({ items: { book: [], image: [], video: [], audio: [] }, counts: { book: 0, image: 0, video: 0, audio: 0, file: 0 } })
    }
    if (path === '/api/library/counts') return json({ book: 0, image: 0, video: 0, audio: 0, file: 0 })
    if (path === `/api/files/${ROOT}`) {
      return json({
        file: { id: ROOT, parent_id: null, name: '我的文件', kind: 'directory', size: 0, status: 'ready', created_at: STAMP, updated_at: STAMP, mime_type: '' },
        breadcrumbs: [],
      })
    }
    if (path === `/api/files/${ROOT}/children`) return json({ items: [], total_bytes: 0, file_count: 0 })
    return json({ items: [] })
  })
}

async function openDialog(page: Page, baseUrl: string) {
  await page.goto(`${baseUrl}/`)
  await expect(page.getByRole('heading', { name: '我的文件', exact: true })).toBeVisible()
  await page.getByRole('button', { name: '新建文件夹', exact: true }).click()
  await expect(page.locator('.app-dialog')).toBeVisible()
  await expect(page.locator('.app-dialog input')).toBeFocused()
}

async function installEscapeProbe(page: Page) {
  await page.evaluate(() => {
    ;(window as Window & { __dialogEscapeDefaultPrevented?: boolean }).__dialogEscapeDefaultPrevented = undefined
    window.addEventListener('keydown', event => {
      if (event.key === 'Escape') {
        ;(window as Window & { __dialogEscapeDefaultPrevented?: boolean }).__dialogEscapeDefaultPrevented = event.defaultPrevented
      }
    }, { once: true })
  })
}

test('通用确认弹窗 Escape 保持 reference 的默认事件语义', async ({ browser }) => {
  const oldUrl = process.env.E2E_REFERENCE_URL || 'http://127.0.0.1:18080'
  const newUrl = process.env.E2E_NEW_URL || 'http://127.0.0.1:18083'
  const oldContext = await browser.newContext({ viewport: { width: 1280, height: 900 } })
  const newContext = await browser.newContext({ viewport: { width: 1280, height: 900 } })
  const oldPage = await oldContext.newPage()
  const newPage = await newContext.newPage()

  try {
    await Promise.all([mockDialog(oldPage), mockDialog(newPage)])
    await Promise.all([openDialog(oldPage, oldUrl), openDialog(newPage, newUrl)])
    await Promise.all([installEscapeProbe(oldPage), installEscapeProbe(newPage)])
    await Promise.all([oldPage.keyboard.press('Escape'), newPage.keyboard.press('Escape')])
    await expect(oldPage.locator('.app-dialog')).toHaveCount(0)
    await expect(newPage.locator('.app-dialog')).toHaveCount(0)
    expect(await oldPage.evaluate(() => (window as Window & { __dialogEscapeDefaultPrevented?: boolean }).__dialogEscapeDefaultPrevented)).toBe(false)
    expect(await newPage.evaluate(() => (window as Window & { __dialogEscapeDefaultPrevented?: boolean }).__dialogEscapeDefaultPrevented)).toBe(false)
  } finally {
    await oldContext.close()
    await newContext.close()
  }
})

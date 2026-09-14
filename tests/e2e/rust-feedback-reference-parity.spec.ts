import { expect, test, type Page } from '@playwright/test'

const ROOT = '00000000-0000-0000-0000-000000000000'
const STAMP = '2026-01-01T00:00:00Z'

const files = ['toast-one.txt', 'toast-two.txt'].map((name, index) => ({
  id: `toast-file-${index + 1}`,
  parent_id: ROOT,
  name,
  kind: 'file',
  size: 12,
  status: 'ready',
  created_at: STAMP,
  updated_at: STAMP,
  mime_type: 'text/plain',
  etag: `etag-${index + 1}`,
}))

async function mockFeedback(page: Page) {
  await page.route('**/api/**', async route => {
    const path = new URL(route.request().url()).pathname
    const json = (value: unknown) => route.fulfill({ json: value })

    if (path === '/api/auth/me') return json({ username: 'admin', has_avatar: false })
    if (path === '/api/events' || path === '/api/system/status/stream') {
      return route.fulfill({ contentType: 'text/event-stream', body: '' })
    }
    if (path === '/api/tasks') return json({ items: [] })
    if (path === '/api/library/all') {
      return json({ items: { book: [], image: [], video: [], audio: [] }, counts: { book: 0, image: 0, video: 0, audio: 0, file: files.length } })
    }
    if (path === '/api/library/counts') return json({ book: 0, image: 0, video: 0, audio: 0, file: files.length })
    if (path === `/api/files/${ROOT}`) {
      return json({
        file: { id: ROOT, parent_id: null, name: '我的文件', kind: 'directory', size: 0, status: 'ready', created_at: STAMP, updated_at: STAMP, mime_type: '' },
        breadcrumbs: [],
      })
    }
    if (path === `/api/files/${ROOT}/children`) return json({ items: files, total_bytes: 24, file_count: files.length })
    if (path === '/api/files/batch-download/prepare') {
      await new Promise(resolve => setTimeout(resolve, 600))
      return json({ token: 'feedback-parity-token' })
    }
    return json({ items: [] })
  })
}

async function openFeedbackFixture(page: Page, baseUrl: string) {
  await page.goto(`${baseUrl}/`)
  await expect(page.getByRole('heading', { name: '我的文件', exact: true })).toBeVisible()
  await page.getByRole('button', { name: '列表', exact: true }).click()
  for (const file of files) {
    const row = page.locator('.file-row').filter({ hasText: file.name })
    await expect(row).toBeVisible()
    await row.getByRole('button', { name: '选择项目' }).click()
  }
  await expect(page.getByRole('toolbar', { name: '所选项目操作' })).toBeVisible()
}

async function toastMetrics(page: Page) {
  return page.locator('.toast').evaluate(element => {
    const style = getComputedStyle(element)
    return {
      pointerEvents: style.pointerEvents,
      position: style.position,
      bottom: style.bottom,
      transform: style.transform,
      background: style.backgroundColor,
      color: style.color,
      padding: style.padding,
      borderRadius: style.borderRadius,
      fontSize: style.fontSize,
    }
  })
}

async function clickToast(page: Page) {
  const box = await page.locator('.toast').boundingBox()
  if (!box) throw new Error('toast did not have a box')
  await page.mouse.click(box.x + box.width / 2, box.y + box.height / 2)
}

test('全局 toast 的命中区域和最新通知交互保持 reference', async ({ browser }) => {
  const oldUrl = process.env.E2E_REFERENCE_URL || 'http://127.0.0.1:18080'
  const newUrl = process.env.E2E_NEW_URL || 'http://127.0.0.1:18083'
  const oldContext = await browser.newContext({ viewport: { width: 1280, height: 900 } })
  const newContext = await browser.newContext({ viewport: { width: 1280, height: 900 } })
  const oldPage = await oldContext.newPage()
  const newPage = await newContext.newPage()

  try {
    await Promise.all([mockFeedback(oldPage), mockFeedback(newPage)])
    await Promise.all([
      openFeedbackFixture(oldPage, oldUrl),
      openFeedbackFixture(newPage, newUrl),
    ])
    await Promise.all([
      oldPage.getByRole('toolbar', { name: '所选项目操作' }).getByRole('button', { name: '下载 (2)', exact: true }).click(),
      newPage.getByRole('toolbar', { name: '所选项目操作' }).getByRole('button', { name: '下载 (2)', exact: true }).click(),
    ])
    await expect(oldPage.locator('.toast')).toHaveText('正在准备 2 个文件…')
    await expect(newPage.locator('.toast')).toHaveText('正在准备 2 个文件…')
    expect(await toastMetrics(newPage), 'Rust toast 的 CSS 命中区域与 reference 不一致')
      .toEqual(await toastMetrics(oldPage))

    await Promise.all([clickToast(oldPage), clickToast(newPage)])
    await expect(oldPage.getByRole('toolbar', { name: '所选项目操作' })).toBeVisible()
    await expect(newPage.getByRole('toolbar', { name: '所选项目操作' })).toBeVisible()
  } finally {
    await oldContext.close()
    await newContext.close()
  }
})

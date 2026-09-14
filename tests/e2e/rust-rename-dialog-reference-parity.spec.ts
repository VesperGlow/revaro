import { expect, test, type Page } from '@playwright/test'

const ROOT = '00000000-0000-0000-0000-000000000000'
const FILE_ID = 'rename-dialog-file'
const STAMP = '2026-01-01T00:00:00Z'

const file = {
  id: FILE_ID,
  parent_id: ROOT,
  name: '旧名称.txt',
  kind: 'file',
  size: 12,
  status: 'ready',
  created_at: STAMP,
  updated_at: STAMP,
  mime_type: 'text/plain',
}

async function mockBrowser(page: Page) {
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
    if (path === `/api/files/${ROOT}`) {
      return json({
        file: { id: ROOT, parent_id: null, name: '我的文件', kind: 'directory', size: 0, status: 'ready', created_at: STAMP, updated_at: STAMP, mime_type: '' },
        breadcrumbs: [],
      })
    }
    if (path === `/api/files/${ROOT}/children`) return json({ items: [file], total_bytes: file.size, file_count: 1 })
    if (path === `/api/files/${FILE_ID}`) return json({ file, breadcrumbs: [] })
    if (path === `/api/files/${FILE_ID}/children`) return json({ items: [], total_bytes: 0, file_count: 0 })
    return json({ items: [] })
  })
}

async function openRename(page: Page, baseUrl: string) {
  await page.goto(`${baseUrl}/`)
  await expect(page.getByRole('heading', { name: '我的文件', exact: true })).toBeVisible()
  await page.getByRole('button', { name: '列表', exact: true }).click()
  const row = page.locator('.file-row').filter({ hasText: file.name })
  await expect(row).toBeVisible()
  await row.getByRole('button', { name: '选择项目' }).click()
  await page.getByRole('toolbar', { name: '所选项目操作' }).getByRole('button', { name: '重命名' }).click()
  await expect(page.locator('.modal-backdrop > .modal')).toBeVisible()
  await expect(page.locator('.selection-toolbar')).toHaveCount(0)
}

async function snapshot(page: Page) {
  return page.locator('.modal-backdrop > .modal').evaluate(dialog => {
    const input = dialog.querySelector('input') as HTMLInputElement | null
    const active = document.activeElement as HTMLElement | null
    return {
      text: dialog.textContent?.replace(/\s+/g, ' ').trim(),
      input: input && {
        value: input.value,
        type: input.type,
        maxLength: input.maxLength,
        placeholder: input.getAttribute('placeholder'),
        autofocus: input.hasAttribute('autofocus'),
      },
      buttons: Array.from(dialog.querySelectorAll('button')).map(button => ({
        text: button.textContent?.replace(/\s+/g, ' ').trim(),
        disabled: button.disabled,
      })),
      active: active === input ? 'input' : active?.tagName.toLowerCase() || null,
      activeClass: active?.className || null,
      selectionToolbarCount: document.querySelectorAll('.selection-toolbar').length,
    }
  })
}

test('重命名弹窗的初始控件状态与 reference 一致', async ({ browser }) => {
  const oldUrl = process.env.E2E_REFERENCE_URL || 'http://127.0.0.1:18080'
  const newUrl = process.env.E2E_NEW_URL || 'http://127.0.0.1:18083'
  const oldContext = await browser.newContext({ viewport: { width: 1280, height: 900 } })
  const newContext = await browser.newContext({ viewport: { width: 1280, height: 900 } })
  const oldPage = await oldContext.newPage()
  const newPage = await newContext.newPage()

  try {
    await Promise.all([mockBrowser(oldPage), mockBrowser(newPage)])
    await Promise.all([openRename(oldPage, oldUrl), openRename(newPage, newUrl)])
    expect(await snapshot(newPage), 'Rust 重命名弹窗初始状态与 reference 不一致').toEqual(await snapshot(oldPage))
  } finally {
    await oldContext.close()
    await newContext.close()
  }
})

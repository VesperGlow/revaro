import { expect, test, type Page } from '@playwright/test'

const ROOT = '00000000-0000-0000-0000-000000000000'
const STAMP = '2026-01-01T00:00:00Z'

function rootFile() {
  return {
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
}

async function mockEmptyShell(page: Page, failChildren = false) {
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
    if (path === `/api/files/${ROOT}`) return json({ file: rootFile(), breadcrumbs: [] })
    if (path === `/api/files/${ROOT}/children` && failChildren) {
      return route.fulfill({ status: 500, json: { error: { status: 500, code: null, message: '模拟读取失败' } } })
    }
    if (path === `/api/files/${ROOT}/children`) return json({ items: [], total_bytes: 0, file_count: 0 })
    if (path === '/api/trash') return json({ items: [], total_bytes: 0, file_count: 0 })
    return json({ items: [] })
  })
}

async function openEmptyShell(page: Page) {
  await page.goto('/')
  await expect(page.getByRole('heading', { name: '我的文件' })).toBeVisible()
  await expect(page.locator('.state.empty')).toBeVisible()
}

test('空目录和空回收站保留 reference 的文案与快捷入口', async ({ page }) => {
  await mockEmptyShell(page)
  await openEmptyShell(page)

  const empty = page.locator('.state.empty')
  await expect(empty.locator('h3')).toHaveText('这里还是空的')
  await expect(empty.locator('p')).toHaveText('拖放文件到这里，或新建一篇文档。')
  await expect(empty.locator('.empty-icon')).toHaveText('⌁')
  await expect(empty.locator('.empty-actions')).toHaveCount(1)
  await expect(empty.getByRole('button', { name: '新建文档' })).toBeVisible()
  await expect(empty.getByRole('button', { name: '上传文件' })).toBeVisible()

  await empty.getByRole('button', { name: '新建文档' }).click()
  await expect(page.locator('.document-editor')).toBeVisible()
  await page.getByRole('button', { name: '关闭编辑器' }).click()
  await expect(page.locator('.document-editor')).toHaveCount(0)

  await page.locator('.topbar .trash-button').click()
  await expect(page.getByRole('heading', { name: '回收站', exact: true })).toBeVisible()
  const trash = page.locator('.state.empty')
  await expect(trash.locator('h3')).toHaveText('回收站是空的')
  await expect(trash.locator('p')).toHaveText('删除的项目会先来到这里。')
  await expect(trash.locator('.empty-actions')).toHaveCount(0)
})

test('目录加载失败通过 reference 的 toast 呈现，并保留原页面结构', async ({ page }) => {
  await mockEmptyShell(page, true)
  await page.goto('/')
  await expect(page.getByRole('heading', { name: '我的文件', exact: true })).toBeVisible()
  await expect(page.locator('.toast')).toContainText('模拟读取失败')
  await expect(page.locator('.state.empty h3')).toHaveText('这里还是空的')
  await expect(page.locator('.state[role="alert"]')).toHaveCount(0)
})

import { expect, test, type Page } from '@playwright/test'
import { login, navigate, openMusicPlayer, uploadFixture, failApplicationRequest, holdApplicationResponse } from './helpers'

const root = '00000000-0000-0000-0000-000000000000'
async function folder(page: Page) {
  const origin = new URL(page.url()).origin
  const response = await page.request.post('/api/directories', { headers: { origin }, data: { parent_id: root, name: `resilience-${Date.now()}` } })
  expect(response.ok()).toBeTruthy()
  return { id: (await response.json()).id, headers: { origin } }
}
async function cleanup(page: Page, id: string, headers: Record<string, string>) {
  await page.request.delete(`/api/files/${id}`, { headers })
  await page.request.delete(`/api/trash/${id}`, { headers })
}
function wav() {
  const bytes = Buffer.alloc(44 + 90 * 8000 * 2)
  bytes.write('RIFF'); bytes.writeUInt32LE(bytes.length - 8, 4); bytes.write('WAVEfmt ', 8)
  bytes.writeUInt32LE(16, 16); bytes.writeUInt16LE(1, 20); bytes.writeUInt16LE(1, 22)
  bytes.writeUInt32LE(8000, 24); bytes.writeUInt32LE(16000, 28); bytes.writeUInt16LE(2, 32)
  bytes.writeUInt16LE(16, 34); bytes.write('data', 36); bytes.writeUInt32LE(bytes.length - 44, 40)
  return bytes
}

test('failed resume reads preserve saved progress and can be retried', async ({ page }) => {
  await login(page)
  const { id, headers } = await folder(page)
  try {
    const song = await uploadFixture(page, id, `resume-${Date.now()}.wav`, 'audio/wav', wav())
    expect((await page.request.put(`/api/files/${song.id}/media/progress`, { headers, data: { position: 60, duration: 90 } })).ok()).toBeTruthy()
    const failure = await failApplicationRequest(page, `/api/files/${song.id}/media/progress`, '模拟历史读取失败')
    await navigate(page, '音乐')
    await page.getByRole('button', { name: `打开 ${song.name}`, exact: true }).click()
    await openMusicPlayer(page)
    await expect(page.getByRole('button', { name: '重试读取进度', exact: true })).toBeVisible()
    expect((await (await page.request.get(`/api/files/${song.id}/media/progress`)).json()).position).toBe(60)
    await failure.clear()
    await page.getByRole('button', { name: '重试读取进度', exact: true }).click()
    await expect.poll(() => page.locator('audio').first().evaluate((audio: HTMLAudioElement) => audio.currentTime)).toBeGreaterThanOrEqual(60)
  } finally { await page.goto('/files'); await cleanup(page, id, headers) }
})

test('refresh warns about edits and offers local draft recovery', async ({ page }) => {
  await login(page)
  const { id, headers } = await folder(page)
  try {
    const response = await page.request.post('/api/documents', { headers, data: { parent_id: id, name: 'draft.md', content: 'saved' } })
    expect(response.ok()).toBeTruthy()
    await page.goto(`/f/${id}`)
    await page.locator('.file-card').filter({ hasText: 'draft.md' }).click()
    await expect(page.getByLabel('文档内容', { exact: true })).toHaveValue('saved')
    await page.getByLabel('文档内容', { exact: true }).fill('important unsaved text')
    const dialog = page.waitForEvent('dialog')
    const refreshed = page.waitForEvent('load')
    await page.evaluate(() => { setTimeout(() => location.reload(), 0) })
    const warning = await dialog
    expect(warning.type()).toBe('beforeunload')
    await warning.accept()
    await refreshed
    await page.locator('.file-card').filter({ hasText: 'draft.md' }).click()
    await expect(page.getByRole('button', { name: '恢复草稿', exact: true })).toBeVisible()
    await expect(page.getByLabel('文档内容', { exact: true })).toHaveValue('saved')
    await page.getByRole('button', { name: '恢复草稿', exact: true }).click()
    await expect(page.getByLabel('文档内容', { exact: true })).toHaveValue('important unsaved text')
    await page.getByRole('button', { name: '关闭编辑器', exact: true }).click()
    await page.getByRole('button', { name: '放弃修改', exact: true }).click()
    await page.locator('.file-card').filter({ hasText: 'draft.md' }).click()
    await expect(page.getByLabel('文档内容', { exact: true })).toHaveValue('saved')
    await expect(page.getByRole('button', { name: '恢复草稿', exact: true })).toHaveCount(0)
  } finally { await page.goto('/files'); await cleanup(page, id, headers) }
})

test('version restoration prevents edits until the result has been applied', async ({ page }) => {
  await login(page)
  const { id, headers } = await folder(page)
  let held: Awaited<ReturnType<typeof holdApplicationResponse>> | undefined
  try {
    const response = await page.request.post('/api/documents', { headers, data: { parent_id: id, name: 'restore.md', content: 'first' } })
    const doc = await response.json()
    expect((await page.request.put(`/api/files/${doc.id}/content`, { headers, data: { etag: doc.etag, content: 'second' } })).ok()).toBeTruthy()
    await page.goto(`/f/${id}`)
    await page.locator('.file-card').filter({ hasText: 'restore.md' }).click()
    const text = page.getByLabel('文档内容', { exact: true })
    await expect(text).toHaveValue('second')
    await page.getByRole('button', { name: '版本历史', exact: true }).click()
    held = await holdApplicationResponse(page, `^/api/files/${doc.id}/versions/[^/]+/restore$`, 'POST')
    await page.getByRole('button', { name: '恢复此版本', exact: true }).click()
    await expect.poll(() => held!.captured()).toBe(true)
    await expect(text).not.toBeEditable()
    await held.release()
    await expect(text).toHaveValue('first')
    await expect(text).toBeEditable()
  } finally { await held?.release().catch(() => {}); await page.goto('/files'); await cleanup(page, id, headers) }
})

test('typing while a save is stalled survives an immediate refresh', async ({ page }) => {
  await login(page)
  const { id, headers } = await folder(page)
  try {
    const response = await page.request.post('/api/documents', { headers, data: { parent_id: id, name: 'pending-save.md', content: 'server copy' } })
    const doc = await response.json()
    await page.goto(`/f/${id}`)
    await page.locator('.file-card').filter({ hasText: 'pending-save.md' }).click()
    const text = page.getByLabel('文档内容', { exact: true })
    await expect(text).toHaveValue('server copy')
    await page.evaluate(path => {
      const original = window.fetch.bind(window)
      window.fetch = (input, init) => {
        const request = input instanceof Request ? input : new Request(new URL(input.toString(), location.href), init)
        if (new URL(request.url).pathname === path && request.method === 'PUT') return new Promise(() => {})
        return original(input, init)
      }
    }, `/api/files/${doc.id}/content`)
    await text.fill('submitted snapshot')
    await page.getByRole('button', { name: '保存', exact: true }).click()
    await expect(page.getByRole('button', { name: '保存中…', exact: true })).toBeVisible()
    await expect(text).toBeEditable()
    await text.fill('newer edits during the stalled save')
    const warning = page.waitForEvent('dialog')
    const reload = page.waitForEvent('load')
    await page.evaluate(() => { setTimeout(() => location.reload(), 0) })
    await (await warning).accept()
    await reload
    await page.locator('.file-card').filter({ hasText: 'pending-save.md' }).click()
    await page.getByRole('button', { name: '恢复草稿', exact: true }).click()
    await expect(text).toHaveValue('newer edits during the stalled save')
  } finally {
    page.once('dialog', dialog => dialog.accept())
    await page.goto('/files')
    await cleanup(page, id, headers)
  }
})

test('a temporary startup session error keeps the existing login and can reconnect', async ({ page }) => {
  await login(page)
  await page.addInitScript(() => {
    // Install this fault above the document transport when it replaces fetch,
    // so session UI recovery is independent of network retry backoff.
    ;(window as any).failSession = true
    let current = window.fetch.bind(window)
    Object.defineProperty(window, 'fetch', {
      configurable: true,
      get: () => current,
      set: (next: typeof fetch) => {
        current = (input, init) => {
          const path = new URL(typeof input === 'string' ? input : input instanceof Request ? input.url : input.toString(), location.href).pathname
          if ((window as any).failSession && path === '/api/auth/me') return Promise.resolve(new Response('{}', { status: 503 }))
          return next(input, init)
        }
      },
    })
  })
  await page.reload()
  const reconnect = page.getByRole('button', { name: '重新连接', exact: true })
  await expect(reconnect).toBeVisible()
  await expect(page.getByLabel('用户名', { exact: true })).toHaveCount(0)
  await page.evaluate(() => { (window as any).failSession = false })
  await reconnect.click()
  await expect(page.getByRole('navigation', { name: '主导航', exact: true })).toBeVisible()
})

test('an expired session preserves current edits for recovery after signing in', async ({ page }) => {
  await login(page)
  const { id, headers } = await folder(page)
  try {
    const response = await page.request.post('/api/documents', { headers, data: { parent_id: id, name: 'expired-session.md', content: 'server copy' } })
    expect(response.ok()).toBeTruthy()
    const doc = await response.json()
    await page.goto(`/f/${id}`)
    await page.locator('.file-card').filter({ hasText: 'expired-session.md' }).click()
    const text = page.getByLabel('文档内容', { exact: true })
    await expect(text).toHaveValue('server copy')
    await page.evaluate(path => {
      const original = window.fetch.bind(window)
      window.fetch = (input, init) => {
        const request = input instanceof Request ? input : new Request(new URL(input.toString(), location.href), init)
        if (new URL(request.url).pathname === path && request.method === 'PUT') return Promise.resolve(new Response('{}', { status: 401 }))
        return original(input, init)
      }
    }, `/api/files/${doc.id}/content`)
    await text.fill('important edits before session expiration')
    await page.getByRole('button', { name: '保存', exact: true }).click()
    await page.getByLabel('用户名', { exact: true }).fill('admin')
    await page.getByLabel('密码', { exact: true }).fill('revaro-e2e-password')
    await page.getByRole('button', { name: '进入我的内容库', exact: true }).click()
    await page.goto(`/f/${id}`)
    await page.locator('.file-card').filter({ hasText: 'expired-session.md' }).click()
    await expect(page.getByRole('button', { name: '恢复草稿', exact: true })).toBeVisible()
    await page.getByRole('button', { name: '恢复草稿', exact: true }).click()
    await expect(text).toHaveValue('important edits before session expiration')
  } finally {
    page.once('dialog', dialog => dialog.accept())
    await page.goto('/files')
    await cleanup(page, id, headers)
  }
})

test('saving a new document retires its initial local draft', async ({ page }) => {
  await login(page)
  const { id, headers } = await folder(page)
  const openNew = async () => {
    await page.getByLabel('更多操作', { exact: true }).click()
    await page.getByLabel('新建', { exact: true }).click()
    await page.getByRole('button', { name: '新建文档', exact: true }).click()
  }
  try {
    await page.goto(`/f/${id}`)
    await openNew()
    await page.getByLabel('文档文件名', { exact: true }).fill('created.md')
    await page.getByLabel('文档内容', { exact: true }).fill('new document content')
    const drafts = () => page.evaluate(() => Object.keys(localStorage).filter(key => key.startsWith('revaro-editor-draft:')))
    await expect.poll(drafts).toHaveLength(1)
    await page.getByRole('button', { name: '保存', exact: true }).click()
    await expect(page.getByRole('button', { name: '保存', exact: true })).toBeDisabled()
    await expect.poll(drafts).toHaveLength(0)
    await page.getByRole('button', { name: '关闭编辑器', exact: true }).click()
    await page.locator('.document-editor').waitFor({ state: 'detached' })
    await openNew()
    await expect(page.getByRole('button', { name: '恢复草稿', exact: true })).toHaveCount(0)
    await expect(page.getByLabel('文档内容', { exact: true })).toHaveValue('')
  } finally { await page.goto('/files'); await cleanup(page, id, headers) }
})

test('draft recovery follows the document when the administrator name changes', async ({ page }) => {
  await login(page)
  const { id, headers } = await folder(page)
  try {
    const response = await page.request.post('/api/documents', { headers, data: { parent_id: id, name: 'renamed-admin.md', content: 'saved' } })
    expect(response.ok()).toBeTruthy()
    await page.goto(`/f/${id}`)
    await page.locator('.file-card').filter({ hasText: 'renamed-admin.md' }).click()
    await expect(page.getByLabel('文档内容', { exact: true })).toHaveValue('saved')
    await page.getByLabel('文档内容', { exact: true }).fill('draft under the former administrator name')
    // Supply the profile returned after an administrator rename, without
    // changing credentials used by the other tests on this disposable server.
    await page.addInitScript(() => {
      const original = window.fetch.bind(window)
      window.fetch = async (input, init) => {
        const response = await original(input, init)
        const path = new URL(typeof input === 'string' ? input : input instanceof Request ? input.url : input.toString(), location.href).pathname
        if (path !== '/api/auth/me' || response.status !== 200) return response
        const profile = await response.json()
        profile.username = 'renamed-administrator'
        const headers = new Headers(response.headers)
        headers.delete('content-length')
        headers.delete('content-encoding')
        return new Response(JSON.stringify(profile), { status: 200, headers })
      }
    })
    const warning = page.waitForEvent('dialog')
    const refreshed = page.waitForEvent('load')
    await page.evaluate(() => { setTimeout(() => location.reload(), 0) })
    await (await warning).accept()
    await refreshed
    await page.locator('.file-card').filter({ hasText: 'renamed-admin.md' }).click()
    await page.getByRole('button', { name: '恢复草稿', exact: true }).click()
    await expect(page.getByLabel('文档内容', { exact: true })).toHaveValue('draft under the former administrator name')
  } finally {
    page.once('dialog', dialog => dialog.accept())
    await page.goto('/files')
    await cleanup(page, id, headers)
  }
})

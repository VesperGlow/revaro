import { expect, test, type Page } from '@playwright/test'

const ROOT = '00000000-0000-0000-0000-000000000000'
const STAMP = '2026-01-01T00:00:00Z'
const posterEtag = 'poster v/1'
const video = {
  id: 'poster-video',
  parent_id: ROOT,
  name: '带版本海报.mp4',
  kind: 'file',
  size: 1024,
  status: 'ready',
  mime_type: 'video/mp4',
  etag: posterEtag,
  created_at: STAMP,
  updated_at: STAMP,
}

async function mock(page: Page, baseUrl: string) {
  await page.route('**/api/**', async route => {
    const path = new URL(route.request().url()).pathname
    const json = (value: unknown) => route.fulfill({ json: value })
    if (path === '/api/auth/me') return json({ username: 'admin', has_avatar: false })
    if (path === '/api/events' || path === '/api/system/status/stream') {
      return route.fulfill({ contentType: 'text/event-stream', body: '' })
    }
    if (path === '/api/tasks') return json({ items: [] })
    if (path === '/api/library/all') {
      return json({
        items: { book: [], image: [], video: [video], audio: [] },
        counts: { book: 0, image: 0, video: 1, audio: 0, file: 1 },
      })
    }
    if (path === '/api/library/counts') return json({ book: 0, image: 0, video: 1, audio: 0, file: 1 })
    if (path === `/api/files/${ROOT}`) {
      return json({
        file: { id: ROOT, parent_id: null, name: '我的文件', kind: 'directory', size: 0, status: 'ready', created_at: STAMP, updated_at: STAMP },
        breadcrumbs: [],
      })
    }
    if (path === `/api/files/${ROOT}/children`) return json({ items: [video], total_bytes: video.size, file_count: 1 })
    if (path === `/api/files/${video.id}`) return json({ file: video, breadcrumbs: [] })
    if (path === `/api/files/${video.id}/media/progress`) return json({ position: 0, duration: 0 })
    if (path.endsWith('/thumbnail')) {
      return route.fulfill({ contentType: 'image/svg+xml', body: '<svg xmlns="http://www.w3.org/2000/svg" width="320" height="180"><rect width="320" height="180" fill="#456"/></svg>' })
    }
    if (path.endsWith('/preview')) {
      return route.fulfill({ contentType: 'video/mp4', body: 'not a decodable video' })
    }
    return json({ items: [] })
  })
  await page.goto(`${baseUrl}/?video-poster-reference=${Date.now()}`)
  await expect(page.locator('.file-card')).toHaveCount(1)
}

test('视频 poster old/new 均使用带 etag 的版本化缩略图地址', async ({ browser }) => {
  const oldUrl = process.env.E2E_REFERENCE_URL || 'http://127.0.0.1:18080'
  const newUrl = process.env.E2E_NEW_URL || 'http://127.0.0.1:18084'
  const oldContext = await browser.newContext({ viewport: { width: 1440, height: 900 } })
  const newContext = await browser.newContext({ viewport: { width: 1440, height: 900 } })
  const oldPage = await oldContext.newPage()
  const newPage = await newContext.newPage()

  async function exercise(page: Page, baseUrl: string) {
    await mock(page, baseUrl)
    await page.locator('.file-card').filter({ hasText: video.name }).click()
    await expect(page.locator('.video-player-shell')).toBeVisible()
    return page.locator('.video-player-shell video').getAttribute('poster')
  }

  try {
    const [oldPoster, newPoster] = await Promise.all([
      exercise(oldPage, oldUrl),
      exercise(newPage, newUrl),
    ])
    expect(oldPoster).toBe(`/api/files/${video.id}/thumbnail?v=${encodeURIComponent(posterEtag)}`)
    expect(newPoster, 'Rust 视频 poster 未恢复 reference 的 etag 版本参数').toBe(oldPoster)
  } finally {
    await Promise.all([oldContext.close(), newContext.close()])
  }
})

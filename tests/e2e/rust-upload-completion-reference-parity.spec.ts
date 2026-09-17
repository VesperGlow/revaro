import { expect, test, type Page } from '@playwright/test'

const ROOT = '00000000-0000-0000-0000-000000000000'
const UPLOAD_ID = 'upload-category-refresh'
const UPLOADED_ID = 'uploaded-category-image'
const STAMP = '2026-01-01T00:00:00Z'
const IMAGE = '<svg xmlns="http://www.w3.org/2000/svg" width="160" height="100"><rect width="160" height="100" fill="#789"/></svg>'

function file(id: string, name: string, parentId = ROOT) {
  return {
    id,
    parent_id: parentId,
    name,
    kind: 'file',
    size: 68,
    status: 'ready',
    created_at: STAMP,
    updated_at: STAMP,
    mime_type: 'image/png',
    etag: 'category-etag',
    folder_path: [],
  }
}

async function mockUploadCompletion(page: Page, name: string) {
  let completed = false
  let libraryRefreshes = 0
  const initialImage = file('existing-category-image', '已有图片.png')
  const uploadedImage = file(UPLOADED_ID, name)
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
  await page.route('**/api/**', async route => {
    const request = route.request()
    const url = new URL(request.url())
    const path = url.pathname
    const json = (value: unknown, status = 200) => route.fulfill({ status, contentType: 'application/json', body: JSON.stringify(value) })
    if (path === '/api/auth/me') return json({ username: 'admin', has_avatar: false })
    if (path === '/api/events' || path === '/api/system/status/stream') return route.fulfill({ contentType: 'text/event-stream', body: '' })
    if (path === '/api/tasks') return json({ items: [] })
    if (path === '/api/files/' + ROOT) return json({ file: root, breadcrumbs: [] })
    if (path === '/api/files/' + ROOT + '/children') {
      return json({ items: completed ? [uploadedImage] : [], total_bytes: completed ? uploadedImage.size : 0, file_count: completed ? 1 : 0 })
    }
    if (path === '/api/library/all') {
      libraryRefreshes += 1
      return json({
        items: { book: [], image: completed ? [initialImage, uploadedImage] : [initialImage], video: [], audio: [] },
        counts: { book: 0, image: completed ? 2 : 1, video: 0, audio: 0, file: completed ? 1 : 0 },
      })
    }
    if (path === '/api/library/counts') {
      return json({ book: 0, image: completed ? 2 : 1, video: 0, audio: 0, file: completed ? 1 : 0 })
    }
    if (path === `/api/files/${UPLOADED_ID}/thumbnail` || path === `/api/files/${UPLOADED_ID}/preview`) {
      return route.fulfill({ contentType: 'image/svg+xml', body: IMAGE })
    }
    if (path === '/api/uploads' && request.method() === 'POST') {
      return json({ upload_id: UPLOAD_ID, mode: 'single', url: `/api/uploads/${UPLOAD_ID}/data`, part_size: 0, part_count: 0 }, 201)
    }
    if (path === `/api/uploads/${UPLOAD_ID}/data` && request.method() === 'PUT') {
      return route.fulfill({ status: 200, headers: { ETag: 'upload-etag' }, body: '' })
    }
    if (path === `/api/uploads/${UPLOAD_ID}/complete` && request.method() === 'POST') {
      completed = true
      return json({}, 200)
    }
    if (path === `/api/uploads/${UPLOAD_ID}` && request.method() === 'GET') {
      return json({ upload_id: UPLOAD_ID, status: 'pending', mode: 'single', url: `/api/uploads/${UPLOAD_ID}/data`, part_size: 0, part_count: 0 })
    }
    if (path === `/api/uploads/${UPLOAD_ID}` && request.method() === 'DELETE') return route.fulfill({ status: 204, body: '' })
    return json({ items: [] })
  })
  return {
    libraryRefreshes: () => libraryRefreshes,
    isCompleted: () => completed,
  }
}

test('old/new 上传完成后刷新当前媒体分类、统计和当前视图', async ({ browser }) => {
  const name = `分类刷新-${crypto.randomUUID()}.png`
  const oldUrl = process.env.E2E_REFERENCE_URL || 'http://127.0.0.1:18080'
  const newUrl = process.env.E2E_NEW_URL || 'http://127.0.0.1:18084'
  const oldContext = await browser.newContext({ viewport: { width: 1440, height: 900 } })
  const newContext = await browser.newContext({ viewport: { width: 1440, height: 900 } })
  const oldPage = await oldContext.newPage()
  const newPage = await newContext.newPage()

  async function exercise(page: Page, baseUrl: string) {
    const state = await mockUploadCompletion(page, name)
    await page.goto(`${baseUrl}/?upload-completion-parity=${Date.now()}`)
    await expect(page.getByRole('heading', { name: '我的文件', exact: true })).toBeVisible()
    await page.locator('[data-category="image"]').click()
    await page.locator('.app-shell').evaluate((element, droppedName) => {
      const transfer = new DataTransfer()
      transfer.items.add(new File(['category refresh'], droppedName, { type: 'image/png' }))
      element.dispatchEvent(new DragEvent('drop', { bubbles: true, cancelable: true, dataTransfer: transfer }))
    }, name)
    await expect.poll(() => page.getByText(name, { exact: false }).count(), { timeout: 15_000 }).toBeGreaterThan(0)
    await expect.poll(() => page.locator('[data-category="image"]').first().textContent() ?? '', { timeout: 15_000 }).toContain('2')
    const result = {
      count: await page.locator('[data-category="image"]').first().textContent(),
      hasUploaded: await page.getByText(name, { exact: false }).count(),
      libraryRefreshes: state.libraryRefreshes(),
      completed: state.isCompleted(),
    }
    return result
  }

  try {
    const [oldResult, newResult] = await Promise.all([
      exercise(oldPage, oldUrl),
      exercise(newPage, newUrl),
    ])
    expect(oldResult.completed, 'reference 上传应完成').toBe(true)
    expect(newResult.completed, 'Rust 上传应完成').toBe(true)
    expect(newResult.count, 'Rust 分类统计未刷新到完成后的数量').toContain('2')
    expect(newResult.hasUploaded, 'Rust 当前分类未显示已完成的上传').toBeGreaterThan(0)
    expect(newResult.libraryRefreshes, 'Rust 上传完成后未刷新媒体库').toBeGreaterThanOrEqual(2)
    expect(newResult).toEqual(oldResult)
  } finally {
    await Promise.all([oldContext.close(), newContext.close()])
  }
})

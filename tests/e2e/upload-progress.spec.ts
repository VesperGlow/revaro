import { expect, test, type Page } from '@playwright/test'
import { createHash } from 'node:crypto'
import { login, navigate } from './helpers'

const overlay = (page: Page) => page.getByRole('region', { name: '上传进度', exact: true })
const row = (page: Page, name: string) => page.locator('.upload-progress-item').filter({ has: page.locator('strong', { hasText: name }) })

test('lost creation and completion responses reuse one session and do not retransmit accepted bytes', async ({ page }) => {
  await login(page)
  const name = `lost-response-${Date.now()}.bin`
  const payload = Buffer.from('recover an accepted upload without duplicating it')
  const ids: string[] = []
  const keys: string[] = []
  let dataRequests = 0
  let completionRequests = 0
  await page.route('**/api/uploads', async route => {
    keys.push(route.request().postDataJSON().idempotency_key)
    const response = await route.fetch()
    ids.push((await response.json()).upload_id)
    if (ids.length === 1) await route.abort('connectionreset')
    else await route.fulfill({ response })
  })
  await page.route('**/api/uploads/*/data', async route => {
    dataRequests++
    await route.continue()
  })
  await page.route('**/api/uploads/*/complete', async route => {
    completionRequests++
    const response = await route.fetch()
    expect(response.status()).toBe(200)
    if (completionRequests === 1) await route.abort('connectionreset')
    else await route.fulfill({ response })
  })
  await page.getByLabel('选择文件上传', { exact: true }).setInputFiles({ name, mimeType: 'application/octet-stream', buffer: payload })
  await expect.poll(() => completionRequests).toBe(2)
  await expect(overlay(page)).toHaveCount(0)
  expect(ids).toHaveLength(2)
  expect(ids[1]).toBe(ids[0])
  expect(keys[0]).toBeTruthy()
  expect(keys[1]).toBe(keys[0])
  expect(dataRequests).toBe(1)
  const status = await (await page.request.get(`/api/uploads/${ids[0]}`)).json()
  expect(status.status).toBe('completed')
  const file = await (await page.request.get(`/api/files/${status.file_id}`)).json()
  expect(file.file.content_hash).toBe(createHash('sha256').update(payload).digest('hex'))
  expect(await page.evaluate(() => JSON.parse(localStorage.getItem('revaro.uploads.v1') || '[]'))).toEqual([])
})

test('multipart retry uses server acknowledgements and only sends the missing part', async ({ page }) => {
  test.setTimeout(90_000)
  await login(page)
  const name = `resume-parts-${Date.now()}.bin`
  const payload = Buffer.alloc(16 * 1024 * 1024 + 1, 97)
  const requests = new Map<number, number>()
  const legacyRequests: string[] = []
  const ids: string[] = []
  let failing = true
  page.on('request', request => {
    if (/\/api\/uploads\/[^/]+\/parts(?:\/|$)/.test(new URL(request.url()).pathname)) legacyRequests.push(request.url())
  })
  await page.route('**/api/uploads', async route => {
    const response = await route.fetch()
    ids.push((await response.json()).upload_id)
    await route.fulfill({ response })
  })
  await page.route('**/api/uploads/*/data/*', async route => {
    const part = Number(new URL(route.request().url()).pathname.split('/').pop())
    requests.set(part, (requests.get(part) || 0) + 1)
    if (failing) {
      // The first part reached disk but its response was lost. The second did
      // not reach the server. A manual retry must reconcile these two cases.
      if (part === 1) expect((await route.fetch()).status()).toBe(204)
      await route.fulfill({ status: 422, json: { error: { message: '模拟分片失败' } } })
    } else await route.continue()
  })
  await page.getByLabel('选择文件上传', { exact: true }).setInputFiles({ name, mimeType: 'application/octet-stream', buffer: payload })
  await expect(row(page, name)).toContainText('上传失败')
  const pending = await (await page.request.get(`/api/uploads/${ids[0]}`)).json()
  expect(pending.parts.map((part: { part_number: number }) => part.part_number)).toEqual([1])
  failing = false
  await row(page, name).getByRole('button', { name: '重试上传', exact: true }).click()
  await expect(overlay(page)).toHaveCount(0)
  expect(ids).toHaveLength(1)
  expect(requests.get(1)).toBe(1)
  expect(requests.get(2)).toBe(2)
  expect(legacyRequests).toEqual([])
  const completed = await (await page.request.get(`/api/uploads/${ids[0]}`)).json()
  expect(completed.status).toBe('completed')
  const file = await (await page.request.get(`/api/files/${completed.file_id}`)).json()
  expect(file.file.content_hash).toBe(createHash('sha256').update(payload).digest('hex'))
})

test('uploads appear automatically in a bounded scrolling overlay and disappear as each file finishes', async ({ page }) => {
  test.setTimeout(90_000)
  const obsoleteRequests: string[] = []
  const errors: string[] = []
  page.on('pageerror', error => errors.push(error.message))
  page.on('request', request => { if (/\/api\/(tasks|events)(\/|$)/.test(new URL(request.url()).pathname)) obsoleteRequests.push(request.url()) })
  await login(page)
  await expect(overlay(page)).toHaveCount(0)
  await expect(page.getByLabel('打开任务通知', { exact: true })).toHaveCount(0)
  for (const path of ['/api/tasks', '/api/tasks/old', '/api/events']) expect((await page.request.get(path)).status()).toBe(404)
  for (const [method, path] of [['POST', '/api/tasks/old/cancel'], ['POST', '/api/tasks/old/retry'], ['DELETE', '/api/tasks/old']]) {
    expect((await page.request.fetch(path, { method, headers: { Origin: new URL(page.url()).origin } })).status()).toBe(404)
  }
  expect(await (await page.request.get('/api/system/status')).json()).not.toHaveProperty('active_tasks')
  const ids = new Map<string, string>()
  const waiting = new Map<string, () => void>()
  let releaseAll = false
  await page.route('**/api/uploads', async route => {
    const response = await route.fetch()
    ids.set(route.request().postDataJSON().name, (await response.json()).upload_id)
    await route.fulfill({ response })
  })
  await page.route('**/api/uploads/*/data', async route => {
    const id = new URL(route.request().url()).pathname.split('/')[3]
    if (!releaseAll) await new Promise<void>(resolve => waiting.set(id, resolve))
    await route.continue().catch(() => {})
  })
  const prefix = `floating-${Date.now()}`
  const names = Array.from({ length: 20 }, (_, index) => `${prefix}-${String(index).padStart(2, '0')}-很长的上传文件名称.txt`)
  await page.getByLabel('选择文件上传', { exact: true }).setInputFiles(names.map(name => ({ name, mimeType: 'text/plain', buffer: Buffer.alloc(4096, 65) })))
  await expect(overlay(page)).toBeVisible()
  await expect(page.locator('.upload-progress-item')).toHaveCount(20)
  await expect.poll(() => waiting.size).toBe(3)
  await expect(row(page, names[0])).toContainText('0%')
  await expect(row(page, names[19])).toContainText('排队中')
  const list = page.getByLabel('上传文件进度，可上下滚动', { exact: true })
  for (const { width, height } of [
    { width: 1920, height: 1080 }, { width: 1280, height: 900 }, { width: 1024, height: 768 },
    { width: 851, height: 900 }, { width: 850, height: 900 }, { width: 768, height: 1024 },
    { width: 390, height: 844 }, { width: 320, height: 568 }, { width: 844, height: 390 },
    { width: 320, height: 280 }, { width: 1280, height: 150 },
  ]) {
    await page.setViewportSize({ width, height })
    const bounds = (await overlay(page).boundingBox())!
    expect(bounds.x).toBeGreaterThanOrEqual(0)
    expect(bounds.x + bounds.width).toBeLessThanOrEqual(width)
    expect(bounds.y).toBeGreaterThanOrEqual(0)
    expect(bounds.y + bounds.height).toBeLessThanOrEqual(height)
    expect(bounds.width).toBeLessThanOrEqual(380)
    expect(bounds.height).toBeLessThanOrEqual(360)
    expect(await page.evaluate(() => document.documentElement.scrollWidth)).toBe(width)
    const dimensions = await list.evaluate(element => ({ scroll: element.scrollHeight, client: element.clientHeight, width: element.scrollWidth, clientWidth: element.clientWidth }))
    expect(dimensions.scroll).toBeGreaterThan(dimensions.client)
    expect(dimensions.width).toBe(dimensions.clientWidth)
  }
  await page.setViewportSize({ width: 390, height: 844 })
  await list.hover()
  await page.mouse.wheel(0, 600)
  await expect.poll(() => list.evaluate(element => element.scrollTop)).toBeGreaterThan(0)
  await page.mouse.wheel(0, -1000)
  await expect.poll(() => list.evaluate(element => element.scrollTop)).toBe(0)
  await navigate(page, '书籍')
  await expect(overlay(page)).toBeVisible()
  await expect(page.locator('.upload-progress-item')).toHaveCount(20)
  const firstId = ids.get(names[0])!
  waiting.get(firstId)!()
  await expect(row(page, names[0])).toHaveCount(0)
  await expect(page.locator('.upload-progress-item')).toHaveCount(19)
  await expect(page.locator('.upload-progress-item').first()).toContainText(names[1])
  releaseAll = true
  for (const release of waiting.values()) release()
  await expect(overlay(page)).toHaveCount(0)
  expect(await page.evaluate(() => JSON.parse(localStorage.getItem('revaro.uploads.v1') || '[]'))).toEqual([])
  await page.reload()
  await expect(overlay(page)).toHaveCount(0)
  expect(obsoleteRequests).toEqual([])
  expect(errors).toEqual([])
})

test('failed uploads remain with retry, processing progress and cancellation reuse upload sessions', async ({ page }) => {
  await login(page)
  await page.setViewportSize({ width: 320, height: 568 })
  const name = `retry-${Date.now()}.txt`
  let fail = true
  let hold = true
  let release!: () => void
  const ids: string[] = []
  await page.route('**/api/uploads', async route => {
    const response = await route.fetch()
    ids.push((await response.json()).upload_id)
    await route.fulfill({ response })
  })
  await page.route('**/api/uploads/*/data', async route => {
    if (fail) await route.fulfill({ status: 422, json: { error: { message: '上传失败，请重试' } } })
    else await route.continue()
  })
  await page.route('**/api/uploads/*/complete', async route => {
    if (hold) await new Promise<void>(resolve => { release = resolve })
    await route.continue().catch(() => {})
  })
  await page.getByLabel('选择文件上传', { exact: true }).setInputFiles({ name, mimeType: 'text/plain', buffer: Buffer.from('保留上传逻辑') })
  await expect(row(page, name)).toContainText('上传失败')
  await expect(row(page, name).getByRole('button', { name: '重试上传', exact: true })).toBeVisible()
  expect(await page.locator('.upload-progress-list').evaluate(element => element.scrollWidth === element.clientWidth)).toBe(true)
  const failedId = ids[0]
  fail = false
  await row(page, name).getByRole('button', { name: '重试上传', exact: true }).click()
  await expect(row(page, name)).toContainText('处理中')
  await expect(row(page, name).getByRole('progressbar')).toHaveAttribute('value', '99')
  expect((await (await page.request.get(`/api/uploads/${failedId}`)).json()).status).toBe('pending')
  expect(ids).toHaveLength(1)
  hold = false
  release()
  await expect(overlay(page)).toHaveCount(0)
  const complete = await page.request.get(`/api/uploads/${ids[0]}`)
  expect((await complete.json()).status).toBe('completed')
  const cancelName = `cancel-${Date.now()}.txt`
  hold = true
  await page.getByLabel('选择文件上传', { exact: true }).setInputFiles({ name: cancelName, mimeType: 'text/plain', buffer: Buffer.from('取消上传') })
  await expect(row(page, cancelName)).toContainText('处理中')
  await row(page, cancelName).getByRole('button', { name: '取消上传', exact: true }).click()
  await expect(overlay(page)).toHaveCount(0)
  await expect.poll(async () => (await page.request.get(`/api/uploads/${ids[1]}`)).status()).toBe(404)
  hold = false
  release()
  await page.reload()
  await expect(overlay(page)).toHaveCount(0)
  expect(await page.evaluate(() => JSON.parse(localStorage.getItem('revaro.uploads.v1') || '[]'))).toEqual([])
})

test('failed rows stay only in the current browser session and refresh does not reopen an upload history', async ({ page }) => {
  await login(page)
  await page.route('**/api/uploads/*/data', route => route.fulfill({ status: 422, json: { error: { message: '上传失败' } } }))
  const name = `temporary-failure-${Date.now()}.txt`
  await page.getByLabel('选择文件上传', { exact: true }).setInputFiles({ name, mimeType: 'text/plain', buffer: Buffer.from('temporary') })
  await expect(row(page, name)).toContainText('上传失败')
  await page.getByRole('navigation', { name: '主导航', exact: true }).getByRole('link', { name: '图片', exact: true }).click()
  await expect(row(page, name)).toContainText('上传失败')
  await page.reload()
  await expect(page.getByLabel('更多操作', { exact: true })).toBeVisible()
  await expect(overlay(page)).toHaveCount(0)
  await expect(page.locator('.task-center, .task-panel')).toHaveCount(0)
})

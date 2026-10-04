import { expect, test as base, type Page } from '@playwright/test'
import { readFileSync } from 'node:fs'
import { login, navigate } from './helpers'

const test = base.extend<{ contentRoot: { id: string, prefix: string } }>({
  contentRoot: async ({ page }, use) => {
    await login(page)
    const prefix = `regression-${Date.now()}-${Math.random().toString(36).slice(2)}`
    const origin = new URL(page.url()).origin
    const response = await page.request.post('/api/directories', {
      headers: { origin }, data: { name: prefix, parent_id: '00000000-0000-0000-0000-000000000000' },
    })
    expect(response.status()).toBe(201)
    const { id } = await response.json()
    try { await use({ id, prefix }) }
    finally {
      if (await page.getByRole('button', { name: '停止音乐', exact: true }).count()) {
        await page.getByRole('button', { name: '停止音乐', exact: true }).click()
      }
      await page.request.delete(`/api/files/${id}`, { headers: { origin } })
      await page.request.delete(`/api/trash/${id}`, { headers: { origin } })
    }
  },
})

function wav() {
  const data = Buffer.alloc(44 + 16000 * 90)
  data.write('RIFF'); data.writeUInt32LE(data.length - 8, 4); data.write('WAVEfmt ', 8)
  data.writeUInt32LE(16, 16); data.writeUInt16LE(1, 20); data.writeUInt16LE(1, 22)
  data.writeUInt32LE(8000, 24); data.writeUInt32LE(16000, 28)
  data.writeUInt16LE(2, 32); data.writeUInt16LE(16, 34)
  data.write('data', 36); data.writeUInt32LE(data.length - 44, 40)
  return data
}

async function upload(page: Page, parent_id: string, name: string, mime_type: string, data: Buffer) {
  const headers = { origin: new URL(page.url()).origin }
  const created = await page.request.post('/api/uploads', { headers, data: { parent_id, name, mime_type, size: data.length } })
  expect(created.status()).toBe(201)
  const session = await created.json()
  const sent = await page.request.put(`/api/uploads/${session.upload_id}/data`, { headers, data })
  expect(sent.ok()).toBeTruthy()
  const completed = await page.request.post(`/api/uploads/${session.upload_id}/complete`, { headers, data: { parts: [] } })
  expect(completed.ok()).toBeTruthy()
  return completed.json()
}

async function savePosition(page: Page, id: string, position: number) {
  const response = await page.request.put(`/api/files/${id}/media/progress`, {
    headers: { origin: new URL(page.url()).origin }, data: { position, duration: 90 },
  })
  expect(response.ok()).toBeTruthy()
}

function deferred() {
  let resolve!: () => void
  const promise = new Promise<void>(done => { resolve = done })
  return { promise, resolve }
}

test('music restores saved progress after delayed metadata and reload without erasing it', async ({ page, contentRoot }) => {
  const errors: string[] = []
  page.on('pageerror', error => errors.push(error.message))
  const song = await upload(page, contentRoot.id, `${contentRoot.prefix}.wav`, 'audio/wav', wav())
  await savePosition(page, song.id, 60)
  const metadata = deferred()
  const progressRead = deferred()
  const progressWrites: number[] = []
  page.on('request', request => {
    if (request.url().endsWith(`/api/files/${song.id}/media/progress`)) {
      if (request.method() === 'GET') progressRead.resolve()
      else if (request.method() === 'PUT') progressWrites.push(request.postDataJSON().position)
    }
  })
  await page.route(`**/api/files/${song.id}/preview`, async route => {
    await metadata.promise
    await route.continue()
  })
  try {
    await navigate(page, '音乐')
    await page.getByRole('button', { name: `打开 ${song.name}`, exact: true }).click()
    await progressRead.promise
    await expect(page.getByLabel('音乐播放进度', { exact: true })).toBeDisabled()
    expect(progressWrites).toEqual([])
    expect((await (await page.request.get(`/api/files/${song.id}/media/progress`)).json()).position).toBe(60)
    metadata.resolve()
    await expect(page.getByLabel('音乐播放进度', { exact: true })).toBeEnabled()
    await expect.poll(() => page.locator('audio').evaluate(audio => audio.currentTime)).toBeGreaterThanOrEqual(60)
    expect(await page.locator('audio').evaluate(audio => audio.currentTime)).toBeLessThan(65)
    await page.getByRole('button', { name: '暂停音乐', exact: true }).click()
    await expect.poll(async () => (await (await page.request.get(`/api/files/${song.id}/media/progress`)).json()).position).toBeGreaterThanOrEqual(60)
    await page.getByRole('button', { name: '停止音乐', exact: true }).click()
    await page.reload()
    await page.getByRole('button', { name: `打开 ${song.name}`, exact: true }).click()
    await expect(page.getByLabel('音乐播放进度', { exact: true })).toBeEnabled()
    expect(await page.locator('audio').evaluate(audio => audio.currentTime)).toBeGreaterThanOrEqual(60)
    expect(await page.locator('audio').evaluate(audio => audio.currentTime)).toBeLessThan(65)
    expect(progressWrites.every(position => position >= 60)).toBeTruthy()
    // Single-track repeat must restart instead of restoring its end position.
    await page.getByRole('button', { name: '循环模式', exact: true }).click()
    await page.getByRole('button', { name: '循环模式', exact: true }).click()
    const restarted = page.waitForEvent('request', { predicate: request => request.url().endsWith(`/api/files/${song.id}/preview`) })
    await page.locator('audio').evaluate(audio => { audio.currentTime = audio.duration - 0.1 })
    await restarted
    await expect(page.getByLabel('音乐播放进度', { exact: true })).toBeEnabled()
    await expect.poll(() => page.locator('audio').evaluate(audio => audio.paused)).toBe(false)
    expect(await page.locator('audio').evaluate(audio => audio.currentTime)).toBeLessThan(5)
    expect(errors).toEqual([])
  } finally { metadata.resolve() }
})

test('late progress responses cannot seek another song or restart stopped music', async ({ page, contentRoot }) => {
  const errors: string[] = []
  page.on('pageerror', error => errors.push(error.message))
  const first = await upload(page, contentRoot.id, `${contentRoot.prefix}-a.wav`, 'audio/wav', wav())
  const second = await upload(page, contentRoot.id, `${contentRoot.prefix}-b.wav`, 'audio/wav', wav())
  await savePosition(page, first.id, 60)
  await savePosition(page, second.id, 30)
  let gate = deferred()
  let intercepted = deferred()
  await page.route(`**/api/files/${first.id}/media/progress`, async route => {
    if (route.request().method() !== 'GET') return route.continue()
    const release = gate.promise
    const response = await route.fetch()
    intercepted.resolve()
    await release
    await route.fulfill({ response })
  })
  try {
    await navigate(page, '音乐')
    await page.getByRole('button', { name: `打开 ${first.name}`, exact: true }).click()
    await intercepted.promise
    await expect.poll(() => page.locator('audio').evaluate(audio => audio.readyState)).toBeGreaterThanOrEqual(1)
    expect(await page.locator('audio').evaluate(audio => audio.paused)).toBe(true)
    expect((await (await page.request.get(`/api/files/${first.id}/media/progress`)).json()).position).toBe(60)
    await page.getByRole('button', { name: `打开 ${second.name}`, exact: true }).click()
    await expect(page.getByLabel('音乐播放进度', { exact: true })).toBeEnabled()
    gate.resolve()
    await page.waitForTimeout(250)
    expect(await page.locator('audio').evaluate(audio => audio.currentSrc)).toContain(`/api/files/${second.id}/preview`)
    expect(await page.locator('audio').evaluate(audio => audio.currentTime)).toBeGreaterThanOrEqual(30)
    expect(await page.locator('audio').evaluate(audio => audio.currentTime)).toBeLessThan(35)
    gate = deferred(); intercepted = deferred()
    await page.getByRole('button', { name: `打开 ${first.name}`, exact: true }).click()
    await intercepted.promise
    await page.getByRole('button', { name: '停止音乐', exact: true }).click()
    gate.resolve()
    await page.waitForTimeout(250)
    await expect(page.locator('.music-dock')).toHaveCount(0)
    expect(await page.locator('audio').evaluate(audio => audio.paused)).toBe(true)
    expect((await (await page.request.get(`/api/files/${first.id}/media/progress`)).json()).position).toBe(60)
    expect(errors).toEqual([])
  } finally { gate.resolve() }
})

test('home shows newly added videos even when an older video was opened', async ({ page, contentRoot }) => {
  const video = readFileSync(new URL('./fixtures/preview.webm', import.meta.url))
  const old = await upload(page, contentRoot.id, `${contentRoot.prefix}-old.webm`, 'video/webm', video)
  const opened = await page.request.patch(`/api/library/items/${old.id}`, {
    headers: { origin: new URL(page.url()).origin }, data: { opened: true },
  })
  expect(opened.ok()).toBeTruthy()
  const newest = await upload(page, contentRoot.id, `${contentRoot.prefix}-new.webm`, 'video/webm', video)
  await navigate(page, '首页')
  const section = page.locator('.home-section').filter({ has: page.getByRole('heading', { name: '最近添加的视频', exact: true }) })
  await expect(section.getByRole('button', { name: `打开 ${newest.name}`, exact: true })).toBeVisible()
  await expect(section.getByRole('button', { name: `打开 ${old.name}`, exact: true })).toBeVisible()
  await expect(section.locator('.home-item').first()).toHaveAttribute('aria-label', `打开 ${newest.name}`)
})

test('trash audio preview keeps saved and local resume positions, defaults and completion behavior', async ({ page, contentRoot }) => {
  const errors: string[] = []
  page.on('pageerror', error => errors.push(error.message))
  const song = await upload(page, contentRoot.id, `${contentRoot.prefix}-preview.wav`, 'audio/wav', wav())
  await savePosition(page, song.id, 60)
  let saved = await (await page.request.get(`/api/files/${song.id}/media/progress`)).json()
  const headers = { origin: new URL(page.url()).origin }
  // Regular file clicks use the music dock; trash audio opens the full preview.
  // Supply progress explicitly because the live progress API excludes trash.
  await page.route(`**/api/files/${song.id}/media/progress`, route => {
    if (route.request().method() === 'GET') return route.fulfill({ json: saved })
    return route.continue()
  })
  expect((await page.request.delete(`/api/files/${song.id}`, { headers })).ok()).toBeTruthy()
  try {
    await navigate(page, '回收站')
    const card = page.locator('.file-card').filter({ hasText: song.name })
    const audio = page.locator('.chapter-audio-player audio')
    await card.click()
    await expect.poll(() => audio.evaluate(element => element.currentTime)).toBeGreaterThanOrEqual(60)
    expect(await audio.evaluate(element => element.currentTime)).toBeLessThan(65)
    expect(await audio.evaluate(element => element.volume)).toBe(0.85)
    await page.locator('.preview-close').click()
    await expect(audio).toHaveCount(0)

    // A successful zero server position still uses this player's local fallback.
    saved = { position: 0, duration: 90 }
    await page.evaluate(id => localStorage.setItem(`revaro-audio-position:${id}`, '30'), song.id)
    await card.click()
    await expect.poll(() => audio.evaluate(element => element.currentTime)).toBeGreaterThanOrEqual(30)
    expect(await audio.evaluate(element => element.currentTime)).toBeLessThan(35)
    await page.locator('.preview-close').click()
    await expect(audio).toHaveCount(0)

    await page.evaluate(id => localStorage.setItem(`revaro-audio-position:${id}`, '89'), song.id)
    await card.click()
    await expect.poll(() => audio.evaluate(element => element.readyState)).toBeGreaterThanOrEqual(1)
    await expect.poll(() => audio.evaluate(element => element.paused)).toBe(false)
    expect(await audio.evaluate(element => element.currentTime)).toBeLessThan(5)
    await page.locator('.preview-close').click()
    expect(errors).toEqual([])
  } finally {
    await page.request.delete(`/api/trash/${song.id}`, { headers })
  }
})

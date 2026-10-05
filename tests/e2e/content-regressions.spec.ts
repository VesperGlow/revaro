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

const png = Buffer.from('iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mNk+A8AAQUBAScY42YAAAAASUVORK5CYII=', 'base64')
const videoFixture = () => readFileSync(new URL('./fixtures/preview.webm', import.meta.url))
const bookText = () => Buffer.from('第一章\n\n这是用于验证导航和内容卡片的书籍正文。\n\n'.repeat(80))

function homeSection(page: Page, title: string) {
  return page.locator('.home-section').filter({ has: page.getByRole('heading', { name: title, exact: true }) })
}

async function markOpened(page: Page, id: string) {
  const response = await page.request.patch(`/api/library/items/${id}`, {
    headers: { origin: new URL(page.url()).origin }, data: { opened: true },
  })
  expect(response.ok()).toBeTruthy()
}

test.describe('touch content captions', () => {
  test.use({ hasTouch: true, viewport: { width: 390, height: 844 } })

  test('book and home titles are readable before tapping, including video captions', async ({ page, contentRoot }) => {
    const book = await upload(page, contentRoot.id, `${contentRoot.prefix}-书籍.txt`, 'text/plain', bookText())
    const song = await upload(page, contentRoot.id, `${contentRoot.prefix}-歌曲.wav`, 'audio/wav', wav())
    const photo = await upload(page, contentRoot.id, `${contentRoot.prefix}-图片.png`, 'image/png', png)
    const video = await upload(page, contentRoot.id, `${contentRoot.prefix}-视频.webm`, 'video/webm', videoFixture())
    await markOpened(page, book.id)
    await markOpened(page, song.id)
    expect(await page.evaluate(() => matchMedia('(hover: none)').matches)).toBe(true)
    for (const [destination, file] of [['书籍', book], ['音乐', song], ['图片', photo], ['视频', video]] as const) {
      await navigate(page, destination)
      const caption = page.getByRole('button', { name: `打开 ${file.name}`, exact: true }).locator('.card-info')
      await expect(caption).toHaveCSS('opacity', '1')
      await expect(caption.locator('strong')).toHaveCSS('white-space', destination === '音乐' ? 'nowrap' : 'normal')
    }
    await navigate(page, '首页')
    for (const file of [book, song, photo, video]) {
      const card = page.getByRole('button', { name: `打开 ${file.name}`, exact: true })
      await expect(card.locator('.card-info')).toHaveCSS('opacity', '1')
    }
    const videoCard = page.getByRole('button', { name: `打开 ${video.name}`, exact: true })
    const duration = (await videoCard.locator('.video-duration').boundingBox())!
    const caption = (await videoCard.locator('.card-info strong').boundingBox())!
    expect(duration.y + duration.height).toBeLessThanOrEqual(caption.y)
    await expect(page.locator('#reader-view,.media-preview')).toHaveCount(0)
  })
})

test('home music always queues its visible songs after refresh and a filtered library visit', async ({ page, contentRoot }) => {
  const first = await upload(page, contentRoot.id, `${contentRoot.prefix}-a.wav`, 'audio/wav', wav())
  const second = await upload(page, contentRoot.id, `${contentRoot.prefix}-b.wav`, 'audio/wav', wav())
  await markOpened(page, first.id)
  await markOpened(page, second.id)
  const section = homeSection(page, '最近播放与收藏')
  for (const visit of ['fresh', 'filtered', 'reload']) {
    if (visit === 'filtered') {
      await navigate(page, '音乐')
      await page.getByLabel('打开搜索', { exact: true }).click()
      const search = page.locator('.topbar input[type=search]')
      await search.fill(first.name)
      await search.press('Enter')
      await expect(page.locator('.library-card')).toHaveCount(1)
      await search.press('Escape')
    }
    await navigate(page, '首页')
    if (visit === 'reload') await page.reload()
    await expect(section).toHaveAttribute('aria-busy', 'false')
    const visibleNames = await section.locator('.card-info strong').allTextContents()
    expect(visibleNames).toContain(first.name)
    expect(visibleNames).toContain(second.name)
    await page.getByRole('button', { name: `打开 ${first.name}`, exact: true }).click()
    await expect(page.locator('audio')).toHaveJSProperty('paused', false)
    await page.getByRole('button', { name: '播放队列', exact: true }).click()
    await expect(page.locator('.music-queue > button strong')).toHaveText(visibleNames.map(name => name.replace(/\.[^.]+$/, '')))
    await page.getByRole('button', { name: '关闭播放队列', exact: true }).click()
    await page.getByRole('button', { name: '停止音乐', exact: true }).click()
  }
})

test('video playback pauses background music from home, video library and files', async ({ page, contentRoot }) => {
  const song = await upload(page, contentRoot.id, `${contentRoot.prefix}.wav`, 'audio/wav', wav())
  const video = await upload(page, contentRoot.id, `${contentRoot.prefix}.webm`, 'video/webm', videoFixture())
  for (const destination of ['首页', '视频', '文件']) {
    await navigate(page, '音乐')
    await page.getByRole('button', { name: `打开 ${song.name}`, exact: true }).click()
    await expect(page.locator('audio')).toHaveJSProperty('paused', false)
    await navigate(page, destination)
    if (destination === '文件') {
      await page.locator('.file-card').filter({ hasText: contentRoot.prefix }).click()
      await page.locator('.file-card').filter({ hasText: video.name }).click()
    } else {
      await page.getByRole('button', { name: `打开 ${video.name}`, exact: true }).click()
    }
    await expect(page.locator('video')).toHaveJSProperty('paused', false)
    await expect(page.locator('audio')).toHaveJSProperty('paused', true)
    await page.keyboard.press('Escape')
    await expect(page.locator('video')).toHaveCount(0)
    await page.getByRole('button', { name: '停止音乐', exact: true }).click()
  }
  // Opening a video must also cancel music that is still waiting for metadata.
  const metadata = deferred(), intercepted = deferred()
  await page.route(`**/api/files/${song.id}/preview`, async route => {
    intercepted.resolve()
    await metadata.promise
    await route.continue()
  })
  try {
    await navigate(page, '音乐')
    await page.getByRole('button', { name: `打开 ${song.name}`, exact: true }).click()
    await intercepted.promise
    await expect(page.locator('audio')).toHaveJSProperty('readyState', 0)
    await navigate(page, '视频')
    await page.getByRole('button', { name: `打开 ${video.name}`, exact: true }).click()
    await expect(page.locator('video')).toHaveJSProperty('paused', false)
    metadata.resolve()
    await expect.poll(() => page.locator('audio').evaluate(audio => audio.readyState)).toBeGreaterThanOrEqual(1)
    await expect(page.getByLabel('音乐播放进度', { exact: true })).toBeEnabled()
    await expect(page.locator('audio')).toHaveJSProperty('paused', true)
    await page.keyboard.press('Escape')
    await expect(page.locator('video')).toHaveCount(0)
    await page.getByRole('button', { name: '播放音乐', exact: true }).click()
    await expect(page.locator('audio')).toHaveJSProperty('paused', false)
  } finally {
    metadata.resolve()
    if (await page.locator('video').count()) await page.keyboard.press('Escape')
  }
})

for (const entry of ['deep link', 'history forward']) {
  test(`late reader responses from ${entry} cannot reopen books, redirect or sign out`, async ({ page, contentRoot }) => {
    const errors: string[] = []
    page.on('pageerror', error => errors.push(error.message))
    const book = await upload(page, contentRoot.id, `${contentRoot.prefix}.txt`, 'text/plain', bookText())
    const origin = new URL(page.url()).origin
    const endpoint = `**/api/files/${book.id}`
    for (const status of [200, 404, 401]) {
      if (entry === 'history forward') {
        await navigate(page, '书籍')
        await page.getByRole('button', { name: `打开 ${book.name}`, exact: true }).click()
        await expect(page.locator('#reader-view')).toBeVisible()
        await page.locator('#reader-back').click()
        await expect(page).toHaveURL(`${origin}/library`)
      }
      const gate = deferred(), intercepted = deferred(), fulfilled = deferred()
      await page.route(endpoint, async route => {
        const response = status === 200 ? await route.fetch() : null
        intercepted.resolve()
        await gate.promise
        if (response) await route.fulfill({ response })
        else await route.fulfill({ status, json: { error: { status, message: '模拟迟到的阅读请求失败' } } })
        fulfilled.resolve()
      })
      try {
        if (entry === 'deep link') await page.goto(`${origin}/read/${book.id}`)
        else await page.evaluate(() => history.forward())
        await intercepted.promise
        await navigate(page, '音乐')
        await expect(page).toHaveURL(`${origin}/music`)
        gate.resolve()
        await fulfilled.promise
        await page.waitForTimeout(250)
        await expect(page).toHaveURL(`${origin}/music`)
        await expect(page.locator('#reader-view')).toHaveCount(0)
        await expect(page.getByRole('navigation', { name: '主导航', exact: true })).toBeVisible()
        await expect(page.locator('.library-error')).toHaveCount(0)
      } finally {
        gate.resolve()
        await page.unroute(endpoint)
      }
    }
    expect(errors).toEqual([])
  })
}

for (const [kind, title, retryLabel] of [['book', '继续阅读', '重试书籍'], ['video', '最近添加的视频', '重试视频']]) {
  test(`home remains usable while ${kind} is slow or fails, and retries only that section`, async ({ page, contentRoot }) => {
    const files = {
      book: await upload(page, contentRoot.id, `${contentRoot.prefix}.txt`, 'text/plain', bookText()),
      audio: await upload(page, contentRoot.id, `${contentRoot.prefix}.wav`, 'audio/wav', wav()),
      image: await upload(page, contentRoot.id, `${contentRoot.prefix}.png`, 'image/png', png),
      video: await upload(page, contentRoot.id, `${contentRoot.prefix}.webm`, 'video/webm', videoFixture()),
    }
    await markOpened(page, files.book.id)
    await markOpened(page, files.audio.id)
    const gate = deferred(), intercepted = deferred()
    const requests = new Map<string, number>()
    let fail = true
    await page.route('**/api/library/items?*', async route => {
      const requestedKind = new URL(route.request().url()).searchParams.get('kind') || ''
      requests.set(requestedKind, (requests.get(requestedKind) || 0) + 1)
      if (requestedKind === kind && fail) {
        intercepted.resolve()
        await gate.promise
        await route.fulfill({ status: 500, json: { error: { status: 500, message: '模拟栏目加载失败' } } })
      } else await route.continue()
    })
    try {
      await navigate(page, '首页')
      await intercepted.promise
      const section = homeSection(page, title)
      await expect(section.locator('.home-loading')).toBeVisible()
      await expect(section.locator('.home-empty')).toHaveCount(0)
      for (const [otherKind, file] of Object.entries(files)) {
        if (otherKind !== kind) await expect(page.getByRole('button', { name: `打开 ${file.name}`, exact: true })).toBeVisible()
      }
      gate.resolve()
      await expect(section.getByRole('alert')).toContainText('模拟栏目加载失败')
      await expect(page.locator('.home-section')).toHaveCount(4)
      const beforeRetry = new Map(requests)
      fail = false
      await section.getByRole('button', { name: retryLabel, exact: true }).click()
      await expect(section.getByRole('button', { name: `打开 ${files[kind as 'book' | 'video'].name}`, exact: true })).toBeVisible()
      await expect(section.getByRole('alert')).toHaveCount(0)
      expect(requests.get(kind)).toBe(beforeRetry.get(kind)! + 1)
      for (const otherKind of Object.keys(files)) {
        if (otherKind !== kind) expect(requests.get(otherKind)).toBe(beforeRetry.get(otherKind))
      }
    } finally { gate.resolve() }
  })
}

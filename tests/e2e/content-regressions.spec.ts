import { expect, test as base, type Page } from '@playwright/test'
import { readFileSync } from 'node:fs'
import { openMusicPlayer, selectMusicMode, pauseMusic, login, navigate, uploadFixture as upload } from './helpers'

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
      await pauseMusic(page)
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
  await page.context().route(`**/api/files/${song.id}/preview`, async route => {
    if (!route.request().serviceWorker()) return route.continue()
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
    await expect.poll(() => page.locator('audio:not([aria-hidden="true"])').evaluate(audio => audio.currentTime)).toBeGreaterThanOrEqual(60)
    expect(await page.locator('audio:not([aria-hidden="true"])').evaluate(audio => audio.currentTime)).toBeLessThan(65)
    await openMusicPlayer(page)
    await page.getByRole('button', { name: '暂停音乐', exact: true }).click()
    await expect.poll(async () => (await (await page.request.get(`/api/files/${song.id}/media/progress`)).json()).position).toBeGreaterThanOrEqual(60)
    await pauseMusic(page)
    await page.reload()
    await page.getByRole('button', { name: `打开 ${song.name}`, exact: true }).click()
    await openMusicPlayer(page)
    await expect(page.getByLabel('音乐播放进度', { exact: true })).toBeEnabled()
    expect(await page.locator('audio:not([aria-hidden="true"])').evaluate(audio => audio.currentTime)).toBeGreaterThanOrEqual(60)
    expect(await page.locator('audio:not([aria-hidden="true"])').evaluate(audio => audio.currentTime)).toBeLessThan(65)
    expect(progressWrites.every(position => position >= 60)).toBeTruthy()
    // Single-track repeat must restart instead of restoring its end position.
    await selectMusicMode(page, 'repeat-one')
    const restarted = page.waitForEvent('request', { predicate: request => request.url().endsWith(`/api/files/${song.id}/preview`) })
    await page.locator('audio:not([aria-hidden="true"])').evaluate(audio => { audio.currentTime = audio.duration - 0.1 })
    await restarted
    await expect(page.getByLabel('音乐播放进度', { exact: true })).toBeEnabled()
    await expect.poll(() => page.locator('audio:not([aria-hidden="true"])').evaluate(audio => audio.paused)).toBe(false)
    expect(await page.locator('audio:not([aria-hidden="true"])').evaluate(audio => audio.currentTime)).toBeLessThan(5)
    expect(errors).toEqual([])
  } finally { metadata.resolve() }
})

test('late progress responses cannot seek another song or restart an ended session', async ({ page, contentRoot }) => {
  const errors: string[] = []
  page.on('pageerror', error => errors.push(error.message))
  const first = await upload(page, contentRoot.id, `${contentRoot.prefix}-a.wav`, 'audio/wav', wav())
  const second = await upload(page, contentRoot.id, `${contentRoot.prefix}-b.wav`, 'audio/wav', wav())
  await savePosition(page, first.id, 60)
  await savePosition(page, second.id, 30)
  let gate = deferred()
  let intercepted = deferred()
  await page.context().route(`**/api/files/${first.id}/media/progress`, async route => {
    if (!route.request().serviceWorker()) return route.continue()
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
    await expect.poll(() => page.locator('audio:not([aria-hidden="true"])').evaluate(audio => audio.readyState)).toBeGreaterThanOrEqual(1)
    expect(await page.locator('audio:not([aria-hidden="true"])').evaluate(audio => audio.paused)).toBe(true)
    expect((await (await page.request.get(`/api/files/${first.id}/media/progress`)).json()).position).toBe(60)
    await page.getByRole('button', { name: `打开 ${second.name}`, exact: true }).click()
    await expect(page.getByLabel('音乐播放进度', { exact: true })).toBeEnabled()
    gate.resolve()
    await page.waitForTimeout(250)
    expect(await page.locator('audio:not([aria-hidden="true"])').evaluate(audio => audio.currentSrc)).toContain(`/api/files/${second.id}/preview`)
    expect(await page.locator('audio:not([aria-hidden="true"])').evaluate(audio => audio.currentTime)).toBeGreaterThanOrEqual(30)
    expect(await page.locator('audio:not([aria-hidden="true"])').evaluate(audio => audio.currentTime)).toBeLessThan(35)
    gate = deferred(); intercepted = deferred()
    await page.getByRole('button', { name: `打开 ${first.name}`, exact: true }).click()
    await intercepted.promise
    await openMusicPlayer(page)
    await page.locator('.music-dock').getByLabel('播放器更多操作', { exact: true }).click()
    await page.locator('.music-dock').getByRole('button', { name: '结束播放', exact: true }).click()
    gate.resolve()
    await page.waitForTimeout(250)
    await expect(page.locator('.music-dock, .music-orb')).toHaveCount(0)
    await expect(page.locator('audio:not([aria-hidden="true"])')).toHaveJSProperty('paused', true)
    await expect(page.locator('audio:not([aria-hidden="true"])')).not.toHaveAttribute('src')
    expect((await (await page.request.get(`/api/files/${first.id}/media/progress`)).json()).position).toBe(60)
    expect(errors).toEqual([])
  } finally { gate.resolve() }
})

test('home mixes only opened content in opening order and directly opens every kind', async ({ page, contentRoot }) => {
  const errors: string[] = []
  page.on('pageerror', error => errors.push(error.message))
  const book = await upload(page, contentRoot.id, `${contentRoot.prefix}.txt`, 'text/plain', bookText())
  const song = await upload(page, contentRoot.id, `${contentRoot.prefix}.wav`, 'audio/wav', wav())
  const photo = await upload(page, contentRoot.id, `${contentRoot.prefix}.png`, 'image/png', png)
  const video = await upload(page, contentRoot.id, `${contentRoot.prefix}.webm`, 'video/webm', videoFixture())
  const unopened = await upload(page, contentRoot.id, `${contentRoot.prefix}-new.png`, 'image/png', png)
  const document = await upload(page, contentRoot.id, `${contentRoot.prefix}.md`, 'text/markdown', Buffer.from('notes'))
  await markOpened(page, book.id)
  await markOpened(page, song.id)
  await markOpened(page, video.id)
  await markOpened(page, document.id)
  await markOpened(page, contentRoot.id)
  // File-page previews must also enter home history.
  await navigate(page, '文件')
  await page.locator('.file-card').filter({ hasText: contentRoot.prefix }).click()
  await page.locator('.file-card').filter({ hasText: photo.name }).click()
  await expect(page.locator('.preview-modal img.preview-image')).toBeVisible()
  await page.keyboard.press('Escape')
  await navigate(page, '首页')
  const section = homeSection(page, '最近使用')
  await expect(section).toHaveAttribute('aria-busy', 'false')
  await expect(page.locator('.home-section')).toHaveCount(1)
  await expect(section.locator('.home-item').nth(0)).toHaveAttribute('aria-label', `打开 ${photo.name}`)
  await expect(section.locator('.home-item').nth(1)).toHaveAttribute('aria-label', `打开 ${video.name}`)
  await expect(section.locator('.home-item').nth(2)).toHaveAttribute('aria-label', `打开 ${song.name}`)
  await expect(section.locator('.home-item').nth(3)).toHaveAttribute('aria-label', `打开 ${book.name}`)
  for (const name of [unopened.name, document.name, contentRoot.prefix]) {
    await expect(section.getByRole('button', { name: `打开 ${name}`, exact: true })).toHaveCount(0)
  }
  // A newer background save must not reorder the explicit opening history.
  await savePosition(page, song.id, 5)
  await page.reload()
  await expect(section.locator('.home-item').first()).toHaveAttribute('aria-label', `打开 ${photo.name}`)
  for (const width of [850, 390, 320]) {
    await page.setViewportSize({ width, height: 900 })
    const columns = await section.locator('.home-content-row').evaluate(el => getComputedStyle(el).gridTemplateColumns.split(' ').length)
    expect(columns).toBe(2)
    expect(await page.evaluate(() => document.documentElement.scrollWidth)).toBe(width)
  }
  await section.getByRole('button', { name: `打开 ${song.name}`, exact: true }).click()
  await expect(page.locator('audio:not([aria-hidden="true"])')).toHaveJSProperty('paused', false)
  await expect(section.locator('.home-item').first()).toHaveAttribute('aria-label', `打开 ${song.name}`)
  await section.getByRole('button', { name: `打开 ${photo.name}`, exact: true }).click()
  await expect(page.locator('.preview-modal img.preview-image')).toBeVisible()
  await expect(page.locator('audio:not([aria-hidden="true"])')).toHaveJSProperty('paused', false)
  await page.keyboard.press('Escape')
  await section.getByRole('button', { name: `打开 ${book.name}`, exact: true }).click()
  await expect(page).toHaveURL(new RegExp(`/read/${book.id}$`))
  await expect(page.locator('#reader-view')).toBeVisible()
  await page.locator('#reader-back').click()
  await expect(page).toHaveURL(new RegExp('/$'))
  await section.getByRole('button', { name: `打开 ${video.name}`, exact: true }).click()
  await expect(page.locator('video')).toHaveJSProperty('paused', false)
  await expect(page.locator('audio:not([aria-hidden="true"])')).toHaveJSProperty('paused', true)
  await page.keyboard.press('Escape')
  await page.reload()
  await expect(section.locator('.home-item').first()).toHaveAttribute('aria-label', `打开 ${video.name}`)
  expect(errors).toEqual([])
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
  await page.context().route(`**/api/files/${song.id}/media/progress`, route => {
    if (!route.request().serviceWorker()) return route.continue()
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
    await markOpened(page, photo.id)
    await markOpened(page, video.id)
    expect(await page.evaluate(() => matchMedia('(hover: none)').matches)).toBe(true)
    for (const [destination, file] of [['书籍', book], ['音乐', song], ['图片', photo], ['视频', video]] as const) {
      await navigate(page, destination)
      const caption = page.getByRole('button', { name: `打开 ${file.name}`, exact: true }).locator('.card-info')
      await expect(caption).toHaveCSS('opacity', '1')
      await expect(caption.locator('strong')).toHaveCSS('white-space', 'normal')
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
  const section = homeSection(page, '最近使用')
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
    const visibleNames = await section.locator('.home-card:has(.audio-cover) .card-info strong').allTextContents()
    expect(visibleNames).toContain(first.name)
    expect(visibleNames).toContain(second.name)
    await page.getByRole('button', { name: `打开 ${visibleNames[0]}`, exact: true }).click()
    await expect(page.locator('audio:not([aria-hidden="true"])')).toHaveJSProperty('paused', false)
    await openMusicPlayer(page)
    // The restored player omits the queue menu. Verify its actual playback
    // order and boundary through the dock's next-track control instead.
    const title = page.locator('.dock-track strong')
    const next = page.getByRole('button', { name: '下一首', exact: true })
    for (const name of visibleNames) {
      await expect(title).toHaveText(name.replace(/\.[^.]+$/, ''))
      await next.click()
    }
    await expect(title).toHaveText(visibleNames.at(-1)!.replace(/\.[^.]+$/, ''))
    await expect(page.locator('audio[aria-hidden="true"]')).not.toHaveAttribute('src')
    await pauseMusic(page)
  }
})

test('video playback pauses background music from home, video library and files', async ({ page, contentRoot }) => {
  const song = await upload(page, contentRoot.id, `${contentRoot.prefix}.wav`, 'audio/wav', wav())
  const video = await upload(page, contentRoot.id, `${contentRoot.prefix}.webm`, 'video/webm', videoFixture())
  await markOpened(page, video.id)
  for (const destination of ['首页', '视频', '文件']) {
    await navigate(page, '音乐')
    await page.getByRole('button', { name: `打开 ${song.name}`, exact: true }).click()
    await expect(page.locator('audio:not([aria-hidden="true"])')).toHaveJSProperty('paused', false)
    await navigate(page, destination)
    if (destination === '文件') {
      await page.locator('.file-card').filter({ hasText: contentRoot.prefix }).click()
      await page.locator('.file-card').filter({ hasText: video.name }).click()
    } else {
      await page.getByRole('button', { name: `打开 ${video.name}`, exact: true }).click()
    }
    await expect(page.locator('video')).toHaveJSProperty('paused', false)
    await expect(page.locator('audio:not([aria-hidden="true"])')).toHaveJSProperty('paused', true)
    await page.keyboard.press('Escape')
    await expect(page.locator('video')).toHaveCount(0)
    await pauseMusic(page)
  }
  // Opening a video must also cancel music that is still waiting for metadata.
  const metadata = deferred(), intercepted = deferred()
  await page.context().route(`**/api/files/${song.id}/preview`, async route => {
    if (!route.request().serviceWorker()) return route.continue()
    intercepted.resolve()
    await metadata.promise
    await route.continue()
  })
  try {
    await navigate(page, '音乐')
    await page.getByRole('button', { name: `打开 ${song.name}`, exact: true }).click()
    await intercepted.promise
    await expect(page.locator('audio:not([aria-hidden="true"])')).toHaveJSProperty('readyState', 0)
    await navigate(page, '视频')
    await page.getByRole('button', { name: `打开 ${video.name}`, exact: true }).click()
    await expect(page.locator('video')).toHaveJSProperty('paused', false)
    metadata.resolve()
    await expect.poll(() => page.locator('audio:not([aria-hidden="true"])').evaluate(audio => audio.readyState)).toBeGreaterThanOrEqual(1)
    await expect(page.getByLabel('音乐播放进度', { exact: true })).toBeEnabled()
    await expect(page.locator('audio:not([aria-hidden="true"])')).toHaveJSProperty('paused', true)
    await page.keyboard.press('Escape')
    await expect(page.locator('video')).toHaveCount(0)
    await openMusicPlayer(page)
    await page.getByRole('button', { name: '播放音乐', exact: true }).click()
    await expect(page.locator('audio:not([aria-hidden="true"])')).toHaveJSProperty('paused', false)
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
      await page.context().route(endpoint, async route => {
        if (!route.request().serviceWorker()) return route.continue()
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
        await page.context().unroute(endpoint)
      }
    }
    expect(errors).toEqual([])
  })
}

test('home loads one mixed history, shows failures and retries without an unopened fallback', async ({ page, contentRoot }) => {
  const book = await upload(page, contentRoot.id, `${contentRoot.prefix}.txt`, 'text/plain', bookText())
  await markOpened(page, book.id)
  const gate = deferred(), intercepted = deferred()
  let requests = 0, fail = true
  await page.context().route('**/api/library/items?*', async route => {
    if (!route.request().serviceWorker()) return route.continue()
    const query = new URL(route.request().url()).searchParams
    if (query.get('opened_only') !== 'true') return route.continue()
    requests++
    expect(query.get('kind')).toBeNull()
    expect(query.get('recent')).toBe('true')
    if (fail) {
      intercepted.resolve()
      await gate.promise
      // A terminal error reaches the UI; transient 5xx retries are covered by file-transport.spec.ts.
      await route.fulfill({ status: 422, json: { error: { status: 422, message: '模拟最近使用加载失败' } } })
    } else await route.continue()
  })
  try {
    await navigate(page, '首页')
    await intercepted.promise
    const section = homeSection(page, '最近使用')
    await expect(section.locator('.home-loading')).toBeVisible()
    await expect(section.locator('.home-empty')).toHaveCount(0)
    gate.resolve()
    await expect(section.getByRole('alert')).toContainText('模拟最近使用加载失败')
    await expect(section.locator('.home-empty')).toHaveCount(0)
    expect(requests).toBe(1)
    fail = false
    await section.getByRole('button', { name: '重试最近使用', exact: true }).click()
    await expect(section.getByRole('button', { name: `打开 ${book.name}`, exact: true })).toBeVisible()
    await expect(section.getByRole('alert')).toHaveCount(0)
    expect(requests).toBe(2)
  } finally { gate.resolve() }
})

test('empty home guides users to files and never falls back to unopened content', async ({ page, contentRoot }) => {
  await upload(page, contentRoot.id, `${contentRoot.prefix}.png`, 'image/png', png)
  const requests: string[] = []
  await page.context().route('**/api/library/items?*', async route => {
    if (!route.request().serviceWorker()) return route.continue()
    const query = new URL(route.request().url()).searchParams
    requests.push(query.get('opened_only') || '')
    await route.fulfill({ json: { items: [], total: 0, offset: 0, limit: 60 } })
  })
  await navigate(page, '首页')
  await expect(page.getByText('还没有最近使用的内容', { exact: true })).toBeVisible()
  await expect(page.locator('.home-card')).toHaveCount(0)
  expect(requests).toEqual(['true'])
  await page.getByRole('button', { name: '前往文件', exact: true }).click()
  await expect(page).toHaveURL(new RegExp('/files$'))
  await expect(page.getByRole('navigation', { name: '当前路径', exact: true })).toBeVisible()
})

test('late library responses cannot replace home history with unopened content', async ({ page, contentRoot }) => {
  const book = await upload(page, contentRoot.id, `${contentRoot.prefix}.txt`, 'text/plain', bookText())
  const song = await upload(page, contentRoot.id, `${contentRoot.prefix}.wav`, 'audio/wav', wav())
  await markOpened(page, book.id)
  const gate = deferred(), intercepted = deferred(), fulfilled = deferred()
  await page.context().route('**/api/library/items?*', async route => {
    if (!route.request().serviceWorker()) return route.continue()
    if (new URL(route.request().url()).searchParams.get('kind') !== 'audio') return route.continue()
    const response = await route.fetch()
    intercepted.resolve()
    await gate.promise
    await route.fulfill({ response })
    fulfilled.resolve()
  })
  try {
    await navigate(page, '音乐')
    await intercepted.promise
    await navigate(page, '首页')
    const home = homeSection(page, '最近使用')
    await expect(home.getByRole('button', { name: `打开 ${book.name}`, exact: true })).toBeVisible()
    const names = await home.locator('.card-info strong').allTextContents()
    expect(names).not.toContain(song.name)
    gate.resolve()
    await fulfilled.promise
    await page.waitForTimeout(250)
    await expect(home.locator('.card-info strong')).toHaveText(names)
    await expect(home.getByRole('button', { name: `打开 ${song.name}`, exact: true })).toHaveCount(0)
  } finally { gate.resolve() }
})

test('home can load older opened content beyond the first page', async ({ page, contentRoot }) => {
  test.setTimeout(90_000)
  let oldest: { id: string, name: string } | undefined
  for (let index = 0; index < 61; index++) {
    const file = await upload(page, contentRoot.id, `${contentRoot.prefix}-${index}.png`, 'image/png', png)
    oldest ??= file
    await markOpened(page, file.id)
  }
  await navigate(page, '首页')
  const home = homeSection(page, '最近使用')
  await expect(home.locator('.home-card')).toHaveCount(60)
  await expect(home.getByRole('button', { name: `打开 ${oldest!.name}`, exact: true })).toHaveCount(0)
  await home.getByRole('button', { name: '加载更多', exact: true }).click()
  await expect(home.getByRole('button', { name: `打开 ${oldest!.name}`, exact: true })).toBeVisible()
  const ids = await home.locator('.home-card').evaluateAll(cards => cards.map(card => card.getAttribute('data-selection-ids')))
  expect(new Set(ids).size).toBe(ids.length)
})

import { expect, test, type Page } from '@playwright/test'
import { readFileSync } from 'node:fs'

// This is the reference media-ui suite with valid Rust Timestamp values. It
// intentionally keeps the old selectors and interaction sequence: parity is
// measured against the Vue page, not against a Rust-specific abstraction.

const ROOT = '00000000-0000-0000-0000-000000000000'
const STAMP = '2026-01-01T00:00:00Z'
const files = [
  { id: 'audio-1', name: '山间来信.m4a', mime_type: 'audio/mp4' },
  { id: 'video-1', name: '山间漫步.webm', mime_type: 'video/webm' },
  { id: 'image-1', name: '群山.png', mime_type: 'image/png' },
  { id: 'image-2', name: '远山.png', mime_type: 'image/png' },
].map(file => ({
  ...file,
  parent_id: ROOT,
  kind: 'file',
  size: 200000,
  status: 'ready',
  created_at: STAMP,
  updated_at: STAMP,
}))

const landscape = (portrait = false) => `<svg xmlns="http://www.w3.org/2000/svg" width="${portrait ? 900 : 1800}" height="${portrait ? 1400 : 1100}" viewBox="0 0 1800 1100"><rect width="1800" height="1100" fill="#c8d7d6"/><circle cx="1320" cy="300" r="110" fill="#eee7d3"/><path d="M0 710 460 200 990 870 1500 400 1800 650V1100H0Z" fill="#809b9d"/><path d="M0 960 650 450 1200 1040 1580 680 1800 800V1100H0Z" fill="#4c6a71"/><path d="M0 990 580 900 1080 1070 1800 970V1100H0Z" fill="#2d4954"/></svg>`

function wav() {
  const length = 8000 * 120
  const buffer = Buffer.alloc(44 + length * 2)
  buffer.write('RIFF'); buffer.writeUInt32LE(36 + length * 2, 4); buffer.write('WAVEfmt ', 8)
  buffer.writeUInt32LE(16, 16); buffer.writeUInt16LE(1, 20); buffer.writeUInt16LE(1, 22)
  buffer.writeUInt32LE(8000, 24); buffer.writeUInt32LE(16000, 28); buffer.writeUInt16LE(2, 32)
  buffer.writeUInt16LE(16, 34); buffer.write('data', 36); buffer.writeUInt32LE(length * 2, 40)
  return buffer
}

async function mockMedia(page: Page, baseUrl?: string) {
  const sound = wav()
  const video = readFileSync(new URL('./fixtures/preview.webm', import.meta.url))
  await page.route('**/api/**', async route => {
    const path = new URL(route.request().url()).pathname
    const json = (value: unknown) => route.fulfill({ json: value })
    if (path === '/api/auth/me') return json({ username: 'admin', has_avatar: false })
    if (path === '/api/events' || path === '/api/system/status/stream') {
      return route.fulfill({ contentType: 'text/event-stream', body: '' })
    }
    if (path === `/api/files/${ROOT}/children`) return json({ items: files, total_bytes: 800000, file_count: 4 })
    if (path === `/api/files/${ROOT}`) {
      return json({
        file: {
          id: ROOT, parent_id: null, name: '我的文件', kind: 'directory', size: 0,
          status: 'ready', created_at: STAMP, updated_at: STAMP,
        },
        breadcrumbs: [],
      })
    }
    if (path.endsWith('/media/progress')) return json({ position: 10, duration: 120 })
    if (path === '/api/files/audio-1/audio') {
      return json({
        duration: 120,
        has_cover: true,
        cover_url: '/api/files/image-1/preview',
        chapters: [
          { id: 1, title: '第一章 · 风从山谷来', start: 0, end: 40 },
          { id: 2, title: '第二章 · 在林间停留', start: 40, end: 80 },
          { id: 3, title: '第三章 · 晚风与归途', start: 80, end: 120 },
        ],
      })
    }
    if (path === '/api/files/video-1/video') {
      return json({ subtitles: [{ id: 'zh', name: 'zh', label: '简体中文', language: 'zh', url: '/api/subtitle.vtt', default: true }] })
    }
    if (path === '/api/subtitle.vtt') {
      return route.fulfill({ contentType: 'text/vtt', body: 'WEBVTT\n\n00:00:00.000 --> 00:00:30.000\n沿着山间的小路，慢慢走。\n' })
    }
    if (path.endsWith('/thumbnail') || path.startsWith('/api/files/image-')) {
      return route.fulfill({ contentType: 'image/svg+xml', body: landscape(path.includes('image-2')) })
    }
    if (path.endsWith('/preview')) {
      const body = path.includes('audio-1') ? sound : video
      const contentType = path.includes('audio-1') ? 'audio/wav' : 'video/webm'
      const range = route.request().headers().range?.match(/bytes=(\d+)-(\d*)/)
      if (range) {
        const start = Number(range[1])
        const end = Math.min(Number(range[2] || body.length - 1), body.length - 1)
        return route.fulfill({
          status: 206,
          contentType,
          headers: { 'accept-ranges': 'bytes', 'content-range': `bytes ${start}-${end}/${body.length}` },
          body: body.subarray(start, end + 1),
        })
      }
      return route.fulfill({ contentType, body })
    }
    return json({ items: [] })
  })
  await page.goto(baseUrl ? `${baseUrl}/` : '/')
  await expect(page.locator('.file-card')).toHaveCount(4)
}

async function open(page: Page, name: string) {
  await page.locator('.file-card').filter({ hasText: name }).click()
  await expect(page.locator('.preview-modal')).toBeVisible()
}

async function noOverflow(page: Page) {
  expect(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth)).toBe(true)
}

for (const width of [1440, 390, 320]) {
  test(`音频 ${width}px：章节、秒数跳转、Esc 和焦点恢复`, async ({ page }) => {
    await page.setViewportSize({ width, height: 900 })
    await mockMedia(page)
    await open(page, '山间来信.m4a')
    await expect(page.locator('audio')).toHaveJSProperty('readyState', 4)
    await expect(page.locator('.audio-panel')).toHaveCount(0)
    await expect(page.locator('.audio-main')).not.toContainText('没有内嵌字幕')
    await page.getByRole('button', { name: '章节', exact: true }).click()
    await page.locator('[data-chapter-index="1"]').click()
    await expect(page.locator('.audio-chapter-current h1')).toHaveText('第二章 · 在林间停留')
    await page.keyboard.press('Escape')
    await expect(page.locator('.audio-panel')).toHaveCount(0)
    await page.locator('audio').evaluate((element: HTMLAudioElement) => element.pause())
    await page.getByRole('button', { name: '前进30秒' }).click()
    await expect.poll(() => page.locator('audio').evaluate((element: HTMLAudioElement) => element.currentTime)).toBeGreaterThanOrEqual(70)
    await page.getByRole('button', { name: '后退15秒' }).click()
    await expect.poll(() => page.locator('audio').evaluate((element: HTMLAudioElement) => Math.floor(element.currentTime))).toBe(55)
    const controls = await page.locator('.audio-playback').boundingBox()
    expect(controls!.x).toBeGreaterThanOrEqual(0)
    expect(controls!.x + controls!.width).toBeLessThanOrEqual(width)
    await noOverflow(page)
    await page.keyboard.press('Escape')
    await expect(page.locator('.preview-modal')).toHaveCount(0)
    await expect(page.locator('.file-card').filter({ hasText: '山间来信.m4a' })).toBeFocused()
  })
}

for (const [name, selector] of [['山间来信.m4a', 'audio'], ['山间漫步.webm', 'video']] as const) {
  test(`不支持的原文件直接报错：${selector}`, async ({ page }) => {
    await mockMedia(page)
    const requested: string[] = []
    page.on('request', request => requested.push(new URL(request.url()).pathname))
    await page.route('**/api/files/*/preview', route => route.fulfill({ contentType: 'application/octet-stream', body: 'not decodable media' }))
    await open(page, name)
    await expect.poll(() => page.locator(selector).evaluate((element: HTMLMediaElement) => element.error?.code)).toBe(4)
    await expect(page.getByRole('alert')).toContainText('浏览器无法播放')
    expect(requested.some(path => /hls|fmp4|transcode|audio\/stream/.test(path))).toBe(false)
  })
}

test('图片：实际大小、拖动边界、缩略图、菜单和逐层退出', async ({ page }) => {
  await page.setViewportSize({ width: 1440, height: 900 })
  await mockMedia(page)
  await open(page, '群山.png')
  await expect(page.locator('.preview-image')).toBeVisible()
  await page.getByRole('button', { name: '实际大小', exact: true }).click()
  await expect(page.locator('.preview-actual-size')).toHaveText('100%')
  const naturalSize = await page.locator('.preview-image').boundingBox()
  expect(naturalSize!.width).toBeCloseTo(1800, 0)
  await page.getByRole('button', { name: '适应窗口' }).click()
  const fitted = await page.locator('.preview-image').boundingBox()
  expect(fitted!.width).toBeLessThan(1440)
  await page.getByRole('button', { name: '缩略图', exact: true }).click()
  await expect(page.locator('.preview-filmstrip button')).toHaveCount(2)
  await expect(page.locator('.preview-filmstrip img').first()).toHaveAttribute(
    'src',
    '/api/files/image-1/thumbnail?v=',
  )
  await page.getByRole('button', { name: '查看 远山.png' }).click()
  await expect(page.locator('.preview-file-meta')).toHaveText('远山.png')
  await page.getByRole('button', { name: '实际大小', exact: true }).click()
  await page.mouse.move(720, 400); await page.mouse.down(); await page.mouse.move(1300, 500); await page.mouse.up()
  const portrait = await page.locator('.preview-image').boundingBox()
  expect(portrait!.x + portrait!.width / 2).toBeCloseTo(720, 0)
  await page.getByRole('button', { name: '适应窗口' }).click()
  await page.locator('.preview-commandbar summary').click()
  await expect(page.getByRole('button', { name: '移动', exact: true })).toBeVisible()
  await page.keyboard.press('Escape')
  await expect(page.locator('.preview-modal')).toBeVisible()
  await expect(page.locator('details[open]')).toHaveCount(0)
  await page.locator('.preview-stage').click({ position: { x: 10, y: 10 } })
  await expect(page.locator('.preview-commandbar')).toBeHidden()
  await page.keyboard.press('Tab')
  await expect(page.locator('.preview-commandbar')).toBeVisible()
  await page.keyboard.press('Escape')
  await expect(page.locator('.preview-modal')).toHaveCount(0)
})

test('图片预览：从根节点按 Tab 首先进入更多操作菜单', async ({ page }) => {
  await mockMedia(page)
  await open(page, '群山.png')
  await page.locator('.preview-modal').focus()
  await page.keyboard.press('Tab')
  await expect(page.locator('.preview-commandbar summary')).toBeFocused()
})

test('old/new 图片缩略图失败时只回退一次到原图地址', async ({ browser }) => {
  const oldUrl = process.env.E2E_REFERENCE_URL || 'http://127.0.0.1:18080'
  const newUrl = process.env.E2E_NEW_URL || 'http://127.0.0.1:18084'
  const oldContext = await browser.newContext({ viewport: { width: 1440, height: 900 } })
  const newContext = await browser.newContext({ viewport: { width: 1440, height: 900 } })
  const oldPage = await oldContext.newPage()
  const newPage = await newContext.newPage()

  async function exercise(page: Page, baseUrl: string) {
    await mockMedia(page, baseUrl)
    let thumbnailFailures = 0
    let previewFallbacks = 0
    await page.route('**/api/files/image-2/thumbnail*', async route => {
      thumbnailFailures += 1
      await route.fulfill({ status: 500, body: 'thumbnail unavailable' })
    })
    await page.route('**/api/files/image-2/preview', async route => {
      previewFallbacks += 1
      await route.fallback()
    })
    await open(page, '群山.png')
    await page.getByRole('button', { name: '缩略图', exact: true }).click()
    const thumbnail = page.getByRole('button', { name: '查看 远山.png' }).locator('img')
    await expect.poll(() => thumbnail.getAttribute('src'), { timeout: 10_000 }).toContain('/api/files/image-2/preview')
    await page.waitForTimeout(300)
    const src = await thumbnail.getAttribute('src')
    return {
      thumbnailFailures,
      previewFallbacks,
      absolute: !!src && /^https?:\/\//.test(src),
      srcPath: src ? new URL(src, baseUrl).pathname : null,
    }
  }

  try {
    const [oldResult, newResult] = await Promise.all([
      exercise(oldPage, oldUrl),
      exercise(newPage, newUrl),
    ])
    expect(oldResult.thumbnailFailures).toBe(1)
    expect(oldResult.previewFallbacks).toBe(1)
    expect(newResult, 'Rust 缩略图失败回退与 reference 不一致').toEqual(oldResult)
  } finally {
    await Promise.all([oldContext.close(), newContext.close()])
  }
})

test('音频和视频恢复旧版各自的音量、倍速与位置存储，不共享错误的倍速设置', async ({ page }) => {
  await mockMedia(page)
  await page.evaluate(() => {
    localStorage.setItem('revaro-audio-volume', '0.42')
    localStorage.setItem('revaro-audio-muted', 'true')
    localStorage.setItem('revaro-audio-rate', '2')
    localStorage.setItem('revaro-video-volume', '0.33')
    localStorage.setItem('revaro-video-rate', '0.5')
    localStorage.setItem('revaro-audio-position:audio-1', '25')
    localStorage.setItem('revaro-video-position:video-1', '35')
  })

  await open(page, '山间来信.m4a')
  await expect.poll(() => page.locator('audio').evaluate((element: HTMLAudioElement) => element.volume)).toBeCloseTo(0.42, 2)
  await expect(page.locator('audio')).toHaveJSProperty('muted', true)
  await expect(page.getByLabel('播放速度', { exact: true })).toHaveValue('1')
  await page.keyboard.press('Escape')

  await open(page, '山间漫步.webm')
  await expect.poll(() => page.locator('.video-player-shell video').evaluate((element: HTMLVideoElement) => element.volume)).toBeCloseTo(0.33, 2)
  await expect.poll(() => page.locator('.video-player-shell video').evaluate((element: HTMLVideoElement) => element.playbackRate)).toBe(0.5)
})

test('old/new 音频用户 seek 与预览关闭的进度持久化时机一致', async ({ browser }) => {
  const oldUrl = process.env.E2E_REFERENCE_URL || 'http://127.0.0.1:18080'
  const newUrl = process.env.E2E_NEW_URL || 'http://127.0.0.1:18084'
  const oldContext = await browser.newContext({ viewport: { width: 1440, height: 900 } })
  const newContext = await browser.newContext({ viewport: { width: 1440, height: 900 } })
  const oldPage = await oldContext.newPage()
  const newPage = await newContext.newPage()

  async function exercise(page: Page, baseUrl: string) {
    await mockMedia(page, baseUrl)
    await open(page, '山间来信.m4a')
    const audio = page.locator('audio')
    await expect(audio).toHaveJSProperty('readyState', 4)
    await audio.evaluate((element: HTMLAudioElement) => element.pause())
    await page.waitForTimeout(50)
    await page.evaluate(() => localStorage.removeItem('revaro-audio-position:audio-1'))
    const progressRequests: Array<{ method: string; body: string | null }> = []
    page.on('request', request => {
      if (new URL(request.url()).pathname === '/api/files/audio-1/media/progress') {
        progressRequests.push({ method: request.method(), body: request.postData() })
      }
    })
    const timeUpdateCount = await page.locator('audio').evaluate((element: HTMLAudioElement) => {
      const state = window as typeof window & { __audioParityTimeUpdates?: number }
      state.__audioParityTimeUpdates = 0
      element.addEventListener('timeupdate', () => {
        state.__audioParityTimeUpdates = (state.__audioParityTimeUpdates ?? 0) + 1
      })
      return state.__audioParityTimeUpdates
    })

    await page.getByRole('button', { name: '前进30秒' }).click()
    await expect.poll(() => page.evaluate(() => (window as typeof window & { __audioParityTimeUpdates?: number }).__audioParityTimeUpdates ?? 0)).toBeGreaterThan(timeUpdateCount)
    const immediatelyAfterSeek = await page.evaluate(() => localStorage.getItem('revaro-audio-position:audio-1'))
    await page.waitForTimeout(650)
    const afterDebounce = await page.evaluate(() => localStorage.getItem('revaro-audio-position:audio-1'))
    await page.keyboard.press('Escape')
    await expect(page.locator('.preview-modal')).toHaveCount(0)
    await page.waitForTimeout(100)
    return { immediatelyAfterSeek, afterDebounce, methods: progressRequests.map(request => request.method) }
  }

  try {
    const [oldResult, newResult] = await Promise.all([
      exercise(oldPage, oldUrl),
      exercise(newPage, newUrl),
    ])
    expect(oldResult.immediatelyAfterSeek).toBeNull()
    expect(oldResult.afterDebounce).not.toBeNull()
    expect(oldResult.methods).toEqual(['PUT'])
    expect(newResult, 'Rust 音频 seek/关闭预览的进度持久化时序与 reference 不一致').toEqual(oldResult)
  } finally {
    await Promise.all([oldContext.close(), newContext.close()])
  }
})

test('old/new 视频用户 seek 与预览关闭的进度持久化时机一致', async ({ browser }) => {
  const oldUrl = process.env.E2E_REFERENCE_URL || 'http://127.0.0.1:18080'
  const newUrl = process.env.E2E_NEW_URL || 'http://127.0.0.1:18084'
  const oldContext = await browser.newContext({ viewport: { width: 1440, height: 900 } })
  const newContext = await browser.newContext({ viewport: { width: 1440, height: 900 } })
  const oldPage = await oldContext.newPage()
  const newPage = await newContext.newPage()

  async function exercise(page: Page, baseUrl: string) {
    await mockMedia(page, baseUrl)
    await open(page, '山间漫步.webm')
    const video = page.locator('.video-player-shell video')
    await expect(video).toHaveJSProperty('readyState', 4)
    await video.evaluate((element: HTMLVideoElement) => element.pause())
    await page.waitForTimeout(50)
    await page.evaluate(() => localStorage.removeItem('revaro-video-position:video-1'))
    const progressRequests: string[] = []
    page.on('request', request => {
      if (new URL(request.url()).pathname === '/api/files/video-1/media/progress') {
        progressRequests.push(request.method())
      }
    })
    const timeUpdateCount = await video.evaluate((element: HTMLVideoElement) => {
      const state = window as typeof window & { __videoParityTimeUpdates?: number }
      state.__videoParityTimeUpdates = 0
      element.addEventListener('timeupdate', () => {
        state.__videoParityTimeUpdates = (state.__videoParityTimeUpdates ?? 0) + 1
      })
      return state.__videoParityTimeUpdates
    })

    const seek = page.locator('.video-seek')
    await seek.fill('20')
    await seek.dispatchEvent('change')
    await expect.poll(() => page.evaluate(() => (window as typeof window & { __videoParityTimeUpdates?: number }).__videoParityTimeUpdates ?? 0)).toBeGreaterThan(timeUpdateCount)
    const immediatelyAfterSeek = await page.evaluate(() => localStorage.getItem('revaro-video-position:video-1'))
    await page.waitForTimeout(650)
    const afterDebounce = await page.evaluate(() => localStorage.getItem('revaro-video-position:video-1'))
    await page.keyboard.press('Escape')
    await expect(page.locator('.preview-modal')).toHaveCount(0)
    await page.waitForTimeout(100)
    return { immediatelyAfterSeek, afterDebounce, methods: progressRequests }
  }

  try {
    const [oldResult, newResult] = await Promise.all([
      exercise(oldPage, oldUrl),
      exercise(newPage, newUrl),
    ])
    expect(oldResult.immediatelyAfterSeek).toBeNull()
    expect(oldResult.afterDebounce).not.toBeNull()
    expect(oldResult.methods).toEqual(['PUT'])
    expect(newResult, 'Rust 视频 seek/关闭预览的进度持久化时序与 reference 不一致').toEqual(oldResult)
  } finally {
    await Promise.all([oldContext.close(), newContext.close()])
  }
})

test('old/new 视频 preview 慢响应期间的初始 loading 控件一致', async ({ browser }) => {
  const oldUrl = process.env.E2E_REFERENCE_URL || 'http://127.0.0.1:18080'
  const newUrl = process.env.E2E_NEW_URL || 'http://127.0.0.1:18084'
  const oldContext = await browser.newContext({ viewport: { width: 1440, height: 900 } })
  const newContext = await browser.newContext({ viewport: { width: 1440, height: 900 } })
  const oldPage = await oldContext.newPage()
  const newPage = await newContext.newPage()

  async function exercise(page: Page, baseUrl: string) {
    await mockMedia(page, baseUrl)
    const videoBody = readFileSync(new URL('./fixtures/preview.webm', import.meta.url))
    await page.route('**/api/files/video-1/preview', async route => {
      await new Promise(resolve => setTimeout(resolve, 700))
      await route.fulfill({ contentType: 'video/webm', body: videoBody })
    })
    await page.locator('.file-card').filter({ hasText: '山间漫步.webm' }).click()
    await expect(page.locator('.preview-modal')).toBeVisible()
    await page.waitForTimeout(100)
    return {
      loadingText: await page.locator('.video-loading').count(),
      centerPlay: await page.locator('.video-center-play').count(),
      controlLabel: await page.locator('.video-controls .video-icon-button').first().getAttribute('aria-label'),
    }
  }

  try {
    const [oldResult, newResult] = await Promise.all([
      exercise(oldPage, oldUrl),
      exercise(newPage, newUrl),
    ])
    expect(oldResult).toEqual({ loadingText: 0, centerPlay: 0, controlLabel: '暂停' })
    expect(newResult, 'Rust 视频慢 preview 初始 loading/播放控件与 reference 不一致').toEqual(oldResult)
  } finally {
    await Promise.all([oldContext.close(), newContext.close()])
  }
})

test('old/new 视频字幕 cue 文本的实体解码与分行一致', async ({ browser }) => {
  const oldUrl = process.env.E2E_REFERENCE_URL || 'http://127.0.0.1:18080'
  const newUrl = process.env.E2E_NEW_URL || 'http://127.0.0.1:18084'
  const oldContext = await browser.newContext({ viewport: { width: 1440, height: 900 } })
  const newContext = await browser.newContext({ viewport: { width: 1440, height: 900 } })
  const oldPage = await oldContext.newPage()
  const newPage = await newContext.newPage()

  async function exercise(page: Page, baseUrl: string) {
    await mockMedia(page, baseUrl)
    await page.route('**/api/subtitle.vtt', route => route.fulfill({
      contentType: 'text/vtt',
      body: 'WEBVTT\n\n00:00:00.000 --> 00:00:30.000 line:10%\n第一行 &nbsp; &amp; &#x2014; <b>加粗</b>\n第二行 &lt;标签&gt;\n',
    }))
    await open(page, '山间漫步.webm')
    await expect(page.locator('.video-subtitle-overlay')).toBeVisible()
    return page.locator('.video-subtitle-overlay').evaluate(element => ({
      text: element.textContent,
      lines: Array.from(element.querySelectorAll('span')).map(line => ({ text: line.textContent, className: line.className })),
      className: element.className,
    }))
  }

  try {
    const [oldResult, newResult] = await Promise.all([
      exercise(oldPage, oldUrl),
      exercise(newPage, newUrl),
    ])
    expect(oldResult).toEqual({
      text: '第一行   & — 加粗第二行 <标签>',
      lines: [
        { text: '第一行   & — 加粗', className: '' },
        { text: '第二行 <标签>', className: 'video-subtitle-secondary-line' },
      ],
      className: 'video-subtitle-overlay top',
    })
    expect(newResult, 'Rust 视频字幕 cue 文本/实体/分行与 reference 不一致').toEqual(oldResult)
  } finally {
    await Promise.all([oldContext.close(), newContext.close()])
  }
})

test('old/new 图片滚轮缩放保持鼠标锚点一致', async ({ browser }) => {
  const oldUrl = process.env.E2E_REFERENCE_URL || 'http://127.0.0.1:18080'
  const newUrl = process.env.E2E_NEW_URL || 'http://127.0.0.1:18084'
  const oldContext = await browser.newContext({ viewport: { width: 1440, height: 900 } })
  const newContext = await browser.newContext({ viewport: { width: 1440, height: 900 } })
  const oldPage = await oldContext.newPage()
  const newPage = await newContext.newPage()

  async function exercise(page: Page, baseUrl: string) {
    await mockMedia(page, baseUrl)
    await open(page, '群山.png')
    const image = page.locator('.preview-image')
    await expect(image).toBeVisible()
    await page.getByRole('button', { name: '实际大小', exact: true }).click()
    await page.mouse.move(280, 390)
    await page.mouse.wheel(0, -100)
    await page.waitForTimeout(50)
    return image.evaluate(element => ({
      transform: getComputedStyle(element).transform,
      box: element.getBoundingClientRect().toJSON(),
    }))
  }

  try {
    const [oldResult, newResult] = await Promise.all([
      exercise(oldPage, oldUrl),
      exercise(newPage, newUrl),
    ])
    expect(oldResult.box.width).toBeGreaterThan(1800)
    expect(newResult, 'Rust 图片滚轮缩放的鼠标锚点与 reference 不一致').toEqual(oldResult)
  } finally {
    await Promise.all([oldContext.close(), newContext.close()])
  }
})

test('old/new 媒体关闭预览的最终进度保存使用相同的 keepalive 语义', async ({ browser }) => {
  const oldUrl = process.env.E2E_REFERENCE_URL || 'http://127.0.0.1:18080'
  const newUrl = process.env.E2E_NEW_URL || 'http://127.0.0.1:18084'
  const oldContext = await browser.newContext({ viewport: { width: 1440, height: 900 } })
  const newContext = await browser.newContext({ viewport: { width: 1440, height: 900 } })
  const oldPage = await oldContext.newPage()
  const newPage = await newContext.newPage()

  async function exercise(page: Page, baseUrl: string) {
    await page.addInitScript(() => {
      const state = window as typeof window & { __mediaParityKeepalive?: Array<boolean | null> }
      state.__mediaParityKeepalive = []
      const original = window.fetch
      window.fetch = (input: RequestInfo | URL, init?: RequestInit) => {
        const url = typeof input === 'string' ? input : input instanceof Request ? input.url : input.toString()
        const request = input instanceof Request ? input : null
        if (url.includes('/media/progress') && (init?.method ?? request?.method) === 'PUT') {
          state.__mediaParityKeepalive?.push(init?.keepalive ?? request?.keepalive ?? null)
        }
        return original(input, init)
      }
    })
    await mockMedia(page, baseUrl)
    async function closePreview(name: string, selector: 'audio' | 'video') {
      await open(page, name)
      const media = page.locator(selector)
      await expect(media).toHaveJSProperty('readyState', 4)
      await media.evaluate((element: HTMLMediaElement) => element.pause())
      await page.waitForTimeout(100)
      await page.evaluate(() => {
        (window as typeof window & { __mediaParityKeepalive?: Array<boolean | null> }).__mediaParityKeepalive = []
      })
      await page.keyboard.press('Escape')
      await expect(page.locator('.preview-modal')).toHaveCount(0)
      return page.evaluate(() => (window as typeof window & { __mediaParityKeepalive?: Array<boolean | null> }).__mediaParityKeepalive ?? [])
    }
    return {
      audio: await closePreview('山间来信.m4a', 'audio'),
      video: await closePreview('山间漫步.webm', 'video'),
    }
  }

  try {
    const [oldResult, newResult] = await Promise.all([
      exercise(oldPage, oldUrl),
      exercise(newPage, newUrl),
    ])
    expect(oldResult).toEqual({ audio: [true], video: [true] })
    expect(newResult, 'Rust 媒体关闭预览未保留 reference 的 keepalive 最终保存').toEqual(oldResult)
  } finally {
    await Promise.all([oldContext.close(), newContext.close()])
  }
})

test('视频全屏按钮和全屏状态同步，退出后恢复预览层', async ({ page }) => {
  await mockMedia(page)
  await open(page, '山间漫步.webm')
  const supported = await page.evaluate(() => Boolean(document.fullscreenEnabled && document.documentElement.requestFullscreen))
  test.skip(!supported, '当前 Chromium 不提供原生 Fullscreen API')

  const enter = page.getByRole('button', { name: '全屏', exact: true })
  await enter.click()
  await expect.poll(() => page.evaluate(() => Boolean(document.fullscreenElement))).toBe(true)
  await expect(page.getByRole('button', { name: '退出全屏', exact: true })).toBeVisible()

  // Headless Chromium reserves Escape for its own browser chrome, so use the
  // in-player exit control for a deterministic old/new assertion. The media
  // shell still ignores Escape while native fullscreen is active, matching the
  // reference's usePreviewDialog guard.
  await page.getByRole('button', { name: '退出全屏', exact: true }).click()
  await expect.poll(() => page.evaluate(() => Boolean(document.fullscreenElement))).toBe(false)
  await expect(page.getByRole('button', { name: '全屏', exact: true })).toBeVisible()
})

for (const width of [1440, 390, 320]) {
  test(`视频 ${width}px：设置、字幕、自动隐藏和无溢出`, async ({ page }) => {
    await page.setViewportSize({ width, height: 844 })
    await mockMedia(page)
    await open(page, '山间漫步.webm')
    await expect(page.locator('video').last()).toHaveJSProperty('paused', false)
    const shell = page.locator('.video-player-shell')
    const video = shell.locator('video')
    await shell.hover()
    await expect(page.locator('.video-time')).toBeVisible()
    await expect(page.locator('.video-subtitle-overlay')).toBeVisible()
    const subtitleBefore = await page.locator('.video-subtitle-overlay').boundingBox()
    await page.getByLabel('播放设置', { exact: true }).click()
    await page.getByLabel('播放速度', { exact: true }).selectOption('1.5')
    await expect(video).toHaveJSProperty('playbackRate', 1.5)
    await page.mouse.move(0, 0); await page.waitForTimeout(3200)
    await expect(page.locator('.video-controls')).toBeVisible()
    await page.keyboard.press('Escape')
    await expect(shell).toBeVisible()
    await video.focus(); await page.mouse.move(0, 0); await page.waitForTimeout(3200)
    await page.evaluate(() => (document.activeElement as HTMLElement)?.blur())
    await page.waitForTimeout(3000)
    await expect(page.locator('.video-controls')).toBeHidden()
    const subtitleAfter = await page.locator('.video-subtitle-overlay').boundingBox()
    expect(subtitleAfter!.y).toBeCloseTo(subtitleBefore!.y, 0)
    await shell.hover()
    await expect(page.locator('.video-time')).toBeVisible()
    const row = await page.locator('.video-control-row').boundingBox()
    expect(row!.x + row!.width).toBeLessThanOrEqual(width)
    await noOverflow(page)
  })
}

test('触屏：视频点按只切换控制条，图片双指缩放和取消手势不误翻页', async ({ browser }) => {
  const context = await browser.newContext({ viewport: { width: 390, height: 844 }, isMobile: true, hasTouch: true })
  const page = await context.newPage()
  try {
    await mockMedia(page)
    await open(page, '山间漫步.webm')
    const video = page.locator('.video-player-shell video')
    await expect(video).toHaveJSProperty('paused', false)
    await page.touchscreen.tap(195, 400)
    await expect(page.locator('.video-controls')).toBeHidden()
    await expect(video).toHaveJSProperty('paused', false)
    await page.touchscreen.tap(195, 400)
    await expect(page.locator('.video-controls')).toBeVisible()
    await expect(video).toHaveJSProperty('paused', false)
    await page.getByRole('button', { name: '退出播放' }).tap()
    await open(page, '群山.png')
    await expect(page.locator('.preview-image')).toBeVisible()
    const before = Number.parseInt((await page.locator('.preview-actual-size').textContent()) ?? '0', 10)
    const session = await context.newCDPSession(page)
    await session.send('Input.dispatchTouchEvent', { type: 'touchStart', touchPoints: [{ x: 120, y: 400, id: 1 }, { x: 270, y: 400, id: 2 }] })
    await session.send('Input.dispatchTouchEvent', { type: 'touchMove', touchPoints: [{ x: 70, y: 400, id: 1 }, { x: 320, y: 400, id: 2 }] })
    await session.send('Input.dispatchTouchEvent', { type: 'touchEnd', touchPoints: [] })
    expect(Number.parseInt((await page.locator('.preview-actual-size').textContent()) ?? '0', 10)).toBeGreaterThan(before)
    await page.getByRole('button', { name: '适应窗口' }).tap()
    await session.send('Input.dispatchTouchEvent', { type: 'touchStart', touchPoints: [{ x: 300, y: 400, id: 1 }] })
    await session.send('Input.dispatchTouchEvent', { type: 'touchMove', touchPoints: [{ x: 100, y: 400, id: 1 }] })
    await session.send('Input.dispatchTouchEvent', { type: 'touchCancel', touchPoints: [] })
    await expect(page.locator('.preview-file-meta')).toHaveText('群山.png')
    await session.send('Input.dispatchTouchEvent', { type: 'touchStart', touchPoints: [{ x: 300, y: 400, id: 1 }] })
    await session.send('Input.dispatchTouchEvent', { type: 'touchMove', touchPoints: [{ x: 100, y: 400, id: 1 }] })
    await session.send('Input.dispatchTouchEvent', { type: 'touchEnd', touchPoints: [] })
    await expect(page.locator('.preview-file-meta')).toHaveText('远山.png')
  } finally {
    await context.close()
  }
})

test('old/new 触屏媒体手势链保持视频点按、图片缩放与取消语义', async ({ browser }) => {
  const oldUrl = process.env.E2E_REFERENCE_URL || 'http://127.0.0.1:18080'
  const newUrl = process.env.E2E_NEW_URL || 'http://127.0.0.1:18084'
  const oldContext = await browser.newContext({ viewport: { width: 390, height: 844 }, isMobile: true, hasTouch: true })
  const newContext = await browser.newContext({ viewport: { width: 390, height: 844 }, isMobile: true, hasTouch: true })
  const oldPage = await oldContext.newPage()
  const newPage = await newContext.newPage()

  async function exercise(page: Page, baseUrl: string) {
    await mockMedia(page, baseUrl)
    await open(page, '山间漫步.webm')
    const video = page.locator('.video-player-shell video')
    await expect(video).toHaveJSProperty('paused', false)
    await page.touchscreen.tap(195, 400)
    const hiddenAfterFirstTap = await page.locator('.video-controls').isHidden()
    const playingAfterFirstTap = !(await video.evaluate((element: HTMLVideoElement) => element.paused))
    await page.touchscreen.tap(195, 400)
    const visibleAfterSecondTap = await page.locator('.video-controls').isVisible()
    const playingAfterSecondTap = !(await video.evaluate((element: HTMLVideoElement) => element.paused))
    await page.getByRole('button', { name: '退出播放' }).tap()

    await open(page, '群山.png')
    const beforeZoom = Number.parseInt((await page.locator('.preview-actual-size').textContent()) ?? '0', 10)
    const session = await page.context().newCDPSession(page)
    await session.send('Input.dispatchTouchEvent', { type: 'touchStart', touchPoints: [{ x: 120, y: 400, id: 1 }, { x: 270, y: 400, id: 2 }] })
    await session.send('Input.dispatchTouchEvent', { type: 'touchMove', touchPoints: [{ x: 70, y: 400, id: 1 }, { x: 320, y: 400, id: 2 }] })
    await session.send('Input.dispatchTouchEvent', { type: 'touchEnd', touchPoints: [] })
    const afterZoom = Number.parseInt((await page.locator('.preview-actual-size').textContent()) ?? '0', 10)

    await page.getByRole('button', { name: '适应窗口' }).tap()
    await session.send('Input.dispatchTouchEvent', { type: 'touchStart', touchPoints: [{ x: 300, y: 400, id: 1 }] })
    await session.send('Input.dispatchTouchEvent', { type: 'touchMove', touchPoints: [{ x: 100, y: 400, id: 1 }] })
    await session.send('Input.dispatchTouchEvent', { type: 'touchCancel', touchPoints: [] })
    const afterCancel = await page.locator('.preview-file-meta').innerText()
    await session.send('Input.dispatchTouchEvent', { type: 'touchStart', touchPoints: [{ x: 300, y: 400, id: 1 }] })
    await session.send('Input.dispatchTouchEvent', { type: 'touchMove', touchPoints: [{ x: 100, y: 400, id: 1 }] })
    await session.send('Input.dispatchTouchEvent', { type: 'touchEnd', touchPoints: [] })
    const afterSwipe = await page.locator('.preview-file-meta').innerText()

    return {
      hiddenAfterFirstTap,
      playingAfterFirstTap,
      visibleAfterSecondTap,
      playingAfterSecondTap,
      beforeZoom,
      afterZoom,
      afterCancel,
      afterSwipe,
    }
  }

  try {
    const [oldResult, newResult] = await Promise.all([
      exercise(oldPage, oldUrl),
      exercise(newPage, newUrl),
    ])
    expect(oldResult.playingAfterFirstTap).toBe(true)
    expect(oldResult.playingAfterSecondTap).toBe(true)
    expect(oldResult.afterZoom).toBeGreaterThan(oldResult.beforeZoom)
    expect(oldResult.afterCancel).toBe('群山.png')
    expect(oldResult.afterSwipe).toBe('远山.png')
    expect(newResult, 'Rust 触屏媒体手势与 reference 不一致').toEqual(oldResult)
  } finally {
    await Promise.all([oldContext.close(), newContext.close()])
  }
})

test('old/new 不支持媒体保留原文件错误分流且不请求转码入口', async ({ browser }) => {
  const oldUrl = process.env.E2E_REFERENCE_URL || 'http://127.0.0.1:18080'
  const newUrl = process.env.E2E_NEW_URL || 'http://127.0.0.1:18084'
  const oldContext = await browser.newContext({ viewport: { width: 1440, height: 900 } })
  const newContext = await browser.newContext({ viewport: { width: 1440, height: 900 } })
  const oldPage = await oldContext.newPage()
  const newPage = await newContext.newPage()

  async function exercise(page: Page, baseUrl: string) {
    await mockMedia(page, baseUrl)
    const requested: string[] = []
    page.on('request', request => requested.push(new URL(request.url()).pathname))
    await page.route('**/api/files/*/preview', route => route.fulfill({ contentType: 'application/octet-stream', body: 'not decodable media' }))

    const errors: Array<{ selector: 'audio' | 'video'; code: number | null; message: string }> = []
    for (const [name, selector] of [['山间来信.m4a', 'audio'], ['山间漫步.webm', 'video']] as const) {
      await open(page, name)
      const media = page.locator(selector)
      await expect.poll(() => media.evaluate((element: HTMLMediaElement) => element.error?.code ?? null)).toBe(4)
      errors.push({ selector, code: await media.evaluate((element: HTMLMediaElement) => element.error?.code ?? null), message: await page.getByRole('alert').innerText() })
      await page.keyboard.press('Escape')
      await expect(page.locator('.preview-modal')).toHaveCount(0)
    }
    return {
      errors,
      transcodingRequests: requested.filter(path => /hls|fmp4|transcode|audio\/stream/.test(path)),
    }
  }

  try {
    const [oldResult, newResult] = await Promise.all([
      exercise(oldPage, oldUrl),
      exercise(newPage, newUrl),
    ])
    expect(oldResult.errors.map(({ selector, code }) => ({ selector, code }))).toEqual([
      { selector: 'audio', code: 4 },
      { selector: 'video', code: 4 },
    ])
    expect(oldResult.errors[0].message).toBe('浏览器无法播放此原始格式，请下载后使用本地播放器打开')
    expect(oldResult.errors[1].message).toContain('浏览器无法播放此原始格式，请下载后使用本地播放器打开')
    expect(oldResult.errors[1].message).toContain('重新尝试')
    expect(oldResult.transcodingRequests).toEqual([])
    expect(newResult, 'Rust 不支持媒体的错误分流与 reference 不一致').toEqual(oldResult)
  } finally {
    await Promise.all([oldContext.close(), newContext.close()])
  }
})

test('old/new 损坏图片与视频重试保持错误层级、禁用状态和请求时序', async ({ browser }) => {
  const oldUrl = process.env.E2E_REFERENCE_URL || 'http://127.0.0.1:18080'
  const newUrl = process.env.E2E_NEW_URL || 'http://127.0.0.1:18084'
  const oldContext = await browser.newContext({ viewport: { width: 1440, height: 900 } })
  const newContext = await browser.newContext({ viewport: { width: 1440, height: 900 } })
  const oldPage = await oldContext.newPage()
  const newPage = await newContext.newPage()
  const validVideo = readFileSync(new URL('./fixtures/preview.webm', import.meta.url))

  async function exercise(page: Page, baseUrl: string) {
    await mockMedia(page, baseUrl)
    let imagePreviewRequests = 0
    let videoPreviewRequests = 0
    let allowVideoRetry = false
    await page.route('**/api/files/image-1/preview', route => {
      imagePreviewRequests += 1
      return route.fulfill({ status: 500, body: 'damaged image' })
    })
    await page.route('**/api/files/video-1/preview', route => {
      videoPreviewRequests += 1
      if (!allowVideoRetry) {
        return route.fulfill({ contentType: 'application/octet-stream', body: 'damaged video' })
      }
      return route.fulfill({ contentType: 'video/webm', body: validVideo })
    })

    await open(page, '群山.png')
    await expect(page.getByRole('alert')).toHaveText(/图片暂时无法加载/)
    const failedImage = await page.evaluate(() => {
      const image = document.querySelector<HTMLImageElement>('.preview-image')
      const tools = Array.from(document.querySelectorAll<HTMLButtonElement>('.preview-image-tools button'))
      return {
        display: image ? getComputedStyle(image).display : null,
        toolsDisabled: tools.map(button => button.disabled),
        navCount: document.querySelectorAll('.preview-nav').length,
      }
    })
    const downloadPromise = page.waitForEvent('download')
    await page.getByRole('button', { name: '下载原图', exact: true }).click()
    const imageDownload = await downloadPromise
    await page.getByRole('button', { name: '关闭预览', exact: true }).click()
    await expect(page.locator('.preview-modal')).toHaveCount(0)

    await open(page, '山间漫步.webm')
    await expect(page.getByRole('alert')).toContainText('浏览器无法播放此原始格式')
    await expect(page.getByRole('button', { name: '重新尝试', exact: true })).toBeVisible()
    const initialVideoRequests = videoPreviewRequests
    allowVideoRetry = true
    await page.getByRole('button', { name: '重新尝试', exact: true }).click()
    await expect.poll(() => page.locator('.video-player-shell video').evaluate(element => element.readyState)).toBeGreaterThan(0)
    await expect(page.locator('.video-error')).toHaveCount(0)
    const retriedVideo = {
      requests: videoPreviewRequests,
      initialRequests: initialVideoRequests,
      paused: await page.locator('.video-player-shell video').evaluate(element => element.paused),
    }
    await page.getByRole('button', { name: '退出播放', exact: true }).click()
    await expect(page.locator('.preview-modal')).toHaveCount(0)
    return { failedImage, imagePreviewRequests, imageDownloadFilename: imageDownload.suggestedFilename(), retriedVideo }
  }

  try {
    const [oldResult, newResult] = await Promise.all([
      exercise(oldPage, oldUrl),
      exercise(newPage, newUrl),
    ])
    expect(oldResult.failedImage).toEqual({ display: 'none', toolsDisabled: [true, true, true, true, false], navCount: 2 })
    expect(oldResult.imagePreviewRequests).toBe(1)
    expect(oldResult.imageDownloadFilename).toBe('群山.png')
    expect(oldResult.retriedVideo.initialRequests).toBeGreaterThanOrEqual(1)
    expect(oldResult.retriedVideo.requests).toBeGreaterThan(oldResult.retriedVideo.initialRequests)
    expect(oldResult.retriedVideo.paused).toBe(false)
    expect(newResult, 'Rust 损坏媒体错误/重试行为与 reference 不一致').toEqual(oldResult)
  } finally {
    await Promise.all([oldContext.close(), newContext.close()])
  }
})

test('old/new 音视频 seek 边界都钳制在媒体时长内', async ({ browser }) => {
  const oldUrl = process.env.E2E_REFERENCE_URL || 'http://127.0.0.1:18080'
  const newUrl = process.env.E2E_NEW_URL || 'http://127.0.0.1:18084'
  const oldContext = await browser.newContext({ viewport: { width: 1440, height: 900 } })
  const newContext = await browser.newContext({ viewport: { width: 1440, height: 900 } })
  const oldPage = await oldContext.newPage()
  const newPage = await newContext.newPage()

  async function exercise(page: Page, baseUrl: string) {
    await mockMedia(page, baseUrl)
    await open(page, '山间来信.m4a')
    const audio = page.locator('audio')
    await expect(audio).toHaveJSProperty('readyState', 4)
    await audio.evaluate((element: HTMLAudioElement) => {
      element.pause()
      element.currentTime = 0
      element.dispatchEvent(new Event('timeupdate'))
    })
    await page.getByRole('button', { name: '后退15秒', exact: true }).click()
    const audioAtStart = await audio.evaluate((element: HTMLAudioElement) => element.currentTime)
    await audio.evaluate((element: HTMLAudioElement) => {
      element.currentTime = element.duration - 1
      element.dispatchEvent(new Event('timeupdate'))
    })
    await page.getByRole('button', { name: '前进30秒', exact: true }).click()
    const audioAtEnd = await audio.evaluate((element: HTMLAudioElement) => element.currentTime)
    await page.keyboard.press('Escape')
    await expect(page.locator('.preview-modal')).toHaveCount(0)

    await open(page, '山间漫步.webm')
    const video = page.locator('.video-player-shell video')
    await expect(video).toHaveJSProperty('readyState', 4)
    await video.evaluate((element: HTMLVideoElement) => {
      element.pause()
      element.currentTime = 0
      element.dispatchEvent(new Event('timeupdate'))
    })
    await page.locator('.video-player-shell').focus()
    await page.keyboard.press('ArrowLeft')
    const videoAtStart = await video.evaluate((element: HTMLVideoElement) => element.currentTime)
    await video.evaluate((element: HTMLVideoElement) => {
      element.currentTime = element.duration - 1
      element.dispatchEvent(new Event('timeupdate'))
    })
    await page.keyboard.press('ArrowRight')
    const videoAtEnd = await video.evaluate((element: HTMLVideoElement) => element.currentTime)
    await page.getByRole('button', { name: '退出播放', exact: true }).click()
    await expect(page.locator('.preview-modal')).toHaveCount(0)
    return {
      audioAtStart: Math.round(audioAtStart * 100) / 100,
      audioAtEnd: Math.round(audioAtEnd * 100) / 100,
      videoAtStart: Math.round(videoAtStart * 100) / 100,
      videoAtEnd: Math.round(videoAtEnd * 100) / 100,
    }
  }

  try {
    const [oldResult, newResult] = await Promise.all([
      exercise(oldPage, oldUrl),
      exercise(newPage, newUrl),
    ])
    expect(oldResult.audioAtStart).toBe(0)
    expect(oldResult.audioAtEnd).toBe(120)
    expect(oldResult.videoAtStart).toBe(0)
    expect(oldResult.videoAtEnd).toBe(30)
    expect(newResult, 'Rust 音视频 seek 边界与 reference 不一致').toEqual(oldResult)
  } finally {
    await Promise.all([oldContext.close(), newContext.close()])
  }
})

import { expect, test, type BrowserContext, type Page } from '@playwright/test'
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
          { id: 11, title: '第一章 · 风从山谷来', start: 0, end: 40 },
          { id: 22, title: '第二章 · 在林间停留', start: 40, end: 80 },
          { id: 33, title: '第三章 · 晚风与归途', start: 80, end: 120 },
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

test('old/new 音频章节面板保持 reference 的切换与焦点回收', async ({ browser }) => {
  const oldUrl = process.env.E2E_REFERENCE_URL || 'http://127.0.0.1:18080'
  const newUrl = process.env.E2E_NEW_URL || 'http://127.0.0.1:18084'
  const oldContext = await browser.newContext({ viewport: { width: 1440, height: 900 } })
  const newContext = await browser.newContext({ viewport: { width: 1440, height: 900 } })
  const oldPage = await oldContext.newPage()
  const newPage = await newContext.newPage()

  async function exercise(page: Page, baseUrl: string) {
    await mockMedia(page, baseUrl)
    await open(page, '山间来信.m4a')
    await expect(page.locator('audio')).toHaveJSProperty('readyState', 4)
    const trigger = page.locator('[data-panel-trigger="chapters"]')
    await trigger.click()
    await expect(page.locator('.audio-panel')).toBeVisible()
    await expect(page.locator('.audio-panel .media-icon-button')).toBeFocused()
    await page.keyboard.press('Escape')
    await expect(page.locator('.audio-panel')).toHaveCount(0)
    await expect(trigger).toBeFocused()
    await trigger.click()
    await expect(page.locator('.audio-panel')).toBeVisible()
    await trigger.click()
    await expect(page.locator('.audio-panel')).toHaveCount(0)
    await expect(trigger).toBeFocused()
    await trigger.click()
    await expect(page.locator('.audio-panel')).toBeVisible()
    await page.locator('.audio-panel .media-icon-button').click()
    await expect(page.locator('.audio-panel')).toHaveCount(0)
    await expect(trigger).toBeFocused()
  }

  try {
    await Promise.all([
      exercise(oldPage, oldUrl),
      exercise(newPage, newUrl),
    ])
  } finally {
    await Promise.all([oldContext.close(), newContext.close()])
  }
})

test('old/new 音频打开和切换章节时保持 reference 的当前项定位', async ({ browser }) => {
  const oldUrl = process.env.E2E_REFERENCE_URL || 'http://127.0.0.1:18080'
  const newUrl = process.env.E2E_NEW_URL || 'http://127.0.0.1:18084'
  const oldContext = await browser.newContext({ viewport: { width: 1440, height: 900 } })
  const newContext = await browser.newContext({ viewport: { width: 1440, height: 900 } })
  const oldPage = await oldContext.newPage()
  const newPage = await newContext.newPage()

  async function exercise(page: Page, baseUrl: string) {
    await page.addInitScript(() => {
      const state = window as Window & { __revaroAudioChapterReveals?: Array<unknown> }
      state.__revaroAudioChapterReveals = []
      const scrollIntoView = Element.prototype.scrollIntoView
      Element.prototype.scrollIntoView = function (options?: boolean | ScrollIntoViewOptions) {
        if (this.matches('.audio-panel [data-chapter-index]')) {
          state.__revaroAudioChapterReveals!.push({
            index: this.getAttribute('data-chapter-index'),
            block: typeof options === 'object' && options !== null ? options.block : options ?? null,
          })
        }
        return scrollIntoView.call(this, options)
      }
    })
    await mockMedia(page, baseUrl)
    await open(page, '山间来信.m4a')
    await page.getByRole('button', { name: '章节', exact: true }).click()
    await expect.poll(() => page.evaluate(() => (window as Window & { __revaroAudioChapterReveals?: Array<unknown> }).__revaroAudioChapterReveals?.length ?? 0)).toBe(1)
    await page.locator('[data-chapter-index="1"]').click()
    await expect(page.locator('[data-chapter-index="1"]')).toHaveAttribute('aria-current', 'true')
    await expect.poll(() => page.evaluate(() => (window as Window & { __revaroAudioChapterReveals?: Array<unknown> }).__revaroAudioChapterReveals?.length ?? 0)).toBe(2)
    return page.evaluate(() => (window as Window & { __revaroAudioChapterReveals?: Array<unknown> }).__revaroAudioChapterReveals)
  }

  try {
    const [oldResult, newResult] = await Promise.all([
      exercise(oldPage, oldUrl),
      exercise(newPage, newUrl),
    ])
    expect(oldResult).toEqual([
      { index: '0', block: 'nearest' },
      { index: '1', block: 'nearest' },
    ])
    expect(newResult, 'Rust 音频章节当前项定位与 reference 不一致').toEqual(oldResult)
  } finally {
    await Promise.all([oldContext.close(), newContext.close()])
  }
})

test('old/new 音频音量入口图标随静音状态保持 reference 几何', async ({ browser }) => {
  const oldUrl = process.env.E2E_REFERENCE_URL || 'http://127.0.0.1:18080'
  const newUrl = process.env.E2E_NEW_URL || 'http://127.0.0.1:18084'
  const oldContext = await browser.newContext({ viewport: { width: 1440, height: 900 } })
  const newContext = await browser.newContext({ viewport: { width: 1440, height: 900 } })
  const oldPage = await oldContext.newPage()
  const newPage = await newContext.newPage()

  async function exercise(page: Page, baseUrl: string) {
    await page.addInitScript(() => {
      localStorage.setItem('revaro-audio-volume', '0')
      localStorage.setItem('revaro-audio-muted', 'false')
    })
    await mockMedia(page, baseUrl)
    await open(page, '山间来信.m4a')
    const summary = page.locator('.audio-options .preview-menu > summary')
    const iconPath = () => summary.locator('svg path').evaluateAll(elements => elements.map(element => element.getAttribute('d')).join('|'))
    const silent = await iconPath()
    await summary.click()
    await page.locator('.audio-volume input').fill('0.5')
    const audible = await iconPath()
    await page.locator('.audio-volume button').click()
    const muted = await iconPath()
    return { silent, audible, muted }
  }

  try {
    const [oldResult, newResult] = await Promise.all([
      exercise(oldPage, oldUrl),
      exercise(newPage, newUrl),
    ])
    expect(oldResult.silent).not.toBe(oldResult.audible)
    expect(oldResult.muted).toBe(oldResult.silent)
    expect(newResult, 'Rust 音频音量入口的静音图标与 reference 不一致').toEqual(oldResult)
  } finally {
    await Promise.all([oldContext.close(), newContext.close()])
  }
})

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

test('old/new 媒体操作菜单保持下载预览、移动复制进入目标选择和原有文案', async ({ browser }) => {
  const oldUrl = process.env.E2E_REFERENCE_URL || 'http://127.0.0.1:18080'
  const newUrl = process.env.E2E_NEW_URL || 'http://127.0.0.1:18084'
  const oldContext = await browser.newContext({ viewport: { width: 1440, height: 900 } })
  const newContext = await browser.newContext({ viewport: { width: 1440, height: 900 } })

  async function exercise(context: BrowserContext, baseUrl: string) {
    const downloadPage = await context.newPage()
    await mockMedia(downloadPage, baseUrl)
    await open(downloadPage, '群山.png')
    await expect(downloadPage.locator('.preview-image')).toBeVisible()
    await downloadPage.locator('.preview-commandbar summary').click()
    const imageMenu = downloadPage.locator('.preview-menu[open]')
    const imageMenuResult = {
      actions: await imageMenu.getByRole('button').allTextContents(),
      detail: await imageMenu.locator('.media-detail').innerText(),
    }
    const downloadPromise = downloadPage.waitForEvent('download')
    await imageMenu.getByRole('button', { name: '下载', exact: true }).click()
    const download = await downloadPromise
    // The reference closes only the native details menu for a normal media
    // download; the preview itself remains open. Move/copy are different:
    // their parent action replaces the preview with the transfer dialog.
    await expect(downloadPage.locator('.preview-modal')).toHaveCount(1)
    const previewCountAfterDownload = await downloadPage.locator('.preview-modal').count()
    await downloadPage.close()

    const movePage = await context.newPage()
    await mockMedia(movePage, baseUrl)
    await open(movePage, '山间来信.m4a')
    await expect(movePage.locator('audio')).toHaveJSProperty('readyState', 4)
    await movePage.locator('.preview-commandbar summary').click()
    const audioMenu = movePage.locator('.preview-menu[open]')
    const audioMenuResult = {
      actions: await audioMenu.getByRole('button').allTextContents(),
      detail: await audioMenu.locator('.media-detail').innerText(),
    }
    await audioMenu.getByRole('button', { name: '移动', exact: true }).click()
    await expect(movePage.locator('.preview-modal')).toHaveCount(0)
    const moveDialog = movePage.locator('.move-copy-dialog')
    await expect(moveDialog).toBeVisible()
    const moveResult = {
      title: await moveDialog.getByRole('heading').innerText(),
      target: await moveDialog.locator('header p').last().innerText(),
    }
    await moveDialog.getByRole('button', { name: '取消', exact: true }).click()
    await expect(moveDialog).toHaveCount(0)
    await movePage.close()

    const copyPage = await context.newPage()
    await mockMedia(copyPage, baseUrl)
    await open(copyPage, '山间漫步.webm')
    await expect(copyPage.locator('.video-player-shell video')).toBeVisible()
    await copyPage.getByLabel('播放设置', { exact: true }).click()
    const videoMenu = copyPage.locator('.preview-menu[open]')
    const videoMenuResult = {
      actions: await videoMenu.getByRole('button').allTextContents(),
      detail: await videoMenu.locator('.media-detail').innerText(),
    }
    await videoMenu.getByRole('button', { name: '复制', exact: true }).click()
    await expect(copyPage.locator('.preview-modal')).toHaveCount(0)
    const copyDialog = copyPage.locator('.move-copy-dialog')
    await expect(copyDialog).toBeVisible()
    const copyResult = {
      title: await copyDialog.getByRole('heading').innerText(),
      target: await copyDialog.locator('header p').last().innerText(),
    }
    await copyDialog.getByRole('button', { name: '取消', exact: true }).click()
    await expect(copyDialog).toHaveCount(0)
    await copyPage.close()

    return {
      imageMenuResult,
      imageDownloadFilename: download.suggestedFilename(),
      previewCountAfterDownload,
      audioMenuResult,
      moveResult,
      videoMenuResult,
      copyResult,
    }
  }

  try {
    const [oldResult, newResult] = await Promise.all([
      exercise(oldContext, oldUrl),
      exercise(newContext, newUrl),
    ])
    expect(oldResult.imageMenuResult.actions).toEqual(['下载', '移动', '复制'])
    expect(oldResult.audioMenuResult.actions).toEqual(['下载', '移动', '复制'])
    expect(oldResult.videoMenuResult.actions).toEqual(['下载', '移动', '复制'])
    expect(oldResult.imageDownloadFilename).toBe('群山.png')
    expect(oldResult.previewCountAfterDownload).toBe(1)
    expect(oldResult.moveResult.title).toBe('移动到')
    expect(oldResult.copyResult.title).toBe('复制到')
    expect(newResult, 'Rust 媒体操作菜单与 reference 的完整操作结果不一致').toEqual(oldResult)
  } finally {
    await Promise.all([oldContext.close(), newContext.close()])
  }
})

test('old/new 图片未达到翻页阈值时保留拖动中的水平跟手位移', async ({ browser }) => {
  const oldUrl = process.env.E2E_REFERENCE_URL || 'http://127.0.0.1:18080'
  const newUrl = process.env.E2E_NEW_URL || 'http://127.0.0.1:18084'
  const oldContext = await browser.newContext({ viewport: { width: 1440, height: 900 } })
  const newContext = await browser.newContext({ viewport: { width: 1440, height: 900 } })
  const oldPage = await oldContext.newPage()
  const newPage = await newContext.newPage()

  async function exercise(page: Page, baseUrl: string) {
    await mockMedia(page, baseUrl)
    await open(page, '群山.png')
    const stage = page.locator('.preview-stage')
    const image = page.locator('.preview-image')
    await expect(image).toBeVisible()
    const bounds = await stage.boundingBox()
    expect(bounds).not.toBeNull()
    const centerX = bounds!.x + bounds!.width / 2
    const centerY = bounds!.y + bounds!.height / 2
    const initialX = await image.evaluate(element => new DOMMatrix(getComputedStyle(element).transform).e)
    await page.mouse.move(centerX, centerY)
    await page.mouse.down()
    // Keep the movement below the 60px gallery-switch threshold. The
    // reference still moves the fitted image with the pointer during this
    // live gesture, even though pointer-up does not change the item.
    await page.mouse.move(centerX + 24, centerY + 5)
    await page.waitForTimeout(40)
    const result = await image.evaluate(element => {
      const style = getComputedStyle(element)
      const transform = new DOMMatrix(style.transform)
      return { transform: style.transform, translateX: transform.e }
    })
    await page.mouse.up()
    return { ...result, dragDelta: result.translateX - initialX }
  }

  try {
    const [oldResult, newResult] = await Promise.all([
      exercise(oldPage, oldUrl),
      exercise(newPage, newUrl),
    ])
    expect(oldResult.dragDelta).toBeGreaterThan(15)
    expect(newResult, 'Rust 图片未达到翻页阈值时没有保持 reference 的跟手位移').toEqual(oldResult)
  } finally {
    await Promise.all([oldContext.close(), newContext.close()])
  }
})

test('old/new 图片缩略图栏切换后保持展开并重新定位当前缩略图', async ({ browser }) => {
  const oldUrl = process.env.E2E_REFERENCE_URL || 'http://127.0.0.1:18080'
  const newUrl = process.env.E2E_NEW_URL || 'http://127.0.0.1:18084'
  const oldContext = await browser.newContext({ viewport: { width: 1440, height: 900 } })
  const newContext = await browser.newContext({ viewport: { width: 1440, height: 900 } })
  const oldPage = await oldContext.newPage()
  const newPage = await newContext.newPage()

  async function exercise(page: Page, baseUrl: string) {
    await page.addInitScript(() => {
      const state = window as Window & { __revaroThumbnailReveals?: Array<unknown> }
      state.__revaroThumbnailReveals = []
      const scrollIntoView = Element.prototype.scrollIntoView
      Element.prototype.scrollIntoView = function (options?: boolean | ScrollIntoViewOptions) {
        if (this.matches('.preview-filmstrip [aria-current="true"]')) {
          state.__revaroThumbnailReveals!.push(
            typeof options === 'object' && options !== null
              ? { block: options.block, inline: options.inline }
              : options ?? null,
          )
        }
        return scrollIntoView.call(this, options)
      }
    })
    await mockMedia(page, baseUrl)
    await open(page, '群山.png')
    await page.getByRole('button', { name: '缩略图', exact: true }).click()
    await expect(page.locator('.preview-filmstrip')).toBeVisible()
    await page.getByRole('button', { name: '查看 远山.png' }).click()
    await expect(page.locator('.preview-file-meta')).toHaveText('远山.png')
    await expect(page.locator('.preview-filmstrip')).toBeVisible()
    return page.evaluate(() => ({
      current: document.querySelector('.preview-filmstrip [aria-current="true"] img')?.getAttribute('alt'),
      reveals: (window as Window & { __revaroThumbnailReveals?: Array<unknown> }).__revaroThumbnailReveals,
    }))
  }

  try {
    const [oldResult, newResult] = await Promise.all([
      exercise(oldPage, oldUrl),
      exercise(newPage, newUrl),
    ])
    expect(oldResult.current).toBe('远山.png')
    expect(oldResult.reveals).toEqual([
      { block: 'nearest', inline: 'center' },
      { block: 'nearest', inline: 'center' },
    ])
    expect(newResult, 'Rust 缩略图栏切换后的展开和当前项定位与 reference 不一致').toEqual(oldResult)
  } finally {
    await Promise.all([oldContext.close(), newContext.close()])
  }
})

test('图片预览：从根节点按 Tab 首先进入更多操作菜单', async ({ page }) => {
  await mockMedia(page)
  await open(page, '群山.png')
  await page.locator('.preview-modal').focus()
  await page.keyboard.press('Tab')
  await expect(page.locator('.preview-commandbar summary')).toBeFocused()
})

test('old/new 图片双击沿用旧版实测不变焦，缩放控件边界状态一致', async ({ browser }) => {
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
    const actual = page.locator('.preview-actual-size')
    const zoomIn = page.getByRole('button', { name: '放大', exact: true })
    const zoomOut = page.getByRole('button', { name: '缩小', exact: true })
    const fit = page.getByRole('button', { name: '适应窗口', exact: true })
    const fitPercent = await actual.innerText()
    const initial = { fitPercent, zoomInDisabled: await zoomIn.isDisabled(), zoomOutDisabled: await zoomOut.isDisabled() }

    await page.evaluate(() => {
      document.addEventListener('dblclick', event => {
        const target = event.target
        document.documentElement.dataset.compatDblclickTarget = target instanceof Element ? target.className.toString() : ''
      }, { capture: true, once: true })
    })
    await image.dblclick()
    await expect(actual).toHaveText(fitPercent)
    const doubleClickTarget = await page.locator('html').getAttribute('data-compat-dblclick-target')
    expect(doubleClickTarget).toContain('preview-stage')
    const afterDoubleClick = { percent: await actual.innerText(), zoomOutDisabled: await zoomOut.isDisabled() }
    await image.dblclick()
    await expect(actual).toHaveText(fitPercent)
    const returnedToFit = { percent: await actual.innerText(), zoomOutDisabled: await zoomOut.isDisabled() }

    let zoomSteps = 0
    while (await zoomIn.isEnabled() && zoomSteps < 24) {
      await zoomIn.click()
      zoomSteps += 1
    }
    const maximum = {
      percent: await actual.innerText(),
      zoomInDisabled: await zoomIn.isDisabled(),
      zoomOutDisabled: await zoomOut.isDisabled(),
      zoomSteps,
    }
    await fit.click()
    const reset = {
      percent: await actual.innerText(),
      zoomInDisabled: await zoomIn.isDisabled(),
      zoomOutDisabled: await zoomOut.isDisabled(),
    }
    return { initial, afterDoubleClick, returnedToFit, maximum, reset }
  }

  try {
    const [oldResult, newResult] = await Promise.all([
      exercise(oldPage, oldUrl),
      exercise(newPage, newUrl),
    ])
    expect(oldResult.initial.zoomInDisabled).toBe(false)
    expect(oldResult.initial.zoomOutDisabled).toBe(true)
    expect(oldResult.afterDoubleClick).toEqual({ percent: oldResult.initial.fitPercent, zoomOutDisabled: true })
    expect(oldResult.returnedToFit).toEqual({ percent: oldResult.initial.fitPercent, zoomOutDisabled: true })
    expect(oldResult.maximum.zoomSteps).toBeGreaterThan(0)
    expect(oldResult.maximum.zoomInDisabled).toBe(true)
    expect(oldResult.maximum.zoomOutDisabled).toBe(false)
    expect(oldResult.maximum.zoomSteps).toBeLessThanOrEqual(24)
    expect(oldResult.reset.percent).toBe(oldResult.initial.fitPercent)
    expect(oldResult.reset.zoomInDisabled).toBe(false)
    expect(oldResult.reset.zoomOutDisabled).toBe(true)
    expect(newResult, 'Rust 图片双击缩放/边界控件状态与 reference 不一致').toEqual(oldResult)
  } finally {
    await Promise.all([oldContext.close(), newContext.close()])
  }
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

test('old/new 视频音量归零后点击静音按钮恢复最近一次可听音量', async ({ browser }) => {
  const oldUrl = process.env.E2E_REFERENCE_URL || 'http://127.0.0.1:18080'
  const newUrl = process.env.E2E_NEW_URL || 'http://127.0.0.1:18084'
  const oldContext = await browser.newContext({ viewport: { width: 1440, height: 900 } })
  const newContext = await browser.newContext({ viewport: { width: 1440, height: 900 } })
  const oldPage = await oldContext.newPage()
  const newPage = await newContext.newPage()

  async function exercise(page: Page, baseUrl: string) {
    await mockMedia(page, baseUrl)
    await page.evaluate(() => localStorage.setItem('revaro-video-volume', '0.27'))
    await open(page, '山间漫步.webm')
    const video = page.locator('.video-player-shell video')
    await expect.poll(() => video.evaluate((element: HTMLVideoElement) => element.volume)).toBeCloseTo(0.27, 2)
    const volume = page.locator('.video-volume').first()
    await volume.evaluate(element => {
      const input = element as HTMLInputElement
      input.value = '0'
      input.dispatchEvent(new Event('input', { bubbles: true }))
    })
    await expect.poll(() => video.evaluate((element: HTMLVideoElement) => element.volume)).toBe(0)
    await expect(page.getByRole('button', { name: '取消静音', exact: true })).toBeVisible()
    await page.getByRole('button', { name: '取消静音', exact: true }).click()
    return {
      volume: await video.evaluate((element: HTMLVideoElement) => element.volume),
      muted: await video.evaluate((element: HTMLVideoElement) => element.muted),
      button: await page.locator('.video-desktop-volume button').getAttribute('aria-label'),
      stored: await page.evaluate(() => localStorage.getItem('revaro-video-volume')),
    }
  }

  try {
    const [oldResult, newResult] = await Promise.all([
      exercise(oldPage, oldUrl),
      exercise(newPage, newUrl),
    ])
    expect(oldResult.volume).toBeCloseTo(0.27, 2)
    expect(oldResult.muted).toBe(false)
    expect(newResult, 'Rust 视频归零后取消静音未恢复 reference 音量').toEqual(oldResult)
  } finally {
    await Promise.all([oldContext.close(), newContext.close()])
  }
})

test('old/new 视频从零音量恢复时保留 reference 的原生 muted 状态', async ({ browser }) => {
  const oldUrl = process.env.E2E_REFERENCE_URL || 'http://127.0.0.1:18080'
  const newUrl = process.env.E2E_NEW_URL || 'http://127.0.0.1:18084'
  const oldContext = await browser.newContext({ viewport: { width: 1440, height: 900 } })
  const newContext = await browser.newContext({ viewport: { width: 1440, height: 900 } })
  const oldPage = await oldContext.newPage()
  const newPage = await newContext.newPage()

  async function exercise(page: Page, baseUrl: string) {
    await page.addInitScript(() => localStorage.setItem('revaro-video-volume', '0'))
    await mockMedia(page, baseUrl)
    await open(page, '山间漫步.webm')
    const video = page.locator('.video-player-shell video')
    await expect(video).toHaveJSProperty('readyState', 4)
    return video.evaluate((element: HTMLVideoElement) => ({
      volume: element.volume,
      muted: element.muted,
      ariaLabel: document.querySelector('.video-desktop-volume button')?.getAttribute('aria-label'),
    }))
  }

  try {
    const [oldResult, newResult] = await Promise.all([
      exercise(oldPage, oldUrl),
      exercise(newPage, newUrl),
    ])
    expect(oldResult).toEqual({ volume: 0, muted: true, ariaLabel: '取消静音' })
    expect(newResult, 'Rust 视频零音量恢复的原生 muted 状态与 reference 不一致').toEqual(oldResult)
  } finally {
    await Promise.all([oldContext.close(), newContext.close()])
  }
})

test('old/new 视频控制条悬停时不自动隐藏', async ({ browser }) => {
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
    await expect.poll(() => video.evaluate((element: HTMLVideoElement) => !element.paused)).toBe(true)
    const controls = page.locator('.video-controls')
    await controls.hover()
    await expect(controls).toHaveClass(/visible/)
    await page.waitForTimeout(3_200)
    return controls.evaluate(element => ({
      visible: element.classList.contains('visible'),
      hovered: element.matches(':hover'),
    }))
  }

  try {
    const [oldResult, newResult] = await Promise.all([
      exercise(oldPage, oldUrl),
      exercise(newPage, newUrl),
    ])
    expect(oldResult).toEqual({ visible: true, hovered: true })
    expect(newResult, 'Rust 视频控制条悬停自动隐藏与 reference 不一致').toEqual(oldResult)
  } finally {
    await Promise.all([oldContext.close(), newContext.close()])
  }
})

test('old/new 视频隐藏控制条的 inert 与 range 无障碍文本保持一致', async ({ browser }) => {
  const oldUrl = process.env.E2E_REFERENCE_URL || 'http://127.0.0.1:18080'
  const newUrl = process.env.E2E_NEW_URL || 'http://127.0.0.1:18084'
  const oldContext = await browser.newContext({ viewport: { width: 1440, height: 900 } })
  const newContext = await browser.newContext({ viewport: { width: 1440, height: 900 } })
  const oldPage = await oldContext.newPage()
  const newPage = await newContext.newPage()

  async function exercise(page: Page, baseUrl: string) {
    await mockMedia(page, baseUrl)
    await page.evaluate(() => localStorage.setItem('revaro-video-volume', '0.27'))
    await open(page, '山间漫步.webm')
    const video = page.locator('.video-player-shell video')
    await expect(video).toHaveJSProperty('paused', false)
    // The reference restores the mocked server position asynchronously. Wait
    // for that observable state before comparing the two independently timed
    // browser contexts; otherwise old can be sampled at 0:00 while Rust has
    // already reached the same eventual 0:10.
    await expect(page.locator('.video-seek')).toHaveAttribute('aria-valuetext', '0:10')
    const rangeState = {
      seek: await page.locator('.video-seek').getAttribute('aria-valuetext'),
      volume: await page.locator('.video-volume').getAttribute('aria-valuetext'),
    }

    await video.focus()
    await page.mouse.move(0, 0)
    await page.waitForTimeout(3200)
    await page.evaluate(() => (document.activeElement as HTMLElement | null)?.blur())
    await expect(page.locator('.video-controls')).toBeHidden()
    return {
      ...rangeState,
      controlsInert: await page.locator('.video-controls').getAttribute('inert'),
      topShadeInert: await page.locator('.video-top-shade').getAttribute('inert'),
    }
  }

  try {
    const [oldResult, newResult] = await Promise.all([
      exercise(oldPage, oldUrl),
      exercise(newPage, newUrl),
    ])
    expect(oldResult.seek).not.toBeNull()
    expect(oldResult.volume).not.toBeNull()
    expect(oldResult.controlsInert).not.toBeNull()
    expect(oldResult.topShadeInert).not.toBeNull()
    expect(newResult, 'Rust 视频隐藏控制条或 range 无障碍属性与 reference 不一致').toEqual(oldResult)
  } finally {
    await Promise.all([oldContext.close(), newContext.close()])
  }
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
    // Let the initial autoplay settle before pausing; otherwise a late play()
    // resolution can advance only one browser between the debounce samples.
    await expect.poll(() => audio.evaluate((element: HTMLAudioElement) => element.paused)).toBe(false)
    await audio.evaluate((element: HTMLAudioElement) => {
      element.pause()
      element.currentTime = 10
      element.dispatchEvent(new Event('timeupdate'))
    })
    await expect(audio).toHaveJSProperty('paused', true)
    await expect.poll(() => audio.evaluate((element: HTMLAudioElement) => element.currentTime)).toBe(10)
    // Drain the normalization seek's own 500ms save timer before observing
    // the user seek; otherwise parallel old/new pages can sample opposite
    // sides of that unrelated timer.
    await page.waitForTimeout(700)
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

test('old/new 字幕轨道错误不会清除已经显示的 cue', async ({ browser }) => {
  const oldUrl = process.env.E2E_REFERENCE_URL || 'http://127.0.0.1:18080'
  const newUrl = process.env.E2E_NEW_URL || 'http://127.0.0.1:18084'
  const oldContext = await browser.newContext({ viewport: { width: 1440, height: 900 } })
  const newContext = await browser.newContext({ viewport: { width: 1440, height: 900 } })
  const oldPage = await oldContext.newPage()
  const newPage = await newContext.newPage()

  async function exercise(page: Page, baseUrl: string) {
    await mockMedia(page, baseUrl)
    await open(page, '山间漫步.webm')
    await expect(page.locator('.video-subtitle-overlay')).toBeVisible()
    const track = page.locator('track').first()
    await track.evaluate(element => element.dispatchEvent(new Event('error')))
    await page.waitForTimeout(100)
    const overlay = page.locator('.video-subtitle-overlay')
    if (await overlay.count() === 0) return { count: 0, text: null, lines: [] }
    return overlay.evaluate(element => ({
      count: 1,
      text: element.textContent,
      lines: Array.from(element.querySelectorAll('span')).map(line => line.textContent),
    }))
  }

  try {
    const [oldResult, newResult] = await Promise.all([
      exercise(oldPage, oldUrl),
      exercise(newPage, newUrl),
    ])
    expect(oldResult).toEqual({
      count: 1,
      text: '沿着山间的小路，慢慢走。',
      lines: ['沿着山间的小路，慢慢走。'],
    })
    expect(newResult, 'Rust 字幕轨道错误时错误地清除了 reference 已显示 cue').toEqual(oldResult)
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

test('old/new 图片键盘快捷键保持翻页、缩放和默认事件语义', async ({ browser }) => {
  const oldUrl = process.env.E2E_REFERENCE_URL || 'http://127.0.0.1:18080'
  const newUrl = process.env.E2E_NEW_URL || 'http://127.0.0.1:18084'
  const oldContext = await browser.newContext({ viewport: { width: 1440, height: 900 } })
  const newContext = await browser.newContext({ viewport: { width: 1440, height: 900 } })
  const oldPage = await oldContext.newPage()
  const newPage = await newContext.newPage()

  async function exercise(page: Page, baseUrl: string) {
    await mockMedia(page, baseUrl)
    await open(page, '群山.png')
    await expect(page.locator('.preview-image')).toBeVisible()

    const dispatch = async (key: string) => page.evaluate(keyValue => new Promise<boolean>(resolve => {
      const handler = (event: KeyboardEvent) => {
        window.removeEventListener('keydown', handler)
        resolve(event.defaultPrevented)
      }
      window.addEventListener('keydown', handler)
      document.activeElement?.dispatchEvent(new KeyboardEvent('keydown', {
        key: keyValue,
        bubbles: true,
        cancelable: true,
      }))
    }), key)

    const initial = await page.locator('.preview-image').evaluate(element => ({
      transform: getComputedStyle(element).transform,
      percent: document.querySelector('.preview-actual-size')?.textContent?.trim() ?? '',
      selected: document.querySelector('.preview-file-meta')?.textContent?.trim() ?? '',
    }))
    const plusPrevented = await dispatch('=')
    const enlarged = await page.locator('.preview-image').evaluate(element => ({
      transform: getComputedStyle(element).transform,
      percent: document.querySelector('.preview-actual-size')?.textContent?.trim() ?? '',
      selected: document.querySelector('.preview-file-meta')?.textContent?.trim() ?? '',
    }))
    const minusPrevented = await dispatch('-')
    const fit = await page.locator('.preview-image').evaluate(element => ({
      transform: getComputedStyle(element).transform,
      percent: document.querySelector('.preview-actual-size')?.textContent?.trim() ?? '',
      selected: document.querySelector('.preview-file-meta')?.textContent?.trim() ?? '',
    }))
    const actualPrevented = await dispatch('1')
    const actual = await page.locator('.preview-image').evaluate(element => ({
      transform: getComputedStyle(element).transform,
      percent: document.querySelector('.preview-actual-size')?.textContent?.trim() ?? '',
      selected: document.querySelector('.preview-file-meta')?.textContent?.trim() ?? '',
    }))
    const fitPrevented = await dispatch('0')
    const refit = await page.locator('.preview-image').evaluate(element => ({
      transform: getComputedStyle(element).transform,
      percent: document.querySelector('.preview-actual-size')?.textContent?.trim() ?? '',
      selected: document.querySelector('.preview-file-meta')?.textContent?.trim() ?? '',
    }))
    const nextPrevented = await dispatch('ArrowRight')
    await expect(page.locator('.preview-file-meta')).toHaveText('远山.png')
    const next = await page.locator('.preview-file-meta').innerText()
    const previousPrevented = await dispatch('ArrowLeft')
    await expect(page.locator('.preview-file-meta')).toHaveText('群山.png')
    const previous = await page.locator('.preview-file-meta').innerText()

    return {
      initial,
      enlarged,
      fit,
      actual,
      refit,
      defaultPrevented: { plusPrevented, minusPrevented, actualPrevented, fitPrevented, nextPrevented, previousPrevented },
      next,
      previous,
    }
  }

  try {
    const [oldResult, newResult] = await Promise.all([
      exercise(oldPage, oldUrl),
      exercise(newPage, newUrl),
    ])
    expect(oldResult.defaultPrevented).toEqual({
      plusPrevented: true,
      minusPrevented: true,
      actualPrevented: false,
      fitPrevented: false,
      nextPrevented: true,
      previousPrevented: true,
    })
    expect(newResult, 'Rust 图片键盘翻页/缩放与 reference 不一致').toEqual(oldResult)
  } finally {
    await Promise.all([oldContext.close(), newContext.close()])
  }
})

test('old/new 视频键盘快捷键保持播放、seek、静音和默认事件语义', async ({ browser }) => {
  const oldUrl = process.env.E2E_REFERENCE_URL || 'http://127.0.0.1:18080'
  const newUrl = process.env.E2E_NEW_URL || 'http://127.0.0.1:18084'
  const oldContext = await browser.newContext({ viewport: { width: 1440, height: 900 } })
  const newContext = await browser.newContext({ viewport: { width: 1440, height: 900 } })
  const oldPage = await oldContext.newPage()
  const newPage = await newContext.newPage()

  async function exercise(page: Page, baseUrl: string) {
    await page.addInitScript(() => {
      localStorage.setItem('revaro-video-volume', '0.9')
    })
    await mockMedia(page, baseUrl)
    await open(page, '山间漫步.webm')
    const shell = page.locator('.video-player-shell')
    const video = shell.locator('video')
    await expect(video).toHaveJSProperty('readyState', 4)
    await shell.focus()

    await video.evaluate((element: HTMLVideoElement) => {
      element.muted = false
      element.volume = 0.9
    })
    const dispatch = async (key: string) => page.evaluate(keyValue => {
      const target = document.querySelector<HTMLElement>('.video-player-shell')
      if (!target) throw new Error('视频播放器未挂载')
      const event = new KeyboardEvent('keydown', { key: keyValue, bubbles: true, cancelable: true })
      target.dispatchEvent(event)
      return event.defaultPrevented
    }, key)

    await video.evaluate((element: HTMLVideoElement) => element.pause())
    const spacePrevented = await dispatch(' ')
    await expect.poll(() => video.evaluate((element: HTMLVideoElement) => element.paused)).toBe(false)
    const kPrevented = await dispatch('k')
    await expect.poll(() => video.evaluate((element: HTMLVideoElement) => element.paused)).toBe(true)

    await video.evaluate((element: HTMLVideoElement) => {
      element.currentTime = 20
      element.dispatchEvent(new Event('timeupdate'))
    })
    const leftPrevented = await dispatch('ArrowLeft')
    await expect.poll(() => video.evaluate((element: HTMLVideoElement) => element.currentTime)).toBe(15)
    const rightPrevented = await dispatch('ArrowRight')
    await expect.poll(() => video.evaluate((element: HTMLVideoElement) => element.currentTime)).toBe(20)

    const mutedBefore = await video.evaluate((element: HTMLVideoElement) => element.muted)
    const mutePrevented = await dispatch('m')
    await expect.poll(() => video.evaluate((element: HTMLVideoElement) => element.muted)).toBe(!mutedBefore)
    const mutedAfter = await video.evaluate((element: HTMLVideoElement) => element.muted)
    const unmutePrevented = await dispatch('m')
    await expect.poll(() => video.evaluate((element: HTMLVideoElement) => element.muted)).toBe(mutedBefore)
    const restored = await video.evaluate((element: HTMLVideoElement) => element.muted === false)

    await page.getByRole('button', { name: '退出播放', exact: true }).click()
    await expect(page.locator('.preview-modal')).toHaveCount(0)
    return {
      defaultPrevented: { spacePrevented, kPrevented, leftPrevented, rightPrevented, mutePrevented, unmutePrevented },
      toggled: mutedAfter === !mutedBefore,
      restored,
    }
  }

  try {
    const [oldResult, newResult] = await Promise.all([
      exercise(oldPage, oldUrl),
      exercise(newPage, newUrl),
    ])
    expect(oldResult.defaultPrevented).toEqual({
      spacePrevented: true,
      kPrevented: true,
      leftPrevented: true,
      rightPrevented: true,
      mutePrevented: false,
      unmutePrevented: false,
    })
    expect(oldResult.toggled).toBe(true)
    expect(oldResult.restored).toBe(true)
    expect(newResult, 'Rust 视频键盘快捷键与 reference 不一致').toEqual(oldResult)
  } finally {
    await Promise.all([oldContext.close(), newContext.close()])
  }
})

test('old/new 音频键盘快捷键保持播放、章节关闭、seek 和默认事件语义', async ({ browser }) => {
  const oldUrl = process.env.E2E_REFERENCE_URL || 'http://127.0.0.1:18080'
  const newUrl = process.env.E2E_NEW_URL || 'http://127.0.0.1:18084'
  const oldContext = await browser.newContext({ viewport: { width: 1440, height: 900 } })
  const newContext = await browser.newContext({ viewport: { width: 1440, height: 900 } })
  const oldPage = await oldContext.newPage()
  const newPage = await newContext.newPage()

  async function exercise(page: Page, baseUrl: string) {
    await mockMedia(page, baseUrl)
    await open(page, '山间来信.m4a')
    const shell = page.locator('.chapter-audio-player')
    const audio = page.locator('audio')
    await expect(audio).toHaveJSProperty('readyState', 4)
    await page.evaluate(() => {
      const state = window as typeof window & {
        __lastAudioKeyEvent?: KeyboardEvent
        __audioKeyReachedWindowBubble?: boolean
        __audioKeyTargetTag?: string
        __audioKeyTargetAriaLabel?: string | null
      }
      window.addEventListener('keydown', event => {
        state.__lastAudioKeyEvent = event
        state.__audioKeyReachedWindowBubble = false
        const target = event.target
        state.__audioKeyTargetTag = target instanceof Element ? target.tagName : ''
        state.__audioKeyTargetAriaLabel = target instanceof Element ? target.getAttribute('aria-label') : null
      }, true)
      window.addEventListener('keydown', event => {
        if (state.__lastAudioKeyEvent === event) state.__audioKeyReachedWindowBubble = true
      })
    })
    const press = async (key: string) => {
      await page.keyboard.press(key)
      return page.evaluate(() => {
        const state = window as typeof window & {
          __lastAudioKeyEvent?: KeyboardEvent
          __audioKeyReachedWindowBubble?: boolean
          __audioKeyTargetTag?: string
          __audioKeyTargetAriaLabel?: string | null
        }
        const event = state.__lastAudioKeyEvent
        if (!event) throw new Error('没有观察到音频播放器键盘事件')
        return {
          key: event.key,
          defaultPrevented: event.defaultPrevented,
          reachedWindowBubble: state.__audioKeyReachedWindowBubble,
          targetTag: state.__audioKeyTargetTag,
          targetAriaLabel: state.__audioKeyTargetAriaLabel,
        }
      })
    }

    const chapterTrigger = page.locator('[data-panel-trigger="chapters"]')
    await chapterTrigger.click()
    await expect(page.locator('.audio-panel')).toBeVisible()
    const chapterEscape = await press('Escape')
    await expect(page.locator('.audio-panel')).toHaveCount(0)
    await expect(page.locator('.preview-modal')).toBeVisible()
    await expect(chapterTrigger).toBeFocused()

    await audio.evaluate((element: HTMLAudioElement) => element.pause())
    await shell.focus()
    const shortcutPlay = await press('Space')
    await expect.poll(() => audio.evaluate((element: HTMLAudioElement) => element.paused)).toBe(false)
    const shortcutPause = await press('Space')
    await expect.poll(() => audio.evaluate((element: HTMLAudioElement) => element.paused)).toBe(true)

    // A focused transport button must retain its native Space-to-click behavior;
    // the player's global shortcut deliberately ignores interactive descendants.
    const playButton = page.getByRole('button', { name: '播放', exact: true })
    await playButton.focus()
    const buttonPlay = await press('Space')
    await expect.poll(() => audio.evaluate((element: HTMLAudioElement) => element.paused)).toBe(false)
    const pauseButton = page.getByRole('button', { name: '暂停', exact: true })
    await expect(pauseButton).toBeFocused()
    const buttonPause = await press('Space')
    await expect.poll(() => audio.evaluate((element: HTMLAudioElement) => element.paused)).toBe(true)

    await audio.evaluate((element: HTMLAudioElement) => {
      element.currentTime = 50
      element.dispatchEvent(new Event('timeupdate'))
    })
    await expect.poll(() => audio.evaluate((element: HTMLAudioElement) => element.currentTime)).toBe(50)
    await shell.focus()
    const left = await press('ArrowLeft')
    await expect.poll(() => audio.evaluate((element: HTMLAudioElement) => element.currentTime)).toBe(35)
    const right = await press('ArrowRight')
    await expect.poll(() => audio.evaluate((element: HTMLAudioElement) => element.currentTime)).toBe(65)

    const pausedBeforeClose = await audio.evaluate((element: HTMLAudioElement) => element.paused)
    await page.getByRole('button', { name: '关闭预览', exact: true }).click()
    await expect(page.locator('.preview-modal')).toHaveCount(0)
    return {
      chapterEscape,
      shortcutPlay,
      shortcutPause,
      buttonPlay,
      buttonPause,
      left,
      right,
      state: {
        chapterClosed: await page.locator('.audio-panel').count() === 0,
        previewClosed: await page.locator('.preview-modal').count() === 0,
        pausedBeforeClose,
      },
    }
  }

  try {
    const [oldResult, newResult] = await Promise.all([
      exercise(oldPage, oldUrl),
      exercise(newPage, newUrl),
    ])
    expect(oldResult.chapterEscape).toEqual({
      key: 'Escape', defaultPrevented: true, reachedWindowBubble: false,
      targetTag: 'BUTTON', targetAriaLabel: '收起面板',
    })
    for (const event of [oldResult.shortcutPlay, oldResult.shortcutPause, oldResult.left, oldResult.right]) {
      expect(event.defaultPrevented).toBe(true)
      expect(event.reachedWindowBubble).toBe(true)
    }
    for (const event of [oldResult.buttonPlay, oldResult.buttonPause]) {
      expect(event.key).toBe(' ')
      expect(event.defaultPrevented).toBe(false)
      expect(event.reachedWindowBubble).toBe(true)
      expect(event.targetTag).toBe('BUTTON')
    }
    expect(oldResult.state).toEqual({ chapterClosed: true, previewClosed: true, pausedBeforeClose: true })
    expect(newResult, 'Rust 音频键盘快捷键与 reference 不一致').toEqual(oldResult)
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

test('old/new 触屏唤出视频控制条后按 reference 自动收起', async ({ browser }) => {
  const oldUrl = process.env.E2E_REFERENCE_URL || 'http://127.0.0.1:18080'
  const newUrl = process.env.E2E_NEW_URL || 'http://127.0.0.1:18084'
  const oldContext = await browser.newContext({ viewport: { width: 390, height: 844 }, isMobile: true, hasTouch: true })
  const newContext = await browser.newContext({ viewport: { width: 390, height: 844 }, isMobile: true, hasTouch: true })
  const oldPage = await oldContext.newPage()
  const newPage = await newContext.newPage()

  async function exercise(page: Page, baseUrl: string) {
    await mockMedia(page, baseUrl)
    await open(page, '山间漫步.webm')
    const shell = page.locator('.video-player-shell')
    const video = shell.locator('video')
    const controls = page.locator('.video-controls')
    await expect(video).toHaveJSProperty('paused', false)
    await shell.evaluate((element: HTMLElement) => element.blur())
    await expect(controls).toBeHidden({ timeout: 5_000 })
    await page.touchscreen.tap(195, 400)
    await expect(controls).toBeVisible()
    await expect(video).toHaveJSProperty('paused', false)
    await page.waitForTimeout(3_100)
    return {
      controlsHiddenAfterTapTimeout: await controls.isHidden(),
      videoStillPlaying: !(await video.evaluate((element: HTMLVideoElement) => element.paused)),
      controlsClass: await controls.getAttribute('class'),
    }
  }

  try {
    const [oldResult, newResult] = await Promise.all([
      exercise(oldPage, oldUrl),
      exercise(newPage, newUrl),
    ])
    expect(oldResult.controlsHiddenAfterTapTimeout).toBe(true)
    expect(oldResult.videoStillPlaying).toBe(true)
    expect(newResult, 'Rust 触屏唤出控制条后未按 reference 自动收起').toEqual(oldResult)
  } finally {
    await Promise.all([oldContext.close(), newContext.close()])
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
    const shell = page.locator('.video-player-shell')
    const controls = page.locator('.video-controls')
    await expect(video).toHaveJSProperty('paused', false)
    await shell.evaluate((element: HTMLElement) => element.blur())
    await expect(controls).toBeHidden({ timeout: 5_000 })
    await page.evaluate(() => {
      const state = window as typeof window & { __mediaParityPointerTypes?: string[] }
      state.__mediaParityPointerTypes = []
      document.querySelector('.video-player-shell')?.addEventListener('pointerdown', event => {
        state.__mediaParityPointerTypes?.push((event as PointerEvent).pointerType)
      }, true)
    })
    const controlsState = () => page.locator('.video-controls').evaluate(element => {
      const style = getComputedStyle(element)
      return {
        className: element.className,
        visibility: style.visibility,
        opacity: style.opacity,
        pointerEvents: style.pointerEvents,
        inert: (element as HTMLElement).inert,
      }
    })
    await page.touchscreen.tap(195, 400)
    await expect(controls).toBeVisible()
    await page.waitForTimeout(220)
    const hiddenAfterFirstTap = await controls.isHidden()
    const controlsAfterFirstTap = await controlsState()
    const playingAfterFirstTap = !(await video.evaluate((element: HTMLVideoElement) => element.paused))
    await page.touchscreen.tap(195, 400)
    await expect(controls).toBeHidden()
    await page.waitForTimeout(220)
    const visibleAfterSecondTap = await controls.isVisible()
    const controlsAfterSecondTap = await controlsState()
    const playingAfterSecondTap = !(await video.evaluate((element: HTMLVideoElement) => element.paused))
    const pointerTypes = await page.evaluate(() => (window as typeof window & { __mediaParityPointerTypes?: string[] }).__mediaParityPointerTypes ?? [])
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
      controlsAfterFirstTap,
      playingAfterFirstTap,
      visibleAfterSecondTap,
      controlsAfterSecondTap,
      playingAfterSecondTap,
      pointerTypes,
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

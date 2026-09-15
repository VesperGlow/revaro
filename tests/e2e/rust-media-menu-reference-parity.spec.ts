import { expect, test, type Page } from '@playwright/test'
import { readFileSync } from 'node:fs'

const ROOT = '00000000-0000-0000-0000-000000000000'
const STAMP = '2026-01-01T00:00:00Z'
const files = [
  { id: 'menu-audio', name: '菜单音频.m4a', mime_type: 'audio/mp4' },
  { id: 'menu-video', name: '菜单视频.webm', mime_type: 'video/webm' },
].map(file => ({
  ...file,
  parent_id: ROOT,
  kind: 'file',
  size: 200_000,
  status: 'ready',
  created_at: STAMP,
  updated_at: STAMP,
}))

function wav() {
  const length = 8_000 * 120
  const buffer = Buffer.alloc(44 + length * 2)
  buffer.write('RIFF')
  buffer.writeUInt32LE(36 + length * 2, 4)
  buffer.write('WAVEfmt ', 8)
  buffer.writeUInt32LE(16, 16)
  buffer.writeUInt16LE(1, 20)
  buffer.writeUInt16LE(1, 22)
  buffer.writeUInt32LE(8_000, 24)
  buffer.writeUInt32LE(16_000, 28)
  buffer.writeUInt16LE(2, 32)
  buffer.writeUInt16LE(16, 34)
  buffer.write('data', 36)
  buffer.writeUInt32LE(length * 2, 40)
  return buffer
}

const video = readFileSync(new URL('./fixtures/preview.webm', import.meta.url))

async function mockMedia(page: Page) {
  const sound = wav()
  await page.route('**/api/**', async route => {
    const path = new URL(route.request().url()).pathname
    const json = (value: unknown) => route.fulfill({ json: value })
    if (path === '/api/auth/me') return json({ username: 'admin', has_avatar: false })
    if (path === '/api/events' || path === '/api/system/status/stream') {
      return route.fulfill({ contentType: 'text/event-stream', body: '' })
    }
    if (path === '/api/tasks') return json({ items: [] })
    if (path === `/api/files/${ROOT}`) {
      return json({
        file: { id: ROOT, parent_id: null, name: '我的文件', kind: 'directory', size: 0, status: 'ready', created_at: STAMP, updated_at: STAMP },
        breadcrumbs: [],
      })
    }
    if (path === `/api/files/${ROOT}/children`) return json({ items: files, total_bytes: 400_000, file_count: files.length })
    if (path === '/api/files/menu-audio/audio') {
      return json({
        duration: 120,
        has_cover: false,
        chapters: [
          { id: 1, title: '第一章', start: 0, end: 60 },
          { id: 2, title: '第二章', start: 60, end: 120 },
        ],
      })
    }
    if (path === '/api/files/menu-video/video') {
      return json({ subtitles: [{ id: 'zh', name: 'zh', label: '简体中文', language: 'zh', url: '/api/menu-subtitle.vtt', default: true }] })
    }
    if (path === '/api/menu-subtitle.vtt') return route.fulfill({ contentType: 'text/vtt', body: 'WEBVTT\n\n00:00:00.000 --> 00:00:30.000\n菜单测试字幕。\n' })
    if (path.endsWith('/media/progress')) return json({ position: 10, duration: 120 })
    if (path.endsWith('/thumbnail') || path.endsWith('/preview')) {
      const body = path.includes('menu-audio') ? sound : video
      return route.fulfill({ contentType: path.includes('menu-audio') ? 'audio/wav' : 'video/webm', body })
    }
    return json({ items: [] })
  })
}

async function open(page: Page, baseUrl: string, name: string) {
  await page.goto(`${baseUrl}/?media-menu-parity=${Date.now()}`)
  await expect(page.getByRole('heading', { name: '我的文件', exact: true })).toBeVisible()
  await page.locator('.file-card').filter({ hasText: name }).click()
  await expect(page.locator('.preview-modal')).toBeVisible()
}

async function menuMetrics(page: Page, selector: string) {
  return page.locator(selector).evaluate(menu => {
    const summary = menu.querySelector('summary')!
    const panel = menu.querySelector('.preview-menu-panel')!
    const relevant = Array.from(panel.querySelectorAll('button, input, select, output, label'))
      .filter(element => !element.closest('.media-detail'))
    const metrics = (element: Element) => {
      const style = getComputedStyle(element)
      const rect = element.getBoundingClientRect()
      return {
        display: style.display,
        position: style.position,
        background: style.backgroundColor,
        color: style.color,
        border: style.border,
        borderRadius: style.borderRadius,
        padding: style.padding,
        gap: style.gap,
        width: Math.round(rect.width * 100) / 100,
        height: Math.round(rect.height * 100) / 100,
        x: Math.round(rect.x * 100) / 100,
        y: Math.round(rect.y * 100) / 100,
      }
    }
    return {
      open: menu.hasAttribute('open'),
      label: summary.getAttribute('aria-label'),
      title: summary.getAttribute('title'),
      summary: metrics(summary),
      panel: metrics(panel),
      controls: relevant.map(element => ({
        tag: element.tagName.toLowerCase(),
        text: element.textContent?.replace(/\s+/g, ' ').trim(),
        label: element.getAttribute('aria-label'),
        value: (element as HTMLInputElement | HTMLSelectElement | HTMLOutputElement).value,
        disabled: (element as HTMLButtonElement | HTMLInputElement | HTMLSelectElement).disabled,
        metrics: metrics(element),
      })),
    }
  })
}

async function exerciseAudio(page: Page, baseUrl: string) {
  await mockMedia(page)
  await open(page, baseUrl, '菜单音频.m4a')
  await expect(page.locator('audio')).toHaveJSProperty('readyState', 4)
  const selector = '.audio-options .preview-menu'
  const menu = page.locator(selector)
  await page.mouse.move(0, 0)
  await page.waitForTimeout(220)
  const initial = await menuMetrics(page, selector)
  await menu.locator('summary').click()
  await expect(menu).toHaveAttribute('open', '')
  await page.mouse.move(0, 0)
  await page.waitForTimeout(220)
  const opened = await menuMetrics(page, selector)
  await menu.hover()
  await page.waitForTimeout(220)
  const hovered = await menuMetrics(page, selector)
  await menu.locator('input[type=range]').fill('0.42')
  const adjusted = await page.locator('audio').evaluate(element => ({
    volume: (element as HTMLAudioElement).volume,
    muted: (element as HTMLAudioElement).muted,
    output: document.querySelector('.audio-volume output')?.textContent,
    storedVolume: localStorage.getItem('revaro-audio-volume'),
  }))
  await page.locator('.audio-chapter-current h1').click()
  await expect(menu).not.toHaveAttribute('open', '')
  await menu.locator('summary').click()
  await page.keyboard.press('Escape')
  await expect(menu).not.toHaveAttribute('open', '')
  return {
    initial,
    opened,
    hovered,
    adjusted,
    focused: await menu.locator('summary').evaluate(element => document.activeElement === element),
  }
}

async function exerciseVideo(page: Page, baseUrl: string) {
  await page.addInitScript(() => {
    localStorage.setItem('revaro-video-volume', '0.73')
  })
  await mockMedia(page)
  await open(page, baseUrl, '菜单视频.webm')
  await expect(page.locator('video')).toHaveJSProperty('readyState', 4)
  const settingsSelector = '.video-controls .preview-menu:has(summary[aria-label="播放设置"])'
  const captionsSelector = '.video-controls .preview-menu:has(summary[aria-label="字幕"])'
  const settings = page.locator(settingsSelector)
  const captions = page.locator(captionsSelector)
  await expect(settings.locator('summary')).toBeVisible()
  await page.mouse.move(0, 0)
  await page.waitForTimeout(220)
  const initial = {
    settings: await menuMetrics(page, settingsSelector),
    captions: await menuMetrics(page, captionsSelector),
  }
  await captions.locator('summary').click()
  await expect(captions).toHaveAttribute('open', '')
  await page.waitForTimeout(220)
  const captionsOpen = await menuMetrics(page, captionsSelector)
  await captions.getByLabel('字幕轨道', { exact: true }).selectOption('-1')
  await captions.locator('summary').click()
  await settings.locator('summary').click()
  await expect(settings).toHaveAttribute('open', '')
  await page.waitForTimeout(220)
  const settingsOpen = await menuMetrics(page, settingsSelector)
  await settings.getByLabel('播放速度', { exact: true }).selectOption('1.5')
  const changed = await page.locator('video').evaluate(element => ({
    rate: (element as HTMLVideoElement).playbackRate,
    storedRate: localStorage.getItem('revaro-video-rate'),
  }))
  await page.locator('body').dispatchEvent('pointerdown', { bubbles: true })
  await expect(settings).not.toHaveAttribute('open', '')
  await settings.locator('summary').click()
  await page.keyboard.press('Escape')
  await expect(settings).not.toHaveAttribute('open', '')
  return {
    initial,
    captionsOpen,
    settingsOpen,
    changed,
    focused: await settings.locator('summary').evaluate(element => document.activeElement === element),
  }
}

test('音频音量菜单在桌面与移动端保持 reference 的状态、定位和焦点', async ({ browser }) => {
  const oldUrl = process.env.E2E_REFERENCE_URL || 'http://127.0.0.1:18080'
  const newUrl = process.env.E2E_NEW_URL || 'http://127.0.0.1:18084'
  for (const mobile of [false, true]) {
    const options = mobile
      ? { viewport: { width: 390, height: 844 }, isMobile: true, hasTouch: true }
      : { viewport: { width: 1440, height: 900 } }
    const oldContext = await browser.newContext(options)
    const newContext = await browser.newContext(options)
    const oldPage = await oldContext.newPage()
    const newPage = await newContext.newPage()
    try {
      const [oldResult, newResult] = await Promise.all([
        exerciseAudio(oldPage, oldUrl),
        exerciseAudio(newPage, newUrl),
      ])
      expect(newResult, `Rust 音频音量菜单 ${mobile ? '移动端' : '桌面'} 与 reference 不一致`).toEqual(oldResult)
    } finally {
      await Promise.all([oldContext.close(), newContext.close()])
    }
  }
})

test('视频字幕与播放设置菜单在桌面与移动端保持 reference 的状态、定位和焦点', async ({ browser }) => {
  const oldUrl = process.env.E2E_REFERENCE_URL || 'http://127.0.0.1:18080'
  const newUrl = process.env.E2E_NEW_URL || 'http://127.0.0.1:18084'
  for (const mobile of [false, true]) {
    const options = mobile
      ? { viewport: { width: 390, height: 844 }, isMobile: true, hasTouch: true }
      : { viewport: { width: 1440, height: 900 } }
    const oldContext = await browser.newContext(options)
    const newContext = await browser.newContext(options)
    const oldPage = await oldContext.newPage()
    const newPage = await newContext.newPage()
    try {
      const [oldResult, newResult] = await Promise.all([
        exerciseVideo(oldPage, oldUrl),
        exerciseVideo(newPage, newUrl),
      ])
      expect(newResult, `Rust 视频菜单 ${mobile ? '移动端' : '桌面'} 与 reference 不一致`).toEqual(oldResult)
    } finally {
      await Promise.all([oldContext.close(), newContext.close()])
    }
  }
})

test('视频菜单开合会按 reference 重置控制条自动隐藏计时器', async ({ browser }) => {
  const oldUrl = process.env.E2E_REFERENCE_URL || 'http://127.0.0.1:18080'
  const newUrl = process.env.E2E_NEW_URL || 'http://127.0.0.1:18084'
  const oldContext = await browser.newContext({ viewport: { width: 1440, height: 900 } })
  const newContext = await browser.newContext({ viewport: { width: 1440, height: 900 } })
  const oldPage = await oldContext.newPage()
  const newPage = await newContext.newPage()

  async function exercise(page: Page, baseUrl: string) {
    await mockMedia(page)
    await open(page, baseUrl, '菜单视频.webm')
    const video = page.locator('video')
    await expect(video).toHaveJSProperty('readyState', 4)
    await expect.poll(() => video.evaluate((element: HTMLVideoElement) => !element.paused)).toBe(true)

    // Keep the pointer away from the controls so only the menu toggle can
    // affect the 2.8s auto-hide timer. This mirrors PreviewMenu's native
    // toggle event without making the assertion depend on pointer geometry.
    await page.mouse.move(0, 0)
    await page.evaluate(() => {
      const menu = document.querySelector<HTMLDetailsElement>('.video-controls .preview-menu')
      if (!menu) throw new Error('video preview menu missing')
      menu.open = true
      menu.dispatchEvent(new Event('toggle'))
    })
    await page.waitForTimeout(1_000)
    await page.evaluate(() => {
      const menu = document.querySelector<HTMLDetailsElement>('.video-controls .preview-menu')
      if (!menu) throw new Error('video preview menu missing')
      menu.open = false
      menu.dispatchEvent(new Event('toggle'))
    })
    await page.waitForTimeout(2_100)
    return page.locator('.video-controls').evaluate(element => element.classList.contains('visible'))
  }

  try {
    const [oldVisible, newVisible] = await Promise.all([
      exercise(oldPage, oldUrl),
      exercise(newPage, newUrl),
    ])
    expect(oldVisible).toBe(true)
    expect(newVisible, 'Rust 视频菜单开合未按 reference 重置控制条自动隐藏计时器').toBe(oldVisible)
  } finally {
    await Promise.all([oldContext.close(), newContext.close()])
  }
})

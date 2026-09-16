import { expect, test, type Page } from '@playwright/test'
import { readFileSync } from 'node:fs'

const ROOT = '00000000-0000-0000-0000-000000000000'
const STAMP = '2026-01-01T00:00:00Z'
const audio = {
  id: 'focus-audio', parent_id: ROOT, name: '焦点音频.wav', kind: 'file', size: 100,
  status: 'ready', created_at: STAMP, updated_at: STAMP, mime_type: 'audio/wav',
}
const video = {
  id: 'focus-video', parent_id: ROOT, name: '焦点视频.webm', kind: 'file', size: 100,
  status: 'ready', created_at: STAMP, updated_at: STAMP, mime_type: 'video/webm',
}

function wav() {
  const samples = 8_000 * 4
  const data = Buffer.alloc(44 + samples * 2)
  data.write('RIFF', 0); data.writeUInt32LE(36 + samples * 2, 4); data.write('WAVEfmt ', 8)
  data.writeUInt32LE(16, 16); data.writeUInt16LE(1, 20); data.writeUInt16LE(1, 22)
  data.writeUInt32LE(8_000, 24); data.writeUInt32LE(16_000, 28); data.writeUInt16LE(2, 32)
  data.writeUInt16LE(16, 34); data.write('data', 36); data.writeUInt32LE(samples * 2, 40)
  return data
}

async function mockMedia(page: Page, nullableAudioMetadata = false) {
  const sound = wav()
  const movie = readFileSync(new URL('./fixtures/preview.webm', import.meta.url))
  await page.addInitScript(() => {
    const original = HTMLElement.prototype.focus
    ;(window as any).__playerFocusCalls = []
    HTMLElement.prototype.focus = function (...args: any[]) {
      if (this.matches('.chapter-audio-player, .video-player-shell')) {
        ;(window as any).__playerFocusCalls.push({
          className: this.className,
          options: args[0] ? { preventScroll: args[0].preventScroll === true } : null,
        })
      }
      return original.apply(this, args)
    }
  })
  await page.route('**/api/**', async route => {
    const path = new URL(route.request().url()).pathname
    const json = (value: unknown) => route.fulfill({ json: value })
    if (path === '/api/auth/me') return json({ username: 'admin', has_avatar: false })
    if (path === '/api/events' || path === '/api/system/status/stream') {
      return route.fulfill({ contentType: 'text/event-stream', body: '' })
    }
    if (path === '/api/tasks') return json({ items: [] })
    if (path === '/api/library/all') {
      return json({ items: { book: [], image: [], video: [], audio: [] }, counts: { book: 0, image: 0, video: 0, audio: 0, file: 2 } })
    }
    if (path === '/api/library/counts') return json({ book: 0, image: 0, video: 0, audio: 0, file: 2 })
    if (path === `/api/files/${ROOT}`) {
      return json({ file: { id: ROOT, parent_id: null, name: '我的文件', kind: 'directory', size: 0, status: 'ready', created_at: STAMP, updated_at: STAMP, mime_type: '' }, breadcrumbs: [] })
    }
    if (path === `/api/files/${ROOT}/children`) return json({ items: [audio, video], total_bytes: 200, file_count: 2 })
    if (path === `/api/files/${audio.id}/audio`) {
      return json(nullableAudioMetadata
        ? {
            duration: null,
            has_cover: true,
            cover_url: null,
            chapters: [
              { id: 1, title: '第一章', start: 0, end: 2 },
              { id: 2, title: '第二章', start: 2, end: 4 },
            ],
          }
        : { duration: 4, has_cover: false, cover_url: '', chapters: [] })
    }
    if (path === `/api/files/${video.id}/video`) return json({ subtitles: [] })
    if (path.endsWith('/media/progress')) return json({ position: 0, duration: 4 })
    if (path.endsWith('/thumbnail')) return route.fulfill({ status: 404, body: '' })
    if (path.endsWith('/preview')) {
      return route.fulfill({ contentType: path.includes(audio.id) ? 'audio/wav' : 'video/webm', body: path.includes(audio.id) ? sound : movie })
    }
    return json({ items: [] })
  })
}

async function exercise(page: Page, baseUrl: string, name: string, selector: string) {
  await mockMedia(page)
  await page.goto(`${baseUrl}/?media-focus=${Date.now()}`)
  const card = page.locator('.file-card').filter({ hasText: name })
  await card.click()
  await expect(page.locator(selector)).toBeVisible()
  await page.waitForFunction(() => ((window as any).__playerFocusCalls ?? []).length > 0)
  return page.evaluate(() => (window as any).__playerFocusCalls ?? [])
}

for (const [name, selector] of [['音频播放器', '.chapter-audio-player'], ['视频播放器', '.video-player-shell']] as const) {
  test(`old/new ${name} 挂载焦点的 preventScroll 语义一致`, async ({ browser }) => {
    const oldUrl = process.env.E2E_REFERENCE_URL || 'http://127.0.0.1:18080'
    const newUrl = process.env.E2E_NEW_URL || 'http://127.0.0.1:18084'
    const oldContext = await browser.newContext({ viewport: { width: 1280, height: 900 } })
    const newContext = await browser.newContext({ viewport: { width: 1280, height: 900 } })
    const oldPage = await oldContext.newPage()
    const newPage = await newContext.newPage()
    try {
      const [oldState, newState] = await Promise.all([
        exercise(oldPage, oldUrl, name === '音频播放器' ? audio.name : video.name, selector),
        exercise(newPage, newUrl, name === '音频播放器' ? audio.name : video.name, selector),
      ])
      expect(oldState).toEqual([{ className: expect.any(String), options: { preventScroll: true } }])
      expect(newState, `Rust ${name} 挂载焦点与 reference 不一致`).toEqual(oldState)
    } finally {
      await oldContext.close()
      await newContext.close()
    }
  })
}

test('old/new 音频 metadata 显式 null 时仍保留其他可用字段', async ({ browser }) => {
  const oldUrl = process.env.E2E_REFERENCE_URL || 'http://127.0.0.1:18080'
  const newUrl = process.env.E2E_NEW_URL || 'http://127.0.0.1:18084'
  const oldContext = await browser.newContext({ viewport: { width: 1280, height: 900 } })
  const newContext = await browser.newContext({ viewport: { width: 1280, height: 900 } })
  const oldPage = await oldContext.newPage()
  const newPage = await newContext.newPage()

  async function exercise(page: Page, baseUrl: string) {
    await mockMedia(page, true)
    const metadata = page.waitForResponse(response => response.url().endsWith(`/api/files/${audio.id}/audio`))
    await page.goto(`${baseUrl}/?media-null-metadata=${Date.now()}`)
    await page.locator('.file-card').filter({ hasText: audio.name }).click()
    await expect(page.locator('.chapter-audio-player')).toBeVisible()
    await metadata
    await page.locator('[data-panel-trigger="chapters"]').click()
    return {
      chapters: await page.locator('.audio-chapter-list button').count(),
      titles: await page.locator('.audio-chapter-list strong').allTextContents(),
    }
  }

  try {
    const [oldState, newState] = await Promise.all([
      exercise(oldPage, oldUrl),
      exercise(newPage, newUrl),
    ])
    expect(oldState).toEqual({ chapters: 2, titles: ['第一章', '第二章'] })
    expect(newState, 'Rust 音频 metadata 显式 null 时不应丢弃其他可用字段').toEqual(oldState)
  } finally {
    await Promise.all([oldContext.close(), newContext.close()])
  }
})

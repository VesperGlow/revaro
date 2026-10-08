import { expect, test, type Locator, type Page } from '@playwright/test'
import { readFileSync } from 'node:fs'
import { login, navigate, openMusicPlayer, selectMusicMode, uploadFixture } from './helpers'

test.use({ reducedMotion: 'reduce' })

const flac = readFileSync(new URL('./fixtures/preview.flac', import.meta.url))
const webm = readFileSync(new URL('./fixtures/preview.webm', import.meta.url))
type MediaFile = { id: string, name: string }

async function fixture(page: Page, run: (files: MediaFile[], headers: Record<string, string>) => Promise<void>, video = false) {
  await login(page)
  const headers = { origin: new URL(page.url()).origin }
  const name = `player-regressions-${Date.now()}`
  const response = await page.request.post('/api/directories', {
    headers, data: { parent_id: '00000000-0000-0000-0000-000000000000', name },
  })
  expect(response.status()).toBe(201)
  const directory = await response.json()
  let collection: string | undefined
  try {
    const files = []
    for (let index = 0; index < (video ? 1 : 2); index++) {
      files.push(await uploadFixture(page, directory.id, `${name}-${index}.${video ? 'webm' : 'flac'}`,
        video ? 'video/webm' : 'audio/flac', video ? webm : flac))
    }
    if (!video) {
      const created = await page.request.post('/api/library/collections', { headers, data: { name, kind: 'audio' } })
      expect(created.status()).toBe(200)
      collection = (await created.json()).id
      for (const file of files) {
        expect((await page.request.put(`/api/library/collections/${collection}/items/${file.id}`, { headers })).status()).toBe(204)
      }
      await navigate(page, '音乐')
      await page.getByLabel('选择集合', { exact: true }).click()
      await page.getByRole('button', { name: `${name} · 2`, exact: true }).click()
    } else {
      await navigate(page, '视频')
    }
    await run(files, headers)
  } finally {
    await page.goto('/files')
    if (collection) await page.request.delete(`/api/library/collections/${collection}`, { headers })
    await page.request.delete(`/api/files/${directory.id}`, { headers })
    await page.request.delete(`/api/trash/${directory.id}`, { headers })
  }
}

async function seek(input: Locator, position: number) {
  await input.evaluate((element: HTMLInputElement, time) => {
    element.value = String(time)
    element.dispatchEvent(new Event('input', { bubbles: true }))
    element.dispatchEvent(new Event('change', { bubbles: true }))
  }, position)
}

test('full audio player accepts seeks while saved progress is still loading', async ({ page }) => {
  await fixture(page, async ([first], headers) => {
    await page.request.put(`/api/files/${first.id}/media/progress`, { headers, data: { position: 10, duration: 30 } })
    let release!: () => void
    const held = new Promise<void>(resolve => { release = resolve })
    await page.context().route(`**/api/files/${first.id}/media/progress`, async route => {
      if (route.request().method() !== 'GET') return route.continue()
      const response = await route.fetch()
      await held
      await route.fulfill({ response })
    })
    try {
      await page.getByRole('button', { name: `打开 ${first.name}`, exact: true }).click()
      await openMusicPlayer(page)
      await page.getByRole('button', { name: '打开音频播放器', exact: true }).click()
      const audio = page.locator('audio').first()
      const input = page.getByLabel('播放进度', { exact: true })
      await expect(input).toBeEnabled()
      // An explicit seek near the end must not use the completed-media resume rule.
      const bounds = (await input.boundingBox())!
      await input.click({ position: { x: bounds.width * 0.9, y: bounds.height / 2 } })
      await expect.poll(() => audio.evaluate((element: HTMLAudioElement) => Math.abs(element.currentTime - 27))).toBeLessThan(1)
      await page.getByRole('button', { name: '暂停', exact: true }).click()
      const position = await audio.evaluate((element: HTMLAudioElement) => element.currentTime)
      const restored = page.waitForResponse(response => response.url().endsWith(`/api/files/${first.id}/media/progress`)
        && response.request().method() === 'GET' && response.fromServiceWorker())
      release()
      await restored
      await expect.poll(() => input.inputValue().then(Number)).toBeCloseTo(position, 1)
      await expect.poll(async () => (await (await page.request.get(`/api/files/${first.id}/media/progress`)).json()).position).toBeCloseTo(position, 1)
    } finally {
      release()
      await page.context().unrouteAll({ behavior: 'wait' })
    }
  })
})

test('full audio timeline waits for native metadata before accepting a seek', async ({ page }) => {
  await fixture(page, async ([first]) => {
    let release!: () => void
    const held = new Promise<void>(resolve => { release = resolve })
    await page.context().route(`**/api/files/${first.id}/preview`, async route => {
      await held
      await route.continue()
    })
    try {
      await page.getByRole('button', { name: `打开 ${first.name}`, exact: true }).click()
      await openMusicPlayer(page)
      await page.getByRole('button', { name: '打开音频播放器', exact: true }).click()
      const input = page.getByLabel('播放进度', { exact: true })
      // The server has a duration before the native element can accept currentTime.
      await expect(input).toHaveAttribute('max', '30')
      await expect(input).toBeDisabled()
      await expect(page.getByRole('button', { name: '后退15秒', exact: true })).toBeDisabled()
      await expect(page.getByRole('button', { name: '前进30秒', exact: true })).toBeDisabled()
      release()
      await expect(input).toBeEnabled()
      await page.getByRole('button', { name: '暂停', exact: true }).click()
      await seek(input, 17)
      await expect.poll(() => page.locator('audio').first().evaluate((element: HTMLAudioElement) => element.currentTime)).toBeCloseTo(17, 1)
    } finally {
      release()
      await page.context().unrouteAll({ behavior: 'wait' })
    }
  })
})

test('automatic next track starts at zero even with a local resume position', async ({ page }) => {
  await fixture(page, async ([first, second]) => {
    await page.evaluate(id => localStorage.setItem(`revaro-audio-position:${id}`, '12'), second.id)
    await page.getByRole('button', { name: `打开 ${first.name}`, exact: true }).click()
    await openMusicPlayer(page)
    await expect(page.getByRole('button', { name: '暂停音乐', exact: true })).toBeVisible()
    const audio = page.locator('audio').first()
    await seek(page.getByLabel('音乐播放进度', { exact: true }), 29.8)
    await expect(audio).toHaveAttribute('src', `/api/files/${second.id}/preview`)
    await expect(page.getByRole('button', { name: '暂停音乐', exact: true })).toBeVisible()
    expect(await audio.evaluate((element: HTMLAudioElement) => element.currentTime)).toBeLessThan(3)
  })
})

test('reselecting the current track preserves an explicit seek in the final seconds', async ({ page }) => {
  await fixture(page, async ([first]) => {
    const card = page.getByRole('button', { name: `打开 ${first.name}`, exact: true })
    await card.click()
    await openMusicPlayer(page)
    await page.getByRole('button', { name: '暂停音乐', exact: true }).click()
    await seek(page.getByLabel('音乐播放进度', { exact: true }), 27)
    await page.getByRole('button', { name: '收起播放器', exact: true }).click()
    await card.click()
    const audio = page.locator('audio').first()
    await expect(audio).toHaveJSProperty('paused', false)
    await expect.poll(() => audio.evaluate((element: HTMLAudioElement) => Math.abs(element.currentTime - 27))).toBeLessThan(1)
  })
})

test('standalone audio keeps a manual seek when its delayed history arrives', async ({ page }) => {
  await fixture(page, async ([file], headers) => {
    expect((await page.request.delete(`/api/files/${file.id}`, { headers })).ok()).toBeTruthy()
    let release!: () => void
    const held = new Promise<void>(resolve => { release = resolve })
    await page.context().route(`**/api/files/${file.id}/media/progress`, async route => {
      if (route.request().method() !== 'GET') return route.continue()
      await held
      await route.fulfill({ json: { position: 10, duration: 30, updated_at: null } })
    })
    try {
      await navigate(page, '回收站')
      await page.locator('.file-card').filter({ hasText: file.name }).click()
      const audio = page.locator('.chapter-audio-player audio')
      await page.getByRole('button', { name: '暂停', exact: true }).click()
      const input = page.getByLabel('播放进度', { exact: true })
      await seek(input, 27)
      const restored = page.waitForResponse(response => response.url().endsWith(`/api/files/${file.id}/media/progress`)
        && response.request().method() === 'GET' && response.fromServiceWorker())
      release()
      await restored
      await expect.poll(() => audio.evaluate((element: HTMLAudioElement) => element.currentTime)).toBeCloseTo(27, 1)
      await seek(input, 0)
      await page.locator('.preview-close').click()
      expect(await page.evaluate(id => localStorage.getItem(`revaro-audio-position:${id}`), file.id)).toBe('0')
    } finally {
      release()
      await page.context().unrouteAll({ behavior: 'wait' })
    }
  })
})

test('seeks preserve the native audio source during playback and volume can unmute', async ({ page }) => {
  await fixture(page, async ([first]) => {
    const errors: string[] = []
    page.on('pageerror', error => errors.push(error.message))
    await page.getByRole('button', { name: `打开 ${first.name}`, exact: true }).click()
    await openMusicPlayer(page)
    await selectMusicMode(page, 'repeat-one')
    const audio = page.locator('audio').first()
    await expect(audio).toHaveJSProperty('paused', false)
    await audio.evaluate((element: HTMLAudioElement) => {
      (window as any).playerReloads = 0
      element.addEventListener('loadstart', () => { (window as any).playerReloads++ })
    })
    const input = page.getByLabel('音乐播放进度', { exact: true })
    for (const position of [18, 2, 26, 8, 0, 21, 4]) {
      await seek(input, position)
      await expect.poll(() => audio.evaluate((element: HTMLAudioElement, target) => Math.abs(element.currentTime - target), position)).toBeLessThan(1)
      await expect(audio).toHaveJSProperty('paused', false)
      await expect(audio).toHaveAttribute('src', `/api/files/${first.id}/preview`)
    }
    expect(await page.evaluate(() => (window as any).playerReloads)).toBe(0)
    await page.getByRole('button', { name: '暂停音乐', exact: true }).click()
    await page.getByLabel('音乐音量设置', { exact: true }).click()
    await page.getByRole('button', { name: '静音', exact: true }).click()
    await page.getByLabel('音乐音量', { exact: true }).fill('0.5')
    await expect(audio).toHaveJSProperty('muted', false)
    await expect(audio).toHaveJSProperty('volume', 0.5)
    expect(errors).toEqual([])
  })
})

test('late video resume cannot override a manual seek and zero is saved on close', async ({ page }) => {
  await fixture(page, async ([file], headers) => {
    let release!: () => void
    const held = new Promise<void>(resolve => { release = resolve })
    await page.context().route(`**/api/files/${file.id}/media/progress`, async route => {
      if (route.request().method() !== 'GET') return route.continue()
      await held
      await route.fulfill({ json: { position: 2, duration: 4, updated_at: null } })
    })
    try {
      await page.getByRole('button', { name: `打开 ${file.name}`, exact: true }).click()
      const video = page.locator('video')
      await expect.poll(() => video.evaluate((element: HTMLVideoElement) => element.readyState)).toBeGreaterThanOrEqual(1)
      await video.evaluate((element: HTMLVideoElement) => element.pause())
      const input = page.getByLabel('视频进度', { exact: true })
      const target = await video.evaluate((element: HTMLVideoElement) => element.duration * 0.75)
      await seek(input, target)
      const restored = page.waitForResponse(response => response.url().endsWith(`/api/files/${file.id}/media/progress`)
        && response.request().method() === 'GET' && response.fromServiceWorker())
      release()
      await restored
      await expect.poll(() => video.evaluate((element: HTMLVideoElement) => element.currentTime)).toBeCloseTo(target, 1)
      await seek(input, 0)
      await page.getByRole('button', { name: '退出播放', exact: true }).click()
      await expect.poll(async () => (await (await page.request.get(`/api/files/${file.id}/media/progress`)).json()).position).toBe(0)
      await expect.poll(() => page.evaluate(id => localStorage.getItem(`revaro-video-position:${id}`), file.id)).toBe('0')
    } finally {
      release()
      await page.context().unrouteAll({ behavior: 'wait' })
    }
  }, true)
})

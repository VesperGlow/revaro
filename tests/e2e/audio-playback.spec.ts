import { expect, test } from '@playwright/test'
import { mkdirSync, readFileSync } from 'node:fs'
import { openMusicPlayer, pauseMusic, login, navigate, enterSelectionMode } from './helpers'

test.use({ reducedMotion: 'reduce' })

const flac = readFileSync(new URL('./fixtures/preview.flac', import.meta.url))
const captions = Buffer.from('WEBVTT\n\n00:01.000 --> 00:05.000\nFirst subtitle\n\n00:10.000 --> 00:15.000\nSecond subtitle\n')
const chapters = Buffer.from('WEBVTT\n\n00:00.000 --> 00:10.000\nIntro\n\n00:10.000 --> 00:20.000\nMiddle\n\n00:20.000 --> 00:30.000\nEnd\n')

test('audio categories keep files, chapters and resume independent', async ({ page }) => {
  test.setTimeout(90_000)
  const errors: string[] = []
  page.on('pageerror', error => errors.push(error.message))
  const prefix = `audio-playback-${Date.now()}`
  const names = [`${prefix}-01.flac`, `${prefix}-02.flac`]
  await login(page)
  await page.locator('input[type=file]').first().setInputFiles([
    ...names.map(name => ({ name, mimeType: 'audio/flac', buffer: flac })),
    { name: `${prefix}-01.vtt`, mimeType: 'text/vtt', buffer: captions },
    { name: `${prefix}-01.chapters.vtt`, mimeType: 'text/vtt', buffer: chapters },
  ])
  await expect.poll(async () => (await (await page.request.get(`/api/library/items?kind=audio&q=${prefix}`)).json()).total).toBe(2)
  const files = (await (await page.request.get(`/api/library/items?kind=audio&q=${prefix}`)).json()).items.map((i: any) => i.file)
  const first = files.find((f: any) => f.name === names[0])
  const headers = { origin: new URL(page.url()).origin }
  const created = await page.request.post('/api/library/collections', { headers, data: { name: prefix, kind: 'audio' } })
  expect(created.status()).toBe(200)
  const collection = await created.json()
  for (const name of names) {
    const file = files.find((f: any) => f.name === name)
    expect((await page.request.put(`/api/library/collections/${collection.id}/items/${file.id}`, { headers })).status()).toBe(204)
  }
  await navigate(page, '音乐')
  await page.getByLabel('选择集合', { exact: true }).click()
  await page.getByRole('button', { name: `${prefix} · 2`, exact: true }).click()
  await expect(page.locator('.library-grid > .library-card')).toHaveCount(2)
  await expect(page.locator('.stack-card,.stack-header')).toHaveCount(0)
  await enterSelectionMode(page)
  for (const name of names) await page.getByRole('button', { name: `打开 ${name}`, exact: true }).click({ force: true })
  await expect(page.getByRole('button', { name: '堆叠', exact: true })).toHaveCount(0)
  await page.getByRole('toolbar', { name: '所选项目操作', exact: true }).getByRole('button', { name: '取消', exact: true }).click()
  await page.getByRole('button', { name: `打开 ${names[0]}`, exact: true }).click()
  const audio = page.locator('audio').first()
  await expect.poll(() => audio.evaluate((a: HTMLAudioElement) => a.readyState)).toBeGreaterThanOrEqual(1)
  await openMusicPlayer(page)
  await expect(page.getByRole('button', { name: '暂停音乐', exact: true })).toBeVisible()
  await expect(audio).toHaveAttribute('preload', 'metadata')
  await expect(page.locator('audio').nth(1)).toHaveAttribute('preload', 'metadata')
  await expect.poll(() => page.locator('audio').nth(1).evaluate((a: HTMLAudioElement) => a.readyState)).toBeGreaterThanOrEqual(1)
  await expect(page.locator('.dock-chapter-marker')).toHaveCount(3)
  await expect(page.locator('.dock-buffer').first()).toBeAttached()
  await page.getByRole('button', { name: '暂停音乐', exact: true }).click()
  const source = await audio.getAttribute('src')
  const seek = page.getByLabel('音乐播放进度', { exact: true })
  await seek.evaluate((input: HTMLInputElement) => { input.value = '2'; input.dispatchEvent(new Event('input', { bubbles: true })) })
  expect(await audio.evaluate((a: HTMLAudioElement) => a.currentTime)).toBeLessThan(2)
  await seek.dispatchEvent('change')
  await expect.poll(() => audio.evaluate((a: HTMLAudioElement) => a.currentTime)).toBeCloseTo(2, 1)
  await expect(page.locator('.audio-subtitles')).toHaveText('First subtitle')
  await page.getByRole('button', { name: '音轨章节', exact: true }).click()
  await expect(page.locator('.music-chapters>button')).toHaveCount(3)
  await page.locator('.music-chapters>button').filter({ hasText: 'Middle' }).click()
  await expect.poll(() => audio.evaluate((a: HTMLAudioElement) => a.currentTime)).toBeCloseTo(10, 1)
  await expect(page.locator('.audio-subtitles')).toHaveText('Second subtitle')
  await expect(audio).toHaveAttribute('src', source!)
  if (process.env.E2E_AUDIO_SCREENSHOTS) {
    mkdirSync(process.env.E2E_AUDIO_SCREENSHOTS, { recursive: true })
    await page.screenshot({ path: `${process.env.E2E_AUDIO_SCREENSHOTS}/audio-desktop.png` })
  }
  await page.setViewportSize({ width: 390, height: 844 })
  await expect(seek).toBeVisible()
  await expect(page.getByRole('button', { name: '音轨章节', exact: true })).toBeVisible()
  await expect(page.locator('.music-chapters')).toBeVisible()
  expect(await page.evaluate(() => document.documentElement.scrollWidth <= window.innerWidth)).toBeTruthy()
  if (process.env.E2E_AUDIO_SCREENSHOTS) await page.screenshot({ path: `${process.env.E2E_AUDIO_SCREENSHOTS}/audio-mobile.png` })
  await page.setViewportSize({ width: 1280, height: 720 })
  await page.reload()
  await openMusicPlayer(page)
  await expect.poll(() => page.locator('audio').first().evaluate((a: HTMLAudioElement) => a.currentTime)).toBeCloseTo(10, 1)
  await expect(page.locator('.dock-track small')).toContainText('第 1 / 2 轨')
  await expect(page.locator('.audio-subtitles')).toHaveText('Second subtitle')
  await page.getByRole('button', { name: '播放音乐', exact: true }).click()
  await seek.evaluate((input: HTMLInputElement) => { input.value = '29.8'; input.dispatchEvent(new Event('change', { bubbles: true })) })
  await expect(page.locator('.dock-track strong')).toHaveText(names[1].replace('.flac', ''))
  await openMusicPlayer(page)
  await expect(page.getByRole('button', { name: '暂停音乐', exact: true })).toBeVisible()
  await expect(page.locator('.dock-chapter-marker')).toHaveCount(0)
  await expect(page.locator('.audio-subtitles')).toHaveCount(0)
  await page.getByRole('button', { name: '暂停音乐', exact: true }).click()
  await seek.evaluate((input: HTMLInputElement) => { input.value = '7'; input.dispatchEvent(new Event('change', { bubbles: true })) })
  await page.reload()
  await openMusicPlayer(page)
  await expect(page.locator('.dock-track strong')).toHaveText(names[1].replace('.flac', ''))
  await expect.poll(() => page.locator('audio').first().evaluate((a: HTMLAudioElement) => a.currentTime)).toBeCloseTo(7, 1)
  await page.getByRole('button', { name: '播放音乐', exact: true }).click()
  await expect(page.getByRole('button', { name: '暂停音乐', exact: true })).toBeVisible()
  await expect(page.locator('.dock-track small')).toContainText('第 2 / 2 轨')
  const head = await page.request.head(`/api/files/${first.id}/preview`)
  expect(head.status()).toBe(200)
  expect(head.headers()['accept-ranges']).toBe('bytes')
  expect(head.headers()['content-type']).toBe('audio/flac')
  expect(head.headers()['content-length']).toBe(String(flac.length))
  const partial = await page.request.get(`/api/files/${first.id}/preview`, { headers: { Range: 'bytes=4-31' } })
  expect(partial.status()).toBe(206)
  expect(partial.headers()['content-range']).toBe(`bytes 4-31/${flac.length}`)
  expect(await partial.body()).toEqual(flac.subarray(4, 32))
  expect(errors).toEqual([])
  await pauseMusic(page)
  await page.request.delete(`/api/library/collections/${collection.id}`, { headers })
})

test('large native FLAC seeks with partial requests and real external subtitles', async ({ page }) => {
  const id = process.env.E2E_LARGE_AUDIO_ID
  test.skip(!id, 'Set E2E_LARGE_AUDIO_ID to an imported large FLAC with a same-name VTT')
  test.setTimeout(90_000)
  await login(page)
  const file = (await (await page.request.get(`/api/files/${id}`)).json()).file
  const metadata = await (await page.request.get(`/api/files/${id}/audio`)).json()
  expect(metadata.duration).toBeGreaterThan(3600)
  expect(metadata.chapters).toHaveLength(6)
  expect(metadata.subtitles.length).toBeGreaterThan(0)
  const cdp = await page.context().newCDPSession(page)
  await cdp.send('Network.enable')
  await cdp.send('Network.emulateNetworkConditions', {
    offline: false, latency: 5, downloadThroughput: 8 * 1024 * 1024, uploadThroughput: -1,
  })
  const ranges: { range: string, status: number, contentRange: string }[] = []
  page.on('response', async response => {
    if (response.url().includes(`/api/files/${id}/preview`)) {
      ranges.push({ range: response.request().headers().range, status: response.status(), contentRange: response.headers()['content-range'] })
    }
  })
  await page.goto('/music')
  await page.getByRole('button', { name: '打开搜索', exact: true }).click()
  await page.getByRole('searchbox').fill(file.name)
  await page.getByRole('button', { name: `打开 ${file.name}`, exact: true }).click()
  const audio = page.locator('audio').first()
  await expect.poll(() => audio.evaluate((a: HTMLAudioElement) => a.readyState), { timeout: 45_000 }).toBeGreaterThanOrEqual(1)
  await openMusicPlayer(page)
  await expect(page.getByRole('button', { name: '暂停音乐', exact: true })).toBeVisible()
  await page.getByRole('button', { name: '暂停音乐', exact: true }).click()
  const source = await audio.getAttribute('src')
  await page.getByRole('button', { name: '音轨章节', exact: true }).click()
  const beforeSeek = ranges.length
  await expect(page.locator('.music-chapters>button')).toHaveCount(6)
  const chapter = metadata.chapters[3]
  await page.locator('.music-chapters>button').nth(3).click()
  await expect.poll(() => audio.evaluate((a: HTMLAudioElement) => a.currentTime), { timeout: 30_000 }).toBeCloseTo(chapter.start, 1)
  await expect.poll(() => ranges.slice(beforeSeek).some(r => r.status === 206 && Number(r.range?.match(/bytes=(\d+)-/)?.[1]) > file.size / 4), { timeout: 30_000 }).toBeTruthy()
  await expect.poll(() => audio.evaluate((a: HTMLAudioElement) => !a.seeking && a.readyState >= 2), { timeout: 30_000 }).toBeTruthy()
  expect(ranges.every(r => r.status === 206 && r.contentRange)).toBeTruthy()
  await expect(audio).toHaveAttribute('src', source!)
  const cue = metadata.subtitles.find((cue: any) => cue.end - cue.start > 1)
  await page.getByLabel('音乐播放进度', { exact: true }).evaluate((input: HTMLInputElement, time: number) => {
    input.value = String(time); input.dispatchEvent(new Event('change', { bubbles: true }))
  }, cue.start + 0.25)
  await expect(page.locator('.audio-subtitles')).toContainText(cue.text)
  await expect.poll(() => audio.evaluate((a: HTMLAudioElement) => a.currentTime), { timeout: 30_000 }).toBeCloseTo(cue.start + 0.25, 1)
  console.log(JSON.stringify({ largeFlacBytes: file.size, duration: metadata.duration, subtitles: metadata.subtitles.length, chapters: metadata.chapters.length, partialRequests: ranges }))
  await pauseMusic(page)
})

import { expect, test } from '@playwright/test'
import { execFileSync } from 'node:child_process'
import { mkdtempSync, readFileSync, rmSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import { login, navigate, uploadFixture } from './helpers'
import { transportProxy } from './fixtures/transport-proxy'

for (const disconnectAtBytes of [0, 1024 * 1024])
test(`${disconnectAtBytes ? 'interrupted' : 'high-bitrate'} native video plays and seeks without worker Range amplification`, async ({ browser, baseURL }, testInfo) => {
  test.setTimeout(180_000)
  const scratch = mkdtempSync(join(tmpdir(), 'revaro-native-video-'))
  const path = join(scratch, 'high-bitrate.webm')
  let bytes: Buffer
  try {
    // Deterministic noise prevents a simple test pattern compressing into a
    // tiny file that cannot expose the old 64–512 KiB request overhead.
    execFileSync(process.env.E2E_FFMPEG || 'ffmpeg', [
      '-v', 'error', '-f', 'lavfi', '-i', 'testsrc2=size=640x360:rate=30',
      '-vf', 'noise=alls=50:allf=t+u:all_seed=42', '-t', '16', '-c:v', 'libvpx-vp9',
      '-b:v', '12M', '-minrate', '12M', '-maxrate', '12M', '-bufsize', '24M',
      '-g', '30', '-deadline', 'realtime', '-cpu-used', '8', '-threads', '2', '-an', path,
    ])
    bytes = readFileSync(path)
  } finally { rmSync(scratch, { recursive: true, force: true }) }
  const bitrate = bytes.length * 8 / 16
  expect(bitrate).toBeGreaterThan(8_000_000)
  const proxy = await transportProxy(baseURL!, { bytesPerSecond: 4 * 1024 * 1024, latencyMs: 60, disconnectAtBytes })
  const context = await browser.newContext({ baseURL: proxy.origin })
  const page = await context.newPage()
  await page.addInitScript(() => {
    const events: unknown[] = []
    Object.assign(window, { nativeMediaEvents: events })
    for (const type of ['loadstart', 'loadedmetadata', 'progress', 'stalled', 'waiting', 'playing', 'pause', 'canplay', 'seeking', 'seeked', 'error', 'emptied', 'ended'])
      document.addEventListener(type, event => {
        const media = event.target
        if (!(media instanceof HTMLMediaElement)) return
        events.push({ type, at: Date.now(), currentTime: media.currentTime, paused: media.paused, readyState: media.readyState,
          networkState: media.networkState, error: media.error?.code,
          buffered: Array.from({ length: media.buffered.length }, (_, i) => [media.buffered.start(i), media.buffered.end(i)]) })
      }, true)
  })
  let directory: { id: string } | undefined
  let completed = false
  const headers = { origin: proxy.origin }, name = `native-video-${Date.now()}`
  try {
    await login(page)
    const created = await page.request.post('/api/directories', {
      headers, data: { parent_id: '00000000-0000-0000-0000-000000000000', name },
    })
    expect(created.status()).toBe(201)
    directory = await created.json()
    const file = await uploadFixture(page, directory!.id, `${name}.webm`, 'video/webm', bytes)
    const preview = `/api/files/${file.id}/preview`
    proxy.tracked.add(preview)
    const rewritten: string[] = []
    page.on('response', response => {
      if (new URL(response.url()).pathname === preview && response.headers()['x-revaro-transport']) rewritten.push(response.url())
    })
    await navigate(page, '视频')
    await page.getByRole('button', { name: '打开搜索', exact: true }).click()
    await page.getByRole('searchbox').fill(file.name)
    const openedAt = Date.now()
    await page.getByRole('button', { name: `打开 ${file.name}`, exact: true }).click()
    const video = page.locator('video').last()
    await video.evaluate((element: HTMLVideoElement) => element.play())
    await expect.poll(() => video.evaluate((v: HTMLVideoElement) => v.currentTime), { timeout: 20_000 }).toBeGreaterThan(3)
    const playingAt = Date.now()
    await expect(video).toHaveAttribute('preload', 'auto')
    // Dragging the UI preview thumb must not perform a seek until committed.
    const beforeDrag = proxy.samples.length
    const position = await video.evaluate((v: HTMLVideoElement) => v.currentTime)
    await page.getByLabel('视频进度', { exact: true }).evaluate((input: HTMLInputElement) => {
      for (const time of [8, 4, 11, 7, 13]) { input.value = String(time); input.dispatchEvent(new Event('input', { bubbles: true })) }
    })
    expect(await video.evaluate((v: HTMLVideoElement) => v.currentTime)).toBeLessThan(position + 1)
    expect(proxy.samples.length).toBeLessThanOrEqual(beforeDrag + 1)
    await video.evaluate((v: HTMLVideoElement) => v.pause())
    for (const target of [12, 2, 9]) {
      await page.getByLabel('视频进度', { exact: true }).evaluate((input: HTMLInputElement, time) => {
        input.value = String(time); input.dispatchEvent(new Event('change', { bubbles: true }))
      }, target)
      await expect.poll(() => video.evaluate((v: HTMLVideoElement, time) => !v.seeking && v.readyState >= 2 && Math.abs(v.currentTime - time) < 0.25, target), { timeout: 20_000 }).toBeTruthy()
    }
    await video.evaluate((v: HTMLVideoElement) => v.play())
    await expect.poll(() => video.evaluate((v: HTMLVideoElement) => v.currentTime), { timeout: 10_000 }).toBeGreaterThan(11)
    await page.getByRole('button', { name: '退出播放', exact: true }).click()
    expect(rewritten).toEqual([])
    expect(proxy.samples.length).toBeGreaterThan(0)
    expect(proxy.samples.length).toBeLessThanOrEqual(20)
    expect(proxy.faults).toBe(disconnectAtBytes ? 1 : 0)
    expect(proxy.samples.filter(s => s.range === 'bytes=0-0')).toHaveLength(0)
    expect(proxy.samples.every(s => [200, 206, 304].includes(s.status))).toBeTruthy()
    const report = { bitrate, fileBytes: bytes.length, faults: proxy.faults, startupToThreeSecondsMs: playingAt - openedAt,
      networkRequests: proxy.samples.length, transferredBytes: proxy.samples.reduce((sum, sample) => sum + sample.bytes, 0), samples: proxy.samples }
    await testInfo.attach('native-video-transfer.json', { body: JSON.stringify(report, null, 2), contentType: 'application/json' })
    console.info('native-video-transfer', JSON.stringify({ ...report, samples: undefined }))
    completed = true
  } finally {
    const events = await page.evaluate(() => (window as unknown as { nativeMediaEvents: unknown[] }).nativeMediaEvents).catch(() => [])
    const media = await page.evaluate(() => {
      const video = Array.from(document.querySelectorAll('video')).at(-1)
      if (!video) return null
      return { currentTime: video.currentTime, paused: video.paused, seeking: video.seeking, ended: video.ended,
        readyState: video.readyState, networkState: video.networkState, error: video.error?.code,
        buffered: Array.from({ length: video.buffered.length }, (_, i) => [video.buffered.start(i), video.buffered.end(i)]) }
    }).catch(() => null)
    const diagnostics = { faults: proxy.faults, samples: proxy.samples, events, media }
    await testInfo.attach('native-transfer-diagnostics.json', { body: JSON.stringify(diagnostics, null, 2), contentType: 'application/json' })
    if (!completed) console.info('native-video-failure', JSON.stringify(diagnostics))
    if (directory) {
      await page.goto('/files')
      await page.request.delete(`/api/files/${directory.id}`, { headers })
      await page.request.delete(`/api/trash/${directory.id}`, { headers })
    }
    await context.close()
    await proxy.close()
  }
})

test('native thumbnail cache revalidates and cannot bypass an expired login', async ({ browser, browserName, baseURL }, testInfo) => {
  const proxy = await transportProxy(baseURL!)
  const context = await browser.newContext({ baseURL: proxy.origin })
  const page = await context.newPage(), headers = { origin: proxy.origin }
  let directory: { id: string } | undefined
  let cookies: Awaited<ReturnType<typeof context.cookies>> = []
  try {
    await login(page)
    const created = await page.request.post('/api/directories', {
      headers, data: { parent_id: '00000000-0000-0000-0000-000000000000', name: `native-cache-${Date.now()}` },
    })
    expect(created.status()).toBe(201)
    directory = await created.json()
    const png = Buffer.from('iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mNk+A8AAQUBAScY42YAAAAASUVORK5CYII=', 'base64')
    const file = await uploadFixture(page, directory!.id, 'cache.png', 'image/png', png)
    const path = `/api/files/${file.id}/thumbnail`
    const prepared = await page.request.get(path)
    expect(prepared.status()).toBe(200)
    expect(prepared.headers()['cache-control']).toBe('private, no-cache')
    proxy.tracked.add(path)
    const load = () => page.evaluate(path => new Promise<boolean>(resolve => {
      const image = new Image()
      image.onload = () => resolve(true); image.onerror = () => resolve(false)
      image.src = path; document.body.append(image)
    }), path)
    for (let i = 0; i < 2; i++) { await page.goto('/files'); expect(await load()).toBe(true) }
    // HTTP caching is optional. WPE WebKit can re-fetch private images rather
    // than retain their bytes; both paths must validate the current session.
    if (browserName !== 'webkit') {
      expect(proxy.samples.some(sample => sample.status === 304 && sample.ifNoneMatch)).toBe(true)
      expect(proxy.samples.filter(sample => sample.status === 200)).toHaveLength(1)
    }
    expect(proxy.samples.every(sample => [200, 304].includes(sample.status))).toBe(true)
    const validated = await page.request.get(path, { headers: { 'if-none-match': prepared.headers().etag } })
    expect(validated.status()).toBe(304)
    cookies = await context.cookies()
    await context.clearCookies()
    await page.goto('/')
    expect(await load()).toBe(false)
    expect(proxy.samples.some(sample => [401, 403].includes(sample.status))).toBe(true)
  } finally {
    await testInfo.attach('native-thumbnail-transfer.json', { body: JSON.stringify({ samples: proxy.samples }, null, 2), contentType: 'application/json' })
    if (cookies.length) await context.addCookies(cookies)
    if (directory) {
      await page.request.delete(`/api/files/${directory.id}`, { headers })
      await page.request.delete(`/api/trash/${directory.id}`, { headers })
    }
    await context.close()
    await proxy.close()
  }
})

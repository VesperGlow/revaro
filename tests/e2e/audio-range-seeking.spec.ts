import { expect, test } from '@playwright/test'
import { execFileSync } from 'node:child_process'
import { mkdtempSync, readFileSync, rmSync, writeFileSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import { login, openMusicPlayer, uploadFixture } from './helpers'

// One hour of real PCM, with enough entropy that its FLAC remains a large file.
// WAV is self-contained; FLAC generation uses the existing fixture-tool FFmpeg.
function longWav() {
  const samples = 3605 * 8000
  const data = Buffer.alloc(44 + samples * 2)
  data.write('RIFF', 0); data.writeUInt32LE(data.length - 8, 4); data.write('WAVEfmt ', 8)
  data.writeUInt32LE(16, 16); data.writeUInt16LE(1, 20); data.writeUInt16LE(1, 22)
  data.writeUInt32LE(8000, 24); data.writeUInt32LE(16000, 28)
  data.writeUInt16LE(2, 32); data.writeUInt16LE(16, 34)
  data.write('data', 36); data.writeUInt32LE(samples * 2, 40)
  let seed = 42
  for (let i = 44; i < data.length; i += 2) {
    seed = (Math.imul(seed, 1664525) + 1013904223) >>> 0
    data.writeUInt16LE(seed >>> 16, i)
  }
  return data
}

for (const format of ['wav', 'flac']) {
  test(`long ${format.toUpperCase()} seeks before the full audio is downloaded`, async ({ page }) => {
    test.setTimeout(180_000)
    let buffer = longWav()
    if (format === 'flac') {
      const ffmpeg = process.env.E2E_FFMPEG || 'ffmpeg'
      try { execFileSync(ffmpeg, ['-version'], { stdio: 'ignore' }) }
      catch { test.skip(true, 'Set E2E_FFMPEG to generate the long FLAC fixture') }
      const directory = mkdtempSync(join(tmpdir(), 'revaro-audio-range-'))
      try {
        writeFileSync(join(directory, 'long.wav'), buffer)
        execFileSync(ffmpeg, ['-v', 'error', '-i', join(directory, 'long.wav'), '-c:a', 'flac', join(directory, 'long.flac')])
        buffer = readFileSync(join(directory, 'long.flac'))
      } finally { rmSync(directory, { recursive: true, force: true }) }
    }
    await login(page)
    await page.evaluate(() => localStorage.setItem('revaro-music-volume', '0'))
    await page.reload()
    const headers = { origin: new URL(page.url()).origin }
    const name = `audio-range-${format}-${Date.now()}`
    const created = await page.request.post('/api/directories', {
      headers, data: { parent_id: '00000000-0000-0000-0000-000000000000', name },
    })
    expect(created.status()).toBe(201)
    const directory = await created.json()
    try {
      const file = await uploadFixture(page, directory.id, `${name}.${format}`, `audio/${format}`, buffer)
      await uploadFixture(page, directory.id, `${name}.chapters.vtt`, 'text/vtt', Buffer.from('WEBVTT\n\n00:00:00.000 --> 00:20:00.000\nIntro\n\n00:20:00.000 --> 00:40:00.000\nMiddle\n\n00:40:00.000 --> 01:00:05.000\nEnd\n'))
      const metadata = await (await page.request.get(`/api/files/${file.id}/audio`)).json()
      expect(metadata.duration).toBe(3605)
      expect(metadata.chapters).toHaveLength(3)
      const head = await page.request.head(`/api/files/${file.id}/preview`)
      expect(head.status()).toBe(200)
      expect(head.headers()['accept-ranges']).toBe('bytes')
      expect(head.headers()['content-length']).toBe(String(buffer.length))
      for (const range of ['bytes=2000000-2004095', 'bytes=-4096']) {
        const response = await page.request.get(`/api/files/${file.id}/preview`, { headers: { range } })
        expect(response.status()).toBe(206)
        expect(response.headers()['content-length']).toBe('4096')
        expect(await response.body()).toEqual(range.startsWith('bytes=-') ? buffer.subarray(-4096) : buffer.subarray(2000000, 2004096))
      }
      const cdp = await page.context().newCDPSession(page)
      await cdp.send('Network.enable')
      await cdp.send('Network.emulateNetworkConditions', {
        offline: false, latency: 5, downloadThroughput: 1024 * 1024, uploadThroughput: -1,
      })
      const requests: { start: number, status: number }[] = []
      const networkRanges: [number, number][] = []
      page.context().on('response', response => {
        if (response.url().endsWith(`/api/files/${file.id}/preview`) && response.request().serviceWorker()) {
          const range = response.headers()['content-range']?.match(/bytes (\d+)-(\d+)\//)
          if (range) networkRanges.push([Number(range[1]), Number(range[2])])
        }
      })
      page.on('response', response => {
        if (response.url().endsWith(`/api/files/${file.id}/preview`)) {
          requests.push({ start: Number(response.headers()['content-range']?.match(/bytes (\d+)-/)?.[1]), status: response.status() })
        }
      })
      await page.goto('/music')
      await page.getByRole('button', { name: '打开搜索', exact: true }).click()
      await page.getByRole('searchbox').fill(file.name)
      await page.getByRole('button', { name: `打开 ${file.name}`, exact: true }).click()
      await openMusicPlayer(page)
      const audio = page.locator('audio').first()
      await expect(page.getByLabel('音乐播放进度', { exact: true })).toBeEnabled({ timeout: 45_000 })
      await expect(page.getByRole('button', { name: '暂停音乐', exact: true })).toBeVisible({ timeout: 45_000 })
      await page.getByRole('button', { name: '暂停音乐', exact: true }).click()
      const initialCount = requests.length
      await page.locator('.dock-chapter-marker[data-chapter-start="2400"]').click()
      await expect.poll(() => audio.evaluate((a: HTMLAudioElement) => !a.seeking && a.readyState >= 2 && Math.abs(a.currentTime - 2400) < 0.2), { timeout: 45_000 }).toBeTruthy()
      await expect.poll(() => requests.slice(initialCount).some(r => r.status === 206 && r.start > buffer.length / 2), { timeout: 45_000 }).toBeTruthy()
      await page.getByLabel('音乐播放进度', { exact: true }).evaluate((input: HTMLInputElement) => {
        input.value = '100'; input.dispatchEvent(new Event('change', { bubbles: true }))
      })
      await expect.poll(() => audio.evaluate((a: HTMLAudioElement) => !a.seeking && Math.abs(a.currentTime - 100) < 0.2), { timeout: 45_000 }).toBeTruthy()
      await page.getByRole('button', { name: '打开音频播放器', exact: true }).click()
      await expect(page.getByLabel('播放进度', { exact: true })).toHaveAttribute('max', '3605')
      await expect.poll(() => page.getByLabel('播放进度', { exact: true }).inputValue().then(Number)).toBeCloseTo(100, 1)
      // Worker responses expose real fetched windows; a page response can
      // advertise the whole stream even though its body was cancelled early.
      expect(networkRanges.length).toBeGreaterThan(0)
      let fetched = 0, last = -1
      for (const [start, end] of networkRanges.sort((a, b) => a[0] - b[0])) {
        fetched += Math.max(0, end - Math.max(start, last + 1) + 1)
        last = Math.max(last, end)
      }
      expect(fetched).toBeLessThan(buffer.length)
      expect(networkRanges.every(([start, end]) => end - start < 8 * 1024 * 1024)).toBeTruthy()
      expect(requests.every(response => response.status === 206)).toBeTruthy()
      await expect(audio).toHaveAttribute('src', `/api/files/${file.id}/preview`)
      await cdp.send('Network.emulateNetworkConditions', { offline: false, latency: 0, downloadThroughput: -1, uploadThroughput: -1 })
    } finally {
      await page.goto('/files')
      await page.request.delete(`/api/files/${directory.id}`, { headers })
      await page.request.delete(`/api/trash/${directory.id}`, { headers })
    }
  })
}

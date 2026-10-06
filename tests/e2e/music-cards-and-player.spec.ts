import { expect, test, type Page } from '@playwright/test'
import { login, navigate, enterSelectionMode, uploadFixture } from './helpers'
import { readFileSync } from 'node:fs'

const nativeFlac = readFileSync(new URL('../../crates/revaro-server/tests/fixtures/preview-chapters.flac', import.meta.url))
const transcript = Buffer.from('WEBVTT\n\n' + Array.from({ length: 28 }, (_, index) => {
  const second = String(index).padStart(2, '0')
  return `00:00:${second}.000 --> 00:00:${second}.750\nLine ${String(index + 1).padStart(2, '0')}\n`
}).join('\n'))

function wav() {
  const data = Buffer.alloc(44 + 16000 * 60)
  data.write('RIFF'); data.writeUInt32LE(data.length - 8, 4); data.write('WAVEfmt ', 8)
  data.writeUInt32LE(16, 16); data.writeUInt16LE(1, 20); data.writeUInt16LE(1, 22)
  data.writeUInt32LE(8000, 24); data.writeUInt32LE(16000, 28)
  data.writeUInt16LE(2, 32); data.writeUInt16LE(16, 34)
  data.write('data', 36); data.writeUInt32LE(data.length - 44, 40)
  return data
}

async function upload(page: Page, parent_id: string, name: string, data = wav(), mime = 'audio/wav') {
  return uploadFixture(page, parent_id, name, mime, data)
}

for (const device of [{ name: 'desktop', width: 1600, hasTouch: false }, { name: 'mobile', width: 390, hasTouch: true }, { name: 'compact', width: 320, hasTouch: true }]) {
  test.describe(device.name, () => {
    test.use({ viewport: { width: device.width, height: 900 }, hasTouch: device.hasTouch, reducedMotion: 'reduce' })

    test('square music cards and the restored player share one native audio session', async ({ page }) => {
      test.setTimeout(90_000)
      await login(page)
      const headers = { origin: new URL(page.url()).origin }
      const prefix = `music-cards-${Date.now()}`
      const created = await page.request.post('/api/directories', { headers, data: { parent_id: '00000000-0000-0000-0000-000000000000', name: prefix } })
      expect(created.status()).toBe(201)
      const { id } = await created.json()
      let collectionId: string | undefined
      try {
        const files = []
        files.push(await upload(page, id, `${prefix}-01.flac`, nativeFlac, 'audio/flac'))
        for (let index = 2; index <= 6; index++) files.push(await upload(page, id, `${prefix}-${String(index).padStart(2, '0')}.wav`))
        await upload(page, id, `${files[0].name}.vtt`, transcript, 'text/vtt')
        await upload(page, id, `${files[0].name}.chapters.vtt`, Buffer.from('WEBVTT\n\n00:00.000 --> 00:30.000\nSynthetic chapter\n'), 'text/vtt')
        const createdCollection = await page.request.post('/api/library/collections', { headers, data: { name: prefix, kind: 'audio' } })
        expect(createdCollection.status()).toBe(200)
        collectionId = (await createdCollection.json()).id
        for (const file of files) expect((await page.request.put(`/api/library/collections/${collectionId}/items/${file.id}`, { headers })).status()).toBe(204)
        await navigate(page, '音乐')
        await page.getByLabel('选择集合', { exact: true }).click()
        await page.getByRole('button', { name: `${prefix} · 6`, exact: true }).click()
        await expect(page.locator('.audio-stack-card,.stack-header')).toHaveCount(0)
        const cards = page.locator('.library-grid > .library-card')
        await expect(cards).toHaveCount(6)
        await expect(page.locator('.song-list,.song-row,.song-play,.song-menu-panel')).toHaveCount(0)
        await expect(page.getByRole('button', { name: '播放全部', exact: true })).toHaveCount(0)
        await expect(cards.first().locator('.card-info strong')).toHaveCSS('white-space', 'normal')
        const bounds = (await cards.first().boundingBox())!
        expect(Math.abs(bounds.width - bounds.height)).toBeLessThanOrEqual(1)
        expect(await page.evaluate(() => document.documentElement.scrollWidth)).toBe(device.width)
        await cards.first().getByRole('button', { name: `打开 ${files[0].name}`, exact: true }).click()
        await expect(page.getByRole('button', { name: '暂停音乐', exact: true })).toBeVisible()
        await page.getByRole('button', { name: '暂停音乐', exact: true }).click()
        const audio = page.locator('audio').first()
        const audioCount = await page.locator('audio').count()
        const source = await audio.getAttribute('src')
        await page.getByLabel('音乐播放进度', { exact: true }).evaluate((input: HTMLInputElement) => { input.value = '8'; input.dispatchEvent(new Event('change', { bubbles: true })) })
        await expect.poll(() => audio.evaluate((a: HTMLAudioElement) => a.currentTime)).toBeCloseTo(8, 1)
        await page.getByRole('button', { name: '打开音频播放器', exact: true }).click()
        const player = page.getByRole('dialog', { name: '音频播放器', exact: true })
        await expect(player.locator('.chapter-audio-player')).toBeVisible()
        await expect(player.getByRole('button', { name: '播放', exact: true })).toBeVisible()
        await expect(page.locator('audio')).toHaveCount(audioCount)
        await expect(audio).toHaveAttribute('src', source!)
        await expect.poll(() => audio.evaluate((a: HTMLAudioElement) => a.currentTime)).toBeCloseTo(8, 1)
        const lyrics = player.getByLabel('滚动台词', { exact: true })
        const scroll = lyrics.locator('.audio-transcript-scroll')
        await expect(lyrics.locator('.audio-transcript-line')).toHaveCount(28)
        await expect(lyrics.locator('[aria-current="true"]')).toHaveText('Line 09')
        await expect(player.locator('.audio-chapter-current,.audio-book-title,.audio-track-list,.audio-subtitles')).toHaveCount(0)
        await expect(page.getByRole('button', { name: '音轨列表', exact: true })).toHaveCount(0)
        await expect(page.getByRole('button', { name: '播放队列', exact: true })).toHaveCount(0)
        await expect.poll(() => scroll.evaluate((element: HTMLElement) => {
          const line = element.querySelector('[aria-current="true"]')!
          const viewport = element.getBoundingClientRect(), current = line.getBoundingClientRect()
          return Math.abs((current.top + current.bottom) / 2 - (viewport.top + viewport.bottom) / 2)
        })).toBeLessThan(4)
        const coverBounds = (await player.locator('.audio-cover').boundingBox())!
        const lyricsBounds = (await lyrics.boundingBox())!
        const controlBounds = (await player.locator('.audio-playback').boundingBox())!
        expect(lyricsBounds.y).toBeGreaterThanOrEqual(coverBounds.y + coverBounds.height)
        expect(lyricsBounds.y + lyricsBounds.height).toBeLessThanOrEqual(controlBounds.y)
        expect(controlBounds.y + controlBounds.height).toBeLessThanOrEqual(900)
        await scroll.dispatchEvent('wheel', { deltaY: -100 })
        await scroll.evaluate((element: HTMLElement) => { element.scrollTop = 0 })
        await expect(lyrics.getByRole('button', { name: '回到当前台词', exact: true })).toBeVisible()
        const seek = player.getByLabel('播放进度', { exact: true })
        await seek.evaluate((input: HTMLInputElement) => { input.value = '8.9'; input.dispatchEvent(new Event('change', { bubbles: true })) })
        await expect(lyrics.locator('[aria-current="true"]')).toHaveCount(0)
        await expect(scroll).toHaveJSProperty('scrollTop', 0)
        await seek.evaluate((input: HTMLInputElement) => { input.value = '2.1'; input.dispatchEvent(new Event('change', { bubbles: true })) })
        await expect(lyrics.locator('[aria-current="true"]')).toHaveText('Line 03')
        await expect(scroll).toHaveJSProperty('scrollTop', 0)
        await lyrics.getByRole('button', { name: '回到当前台词', exact: true }).click()
        await expect(lyrics.getByRole('button', { name: '回到当前台词', exact: true })).toHaveCount(0)
        await player.getByLabel('播放速度', { exact: true }).selectOption('1.25')
        await expect(audio).toHaveJSProperty('playbackRate', 1.25)
        await player.locator('[data-panel-trigger="chapters"]').click()
        await expect(player.locator('.audio-chapter-list > button')).toHaveCount(6)
        await player.locator('.audio-chapter-list > button').filter({ hasText: 'Chapter 3' }).click()
        await expect.poll(() => audio.evaluate((a: HTMLAudioElement) => a.currentTime)).toBeGreaterThanOrEqual(10)
        // On phones the chapter sheet covers the playback controls.
        await player.locator('.audio-panel .media-icon-button').click()
        await player.getByRole('button', { name: '暂停', exact: true }).click()
        await lyrics.getByRole('button', { name: 'Line 21', exact: true }).click()
        await expect.poll(() => audio.evaluate((a: HTMLAudioElement) => a.currentTime)).toBeCloseTo(20, 1)
        await expect(lyrics.locator('[aria-current="true"]')).toHaveText('Line 21')
        await expect(player.getByRole('button', { name: '播放', exact: true })).toBeVisible()
        await expect(audio).toHaveAttribute('src', source!)
        await expect(audio).toHaveJSProperty('playbackRate', 1.25)
        await expect(page.locator('audio')).toHaveCount(audioCount)
        await page.getByRole('button', { name: '收起音频播放器', exact: true }).click()
        await expect(player).toHaveCount(0)
        await expect(page.getByRole('button', { name: '播放音乐', exact: true })).toBeVisible()
        await expect(audio).toHaveAttribute('src', source!)
        await cards.last().getByRole('button', { name: `打开 ${files[5].name}`, exact: true }).click()
        await expect(page.getByRole('button', { name: '暂停音乐', exact: true })).toBeVisible()
        await expect(page.locator('.dock-track small')).toContainText('第 6 / 6 轨')
        await expect(audio).toHaveAttribute('src', `/api/files/${files[5].id}/preview`)
        await page.getByRole('button', { name: '打开音频播放器', exact: true }).click()
        await expect(player.getByLabel('滚动台词', { exact: true })).toHaveText('暂无台词')
        await page.getByRole('button', { name: '收起音频播放器', exact: true }).click()
        await enterSelectionMode(page)
        await cards.first().getByRole('checkbox').press('Space')
        await expect(page.getByRole('toolbar', { name: '所选项目操作', exact: true })).toBeVisible()
        await expect(page.getByRole('button', { name: '堆叠', exact: true })).toHaveCount(0)
        await expect(cards.first()).toHaveClass(/selected/)
        await expect(audio).toHaveAttribute('src', `/api/files/${files[5].id}/preview`)
      } finally {
        const collapse = page.getByRole('button', { name: '收起音频播放器', exact: true })
        if (await collapse.count()) await collapse.click()
        const stop = page.getByRole('button', { name: '停止音乐', exact: true })
        if (await stop.count()) await stop.click()
        if (collectionId) await page.request.delete(`/api/library/collections/${collectionId}`, { headers })
        await page.request.delete(`/api/files/${id}`, { headers })
        await page.request.delete(`/api/trash/${id}`, { headers })
      }
    })
  })
}

import { expect, test, type Locator } from '@playwright/test'
import { mkdirSync } from 'node:fs'
import { openMusicPlayer, pauseMusic, login, navigate, uploadFixture } from './helpers'

function wav() {
  const data = Buffer.alloc(44 + 16000 * 30)
  data.write('RIFF'); data.writeUInt32LE(data.length - 8, 4); data.write('WAVEfmt ', 8)
  data.writeUInt32LE(16, 16); data.writeUInt16LE(1, 20); data.writeUInt16LE(1, 22)
  data.writeUInt32LE(8000, 24); data.writeUInt32LE(16000, 28)
  data.writeUInt16LE(2, 32); data.writeUInt16LE(16, 34)
  data.write('data', 36); data.writeUInt32LE(data.length - 44, 40)
  return data
}

for (const device of [{ name: 'desktop', width: 1280, hasTouch: false }, { name: 'mobile', width: 320, hasTouch: true }]) {
  test.describe(device.name, () => {
    test.use({ viewport: { width: device.width, height: 900 }, hasTouch: device.hasTouch, reducedMotion: 'reduce' })

    test('one mode picker reflects the queue and lightweight chapter buttons close only the panel', async ({ page }) => {
      test.setTimeout(120_000)
      const errors: string[] = []
      page.on('pageerror', error => errors.push(error.message))
      await login(page)
      const headers = { origin: new URL(page.url()).origin }
      const prefix = `player-controls-${Date.now()}`
      const directory = await page.request.post('/api/directories', { headers, data: { parent_id: '00000000-0000-0000-0000-000000000000', name: prefix } })
      expect(directory.status()).toBe(201)
      const { id } = await directory.json()
      const collections: string[] = []
      try {
        const files = []
        for (let index = 1; index <= 3; index++) files.push(await uploadFixture(page, id, `${prefix}-${index}.wav`, 'audio/wav', wav()))
        await uploadFixture(page, id, `${files[0].name}.chapters.vtt`, 'text/vtt', Buffer.from('WEBVTT\n\n00:00.000 --> 00:10.000\nIntro\n\n00:10.000 --> 00:20.000\nMiddle\n\n00:20.000 --> 00:30.000\nEnd\n'))
        for (const [name, tracks] of [[prefix, files], [`${prefix}-single`, files.slice(0, 1)]] as const) {
          const created = await page.request.post('/api/library/collections', { headers, data: { name, kind: 'audio' } })
          expect(created.status()).toBe(200)
          const collection = await created.json()
          collections.push(collection.id)
          for (const file of tracks) expect((await page.request.put(`/api/library/collections/${collection.id}/items/${file.id}`, { headers })).status()).toBe(204)
        }
        await navigate(page, '音乐')
        await page.getByLabel('选择集合', { exact: true }).click()
        await page.getByRole('button', { name: `${prefix} · 3`, exact: true }).click()
        await page.getByRole('button', { name: `打开 ${files[0].name}`, exact: true }).click()
        await openMusicPlayer(page)
        await page.getByRole('button', { name: '暂停音乐', exact: true }).click()
        const audio = page.locator('audio').first()
        const audioCount = await page.locator('audio').count()
        const firstSource = `/api/files/${files[0].id}/preview`
        await page.getByLabel('音乐播放进度', { exact: true }).evaluate((input: HTMLInputElement) => { input.value = '8'; input.dispatchEvent(new Event('change', { bubbles: true })) })
        await expect(page.getByRole('button', { name: '随机播放', exact: true })).toHaveCount(0)
        await expect(page.getByRole('button', { name: '循环模式', exact: true })).toHaveCount(0)
        await expect(page.locator('.music-dock .playback-mode')).toHaveAttribute('data-playback-mode', 'sequential')

        const verifyClose = async (panel: Locator) => {
          const close = panel.getByRole('button', { name: '关闭章节', exact: true })
          await page.mouse.move(0, 0)
          expect((await close.boundingBox())!.width).toBe(36)
          expect((await close.boundingBox())!.height).toBe(36)
          await expect(close).toHaveCSS('border-top-width', '0px')
          await expect(close).toHaveCSS('background-color', 'rgba(0, 0, 0, 0)')
          await expect(close).toHaveCSS('box-shadow', 'none')
          await expect(close.locator('svg:visible')).toHaveCount(1)
          if (!device.hasTouch) {
            await close.hover()
            await expect(close).not.toHaveCSS('background-color', 'rgba(0, 0, 0, 0)')
          }
          await close.click()
          await expect(panel).toBeHidden()
          await expect(audio).toHaveAttribute('src', firstSource)
          await expect.poll(() => audio.evaluate((element: HTMLAudioElement) => element.currentTime)).toBeCloseTo(8, 1)
        }
        await page.getByRole('button', { name: '音轨章节', exact: true }).click()
        await verifyClose(page.locator('.music-chapters'))
        await page.getByRole('button', { name: '打开音频播放器', exact: true }).click()
        const player = page.getByRole('dialog', { name: '音频播放器', exact: true })
        const mode = player.getByRole('combobox', { name: '播放模式', exact: true })
        await expect(mode.locator('option')).toHaveText(['顺序播放', '随机播放', '列表循环', '单曲循环'])
        for (const value of ['shuffle', 'repeat-all', 'repeat-one', 'sequential']) {
          await mode.selectOption(value)
          await expect(mode).toHaveValue(value)
          await expect(audio).toHaveAttribute('src', firstSource)
          await expect(audio).toHaveJSProperty('paused', true)
          await expect.poll(() => audio.evaluate((element: HTMLAudioElement) => element.currentTime)).toBeCloseTo(8, 1)
          await expect(page.locator('audio')).toHaveCount(audioCount)
        }
        expect(await player.locator('.audio-options').evaluate(element => element.scrollWidth <= element.clientWidth)).toBeTruthy()
        await expect(player.locator('[data-panel-trigger="chapters"] span')).toHaveCSS('white-space', 'nowrap')
        await player.locator('[data-panel-trigger="chapters"]').click()
        await verifyClose(player.locator('.audio-panel'))
        await expect(player).toBeVisible()

        // Changing from shuffle to repeat-one must clear shuffle and restart this track.
        await mode.selectOption('shuffle')
        await mode.selectOption('repeat-one')
        await player.getByRole('button', { name: '播放', exact: true }).click()
        await audio.evaluate((element: HTMLAudioElement) => { element.currentTime = element.duration - 0.1 })
        await expect.poll(() => audio.evaluate((element: HTMLAudioElement) => element.currentTime)).toBeLessThan(5)
        await expect(audio).toHaveAttribute('src', firstSource)
        await expect(audio).toHaveJSProperty('paused', false)
        await player.getByRole('button', { name: '暂停', exact: true }).click()
        await mode.selectOption('shuffle')
        await page.getByRole('button', { name: '收起音频播放器', exact: true }).click()
        await expect(page.locator('.music-dock .playback-mode')).toHaveAttribute('data-playback-mode', 'shuffle')

        // A chaptered file in a one-track collection is still a single audio.
        await page.getByLabel('选择集合', { exact: true }).click()
        await page.getByRole('button', { name: `${prefix}-single · 1`, exact: true }).click()
        await page.getByRole('button', { name: `打开 ${files[0].name}`, exact: true }).click()
        await openMusicPlayer(page)
        await page.getByRole('button', { name: '暂停音乐', exact: true }).click()
        await expect(page.locator('.music-dock .playback-mode')).toHaveAttribute('data-playback-mode', 'sequential')
        await page.getByRole('button', { name: '打开音频播放器', exact: true }).click()
        await expect(mode.locator('option')).toHaveText(['播放一次', '单曲循环'])
        await expect(mode).toHaveValue('sequential')
        await player.locator('[data-panel-trigger="chapters"]').click()
        await expect(player.locator('.audio-chapter-list > button')).toHaveCount(3)
        await player.getByRole('button', { name: '关闭章节', exact: true }).click()
        await mode.selectOption('repeat-one')
        if (process.env.E2E_AUDIO_SCREENSHOTS) {
          mkdirSync(process.env.E2E_AUDIO_SCREENSHOTS, { recursive: true })
          await page.screenshot({ path: `${process.env.E2E_AUDIO_SCREENSHOTS}/controls-${device.name}.png` })
        }
        expect(await page.evaluate(() => document.documentElement.scrollWidth <= window.innerWidth)).toBeTruthy()
        expect(errors).toEqual([])
      } finally {
        const collapse = page.getByRole('button', { name: '收起音频播放器', exact: true })
        if (await collapse.count()) await collapse.click()
        await pauseMusic(page)
        for (const collection of collections) await page.request.delete(`/api/library/collections/${collection}`, { headers })
        await page.request.delete(`/api/files/${id}`, { headers })
        await page.request.delete(`/api/trash/${id}`, { headers })
      }
    })
  })
}

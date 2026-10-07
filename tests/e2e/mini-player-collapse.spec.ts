import { expect, test } from '@playwright/test'
import { mkdirSync } from 'node:fs'
import { login, navigate, pauseMusic, uploadFixture } from './helpers'

function wav() {
  const data = Buffer.alloc(44 + 16000 * 60)
  data.write('RIFF'); data.writeUInt32LE(data.length - 8, 4); data.write('WAVEfmt ', 8)
  data.writeUInt32LE(16, 16); data.writeUInt16LE(1, 20); data.writeUInt16LE(1, 22)
  data.writeUInt32LE(8000, 24); data.writeUInt32LE(16000, 28)
  data.writeUInt16LE(2, 32); data.writeUInt16LE(16, 34)
  data.write('data', 36); data.writeUInt32LE(data.length - 44, 40)
  return data
}

for (const width of [1600, 1024, 390, 320]) {
  test.describe(`mini player at ${width}px`, () => {
    test.use({ viewport: { width, height: 900 }, hasTouch: width < 1024, reducedMotion: 'no-preference' })

    test('desktop collapse preserves the audio session and mobile keeps its controls', async ({ page }) => {
      const errors: string[] = []
      page.on('pageerror', error => errors.push(error.message))
      await login(page)
      const headers = { origin: new URL(page.url()).origin }
      const prefix = `mini-collapse-${Date.now()}`
      const created = await page.request.post('/api/directories', { headers, data: { parent_id: '00000000-0000-0000-0000-000000000000', name: prefix } })
      expect(created.status()).toBe(201)
      const { id } = await created.json()
      let collectionId: string | undefined
      try {
        const files = []
        for (let index = 1; index <= 2; index++) files.push(await uploadFixture(page, id, `${prefix}-${index}.wav`, 'audio/wav', wav()))
        await uploadFixture(page, id, `${files[0].name}.chapters.vtt`, 'text/vtt', Buffer.from('WEBVTT\n\n00:00.000 --> 00:30.000\nIntro\n\n00:00:30.000 --> 00:01:00.000\nEnd\n'))
        const collection = await page.request.post('/api/library/collections', { headers, data: { name: prefix, kind: 'audio' } })
        expect(collection.status()).toBe(200)
        collectionId = (await collection.json()).id
        for (const file of files) await page.request.put(`/api/library/collections/${collectionId}/items/${file.id}`, { headers })
        await navigate(page, '音乐')
        await page.getByLabel('选择集合', { exact: true }).click()
        await page.getByRole('button', { name: `${prefix} · 2`, exact: true }).click()
        await page.getByRole('button', { name: `打开 ${files[0].name}`, exact: true }).click()
        const audio = page.locator('audio:not([aria-hidden="true"])')
        const dock = page.locator('.music-dock')
        const collapse = dock.getByRole('button', { name: '收起播放条', exact: true })
        const expand = page.locator('.dock-expand')
        await expect(audio).toHaveJSProperty('paused', false)
        await expect(page.getByRole('button', { name: '停止音乐', exact: true })).toHaveCount(0)
        await expect(dock.getByText('×', { exact: true })).toHaveCount(0)
        const audioCount = await page.locator('audio').count()
        let source = `/api/files/${files[0].id}/preview`

        if (width >= 1024) {
          await expect(collapse).toBeVisible()
          await expect(expand).toBeHidden()
          const dockBounds = (await dock.boundingBox())!
          const options = (await dock.locator('.dock-options').boundingBox())!
          const control = (await collapse.boundingBox())!
          expect(control.x).toBeGreaterThanOrEqual(options.x + options.width)
          expect(control.x + control.width).toBeLessThanOrEqual(width)
          await dock.getByRole('combobox', { name: '播放模式', exact: true }).selectOption('repeat-all')
          await dock.getByLabel('音乐音量', { exact: true }).evaluate((input: HTMLInputElement) => { input.value = '0.35'; input.dispatchEvent(new Event('input', { bubbles: true })) })
          await audio.evaluate((element: HTMLAudioElement) => {
            element.dataset.dockEvents = '0'
            for (const type of ['pause', 'play', 'loadstart', 'emptied', 'loadedmetadata']) element.addEventListener(type, () => { element.dataset.dockEvents = String(Number(element.dataset.dockEvents) + 1) })
          })
          const beforeTime = await audio.evaluate((element: HTMLAudioElement) => element.currentTime)
          const libraryBounds = (await page.locator('.library-grid').boundingBox())!
          await page.mouse.move(0, 0)
          await expect(collapse).toHaveCSS('border-top-width', '0px')
          await expect(collapse).toHaveCSS('outline-style', 'none')
          await expect(collapse).toHaveCSS('box-shadow', 'none')
          await expect(collapse).toHaveCSS('background-color', 'rgba(0, 0, 0, 0)')
          await collapse.hover()
          await expect(collapse).not.toHaveCSS('background-color', 'rgba(0, 0, 0, 0)')
          await collapse.click()
          await expect(expand).toBeVisible()
          await expect(expand).toBeFocused()
          await expect(dock).toHaveCSS('transition-duration', '0.24s, 0s')
          await expect.poll(() => dock.evaluate(element => element.getBoundingClientRect().right)).toBeLessThan(1)
          await expect(dock).toBeHidden()
          await expect(dock).toHaveAttribute('inert', '')
          await expect(dock).toHaveAttribute('aria-hidden', 'true')
          expect((await expand.boundingBox())!.width).toBe(28)
          await expect(expand).toHaveCSS('outline-style', 'none')
          await expect(expand).toHaveCSS('box-shadow', 'none')
          await expect(audio).toHaveAttribute('src', source)
          await expect(audio).toHaveJSProperty('paused', false)
          await expect(audio).toHaveJSProperty('volume', 0.35)
          await expect(audio).toHaveAttribute('data-dock-events', '0')
          await expect.poll(() => audio.evaluate((element: HTMLAudioElement) => element.currentTime)).toBeGreaterThan(beforeTime)
          expect((await page.locator('.library-grid').boundingBox())!).toEqual(libraryBounds)
          if (process.env.E2E_AUDIO_SCREENSHOTS) {
            mkdirSync(process.env.E2E_AUDIO_SCREENSHOTS, { recursive: true })
            await page.mouse.move(0, 0)
            await page.screenshot({ path: `${process.env.E2E_AUDIO_SCREENSHOTS}/mini-collapsed-${width}.png` })
          }

          // Tab from the page into the handle must restore its keyboard focus ring.
          await expand.evaluate(handle => {
            const candidates = [...document.querySelectorAll<HTMLElement>('button, summary, input, select, a[href], [tabindex="0"]')]
              .filter(element => !!(element.compareDocumentPosition(handle) & Node.DOCUMENT_POSITION_FOLLOWING)
                && element.getClientRects().length > 0 && !element.closest('[inert], [aria-hidden="true"]')
                && getComputedStyle(element).visibility !== 'hidden')
            candidates.at(-1)!.focus({ preventScroll: true })
          })
          await page.keyboard.press('Tab')
          await expect(expand).toBeFocused()
          await expect(expand).toHaveCSS('outline-width', '1px')
          await expect(expand).toHaveCSS('box-shadow', 'none')

          await expand.click()
          await expect(dock).toBeVisible()
          await expect.poll(async () => (await dock.boundingBox())!).toEqual(dockBounds)
          await expect(collapse).toBeFocused()
          await expect(collapse).toHaveCSS('outline-style', 'none')
          await expect(collapse).toHaveCSS('box-shadow', 'none')
          await expect(expand).toBeHidden()
          await expect(dock.getByRole('combobox', { name: '播放模式', exact: true })).toHaveValue('repeat-all')
          await expect(dock.locator('.dock-track small')).toContainText('第 1 / 2 轨')
          await expect(audio).toHaveAttribute('data-dock-events', '0')
          await expect(page.locator('audio')).toHaveCount(audioCount)

          // Both keyboard controls get a thin ring; pointer activation does not.
          await collapse.press('Enter')
          await expect(expand).toBeFocused()
          await expect(expand).toHaveCSS('outline-width', '1px')
          await expand.press('Enter')
          await expect(collapse).toBeFocused()
          await expect(collapse).toHaveCSS('outline-width', '1px')
          await expect(audio).toHaveAttribute('data-dock-events', '0')

          // A paused track stays paused and seeks are preserved through a round trip.
          await dock.getByRole('button', { name: '暂停音乐', exact: true }).click()
          await dock.getByLabel('音乐播放进度', { exact: true }).evaluate((input: HTMLInputElement) => { input.value = '12'; input.dispatchEvent(new Event('change', { bubbles: true })) })
          await collapse.click()
          await expect(dock).toBeHidden()
          await expand.click()
          await expect(dock).toBeVisible()
          await expect(audio).toHaveJSProperty('paused', true)
          await expect.poll(() => audio.evaluate((element: HTMLAudioElement) => element.currentTime)).toBeCloseTo(12, 1)

          // The queue still advances while every dock control is offscreen.
          await dock.getByRole('button', { name: '播放音乐', exact: true }).click()
          await collapse.click()
          await expect(dock).toBeHidden()
          await audio.evaluate((element: HTMLAudioElement) => { element.currentTime = element.duration - 0.1 })
          source = `/api/files/${files[1].id}/preview`
          await expect(audio).toHaveAttribute('src', source)
          await expect(audio).toHaveJSProperty('paused', false)
          await expect(dock).toBeHidden()
          await expect(expand).toBeVisible()
          await expand.click()
          await expect(dock.locator('.dock-track small')).toContainText('第 2 / 2 轨')
          await dock.getByRole('button', { name: '暂停音乐', exact: true }).click()

          // Resizing a collapsed desktop dock restores the mobile controls immediately.
          await collapse.click()
          await expect(dock).toBeHidden()
          await page.setViewportSize({ width: 390, height: 900 })
          await expect(dock).toBeVisible()
          await expect(dock).not.toHaveAttribute('inert', '')
          await expect(dock.locator('.dock-collapse')).toBeHidden()
          await expect(expand).toBeHidden()
          await expect(dock.getByRole('button', { name: '播放音乐', exact: true })).toBeVisible()
          await page.setViewportSize({ width, height: 900 })
          await expect(dock).toBeHidden()
          await expand.click()
          await expect(dock).toBeVisible()
          await expect.poll(async () => (await dock.boundingBox())!).toEqual(dockBounds)
          await expect(audio).toHaveJSProperty('paused', true)
          if (process.env.E2E_AUDIO_SCREENSHOTS) await page.screenshot({ path: `${process.env.E2E_AUDIO_SCREENSHOTS}/mini-expanded-${width}.png` })
        } else {
          await expect(dock).toBeVisible()
          await expect(dock.locator('.dock-collapse')).toBeHidden()
          await expect(expand).toBeHidden()
          await expect(dock.getByRole('button', { name: '暂停音乐', exact: true })).toBeVisible()
          await dock.getByRole('button', { name: '音轨章节', exact: true }).click()
          await expect(page.locator('.music-chapters')).toBeVisible()
          await page.getByRole('button', { name: '关闭章节', exact: true }).click()
          await expect(page.locator('.music-chapters')).toHaveCount(0)
          await expect(audio).toHaveJSProperty('paused', false)
        }
        await expect(audio).toHaveAttribute('src', source)
        await expect(page.locator('audio')).toHaveCount(audioCount)
        expect(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth)).toBeTruthy()
        expect(errors).toEqual([])
      } finally {
        await pauseMusic(page)
        if (collectionId) await page.request.delete(`/api/library/collections/${collectionId}`, { headers })
        await page.request.delete(`/api/files/${id}`, { headers })
        await page.request.delete(`/api/trash/${id}`, { headers })
      }
    })
  })
}

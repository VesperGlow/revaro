import { expect, test, type Locator, type Page } from '@playwright/test'
import { readFileSync } from 'node:fs'
import { login, navigate, uploadFixture as upload } from './helpers'

const root = '00000000-0000-0000-0000-000000000000'
const png = Buffer.from('iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mNk+A8AAQUBAScY42YAAAAASUVORK5CYII=', 'base64')
const flacWithCover = readFileSync(new URL('../../crates/revaro-media/tests/fixtures/preview-cover.flac', import.meta.url))

function wav() {
  const data = Buffer.alloc(44 + 16000)
  data.write('RIFF'); data.writeUInt32LE(data.length - 8, 4); data.write('WAVEfmt ', 8)
  data.writeUInt32LE(16, 16); data.writeUInt16LE(1, 20); data.writeUInt16LE(1, 22)
  data.writeUInt32LE(8000, 24); data.writeUInt32LE(16000, 28)
  data.writeUInt16LE(2, 32); data.writeUInt16LE(16, 34)
  data.write('data', 36); data.writeUInt32LE(data.length - 44, 40)
  return data
}


async function stableCaption(page: Page, card: Locator) {
  await card.scrollIntoViewIfNeeded()
  await page.mouse.move(0, 0)
  const caption = card.locator('.card-info')
  const title = caption.locator('strong')
  const placement = () => title.evaluate(element => {
    const titleBounds = element.getBoundingClientRect()
    const contentBounds = element.closest('.file-card-content')!.getBoundingClientRect()
    return { x: titleBounds.x - contentBounds.x, y: titleBounds.y - contentBounds.y, width: titleBounds.width, height: titleBounds.height }
  })
  await expect(caption).toHaveCSS('opacity', '1')
  await expect(title).toHaveCSS('-webkit-line-clamp', '2')
  await expect(title).toHaveCSS('white-space', 'normal')
  const before = await title.boundingBox()
  const beforePlacement = await placement()
  const bounds = (await card.boundingBox())!
  const icon = card.locator('.card-preview .file-type-icon')
  if (await icon.count()) {
    const iconBounds = (await icon.boundingBox())!
    const previewBounds = (await card.locator('.card-preview').boundingBox())!
    expect(iconBounds.y).toBeGreaterThanOrEqual(previewBounds.y)
    expect(iconBounds.y + iconBounds.height).toBeLessThanOrEqual(previewBounds.y + previewBounds.height)
  }
  expect(before!.y + before!.height).toBeLessThanOrEqual(bounds.y + bounds.height)
  expect(before!.y).toBeGreaterThan(bounds.y + bounds.height / 2)
  await card.hover()
  expect(await placement()).toEqual(beforePlacement)
  await title.evaluate(element => ((element.closest('button') || element.closest('article')) as HTMLElement).focus())
  expect(await placement()).toEqual(beforePlacement)
  expect(await title.evaluate(element => element.clientHeight)).toBeLessThanOrEqual(34)
  return before
}

for (const device of [
  { name: 'desktop', width: 1280, hasTouch: false },
  { name: 'mobile', width: 390, hasTouch: true },
  { name: 'compact mobile', width: 320, hasTouch: true },
  { name: 'wide touch', width: 1280, hasTouch: true },
]) {
  test.describe(device.name, () => {
    test.use({ viewport: { width: device.width, height: 900 }, hasTouch: device.hasTouch })

    test('file, home and library cards keep equal heights and persistent two-line filenames', async ({ page }) => {
      await login(page)
      const headers = { origin: new URL(page.url()).origin }
      const response = await page.request.post('/api/directories', { headers, data: { parent_id: root, name: `cards-${Date.now()}` } })
      expect(response.status()).toBe(201)
      const { id } = await response.json()
      const prefix = `卡片-${Date.now()}`
      const long = '很长的文件名称需要两行显示并省略多余文字'.repeat(3)
      try {
        const book = await upload(page, id, `${prefix}-${long}.txt`, 'text/plain', Buffer.from('第一章\n\n这是一份用于卡片布局检查的文本。'))
        const audio = await upload(page, id, `${prefix}-${long}.wav`, 'audio/wav', wav())
        const coveredAudio = await upload(page, id, `${prefix}-${long}.flac`, 'audio/flac', flacWithCover)
        const photo = await upload(page, id, `${prefix}-${long}.png`, 'image/png', png)
        const video = await upload(page, id, `${prefix}-${long}.webm`, 'video/webm', readFileSync(new URL('./fixtures/preview.webm', import.meta.url)))
        const listing = await page.request.get(`/api/files?parent_id=${id}`)
        expect(listing.ok()).toBeTruthy()
        expect((await listing.json()).items.find((file: any) => file.id === coveredAudio.id).has_cover).toBeFalsy()
        for (const file of [book, audio, coveredAudio, photo, video]) {
          expect((await page.request.patch(`/api/library/items/${file.id}`, { headers, data: { opened: true } })).ok()).toBeTruthy()
        }
        expect((await page.request.post('/api/directories', { headers, data: { parent_id: id, name: '文件夹' } })).ok()).toBeTruthy()

        await page.goto(`/f/${id}`)
        const cards = page.locator('.file-grid>.file-card')
        await expect(cards).toHaveCount(6)
        const audioCover = cards.filter({ hasText: coveredAudio.name }).locator('.card-preview img')
        await expect(audioCover).toBeVisible()
        await expect.poll(() => audioCover.evaluate((img: HTMLImageElement) => img.naturalWidth)).toBe(640)
        await expect(cards.filter({ hasText: audio.name }).locator('.card-preview img')).toHaveCount(0)
        await expect(page.locator('.preview-tile .card-preview img')).toHaveCount(3)
        for (const icon of ['.folder-type-icon', '.audio-type-icon', '.document-type-icon']) {
          await expect(page.locator(`.file-grid ${icon}`)).toBeVisible()
          const iconBounds = (await page.locator(`.file-grid ${icon}`).boundingBox())!
          const preview = (await page.locator(`.file-grid .card-preview`).filter({ has: page.locator(icon) }).boundingBox())!
          expect(Math.abs(iconBounds.y + iconBounds.height / 2 - preview.y - preview.height / 2)).toBeLessThan(1)
        }
        const heights = await cards.evaluateAll(elements => elements.map(element => element.getBoundingClientRect().height))
        expect(Math.max(...heights) - Math.min(...heights)).toBeLessThan(1)
        for (const card of await cards.all()) await stableCaption(page, card)
        expect(await cards.filter({ hasText: photo.name }).locator('strong').evaluate(element => element.scrollHeight > element.clientHeight)).toBe(true)

        for (const [destination, file] of [['书籍', book], ['音乐', audio], ['图片', photo], ['视频', video]] as const) {
          await navigate(page, destination)
          const card = page.getByRole('button', { name: `打开 ${file.name}`, exact: true })
          await stableCaption(page, card)
          await expect(card.locator('.file-card-content>.card-preview')).toHaveCount(1)
        }
        await page.goto(`/f/${id}`)
        await expect(audioCover).toBeVisible()
        await expect.poll(() => audioCover.evaluate((img: HTMLImageElement) => img.naturalWidth)).toBe(640)
        await navigate(page, '首页')
        const homeHeights: number[] = []
        for (const file of [book, audio, photo, video]) {
          const card = page.getByRole('button', { name: `打开 ${file.name}`, exact: true })
          await stableCaption(page, card)
          homeHeights.push((await card.boundingBox())!.height)
        }
        expect(Math.max(...homeHeights) - Math.min(...homeHeights)).toBeLessThan(1)
        expect(await page.evaluate(() => document.documentElement.scrollWidth)).toBe(device.width)

        await page.context().route(`**/api/files/${photo.id}/thumbnail*`, route => route.fulfill({ status: 404 }))
        await page.context().route(`**/api/files/${photo.id}/preview`, route => route.fulfill({ status: 404 }))
        await page.goto(`/f/${id}`)
        const broken = page.locator('.file-card').filter({ hasText: photo.name })
        await expect(broken.locator('.file-type-icon')).toBeVisible()
        await stableCaption(page, broken)
        const fallbackHeights = await cards.evaluateAll(elements => elements.map(element => element.getBoundingClientRect().height))
        expect(Math.max(...fallbackHeights) - Math.min(...fallbackHeights)).toBeLessThan(1)

        // Audio thumbnail failures must use the icon, never request raw audio
        // through the image preview URL, and retain the same card layout.
        let audioPreviewRequested = false
        await page.context().route(`**/api/files/${coveredAudio.id}/preview*`, route => {
          audioPreviewRequested = true
          return route.fulfill({ status: 500 })
        })
        await page.context().route(`**/api/files/${coveredAudio.id}/thumbnail*`, route => route.fulfill({ status: 404 }))
        await page.goto(`/f/${id}`)
        const brokenAudio = cards.filter({ hasText: coveredAudio.name })
        await expect(brokenAudio.locator('.audio-type-icon')).toBeVisible()
        await expect(brokenAudio.locator('.card-preview img')).toHaveCount(0)
        await expect(brokenAudio).toHaveClass(/fallback-tile/)
        await stableCaption(page, brokenAudio)
        expect(audioPreviewRequested).toBe(false)
      } finally {
        await page.request.delete(`/api/files/${id}`, { headers })
        await page.request.delete(`/api/trash/${id}`, { headers })
      }
    })
  })
}

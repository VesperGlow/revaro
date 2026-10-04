import { expect, test, type Locator, type Page } from '@playwright/test'
import { readFileSync } from 'node:fs'
import { login } from './helpers'
import { zip } from './fixtures/epub'

const png = Buffer.from('iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mNk+A8AAQUBAScY42YAAAAASUVORK5CYII=', 'base64')
const xml = (value: string) => value.replaceAll('&', '&amp;').replaceAll('<', '&lt;').replaceAll('"', '&quot;')

function epub(series: string, index: number, calibre = false) {
  const metadata = calibre
    ? `<meta name="calibre:series" content="${xml(series)}"/><meta name="calibre:series_index" content="${index}"/>`
    : `<meta property="belongs-to-collection" id="series">${xml(series)}</meta><meta refines="#series" property="collection-type">series</meta><meta refines="#series" property="group-position">${index}</meta>`
  const chapter = `<html><body><h1>第${index}卷</h1>${'<p>这是一段用于验证真实阅读进度的文字。书架、系列和触屏都应保持相同的阅读体验。</p>'.repeat(120)}</body></html>`
  return zip([
    ['META-INF/container.xml', Buffer.from('<container><rootfiles><rootfile full-path="content.opf"/></rootfiles></container>')],
    ['content.opf', Buffer.from(`<package><metadata><title>第${index}卷</title>${metadata}</metadata><manifest><item id="chapter" href="chapter.xhtml" media-type="application/xhtml+xml"/></manifest><spine><itemref idref="chapter"/></spine></package>`) ],
    ['chapter.xhtml', Buffer.from(chapter)],
  ])
}

function wav() {
  const data = Buffer.alloc(44 + 16000)
  data.write('RIFF'); data.writeUInt32LE(data.length - 8, 4); data.write('WAVEfmt ', 8)
  data.writeUInt32LE(16, 16); data.writeUInt16LE(1, 20); data.writeUInt16LE(1, 22)
  data.writeUInt32LE(8000, 24); data.writeUInt32LE(16000, 28)
  data.writeUInt16LE(2, 32); data.writeUInt16LE(16, 34)
  data.write('data', 36); data.writeUInt32LE(16000, 40)
  return data
}

async function navigate(page: Page, name: string, touch: boolean) {
  await page.getByRole('navigation', { name: touch ? '移动端导航' : '主导航', exact: true }).getByRole('link', { name, exact: true }).click()
}

async function press(page: Page, card: Locator, touch: boolean, cancel: false | 'move' | 'scroll' = false) {
  await card.evaluate(async el => {
    el.scrollIntoView({ block: 'center' })
    await new Promise(resolve => requestAnimationFrame(() => requestAnimationFrame(resolve)))
  })
  const box = (await card.boundingBox())!
  const point = { x: box.x + box.width / 2, y: box.y + box.height / 2 }
  const session = touch ? await page.context().newCDPSession(page) : null
  if (session) await session.send('Input.dispatchTouchEvent', { type: 'touchStart', touchPoints: [point] })
  else { await page.mouse.move(point.x, point.y); await page.mouse.down() }
  if (cancel) {
    if (cancel === 'scroll') {
      await page.evaluate(() => window.dispatchEvent(new Event('scroll')))
      if (session) await session.send('Input.dispatchTouchEvent', { type: 'touchCancel', touchPoints: [] })
      else await page.mouse.move(box.x + box.width + 40, point.y + 35)
    } else if (session) {
      await session.send('Input.dispatchTouchEvent', { type: 'touchMove', touchPoints: [{ x: point.x, y: point.y + 35 }] })
      await session.send('Input.dispatchTouchEvent', { type: 'touchCancel', touchPoints: [] })
    } else await page.mouse.move(box.x + box.width + 40, point.y + 35)
    await page.waitForTimeout(600)
  } else await expect(page.locator('.selection-toggle')).toHaveAttribute('aria-pressed', 'true')
  if (session) {
    if (!cancel) await session.send('Input.dispatchTouchEvent', { type: 'touchEnd', touchPoints: [] })
    await session.detach()
  } else await page.mouse.up()
}

for (const touch of [false, true]) {
  test.describe(touch ? 'touch book interactions' : 'mouse book interactions', () => {
    test.use({ hasTouch: touch, viewport: touch ? { width: 390, height: 844 } : { width: 1280, height: 800 } })

    test('series cards reuse selection, show saved progress and restore all volumes after reading', async ({ page }) => {
      const errors: string[] = []
      page.on('pageerror', error => errors.push(error.message))
      const prefix = `series-${Date.now()}-${touch}`
      const seriesName = `星河 / 海 & 天 ${prefix}`
      const first = `${prefix}-volume-1.epub`, second = `${prefix}-volume-2.epub`, standalone = `${prefix}-alone.txt`
      await login(page)
      await page.locator('input[type=file]').first().setInputFiles([
        { name: first, mimeType: 'application/epub+zip', buffer: epub(seriesName, 1) },
        { name: second, mimeType: 'application/epub+zip', buffer: epub(seriesName, 2, true) },
        { name: standalone, mimeType: 'text/plain', buffer: Buffer.from('没有系列信息的独立书籍\n'.repeat(20)) },
      ])
      await expect.poll(async () => (await (await page.request.get(`/api/library/items?kind=book&q=${prefix}`)).json()).total).toBe(3)
      const listing = await (await page.request.get(`/api/library/items?kind=book&q=${prefix}`)).json()
      const book = listing.items.find((item: any) => item.file.name === first)
      const saved = await page.request.put(`/api/files/${book.file.id}/book/progress`, { headers: { origin: new URL(page.url()).origin }, data: { anchor: { spine: 0, block: 20, offset: 0 }, percent: 37.5 } })
      expect(saved.ok()).toBeTruthy()
      await navigate(page, '书籍', touch)
      const card = page.locator('.series-card').filter({ hasText: seriesName })
      await expect(card).toHaveCount(1)
      await expect(card.locator('.series-count')).toHaveText('2 本')
      const progress = card.getByRole('progressbar', { name: '阅读进度' })
      await expect(progress).toHaveAttribute('aria-valuenow', '18.75')
      await expect(page.locator('.library-card').filter({ hasText: standalone }).locator('.book-reading-progress')).toHaveCount(0)
      if (touch) await expect(progress).toHaveCSS('opacity', '1')
      else {
        await page.mouse.move(1, 1)
        await expect(progress).toHaveCSS('opacity', '0')
        await card.hover()
        await expect(progress).toHaveCSS('opacity', '1')
      }
      await press(page, card, touch)
      await expect(page.locator('.selection-summary b')).toHaveText('已选择 2 项')
      await expect(page.locator('.series-header')).toHaveCount(0)
      await expect(page.locator('#reader-view')).toHaveCount(0)
      await page.locator('.selection-toolbar').getByRole('button', { name: '收藏', exact: true }).click()
      await expect.poll(async () => (await (await page.request.get(`/api/library/items?kind=book&q=${prefix}&favorite=true`)).json()).total).toBe(2)
      await page.locator('.selection-toolbar').getByRole('button', { name: '取消', exact: true }).click()
      await card.locator('.library-card-open').click()
      await expect(page.locator('.series-header h1')).toHaveText(seriesName)
      await expect(page.locator('.library-card')).toHaveCount(2)
      await expect(page.locator('.library-card-open').first()).toHaveAttribute('aria-label', `打开 ${first}`)
      await expect(page.locator('.library-card-open').last()).toHaveAttribute('aria-label', `打开 ${second}`)
      const detailUrl = page.url()
      await page.reload()
      await expect(page.locator('.series-header h1')).toHaveText(seriesName)
      await expect(page.locator('.library-card')).toHaveCount(2)
      await page.getByRole('button', { name: `打开 ${first}`, exact: true }).click()
      await expect(page.locator('#loading')).toBeHidden()
      await expect(page.locator('#flow')).toContainText('验证真实阅读进度')
      await page.locator('#reader-back').click()
      await expect(page).toHaveURL(detailUrl)
      await expect(page.locator('.series-header h1')).toHaveText(seriesName)
      await expect(page.locator('.library-card')).toHaveCount(2)
      // A new reader save supplies the same percentage to the card and progress API.
      await expect.poll(async () => (await (await page.request.get(`/api/files/${book.file.id}/book/progress`)).json()).percent).not.toBe(37.5)
      const updated = await (await page.request.get(`/api/files/${book.file.id}/book/progress`)).json()
      if (updated.percent > 0) await expect(page.locator('.library-card').first().getByRole('progressbar')).toHaveAttribute('aria-valuenow', `${updated.percent}`)
      await page.getByRole('button', { name: '← 返回书架' }).click()
      await expect(card).toHaveCount(1)
      expect(errors).toEqual([])
    })

    test('long press works across listings and moving or scrolling cancels the gesture', async ({ page }) => {
      const errors: string[] = []
      page.on('pageerror', error => errors.push(error.message))
      const prefix = `press-${Date.now()}-${touch}`
      await login(page)
      await page.locator('input[type=file]').first().setInputFiles([
        { name: `${prefix}.txt`, mimeType: 'text/plain', buffer: Buffer.from('长按选择测试\n'.repeat(30)) },
        { name: `${prefix}.png`, mimeType: 'image/png', buffer: png },
        { name: `${prefix}.wav`, mimeType: 'audio/wav', buffer: wav() },
        { name: `${prefix}.webm`, mimeType: 'video/webm', buffer: readFileSync(new URL('./fixtures/preview.webm', import.meta.url)) },
      ])
      await expect.poll(async () => (await (await page.request.get(`/api/library/items?q=${prefix}`)).json()).total).toBe(4)
      const fileCard = page.locator('.file-card').filter({ hasText: `${prefix}.txt` })
      await press(page, fileCard, touch, 'move')
      await expect(page.locator('.selection-toggle')).toHaveAttribute('aria-pressed', 'false')
      await press(page, fileCard, touch, 'scroll')
      await expect(page.locator('.selection-toggle')).toHaveAttribute('aria-pressed', 'false')
      for (const name of ['文件', '书籍', '音乐', '图片', '视频', '首页']) {
        if (name !== '文件') await navigate(page, name, touch)
        const cards = page.locator(name === '首页' ? '.home-card' : name === '文件' ? '.file-card' : '.library-card')
        const card = name === '首页' ? cards.first() : cards.filter({ hasText: prefix }).first()
        await expect(card).toBeVisible()
        await press(page, card, touch)
        await expect(page.locator('.selection-summary b')).toHaveText('已选择 1 项')
        await expect(page.locator('#reader-view,.media-preview,.document-editor')).toHaveCount(0)
        await page.locator('.selection-toolbar').getByRole('button', { name: '取消', exact: true }).click()
      }
      await navigate(page, '文件', touch)
      const files = await (await page.request.get(`/api/library/items?q=${prefix}`)).json()
      const id = files.items.find((item: any) => item.file.name.endsWith('.txt')).file.id
      expect((await page.request.delete(`/api/files/${id}`, { headers: { origin: new URL(page.url()).origin } })).ok()).toBeTruthy()
      await navigate(page, '回收站', touch)
      await press(page, page.locator('.file-card').filter({ hasText: `${prefix}.txt` }), touch)
      await expect(page.locator('.selection-summary b')).toHaveText('已选择 1 项')
      await page.locator('.selection-toolbar').getByRole('button', { name: '取消', exact: true }).click()
      expect(errors).toEqual([])
    })
  })
}

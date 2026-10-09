import { expect, test, type Locator, type Page } from '@playwright/test'
import { readFileSync } from 'node:fs'
import { login, navigate, failApplicationRequest } from './helpers'
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
    ['content.opf', Buffer.from(`<package><metadata><title>第${index}卷</title>${metadata}</metadata><manifest><item id="cover" href="cover.png" media-type="image/png" properties="cover-image"/><item id="chapter" href="chapter.xhtml" media-type="application/xhtml+xml"/></manifest><spine><itemref idref="chapter"/></spine></package>`) ],
    ['cover.png', png],
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

async function stackMenu(page: Page, action: string) {
  await page.getByLabel('堆叠更多操作', { exact: true }).click()
  await page.locator('.stack-header .action-menu-panel').getByRole('button', { name: action, exact: true }).click()
}

async function dragBook(page: Page, source: Locator, target: Locator) {
  await expect(source).toHaveAttribute('draggable', 'true')
  await source.hover()
  await expect(target).toBeVisible()
  const box = (await target.boundingBox())!
  const point = { x: box.x + box.width / 2, y: box.y + box.height / 2 }
  await page.mouse.down()
  try {
    await page.mouse.move(point.x, point.y, { steps: 8 })
    // A second move lets the browser deliver dragover after native dragstart.
    await page.mouse.move(point.x, point.y)
    await expect(target).toHaveClass(/stack-drop-target/)
  } finally {
    await page.mouse.up()
  }
}

async function holdDrag(page: Page, source: Locator, target: Locator, touch: boolean) {
  await source.evaluate(async el => {
    el.scrollIntoView({ block: 'center' })
    await new Promise(resolve => requestAnimationFrame(() => requestAnimationFrame(resolve)))
  })
  const box = (await source.boundingBox())!
  const point = { x: box.x + box.width / 2, y: box.y + box.height / 2 }
  const session = touch ? await page.context().newCDPSession(page) : null
  if (session) await session.send('Input.dispatchTouchEvent', { type: 'touchStart', touchPoints: [point] })
  else { await page.mouse.move(point.x, point.y); await page.mouse.down() }
  await page.waitForTimeout(600)
  await expect(page.getByRole('toolbar', { name: '管理堆叠书籍' })).toBeVisible()
  // Entering management reveals its toolbar; use the target's new position.
  const destination = (await target.boundingBox())!
  const end = { x: destination.x + destination.width / 2, y: destination.y + destination.height / 2 }
  for (let i = 1; i <= 8; i++) {
    const next = { x: point.x + (end.x - point.x) * i / 8, y: point.y + (end.y - point.y) * i / 8 }
    if (session) await session.send('Input.dispatchTouchEvent', { type: 'touchMove', touchPoints: [next] })
    else await page.mouse.move(next.x, next.y)
  }
  await expect(target).toHaveClass(/stack-order-target/)
  if (session) {
    await session.send('Input.dispatchTouchEvent', { type: 'touchEnd', touchPoints: [] })
    await session.detach()
  } else await page.mouse.up()
  await expect(page.locator('.stack-drag-preview')).toHaveCount(0)
  await expect(page.getByRole('status').filter({ hasText: '顺序已保存' })).toHaveCount(1)
}

for (const touch of [false, true]) {
  test.describe(touch ? 'touch book interactions' : 'mouse book interactions', () => {
    test.use({ hasTouch: touch, viewport: touch ? { width: 390, height: 844 } : { width: 1280, height: 800 } })

    test('manual stacks reuse long press selection, persist ordering and preserve files when dissolved', async ({ page }) => {
      test.setTimeout(90_000)
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
      await navigate(page, '书籍')
      await expect(page.locator('.series-card,.stack-card').filter({ hasText: seriesName })).toHaveCount(0)
      // Recommendations leave every book independent until the user submits a stack.
      await page.getByRole('button', { name: '推荐堆叠', exact: true }).click()
      const stackDialog = page.getByRole('dialog', { name: '管理堆叠', exact: true })
      await expect(stackDialog.getByRole('button', { name: new RegExp(seriesName.replace(/[.*+?^${}()|[\]\\]/g, '\\$&')) })).toBeVisible()
      await stackDialog.getByRole('button', { name: '关闭堆叠对话框' }).click()
      await expect(page.locator('.stack-card').filter({ hasText: seriesName })).toHaveCount(0)
      const firstCard = page.locator('.library-card').filter({ has: page.getByRole('button', { name: `打开 ${first}`, exact: true }) })
      await press(page, firstCard, touch)
      await page.getByRole('button', { name: `打开 ${second}`, exact: true }).click({ force: true })
      await expect(page.locator('.selection-summary b')).toHaveText('已选择 2 项')
      await page.locator('.selection-toolbar').getByRole('button', { name: '堆叠', exact: true }).click()
      await stackDialog.getByLabel('堆叠名称', { exact: true }).fill(seriesName)
      await stackDialog.getByRole('button', { name: '新建堆叠', exact: true }).click()
      await expect(stackDialog).toHaveCount(0)
      const card = page.locator('.stack-card').filter({ hasText: seriesName })
      await expect(card).toHaveCount(1)
      await expect(card.locator('.stack-count')).toHaveText('2 本')
      await expect(card.locator('.stack-cover-layer')).toHaveCount(2)
      // Manual order is editable independently of volume metadata.
      await card.locator('.library-card-open').click()
      await expect(page.locator('.stack-remove-book')).toHaveCount(0)
      await expect(page.getByRole('toolbar', { name: '管理堆叠书籍' })).toHaveCount(0)
      await expect(page.locator('.stack-header-actions > *')).toHaveCount(2)
      await expect(page.getByRole('button', { name: '内部排序', exact: true })).toHaveCount(0)
      const detailBook = (name: string) => page.locator('.library-card').filter({ has: page.getByRole('button', { name: `打开 ${name}`, exact: true }) })
      await expect(detailBook(first).locator('.library-cover img')).toBeVisible()
      await press(page, detailBook(first), touch, 'move')
      await press(page, detailBook(first), touch, 'scroll')
      await expect(page.getByRole('toolbar', { name: '管理堆叠书籍' })).toHaveCount(0)
      await holdDrag(page, detailBook(first), detailBook(second), touch)
      if (await page.locator('.library-card-open').first().getAttribute('aria-label') !== `打开 ${first}`) {
        await holdDrag(page, detailBook(first), detailBook(second), touch)
      }
      await expect(page.locator('.library-card-open').first()).toHaveAttribute('aria-label', `打开 ${first}`)
      await page.getByRole('toolbar', { name: '管理堆叠书籍' }).getByRole('button', { name: '完成', exact: true }).click()
      await page.getByRole('button', { name: '返回书架', exact: true }).click()
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
      await expect(page.locator('.stack-header')).toHaveCount(0)
      await expect(page.locator('#reader-view')).toHaveCount(0)
      await page.locator('.selection-toolbar').getByRole('button', { name: '收藏', exact: true }).click()
      await expect.poll(async () => (await (await page.request.get(`/api/library/items?kind=book&q=${prefix}&favorite=true`)).json()).total).toBe(2)
      await page.locator('.selection-toolbar').getByRole('button', { name: '取消', exact: true }).click()
      await card.locator('.library-card-open').click()
      await expect(page.locator('.stack-header h1')).toHaveText(seriesName)
      await expect(page.locator('.library-card')).toHaveCount(2)
      await expect(page.locator('.library-card-open').first()).toHaveAttribute('aria-label', `打开 ${first}`)
      await expect(page.locator('.library-card-open').last()).toHaveAttribute('aria-label', `打开 ${second}`)
      const detailUrl = page.url()
      await page.reload()
      await expect(page.locator('.stack-header h1')).toHaveText(seriesName)
      await expect(page.locator('.library-card')).toHaveCount(2)
      await page.getByRole('button', { name: `打开 ${first}`, exact: true }).click()
      await expect(page.locator('#loading')).toBeHidden()
      await expect(page.locator('#flow')).toContainText('验证真实阅读进度')
      await page.locator('#reader-back').click()
      await expect(page).toHaveURL(detailUrl)
      await expect(page.locator('.stack-header h1')).toHaveText(seriesName)
      await expect(page.locator('.library-card')).toHaveCount(2)
      // A new reader save supplies the same percentage to the card and progress API.
      await expect.poll(async () => (await (await page.request.get(`/api/files/${book.file.id}/book/progress`)).json()).percent).not.toBe(37.5)
      const updated = await (await page.request.get(`/api/files/${book.file.id}/book/progress`)).json()
      if (updated.percent > 0) await expect(page.locator('.library-card').first().getByRole('progressbar')).toHaveAttribute('aria-valuenow', `${updated.percent}`)
      await page.getByRole('button', { name: '返回书架', exact: true }).click()
      await expect(card).toHaveCount(1)
      await card.locator('.library-card-open').click()
      await stackMenu(page, '重命名')
      await stackDialog.getByLabel('堆叠名称', { exact: true }).fill(`Renamed ${prefix}`)
      await stackDialog.getByRole('button', { name: '保存名称', exact: true }).click()
      await expect(page.locator('.stack-header h1')).toHaveText(`Renamed ${prefix}`)
      await page.getByRole('button', { name: '添加书籍', exact: true }).click()
      const picker = page.getByRole('dialog', { name: '添加书籍', exact: true })
      await picker.getByLabel('搜索待添加书籍').fill(standalone)
      await picker.getByRole('checkbox', { name: `添加 ${standalone}`, exact: true }).check()
      await expect(picker.locator('.stack-picker-footer')).toContainText('已选 1 本')
      await picker.getByRole('button', { name: '添加', exact: true }).click()
      await expect(page.locator('.library-card')).toHaveCount(3)
      await stackMenu(page, '管理书籍')
      const management = page.getByRole('toolbar', { name: '管理堆叠书籍' })
      await expect(management.getByRole('button', { name: '移出堆叠', exact: true })).toBeDisabled()
      await management.getByRole('button', { name: '全选', exact: true }).click()
      await expect(management).toContainText('已选 3 本')
      await management.getByRole('button', { name: '取消全选', exact: true }).click()
      await expect(management).toContainText('已选 0 本')
      await page.getByRole('button', { name: `打开 ${standalone}`, exact: true }).click({ force: true })
      await page.getByRole('button', { name: `打开 ${second}`, exact: true }).click({ force: true })
      await expect(management).toContainText('已选 2 本')
      await management.getByRole('button', { name: '移出堆叠', exact: true }).click()
      await expect(page.locator('.library-card')).toHaveCount(1)
      await expect(management).toHaveCount(0)
      await page.getByRole('button', { name: '添加书籍', exact: true }).click()
      await picker.getByLabel('搜索待添加书籍').fill(second)
      await picker.getByRole('checkbox', { name: `添加 ${second}`, exact: true }).check()
      await picker.getByRole('button', { name: '添加', exact: true }).click()
      await expect(page.locator('.library-card')).toHaveCount(2)
      expect((await (await page.request.get(`/api/library/items?kind=book&q=${standalone}`)).json()).total).toBe(1)
      await page.getByLabel('堆叠更多操作', { exact: true }).click()
      await expect(page.getByRole('button', { name: '解除堆叠', exact: true })).toHaveClass('danger')
      await page.getByRole('button', { name: '解除堆叠', exact: true }).click()
      await expect(page.locator('.stack-header')).toHaveCount(0)
      await expect(page.getByRole('button', { name: `打开 ${first}`, exact: true })).toBeVisible()
      await expect(page.getByRole('button', { name: `打开 ${second}`, exact: true })).toBeVisible()
      await expect(page.getByRole('button', { name: `打开 ${standalone}`, exact: true })).toBeVisible()
      const after = await (await page.request.get(`/api/library/items?kind=book&q=${prefix}`)).json()
      expect(after.total).toBe(3)
      expect(after.items.filter((item: any) => item.favorite)).toHaveLength(2)
      if (touch) expect(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth)).toBeTruthy()
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
        if (name !== '文件') await navigate(page, name)
        const cards = page.locator(name === '首页' ? '.home-card' : name === '文件' ? '.file-card' : '.library-card')
        const card = name === '首页' ? cards.first() : cards.filter({ hasText: prefix }).first()
        await expect(card).toBeVisible()
        await press(page, card, touch)
        await expect(page.locator('.selection-summary b')).toHaveText('已选择 1 项')
        await expect(page.locator('#reader-view,.media-preview,.document-editor')).toHaveCount(0)
        await page.locator('.selection-toolbar').getByRole('button', { name: '取消', exact: true }).click()
      }
      await navigate(page, '文件')
      const files = await (await page.request.get(`/api/library/items?q=${prefix}`)).json()
      const id = files.items.find((item: any) => item.file.name.endsWith('.txt')).file.id
      expect((await page.request.delete(`/api/files/${id}`, { headers: { origin: new URL(page.url()).origin } })).ok()).toBeTruthy()
      await navigate(page, '回收站')
      await press(page, page.locator('.file-card').filter({ hasText: `${prefix}.txt` }), touch)
      await expect(page.locator('.selection-summary b')).toHaveText('已选择 1 项')
      await page.locator('.selection-toolbar').getByRole('button', { name: '取消', exact: true }).click()
      expect(errors).toEqual([])
    })
  })
}

test('desktop book drags create a stack and append to its existing ordered members', async ({ page }) => {
  const errors: string[] = []
  page.on('pageerror', error => errors.push(error.message))
  const prefix = `drag-stack-${Date.now()}`
  await login(page)
  await page.locator('input[type=file]').first().setInputFiles([1, 2, 3].map(n => ({
    name: `${prefix}-${n}.txt`, mimeType: 'text/plain', buffer: Buffer.from('手动拖放堆叠测试\n'.repeat(20)),
  })))
  await expect.poll(async () => (await (await page.request.get(`/api/library/items?kind=book&q=${prefix}`)).json()).total).toBe(3)
  await navigate(page, '书籍')
  const book = (n: number) => page.locator('.library-card').filter({ has: page.getByRole('button', { name: `打开 ${prefix}-${n}.txt`, exact: true }) })
  await expect(book(2)).toBeVisible()
  const sourceId = await book(2).getAttribute('data-file-id')
  await page.evaluate(sourceId => {
    const original = window.fetch.bind(window)
    const state = (window as any).libraryRefreshProbe = {
      seen: false, completed: false, release: () => {}, restore: () => { window.fetch = original },
      source: document.querySelector(`[data-file-id="${sourceId}"]`),
    }
    window.fetch = async (input, init) => {
      const url = new URL(typeof input === 'string' ? input : input instanceof Request ? input.url : input.toString(), location.href)
      if (url.pathname === '/api/library/items' && url.searchParams.get('kind') === 'book' && !state.seen) {
        state.seen = true
        await new Promise<void>(resolve => { state.release = resolve })
        const response = await original(input, init)
        state.completed = true
        return response
      }
      return original(input, init)
    }
  }, sourceId)
  await page.getByRole('navigation', { name: '主导航', exact: true }).getByRole('link', { name: '书籍', exact: true }).click()
  await expect.poll(() => page.evaluate(() => (window as any).libraryRefreshProbe.seen)).toBe(true)
  await expect(book(2), 'background refresh keeps the drag source mounted').toBeVisible()
  await page.evaluate(() => (window as any).libraryRefreshProbe.release())
  await expect.poll(() => page.evaluate(() => (window as any).libraryRefreshProbe.completed)).toBe(true)
  await page.evaluate(() => new Promise(resolve => requestAnimationFrame(() => requestAnimationFrame(resolve))))
  expect(await page.evaluate(sourceId => (window as any).libraryRefreshProbe.source === document.querySelector(`[data-file-id="${sourceId}"]`), sourceId)).toBe(true)
  await page.evaluate(() => { (window as any).libraryRefreshProbe.restore(); delete (window as any).libraryRefreshProbe })
  await dragBook(page, book(2), book(1))
  let stackId: string
  await expect.poll(async () => {
    const created = (await (await page.request.get('/api/library/stacks')).json()).find((stack: any) => stack.files.some((file: any) => file.name === `${prefix}-1.txt`))
    stackId = created?.id
    return created?.files.length
  }).toBe(2)
  const stack = page.locator(`.stack-card[data-stack-id="${stackId!}"]`)
  await expect(stack.locator('.stack-count')).toHaveText('2 本')
  await dragBook(page, book(3), stack)
  await expect(stack.locator('.stack-count')).toHaveText('3 本')
  await expect(stack.locator('.stack-cover-layer')).toHaveCount(3)
  await stack.locator('.library-card-open').click()
  await expect(page).toHaveURL(new RegExp(`/library/stacks/${stackId}$`))
  await expect(page.locator('.library-card-open')).toHaveCount(3)
  for (let n=1;n<=3;n++) await expect(page.locator('.library-card-open').nth(n-1)).toHaveAttribute('aria-label', `打开 ${prefix}-${n}.txt`)
  await stackMenu(page, '管理书籍')
  await holdDrag(page, book(3), book(1), false)
  await expect(page.locator('.library-card-open').first()).toHaveAttribute('aria-label', `打开 ${prefix}-3.txt`)
  await page.reload()
  await expect(page.locator('.library-card-open').first()).toHaveAttribute('aria-label', `打开 ${prefix}-3.txt`)
  await stackMenu(page, '管理书籍')
  // A rejected save restores the server order and keeps the inline management controls usable.
  const fault = await failApplicationRequest(page, `/api/library/stacks/${stackId}/order`, '暂时无法保存顺序')
  await page.getByRole('button', { name: `打开 ${prefix}-3.txt`, exact: true }).press('Alt+ArrowRight')
  await expect(page.locator('.library-error')).toBeVisible()
  await expect(page.locator('.library-error')).toContainText('暂时无法保存顺序')
  await expect(page.locator('.library-card-open').first()).toHaveAttribute('aria-label', `打开 ${prefix}-3.txt`)
  await expect(page.getByRole('toolbar', { name: '管理堆叠书籍' })).toBeVisible()
  await fault.clear()
  await page.getByRole('button', { name: `打开 ${prefix}-3.txt`, exact: true }).press('Alt+ArrowRight')
  await expect(page.locator('.library-error')).toHaveCount(0)
  await expect(page.locator('.library-card-open').first()).toHaveAttribute('aria-label', `打开 ${prefix}-1.txt`)
  await expect(page.getByRole('status').filter({ hasText: '顺序已保存' })).toHaveCount(1)
  await stackMenu(page, '解除堆叠')
  await expect(page.locator(`[data-stack-id="${stackId}"]`)).toHaveCount(0)
  expect(errors).toEqual([])
})

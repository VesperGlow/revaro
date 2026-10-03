import { expect, test, type Page, type Locator } from '@playwright/test'
import { readFileSync } from 'node:fs'
import { login, enterSelectionMode } from './helpers'

const root = '00000000-0000-0000-0000-000000000000'
const png = Buffer.from('iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mNk+A8AAQUBAScY42YAAAAASUVORK5CYII=', 'base64')
function wav() {
  const data = Buffer.alloc(44 + 8000 * 60 * 2)
  data.write('RIFF'); data.writeUInt32LE(data.length - 8, 4); data.write('WAVEfmt ', 8)
  data.writeUInt32LE(16, 16); data.writeUInt16LE(1, 20); data.writeUInt16LE(1, 22)
  data.writeUInt32LE(8000, 24); data.writeUInt32LE(16000, 28); data.writeUInt16LE(2, 32)
  data.writeUInt16LE(16, 34); data.write('data', 36); data.writeUInt32LE(data.length - 44, 40)
  return data
}
async function upload(page: Page, prefix: string, extensions: string[]) {
  const buffers: Record<string, Buffer> = { txt: Buffer.from('第一章\n\n选择卡片不会打开阅读器。'), wav: wav(), png, webm: readFileSync(new URL('./fixtures/preview.webm', import.meta.url)) }
  const mime: Record<string, string> = { txt: 'text/plain', wav: 'audio/wav', png: 'image/png', webm: 'video/webm' }
  await page.locator('input[type=file]').first().setInputFiles(extensions.flatMap(ext => [1, 2].map(index => ({ name: `${prefix}-${index}.${ext}`, mimeType: mime[ext], buffer: buffers[ext] }))))
  await expect.poll(async () => (await (await page.request.get(`/api/library/items?q=${prefix}`)).json()).total).toBe(extensions.length * 2)
}

// Pointer clicks sample moving card bounds directly; keyboard assertions use the native control.
async function clickCard(page: Page, card: Locator) {
  await card.evaluate(el => el.scrollIntoView({ block: 'nearest' }))
  const box = (await card.boundingBox())!
  await page.mouse.click(box.x + box.width / 2, box.y + box.height / 2)
}

test('music cover playback stays functional and management uses only the batch toolbar', async ({ page }) => {
  await login(page)
  const prefix = `managed-song-${Date.now()}`
  await upload(page, prefix, ['wav'])
  await page.getByRole('navigation', { name: '主导航', exact: true }).getByRole('link', { name: '音乐', exact: true }).click()
  const row = page.locator('.library-card').filter({ has: page.getByRole('button', { name: `打开 ${prefix}-1.wav`, exact: true }) })
  await expect(page.locator('.library-card-actions')).toHaveCount(0)
  for (const width of [1600, 1280, 851, 390, 320]) {
    await page.setViewportSize({ width, height: 900 })
    await page.mouse.move(1, 80)
    await expect(row.locator('.card-info')).toHaveCSS('opacity', '1')
    await expect(row.locator('.card-info small')).toHaveCSS('opacity', '0')
    await row.hover()
    await expect(row.locator('.card-info small')).toHaveCSS('opacity', '1')
    expect(await page.evaluate(() => document.documentElement.scrollWidth)).toBe(width)
  }
  await row.getByRole('button', { name: `打开 ${prefix}-1.wav`, exact: true }).click()
  await expect(page.getByRole('button', { name: '暂停音乐', exact: true })).toBeVisible()
  await enterSelectionMode(page)
  await row.getByRole('checkbox').press('Space')
  const toolbar = page.getByRole('toolbar', { name: '所选项目操作', exact: true })
  await toolbar.getByRole('button', { name: '收藏', exact: true }).click()
  await expect.poll(async () => (await (await page.request.get(`/api/library/items?q=${prefix}`)).json()).items.find((item: any) => item.file.name === `${prefix}-1.wav`).favorite).toBe(true)
  const response = await page.request.post('/api/library/collections', { headers: { Origin: new URL(page.url()).origin }, data: { kind: 'audio', name: prefix } })
  expect(response.ok()).toBeTruthy()
  await page.reload()
  await enterSelectionMode(page)
  await row.getByRole('checkbox').press('Space')
  const second = page.locator('.library-card').filter({ has: page.getByRole('button', { name: `打开 ${prefix}-2.wav`, exact: true }) })
  await second.getByRole('checkbox').press('Space')
  await toolbar.getByLabel('更多管理操作', { exact: true }).click()
  await page.locator('.selection-management-panel').getByRole('button', { name: '加入歌单', exact: true }).click()
  await expect(page.getByRole('dialog', { name: '管理集合' })).toContainText('2 项音乐')
  await page.getByRole('dialog', { name: '管理集合' }).getByRole('button', { name: new RegExp(prefix) }).click()
  await expect(page.getByRole('dialog', { name: '管理集合' })).toBeHidden()
  await expect.poll(async () => (await (await page.request.get(`/api/library/items?collection=${(await response.json()).id}`)).json()).total).toBe(2)
})

test('global selection mode shares subtle motion, selected surfaces, batch actions and dismissal across every listing', async ({ page }) => {
  test.setTimeout(180_000)
  page.setDefaultTimeout(15_000)
  const errors: string[] = []
  page.on('pageerror', error => errors.push(error.message))
  await login(page)
  const prefix = `selection-${Date.now()}`
  await upload(page, prefix, ['txt', 'wav', 'png', 'webm'])
  const toolbar = page.getByRole('toolbar', { name: '所选项目操作', exact: true })
  const enter = page.getByRole('button', { name: '进入选择模式', exact: true })
  const exit = page.getByRole('button', { name: '退出选择模式', exact: true })
  for (const width of [1280, 390]) {
    await page.setViewportSize({ width, height: 900 })
    const nav = page.getByRole('navigation', { name: width > 850 ? '主导航' : '移动端导航', exact: true })
    for (const name of ['首页', '书籍', '音乐', '图片', '视频', '文件']) {
      await nav.getByRole('link', { name, exact: true }).click()
      if (name === '文件') {
        await page.getByLabel('打开搜索', { exact: true }).click()
        await page.getByLabel('搜索文件名', { exact: true }).fill(prefix)
        await page.getByLabel('搜索文件名', { exact: true }).press('Enter')
      }
      const cards = page.locator(name === '首页' ? '.home-card' : name === '文件' ? '.file-card' : '.library-card')
      const targets = name === '首页' ? cards : cards.filter({ hasText: prefix })
      await expect.poll(() => targets.count()).toBeGreaterThanOrEqual(2)
      await targets.first().hover()
      await expect(page.locator('.selection-checkbox, .card-select')).toHaveCount(0)
      await expect(enter).toHaveAttribute('aria-pressed', 'false')
      await expect(targets.first()).toHaveCSS('animation-name', 'none')
      await page.mouse.move(1, 80)
      await expect(page.locator('.library-card-actions')).toHaveCount(0)
      if (name !== '音乐') {
        await expect(targets.first().locator('.card-info')).toHaveCSS('opacity', '0')
        await targets.first().hover()
        await expect(targets.first().locator('.card-info')).toHaveCSS('opacity', '1')
        const surface = (await targets.first().boundingBox())!, caption = (await targets.first().locator('.card-info').boundingBox())!
        expect(caption.x).toBeGreaterThanOrEqual(surface.x - 1)
        expect(caption.y).toBeGreaterThanOrEqual(surface.y - 1)
        expect(caption.x + caption.width).toBeLessThanOrEqual(surface.x + surface.width + 1)
        expect(caption.y + caption.height).toBeLessThanOrEqual(surface.y + surface.height + 1)
        if (name !== '文件') {
          const cover = (await targets.first().locator('.library-cover').boundingBox())!
          expect(Math.abs(surface.height - cover.height)).toBeLessThanOrEqual(1)
        }
      }
      await enterSelectionMode(page)
      await expect(toolbar).toBeHidden()
      await expect(cards.getByRole('checkbox')).toHaveCount(await cards.count())
      const firstCheckbox = targets.first().getByRole('checkbox')
      await expect(page.locator('.selection-corner, .selection-checkbox svg')).toHaveCount(0)
      await expect(targets.first().locator('.selection-checkbox')).toHaveCSS('clip-path', 'inset(50%)')
      await expect(targets.first()).toHaveCSS('animation-name', 'selection-sway')
      await expect(targets.first()).toHaveCSS('animation-duration', '2.8s')
      const frames = await targets.first().evaluate(el => (el.getAnimations().find(animation => (animation as CSSAnimation).animationName === 'selection-sway')!.effect as KeyframeEffect).getKeyframes().map(frame => frame.transform))
      expect(frames).toEqual(['rotate(-2deg)', 'rotate(2deg)', 'rotate(-2deg)'])
      const motion = await targets.first().evaluate(el => {
        const style = getComputedStyle(el)
        const siblings = [...el.parentElement!.children]
        return {
          origin: style.transformOrigin.split(' ').map(parseFloat),
          center: [parseFloat(style.width) / 2, parseFloat(style.height) / 2],
          delays: siblings.map(card => getComputedStyle(card).animationDelay),
        }
      })
      expect(motion.origin[0]).toBeCloseTo(motion.center[0], 2)
      expect(motion.origin[1]).toBeCloseTo(motion.center[1], 2)
      expect(new Set(motion.delays).size).toBeGreaterThan(1)
      const dimensions = await targets.first().evaluate(el => ({ width: el.clientWidth, height: el.clientHeight }))
      if (name === '音乐') {
        await expect(page.locator('.song-number')).toHaveCount(await cards.count())
      }
      await clickCard(page, targets.first())
      await expect(toolbar).toBeVisible()
      await expect(page.locator('.selection-toolbar')).toHaveCount(1)
      await expect(targets.first()).toHaveCSS('animation-name', 'none')
      await expect(targets.first()).toHaveCSS('transform', 'matrix(0.985, 0, 0, 0.985, 0, 0)')
      await expect(targets.first()).toHaveCSS('outline-style', 'none')
      const highlight = await targets.first().evaluate(el => {
        const style = getComputedStyle(el, '::after')
        return { inset: style.inset, border: style.borderTopWidth, radius: style.borderRadius }
      })
      expect(highlight.inset).toBe('0px')
      expect(highlight.border).toBe('1px')
      expect(highlight.radius).toBe(await targets.first().evaluate(el => getComputedStyle(el).borderRadius))
      await expect(targets.first()).not.toHaveCSS('box-shadow', 'none')
      expect(await targets.first().evaluate(el => ({ width: el.clientWidth, height: el.clientHeight }))).toEqual(dimensions)
      await expect(targets.last()).toHaveCSS('animation-name', 'selection-sway')
      await expect(toolbar.locator('.selection-summary b')).toHaveText('已选择 1 项')
      // Selecting the card itself with the keyboard must also avoid opening it.
      const lastOpen = name === '文件' ? targets.last() : targets.last().locator(name === '首页' ? '.home-item' : '.library-card-open')
      await lastOpen.press('Space')
      await expect(toolbar.locator('.selection-summary b')).toHaveText('已选择 2 项')
      await expect(page.locator('#reader-view, .video-player-shell, .preview-image, .music-dock')).toHaveCount(0)
      await expect(cards.locator('input:checked')).toHaveCount(2)
      if (name !== '文件') {
        for (const label of ['重命名', '复制到', '分享']) await expect(toolbar.getByRole('button', { name: label, exact: true })).toHaveCount(0)
      }
      for (const label of ['移动', '删除', '取消']) await expect(toolbar.getByRole('button', { name: label, exact: true })).toBeVisible()
      await expect(toolbar.getByRole('button', { name: /^下载/ })).toBeVisible()
      if (name === '图片' || name === '音乐') await page.screenshot({ path: `../../.demo-shots/selection-mode-${name === '图片' ? 'gallery' : 'music'}-${width}.png` })
      await firstCheckbox.press('Space')
      await expect(toolbar.locator('.selection-summary b')).toHaveText('已选择 1 项')
      await toolbar.getByRole('button', { name: '全选', exact: true }).click()
      await expect(toolbar.locator('.selection-summary b')).toHaveText(`已选择 ${await cards.count()} 项`)
      await toolbar.getByRole('button', { name: '取消全选', exact: true }).click()
      await expect(toolbar).toBeHidden()
      await expect(exit).toBeVisible()
      await firstCheckbox.press('Space')
      await toolbar.getByRole('button', { name: '取消', exact: true }).click()
      await expect(page.locator('.selection-checkbox')).toHaveCount(0)
      await enterSelectionMode(page)
      await firstCheckbox.press('Space')
      await firstCheckbox.press('Escape')
      await expect(page.locator('.selection-checkbox')).toHaveCount(0)
      await expect(toolbar).toBeHidden()
      await expect(enter).toBeVisible()
      expect(await page.evaluate(() => document.documentElement.scrollWidth)).toBe(width)
    }
  }
  await page.setViewportSize({ width: 1280, height: 900 })
  const nav = page.getByRole('navigation', { name: '主导航', exact: true })
  await nav.getByRole('link', { name: '图片', exact: true }).click()
  const images = page.locator('.library-card').filter({ hasText: prefix })
  await enterSelectionMode(page)
  await images.first().getByRole('checkbox').press('Space')
  await nav.getByRole('link', { name: '视频', exact: true }).click()
  await expect(toolbar).toBeHidden()
  await expect(exit).toBeVisible()
  await expect(page.locator('.selection-checkbox input:checked')).toHaveCount(0)
  await page.locator('.library-card').filter({ hasText: prefix }).first().getByRole('checkbox').press('Space')
  await page.goBack()
  await expect(nav.getByRole('link', { name: '图片', exact: true })).toHaveAttribute('aria-current', 'page')
  await expect(toolbar).toBeHidden()
  await expect(page.locator('.selection-checkbox input:checked')).toHaveCount(0)
  await clickCard(page, images.first())
  await expect(toolbar.locator('.selection-summary b')).toHaveText('已选择 1 项')
  const download = page.waitForEvent('download')
  await toolbar.getByRole('button', { name: '下载', exact: true }).click()
  expect(await (await download).failure()).toBeNull()
  await toolbar.getByRole('button', { name: '移动', exact: true }).click()
  await expect(page.getByRole('dialog', { name: '移动到', exact: true })).toBeVisible()
  await page.getByRole('dialog').getByRole('button', { name: '取消', exact: true }).click()
  await expect(toolbar).toBeVisible()
  await toolbar.getByRole('button', { name: '删除', exact: true }).click()
  await page.getByRole('dialog').getByRole('button', { name: '移入回收站', exact: true }).click()
  await expect(toolbar).toBeHidden()
  await nav.getByRole('link', { name: '回收站', exact: true }).click()
  const trash = page.locator('.file-card').filter({ hasText: prefix })
  await trash.first().getByRole('checkbox').press('Space')
  for (const label of ['下载', '移动', '重命名', '分享', '复制到']) await expect(toolbar.getByRole('button', { name: label, exact: true })).toHaveCount(0)
  await expect(toolbar.getByRole('button', { name: /^(全选|取消全选)$/ })).toBeVisible()
  for (const label of ['恢复', '永久删除', '取消']) await expect(toolbar.getByRole('button', { name: label, exact: true })).toBeVisible()
  await nav.getByRole('link', { name: '图片', exact: true }).click()
  await expect(toolbar).toBeHidden()
  await images.first().getByRole('checkbox').press('Space')
  await page.getByRole('button', { name: '我的收藏', exact: true }).click()
  await expect(toolbar).toBeHidden()
  await page.getByRole('button', { name: '我的收藏', exact: true }).click()
  await images.first().getByRole('checkbox').press('Space')
  await page.locator('.library-main').click({ position: { x: 1, y: 1 } })
  await expect(toolbar).toBeHidden()
  await expect(page.locator('.selection-checkbox')).toHaveCount(0)
  await expect(enter).toBeVisible()
  await enterSelectionMode(page)
  await expect(page.locator('.selection-checkbox input:checked')).toHaveCount(0)
  await images.first().getByRole('checkbox').press('Space')
  await exit.click()
  await expect(toolbar).toBeHidden()
  await expect(page.locator('.selection-checkbox')).toHaveCount(0)
  expect(errors).toEqual([])
})


test('batch bars match file margins and background dismissal never intercepts cards or controls', async ({ page }) => {
  test.setTimeout(180_000)
  await login(page)
  const prefix = `batch-layout-${Date.now()}`
  await upload(page, prefix, ['txt', 'wav', 'png', 'webm'])
  const toolbar = page.getByRole('toolbar', { name: '所选项目操作', exact: true })
  const enter = page.getByRole('button', { name: '进入选择模式', exact: true })
  const exit = page.getByRole('button', { name: '退出选择模式', exact: true })
  for (const width of [1280, 1600, 1920, 390, 320]) {
    await page.setViewportSize({ width, height: 1000 })
    const nav = page.getByRole('navigation', { name: width > 850 ? '主导航' : '移动端导航', exact: true })
    let reference: { x: number; width: number; styles: unknown } | undefined
    for (const name of ['文件', '图片', '视频', '书籍', '音乐', '首页']) {
      await nav.getByRole('link', { name, exact: true }).click()
      if (name === '文件') {
        await page.getByLabel('打开搜索', { exact: true }).click()
        await page.getByLabel('搜索文件名', { exact: true }).fill(prefix)
        await page.getByLabel('搜索文件名', { exact: true }).press('Enter')
      }
      const cards = page.locator(name === '首页' ? '.home-card' : name === '文件' ? '.file-card' : '.library-card')
      await expect.poll(() => cards.count()).toBeGreaterThanOrEqual(2)
      const content = page.locator(name === '文件' ? '.app-shell>.content' : '.library-main')
      await enterSelectionMode(page)
      // A background click also dismisses a mode with no selections.
      await page.mouse.click(1, 200)
      await expect(enter).toBeVisible()
      await expect(page.locator('.selection-checkbox')).toHaveCount(0)
      await enterSelectionMode(page)
      await cards.first().getByRole('checkbox').press('Space')
      // Click card padding (or the music row number), without relying on any corner control.
      const second = (await cards.nth(1).boundingBox())!
      await page.mouse.click(second.x + 5, second.y + second.height / 2)
      await expect(toolbar.locator('.selection-summary b')).toHaveText('已选择 2 项')
      await expect(exit).toBeVisible()
      const box = (await toolbar.boundingBox())!
      const styles = await toolbar.evaluate(el => {
        const bar = getComputedStyle(el), actions = getComputedStyle(el.querySelector('.selection-actions')!), summary = getComputedStyle(el.querySelector('.selection-summary')!)
        return { padding: bar.padding, gap: bar.gap, alignment: bar.alignItems, actionGap: actions.gap, actionAlignment: actions.alignItems, separator: summary.borderRight, summaryPadding: summary.paddingRight }
      })
      if (!reference) reference = { x: box.x, width: box.width, styles }
      expect(Math.abs(box.x - reference.x)).toBeLessThanOrEqual(0.1)
      expect(Math.abs(box.width - reference.width)).toBeLessThanOrEqual(0.1)
      expect(styles).toEqual(reference.styles)
      const centers = await toolbar.locator('.selection-actions>button').evaluateAll(buttons => buttons.map(button => { const rect = button.getBoundingClientRect(); return rect.y + rect.height / 2 }))
      expect(Math.max(...centers) - Math.min(...centers)).toBeLessThanOrEqual(0.1)
      await toolbar.locator('.selection-summary').click()
      await expect(toolbar.locator('.selection-summary b')).toHaveText('已选择 2 项')
      if (name !== '首页') {
        const toggle = page.getByLabel(name === '文件' ? '选择排序字段' : '选择集合', { exact: true })
        await toggle.click()
        await expect(exit).toBeVisible()
        await expect(toolbar.locator('.selection-summary b')).toHaveText('已选择 2 项')
        await toggle.click()
        await expect(exit).toBeVisible()
      }
      const open = name === '文件' ? cards.first() : cards.first().locator(name === '首页' ? '.home-item' : '.library-card-open')
      await open.click()
      await expect(toolbar.locator('.selection-summary b')).toHaveText('已选择 1 项')
      await expect(exit).toBeVisible()
      await content.click({ position: { x: 1, y: 1 } })
      await expect(enter).toBeVisible()
      await expect(toolbar).toBeHidden()
      await expect(page.locator('.selection-checkbox')).toHaveCount(0)
      await enter.click()
      await expect(page.locator('.selection-checkbox input:checked')).toHaveCount(0)
      if (name === '音乐') {
        await page.emulateMedia({ reducedMotion: 'reduce' })
        await expect(cards.first()).toHaveCSS('animation-name', 'none')
        await expect(cards.first()).toHaveCSS('transform', 'none')
        await cards.first().getByRole('checkbox').press('Space')
        await expect(cards.first()).toHaveCSS('transform', 'matrix(0.985, 0, 0, 0.985, 0, 0)')
        await cards.first().getByRole('checkbox').press('Space')
        await page.emulateMedia({ reducedMotion: 'no-preference' })
        await expect(cards.first()).toHaveCSS('animation-name', 'selection-sway')
        // Short lists leave blank space in the outer shell below the main element.
        await page.setViewportSize({ width, height: 2000 })
        await cards.first().getByRole('checkbox').press('Space')
        await page.evaluate(() => window.scrollTo(0, 0))
        const blank = { x: width / 2, y: 1900 }
        expect(await page.evaluate(({ x, y }) => document.elementFromPoint(x, y)?.classList.contains('content-library'), blank)).toBe(true)
        await page.mouse.click(blank.x, blank.y)
        await expect(enter).toBeVisible()
        await expect(toolbar).toBeHidden()
        await page.setViewportSize({ width, height: 1000 })
        await enterSelectionMode(page)
        await expect(page.locator('.selection-checkbox input:checked')).toHaveCount(0)
      }
      await exit.click()
      expect(await page.evaluate(() => document.documentElement.scrollWidth)).toBe(width)
    }
  }
})

test('mixed file selections share typed batch management, retry failures and keep menus visible on mobile', async ({ page }) => {
  test.setTimeout(180_000)
  await login(page)
  const prefix = `mixed-management-${Date.now()}`
  await upload(page, prefix, ['txt', 'wav', 'png', 'webm'])
  const origin = new URL(page.url()).origin
  const folder = await page.request.post('/api/directories', { headers: { Origin: origin }, data: { name: `${prefix}-folder`, parent_id: root } })
  expect(folder.ok()).toBeTruthy()
  const collections: Record<string, string> = {}
  for (const kind of ['book', 'audio', 'image', 'video']) {
    const response = await page.request.post('/api/library/collections', { headers: { Origin: origin }, data: { kind, name: `${prefix}-${kind}` } })
    expect(response.ok()).toBeTruthy()
    collections[kind] = (await response.json()).id
  }
  await page.getByRole('navigation', { name: '主导航', exact: true }).getByRole('link', { name: '文件', exact: true }).click()
  await page.getByLabel('打开搜索', { exact: true }).click()
  await page.getByLabel('搜索文件名', { exact: true }).fill(prefix)
  await page.getByLabel('搜索文件名', { exact: true }).press('Enter')
  const cards = page.locator('.file-card')
  await expect(cards).toHaveCount(9)
  await enterSelectionMode(page)
  for (const checkbox of await cards.getByRole('checkbox').all()) await checkbox.press('Space')
  const toolbar = page.getByRole('toolbar', { name: '所选项目操作', exact: true })
  await expect(toolbar.locator('.selection-summary b')).toHaveText('已选择 9 项')
  await toolbar.getByRole('button', { name: '收藏', exact: true }).click()
  const listing = async () => (await (await page.request.get(`/api/library/items?q=${prefix}`)).json()).items
  await expect.poll(async () => (await listing()).filter((item: any) => item.favorite).length).toBe(8)
  await page.setViewportSize({ width: 320, height: 1000 })
  const more = toolbar.getByLabel('更多管理操作', { exact: true })
  const menu = page.locator('.selection-management-panel')
  for (const [kind, label] of [['book', '书架'], ['audio', '歌单'], ['image', '相册']]) {
    await more.click()
    const bounds = (await menu.boundingBox())!
    expect(bounds.x).toBeGreaterThanOrEqual(0)
    expect(bounds.x + bounds.width).toBeLessThanOrEqual(320)
    for (const name of ['加入书架', '加入歌单', '加入相册', '加入视频集']) await expect(menu.getByRole('button', { name, exact: true })).toBeVisible()
    await expect(menu.getByRole('button', { name: '移出当前集合', exact: true })).toHaveCount(0)
    await menu.getByRole('button', { name: `加入${label}`, exact: true }).click()
    const dialog = page.getByRole('dialog', { name: '管理集合', exact: true })
    await expect(dialog).toContainText('2 项')
    await expect(dialog.getByRole('button', { name: new RegExp(`${prefix}-${kind}`) })).toBeVisible()
    await dialog.getByRole('button', { name: new RegExp(`${prefix}-${kind}`) }).click()
    await expect(dialog).toBeHidden()
    await expect.poll(async () => (await (await page.request.get(`/api/library/items?collection=${collections[kind]}`)).json()).total).toBe(2)
    await expect(toolbar.locator('.selection-summary b')).toHaveText('已选择 9 项')
  }
  await more.click()
  await menu.getByRole('button', { name: '加入视频集', exact: true }).click()
  const dialog = page.getByRole('dialog', { name: '管理集合', exact: true })
  await expect(dialog).toContainText('2 项视频')
  await dialog.getByRole('button', { name: '＋ 创建新集合', exact: true }).click()
  await expect(dialog.getByRole('heading')).toHaveText('新建视频集')
  const createdName = `${prefix}-created-videos`
  await dialog.getByLabel('集合名称').fill(createdName)
  await dialog.getByRole('button', { name: '创建', exact: true }).click()
  await expect(dialog).toBeHidden()
  const created = (await (await page.request.get('/api/library/collections')).json()).find((collection: any) => collection.name === createdName)
  expect(created.kind).toBe('video')
  await expect.poll(async () => (await (await page.request.get(`/api/library/items?collection=${created.id}`)).json()).total).toBe(2)
  await expect(toolbar.locator('.selection-summary b')).toHaveText('已选择 9 项')
  const failed = (await listing()).find((item: any) => item.file.name === `${prefix}-1.png`).file.id
  await page.route(`**/api/library/items/${failed}`, route => route.fulfill({ status: 503, contentType: 'application/json', body: JSON.stringify({ error: 'try again' }) }))
  await more.click()
  await menu.getByRole('button', { name: '取消收藏', exact: true }).click()
  await expect(page.locator('.toast')).toContainText('已完成 7/8 项')
  await expect.poll(async () => (await listing()).filter((item: any) => item.favorite).length).toBe(1)
  await expect(toolbar.locator('.selection-summary b')).toHaveText('已选择 9 项')
  await page.unroute(`**/api/library/items/${failed}`)
  await more.click()
  await menu.getByRole('button', { name: '取消收藏', exact: true }).click()
  await expect.poll(async () => (await listing()).filter((item: any) => item.favorite).length).toBe(0)
  await expect(toolbar.locator('.selection-summary b')).toHaveText('已选择 9 项')
  await page.getByRole('button', { name: '退出选择模式', exact: true }).click()
  await enterSelectionMode(page)
  await cards.filter({ hasText: `${prefix}-folder` }).getByRole('checkbox').press('Space')
  await expect(toolbar.locator('.selection-management')).toHaveCount(0)
  expect(await page.evaluate(() => document.documentElement.scrollWidth)).toBe(320)
})

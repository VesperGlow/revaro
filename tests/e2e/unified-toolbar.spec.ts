import { expect, test } from '@playwright/test'
import { login, openTopbarMenu } from './helpers'

const png = Buffer.from('iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mNk+A8AAQUBAScY42YAAAAASUVORK5CYII=', 'base64')
const wav = Buffer.alloc(16044)
wav.write('RIFF'); wav.writeUInt32LE(16036, 4); wav.write('WAVEfmt ', 8)
wav.writeUInt32LE(16, 16); wav.writeUInt16LE(1, 20); wav.writeUInt16LE(1, 22)
wav.writeUInt32LE(8000, 24); wav.writeUInt32LE(16000, 28); wav.writeUInt16LE(2, 32)
wav.writeUInt16LE(16, 34); wav.write('data', 36); wav.writeUInt32LE(16000, 40)

test('one header search keeps existing module queries and file scope separate', async ({ page }) => {
  const errors: string[] = []
  page.on('pageerror', error => errors.push(error.message))
  await login(page)
  const prefix = `unified-search-${Date.now()}`
  await page.locator('input[type=file]').first().setInputFiles([
    { name: `${prefix}.txt`, mimeType: 'text/plain', buffer: Buffer.from('统一搜索测试') },
    { name: `${prefix}.wav`, mimeType: 'audio/wav', buffer: wav },
    { name: `${prefix}.png`, mimeType: 'image/png', buffer: png },
  ])
  await expect.poll(async () => (await (await page.request.get(`/api/library/items?q=${prefix}`)).json()).total).toBe(3)
  const nav = page.getByRole('navigation', { name: '主导航', exact: true })
  for (const [name, label, placeholder, kind, extension] of [
    ['书籍', '搜索书籍', '搜索书籍…', 'book', 'txt'],
    ['音乐', '搜索歌曲', '搜索歌曲…', 'audio', 'wav'],
    ['图片', '搜索图片', '搜索图片…', 'image', 'png'],
  ]) {
    await nav.getByRole('link', { name, exact: true }).click()
    await expect(page.locator('main input[type=search], .library-toolbar input, .library-toolbar select')).toHaveCount(0)
    const input = page.getByLabel(label, { exact: true })
    await expect(input).toHaveAttribute('placeholder', placeholder)
    await expect(input).toHaveValue('')
    await page.getByLabel('打开搜索', { exact: true }).click()
    await expect(input).toBeFocused()
    await expect(page.locator('.search-scope')).toHaveCount(0)
    await input.fill(prefix)
    const requested = page.waitForResponse(response => {
      const url = new URL(response.url())
      return url.pathname === '/api/library/items' && url.searchParams.get('kind') === kind && url.searchParams.get('q') === prefix
    })
    await input.press('Enter')
    expect((await requested).ok()).toBe(true)
    await expect(page.locator('.library-card')).toHaveCount(1)
    await expect(page.getByRole('button', { name: `打开 ${prefix}.${extension}`, exact: true })).toBeVisible()
    const cleared = page.waitForResponse(response => {
      const url = new URL(response.url())
      return url.pathname === '/api/library/items' && url.searchParams.get('kind') === kind && !url.searchParams.get('q')
    })
    await input.fill('')
    expect((await cleared).ok()).toBe(true)
    await input.press('Escape')
    await expect(page.getByLabel('打开搜索', { exact: true })).toBeFocused()
  }
  await nav.getByRole('link', { name: '文件', exact: true }).click()
  await page.getByLabel('打开搜索', { exact: true }).click()
  await expect(page.getByLabel('搜索文件名')).toHaveAttribute('placeholder', '搜索文件…')
  await expect(page.locator('.search-scope, #search-scope-options')).toHaveCount(0)
  await page.getByLabel('搜索文件名').fill(prefix)
  await page.getByLabel('搜索文件名').press('Enter')
  await expect(page.locator('.file-card')).toHaveCount(3)
  expect(errors).toEqual([])
})

test('SVG navigation, inline search, favorites and anchored menus fit every breakpoint', async ({ page }) => {
  test.setTimeout(90_000)
  await login(page)
  for (const kind of ['book', 'audio', 'image']) {
    const response = await page.request.post('/api/library/collections', {
      headers: { Origin: new URL(page.url()).origin },
      data: { kind, name: `这是一个用于检查菜单边缘与长名称换行的集合-${kind}-${Date.now()}` },
    })
    expect(response.ok()).toBe(true)
  }
  await page.reload()
  await expect(page.getByRole('navigation', { name: '当前路径', exact: true }).getByRole('button', { name: '我的文件', exact: true })).toBeVisible()
  for (const width of [1280, 1100, 851, 850, 390, 320]) {
    await page.setViewportSize({ width, height: 900 })
    const nav = page.getByRole('navigation', { name: width <= 850 ? '移动端导航' : '主导航', exact: true })
    for (const name of ['首页', '书籍', '音乐', '图片', '文件']) {
      const link = nav.getByRole('link', { name, exact: true })
      await expect(link).toHaveAttribute('title', name)
      await expect(link).toHaveText('')
      await expect(link.locator('svg')).toHaveCount(1)
      await link.click()
      await expect(link).toHaveAttribute('aria-current', 'page')
      if (width > 850) {
        const header = (await page.locator('header.topbar').boundingBox())!
        const navigation = (await nav.boundingBox())!
        expect(Math.abs(navigation.x + navigation.width / 2 - header.x - header.width / 2)).toBeLessThanOrEqual(0.5)
      }
      const underline = await link.evaluate(element => {
        const icon = element.querySelector('svg')!.getBoundingClientRect()
        const link = element.getBoundingClientRect()
        const style = getComputedStyle(element, '::after')
        return { icon: icon.x + icon.width / 2, underline: link.x + parseFloat(style.left) }
      })
      expect(Math.abs(underline.icon - underline.underline)).toBeLessThanOrEqual(0.5)
      if (name === '首页') {
        await expect(page.locator('.home-shortcuts')).toHaveCount(0)
        for (const text of ['我的书库', '我的音乐', '我的图库']) await expect(page.getByText(text, { exact: true })).toHaveCount(0)
      } else {
        const triggerBefore = await page.getByLabel('打开搜索', { exact: true }).boundingBox()
        await page.getByLabel('打开搜索', { exact: true }).click()
        await expect(page.locator('.search-fields input')).toBeFocused()
        const surface = (await page.locator('.search-surface').boundingBox())!
        const header = (await page.locator('header.topbar').boundingBox())!
        expect(surface.x).toBeLessThan(triggerBefore!.x)
        expect(Math.abs(surface.x + surface.width - triggerBefore!.x - triggerBefore!.width)).toBeLessThanOrEqual(1)
        expect(surface.y).toBeGreaterThanOrEqual(header.y)
        expect(surface.y + surface.height).toBeLessThanOrEqual(header.y + header.height)
        expect(surface.x).toBeGreaterThanOrEqual(0)
        expect(surface.x + surface.width).toBeLessThanOrEqual(width)
        await page.keyboard.press('Escape')
        if (name !== '文件') {
          const favorites = page.getByRole('button', { name: '我的收藏', exact: true })
          await expect(favorites).toHaveText('')
          await expect(favorites.locator('svg')).toHaveCount(1)
          await favorites.click()
          await expect(favorites).toHaveAttribute('aria-pressed', 'true')
          await favorites.click()
          await expect(favorites).toHaveAttribute('aria-pressed', 'false')
          const collection = page.getByLabel('选择集合', { exact: true })
          await collection.click()
          await expect(page.locator('.collection-menu .action-menu-panel').getByRole('button', { name: /这是一个用于检查菜单边缘/ }).first()).toBeVisible()
          const menu = (await page.locator('.collection-menu .action-menu-panel').boundingBox())!
          const trigger = (await collection.boundingBox())!
          expect(Math.abs(menu.x - Math.max(8, Math.min(trigger.x + (trigger.width - menu.width) / 2, width - 8 - menu.width)))).toBeLessThanOrEqual(1)
          expect(menu.y).toBe(trigger.y + trigger.height + 8)
          expect(menu.x).toBeGreaterThanOrEqual(0)
          expect(menu.x + menu.width).toBeLessThanOrEqual(width)
          await page.keyboard.press('Escape')
          await expect(collection).toBeFocused()
        }
      }
      expect(await page.evaluate(() => document.documentElement.scrollWidth)).toBe(width)
    }

    const buttons = page.locator('.topbar .search-toggle, .topbar .top-actions>.action-menu>summary, .topbar .top-actions>.account-button')
    const geometry = await buttons.evaluateAll(elements => elements.map(element => {
      const rect = element.getBoundingClientRect()
      return { width: rect.width, height: rect.height, center: rect.y + rect.height / 2 }
    }))
    for (const button of geometry) {
      expect(button.width).toBe(width <= 1220 ? 34 : 44)
      expect(button.height).toBe(width <= 850 ? 40 : 44)
      expect(button.center).toBe(geometry[0].center)
    }
    await openTopbarMenu(page)
    for (const label of ['新建', '上传']) {
      const trigger = page.locator('.topbar').getByLabel(label, { exact: true })
      await trigger.click()
      const menu = (await page.locator('.topbar .embedded-menu[open] > .action-menu-panel').boundingBox())!
      const bounds = (await trigger.boundingBox())!
      expect(menu.y).toBeGreaterThanOrEqual(bounds.y + bounds.height)
      expect(menu.x).toBeGreaterThanOrEqual(0)
      expect(menu.x + menu.width).toBeLessThanOrEqual(width)
      await page.keyboard.press('Escape')
      await expect(trigger).toBeFocused()
    }
    await page.keyboard.press('Escape')
    await expect(page.getByLabel('更多操作', { exact: true })).toBeFocused()
  }
})

test('header search grows with typed text, shrinks on clear and keeps tools fixed', async ({ page }) => {
  const errors: string[] = []
  page.on('pageerror', error => errors.push(error.message))
  await login(page)
  for (const width of [1600, 1280, 1100, 850, 390, 320]) {
    await page.setViewportSize({ width, height: 900 })
    await page.getByLabel('打开搜索', { exact: true }).click()
    const input = page.getByLabel('搜索文件名')
    await expect(input).toBeFocused()
    await input.fill('')
    const surface = page.locator('.search-surface')
    const tools = page.locator('.topbar .top-actions>.action-menu>summary, .topbar .top-actions>.account-button')
    const available = await page.locator('.topbar').evaluate(header => {
      const headerStyle = getComputedStyle(header), actions = header.querySelector('.top-actions')!, nav = header.querySelector('.app-navigation')!
      const right = header.querySelector('.top-actions > .action-menu > summary')!.getBoundingClientRect().left - parseFloat(getComputedStyle(actions).columnGap)
      const left = innerWidth > 1550 ? nav.getBoundingClientRect().right + parseFloat(headerStyle.columnGap) : header.getBoundingClientRect().left + parseFloat(headerStyle.paddingLeft)
      return right - left
    })
    const initial = width <= 850 ? Math.min(208, available) : 208
    await expect.poll(async () => (await surface.boundingBox())!.width).toBe(initial)
    const positions = await tools.evaluateAll(elements => elements.map(element => element.getBoundingClientRect().toJSON()))
    const right = (await surface.boundingBox())!.x + initial
    await input.fill('适度长度的文件名称')
    const maximum = width <= 850 || width > 1550 ? Math.min(420, available) : 420
    await expect.poll(async () => (await surface.boundingBox())!.width).toBeGreaterThanOrEqual(initial)
    await input.fill('超长的文件名称与关键词'.repeat(12))
    await expect.poll(async () => (await surface.boundingBox())!.width).toBeGreaterThanOrEqual(maximum - 1)
    const expanded = (await surface.boundingBox())!
    expect(expanded.width).toBeLessThanOrEqual(maximum + 2)
    expect(Math.abs(expanded.x + expanded.width - right)).toBeLessThanOrEqual(1)
    expect(expanded.x).toBeGreaterThanOrEqual(8)
    expect(await tools.evaluateAll(elements => elements.map(element => element.getBoundingClientRect().toJSON()))).toEqual(positions)
    expect(await page.evaluate(() => document.documentElement.scrollWidth)).toBe(width)
    await input.fill('')
    await expect.poll(async () => (await surface.boundingBox())!.width).toBe(initial)
    await input.press('Escape')
  }
  expect(errors).toEqual([])
})

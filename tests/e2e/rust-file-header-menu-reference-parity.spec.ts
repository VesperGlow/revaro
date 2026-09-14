import { expect, test, type Page } from '@playwright/test'

const ROOT = '00000000-0000-0000-0000-000000000000'
const STAMP = '2026-01-01T00:00:00Z'

async function mockHeader(page: Page) {
  await page.route('**/api/**', async route => {
    const path = new URL(route.request().url()).pathname
    const json = (value: unknown) => route.fulfill({ json: value })

    if (path === '/api/auth/me') return json({ username: 'admin', has_avatar: false })
    if (path === '/api/events' || path === '/api/system/status/stream') {
      return route.fulfill({ contentType: 'text/event-stream', body: '' })
    }
    if (path === '/api/tasks') return json({ items: [] })
    if (path === '/api/library/all') {
      return json({ items: { book: [], image: [], video: [], audio: [] }, counts: { book: 0, image: 0, video: 0, audio: 0, file: 0 } })
    }
    if (path === '/api/library/counts') return json({ book: 0, image: 0, video: 0, audio: 0, file: 0 })
    if (path === `/api/files/${ROOT}`) {
      return json({
        file: { id: ROOT, parent_id: null, name: '我的文件', kind: 'directory', size: 0, status: 'ready', created_at: STAMP, updated_at: STAMP, mime_type: '' },
        breadcrumbs: [],
      })
    }
    if (path === `/api/files/${ROOT}/children`) return json({ items: [], total_bytes: 0, file_count: 0 })
    return json({ items: [] })
  })
}

async function openShell(page: Page, baseUrl: string) {
  await page.goto(`${baseUrl}/`)
  await expect(page.getByRole('heading', { name: '我的文件', exact: true })).toBeVisible()
}

async function menuMetrics(page: Page, selector: string) {
  return page.locator(selector).evaluate(element => {
    const summary = element.querySelector('summary')!
    const popover = element.querySelector(':scope > div')!
    const firstItem = popover.querySelector('button')!
    const styleOf = (target: Element) => {
      const style = getComputedStyle(target)
      const rect = target.getBoundingClientRect()
      return {
        display: style.display,
        position: style.position,
        background: style.backgroundColor,
        color: style.color,
        border: style.border,
        borderRadius: style.borderRadius,
        boxShadow: style.boxShadow,
        padding: style.padding,
        gap: style.gap,
        width: Math.round(rect.width * 100) / 100,
        height: Math.round(rect.height * 100) / 100,
      }
    }
    return {
      open: element.hasAttribute('open'),
      summaryText: summary.textContent?.replace(/\s+/g, ' ').trim(),
      summary: styleOf(summary),
      popover: styleOf(popover),
      firstItem: styleOf(firstItem),
      firstIcon: styleOf(firstItem.querySelector('span')!),
      firstText: firstItem.textContent?.replace(/\s+/g, ' ').trim(),
    }
  })
}

async function hoverItemMetrics(page: Page, selector: string) {
  const item = page.locator(`${selector} > div button`).first()
  await item.hover()
  await page.waitForTimeout(220)
  return item.evaluate(element => {
    const style = getComputedStyle(element)
    const icon = element.querySelector('span')!
    const iconStyle = getComputedStyle(icon)
    return {
      background: style.backgroundColor,
      color: style.color,
      iconBackground: iconStyle.backgroundColor,
      iconColor: iconStyle.color,
    }
  })
}

test('文件头新建/上传下拉菜单的状态、样式和快捷动作保持 reference', async ({ browser }) => {
  const oldUrl = process.env.E2E_REFERENCE_URL || 'http://127.0.0.1:18080'
  const newUrl = process.env.E2E_NEW_URL || 'http://127.0.0.1:18083'
  const oldContext = await browser.newContext({ viewport: { width: 390, height: 844 } })
  const newContext = await browser.newContext({ viewport: { width: 390, height: 844 } })
  const oldPage = await oldContext.newPage()
  const newPage = await newContext.newPage()

  try {
    await Promise.all([mockHeader(oldPage), mockHeader(newPage)])
    await Promise.all([openShell(oldPage, oldUrl), openShell(newPage, newUrl)])
    for (const selector of ['.create-menu', '.upload-menu']) {
      await expect(oldPage.locator(selector)).toBeVisible()
      await expect(newPage.locator(selector)).toBeVisible()
      expect(await menuMetrics(newPage, selector), `Rust ${selector} 初始菜单状态与 reference 不一致`)
        .toEqual(await menuMetrics(oldPage, selector))

      await Promise.all([
        oldPage.locator(selector).locator('summary').click(),
        newPage.locator(selector).locator('summary').click(),
      ])
      await expect(oldPage.locator(selector)).toHaveAttribute('open', '')
      await expect(newPage.locator(selector)).toHaveAttribute('open', '')
      await Promise.all([oldPage.waitForTimeout(220), newPage.waitForTimeout(220)])
      expect(await menuMetrics(newPage, selector), `Rust ${selector} 展开菜单状态与 reference 不一致`)
        .toEqual(await menuMetrics(oldPage, selector))
      expect(await hoverItemMetrics(newPage, selector), `Rust ${selector} 菜单项 hover 与 reference 不一致`)
        .toEqual(await hoverItemMetrics(oldPage, selector))

      await Promise.all([
        oldPage.getByRole('heading', { name: '我的文件', exact: true }).click(),
        newPage.getByRole('heading', { name: '我的文件', exact: true }).click(),
      ])
      await expect(oldPage.locator(selector)).not.toHaveAttribute('open', '')
      await expect(newPage.locator(selector)).not.toHaveAttribute('open', '')
    }

    await Promise.all([
      oldPage.locator('.create-menu summary').click(),
      newPage.locator('.create-menu summary').click(),
    ])
    await Promise.all([
      oldPage.locator('.create-menu-popover').getByRole('button', { name: '新建文档' }).click(),
      newPage.locator('.create-menu-popover').getByRole('button', { name: '新建文档' }).click(),
    ])
    await expect(oldPage.locator('.document-editor')).toBeVisible()
    await expect(newPage.locator('.document-editor')).toBeVisible()
    await expect(oldPage.locator('.create-menu')).not.toHaveAttribute('open', '')
    await expect(newPage.locator('.create-menu')).not.toHaveAttribute('open', '')
  } finally {
    await oldContext.close()
    await newContext.close()
  }
})

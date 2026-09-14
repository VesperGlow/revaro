import { expect, test, type Page } from '@playwright/test'

const ROOT = '00000000-0000-0000-0000-000000000000'
const STAMP = '2026-01-01T00:00:00Z'

async function mockAccount(page: Page) {
  await page.route('**/api/**', async route => {
    const path = new URL(route.request().url()).pathname
    const json = (value: unknown) => route.fulfill({ json: value })
    if (path === '/api/auth/me') return json({ username: 'admin', has_avatar: false })
    if (path === '/api/events' || path === '/api/system/status/stream') {
      return route.fulfill({ contentType: 'text/event-stream', body: '' })
    }
    if (path === '/api/tasks') return json({ items: [] })
    if (path === '/api/auth/totp') return json({ enabled: false, recovery_codes: 0 })
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

async function openAccount(page: Page, baseUrl: string) {
  await page.goto(`${baseUrl}/?account-reference=${Date.now()}`)
  await expect(page.getByRole('heading', { name: '我的文件', exact: true })).toBeVisible()
  await page.locator('button[title="打开账户设置"]').click()
  await expect(page.locator('.account-modal')).toBeVisible()
  await expect(page.locator('.account-modal').getByRole('button', { name: '设置', exact: true })).toBeEnabled()
}

async function accountMetrics(page: Page) {
  return page.locator('.account-modal').evaluate(account => {
    const edit = account.querySelector('.edit-username')!
    const session = account.querySelector('.account-session-row')!
    const rect = session.getBoundingClientRect()
    const style = getComputedStyle(session)
    return {
      text: account.textContent?.replace(/\s+/g, ' ').trim(),
      edit: {
        text: edit.textContent?.replace(/\s+/g, ' ').trim(),
        children: Array.from(edit.children).map(child => child.tagName.toLowerCase()),
        iconPaths: Array.from(edit.querySelectorAll('path')).map(path => path.getAttribute('d')),
        iconStyle: edit.querySelector('svg') && (() => {
          const icon = getComputedStyle(edit.querySelector('svg')!)
          const iconRect = edit.querySelector('svg')!.getBoundingClientRect()
          return { width: icon.width, height: icon.height, strokeWidth: icon.strokeWidth, rectWidth: iconRect.width, rectHeight: iconRect.height }
        })(),
      },
      session: {
        childCount: session.children.length,
        display: style.display,
        rectWidth: Math.round(rect.width * 100) / 100,
        rectHeight: Math.round(rect.height * 100) / 100,
      },
    }
  })
}

test('账户设置的用户名编辑入口和会话区保持 reference', async ({ browser }) => {
  const oldUrl = process.env.E2E_REFERENCE_URL || 'http://127.0.0.1:18080'
  const newUrl = process.env.E2E_NEW_URL || 'http://127.0.0.1:18083'
  const oldContext = await browser.newContext({ viewport: { width: 1440, height: 900 } })
  const newContext = await browser.newContext({ viewport: { width: 1440, height: 900 } })
  const oldPage = await oldContext.newPage()
  const newPage = await newContext.newPage()

  try {
    await Promise.all([mockAccount(oldPage), mockAccount(newPage)])
    await Promise.all([openAccount(oldPage, oldUrl), openAccount(newPage, newUrl)])
    expect(await accountMetrics(newPage), 'Rust 账户设置结构与 reference 不一致').toEqual(await accountMetrics(oldPage))

    await Promise.all([
      oldPage.locator('.edit-username').hover(),
      newPage.locator('.edit-username').hover(),
    ])
    await Promise.all([oldPage.waitForTimeout(220), newPage.waitForTimeout(220)])
    const oldHover = await oldPage.locator('.edit-username').evaluate(element => {
      const style = getComputedStyle(element)
      const icon = element.querySelector('svg')
      return { background: style.backgroundColor, color: style.color, iconColor: icon && getComputedStyle(icon).color }
    })
    const newHover = await newPage.locator('.edit-username').evaluate(element => {
      const style = getComputedStyle(element)
      const icon = element.querySelector('svg')
      return { background: style.backgroundColor, color: style.color, iconColor: icon && getComputedStyle(icon).color }
    })
    expect(newHover, 'Rust 编辑用户名 hover 与 reference 不一致').toEqual(oldHover)

    await Promise.all([
      oldPage.locator('.edit-username').click(),
      newPage.locator('.edit-username').click(),
    ])
    await expect(oldPage.locator('input[aria-label="用户名"]')).toBeFocused()
    await expect(newPage.locator('input[aria-label="用户名"]')).toBeFocused()
    await Promise.all([oldPage.keyboard.press('Escape'), newPage.keyboard.press('Escape')])
    await expect(oldPage.locator('input[aria-label="用户名"]')).toHaveCount(0)
    await expect(newPage.locator('input[aria-label="用户名"]')).toHaveCount(0)
  } finally {
    await oldContext.close()
    await newContext.close()
  }
})

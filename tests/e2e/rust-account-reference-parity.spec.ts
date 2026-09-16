import { expect, test, type Page } from '@playwright/test'

const ROOT = '00000000-0000-0000-0000-000000000000'
const STAMP = '2026-01-01T00:00:00Z'

async function mockAccount(page: Page, emptyTotpStatus = false, nullableTotpStatus = false) {
  await page.route('**/api/**', async route => {
    const path = new URL(route.request().url()).pathname
    const json = (value: unknown) => route.fulfill({ json: value })
    if (path === '/api/auth/me') return json({ username: 'admin', has_avatar: false })
    if (path === '/api/events' || path === '/api/system/status/stream') {
      return route.fulfill({ contentType: 'text/event-stream', body: '' })
    }
    if (path === '/api/tasks') return json({ items: [] })
    if (path === '/api/auth/totp') {
      return json(emptyTotpStatus
        ? {}
        : nullableTotpStatus
          ? { enabled: null, recovery_codes: null }
          : { enabled: false, recovery_codes: 0 })
    }
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

async function installEscapeProbe(page: Page) {
  await page.evaluate(() => {
    ;(window as Window & { __accountEscapeDefaultPrevented?: boolean }).__accountEscapeDefaultPrevented = undefined
    window.addEventListener('keydown', event => {
      if (event.key === 'Escape') {
        ;(window as Window & { __accountEscapeDefaultPrevented?: boolean }).__accountEscapeDefaultPrevented = event.defaultPrevented
      }
    }, { once: true })
  })
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

test('old/new TOTP 状态成功响应缺少字段时仍显示未启用设置入口', async ({ browser }) => {
  const oldUrl = process.env.E2E_REFERENCE_URL || 'http://127.0.0.1:18080'
  const newUrl = process.env.E2E_NEW_URL || 'http://127.0.0.1:18084'
  const oldContext = await browser.newContext({ viewport: { width: 1440, height: 900 } })
  const newContext = await browser.newContext({ viewport: { width: 1440, height: 900 } })
  const oldPage = await oldContext.newPage()
  const newPage = await newContext.newPage()

  async function exercise(page: Page, baseUrl: string) {
    await mockAccount(page, true)
    await page.goto(`${baseUrl}/?account-totp-empty=${Date.now()}`)
    await expect(page.getByRole('heading', { name: '我的文件', exact: true })).toBeVisible()
    await page.locator('button[title="打开账户设置"]').click()
    const account = page.locator('.account-modal')
    await expect(account).toBeVisible()
    await expect(account.getByRole('button', { name: '设置', exact: true })).toBeEnabled()
    const error = account.locator('.account-overview > .form-error')
    return {
      security: await account.locator('.security-row').textContent(),
      error: (await error.count()) > 0 ? await error.textContent() : null,
    }
  }

  try {
    const [oldResult, newResult] = await Promise.all([
      exercise(oldPage, oldUrl),
      exercise(newPage, newUrl),
    ])
    expect(oldResult).toEqual({
      security: '两步验证未启用使用 TOTP 验证码保护管理员登录。设置',
      error: null,
    })
    expect(newResult, 'Rust TOTP 状态缺少字段时与 reference 不一致').toEqual(oldResult)
  } finally {
    await Promise.all([oldContext.close(), newContext.close()])
  }
})

test('old/new TOTP 状态成功响应显式 null 字段时仍显示未启用设置入口', async ({ browser }) => {
  const oldUrl = process.env.E2E_REFERENCE_URL || 'http://127.0.0.1:18080'
  const newUrl = process.env.E2E_NEW_URL || 'http://127.0.0.1:18084'
  const oldContext = await browser.newContext({ viewport: { width: 1440, height: 900 } })
  const newContext = await browser.newContext({ viewport: { width: 1440, height: 900 } })
  const oldPage = await oldContext.newPage()
  const newPage = await newContext.newPage()

  async function exercise(page: Page, baseUrl: string) {
    await mockAccount(page, false, true)
    await page.goto(`${baseUrl}/?account-totp-null=${Date.now()}`)
    await expect(page.getByRole('heading', { name: '我的文件', exact: true })).toBeVisible()
    await page.locator('button[title="打开账户设置"]').click()
    const account = page.locator('.account-modal')
    await expect(account).toBeVisible()
    await expect(account.getByRole('button', { name: '设置', exact: true })).toBeEnabled()
    const error = account.locator('.account-overview > .form-error')
    return {
      security: await account.locator('.security-row').textContent(),
      error: (await error.count()) > 0 ? await error.textContent() : null,
    }
  }

  try {
    const [oldResult, newResult] = await Promise.all([
      exercise(oldPage, oldUrl),
      exercise(newPage, newUrl),
    ])
    expect(oldResult).toEqual({
      security: '两步验证未启用使用 TOTP 验证码保护管理员登录。设置',
      error: null,
    })
    expect(newResult, 'Rust TOTP 状态显式 null 时与 reference 不一致').toEqual(oldResult)
  } finally {
    await Promise.all([oldContext.close(), newContext.close()])
  }
})

test('账户设置的用户名编辑入口和会话区保持 reference', async ({ browser }) => {
  const oldUrl = process.env.E2E_REFERENCE_URL || 'http://127.0.0.1:18080'
  const newUrl = process.env.E2E_NEW_URL || 'http://127.0.0.1:18084'
  const oldContext = await browser.newContext({ viewport: { width: 1440, height: 900 } })
  const newContext = await browser.newContext({ viewport: { width: 1440, height: 900 } })
  const oldPage = await oldContext.newPage()
  const newPage = await newContext.newPage()

  try {
    await Promise.all([mockAccount(oldPage), mockAccount(newPage)])
    await Promise.all([openAccount(oldPage, oldUrl), openAccount(newPage, newUrl)])
    expect(await accountMetrics(newPage), 'Rust 账户设置结构与 reference 不一致').toEqual(await accountMetrics(oldPage))

    await Promise.all([installEscapeProbe(oldPage), installEscapeProbe(newPage)])
    await Promise.all([oldPage.keyboard.press('Escape'), newPage.keyboard.press('Escape')])
    await expect(oldPage.locator('.account-modal')).toBeVisible()
    await expect(newPage.locator('.account-modal')).toBeVisible()
    expect(await oldPage.evaluate(() => (window as Window & { __accountEscapeDefaultPrevented?: boolean }).__accountEscapeDefaultPrevented)).toBe(false)
    expect(await newPage.evaluate(() => (window as Window & { __accountEscapeDefaultPrevented?: boolean }).__accountEscapeDefaultPrevented)).toBe(false)

    await Promise.all([
      oldPage.locator('.password-entry').click(),
      newPage.locator('.password-entry').click(),
    ])
    const passwordFocus = await Promise.all([
      oldPage.evaluate(() => {
        const element = document.activeElement
        return { tag: element?.tagName ?? '', type: (element as HTMLInputElement | null)?.type ?? '', aria: element?.getAttribute('aria-label') ?? '' }
      }),
      newPage.evaluate(() => {
        const element = document.activeElement
        return { tag: element?.tagName ?? '', type: (element as HTMLInputElement | null)?.type ?? '', aria: element?.getAttribute('aria-label') ?? '' }
      }),
    ])
    expect(passwordFocus[1], 'Rust 密码子面板打开后的焦点与 reference 不一致').toEqual(passwordFocus[0])
    await Promise.all([
      oldPage.locator('.password-dialog input[type="password"]').first().focus(),
      newPage.locator('.password-dialog input[type="password"]').first().focus(),
    ])
    await Promise.all([installEscapeProbe(oldPage), installEscapeProbe(newPage)])
    await Promise.all([oldPage.keyboard.press('Escape'), newPage.keyboard.press('Escape')])
    await expect(oldPage.locator('.password-dialog')).toBeVisible()
    await expect(newPage.locator('.password-dialog')).toBeVisible()
    expect(await oldPage.evaluate(() => (window as Window & { __accountEscapeDefaultPrevented?: boolean }).__accountEscapeDefaultPrevented)).toBe(false)
    expect(await newPage.evaluate(() => (window as Window & { __accountEscapeDefaultPrevented?: boolean }).__accountEscapeDefaultPrevented)).toBe(false)
    await Promise.all([
      oldPage.locator('.password-dialog button[aria-label="关闭"]').click(),
      newPage.locator('.password-dialog button[aria-label="关闭"]').click(),
    ])
    await expect(oldPage.locator('.password-dialog')).toHaveCount(0)
    await expect(newPage.locator('.password-dialog')).toHaveCount(0)

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

test('账户头像选择器只按 reference 打开一次', async ({ browser }) => {
  const oldUrl = process.env.E2E_REFERENCE_URL || 'http://127.0.0.1:18080'
  const newUrl = process.env.E2E_NEW_URL || 'http://127.0.0.1:18084'
  const oldContext = await browser.newContext({ viewport: { width: 1440, height: 900 } })
  const newContext = await browser.newContext({ viewport: { width: 1440, height: 900 } })
  const oldPage = await oldContext.newPage()
  const newPage = await newContext.newPage()

  async function exercise(page: Page, baseUrl: string) {
    await mockAccount(page)
    await page.addInitScript(() => {
      const state = window as Window & { __avatarPickerClicks?: number }
      state.__avatarPickerClicks = 0
      const original = HTMLInputElement.prototype.click
      HTMLInputElement.prototype.click = function click(this: HTMLInputElement) {
        if (this.matches('.avatar-settings input[type="file"]')) {
          state.__avatarPickerClicks = (state.__avatarPickerClicks ?? 0) + 1
        }
        return original.call(this)
      }
    })
    await openAccount(page, baseUrl)
    await page.locator('.avatar-actions button').first().click()
    return page.evaluate(() => (window as Window & { __avatarPickerClicks?: number }).__avatarPickerClicks ?? 0)
  }

  try {
    const [oldClicks, newClicks] = await Promise.all([
      exercise(oldPage, oldUrl),
      exercise(newPage, newUrl),
    ])
    expect(oldClicks).toBe(1)
    expect(newClicks, 'Rust 头像文件选择器被重复打开').toBe(oldClicks)
  } finally {
    await Promise.all([oldContext.close(), newContext.close()])
  }
})

function deferred() {
  let resolve!: () => void
  const promise = new Promise<void>(value => { resolve = value })
  return { promise, resolve }
}

async function mockTotpLoading(page: Page) {
  const setupGate = deferred()
  await page.route('**/api/**', async route => {
    const request = route.request()
    const path = new URL(request.url()).pathname
    const json = (value: unknown) => route.fulfill({ json: value })
    if (path === '/api/auth/me') return json({ username: 'admin', has_avatar: false })
    if (path === '/api/events' || path === '/api/system/status/stream') {
      return route.fulfill({ contentType: 'text/event-stream', body: '' })
    }
    if (path === '/api/tasks') return json({ items: [] })
    if (path === '/api/auth/totp' && request.method() === 'GET') {
      return json({ enabled: false, recovery_codes: 0 })
    }
    if (path === '/api/auth/totp/setup' && request.method() === 'POST') {
      await setupGate.promise
      return json({ secret: 'JBSWY3DPEHPK3PXP', uri: 'otpauth://totp/Revaro:admin', qr_data_url: 'data:image/png;base64,AA==' })
    }
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
  return { setupGate }
}

async function startTotpSetup(page: Page, baseUrl: string) {
  await page.goto(`${baseUrl}/?account-totp-loading=${Date.now()}`)
  await expect(page.getByRole('heading', { name: '我的文件', exact: true })).toBeVisible()
  await page.locator('button[title="打开账户设置"]').click()
  const account = page.locator('.account-modal')
  await account.getByRole('button', { name: '设置', exact: true }).click()
  const totp = page.locator('.totp-dialog')
  await expect(totp).toBeVisible()
  await totp.getByLabel('当前密码', { exact: true }).fill('current-password')
  await totp.getByRole('button', { name: '开始设置' }).click()
  await expect(totp.locator('.two-factor-idle button')).toBeDisabled()
}

test('TOTP 设置请求进行中点击子弹窗空白仍关闭面板', async ({ browser }) => {
  const oldUrl = process.env.E2E_REFERENCE_URL || 'http://127.0.0.1:18080'
  const newUrl = process.env.E2E_NEW_URL || 'http://127.0.0.1:18084'
  const oldContext = await browser.newContext({ viewport: { width: 1440, height: 900 } })
  const newContext = await browser.newContext({ viewport: { width: 1440, height: 900 } })
  const oldPage = await oldContext.newPage()
  const newPage = await newContext.newPage()

  try {
    const [oldMock, newMock] = await Promise.all([mockTotpLoading(oldPage), mockTotpLoading(newPage)])
    await Promise.all([startTotpSetup(oldPage, oldUrl), startTotpSetup(newPage, newUrl)])
    await Promise.all([
      oldPage.locator('.account-subdialog-backdrop').click({ position: { x: 8, y: 8 } }),
      newPage.locator('.account-subdialog-backdrop').click({ position: { x: 8, y: 8 } }),
    ])
    await expect(oldPage.locator('.totp-dialog')).toHaveCount(0)
    await expect(newPage.locator('.totp-dialog')).toHaveCount(0)
    oldMock.setupGate.resolve()
    newMock.setupGate.resolve()
  } finally {
    await oldContext.close()
    await newContext.close()
  }
})

async function mockPasswordLoading(page: Page) {
  const passwordGate = deferred()
  await page.route('**/api/**', async route => {
    const request = route.request()
    const path = new URL(request.url()).pathname
    const json = (value: unknown) => route.fulfill({ json: value })
    if (path === '/api/auth/me') return json({ username: 'admin', has_avatar: false })
    if (path === '/api/events' || path === '/api/system/status/stream') {
      return route.fulfill({ contentType: 'text/event-stream', body: '' })
    }
    if (path === '/api/tasks') return json({ items: [] })
    if (path === '/api/auth/totp' && request.method() === 'GET') {
      return json({ enabled: false, recovery_codes: 0 })
    }
    if (path === '/api/auth/password' && request.method() === 'PATCH') {
      await passwordGate.promise
      return json({})
    }
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
  return { passwordGate }
}

async function startPasswordChange(page: Page, baseUrl: string) {
  await page.goto(`${baseUrl}/?account-password-loading=${Date.now()}`)
  await expect(page.getByRole('heading', { name: '我的文件', exact: true })).toBeVisible()
  await page.locator('button[title="打开账户设置"]').click()
  const account = page.locator('.account-modal')
  await account.getByRole('button', { name: '修改密码', exact: true }).click()
  const password = page.locator('.password-dialog')
  await expect(password).toBeVisible()
  await password.getByLabel('当前密码', { exact: true }).fill('current-password')
  await password.getByLabel('新密码', { exact: true }).fill('new-password-123')
  await password.getByLabel('确认新密码', { exact: true }).fill('new-password-123')
  await password.getByRole('button', { name: '修改密码', exact: true }).click()
  await expect(password.locator('button.primary')).toBeDisabled()
}

test('密码修改请求进行中点击账户外层遮罩仍关闭账户弹层', async ({ browser }) => {
  const oldUrl = process.env.E2E_REFERENCE_URL || 'http://127.0.0.1:18080'
  const newUrl = process.env.E2E_NEW_URL || 'http://127.0.0.1:18084'
  const oldContext = await browser.newContext({ viewport: { width: 1440, height: 900 } })
  const newContext = await browser.newContext({ viewport: { width: 1440, height: 900 } })
  const oldPage = await oldContext.newPage()
  const newPage = await newContext.newPage()

  try {
    const [oldMock, newMock] = await Promise.all([mockPasswordLoading(oldPage), mockPasswordLoading(newPage)])
    await Promise.all([startPasswordChange(oldPage, oldUrl), startPasswordChange(newPage, newUrl)])
    await Promise.all([
      oldPage.locator('.password-dialog header button').click(),
      newPage.locator('.password-dialog header button').click(),
    ])
    await expect(oldPage.locator('.password-dialog')).toHaveCount(0)
    await expect(newPage.locator('.password-dialog')).toHaveCount(0)
    await Promise.all([
      oldPage.locator('.modal-backdrop.accounting').click({ position: { x: 8, y: 8 } }),
      newPage.locator('.modal-backdrop.accounting').click({ position: { x: 8, y: 8 } }),
    ])
    await expect(oldPage.locator('.account-modal')).toHaveCount(0)
    await expect(newPage.locator('.account-modal'), 'Rust 密码请求中外层遮罩关闭与 reference 不一致').toHaveCount(0)
    oldMock.passwordGate.resolve()
    newMock.passwordGate.resolve()
  } finally {
    await oldContext.close()
    await newContext.close()
  }
})

async function mockUsernameSave(page: Page) {
  const gate = deferred()
  let started = false
  await page.route('**/api/**', async route => {
    const request = route.request()
    const path = new URL(request.url()).pathname
    const json = (value: unknown) => route.fulfill({ json: value })
    if (path === '/api/auth/me') return json({ username: 'admin', has_avatar: false })
    if (path === '/api/events' || path === '/api/system/status/stream') {
      return route.fulfill({ contentType: 'text/event-stream', body: '' })
    }
    if (path === '/api/tasks') return json({ items: [] })
    if (path === '/api/auth/totp') return json({ enabled: false, recovery_codes: 0 })
    if (path === '/api/profile/username' && request.method() === 'PATCH') {
      started = true
      await gate.promise
      return json({})
    }
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
  return { gate, started: () => started }
}

test('用户名 Enter 先按 reference 失焦再提交保存', async ({ browser }) => {
  const oldUrl = process.env.E2E_REFERENCE_URL || 'http://127.0.0.1:18080'
  const newUrl = process.env.E2E_NEW_URL || 'http://127.0.0.1:18084'
  const oldContext = await browser.newContext({ viewport: { width: 1440, height: 900 } })
  const newContext = await browser.newContext({ viewport: { width: 1440, height: 900 } })
  const oldPage = await oldContext.newPage()
  const newPage = await newContext.newPage()

  async function exercise(page: Page, baseUrl: string) {
    const mock = await mockUsernameSave(page)
    await openAccount(page, baseUrl)
    await page.locator('.edit-username').click()
    const input = page.locator('input[aria-label="用户名"]')
    await page.evaluate(() => {
      const element = document.querySelector('input[aria-label="用户名"]')!
      const state = window as Window & { __usernameEvents?: string[] }
      state.__usernameEvents = []
      for (const name of ['keydown', 'blur', 'focusout']) {
        element.addEventListener(name, () => state.__usernameEvents?.push(name), { capture: true })
      }
    })
    await input.fill('enter-user')
    await input.press('Enter')
    await expect.poll(mock.started).toBe(true)
    await page.waitForTimeout(30)
    const state = await page.evaluate(() => ({
      activeTag: document.activeElement?.tagName ?? '',
      activeAria: document.activeElement?.getAttribute('aria-label') ?? '',
      disabled: (document.querySelector('input[aria-label="用户名"]') as HTMLInputElement | null)?.disabled ?? false,
    }))
    const usernameEvents = await page.evaluate(() => (window as Window & { __usernameEvents?: string[] }).__usernameEvents ?? [])
    mock.gate.resolve()
    return { state, usernameEvents, started: mock.started() }
  }

  try {
    const [oldState, newState] = await Promise.all([
      exercise(oldPage, oldUrl),
      exercise(newPage, newUrl),
    ])
    expect(newState, 'Rust 用户名 Enter 的失焦/提交行为与 reference 不一致').toEqual(oldState)
    expect(oldState.started).toBe(true)
    expect(oldState.usernameEvents).toEqual(['keydown', 'blur', 'focusout'])
  } finally {
    await oldContext.close()
    await newContext.close()
  }
})

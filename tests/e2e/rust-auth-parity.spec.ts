import { expect, test, type Page } from '@playwright/test'
import { login } from './helpers'

const ROOT = '00000000-0000-0000-0000-000000000000'
const STAMP = '2026-01-01T00:00:00Z'

async function mockProfileWithoutAvatar(page: Page, response: 'session' | 'login' = 'session', nullableAvatar = false) {
  await page.route('**/api/**', async route => {
    const path = new URL(route.request().url()).pathname
    const json = (value: unknown) => route.fulfill({ json: value })
    if (path === '/api/auth/me') {
      return response === 'session'
        ? json(nullableAvatar ? { username: 'admin', has_avatar: null } : { username: 'admin' })
        : route.fulfill({ status: 401, json: { error: { status: 401, message: 'not signed in' } } })
    }
    if (path === '/api/auth/login') {
      return response === 'login'
        ? json(nullableAvatar ? { username: 'admin', has_avatar: null } : { username: 'admin' })
        : route.fulfill({ status: 401, json: { error: { status: 401, message: 'invalid credentials' } } })
    }
    if (path === '/api/events' || path === '/api/system/status/stream') {
      return route.fulfill({ contentType: 'text/event-stream', body: '' })
    }
    if (path === '/api/tasks') return json({ items: [] })
    if (path === '/api/library/all') {
      return json({
        items: { book: [], image: [], video: [], audio: [] },
        counts: { book: 0, image: 0, video: 0, audio: 0, file: 0 },
      })
    }
    if (path === '/api/library/counts') return json({ book: 0, image: 0, video: 0, audio: 0, file: 0 })
    if (path === '/api/system/status') {
      return json({
        status: 'ok',
        database: { status: 'ok', bytes: 0 },
        storage: { status: 'ok', bytes: 0, trash_bytes: 0, file_count: 0 },
        cache: { status: 'ok', memory_bytes: 0, disk_bytes: 0, memory_entries: 0, disk_entries: 0, classes: {} },
      })
    }
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

test('old/new 会话成功响应缺少 has_avatar 时仍进入已认证壳层', async ({ browser }) => {
  const oldUrl = process.env.E2E_REFERENCE_URL || 'http://127.0.0.1:18080'
  const newUrl = process.env.E2E_NEW_URL || 'http://127.0.0.1:18084'
  const oldContext = await browser.newContext()
  const newContext = await browser.newContext()
  const oldPage = await oldContext.newPage()
  const newPage = await newContext.newPage()

  try {
    await Promise.all([mockProfileWithoutAvatar(oldPage), mockProfileWithoutAvatar(newPage)])
    await Promise.all([oldPage.goto(`${oldUrl}/`), newPage.goto(`${newUrl}/`)])
    await Promise.all([
      expect(oldPage.getByRole('heading', { name: '我的文件', exact: true })).toBeVisible(),
      expect(newPage.getByRole('heading', { name: '我的文件', exact: true })).toBeVisible(),
    ])
    await Promise.all([
      expect(oldPage.getByRole('heading', { name: '登录私人空间', exact: true })).toHaveCount(0),
      expect(newPage.getByRole('heading', { name: '登录私人空间', exact: true })).toHaveCount(0),
    ])
  } finally {
    await oldContext.close()
    await newContext.close()
  }
})

test('old/new 登录成功响应缺少 has_avatar 时仍进入已认证壳层', async ({ browser }) => {
  const oldUrl = process.env.E2E_REFERENCE_URL || 'http://127.0.0.1:18080'
  const newUrl = process.env.E2E_NEW_URL || 'http://127.0.0.1:18084'
  const oldContext = await browser.newContext()
  const newContext = await browser.newContext()
  const oldPage = await oldContext.newPage()
  const newPage = await newContext.newPage()

  try {
    await Promise.all([mockProfileWithoutAvatar(oldPage, 'login'), mockProfileWithoutAvatar(newPage, 'login')])
    await Promise.all([oldPage.goto(`${oldUrl}/`), newPage.goto(`${newUrl}/`)])
    await Promise.all([
      oldPage.getByLabel('用户名').fill('admin'),
      newPage.getByLabel('用户名').fill('admin'),
      oldPage.getByLabel('密码').fill('revaro-e2e-password'),
      newPage.getByLabel('密码').fill('revaro-e2e-password'),
    ])
    await Promise.all([
      oldPage.getByRole('button', { name: '进入我的网盘' }).click(),
      newPage.getByRole('button', { name: '进入我的网盘' }).click(),
    ])
    await Promise.all([
      expect(oldPage.getByRole('heading', { name: '我的文件', exact: true })).toBeVisible(),
      expect(newPage.getByRole('heading', { name: '我的文件', exact: true })).toBeVisible(),
    ])
  } finally {
    await oldContext.close()
    await newContext.close()
  }
})

test('old/new 会话成功响应显式 null 的 has_avatar 时仍进入已认证壳层', async ({ browser }) => {
  const oldUrl = process.env.E2E_REFERENCE_URL || 'http://127.0.0.1:18080'
  const newUrl = process.env.E2E_NEW_URL || 'http://127.0.0.1:18084'
  const oldContext = await browser.newContext()
  const newContext = await browser.newContext()
  const oldPage = await oldContext.newPage()
  const newPage = await newContext.newPage()

  try {
    await Promise.all([
      mockProfileWithoutAvatar(oldPage, 'session', true),
      mockProfileWithoutAvatar(newPage, 'session', true),
    ])
    await Promise.all([oldPage.goto(`${oldUrl}/`), newPage.goto(`${newUrl}/`)])
    await Promise.all([
      expect(oldPage.getByRole('heading', { name: '我的文件', exact: true })).toBeVisible(),
      expect(newPage.getByRole('heading', { name: '我的文件', exact: true })).toBeVisible(),
    ])
    await Promise.all([
      expect(oldPage.getByRole('heading', { name: '登录私人空间', exact: true })).toHaveCount(0),
      expect(newPage.getByRole('heading', { name: '登录私人空间', exact: true })).toHaveCount(0),
    ])
  } finally {
    await oldContext.close()
    await newContext.close()
  }
})

test('old/new 登录成功响应显式 null 的 has_avatar 时仍进入已认证壳层', async ({ browser }) => {
  const oldUrl = process.env.E2E_REFERENCE_URL || 'http://127.0.0.1:18080'
  const newUrl = process.env.E2E_NEW_URL || 'http://127.0.0.1:18084'
  const oldContext = await browser.newContext()
  const newContext = await browser.newContext()
  const oldPage = await oldContext.newPage()
  const newPage = await newContext.newPage()

  try {
    await Promise.all([
      mockProfileWithoutAvatar(oldPage, 'login', true),
      mockProfileWithoutAvatar(newPage, 'login', true),
    ])
    await Promise.all([oldPage.goto(`${oldUrl}/`), newPage.goto(`${newUrl}/`)])
    await Promise.all([
      oldPage.getByLabel('用户名').fill('admin'),
      newPage.getByLabel('用户名').fill('admin'),
      oldPage.getByLabel('密码').fill('revaro-e2e-password'),
      newPage.getByLabel('密码').fill('revaro-e2e-password'),
    ])
    await Promise.all([
      oldPage.getByRole('button', { name: '进入我的网盘' }).click(),
      newPage.getByRole('button', { name: '进入我的网盘' }).click(),
    ])
    await Promise.all([
      expect(oldPage.getByRole('heading', { name: '我的文件', exact: true })).toBeVisible(),
      expect(newPage.getByRole('heading', { name: '我的文件', exact: true })).toBeVisible(),
    ])
  } finally {
    await oldContext.close()
    await newContext.close()
  }
})

test('会话检查期间保留旧版启动屏，登录失败和 Enter 提交行为一致', async ({ page }) => {
  await page.route('**/api/auth/me', async route => {
    await new Promise(resolve => setTimeout(resolve, 450))
    await route.fulfill({
      status: 401,
      contentType: 'application/json',
      body: JSON.stringify({ error: { status: 401, message: 'not signed in' } }),
    })
  })
  await page.route('**/api/auth/login', async route => {
    await new Promise(resolve => setTimeout(resolve, 450))
    await route.fulfill({
      status: 401,
      contentType: 'application/json',
      body: JSON.stringify({ error: { status: 401, message: 'invalid credentials' } }),
    })
  })

  await page.goto('/')
  await expect(page.locator('.splash')).toBeVisible()
  await expect(page.getByRole('heading', { name: '登录私人空间' })).toBeVisible()

  await page.getByLabel('用户名').fill('admin')
  const password = page.getByLabel('密码')
  await password.fill('wrong-password')
  await password.press('Enter')
  const submit = page.locator('.login-form button')
  await expect(submit).toBeDisabled()
  await expect(submit).toHaveText('正在验证…')
  await expect(page.getByText('invalid credentials', { exact: true })).toBeVisible()
  await expect(submit).toBeEnabled()
  await expect(submit).toHaveText('进入我的网盘')
})

test('明确的退出登录动作才结束会话并回到登录页', async ({ page }) => {
  await login(page)
  await page.locator('button[title="打开账户设置"]').click()
  const account = page.locator('.account-modal')
  await expect(account).toBeVisible()
  await account.getByRole('button', { name: '退出登录', exact: true }).click()
  await expect(page.getByRole('heading', { name: '登录私人空间' })).toBeVisible()
  await expect(page.locator('.account-modal')).toHaveCount(0)
  await expect(page.getByLabel('密码')).toBeVisible()
})

test('登录的 TOTP 分支保留二次输入和错误状态', async ({ page }) => {
  let attempt = 0
  await page.route('**/api/auth/login', async route => {
    attempt += 1
    const code = attempt === 1 ? 'totp_required' : 'invalid_second_factor'
    const message = attempt === 1 ? 'enter your authenticator or recovery code' : 'invalid second factor'
    await route.fulfill({
      status: 401,
      contentType: 'application/json',
      body: JSON.stringify({ error: { status: 401, code, message } }),
    })
  })

  await page.goto('/')
  await page.getByLabel('用户名').fill('admin')
  await page.getByLabel('密码').fill('not-the-password')
  const submit = page.getByRole('button', { name: '进入我的网盘' })
  await submit.click()
  await expect(page.getByLabel('验证码或恢复码')).toBeVisible()
  await expect(page.getByText('请输入身份验证器验证码或恢复码')).toBeVisible()

  await page.getByLabel('验证码或恢复码').fill('000000')
  await expect(submit).toBeEnabled()
  await submit.click()
  await expect(page.getByText('验证码或恢复码不正确')).toBeVisible()
  await expect(page.getByLabel('验证码或恢复码')).toHaveValue('000000')
  expect(attempt).toBe(2)
})

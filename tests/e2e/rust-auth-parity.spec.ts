import { expect, test } from '@playwright/test'
import { login } from './helpers'

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

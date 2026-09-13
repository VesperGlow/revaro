import { expect, test } from '@playwright/test'
import { login } from './helpers'

test('修改密码成功后回到登录页并保留旧版提示和用户名', async ({ page, request }) => {
  const originalPassword = process.env.E2E_PASSWORD || 'revaro-e2e-password'
  const origin = process.env.E2E_BASE_URL || 'http://127.0.0.1:18080'
  const replacementPassword = `parity-password-${crypto.randomUUID().replaceAll('-', '').slice(0, 20)}`
  let passwordChanged = false

  try {
    await login(page)
    await page.locator('button[title="打开账户设置"]').click()
    const account = page.locator('.account-modal')
    await expect(account).toBeVisible()
    const username = (await account.locator('.identity-row strong').innerText()).trim()

    await account.getByRole('button', { name: '修改密码', exact: true }).click()
    const password = page.locator('.password-dialog')
    await password.getByLabel('当前密码', { exact: true }).fill(originalPassword)
    await password.getByLabel('新密码', { exact: true }).fill(replacementPassword)
    await password.getByLabel('确认新密码', { exact: true }).fill(replacementPassword)
    await password.getByRole('button', { name: '修改密码', exact: true }).click()
    passwordChanged = true

    await expect(page.getByRole('heading', { name: '登录私人空间' })).toBeVisible()
    await expect(page.getByLabel('用户名')).toHaveValue(username)
    await expect(page.getByText('密码已更新，请重新登录')).toBeVisible()

    await page.getByLabel('密码').fill(replacementPassword)
    await page.getByRole('button', { name: '进入我的网盘' }).click()
    await expect(page.locator('.app-shell')).toBeVisible()

    // Restore the isolated test account through the same UI flow.
    await page.locator('button[title="打开账户设置"]').click()
    await page.locator('.account-modal').getByRole('button', { name: '修改密码', exact: true }).click()
    const restore = page.locator('.password-dialog')
    await restore.getByLabel('当前密码', { exact: true }).fill(replacementPassword)
    await restore.getByLabel('新密码', { exact: true }).fill(originalPassword)
    await restore.getByLabel('确认新密码', { exact: true }).fill(originalPassword)
    await restore.getByRole('button', { name: '修改密码', exact: true }).click()
    await expect(page.getByText('密码已更新，请重新登录')).toBeVisible()
    passwordChanged = false
  } finally {
    if (passwordChanged) {
      // If an assertion failed after the first PATCH, leave the shared test
      // account usable for the next isolated old/new run.
      const loggedIn = await request.post('/api/auth/login', {
        headers: { Origin: origin },
        data: { username: 'admin', password: replacementPassword, second_factor: '' },
      })
      if (loggedIn.ok()) {
        await request.patch('/api/auth/password', {
          headers: { Origin: origin },
          data: { current_password: replacementPassword, password: originalPassword },
        })
      }
    }
  }
})

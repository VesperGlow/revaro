import { test, expect, type Page } from '@playwright/test'
import { readFileSync } from 'node:fs'
import { login, uploadFixture, enterSelectionMode } from './helpers'

// This synthetic ZIP verifies sharing and index loading, never engine/game compatibility.
const archive = readFileSync(new URL('./fixtures/galgame-index.zip', import.meta.url))
const root = '00000000-0000-0000-0000-000000000000'
test.beforeEach(({ browserName }) => {
  test.skip(browserName !== 'chromium', 'Kirikiri JSPI runtime is verified on Chromium only')
})
async function search(page: Page, name: string) {
  if (!await page.getByLabel('搜索文件名').isVisible()) await page.getByRole('button', { name: '打开搜索', exact: true }).click()
  await page.getByLabel('搜索文件名').fill(name)
  await page.getByLabel('搜索文件名').press('Enter')
  await expect(page.locator('.file-card')).toHaveCount(1)
}

test('ZIP can create a password protected /g/ share in the file dialog', async ({ page, browser }) => {
  await login(page)
  const name = `galgame-${Date.now()}.zip`
  const file = await uploadFixture(page, root, name, 'application/zip', archive)
  await page.reload()
  await search(page, name)
  await enterSelectionMode(page)
  await page.locator('.file-card').press('Space')
  await page.locator('.selection-toolbar').getByRole('button', { name: '分享', exact: true }).click()
  const dialog = page.getByRole('dialog', { name: '分享文件' })
  await dialog.getByLabel('分享类型', { exact: true }).selectOption('galgame')
  await dialog.getByLabel('入口文件', { exact: true }).fill('data.xp3')
  await dialog.getByLabel('分享密码', { exact: true }).fill('galgame-test-password')
  await dialog.getByRole('button', { name: '创建公开链接' }).click()
  const link = dialog.getByLabel('分享链接', { exact: true })
  await expect(link).toHaveValue(/\/g\/[A-Za-z0-9_-]+$/)
  const url = await link.inputValue()
  const visitor = await browser.newContext()
  const game = await visitor.newPage()
  try {
    const requested: { method: string, range?: string }[] = []
    game.on('request', req => { if (req.url().endsWith('/archive')) requested.push({ method: req.method(), range: req.headers()['range'] }) })
    // Do not run the engine on the synthetic fixture. Real runtime is exercised separately with a real game.
    await game.route('**/galgame/kirikiri2/index.js', route => route.fulfill({ contentType: 'text/javascript', body: '' }))
    await game.goto(url)
    await expect(game.getByLabel('分享密码', { exact: true })).toBeVisible()
    expect(requested).toEqual([])
    expect((await visitor.request.head(`${url}/archive`)).status()).toBe(401)
    expect((await visitor.request.get(`${url}/manifest`)).status()).toBe(401)
    const range = game.waitForResponse(res => res.url().endsWith('/archive') && res.status() === 206)
    await game.getByLabel('分享密码', { exact: true }).fill('galgame-test-password')
    await game.getByRole('button', { name: '打开游戏', exact: true }).click()
    await range
    const manifest = await visitor.request.get(`${url}/manifest`)
    expect(manifest.status()).toBe(200)
    expect((await manifest.json()).entry).toBe('data.xp3')
    expect(requested.filter(req => req.method === 'GET').every(req => req.range)).toBeTruthy()
    expect(await game.evaluate(() => crossOriginIsolated)).toBeTruthy()
    const headers = { origin: new URL(page.url()).origin }
    expect((await page.request.delete(`/api/files/${file.id}/share`, { headers })).status()).toBe(204)
    expect((await visitor.request.head(`${url}/archive`)).status()).toBe(404)
  } finally {
    await visitor.close()
    await page.request.delete(`/api/files/${file.id}`, { headers: { origin: new URL(page.url()).origin } })
  }
})

for (const scenario of ['missing-range', 'missing-entry'] as const) {
  test(`player reports ${scenario} without downloading the whole ZIP`, async ({ page, browser }) => {
    await login(page)
    const file = await uploadFixture(page, root, `galgame-${scenario}-${Date.now()}.zip`, 'application/zip', archive)
    const headers = { origin: new URL(page.url()).origin }
    const created = await page.request.post(`/api/files/${file.id}/share`, { headers, data: {
      share_type: 'galgame', engine: 'kirikiri2', entry: scenario === 'missing-entry' ? 'absent.xp3' : 'data.xp3',
    } })
    expect(created.status()).toBe(201)
    const { url } = await created.json()
    const visitor = await browser.newContext()
    const game = await visitor.newPage()
    const downloads: string[] = []
    game.on('request', req => { if (req.url().endsWith('/archive') && req.method() === 'GET' && !req.headers()['range']) downloads.push(req.url()) })
    try {
      await game.route('**/galgame/kirikiri2/index.js', route => route.fulfill({ contentType: 'text/javascript', body: '' }))
      if (scenario === 'missing-range') {
        await game.route('**/archive', route => route.request().method() === 'HEAD'
          ? route.fulfill({ status: 200, headers: { 'Content-Length': String(archive.length) } })
          : route.continue())
      }
      await game.goto(url)
      await expect(game.locator('#error-overlay')).toHaveClass(/visible/)
      await expect(game.locator('#error-message')).toContainText(scenario === 'missing-entry' ? '入口 XP3 不存在' : 'HTTP Range')
      expect(downloads).toEqual([])
    } finally {
      await visitor.close()
      await page.request.delete(`/api/files/${file.id}`, { headers })
    }
  })
}

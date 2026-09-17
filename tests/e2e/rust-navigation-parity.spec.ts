import { expect, test } from '@playwright/test'
import { login } from './helpers'

const ROOT = '00000000-0000-0000-0000-000000000000'

async function createNestedFolders(page: Parameters<typeof login>[0], names: string[]) {
  return page.evaluate(async ({ names, root }) => {
    const created: string[] = []
    let parentId = root
    for (const name of names) {
      const response = await fetch('/api/directories', {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ parent_id: parentId, name }),
      })
      if (!response.ok) throw new Error(`创建目录失败：${response.status}`)
      const file = await response.json() as { id: string }
      created.push(file.id)
      parentId = file.id
    }
    return created
  }, { names, root: ROOT })
}

async function purgeNestedRoot(page: Parameters<typeof login>[0], name: string) {
  await page.evaluate(async ({ wanted, root }) => {
    const headers = { 'Content-Type': 'application/json' }
    const childrenResponse = await fetch(`/api/files/${root}/children`)
    if (childrenResponse.ok) {
      const children = await childrenResponse.json() as { items?: Array<{ id: string; name: string }> }
      const item = (children.items ?? []).find(entry => entry.name === wanted)
      if (item) await fetch(`/api/files/${item.id}`, { method: 'DELETE', headers })
    }
    const trashResponse = await fetch('/api/trash')
    if (trashResponse.ok) {
      const trash = await trashResponse.json() as { items?: Array<{ id: string; name: string }> }
      const item = (trash.items ?? []).find(entry => entry.name === wanted)
      if (item) await fetch(`/api/trash/${item.id}`, { method: 'DELETE', headers })
    }
  }, { wanted: name, root: ROOT })
}

test('深层目录面包屑自动显露当前路径并支持逐级返回', async ({ page }) => {
  await page.setViewportSize({ width: 390, height: 844 })
  await login(page)
  const suffix = crypto.randomUUID()
  const names = [
    `compat-nav-root-${suffix}`,
    `compat-nav-middle-${suffix}`,
    `compat-nav-deep-${suffix}`,
    `compat-nav-leaf-${suffix}`,
  ]
  await createNestedFolders(page, names)

  try {
    await page.reload()
    await expect(page.getByRole('heading', { name: '我的文件', exact: true })).toBeVisible()
    for (const name of names) {
      const entry = page.locator('.file-card').filter({ hasText: name })
      await expect(entry).toBeVisible()
      await entry.click()
      await expect(page.getByRole('heading', { name, exact: true })).toBeVisible()
    }

    const breadcrumbs = page.locator('nav.breadcrumbs')
    await expect(breadcrumbs).toBeVisible()
    await page.waitForTimeout(350)
    const metrics = await breadcrumbs.evaluate(element => ({
      clientWidth: element.clientWidth,
      scrollWidth: element.scrollWidth,
    }))
    expect(metrics.scrollWidth).toBeGreaterThan(metrics.clientWidth)
    await expect.poll(async () => breadcrumbs.evaluate(element =>
      element.scrollLeft - (element.scrollWidth - element.clientWidth)
    ), { timeout: 2_000 }).toBeGreaterThanOrEqual(-2)

    await page.goBack()
    await expect(page.getByRole('heading', { name: names[names.length - 2], exact: true })).toBeVisible()
    await page.locator('nav.breadcrumbs').getByRole('button', { name: '我的文件', exact: true }).click()
    await expect(page.getByRole('heading', { name: '我的文件', exact: true })).toBeVisible()
  } finally {
    await purgeNestedRoot(page, names[0])
  }
})

test('目录导航与浏览器后退前进保持 reference history 语义', async ({ page }) => {
  await page.setViewportSize({ width: 1280, height: 720 })
  await login(page)
  const name = `compat-history-${crypto.randomUUID()}`
  const folderId = await page.evaluate(async ({ name, root }) => {
    const response = await fetch('/api/directories', {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ parent_id: root, name }),
    })
    if (!response.ok) throw new Error(`创建目录失败：${response.status}`)
    return (await response.json() as { id: string }).id
  }, { name, root: ROOT })

  try {
    await page.reload()
    await page.locator('.file-card').filter({ hasText: name }).click()
    await expect(page.getByRole('heading', { name, exact: true })).toBeVisible()
    expect(new URL(page.url()).pathname).toBe(`/f/${folderId}`)

    await page.goBack()
    await expect(page.getByRole('heading', { name: '我的文件', exact: true })).toBeVisible()
    expect(new URL(page.url()).pathname).toBe('/')

    // The reference only handles the back action it pushed. A browser
    // forward restores the URL entry but does not replay the folder request;
    // keep this observed behaviour explicit so old/new runs remain honest.
    await page.goForward()
    await expect(page.getByRole('heading', { name: '我的文件', exact: true })).toBeVisible()
    expect(new URL(page.url()).pathname).toBe(`/f/${folderId}`)
  } finally {
    await page.evaluate(async ({ folderId }) => {
      await fetch(`/api/files/${folderId}`, {
        method: 'DELETE',
        headers: { 'Content-Type': 'application/json' },
      })
      await fetch(`/api/trash/${folderId}`, {
        method: 'DELETE',
        headers: { 'Content-Type': 'application/json' },
      })
    }, { folderId })
  }
})

test('弹层打开后浏览器后退先关闭弹层并保留当前页面', async ({ page }) => {
  await login(page)
  const account = page.locator('button[title="打开账户设置"]')
  await account.click()
  await expect(page.locator('.account-modal')).toBeVisible()
  expect(new URL(page.url()).pathname).toBe('/')

  await page.goBack()
  await expect(page.locator('.account-modal')).toHaveCount(0)
  await expect(page.getByRole('heading', { name: '我的文件', exact: true })).toBeVisible()
  expect(new URL(page.url()).pathname).toBe('/')
})

test('无效文件夹深链回到根目录并加载 reference 的默认页面', async ({ page }) => {
  await page.goto('/f/compatibility-folder-that-does-not-exist')
  await page.getByLabel('用户名').fill(process.env.E2E_USERNAME || 'admin')
  await page.getByLabel('密码').fill(process.env.E2E_PASSWORD || 'revaro-e2e-password')
  await page.getByRole('button', { name: '进入我的网盘' }).click()

  await expect(page.getByRole('heading', { name: '我的文件', exact: true })).toBeVisible()
  await expect.poll(() => new URL(page.url()).pathname).toBe('/')
  await expect(page.locator('.state[role="alert"]')).toHaveCount(0)
})

test('移动端账户工具菜单 Escape 保留 reference 的默认事件与 summary 焦点行为', async ({ page }) => {
  await page.setViewportSize({ width: 390, height: 844 })
  await login(page)
  const menu = page.locator('.mobile-account-menu')
  const summary = menu.locator('summary')
  await summary.click()
  await expect(menu).toHaveAttribute('open', '')
  await page.evaluate(() => {
    ;(window as Window & { __escapeDefaultPrevented?: boolean }).__escapeDefaultPrevented = undefined
    window.addEventListener('keydown', event => {
      if (event.key === 'Escape') {
        ;(window as Window & { __escapeDefaultPrevented?: boolean }).__escapeDefaultPrevented = event.defaultPrevented
      }
    }, { once: true })
  })
  await page.keyboard.press('Escape')
  await expect(menu).not.toHaveAttribute('open')
  expect(await page.evaluate(() => (window as Window & { __escapeDefaultPrevented?: boolean }).__escapeDefaultPrevented)).toBe(false)
  expect(await summary.evaluate(element => document.activeElement === element)).toBe(true)
})

test('文件浏览头下拉菜单 Escape 保留 reference 的默认事件和关闭行为', async ({ page }) => {
  await page.setViewportSize({ width: 390, height: 844 })
  await login(page)
  for (const selector of ['.create-menu', '.upload-menu']) {
    const menu = page.locator(selector)
    await menu.locator('summary').click()
    await expect(menu).toHaveAttribute('open', '')
    await page.evaluate(() => {
      ;(window as Window & { __escapeDefaultPrevented?: boolean }).__escapeDefaultPrevented = undefined
      window.addEventListener('keydown', event => {
        if (event.key === 'Escape') {
          ;(window as Window & { __escapeDefaultPrevented?: boolean }).__escapeDefaultPrevented = event.defaultPrevented
        }
      }, { once: true })
    })
    await page.keyboard.press('Escape')
    await expect(menu).not.toHaveAttribute('open')
    expect(await page.evaluate(() => (window as Window & { __escapeDefaultPrevented?: boolean }).__escapeDefaultPrevented)).toBe(false)
  }
})

test('认证壳层卸载时释放响应式媒体查询监听', async ({ page }) => {
  await page.addInitScript(() => {
    const state = { adds: 0, removes: 0 }
    ;(window as Window & { __compatMediaQuery?: typeof state }).__compatMediaQuery = state
    const prototype = MediaQueryList.prototype as MediaQueryList & {
      addEventListener: typeof MediaQueryList.prototype.addEventListener
      removeEventListener: typeof MediaQueryList.prototype.removeEventListener
    }
    const add = prototype.addEventListener
    const remove = prototype.removeEventListener
    prototype.addEventListener = function (...args) {
      if (args[0] === 'change') state.adds += 1
      return add.apply(this, args)
    }
    prototype.removeEventListener = function (...args) {
      if (args[0] === 'change') state.removes += 1
      return remove.apply(this, args)
    }
  })

  await login(page)
  await expect(page.getByRole('heading', { name: '我的文件', exact: true })).toBeVisible()
  const counts = await page.evaluate(() => window.__compatMediaQuery)
  expect(counts?.adds).toBeGreaterThan(0)

  await page.locator('button[title="打开账户设置"]').click()
  await page.locator('.account-modal').getByRole('button', { name: '退出登录', exact: true }).click()
  await expect(page.getByLabel('用户名')).toBeVisible()
  await expect.poll(async () => page.evaluate(() => window.__compatMediaQuery?.removes ?? 0)).toBe(counts?.adds)
})

test('目录读取中或失败时保留 reference 的当前选择状态', async ({ page }) => {
  const rootFile = {
    id: ROOT,
    parent_id: null,
    name: '我的文件',
    kind: 'directory',
    size: 0,
    status: 'ready',
    created_at: '2026-01-01T00:00:00Z',
    updated_at: '2026-01-01T00:00:00Z',
    mime_type: '',
  }
  const selectedFile = {
    id: 'compat-selected-file',
    parent_id: ROOT,
    name: '保留选择.txt',
    kind: 'file',
    size: 10,
    status: 'ready',
    created_at: '2026-01-01T00:00:00Z',
    updated_at: '2026-01-01T00:00:00Z',
    mime_type: 'text/plain',
  }
  const brokenFolder = {
    id: 'compat-broken-folder',
    parent_id: ROOT,
    name: '读取失败的目录',
    kind: 'directory',
    size: 0,
    status: 'ready',
    created_at: '2026-01-01T00:00:00Z',
    updated_at: '2026-01-01T00:00:00Z',
    mime_type: '',
  }

  await page.route('**/api/**', async route => {
    const path = new URL(route.request().url()).pathname
    const json = (value: unknown) => route.fulfill({ json: value })
    if (path === '/api/auth/me') return json({ username: 'admin', has_avatar: false })
    if (path === '/api/events' || path === '/api/system/status/stream') {
      return route.fulfill({ contentType: 'text/event-stream', body: '' })
    }
    if (path === '/api/tasks') return json({ items: [] })
    if (path === '/api/library/all') {
      return json({ items: { book: [], image: [], video: [], audio: [] }, counts: { book: 0, image: 0, video: 0, audio: 0, file: 2 } })
    }
    if (path === '/api/library/counts') return json({ book: 0, image: 0, video: 0, audio: 0, file: 2 })
    if (path === `/api/files/${ROOT}`) return json({ file: rootFile, breadcrumbs: [] })
    if (path === `/api/files/${ROOT}/children`) {
      return json({ items: [selectedFile, brokenFolder], total_bytes: 10, file_count: 1 })
    }
    if (path === `/api/files/${brokenFolder.id}` || path === `/api/files/${brokenFolder.id}/children`) {
      await new Promise(resolve => setTimeout(resolve, 500))
      return route.fulfill({ status: 500, json: { error: { status: 500, code: null, message: '模拟读取失败' } } })
    }
    return json({ items: [] })
  })

  await page.goto('/')
  await expect(page.getByRole('heading', { name: '我的文件', exact: true })).toBeVisible()
  const selectedRow = page.locator('.file-card').filter({ hasText: selectedFile.name })
  const brokenRow = page.locator('.file-card').filter({ hasText: brokenFolder.name })
  await selectedRow.getByRole('button', { name: '选择项目' }).click()
  await expect(page.locator('.selection-toolbar')).toBeVisible()

  await brokenRow.click()
  await expect(page.locator('.selection-toolbar')).toBeVisible()
  await expect(page.locator('.state')).toContainText('正在读取文件')
  await expect(page.locator('.toast')).toContainText('模拟读取失败')
  await expect(page.locator('.selection-toolbar')).toBeVisible()
  await expect(page.locator('.file-card').filter({ hasText: selectedFile.name })).toBeVisible()
})

test('old/new 目录导航丢弃迟到响应并保留最后一次点击结果', async ({ browser }) => {
  const oldUrl = process.env.E2E_REFERENCE_URL || 'http://127.0.0.1:18080'
  const newUrl = process.env.E2E_NEW_URL || 'http://127.0.0.1:18084'
  const oldContext = await browser.newContext({ viewport: { width: 1440, height: 900 } })
  const newContext = await browser.newContext({ viewport: { width: 1440, height: 900 } })
  const oldPage = await oldContext.newPage()
  const newPage = await newContext.newPage()
  const stamp = '2026-01-01T00:00:00Z'
  const root = { id: ROOT, parent_id: null, name: '我的文件', kind: 'directory', size: 0, status: 'ready', created_at: stamp, updated_at: stamp, mime_type: '' }
  const slow = { id: 'compat-stale-slow', parent_id: ROOT, name: '慢目录', kind: 'directory', size: 0, status: 'ready', created_at: stamp, updated_at: stamp, mime_type: '' }
  const fast = { id: 'compat-stale-fast', parent_id: ROOT, name: '快目录', kind: 'directory', size: 0, status: 'ready', created_at: stamp, updated_at: stamp, mime_type: '' }

  async function mock(page: Parameters<typeof login>[0]) {
    await page.route('**/api/**', async route => {
      const path = new URL(route.request().url()).pathname
      const json = (value: unknown) => route.fulfill({ json: value })
      if (path === '/api/auth/me') return json({ username: 'admin', has_avatar: false })
      if (path === '/api/events' || path === '/api/system/status/stream') {
        return route.fulfill({ contentType: 'text/event-stream', body: '' })
      }
      if (path === '/api/tasks') return json({ items: [] })
      if (path === '/api/library/all') {
        return json({ items: { book: [], image: [], video: [], audio: [] }, counts: { book: 0, image: 0, video: 0, audio: 0, file: 2 } })
      }
      if (path === '/api/library/counts') return json({ book: 0, image: 0, video: 0, audio: 0, file: 2 })
      if (path === `/api/files/${ROOT}`) return json({ file: root, breadcrumbs: [] })
      if (path === `/api/files/${ROOT}/children`) return json({ items: [slow, fast], total_bytes: 0, file_count: 2 })
      if (path === `/api/files/${slow.id}` || path === `/api/files/${slow.id}/children`) {
        await new Promise(resolve => setTimeout(resolve, 700))
        return json({
          ...(path.endsWith('/children') ? { items: [], total_bytes: 0, file_count: 0 } : {
            file: slow,
            breadcrumbs: [root, slow],
          }),
        })
      }
      if (path === `/api/files/${fast.id}` || path === `/api/files/${fast.id}/children`) {
        await new Promise(resolve => setTimeout(resolve, 30))
        return json({
          ...(path.endsWith('/children') ? { items: [], total_bytes: 0, file_count: 0 } : {
            file: fast,
            breadcrumbs: [root, fast],
          }),
        })
      }
      return json({ items: [] })
    })
  }

  async function exercise(page: Parameters<typeof login>[0], baseUrl: string) {
    await mock(page)
    await page.goto(`${baseUrl}/?stale-navigation-reference=${Date.now()}`)
    await expect(page.getByRole('heading', { name: '我的文件', exact: true })).toBeVisible()
    // Dispatch both clicks in one task. The first navigation may immediately
    // render its loading state and remove the root rows before Playwright can
    // perform a second action; the reference test is about the request race,
    // so both user click events must enter the controller before that render.
    await page.evaluate(({ slowName, fastName }) => {
      const find = (name: string) => [...document.querySelectorAll<HTMLElement>('.file-card')]
        .find(element => element.textContent?.includes(name))
      find(slowName)?.click()
      find(fastName)?.click()
    }, { slowName: slow.name, fastName: fast.name })
    await expect(page.getByRole('heading', { name: fast.name, exact: true })).toBeVisible()
    await expect.poll(() => new URL(page.url()).pathname).toBe(`/f/${fast.id}`)
    await page.waitForTimeout(900)
    return {
      heading: await page.getByRole('heading', { name: fast.name, exact: true }).count(),
      path: new URL(page.url()).pathname,
      slowHeading: await page.getByRole('heading', { name: slow.name, exact: true }).count(),
    }
  }

  try {
    const [oldState, newState] = await Promise.all([
      exercise(oldPage, oldUrl),
      exercise(newPage, newUrl),
    ])
    expect(oldState).toEqual({ heading: 1, path: `/f/${fast.id}`, slowHeading: 0 })
    expect(newState, 'Rust 迟到目录响应覆盖了 reference 的最后一次导航').toEqual(oldState)
  } finally {
    await Promise.all([oldContext.close(), newContext.close()])
  }
})

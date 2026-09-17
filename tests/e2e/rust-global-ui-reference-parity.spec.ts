import { expect, test, type Page } from '@playwright/test'

const ROOT = '00000000-0000-0000-0000-000000000000'
const STAMP = '2026-01-01T00:00:00Z'

const task = {
  id: 'global-ui-active-task',
  type: 'upload',
  status: 'running',
  phase: '上传中',
  progress: 42,
  speed: 0,
  retry_count: 0,
  max_retries: 3,
  error: '',
  source_type: '',
  source_id: '',
  cancel_requested: false,
  created_at: STAMP,
  updated_at: STAMP,
  finished_at: STAMP,
  name: '活动任务.bin',
}

const status = {
  status: 'degraded',
  database: { status: 'ok', bytes: 1024 },
  storage: { status: 'degraded', bytes: 2048, trash_bytes: 512, file_count: 1 },
  cache: {
    status: 'critical',
    memory_bytes: 1024,
    disk_bytes: 2048,
    memory_entries: 1,
    disk_entries: 2,
    classes: { default: { hits: 2, misses: 1, loads: 1, load_errors: 0, evictions: 0 } },
  },
}

async function mockShell(page: Page, statusValue = status) {
  await page.route('**/api/**', async route => {
    const path = new URL(route.request().url()).pathname
    const json = (value: unknown) => route.fulfill({ json: value })
    if (path === '/api/auth/me') return json({ username: 'admin', has_avatar: false })
    if (path === '/api/events') return route.fulfill({ contentType: 'text/event-stream', body: '' })
    if (path === '/api/system/status/stream') {
      return route.fulfill({
        contentType: 'text/event-stream',
        body: `event: status\ndata: ${JSON.stringify(statusValue)}\n\n`,
      })
    }
    if (path === '/api/tasks') return json({ items: [task] })
    if (path === '/api/library/all') {
      return json({ items: { book: [], image: [], video: [], audio: [] }, counts: { book: 0, image: 0, video: 0, audio: 0, file: 0 } })
    }
    if (path === '/api/library/counts') return json({ book: 0, image: 0, video: 0, audio: 0, file: 0 })
    if (path === '/api/trash') return json({ items: [], total_bytes: 0, file_count: 0 })
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

async function mockShellWithReconnect(page: Page) {
  let streamRequests = 0
  await page.route('**/api/**', async route => {
    const path = new URL(route.request().url()).pathname
    const json = (value: unknown) => route.fulfill({ json: value })
    if (path === '/api/auth/me') return json({ username: 'admin', has_avatar: false })
    if (path === '/api/events') return route.fulfill({ contentType: 'text/event-stream', body: '' })
    if (path === '/api/system/status/stream') {
      streamRequests += 1
      const statusValue = streamRequests === 1 ? status : { ...status, status: 'ok' }
      return route.fulfill({
        contentType: 'text/event-stream',
        body: `event: status\ndata: ${JSON.stringify(statusValue)}\n\n`,
      })
    }
    if (path === '/api/tasks') return json({ items: [task] })
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
  return () => streamRequests
}

async function openShell(page: Page, baseUrl: string) {
  await page.goto(`${baseUrl}/`)
  await expect(page.getByRole('heading', { name: '我的文件', exact: true })).toBeVisible()
}

async function badgeMetrics(page: Page, selector: string) {
  return page.locator(selector).evaluateAll(elements => elements.map(element => {
    const style = getComputedStyle(element)
    const rect = element.getBoundingClientRect()
    return {
      className: element.className,
      width: rect.width,
      height: rect.height,
      fontSize: style.fontSize,
      padding: style.padding,
      minHeight: style.minHeight,
    }
  }))
}

async function fileHeaderMetrics(page: Page) {
  return page.locator('.content-head').evaluate(element => {
    const styleOf = (selector: string) => getComputedStyle(element.querySelector(selector) as Element).display
    return {
      title: element.querySelector('h1')?.textContent?.trim(),
      meta: element.querySelector('.folder-meta')?.textContent?.replace(/\s+/g, ' ').trim(),
      desktopCreateActions: styleOf('.desktop-create-actions'),
      createMenu: styleOf('.create-menu'),
      uploadMenu: styleOf('.upload-menu'),
      titleRect: (() => {
        const rect = element.querySelector('h1')!.getBoundingClientRect()
        return { width: rect.width, height: rect.height }
      })(),
    }
  })
}

test('任务中心和系统状态 badge 保持 reference 尺寸与视觉层级', async ({ browser }) => {
  const oldUrl = process.env.E2E_REFERENCE_URL || 'http://127.0.0.1:18080'
  const newUrl = process.env.E2E_NEW_URL || 'http://127.0.0.1:18084'
  const oldContext = await browser.newContext({ viewport: { width: 1440, height: 900 } })
  const newContext = await browser.newContext({ viewport: { width: 1440, height: 900 } })
  const oldPage = await oldContext.newPage()
  const newPage = await newContext.newPage()

  try {
    await Promise.all([mockShell(oldPage), mockShell(newPage)])
    await Promise.all([openShell(oldPage, oldUrl), openShell(newPage, newUrl)])

    await oldPage.getByTitle('任务中心').click()
    await newPage.getByTitle('任务中心').click()
    const taskSelector = '.task-panel > header .status-badge'
    await expect(oldPage.locator(taskSelector)).toBeVisible()
    await expect(newPage.locator(taskSelector)).toBeVisible()
    expect(await badgeMetrics(newPage, taskSelector)).toEqual(await badgeMetrics(oldPage, taskSelector))

    await oldPage.getByTitle('任务中心').click()
    await newPage.getByTitle('任务中心').click()
    await oldPage.locator('.system-status > summary').click()
    await newPage.locator('.system-status > summary').click()
    const serviceSelector = '.status-grid .service-card .status-badge'
    await expect(oldPage.locator(serviceSelector)).toHaveCount(3)
    await expect(newPage.locator(serviceSelector)).toHaveCount(3)
    expect(await badgeMetrics(newPage, serviceSelector)).toEqual(await badgeMetrics(oldPage, serviceSelector))
    expect(await newPage.locator('.status-grid .service-card').nth(2).innerText())
      .toBe(await oldPage.locator('.status-grid .service-card').nth(2).innerText())
  } finally {
    await oldContext.close()
    await newContext.close()
  }
})

test('old/new 系统状态 SSE 显式 null 状态仍保留服务卡', async ({ browser }) => {
  const oldUrl = process.env.E2E_REFERENCE_URL || 'http://127.0.0.1:18080'
  const newUrl = process.env.E2E_NEW_URL || 'http://127.0.0.1:18084'
  const nullableStatus = {
    status: null,
    database: { status: null, bytes: 1024 },
    storage: { status: null, bytes: 2048, trash_bytes: 512, file_count: 1 },
    cache: {
      status: null,
      memory_bytes: 1024,
      disk_bytes: 2048,
      memory_entries: 1,
      disk_entries: 2,
      classes: { default: { hits: 2, misses: 1, loads: 1, load_errors: 0, evictions: 0 } },
    },
  }
  const oldContext = await browser.newContext({ viewport: { width: 1440, height: 900 } })
  const newContext = await browser.newContext({ viewport: { width: 1440, height: 900 } })
  const oldPage = await oldContext.newPage()
  const newPage = await newContext.newPage()

  try {
    await Promise.all([mockShell(oldPage, nullableStatus), mockShell(newPage, nullableStatus)])
    await Promise.all([openShell(oldPage, oldUrl), openShell(newPage, newUrl)])
    await Promise.all([
      oldPage.locator('.system-status > summary').click(),
      newPage.locator('.system-status > summary').click(),
    ])
    const snapshot = async (page: Page) => ({
      className: await page.locator('.system-status').getAttribute('class'),
      error: (await page.locator('.status-error').count()) > 0 ? await page.locator('.status-error').textContent() : null,
      cards: await page.locator('.status-grid .service-card').count(),
      badges: await page.locator('.status-grid .status-badge').allTextContents(),
    })
    const oldState = await snapshot(oldPage)
    const newState = await snapshot(newPage)
    expect(oldState).toEqual({ className: 'system-status pending', error: null, cards: 3, badges: ['正常', '2.0 KB', '正常'] })
    expect(newState, 'Rust 系统状态不应因 null 状态字段丢弃整条 SSE').toEqual(oldState)
  } finally {
    await Promise.all([oldContext.close(), newContext.close()])
  }
})

test('old/new 系统状态统计字段缺省或为 null 时仍保留服务卡', async ({ browser }) => {
  const oldUrl = process.env.E2E_REFERENCE_URL || 'http://127.0.0.1:18180'
  const newUrl = process.env.E2E_NEW_URL || 'http://127.0.0.1:18184'
  const sparseStatus = {
    status: null,
    database: { status: null },
    storage: { status: null, bytes: null, file_count: null },
    cache: {
      status: null,
      memory_bytes: null,
      memory_entries: null,
      disk_entries: null,
      classes: { default: { hits: null, loads: null, evictions: null } },
    },
  }
  const oldContext = await browser.newContext({ viewport: { width: 1440, height: 900 } })
  const newContext = await browser.newContext({ viewport: { width: 1440, height: 900 } })
  const oldPage = await oldContext.newPage()
  const newPage = await newContext.newPage()
  try {
    await Promise.all([mockShell(oldPage, sparseStatus), mockShell(newPage, sparseStatus)])
    await Promise.all([openShell(oldPage, oldUrl), openShell(newPage, newUrl)])
    await Promise.all([
      oldPage.locator('.system-status > summary').click(),
      newPage.locator('.system-status > summary').click(),
    ])
    const snapshot = async (page: Page) => ({
      className: await page.locator('.system-status').getAttribute('class'),
      error: (await page.locator('.status-error').count()) > 0 ? await page.locator('.status-error').textContent() : null,
      cards: await page.locator('.status-grid .service-card').count(),
      texts: await page.locator('.status-grid .service-card').allTextContents(),
    })
    const oldState = await snapshot(oldPage)
    const newState = await snapshot(newPage)
    expect(oldState).toEqual({
      className: 'system-status pending',
      error: null,
      cards: 3,
      texts: [
        '正常数据库数据占用 NaN undefined',
        'NaN undefined网盘存储使用量null 个文件 · 回收站 NaN undefined',
        '正常服务端缓存内存 NaN undefined · 磁盘 NaN undefined · 暂无读取',
      ],
    })
    expect(newState, 'Rust 不应因状态统计缺省/null 丢弃整条 SSE').toEqual({
      className: 'system-status pending',
      error: null,
      cards: 3,
      texts: [
        '正常数据库数据占用 0 B',
        '0 B网盘存储使用量0 个文件 · 回收站 0 B',
        '正常服务端缓存内存 0 B · 磁盘 0 B · 暂无读取',
      ],
    })
  } finally {
    await Promise.all([oldContext.close(), newContext.close()])
  }
})

test('桌面与移动顶栏入口保持 reference 的完整分流', async ({ browser }) => {
  const oldUrl = process.env.E2E_REFERENCE_URL || 'http://127.0.0.1:18080'
  const newUrl = process.env.E2E_NEW_URL || 'http://127.0.0.1:18084'
  const oldContext = await browser.newContext({ viewport: { width: 1440, height: 900 } })
  const newContext = await browser.newContext({ viewport: { width: 1440, height: 900 } })
  const oldPage = await oldContext.newPage()
  const newPage = await newContext.newPage()

  const closeAccount = async (page: Page) => {
    await page.locator('.account-modal > header button').click()
    await expect(page.locator('.account-modal')).toHaveCount(0)
  }

  const closeOutside = async (page: Page) => {
    const viewport = page.viewportSize()!
    await page.mouse.click(4, viewport.height - 4)
  }

  const exerciseDesktop = async (page: Page) => {
    await page.getByTitle('任务中心').click()
    await expect(page.locator('.task-panel')).toBeVisible()
    await page.getByTitle('任务中心').click()
    await expect(page.locator('.task-panel')).toBeHidden()

    await page.locator('.system-status > summary').click()
    await expect(page.locator('.status-panel')).toBeVisible()
    await closeOutside(page)
    await expect(page.locator('.status-panel')).toBeHidden()

    await page.locator('button[title="打开账户设置"]').click()
    await expect(page.locator('.account-modal')).toBeVisible()
    await expect(page.getByRole('heading', { name: '登录私人空间' })).toHaveCount(0)
    await closeAccount(page)

    await page.locator('.topbar button[title="回收站"]').click()
    await expect(page.getByRole('heading', { name: '回收站', exact: true })).toBeVisible()
    await page.getByRole('button', { name: '返回我的文件', exact: true }).click()
    await expect(page.getByRole('heading', { name: '我的文件', exact: true })).toBeVisible()
  }

  const exerciseMobile = async (page: Page) => {
    await page.setViewportSize({ width: 390, height: 844 })
    await page.locator('summary[aria-label="打开账户与工具菜单"]').click()
    const menu = page.locator('.mobile-account-menu')
    await expect(menu).toHaveAttribute('open', '')

    await menu.getByRole('button', { name: /^任务中心/ }).click()
    await expect(menu).not.toHaveAttribute('open')
    await expect(page.locator('.task-panel')).toBeVisible()
    await closeOutside(page)
    await expect(page.locator('.task-panel')).toBeHidden()

    await menu.locator('summary').click()
    await menu.getByRole('button', { name: '账户设置', exact: true }).click()
    await expect(menu).not.toHaveAttribute('open')
    await expect(page.locator('.account-modal')).toBeVisible()
    await closeAccount(page)

    await menu.locator('summary').click()
    await menu.getByRole('button', { name: '回收站', exact: true }).click()
    await expect(menu).not.toHaveAttribute('open')
    await expect(page.getByRole('heading', { name: '回收站', exact: true })).toBeVisible()
    await page.getByRole('button', { name: '返回我的文件', exact: true }).click()
    await expect(page.getByRole('heading', { name: '我的文件', exact: true })).toBeVisible()

    await page.locator('.system-status > summary').click()
    await expect(page.locator('.status-panel')).toBeVisible()
    await closeOutside(page)
    await expect(page.locator('.status-panel')).toBeHidden()
  }

  try {
    await Promise.all([mockShell(oldPage), mockShell(newPage)])
    await Promise.all([openShell(oldPage, oldUrl), openShell(newPage, newUrl)])
    await Promise.all([exerciseDesktop(oldPage), exerciseDesktop(newPage)])
    await Promise.all([exerciseMobile(oldPage), exerciseMobile(newPage)])
  } finally {
    await oldContext.close()
    await newContext.close()
  }
})

async function globalDisclosureSnapshot(page: Page) {
  return page.evaluate(() => {
    const visible = (selector: string) => {
      const element = document.querySelector(selector) as HTMLElement | null
      if (!element) return false
      const style = getComputedStyle(element)
      return style.display !== 'none' && style.visibility !== 'hidden'
    }
    return {
      taskOpen: document.querySelector('.task-center')?.hasAttribute('open') ?? false,
      taskVisible: visible('.task-panel'),
      statusOpen: document.querySelector('.system-status')?.hasAttribute('open') ?? false,
      statusVisible: visible('.status-panel'),
      mobileMenuOpen: document.querySelector('.mobile-account-menu')?.hasAttribute('open') ?? false,
      accountVisible: visible('.account-modal'),
      trashHeading: document.querySelector('h1')?.textContent?.trim() ?? null,
      pathname: location.pathname,
    }
  })
}

test('顶栏 disclosure 互相切换、外部关闭和重复操作保持 reference', async ({ browser }) => {
  const oldUrl = process.env.E2E_REFERENCE_URL || 'http://127.0.0.1:18080'
  const newUrl = process.env.E2E_NEW_URL || 'http://127.0.0.1:18084'
  const oldContext = await browser.newContext({ viewport: { width: 1440, height: 900 } })
  const newContext = await browser.newContext({ viewport: { width: 1440, height: 900 } })
  const oldPage = await oldContext.newPage()
  const newPage = await newContext.newPage()

  const compare = async (label: string) => {
    expect(await globalDisclosureSnapshot(newPage), `Rust ${label} 与 reference 的全局 disclosure 状态不一致`)
      .toEqual(await globalDisclosureSnapshot(oldPage))
  }

  const desktopSequence = async (page: Page, baseUrl: string) => {
    await openShell(page, baseUrl)

    await page.locator('.task-center > summary').click()
    await expect(page.locator('.task-panel')).toBeVisible()
    await page.locator('.system-status > summary').click()
    await expect(page.locator('.status-panel')).toBeVisible()
    await page.locator('.system-status > summary').click()
    await page.locator('.task-center > summary').click()
    await page.locator('.task-center > summary').click()
    await expect(page.locator('.task-panel')).toBeHidden()

    await page.locator('.system-status > summary').click()
    await page.mouse.click(4, 896)
    await expect(page.locator('.status-panel')).toBeHidden()

    await page.locator('.system-status > summary').click()
    await page.locator('button[title="打开账户设置"]').click()
    await expect(page.locator('.account-modal')).toBeVisible()
    await expect(page.locator('.task-panel')).toBeHidden()
    await expect(page.locator('.status-panel')).toBeHidden()
    await page.locator('.account-modal > header button').click()

    await page.locator('.topbar button[title="回收站"]').click()
    await expect(page.getByRole('heading', { name: '回收站', exact: true })).toBeVisible()
    await page.getByRole('button', { name: '返回我的文件', exact: true }).click()
    await expect(page.getByRole('heading', { name: '我的文件', exact: true })).toBeVisible()
  }

  const mobileSequence = async (page: Page, baseUrl: string) => {
    await page.setViewportSize({ width: 390, height: 844 })
    await expect(page.locator('.mobile-account-menu > summary')).toBeVisible()

    await page.locator('.mobile-account-menu > summary').click()
    await page.locator('.system-status > summary').click()
    await expect(page.locator('.status-panel')).toBeVisible()
    await page.locator('.system-status > summary').click()
    await page.locator('.mobile-account-menu > summary').click()
    await expect(page.locator('.mobile-account-menu')).toHaveAttribute('open', '')
    await page.locator('.mobile-account-menu > summary').click()
    await expect(page.locator('.mobile-account-menu')).not.toHaveAttribute('open')

    await page.locator('.mobile-account-menu > summary').click()
    await page.locator('.mobile-account-menu').getByRole('button', { name: /^任务中心/ }).click()
    await expect(page.locator('.task-panel')).toBeVisible()
    await page.locator('.system-status > summary').click()
    await expect(page.locator('.status-panel')).toBeVisible()
    await expect(page.locator('.task-panel')).toBeHidden()
    await page.mouse.click(4, 840)
    await expect(page.locator('.status-panel')).toBeHidden()

    await page.locator('.mobile-account-menu > summary').click()
    await page.locator('.mobile-account-menu').getByRole('button', { name: '账户设置', exact: true }).click()
    await expect(page.locator('.account-modal')).toBeVisible()
    await page.locator('.account-modal > header button').click()
  }

  try {
    await Promise.all([mockShell(oldPage), mockShell(newPage)])
    await Promise.all([desktopSequence(oldPage, oldUrl), desktopSequence(newPage, newUrl)])
    await compare('桌面连续切换完成后')

    await Promise.all([mobileSequence(oldPage, oldUrl), mobileSequence(newPage, newUrl)])
    await compare('移动端连续切换完成后')
  } finally {
    await Promise.all([oldContext.close(), newContext.close()])
  }
})

test('文件浏览头断点布局保持 reference', async ({ browser }) => {
  const oldUrl = process.env.E2E_REFERENCE_URL || 'http://127.0.0.1:18080'
  const newUrl = process.env.E2E_NEW_URL || 'http://127.0.0.1:18084'
  const oldContext = await browser.newContext({ viewport: { width: 1440, height: 900 } })
  const newContext = await browser.newContext({ viewport: { width: 1440, height: 900 } })
  const oldPage = await oldContext.newPage()
  const newPage = await newContext.newPage()

  try {
    await Promise.all([mockShell(oldPage), mockShell(newPage)])
    await Promise.all([openShell(oldPage, oldUrl), openShell(newPage, newUrl)])

    for (const viewport of [{ width: 1440, height: 900 }, { width: 390, height: 844 }]) {
      await Promise.all([oldPage.setViewportSize(viewport), newPage.setViewportSize(viewport)])
      await expect(oldPage.locator('.content-head')).toBeVisible()
      await expect(newPage.locator('.content-head')).toBeVisible()
      expect(await fileHeaderMetrics(newPage)).toEqual(await fileHeaderMetrics(oldPage))
    }
  } finally {
    await oldContext.close()
    await newContext.close()
  }
})

test('系统状态保留 reference 的未知/critical 顶层状态 class', async ({ page }) => {
  await mockShell(page, { ...status, status: 'critical' })
  await openShell(page, process.env.E2E_BASE_URL || 'http://127.0.0.1:18080')
  await expect(page.locator('.system-status')).toHaveClass(/system-status critical/)
})

test('系统状态 Escape 保留 reference 的默认事件与 summary 焦点行为', async ({ page }) => {
  await mockShell(page)
  await openShell(page, process.env.E2E_BASE_URL || 'http://127.0.0.1:18080')
  await page.locator('.system-status > summary').click()
  await page.evaluate(() => {
    ;(window as Window & { __escapeDefaultPrevented?: boolean }).__escapeDefaultPrevented = undefined
    window.addEventListener('keydown', event => {
      if (event.key === 'Escape') {
        ;(window as Window & { __escapeDefaultPrevented?: boolean }).__escapeDefaultPrevented = event.defaultPrevented
      }
    }, { once: true })
  })
  await page.keyboard.press('Escape')
  await expect(page.locator('.system-status')).not.toHaveAttribute('open')
  expect(await page.evaluate(() => (window as Window & { __escapeDefaultPrevented?: boolean }).__escapeDefaultPrevented)).toBe(false)
  expect(await page.locator('.system-status > summary').evaluate(element => document.activeElement === element)).toBe(true)
})

test('系统状态 SSE 断线按 reference 重连并更新顶层状态', async ({ page }) => {
  const streamRequests = await mockShellWithReconnect(page)
  await openShell(page, process.env.E2E_BASE_URL || 'http://127.0.0.1:18080')
  await expect(page.locator('.system-status')).toHaveClass(/system-status degraded/)
  await expect.poll(streamRequests, { timeout: 5_000 }).toBeGreaterThan(1)
  await expect(page.locator('.system-status')).toHaveClass(/system-status ok/)
  await expect(page.locator('.system-status')).toContainText('所有服务正常')
})

test('系统状态在退出登录时清理 EventSource 与重连定时器', async ({ page }) => {
  const streamRequests = await mockShellWithReconnect(page)
  await openShell(page, process.env.E2E_BASE_URL || 'http://127.0.0.1:18080')
  await expect(page.locator('.system-status')).toHaveClass(/system-status degraded/)
  await page.locator('button[title="打开账户设置"]').click()
  const account = page.locator('.account-modal')
  await expect(account).toBeVisible()
  await account.getByRole('button', { name: '退出登录', exact: true }).click()
  await expect(page.getByRole('heading', { name: '登录私人空间' })).toBeVisible()
  const requestsAfterLogout = streamRequests()
  await page.waitForTimeout(1_300)
  expect(streamRequests()).toBe(requestsAfterLogout)
})

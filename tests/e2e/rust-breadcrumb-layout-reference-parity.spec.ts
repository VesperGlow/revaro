import { expect, test, type Page } from '@playwright/test'

const ROOT = '00000000-0000-0000-0000-000000000000'
const stamp = '2026-01-01T00:00:00Z'

const folders = [
  { id: 'breadcrumb-level-1', name: '一级目录', parent_id: ROOT },
  { id: 'breadcrumb-level-2', name: '二级目录', parent_id: 'breadcrumb-level-1' },
  { id: 'breadcrumb-level-3', name: '三级目录', parent_id: 'breadcrumb-level-2' },
]

function file(folder: { id: string; name: string; parent_id: string | null }) {
  return {
    ...folder,
    kind: 'directory',
    size: 0,
    status: 'ready',
    created_at: stamp,
    updated_at: stamp,
    mime_type: '',
  }
}

const root = file({ id: ROOT, name: '我的文件', parent_id: null })

async function mockNavigation(page: Page) {
  await page.route('**/api/**', async route => {
    const path = new URL(route.request().url()).pathname
    const json = (value: unknown) => route.fulfill({ json: value })

    if (path === '/api/auth/me') return json({ username: 'admin', has_avatar: false })
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
    if (path === '/api/library/counts') {
      return json({ book: 0, image: 0, video: 0, audio: 0, file: 0 })
    }
    if (path === `/api/files/${ROOT}`) return json({ file: root, breadcrumbs: [] })
    if (path === `/api/files/${ROOT}/children`) return json({ items: [file(folders[0])], total_bytes: 0, file_count: 0 })

    const folder = folders.find(entry => path === `/api/files/${entry.id}`)
    if (folder) {
      const index = folders.indexOf(folder)
      return json({
        file: file(folder),
        breadcrumbs: [root, ...folders.slice(0, index + 1).map(file)],
      })
    }
    const folderIndex = folders.findIndex(entry => path === `/api/files/${entry.id}/children`)
    if (folderIndex >= 0) {
      const child = folders[folderIndex + 1]
      return json({ items: child ? [file(child)] : [], total_bytes: 0, file_count: 0 })
    }
    return json({ items: [] })
  })
}

async function openDeepPath(page: Page, baseUrl: string) {
  await page.goto(`${baseUrl}/`)
  await expect(page.getByRole('heading', { name: '我的文件', exact: true })).toBeVisible()
  for (const folder of folders) {
    await page.locator('.file-card').filter({ hasText: folder.name }).click()
    await expect(page.getByRole('heading', { name: folder.name, exact: true })).toBeVisible()
  }
  await expect(page.locator('nav.breadcrumbs')).toBeVisible()
}

async function installBreadcrumbScrollProbe(page: Page) {
  await page.addInitScript(() => {
    const calls: Array<{ left?: number; behavior?: string }> = []
    const original = Element.prototype.scrollTo as unknown as (...args: unknown[]) => void
    Element.prototype.scrollTo = function (...args: unknown[]) {
      if (this.matches('nav.breadcrumbs')) {
        const options = args[0]
        if (typeof options === 'object' && options !== null) {
          const value = options as { left?: number; behavior?: string }
          calls.push({ left: value.left, behavior: value.behavior })
        } else {
          calls.push({ left: typeof options === 'number' ? options : undefined })
        }
      }
      original.apply(this, args)
    } as typeof Element.prototype.scrollTo
    ;(window as typeof window & { __breadcrumbScrollCalls?: typeof calls }).__breadcrumbScrollCalls = calls
  })
}

async function breadcrumbScrollCalls(page: Page) {
  return page.evaluate(() =>
    (window as typeof window & { __breadcrumbScrollCalls?: Array<{ left?: number; behavior?: string }> }).__breadcrumbScrollCalls ?? [],
  )
}

async function waitForBreadcrumbScrollSettled(page: Page) {
  await expect.poll(
    () => page.locator('nav.breadcrumbs').evaluate(nav => nav.scrollLeft >= nav.scrollWidth - nav.clientWidth - 1),
    { timeout: 2000, intervals: [50, 100, 200] },
  ).toBe(true)
}

async function breadcrumbLayout(page: Page) {
  return page.locator('nav.breadcrumbs').evaluate(nav => ({
    childTags: Array.from(nav.children).map(child => child.tagName.toLowerCase()),
    buttons: Array.from(nav.querySelectorAll('button')).map(button => {
      const style = getComputedStyle(button)
      const rect = button.getBoundingClientRect()
      return {
        name: button.textContent?.trim(),
        marginLeft: style.marginLeft,
        marginRight: style.marginRight,
        left: Math.round(rect.left * 100) / 100,
        right: Math.round(rect.right * 100) / 100,
      }
    }),
  }))
}

test('移动端深层面包屑的 DOM 层级和首末边距保持 reference', async ({ browser }) => {
  const oldUrl = process.env.E2E_REFERENCE_URL || 'http://127.0.0.1:18080'
  const newUrl = process.env.E2E_NEW_URL || 'http://127.0.0.1:18084'
  const oldContext = await browser.newContext({ viewport: { width: 390, height: 844 } })
  const newContext = await browser.newContext({ viewport: { width: 390, height: 844 } })
  const oldPage = await oldContext.newPage()
  const newPage = await newContext.newPage()

  try {
    await Promise.all([mockNavigation(oldPage), mockNavigation(newPage)])
    await Promise.all([openDeepPath(oldPage, oldUrl), openDeepPath(newPage, newUrl)])
    await Promise.all([waitForBreadcrumbScrollSettled(oldPage), waitForBreadcrumbScrollSettled(newPage)])
    const oldLayout = await breadcrumbLayout(oldPage)
    const newLayout = await breadcrumbLayout(newPage)
    expect(newLayout, 'Rust 面包屑不应改变 reference 的子节点层级或移动端边距').toEqual(oldLayout)
  } finally {
    await oldContext.close()
    await newContext.close()
  }
})

test('深层面包屑沿用 reference 的平滑自动显露行为', async ({ browser }) => {
  const oldUrl = process.env.E2E_REFERENCE_URL || 'http://127.0.0.1:18080'
  const newUrl = process.env.E2E_NEW_URL || 'http://127.0.0.1:18084'
  const oldContext = await browser.newContext({ viewport: { width: 390, height: 844 } })
  const newContext = await browser.newContext({ viewport: { width: 390, height: 844 } })
  const oldPage = await oldContext.newPage()
  const newPage = await newContext.newPage()

  try {
    await Promise.all([
      mockNavigation(oldPage),
      mockNavigation(newPage),
      installBreadcrumbScrollProbe(oldPage),
      installBreadcrumbScrollProbe(newPage),
    ])
    await Promise.all([openDeepPath(oldPage, oldUrl), openDeepPath(newPage, newUrl)])
    const oldCalls = await breadcrumbScrollCalls(oldPage)
    const newCalls = await breadcrumbScrollCalls(newPage)
    expect(oldCalls.length).toBeGreaterThan(0)
    expect(oldCalls.every(call => call.behavior === 'smooth')).toBe(true)
    expect(newCalls).toEqual(oldCalls)
  } finally {
    await oldContext.close()
    await newContext.close()
  }
})

test('深层面包屑中间级保留 reference 的点击、Enter 和触摸导航行为', async ({ browser }) => {
  const oldUrl = process.env.E2E_REFERENCE_URL || 'http://127.0.0.1:18080'
  const newUrl = process.env.E2E_NEW_URL || 'http://127.0.0.1:18084'
  const oldContext = await browser.newContext({ viewport: { width: 390, height: 844 }, hasTouch: true, isMobile: true })
  const newContext = await browser.newContext({ viewport: { width: 390, height: 844 }, hasTouch: true, isMobile: true })
  const oldPage = await oldContext.newPage()
  const newPage = await newContext.newPage()

  try {
    await Promise.all([mockNavigation(oldPage), mockNavigation(newPage)])
    await Promise.all([openDeepPath(oldPage, oldUrl), openDeepPath(newPage, newUrl)])

    await oldPage.locator('nav.breadcrumbs button').nth(1).click()
    await newPage.locator('nav.breadcrumbs button').nth(1).click()
    await expect(oldPage.getByRole('heading', { name: '一级目录', exact: true })).toBeVisible()
    await expect(newPage.getByRole('heading', { name: '一级目录', exact: true })).toBeVisible()
    expect(new URL(newPage.url()).pathname).toBe(new URL(oldPage.url()).pathname)
    expect(await newPage.locator('nav.breadcrumbs').innerText()).toBe(await oldPage.locator('nav.breadcrumbs').innerText())

    await Promise.all([openDeepPath(oldPage, oldUrl), openDeepPath(newPage, newUrl)])
    await oldPage.locator('nav.breadcrumbs button').nth(2).focus()
    await newPage.locator('nav.breadcrumbs button').nth(2).focus()
    await oldPage.keyboard.press('Enter')
    await newPage.keyboard.press('Enter')
    await expect(oldPage.getByRole('heading', { name: '二级目录', exact: true })).toBeVisible()
    await expect(newPage.getByRole('heading', { name: '二级目录', exact: true })).toBeVisible()
    expect(new URL(newPage.url()).pathname).toBe(new URL(oldPage.url()).pathname)

    await Promise.all([openDeepPath(oldPage, oldUrl), openDeepPath(newPage, newUrl)])
    await oldPage.locator('nav.breadcrumbs button').nth(3).tap()
    await newPage.locator('nav.breadcrumbs button').nth(3).tap()
    await expect(oldPage.getByRole('heading', { name: '三级目录', exact: true })).toBeVisible()
    await expect(newPage.getByRole('heading', { name: '三级目录', exact: true })).toBeVisible()
    expect(new URL(newPage.url()).pathname).toBe(new URL(oldPage.url()).pathname)
  } finally {
    await oldContext.close()
    await newContext.close()
  }
})

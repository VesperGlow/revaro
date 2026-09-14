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
      const entry = page.locator('.file-card, .file-row').filter({ hasText: name })
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

test('从嵌套目录进入回收站后点击文件分类返回原目录', async ({ page }) => {
  await login(page)
  const name = `compat-trash-return-${crypto.randomUUID()}`
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
    await page.locator('.file-card, .file-row').filter({ hasText: name }).click()
    await expect(page.getByRole('heading', { name, exact: true })).toBeVisible()

    await page.getByTitle('回收站').first().click()
    await expect(page.getByRole('heading', { name: '回收站', exact: true })).toBeVisible()

    await page.locator('.category-main[data-category="file"]').click()
    await expect(page.getByRole('heading', { name, exact: true })).toBeVisible()
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

test('桌面侧栏折叠与分类路径手风琴在刷新后保持 reference 状态', async ({ page }) => {
  await page.addInitScript(() => {
    if (!sessionStorage.getItem('compat-sidebar-reset')) {
      localStorage.removeItem('revaro:sidebar:collapsed')
      localStorage.removeItem('revaro:sidebar:expanded')
      sessionStorage.setItem('compat-sidebar-reset', '1')
    }
  })
  await login(page)
  await expect(page.getByRole('heading', { name: '我的文件', exact: true })).toBeVisible()

  const sidebar = page.locator('.app-sidebar')
  const collapse = page.locator('.sidebar-collapse')
  await expect(sidebar).not.toHaveClass(/collapsed/)
  await expect(collapse).toHaveAttribute('aria-expanded', 'true')

  await collapse.click()
  await expect(sidebar).toHaveClass(/collapsed/)
  await expect(collapse).toHaveAttribute('aria-expanded', 'false')
  await page.reload()
  await expect(page.getByRole('heading', { name: '我的文件', exact: true })).toBeVisible()
  await expect(sidebar).toHaveClass(/collapsed/)
  await collapse.click()
  await expect(sidebar).not.toHaveClass(/collapsed/)

  const book = page.locator('.sidebar-category').filter({ has: page.locator('[data-category="book"]') })
  const bookExpand = book.locator('.category-expand')
  await expect(bookExpand).toHaveAttribute('aria-expanded', 'false')
  await bookExpand.click()
  await expect(bookExpand).toHaveAttribute('aria-expanded', 'true')
  await expect(book.locator('.category-paths')).toBeVisible()
  await page.reload()
  await expect(page.getByRole('heading', { name: '我的文件', exact: true })).toBeVisible()
  await expect(page.locator('.sidebar-category').filter({ has: page.locator('[data-category="book"]') }).locator('.category-paths')).toBeVisible()
})

test('移动端分类抽屉隐藏桌面折叠与路径展开控件，并支持遮罩关闭', async ({ page }) => {
  await page.addInitScript(() => {
    if (!sessionStorage.getItem('compat-mobile-sidebar-reset')) {
      localStorage.removeItem('revaro:sidebar:collapsed')
      localStorage.removeItem('revaro:sidebar:expanded')
      sessionStorage.setItem('compat-mobile-sidebar-reset', '1')
    }
  })
  await page.setViewportSize({ width: 390, height: 844 })
  await login(page)
  await expect(page.getByRole('heading', { name: '我的文件', exact: true })).toBeVisible()

  await expect(page.locator('.sidebar-collapse')).toBeHidden()
  await expect(page.locator('.category-expand')).toHaveCount(0)
  await expect(page.locator('.sidebar-handle')).toHaveAttribute('aria-expanded', 'false')
  await page.locator('.sidebar-handle').click()
  await expect(page.locator('.app-sidebar')).toHaveClass(/mobile-open/)
  await expect(page.locator('.sidebar-backdrop')).toHaveClass(/open/)
  await expect(page.locator('.category-label')).toHaveCount(6)
  await page.locator('.sidebar-backdrop').click({ position: { x: 380, y: 400 } })
  await expect(page.locator('.app-sidebar')).not.toHaveClass(/mobile-open/)
})

import { expect, test, type Page } from '@playwright/test'

const ROOT = '00000000-0000-0000-0000-000000000000'
const FOLDER = 'file-card-interaction-folder'
const NOTE = 'file-card-interaction-note'
const STAMP = '2026-01-01T00:00:00Z'

const root = {
  id: ROOT,
  parent_id: null,
  name: '我的文件',
  kind: 'directory',
  size: 0,
  status: 'ready',
  created_at: STAMP,
  updated_at: STAMP,
  mime_type: '',
}

const folder = {
  id: FOLDER,
  parent_id: ROOT,
  name: '交互目录',
  kind: 'directory',
  size: 0,
  status: 'ready',
  created_at: STAMP,
  updated_at: STAMP,
  mime_type: '',
}

const note = {
  id: NOTE,
  parent_id: ROOT,
  name: '交互笔记.txt',
  kind: 'file',
  size: 1024,
  status: 'ready',
  created_at: STAMP,
  updated_at: STAMP,
  mime_type: 'text/plain',
  etag: 'file-card-interaction-note-etag',
  has_cover: false,
}

const image = {
  id: 'file-card-interaction-image',
  parent_id: ROOT,
  name: '交互图片.png',
  kind: 'file',
  size: 2048,
  status: 'ready',
  created_at: STAMP,
  updated_at: STAMP,
  mime_type: 'image/png',
  etag: 'file-card-interaction-image-etag',
  has_cover: false,
}

const items = [folder, note, image]
const cover = '<svg xmlns="http://www.w3.org/2000/svg" width="40" height="40"><rect width="40" height="40" fill="#789"/></svg>'

async function mockBrowser(page: Page) {
  await page.route('**/api/**', async route => {
    const path = new URL(route.request().url()).pathname
    const json = (value: unknown) => route.fulfill({ json: value })
    if (path === '/api/auth/me') return json({ username: 'admin', has_avatar: false })
    if (path === '/api/events' || path === '/api/system/status/stream') {
      return route.fulfill({ contentType: 'text/event-stream', body: '' })
    }
    if (path === '/api/tasks') return json({ items: [] })
    if (path === '/api/library/all') {
      return json({ items: { book: [], image: [], video: [], audio: [] }, counts: { book: 0, image: 0, video: 0, audio: 0, file: items.length } })
    }
    if (path === '/api/library/counts') return json({ book: 0, image: 0, video: 0, audio: 0, file: items.length })
    if (path === `/api/files/${ROOT}`) return json({ file: root, breadcrumbs: [] })
    if (path === `/api/files/${ROOT}/children`) return json({ items, total_bytes: 3072, file_count: items.length })
    if (path === `/api/files/${FOLDER}`) return json({ file: folder, breadcrumbs: [root] })
    if (path === `/api/files/${FOLDER}/children`) return json({ items: [], total_bytes: 0, file_count: 0 })
    if (path === `/api/files/${NOTE}/content`) return json({ content: '交互测试内容', etag: note.etag })
    if (path.endsWith('/thumbnail')) return route.fulfill({ contentType: 'image/svg+xml', body: cover })
    return json({ items: [] })
  })
}

async function openBrowser(page: Page, baseUrl: string) {
  await page.goto(`${baseUrl}/?file-card-interaction=${crypto.randomUUID()}`)
  await expect(page.getByRole('heading', { name: '我的文件', exact: true })).toBeVisible()
  await expect(page.locator('.file-card')).toHaveCount(items.length)
}

async function defaultPreventedAfterContextMenu(page: Page, selector: string) {
  return page.locator(selector).first().evaluate(element => {
    let prevented = false
    element.addEventListener('contextmenu', event => { prevented = event.defaultPrevented })
    element.dispatchEvent(new MouseEvent('contextmenu', { bubbles: true, cancelable: true, button: 2 }))
    return prevented
  })
}

async function defaultPreventedAfterKey(page: Page, selector: string, key: string) {
  return page.locator(selector).first().evaluate((element, key) => {
    let prevented = false
    element.addEventListener('keydown', event => { prevented = event.defaultPrevented })
    element.dispatchEvent(new KeyboardEvent('keydown', { bubbles: true, cancelable: true, key }))
    return prevented
  }, key)
}

async function interactionSnapshot(page: Page) {
  return page.evaluate(() => ({
    path: location.pathname,
    heading: document.querySelector('.content-head h1')?.textContent?.trim() ?? null,
    toolbar: document.querySelector('.selection-toolbar')?.textContent?.replace(/\s+/g, ' ').trim() ?? null,
    editor: document.querySelector('.modal-backdrop.editing') !== null,
  }))
}

test('桌面文件卡/列表行的右键、键盘、选择和目录进入保持 reference', async ({ browser }) => {
  const oldUrl = process.env.E2E_REFERENCE_URL || 'http://127.0.0.1:18080'
  const newUrl = process.env.E2E_NEW_URL || 'http://127.0.0.1:18084'
  const oldContext = await browser.newContext({ viewport: { width: 1440, height: 900 } })
  const newContext = await browser.newContext({ viewport: { width: 1440, height: 900 } })
  const oldPage = await oldContext.newPage()
  const newPage = await newContext.newPage()

  try {
    await Promise.all([mockBrowser(oldPage), mockBrowser(newPage)])
    await Promise.all([openBrowser(oldPage, oldUrl), openBrowser(newPage, newUrl)])

    const oldFolder = oldPage.locator('.file-card').filter({ hasText: folder.name })
    const newFolder = newPage.locator('.file-card').filter({ hasText: folder.name })
    expect(await defaultPreventedAfterContextMenu(oldPage, '.file-card'), 'reference 文件卡应阻止原生右键菜单').toBe(true)
    expect(await defaultPreventedAfterContextMenu(newPage, '.file-card'), 'Rust 文件卡未阻止原生右键菜单').toBe(true)
    // The reference grid has no selection control, so Space there only guards
    // scrolling; the Rust grid selects through the card control instead.
    expect(await defaultPreventedAfterKey(oldPage, '.file-card', ' '), 'reference 文件卡 Space 应阻止页面滚动').toBe(true)
    expect(await oldPage.locator('.selection-toolbar').count()).toBe(0)

    await Promise.all([oldFolder.focus(), newFolder.focus()])
    await Promise.all([oldPage.keyboard.press('Enter'), newPage.keyboard.press('Enter')])
    await Promise.all([
      expect(oldPage.getByRole('heading', { name: folder.name, exact: true })).toBeVisible(),
      expect(newPage.getByRole('heading', { name: folder.name, exact: true })).toBeVisible(),
    ])
    expect(await interactionSnapshot(newPage), 'Rust 文件卡 Enter 进入目录结果与 reference 不一致').toEqual(await interactionSnapshot(oldPage))

    await Promise.all([oldPage.goBack(), newPage.goBack()])
    await Promise.all([
      expect(oldPage.getByRole('heading', { name: '我的文件', exact: true })).toBeVisible(),
      expect(newPage.getByRole('heading', { name: '我的文件', exact: true })).toBeVisible(),
    ])

    // Selection now has different entry points: the reference switches to its
    // list view and uses the row control, while the Rust grid selects directly.
    await oldPage.getByTitle('列表视图').click()
    await expect(oldPage.locator('.file-row')).toHaveCount(items.length)

    const oldRow = oldPage.locator('.file-row').filter({ hasText: note.name })
    const newCard = newPage.locator('.file-card').filter({ hasText: note.name })
    expect(await defaultPreventedAfterContextMenu(oldPage, '.file-row'), 'reference 列表行应阻止原生右键菜单').toBe(true)
    expect(await defaultPreventedAfterContextMenu(newPage, '.file-card'), 'Rust 方块卡未阻止原生右键菜单').toBe(true)
    await Promise.all([oldRow.focus(), newCard.focus()])
    expect(await defaultPreventedAfterKey(oldPage, '.file-row', ' '), 'reference 列表行 Space 应阻止默认行为').toBe(true)
    expect(await defaultPreventedAfterKey(newPage, '.file-card', ' '), 'Rust 方块卡 Space 未阻止默认行为').toBe(true)
    await Promise.all([
      expect(oldPage.locator('.selection-toolbar')).toContainText('1 项'),
      expect(newPage.locator('.selection-toolbar')).toContainText('1 项'),
    ])
    expect(await interactionSnapshot(newPage), 'Rust Space 选择结果与 reference 不一致').toEqual(await interactionSnapshot(oldPage))
  } finally {
    await Promise.all([oldContext.close(), newContext.close()])
  }
})

test('移动端选择模式轻触选择控件只切换选择，不打开 editor', async ({ browser }) => {
  const oldUrl = process.env.E2E_REFERENCE_URL || 'http://127.0.0.1:18080'
  const newUrl = process.env.E2E_NEW_URL || 'http://127.0.0.1:18084'
  const oldContext = await browser.newContext({ viewport: { width: 390, height: 844 }, isMobile: true, hasTouch: true })
  const newContext = await browser.newContext({ viewport: { width: 390, height: 844 }, isMobile: true, hasTouch: true })
  const oldPage = await oldContext.newPage()
  const newPage = await newContext.newPage()

  try {
    await Promise.all([mockBrowser(oldPage), mockBrowser(newPage)])
    await Promise.all([openBrowser(oldPage, oldUrl), openBrowser(newPage, newUrl)])
    // The reference reaches selection through the list view; the Rust grid has
    // a select control on every card.
    await oldPage.getByTitle('列表视图').click()
    await expect(oldPage.locator('.file-row')).toHaveCount(items.length)
    const oldRow = oldPage.locator('.file-row').filter({ hasText: note.name })
    const newCard = newPage.locator('.file-card').filter({ hasText: note.name })
    await Promise.all([
      oldRow.getByRole('button', { name: '选择项目' }).click(),
      newCard.getByRole('button', { name: '选择项目' }).click(),
    ])
    await Promise.all([
      expect(oldPage.locator('.selection-toolbar')).toContainText('1 项'),
      expect(newPage.locator('.selection-toolbar')).toContainText('1 项'),
    ])

    // While selection mode is active, tapping the select affordance toggles the
    // item back off without opening the editor.
    await Promise.all([
      oldRow.locator('.row-info').tap(),
      newCard.getByRole('button', { name: '取消选择' }).tap(),
    ])
    await Promise.all([
      expect(oldPage.locator('.selection-toolbar')).toHaveCount(0),
      expect(newPage.locator('.selection-toolbar')).toHaveCount(0),
      expect(oldPage.locator('.modal-backdrop.editing')).toHaveCount(0),
      expect(newPage.locator('.modal-backdrop.editing')).toHaveCount(0),
    ])
    expect(await interactionSnapshot(newPage), 'Rust 移动端选择模式切换结果与 reference 不一致').toEqual(await interactionSnapshot(oldPage))
  } finally {
    await Promise.all([oldContext.close(), newContext.close()])
  }
})

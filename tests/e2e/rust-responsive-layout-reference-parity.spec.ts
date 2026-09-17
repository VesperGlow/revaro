import { expect, test, type Page } from '@playwright/test'

const ROOT = '00000000-0000-0000-0000-000000000000'
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

const items = [
  {
    id: 'responsive-folder',
    parent_id: ROOT,
    name: '响应式目录',
    kind: 'directory',
    size: 0,
    status: 'ready',
    created_at: STAMP,
    updated_at: STAMP,
    mime_type: '',
  },
  {
    id: 'responsive-note',
    parent_id: ROOT,
    name: '响应式说明.txt',
    kind: 'file',
    size: 2048,
    status: 'ready',
    created_at: STAMP,
    updated_at: STAMP,
    mime_type: 'text/plain',
    etag: 'responsive-etag',
  },
]

async function mockShell(page: Page) {
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
        counts: { book: 0, image: 0, video: 0, audio: 0, file: items.length },
      })
    }
    if (path === '/api/library/counts') {
      return json({ book: 0, image: 0, video: 0, audio: 0, file: items.length })
    }
    if (path === '/api/trash') return json({ items: [], total_bytes: 0, file_count: 0 })
    if (path === `/api/files/${ROOT}`) return json({ file: root, breadcrumbs: [] })
    if (path === `/api/files/${ROOT}/children`) {
      return json({ items, total_bytes: 2048, file_count: 1 })
    }
    return json({ items: [] })
  })
}

async function openShell(page: Page, baseUrl: string) {
  await page.goto(`${baseUrl}/?responsive-reference=${crypto.randomUUID()}`)
  await expect(page.getByRole('heading', { name: '我的文件', exact: true })).toBeVisible()
  await expect(page.locator('.file-card')).toHaveCount(items.length)
}

async function layoutSnapshot(page: Page) {
  return page.evaluate(() => {
    const rect = (selector: string) => {
      const element = document.querySelector<HTMLElement>(selector)
      if (!element) return null
      const box = element.getBoundingClientRect()
      return { x: box.x, y: box.y, width: box.width, height: box.height }
    }
    const style = (selector: string, property: string) => {
      const element = document.querySelector<HTMLElement>(selector)
      return element ? getComputedStyle(element).getPropertyValue(property) : null
    }
    const visible = (selector: string) => {
      const element = document.querySelector<HTMLElement>(selector)
      if (!element) return false
      const computed = getComputedStyle(element)
      return computed.display !== 'none' && computed.visibility !== 'hidden'
    }
    return {
      viewport: { width: window.innerWidth, height: window.innerHeight },
      body: { clientWidth: document.body.clientWidth, scrollWidth: document.body.scrollWidth },
      shell: rect('.app-shell'),
      shellColumns: style('.app-shell', 'grid-template-columns'),
      topbar: rect('.topbar'),
      content: rect('.content'),
      contentHead: rect('.content-head'),
      fileGrid: rect('.file-grid'),
      gridColumns: style('.file-grid', 'grid-template-columns'),
      desktopCreateActions: visible('.desktop-create-actions'),
      createMenu: visible('.create-menu'),
      uploadMenu: visible('.upload-menu'),
      mobileToolsToggle: visible('summary[aria-label="打开账户与工具菜单"]'),
      cardCount: document.querySelectorAll('.file-card').length,
    }
  })
}

// 产品决定：Rust 前端移除侧边分类栏，壳层回到顶栏 + 全宽内容区。因此桌面宽度
// 下不再与带侧栏的 reference 比较整页几何；这里改为固定“无侧栏、全宽、不溢出”
// 这一新版不变量。对话框几何仍按下面一条用例与 reference 对照。
test('六个 viewport 断点的无侧栏全宽壳层不溢出', async ({ browser }) => {
  const newUrl = process.env.E2E_NEW_URL || 'http://127.0.0.1:18084'
  const viewports = [
    { width: 1440, height: 900 },
    { width: 1024, height: 900 },
    { width: 851, height: 900 },
    { width: 850, height: 900 },
    { width: 390, height: 844 },
    { width: 320, height: 700 },
  ]

  for (const viewport of viewports) {
    const newContext = await browser.newContext({ viewport })
    const newPage = await newContext.newPage()
    try {
      await mockShell(newPage)
      await openShell(newPage, newUrl)
      const layout = await layoutSnapshot(newPage)
      const desktop = viewport.width > 850
      expect(await newPage.locator('.app-sidebar').count(), `${viewport.width}px 不应再有侧栏`).toBe(0)
      expect(layout.shellColumns, `${viewport.width}px 壳层应为单列全宽`).toBe(`${viewport.width}px`)
      expect(layout.content?.x).toBe(0)
      expect(layout.content?.width).toBe(viewport.width)
      expect(layout.body.scrollWidth, `Rust ${viewport.width}px 出现横向页面溢出`).toBeLessThanOrEqual(viewport.width)
      expect(layout.cardCount).toBe(items.length)
      expect(layout.desktopCreateActions, `${viewport.width}px 桌面入口可见性`).toBe(desktop)
      expect(layout.mobileToolsToggle, `${viewport.width}px 移动工具入口可见性`).toBe(!desktop)
    } finally {
      await newContext.close()
    }
  }
})

async function dialogSnapshot(page: Page) {
  return page.locator('.dialog-backdrop').evaluate(backdrop => {
    const rect = (selector: string) => {
      const element = backdrop.querySelector<HTMLElement>(selector)
      if (!element) return null
      const box = element.getBoundingClientRect()
      return {
        x: Math.round(box.x * 100) / 100,
        y: Math.round(box.y * 100) / 100,
        width: Math.round(box.width * 100) / 100,
        height: Math.round(box.height * 100) / 100,
      }
    }
    const style = (selector: string, property: string) => {
      const element = backdrop.querySelector<HTMLElement>(selector)
      return element ? getComputedStyle(element).getPropertyValue(property) : null
    }
    return {
      backdrop: (() => {
        const box = backdrop.getBoundingClientRect()
        return {
          x: Math.round(box.x * 100) / 100,
          y: Math.round(box.y * 100) / 100,
          width: Math.round(box.width * 100) / 100,
          height: Math.round(box.height * 100) / 100,
        }
      })(),
      modal: rect('.app-dialog'),
      icon: rect('.dialog-icon'),
      copy: rect('.dialog-copy'),
      input: rect('.app-dialog input'),
      footer: rect('.app-dialog footer'),
      modalDisplay: style('.app-dialog', 'display'),
      modalPadding: style('.app-dialog', 'padding'),
      footerGap: style('.app-dialog footer', 'gap'),
      buttonHeight: style('.app-dialog footer button', 'min-height'),
    }
  })
}

async function selectionSnapshot(page: Page) {
  return page.locator('.selection-toolbar').evaluate(toolbar => {
    const rect = (element: Element | null) => {
      if (!(element instanceof HTMLElement)) return null
      const box = element.getBoundingClientRect()
      return {
        x: Math.round(box.x * 100) / 100,
        y: Math.round(box.y * 100) / 100,
        width: Math.round(box.width * 100) / 100,
        height: Math.round(box.height * 100) / 100,
      }
    }
    const style = (element: Element | null, property: string) =>
      element instanceof HTMLElement ? getComputedStyle(element).getPropertyValue(property) : null
    const buttons = [...toolbar.querySelectorAll<HTMLButtonElement>('.selection-actions button')]
    return {
      toolbar: rect(toolbar),
      summary: rect(toolbar.querySelector('.selection-summary')),
      actions: rect(toolbar.querySelector('.selection-actions')),
      actionCount: buttons.length,
      actionRects: buttons.map(button => rect(button)),
      actionFlexDirection: style(buttons[0], 'flex-direction'),
      actionOverflow: style(toolbar.querySelector('.selection-actions'), 'overflow-x'),
    }
  })
}

test('对话框和选择工具栏在桌面/断点/手机宽度保持 reference 几何', async ({ browser }) => {
  const oldUrl = process.env.E2E_REFERENCE_URL || 'http://127.0.0.1:18080'
  const newUrl = process.env.E2E_NEW_URL || 'http://127.0.0.1:18084'
  const viewports = [
    { width: 1440, height: 900 },
    { width: 851, height: 900 },
    { width: 850, height: 900 },
    { width: 390, height: 844 },
  ]

  for (const viewport of viewports) {
    const oldContext = await browser.newContext({ viewport })
    const newContext = await browser.newContext({ viewport })
    const oldPage = await oldContext.newPage()
    const newPage = await newContext.newPage()
    try {
      await Promise.all([mockShell(oldPage), mockShell(newPage)])
      await Promise.all([openShell(oldPage, oldUrl), openShell(newPage, newUrl)])

      const openCreateFolder = async (page: Page) => {
        const desktopButton = page.locator('.desktop-create-actions button').filter({ hasText: '新建文件夹' })
        if (await desktopButton.isVisible()) {
          await desktopButton.click()
        } else {
          await page.locator('.create-menu summary').click()
          await page.locator('.create-menu-popover').getByRole('button', { name: '新建文件夹' }).click()
        }
        await expect(page.locator('.dialog-backdrop .app-dialog')).toBeVisible()
      }
      await Promise.all([openCreateFolder(oldPage), openCreateFolder(newPage)])
      expect(await dialogSnapshot(newPage), `Rust ${viewport.width}px 对话框几何与 reference 不一致`)
        .toEqual(await dialogSnapshot(oldPage))

      await Promise.all([
        oldPage.locator('.dialog-backdrop .app-dialog button.secondary').click(),
        newPage.locator('.dialog-backdrop .app-dialog button.secondary').click(),
      ])
      await oldPage.getByTitle('列表视图').click()
      await Promise.all([
        oldPage.locator('.file-row').getByRole('button', { name: '选择项目' }).first().click(),
        newPage.locator('.file-card').getByRole('button', { name: '选择项目' }).first().click(),
      ])
      const newSelection = await selectionSnapshot(newPage)
      if (viewport.width <= 850) {
        expect(newSelection, `Rust ${viewport.width}px 选择工具栏几何与 reference 不一致`)
          .toEqual(await selectionSnapshot(oldPage))
      } else {
        // 桌面宽度下 reference 的内容区被 236px 侧栏挤窄，新版是全宽单列；选择
        // 工具栏几何不再逐像素对照，只固定与侧栏无关的动作集合与溢出行为。
        const oldSelection = await selectionSnapshot(oldPage)
        expect(newSelection.actionCount).toBe(oldSelection.actionCount)
        expect(newSelection.actionCount).toBeGreaterThan(0)
        expect(newSelection.actionOverflow).toBe(oldSelection.actionOverflow)
        expect(newSelection.actionFlexDirection).toBe(oldSelection.actionFlexDirection)
      }
    } finally {
      await Promise.all([oldContext.close(), newContext.close()])
    }
  }
})

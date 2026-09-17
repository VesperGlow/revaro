import { expect, test, type Page } from '@playwright/test'

const ROOT = '00000000-0000-0000-0000-000000000000'
const STAMP = '2026-01-01T00:00:00Z'

const files = [
  { id: 'focus-folder', parent_id: ROOT, name: '焦点目录', kind: 'directory', size: 0, status: 'ready', created_at: STAMP, updated_at: STAMP, mime_type: '' },
  { id: 'focus-text', parent_id: ROOT, name: '焦点文档.txt', kind: 'file', size: 100, status: 'ready', created_at: STAMP, updated_at: STAMP, mime_type: 'text/plain' },
]

const status = {
  status: 'ok',
  database: { status: 'ok', bytes: 1024 },
  storage: { status: 'ok', bytes: 2048, trash_bytes: 0, file_count: files.length },
  cache: { status: 'ok', memory_bytes: 1024, disk_bytes: 2048, memory_entries: 1, disk_entries: 1, classes: {} },
}

async function mockShell(page: Page) {
  await page.route('**/api/**', async route => {
    const path = new URL(route.request().url()).pathname
    const json = (value: unknown) => route.fulfill({ json: value })
    if (path === '/api/auth/me') return json({ username: 'admin', has_avatar: false })
    if (path === '/api/events') return route.fulfill({ contentType: 'text/event-stream', body: '' })
    if (path === '/api/system/status/stream') {
      return route.fulfill({ contentType: 'text/event-stream', body: `event: status\ndata: ${JSON.stringify(status)}\n\n` })
    }
    if (path === '/api/tasks') return json({ items: [] })
    if (path === '/api/library/all') {
      return json({ items: { book: [], image: [], video: [], audio: [] }, counts: { book: 0, image: 0, video: 0, audio: 0, file: files.length } })
    }
    if (path === '/api/library/counts') return json({ book: 0, image: 0, video: 0, audio: 0, file: files.length })
    if (path === `/api/files/${ROOT}`) {
      return json({
        file: { id: ROOT, parent_id: null, name: '我的文件', kind: 'directory', size: 0, status: 'ready', created_at: STAMP, updated_at: STAMP, mime_type: '' },
        breadcrumbs: [],
      })
    }
    if (path === `/api/files/${ROOT}/children`) return json({ items: files, total_bytes: 100, file_count: 1 })
    return json({ items: [] })
  })
}

async function openShell(page: Page, baseUrl: string) {
  await page.goto(`${baseUrl}/`)
  await expect(page.getByRole('heading', { name: '我的文件', exact: true })).toBeVisible()
  await expect(page.locator('.file-card')).toHaveCount(files.length)
  await page.waitForTimeout(180)
}

async function activeDescriptor(page: Page) {
  return page.evaluate(() => {
    const element = document.activeElement
    if (!element || element === document.body) return 'BODY'
    const style = getComputedStyle(element)
    const rect = element.getBoundingClientRect()
    if (style.display === 'none' || style.visibility === 'hidden' || rect.width === 0 || rect.height === 0) return 'HIDDEN'
    const text = (element.textContent ?? '').replace(/\s+/g, ' ').trim().slice(0, 60)
    return {
      tag: element.tagName.toLowerCase(),
      title: element.getAttribute('title'),
      text,
      className: Array.from(element.classList).sort(),
    }
  })
}

async function tabSequence(page: Page, count: number) {
  const sequence = []
  for (let index = 0; index < count; index += 1) {
    await page.keyboard.press('Tab')
    sequence.push(await activeDescriptor(page))
  }
  return sequence
}

// 产品决定：Rust 前端移除了侧边分类栏和列表视图，网格成为唯一布局，并在每张
// 方块卡片上新增了左上角选择按钮。桌面 Tab 顺序因此与带侧栏的 reference 有意
// 不同，本用例改为固定新版顺序：顶栏 → 内容头动作 → 每张卡片（卡片本身 →
// 左上角选择按钮），并验证顺序稳定、没有隐藏或 body 焦点。
test('桌面 Tab 焦点顺序符合无侧栏方块网格', async ({ browser }) => {
  const newUrl = process.env.E2E_NEW_URL || 'http://127.0.0.1:18084'
  const newContext = await browser.newContext({ viewport: { width: 1440, height: 900 } })
  const newPage = await newContext.newPage()

  try {
    await mockShell(newPage)
    await openShell(newPage, newUrl)
    await newPage.evaluate(() => (document.activeElement as HTMLElement | null)?.blur())

    const stopsPerShell = 9
    const stopCount = stopsPerShell + files.length * 2
    const sequence = await tabSequence(newPage, stopCount)
    expect(sequence).not.toContain('HIDDEN')
    expect(sequence).not.toContain('BODY')

    const label = (entry: unknown) => {
      if (typeof entry === 'string') return entry
      const descriptor = entry as { title?: string | null; text?: string }
      return descriptor.title ?? descriptor.text ?? ''
    }
    const expectedLabels = [
      '回到我的文件',
      '任务中心',
      '系统状态',
      '回收站',
      '打开账户设置',
      '我的文件',
      '新建文档',
      '新建文件夹',
      '上传',
    ]
    expect(sequence.slice(0, stopsPerShell).map(label)).toEqual(expectedLabels)

    for (let index = 0; index < files.length; index += 1) {
      const card = sequence[stopsPerShell + index * 2] as {
        tag: string
        className: string[]
      }
      const select = sequence[stopsPerShell + index * 2 + 1] as {
        tag: string
        title: string | null
        className: string[]
      }
      expect(card.tag, `第 ${index + 1} 张卡片本身应可聚焦`).toBe('article')
      expect(card.className).toContain('file-card')
      expect(select.tag, `第 ${index + 1} 张卡片的选择控件应可聚焦`).toBe('button')
      expect(select.className).toContain('card-select')
      expect(select.title).toBe('选择项目')
    }

    // The order must be stable across a fresh mount, not incidental to the
    // first pass: reload resets the browser's sequential-focus start point.
    await newPage.reload()
    await expect(newPage.locator('.file-card')).toHaveCount(files.length)
    await newPage.waitForTimeout(180)
    await newPage.evaluate(() => (document.activeElement as HTMLElement | null)?.blur())
    const secondPass = await tabSequence(newPage, stopCount)
    expect(secondPass).toEqual(sequence)
  } finally {
    await newContext.close()
  }
})

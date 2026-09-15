import { expect, test, type Page } from '@playwright/test'

const ROOT = '00000000-0000-0000-0000-000000000000'
const STAMP = '2026-01-01T00:00:00Z'

const task = (value: Record<string, unknown>) => ({
  id: 'task-default',
  type: 'upload',
  status: 'completed',
  phase: 'completed',
  progress: 100,
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
  name: '已完成任务',
  ...value,
})

const tasks = [
  task({ id: 'active-task', status: 'running', phase: '上传中', progress: 37.5, speed: 2048, name: '活动文件.bin' }),
  task({ id: 'waiting-task', type: 'archive_extract', status: 'waiting_input', phase: 'waiting_input', progress: 42, finished_at: null, name: '需要密码.zip' }),
  ...Array.from({ length: 5 }, (_, index) => task({ id: `completed-${index}`, name: `完成 ${index + 1}.txt` })),
  task({ id: 'failed-task', status: 'failed', phase: 'failed', progress: 100, retry_count: 0, max_retries: 2, error: '解压失败', name: '失败任务.zip' }),
  task({ id: 'maxed-task', status: 'failed', phase: 'failed', progress: 100, retry_count: 3, max_retries: 3, error: '不可重试', name: '不可重试.zip' }),
]

async function mockTasks(page: Page) {
  await page.route('**/api/**', async route => {
    const path = new URL(route.request().url()).pathname
    const json = (value: unknown) => route.fulfill({ json: value })
    if (path === '/api/auth/me') return json({ username: 'admin', has_avatar: false })
    if (path === '/api/events' || path === '/api/system/status/stream') {
      return route.fulfill({ contentType: 'text/event-stream', body: '' })
    }
    if (path === '/api/tasks') return json({ items: tasks })
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
}

async function openBrowser(page: Page, baseUrl: string) {
  await page.goto(`${baseUrl}/?task-center-reference=${crypto.randomUUID()}`)
  await expect(page.getByRole('heading', { name: '我的文件', exact: true })).toBeVisible()
  await expect(page.getByTitle('任务中心')).toBeVisible()
}

async function snapshot(page: Page) {
  return page.evaluate(() => {
    const details = document.querySelector<HTMLDetailsElement>('.task-center')
    const panel = document.querySelector('.task-panel')
    const group = (selector: string) => Array.from(document.querySelectorAll(`${selector} article`)).map(article => ({
      text: article.textContent?.replace(/\s+/g, ' ').trim() ?? '',
      progress: article.querySelector('b')?.getAttribute('style')?.replace(/;$/, '') ?? null,
      actions: Array.from(article.querySelectorAll('button')).map(button => button.getAttribute('title')),
    }))
    return {
      open: details?.open ?? false,
      header: panel?.querySelector('header')?.textContent?.replace(/\s+/g, ' ').trim() ?? null,
      active: group('.active-group'),
      completed: group('.completed-group'),
      failed: group('.failed-group'),
      expand: panel?.querySelector('.expand')?.textContent?.replace(/\s+/g, ' ').trim() ?? null,
    }
  })
}

test('任务中心分组、进度、展开、密码弹窗、Escape 与空白关闭保持 reference', async ({ browser }) => {
  const oldUrl = process.env.E2E_REFERENCE_URL || 'http://127.0.0.1:18080'
  const newUrl = process.env.E2E_NEW_URL || 'http://127.0.0.1:18084'
  const oldContext = await browser.newContext({ viewport: { width: 1440, height: 900 } })
  const newContext = await browser.newContext({ viewport: { width: 1440, height: 900 } })
  const oldPage = await oldContext.newPage()
  const newPage = await newContext.newPage()

  try {
    await Promise.all([mockTasks(oldPage), mockTasks(newPage)])
    await Promise.all([openBrowser(oldPage, oldUrl), openBrowser(newPage, newUrl)])
    await Promise.all([oldPage.getByTitle('任务中心').click(), newPage.getByTitle('任务中心').click()])
    const oldPanel = oldPage.locator('.task-panel')
    const newPanel = newPage.locator('.task-panel')
    await Promise.all([expect(oldPanel).toBeVisible(), expect(newPanel).toBeVisible()])
    expect(await snapshot(newPage), 'Rust 任务中心初始分组和状态与 reference 不一致').toEqual(await snapshot(oldPage))
    await expect(oldPanel.locator('.completed-group article')).toHaveCount(4)
    await expect(newPanel.locator('.completed-group article')).toHaveCount(4)

    await Promise.all([
      oldPanel.getByRole('button', { name: /展开其余 1 项/ }).click(),
      newPanel.getByRole('button', { name: /展开其余 1 项/ }).click(),
    ])
    await Promise.all([
      expect(oldPanel.locator('.completed-group article')).toHaveCount(5),
      expect(newPanel.locator('.completed-group article')).toHaveCount(5),
    ])
    expect(await snapshot(newPage), 'Rust 任务中心展开完成任务与 reference 不一致').toEqual(await snapshot(oldPage))

    await Promise.all([
      oldPanel.locator('.active-group article').filter({ hasText: '需要密码.zip' }).click(),
      newPanel.locator('.active-group article').filter({ hasText: '需要密码.zip' }).click(),
    ])
    await Promise.all([
      expect(oldPage.locator('.input-dialog')).toBeVisible(),
      expect(newPage.locator('.input-dialog')).toBeVisible(),
    ])
    const dialogSnapshot = async (page: Page) => page.locator('.input-dialog').evaluate(dialog => ({
      text: dialog.textContent?.replace(/\s+/g, ' ').trim() ?? '',
      inputType: dialog.querySelector('input')?.getAttribute('type') ?? null,
      maxLength: dialog.querySelector('input')?.getAttribute('maxlength') ?? null,
      continueDisabled: dialog.querySelector('button:last-child')?.hasAttribute('disabled') ?? false,
    }))
    expect(await dialogSnapshot(newPage), 'Rust 任务密码弹窗与 reference 不一致').toEqual(await dialogSnapshot(oldPage))

    await Promise.all([oldPage.keyboard.press('Escape'), newPage.keyboard.press('Escape')])
    await Promise.all([
      expect(oldPage.locator('.input-dialog')).toHaveCount(0),
      expect(newPage.locator('.input-dialog')).toHaveCount(0),
    ])
    const activeElement = async (page: Page) => page.evaluate(() => {
      const element = document.activeElement
      return element ? {
        tag: element.tagName,
        className: element.getAttribute('class'),
        title: element.getAttribute('title'),
        type: element.getAttribute('type'),
      } : null
    })
    expect(await activeElement(newPage), 'Rust 密码弹窗 Escape 后焦点与 reference 不一致').toEqual(await activeElement(oldPage))
    expect(await snapshot(newPage), 'Rust 任务中心 Escape 关闭/焦点恢复与 reference 不一致').toEqual(await snapshot(oldPage))

    await Promise.all([oldPage.getByTitle('任务中心').click(), newPage.getByTitle('任务中心').click()])
    await Promise.all([oldPage.mouse.click(2, 2), newPage.mouse.click(2, 2)])
    await Promise.all([expect(oldPanel).toBeHidden(), expect(newPanel).toBeHidden()])
  } finally {
    await Promise.all([oldContext.close(), newContext.close()])
  }
})

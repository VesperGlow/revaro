import { expect, test, type Page } from '@playwright/test'

const ROOT = '00000000-0000-0000-0000-000000000000'
const STAMP = '2026-01-01T00:00:00Z'

const task = (value: Record<string, unknown>) => ({
  id: `task-${crypto.randomUUID()}`,
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

async function mockTasks(page: Page) {
  let taskRequests = 0
  const tasks = [
    task({ id: 'waiting-archive', type: 'archive_extract', status: 'waiting_input', phase: 'waiting_input', progress: 42, name: '需要密码.zip' }),
    ...Array.from({ length: 5 }, (_, index) => task({ id: `completed-${index}`, name: `已完成 ${index + 1}` })),
    task({ id: 'failed-task', status: 'failed', phase: 'failed', progress: 100, retry_count: 0, max_retries: 2, error: '解压失败', name: '失败任务.zip' }),
    task({ id: 'maxed-task', status: 'failed', phase: 'failed', progress: 100, retry_count: 3, max_retries: 3, error: '重试次数已用尽', name: '不可重试.zip' }),
  ]

  await page.route('**/api/**', async route => {
    const path = new URL(route.request().url()).pathname
    const json = (value: unknown) => route.fulfill({ json: value })
    if (path === '/api/auth/me') return json({ username: 'admin', has_avatar: false })
    if (path === '/api/events' || path === '/api/system/status/stream') {
      return route.fulfill({ contentType: 'text/event-stream', body: '' })
    }
    if (path === '/api/tasks') {
      taskRequests += 1
      return json({ items: tasks })
    }
    if (path === '/api/library/all') {
      return json({ items: { book: [], image: [], video: [], audio: [] }, counts: { book: 0, image: 0, video: 0, audio: 0, file: 0 } })
    }
    if (path === '/api/library/counts') return json({ book: 0, image: 0, video: 0, audio: 0, file: 0 })
    if (path === `/api/files/${ROOT}`) return json({ file: { id: ROOT, parent_id: null, name: '我的文件', kind: 'directory', size: 0, status: 'ready', created_at: STAMP, updated_at: STAMP, mime_type: '' }, breadcrumbs: [] })
    if (path === `/api/files/${ROOT}/children`) return json({ items: [], total_bytes: 0, file_count: 0 })
    return json({ items: [] })
  })
  return { taskRequests: () => taskRequests }
}

test('任务中心保留等待输入、分组、展开和关闭行为', async ({ page }) => {
  await mockTasks(page)
  await page.goto('/')
  await expect(page.getByRole('heading', { name: '我的文件' })).toBeVisible()

  await page.getByTitle('任务中心').click()
  const panel = page.locator('.task-panel')
  await expect(panel).toBeVisible()
  await expect(panel).toContainText('进行中')
  await expect(panel).toContainText('等待输入密码')
  await expect(panel.locator('.active-group article')).toContainText('需要密码.zip')
  await expect(panel.locator('.completed-group article')).toHaveCount(4)
  await expect(panel.locator('.failed-group article').filter({ hasText: '失败任务.zip' })).toContainText('失败任务.zip')
  await expect(panel.locator('.failed-group button[title="重试"]')).toBeVisible()
  await expect(panel.locator('.failed-group article').filter({ hasText: '不可重试.zip' }).getByRole('button', { name: '重试' })).toHaveCount(0)
  await expect(panel.getByRole('button', { name: /展开其余 1 项/ })).toBeVisible()

  await panel.getByRole('button', { name: /展开其余 1 项/ }).click()
  await expect(panel.locator('.completed-group article')).toHaveCount(5)
  await expect(panel.getByRole('button', { name: '收起' })).toBeVisible()

  await panel.locator('.active-group article').click()
  await expect(page.locator('.input-dialog')).toBeVisible()
  await expect(page.locator('.input-dialog')).toContainText('需要密码.zip')
  await page.keyboard.press('Escape')
  await expect(page.locator('.input-dialog')).toHaveCount(0)
  await expect(panel).toBeHidden()

  await page.getByTitle('任务中心').click()
  await expect(panel).toBeVisible()
  await page.getByRole('heading', { name: '我的文件' }).click()
  await expect(panel).toBeHidden()

  await page.getByTitle('任务中心').click()
  await page.keyboard.press('Escape')
  await expect(panel).toBeHidden()
  await expect(page.getByTitle('任务中心')).toBeFocused()
})

test('桌面与移动任务中心切换不重置共享任务流', async ({ page }) => {
  const fixture = await mockTasks(page)
  await page.goto('/')
  await expect(page.getByRole('heading', { name: '我的文件' })).toBeVisible()
  expect(fixture.taskRequests()).toBe(1)

  await page.setViewportSize({ width: 390, height: 844 })
  await page.waitForTimeout(250)
  expect(fixture.taskRequests()).toBe(1)
  await page.locator('summary[aria-label="打开账户与工具菜单"]').click()
  await page.getByRole('button', { name: '任务中心' }).click()
  await expect(page.locator('.task-panel')).toContainText('需要密码.zip')

  await page.setViewportSize({ width: 1440, height: 900 })
  await page.waitForTimeout(250)
  expect(fixture.taskRequests()).toBe(1)
  await page.getByTitle('任务中心').click()
  await expect(page.locator('.task-panel')).toContainText('需要密码.zip')
})

test('任务中心的取消、重试、密码输入和清除完成操作保持可用', async ({ page }) => {
  const state = [
    task({ id: 'active-task', status: 'running', phase: '上传中', progress: 30, name: '活动任务.bin' }),
    task({ id: 'waiting-password', type: 'archive_extract', status: 'waiting_input', phase: 'waiting_input', progress: 42, name: '密码归档.zip' }),
    task({ id: 'failed-action', status: 'failed', phase: 'failed', progress: 100, retry_count: 0, max_retries: 2, error: '解压失败', name: '失败归档.zip' }),
    task({ id: 'completed-action', name: '已完成任务.txt' }),
  ]
  const requests: string[] = []

  await page.route('**/api/**', async route => {
    const request = route.request()
    const url = new URL(request.url())
    const path = url.pathname
    const json = (value: unknown) => route.fulfill({ json: value })
    if (path === '/api/auth/me') return json({ username: 'admin', has_avatar: false })
    if (path === '/api/events' || path === '/api/system/status/stream') {
      return route.fulfill({ contentType: 'text/event-stream', body: '' })
    }
    if (path === '/api/tasks' && request.method() === 'GET') return json({ items: state })
    const action = path.match(/^\/api\/tasks\/([^/]+)\/(cancel|retry|input)$/)
    if (action) {
      requests.push(`${request.method()} ${path}`)
      const item = state.find(value => value.id === action[1])
      if (item && action[2] === 'cancel') Object.assign(item, { status: 'cancelled', phase: 'cancelled' })
      if (item && action[2] === 'retry') Object.assign(item, { status: 'retrying', phase: 'retrying', retry_count: item.retry_count + 1 })
      if (item && action[2] === 'input') Object.assign(item, { status: 'running', phase: '解压中', error: '' })
      return route.fulfill({ status: 204, body: '' })
    }
    const deletion = path.match(/^\/api\/tasks\/([^/]+)$/)
    if (deletion && request.method() === 'DELETE') {
      requests.push(`${request.method()} ${path}`)
      const index = state.findIndex(value => value.id === deletion[1])
      if (index >= 0) state.splice(index, 1)
      return route.fulfill({ status: 204, body: '' })
    }
    if (path === '/api/library/all') return json({ items: { book: [], image: [], video: [], audio: [] }, counts: { book: 0, image: 0, video: 0, audio: 0, file: 0 } })
    if (path === '/api/library/counts') return json({ book: 0, image: 0, video: 0, audio: 0, file: 0 })
    if (path === `/api/files/${ROOT}`) return json({ file: { id: ROOT, parent_id: null, name: '我的文件', kind: 'directory', size: 0, status: 'ready', created_at: STAMP, updated_at: STAMP, mime_type: '' }, breadcrumbs: [] })
    if (path === `/api/files/${ROOT}/children`) return json({ items: [], total_bytes: 0, file_count: 0 })
    return json({ items: [] })
  })

  await page.goto('/')
  await expect(page.getByRole('heading', { name: '我的文件' })).toBeVisible()
  const panel = page.locator('.task-panel')
  await page.getByTitle('任务中心').click()

  const activeRow = panel.locator('.active-group article').filter({ hasText: '活动任务.bin' })
  await activeRow.getByRole('button', { name: '取消' }).click()
  await expect(panel.locator('.completed-group article').filter({ hasText: '活动任务.bin' })).toBeVisible()
  expect(requests).toContain('POST /api/tasks/active-task/cancel')

  const failedRow = panel.locator('.failed-group article').filter({ hasText: '失败归档.zip' })
  await failedRow.getByRole('button', { name: '重试' }).click()
  await expect(panel.locator('.active-group article').filter({ hasText: '失败归档.zip' })).toBeVisible()
  await expect(panel.locator('.active-group article').filter({ hasText: '失败归档.zip' })).toContainText('等待重试')
  expect(requests).toContain('POST /api/tasks/failed-action/retry')

  const waitingRow = panel.locator('.active-group article').filter({ hasText: '密码归档.zip' })
  await waitingRow.click()
  const input = page.locator('.input-dialog')
  await expect(input).toBeVisible()
  await input.locator('input').fill('correct-password')
  await input.getByRole('button', { name: '继续任务' }).click()
  await expect(input).toHaveCount(0)
  await expect(panel.locator('.active-group article').filter({ hasText: '密码归档.zip' })).toContainText('解压中')
  expect(requests).toContain('POST /api/tasks/waiting-password/input')

  await page.getByTitle('任务中心').click()
  await panel.getByRole('button', { name: '清除完成' }).click()
  await expect(panel.locator('.completed-group')).toHaveCount(0)
  expect(requests).toContain('DELETE /api/tasks/completed-action')
  expect(requests).toContain('DELETE /api/tasks/active-task')
})

test('任务中心活动进度汇总按 reference 使用原始值四舍五入', async ({ page }) => {
  const tasks = [
    task({ id: 'progress-one', status: 'running', phase: '上传中', progress: 1, name: '低进度一' }),
    task({ id: 'progress-two', status: 'running', phase: '上传中', progress: 2, name: '低进度二' }),
  ]

  await page.route('**/api/**', async route => {
    const path = new URL(route.request().url()).pathname
    const json = (value: unknown) => route.fulfill({ json: value })
    if (path === '/api/auth/me') return json({ username: 'admin', has_avatar: false })
    if (path === '/api/events' || path === '/api/system/status/stream') {
      return route.fulfill({ contentType: 'text/event-stream', body: '' })
    }
    if (path === '/api/tasks') return json({ items: tasks })
    if (path === '/api/library/all') return json({ items: { book: [], image: [], video: [], audio: [] }, counts: { book: 0, image: 0, video: 0, audio: 0, file: 0 } })
    if (path === '/api/library/counts') return json({ book: 0, image: 0, video: 0, audio: 0, file: 0 })
    if (path === `/api/files/${ROOT}`) return json({ file: { id: ROOT, parent_id: null, name: '我的文件', kind: 'directory', size: 0, status: 'ready', created_at: STAMP, updated_at: STAMP }, breadcrumbs: [] })
    if (path === `/api/files/${ROOT}/children`) return json({ items: [], total_bytes: 0, file_count: 0 })
    return json({ items: [] })
  })

  await page.goto('/')
  await expect(page.getByRole('heading', { name: '我的文件' })).toBeVisible()
  await page.getByTitle('任务中心').click()
  await expect(page.locator('.task-panel > header small')).toHaveText('2 项进行中 · 2%')
})

test('任务中心进度条保留 reference 的小数宽度和终态满格', async ({ page }) => {
  const tasks = [
    task({ id: 'fractional-progress', status: 'running', phase: '上传中', progress: 12.5, name: '小数进度.bin' }),
    task({ id: 'failed-progress', status: 'failed', phase: 'failed', progress: 42, error: '失败任务', name: '失败进度.zip' }),
  ]

  await page.route('**/api/**', async route => {
    const path = new URL(route.request().url()).pathname
    const json = (value: unknown) => route.fulfill({ json: value })
    if (path === '/api/auth/me') return json({ username: 'admin', has_avatar: false })
    if (path === '/api/events' || path === '/api/system/status/stream') {
      return route.fulfill({ contentType: 'text/event-stream', body: '' })
    }
    if (path === '/api/tasks') return json({ items: tasks })
    if (path === '/api/library/all') return json({ items: { book: [], image: [], video: [], audio: [] }, counts: { book: 0, image: 0, video: 0, audio: 0, file: 0 } })
    if (path === '/api/library/counts') return json({ book: 0, image: 0, video: 0, audio: 0, file: 0 })
    if (path === `/api/files/${ROOT}`) return json({ file: { id: ROOT, parent_id: null, name: '我的文件', kind: 'directory', size: 0, status: 'ready', created_at: STAMP, updated_at: STAMP }, breadcrumbs: [] })
    if (path === `/api/files/${ROOT}/children`) return json({ items: [], total_bytes: 0, file_count: 0 })
    return json({ items: [] })
  })

  await page.goto('/')
  await expect(page.getByRole('heading', { name: '我的文件' })).toBeVisible()
  await page.getByTitle('任务中心').click()

  const runningBar = page.locator('.active-group article').filter({ hasText: '小数进度.bin' }).locator('b')
  await expect(runningBar).toHaveClass(/running/)
  await expect.poll(() => runningBar.evaluate(element => (element as HTMLElement).style.width)).toBe('12.5%')

  const failedBar = page.locator('.failed-group article').filter({ hasText: '失败进度.zip' }).locator('b')
  await expect(failedBar).toHaveClass(/failed/)
  await expect.poll(() => failedBar.evaluate(element => (element as HTMLElement).style.width)).toBe('100%')
})

test('任务中心空名称沿用 reference 的已知类型与未知类型回退', async ({ page }) => {
  const tasks = [
    task({ id: 'known-empty', type: 'upload', name: '' }),
    task({ id: 'unknown-empty', type: 'future_task', name: '' }),
  ]

  await page.route('**/api/**', async route => {
    const path = new URL(route.request().url()).pathname
    const json = (value: unknown) => route.fulfill({ json: value })
    if (path === '/api/auth/me') return json({ username: 'admin', has_avatar: false })
    if (path === '/api/events' || path === '/api/system/status/stream') {
      return route.fulfill({ contentType: 'text/event-stream', body: '' })
    }
    if (path === '/api/tasks') return json({ items: tasks })
    if (path === '/api/library/all') return json({ items: { book: [], image: [], video: [], audio: [] }, counts: { book: 0, image: 0, video: 0, audio: 0, file: 0 } })
    if (path === '/api/library/counts') return json({ book: 0, image: 0, video: 0, audio: 0, file: 0 })
    if (path === `/api/files/${ROOT}`) return json({ file: { id: ROOT, parent_id: null, name: '我的文件', kind: 'directory', size: 0, status: 'ready', created_at: STAMP, updated_at: STAMP }, breadcrumbs: [] })
    if (path === `/api/files/${ROOT}/children`) return json({ items: [], total_bytes: 0, file_count: 0 })
    return json({ items: [] })
  })

  await page.goto('/')
  await expect(page.getByRole('heading', { name: '我的文件' })).toBeVisible()
  await page.getByTitle('任务中心').click()

  const known = page.locator('.completed-group article').filter({ hasText: '上传' }).locator('strong')
  await expect(known).toHaveText('上传')
  await expect(known).toHaveAttribute('title', '')
  const unknown = page.locator('.completed-group article').filter({ hasText: 'unknown-empty' }).locator('strong')
  await expect(unknown).toHaveText('unknown-empty')
  await expect(unknown).toHaveAttribute('title', '')
})

test('任务中心动作等待期间保留 reference 的按钮状态', async ({ page }) => {
  const tasks = [
    task({ id: 'cancel-action', status: 'running', phase: '上传中', progress: 42, name: '取消.bin' }),
    task({ id: 'password-action', type: 'archive_extract', status: 'waiting_input', phase: 'waiting_input', progress: 42, name: '密码.zip', finished_at: null }),
    task({ id: 'clear-action', name: '完成.txt' }),
  ]

  await page.route('**/api/**', async route => {
    const request = route.request()
    const path = new URL(request.url()).pathname
    const json = (value: unknown) => route.fulfill({ json: value })
    const delay = async () => new Promise(resolve => setTimeout(resolve, 800))
    if (path === '/api/auth/me') return json({ username: 'admin', has_avatar: false })
    if (path === '/api/events' || path === '/api/system/status/stream') {
      return route.fulfill({ contentType: 'text/event-stream', body: '' })
    }
    if (path === '/api/tasks' && request.method() === 'GET') return json({ items: tasks })
    if (path === '/api/tasks/cancel-action/cancel' || path === '/api/tasks/password-action/input' || (path === '/api/tasks/clear-action' && request.method() === 'DELETE')) {
      await delay()
      return route.fulfill({ status: 204, body: '' })
    }
    if (path === '/api/library/all') return json({ items: { book: [], image: [], video: [], audio: [] }, counts: { book: 0, image: 0, video: 0, audio: 0, file: 0 } })
    if (path === '/api/library/counts') return json({ book: 0, image: 0, video: 0, audio: 0, file: 0 })
    if (path === `/api/files/${ROOT}`) return json({ file: { id: ROOT, parent_id: null, name: '我的文件', kind: 'directory', size: 0, status: 'ready', created_at: STAMP, updated_at: STAMP }, breadcrumbs: [] })
    if (path === `/api/files/${ROOT}/children`) return json({ items: [], total_bytes: 0, file_count: 0 })
    return json({ items: [] })
  })

  await page.goto('/')
  await expect(page.getByRole('heading', { name: '我的文件' })).toBeVisible()
  await page.getByTitle('任务中心').click()

  const cancel = page.locator('.active-group article').filter({ hasText: '取消.bin' }).locator('button[title="取消"]')
  await cancel.click()
  await page.waitForTimeout(100)
  await expect(cancel).toBeEnabled()

  const waiting = page.locator('.active-group article').filter({ hasText: '密码.zip' })
  await waiting.click()
  const dialog = page.locator('.input-dialog')
  await dialog.locator('input').fill('password')
  await dialog.getByRole('button', { name: '继续任务' }).click()
  await page.waitForTimeout(100)
  await expect(dialog.getByRole('button', { name: '取消' })).toBeEnabled()
  await expect(dialog.getByRole('button', { name: '继续任务' })).toBeEnabled()
  await expect(dialog.getByRole('button', { name: '继续任务' })).toHaveText('继续任务')

  await page.keyboard.press('Escape')
  await page.getByTitle('任务中心').click()
  const clear = page.getByRole('button', { name: '清除完成' })
  await clear.click()
  await page.waitForTimeout(100)
  await expect(clear).toBeEnabled()
  await expect(clear).toHaveText('清除完成')
})

test('任务中心清除完成沿用 reference 的并发删除时序', async ({ page }) => {
  const tasks = [
    task({ id: 'clear-one', name: '完成一.txt' }),
    task({ id: 'clear-two', name: '完成二.txt' }),
  ]
  let inFlight = 0
  let maxConcurrent = 0
  let deleteRequests = 0

  await page.route('**/api/**', async route => {
    const request = route.request()
    const path = new URL(request.url()).pathname
    const json = (value: unknown) => route.fulfill({ json: value })
    if (path === '/api/auth/me') return json({ username: 'admin', has_avatar: false })
    if (path === '/api/events' || path === '/api/system/status/stream') {
      return route.fulfill({ contentType: 'text/event-stream', body: '' })
    }
    if (path === '/api/tasks' && request.method() === 'GET') return json({ items: tasks })
    if (path === '/api/tasks/clear-one' || path === '/api/tasks/clear-two') {
      deleteRequests += 1
      inFlight += 1
      maxConcurrent = Math.max(maxConcurrent, inFlight)
      await new Promise(resolve => setTimeout(resolve, 300))
      inFlight -= 1
      return route.fulfill({ status: 204, body: '' })
    }
    if (path === '/api/library/all') return json({ items: { book: [], image: [], video: [], audio: [] }, counts: { book: 0, image: 0, video: 0, audio: 0, file: 0 } })
    if (path === '/api/library/counts') return json({ book: 0, image: 0, video: 0, audio: 0, file: 0 })
    if (path === `/api/files/${ROOT}`) return json({ file: { id: ROOT, parent_id: null, name: '我的文件', kind: 'directory', size: 0, status: 'ready', created_at: STAMP, updated_at: STAMP }, breadcrumbs: [] })
    if (path === `/api/files/${ROOT}/children`) return json({ items: [], total_bytes: 0, file_count: 0 })
    return json({ items: [] })
  })

  await page.goto('/')
  await expect(page.getByRole('heading', { name: '我的文件' })).toBeVisible()
  await page.getByTitle('任务中心').click()
  await page.getByRole('button', { name: '清除完成' }).click()
  await expect.poll(() => deleteRequests).toBe(2)
  await expect.poll(() => maxConcurrent).toBe(2)
})

async function mockDelayedEmptyTasks(page: Page) {
  await page.route('**/api/**', async route => {
    const path = new URL(route.request().url()).pathname
    const json = (value: unknown) => route.fulfill({ json: value })
    if (path === '/api/auth/me') return json({ username: 'admin', has_avatar: false })
    if (path === '/api/events' || path === '/api/system/status/stream') {
      return route.fulfill({ contentType: 'text/event-stream', body: '' })
    }
    if (path === '/api/tasks') {
      await new Promise(resolve => setTimeout(resolve, 1_500))
      return json({ items: [] })
    }
    if (path === '/api/library/all') {
      return json({ items: { book: [], image: [], video: [], audio: [] }, counts: { book: 0, image: 0, video: 0, audio: 0, file: 0 } })
    }
    if (path === '/api/library/counts') return json({ book: 0, image: 0, video: 0, audio: 0, file: 0 })
    if (path === `/api/files/${ROOT}`) {
      return json({ file: { id: ROOT, parent_id: null, name: '我的文件', kind: 'directory', size: 0, status: 'ready', created_at: STAMP, updated_at: STAMP }, breadcrumbs: [] })
    }
    if (path === `/api/files/${ROOT}/children`) return json({ items: [], total_bytes: 0, file_count: 0 })
    return json({ items: [] })
  })
}

test('任务初次读取尚未返回时仍显示 reference 的空任务状态', async ({ browser }) => {
  const oldUrl = process.env.E2E_REFERENCE_URL || 'http://127.0.0.1:18080'
  const newUrl = process.env.E2E_NEW_URL || 'http://127.0.0.1:18083'
  const oldContext = await browser.newContext({ viewport: { width: 1440, height: 900 } })
  const newContext = await browser.newContext({ viewport: { width: 1440, height: 900 } })
  const oldPage = await oldContext.newPage()
  const newPage = await newContext.newPage()

  try {
    await Promise.all([mockDelayedEmptyTasks(oldPage), mockDelayedEmptyTasks(newPage)])
    await Promise.all([
      oldPage.goto(`${oldUrl}/`),
      newPage.goto(`${newUrl}/`),
    ])
    await Promise.all([
      expect(oldPage.getByRole('heading', { name: '我的文件', exact: true })).toBeVisible(),
      expect(newPage.getByRole('heading', { name: '我的文件', exact: true })).toBeVisible(),
    ])
    await oldPage.getByTitle('任务中心').click()
    await newPage.getByTitle('任务中心').click()
    await expect(oldPage.locator('.task-panel .empty')).toHaveText('还没有后台任务')
    await expect(newPage.locator('.task-panel .empty')).toHaveText('还没有后台任务')
  } finally {
    await oldContext.close()
    await newContext.close()
  }
})

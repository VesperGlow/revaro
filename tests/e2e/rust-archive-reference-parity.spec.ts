import { expect, test, type Page } from '@playwright/test'

const ROOT = '00000000-0000-0000-0000-000000000000'
const STAMP = '2026-01-01T00:00:00Z'

const archive = {
  id: 'archive-1',
  parent_id: ROOT,
  name: '需要密码.zip',
  kind: 'file',
  size: 4096,
  status: 'ready',
  created_at: STAMP,
  updated_at: STAMP,
  mime_type: 'application/zip',
  etag: 'archive-etag',
}

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

const waitingTask = {
  id: 'archive-task-1',
  type: 'archive_extract',
  status: 'waiting_input',
  phase: 'waiting_input',
  progress: 42,
  speed: 0,
  retry_count: 0,
  max_retries: 3,
  error: '',
  source_type: 'archive',
  source_id: 'archive-1',
  cancel_requested: false,
  name: archive.name,
  created_at: STAMP,
  updated_at: STAMP,
  finished_at: null,
}

const runningTask = { ...waitingTask, status: 'running', phase: '解压中', progress: 58, updated_at: '2026-01-01T00:00:01Z' }

const job = {
  id: waitingTask.id,
  file_id: archive.id,
  parent_id: ROOT,
  name: archive.name,
  status: 'queued',
  progress: 0,
  message: '',
  output_id: '',
  output_name: '',
  created_at: STAMP,
  updated_at: STAMP,
}

function dialogSnapshot(page: Page) {
  return page.getByRole('dialog').evaluate(element => ({
    title: element.querySelector('h2')?.textContent?.trim(),
    message: element.querySelector('.dialog-copy p')?.textContent?.trim(),
    buttons: Array.from(element.querySelectorAll('footer button')).map(button => ({
      text: button.textContent?.replace(/\s+/g, ' ').trim(),
      className: button.className,
      disabled: (button as HTMLButtonElement).disabled,
    })),
    iconPath: element.querySelector('.dialog-icon path')?.getAttribute('d'),
  }))
}

function taskSnapshot(page: Page) {
  return page.locator('.task-panel').evaluate(element => ({
    active: Array.from(element.querySelectorAll('.active-group article')).map(row => ({
      kind: row.querySelector('.kind')?.textContent?.trim(),
      name: row.querySelector('strong')?.textContent?.trim(),
      status: row.querySelector('small')?.textContent?.replace(/\s+/g, ' ').trim(),
      progress: row.querySelector('em')?.textContent?.trim(),
      actions: Array.from(row.querySelectorAll('.actions button')).map(button => button.getAttribute('title')),
    })),
    completed: element.querySelector('.completed-group')?.textContent?.replace(/\s+/g, ' ').trim() ?? null,
    failed: element.querySelector('.failed-group')?.textContent?.replace(/\s+/g, ' ').trim() ?? null,
  }))
}

function passwordSnapshot(page: Page) {
  return page.locator('.input-dialog').evaluate(element => ({
    title: element.querySelector('strong')?.textContent?.trim(),
    name: element.querySelector('small')?.textContent?.trim(),
    inputType: element.querySelector('input')?.getAttribute('type'),
    buttons: Array.from(element.querySelectorAll('footer button')).map(button => ({
      text: button.textContent?.replace(/\s+/g, ' ').trim(),
      disabled: (button as HTMLButtonElement).disabled,
    })),
  }))
}

async function mockArchive(page: Page) {
  let tasks: unknown[] = []
  const requests: string[] = []
  await page.route('**/api/**', async route => {
    const request = route.request()
    const path = new URL(request.url()).pathname
    const json = (value: unknown) => route.fulfill({ json: value })
    if (path === '/api/auth/me') return json({ username: 'admin', has_avatar: false })
    if (path === '/api/events' || path === '/api/system/status/stream') {
      return route.fulfill({ contentType: 'text/event-stream', body: '' })
    }
    if (path === '/api/tasks') return json({ items: tasks })
    if (path === '/api/library/all') {
      return json({ items: { book: [], image: [], video: [], audio: [] }, counts: { book: 0, image: 0, video: 0, audio: 0, file: 1 } })
    }
    if (path === '/api/library/counts') return json({ book: 0, image: 0, video: 0, audio: 0, file: 1 })
    if (path === `/api/files/${ROOT}`) return json({ file: root, breadcrumbs: [] })
    if (path === `/api/files/${ROOT}/children`) return json({ items: [archive], total_bytes: archive.size, file_count: 1 })
    if (path === `/api/files/${archive.id}/extract`) {
      requests.push(`${request.method()} ${path}`)
      tasks = [waitingTask]
      return json(job)
    }
    if (path === `/api/tasks/${waitingTask.id}/input`) {
      requests.push(`${request.method()} ${path}`)
      tasks = [runningTask]
      return json({ ...job, status: 'extracting', progress: runningTask.progress, message: runningTask.phase })
    }
    return json({ items: [] })
  })
  return { requests: () => requests }
}

async function openArchive(page: Page, url: string) {
  await page.goto(`${url}/`)
  await expect(page.getByRole('heading', { name: '我的文件', exact: true })).toBeVisible()
  await page.getByRole('button', { name: '列表' }).click()
  const row = page.locator('.file-row').filter({ hasText: archive.name })
  await expect(row).toBeVisible()
  await row.getByRole('button', { name: '选择项目' }).click()
}

test('旧版与 Rust 版归档解压入口、密码任务和状态刷新一致', async ({ browser }) => {
  const oldUrl = process.env.E2E_REFERENCE_URL || 'http://127.0.0.1:18080'
  const newUrl = process.env.E2E_NEW_URL || 'http://127.0.0.1:18084'
  const oldContext = await browser.newContext({ viewport: { width: 1440, height: 950 } })
  const newContext = await browser.newContext({ viewport: { width: 1440, height: 950 } })
  const oldPage = await oldContext.newPage()
  const newPage = await newContext.newPage()

  try {
    const [oldFixture, newFixture] = await Promise.all([mockArchive(oldPage), mockArchive(newPage)])
    await Promise.all([openArchive(oldPage, oldUrl), openArchive(newPage, newUrl)])
    const oldToolbar = oldPage.getByRole('toolbar', { name: '所选项目操作' })
    const newToolbar = newPage.getByRole('toolbar', { name: '所选项目操作' })
    await Promise.all([
      oldToolbar.getByRole('button', { name: '在线解压' }).click(),
      newToolbar.getByRole('button', { name: '在线解压' }).click(),
    ])
    await Promise.all([
      expect(oldPage.getByRole('dialog')).toContainText('解压到当前目录中的新文件夹'),
      expect(newPage.getByRole('dialog')).toContainText('解压到当前目录中的新文件夹'),
    ])
    expect(await dialogSnapshot(newPage), 'Rust 解压确认框与 reference 不一致').toEqual(await dialogSnapshot(oldPage))

    await Promise.all([
      oldPage.getByRole('dialog').getByRole('button', { name: '开始解压' }).click(),
      newPage.getByRole('dialog').getByRole('button', { name: '开始解压' }).click(),
    ])
    await Promise.all([
      expect(oldPage.getByRole('dialog')).toHaveCount(0),
      expect(newPage.getByRole('dialog')).toHaveCount(0),
      expect(oldPage.locator('.toast')).toHaveText('「需要密码.zip」已加入解压队列'),
      expect(newPage.locator('.toast')).toHaveText('「需要密码.zip」已加入解压队列'),
    ])
    expect(oldFixture.requests()).toEqual(['POST /api/files/archive-1/extract'])
    expect(newFixture.requests()).toEqual(['POST /api/files/archive-1/extract'])

    await Promise.all([
      oldPage.getByTitle('任务中心').click(),
      newPage.getByTitle('任务中心').click(),
    ])
    await Promise.all([
      expect(oldPage.locator('.active-group article')).toContainText('等待输入密码'),
      expect(newPage.locator('.active-group article')).toContainText('等待输入密码'),
    ])
    expect(await taskSnapshot(newPage), 'Rust 密码任务中心与 reference 不一致').toEqual(await taskSnapshot(oldPage))

    await Promise.all([
      oldPage.locator('.active-group article').click(),
      newPage.locator('.active-group article').click(),
    ])
    await Promise.all([
      expect(oldPage.locator('.input-dialog')).toBeVisible(),
      expect(newPage.locator('.input-dialog')).toBeVisible(),
    ])
    expect(await passwordSnapshot(newPage), 'Rust 密码输入框与 reference 不一致').toEqual(await passwordSnapshot(oldPage))
    await Promise.all([
      oldPage.locator('.input-dialog input').fill('archive-password'),
      newPage.locator('.input-dialog input').fill('archive-password'),
    ])
    await Promise.all([
      oldPage.locator('.input-dialog').getByRole('button', { name: '继续任务' }).click(),
      newPage.locator('.input-dialog').getByRole('button', { name: '继续任务' }).click(),
    ])
    await Promise.all([
      expect(oldPage.locator('.input-dialog')).toHaveCount(0),
      expect(newPage.locator('.input-dialog')).toHaveCount(0),
      expect(oldPage.locator('.active-group article')).toContainText('解压中'),
      expect(newPage.locator('.active-group article')).toContainText('解压中'),
    ])
    expect(oldFixture.requests()).toEqual(['POST /api/files/archive-1/extract', 'POST /api/tasks/archive-task-1/input'])
    expect(newFixture.requests()).toEqual(['POST /api/files/archive-1/extract', 'POST /api/tasks/archive-task-1/input'])
  } finally {
    await oldContext.close()
    await newContext.close()
  }
})

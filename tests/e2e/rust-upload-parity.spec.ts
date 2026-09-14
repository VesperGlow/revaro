import { mkdir, writeFile } from 'node:fs/promises'
import path from 'node:path'
import { expect, test } from '@playwright/test'
import { login } from './helpers'

async function removeCreated(page: Parameters<typeof login>[0], names: string[]) {
  await page.evaluate(async wanted => {
    const headers = { 'Content-Type': 'application/json' }
    const childrenResponse = await fetch('/api/files/00000000-0000-0000-0000-000000000000/children')
    if (!childrenResponse.ok) return
    const children = await childrenResponse.json() as { items?: Array<{ id: string; name: string }> }
    for (const item of children.items ?? []) {
      if (wanted.includes(item.name)) await fetch(`/api/files/${item.id}`, { method: 'DELETE', headers })
    }
    const trashResponse = await fetch('/api/trash')
    if (!trashResponse.ok) return
    const trash = await trashResponse.json() as { items?: Array<{ id: string; name: string }> }
    for (const item of trash.items ?? []) {
      if (wanted.includes(item.name)) await fetch(`/api/trash/${item.id}`, { method: 'DELETE', headers })
    }
  }, names)
}

test('上传文件夹保留旧版的相对目录结构并通过任务状态完成刷新', async ({ page }, testInfo) => {
  const directory = testInfo.outputPath(`folder-${crypto.randomUUID()}`)
  const rootName = path.basename(directory)
  await mkdir(path.join(directory, 'nested'), { recursive: true })
  await writeFile(path.join(directory, 'nested', 'first.txt'), 'folder upload parity first\n')
  await writeFile(path.join(directory, 'nested', 'second.txt'), 'folder upload parity second\n')

  try {
    await login(page)
    await page.evaluate(() => {
      const marks: string[] = []
      const record = () => {
        const text = document.querySelector('.toast')?.textContent?.trim() ?? ''
        if (text && marks.at(-1) !== text) marks.push(text)
      }
      new MutationObserver(record).observe(document.body, { subtree: true, childList: true, characterData: true })
      ;(window as typeof window & { __folderUploadToastMarks?: string[] }).__folderUploadToastMarks = marks
    })
    await page.locator('input[webkitdirectory]').setInputFiles(directory)
    await expect.poll(
      () => page.evaluate(() => (window as typeof window & { __folderUploadToastMarks?: string[] }).__folderUploadToastMarks?.includes('已保留目录结构，开始上传 2 个文件') ?? false),
      { timeout: 15_000 },
    ).toBe(true)
    await expect(page.locator('.file-card, .file-row').filter({ hasText: rootName })).toBeVisible({ timeout: 20_000 })

    await page.locator('.file-card, .file-row').filter({ hasText: rootName }).click()
    await expect(page.locator('.file-card, .file-row').filter({ hasText: 'nested' })).toBeVisible({ timeout: 20_000 })
    await page.locator('.file-card, .file-row').filter({ hasText: 'nested' }).click()
    await expect(page.locator('.file-card, .file-row').filter({ hasText: 'first.txt' })).toBeVisible({ timeout: 20_000 })
    await expect(page.locator('.file-card, .file-row').filter({ hasText: 'second.txt' })).toBeVisible({ timeout: 20_000 })
  } finally {
    await removeCreated(page, [rootName])
  }
})

test('拖放文件时显示并关闭旧版上传覆盖层，回收站中不接受拖放', async ({ page }) => {
  await login(page)
  const shell = page.locator('.app-shell')

  await shell.evaluate(element => {
    const transfer = new DataTransfer()
    transfer.items.add(new File(['drop parity'], 'drop-parity.txt', { type: 'text/plain' }))
    element.dispatchEvent(new DragEvent('dragover', { bubbles: true, cancelable: true, dataTransfer: transfer }))
  })
  await expect(page.locator('.drop-zone')).toBeVisible()

  await shell.evaluate(element => element.dispatchEvent(new DragEvent('dragleave', { bubbles: true, cancelable: true })))
  await expect(page.locator('.drop-zone')).toHaveCount(0)

  await page.getByRole('button', { name: '打开回收站' }).click()
  await shell.evaluate(element => {
    const transfer = new DataTransfer()
    transfer.items.add(new File(['drop parity'], 'drop-parity-trash.txt', { type: 'text/plain' }))
    element.dispatchEvent(new DragEvent('dragover', { bubbles: true, cancelable: true, dataTransfer: transfer }))
  })
  await expect(page.locator('.drop-zone')).toHaveCount(0)
})

test('拖入内容子元素时保留 reference 的上传覆盖层', async ({ page }) => {
  await login(page)
  const shell = page.locator('.app-shell')
  const content = page.locator('.content-head')

  await shell.evaluate(element => {
    const transfer = new DataTransfer()
    transfer.items.add(new File(['child dragleave parity'], 'child-dragleave.txt', { type: 'text/plain' }))
    element.dispatchEvent(new DragEvent('dragover', { bubbles: true, cancelable: true, dataTransfer: transfer }))
  })
  await expect(page.locator('.drop-zone')).toBeVisible()

  await content.evaluate(element => {
    const transfer = new DataTransfer()
    element.dispatchEvent(new DragEvent('dragleave', { bubbles: true, cancelable: true, dataTransfer: transfer }))
  })
  await expect(page.locator('.drop-zone')).toBeVisible()

  await shell.evaluate(element => element.dispatchEvent(new DragEvent('dragleave', { bubbles: true, cancelable: true })))
  await expect(page.locator('.drop-zone')).toHaveCount(0)
})

test('文件夹上传在当前目录刷新完成后才显示成功反馈', async ({ page }, testInfo) => {
  const directory = testInfo.outputPath(`timing-${crypto.randomUUID()}`)
  const rootName = path.basename(directory)
  await mkdir(directory, { recursive: true })
  await writeFile(path.join(directory, 'timing.txt'), 'upload timing parity\n')

  let delayedChildren = 0
  let refreshFinishedAt = 0
  await page.route('**/api/directories', async route => {
    const response = await route.fetch()
    if (response.ok()) delayedChildren = 4
    await route.fulfill({ response })
  })
  await page.route('**/api/files/00000000-0000-0000-0000-000000000000/children', async route => {
    const response = await route.fetch()
    if (delayedChildren > 0) {
      delayedChildren -= 1
      await new Promise(resolve => setTimeout(resolve, 1_500))
      refreshFinishedAt = Date.now()
    }
    await route.fulfill({ response })
  })

  try {
    await login(page)
    await page.evaluate(() => {
      const marks: Array<{ at: number; text: string }> = []
      const record = () => {
        const text = document.querySelector('.toast')?.textContent?.trim() ?? ''
        if (marks.at(-1)?.text !== text) marks.push({ at: Date.now(), text })
      }
      new MutationObserver(record).observe(document.body, { subtree: true, childList: true, characterData: true })
      ;(window as typeof window & { __toastMarks?: typeof marks }).__toastMarks = marks
    })
    await page.locator('input[webkitdirectory]').setInputFiles(directory)
    await expect.poll(
      () => page.evaluate(() => (window as typeof window & { __toastMarks?: Array<{ at: number; text: string }> }).__toastMarks?.some(mark => mark.text === '已保留目录结构，开始上传 1 个文件') ?? false),
      { timeout: 10_000 },
    ).toBe(true)
    const marks = await page.evaluate(() => (window as typeof window & { __toastMarks?: Array<{ at: number; text: string }> }).__toastMarks ?? [])
    const success = marks.find(mark => mark.text === '已保留目录结构，开始上传 1 个文件')
    expect(success).toBeDefined()
    expect(refreshFinishedAt).toBeGreaterThan(0)
    expect(success!.at).toBeGreaterThanOrEqual(refreshFinishedAt)
  } finally {
    await removeCreated(page, [rootName])
  }
})

async function loginAt(page: Parameters<typeof login>[0], baseUrl: string) {
  await page.goto(`${baseUrl}/?folder-upload-reference=${Date.now()}`)
  await page.getByLabel('用户名').fill(process.env.E2E_USERNAME || 'admin')
  await page.getByLabel('密码').fill(process.env.E2E_PASSWORD || 'revaro-e2e-password')
  await page.getByRole('button', { name: '进入我的网盘' }).click()
  await expect(page.getByRole('heading', { name: '我的文件', exact: true })).toBeVisible()
}

async function watchFolderUploadFeedback(page: Parameters<typeof login>[0]) {
  await page.evaluate(() => {
    const marks: string[] = []
    const record = () => {
      const text = document.querySelector('.toast')?.textContent?.trim() ?? ''
      if (text && marks.at(-1) !== text) marks.push(text)
    }
    new MutationObserver(record).observe(document.body, { subtree: true, childList: true, characterData: true })
    ;(window as typeof window & { __folderUploadToastMarks?: string[] }).__folderUploadToastMarks = marks
  })
}

test('old/new 文件夹上传保留相同反馈与嵌套目录结果', async ({ browser }, testInfo) => {
  const directory = testInfo.outputPath(`folder-reference-${crypto.randomUUID()}`)
  const rootName = path.basename(directory)
  await mkdir(path.join(directory, 'nested'), { recursive: true })
  await writeFile(path.join(directory, 'nested', 'first.txt'), 'folder upload reference first\n')
  await writeFile(path.join(directory, 'nested', 'second.txt'), 'folder upload reference second\n')

  const oldUrl = process.env.E2E_REFERENCE_URL || 'http://127.0.0.1:18080'
  const newUrl = process.env.E2E_NEW_URL || 'http://127.0.0.1:18083'
  const oldContext = await browser.newContext({ viewport: { width: 1440, height: 900 } })
  const newContext = await browser.newContext({ viewport: { width: 1440, height: 900 } })
  const oldPage = await oldContext.newPage()
  const newPage = await newContext.newPage()

  try {
    await Promise.all([loginAt(oldPage, oldUrl), loginAt(newPage, newUrl)])
    await Promise.all([watchFolderUploadFeedback(oldPage), watchFolderUploadFeedback(newPage)])
    await Promise.all([
      oldPage.locator('input[webkitdirectory]').setInputFiles(directory),
      newPage.locator('input[webkitdirectory]').setInputFiles(directory),
    ])
    await Promise.all([
      expect.poll(() => oldPage.evaluate(() => (window as typeof window & { __folderUploadToastMarks?: string[] }).__folderUploadToastMarks?.includes('已保留目录结构，开始上传 2 个文件') ?? false), { timeout: 15_000 }).toBe(true),
      expect.poll(() => newPage.evaluate(() => (window as typeof window & { __folderUploadToastMarks?: string[] }).__folderUploadToastMarks?.includes('已保留目录结构，开始上传 2 个文件') ?? false), { timeout: 15_000 }).toBe(true),
    ])
    await Promise.all([
      expect(oldPage.locator('.file-card, .file-row').filter({ hasText: rootName })).toBeVisible({ timeout: 20_000 }),
      expect(newPage.locator('.file-card, .file-row').filter({ hasText: rootName })).toBeVisible({ timeout: 20_000 }),
    ])
    for (const page of [oldPage, newPage]) {
      await page.locator('.file-card, .file-row').filter({ hasText: rootName }).click()
      await expect(page.locator('.file-card, .file-row').filter({ hasText: 'nested' })).toBeVisible({ timeout: 20_000 })
      await page.locator('.file-card, .file-row').filter({ hasText: 'nested' }).click()
      await expect(page.locator('.file-card, .file-row').filter({ hasText: 'first.txt' })).toBeVisible({ timeout: 20_000 })
      await expect(page.locator('.file-card, .file-row').filter({ hasText: 'second.txt' })).toBeVisible({ timeout: 20_000 })
    }
  } finally {
    await Promise.all([removeCreated(oldPage, [rootName]), removeCreated(newPage, [rootName])])
    await oldContext.close()
    await newContext.close()
  }
})

test('old/new 普通文件上传按 reference 的时机进入任务中心', async ({ browser }) => {
  const name = `upload-queue-reference-${crypto.randomUUID()}.txt`
  const buffer = Buffer.from('upload queue reference\n')
  const oldUrl = process.env.E2E_REFERENCE_URL || 'http://127.0.0.1:18080'
  const newUrl = process.env.E2E_NEW_URL || 'http://127.0.0.1:18083'
  const oldContext = await browser.newContext({ viewport: { width: 1440, height: 900 } })
  const newContext = await browser.newContext({ viewport: { width: 1440, height: 900 } })
  const oldPage = await oldContext.newPage()
  const newPage = await newContext.newPage()

  async function delayByteRequest(page: Parameters<typeof login>[0]) {
    await page.route(/\/api\/uploads\/[^/]+\/data$/, async route => {
      if (route.request().method() !== 'PUT') {
        await route.continue()
        return
      }
      await new Promise(resolve => setTimeout(resolve, 5_000))
      await route.continue()
    })
  }

  async function waitForTask(page: Parameters<typeof login>[0]) {
    await expect.poll(async () => page.evaluate(async fileName => {
      const response = await fetch('/api/tasks')
      if (!response.ok) return false
      const payload = await response.json() as { items?: Array<{ name: string; status: string; source_type?: string }> }
      return (payload.items ?? []).some(task => task.name === fileName && task.source_type === 'upload' && ['queued', 'running'].includes(task.status))
    }, name), { timeout: 10_000 }).toBe(true)
  }

  async function waitForCompletedTask(page: Parameters<typeof login>[0]) {
    await expect.poll(async () => page.evaluate(async fileName => {
      const response = await fetch('/api/tasks')
      if (!response.ok) return false
      const payload = await response.json() as { items?: Array<{ name: string; status: string; source_type?: string }> }
      return (payload.items ?? []).some(task => task.name === fileName && task.source_type === 'upload' && task.status === 'completed')
    }, name), { timeout: 15_000 }).toBe(true)
  }

  async function taskSnapshot(page: Parameters<typeof login>[0]) {
    const row = page.locator('.task-panel .task-list article, .task-panel .task-group-row').filter({ hasText: name }).first()
    await expect(row).toBeVisible()
    return row.evaluate(element => ({
      name: element.querySelector('strong')?.textContent?.trim(),
      kind: element.querySelector('.kind')?.textContent?.trim(),
      status: element.querySelector('small')?.textContent?.trim(),
      progress: element.querySelector('b')?.className,
      hasCancel: Boolean(element.querySelector('button[title="取消"]')),
    }))
  }

  try {
    await Promise.all([delayByteRequest(oldPage), delayByteRequest(newPage)])
    await Promise.all([
      loginAt(oldPage, oldUrl),
      loginAt(newPage, newUrl),
    ])
    await Promise.all([
      oldPage.locator('input[type=file]').first().setInputFiles({ name, mimeType: 'text/plain', buffer }),
      newPage.locator('input[type=file]').first().setInputFiles({ name, mimeType: 'text/plain', buffer }),
    ])
    await Promise.all([waitForTask(oldPage), waitForTask(newPage)])
    await Promise.all([
      oldPage.getByTitle('任务中心').click(),
      newPage.getByTitle('任务中心').click(),
    ])
    await Promise.all([
      expect(oldPage.locator('.task-panel .task-list article, .task-panel .task-group-row').filter({ hasText: name })).toHaveCount(0),
      expect(newPage.locator('.task-panel .task-list article, .task-panel .task-group-row').filter({ hasText: name })).toHaveCount(0),
    ])
    await Promise.all([waitForCompletedTask(oldPage), waitForCompletedTask(newPage)])
    const [oldTask, newTask] = await Promise.all([taskSnapshot(oldPage), taskSnapshot(newPage)])
    expect(newTask, 'Rust 普通上传任务中心状态与 reference 不一致').toEqual(oldTask)
  } finally {
    await Promise.all([removeCreated(oldPage, [name]), removeCreated(newPage, [name])])
    await oldContext.close()
    await newContext.close()
  }
})

test('old/new 字节上传连续失败时保留相同的任务中心可见状态', async ({ browser }) => {
  const name = `upload-failure-reference-${crypto.randomUUID()}.txt`
  const buffer = Buffer.from('upload failure reference\n')
  const oldUrl = process.env.E2E_REFERENCE_URL || 'http://127.0.0.1:18080'
  const newUrl = process.env.E2E_NEW_URL || 'http://127.0.0.1:18084'
  const oldContext = await browser.newContext({ viewport: { width: 1440, height: 900 } })
  const newContext = await browser.newContext({ viewport: { width: 1440, height: 900 } })
  const oldPage = await oldContext.newPage()
  const newPage = await newContext.newPage()
  const oldState = { attempts: 0, uploadId: '' }
  const newState = { attempts: 0, uploadId: '' }

  async function forceByteFailure(page: Parameters<typeof login>[0], state: typeof oldState) {
    await page.route(/\/api\/uploads$/, async route => {
      if (route.request().method() !== 'POST') {
        await route.continue()
        return
      }
      const response = await route.fetch()
      const payload = await response.json() as { upload_id?: string }
      state.uploadId = payload.upload_id || ''
      await route.fulfill({ response })
    })
    await page.route(/\/api\/uploads\/[^/]+\/data$/, async route => {
      if (route.request().method() !== 'PUT') {
        await route.continue()
        return
      }
      state.attempts += 1
      await route.fulfill({
        status: 503,
        contentType: 'application/json',
        body: JSON.stringify({ error: { code: 'forced_upload_failure', message: 'forced upload failure' } }),
      })
    })
  }

  async function durableSnapshot(page: Parameters<typeof login>[0], uploadId: string) {
    return page.evaluate(async id => {
      const response = await fetch('/api/tasks')
      if (!response.ok) return null
      const payload = await response.json() as { items?: Array<{ name: string; status: string; phase: string; progress: number; error: string; source_type?: string; source_id?: string }> }
      const task = (payload.items ?? []).find(item => item.source_type === 'upload' && item.source_id === id)
      return task ? { status: task.status, phase: task.phase, progress: task.progress, error: task.error ?? '' } : null
    }, uploadId)
  }

  async function uploadSnapshot(page: Parameters<typeof login>[0], uploadId: string) {
    return page.evaluate(async id => {
      const response = await fetch(`/api/uploads/${id}`)
      if (!response.ok) return null
      const payload = await response.json() as { status?: string }
      return payload.status || null
    }, uploadId)
  }

  try {
    await Promise.all([
      forceByteFailure(oldPage, oldState),
      forceByteFailure(newPage, newState),
      loginAt(oldPage, oldUrl),
      loginAt(newPage, newUrl),
    ])
    await Promise.all([
      oldPage.locator('input[type=file]').first().setInputFiles({ name, mimeType: 'text/plain', buffer }),
      newPage.locator('input[type=file]').first().setInputFiles({ name, mimeType: 'text/plain', buffer }),
    ])
    await Promise.all([
      expect.poll(() => oldState.attempts, { timeout: 15_000 }).toBe(5),
      expect.poll(() => newState.attempts, { timeout: 15_000 }).toBe(5),
    ])
    await Promise.all([
      expect.poll(() => uploadSnapshot(oldPage, oldState.uploadId), { timeout: 10_000 }).toBe('pending'),
      expect.poll(() => uploadSnapshot(newPage, newState.uploadId), { timeout: 10_000 }).toBe('pending'),
    ])
    const [oldTask, newTask] = await Promise.all([
      durableSnapshot(oldPage, oldState.uploadId),
      durableSnapshot(newPage, newState.uploadId),
    ])
    expect(oldTask).toEqual({ status: 'queued', phase: 'uploading', progress: 0, error: '' })
    expect(newTask, 'Rust 字节上传失败后的持久化任务状态与 reference 不一致').toEqual(oldTask)

    await Promise.all([
      oldPage.getByTitle('任务中心').click(),
      newPage.getByTitle('任务中心').click(),
    ])
    await Promise.all([
      expect(oldPage.locator('.task-panel .task-list article, .task-panel .task-group-row').filter({ hasText: name })).toHaveCount(0),
      expect(newPage.locator('.task-panel .task-list article, .task-panel .task-group-row').filter({ hasText: name })).toHaveCount(0),
    ])
  } finally {
    await Promise.all([
      oldState.uploadId ? oldPage.evaluate(async id => { await fetch(`/api/uploads/${id}`, { method: 'DELETE' }) }, oldState.uploadId) : Promise.resolve(),
      newState.uploadId ? newPage.evaluate(async id => { await fetch(`/api/uploads/${id}`, { method: 'DELETE' }) }, newState.uploadId) : Promise.resolve(),
    ])
    await Promise.all([oldContext.close(), newContext.close()])
  }
})

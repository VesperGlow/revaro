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

test('old/new 文件夹上传按 reference 保留同层目录的创建顺序', async ({ browser }) => {
  const rootName = `upload-order-${crypto.randomUUID()}`
  const oldUrl = process.env.E2E_REFERENCE_URL || 'http://127.0.0.1:18080'
  const newUrl = process.env.E2E_NEW_URL || 'http://127.0.0.1:18084'
  const oldContext = await browser.newContext({ viewport: { width: 1440, height: 900 } })
  const newContext = await browser.newContext({ viewport: { width: 1440, height: 900 } })
  const oldPage = await oldContext.newPage()
  const newPage = await newContext.newPage()

  async function exercise(page: Parameters<typeof login>[0], baseUrl: string) {
    const directoryNames: string[] = []
    await page.route('**/api/directories', async route => {
      if (route.request().method() === 'POST') {
        const body = route.request().postDataJSON() as { name?: string }
        directoryNames.push(body.name ?? '')
      }
      await route.continue()
    })
    await loginAt(page, baseUrl)
    await page.evaluate(({ root }) => {
      const input = document.querySelector('input[webkitdirectory]')
      if (!(input instanceof HTMLInputElement)) throw new Error('folder input is missing')
      const files = [
        new File(['z'], 'z.txt', { type: 'text/plain' }),
        new File(['a'], 'a.txt', { type: 'text/plain' }),
      ]
      Object.defineProperty(files[0], 'webkitRelativePath', { value: `${root}/z-tree/child/z.txt` })
      Object.defineProperty(files[1], 'webkitRelativePath', { value: `${root}/a-tree/child/a.txt` })
      const transfer = new DataTransfer()
      for (const file of files) transfer.items.add(file)
      input.files = transfer.files
      input.dispatchEvent(new Event('change', { bubbles: true }))
    }, { root: rootName })
    await expect.poll(() => directoryNames.length, { timeout: 15_000 }).toBe(5)
    await expect(page.locator('.file-card, .file-row').filter({ hasText: rootName })).toBeVisible({ timeout: 20_000 })
    return directoryNames
  }

  try {
    const [oldNames, newNames] = await Promise.all([
      exercise(oldPage, oldUrl),
      exercise(newPage, newUrl),
    ])
    expect(oldNames).toEqual([
      rootName,
      'z-tree',
      'a-tree',
      'child',
      'child',
    ])
    expect(newNames, 'Rust 文件夹上传同层目录创建顺序与 reference 不一致').toEqual(oldNames)
  } finally {
    await Promise.all([
      removeCreated(oldPage, [rootName]),
      removeCreated(newPage, [rootName]),
      oldContext.close(),
      newContext.close(),
    ])
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

test('old/new shell dragleave 保留 reference 的默认事件语义', async ({ browser }) => {
  const oldUrl = process.env.E2E_REFERENCE_URL || 'http://127.0.0.1:18080'
  const newUrl = process.env.E2E_NEW_URL || 'http://127.0.0.1:18084'
  const oldContext = await browser.newContext({ viewport: { width: 1440, height: 900 } })
  const newContext = await browser.newContext({ viewport: { width: 1440, height: 900 } })
  const oldPage = await oldContext.newPage()
  const newPage = await newContext.newPage()

  async function dragLeaveDefaultPrevented(page: Parameters<typeof login>[0], baseUrl: string) {
    await loginAt(page, baseUrl)
    return page.evaluate(() => new Promise<boolean>(resolve => {
      window.addEventListener('dragleave', event => resolve(event.defaultPrevented), { once: true })
      const shell = document.querySelector('.app-shell')
      if (!shell) throw new Error('app shell is missing')
      shell.dispatchEvent(new DragEvent('dragleave', { bubbles: true, cancelable: true }))
    }))
  }

  try {
    const [oldPrevented, newPrevented] = await Promise.all([
      dragLeaveDefaultPrevented(oldPage, oldUrl),
      dragLeaveDefaultPrevented(newPage, newUrl),
    ])
    expect(oldPrevented).toBe(false)
    expect(newPrevented, 'Rust shell dragleave 不应额外阻止默认事件').toBe(oldPrevented)
  } finally {
    await Promise.all([oldContext.close(), newContext.close()])
  }
})

test('old/new 实际拖放文件按 reference 上传到当前目录并拒绝回收站目标', async ({ browser }) => {
  const name = `drop-upload-reference-${crypto.randomUUID()}.txt`
  const trashName = `drop-trash-reference-${crypto.randomUUID()}.txt`
  const oldUrl = process.env.E2E_REFERENCE_URL || 'http://127.0.0.1:18080'
  const newUrl = process.env.E2E_NEW_URL || 'http://127.0.0.1:18084'
  const oldContext = await browser.newContext({ viewport: { width: 1440, height: 900 } })
  const newContext = await browser.newContext({ viewport: { width: 1440, height: 900 } })
  const oldPage = await oldContext.newPage()
  const newPage = await newContext.newPage()

  async function dropFile(page: Parameters<typeof login>[0], baseUrl: string, fileName: string) {
    const uploadResponses: number[] = []
    page.on('response', response => {
      const url = new URL(response.url())
      if (url.pathname === '/api/uploads' && response.request().method() === 'POST') {
        uploadResponses.push(response.status())
      }
    })
    await loginAt(page, baseUrl)
    const defaultPrevented = await page.locator('.app-shell').evaluate((element, droppedName) => {
      const transfer = new DataTransfer()
      transfer.items.add(new File(['drop upload reference\n'], droppedName, { type: 'text/plain' }))
      const event = new DragEvent('drop', { bubbles: true, cancelable: true, dataTransfer: transfer })
      element.dispatchEvent(event)
      return event.defaultPrevented
    }, fileName)
    await expect.poll(() => uploadResponses.length, { timeout: 15_000 }).toBe(1)
    await expect.poll(() => page.evaluate(async wanted => {
      const response = await fetch('/api/files/00000000-0000-0000-0000-000000000000/children')
      if (!response.ok) return false
      const payload = await response.json() as { items?: Array<{ name: string; status?: string }> }
      return (payload.items ?? []).some(item => item.name === wanted && item.status === 'ready')
    }, fileName), { timeout: 20_000 }).toBe(true)

    // The reference schedules one more current-folder refresh 250 ms after
    // completion. Let that refresh settle before the next navigation so this
    // test compares the trash/drop behavior rather than its timing race.
    await page.waitForTimeout(500)

    await page.getByRole('button', { name: '打开回收站' }).click()
    await expect(page.getByRole('heading', { name: '回收站', exact: true })).toBeVisible()
    const responseCountBeforeTrashDrop = uploadResponses.length
    const trashDefaultPrevented = await page.locator('.app-shell').evaluate((element, droppedName) => {
      const transfer = new DataTransfer()
      transfer.items.add(new File(['must not upload'], droppedName, { type: 'text/plain' }))
      const event = new DragEvent('drop', { bubbles: true, cancelable: true, dataTransfer: transfer })
      element.dispatchEvent(event)
      return event.defaultPrevented
    }, trashName)
    await page.waitForTimeout(300)
    return {
      defaultPrevented,
      trashDefaultPrevented,
      uploadResponses,
      responseCountBeforeTrashDrop,
    }
  }

  try {
    const [oldResult, newResult] = await Promise.all([
      dropFile(oldPage, oldUrl, name),
      dropFile(newPage, newUrl, name),
    ])
    expect(oldResult).toEqual({
      defaultPrevented: true,
      trashDefaultPrevented: true,
      uploadResponses: [201],
      responseCountBeforeTrashDrop: 1,
    })
    expect(newResult, 'Rust 实际拖放上传与 reference 不一致').toEqual(oldResult)
  } finally {
    await Promise.all([removeCreated(oldPage, [name]), removeCreated(newPage, [name])])
    await Promise.all([oldContext.close(), newContext.close()])
  }
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
  const newUrl = process.env.E2E_NEW_URL || 'http://127.0.0.1:18084'
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
  const newUrl = process.env.E2E_NEW_URL || 'http://127.0.0.1:18084'
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

  async function loginAndAwaitInitialTaskSnapshot(page: Parameters<typeof login>[0], baseUrl: string) {
    const initialTasks = page.waitForResponse(response => {
      const url = new URL(response.url())
      return url.pathname === '/api/tasks' && response.request().method() === 'GET' && response.ok()
    })
    await loginAt(page, baseUrl)
    const response = await initialTasks
    await response.finished()
    // Let TaskCenter consume the completed response before the upload starts;
    // otherwise a slow initial snapshot can legitimately include the newly
    // created server row and make the event-timing assertion flaky.
    await page.waitForTimeout(50)
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
      loginAndAwaitInitialTaskSnapshot(oldPage, oldUrl),
      loginAndAwaitInitialTaskSnapshot(newPage, newUrl),
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

test('old/new 刷新后任务中心取消上传仍走 reference 的任务取消入口', async ({ browser }) => {
  const name = `upload-task-cancel-reference-${crypto.randomUUID()}.bin`
  const oldUrl = process.env.E2E_REFERENCE_URL || 'http://127.0.0.1:18080'
  const newUrl = process.env.E2E_NEW_URL || 'http://127.0.0.1:18084'
  const oldContext = await browser.newContext({ viewport: { width: 1440, height: 900 } })
  const newContext = await browser.newContext({ viewport: { width: 1440, height: 900 } })
  const oldPage = await oldContext.newPage()
  const newPage = await newContext.newPage()

  async function exercise(page: Parameters<typeof login>[0], baseUrl: string) {
    const requests: string[] = []
    page.on('request', request => {
      const url = new URL(request.url())
      if (url.pathname.startsWith('/api/tasks/') || url.pathname.startsWith('/api/uploads/')) {
        requests.push(`${request.method()} ${url.pathname}`)
      }
    })
    await loginAt(page, baseUrl)
    const upload = await page.evaluate(async uploadName => {
      const response = await fetch('/api/uploads', {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({
          parent_id: '00000000-0000-0000-0000-000000000000',
          name: uploadName,
          size: 1,
          mime_type: 'application/octet-stream',
        }),
      })
      if (!response.ok) throw new Error(`create upload failed: ${response.status}`)
      return await response.json() as { upload_id: string }
    }, name)
    await page.reload()
    await expect(page.getByRole('heading', { name: '我的文件', exact: true })).toBeVisible()
    const task = await page.evaluate(async uploadId => {
      const response = await fetch('/api/tasks')
      if (!response.ok) throw new Error(`list tasks failed: ${response.status}`)
      const payload = await response.json() as { items?: Array<{ id: string; source_type?: string; source_id?: string }> }
      return (payload.items ?? []).find(item => item.source_type === 'upload' && item.source_id === uploadId)
    }, upload.upload_id)
    if (!task) throw new Error('upload task is missing after reload')

    await page.getByTitle('任务中心').click()
    const row = page.locator('.active-group article, .task-group-row').filter({ hasText: name })
    await expect(row).toBeVisible()
    await row.getByRole('button', { name: '取消' }).click()
    await expect.poll(() => page.evaluate(async uploadId => {
      const response = await fetch(`/api/uploads/${uploadId}`)
      return response.status
    }, upload.upload_id), { timeout: 10_000 }).toBe(404)
    return requests
      .filter(request => request.includes('/cancel') || request.includes(`/api/uploads/${upload.upload_id}`))
      .map(request => request.replace(/[0-9a-f-]{36}/g, ':id'))
  }

  try {
    const [oldRequests, newRequests] = await Promise.all([
      exercise(oldPage, oldUrl),
      exercise(newPage, newUrl),
    ])
    expect(oldRequests).toContainEqual(expect.stringMatching(/^POST \/api\/tasks\/[^/]+\/cancel$/))
    expect(newRequests, 'Rust 刷新后任务中心取消上传没有沿用 reference 入口').toEqual(oldRequests)
  } finally {
    await Promise.all([oldContext.close(), newContext.close()])
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

  async function forceByteFailure(page: Parameters<typeof login>[0], state: typeof oldState, status = 503) {
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
        status,
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

test('old/new 字节上传遇到非 5xx 响应仍按 reference 重试五次', async ({ browser }) => {
  const name = `upload-4xx-retry-reference-${crypto.randomUUID()}.txt`
  const buffer = Buffer.from('upload 4xx retry reference\n')
  const oldUrl = process.env.E2E_REFERENCE_URL || 'http://127.0.0.1:18080'
  const newUrl = process.env.E2E_NEW_URL || 'http://127.0.0.1:18084'
  const oldContext = await browser.newContext({ viewport: { width: 1440, height: 900 } })
  const newContext = await browser.newContext({ viewport: { width: 1440, height: 900 } })
  const oldPage = await oldContext.newPage()
  const newPage = await newContext.newPage()
  const oldState = { attempts: 0, uploadId: '' }
  const newState = { attempts: 0, uploadId: '' }

  async function exercise(page: Parameters<typeof login>[0], baseUrl: string, state: typeof oldState) {
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
        status: 409,
        contentType: 'application/json',
        body: JSON.stringify({ error: { code: 'forced_upload_conflict', message: 'forced upload conflict' } }),
      })
    })
    await loginAt(page, baseUrl)
    await page.locator('input[type=file]').first().setInputFiles({ name, mimeType: 'text/plain', buffer })
    await expect.poll(() => state.attempts, { timeout: 15_000 }).toBe(5)
  }

  try {
    await Promise.all([
      exercise(oldPage, oldUrl, oldState),
      exercise(newPage, newUrl, newState),
    ])
  } finally {
    await Promise.all([
      oldState.uploadId ? oldPage.evaluate(async id => { await fetch(`/api/uploads/${id}`, { method: 'DELETE' }) }, oldState.uploadId) : Promise.resolve(),
      newState.uploadId ? newPage.evaluate(async id => { await fetch(`/api/uploads/${id}`, { method: 'DELETE' }) }, newState.uploadId) : Promise.resolve(),
    ])
    await Promise.all([oldContext.close(), newContext.close()])
  }
})

test('old/new 文件选择的空输入与同名重复结果保持 reference 行为', async ({ browser }) => {
  const name = `upload-duplicate-reference-${crypto.randomUUID()}.txt`
  const oldUrl = process.env.E2E_REFERENCE_URL || 'http://127.0.0.1:18080'
  const newUrl = process.env.E2E_NEW_URL || 'http://127.0.0.1:18084'
  const oldContext = await browser.newContext({ viewport: { width: 1440, height: 900 } })
  const newContext = await browser.newContext({ viewport: { width: 1440, height: 900 } })
  const oldPage = await oldContext.newPage()
  const newPage = await newContext.newPage()

  async function exercise(page: Parameters<typeof login>[0], baseUrl: string) {
    const uploadResponses: number[] = []
    page.on('response', response => {
      if (new URL(response.url()).pathname === '/api/uploads' && response.request().method() === 'POST') {
        uploadResponses.push(response.status())
      }
    })
    await loginAt(page, baseUrl)
    const input = page.locator('input[type=file]').first()
    await input.setInputFiles([])
    await new Promise(resolve => setTimeout(resolve, 250))
    expect(uploadResponses, '空文件选择不应创建 upload session').toEqual([])
    await input.setInputFiles([
      { name, mimeType: 'text/plain', buffer: Buffer.from('first duplicate\n') },
      { name, mimeType: 'text/plain', buffer: Buffer.from('second duplicate\n') },
    ])
    await expect.poll(() => uploadResponses.length, { timeout: 15_000 }).toBe(2)
    await expect.poll(() => page.evaluate(async fileName => {
      const response = await fetch('/api/files/00000000-0000-0000-0000-000000000000/children')
      if (!response.ok) return -1
      const payload = await response.json() as { items?: Array<{ name: string; status?: string }> }
      return (payload.items ?? []).filter(item => item.name === fileName).length
    }, name), { timeout: 20_000 }).toBe(1)
    return [...uploadResponses].sort((a, b) => a - b)
  }

  try {
    const [oldResponses, newResponses] = await Promise.all([exercise(oldPage, oldUrl), exercise(newPage, newUrl)])
    expect(oldResponses).toEqual([201, 409])
    expect(newResponses, 'Rust 重复文件上传响应与 reference 不一致').toEqual(oldResponses)
  } finally {
    await Promise.all([removeCreated(oldPage, [name]), removeCreated(newPage, [name])])
    await Promise.all([oldContext.close(), newContext.close()])
  }
})

test('old/new 单文件上传没有 ETag 时保留 reference 的完成行为', async ({ browser }) => {
  const name = `upload-no-etag-reference-${crypto.randomUUID()}.txt`
  const buffer = Buffer.from('upload without etag reference\n')
  const oldUrl = process.env.E2E_REFERENCE_URL || 'http://127.0.0.1:18080'
  const newUrl = process.env.E2E_NEW_URL || 'http://127.0.0.1:18084'
  const oldContext = await browser.newContext({ viewport: { width: 1440, height: 900 } })
  const newContext = await browser.newContext({ viewport: { width: 1440, height: 900 } })
  const oldPage = await oldContext.newPage()
  const newPage = await newContext.newPage()
  const oldState = { uploadId: '' }
  const newState = { uploadId: '' }

  async function removeUpload(page: Parameters<typeof login>[0], uploadId: string) {
    if (uploadId) await page.evaluate(async id => { await fetch(`/api/uploads/${id}`, { method: 'DELETE' }) }, uploadId)
  }

  async function stripEtag(page: Parameters<typeof login>[0], state: typeof oldState) {
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
      const response = await route.fetch()
      const headers = Object.fromEntries(Object.entries(response.headers()).filter(([key]) => key.toLowerCase() !== 'etag'))
      await route.fulfill({ status: response.status(), headers, body: await response.body() })
    })
  }

  async function exercise(page: Parameters<typeof login>[0], baseUrl: string) {
    await loginAt(page, baseUrl)
    await page.locator('input[type=file]').first().setInputFiles({ name, mimeType: 'text/plain', buffer })
    await expect.poll(() => page.evaluate(async fileName => {
      const response = await fetch('/api/files/00000000-0000-0000-0000-000000000000/children')
      if (!response.ok) return false
      const payload = await response.json() as { items?: Array<{ name: string; status?: string }> }
      return (payload.items ?? []).some(item => item.name === fileName && item.status === 'ready')
    }, name), { timeout: 20_000 }).toBe(true)
  }

  try {
    await Promise.all([
      stripEtag(oldPage, oldState),
      stripEtag(newPage, newState),
      exercise(oldPage, oldUrl),
      exercise(newPage, newUrl),
    ])
  } finally {
    await Promise.all([
      removeUpload(oldPage, oldState.uploadId),
      removeUpload(newPage, newState.uploadId),
      removeCreated(oldPage, [name]),
      removeCreated(newPage, [name]),
    ])
    await Promise.all([oldContext.close(), newContext.close()])
  }
})

test('old/new multipart 上传按 reference 请求分片、记录校验并按序完成', async ({ browser }) => {
  const name = 'upload-multipart-reference-' + crypto.randomUUID() + '.bin'
  const partSize = 8 * 1024 * 1024
  const fileSize = partSize * 2 + 5
  const buffer = Buffer.alloc(fileSize, 0x61)
  const oldUrl = process.env.E2E_REFERENCE_URL || 'http://127.0.0.1:18080'
  const newUrl = process.env.E2E_NEW_URL || 'http://127.0.0.1:18084'
  const oldContext = await browser.newContext({ viewport: { width: 1440, height: 900 } })
  const newContext = await browser.newContext({ viewport: { width: 1440, height: 900 } })
  const oldPage = await oldContext.newPage()
  const newPage = await newContext.newPage()

  type UploadCreate = { parent_id?: string; name?: string; size?: number; mime_type?: string }
  type RecordedPart = { part_number: number; etag: string; size: number; content_hash?: string }
  type MultipartState = {
    create: UploadCreate | null
    partRequests: number[][]
    data: Array<{ part: number; size: number; contentType: string }>
    records: RecordedPart[]
    complete: Array<{ part_number: number; etag: string }> | null
  }

  async function exercise(page: Parameters<typeof login>[0], baseUrl: string, uploadId: string) {
    const state: MultipartState = { create: null, partRequests: [], data: [], records: [], complete: null }
    await page.route(/\/api\/uploads(?:\/|$)/, async route => {
      const request = route.request()
      const url = new URL(request.url())
      const pathName = url.pathname
      if (pathName === '/api/uploads' && request.method() === 'POST') {
        state.create = request.postDataJSON() as UploadCreate
        return route.fulfill({
          status: 201,
          json: {
            upload_id: uploadId,
            file_id: 'file-' + uploadId,
            mode: 'multipart',
            url: '',
            part_size: partSize,
            part_count: 3,
            expires_at: '2026-12-01T00:00:00Z',
          },
        })
      }
      const partsPath = pathName === '/api/uploads/' + uploadId + '/parts'
      if (partsPath && request.method() === 'POST') {
        const body = request.postDataJSON() as { part_numbers?: number[] }
        const numbers = body.part_numbers ?? []
        state.partRequests.push(numbers)
        return route.fulfill({
          json: {
            parts: numbers.map(part => ({ part_number: part, url: '/api/uploads/' + uploadId + '/data/' + part })),
          },
        })
      }
      const dataMatch = pathName.match(new RegExp('^/api/uploads/' + uploadId + '/data/(\\d+)$'))
      if (dataMatch && request.method() === 'PUT') {
        state.data.push({
          part: Number(dataMatch[1]),
          size: request.postDataBuffer()?.length ?? 0,
          contentType: request.headers()['content-type'] ?? '',
        })
        return route.fulfill({ status: 200, headers: { ETag: 'etag-' + dataMatch[1] }, body: '' })
      }
      const recordMatch = pathName.match(new RegExp('^/api/uploads/' + uploadId + '/parts/(\\d+)$'))
      if (recordMatch && request.method() === 'PUT') {
        const body = request.postDataJSON() as RecordedPart
        state.records.push({ ...body, part_number: Number(recordMatch[1]) })
        return route.fulfill({ status: 204, body: '' })
      }
      const completePath = '/api/uploads/' + uploadId + '/complete'
      if (pathName === completePath && request.method() === 'POST') {
        const body = request.postDataJSON() as { parts?: Array<{ part_number: number; etag: string }> }
        state.complete = body.parts ?? []
        return route.fulfill({
          json: {
            id: 'file-' + uploadId,
            parent_id: '00000000-0000-0000-0000-000000000000',
            name,
            kind: 'file',
            size: fileSize,
            mime_type: 'application/octet-stream',
            status: 'ready',
            created_at: '2026-01-01T00:00:00Z',
            updated_at: '2026-01-01T00:00:00Z',
          },
        })
      }
      if (pathName === '/api/uploads/' + uploadId && request.method() === 'DELETE') {
        return route.fulfill({ status: 204, body: '' })
      }
      return route.continue()
    })

    await loginAt(page, baseUrl)
    await page.locator('input[type=file]').first().setInputFiles({
      name,
      mimeType: 'application/octet-stream',
      buffer,
      lastModified: 123,
    })
    await expect.poll(() => state.complete !== null, { timeout: 30_000 }).toBe(true)
    await expect.poll(() => page.locator('.file-card, .file-row').filter({ hasText: name }).count(), { timeout: 2_000 }).toBe(0)
    return {
      create: state.create,
      partRequests: state.partRequests,
      data: [...state.data].sort((a, b) => a.part - b.part),
      records: [...state.records]
        .sort((a, b) => a.part_number - b.part_number)
        .map(({ part_number, etag, size, content_hash }) => ({
          part_number,
          etag,
          size,
          ...(content_hash === undefined ? {} : { content_hash }),
        })),
      complete: [...(state.complete ?? [])].sort((a, b) => a.part_number - b.part_number),
    }
  }

  try {
    const [oldResult, newResult] = await Promise.all([
      exercise(oldPage, oldUrl, 'multipart-old'),
      exercise(newPage, newUrl, 'multipart-new'),
    ])
    expect(oldResult.create).toEqual({
      parent_id: '00000000-0000-0000-0000-000000000000',
      name,
      size: fileSize,
      mime_type: 'application/octet-stream',
    })
    expect(oldResult.partRequests).toEqual([[1, 2, 3]])
    expect(oldResult.data).toEqual([
      { part: 1, size: partSize, contentType: '' },
      { part: 2, size: partSize, contentType: '' },
      { part: 3, size: 5, contentType: '' },
    ])
    expect(oldResult.records).toEqual([
      { part_number: 1, etag: 'etag-1', size: partSize },
      { part_number: 2, etag: 'etag-2', size: partSize },
      { part_number: 3, etag: 'etag-3', size: 5 },
    ])
    expect(oldResult.complete).toEqual([
      { part_number: 1, etag: 'etag-1' },
      { part_number: 2, etag: 'etag-2' },
      { part_number: 3, etag: 'etag-3' },
    ])
    expect(newResult, 'Rust multipart 请求与 reference 不一致').toEqual(oldResult)
  } finally {
    await Promise.all([oldContext.close(), newContext.close()])
  }
})

test('old/new 刷新后用 revaro.uploads.v1 恢复未完成 multipart 上传', async ({ browser }) => {
  const name = 'upload-resume-reference-' + crypto.randomUUID() + '.bin'
  const partSize = 8 * 1024 * 1024
  const fileSize = partSize * 2 + 5
  const buffer = Buffer.alloc(fileSize, 0x62)
  const oldUrl = process.env.E2E_REFERENCE_URL || 'http://127.0.0.1:18080'
  const newUrl = process.env.E2E_NEW_URL || 'http://127.0.0.1:18084'
  const oldContext = await browser.newContext({ viewport: { width: 1440, height: 900 } })
  const newContext = await browser.newContext({ viewport: { width: 1440, height: 900 } })
  const oldPage = await oldContext.newPage()
  const newPage = await newContext.newPage()

  type ResumeState = {
    getCalls: number
    createCalls: number
    partRequests: number[][]
    data: Array<{ part: number; size: number }>
    records: Array<{ part_number: number; etag: string; size: number }>
    complete: Array<{ part_number: number; etag: string }> | null
  }

  async function exercise(page: Parameters<typeof login>[0], baseUrl: string, uploadId: string) {
    const state: ResumeState = { getCalls: 0, createCalls: 0, partRequests: [], data: [], records: [], complete: null }
    await page.route(/\/api\/uploads(?:\/|$)/, async route => {
      const request = route.request()
      const pathName = new URL(request.url()).pathname
      if (pathName === '/api/uploads' && request.method() === 'POST') {
        state.createCalls += 1
        return route.fulfill({ status: 500, body: 'resume must not create a new session' })
      }
      if (pathName === '/api/uploads/' + uploadId && request.method() === 'GET') {
        state.getCalls += 1
        return route.fulfill({
          json: {
            upload_id: uploadId,
            file_id: 'file-' + uploadId,
            mode: 'multipart',
            url: '',
            part_size: partSize,
            part_count: 3,
            expected_size: fileSize,
            mime_type: 'application/octet-stream',
            status: 'pending',
            expires_at: '2026-12-01T00:00:00Z',
            parts: [
              { part_number: 1, size: partSize, etag: 'saved-etag-1', content_hash: '' },
              { part_number: 2, size: partSize, etag: 'saved-etag-2', content_hash: '' },
            ],
          },
        })
      }
      if (pathName === '/api/uploads/' + uploadId + '/parts' && request.method() === 'POST') {
        const body = request.postDataJSON() as { part_numbers?: number[] }
        const numbers = body.part_numbers ?? []
        state.partRequests.push(numbers)
        return route.fulfill({
          json: {
            parts: numbers.map(part => ({ part_number: part, url: '/api/uploads/' + uploadId + '/data/' + part })),
          },
        })
      }
      const dataMatch = pathName.match(new RegExp('^/api/uploads/' + uploadId + '/data/(\\d+)$'))
      if (dataMatch && request.method() === 'PUT') {
        state.data.push({ part: Number(dataMatch[1]), size: request.postDataBuffer()?.length ?? 0 })
        return route.fulfill({ status: 200, headers: { ETag: 'resumed-etag-3' }, body: '' })
      }
      const recordMatch = pathName.match(new RegExp('^/api/uploads/' + uploadId + '/parts/(\\d+)$'))
      if (recordMatch && request.method() === 'PUT') {
        const body = request.postDataJSON() as { etag?: string; size?: number }
        state.records.push({
          part_number: Number(recordMatch[1]),
          etag: body.etag ?? '',
          size: body.size ?? 0,
        })
        return route.fulfill({ status: 204, body: '' })
      }
      if (pathName === '/api/uploads/' + uploadId + '/complete' && request.method() === 'POST') {
        const body = request.postDataJSON() as { parts?: Array<{ part_number: number; etag: string }> }
        state.complete = body.parts ?? []
        return route.fulfill({
          json: {
            id: 'file-' + uploadId,
            parent_id: '00000000-0000-0000-0000-000000000000',
            name,
            kind: 'file',
            size: fileSize,
            mime_type: 'application/octet-stream',
            status: 'ready',
            created_at: '2026-01-01T00:00:00Z',
            updated_at: '2026-01-01T00:00:00Z',
          },
        })
      }
      return route.continue()
    })

    await loginAt(page, baseUrl)
    await page.evaluate(({ uploadId: id, uploadName, size }) => {
      localStorage.setItem('revaro.uploads.v1', JSON.stringify([
        { uploadId: 42, parentId: '', name: '', size: -1, lastModified: 'bad' },
        {
          uploadId: id,
          parentId: '00000000-0000-0000-0000-000000000000',
          name: uploadName,
          size,
          lastModified: 123,
        },
      ]))
    }, { uploadId, uploadName: name, size: fileSize })
    await page.evaluate(({ uploadName, size }) => {
      const input = document.querySelector('input[type="file"]')
      if (!(input instanceof HTMLInputElement)) throw new Error('file input is missing')
      const transfer = new DataTransfer()
      transfer.items.add(new File([new Uint8Array(size)], uploadName, {
        type: 'application/octet-stream',
        lastModified: 123,
      }))
      input.files = transfer.files
      input.dispatchEvent(new Event('change', { bubbles: true }))
    }, { uploadName: name, size: fileSize })
    await expect.poll(() => state.complete !== null, { timeout: 30_000 }).toBe(true)
    return {
      getCalls: state.getCalls,
      createCalls: state.createCalls,
      partRequests: state.partRequests,
      data: [...state.data].sort((a, b) => a.part - b.part),
      records: [...state.records].sort((a, b) => a.part_number - b.part_number),
      complete: [...(state.complete ?? [])].sort((a, b) => a.part_number - b.part_number),
    }
  }

  try {
    const [oldResult, newResult] = await Promise.all([
      exercise(oldPage, oldUrl, 'resume-old'),
      exercise(newPage, newUrl, 'resume-new'),
    ])
    expect(oldResult).toEqual({
      getCalls: 1,
      createCalls: 0,
      partRequests: [[3]],
      data: [{ part: 3, size: 5 }],
      records: [{ part_number: 3, etag: 'resumed-etag-3', size: 5 }],
      complete: [
        { part_number: 1, etag: 'saved-etag-1', size: partSize, content_hash: '' },
        { part_number: 2, etag: 'saved-etag-2', size: partSize, content_hash: '' },
        { part_number: 3, etag: 'resumed-etag-3' },
      ],
    })
    expect(newResult, 'Rust 断点续传请求与 reference 不一致').toEqual(oldResult)
  } finally {
    await Promise.all([oldContext.close(), newContext.close()])
  }
})

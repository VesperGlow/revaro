import { expect, test, type Page } from '@playwright/test'

const ROOT = '00000000-0000-0000-0000-000000000000'

async function loginAt(page: Page, baseUrl: string) {
  await page.goto(`${baseUrl}/?upload-cancel-reference=${Date.now()}`)
  await page.getByLabel('用户名').fill(process.env.E2E_USERNAME || 'admin')
  await page.getByLabel('密码').fill(process.env.E2E_PASSWORD || 'revaro-e2e-password')
  await page.getByRole('button', { name: '进入我的网盘' }).click()
  await expect(page.getByRole('heading', { name: '我的文件', exact: true })).toBeVisible()
}

async function exercise(page: Page, baseUrl: string) {
  const name = `upload-cancel-reference-${crypto.randomUUID()}.bin`
  const requests: string[] = []
  let uploadId = ''
  let dataAbort = false
  let releaseEvents!: () => void
  const eventsReady = new Promise<void>(resolve => { releaseEvents = resolve })

  page.on('request', request => {
    const url = new URL(request.url())
    if (request.method() !== 'GET' && (url.pathname === '/api/uploads' || url.pathname.match(/^\/api\/uploads\/[^/]+(?:\/data)?$/))) {
      requests.push(`${request.method()} ${url.pathname}`)
    }
  })
  page.on('requestfailed', request => {
    const url = new URL(request.url())
    if (url.pathname.match(/^\/api\/uploads\/[^/]+\/data$/)) dataAbort = true
  })
  await page.route(/\/api\/events$/, async route => {
    await eventsReady
    await route.fulfill({ contentType: 'text/event-stream', body: 'event: jobs\ndata: {}\n\n' })
  })
  await page.route(/\/api\/uploads\/[^/]+\/data$/, async route => {
    if (route.request().method() !== 'PUT') {
      await route.continue()
      return
    }
    try {
      await new Promise(resolve => setTimeout(resolve, 5_000))
      const response = await route.fetch()
      await route.fulfill({ response })
    } catch {
      // Cancelling the local XHR intentionally interrupts this route.
    }
  })

  await loginAt(page, baseUrl)
  const created = page.waitForResponse(response => {
    const url = new URL(response.url())
    return url.pathname === '/api/uploads' && response.request().method() === 'POST' && response.ok()
  })
  const dataStarted = page.waitForRequest(request => {
    const url = new URL(request.url())
    return url.pathname.match(/^\/api\/uploads\/[^/]+\/data$/) !== null && request.method() === 'PUT'
  })
  await page.locator('input[type=file]').first().setInputFiles({
    name,
    mimeType: 'application/octet-stream',
    buffer: Buffer.from('upload cancellation reference\n'),
  })
  uploadId = (await (await created).json() as { upload_id: string }).upload_id
  await dataStarted
  releaseEvents()
  await expect.poll(() => page.evaluate(async ({ uploadId: wanted }) => {
    const response = await fetch('/api/tasks')
    if (!response.ok) return false
    const payload = await response.json() as { items?: Array<{ source_type?: string; source_id?: string; status?: string }> }
    return (payload.items ?? []).some(task => task.source_type === 'upload' && task.source_id === wanted && ['queued', 'running'].includes(task.status || ''))
  }, { uploadId }), { timeout: 10_000 }).toBe(true)

  await page.getByTitle('任务中心').click()
  const row = page.locator('.active-group article, .task-group-row').filter({ hasText: name })
  await expect(row).toBeVisible({ timeout: 10_000 })
  await row.getByRole('button', { name: '取消' }).click()
  await expect.poll(() => page.evaluate(async id => {
    const response = await fetch(`/api/uploads/${id}`)
    return response.status
  }, uploadId), { timeout: 10_000 }).toBe(404)
  await expect.poll(() => dataAbort, { timeout: 2_000 }).toBe(true)
  await page.waitForTimeout(500)

  return {
    requests: requests.map(request => request.replace(/[0-9a-f-]{36}/g, ':id')),
    dataAbort,
  }
}

test('传输中从任务中心取消上传先中止本地请求再删除 reference session', async ({ browser }) => {
  const oldUrl = process.env.E2E_REFERENCE_URL || 'http://127.0.0.1:18080'
  const newUrl = process.env.E2E_NEW_URL || 'http://127.0.0.1:18084'
  const oldContext = await browser.newContext({ viewport: { width: 1440, height: 900 } })
  const newContext = await browser.newContext({ viewport: { width: 1440, height: 900 } })
  const oldPage = await oldContext.newPage()
  const newPage = await newContext.newPage()

  try {
    const [oldState, newState] = await Promise.all([
      exercise(oldPage, oldUrl),
      exercise(newPage, newUrl),
    ])
    expect(oldState).toEqual({
      requests: [
        'POST /api/uploads',
        'PUT /api/uploads/:id/data',
        'DELETE /api/uploads/:id',
      ],
      dataAbort: true,
    })
    expect(newState, 'Rust 传输中取消未保持 reference 的 XHR/session 顺序').toEqual(oldState)
  } finally {
    await Promise.all([oldContext.close(), newContext.close()])
  }
})

test('取消上传后再次选择同一文件按 reference 重新创建并完成 session', async ({ browser }) => {
  const oldUrl = process.env.E2E_REFERENCE_URL || 'http://127.0.0.1:18080'
  const newUrl = process.env.E2E_NEW_URL || 'http://127.0.0.1:18084'
  const name = `upload-cancel-reselect-${crypto.randomUUID()}.txt`
  const buffer = Buffer.from('cancel then select again\n')
  const oldContext = await browser.newContext({ viewport: { width: 1440, height: 900 } })
  const newContext = await browser.newContext({ viewport: { width: 1440, height: 900 } })
  const oldPage = await oldContext.newPage()
  const newPage = await newContext.newPage()

  async function exercise(page: Page, baseUrl: string) {
    const calls: string[] = []
    const uploadIds: string[] = []
    let firstDataAbort = false
    let releaseEvents!: () => void
    const eventsReady = new Promise<void>(resolve => { releaseEvents = resolve })
    let releaseDataStarted!: () => void
    const dataStarted = new Promise<void>(resolve => { releaseDataStarted = resolve })

    page.on('request', request => {
      const url = new URL(request.url())
      if (request.method() === 'POST' && (url.pathname === '/api/uploads' || url.pathname.match(/^\/api\/uploads\/[^/]+\/complete$/))) {
        calls.push(`POST ${url.pathname}`)
      } else if (request.method() === 'PUT' && url.pathname.match(/^\/api\/uploads\/[^/]+\/data$/)) {
        calls.push(`PUT ${url.pathname}`)
        if (uploadIds.length === 1) releaseDataStarted()
      } else if (request.method() === 'DELETE' && url.pathname.match(/^\/api\/uploads\/[^/]+$/)) {
        calls.push(`DELETE ${url.pathname}`)
      }
    })
    page.on('requestfailed', request => {
      const url = new URL(request.url())
      if (uploadIds[0] && url.pathname === `/api/uploads/${uploadIds[0]}/data`) firstDataAbort = true
    })
    await page.route(/\/api\/events$/, async route => {
      await eventsReady
      await route.fulfill({ contentType: 'text/event-stream', body: 'event: jobs\ndata: {}\n\n' })
    })
    await page.route(/\/api\/uploads$/, async route => {
      if (route.request().method() !== 'POST') {
        await route.continue()
        return
      }
      const response = await route.fetch()
      if (response.ok()) {
        uploadIds.push(String((await response.json() as { upload_id: string }).upload_id))
      }
      await route.fulfill({ response })
    })
    await page.route(/\/api\/uploads\/[^/]+\/data$/, async route => {
      if (route.request().method() !== 'PUT') {
        await route.continue()
        return
      }
      const id = new URL(route.request().url()).pathname.split('/')[3]
      if (id !== uploadIds[0]) {
        await route.continue()
        return
      }
      try {
        await new Promise(resolve => setTimeout(resolve, 5_000))
        const response = await route.fetch()
        await route.fulfill({ response })
      } catch {
        // The first transfer is intentionally aborted by the task centre.
      }
    })

    await loginAt(page, baseUrl)
    const file = { name, mimeType: 'text/plain', buffer }
    await page.locator('input[type=file]').first().setInputFiles(file)
    await expect.poll(() => uploadIds.length, { timeout: 10_000 }).toBe(1)
    const firstUploadId = uploadIds[0]
    await dataStarted
    releaseEvents()
    await expect.poll(() => page.evaluate(async id => {
      const response = await fetch('/api/tasks')
      if (!response.ok) return false
      const payload = await response.json() as { items?: Array<{ source_type?: string; source_id?: string; status?: string }> }
      return (payload.items ?? []).some(task => task.source_type === 'upload' && task.source_id === id && ['queued', 'running'].includes(task.status || ''))
    }, firstUploadId), { timeout: 10_000 }).toBe(true)
    await expect.poll(() => page.evaluate(async id => {
      const response = await fetch(`/api/uploads/${id}`)
      return response.status
    }, firstUploadId), { timeout: 10_000 }).toBe(200)

    await page.getByTitle('任务中心').click()
    const row = page.locator('.active-group article, .task-group-row').filter({ hasText: name })
    await expect(row).toBeVisible({ timeout: 10_000 })
    await row.getByRole('button', { name: '取消' }).click()
    await expect.poll(() => page.evaluate(async id => {
      const response = await fetch(`/api/uploads/${id}`)
      return response.status
    }, firstUploadId), { timeout: 10_000 }).toBe(404)
    await expect.poll(() => firstDataAbort, { timeout: 2_000 }).toBe(true)

    await page.locator('input[type=file]').first().setInputFiles(file)
    await expect.poll(() => uploadIds.length, { timeout: 10_000 }).toBe(2)
    await expect.poll(() => page.evaluate(async wanted => {
      const response = await fetch('/api/files/00000000-0000-0000-0000-000000000000/children')
      if (!response.ok) return false
      const payload = await response.json() as { items?: Array<{ id: string; name: string; status?: string }> }
      return (payload.items ?? []).some(item => item.id && item.name === wanted && item.status === 'ready')
    }, name), { timeout: 20_000 }).toBe(true)

    const normalized = calls.map(call => call.replace(/[0-9a-f-]{36}/g, ':id'))
    return { calls: normalized, firstDataAbort }
  }

  async function cleanup(page: Page) {
    await page.evaluate(async wanted => {
      const response = await fetch('/api/files/00000000-0000-0000-0000-000000000000/children')
      if (!response.ok) return
      const payload = await response.json() as { items?: Array<{ id: string; name: string }> }
      for (const item of payload.items ?? []) {
        if (item.name !== wanted) continue
        await fetch(`/api/files/${item.id}`, { method: 'DELETE' })
        await fetch(`/api/trash/${item.id}`, { method: 'DELETE' })
      }
    }, name)
  }

  try {
    const [oldResult, newResult] = await Promise.all([
      exercise(oldPage, oldUrl),
      exercise(newPage, newUrl),
    ])
    expect(oldResult.firstDataAbort).toBe(true)
    expect(oldResult.calls).toEqual([
      'POST /api/uploads',
      'PUT /api/uploads/:id/data',
      'DELETE /api/uploads/:id',
      'POST /api/uploads',
      'PUT /api/uploads/:id/data',
      'POST /api/uploads/:id/complete',
    ])
    expect(newResult, 'Rust 取消后重新选择文件的 session 生命周期与 reference 不一致').toEqual(oldResult)
  } finally {
    await Promise.all([cleanup(oldPage), cleanup(newPage)])
    await Promise.all([oldContext.close(), newContext.close()])
  }
})

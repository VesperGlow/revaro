import { expect, test, type Page } from '@playwright/test'

const ROOT = '00000000-0000-0000-0000-000000000000'
const ZIP_BASE64 = 'UEsDBAoACQAAAEOnL13hAghUKAAAABwAAAAKABwAc2VjcmV0LnR4dFVUCQAD3UCpat1AqWp1eAsAAQToAwAABOgDAAA7bIAwEhh2wIhc1ASfiwu6IsLgqdhIZ6CutzDXLbC7vkM9w7BuLbRxUEsHCOECCFQoAAAAHAAAAFBLAQIeAwoACQAAAEOnL13hAghUKAAAABwAAAAKABgAAAAAAAEAAACkgQAAAABzZWNyZXQudHh0VVQFAAPdQKlqdXgLAAEE6AMAAAToAwAAUEsFBgAAAAABAAEAUAAAAHwAAAAAAA=='

function crc32(data: Buffer) {
  let value = 0xffffffff
  for (const byte of data) {
    value ^= byte
    for (let bit = 0; bit < 8; bit += 1) value = (value >>> 1) ^ (value & 1 ? 0xedb88320 : 0)
  }
  return value ^ 0xffffffff
}

function storedZip(entries: Array<[string, Buffer]>) {
  const local: Buffer[] = []
  const central: Buffer[] = []
  let offset = 0
  for (const [name, data] of entries) {
    const nameBytes = Buffer.from(name)
    const checksum = crc32(data) >>> 0
    const localHeader = Buffer.alloc(30)
    localHeader.writeUInt32LE(0x04034b50, 0)
    localHeader.writeUInt16LE(20, 4)
    localHeader.writeUInt16LE(0x800, 6)
    localHeader.writeUInt32LE(checksum, 14)
    localHeader.writeUInt32LE(data.length, 18)
    localHeader.writeUInt32LE(data.length, 22)
    localHeader.writeUInt16LE(nameBytes.length, 26)
    local.push(Buffer.concat([localHeader, nameBytes, data]))

    const centralHeader = Buffer.alloc(46)
    centralHeader.writeUInt32LE(0x02014b50, 0)
    centralHeader.writeUInt16LE(20, 4)
    centralHeader.writeUInt16LE(20, 6)
    centralHeader.writeUInt16LE(0x800, 8)
    centralHeader.writeUInt32LE(checksum, 16)
    centralHeader.writeUInt32LE(data.length, 20)
    centralHeader.writeUInt32LE(data.length, 24)
    centralHeader.writeUInt16LE(nameBytes.length, 28)
    centralHeader.writeUInt32LE(offset, 42)
    central.push(Buffer.concat([centralHeader, nameBytes]))
    offset += localHeader.length + nameBytes.length + data.length
  }
  const directory = Buffer.concat(central)
  const end = Buffer.alloc(22)
  end.writeUInt32LE(0x06054b50, 0)
  end.writeUInt16LE(entries.length, 8)
  end.writeUInt16LE(entries.length, 10)
  end.writeUInt32LE(directory.length, 12)
  end.writeUInt32LE(offset, 16)
  return Buffer.concat([...local, directory, end])
}

type TaskSnapshot = {
  id: string
  status: string
  phase: string
  progress: number
  error: string
  name: string
  output_id: string
  output_name: string
}

async function login(page: Page, baseUrl: string) {
  await page.goto(`${baseUrl}/?archive-real-reference=${Date.now()}`)
  await page.getByLabel('用户名').fill('admin')
  await page.getByLabel('密码').fill('revaro-e2e-password')
  await page.getByRole('button', { name: '进入我的网盘' }).click()
  await expect(page.getByRole('heading', { name: '我的文件', exact: true })).toBeVisible()
}

async function uploadArchive(page: Page, name: string, bytes: Buffer) {
  return page.evaluate(async ({ name, bytes, root }) => {
    const jsonHeaders = { 'Content-Type': 'application/json' }
    const created = await fetch('/api/uploads', {
      method: 'POST',
      headers: jsonHeaders,
      body: JSON.stringify({ parent_id: root, name, size: bytes.length, mime_type: 'application/zip' }),
    })
    if (!created.ok) throw new Error(`create upload failed: ${created.status}`)
    const session = await created.json() as { upload_id: string; url: string; mode: string }
    if (session.mode !== 'single') throw new Error(`unexpected upload mode: ${session.mode}`)
    const uploaded = await fetch(session.url, {
      method: 'PUT',
      headers: { 'Content-Type': 'application/zip' },
      body: new Uint8Array(bytes),
    })
    if (!uploaded.ok) throw new Error(`upload bytes failed: ${uploaded.status}`)
    const completed = await fetch(`/api/uploads/${session.upload_id}/complete`, {
      method: 'POST',
      headers: jsonHeaders,
      body: JSON.stringify({ parts: [] }),
    })
    if (!completed.ok) throw new Error(`complete upload failed: ${completed.status}`)
    const file = await completed.json() as { id: string }
    return { id: file.id, uploadId: session.upload_id }
  }, { name, bytes: Array.from(bytes), root: ROOT })
}

async function taskSnapshot(page: Page, name: string): Promise<TaskSnapshot | null> {
  return page.evaluate(async wanted => {
    const response = await fetch('/api/tasks')
    if (!response.ok) return null
    const payload = await response.json() as { items?: Array<Partial<TaskSnapshot> & { type?: string; source_type?: string }> }
    const task = (payload.items ?? []).find(item => item.type === 'archive_extract' && item.name === wanted)
    if (!task) return null
    return {
      id: task.id ?? '',
      status: task.status ?? '',
      phase: task.phase ?? '',
      progress: task.progress ?? 0,
      error: task.error ?? '',
      name: task.name ?? '',
      output_id: task.output_id ?? '',
      output_name: task.output_name ?? '',
    }
  }, name)
}

async function waitForTask(page: Page, name: string, status: string) {
  await expect.poll(async () => (await taskSnapshot(page, name))?.status ?? '', { timeout: 30_000 }).toBe(status)
  return (await taskSnapshot(page, name))!
}

async function waitForRootItem(page: Page, name: string) {
  await expect.poll(() => page.evaluate(async ({ wanted, root }) => {
    const response = await fetch(`/api/files/${root}/children`)
    if (!response.ok) return false
    const payload = await response.json() as { items?: Array<{ id: string; name: string; status?: string }> }
    return (payload.items ?? []).some(item => item.name === wanted && item.status === 'ready')
  }, { wanted: name, root: ROOT }), { timeout: 30_000 }).toBe(true)
}

async function startExtraction(page: Page, name: string) {
  await waitForRootItem(page, name)
  await page.reload()
  await expect(page.getByRole('heading', { name: '我的文件', exact: true })).toBeVisible()
  await page.getByRole('button', { name: '列表' }).click()
  const row = page.locator('.file-row').filter({ hasText: name })
  await expect(row).toBeVisible()
  await row.getByRole('button', { name: '选择项目' }).click()
  const toolbar = page.getByRole('toolbar', { name: '所选项目操作' })
  await toolbar.getByRole('button', { name: '在线解压' }).click()
  await page.getByRole('dialog').getByRole('button', { name: '开始解压' }).click()
  await expect.poll(() => page.locator('.toast').textContent().catch(() => ''), { timeout: 10_000 }).toContain(name)
}

async function openPasswordDialog(page: Page, name: string) {
  await page.getByTitle('任务中心').click()
  const row = page.locator('.active-group article').filter({ hasText: name })
  await expect(row).toBeVisible()
  await row.getByTitle('输入密码').click()
  const dialog = page.locator('.input-dialog')
  await expect(dialog).toBeVisible()
  return {
    title: await dialog.locator('strong').textContent(),
    name: await dialog.locator('small').textContent(),
    inputType: await dialog.locator('input').getAttribute('type'),
    buttons: await dialog.locator('footer button').evaluateAll(buttons => buttons.map(button => ({
      text: button.textContent?.replace(/\s+/g, ' ').trim(),
      disabled: (button as HTMLButtonElement).disabled,
    }))),
  }
}

async function cleanup(page: Page, names: string[]) {
  await page.evaluate(async ({ wanted, root }) => {
    const headers = { 'Content-Type': 'application/json' }
    const taskResponse = await fetch('/api/tasks')
    if (taskResponse.ok) {
      const taskPayload = await taskResponse.json() as { items?: Array<{ id: string; type?: string; status?: string; name?: string }> }
      for (const task of taskPayload.items ?? []) {
        if (task.type !== 'archive_extract' || !task.id || !wanted.includes(task.name ?? '')) continue
        if (!['queued', 'running', 'retrying', 'waiting_input'].includes(task.status ?? '')) continue
        await fetch(`/api/tasks/${task.id}/cancel`, { method: 'POST', headers })
      }
    }
    const response = await fetch(`/api/files/${root}/children`)
    if (!response.ok) return
    const payload = await response.json() as { items?: Array<{ id: string; name: string }> }
    for (const item of payload.items ?? []) {
      if (!wanted.includes(item.name)) continue
      await fetch(`/api/files/${item.id}`, { method: 'DELETE', headers })
      await fetch(`/api/trash/${item.id}`, { method: 'DELETE', headers })
    }
  }, { wanted: names, root: ROOT })
}

test('old/new 真实未加密 ZIP 的完整解压结果保持 reference 行为', async ({ browser }) => {
  test.setTimeout(90_000)
  const oldUrl = process.env.E2E_REFERENCE_URL || 'http://127.0.0.1:18080'
  const newUrl = process.env.E2E_NEW_URL || 'http://127.0.0.1:18084'
  const suffix = crypto.randomUUID()
  const name = `archive-real-reference-${suffix}.zip`
  const outputName = name.replace(/\.zip$/, '')
  const entryName = `secret-${suffix}.txt`
  const bytes = storedZip([[entryName, Buffer.from(`archive reference ${suffix}\n`)]])
  const oldContext = await browser.newContext({ viewport: { width: 1440, height: 950 } })
  const newContext = await browser.newContext({ viewport: { width: 1440, height: 950 } })
  const oldPage = await oldContext.newPage()
  const newPage = await newContext.newPage()

  try {
    await Promise.all([login(oldPage, oldUrl), login(newPage, newUrl)])
    const [oldFile, newFile] = await Promise.all([
      uploadArchive(oldPage, name, bytes),
      uploadArchive(newPage, name, bytes),
    ])
    expect(oldFile.id).not.toBe('')
    expect(newFile.id).not.toBe('')
    await Promise.all([startExtraction(oldPage, name), startExtraction(newPage, name)])
    const [oldWaiting, newWaiting] = await Promise.all([
      waitForTask(oldPage, name, 'completed'),
      waitForTask(newPage, name, 'completed'),
    ])
    expect(newWaiting && { status: newWaiting.status, phase: newWaiting.phase, progress: newWaiting.progress, error: newWaiting.error, name: newWaiting.name })
      .toEqual(oldWaiting && { status: oldWaiting.status, phase: oldWaiting.phase, progress: oldWaiting.progress, error: oldWaiting.error, name: oldWaiting.name })

    const [oldChildren, newChildren] = await Promise.all([
      oldPage.evaluate(async ({ name, root }) => {
        const response = await fetch(`/api/files/${root}/children`)
        const payload = await response.json() as { items?: Array<{ id: string; name: string; size: number; kind: string; status?: string }> }
        const output = (payload.items ?? []).find(item => item.name === name && item.status === 'ready')
        if (!output) throw new Error('old extraction output directory not found')
        return (await (await fetch(`/api/files/${output.id}/children`)).json()).items
      }, { name: outputName, root: ROOT }),
      newPage.evaluate(async ({ name, root }) => {
        const response = await fetch(`/api/files/${root}/children`)
        const payload = await response.json() as { items?: Array<{ id: string; name: string; size: number; kind: string; status?: string }> }
        const output = (payload.items ?? []).find(item => item.name === name && item.status === 'ready')
        if (!output) throw new Error('new extraction output directory not found')
        return (await (await fetch(`/api/files/${output.id}/children`)).json()).items
      }, { name: outputName, root: ROOT }),
    ])
    expect(newChildren.map((item: { name: string; size: number; kind: string }) => ({ name: item.name, size: item.size, kind: item.kind })))
      .toEqual(oldChildren.map((item: { name: string; size: number; kind: string }) => ({ name: item.name, size: item.size, kind: item.kind })))
  } finally {
    await Promise.all([
      cleanup(oldPage, [name, outputName]),
      cleanup(newPage, [name, outputName]),
    ])
    await Promise.all([oldContext.close(), newContext.close()])
  }
})

test('old/new 真实加密 ZIP 的等待密码入口保持 reference 行为', async ({ browser }) => {
  test.setTimeout(60_000)
  const oldUrl = process.env.E2E_REFERENCE_URL || 'http://127.0.0.1:18080'
  const newUrl = process.env.E2E_NEW_URL || 'http://127.0.0.1:18084'
  const name = `archive-encrypted-reference-${crypto.randomUUID()}.zip`
  const bytes = Buffer.from(ZIP_BASE64, 'base64')
  const oldContext = await browser.newContext({ viewport: { width: 1440, height: 950 } })
  const newContext = await browser.newContext({ viewport: { width: 1440, height: 950 } })
  const oldPage = await oldContext.newPage()
  const newPage = await newContext.newPage()

  try {
    await Promise.all([login(oldPage, oldUrl), login(newPage, newUrl)])
    await Promise.all([
      uploadArchive(oldPage, name, bytes),
      uploadArchive(newPage, name, bytes),
    ])
    await Promise.all([startExtraction(oldPage, name), startExtraction(newPage, name)])
    const [oldWaiting, newWaiting] = await Promise.all([
      waitForTask(oldPage, name, 'waiting_input'),
      waitForTask(newPage, name, 'waiting_input'),
    ])
    expect(newWaiting && { status: newWaiting.status, phase: newWaiting.phase, progress: newWaiting.progress, error: newWaiting.error, name: newWaiting.name })
      .toEqual(oldWaiting && { status: oldWaiting.status, phase: oldWaiting.phase, progress: oldWaiting.progress, error: oldWaiting.error, name: oldWaiting.name })

    const [oldDialog, newDialog] = await Promise.all([
      openPasswordDialog(oldPage, name),
      openPasswordDialog(newPage, name),
    ])
    expect(newDialog, 'Rust 加密归档密码入口与 reference 不一致').toEqual(oldDialog)
  } finally {
    await Promise.all([
      cleanup(oldPage, [name]),
      cleanup(newPage, [name]),
    ])
    await Promise.all([oldContext.close(), newContext.close()])
  }
})

import { execFileSync } from 'node:child_process'
import { mkdtempSync, readFileSync, rmSync, writeFileSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import { gzipSync } from 'node:zlib'
import { expect, test, type Page } from '@playwright/test'

const ROOT = '00000000-0000-0000-0000-000000000000'
const ZIP_BASE64 = 'UEsDBAoACQAAAEOnL13hAghUKAAAABwAAAAKABwAc2VjcmV0LnR4dFVUCQAD3UCpat1AqWp1eAsAAQToAwAABOgDAAA7bIAwEhh2wIhc1ASfiwu6IsLgqdhIZ6CutzDXLbC7vkM9w7BuLbRxUEsHCOECCFQoAAAAHAAAAFBLAQIeAwoACQAAAEOnL13hAghUKAAAABwAAAAKABgAAAAAAAEAAACkgQAAAABzZWNyZXQudHh0VVQFAAPdQKlqdXgLAAEE6AMAAAToAwAAUEsFBgAAAAABAAEAUAAAAHwAAAAAAA=='
const RAR_BASE64 = 'UmFyIRoHAM+QcwAADQAAAAAAAACEUnQgkDIAFAAAABQAAAADQqLIvrd22j4UMAgApIEAAHRlc3QudHh0gAi3dto+t3baPnRlc3QgdGV4dCBkb2N1bWVudA0KnS90IJAyAAgAAAAIAAAAA3tEybbRTNg+FDAIAP+hAAB0ZXN0bGlua8AI0UzYPlBf2j50ZXN0LnR4dM3gdCCQOgAUAAAAFAAAAANCosi+Y3faPhQwEACkgQAAdGVzdGRpclx0ZXN0LnR4dMDMY3faPmN32j50ZXN0IHRleHQgZG9jdW1lbnQNCqHIdOCQMQAAAAAAAAAAAAMAAAAAY3faPhQwBwDtQQAAdGVzdGRpcsDMY3faPmR32j7m53TgkDYAAAAAAAAAAAADAAAAAJ2r1T4UMAwA7UEAAHRlc3RlbXB0eWRpcoDMnavVPsVd2j7EPXsAQAcA'

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

function tar(entries: Array<[string, Buffer]>) {
  const blocks: Buffer[] = []
  for (const [name, body] of entries) {
    const header = Buffer.alloc(512)
    header.write(name, 0, 100, 'utf8')
    header.write('0000644\0', 100, 8, 'ascii')
    header.write('0000000\0', 108, 8, 'ascii')
    header.write('0000000\0', 116, 8, 'ascii')
    header.write(`${body.length.toString(8).padStart(11, '0')}\0`, 124, 12, 'ascii')
    header.write('00000000000\0', 136, 12, 'ascii')
    header.fill(0x20, 148, 156)
    header.write('0', 156, 1, 'ascii')
    header.write('ustar\0', 257, 6, 'ascii')
    header.write('00', 263, 2, 'ascii')
    const checksum = header.reduce((sum, value) => sum + value, 0)
    header.write(`${checksum.toString(8).padStart(6, '0')}\0 `, 148, 8, 'ascii')
    blocks.push(header, body)
    const padding = (512 - (body.length % 512)) % 512
    if (padding) blocks.push(Buffer.alloc(padding))
  }
  blocks.push(Buffer.alloc(1024))
  return Buffer.concat(blocks)
}

function compress(command: string, body: Buffer) {
  const args = command === 'zstd' ? ['-q', '-c'] : ['-c']
  return execFileSync(command, args, { input: body })
}

function sevenZip(body: Buffer) {
  const root = mkdtempSync(join(tmpdir(), 'revaro-archive-format-'))
  try {
    writeFileSync(join(root, 'entry.txt'), body)
    const output = join(root, 'fixture.7z')
    execFileSync('7z', ['a', '-bd', '-y', output, 'entry.txt'], { cwd: root, stdio: 'ignore' })
    return readFileSync(output)
  } finally {
    rmSync(root, { recursive: true, force: true })
  }
}

function archiveBaseName(name: string) {
  for (const suffix of ['.tar.gz', '.tar.bz2', '.tar.xz', '.tar.zst', '.tgz', '.tbz2', '.tbz', '.txz', '.tzst', '.zip', '.7z', '.rar', '.tar', '.gz', '.bz2', '.xz', '.zst']) {
    if (name.toLowerCase().endsWith(suffix)) return name.slice(0, -suffix.length)
  }
  return name
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

async function waitForTerminalTask(page: Page, name: string) {
  await expect.poll(async () => (await taskSnapshot(page, name))?.status ?? '', { timeout: 30_000 })
    .toMatch(/^(completed|failed|waiting_input|cancelled)$/)
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

async function readyRootNames(page: Page, names: string[]) {
  return page.evaluate(async ({ names, root }) => {
    const response = await fetch(`/api/files/${root}/children`)
    if (!response.ok) throw new Error(`read root children failed: ${response.status}`)
    const payload = await response.json() as { items?: Array<{ name: string; kind: string; status?: string }> }
    return (payload.items ?? [])
      .filter(item => item.kind === 'directory' && item.status === 'ready' && names.includes(item.name))
      .map(item => item.name)
      .sort()
  }, { names, root: ROOT })
}

async function outputSnapshot(page: Page, outputName: string) {
  return page.evaluate(async ({ outputName, root }) => {
    const rootResponse = await fetch(`/api/files/${root}/children`)
    const rootPayload = await rootResponse.json() as { items?: Array<{ id: string; name: string; kind: string; status?: string }> }
    const output = (rootPayload.items ?? []).find(item => item.name === outputName && item.kind === 'directory' && item.status === 'ready')
    if (!output) throw new Error(`output directory not found: ${outputName}`)
    const childrenResponse = await fetch(`/api/files/${output.id}/children`)
    const childrenPayload = await childrenResponse.json() as { items?: Array<{ id: string; name: string; kind: string; size: number; mime_type?: string; status?: string }> }
    const children = []
    for (const item of childrenPayload.items ?? []) {
      let body = ''
      if (item.kind === 'file') {
        const response = await fetch(`/api/files/${item.id}/download`)
        body = Array.from(new Uint8Array(await response.arrayBuffer())).map(value => value.toString(16).padStart(2, '0')).join('')
      }
      children.push({ name: item.name, kind: item.kind, size: item.size, mime_type: item.mime_type ?? '', status: item.status ?? '', body })
    }
    return children.sort((left, right) => left.name.localeCompare(right.name))
  }, { outputName, root: ROOT })
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

async function startExtractionWithoutToast(page: Page, name: string) {
  await waitForRootItem(page, name)
  return page.evaluate(async ({ name, root }) => {
    const childrenResponse = await fetch(`/api/files/${root}/children`)
    const payload = await childrenResponse.json() as { items?: Array<{ id: string; name: string; status?: string }> }
    const file = (payload.items ?? []).find(item => item.name === name && item.status === 'ready')
    if (!file) throw new Error('archive source not found')
    const response = await fetch(`/api/files/${file.id}/extract`, { method: 'POST' })
    return response.status
  }, { name, root: ROOT })
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
    const [oldStart, newStart] = await Promise.all([
      startExtractionWithoutToast(oldPage, name),
      startExtractionWithoutToast(newPage, name),
    ])
    expect(oldStart).toBe(202)
    expect(newStart).toBe(202)
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

    // A second extraction is the reference conflict policy: create a sibling
    // with the next available suffix instead of failing or overwriting.
    await Promise.all([startExtraction(oldPage, name), startExtraction(newPage, name)])
    await Promise.all([
      waitForTask(oldPage, name, 'completed'),
      waitForTask(newPage, name, 'completed'),
    ])
    expect(await readyRootNames(oldPage, [outputName, `${outputName} (2)`]))
      .toEqual([outputName, `${outputName} (2)`])
    expect(await readyRootNames(newPage, [outputName, `${outputName} (2)`]))
      .toEqual([outputName, `${outputName} (2)`])
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

    const probeTaskInput = (page: Page, id: string, body: string) => page.evaluate(async ({ id, body }) => {
      const response = await fetch(`/api/tasks/${id}/input`, {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body,
      })
      return { status: response.status, body: await response.json() }
    }, { id, body })
    const [oldMissing, newMissing] = await Promise.all([
      probeTaskInput(oldPage, '0190f8f0-1c2b-7c3d-9e4f-5a6b7c8d9e0f', '{"password":'),
      probeTaskInput(newPage, '0190f8f0-1c2b-7c3d-9e4f-5a6b7c8d9e0f', '{"password":'),
    ])
    expect(newMissing).toEqual(oldMissing)
    expect(oldMissing).toEqual({
      status: 409,
      body: { error: { status: 409, message: 'task is not waiting for input' } },
    })
    const [oldEmpty, newEmpty] = await Promise.all([
      probeTaskInput(oldPage, oldWaiting.id, '{}'),
      probeTaskInput(newPage, newWaiting.id, '{}'),
    ])
    expect(newEmpty).toEqual(oldEmpty)
    expect(oldEmpty).toEqual({
      status: 400,
      body: { error: { status: 400, message: 'archive password is required' } },
    })
    const [oldUnknown, newUnknown] = await Promise.all([
      probeTaskInput(oldPage, oldWaiting.id, '{"password":"x","extra":true}'),
      probeTaskInput(newPage, newWaiting.id, '{"password":"x","extra":true}'),
    ])
    expect(newUnknown).toEqual(oldUnknown)
    expect(oldUnknown).toEqual({
      status: 400,
      body: { error: { status: 400, message: 'invalid JSON request' } },
    })

    const [oldDialog, newDialog] = await Promise.all([
      openPasswordDialog(oldPage, name),
      openPasswordDialog(newPage, name),
    ])
    expect(newDialog, 'Rust 加密归档密码入口与 reference 不一致').toEqual(oldDialog)

    const [oldCancel, newCancel] = await Promise.all([
      oldPage.evaluate(async id => (await fetch(`/api/tasks/${id}/cancel`, { method: 'POST' })).status, oldWaiting.id),
      newPage.evaluate(async id => (await fetch(`/api/tasks/${id}/cancel`, { method: 'POST' })).status, newWaiting.id),
    ])
    expect(newCancel).toBe(oldCancel)
    expect(oldCancel).toBe(204)
    const [oldCancelled, newCancelled] = await Promise.all([
      waitForTask(oldPage, name, 'cancelled'),
      waitForTask(newPage, name, 'cancelled'),
    ])
    expect({ status: newCancelled.status, phase: newCancelled.phase, progress: newCancelled.progress, error: newCancelled.error, name: newCancelled.name })
      .toEqual({ status: oldCancelled.status, phase: oldCancelled.phase, progress: oldCancelled.progress, error: oldCancelled.error, name: oldCancelled.name })
  } finally {
    await Promise.all([
      cleanup(oldPage, [name]),
      cleanup(newPage, [name]),
    ])
    await Promise.all([oldContext.close(), newContext.close()])
  }
})

test('old/new 损坏归档的任务失败状态与错误文案保持 reference 行为', async ({ browser }) => {
  test.setTimeout(60_000)
  const oldUrl = process.env.E2E_REFERENCE_URL || 'http://127.0.0.1:18080'
  const newUrl = process.env.E2E_NEW_URL || 'http://127.0.0.1:18084'
  const name = `archive-corrupt-reference-${crypto.randomUUID()}.zip`
  const bytes = Buffer.from('this is not a valid archive\n')
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
    const [oldFailed, newFailed] = await Promise.all([
      waitForTask(oldPage, name, 'failed'),
      waitForTask(newPage, name, 'failed'),
    ])
    expect({ status: newFailed.status, phase: newFailed.phase, progress: newFailed.progress, error: newFailed.error, name: newFailed.name })
      .toEqual({ status: oldFailed.status, phase: oldFailed.phase, progress: oldFailed.progress, error: oldFailed.error, name: oldFailed.name })
  } finally {
    await Promise.all([
      cleanup(oldPage, [name]),
      cleanup(newPage, [name]),
    ])
    await Promise.all([oldContext.close(), newContext.close()])
  }
})

test('old/new 归档路径安全拒绝的任务错误保持 reference 行为', async ({ browser }) => {
  test.setTimeout(60_000)
  const oldUrl = process.env.E2E_REFERENCE_URL || 'http://127.0.0.1:18080'
  const newUrl = process.env.E2E_NEW_URL || 'http://127.0.0.1:18084'
  const name = `archive-unsafe-reference-${crypto.randomUUID()}.zip`
  const bytes = storedZip([['../escape.txt', Buffer.from('must not escape\n')]])
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
    const [oldStart, newStart] = await Promise.all([
      startExtractionWithoutToast(oldPage, name),
      startExtractionWithoutToast(newPage, name),
    ])
    expect(oldStart).toBe(202)
    expect(newStart).toBe(202)
    const [oldFailed, newFailed] = await Promise.all([
      waitForTask(oldPage, name, 'failed'),
      waitForTask(newPage, name, 'failed'),
    ])
    expect({ status: newFailed.status, phase: newFailed.phase, progress: newFailed.progress, error: newFailed.error, name: newFailed.name })
      .toEqual({ status: oldFailed.status, phase: oldFailed.phase, progress: oldFailed.progress, error: oldFailed.error, name: oldFailed.name })
  } finally {
    await Promise.all([
      cleanup(oldPage, [name]),
      cleanup(newPage, [name]),
    ])
    await Promise.all([oldContext.close(), newContext.close()])
  }
})

test('old/new 真实归档格式后缀和解压结果保持 reference 行为', async ({ browser }) => {
  test.setTimeout(180_000)
  const oldUrl = process.env.E2E_REFERENCE_URL || 'http://127.0.0.1:18080'
  const newUrl = process.env.E2E_NEW_URL || 'http://127.0.0.1:18084'
  const suffix = crypto.randomUUID()
  const body = Buffer.from(`archive format parity ${suffix}\n`)
  const archiveEntries: Array<[string, Buffer]> = [
    ['entry.txt', body],
    ['entry.md', Buffer.from(`# ${suffix}\n`)],
    ['entry.json', Buffer.from('{"archive":true}\n')],
    ['entry.html', Buffer.from('<p>archive</p>\n')],
    ['entry.svg', Buffer.from('<svg xmlns="http://www.w3.org/2000/svg"/>\n')],
    ['entry.bin', Buffer.from([0, 1, 2, 3])],
  ]
  const tarBytes = tar(archiveEntries)
  const formats = [
    ['zip', storedZip(archiveEntries)],
    ['tar', tarBytes],
    ['tar.gz', gzipSync(tarBytes)],
    ['tgz', gzipSync(tarBytes)],
    ['tar.bz2', compress('bzip2', tarBytes)],
    ['tbz2', compress('bzip2', tarBytes)],
    ['tbz', compress('bzip2', tarBytes)],
    ['tar.xz', compress('xz', tarBytes)],
    ['txz', compress('xz', tarBytes)],
    ['tar.zst', compress('zstd', tarBytes)],
    ['tzst', compress('zstd', tarBytes)],
    ['gz', gzipSync(body)],
    ['bz2', compress('bzip2', body)],
    ['xz', compress('xz', body)],
    ['zst', compress('zstd', body)],
    ['7z', sevenZip(body)],
    // A real RAR cannot be produced by the installed open-source 7z binary.
    // Keep the suffix in the matrix with a valid archive payload to verify the
    // old route's suffix recognition; genuine RAR decoder coverage remains an
    // explicit fixture gap rather than being silently called PASS.
    ['rar', storedZip(archiveEntries)],
  ] as const
  const oldContext = await browser.newContext({ viewport: { width: 1440, height: 950 } })
  const newContext = await browser.newContext({ viewport: { width: 1440, height: 950 } })
  const oldPage = await oldContext.newPage()
  const newPage = await newContext.newPage()
  const names: string[] = []

  try {
    await Promise.all([login(oldPage, oldUrl), login(newPage, newUrl)])
    for (const [extension, bytes] of formats) {
      const name = `archive-format-${extension.replaceAll('.', '-')}-${suffix}.${extension}`
      const outputName = archiveBaseName(name)
      names.push(name, outputName)
      await Promise.all([
        uploadArchive(oldPage, name, bytes),
        uploadArchive(newPage, name, bytes),
      ])
      await Promise.all([startExtraction(oldPage, name), startExtraction(newPage, name)])
      const [oldResult, newResult] = await Promise.all([
        waitForTerminalTask(oldPage, name),
        waitForTerminalTask(newPage, name),
      ])
      expect({ status: newResult.status, phase: newResult.phase, progress: newResult.progress, error: newResult.error })
        .toEqual({ status: oldResult.status, phase: oldResult.phase, progress: oldResult.progress, error: oldResult.error })
      if (oldResult.status === 'completed') {
        const [oldOutput, newOutput] = await Promise.all([
          outputSnapshot(oldPage, outputName),
          outputSnapshot(newPage, outputName),
        ])
        expect(newOutput, `${extension} 解压结果与 reference 不一致`).toEqual(oldOutput)
      }
    }
  } finally {
    await Promise.all([
      cleanup(oldPage, names),
      cleanup(newPage, names),
    ])
    await Promise.all([oldContext.close(), newContext.close()])
  }
})

test('old/new 真实 RAR 的文件、目录和链接结果保持 reference 行为', async ({ browser }) => {
  test.setTimeout(60_000)
  const oldUrl = process.env.E2E_REFERENCE_URL || 'http://127.0.0.1:18080'
  const newUrl = process.env.E2E_NEW_URL || 'http://127.0.0.1:18084'
  const name = `archive-real-rar-reference-${crypto.randomUUID()}.rar`
  const bytes = Buffer.from(RAR_BASE64, 'base64')
  const outputName = archiveBaseName(name)
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
    const [oldStart, newStart] = await Promise.all([
      startExtractionWithoutToast(oldPage, name),
      startExtractionWithoutToast(newPage, name),
    ])
    expect(oldStart).toBe(202)
    expect(newStart).toBe(202)
    const [oldResult, newResult] = await Promise.all([
      waitForTerminalTask(oldPage, name),
      waitForTerminalTask(newPage, name),
    ])
    expect({ status: newResult.status, phase: newResult.phase, progress: newResult.progress, error: newResult.error })
      .toEqual({ status: oldResult.status, phase: oldResult.phase, progress: oldResult.progress, error: oldResult.error })

    if (oldResult.status === 'completed') {
      const [oldOutput, newOutput] = await Promise.all([
        outputSnapshot(oldPage, outputName),
        outputSnapshot(newPage, outputName),
      ])
      expect(newOutput, 'Rust 真实 RAR 解压结果与 reference 不一致').toEqual(oldOutput)
    }
  } finally {
    await Promise.all([
      cleanup(oldPage, [name, outputName]),
      cleanup(newPage, [name, outputName]),
    ])
    await Promise.all([oldContext.close(), newContext.close()])
  }
})

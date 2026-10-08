import { test, expect } from '@playwright/test'
import { createServer, request as httpRequest, type Server } from 'node:http'
import { readFile } from 'node:fs/promises'
import { createHash } from 'node:crypto'
import { login } from './helpers'

// An actual HTTP proxy injects failures below the worker (page.route cannot
// faithfully model a truncated streamed HTTP body). Only test-owned files fail.
test('all file types, native downloads and uploads recover through the shared worker', async ({ page, context, baseURL }) => {
  test.setTimeout(120_000)
  const upstream = new URL(baseURL!)
  const tracked = new Set<string>(), sharedPaths = new Set<string>(), ranges: string[] = [], puts = new Map<string, number>()
  let faulted = false, lostAck = false, requests = 0, active = 0, maximum = 0, faults = 0
  let resumedRange = ''
  const proxy: Server = createServer((req, res) => {
    const headers = { ...req.headers, host: upstream.host }
    if (headers.origin) headers.origin = upstream.origin
    const target = httpRequest(new URL(req.url!, upstream), { method: req.method, headers }, response => {
      const trackedDownload = [...tracked].some(id => req.url?.startsWith(`/api/files/${id}/download`)) || sharedPaths.has(req.url || '')
      const range = String(req.headers.range || '')
      if (trackedDownload && range) {
        ranges.push(range); requests++; active++; maximum = Math.max(maximum, active)
        let finished = false
        const finish = () => { if (!finished) { active--; finished = true } }
        res.on('close', finish)
        if (requests % 7 === 0) { faults++; response.destroy(); res.destroy(); return }
        const block = /^bytes=(\d+)-(\d+)$/.exec(range)
        if (!faulted && block && Number(block[1]) > 0 && Number(block[2]) - Number(block[1]) >= 32768) {
          faulted = true; faults++
          resumedRange = `bytes=${Number(block[1]) + 32768}-${block[2]}`
          res.writeHead(response.statusCode!, response.headers)
          let sent = 0
          response.on('data', chunk => {
            if (sent < 32768) { const part = chunk.subarray(0, 32768 - sent); sent += part.length; res.write(part) }
            if (sent === 32768) { response.destroy(); setTimeout(() => res.destroy(), 100) }
          })
          return
        }
      }
      if (req.method === 'PUT' && /\/api\/uploads\/[^/]+\/data\/\d+/.test(req.url || '')) {
        puts.set(req.url!, (puts.get(req.url!) || 0) + 1)
        if (!lostAck && req.url!.endsWith('/2') && response.statusCode === 204) {
          lostAck = true; response.resume(); res.destroy(); return
        }
      }
      res.writeHead(response.statusCode!, response.headers); response.pipe(res)
    })
    target.on('error', () => res.destroy()); req.pipe(target)
  })
  await new Promise<void>(resolve => proxy.listen(0, '127.0.0.1', resolve))
  const port = (proxy.address() as { port: number }).port
  const origin = `http://${upstream.hostname}:${port}`
  const ownedFiles: string[] = []
  try {
    await login(page)
    console.info("transport: authenticated")
    await page.goto(origin)
    await expect.poll(() => page.evaluate(() => Boolean(navigator.serviceWorker.controller))).toBe(true)
    await expect(page.locator('.topbar')).toBeVisible()
    console.info('transport: worker controls proxy page')
    const data = Buffer.alloc(3 * 1024 * 1024 + 17)
    for (let i = 0; i < data.length; i++) data[i] = (i * 31 + 7) % 251
    const expected = createHash('sha256').update(data).digest('hex')
    for (const [extension, mime] of [['pdf', 'application/pdf'], ['zip', 'application/zip'], ['txt', 'text/plain'], ['dat', 'application/octet-stream']]) {
      const name = `transport-${Date.now()}.${extension}`
      const created = await context.request.post(new URL('/api/uploads', upstream).href, { headers: { Origin: upstream.origin }, data: {
        parent_id: '00000000-0000-0000-0000-000000000000', name, size: data.length, mime_type: mime,
      } })
      expect(created.ok()).toBe(true)
      const upload = await created.json(); ownedFiles.push(upload.file_id); tracked.add(upload.file_id)
      const parts = []
      for (let start = 0, number = 1; start < data.length; start += upload.part_size, number++) {
        const block = data.subarray(start, start + upload.part_size)
        const put = await context.request.put(new URL(`/api/uploads/${upload.upload_id}/data/${number}`, upstream).href, {
          headers: { Origin: upstream.origin, 'X-Content-SHA256': createHash('sha256').update(block).digest('hex') }, data: block,
        })
        expect(put.status()).toBe(204); parts.push({ part_number: number, etag: put.headers().etag })
      }
      const complete = await context.request.post(new URL(`/api/uploads/${upload.upload_id}/complete`, upstream).href, { headers: { Origin: upstream.origin }, data: { parts } })
      expect(complete.ok()).toBe(true)
      console.info(`transport: downloading ${extension}`)
      const downloadEvent = page.waitForEvent('download')
      await page.evaluate(({ id, name }) => {
        const a = document.createElement('a'); a.href = `/api/files/${id}/download`; a.title = name
        document.body.appendChild(a); a.click(); a.remove()
      }, { id: upload.file_id, name })
      const download = await downloadEvent
      console.info(`transport: download started ${extension}, ranges=${ranges.length}`)
      expect(await download.failure()).toBeNull()
      const saved = await readFile((await download.path())!)
      expect(createHash('sha256').update(saved).digest('hex')).toBe(expected)
      if (extension === 'dat') {
        const share = await context.request.post(new URL(`/api/files/${upload.file_id}/share`, upstream).href,
          { headers: { Origin: upstream.origin }, data: {} })
        const path = new URL((await share.json()).url).pathname; sharedPaths.add(path)
        const recipient = await page.context().browser()!.newContext()
        try {
          const publicPage = await recipient.newPage()
          const publicDownload = publicPage.waitForEvent('download')
          await publicPage.goto(origin + path, { waitUntil: 'commit' })
          const shared = await publicDownload
          expect(await shared.failure()).toBeNull()
          expect(createHash('sha256').update(await readFile((await shared.path())!)).digest('hex')).toBe(expected)
          expect(await publicPage.evaluate(() => Boolean(navigator.serviceWorker.controller))).toBe(true)
        } finally { await recipient.close() }
      }
    }
    expect(faulted).toBe(true); expect(faults).toBeGreaterThan(1)
    expect(ranges).toContain(resumedRange)
    expect(maximum).toBeGreaterThanOrEqual(2)

    const archive = await context.request.post(new URL('/api/files/batch-download/prepare', upstream).href,
      { headers: { Origin: upstream.origin }, data: { ids: ownedFiles } })
    expect(archive.ok()).toBe(true)
    const archivePath = `/api/files/batch-download/${(await archive.json()).token}`
    sharedPaths.add(archivePath)
    const original = await context.request.get(new URL(archivePath, upstream).href)
    expect(original.ok()).toBe(true)
    const archiveHash = createHash('sha256').update(await original.body()).digest('hex')
    const beforeArchive = ranges.length, archiveEvent = page.waitForEvent('download')
    await page.evaluate(path => { const a = document.createElement('a'); a.href = path; document.body.appendChild(a); a.click(); a.remove() }, archivePath)
    const zipped = await archiveEvent
    expect(await zipped.failure()).toBeNull()
    expect(createHash('sha256').update(await readFile((await zipped.path())!)).digest('hex')).toBe(archiveHash)
    expect(ranges.length - beforeArchive).toBeGreaterThanOrEqual(2)

    // Drive the actual Rust upload queue through its shared JS adapter.
    console.info("transport: validating UI upload")
    await page.goto(`${origin}/files`)
    const uiName = `transport-ui-${Date.now()}.bin`
    await page.getByLabel('选择文件上传', { exact: true }).setInputFiles({ name: uiName, mimeType: 'application/octet-stream', buffer: data })
    await expect.poll(() => lostAck, { timeout: 30000 }).toBe(true)
    await expect(page.locator('.upload-progress')).toHaveCount(0, { timeout: 30000 })
    expect([...puts.values()].filter(count => count > 1)).toEqual([2])
    expect([...puts.values()].filter(count => count === 1).length).toBeGreaterThanOrEqual(2)
    const listing = await context.request.get(new URL('/api/files/00000000-0000-0000-0000-000000000000/children', upstream).href)
    const files = (await listing.json()).items.filter((file: { name: string }) => file.name === uiName)
    for (const file of files) { ownedFiles.push(file.id); expect(file.content_hash).toBe(expected) }
    expect(files.length).toBe(1)
    await page.reload()
    await page.getByRole('button', { name: '打开搜索', exact: true }).click()
    await page.getByLabel('搜索文件名').fill(uiName)
    await page.getByLabel('搜索文件名').press('Enter')
    const card = page.locator('.file-card').filter({ hasText: files[0].name })
    await expect(card).toBeVisible()
    // Other feature tests exercise the actual menu download action; this test
    // verifies the underlying native attachment navigation under body faults.
  } finally {
    for (const id of ownedFiles) {
      await context.request.delete(new URL(`/api/files/${id}`, upstream).href, { headers: { Origin: upstream.origin } })
      await context.request.delete(new URL(`/api/trash/${id}`, upstream).href, { headers: { Origin: upstream.origin } })
    }
    proxy.closeAllConnections(); await new Promise<void>(resolve => proxy.close(() => resolve()))
  }
})

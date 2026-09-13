import { expect, test, type Page } from '@playwright/test'
import { login } from './helpers'

const ROOT = '00000000-0000-0000-0000-000000000000'

async function fileId(page: Page, name: string) {
  return page.evaluate(async ({ root, wanted }) => {
    const response = await fetch(`/api/files/${root}/children`)
    if (!response.ok) throw new Error(`读取文件列表失败：${response.status}`)
    const payload = await response.json() as { items?: Array<{ id: string; name: string }> }
    const item = (payload.items ?? []).find(entry => entry.name === wanted)
    if (!item) throw new Error(`找不到文件：${wanted}`)
    return item.id
  }, { root: ROOT, wanted: name })
}

async function removeFile(page: Page, name: string) {
  await page.evaluate(async ({ root, wanted }) => {
    const headers = { 'Content-Type': 'application/json' }
    const live = await fetch(`/api/files/${root}/children`)
    if (live.ok) {
      const payload = await live.json() as { items?: Array<{ id: string; name: string }> }
      for (const item of payload.items ?? []) {
        if (item.name === wanted) await fetch(`/api/files/${item.id}`, { method: 'DELETE', headers })
      }
    }
    const trash = await fetch('/api/trash')
    if (trash.ok) {
      const payload = await trash.json() as { items?: Array<{ id: string; name: string }> }
      for (const item of payload.items ?? []) {
        if (item.name === wanted) await fetch(`/api/trash/${item.id}`, { method: 'DELETE', headers })
      }
    }
  }, { root: ROOT, wanted: name })
}

async function responseShape(response: Awaited<ReturnType<Page['request']['get']>>) {
  const headers = response.headers()
  return {
    status: response.status(),
    type: headers['content-type'] ?? '',
    disposition: headers['content-disposition'] ?? '',
    ranges: headers['accept-ranges'] ?? '',
    range: headers['content-range'] ?? '',
    length: headers['content-length'] ?? '',
    etag: headers.etag ?? '',
    body: await response.body(),
  }
}

test('真实上传文件的下载、预览和 Range 响应保持 reference 行为', async ({ page }) => {
  const suffix = crypto.randomUUID()
  const name = `parity-range-${suffix}.txt`
  const imageName = `parity-preview-${suffix}.png`
  const content = `range parity ${suffix}\n第二行\n`
  const image = Buffer.from(
    'iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mNk+A8AAQUBAScY42YAAAAASUVORK5CYII=',
    'base64',
  )

  try {
    await login(page)
    await page.locator('input[type=file]').first().setInputFiles([
      { name, mimeType: 'text/plain', buffer: Buffer.from(content) },
      { name: imageName, mimeType: 'image/png', buffer: image },
    ])
    await expect(page.locator('.file-card, .file-row').filter({ hasText: name })).toBeVisible({ timeout: 20_000 })
    await expect(page.locator('.file-card, .file-row').filter({ hasText: imageName })).toBeVisible({ timeout: 20_000 })

    const id = await fileId(page, name)
    const full = await responseShape(await page.request.get(`/api/files/${id}/download`))
    expect(full.status).toBe(200)
    expect(full.type).toContain('text/plain')
    expect(full.disposition).toContain(`attachment; filename*=UTF-8''${encodeURIComponent(name)}`)
    expect(full.ranges).toBe('bytes')
    expect(full.length).toBe(String(Buffer.byteLength(content)))
    expect(full.body.equals(Buffer.from(content))).toBe(true)
    expect(full.etag).toMatch(/^".*"$/)

    const partial = await responseShape(await page.request.get(`/api/files/${id}/download`, {
      headers: { Range: 'bytes=2-8' },
    }))
    expect(partial.status).toBe(206)
    expect(partial.range).toBe(`bytes 2-8/${Buffer.byteLength(content)}`)
    expect(partial.length).toBe('7')
    expect(partial.body.equals(Buffer.from(content.slice(2, 9)))).toBe(true)

    const textPreview = await responseShape(await page.request.get(`/api/files/${id}/preview`))
    expect(textPreview.status).toBe(415)

    const imageId = await fileId(page, imageName)
    const suffixRange = await responseShape(await page.request.get(`/api/files/${imageId}/preview`, {
      headers: { Range: 'bytes=-5' },
    }))
    expect(suffixRange.status).toBe(206)
    expect(suffixRange.type).toContain('image/png')
    expect(suffixRange.range).toBe(`bytes ${image.length - 5}-${image.length - 1}/${image.length}`)
    expect(suffixRange.disposition).toContain(`inline; filename*=UTF-8''${encodeURIComponent(imageName)}`)
    expect(suffixRange.body.equals(image.subarray(-5))).toBe(true)

    const invalid = await responseShape(await page.request.get(`/api/files/${id}/download`, {
      headers: { Range: 'bytes=999999-' },
    }))
    expect(invalid.status).toBe(416)
    expect(invalid.range).toBe(`bytes */${Buffer.byteLength(content)}`)
    expect(invalid.type).toBe('text/plain; charset=utf-8')
    expect(invalid.disposition).toContain(`attachment; filename*=UTF-8''${encodeURIComponent(name)}`)
    expect(invalid.ranges).toBe('')
    expect(invalid.etag).toBe('')
    expect(invalid.length).toBe('33')
    expect(invalid.body.equals(Buffer.from('invalid range: failed to overlap\n'))).toBe(true)
  } finally {
    await removeFile(page, name)
    await removeFile(page, imageName)
  }
})

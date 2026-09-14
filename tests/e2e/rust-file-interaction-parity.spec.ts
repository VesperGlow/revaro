import { expect, test } from '@playwright/test'
import { readFileSync } from 'node:fs'
import { login } from './helpers'

const ROOT = '00000000-0000-0000-0000-000000000000'

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

  const centralDirectory = Buffer.concat(central)
  const end = Buffer.alloc(22)
  end.writeUInt32LE(0x06054b50, 0)
  end.writeUInt16LE(entries.length, 8)
  end.writeUInt16LE(entries.length, 10)
  end.writeUInt32LE(centralDirectory.length, 12)
  end.writeUInt32LE(offset, 16)
  return Buffer.concat([...local, centralDirectory, end])
}

function iconEpub() {
  const container = '<?xml version="1.0" encoding="UTF-8"?><container xmlns="urn:oasis:names:tc:opendocument:xmlns:container" version="1.0"><rootfiles><rootfile full-path="OEBPS/content.opf" media-type="application/oebps-package+xml"/></rootfiles></container>'
  const opf = '<?xml version="1.0" encoding="UTF-8"?><package xmlns="http://www.idpf.org/2007/opf" version="3.0"><metadata xmlns:dc="http://purl.org/dc/elements/1.1/"><dc:title>图标回归验收</dc:title></metadata><manifest><item id="chapter" href="chapter.xhtml" media-type="application/xhtml+xml"/></manifest><spine><itemref idref="chapter"/></spine></package>'
  const chapter = '<html xmlns="http://www.w3.org/1999/xhtml"><body><h1>图标回归验收</h1><p>书籍图标没有封面时应使用 reference 几何。</p></body></html>'
  return storedZip([
    ['mimetype', Buffer.from('application/epub+zip')],
    ['META-INF/container.xml', Buffer.from(container)],
    ['OEBPS/content.opf', Buffer.from(opf)],
    ['OEBPS/chapter.xhtml', Buffer.from(chapter)],
  ])
}

test('移动端列表在选择模式下轻触行只切换选择，不打开文件', async ({ browser }) => {
  const context = await browser.newContext({
    viewport: { width: 390, height: 844 },
    isMobile: true,
    hasTouch: true,
  })
  const page = await context.newPage()
  const name = `compat-touch-${crypto.randomUUID()}.txt`
  let id = ''

  try {
    await login(page)
    id = await page.evaluate(async ({ name, root }) => {
      const response = await fetch('/api/documents', {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ parent_id: root, name, content: 'touch parity' }),
      })
      if (!response.ok) throw new Error(`创建文档失败：${response.status}`)
      const file = await response.json() as { id: string }
      return file.id
    }, { name, root: ROOT })

    await page.reload()
    await page.getByRole('button', { name: '列表' }).click()
    const row = page.locator('.file-row').filter({ hasText: name })
    await expect(row).toBeVisible()
    await row.getByRole('button', { name: '选择项目' }).click()
    await expect(page.locator('.selection-toolbar')).toContainText('1 项')

    await row.locator('.row-info').click()
    await expect(page.locator('.selection-toolbar')).toHaveCount(0)
    await expect(page.locator('.modal-backdrop.editing')).toHaveCount(0)

    await row.getByRole('button', { name: '选择项目' }).click()
    await expect(page.locator('.selection-toolbar')).toContainText('1 项')
  } finally {
    if (id) {
      await page.evaluate(async ({ id }) => {
        await fetch(`/api/files/${id}`, { method: 'DELETE' })
        await fetch(`/api/trash/${id}`, { method: 'DELETE' })
      }, { id })
    }
    await context.close()
  }
})

test('点击内容空白处会清除文件选择', async ({ page }) => {
  const name = `compat-blank-${crypto.randomUUID()}.txt`
  let id = ''

  try {
    await login(page)
    id = await page.evaluate(async ({ name, root }) => {
      const response = await fetch('/api/documents', {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ parent_id: root, name, content: 'blank selection parity' }),
      })
      if (!response.ok) throw new Error(`创建文档失败：${response.status}`)
      const file = await response.json() as { id: string }
      return file.id
    }, { name, root: ROOT })

    await page.reload()
    await page.getByRole('button', { name: '列表' }).click()
    const row = page.locator('.file-row').filter({ hasText: name })
    await expect(row).toBeVisible()
    await row.getByRole('button', { name: '选择项目' }).click()
    await expect(page.locator('.selection-toolbar')).toContainText('1 项')

    await page.evaluate(() => window.scrollTo(0, 0))
    const content = page.locator('.content')
    const box = await content.boundingBox()
    if (!box) throw new Error('内容区域没有布局盒')
    await page.mouse.click(box.x + 2, box.y + 2)
    await expect(page.locator('.selection-toolbar')).toHaveCount(0)
  } finally {
    if (id) {
      await page.evaluate(async ({ id }) => {
        await fetch(`/api/files/${id}`, { method: 'DELETE' })
        await fetch(`/api/trash/${id}`, { method: 'DELETE' })
      }, { id })
    }
  }
})

test('可编辑 TXT 在文件卡上使用旧版文档图标，而不是阅读器图标', async ({ page }) => {
  const name = `compat-icon-${crypto.randomUUID()}.txt`
  let id = ''

  try {
    await login(page)
    id = await page.evaluate(async ({ name, root }) => {
      const response = await fetch('/api/documents', {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ parent_id: root, name, content: 'icon parity' }),
      })
      if (!response.ok) throw new Error(`创建文档失败：${response.status}`)
      const file = await response.json() as { id: string }
      return file.id
    }, { name, root: ROOT })

    await page.reload()
    const card = page.locator('.file-card').filter({ hasText: name })
    await expect(card).toBeVisible()
    await expect(card.locator('.document-type-icon')).toHaveCount(1)
    await expect(card.locator('.book-type-icon')).toHaveCount(0)
  } finally {
    if (id) {
      await page.evaluate(async ({ id }) => {
        await fetch(`/api/files/${id}`, { method: 'DELETE' })
        await fetch(`/api/trash/${id}`, { method: 'DELETE' })
      }, { id })
    }
  }
})

test('EPUB 文件卡使用旧版书籍图标几何', async ({ page }) => {
  const name = `compat-book-icon-${crypto.randomUUID()}.epub`
  let id = ''

  try {
    await login(page)
    await page.locator('input[type=file]').first().setInputFiles({
      name,
      mimeType: 'application/epub+zip',
      buffer: iconEpub(),
    })

    const card = page.locator('.file-card').filter({ hasText: name })
    await expect(card).toBeVisible({ timeout: 20_000 })
    await expect(card.locator('.book-type-icon')).toHaveCount(1)
    await expect(card).toHaveClass(/fallback-tile/)
    await expect(card).not.toHaveClass(/preview-tile/)
    await expect(card.locator('.book-type-icon .icon-detail')).toHaveAttribute(
      'd',
      'M48 24v57M23 31c7 0 13 1 18 4M23 44c7 0 13 1 18 4M73 31c-7 0-13 1-18 4M73 44c-7 0-13 1-18 4',
    )

    id = await page.evaluate(async ({ name, root }) => {
      const response = await fetch(`/api/files/${root}/children`)
      if (!response.ok) throw new Error(`刷新文件列表失败：${response.status}`)
      const data = await response.json() as { items?: Array<{ id: string; name: string }> }
      return data.items?.find(item => item.name === name)?.id ?? ''
    }, { name, root: ROOT })
  } finally {
    if (id) {
      await page.evaluate(async ({ id }) => {
        await fetch(`/api/files/${id}`, { method: 'DELETE' })
        await fetch(`/api/trash/${id}`, { method: 'DELETE' })
      }, { id })
    }
  }
})

test('视频文件卡使用旧版 preview 状态 class', async ({ page }) => {
  const name = `compat-video-tile-${crypto.randomUUID()}.webm`
  let id = ''

  try {
    await login(page)
    await page.locator('input[type=file]').first().setInputFiles({
      name,
      mimeType: 'video/webm',
      buffer: readFileSync(new URL('./fixtures/preview.webm', import.meta.url)),
    })

    const card = page.locator('.file-card').filter({ hasText: name })
    await expect(card).toBeVisible({ timeout: 20_000 })
    await expect(card).toHaveClass(/preview-tile/)
    await expect(card).not.toHaveClass(/fallback-tile/)
    await expect(card.locator('.video-thumb')).toHaveCount(1)

    id = await page.evaluate(async ({ name, root }) => {
      const response = await fetch(`/api/files/${root}/children`)
      if (!response.ok) throw new Error(`刷新文件列表失败：${response.status}`)
      const data = await response.json() as { items?: Array<{ id: string; name: string }> }
      return data.items?.find(item => item.name === name)?.id ?? ''
    }, { name, root: ROOT })
  } finally {
    if (id) {
      await page.evaluate(async ({ id }) => {
        await fetch(`/api/files/${id}`, { method: 'DELETE' })
        await fetch(`/api/trash/${id}`, { method: 'DELETE' })
      }, { id })
    }
  }
})

test('方块文件卡聚焦后按空格不会滚动页面', async ({ page }) => {
  const name = `compat-space-${crypto.randomUUID()}.txt`
  let id = ''

  try {
    await login(page)
    id = await page.evaluate(async ({ name, root }) => {
      const response = await fetch('/api/documents', {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ parent_id: root, name, content: 'space parity' }),
      })
      if (!response.ok) throw new Error(`创建文档失败：${response.status}`)
      return (await response.json() as { id: string }).id
    }, { name, root: ROOT })

    await page.reload()
    const card = page.locator('.file-card').filter({ hasText: name })
    await expect(card).toBeVisible()
    await card.focus()
    await page.evaluate(() => window.scrollTo(0, 150))
    const before = await page.evaluate(() => window.scrollY)
    await page.keyboard.press('Space')
    await page.waitForTimeout(100)
    const after = await page.evaluate(() => window.scrollY)
    expect(after).toBe(before)
    await expect(page.locator('.modal-backdrop')).toHaveCount(0)
  } finally {
    if (id) {
      await page.evaluate(async ({ id }) => {
        await fetch(`/api/files/${id}`, { method: 'DELETE' })
        await fetch(`/api/trash/${id}`, { method: 'DELETE' })
      }, { id })
    }
  }
})

test('回收站目录行按 Enter 保留 reference 的默认事件处理', async ({ page }) => {
  const name = `compat-trash-folder-key-${crypto.randomUUID()}`
  let id = ''

  try {
    await login(page)
    id = await page.evaluate(async ({ name, root }) => {
      const response = await fetch('/api/directories', {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ parent_id: root, name }),
      })
      if (!response.ok) throw new Error(`创建目录失败：${response.status}`)
      return (await response.json() as { id: string }).id
    }, { name, root: ROOT })
    await page.evaluate(async folderId => {
      const response = await fetch(`/api/files/${folderId}`, { method: 'DELETE' })
      if (!response.ok) throw new Error(`移入回收站失败：${response.status}`)
    }, id)

    await page.getByTitle('回收站').first().click()
    await expect(page.getByRole('heading', { name: '回收站', exact: true })).toBeVisible()
    const row = page.locator('.file-card, .file-row').filter({ hasText: name })
    await expect(row).toBeVisible()
    await row.focus()
    await page.evaluate(() => {
      ;(window as Window & { __enterDefaultPrevented?: boolean }).__enterDefaultPrevented = undefined
      window.addEventListener('keydown', event => {
        if (event.key === 'Enter') {
          ;(window as Window & { __enterDefaultPrevented?: boolean }).__enterDefaultPrevented = event.defaultPrevented
        }
      }, { once: true })
    })
    await page.keyboard.press('Enter')
    expect(await page.evaluate(() => (window as Window & { __enterDefaultPrevented?: boolean }).__enterDefaultPrevented)).toBe(true)
    await expect(page.locator('.modal-backdrop')).toHaveCount(0)
  } finally {
    if (id) await page.evaluate(async folderId => { await fetch(`/api/trash/${folderId}`, { method: 'DELETE' }) }, id)
  }
})

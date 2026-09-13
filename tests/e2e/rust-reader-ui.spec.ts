import { expect, test } from '@playwright/test'
import { login } from './helpers'

function crc32(data: Buffer) {
  let value = 0xffffffff
  for (const byte of data) {
    value ^= byte
    for (let bit = 0; bit < 8; bit += 1) value = (value >>> 1) ^ (value & 1 ? 0xedb88320 : 0)
  }
  return value ^ 0xffffffff
}

/** Build a small stored ZIP so the browser test exercises the real EPUB upload path. */
function zip(entries: Array<[string, Buffer]>) {
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

function epubChapter(id: string, title: string, marker: string) {
  const paragraphs = Array.from({ length: 18 }, (_, index) =>
    `<p>这是 EPUB 浏览器验收的第 ${index + 1} 段。Rust 服务端会清洗章节内容，浏览器再把 reading flow 排成连续页面；这段文字用于覆盖真实上传、打开、目录定位和重新打开时的阅读位置。${marker}</p>`,
  ).join('')
  return Buffer.from(
    `<?xml version="1.0" encoding="UTF-8"?><html xmlns="http://www.w3.org/1999/xhtml"><head><title>${title}</title></head><body><h1 id="${id}">${title}</h1>${paragraphs}</body></html>`,
  )
}

function readerEpub() {
  const opf = `<?xml version="1.0" encoding="UTF-8"?><package xmlns="http://www.idpf.org/2007/opf" version="3.0"><metadata xmlns:dc="http://purl.org/dc/elements/1.1/"><dc:title>浏览器 EPUB 验收</dc:title></metadata><manifest><item id="nav" href="nav.xhtml" media-type="application/xhtml+xml" properties="nav"/><item id="ch1" href="ch1.xhtml" media-type="application/xhtml+xml"/><item id="ch2" href="ch2.xhtml" media-type="application/xhtml+xml"/></manifest><spine><itemref idref="ch1"/><itemref idref="ch2"/></spine></package>`
  const nav = '<!DOCTYPE html><html xmlns="http://www.w3.org/1999/xhtml" xmlns:epub="http://www.idpf.org/2007/ops"><body><nav epub:type="toc"><ol><li><a href="ch1.xhtml#chapter-one">第一章 · 起点</a></li><li><a href="ch2.xhtml#chapter-two">第二章 · 继续</a></li></ol></nav></body></html>'
  const container = '<?xml version="1.0" encoding="UTF-8"?><container xmlns="urn:oasis:names:tc:opendocument:xmlns:container" version="1.0"><rootfiles><rootfile full-path="OEBPS/content.opf" media-type="application/oebps-package+xml"/></rootfiles></container>'
  return zip([
    ['mimetype', Buffer.from('application/epub+zip')],
    ['META-INF/container.xml', Buffer.from(container)],
    ['OEBPS/content.opf', Buffer.from(opf)],
    ['OEBPS/nav.xhtml', Buffer.from(nav)],
    ['OEBPS/ch1.xhtml', epubChapter('chapter-one', '第一章 · 起点', 'EPUB chapter one marker')],
    ['OEBPS/ch2.xhtml', epubChapter('chapter-two', '第二章 · 继续', 'EPUB chapter two marker')],
  ])
}

function readerText() {
  const paragraphs: string[] = []
  for (let chapter = 1; chapter <= 5; chapter += 1) {
    paragraphs.push(`第${chapter}章 ${chapter === 1 ? '初见' : '继续前行'}`)
    for (let paragraph = 0; paragraph < 24; paragraph += 1) {
      paragraphs.push(
        `这是第 ${chapter} 章的第 ${paragraph + 1} 段。Rust 阅读器把服务端清洗后的内容放进连续的分页流，字号变化、窗口移动和重新打开都要保持同一个阅读位置。`,
      )
    }
  }
  return `${paragraphs.join('\n\n')}\n`
}

test('Rust bundle opens editable TXT files in the document editor', async ({ page }) => {
  const name = `rust-reader-${Date.now().toString(36)}.txt`

  await login(page)
  await page.locator('input[type=file]').first().setInputFiles({
    name,
    mimeType: 'text/plain',
    buffer: Buffer.from(readerText()),
  })
  const card = page.locator('.file-card').filter({ hasText: name })
  await card.waitFor({ timeout: 20_000 })
  await card.click()
  const editor = page.locator('.document-editor')
  await expect(editor).toBeVisible()
  await expect(editor.locator('#editor-title')).toHaveText(name)
  await expect(editor.locator('textarea')).toHaveValue(readerText())
  await expect(editor.getByRole('button', { name: '保存' })).toBeDisabled()
  await editor.getByRole('button', { name: '关闭编辑器' }).click()
  await expect(editor).toHaveCount(0)
  await expect(page.getByRole('heading', { name: '我的文件' })).toBeVisible()
})

test('文档编辑器按文件名和内容共同判断未保存状态，并格式化字节数', async ({ page }) => {
  const name = `editor-bytes-${Date.now().toString(36)}.txt`

  await login(page)
  await page.locator('input[type=file]').first().setInputFiles({
    name,
    mimeType: 'text/plain',
    buffer: Buffer.alloc(1234, 'x'),
  })
  const card = page.locator('.file-card').filter({ hasText: name })
  await card.waitFor({ timeout: 20_000 })
  await card.click()

  const editor = page.locator('.document-editor')
  await expect(editor).toBeVisible()
  await expect(editor.locator('.editor-meta b')).toHaveText('1,234 字节')
  await editor.getByRole('button', { name: '关闭编辑器' }).click()

  await page.getByRole('button', { name: '新建文档', exact: true }).first().click()
  await expect(editor).toBeVisible()
  const filename = editor.getByRole('textbox', { name: '文档文件名' })
  await filename.fill('changed.md')
  await editor.getByRole('button', { name: '关闭编辑器' }).click()
  const discard = page.locator('.app-dialog')
  await expect(discard).toContainText('放弃未保存的修改？')
  await discard.getByRole('button', { name: '取消' }).click()
  await expect(editor).toBeVisible()
  await filename.fill('未命名文档.md')
  await editor.getByRole('button', { name: '关闭编辑器' }).click()
  await expect(editor).toHaveCount(0)
})

test('Markdown 预览保留 reference 的 GFM 元素并清理主动 HTML', async ({ page }) => {
  const name = `editor-markdown-${Date.now().toString(36)}.md`
  const content = '# Title\n\n#### Deep heading\n\n1. one\n2. two\n\n- [x] done\n- [ ] todo\n\n| a | b |\n| --- | :---: |\n| 1 | 2 |\n\n[link](https://example.com "T") and ![alt](cover.png)\n\n~~gone~~ and <u>under</u>\n\n<script>alert(1)</script>'

  await login(page)
  await page.locator('input[type=file]').first().setInputFiles({
    name,
    mimeType: 'text/markdown',
    buffer: Buffer.from(content),
  })
  const card = page.locator('.file-card').filter({ hasText: name })
  await card.waitFor({ timeout: 20_000 })
  await card.click()

  const editor = page.locator('.document-editor')
  await expect(editor).toBeVisible()
  await editor.locator('textarea').fill(content)
  await editor.getByRole('button', { name: '预览' }).click()
  const preview = editor.locator('.markdown-preview')
  await expect(preview.locator('h1')).toHaveText('Title')
  await expect(preview.locator('h4')).toHaveText('Deep heading')
  await expect(preview.locator('ol li')).toHaveCount(2)
  await expect(preview.locator('input[type=checkbox]')).toHaveCount(2)
  await expect(preview.locator('table th[align=center]')).toHaveText('b')
  await expect(preview.locator('a[href="https://example.com"][title="T"]')).toHaveText('link')
  await expect(preview.locator('img[src="cover.png"][alt="alt"]')).toHaveCount(1)
  await expect(preview.locator('del')).toHaveText('gone')
  await expect(preview.locator('u')).toHaveText('under')
  await expect(preview.locator('script')).toHaveCount(0)
  await expect(preview.locator('[onclick]')).toHaveCount(0)
})

test('Rust bundle opens an EPUB reader, follows its TOC and restores progress', async ({ page }) => {
  const name = `rust-reader-${Date.now().toString(36)}.epub`

  await login(page)
  await page.locator('input[type=file]').first().setInputFiles({
    name,
    mimeType: 'application/epub+zip',
    buffer: readerEpub(),
  })
  const card = page.locator('.file-card').filter({ hasText: name })
  await card.waitFor({ timeout: 20_000 })
  await card.click()
  await expect(page.locator('#reader-view')).toBeVisible()
  await expect(page.locator('#loading')).toBeHidden({ timeout: 20_000 })
  await expect(page.locator('#reader-title')).toHaveText(name.replace(/\.epub$/i, ''))
  await expect(page.locator('#flow .rf-chunk').first()).toBeAttached()
  await expect(page.locator('#flow')).toContainText('第一章 · 起点')
  await expect(page.locator('#flow')).toContainText('EPUB chapter one marker')

  await page.locator('#toc-button').click()
  await expect(page.locator('#toc-drawer')).toHaveClass(/open/)
  const tocItems = page.locator('#toc-list .toc-item')
  await expect(tocItems).toHaveCount(2)
  await expect(tocItems.nth(0)).toHaveText('第一章 · 起点')
  await expect(tocItems.nth(1)).toHaveText('第二章 · 继续')

  const progressResponse = page.waitForResponse(
    response => response.url().endsWith('/book/progress') && response.request().method() === 'PUT',
  )
  await tocItems.nth(1).click()
  await expect(page.locator('#toc-drawer')).not.toHaveClass(/open/)
  await expect(tocItems.nth(1)).toHaveClass(/active/)
  await expect(page.locator('#flow')).toContainText('EPUB chapter two marker')
  await progressResponse

  await page.locator('#reader-back').click()
  await expect(page.locator('#reader-view')).toHaveCount(0)
  await expect(page.getByRole('heading', { name: '我的文件' })).toBeVisible()

  await card.click()
  await expect(page.locator('#reader-view')).toBeVisible()
  await expect(page.locator('#loading')).toBeHidden({ timeout: 20_000 })
  await expect(page.locator('#toc-list .toc-item').nth(1)).toHaveClass(/active/)
  await expect(page.locator('#flow')).toContainText('EPUB chapter two marker')
  await page.locator('#reader-back').click()
  await expect(page.locator('#reader-view')).toHaveCount(0)
})

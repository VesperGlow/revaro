import { expect, test } from '@playwright/test'
import { login } from './helpers'

const ROOT_ID = '00000000-0000-0000-0000-000000000000'

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

async function fileId(page: Parameters<typeof login>[0], name: string) {
  return page.evaluate(async ({ root, target }) => {
    const response = await fetch(`/api/files/${root}/children`)
    const data = await response.json() as { items: Array<{ id: string; name: string }> }
    return data.items.find(item => item.name === target)?.id ?? null
  }, { root: ROOT_ID, target: name })
}

test('Rust bundle opens TXT reader, paginates, restores progress and handles deep links', async ({ page }) => {
  const name = `rust-reader-${Date.now().toString(36)}.txt`

  await login(page)
  await page.locator('input[type=file]').first().setInputFiles({
    name,
    mimeType: 'text/plain',
    buffer: Buffer.from(readerText()),
  })
  const card = page.locator('.file-card').filter({ hasText: name })
  await card.waitFor({ timeout: 20_000 })
  await page.getByRole('region', { name: '上传队列' }).getByRole('button', { name: '清除已完成' }).click()
  await page.getByRole('region', { name: '上传队列' }).waitFor({ state: 'detached' })

  await card.click()
  await expect(page.locator('#reader-view')).toBeVisible()
  await expect(page.locator('#loading')).toBeHidden({ timeout: 20_000 })
  await expect(page.locator('#flow .rf-chunk').first()).toBeAttached()
  await expect(page.locator('#flow')).toContainText('第1章 初见')
  await expect.poll(() => page.locator('#flow').evaluate(element => element.scrollWidth > element.clientWidth)).toBe(true)

  await page.waitForTimeout(1_500)
  const before = await page.locator('#flow').evaluate(element => (element as HTMLElement).style.transform)
  const progressResponse = page.waitForResponse(response => response.url().endsWith('/book/progress') && response.request().method() === 'PUT')
  await page.locator('#next-zone').click()
  await progressResponse
  await expect.poll(() => page.locator('#flow').evaluate(element => (element as HTMLElement).style.transform)).not.toBe(before)
  const beforeSwipe = await page.locator('#flow').evaluate(element => (element as HTMLElement).style.transform)
  const viewport = page.locator('#viewport')
  await viewport.dispatchEvent('pointerdown', { pointerId: 7, pointerType: 'touch', button: 0, clientX: 120, clientY: 300 })
  await viewport.dispatchEvent('pointermove', { pointerId: 7, pointerType: 'touch', button: 0, clientX: 300, clientY: 302 })
  await viewport.dispatchEvent('pointerup', { pointerId: 7, pointerType: 'touch', button: 0, clientX: 300, clientY: 302 })
  await expect.poll(() => page.locator('#flow').evaluate(element => (element as HTMLElement).style.transform)).not.toBe(beforeSwipe)

  await page.locator('#font-button').click()
  await expect(page.locator('#font-popover')).toBeVisible()
  const slider = page.locator('#font-slider')
  const originalFont = Number(await slider.inputValue())
  await page.locator('#font-larger').click()
  await expect(slider).toHaveValue(String(originalFont + 1))
  await page.locator('.v2-lineheight .font-step').nth(2).click()
  await expect(page.locator('.v2-lineheight .font-step').nth(2)).toHaveClass(/v2-active/)
  await page.locator('#theme-button').click()
  await expect(page.locator('#reader-view')).toHaveClass(/dark/)

  await page.locator('#toc-button').click()
  await expect(page.locator('#toc-drawer')).toHaveClass(/open/)
  await expect(page.locator('#toc-list .toc-item')).toHaveCount(5)
  await page.locator('#toc-list .toc-item').nth(1).click()
  await expect(page.locator('#toc-drawer')).not.toHaveClass(/open/)
  await expect(page.locator('#toc-list .toc-item').nth(1)).toHaveClass(/active/)
  await page.waitForTimeout(1_500)

  await page.locator('#reader-back').click()
  await expect(page.locator('#reader-view')).toHaveCount(0)
  await expect(page.getByRole('heading', { name: '我的文件' })).toBeVisible()

  await card.click()
  await expect(page.locator('#loading')).toBeHidden({ timeout: 20_000 })
  await expect(page.locator('#toc-list .toc-item').nth(1)).toHaveClass(/active/)
  const id = await fileId(page, name)
  expect(id).toBeTruthy()
  await page.locator('#reader-back').click()
  await expect(page.locator('#reader-view')).toHaveCount(0)

  await page.goto(`/read/${id}`)
  await expect(page.locator('#reader-view')).toBeVisible({ timeout: 20_000 })
  await expect(page.locator('#loading')).toBeHidden({ timeout: 20_000 })
  await expect(page.locator('#reader-title')).toHaveText(name.replace(/\.txt$/i, ''))
  await page.locator('#reader-back').click()
  await expect(page.locator('#reader-view')).toHaveCount(0)
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
  await page.getByRole('region', { name: '上传队列' }).getByRole('button', { name: '清除已完成' }).click()
  await page.getByRole('region', { name: '上传队列' }).waitFor({ state: 'detached' })

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

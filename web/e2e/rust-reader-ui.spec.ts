import { expect, test } from '@playwright/test'
import { login } from './helpers'

const ROOT_ID = '00000000-0000-0000-0000-000000000000'

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

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
    await page.locator('input[webkitdirectory]').setInputFiles(directory)
    await expect(page.getByText('已保留目录结构，开始上传 2 个文件')).toBeVisible()
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

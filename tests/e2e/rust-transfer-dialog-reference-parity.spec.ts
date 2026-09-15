import { expect, test, type Page } from '@playwright/test'

const ROOT = '00000000-0000-0000-0000-000000000000'
const FILE_ID = 'transfer-dialog-file'
const STAMP = '2026-01-01T00:00:00Z'

const file = {
  id: FILE_ID,
  parent_id: ROOT,
  name: '传输中的文件.txt',
  kind: 'file',
  size: 12,
  status: 'ready',
  created_at: STAMP,
  updated_at: STAMP,
  mime_type: 'text/plain',
}

function deferred() {
  let resolve!: () => void
  const promise = new Promise<void>(value => { resolve = value })
  return { promise, resolve }
}

async function mockTransfer(page: Page, failure = false) {
  const transferGate = deferred()
  await page.route('**/api/**', async route => {
    const request = route.request()
    const path = new URL(request.url()).pathname
    const json = (value: unknown) => route.fulfill({ json: value })
    if (path === '/api/auth/me') return json({ username: 'admin', has_avatar: false })
    if (path === '/api/events' || path === '/api/system/status/stream') {
      return route.fulfill({ contentType: 'text/event-stream', body: '' })
    }
    if (path === '/api/tasks') return json({ items: [] })
    if (path === '/api/library/all') {
      return json({ items: { book: [], image: [], video: [], audio: [] }, counts: { book: 0, image: 0, video: 0, audio: 0, file: 1 } })
    }
    if (path === '/api/library/counts') return json({ book: 0, image: 0, video: 0, audio: 0, file: 1 })
    if (path === `/api/files/${ROOT}`) {
      return json({
        file: { id: ROOT, parent_id: null, name: '我的文件', kind: 'directory', size: 0, status: 'ready', created_at: STAMP, updated_at: STAMP, mime_type: '' },
        breadcrumbs: [],
      })
    }
    if (path === `/api/files/${ROOT}/children`) return json({ items: [file], total_bytes: file.size, file_count: 1 })
    if (path === `/api/files/${FILE_ID}` && request.method() === 'PATCH') {
      if (failure) {
        return route.fulfill({
          status: 500,
          contentType: 'application/json',
          body: JSON.stringify({ error: { status: 500, message: 'move failed' } }),
        })
      }
      await transferGate.promise
      return json({ ...file })
    }
    return json({ items: [] })
  })
  return { transferGate }
}

async function startTransfer(page: Page, baseUrl: string, waitForBusy = true) {
  await page.goto(`${baseUrl}/?transfer-reference=${Date.now()}`)
  await expect(page.getByRole('heading', { name: '我的文件', exact: true })).toBeVisible()
  await page.getByTitle('列表视图').click()
  const row = page.locator('.file-row').filter({ hasText: file.name })
  await row.getByRole('button', { name: '选择项目' }).click()
  await page.getByRole('toolbar', { name: '所选项目操作' }).getByRole('button', { name: '移动' }).click()
  await expect(page.locator('.move-copy-dialog')).toBeVisible()
  await expect(page.locator('.directory-trigger')).toBeVisible()
  await page.locator('.move-copy-dialog').getByRole('button', { name: '移动', exact: true }).click()
  if (waitForBusy) await expect(page.locator('.move-copy-dialog .primary')).toBeDisabled()
}

async function openTransferOnly(page: Page, baseUrl: string) {
  await page.goto(`${baseUrl}/?transfer-mobile-reference=${Date.now()}`)
  await expect(page.getByRole('heading', { name: '我的文件', exact: true })).toBeVisible()
  await page.getByTitle('列表视图').click()
  const row = page.locator('.file-row').filter({ hasText: file.name })
  await row.getByRole('button', { name: '选择项目' }).click()
  await page.getByRole('toolbar', { name: '所选项目操作' }).getByRole('button', { name: '移动' }).click()
  await expect(page.locator('.move-copy-dialog')).toBeVisible()
}

async function transferLayout(page: Page) {
  return page.locator('.move-copy-dialog').evaluate(element => {
    const rect = element.getBoundingClientRect()
    const style = getComputedStyle(element)
    const child = (selector: string) => {
      const child = element.querySelector(selector)
      if (!child) return null
      const childRect = child.getBoundingClientRect()
      return {
        x: Math.round(childRect.x * 100) / 100,
        y: Math.round(childRect.y * 100) / 100,
        width: Math.round(childRect.width * 100) / 100,
        height: Math.round(childRect.height * 100) / 100,
      }
    }
    return {
      rect: {
        x: Math.round(rect.x * 100) / 100,
        y: Math.round(rect.y * 100) / 100,
        width: Math.round(rect.width * 100) / 100,
        height: Math.round(rect.height * 100) / 100,
      },
      width: style.width,
      padding: style.padding,
      header: child('header'),
      body: child('.move-copy-body'),
      picker: child('.directory-picker'),
      footer: child('footer'),
      buttons: Array.from(element.querySelectorAll('footer button')).map(button => {
        const buttonRect = button.getBoundingClientRect()
        return {
          text: button.textContent?.trim(),
          width: Math.round(buttonRect.width * 100) / 100,
          height: Math.round(buttonRect.height * 100) / 100,
        }
      }),
    }
  })
}

async function pickerLayout(page: Page) {
  return page.locator('.directory-popover').evaluate(element => {
    const rect = element.getBoundingClientRect()
    const style = getComputedStyle(element)
    return {
      x: Math.round(rect.x * 100) / 100,
      y: Math.round(rect.y * 100) / 100,
      width: Math.round(rect.width * 100) / 100,
      height: Math.round(rect.height * 100) / 100,
      maxHeight: style.maxHeight,
      position: style.position,
    }
  })
}

test('移动弹窗和目录下拉在手机视口保持 reference 布局', async ({ browser }) => {
  const oldUrl = process.env.E2E_REFERENCE_URL || 'http://127.0.0.1:18080'
  const newUrl = process.env.E2E_NEW_URL || 'http://127.0.0.1:18084'
  const oldContext = await browser.newContext({ viewport: { width: 390, height: 844 } })
  const newContext = await browser.newContext({ viewport: { width: 390, height: 844 } })
  const oldPage = await oldContext.newPage()
  const newPage = await newContext.newPage()

  try {
    await Promise.all([mockTransfer(oldPage), mockTransfer(newPage)])
    await Promise.all([openTransferOnly(oldPage, oldUrl), openTransferOnly(newPage, newUrl)])
    expect(await transferLayout(newPage), 'Rust 手机移动弹窗几何与 reference 不一致').toEqual(await transferLayout(oldPage))

    await Promise.all([
      oldPage.locator('.directory-trigger').click(),
      newPage.locator('.directory-trigger').click(),
    ])
    await Promise.all([
      expect(oldPage.getByRole('region', { name: '选择目标目录' })).toBeVisible(),
      expect(newPage.getByRole('region', { name: '选择目标目录' })).toBeVisible(),
    ])
    await Promise.all([oldPage.waitForTimeout(220), newPage.waitForTimeout(220)])
    expect(await pickerLayout(newPage), 'Rust 手机目录下拉几何与 reference 不一致').toEqual(await pickerLayout(oldPage))
    await Promise.all([
      oldPage.keyboard.press('Escape'),
      newPage.keyboard.press('Escape'),
    ])
    await Promise.all([
      expect(oldPage.getByRole('region', { name: '选择目标目录' })).toHaveCount(0),
      expect(newPage.getByRole('region', { name: '选择目标目录' })).toHaveCount(0),
    ])
  } finally {
    await oldContext.close()
    await newContext.close()
  }
})

test('移动请求进行中点击遮罩仍关闭传输弹窗', async ({ browser }) => {
  const oldUrl = process.env.E2E_REFERENCE_URL || 'http://127.0.0.1:18080'
  const newUrl = process.env.E2E_NEW_URL || 'http://127.0.0.1:18084'
  const oldContext = await browser.newContext({ viewport: { width: 1440, height: 900 } })
  const newContext = await browser.newContext({ viewport: { width: 1440, height: 900 } })
  const oldPage = await oldContext.newPage()
  const newPage = await newContext.newPage()

  try {
    const [oldMock, newMock] = await Promise.all([mockTransfer(oldPage), mockTransfer(newPage)])
    await Promise.all([startTransfer(oldPage, oldUrl), startTransfer(newPage, newUrl)])
    await Promise.all([
      oldPage.locator('.modal-backdrop').filter({ has: oldPage.locator('.move-copy-dialog') }).click({ position: { x: 8, y: 8 } }),
      newPage.locator('.transfer-backdrop').click({ position: { x: 8, y: 8 } }),
    ])
    await expect(oldPage.locator('.move-copy-dialog')).toHaveCount(0)
    await expect(newPage.locator('.move-copy-dialog'), 'Rust 传输请求中遮罩关闭与 reference 不一致').toHaveCount(0)
    oldMock.transferGate.resolve()
    newMock.transferGate.resolve()
  } finally {
    await oldContext.close()
    await newContext.close()
  }
})

test('移动部分失败的全局反馈文案保持 reference', async ({ browser }) => {
  const oldUrl = process.env.E2E_REFERENCE_URL || 'http://127.0.0.1:18080'
  const newUrl = process.env.E2E_NEW_URL || 'http://127.0.0.1:18084'
  const oldContext = await browser.newContext({ viewport: { width: 1440, height: 900 } })
  const newContext = await browser.newContext({ viewport: { width: 1440, height: 900 } })
  const oldPage = await oldContext.newPage()
  const newPage = await newContext.newPage()

  try {
    await Promise.all([mockTransfer(oldPage, true), mockTransfer(newPage, true)])
    await Promise.all([startTransfer(oldPage, oldUrl, false), startTransfer(newPage, newUrl, false)])
    await expect(oldPage.locator('.move-copy-dialog')).toHaveCount(0)
    await expect(newPage.locator('.move-copy-dialog')).toHaveCount(0)
    await expect(oldPage.locator('.toast')).toHaveText('已移动 0 项，1 项失败：传输中的文件.txt：move failed')
    await expect(newPage.locator('.toast')).toHaveText('已移动 0 项，1 项失败：传输中的文件.txt：move failed')
    expect(await newPage.locator('.toast').textContent()).toBe(await oldPage.locator('.toast').textContent())
  } finally {
    await oldContext.close()
    await newContext.close()
  }
})

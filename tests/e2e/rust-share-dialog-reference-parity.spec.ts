import { expect, test, type Page } from '@playwright/test'

const ROOT = '00000000-0000-0000-0000-000000000000'
const FILE_ID = 'share-dialog-file'
const STAMP = '2026-01-01T00:00:00Z'

const file = {
  id: FILE_ID,
  parent_id: ROOT,
  name: '分享状态.txt',
  kind: 'file',
  size: 123,
  status: 'ready',
  created_at: STAMP,
  updated_at: STAMP,
  mime_type: 'text/plain',
  etag: 'share-dialog-etag',
}

function deferred() {
  let resolve!: () => void
  const promise = new Promise<void>(value => { resolve = value })
  return { promise, resolve }
}

async function mockShare(page: Page) {
  const readGate = deferred()
  const createGate = deferred()
  let firstRead = true
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
    if (path === `/api/files/${FILE_ID}/share` && request.method() === 'GET') {
      if (firstRead) {
        firstRead = false
        await readGate.promise
      }
      return json({ active: false })
    }
    if (path === `/api/files/${FILE_ID}/share` && request.method() === 'POST') {
      await createGate.promise
      return json({ active: true, url: 'http://127.0.0.1:18084/s/share-dialog-token', created_at: STAMP })
    }
    return json({ items: [] })
  })
  return { readGate, createGate }
}

async function openShare(page: Page, baseUrl: string) {
  await page.goto(`${baseUrl}/`)
  await expect(page.getByRole('heading', { name: '我的文件', exact: true })).toBeVisible()
  await expect(page.locator('.file-card')).toHaveCount(1)
  await page.getByTitle('列表视图').click()
  const row = page.locator('.file-row').filter({ hasText: file.name })
  await row.getByRole('button', { name: '选择项目' }).click()
  await page.getByRole('toolbar', { name: '所选项目操作' }).getByRole('button', { name: '分享' }).click()
  await expect(page.locator('.share-modal .state.small')).toBeVisible()
}

async function dialogSnapshot(page: Page) {
  return page.locator('.share-modal').evaluate(dialog => ({
    text: dialog.textContent?.replace(/\s+/g, ' ').trim(),
    buttons: Array.from(dialog.querySelectorAll('button')).map(button => ({
      text: button.textContent?.replace(/\s+/g, ' ').trim(),
      disabled: (button as HTMLButtonElement).disabled,
    })),
    input: (() => {
      const input = dialog.querySelector('input') as HTMLInputElement | null
      return input ? { value: input.value, type: input.type, readOnly: input.readOnly } : null
    })(),
    rect: (() => {
      const rect = dialog.getBoundingClientRect()
      return { width: rect.width, height: rect.height }
    })(),
  }))
}

test('分享弹窗 loading 与 active 状态保持 reference', async ({ browser }) => {
  const oldUrl = process.env.E2E_REFERENCE_URL || 'http://127.0.0.1:18080'
  const newUrl = process.env.E2E_NEW_URL || 'http://127.0.0.1:18084'
  const oldContext = await browser.newContext({ viewport: { width: 1280, height: 900 } })
  const newContext = await browser.newContext({ viewport: { width: 1280, height: 900 } })
  const oldPage = await oldContext.newPage()
  const newPage = await newContext.newPage()

  try {
    const [oldMock, newMock] = await Promise.all([mockShare(oldPage), mockShare(newPage)])
    await Promise.all([openShare(oldPage, oldUrl), openShare(newPage, newUrl)])

    const oldClose = oldPage.locator('.share-modal header button')
    const newClose = newPage.locator('.share-modal header button')
    expect(await newClose.isDisabled(), 'Rust loading 时关闭按钮应保持 reference 可用状态').toBe(await oldClose.isDisabled())
    expect(await oldClose.isDisabled()).toBe(false)

    await Promise.all([oldClose.click(), newClose.click()])
    await expect(oldPage.locator('.share-modal')).toHaveCount(0)
    await expect(newPage.locator('.share-modal')).toHaveCount(0)
    oldMock.readGate.resolve()
    newMock.readGate.resolve()

    await Promise.all([
      oldPage.getByRole('toolbar', { name: '所选项目操作' }).getByRole('button', { name: '分享' }).click(),
      newPage.getByRole('toolbar', { name: '所选项目操作' }).getByRole('button', { name: '分享' }).click(),
    ])
    await expect(oldPage.locator('.share-modal').getByRole('button', { name: '创建公开链接' })).toBeVisible()
    await expect(newPage.locator('.share-modal').getByRole('button', { name: '创建公开链接' })).toBeVisible()
    expect(await dialogSnapshot(newPage), 'Rust 分享弹窗初始状态与 reference 不一致').toEqual(await dialogSnapshot(oldPage))

    await Promise.all([
      oldPage.locator('.share-modal').getByRole('button', { name: '创建公开链接' }).click(),
      newPage.locator('.share-modal').getByRole('button', { name: '创建公开链接' }).click(),
    ])
    await expect(oldPage.locator('.share-modal .state.small')).toBeVisible()
    await expect(newPage.locator('.share-modal .state.small')).toBeVisible()
    expect(await newClose.isDisabled(), 'Rust 创建分享 loading 时关闭按钮应保持 reference 可用状态').toBe(await oldClose.isDisabled())
    expect(await oldClose.isDisabled()).toBe(false)

    oldMock.createGate.resolve()
    newMock.createGate.resolve()
    await expect(oldPage.locator('.share-modal input[aria-label="分享链接"]')).toHaveValue(/\/s\//)
    await expect(newPage.locator('.share-modal input[aria-label="分享链接"]')).toHaveValue(/\/s\//)
    expect(await dialogSnapshot(newPage), 'Rust 分享 active 状态与 reference 不一致').toEqual(await dialogSnapshot(oldPage))

    await Promise.all([oldPage.keyboard.press('Escape'), newPage.keyboard.press('Escape')])
    await expect(oldPage.locator('.share-modal')).toBeVisible()
    await expect(newPage.locator('.share-modal')).toBeVisible()
  } finally {
    await oldContext.close()
    await newContext.close()
  }
})

async function mockShareConfirmation(page: Page) {
  const postGate = deferred()
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
    if (path === `/api/files/${FILE_ID}/share` && request.method() === 'GET') {
      return json({ active: true, url: 'http://127.0.0.1:18084/s/confirmation-token', created_at: STAMP })
    }
    if (path === `/api/files/${FILE_ID}/share` && request.method() === 'POST') {
      await postGate.promise
      return route.fulfill({
        status: 500,
        contentType: 'application/json',
        body: JSON.stringify({ error: { status: 500, message: 'share create failed' } }),
      })
    }
    return json({ items: [] })
  })
  return { postGate }
}

async function openActiveShare(page: Page, baseUrl: string) {
  await page.goto(`${baseUrl}/`)
  await expect(page.getByRole('heading', { name: '我的文件', exact: true })).toBeVisible()
  await page.getByTitle('列表视图').click()
  const row = page.locator('.file-row').filter({ hasText: file.name })
  await row.getByRole('button', { name: '选择项目' }).click()
  await page.getByRole('toolbar', { name: '所选项目操作' }).getByRole('button', { name: '分享' }).click()
  await expect(page.locator('.share-modal input[aria-label="分享链接"]')).toHaveValue(/\/s\//)
}

async function mockShareRegenerate(page: Page) {
  await page.route('**/api/**', async route => {
    const request = route.request()
    const path = new URL(request.url()).pathname
    const json = (value: unknown) => route.fulfill({ json: value })
    if (path === '/api/auth/me') return json({ username: 'admin', has_avatar: false })
    if (path === '/api/events' || path === '/api/system/status/stream') return route.fulfill({ contentType: 'text/event-stream', body: '' })
    if (path === '/api/tasks') return json({ items: [] })
    if (path === '/api/library/all') return json({ items: { book: [], image: [], video: [], audio: [] }, counts: { book: 0, image: 0, video: 0, audio: 0, file: 1 } })
    if (path === '/api/library/counts') return json({ book: 0, image: 0, video: 0, audio: 0, file: 1 })
    if (path === `/api/files/${ROOT}`) {
      return json({
        file: { id: ROOT, parent_id: null, name: '我的文件', kind: 'directory', size: 0, status: 'ready', created_at: STAMP, updated_at: STAMP, mime_type: '' },
        breadcrumbs: [],
      })
    }
    if (path === `/api/files/${ROOT}/children`) return json({ items: [file], total_bytes: file.size, file_count: 1 })
    if (path === `/api/files/${FILE_ID}/share` && request.method() === 'GET') {
      return json({ active: true, url: 'http://127.0.0.1:18084/s/old-share-token', created_at: STAMP })
    }
    if (path === `/api/files/${FILE_ID}/share` && request.method() === 'POST') {
      return json({ active: true, url: 'http://127.0.0.1:18084/s/new-share-token', created_at: STAMP })
    }
    return json({ items: [] })
  })
}

test('分享链接重生成成功时不额外产生全局 toast', async ({ browser }) => {
  const oldUrl = process.env.E2E_REFERENCE_URL || 'http://127.0.0.1:18080'
  const newUrl = process.env.E2E_NEW_URL || 'http://127.0.0.1:18084'
  const oldContext = await browser.newContext({ viewport: { width: 1280, height: 900 } })
  const newContext = await browser.newContext({ viewport: { width: 1280, height: 900 } })
  const oldPage = await oldContext.newPage()
  const newPage = await newContext.newPage()

  try {
    await Promise.all([mockShareRegenerate(oldPage), mockShareRegenerate(newPage)])
    await Promise.all([openActiveShare(oldPage, oldUrl), openActiveShare(newPage, newUrl)])
    await Promise.all([
      oldPage.locator('.share-modal').getByRole('button', { name: '重新生成链接' }).click(),
      newPage.locator('.share-modal').getByRole('button', { name: '重新生成链接' }).click(),
    ])
    await Promise.all([
      oldPage.locator('.app-dialog').getByRole('button', { name: '重新生成' }).click(),
      newPage.locator('.app-dialog').getByRole('button', { name: '重新生成' }).click(),
    ])
    await Promise.all([
      expect(oldPage.locator('.share-modal input[aria-label="分享链接"]')).toHaveValue(/new-share-token/),
      expect(newPage.locator('.share-modal input[aria-label="分享链接"]')).toHaveValue(/new-share-token/),
    ])
    expect(await newPage.locator('.toast').count(), 'Rust 重生成成功不应新增 reference 没有的全局 toast').toBe(await oldPage.locator('.toast').count())
    expect(await oldPage.locator('.toast').count()).toBe(0)

    await Promise.all([
      oldPage.locator('.share-modal').getByRole('button', { name: '停止分享' }).click(),
      newPage.locator('.share-modal').getByRole('button', { name: '停止分享' }).click(),
    ])
    await Promise.all([
      oldPage.locator('.app-dialog').getByRole('button', { name: '停止分享' }).click(),
      newPage.locator('.app-dialog').getByRole('button', { name: '停止分享' }).click(),
    ])
    await expect(oldPage.locator('.toast')).toHaveText('分享已停止')
    await expect(newPage.locator('.toast')).toHaveText('分享已停止')
    expect(await newPage.locator('.toast').getAttribute('class')).toBe(await oldPage.locator('.toast').getAttribute('class'))
  } finally {
    await Promise.all([oldContext.close(), newContext.close()])
  }
})

test('分享二级确认取消、提交关闭和错误回显保持 reference', async ({ browser }) => {
  const oldUrl = process.env.E2E_REFERENCE_URL || 'http://127.0.0.1:18080'
  const newUrl = process.env.E2E_NEW_URL || 'http://127.0.0.1:18084'
  const oldContext = await browser.newContext({ viewport: { width: 1280, height: 900 } })
  const newContext = await browser.newContext({ viewport: { width: 1280, height: 900 } })
  const oldPage = await oldContext.newPage()
  const newPage = await newContext.newPage()

  try {
    const [oldMock, newMock] = await Promise.all([mockShareConfirmation(oldPage), mockShareConfirmation(newPage)])
    await Promise.all([openActiveShare(oldPage, oldUrl), openActiveShare(newPage, newUrl)])

    const oldShare = oldPage.locator('.share-modal')
    const newShare = newPage.locator('.share-modal')
    await Promise.all([
      oldShare.getByRole('button', { name: '重新生成链接' }).click(),
      newShare.getByRole('button', { name: '重新生成链接' }).click(),
    ])
    const oldConfirm = oldPage.locator('.app-dialog')
    const newConfirm = newPage.locator('.app-dialog')
    await expect(oldConfirm).toBeVisible()
    await expect(newConfirm).toBeVisible()
    const confirmationSnapshot = async (page: Page) => page.locator('.app-dialog').evaluate(dialog => ({
      text: dialog.textContent?.replace(/\s+/g, ' ').trim(),
      buttons: Array.from(dialog.querySelectorAll('button')).map(button => ({
        text: button.textContent?.replace(/\s+/g, ' ').trim(),
        disabled: (button as HTMLButtonElement).disabled,
      })),
    }))
    expect(await confirmationSnapshot(newPage), 'Rust 分享二级确认与 reference 不一致').toEqual(await confirmationSnapshot(oldPage))

    await Promise.all([
      oldConfirm.getByRole('button', { name: '取消' }).click(),
      newConfirm.getByRole('button', { name: '取消' }).click(),
    ])
    await expect(oldPage.locator('.app-dialog')).toHaveCount(0)
    await expect(newPage.locator('.app-dialog')).toHaveCount(0)
    await expect(oldShare.locator('input[aria-label="分享链接"]')).toHaveValue(/\/s\//)
    await expect(newShare.locator('input[aria-label="分享链接"]')).toHaveValue(/\/s\//)

    await Promise.all([
      oldShare.getByRole('button', { name: '重新生成链接' }).click(),
      newShare.getByRole('button', { name: '重新生成链接' }).click(),
    ])
    await Promise.all([
      oldPage.locator('.app-dialog').getByRole('button', { name: '重新生成' }).click(),
      newPage.locator('.app-dialog').getByRole('button', { name: '重新生成' }).click(),
    ])
    await expect(oldPage.locator('.app-dialog')).toHaveCount(0)
    await expect(newPage.locator('.app-dialog')).toHaveCount(0)
    await expect(oldShare.locator('.state.small')).toBeVisible()
    await expect(newShare.locator('.state.small')).toBeVisible()

    oldMock.postGate.resolve()
    newMock.postGate.resolve()
    await expect(oldShare.locator('.form-error')).toHaveText('share create failed')
    await expect(newShare.locator('.form-error')).toHaveText('share create failed')
    expect(await newShare.locator('.form-error').getAttribute('role'), 'Rust 分享失败提示的 DOM 语义应与 reference 一致')
      .toBe(await oldShare.locator('.form-error').getAttribute('role'))
    expect(await dialogSnapshot(newPage), 'Rust 分享二级确认失败状态与 reference 不一致').toEqual(await dialogSnapshot(oldPage))
  } finally {
    await oldContext.close()
    await newContext.close()
  }
})

async function installClipboard(page: Page) {
  await page.addInitScript(() => {
    Object.defineProperty(navigator, 'clipboard', {
      configurable: true,
      value: {
        writeText: async (value: string) => {
          ;(window as Window & { __copiedShare?: string }).__copiedShare = value
        },
      },
    })
  })
}

async function installClipboardFailure(page: Page) {
  await page.addInitScript(() => {
    Object.defineProperty(navigator, 'clipboard', {
      configurable: true,
      value: {
        writeText: async () => {
          throw new Error('clipboard denied')
        },
      },
    })
  })
}

test('复制分享链接只更新弹窗状态，不额外产生全局 toast', async ({ browser }) => {
  const oldUrl = process.env.E2E_REFERENCE_URL || 'http://127.0.0.1:18080'
  const newUrl = process.env.E2E_NEW_URL || 'http://127.0.0.1:18084'
  const oldContext = await browser.newContext({ viewport: { width: 1280, height: 900 } })
  const newContext = await browser.newContext({ viewport: { width: 1280, height: 900 } })
  const oldPage = await oldContext.newPage()
  const newPage = await newContext.newPage()

  try {
    await Promise.all([
      mockShareConfirmation(oldPage),
      mockShareConfirmation(newPage),
      installClipboard(oldPage),
      installClipboard(newPage),
    ])
    await Promise.all([openActiveShare(oldPage, oldUrl), openActiveShare(newPage, newUrl)])

    await Promise.all([
      oldPage.locator('.share-modal').getByRole('button', { name: '复制链接' }).click(),
      newPage.locator('.share-modal').getByRole('button', { name: '复制链接' }).click(),
    ])
    await expect(oldPage.locator('.share-modal').getByRole('button', { name: '已复制' })).toBeVisible()
    await expect(newPage.locator('.share-modal').getByRole('button', { name: '已复制' })).toBeVisible()
    expect(await newPage.locator('.toast').count(), 'Rust 复制成功不应新增 reference 没有的 toast').toBe(await oldPage.locator('.toast').count())
    expect(await newPage.evaluate(() => (window as Window & { __copiedShare?: string }).__copiedShare))
      .toBe(await newPage.locator('.share-modal input[aria-label="分享链接"]').inputValue())
  } finally {
    await oldContext.close()
    await newContext.close()
  }
})

test('复制分享链接失败时保留 reference 的弹窗错误且不产生全局 toast', async ({ browser }) => {
  const oldUrl = process.env.E2E_REFERENCE_URL || 'http://127.0.0.1:18080'
  const newUrl = process.env.E2E_NEW_URL || 'http://127.0.0.1:18084'
  const oldContext = await browser.newContext({ viewport: { width: 1280, height: 900 } })
  const newContext = await browser.newContext({ viewport: { width: 1280, height: 900 } })
  const oldPage = await oldContext.newPage()
  const newPage = await newContext.newPage()

  try {
    await Promise.all([
      mockShareConfirmation(oldPage),
      mockShareConfirmation(newPage),
      installClipboardFailure(oldPage),
      installClipboardFailure(newPage),
    ])
    await Promise.all([openActiveShare(oldPage, oldUrl), openActiveShare(newPage, newUrl)])

    await Promise.all([
      oldPage.locator('.share-modal').getByRole('button', { name: '复制链接' }).click(),
      newPage.locator('.share-modal').getByRole('button', { name: '复制链接' }).click(),
    ])
    await expect(oldPage.locator('.share-modal .form-error')).toHaveText('复制失败，请手动选择链接复制')
    await expect(newPage.locator('.share-modal .form-error')).toHaveText('复制失败，请手动选择链接复制')
    expect(await newPage.locator('.share-modal .form-error').getAttribute('role'), 'Rust 分享复制错误提示的 DOM 语义应与 reference 一致')
      .toBe(await oldPage.locator('.share-modal .form-error').getAttribute('role'))
    await expect(oldPage.locator('.share-modal').getByRole('button', { name: '复制链接' })).toBeVisible()
    await expect(newPage.locator('.share-modal').getByRole('button', { name: '复制链接' })).toBeVisible()
    expect(await newPage.locator('.share-modal').textContent()).toBe(await oldPage.locator('.share-modal').textContent())
    expect(await newPage.locator('.toast').count(), '复制失败不应冒出 reference 没有的全局 toast').toBe(await oldPage.locator('.toast').count())
  } finally {
    await oldContext.close()
    await newContext.close()
  }
})

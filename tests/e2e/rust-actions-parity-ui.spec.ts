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

/** Build a small stored ZIP for the real online-extraction flow. */
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

async function selectRow(page: Parameters<typeof login>[0], name: string) {
  const row = page.locator('.file-row').filter({ hasText: name })
  await expect(row).toBeVisible()
  await row.getByRole('button', { name: '选择项目' }).click()
}

async function publicFetch(page: Parameters<typeof login>[0], url: string) {
  return page.evaluate(async target => {
    const response = await fetch(target, { credentials: 'omit' })
    return { status: response.status, body: await response.text() }
  }, url)
}

async function publicResponse(page: Parameters<typeof login>[0], url: string, range?: string) {
  return page.evaluate(async ({ target, rangeValue }) => {
    const response = await fetch(target, {
      credentials: 'omit',
      headers: rangeValue ? { Range: rangeValue } : undefined,
    })
    const headers = Object.fromEntries([
      'cache-control',
      'content-disposition',
      'content-length',
      'content-range',
      'content-security-policy',
      'accept-ranges',
      'referrer-policy',
      'x-robots-tag',
    ].map(name => [name, response.headers.get(name) || '']))
    return { status: response.status, body: await response.text(), headers }
  }, { target: url, rangeValue: range })
}

async function removeCreated(page: Parameters<typeof login>[0], names: string[]) {
  await page.evaluate(async wanted => {
    const headers = { 'Content-Type': 'application/json' }
    const childrenResponse = await fetch('/api/files/00000000-0000-0000-0000-000000000000/children')
    if (!childrenResponse.ok) return
    const children = await childrenResponse.json() as { items?: Array<{ id: string; name: string }> }
    for (const item of children.items ?? []) {
      if (wanted.includes(item.name)) {
        await fetch(`/api/files/${item.id}`, { method: 'DELETE', headers })
      }
    }
    const trashResponse = await fetch('/api/trash')
    if (!trashResponse.ok) return
    const trash = await trashResponse.json() as { items?: Array<{ id: string; name: string }> }
    for (const item of trash.items ?? []) {
      if (wanted.includes(item.name)) {
        await fetch(`/api/trash/${item.id}`, { method: 'DELETE', headers })
      }
    }
  }, names)
}

test('单文件下载与分享链接生命周期保持 reference 行为', async ({ page, context }) => {
  const suffix = crypto.randomUUID()
  const name = `parity-share-${suffix}.txt`
  const content = `share parity ${suffix}\n`
  const baseOrigin = new URL(process.env.E2E_BASE_URL || 'http://127.0.0.1:18080').origin
  await context.grantPermissions(['clipboard-read', 'clipboard-write'], { origin: baseOrigin })

  try {
    await login(page)
    await page.locator('input[type=file]').first().setInputFiles({ name, mimeType: 'text/plain', buffer: Buffer.from(content) })
    await expect(page.locator('.file-card').filter({ hasText: name })).toBeVisible({ timeout: 20_000 })
    await page.getByRole('button', { name: '列表' }).click()
    await selectRow(page, name)

    const toolbar = page.getByRole('toolbar', { name: '所选项目操作' })
    await toolbar.getByRole('button', { name: '分享' }).click()
    const share = page.locator('.share-modal')
    await expect(share).toBeVisible()
    await expect(share).toContainText('创建公开链接')
    await share.getByRole('button', { name: '创建公开链接' }).click()
    const link = share.locator('input[aria-label="分享链接"]')
    await expect.poll(() => link.inputValue()).toMatch(/\/s\/[A-Za-z0-9_-]{32,}/)
    const firstUrl = await link.inputValue()
    expect(await publicFetch(page, firstUrl)).toEqual({ status: 200, body: content })

    const publicFile = await publicResponse(page, firstUrl)
    expect(publicFile.status).toBe(200)
    expect(publicFile.body).toBe(content)
    expect(publicFile.headers['cache-control']).toBe('no-store')
    expect(publicFile.headers['referrer-policy']).toBe('no-referrer')
    expect(publicFile.headers['x-robots-tag']).toBe('noindex, nofollow, noarchive')
    expect(publicFile.headers['content-security-policy']).toBe('sandbox; default-src \'none\'; base-uri \'none\'; form-action \'none\'')
    expect(publicFile.headers['accept-ranges']).toBe('bytes')
    // Text is intentionally downloaded by the reference server; only its
    // previewable media types are served inline through a public link.
    expect(publicFile.headers['content-disposition']).toContain(`attachment; filename*=UTF-8''${encodeURIComponent(name)}`)

    const publicRange = await publicResponse(page, firstUrl, 'bytes=0-5')
    expect(publicRange.status).toBe(206)
    expect(publicRange.body).toBe(content.slice(0, 6))
    expect(publicRange.headers['content-range']).toBe(`bytes 0-5/${Buffer.byteLength(content)}`)
    expect(publicRange.headers['content-length']).toBe('6')

    const malformed = await publicResponse(page, `${baseOrigin}/s/short`)
    expect(malformed.status).toBe(404)

    await share.getByRole('button', { name: '复制链接' }).click()
    await expect(share.getByRole('button', { name: '已复制' })).toBeVisible()

    await share.getByRole('button', { name: '重新生成链接' }).click()
    const regenerate = page.getByRole('dialog').filter({ hasText: '重新生成分享链接？' })
    await regenerate.getByRole('button', { name: '重新生成' }).click()
    await expect.poll(() => link.inputValue()).not.toBe(firstUrl)
    const secondUrl = await link.inputValue()
    expect((await publicFetch(page, firstUrl)).status).toBe(404)
    expect(await publicFetch(page, secondUrl)).toEqual({ status: 200, body: content })

    await share.getByRole('button', { name: '停止分享' }).click()
    const revoke = page.getByRole('dialog').filter({ hasText: '停止分享？' })
    await revoke.getByRole('button', { name: '停止分享' }).click()
    await expect(share).toContainText('创建公开链接')
    expect((await publicFetch(page, secondUrl)).status).toBe(404)

    await share.locator('header button').click()
    await expect(share).toHaveCount(0)
    const downloadPromise = page.waitForEvent('download')
    await toolbar.getByRole('button', { name: '下载' }).click()
    expect((await downloadPromise).suggestedFilename()).toBe(name)
  } finally {
    await removeCreated(page, [name])
  }
})

test('在线解压通过任务中心完成并显示解压目录', async ({ page }) => {
  const suffix = crypto.randomUUID()
  const archive = `parity-archive-${suffix}.zip`
  const output = archive.replace(/\.zip$/i, '')
  const entry = `entry-${suffix}.txt`

  try {
    await login(page)
    await page.locator('input[type=file]').first().setInputFiles({
      name: archive,
      mimeType: 'application/zip',
      buffer: zip([[entry, Buffer.from(`archive parity ${suffix}\n`)]]) ,
    })
    await expect(page.locator('.file-card').filter({ hasText: archive })).toBeVisible({ timeout: 20_000 })
    await page.getByRole('button', { name: '列表' }).click()
    await selectRow(page, archive)
    const toolbar = page.getByRole('toolbar', { name: '所选项目操作' })
    await toolbar.getByRole('button', { name: '在线解压' }).click()
    const confirm = page.getByRole('dialog').filter({ hasText: '在线解压' })
    await expect(confirm).toContainText('解压到当前目录中的新文件夹')
    await confirm.getByRole('button', { name: '开始解压' }).click()

    await expect.poll(async () => page.evaluate(async fileName => {
      const response = await fetch('/api/tasks')
      if (!response.ok) return false
      const payload = await response.json() as { items?: Array<{ type: string; status: string; name: string }> }
      return (payload.items ?? []).some(task => task.type === 'archive_extract' && task.name === fileName && task.status === 'completed')
    }, archive), { timeout: 45_000 }).toBe(true)

    await page.reload()
    await expect(page.getByRole('heading', { name: '我的文件' })).toBeVisible()
    const outputEntry = page.locator('.file-card, .file-row').filter({ hasText: output }).last()
    await expect(outputEntry).toBeVisible({ timeout: 20_000 })
    await outputEntry.click()
    await expect(page.getByRole('heading', { name: output })).toBeVisible()
    await expect(page.locator('.file-card, .file-row').filter({ hasText: entry })).toBeVisible()
  } finally {
    await removeCreated(page, [archive, output])
  }
})

test('回收站中的 TXT 仍可通过键盘 Enter 打开 reference 阅读器', async ({ page }) => {
  const suffix = crypto.randomUUID()
  const name = `parity-trash-${suffix}.txt`

  try {
    await login(page)
    await page.locator('input[type=file]').first().setInputFiles({ name, mimeType: 'text/plain', buffer: Buffer.from('trash keyboard parity\n') })
    await expect(page.locator('.file-card, .file-row').filter({ hasText: name })).toBeVisible({ timeout: 20_000 })
    await page.getByRole('button', { name: '列表' }).click()
    await selectRow(page, name)
    await page.getByRole('toolbar', { name: '所选项目操作' }).getByRole('button', { name: '删除' }).click()
    await page.getByRole('dialog').getByRole('button', { name: '移入回收站' }).click()
    await expect(page.locator('.file-row').filter({ hasText: name })).toHaveCount(0)

    await page.locator('.trash-entry').click()
    const trashRow = page.locator('.file-row').filter({ hasText: name })
    await expect(trashRow).toBeVisible()
    await trashRow.focus()
    await page.keyboard.press('Enter')
    await expect(page.locator('.reader-shell')).toBeVisible()
    await page.getByRole('button', { name: '返回', exact: true }).click()
  } finally {
    await removeCreated(page, [name])
  }
})

test('成功 toast 使用 reference 的颜色并在固定时限后消失', async ({ page }) => {
  const suffix = crypto.randomUUID()
  const name = `parity-toast-${suffix}`

  try {
    await login(page)
    await page.getByRole('button', { name: '新建文件夹', exact: true }).first().click()
    const dialog = page.locator('.app-dialog')
    await expect(dialog).toBeVisible()
    await dialog.locator('input').fill(name)
    await dialog.getByRole('button', { name: '创建' }).click()

    const toast = page.locator('.toast')
    await expect(toast).toHaveText('文件夹已创建')
    await expect(toast).toHaveClass(/success/)
    await expect(toast).toHaveCount(0, { timeout: 5_000 })
  } finally {
    await removeCreated(page, [name])
  }
})

test('目录刷新不会清除刚显示的成功 toast', async ({ page }) => {
  const name = `parity-toast-navigation-${crypto.randomUUID()}`

  try {
    await login(page)
    await page.getByRole('button', { name: '新建文件夹', exact: true }).first().click()
    const dialog = page.locator('.app-dialog')
    await dialog.locator('input').fill(name)
    await dialog.getByRole('button', { name: '创建', exact: true }).click()

    const toast = page.locator('.toast')
    await expect(toast).toHaveText('文件夹已创建')
    await page.getByTitle('回到我的文件').click()
    await expect(page.getByRole('heading', { name: '我的文件', exact: true })).toBeVisible()
    await expect(toast).toHaveText('文件夹已创建')
  } finally {
    await removeCreated(page, [name])
  }
})

test('新建文件夹弹窗保留空值禁用、Enter、Escape 和点击空白关闭行为', async ({ page }) => {
  const name = `parity-dialog-${crypto.randomUUID()}`

  try {
    await login(page)
    await page.getByRole('button', { name: '新建文件夹', exact: true }).first().click()
    const dialog = page.locator('.app-dialog')
    const input = dialog.locator('input')
    const confirm = dialog.getByRole('button', { name: '创建', exact: true })
    await expect(dialog).toBeVisible()
    await expect(confirm).toBeDisabled()
    await input.press('Enter')
    await expect(dialog).toBeVisible()

    await page.locator('.dialog-backdrop').click({ position: { x: 8, y: 8 } })
    await expect(dialog).toHaveCount(0)

    await page.getByRole('button', { name: '新建文件夹', exact: true }).first().click()
    await expect(page.locator('.app-dialog')).toBeVisible()
    await page.keyboard.press('Escape')
    await expect(page.locator('.app-dialog')).toHaveCount(0)

    await page.getByRole('button', { name: '新建文件夹', exact: true }).first().click()
    await page.locator('.app-dialog input').fill(name)
    await page.locator('.app-dialog input').press('Enter')
    await expect(page.locator('.file-card, .file-row').filter({ hasText: name })).toBeVisible()
  } finally {
    await removeCreated(page, [name])
  }
})

test('通用确认/输入操作失败时按 reference 关闭弹窗并显示错误 toast', async ({ page }) => {
  await login(page)
  await page.route('**/api/directories', route => route.fulfill({
    status: 409,
    contentType: 'application/json',
    body: JSON.stringify({ error: { status: 409, message: 'folder already exists' } }),
  }))

  await page.getByRole('button', { name: '新建文件夹', exact: true }).first().click()
  const dialog = page.locator('.app-dialog')
  await dialog.locator('input').fill(`compat-dialog-error-${crypto.randomUUID()}`)
  await dialog.getByRole('button', { name: '创建', exact: true }).click()

  await expect(dialog).toHaveCount(0)
  await expect(page.locator('.toast')).toHaveText('folder already exists')
})

async function createFolder(page: Parameters<typeof login>[0], name: string) {
  await page.getByRole('button', { name: '新建文件夹', exact: true }).first().click()
  const dialog = page.locator('.app-dialog')
  await expect(dialog).toBeVisible()
  await dialog.locator('input').fill(name)
  await dialog.getByRole('button', { name: '创建' }).click()
  await expect(page.locator('.file-card, .file-row').filter({ hasText: name })).toBeVisible()
}

async function chooseDirectory(page: Parameters<typeof login>[0], name: string) {
  await page.locator('.directory-trigger').click()
  const picker = page.getByRole('region', { name: '选择目标目录' })
  // Each transfer dialog remembers its current picker location.  Return to
  // the virtual root so sibling targets are reachable after the first move.
  await picker.getByRole('button', { name: '我的文件', exact: true }).click()
  await picker.getByRole('button', { name, exact: true }).click()
  // Folder navigation is asynchronous; wait until the parent dialog has
  // received the selected id before submitting the transfer.
  await expect(page.locator('.directory-trigger')).toHaveAttribute('title', new RegExp(name))
  // Selecting a folder changes the target but keeps the Vue picker open.
  // Close it through the reference Escape path before confirming the dialog.
  await page.keyboard.press('Escape')
  await expect(picker).toHaveCount(0)
}

test('列表模式覆盖新建、重命名、移动、恢复和永久删除', async ({ page }) => {
  const suffix = crypto.randomUUID()
  const sourceFolder = `parity-crud-source-${suffix}`
  const targetFolder = `parity-crud-target-${suffix}`
  const sourceFile = `parity-crud-${suffix}.txt`
  const renamedFile = `parity-crud-renamed-${suffix}.txt`
  let movedName = renamedFile

  try {
    await login(page)
    await createFolder(page, sourceFolder)
    await createFolder(page, targetFolder)
    await page.locator('input[type=file]').first().setInputFiles({
      name: sourceFile,
      mimeType: 'text/plain',
      buffer: Buffer.from('crud parity\n'),
    })
    await expect(page.locator('.file-card').filter({ hasText: sourceFile })).toBeVisible({ timeout: 20_000 })
    await page.getByRole('button', { name: '列表', exact: true }).click()

    await selectRow(page, sourceFile)
    let toolbar = page.getByRole('toolbar', { name: '所选项目操作' })
    await toolbar.getByRole('button', { name: '重命名' }).click()
    const rename = page.locator('.modal-backdrop > .modal').filter({ hasText: '重命名' })
    await rename.locator('input').fill(renamedFile)
    await rename.getByRole('button', { name: '保存' }).click()
    await expect(page.locator('.file-row').filter({ hasText: renamedFile })).toBeVisible()

    await selectRow(page, renamedFile)
    toolbar = page.getByRole('toolbar', { name: '所选项目操作' })
    await toolbar.getByRole('button', { name: '移动' }).click()
    await chooseDirectory(page, sourceFolder)
    await page.locator('.move-copy-dialog').getByRole('button', { name: '移动', exact: true }).click()
    await page.locator('.move-copy-dialog').waitFor({ state: 'detached' })
    await expect(page.locator('.file-row').filter({ hasText: renamedFile })).toHaveCount(0)

    await page.locator('.file-row').filter({ hasText: sourceFolder }).click()
    const movedInSource = page.locator('.file-row').filter({ hasText: renamedFile })
    await expect(movedInSource).toBeVisible()
    await selectRow(page, renamedFile)
    toolbar = page.getByRole('toolbar', { name: '所选项目操作' })
    await toolbar.getByRole('button', { name: '移动' }).click()
    await chooseDirectory(page, targetFolder)
    await page.locator('.move-copy-dialog').getByRole('button', { name: '移动', exact: true }).click()
    await expect(page.locator('.move-copy-dialog')).toHaveCount(0)

    await page.getByRole('button', { name: '回到我的文件' }).click()
    await page.locator('.file-row').filter({ hasText: targetFolder }).click()
    const movedInTarget = page.locator('.file-row').filter({ hasText: renamedFile })
    await expect(movedInTarget).toBeVisible()
    movedName = await movedInTarget.locator('strong').innerText()

    await selectRow(page, movedName)
    toolbar = page.getByRole('toolbar', { name: '所选项目操作' })
    await toolbar.getByRole('button', { name: '删除' }).click()
    await page.getByRole('dialog').getByRole('button', { name: '移入回收站' }).click()
    await expect(page.locator('.file-row').filter({ hasText: movedName })).toHaveCount(0)

    await page.getByRole('button', { name: '打开回收站' }).click()
    await selectRow(page, movedName)
    await page.getByRole('toolbar', { name: '所选项目操作' }).getByRole('button', { name: '恢复' }).click()
    await expect(page.locator('.file-row').filter({ hasText: movedName })).toHaveCount(0)

    await page.getByRole('button', { name: '回到我的文件' }).click()
    await page.locator('.file-row').filter({ hasText: targetFolder }).click()
    await expect(page.locator('.file-row').filter({ hasText: movedName })).toBeVisible()
    await selectRow(page, movedName)
    await page.getByRole('toolbar', { name: '所选项目操作' }).getByRole('button', { name: '删除' }).click()
    await page.getByRole('dialog').getByRole('button', { name: '移入回收站' }).click()
    await page.getByRole('button', { name: '打开回收站' }).click()
    await selectRow(page, movedName)
    await page.getByRole('toolbar', { name: '所选项目操作' }).getByRole('button', { name: '永久删除' }).click()
    await page.getByRole('dialog').getByRole('button', { name: '永久删除' }).click()
    await expect(page.locator('.file-row').filter({ hasText: movedName })).toHaveCount(0)
  } finally {
    await removeCreated(page, [sourceFolder, targetFolder, sourceFile, renamedFile])
  }
})

test('列表模式多选文件通过一次 ZIP 下载并保留 frame CSP', async ({ page }) => {
  const suffix = crypto.randomUUID()
  const names = [`parity-batch-one-${suffix}.txt`, `parity-batch-two-${suffix}.txt`]

  try {
    await login(page)
    await page.locator('input[type=file]').first().setInputFiles(names.map(name => ({
      name,
      mimeType: 'text/plain',
      buffer: Buffer.from(`batch ${name}\n`),
    })))
    for (const name of names) {
      await expect(page.locator('.file-card').filter({ hasText: name })).toBeVisible({ timeout: 20_000 })
    }
    await page.getByRole('button', { name: '列表', exact: true }).click()
    for (const name of names) await selectRow(page, name)
    await expect(page.locator('iframe')).toHaveCount(0)

    const csp = await page.evaluate(() => document.querySelector('meta[http-equiv="Content-Security-Policy"]')?.getAttribute('content') || '')
    const response = await page.request.get('/')
    expect(response.headers()['content-security-policy']).toContain("frame-src 'none'")
    expect(csp).toBe('')

    const downloadPromise = page.waitForEvent('download')
    await page.getByRole('toolbar', { name: '所选项目操作' }).getByRole('button', { name: '下载 (2)' }).click()
    expect((await downloadPromise).suggestedFilename()).toBe('revaro-download.zip')
    await expect(page.locator('iframe')).toHaveCount(0)
  } finally {
    await removeCreated(page, names)
  }
})

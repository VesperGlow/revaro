import { expect, test } from '@playwright/test'
import { login } from './helpers'

async function removeByName(page: Parameters<typeof login>[0], name: string) {
  await page.evaluate(async wanted => {
    const headers = { 'Content-Type': 'application/json' }
    const root = await fetch('/api/files/00000000-0000-0000-0000-000000000000/children')
    if (!root.ok) return
    const children = await root.json() as { items?: Array<{ id: string; name: string }> }
    for (const item of children.items ?? []) {
      if (wanted.includes(item.name)) await fetch(`/api/files/${item.id}`, { method: 'DELETE', headers })
    }
    const trash = await fetch('/api/trash')
    if (!trash.ok) return
    const deleted = await trash.json() as { items?: Array<{ id: string; name: string }> }
    for (const item of deleted.items ?? []) {
      if (wanted.includes(item.name)) await fetch(`/api/trash/${item.id}`, { method: 'DELETE', headers })
    }
  }, [name])
}

async function removeByNames(page: Parameters<typeof login>[0], names: string[]) {
  await page.evaluate(async wanted => {
    const headers = { 'Content-Type': 'application/json' }
    const root = await fetch('/api/files/00000000-0000-0000-0000-000000000000/children')
    if (root.ok) {
      const children = await root.json() as { items?: Array<{ id: string; name: string }> }
      for (const item of children.items ?? []) {
        if (wanted.includes(item.name)) await fetch(`/api/files/${item.id}`, { method: 'DELETE', headers })
      }
    }
    const trash = await fetch('/api/trash')
    if (!trash.ok) return
    const deleted = await trash.json() as { items?: Array<{ id: string; name: string }> }
    for (const item of deleted.items ?? []) {
      if (wanted.includes(item.name)) await fetch(`/api/trash/${item.id}`, { method: 'DELETE', headers })
    }
  }, names)
}

async function loginAt(page: Parameters<typeof login>[0], baseUrl: string) {
  await page.goto(`${baseUrl}/?editor-reference=${Date.now()}`)
  await page.getByLabel('用户名').fill(process.env.E2E_USERNAME || 'admin')
  await page.getByLabel('密码').fill(process.env.E2E_PASSWORD || 'revaro-e2e-password')
  await page.getByRole('button', { name: '进入我的网盘' }).click()
  await expect(page.getByRole('heading', { name: '我的文件', exact: true })).toBeVisible()
}

async function createDocument(page: Parameters<typeof login>[0], name: string, content: string) {
  return page.evaluate(async ({ name, content }) => {
    const response = await fetch('/api/documents', {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({
        parent_id: '00000000-0000-0000-0000-000000000000',
        name,
        content,
      }),
    })
    if (!response.ok) throw new Error(`创建文档失败：${response.status}`)
    return (await response.json() as { id: string }).id
  }, { name, content })
}

// The reference app still exposes a grid/list switch, while the Rust app only
// renders the grid. Detect the reference switch so dual-version exercises can
// enter through each app's own listing layout.
async function useReferenceListView(page: Parameters<typeof login>[0]) {
  const toggle = page.getByTitle('列表视图')
  if (await toggle.count()) {
    await toggle.click()
    return true
  }
  return false
}

function fileEntries(page: Parameters<typeof login>[0], useListView: boolean) {
  return page.locator(useListView ? '.file-row' : '.file-card')
}

test('新文档扩展名校验保留原始输入的尾随空格行为', async ({ page }) => {
  const trimmedName = `editor-trailing-${Date.now().toString(36)}.md`
  const typedName = `${trimmedName} `

  try {
    await login(page)
    await page.getByRole('button', { name: '新建文档', exact: true }).first().click()
    const editor = page.locator('.document-editor')
    await expect(editor).toBeVisible()
    await editor.getByRole('textbox', { name: '文档文件名' }).fill(typedName)
    await editor.getByRole('button', { name: '保存' }).click()
    await expect(editor).toContainText('支持 Markdown、TXT、YAML、JSON、TOML、INI、CONF、LOG 和 CSV')
    await expect(editor.getByRole('button', { name: '保存' })).toBeEnabled()
  } finally {
    await removeByName(page, trimmedName)
  }
})

test('回收站可编辑文档按 reference 保持只读编辑器分流', async ({ page }) => {
  const suffix = Date.now().toString(36)
  const textName = `editor-trash-${suffix}.yaml`
  const markdownName = `editor-trash-${suffix}.md`

  try {
    await login(page)
    await createDocument(page, textName, 'message: 回收站 YAML 内容')
    await createDocument(page, markdownName, '# 回收站 Markdown')
    await page.reload()
    await expect(page.locator('.file-card').filter({ hasText: textName })).toBeVisible()
    await expect(page.locator('.file-card').filter({ hasText: markdownName })).toBeVisible()

    for (const name of [textName, markdownName]) {
      const card = page.locator('.file-card').filter({ hasText: name })
      await card.getByRole('button', { name: '选择项目' }).click()
    }
    await page.getByRole('toolbar', { name: '所选项目操作' }).getByRole('button', { name: '删除' }).click()
    const confirm = page.getByRole('dialog').filter({ hasText: '移入回收站？' })
    await confirm.getByRole('button', { name: '移入回收站' }).click()
    await expect(page.locator('.file-card').filter({ hasText: textName })).toHaveCount(0)

    await page.locator('.topbar .trash-button').click()
    const textRow = page.locator('.file-card').filter({ hasText: textName })
    const markdownRow = page.locator('.file-card').filter({ hasText: markdownName })
    await expect(textRow).toBeVisible()
    await textRow.click()
    const textEditor = page.locator('.document-editor')
    await expect(textEditor).toBeVisible()
    await expect(textEditor.locator('.editor-title small')).toHaveText('回收站只读预览')
    await expect(textEditor.locator('textarea')).toHaveValue('message: 回收站 YAML 内容')
    await expect(textEditor.locator('textarea')).toHaveAttribute('readonly', '')
    await expect(textEditor.locator('.editor-workspace')).toHaveClass(/mode-edit/)
    await expect(textEditor.locator('.editor-tabs')).toHaveCount(0)
    await textEditor.getByRole('button', { name: '关闭编辑器' }).click()
    await expect(textEditor).toHaveCount(0)

    await markdownRow.click()
    const markdownEditor = page.locator('.document-editor')
    await expect(markdownEditor.locator('.editor-title small')).toHaveText('回收站只读预览')
    await expect(markdownEditor.locator('.editor-workspace')).toHaveClass(/mode-preview/)
    await expect(markdownEditor.locator('textarea')).toHaveCount(0)
    await expect(markdownEditor.locator('.markdown-preview h1')).toHaveText('回收站 Markdown')
    await markdownEditor.getByRole('button', { name: '关闭编辑器' }).click()
    await expect(markdownEditor).toHaveCount(0)
  } finally {
    await page.evaluate(async names => {
      const headers = { 'Content-Type': 'application/json' }
      const root = await fetch('/api/files/00000000-0000-0000-0000-000000000000/children')
      if (root.ok) {
        const children = await root.json() as { items?: Array<{ id: string; name: string }> }
        for (const item of children.items ?? []) {
          if (names.includes(item.name)) await fetch(`/api/files/${item.id}`, { method: 'DELETE', headers })
        }
      }
      const trash = await fetch('/api/trash')
      if (trash.ok) {
        const deleted = await trash.json() as { items?: Array<{ id: string; name: string }> }
        for (const item of deleted.items ?? []) {
          if (names.includes(item.name)) await fetch(`/api/trash/${item.id}`, { method: 'DELETE', headers })
        }
      }
    }, [textName, markdownName])
  }
})

test('文档保存在 reference 的目录刷新完成后才显示成功反馈', async ({ page }) => {
  const name = `editor-save-order-${Date.now().toString(36)}.md`
  let delayRefresh = false
  let refreshStarted = false

  await page.route('**/api/files/00000000-0000-0000-0000-000000000000/children', async route => {
    if (delayRefresh) {
      delayRefresh = false
      refreshStarted = true
      await new Promise(resolve => setTimeout(resolve, 1200))
    }
    await route.continue()
  })

  try {
    await login(page)
    await page.getByRole('button', { name: '新建文档', exact: true }).first().click()
    const editor = page.locator('.document-editor')
    await editor.getByRole('textbox', { name: '文档文件名' }).fill(name)
    await editor.locator('textarea').fill('保存时序')
    delayRefresh = true
    await editor.getByRole('button', { name: '保存' }).click()
    await expect.poll(() => refreshStarted).toBe(true)
    await expect(page.locator('.toast')).toHaveCount(0, { timeout: 100 })
    await expect(page.locator('.toast')).toContainText('文档已保存', { timeout: 2_000 })
  } finally {
    await removeByName(page, name)
  }
})

test('old/new 新建空文档保留未保存标记但关闭不触发放弃确认', async ({ browser }) => {
  const oldUrl = process.env.E2E_REFERENCE_URL || 'http://127.0.0.1:18080'
  const newUrl = process.env.E2E_NEW_URL || 'http://127.0.0.1:18084'
  const oldContext = await browser.newContext({ viewport: { width: 1440, height: 900 } })
  const newContext = await browser.newContext({ viewport: { width: 1440, height: 900 } })
  const oldPage = await oldContext.newPage()
  const newPage = await newContext.newPage()

  async function exercise(page: Parameters<typeof login>[0], baseUrl: string) {
    await loginAt(page, baseUrl)
    await page.getByRole('button', { name: '新建文档', exact: true }).first().click()
    const editor = page.locator('.document-editor')
    await expect(editor).toBeVisible()
    const initial = {
      unsaved: await editor.locator('.unsaved-dot').count(),
      saveDisabled: await editor.getByRole('button', { name: '保存', exact: true }).isDisabled(),
    }
    await editor.getByRole('button', { name: '关闭编辑器', exact: true }).click()
    return {
      initial,
      editor: await editor.count(),
      discard: await page.locator('.app-dialog').filter({ hasText: '放弃未保存的修改？' }).count(),
    }
  }

  try {
    const [oldResult, newResult] = await Promise.all([
      exercise(oldPage, oldUrl),
      exercise(newPage, newUrl),
    ])
    expect(oldResult).toEqual({ initial: { unsaved: 1, saveDisabled: false }, editor: 0, discard: 0 })
    expect(newResult, 'Rust 新建空文档的未保存标记/关闭语义与 reference 不一致').toEqual(oldResult)
  } finally {
    await Promise.all([oldContext.close(), newContext.close()])
  }
})

test('old/new 空目录空状态与窄屏创建菜单进入同一新文档 editor，关闭后不创建文件', async ({ browser }) => {
  const oldUrl = process.env.E2E_REFERENCE_URL || 'http://127.0.0.1:18080'
  const newUrl = process.env.E2E_NEW_URL || 'http://127.0.0.1:18084'
  const oldContext = await browser.newContext({ viewport: { width: 1440, height: 900 } })
  const newContext = await browser.newContext({ viewport: { width: 1440, height: 900 } })
  const oldPage = await oldContext.newPage()
  const newPage = await newContext.newPage()
  const root = '00000000-0000-0000-0000-000000000000'
  const suffix = Date.now().toString(36)
  let oldFolderId = ''
  let newFolderId = ''

  async function exercise(page: Parameters<typeof login>[0], baseUrl: string, folderName: string, rememberFolderId: (id: string) => void) {
    await loginAt(page, baseUrl)
    const created = await page.evaluate(async ({ name, parentId }) => {
      const response = await fetch('/api/directories', {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ parent_id: parentId, name }),
      })
      return { status: response.status, body: await response.json() as { id?: string } }
    }, { name: folderName, parentId: root })
    expect(created.status, '创建用于验证空状态入口的隔离目录').toBe(201)
    if (!created.body.id) throw new Error('空目录入口测试夹具缺少 id')
    rememberFolderId(created.body.id)

    await page.reload()
    const useListView = await useReferenceListView(page)
    await fileEntries(page, useListView).filter({ hasText: folderName }).click()
    await expect(page.locator('.folder-heading h1')).toHaveText(folderName)
    await expect(page.locator('.state.empty')).toBeVisible()

    const emptyState = page.locator('.empty-actions')
    await emptyState.getByRole('button', { name: '新建文档', exact: true }).click()
    const editor = page.locator('.document-editor')
    await expect(editor).toBeVisible()
    await expect(editor.getByRole('textbox', { name: '文档文件名' })).toHaveValue('未命名文档.md')
    await editor.getByRole('button', { name: '关闭编辑器', exact: true }).click()
    await expect(editor).toHaveCount(0)
    await expect(page.locator('.app-dialog').filter({ hasText: '放弃未保存的修改？' })).toHaveCount(0)
    await expect(page.locator('.state.empty')).toBeVisible()

    await page.setViewportSize({ width: 390, height: 844 })
    await expect(page.locator('.desktop-create-actions')).toBeHidden()
    const createMenu = page.locator('.create-menu')
    await createMenu.locator(':scope > summary').click()
    await expect.poll(() => createMenu.evaluate(element => (element as HTMLDetailsElement).open)).toBe(true)
    await createMenu.locator('.create-menu-popover').getByRole('button', { name: /新建文档/ }).click()
    await expect.poll(() => createMenu.evaluate(element => (element as HTMLDetailsElement).open)).toBe(false)
    await expect(editor).toBeVisible()
    await expect(editor.getByRole('textbox', { name: '文档文件名' })).toHaveValue('未命名文档.md')
    await editor.getByRole('button', { name: '关闭编辑器', exact: true }).click()
    await expect(editor).toHaveCount(0)
    await expect(page.locator('.app-dialog').filter({ hasText: '放弃未保存的修改？' })).toHaveCount(0)

    const children = await page.evaluate(async folderId => {
      const response = await fetch(`/api/files/${folderId}/children`)
      return response.ok ? (await response.json() as { items?: unknown[] }).items?.length ?? -1 : -1
    }, created.body.id)
    return { folderId: created.body.id, children }
  }

  async function cleanup(page: Parameters<typeof login>[0], folderId: string) {
    if (!folderId) return
    await page.evaluate(async id => {
      await fetch(`/api/files/${id}`, { method: 'DELETE' })
      await fetch(`/api/trash/${id}`, { method: 'DELETE' })
    }, folderId)
  }

  try {
    const [oldResult, newResult] = await Promise.all([
      exercise(oldPage, oldUrl, `editor-entry-old-${suffix}`, id => { oldFolderId = id }),
      exercise(newPage, newUrl, `editor-entry-new-${suffix}`, id => { newFolderId = id }),
    ])
    oldFolderId = oldResult.folderId
    newFolderId = newResult.folderId
    expect(oldResult.children, 'reference 的两个取消入口都不应创建文档').toBe(0)
    expect(newResult.children, 'Rust 空状态/窄屏菜单取消后不应留下文档').toBe(oldResult.children)
  } finally {
    await Promise.all([cleanup(oldPage, oldFolderId), cleanup(newPage, newFolderId)])
    await Promise.all([oldContext.close(), newContext.close()])
  }
})

test('old/new 新文档创建失败保留编辑器、错误和可重试保存', async ({ browser }) => {
  const oldUrl = process.env.E2E_REFERENCE_URL || 'http://127.0.0.1:18080'
  const newUrl = process.env.E2E_NEW_URL || 'http://127.0.0.1:18084'
  const oldContext = await browser.newContext({ viewport: { width: 1440, height: 900 } })
  const newContext = await browser.newContext({ viewport: { width: 1440, height: 900 } })
  const oldPage = await oldContext.newPage()
  const newPage = await newContext.newPage()

  async function exercise(page: Parameters<typeof login>[0], baseUrl: string) {
    await loginAt(page, baseUrl)
    await page.route('**/api/documents', route => route.fulfill({
      status: 409,
      contentType: 'application/json',
      body: JSON.stringify({ error: { status: 409, message: 'document already exists' } }),
    }))
    await page.getByRole('button', { name: '新建文档', exact: true }).first().click()
    const editor = page.locator('.document-editor')
    await editor.locator('textarea').fill('创建失败后仍可重试')
    await editor.getByRole('button', { name: '保存', exact: true }).click()
    await expect(editor.locator('.editor-header-message.error')).toHaveText('document already exists')
    return {
      editor: await editor.count(),
      error: await editor.locator('.editor-header-message.error').textContent(),
      unsaved: await editor.locator('.unsaved-dot').count(),
      saveEnabled: await editor.getByRole('button', { name: '保存', exact: true }).isEnabled(),
    }
  }

  try {
    const [oldResult, newResult] = await Promise.all([
      exercise(oldPage, oldUrl),
      exercise(newPage, newUrl),
    ])
    expect(oldResult).toEqual({ editor: 1, error: 'document already exists', unsaved: 1, saveEnabled: true })
    expect(newResult, 'Rust 新文档创建失败后的编辑器/错误/重试状态与 reference 不一致').toEqual(oldResult)
  } finally {
    await Promise.all([oldContext.close(), newContext.close()])
  }
})

test('old/new 全部旧版可编辑扩展名都从文件入口进入相同 editor', async ({ browser }) => {
  const suffix = crypto.randomUUID()
  const extensions = ['md', 'markdown', 'txt', 'yaml', 'yml', 'json', 'toml', 'ini', 'conf', 'log', 'csv']
  const documents = extensions.map(extension => ({
    name: `editor-extension-${suffix}.${extension}`,
    extension,
    content: `editor extension ${extension}`,
  }))
  const names = documents.map(document => document.name)
  const oldUrl = process.env.E2E_REFERENCE_URL || 'http://127.0.0.1:18080'
  const newUrl = process.env.E2E_NEW_URL || 'http://127.0.0.1:18084'
  const oldContext = await browser.newContext({ viewport: { width: 1440, height: 900 } })
  const newContext = await browser.newContext({ viewport: { width: 1440, height: 900 } })
  const oldPage = await oldContext.newPage()
  const newPage = await newContext.newPage()

  async function exercise(page: Parameters<typeof login>[0], baseUrl: string) {
    await loginAt(page, baseUrl)
    await Promise.all(documents.map(document => createDocument(page, document.name, document.content)))
    await page.reload()
    const useListView = await useReferenceListView(page)

    const result: Array<{ extension: string; label: string; tabs: boolean; saveDisabled: boolean }> = []
    for (const document of documents) {
      const row = fileEntries(page, useListView).filter({ hasText: document.name })
      await expect(row).toBeVisible({ timeout: 20_000 })
      await row.click()
      const editor = page.locator('.document-editor')
      await expect(editor).toBeVisible()
      await expect(editor.locator('textarea')).toHaveValue(document.content)
      await expect(editor.locator('.editor-title small')).toHaveText('文本编辑器')
      result.push({
        extension: document.extension,
        label: await editor.locator('.editor-title small').textContent() || '',
        tabs: await editor.locator('.editor-tabs').isVisible().catch(() => false),
        saveDisabled: await editor.getByRole('button', { name: '保存' }).isDisabled(),
      })
      await editor.getByRole('button', { name: '关闭编辑器' }).click()
      await expect(editor).toHaveCount(0)
    }
    return result
  }

  try {
    const [oldResult, newResult] = await Promise.all([
      exercise(oldPage, oldUrl),
      exercise(newPage, newUrl),
    ])
    expect(oldResult).toEqual(documents.map(document => ({
      extension: document.extension,
      label: '文本编辑器',
      tabs: ['md', 'markdown'].includes(document.extension),
      saveDisabled: true,
    })))
    expect(newResult, 'Rust 可编辑扩展名入口与 reference 不一致').toEqual(oldResult)
  } finally {
    await Promise.all([removeByNames(oldPage, names), removeByNames(newPage, names)])
    await Promise.all([oldContext.close(), newContext.close()])
  }
})

test('编辑器输入与原值相同仍保持 reference 的未修改状态', async ({ browser }) => {
  const name = `editor-same-value-${crypto.randomUUID()}.md`
  const content = '# unchanged\n\n输入同样的内容'
  const oldUrl = process.env.E2E_REFERENCE_URL || 'http://127.0.0.1:18080'
  const newUrl = process.env.E2E_NEW_URL || 'http://127.0.0.1:18084'
  const oldContext = await browser.newContext({ viewport: { width: 1440, height: 900 } })
  const newContext = await browser.newContext({ viewport: { width: 1440, height: 900 } })
  const oldPage = await oldContext.newPage()
  const newPage = await newContext.newPage()

  async function exercise(page: Parameters<typeof login>[0], baseUrl: string) {
    await loginAt(page, baseUrl)
    await createDocument(page, name, content)
    await page.reload()
    const useListView = await useReferenceListView(page)
    await fileEntries(page, useListView).filter({ hasText: name }).click()

    const editor = page.locator('.document-editor')
    await expect(editor.locator('textarea')).toHaveValue(content)
    const before = {
      unsaved: await editor.locator('.unsaved-dot').count(),
      saveDisabled: await editor.getByRole('button', { name: '保存' }).isDisabled(),
    }
    await editor.locator('textarea').fill(content)
    await page.waitForTimeout(50)
    return {
      before,
      after: {
        unsaved: await editor.locator('.unsaved-dot').count(),
        saveDisabled: await editor.getByRole('button', { name: '保存' }).isDisabled(),
      },
    }
  }

  try {
    const [oldResult, newResult] = await Promise.all([
      exercise(oldPage, oldUrl),
      exercise(newPage, newUrl),
    ])
    expect(oldResult).toEqual({
      before: { unsaved: 0, saveDisabled: true },
      after: { unsaved: 0, saveDisabled: true },
    })
    expect(newResult, 'Rust 编辑器相同值输入后的 dirty 状态与 reference 不一致').toEqual(oldResult)
  } finally {
    await Promise.all([removeByName(oldPage, name), removeByName(newPage, name)])
    await Promise.all([oldContext.close(), newContext.close()])
  }
})

test('old/new Markdown 编辑模式、分栏预览和安全渲染状态一致', async ({ browser }) => {
  const name = `editor-modes-${crypto.randomUUID()}.md`
  const content = '# Title\n\n#### Deep heading\n\n1. one\n2. two\n\n- [x] done\n- [ ] todo\n\n| a | b |\n| --- | :---: |\n| 1 | 2 |\n\n[link](https://example.com "T") and ![alt](cover.png)\n\n~~gone~~ and <u>under</u>\n\n<script>alert(1)</script>'
  const oldUrl = process.env.E2E_REFERENCE_URL || 'http://127.0.0.1:18080'
  const newUrl = process.env.E2E_NEW_URL || 'http://127.0.0.1:18084'
  const oldContext = await browser.newContext({ viewport: { width: 1440, height: 900 } })
  const newContext = await browser.newContext({ viewport: { width: 1440, height: 900 } })
  const oldPage = await oldContext.newPage()
  const newPage = await newContext.newPage()

  async function exercise(page: Parameters<typeof login>[0], baseUrl: string) {
    await loginAt(page, baseUrl)
    await createDocument(page, name, content)
    await page.reload()
    const useListView = await useReferenceListView(page)
    await fileEntries(page, useListView).filter({ hasText: name }).click()

    const editor = page.locator('.document-editor')
    await expect(editor.locator('textarea')).toHaveValue(content)
    await expect(editor.locator('.editor-meta b')).toHaveText(`${new TextEncoder().encode(content).length.toLocaleString()} 字节`)
    const initial = {
      workspace: await editor.locator('.editor-workspace').getAttribute('class'),
      textarea: await editor.locator('textarea').count(),
      preview: await editor.locator('.markdown-preview').count(),
      activeTab: await editor.locator('.editor-tabs button.active').textContent(),
    }

    await editor.getByRole('button', { name: '分栏' }).click()
    const split = {
      workspace: await editor.locator('.editor-workspace').getAttribute('class'),
      textarea: await editor.locator('textarea').count(),
      preview: await editor.locator('.markdown-preview').count(),
      activeTab: await editor.locator('.editor-tabs button.active').textContent(),
    }

    await editor.getByRole('button', { name: '预览' }).click()
    const preview = editor.locator('.markdown-preview')
    const previewState = {
      workspace: await editor.locator('.editor-workspace').getAttribute('class'),
      textarea: await editor.locator('textarea').count(),
      preview: await preview.count(),
      activeTab: await editor.locator('.editor-tabs button.active').textContent(),
      heading: await preview.locator('h1').textContent(),
      deepHeading: await preview.locator('h4').textContent(),
      orderedItems: await preview.locator('ol li').count(),
      checkboxes: await preview.locator('input[type=checkbox]').count(),
      centeredHeader: await preview.locator('table th[align=center]').textContent(),
      link: await preview.locator('a[href="https://example.com"][title="T"]').textContent(),
      image: await preview.locator('img[src="cover.png"][alt="alt"]').count(),
      deleted: await preview.locator('del').textContent(),
      underline: await preview.locator('u').textContent(),
      scripts: await preview.locator('script').count(),
      inlineHandlers: await preview.locator('[onclick]').count(),
    }
    return { initial, split, preview: previewState }
  }

  try {
    const [oldResult, newResult] = await Promise.all([
      exercise(oldPage, oldUrl),
      exercise(newPage, newUrl),
    ])
    expect(oldResult).toEqual({
      initial: { workspace: 'editor-workspace mode-edit markdown', textarea: 1, preview: 0, activeTab: '编辑' },
      split: { workspace: 'editor-workspace mode-split markdown', textarea: 1, preview: 1, activeTab: '分栏' },
      preview: {
        workspace: 'editor-workspace mode-preview markdown',
        textarea: 0,
        preview: 1,
        activeTab: '预览',
        heading: 'Title',
        deepHeading: 'Deep heading',
        orderedItems: 2,
        checkboxes: 2,
        centeredHeader: 'b',
        link: 'link',
        image: 1,
        deleted: 'gone',
        underline: 'under',
        scripts: 0,
        inlineHandlers: 0,
      },
    })
    expect(newResult, 'Rust Markdown 编辑器模式或预览渲染与 reference 不一致').toEqual(oldResult)
  } finally {
    await Promise.all([removeByName(oldPage, name), removeByName(newPage, name)])
    await Promise.all([oldContext.close(), newContext.close()])
  }
})

test('old/new Markdown edit/split 保留 textarea 光标滚动，preview 切换按旧版重建状态', async ({ browser }) => {
  const name = `editor-caret-scroll-${crypto.randomUUID()}.md`
  const content = Array.from({ length: 180 }, (_, index) =>
    `## Section ${index + 1}\n\nParagraph ${index + 1}: caret and scroll parity.`,
  ).join('\n\n')
  const oldUrl = process.env.E2E_REFERENCE_URL || 'http://127.0.0.1:18080'
  const newUrl = process.env.E2E_NEW_URL || 'http://127.0.0.1:18084'
  const oldContext = await browser.newContext({ viewport: { width: 1440, height: 900 } })
  const newContext = await browser.newContext({ viewport: { width: 1440, height: 900 } })
  const oldPage = await oldContext.newPage()
  const newPage = await newContext.newPage()

  async function exercise(page: Parameters<typeof login>[0], baseUrl: string) {
    await loginAt(page, baseUrl)
    await createDocument(page, name, content)
    await page.reload()
    const useListView = await useReferenceListView(page)
    await fileEntries(page, useListView).filter({ hasText: name }).click()
    const editor = page.locator('.document-editor')
    const textarea = editor.locator('textarea')
    await expect(textarea).toHaveValue(content)

    const editState = () => textarea.evaluate(element => {
      const field = element as HTMLTextAreaElement
      return {
        selectionStart: field.selectionStart,
        selectionEnd: field.selectionEnd,
        scrollTop: field.scrollTop,
        valueLength: field.value.length,
      }
    })
    await textarea.evaluate(element => {
      const field = element as HTMLTextAreaElement
      field.dataset.parityProbe = 'retained-edit-node'
      field.focus()
      field.setSelectionRange(525, 539)
      field.scrollTop = 640
    })
    const initial = await editState()

    await editor.getByRole('button', { name: '分栏', exact: true }).click()
    await expect(editor.locator('.editor-workspace')).toHaveClass(/mode-split/)
    const split = await editState()
    const sameTextareaInSplit = await textarea.getAttribute('data-parity-probe')

    await editor.getByRole('button', { name: '预览', exact: true }).click()
    await expect(editor.locator('.markdown-preview')).toBeVisible()
    await expect(textarea).toHaveCount(0)
    const preview = editor.locator('.markdown-preview')
    const previewScrollBeforeLeaving = await preview.evaluate(element => {
      element.scrollTop = 640
      return element.scrollTop
    })

    await editor.getByRole('button', { name: '编辑', exact: true }).click()
    await expect(textarea).toHaveValue(content)
    const afterPreviewEdit = {
      ...await editState(),
      retainedNodeMarker: await textarea.getAttribute('data-parity-probe'),
    }

    await editor.getByRole('button', { name: '预览', exact: true }).click()
    await expect(preview).toBeVisible()
    const previewScrollAfterReopen = await preview.evaluate(element => element.scrollTop)
    const saveDisabled = await editor.getByRole('button', { name: '保存', exact: true }).isDisabled()
    const dirtyCount = await editor.locator('.unsaved-dot').count()
    return {
      initial,
      split,
      sameTextareaInSplit,
      previewScrollBeforeLeaving,
      afterPreviewEdit,
      previewScrollAfterReopen,
      saveDisabled,
      dirtyCount,
    }
  }

  try {
    const [oldResult, newResult] = await Promise.all([
      exercise(oldPage, oldUrl),
      exercise(newPage, newUrl),
    ])
    expect(oldResult.initial.selectionStart).toBe(525)
    expect(oldResult.initial.selectionEnd).toBe(539)
    expect(oldResult.initial.scrollTop).toBe(640)
    expect(oldResult.split).toEqual(oldResult.initial)
    expect(oldResult.sameTextareaInSplit).toBe('retained-edit-node')
    expect(oldResult.previewScrollBeforeLeaving).toBe(640)
    expect(oldResult.afterPreviewEdit.retainedNodeMarker).toBeNull()
    expect(oldResult.saveDisabled).toBe(true)
    expect(oldResult.dirtyCount).toBe(0)
    expect(newResult, 'Rust Markdown textarea 光标/滚动生命周期与 reference 不一致').toEqual(oldResult)
  } finally {
    await Promise.all([removeByName(oldPage, name), removeByName(newPage, name)])
    await Promise.all([oldContext.close(), newContext.close()])
  }
})

test('old/new editor 保留加载态、未保存关闭确认、快捷保存和 ETag 冲突反馈', async ({ browser }) => {
  const name = `editor-conflict-${crypto.randomUUID()}.md`
  const content = '# conflict reference\n'
  const oldUrl = process.env.E2E_REFERENCE_URL || 'http://127.0.0.1:18080'
  const newUrl = process.env.E2E_NEW_URL || 'http://127.0.0.1:18084'
  const oldContext = await browser.newContext({ viewport: { width: 1440, height: 900 } })
  const newContext = await browser.newContext({ viewport: { width: 1440, height: 900 } })
  const oldPage = await oldContext.newPage()
  const newPage = await newContext.newPage()

  async function exercise(page: Parameters<typeof login>[0], baseUrl: string) {
    await loginAt(page, baseUrl)
    const id = await createDocument(page, name, content)
    await page.reload()
    const useListView = await useReferenceListView(page)
    await page.route(`**/api/files/${id}/content`, async route => {
      if (route.request().method() === 'GET') {
        await new Promise(resolve => setTimeout(resolve, 900))
        await route.continue()
        return
      }
      await route.fulfill({
        status: 409,
        contentType: 'application/json',
        body: JSON.stringify({
          error: {
            status: 409,
            code: 'document_conflict',
            message: 'document changed elsewhere; reopen it before saving',
          },
        }),
      })
    })

    await fileEntries(page, useListView).filter({ hasText: name }).click()
    const editor = page.locator('.document-editor')
    await expect(editor.locator('.editor-loading')).toBeVisible()
    await expect(editor.locator('textarea')).toHaveValue(content, { timeout: 10_000 })
    await editor.locator('textarea').fill(`${content}edited`)
    await expect(editor.getByRole('button', { name: '保存' })).toBeEnabled()

    await editor.getByRole('button', { name: '关闭编辑器' }).click()
    const discard = page.locator('.app-dialog').filter({ hasText: '放弃未保存的修改？' })
    await expect(discard).toBeVisible()
    await discard.getByRole('button', { name: '取消' }).focus()
    await page.keyboard.press('Escape')
    await expect(discard).toHaveCount(0)
    await expect(editor).toBeVisible()
    await editor.getByRole('button', { name: '关闭编辑器' }).click()
    await expect(discard).toBeVisible()
    await discard.getByRole('button', { name: '取消' }).click()
    await expect(editor).toBeVisible()

    await editor.locator('textarea').press('Control+s')
    const error = editor.locator('.editor-header-message.error')
    await expect(error).toHaveText('document changed elsewhere; reopen it before saving')
    await expect(editor.getByRole('button', { name: '保存' })).toBeEnabled()
    const result = {
      loading: await editor.locator('.editor-loading').count(),
      discard: await discard.count(),
      error: await error.textContent(),
      stillOpen: await editor.count(),
      saveEnabled: await editor.getByRole('button', { name: '保存' }).isEnabled(),
    }

    await editor.getByRole('button', { name: '关闭编辑器' }).click()
    await page.locator('.app-dialog').filter({ hasText: '放弃未保存的修改？' }).getByRole('button', { name: '放弃修改' }).click()
    await expect(editor).toHaveCount(0)
    return result
  }

  try {
    const [oldResult, newResult] = await Promise.all([
      exercise(oldPage, oldUrl),
      exercise(newPage, newUrl),
    ])
    expect(oldResult).toEqual({
      loading: 0,
      discard: 0,
      error: 'document changed elsewhere; reopen it before saving',
      stillOpen: 1,
      saveEnabled: true,
    })
    expect(newResult, 'Rust editor 加载/冲突/关闭行为与 reference 不一致').toEqual(oldResult)
  } finally {
    await Promise.all([removeByName(oldPage, name), removeByName(newPage, name)])
    await Promise.all([oldContext.close(), newContext.close()])
  }
})

test('old/new 文档 PUT 普通失败保留修改与 ETag，busy 锁定后可重试保存', async ({ browser }) => {
  const name = `editor-save-retry-${crypto.randomUUID()}.md`
  const originalContent = '# retry reference\n'
  const editedContent = `${originalContent}edited after a server error\n`
  const oldUrl = process.env.E2E_REFERENCE_URL || 'http://127.0.0.1:18080'
  const newUrl = process.env.E2E_NEW_URL || 'http://127.0.0.1:18084'
  const oldContext = await browser.newContext({ viewport: { width: 1440, height: 900 } })
  const newContext = await browser.newContext({ viewport: { width: 1440, height: 900 } })
  const oldPage = await oldContext.newPage()
  const newPage = await newContext.newPage()

  async function exercise(page: Parameters<typeof login>[0], baseUrl: string) {
    await loginAt(page, baseUrl)
    const id = await createDocument(page, name, originalContent)
    await page.reload()
    const useListView = await useReferenceListView(page)
    const bodies: Array<{ content: string; etag: string }> = []
    await page.route(`**/api/files/${id}/content`, async route => {
      if (route.request().method() === 'GET') {
        await route.continue()
        return
      }
      bodies.push(route.request().postDataJSON() as { content: string; etag: string })
      if (bodies.length === 1) {
        await new Promise(resolve => setTimeout(resolve, 500))
        await route.fulfill({
          status: 500,
          contentType: 'application/json',
          body: JSON.stringify({
            error: { status: 500, code: 'internal_error', message: 'disk write failed' },
          }),
        })
        return
      }
      await route.continue()
    })

    await fileEntries(page, useListView).filter({ hasText: name }).click()
    const editor = page.locator('.document-editor')
    const textarea = editor.locator('textarea')
    await expect(textarea).toHaveValue(originalContent)
    await textarea.fill(editedContent)
    const save = editor.locator('.editor-actions button.primary')
    await save.click()
    await expect(save).toHaveText('保存中…')
    await expect(save).toBeDisabled()
    const error = editor.locator('.editor-header-message.error')
    await expect(error).toHaveText('disk write failed')
    await expect(textarea).toHaveValue(editedContent)
    await expect(editor.locator('.unsaved-dot')).toHaveCount(1)
    await expect(save).toBeEnabled()
    const failure = {
      message: await error.textContent(),
      content: await textarea.inputValue(),
      unsaved: await editor.locator('.unsaved-dot').count(),
      saveEnabled: await save.isEnabled(),
    }

    await save.click()
    await expect(page.locator('.toast')).toHaveText('文档已保存')
    await expect(textarea).toHaveValue(editedContent)
    await expect(editor.locator('.unsaved-dot')).toHaveCount(0)
    await expect(save).toBeDisabled()
    const persisted = await page.evaluate(async fileId => {
      const response = await fetch(`/api/files/${fileId}/content`)
      return response.ok ? (await response.json() as { content?: string }).content : undefined
    }, id)

    return {
      failure,
      requestContents: bodies.map(body => body.content),
      sameEtagOnRetry: bodies.length === 2 && bodies[0].etag === bodies[1].etag,
      etagsPresent: bodies.length === 2 && bodies.every(body => Boolean(body.etag)),
      persisted,
      dirtyAfterSuccess: await editor.locator('.unsaved-dot').count(),
      saveDisabledAfterSuccess: await save.isDisabled(),
    }
  }

  try {
    const [oldResult, newResult] = await Promise.all([
      exercise(oldPage, oldUrl),
      exercise(newPage, newUrl),
    ])
    expect(oldResult).toEqual({
      failure: {
        message: 'disk write failed',
        content: editedContent,
        unsaved: 1,
        saveEnabled: true,
      },
      requestContents: [editedContent, editedContent],
      sameEtagOnRetry: true,
      etagsPresent: true,
      persisted: editedContent,
      dirtyAfterSuccess: 0,
      saveDisabledAfterSuccess: true,
    })
    expect(newResult, 'Rust PUT 普通失败/重试后的 editor 状态与 reference 不一致').toEqual(oldResult)
  } finally {
    await Promise.all([removeByName(oldPage, name), removeByName(newPage, name)])
    await Promise.all([oldContext.close(), newContext.close()])
  }
})

test('old/new 文档保存后列表大小/本地时间 metadata 随目录刷新更新', async ({ browser }) => {
  const name = `editor-list-metadata-${crypto.randomUUID()}.md`
  const originalContent = '# before metadata\n'
  const editedContent = 'x'.repeat(4097)
  const oldUrl = process.env.E2E_REFERENCE_URL || 'http://127.0.0.1:18080'
  const newUrl = process.env.E2E_NEW_URL || 'http://127.0.0.1:18084'
  const oldContext = await browser.newContext({ viewport: { width: 1440, height: 900 } })
  const newContext = await browser.newContext({ viewport: { width: 1440, height: 900 } })
  const oldPage = await oldContext.newPage()
  const newPage = await newContext.newPage()

  async function exercise(page: Parameters<typeof login>[0], baseUrl: string) {
    await loginAt(page, baseUrl)
    const id = await createDocument(page, name, originalContent)
    await page.reload()
    const useListView = await useReferenceListView(page)
    const row = fileEntries(page, useListView).filter({ hasText: name })
    await expect(row).toBeVisible()
    const initialMeta = await row.locator('small').first().innerText()

    await row.click()
    const editor = page.locator('.document-editor')
    await expect(editor.locator('textarea')).toHaveValue(originalContent)
    await editor.locator('textarea').fill(editedContent)
    await editor.getByRole('button', { name: '保存', exact: true }).click()
    await expect(page.locator('.toast')).toHaveText('文档已保存')
    await editor.getByRole('button', { name: '关闭编辑器', exact: true }).click()
    await expect(editor).toHaveCount(0)
    await expect(row.locator('small').first()).toContainText(' · ')
    const updatedMeta = await row.locator('small').first().innerText()
    const server = await page.evaluate(async ({ fileId, rootId }) => {
      const listing = await fetch(`/api/files/${rootId}/children`)
      if (!listing.ok) throw new Error(`children request failed: ${listing.status}`)
      const children = await listing.json() as { items?: Array<{ id: string; size: number; updated_at: string }> }
      const file = children.items?.find(item => item.id === fileId)
      if (!file) throw new Error('saved document missing from children')
      const contentResponse = await fetch(`/api/files/${fileId}/content`)
      if (!contentResponse.ok) throw new Error(`content request failed: ${contentResponse.status}`)
      const body = await contentResponse.json() as { content: string }
      const formattedDate = new Intl.DateTimeFormat('zh-CN', {
        month: 'short', day: 'numeric', hour: '2-digit', minute: '2-digit',
      }).format(new Date(file.updated_at))
      return { size: file.size, date: formattedDate, content: body.content }
    }, { fileId: id, rootId: '00000000-0000-0000-0000-000000000000' })
    const initialParts = initialMeta.split(' · ')
    const updatedParts = updatedMeta.split(' · ')
    return {
      initialSize: initialParts[0],
      initialHasDate: initialParts.length === 2,
      updatedSize: updatedParts[0],
      updatedHasDate: updatedParts.length === 2,
      updatedDateMatchesServer: updatedParts[1] === server.date,
      serverSize: server.size,
      savedContent: server.content === editedContent,
    }
  }

  try {
    const [oldResult, newResult] = await Promise.all([
      exercise(oldPage, oldUrl),
      exercise(newPage, newUrl),
    ])
    expect(oldResult.initialHasDate).toBe(true)
    expect(oldResult.initialSize).not.toBe(oldResult.updatedSize)
    expect(oldResult.updatedHasDate).toBe(true)
    expect(oldResult.updatedDateMatchesServer).toBe(true)
    expect(oldResult.serverSize).toBe(editedContent.length)
    expect(oldResult.savedContent).toBe(true)
    expect(newResult, 'Rust 文档保存后的列表 metadata/目录刷新与 reference 不一致').toEqual(oldResult)
  } finally {
    await Promise.all([removeByName(oldPage, name), removeByName(newPage, name)])
    await Promise.all([oldContext.close(), newContext.close()])
  }
})

test('old/new 文档内容 GET 首次失败后退出加载态，关闭重开可重新读取', async ({ browser }) => {
  const name = `editor-read-retry-${crypto.randomUUID()}.md`
  const content = '# fetched after retry\n'
  const oldUrl = process.env.E2E_REFERENCE_URL || 'http://127.0.0.1:18080'
  const newUrl = process.env.E2E_NEW_URL || 'http://127.0.0.1:18084'
  const oldContext = await browser.newContext({ viewport: { width: 1440, height: 900 } })
  const newContext = await browser.newContext({ viewport: { width: 1440, height: 900 } })
  const oldPage = await oldContext.newPage()
  const newPage = await newContext.newPage()

  async function exercise(page: Parameters<typeof login>[0], baseUrl: string) {
    await loginAt(page, baseUrl)
    const id = await createDocument(page, name, content)
    await page.reload()
    const useListView = await useReferenceListView(page)
    let reads = 0
    await page.route(`**/api/files/${id}/content`, async route => {
      if (route.request().method() !== 'GET') {
        await route.continue()
        return
      }
      reads += 1
      if (reads === 1) {
        await new Promise(resolve => setTimeout(resolve, 500))
        await route.fulfill({
          status: 500,
          contentType: 'application/json',
          body: JSON.stringify({
            error: { status: 500, code: 'internal_error', message: 'content temporarily unavailable' },
          }),
        })
        return
      }
      await route.continue()
    })

    const row = fileEntries(page, useListView).filter({ hasText: name })
    await row.click()
    const editor = page.locator('.document-editor')
    await expect(editor.locator('.editor-loading')).toBeVisible()
    const error = editor.locator('.editor-header-message.error')
    await expect(error).toHaveText('content temporarily unavailable')
    await expect(editor.locator('.editor-loading')).toHaveCount(0)
    await expect(editor.locator('textarea')).toHaveValue('')
    await expect(editor.locator('.unsaved-dot')).toHaveCount(0)
    await expect(editor.getByRole('button', { name: '保存', exact: true })).toBeDisabled()
    const failedRead = {
      error: await error.textContent(),
      loader: await editor.locator('.editor-loading').count(),
      content: await editor.locator('textarea').inputValue(),
      unsaved: await editor.locator('.unsaved-dot').count(),
      saveDisabled: await editor.getByRole('button', { name: '保存', exact: true }).isDisabled(),
    }

    await editor.getByRole('button', { name: '关闭编辑器', exact: true }).click()
    await expect(editor).toHaveCount(0)
    await row.click()
    await expect(editor.locator('textarea')).toHaveValue(content)
    await expect(error).toHaveCount(0)
    await expect(editor.getByRole('button', { name: '保存', exact: true })).toBeDisabled()
    return {
      failedRead,
      recoveredContent: await editor.locator('textarea').inputValue(),
      reads,
    }
  }

  try {
    const [oldResult, newResult] = await Promise.all([
      exercise(oldPage, oldUrl),
      exercise(newPage, newUrl),
    ])
    expect(oldResult).toEqual({
      failedRead: {
        error: 'content temporarily unavailable',
        loader: 0,
        content: '',
        unsaved: 0,
        saveDisabled: true,
      },
      recoveredContent: content,
      reads: 2,
    })
    expect(newResult, 'Rust 文档 GET 错误/关闭重试行为与 reference 不一致').toEqual(oldResult)
  } finally {
    await Promise.all([removeByName(oldPage, name), removeByName(newPage, name)])
    await Promise.all([oldContext.close(), newContext.close()])
  }
})

test('过期会话读取文档时保留旧版实测差异并由 Rust 清理敏感 editor 状态', async ({ browser }) => {
  const name = `editor-expired-session-${crypto.randomUUID()}.md`
  const oldUrl = process.env.E2E_REFERENCE_URL || 'http://127.0.0.1:18080'
  const newUrl = process.env.E2E_NEW_URL || 'http://127.0.0.1:18084'
  const oldContext = await browser.newContext({ viewport: { width: 1440, height: 900 } })
  const newContext = await browser.newContext({ viewport: { width: 1440, height: 900 } })
  const oldPage = await oldContext.newPage()
  const newPage = await newContext.newPage()

  async function exercise(page: Parameters<typeof login>[0], baseUrl: string, isReference: boolean) {
    await loginAt(page, baseUrl)
    const id = await createDocument(page, name, 'sensitive content behind an expired session')
    await page.reload()
    const useListView = await useReferenceListView(page)
    await page.route(`**/api/files/${id}/content`, route => route.fulfill({
      status: 401,
      contentType: 'application/json',
      body: JSON.stringify({
        error: { status: 401, code: 'unauthorized', message: 'session expired' },
      }),
    }))
    await fileEntries(page, useListView).filter({ hasText: name }).click()
    const editor = page.locator('.document-editor')
    if (isReference) {
      await expect(editor.locator('.editor-header-message.error')).toHaveText('session expired')
      return {
        editor: await editor.count(),
        error: await editor.locator('.editor-header-message.error').textContent(),
        authenticatedShell: await page.locator('.app-shell').count(),
        loginButton: await page.getByRole('button', { name: '进入我的网盘', exact: true }).count(),
      }
    }

    await expect(page.getByRole('button', { name: '进入我的网盘', exact: true })).toBeVisible()
    return {
      editor: await editor.count(),
      error: null,
      authenticatedShell: await page.locator('.app-shell').count(),
      loginButton: await page.getByRole('button', { name: '进入我的网盘', exact: true }).count(),
    }
  }

  async function cleanup(baseUrl: string) {
    const context = await browser.newContext()
    const page = await context.newPage()
    try {
      await loginAt(page, baseUrl)
      await removeByName(page, name)
    } finally {
      await context.close()
    }
  }

  try {
    const [oldResult, newResult] = await Promise.all([
      exercise(oldPage, oldUrl, true),
      exercise(newPage, newUrl, false),
    ])
    expect(oldResult).toEqual({
      editor: 1,
      error: 'session expired',
      authenticatedShell: 1,
      loginButton: 0,
    })
    expect(newResult, 'Rust should clear the editor and return to login after a 401').toEqual({
      editor: 0,
      error: null,
      authenticatedShell: 0,
      loginButton: 1,
    })
  } finally {
    await Promise.all([oldContext.close(), newContext.close()])
    await Promise.all([cleanup(oldUrl), cleanup(newUrl)])
  }
})

test('old/new 回收站只读 editor 忽略 Escape，遮罩与关闭按钮均不触发放弃确认', async ({ browser }) => {
  const name = `editor-readonly-close-${crypto.randomUUID()}.yaml`
  const content = 'message: trash preview\n'
  const oldUrl = process.env.E2E_REFERENCE_URL || 'http://127.0.0.1:18080'
  const newUrl = process.env.E2E_NEW_URL || 'http://127.0.0.1:18084'
  const oldContext = await browser.newContext({ viewport: { width: 1440, height: 900 } })
  const newContext = await browser.newContext({ viewport: { width: 1440, height: 900 } })
  const oldPage = await oldContext.newPage()
  const newPage = await newContext.newPage()

  async function exercise(page: Parameters<typeof login>[0], baseUrl: string) {
    await loginAt(page, baseUrl)
    const useListView = await useReferenceListView(page)
    const id = await createDocument(page, name, content)
    const removed = await page.evaluate(async fileId => {
      const response = await fetch(`/api/files/${fileId}`, { method: 'DELETE' })
      return response.ok
    }, id)
    expect(removed, '创建回收站只读 editor fixture').toBe(true)
    await page.locator('.topbar .trash-button').click()
    const row = fileEntries(page, useListView).filter({ hasText: name })
    await expect(row).toBeVisible()
    await row.click()

    const editor = page.locator('.document-editor')
    await expect(editor.locator('.editor-title small')).toHaveText('回收站只读预览')
    await expect(editor.locator('textarea')).toHaveAttribute('readonly', '')
    await expect(editor.locator('.editor-tabs')).toHaveCount(0)
    await expect(editor.getByRole('button', { name: '保存', exact: true })).toHaveCount(0)

    await page.keyboard.press('Escape')
    await expect(editor).toBeVisible()
    await expect(page.locator('.app-dialog')).toHaveCount(0)
    await page.locator('.modal-backdrop.editing').click({ position: { x: 8, y: 8 } })
    await expect(editor).toHaveCount(0)
    await expect(page.locator('.app-dialog')).toHaveCount(0)

    await row.click()
    await expect(editor).toBeVisible()
    await editor.getByRole('button', { name: '关闭编辑器', exact: true }).click()
    await expect(editor).toHaveCount(0)
    await expect(page.locator('.app-dialog')).toHaveCount(0)
    return {
      readonly: await editor.count(),
      discarded: await page.locator('.app-dialog').count(),
      trashRow: await row.count(),
    }
  }

  try {
    const [oldResult, newResult] = await Promise.all([
      exercise(oldPage, oldUrl),
      exercise(newPage, newUrl),
    ])
    expect(oldResult).toEqual({ readonly: 0, discarded: 0, trashRow: 1 })
    expect(newResult, 'Rust 回收站只读 editor 关闭/键盘语义与 reference 不一致').toEqual(oldResult)
  } finally {
    await Promise.all([removeByName(oldPage, name), removeByName(newPage, name)])
    await Promise.all([oldContext.close(), newContext.close()])
  }
})

test('放弃未保存编辑不会清除已有的 reference 全局 toast', async ({ browser }) => {
  const folderName = `editor-toast-${crypto.randomUUID()}`
  const oldUrl = process.env.E2E_REFERENCE_URL || 'http://127.0.0.1:18080'
  const newUrl = process.env.E2E_NEW_URL || 'http://127.0.0.1:18084'
  const oldContext = await browser.newContext({ viewport: { width: 1440, height: 900 } })
  const newContext = await browser.newContext({ viewport: { width: 1440, height: 900 } })
  const oldPage = await oldContext.newPage()
  const newPage = await newContext.newPage()

  async function exercise(page: Parameters<typeof login>[0], baseUrl: string) {
    await loginAt(page, baseUrl)
    await page.getByRole('button', { name: '新建文件夹', exact: true }).click()
    const folderDialog = page.locator('.app-dialog')
    await folderDialog.locator('input').fill(folderName)
    await folderDialog.getByRole('button', { name: '创建', exact: true }).click()
    await expect(page.locator('.toast')).toHaveText('文件夹已创建')

    await page.getByRole('button', { name: '新建文档', exact: true }).first().click()
    const editor = page.locator('.document-editor')
    await expect(editor).toBeVisible()
    await editor.locator('textarea').fill('toast must survive discard')
    await editor.getByRole('button', { name: '关闭编辑器' }).click()
    const discard = page.locator('.app-dialog').filter({ hasText: '放弃未保存的修改？' })
    await expect(discard).toBeVisible()
    await discard.getByRole('button', { name: '放弃修改' }).click()
    await expect(editor).toHaveCount(0)
    return {
      toast: await page.locator('.toast').innerText(),
      className: await page.locator('.toast').getAttribute('class'),
    }
  }

  try {
    const [oldResult, newResult] = await Promise.all([
      exercise(oldPage, oldUrl),
      exercise(newPage, newUrl),
    ])
    expect(newResult, 'Rust 放弃编辑时不应清除 reference 的全局 toast').toEqual(oldResult)
    expect(oldResult).toEqual({ toast: '文件夹已创建', className: 'toast success' })
  } finally {
    await Promise.all([removeByName(oldPage, folderName), removeByName(newPage, folderName)])
    await Promise.all([oldContext.close(), newContext.close()])
  }
})

test('old/new Markdown 边界语义保持 reference 的 DOM 结构和安全清理', async ({ browser }) => {
  const name = `editor-markdown-boundary-${crypto.randomUUID()}.md`
  const content = [
    '# Boundary',
    '',
    '第一行  ',
    '硬换行',
    '',
    '> 引用第一行',
    '> 引用第二行',
    '',
    '- 父项',
    '  - 子项',
    '    1. 嵌套编号',
    '',
    '- [x] 已完成',
    '- [ ] 待完成',
    '',
    '```javascript',
    'const value = 1 < 2',
    '```',
    '',
    '<https://example.com>',
    '',
    '<div onclick="alert(1)">安全文本</div>',
    '',
    '[危险链接](javascript:alert(2))',
    '',
    '<script>alert(3)</script>',
  ].join('\n')
  const oldUrl = process.env.E2E_REFERENCE_URL || 'http://127.0.0.1:18080'
  const newUrl = process.env.E2E_NEW_URL || 'http://127.0.0.1:18084'
  const oldContext = await browser.newContext({ viewport: { width: 1440, height: 900 } })
  const newContext = await browser.newContext({ viewport: { width: 1440, height: 900 } })
  const oldPage = await oldContext.newPage()
  const newPage = await newContext.newPage()

  async function exercise(page: Parameters<typeof login>[0], baseUrl: string) {
    await loginAt(page, baseUrl)
    await createDocument(page, name, content)
    await page.reload()
    const useListView = await useReferenceListView(page)
    await fileEntries(page, useListView).filter({ hasText: name }).click()
    const editor = page.locator('.document-editor')
    await expect(editor.locator('textarea')).toHaveValue(content)
    await editor.getByRole('button', { name: '预览' }).click()
    const preview = editor.locator('.markdown-preview')
    await expect(preview).toBeVisible()
    return preview.evaluate(element => {
      const directText = (node: Element) => Array.from(node.childNodes)
        .filter(child => child.nodeType === Node.TEXT_NODE)
        .map(child => child.textContent ?? '')
        .join('')
        .replace(/\s+/g, ' ')
        .trim()
      const attributes = (node: Element) => Object.fromEntries(
        Array.from(node.attributes)
          .sort((left, right) => left.name.localeCompare(right.name))
          .map(attribute => [attribute.name, attribute.value]),
      )
      return Array.from(element.querySelectorAll('*')).map(node => ({
        tag: node.tagName.toLowerCase(),
        attributes: attributes(node),
        directText: directText(node),
        text: (node.textContent ?? '').replace(/\s+/g, ' ').trim(),
      }))
    })
  }

  try {
    const [oldResult, newResult] = await Promise.all([
      exercise(oldPage, oldUrl),
      exercise(newPage, newUrl),
    ])
    expect(newResult, 'Rust Markdown 边界 DOM 结构或安全清理与 reference 不一致').toEqual(oldResult)
  } finally {
    await Promise.all([removeByName(oldPage, name), removeByName(newPage, name)])
    await Promise.all([oldContext.close(), newContext.close()])
  }
})

const DIRTY_ROOT = '00000000-0000-0000-0000-000000000000'
const DIRTY_STAMP = '2026-01-01T00:00:00Z'
const DIRTY_FILE = {
  id: 'editor-dirty-reference',
  parent_id: DIRTY_ROOT,
  name: 'dirty-reference.md',
  kind: 'file',
  size: 32,
  status: 'ready',
  created_at: DIRTY_STAMP,
  updated_at: DIRTY_STAMP,
  mime_type: 'text/markdown',
  etag: 'dirty-reference-etag',
}

async function mockDirtyEditor(
  page: Parameters<typeof login>[0],
  includeUpdatedAt = true,
  etag: string | null | undefined = DIRTY_FILE.etag,
  updatedAt: string | null | undefined = includeUpdatedAt ? DIRTY_STAMP : undefined,
) {
  await page.route('**/api/**', async route => {
    const path = new URL(route.request().url()).pathname
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
    if (path === `/api/files/${DIRTY_ROOT}`) {
      return json({ file: { ...DIRTY_FILE, id: DIRTY_ROOT, name: '我的文件', kind: 'directory', parent_id: null, size: 0, mime_type: '' }, breadcrumbs: [] })
    }
    if (path === `/api/files/${DIRTY_ROOT}/children`) {
      return json({ items: [DIRTY_FILE], total_bytes: DIRTY_FILE.size, file_count: 1 })
    }
    if (path === `/api/files/${DIRTY_FILE.id}/content`) {
      return json({
        content: '# 原始内容\n',
        ...(etag === undefined ? {} : { etag }),
        ...(updatedAt === undefined ? {} : { updated_at: updatedAt }),
      })
    }
    return json({ items: [] })
  })
}

async function openDirtyEditor(page: Parameters<typeof login>[0], baseUrl: string) {
  await page.goto(`${baseUrl}/?editor-dirty-reference=${crypto.randomUUID()}`)
  await expect(page.getByRole('heading', { name: '我的文件', exact: true })).toBeVisible()
  await page.locator('.file-card').filter({ hasText: DIRTY_FILE.name }).click()
  const editor = page.locator('.document-editor')
  await expect(editor.locator('textarea')).toHaveValue('# 原始内容\n')
  await editor.locator('textarea').fill('# 修改后的内容\n')
  await expect(editor.locator('.unsaved-dot')).toHaveText('未保存')
  return editor
}

test('old/new 文档读取成功响应缺少 updated_at 时仍按 content 和 ETag 打开编辑器', async ({ browser }) => {
  const oldUrl = process.env.E2E_REFERENCE_URL || 'http://127.0.0.1:18080'
  const newUrl = process.env.E2E_NEW_URL || 'http://127.0.0.1:18084'
  const oldContext = await browser.newContext({ viewport: { width: 1440, height: 900 } })
  const newContext = await browser.newContext({ viewport: { width: 1440, height: 900 } })
  const oldPage = await oldContext.newPage()
  const newPage = await newContext.newPage()

  async function exercise(page: Parameters<typeof login>[0], baseUrl: string) {
    await mockDirtyEditor(page, false)
    await page.goto(`${baseUrl}/?editor-content-shape=${crypto.randomUUID()}`)
    await expect(page.getByRole('heading', { name: '我的文件', exact: true })).toBeVisible()
    await page.locator('.file-card').filter({ hasText: DIRTY_FILE.name }).click()
    const editor = page.locator('.document-editor')
    await expect(editor).toBeVisible()
    await page.waitForTimeout(300)
    const error = editor.locator('.editor-header-message.error')
    return {
      content: await editor.locator('textarea').inputValue(),
      error: (await error.count()) > 0 ? await error.textContent() : null,
      busy: await editor.locator('.editor-loading').count(),
    }
  }

  try {
    const [oldResult, newResult] = await Promise.all([
      exercise(oldPage, oldUrl),
      exercise(newPage, newUrl),
    ])
    expect(oldResult).toEqual({ content: '# 原始内容\n', error: null, busy: 0 })
    expect(newResult, 'Rust 文档读取缺少 updated_at 时与 reference 不一致').toEqual(oldResult)
  } finally {
    await Promise.all([oldContext.close(), newContext.close()])
  }
})

test('old/new 文档读取成功响应的 ETag 为 null 时仍按空 ETag 打开编辑器', async ({ browser }) => {
  const oldUrl = process.env.E2E_REFERENCE_URL || 'http://127.0.0.1:18080'
  const newUrl = process.env.E2E_NEW_URL || 'http://127.0.0.1:18084'
  const oldContext = await browser.newContext({ viewport: { width: 1440, height: 900 } })
  const newContext = await browser.newContext({ viewport: { width: 1440, height: 900 } })
  const oldPage = await oldContext.newPage()
  const newPage = await newContext.newPage()

  async function exercise(page: Parameters<typeof login>[0], baseUrl: string) {
    await mockDirtyEditor(page, true, null)
    await page.goto(`${baseUrl}/?editor-null-etag=${crypto.randomUUID()}`)
    await expect(page.getByRole('heading', { name: '我的文件', exact: true })).toBeVisible()
    await page.locator('.file-card').filter({ hasText: DIRTY_FILE.name }).click()
    const editor = page.locator('.document-editor')
    await expect(editor).toBeVisible()
    await expect(editor.locator('textarea')).toHaveValue('# 原始内容\n')
    await page.waitForTimeout(300)
    const error = editor.locator('.editor-header-message.error')
    return {
      content: await editor.locator('textarea').inputValue(),
      error: (await error.count()) > 0 ? await error.textContent() : null,
      busy: await editor.locator('.editor-loading').count(),
    }
  }

  try {
    const [oldResult, newResult] = await Promise.all([
      exercise(oldPage, oldUrl),
      exercise(newPage, newUrl),
    ])
    expect(oldResult).toEqual({ content: '# 原始内容\n', error: null, busy: 0 })
    expect(newResult, 'Rust 文档读取 null ETag 时与 reference 不一致').toEqual(oldResult)
  } finally {
    await Promise.all([oldContext.close(), newContext.close()])
  }
})

test('old/new 文档读取成功响应的 updated_at 为 null 时仍按 content 和 ETag 打开编辑器', async ({ browser }) => {
  const oldUrl = process.env.E2E_REFERENCE_URL || 'http://127.0.0.1:18080'
  const newUrl = process.env.E2E_NEW_URL || 'http://127.0.0.1:18084'
  const oldContext = await browser.newContext({ viewport: { width: 1440, height: 900 } })
  const newContext = await browser.newContext({ viewport: { width: 1440, height: 900 } })
  const oldPage = await oldContext.newPage()
  const newPage = await newContext.newPage()

  async function exercise(page: Parameters<typeof login>[0], baseUrl: string) {
    await mockDirtyEditor(page, true, DIRTY_FILE.etag, null)
    await page.goto(`${baseUrl}/?editor-null-updated-at=${crypto.randomUUID()}`)
    await expect(page.getByRole('heading', { name: '我的文件', exact: true })).toBeVisible()
    await page.locator('.file-card').filter({ hasText: DIRTY_FILE.name }).click()
    const editor = page.locator('.document-editor')
    await expect(editor).toBeVisible()
    await expect(editor.locator('textarea')).toHaveValue('# 原始内容\n')
    await page.waitForTimeout(300)
    const error = editor.locator('.editor-header-message.error')
    return {
      content: await editor.locator('textarea').inputValue(),
      error: (await error.count()) > 0 ? await error.textContent() : null,
      busy: await editor.locator('.editor-loading').count(),
    }
  }

  try {
    const [oldResult, newResult] = await Promise.all([
      exercise(oldPage, oldUrl),
      exercise(newPage, newUrl),
    ])
    expect(oldResult).toEqual({ content: '# 原始内容\n', error: null, busy: 0 })
    expect(newResult, 'Rust 文档读取 null updated_at 时与 reference 不一致').toEqual(oldResult)
  } finally {
    await Promise.all([oldContext.close(), newContext.close()])
  }
})

test('dirty 编辑器的遮罩关闭与浏览器后退保持 reference 语义', async ({ browser }) => {
  const oldUrl = process.env.E2E_REFERENCE_URL || 'http://127.0.0.1:18080'
  const newUrl = process.env.E2E_NEW_URL || 'http://127.0.0.1:18084'
  const oldContext = await browser.newContext({ viewport: { width: 1440, height: 900 } })
  const newContext = await browser.newContext({ viewport: { width: 1440, height: 900 } })
  const oldPage = await oldContext.newPage()
  const newPage = await newContext.newPage()

  async function exercise(page: Parameters<typeof login>[0], baseUrl: string) {
    await mockDirtyEditor(page)
    let editor = await openDirtyEditor(page, baseUrl)
    await page.keyboard.press('Escape')
    await expect(editor).toBeVisible()
    await expect(page.locator('.app-dialog')).toHaveCount(0)
    await editor.getByRole('button', { name: '关闭编辑器' }).click()
    const discard = page.locator('.app-dialog').filter({ hasText: '放弃未保存的修改？' })
    await expect(discard).toBeVisible()
    await discard.getByRole('button', { name: '取消', exact: true }).click()
    await expect(editor).toBeVisible()

    await page.locator('.modal-backdrop.editing').click({ position: { x: 8, y: 8 } })
    await expect(discard).toBeVisible()
    await discard.getByRole('button', { name: '取消', exact: true }).click()
    await expect(editor).toBeVisible()

    await page.goBack()
    await expect(editor).toHaveCount(0)
    await expect(page.locator('.app-dialog')).toHaveCount(0)
    return { path: new URL(page.url()).pathname, editor: await editor.count(), discard: await discard.count() }
  }

  try {
    const [oldResult, newResult] = await Promise.all([
      exercise(oldPage, oldUrl),
      exercise(newPage, newUrl),
    ])
    expect(oldResult).toEqual({ path: '/', editor: 0, discard: 0 })
    expect(newResult, 'Rust dirty 编辑器关闭/后退与 reference 不一致').toEqual(oldResult)
  } finally {
    await Promise.all([oldContext.close(), newContext.close()])
  }
})

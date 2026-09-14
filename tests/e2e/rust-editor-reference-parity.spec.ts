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
    await page.getByRole('button', { name: '列表' }).click()
    await expect(page.locator('.file-row').filter({ hasText: textName })).toBeVisible()
    await expect(page.locator('.file-row').filter({ hasText: markdownName })).toBeVisible()

    for (const name of [textName, markdownName]) {
      const row = page.locator('.file-row').filter({ hasText: name })
      await row.getByRole('button', { name: '选择项目' }).click()
    }
    await page.getByRole('toolbar', { name: '所选项目操作' }).getByRole('button', { name: '删除' }).click()
    const confirm = page.getByRole('dialog').filter({ hasText: '移入回收站？' })
    await confirm.getByRole('button', { name: '移入回收站' }).click()
    await expect(page.locator('.file-row').filter({ hasText: textName })).toHaveCount(0)

    await page.locator('.trash-entry').click()
    const textRow = page.locator('.file-row').filter({ hasText: textName })
    const markdownRow = page.locator('.file-row').filter({ hasText: markdownName })
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
    await page.getByRole('button', { name: '列表', exact: true }).click()

    const result: Array<{ extension: string; label: string; tabs: boolean; saveDisabled: boolean }> = []
    for (const document of documents) {
      const row = page.locator('.file-row').filter({ hasText: document.name })
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
    await page.getByRole('button', { name: '列表', exact: true }).click()
    await page.locator('.file-row').filter({ hasText: name }).click()

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
    await page.getByRole('button', { name: '列表', exact: true }).click()
    await page.locator('.file-row').filter({ hasText: name }).click()

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
    await page.getByRole('button', { name: '列表', exact: true }).click()
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

    await page.locator('.file-row').filter({ hasText: name }).click()
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

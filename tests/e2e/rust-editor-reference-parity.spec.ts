import { expect, test } from '@playwright/test'
import { login } from './helpers'

async function removeByName(page: Parameters<typeof login>[0], name: string) {
  await page.evaluate(async wanted => {
    const headers = { 'Content-Type': 'application/json' }
    const root = await fetch('/api/files/00000000-0000-0000-0000-000000000000/children')
    if (!root.ok) return
    const children = await root.json() as { items?: Array<{ id: string; name: string }> }
    for (const item of children.items ?? []) {
      if (item.name === wanted) await fetch(`/api/files/${item.id}`, { method: 'DELETE', headers })
    }
    const trash = await fetch('/api/trash')
    if (!trash.ok) return
    const deleted = await trash.json() as { items?: Array<{ id: string; name: string }> }
    for (const item of deleted.items ?? []) {
      if (item.name === wanted) await fetch(`/api/trash/${item.id}`, { method: 'DELETE', headers })
    }
  }, name)
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

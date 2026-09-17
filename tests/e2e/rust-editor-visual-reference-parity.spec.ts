import { expect, test, type Page } from '@playwright/test'

const ROOT = '00000000-0000-0000-0000-000000000000'

async function loginAt(page: Page, baseUrl: string) {
  await page.goto(`${baseUrl}/?editor-visual-reference=${Date.now()}`)
  await page.getByLabel('用户名').fill(process.env.E2E_USERNAME || 'admin')
  await page.getByLabel('密码').fill(process.env.E2E_PASSWORD || 'revaro-e2e-password')
  await page.getByRole('button', { name: '进入我的网盘' }).click()
  await expect(page.getByRole('heading', { name: '我的文件', exact: true })).toBeVisible()
}

async function createDocument(page: Page, name: string) {
  const result = await page.evaluate(async ({ name, root }) => {
    const response = await fetch('/api/documents', {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ parent_id: root, name, content: '# visual parity\n\n编辑器布局检查', }),
    })
    return { ok: response.ok, status: response.status }
  }, { name, root: ROOT })
  expect(result.ok, `创建测试文档失败：${result.status}`).toBe(true)
}

async function cleanup(page: Page, name: string) {
  await page.evaluate(async wanted => {
    const headers = { 'Content-Type': 'application/json' }
    const root = await fetch(`/api/files/${'00000000-0000-0000-0000-000000000000'}/children`)
    if (root.ok) {
      const children = await root.json() as { items?: Array<{ id: string; name: string }> }
      for (const item of children.items ?? []) {
        if (item.name === wanted) await fetch(`/api/files/${item.id}`, { method: 'DELETE', headers })
      }
    }
    const trash = await fetch('/api/trash')
    if (trash.ok) {
      const deleted = await trash.json() as { items?: Array<{ id: string; name: string }> }
      for (const item of deleted.items ?? []) {
        if (item.name === wanted) await fetch(`/api/trash/${item.id}`, { method: 'DELETE', headers })
      }
    }
  }, name)
}

async function snapshot(page: Page) {
  return page.locator('.document-editor').evaluate(editor => {
    const style = (target: Element | null) => {
      if (!target) return null
      const computed = getComputedStyle(target)
      const rect = target.getBoundingClientRect()
      return {
        rect: { x: rect.x, y: rect.y, width: rect.width, height: rect.height },
        display: computed.display,
        position: computed.position,
        minHeight: computed.minHeight,
        width: computed.width,
        height: computed.height,
        gap: computed.gap,
        padding: computed.padding,
        border: computed.border,
        borderRadius: computed.borderRadius,
        background: computed.backgroundColor,
        color: computed.color,
        boxShadow: computed.boxShadow,
        font: computed.font,
        lineHeight: computed.lineHeight,
      }
    }

    const titleIcon = editor.querySelector('.editor-title > span:first-child')
    const textarea = editor.querySelector('textarea')
    const tabs = editor.querySelector('.editor-tabs')
    return {
      editor: style(editor),
      backdrop: style(editor.parentElement),
      header: style(editor.querySelector('.editor-header')),
      title: style(editor.querySelector('.editor-title')),
      titleIcon: style(titleIcon),
      titleText: style(editor.querySelector('.editor-title > div')),
      meta: style(editor.querySelector('.editor-meta')),
      tabs: style(tabs),
      actions: style(editor.querySelector('.editor-actions')),
      close: style(editor.querySelector('.editor-close')),
      workspace: style(editor.querySelector('.editor-workspace')),
      textarea: style(textarea),
      tabLabels: Array.from(editor.querySelectorAll('.editor-tabs button')).map(button => ({
        text: button.textContent?.trim(),
        className: button.className,
        style: style(button),
      })),
      closeLabel: editor.querySelector('.editor-close')?.getAttribute('aria-label'),
      metaText: editor.querySelector('.editor-meta')?.textContent?.replace(/\s+/g, ' ').trim(),
    }
  })
}

async function openEditor(page: Page, baseUrl: string, name: string) {
  await loginAt(page, baseUrl)
  await createDocument(page, name)
  await page.reload()
  // The reference app opens documents through its list view; the Rust app only
  // has the grid, where the card itself opens the document.
  const toggle = page.getByTitle('列表视图')
  const useListView = (await toggle.count()) > 0
  if (useListView) await toggle.click()
  await page.locator(useListView ? '.file-row' : '.file-card').filter({ hasText: name }).click()
  await expect(page.locator('.document-editor')).toBeVisible()
  await expect(page.locator('.document-editor textarea')).toHaveValue('# visual parity\n\n编辑器布局检查')
}

test('桌面和移动端编辑器的视觉层级、几何与模式布局保持 reference', async ({ browser }) => {
  const oldUrl = process.env.E2E_REFERENCE_URL || 'http://127.0.0.1:18080'
  const newUrl = process.env.E2E_NEW_URL || 'http://127.0.0.1:18084'
  const suffix = crypto.randomUUID().slice(0, 8)

  for (const viewport of [{ width: 1440, height: 900 }, { width: 390, height: 844 }]) {
    const name = `editor-visual-${suffix}-${viewport.width}.md`
    const oldContext = await browser.newContext({ viewport })
    const newContext = await browser.newContext({ viewport })
    const oldPage = await oldContext.newPage()
    const newPage = await newContext.newPage()
    try {
      await Promise.all([openEditor(oldPage, oldUrl, name), openEditor(newPage, newUrl, name)])
      expect(await snapshot(newPage), `Rust ${viewport.width}px 编辑器视觉与 reference 不一致`).toEqual(await snapshot(oldPage))

      await Promise.all([
        oldPage.getByRole('button', { name: '分栏' }).click(),
        newPage.getByRole('button', { name: '分栏' }).click(),
      ])
      await Promise.all([oldPage.waitForTimeout(260), newPage.waitForTimeout(260)])
      expect(await snapshot(newPage), `Rust ${viewport.width}px 分栏布局与 reference 不一致`).toEqual(await snapshot(oldPage))

      await Promise.all([
        oldPage.getByRole('button', { name: '预览' }).click(),
        newPage.getByRole('button', { name: '预览' }).click(),
      ])
      await Promise.all([oldPage.waitForTimeout(260), newPage.waitForTimeout(260)])
      expect(await snapshot(newPage), `Rust ${viewport.width}px 预览布局与 reference 不一致`).toEqual(await snapshot(oldPage))
    } finally {
      await Promise.all([
        cleanup(oldPage, name),
        cleanup(newPage, name),
        oldContext.close(),
        newContext.close(),
      ])
    }
  }
})

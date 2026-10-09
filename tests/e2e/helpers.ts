import { expect, type Page } from '@playwright/test'
import { createHash } from 'node:crypto'

// Inject a failure where the application receives it. Page routes cannot see
// worker-owned API reads, and a worker route would exercise transport retries
// before the UI sees the error. Transport recovery has its own proxy tests.
export async function failApplicationRequest(page: Page, pathname: string, message: string, query: Record<string, string> = {}) {
  await page.evaluate(({ pathname, message, query }) => {
    const original = window.fetch.bind(window)
    ;(window as any).__applicationFault = { original, hits: 0 }
    window.fetch = (input, init) => {
      const url = new URL(typeof input === 'string' ? input : input instanceof Request ? input.url : input.toString(), location.href)
      if (url.pathname === pathname && Object.entries(query).every(([key, value]) => url.searchParams.get(key) === value)) {
        ;(window as any).__applicationFault.hits++
        return Promise.resolve(new Response(JSON.stringify({ error: { status: 503, message } }), { status: 503, headers: { 'Content-Type': 'application/json' } }))
      }
      return original(input, init)
    }
  }, { pathname, message, query })
  return {
    hits: () => page.evaluate(() => (window as any).__applicationFault.hits as number),
    clear: () => page.evaluate(() => {
      window.fetch = (window as any).__applicationFault.original
      delete (window as any).__applicationFault
    }),
  }
}

// Fixture setup follows the server's upload geometry for every MIME type.
// Recovery itself is exercised through the browser UI in upload-progress.spec.ts.
export async function uploadFixture(page: Page, parent_id: string, name: string, mime_type: string, data: Buffer) {
  const headers = { origin: new URL(page.url()).origin }
  const created = await page.request.post('/api/uploads', { headers, data: { parent_id, name, mime_type, size: data.length } })
  expect(created.status(), await created.text()).toBe(201)
  const session = await created.json()
  if (session.mode === 'multipart') {
    for (let number = 1; number <= session.part_count; number++) {
      const bytes = data.subarray((number - 1) * session.part_size, number * session.part_size)
      const hash = createHash('sha256').update(bytes).digest('hex')
      const sent = await page.request.put(`/api/uploads/${session.upload_id}/data/${number}`, {
        headers: { ...headers, 'X-Content-SHA256': hash }, data: bytes,
      })
      expect(sent.status(), await sent.text()).toBe(204)
      expect(sent.headers()['x-content-sha256']).toBe(hash)
    }
  } else {
    const sent = await page.request.put(`/api/uploads/${session.upload_id}/data`, { headers, data })
    expect(sent.ok(), await sent.text()).toBeTruthy()
  }
  const completed = await page.request.post(`/api/uploads/${session.upload_id}/complete`, { headers, data: { parts: [] } })
  expect(completed.ok(), await completed.text()).toBeTruthy()
  return completed.json()
}

export async function enterSelectionMode(page: Page) {
  await openTopbarMenu(page)
  const toggle = page.locator('.selection-toggle')
  await expect(toggle).toBeVisible()
  if (await toggle.getAttribute('aria-pressed') === 'false') await toggle.click()
  else await page.getByLabel('关闭菜单', { exact: true }).click()
  await expect(toggle).toHaveAttribute('aria-pressed', 'true')
}

export async function login(page: Page) {
  await page.goto('/')
  await page.getByLabel('用户名').fill(process.env.E2E_USERNAME || 'admin')
  await page.getByLabel('密码').fill(process.env.E2E_PASSWORD || 'revaro-e2e-password')
  await page.getByRole('button', { name: '进入我的内容库' }).click()
  if ((page.viewportSize()?.width || 1280) <= 850) {
    await page.getByRole('navigation', { name: '移动端导航', exact: true }).getByRole('link', { name: '文件', exact: true }).click()
  } else {
    await page.getByRole('navigation', { name: '主导航', exact: true }).getByRole('link', { name: '文件', exact: true }).click()
  }
  await expect(page.getByRole('navigation', { name: '当前路径', exact: true }).getByRole('button', { name: '我的文件', exact: true })).toBeVisible()
}

export async function openMusicPlayer(page: Page) {
  const panel = page.getByRole('dialog', { name: '详细音频播放器', exact: true })
  if (await panel.isVisible()) return
  const expand = page.getByRole('button', { name: '展开播放器', exact: true })
  await expect(expand).toBeVisible()
  await expand.click()
  await expect(panel).toBeVisible()
}

export async function selectMusicMode(page: Page, value: string) {
  await openMusicPlayer(page)
  const mode = page.locator('.music-dock .playback-mode')
  await mode.getByLabel('播放模式', { exact: true }).click()
  const labels: Record<string, RegExp> = {
    sequential: /^(顺序播放|播放一次)$/, shuffle: /^随机播放$/, 'repeat-all': /^列表循环$/, 'repeat-one': /^单曲循环$/,
  }
  await mode.getByRole('button', { name: labels[value] }).click()
  await expect(mode).toHaveAttribute('data-playback-mode', value)
}

// The player starts as an orb; pausing remains an explicit playback action.
export async function pauseMusic(page: Page) {
  if (await page.getByRole('button', { name: '展开播放器', exact: true }).isVisible()) await openMusicPlayer(page)
  const pause = page.locator('.music-dock').getByRole('button', { name: '暂停音乐', exact: true })
  if (await pause.isVisible()) await pause.click()
}

export async function openTopbarMenu(page: Page) {
  const menu = page.locator('.topbar .topbar-menu')
  if (!await menu.evaluate(element => (element as HTMLDetailsElement).open)) {
    await page.getByLabel('更多操作', { exact: true }).click()
  }
  await expect(page.locator('.topbar-menu-panel')).toBeVisible()
}

// Existing instances can contain more than one page of files. Newly uploaded
// fixtures should remain visible without depending on an empty root folder.
export async function showRecentFiles(page: Page) {
  await page.getByRole('button', { name: '选择排序字段', exact: true }).click()
  await page.getByRole('group', { name: '排序字段', exact: true }).getByRole('button', { name: '时间', exact: true }).click()
  const direction = page.getByRole('button', { name: '切换排序方向', exact: true })
  if ((await direction.getAttribute('aria-description'))?.includes('当前升序')) await direction.click()
}

export async function navigate(page: Page, name: string) {
  const mobile = (page.viewportSize()?.width || 1280) <= 850
  const nav = page.getByRole('navigation', { name: mobile ? '移动端导航' : '主导航', exact: true })
  if (mobile && ['书籍', '音乐', '图片', '视频'].includes(name)) {
    await nav.getByLabel('内容库', { exact: true }).click()
  }
  const link = nav.getByRole('link', { name: mobile && name === '图片' ? '图库' : name, exact: true, includeHidden: true })
  await link.click()
  return link
}

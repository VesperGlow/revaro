import { expect, test, type Page } from '@playwright/test'
import { login } from './helpers'

async function openManagement(page: Page) {
  await login(page)
  await page.getByRole('button', { name: '打开账户设置', exact: true }).click()
  const management = page.locator('.management')
  await expect(management.getByRole('button', { name: '刷新状态', exact: true })).toBeEnabled()
  return management
}

test('account management shows only the summary and empty links, refreshes and clears reading caches', async ({ page }) => {
  let requests = 0
  await page.route('**/api/shares?**', route => route.fulfill({ json: [] }))
  await page.route('**/api/system/status', route => route.fulfill({ json: {
    disk_available_bytes: 4 * 1024 ** 3,
    cache: { memory_bytes: 2 * 1024 ** 2, disk_bytes: 3 * 1024 ** 2, classes: [{ name: 'internal-cache-detail' }] },
    active_tasks: 12 + requests++, maintenance: { diagnostic: 'internal-maintenance-detail' },
    reader_available_slots: 999,
  } }))
  const management = await openManagement(page)
  await expect(management.locator('.status-metrics > span')).toHaveText(['磁盘可用空间4.0 GiB', '内存缓存2.0 MiB', '磁盘缓存3.0 MiB', '活动任务12'])
  await expect(management.getByText('暂无公开链接', { exact: true })).toBeVisible()
  await expect(management.getByRole('button')).toHaveCount(2)
  await expect(management.locator('details, pre')).toHaveCount(0)
  await expect(management).not.toContainText('缓存与维护详情')
  await expect(management).not.toContainText('internal-cache-detail')
  await expect(management).not.toContainText('internal-maintenance-detail')
  await expect(management).not.toContainText('reader_available_slots')
  await management.getByRole('button', { name: '刷新状态', exact: true }).click()
  await expect(management.getByRole('status')).toContainText('活动任务13')
  expect(requests).toBe(2)
  await page.evaluate(async () => {
    localStorage.setItem('revaro-reader-manifest:account-test', 'cached manifest')
    localStorage.setItem('account-management-keep', 'preserve')
    const cache = await caches.open('revaro-reader-flow-v2')
    await cache.put('/account-test-chunk', new Response('cached chunk'))
    const other = await caches.open('account-management-keep')
    await other.put('/other-chunk', new Response('preserve'))
  })
  await management.getByRole('button', { name: '清理阅读缓存', exact: true }).click()
  await expect.poll(() => page.evaluate(() => localStorage.getItem('revaro-reader-manifest:account-test'))).toBeNull()
  await expect.poll(() => page.evaluate(() => caches.has('revaro-reader-flow-v2'))).toBe(false)
  expect(await page.evaluate(() => localStorage.getItem('account-management-keep'))).toBe('preserve')
  expect(await page.evaluate(() => caches.has('account-management-keep'))).toBe(true)
  await page.setViewportSize({ width: 390, height: 844 })
  const empty = management.getByText('暂无公开链接', { exact: true })
  await empty.scrollIntoViewIfNeeded()
  expect(await empty.evaluate(element => {
    const box = element.getBoundingClientRect()
    return Boolean(document.elementFromPoint(box.x + box.width / 2, box.y + box.height / 2)?.closest('.account-modal'))
  })).toBe(true)
})

test('public links paginate only when needed and return to a valid page after its last link is revoked', async ({ page }) => {
  let count = 200
  const links = Array.from({ length: 201 }, (_, index) => ({
    file_id: `account-link-${index}`, name: `公开文件 ${index}`, active: true,
    url: `http://localhost/s/account-link-${index}`, created_at: '2026-10-03T00:00:00Z',
  }))
  await page.route('**/api/shares?**', route => {
    const offset = Number(new URL(route.request().url()).searchParams.get('offset'))
    return route.fulfill({ json: links.slice(0, count).slice(offset, offset + 200) })
  })
  await page.route('**/api/files/account-link-200/share', route => {
    expect(route.request().method()).toBe('DELETE')
    count = 200
    return route.fulfill({ status: 204 })
  })
  const management = await openManagement(page)
  await expect(management.locator('.history-row')).toHaveCount(200)
  await expect(management.getByRole('button', { name: '上一页', exact: true })).toHaveCount(0)
  await expect(management.getByRole('button', { name: '下一页', exact: true })).toHaveCount(0)
  count = 201
  await management.getByRole('button', { name: '刷新状态', exact: true }).click()
  await expect(management.getByRole('button', { name: '下一页', exact: true })).toBeEnabled()
  await expect(management.getByRole('button', { name: '上一页', exact: true })).toBeDisabled()
  await management.getByRole('button', { name: '下一页', exact: true }).click()
  await expect(management.locator('.history-row')).toHaveCount(1)
  await expect(management).toContainText('公开文件 200')
  await expect(management.getByRole('button', { name: '上一页', exact: true })).toBeEnabled()
  await expect(management.getByRole('button', { name: '下一页', exact: true })).toBeDisabled()
  await management.getByRole('button', { name: '撤销链接', exact: true }).click()
  await expect(management.locator('.history-row')).toHaveCount(200)
  await expect(management.getByRole('button', { name: '上一页', exact: true })).toHaveCount(0)
  await expect(management.getByRole('button', { name: '下一页', exact: true })).toHaveCount(0)
  await expect(management.getByText('暂无公开链接', { exact: true })).toHaveCount(0)
})

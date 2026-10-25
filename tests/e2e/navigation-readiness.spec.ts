import { test, expect } from '@playwright/test'
import { navigate } from './helpers'

for (const state of ['closed-stale-expanded', 'open-stale-collapsed'] as const) {
  test(`navigation reads the native disclosure state: ${state}`, async ({ page }) => {
    test.setTimeout(5_000)
    // Native details changes synchronously; its toggle event can update ARIA
    // later. Both directions must work without activating a hidden link.
    await page.route('**/navigation-check', route => route.fulfill({ contentType: 'text/html', body: `
      <meta charset="utf-8"><nav aria-label="主导航"><details ${state === 'open-stale-collapsed' ? 'open' : ''}>
      <summary aria-label="内容库" aria-expanded="${state === 'closed-stale-expanded'}">内容库</summary>
      <a aria-label="音乐" href="#music">音乐</a></details></nav>
    ` }))
    await page.goto('/navigation-check')
    await navigate(page, '音乐')
    await expect(page).toHaveURL(/#music$/)
  })
}

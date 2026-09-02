import { defineConfig } from '@playwright/test'

// 阅读器 v2 的浏览器级验证：vite dev 起 SPA，API 全部在用例内 route-mock
// （不依赖后端/存储），专注验证三页窗口、预取、纯 transform 翻页、
// 重排无缝切换与进度恢复。
export default defineConfig({
  testDir: './e2e',
  testMatch: /reader-v2.*\.spec\.ts/,
  timeout: 90_000,
  expect: { timeout: 10_000 },
  fullyParallel: false,
  workers: 1,
  reporter: process.env.CI ? [['list'], ['html', { open: 'never' }]] : 'list',
  use: { baseURL: 'http://localhost:18777', trace: 'retain-on-failure', screenshot: 'only-on-failure', video: 'retain-on-failure' },
  webServer: {
    command: 'npm run dev -- --port 18777 --strictPort',
    url: 'http://localhost:18777',
    reuseExistingServer: false,
    timeout: 60_000,
  },
  projects: [{ name: 'chromium', use: { browserName: 'chromium' } }],
})

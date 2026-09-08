import { defineConfig } from '@playwright/test'

export default defineConfig({
  testDir: './e2e',
  testMatch: /(?:media-ui|reader-flow)\.spec\.ts/,
  timeout: 60_000,
  expect: { timeout: 10_000 },
  workers: 1,
  reporter: 'list',
  use: {
    baseURL: 'http://127.0.0.1:18779',
    screenshot: 'only-on-failure',
    trace: 'retain-on-failure',
    launchOptions: process.env.PLAYWRIGHT_EXECUTABLE_PATH ? { executablePath: process.env.PLAYWRIGHT_EXECUTABLE_PATH } : {},
  },
  webServer: {
    command: 'npm run dev -- --host 127.0.0.1 --port 18779 --strictPort',
    url: 'http://127.0.0.1:18779',
    reuseExistingServer: !process.env.CI,
  },
})

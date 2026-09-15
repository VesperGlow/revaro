import { defineConfig, devices } from '@playwright/test'

const executablePath = process.env.PLAYWRIGHT_EXECUTABLE_PATH

// Dual-version parity specs read E2E_NEW_URL directly and historically used
// 18084 as their fallback. Keep that fallback on the current Rust instance so
// an omitted shell variable cannot silently exercise a stale build.
process.env.E2E_NEW_URL ||= 'http://127.0.0.1:18084'

export default defineConfig({
  testDir: '.',
  testIgnore: process.env.E2E_READER_FLOW === '1' ? [] : ['rust-reader-flow-reference-parity.spec.ts'],
  outputDir: 'test-results',
  timeout: 45_000,
  expect: { timeout: 10_000 },
  fullyParallel: false,
  workers: 1,
  retries: process.env.CI ? 1 : 0,
  reporter: process.env.CI
    ? [['html', { open: 'never' }], ['github']]
    : [['list'], ['html', { open: 'never' }]],
  use: {
    baseURL: process.env.E2E_BASE_URL || 'http://127.0.0.1:18080',
    trace: 'retain-on-failure',
    screenshot: 'only-on-failure',
    video: executablePath ? 'off' : 'retain-on-failure',
    ...(executablePath ? { launchOptions: { executablePath } } : {}),
  },
  projects: [{ name: 'chromium', use: { ...devices['Desktop Chrome'] } }],
})

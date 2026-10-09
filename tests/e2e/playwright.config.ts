import { defineConfig, devices } from '@playwright/test'

// Playwright 1.55 gates routing/observing worker-owned network requests.
process.env.PW_EXPERIMENTAL_SERVICE_WORKER_NETWORK_EVENTS = '1'

const executablePath = process.env.PLAYWRIGHT_EXECUTABLE_PATH
const browser = process.env.E2E_BROWSER || 'chromium'
const desktop = { chromium: 'Desktop Chrome', firefox: 'Desktop Firefox', webkit: 'Desktop Safari' }[browser]
if (!desktop) throw new Error(`Unsupported E2E_BROWSER: ${browser}`)

export default defineConfig({
  testDir: '.',
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
  projects: [{ name: browser, use: { ...devices[desktop] } }],
})

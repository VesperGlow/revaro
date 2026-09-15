import { defineConfig, devices } from '@playwright/test'

const oldUrl = process.env.E2E_REFERENCE_URL || 'http://127.0.0.1:18180'
const newUrl = process.env.E2E_NEW_URL || 'http://127.0.0.1:18184'

export default defineConfig({
  testDir: '.',
  testMatch: /rust-reader-(flow|real-epub)-reference-parity\.spec\.ts/,
  timeout: 90_000,
  expect: { timeout: 10_000 },
  fullyParallel: false,
  workers: 1,
  reporter: process.env.CI ? [['line'], ['github']] : 'line',
  use: {
    trace: 'retain-on-failure',
    video: 'off',
  },
  projects: [
    { name: 'old-reference', use: { ...devices['Desktop Chrome'], baseURL: oldUrl } },
    { name: 'rust-current', use: { ...devices['Desktop Chrome'], baseURL: newUrl } },
  ],
})

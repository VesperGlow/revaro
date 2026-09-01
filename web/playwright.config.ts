import { defineConfig, devices } from '@playwright/test'

export default defineConfig({
  testDir:'./e2e',
  timeout:45_000,
  expect:{timeout:10_000},
  fullyParallel:false,
  workers:1,
  retries:process.env.CI?1:0,
  reporter:process.env.CI?[['html',{open:'never'}],['github']]:[['list'],['html',{open:'never'}]],
  use:{baseURL:process.env.E2E_BASE_URL||'http://127.0.0.1:18080',trace:'retain-on-failure',screenshot:'only-on-failure',video:'retain-on-failure'},
  projects:[{name:'chromium',use:{...devices['Desktop Chrome']}}],
})

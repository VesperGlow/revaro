import { test, expect, type Page } from '@playwright/test'

// The packaged player is evaluated in real browsers, with a synthetic VFS/engine.
// This catches module syntax, automatic launch and silent host initialization
// failures without requiring copyrighted game resources in CI.
async function player(page: Page, options: { entry?: string, engine?: string, key?: string } = {}) {
  const html = await page.request.get('/galgame/siglus/index.html')
  expect(html.status()).toBe(200)
  const body = await html.body()
  await page.route('**/g/siglus-check?*', route => route.fulfill({ contentType: 'text/html', body }))
  await page.route('**/g/siglus-check/index', route => route.fulfill({ json: {
    engine: 'siglus', entry: options.entry || 'scene.chs', save_space: 'siglus-startup-test', key_config: options.key,
    entries: [
      { path: 'scene.chs', size: 0, packed: 0, method: 0, crc: 0, offset: 0 },
      { path: 'gameexe.chs', size: 0, packed: 0, method: 0, crc: 0, offset: 0 },
      { path: 'Scene.pck', size: 0, packed: 0, method: 0, crc: 0, offset: 0 },
      { path: 'savedata/foreign.sav', size: 0, packed: 0, method: 0, crc: 0, offset: 0 },
    ],
  } }))
  await page.route('**/galgame/siglus/pkg/game_launcher.js', route => route.fulfill({ contentType: 'text/javascript', body: options.engine || `
    export default async function init() {}
    export function start_siglus_from_directory(id) {
      window.__siglusStarted = { id,
        key: globalThis.siglusFileExists('key.toml') ? new TextDecoder().decode(globalThis.siglusReadFile('key.toml')) : null,
        foreignSave: globalThis.siglusFileExists('savedata/foreign.sav') };
    }
  ` }))
}

test('Siglus loads automatically without a toolbar and uses the shared entry and config', async ({ page }) => {
  await player(page, { key: 'key = [0,1,2,3,4,5,6,7,8,9,10,11,12,13,14,15]' })
  const errors: string[] = []
  page.on('pageerror', error => errors.push(error.message))
  // Query parameters must not override the administrator's selected entry.
  await page.goto('/g/siglus-check?profile=original')
  await expect.poll(() => page.evaluate(() => (window as any).__siglusStarted)).toBeTruthy()
  expect(await page.evaluate(() => (window as any).__siglusStarted)).toMatchObject({ id: 'siglus-canvas', foreignSave: false, key: 'key = [0,1,2,3,4,5,6,7,8,9,10,11,12,13,14,15]' })
  expect(await page.evaluate(() => (window as any).siglusProbe.profile)).toBe('chinese')
  await expect(page.locator('#loading')).toBeHidden()
  await expect(page.locator('#error')).toBeHidden()
  await expect(page.locator('#bar, #start')).toHaveCount(0)
  const bounds = await page.locator('canvas').boundingBox()
  expect(bounds?.height).toBe(await page.evaluate(() => innerHeight))
  expect(errors).toEqual([])
})

test('Siglus displays asynchronous host errors instead of leaving a black screen', async ({ page }) => {
  await player(page, { entry: 'Scene.pck', engine: `
    export default async function init() {}
    export function start_siglus_from_directory() {
      setTimeout(() => console.error('host init: test scene decode failed'), 0);
    }
  ` })
  await page.goto('/g/siglus-check?profile=chinese')
  await expect(page.getByRole('alert')).toContainText('test scene decode failed')
  await expect(page.locator('#loading')).toBeHidden()
  expect(await page.evaluate(() => (window as any).siglusProbe.profile)).toBe('original')
})

test('Siglus reports a failed engine import on the page', async ({ page }) => {
  await player(page, { engine: 'export default async function init( { broken syntax' })
  await page.goto('/g/siglus-check?profile=chinese')
  await expect(page.getByRole('alert')).toContainText('游戏启动失败')
  await expect(page.locator('#loading')).toBeHidden()
})

test('a WASM abort preserves the Rust panic that explains the failure', async ({ page }) => {
  await player(page, { engine: `
    export default async function init() {}
    export function start_siglus_from_directory() {
      setTimeout(() => {
        console.error('panicked at cpal: Unable to create a webaudio buffer source');
        throw new WebAssembly.RuntimeError('unreachable');
      }, 0);
    }
  ` })
  await page.goto('/g/siglus-check?profile=original')
  await expect(page.getByRole('alert')).toContainText('Unable to create a webaudio buffer source')
  await expect(page.getByRole('alert')).not.toHaveText('游戏启动失败\nunreachable')
  expect(await page.evaluate(() => (window as any).siglusProbe.phase)).toBe('failed')
})

test('the first canvas gesture resumes autoplay-blocked audio without a launch toolbar', async ({ page }) => {
  await page.addInitScript(() => {
    class FakeAudioContext extends EventTarget {
      state = 'suspended'
      async resume() { this.state = 'running'; this.dispatchEvent(new Event('statechange')) }
      async suspend() { this.state = 'suspended'; this.dispatchEvent(new Event('statechange')) }
      async close() { this.state = 'closed'; this.dispatchEvent(new Event('statechange')) }
    }
    Object.defineProperty(window, 'AudioContext', { writable: true, configurable: true, value: FakeAudioContext })
  })
  await player(page, { engine: `
    export default async function init() {}
    export function start_siglus_from_directory() { window.__context = new AudioContext() }
  ` })
  await page.goto('/g/siglus-check?profile=original')
  await expect(page.locator('#audio-hint')).toBeVisible()
  await page.locator('canvas').click()
  await expect(page.locator('#audio-hint')).toBeHidden()
  expect(await page.evaluate(() => (window as any).__context.state)).toBe('running')
  await expect(page.locator('#bar, #start')).toHaveCount(0)
})

test('retired audio contexts do not cover a playing game with a failure overlay', async ({ page }) => {
  await page.addInitScript(() => {
    class StrictAudioContext extends EventTarget {
      state = 'running'
      resume() { return this.state === 'closed' ? Promise.reject(new DOMException('AudioContext is closed.', 'InvalidStateError')) : Promise.resolve() }
      suspend() { return this.resume() }
      close() {
        if (this.state === 'closed') return Promise.reject(new DOMException('AudioContext is closed.', 'InvalidStateError'))
        this.state = 'closed'
        this.dispatchEvent(new Event('statechange'))
        return Promise.resolve()
      }
    }
    Object.defineProperty(window, 'AudioContext', { writable: true, configurable: true, value: StrictAudioContext })
  })
  await player(page, { engine: `
    export default async function init() {}
    export async function start_siglus_from_directory() {
      const probe = new AudioContext();
      await probe.close();
      await probe.close();
      await probe.resume();
      window.__liveAudio = new AudioContext();
      const ctx = document.getElementById('siglus-canvas').getContext('2d');
      ctx.fillStyle = 'green'; ctx.fillRect(0, 0, 100, 100);
      // Model a late event from a stream that was retired by CPAL.
      window.addEventListener('unhandledrejection', event => {
        window.__retiredAudioNotice = { message: event.reason.message, handled: event.defaultPrevented };
      }, { once: true });
      setTimeout(() => Promise.reject(new DOMException('AudioContext is closed.', 'InvalidStateError')), 0);
      window.__siglusStarted = true;
    }
  ` })
  await page.goto('/g/siglus-check?profile=original')
  await expect.poll(() => page.evaluate(() => (window as any).__siglusStarted)).toBe(true)
  await expect.poll(() => page.evaluate(() => (window as any).siglusProbe.phase)).toBe('running')
  // Firefox also emits protocol pageerror for a canceled rejection. Check the
  // browser event directly so this verifies the player's handling on every engine.
  await expect.poll(() => page.evaluate(() => (window as any).__retiredAudioNotice)).toEqual({
    message: 'AudioContext is closed.', handled: true,
  })
  await expect(page.getByRole('alert')).toBeHidden()
  expect(await page.evaluate(() => (window as any).__liveAudio.state)).toBe('running')
  // Without a replacement stream, a closed-context error is a real failure.
  await page.evaluate(async () => {
    await (window as any).__liveAudio.close()
    void Promise.reject(new DOMException('AudioContext is closed.', 'InvalidStateError'))
  })
  await expect(page.getByRole('alert')).toContainText('AudioContext is closed.')
  // An unrelated runtime failure must remain visible.
  await page.evaluate(() => { void Promise.reject(new Error('actual VM failure')) })
  await expect(page.getByRole('alert')).toContainText('actual VM failure')
})

import { test, expect } from '@playwright/test'

// Exercise the real adapter against a downstream SDL-style cancelling listener.
// No game fixture is needed to check browser shortcut ownership during startup.
for (const stage of ['startup', 'game', 'modal'] as const) {
  test(`Galgame leaves browser shortcuts uncancelled during ${stage}`, async ({ page, browserName }) => {
    test.skip(browserName !== 'chromium', 'The Kirikiri runtime targets Chromium')
    await page.route('**/shortcut-check', route => route.fulfill({ contentType: 'text/html', body:
      '<div id="container"><canvas id="canvas" tabindex="0"></canvas></div><script src="/galgame/kirikiri2/input.js"></script>',
    }))
    await page.goto('/shortcut-check')
    await page.waitForFunction(() => Boolean((window as any).RevaroPlayer))
    const result = await page.evaluate(stage => {
      const w = window as any
      if (stage !== 'startup') {
        const memory = new ArrayBuffer(8 << 20)
        Object.assign(w, { HEAPU8: new Uint8Array(memory), HEAPU32: new Uint32Array(memory),
          HEAP32: new Int32Array(memory), HEAPF32: new Float32Array(memory),
          wasmExports: { revaro_mouse_scroll() {}, revaro_key_release() {} } })
        const id = 7_000_000
        w.HEAPU32[5858060 >>> 2] = id
        w.HEAPU32[id >>> 2] = 5101940
        w.HEAP32[(id + 860) >>> 2] = 1280
        w.HEAP32[(id + 864) >>> 2] = 720
        w.HEAPF32[(id + 652) >>> 2] = 1280
        w.HEAPF32[(id + 656) >>> 2] = 720
        w.HEAPU8[id + 6503] = stage === 'modal' ? 1 : 0
      }
      const received: string[] = []
      for (const type of ['keydown', 'keyup']) document.addEventListener(type, event => {
        received.push((event as KeyboardEvent).code)
        event.preventDefault()
      }, true)
      const canvas = document.getElementById('canvas')!
      canvas.focus()
      const keys = [
        { key: 'F12', code: 'F12', keyCode: 123 },
        { key: 'F12', code: '', keyCode: 123 },
        { key: 'F11', code: 'F11', keyCode: 122 },
        ...['I', 'J', 'C'].map(key => ({ key, code: `Key${key}`, keyCode: key.charCodeAt(0), ctrlKey: true, shiftKey: true })),
      ]
      const cancelled = keys.flatMap(key => ['keydown', 'keyup'].map(type => {
        const event = new KeyboardEvent(type, { ...key, bubbles: true, cancelable: true })
        canvas.dispatchEvent(event)
        return event.defaultPrevented
      }))
      const held = w.RevaroPlayer.snapshot().heldKeys
      if (stage !== 'startup') {
        const enter = new KeyboardEvent('keydown', { key: 'Enter', code: 'Enter', keyCode: 13, bubbles: true, cancelable: true })
        canvas.dispatchEvent(enter)
      }
      return { cancelled, received, held }
    }, stage)
    expect(result.cancelled.every(value => !value)).toBeTruthy()
    expect(result.held).toBe(0)
    expect(result.received).toEqual(stage === 'startup' ? [] : ['Enter'])
  })
}

import { afterEach, expect, it, vi } from 'vitest'
import { api } from './api'

afterEach(() => { vi.useRealTimers(); vi.unstubAllGlobals() })

it('allows long verification requests while preserving explicit cancellation', async () => {
  vi.useFakeTimers()
  vi.stubGlobal('window', globalThis)
  vi.stubGlobal('fetch', vi.fn((_path: string, init: RequestInit) => new Promise((_resolve, reject) => {
    init.signal?.addEventListener('abort', () => reject(new Error('cancelled')), { once: true })
  })))
  const controller = new AbortController()
  const request = api('/api/uploads/slow/complete', { signal: controller.signal }, 0)
  const rejected = expect(request).rejects.toThrow('cancelled')
  await vi.advanceTimersByTimeAsync(180_000)
  const signal = vi.mocked(fetch).mock.calls[0][1]?.signal
  expect(signal?.aborted).toBe(false)
  controller.abort()
  await rejected
})

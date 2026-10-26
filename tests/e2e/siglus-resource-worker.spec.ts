import { test, expect } from '@playwright/test'

test('resource worker reads binary ranges, inflates and rejects corrupt bytes', async ({ page }) => {
  const asset = await page.request.get('/galgame/siglus/resource-worker.js')
  expect(asset.status()).toBe(200)
  expect(asset.headers()['content-type']).toContain('javascript')
  await page.route('**/siglus-resource-check', route => route.fulfill({ contentType: 'text/html', body: '<!doctype html><title>Resource worker verification</title>' }))
  const ranges: string[] = []
  await page.route('**/prefetch-archive?*', route => {
    ranges.push(route.request().headers().range)
    const deflated = route.request().url().includes('deflate')
    return route.fulfill({ status: 206, contentType: 'application/zip', body: deflated ? Buffer.from([75, 76, 74, 6, 0]) : Buffer.from('abc') })
  })
  await page.goto('/siglus-resource-check')
  const results = await page.evaluate(async () => {
    const worker = new Worker('/galgame/siglus/resource-worker.js')
    try {
      const results = []
      for (const task of [
        { id: 1, method: 0, packed: 3, crc: 0x352441c2 },
        { id: 2, method: 8, packed: 5, crc: 0x352441c2 },
        { id: 3, method: 0, packed: 3, crc: 0 },
      ]) {
        results.push(await new Promise(resolve => {
          worker.onmessage = ({ data }) => resolve({ id: data.id, error: data.error, text: data.bytes ? new TextDecoder().decode(data.bytes) : undefined })
          worker.postMessage({ ...task, offset: 7, size: 3, url: new URL(`/prefetch-archive?${task.method === 8 ? 'deflate' : 'store'}`, location.href).href })
        }))
      }
      return results
    } finally { worker.terminate() }
  })
  expect(results).toEqual([{ id: 1, error: undefined, text: 'abc' }, { id: 2, error: undefined, text: 'abc' }, { id: 3, error: 'Prefetch size/CRC mismatch', text: undefined }])
  expect(ranges).toEqual(['bytes=7-9', 'bytes=7-11', 'bytes=7-9'])
})

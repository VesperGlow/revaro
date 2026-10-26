import { test, expect } from '@playwright/test'

test('OVK bridge reuses the voice pack and returns only the selected segment', async ({ page }) => {
  const archive = Buffer.alloc(42)
  archive.writeUInt32LE(2, 0)
  for (const [i, no] of [101, 202].entries()) {
    archive.writeUInt32LE(3, 4 + i * 16)
    archive.writeUInt32LE(36 + i * 3, 8 + i * 16)
    archive.writeUInt32LE(no, 12 + i * 16)
  }
  archive.write('oneTWO', 36)
  let crc = -1
  for (const byte of archive) { crc ^= byte; for (let i = 0; i < 8; i++) crc = (crc >>> 1) ^ ((crc & 1) ? 0xedb88320 : 0) }
  const checksum = (crc ^ -1) >>> 0
  const html = await (await page.request.get('/galgame/siglus/index.html')).body()
  await page.route('**/g/voice-cache-check', route => route.fulfill({ contentType: 'text/html', body: html }))
  await page.route('**/galgame/siglus/renderer.js', route => route.fulfill({ contentType: 'text/javascript', body: "export async function selectRenderer(){return {backend:'gl'}}" }))
  await page.route('**/galgame/siglus/pkg/game_launcher.js', route => route.fulfill({ contentType: 'text/javascript', body: 'export default async function init(){}; export function start_siglus_from_directory(){}' }))
  await page.route('**/g/voice-cache-check/index', route => route.fulfill({ json: { entry: 'scene.chs', save_space: 'siglus-voice-cache-test', entries: [
    { path: 'scene.chs', size: 0, packed: 0 }, { path: 'gameexe.chs', size: 0, packed: 0 },
    { path: 'koe/test.ovk', offset: 7, size: archive.length, packed: archive.length, method: 0, crc: checksum },
  ] } }))
  let requests = 0
  await page.route('**/g/voice-cache-check/archive', route => {
    requests++
    expect(route.request().headers().range).toBe('bytes=7-48')
    return route.fulfill({ status: 206, contentType: 'application/zip', body: archive })
  })
  await page.goto('/g/voice-cache-check')
  await page.waitForFunction(() => (window as any).siglusProbe?.phase === 'running')
  const result = await page.evaluate(() => {
    const read = (no: number) => new TextDecoder().decode((window as any).siglusReadOvkEntry('koe/test.ovk', no))
    const values = Array.from({ length: 100 }, (_, i) => read(i % 2 ? 101 : 202))
    let missing = ''
    try { read(999) } catch (error: any) { missing = error.message }
    ;(window as any).gameFsWrite('koe/test.ovk', new Uint8Array([2, 0, 0, 0]))
    let malformed = ''
    try { read(101) } catch (error: any) { malformed = error.message }
    ;(window as any).gameFsRemove('koe/test.ovk')
    return { values, missing, malformed, restored: read(101), lengths: (window as any).siglusProbe.voiceReads.map((r: any) => r.bytes) }
  })
  expect(result.values).toEqual(Array.from({ length: 100 }, (_, i) => i % 2 ? 'one' : 'TWO'))
  expect(result.missing).toContain('not found')
  expect(result.malformed).toContain('truncated')
  expect(result.restored).toBe('one')
  expect(result.lengths.every((n: number) => n === 3)).toBe(true)
  expect(requests).toBe(1)
})

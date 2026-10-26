import test from 'node:test'
import assert from 'node:assert/strict'
import { readFile } from 'node:fs/promises'
import vm from 'node:vm'

const source = await readFile(new URL('../../crates/revaro-web/static/galgame/kirikiri2/vlfs.js', import.meta.url), 'utf8')
function fixture() {
  const window = {}
  vm.runInNewContext(source, { window, console, Blob, URL, Uint8Array, TextDecoder, TextEncoder, crypto, performance })
  const fs = window.VLFS
  fs.registerBlobFile('/sample', new Blob([new Uint8Array(600_000).fill(73)]))
  return fs
}

test('Kiri concurrent reads of one block share the same source read', async () => {
  const fs = fixture(), original = fs._readSource.bind(fs)
  let requests = 0
  fs._readSource = async (...args) => { requests++; return original(...args) }
  const results = await Promise.all(Array.from({ length: 100 }, () => fs.read(fs.open('/sample', 0), 64)))
  assert.equal(requests, 1)
  for (const bytes of results) assert.deepEqual([...bytes], Array(64).fill(73))
  const fd = fs.open('/sample', 0)
  assert.deepEqual([...fs.readCached(fd, 64)], Array(64).fill(73))
})

test('Kiri failed coalesced reads can retry', async () => {
  const fs = fixture(), original = fs._readSource.bind(fs)
  let requests = 0
  fs._readSource = async (...args) => { requests++; if (requests === 1) throw Error('temporary read failure'); return original(...args) }
  const failed = await Promise.allSettled([fs.read(fs.open('/sample', 0), 64), fs.read(fs.open('/sample', 0), 64)])
  assert(failed.every(result => result.status === 'rejected'))
  assert.equal(requests, 1)
  assert.equal((await fs.read(fs.open('/sample', 0), 64))[0], 73)
  assert.equal(requests, 2)
})

test('Kiri pending multiblock read survives another reader evicting its first block', async () => {
  const fs = fixture(), original = fs._readSource.bind(fs)
  fs._readSource = async (entry, pos, len) => {
    if (pos > 0) { fs._blockCache.clear(); fs._blockCacheBytes = 0 }
    return original(entry, pos, len)
  }
  const bytes = await fs.read(fs.open('/sample', 0), 300_000)
  assert.equal(bytes.length, 300_000)
  assert(bytes.every(value => value === 73))
})

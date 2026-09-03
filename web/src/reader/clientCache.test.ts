import { describe, expect, it } from 'vitest'
import type { PersistentKV, CacheEntry } from './clientCache'
import { chunkKey, ClientCacheManager, manifestKey } from './clientCache'
import type { FlowManifest } from './types'

// memKV 是 PersistentKV 的内存实现（单测替身；生产路径为 IndexedDB）。
function memKV(): PersistentKV & { dump(): Map<string, CacheEntry> } {
  const map = new Map<string, CacheEntry>()
  return {
    async get(key) {
      return map.get(key)
    },
    async set(key, entry) {
      map.set(key, entry)
    },
    async remove(key) {
      map.delete(key)
    },
    async entries() {
      return Array.from(map.entries())
    },
    dump: () => map,
  }
}

function makeManifest(overrides: Partial<FlowManifest> = {}): FlowManifest {
  return {
    version: 4,
    format: 'epub',
    total_chars: 1000,
    book_key: 'book-a',
    spines: [{ block_start: 0, block_count: 10 }],
    chunks: Array.from({ length: 5 }, (_, i) => ({ index: i, block_start: i * 2, block_count: 2, chars: 200 })),
    toc: [{ label: '一', depth: 0, spine: 0, block: 0 }],
    ...overrides,
  }
}

describe('ClientCacheManager（持久 L2）', () => {
  it('manifest/chunks 往返；chunk 键绑定内容指纹与 flow 版本', async () => {
    const kv = memKV()
    const cache = new ClientCacheManager(1 << 20, kv)
    const m = makeManifest()
    await cache.putManifest('file-1', m)
    expect(await cache.getManifest('file-1')).toEqual(m)

    await cache.putChunk('book-a', 4, 0, '<p>chunk0</p>')
    expect(await cache.getChunk('book-a', 4, 0)).toBe('<p>chunk0</p>')
    // 指纹或版本不同 → miss（同一文件 id 换内容/升级 flow 后旧缓存不串用）
    expect(await cache.getChunk('book-b', 4, 0)).toBeNull()
    expect(await cache.getChunk('book-a', 3, 0)).toBeNull()
    expect(cache.stats.hits).toBe(2)
    expect(cache.stats.misses).toBe(2)
    expect(cache.stats.puts).toBe(2)
  })

  it('putManifest 版本/内容变化时清除旧 chunks', async () => {
    const kv = memKV()
    const cache = new ClientCacheManager(1 << 20, kv)
    await cache.putManifest('file-1', makeManifest())
    await cache.putChunk('book-a', 4, 0, 'c0')
    await cache.putChunk('book-a', 4, 1, 'c1')

    await cache.putManifest('file-1', makeManifest({ version: 5 }))
    expect(await cache.getChunk('book-a', 4, 0)).toBeNull()
    expect(await cache.getChunk('book-a', 4, 1)).toBeNull()

    // 同一书内容重传（book_key 变化）：按旧指纹清除
    await cache.putManifest('file-1', makeManifest({ book_key: 'book-a2' }))
    await cache.putChunk('book-a2', 4, 0, 'new')
    expect(await cache.getChunk('book-a2', 4, 0)).toBe('new')
    await cache.putManifest('file-1', makeManifest({ book_key: 'book-a3' }))
    expect(await cache.getChunk('book-a2', 4, 0)).toBeNull()
  })

  it('字节预算内 LRU 淘汰最旧条目', async () => {
    const kv = memKV()
    // 每字符 UTF-16 计 2 字节：条目 8 字符 = 16 字节，预算 40
    const cache = new ClientCacheManager(40, kv)
    await cache.putChunk('book-a', 4, 0, 'chunk-00')
    await cache.putChunk('book-a', 4, 1, 'chunk-01')
    await cache.getChunk('book-a', 4, 0) // touch chunk0
    await cache.putChunk('book-a', 4, 2, 'chunk-02')
    expect(await cache.getChunk('book-a', 4, 0)).toBe('chunk-00') // 最近使用
    await cache.putChunk('book-a', 4, 3, 'chunk-03') // 总量 64 > 40 → 按 LRU 逐出 chunk-01、chunk-02
    expect(await cache.getChunk('book-a', 4, 1)).toBeNull()
    expect(await cache.getChunk('book-a', 4, 2)).toBeNull()
    expect(await cache.getChunk('book-a', 4, 0)).toBe('chunk-00') // 最近使用存活
    expect(await cache.getChunk('book-a', 4, 3)).toBe('chunk-03')
    expect(cache.stats.evictions).toBe(2)
  })

  it('purgeBook 清除 manifest 与全部 chunk', async () => {
    const kv = memKV()
    const cache = new ClientCacheManager(1 << 20, kv)
    await cache.putManifest('file-1', makeManifest())
    await cache.putChunk('book-a', 4, 0, 'c0')
    await cache.putChunk('book-a', 4, 1, 'c1')
    await cache.purgeBook('file-1')
    expect(await cache.getManifest('file-1')).toBeNull()
    expect(await kv.get(chunkKey('book-a', 4, 0))).toBeUndefined()
    expect(await kv.get(manifestKey('file-1'))).toBeUndefined()
  })

  it('损坏的 manifest 数据视为 miss', async () => {
    const kv = memKV()
    const cache = new ClientCacheManager(1 << 20, kv)
    await kv.set(manifestKey('file-1'), { data: '{not json', bytes: 9, at: Date.now() })
    expect(await cache.getManifest('file-1')).toBeNull()
  })
})

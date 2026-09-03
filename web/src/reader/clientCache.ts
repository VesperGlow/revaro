// 客户端持久缓存 L2（ClientCacheManager）：内存 PageCache 仍是 L1，这里
// 在其下增加 IndexedDB 持久层，缓存 reader manifest/chunks。重新打开同一
// 本书时优先本地立即显示（L2 manifest + L2 chunks 命中则零网络排版），
// 网络 manifest（no-cache）随后后台校验，版本/内容变化才重建。
//
// 键空间（与统一 Global CacheManager 的 key namespace 对齐）：
//   manifest →  m:<fileId>
//   chunk    →  c:<bookKey>:v<version>:<index>
// bookKey 是服务端注入 manifest.book_key 的书 blob 内容指纹：同一文件 id
// 被替换为不同内容后指纹改变，旧 chunk 缓存按前缀整体清除。
//
// 容量与淘汰：全局字节预算内按 LRU 淘汰；统计 hits/misses/evictions/puts
// 供调试与测试断言。媒体 range / 缩略图 / 资产图片由 HTTP immutable 缓存
// 头与 S3 直链承担，不复制进 IndexedDB。

import type { FlowManifest } from './types'

export interface CacheEntry {
  data: string
  bytes: number
  at: number
}

export interface CacheStats {
  hits: number
  misses: number
  puts: number
  evictions: number
  bytes: number
}

// PersistentKV 抽象掉 IndexedDB，便于单测注入内存实现。
export interface PersistentKV {
  get(key: string): Promise<CacheEntry | undefined>
  set(key: string, entry: CacheEntry): Promise<void>
  remove(key: string): Promise<void>
  entries(): Promise<Array<[string, CacheEntry]>>
}

const DB_NAME = 'revaro-reader-cache'
const DB_VERSION = 1
const STORE = 'entries'

export function manifestKey(fileId: string): string {
  return `m:${fileId}`
}

export function chunkKey(bookKey: string, version: number, index: number): string {
  return `c:${bookKey}:v${version}:${index}`
}

// IndexedDB-backed KV；打开失败（隐私模式等）时所有操作静默降级为 miss。
class IdbKV implements PersistentKV {
  private db: Promise<IDBDatabase | null>

  constructor() {
    this.db = new Promise(resolve => {
      try {
        const open = indexedDB.open(DB_NAME, DB_VERSION)
        open.onupgradeneeded = () => {
          const db = open.result
          if (!db.objectStoreNames.contains(STORE)) db.createObjectStore(STORE)
        }
        open.onsuccess = () => resolve(open.result)
        open.onerror = () => resolve(null)
        open.onblocked = () => resolve(null)
      } catch {
        resolve(null)
      }
    })
  }

  private async tx<T>(mode: IDBTransactionMode, run: (store: IDBObjectStore) => IDBRequest<T>): Promise<T | null> {
    const db = await this.db
    if (!db) return null
    return new Promise(resolve => {
      try {
        const tx = db.transaction(STORE, mode)
        const request = run(tx.objectStore(STORE))
        tx.oncomplete = () => resolve(request.result)
        tx.onerror = () => resolve(null)
        tx.onabort = () => resolve(null)
      } catch {
        resolve(null)
      }
    })
  }

  get(key: string): Promise<CacheEntry | undefined> {
    return this.tx('readonly', store => store.get(key) as IDBRequest<CacheEntry | undefined>).then(v => v ?? undefined)
  }

  async set(key: string, entry: CacheEntry): Promise<void> {
    await this.tx('readwrite', store => store.put(entry, key) as IDBRequest<IDBValidKey>)
  }

  async remove(key: string): Promise<void> {
    await this.tx('readwrite', store => store.delete(key) as IDBRequest<undefined>)
  }

  entries(): Promise<Array<[string, CacheEntry]>> {
    return this.tx('readonly', store => store.getAll() as IDBRequest<CacheEntry[]>).then(async values => {
      if (!values) return []
      const keys = (await this.tx('readonly', store => store.getAllKeys() as IDBRequest<IDBValidKey[]>)) ?? []
      return values.map((value, i) => [String(keys[i]), value] as [string, CacheEntry]).filter(([key]) => !!key)
    })
  }
}

export class ClientCacheManager {
  stats: CacheStats = { hits: 0, misses: 0, puts: 0, evictions: 0, bytes: 0 }
  private kv: PersistentKV
  private lastTick = 0

  constructor(private budget = 64 << 20, kv?: PersistentKV) {
    this.kv = kv ?? new IdbKV()
  }

  // tick 产生单调递增的 LRU 时钟：同一毫秒内的多次读取也有确定的新旧序。
  private tick(): number {
    const now = Date.now()
    this.lastTick = Math.max(now, this.lastTick + 1)
    return this.lastTick
  }

  private async load(key: string): Promise<CacheEntry | undefined> {
    const hit = await this.kv.get(key)
    if (!hit) {
      this.stats.misses++
      return undefined
    }
    this.stats.hits++
    // 读取即视为最近使用（LRU touch）
    await this.kv.set(key, { ...hit, at: this.tick() })
    return hit
  }

  // peek 读取但不计入 hit/miss 统计（putManifest/purgeBook 的内部校验）。
  private async peek(key: string): Promise<CacheEntry | undefined> {
    return this.kv.get(key)
  }

  private async store(key: string, data: string): Promise<void> {
    const entry: CacheEntry = { data, bytes: data.length * 2 /* UTF-16 字节估算 */, at: this.tick() }
    this.stats.puts++
    await this.kv.set(key, entry)
    await this.evict()
  }

  // evict 在字节预算内按 LRU 淘汰最旧条目。
  private async evict(): Promise<void> {
    const entries = await this.kv.entries()
    let total = 0
    for (const [, entry] of entries) total += entry.bytes
    if (total <= this.budget) {
      this.stats.bytes = total
      return
    }
    const oldest = entries.sort((a, b) => a[1].at - b[1].at)
    for (const [key, entry] of oldest) {
      if (total <= this.budget) break
      await this.kv.remove(key)
      total -= entry.bytes
      this.stats.evictions++
    }
    this.stats.bytes = total
  }

  // getManifest 读取 L2 manifest。
  async getManifest(fileId: string): Promise<FlowManifest | null> {
    const entry = await this.load(manifestKey(fileId))
    if (!entry) return null
    try {
      const manifest = JSON.parse(entry.data) as FlowManifest
      if (!manifest || !Array.isArray(manifest.chunks) || !Array.isArray(manifest.spines)) return null
      return manifest
    } catch {
      void this.kv.remove(manifestKey(fileId))
      return null
    }
  }

  // putManifest 写 L2 manifest，并清除该书旧内容指纹下的全部 chunk。
  async putManifest(fileId: string, manifest: FlowManifest): Promise<void> {
    const previous = await this.peekManifest(fileId)
    await this.store(manifestKey(fileId), JSON.stringify(manifest))
    const oldBookKey = previous?.book_key
    const newBookKey = manifest.book_key ?? ''
    const oldVersion = previous?.version
    if (oldBookKey && (oldBookKey !== newBookKey || oldVersion !== manifest.version)) {
      await this.purgeChunks(oldBookKey, oldVersion)
    }
    if (!newBookKey && previous) {
      // 无指纹的 manifest 无法安全隔离 chunk：清除其旧 chunks 防串书
      await this.purgeChunks(oldBookKey ?? '', oldVersion)
    }
  }

  async getChunk(bookKey: string, version: number, index: number): Promise<string | null> {
    const entry = await this.load(chunkKey(bookKey, version, index))
    return entry?.data ?? null
  }

  async putChunk(bookKey: string, version: number, index: number, html: string): Promise<void> {
    await this.store(chunkKey(bookKey, version, index), html)
  }

  // purgeChunks 清除某书（指定内容指纹 + 版本）的全部 chunk 条目；
  // version 为空时清除该书所有版本的 chunk。
  async purgeChunks(bookKey: string, version?: number): Promise<void> {
    if (!bookKey) return
    const prefix = `c:${bookKey}:v${version ?? ''}`
    const entries = await this.kv.entries()
    for (const [key, entry] of entries) {
      const match = version === undefined ? key.startsWith(`c:${bookKey}:v`) : key.startsWith(prefix)
      if (match) {
        await this.kv.remove(key)
        this.stats.evictions++
        this.stats.bytes = Math.max(0, this.stats.bytes - entry.bytes)
      }
    }
  }

  // peekManifest 读取 manifest 但不计统计。
  private async peekManifest(fileId: string): Promise<FlowManifest | null> {
    const entry = await this.peek(manifestKey(fileId))
    if (!entry) return null
    try {
      return JSON.parse(entry.data) as FlowManifest
    } catch {
      return null
    }
  }

  // purgeBook 清除某书的 manifest 与全部 chunk（版本/内容变化时调用）。
  async purgeBook(fileId: string): Promise<void> {
    const previous = await this.peekManifest(fileId)
    if (previous) await this.purgeChunks(previous.book_key ?? '')
    await this.kv.remove(manifestKey(fileId))
  }
}

// chunk HTML 缓存：客户端只渲染当前位置附近的少量 chunk，翻页热路径零网络。
// 键 = chunk 序号；LRU 裁剪。
export class PageCache {
  private entries = new Map<number, string>()

  constructor(private capacity = 24) {}

  has(key: number): boolean {
    return this.entries.has(key)
  }

  get(key: number): string | undefined {
    const html = this.entries.get(key)
    if (html !== undefined) {
      // 读取即视为最近使用
      this.entries.delete(key)
      this.entries.set(key, html)
    }
    return html
  }

  set(key: number, html: string): void {
    if (this.entries.has(key)) this.entries.delete(key)
    this.entries.set(key, html)
    while (this.entries.size > this.capacity) {
      const oldest = this.entries.keys().next().value
      if (oldest === undefined) break
      this.entries.delete(oldest)
    }
  }

  get size(): number {
    return this.entries.size
  }

  clear(): void {
    this.entries.clear()
  }
}

// InFlight 去重同一 chunk 的并发抓取。
export class InFlight {
  private pending = new Map<number, Promise<string>>()

  run(key: number, load: () => Promise<string>): Promise<string> {
    const existing = this.pending.get(key)
    if (existing) return existing
    const promise = load().finally(() => this.pending.delete(key))
    this.pending.set(key, promise)
    return promise
  }

  clear(): void {
    this.pending.clear()
  }
}

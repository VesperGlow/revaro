// 页 HTML 缓存：三页窗口只渲染 3 个节点，前后页提前预取到这里，
// 翻页热路径零网络。键 = 页面 URL（(spine, col) 寻址一经写入永久稳定；
// 渐进式分页里全局页码随快照漂移，URL 键保证缓存跨快照命中）。LRU 裁剪。
export class PageCache {
  private entries = new Map<string, string>()

  constructor(private capacity = 12) {}

  has(key: string): boolean {
    return this.entries.has(key)
  }

  get(key: string): string | undefined {
    const html = this.entries.get(key)
    if (html !== undefined) {
      // 读取即视为最近使用：正在阅读的页不被逐出
      this.entries.delete(key)
      this.entries.set(key, html)
    }
    return html
  }

  set(key: string, html: string): void {
    if (this.entries.has(key)) this.entries.delete(key) // 刷新最近使用
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

// InFlight 去重同一 URL 的并发抓取：预取与翻页兜底同时缺页时只发一次请求。
export class InFlight {
  private pending = new Map<string, Promise<string>>()

  run(key: string, load: () => Promise<string>): Promise<string> {
    const existing = this.pending.get(key)
    if (existing) return existing
    const promise = load().finally(() => this.pending.delete(key))
    this.pending.set(key, promise)
    return promise
  }
}

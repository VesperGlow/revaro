import type { Ref } from 'vue'
import { fetchChunk } from '../reader/api'
import { InFlight, PageCache } from '../reader/cache'
import type { ClientCacheManager } from '../reader/clientCache'
import { chunkForBlock, stableWindowRange } from '../reader/flow'
import { clamp } from '../reader/prefs'
import type { FlowManifest } from '../reader/types'

// useReaderWindow owns the reader's incremental DOM window and its L1/L2 chunk
// loading path. The caller still owns pagination and navigation; keeping those
// outside makes the stable-spine boundary an explicit contract at this seam.
export function useReaderWindow(
  fileId: string,
  flowEl: Ref<HTMLElement | null>,
  manifest: Ref<FlowManifest | null>,
  persistentCache: ClientCacheManager,
) {
  const aheadMargin = 3
  const chunkHTML = new PageCache(24)
  const chunkFlight = new InFlight()
  let first = 0
  let last = -1

  // Chunk reads preserve the existing L1 -> IndexedDB L2 -> network order.
  async function chunkText(index: number): Promise<string> {
    const hit = chunkHTML.get(index)
    if (hit !== undefined) return hit
    return chunkFlight.run(index, async () => {
      const current = manifest.value
      const bookKey = current?.book_key ?? ''
      const version = current?.version ?? 0
      if (bookKey) {
        const cached = await persistentCache.getChunk(bookKey, version, index)
        if (cached !== null) {
          chunkHTML.set(index, cached)
          return cached
        }
      }
      const html = await fetchChunk(fileId, index)
      chunkHTML.set(index, html)
      if (bookKey) void persistentCache.putChunk(bookKey, version, index, html)
      return html
    })
  }

  function chunkElement(index: number): HTMLElement | null {
    const flow = flowEl.value
    return flow ? (flow.querySelector(`.rf-chunk[data-chunk="${index}"]`) as HTMLElement | null) : null
  }

  // Change the window incrementally. Long stable-spine prefixes use bounded
  // concurrent reads and batched DOM insertion while retaining DOM and LRU order.
  async function ensureWindow(newFirst: number, newLast: number): Promise<void> {
    const flow = flowEl.value
    const current = manifest.value
    if (!flow || !current) return
    const finalChunk = Math.max(0, current.chunks.length - 1)
    newFirst = clamp(newFirst, 0, finalChunk)
    newLast = clamp(newLast, newFirst, finalChunk)
    if (last >= 0 && newFirst === first && newLast === last) return
    const existing = new Map<number, HTMLElement>()
    for (const child of Array.from(flow.children) as HTMLElement[]) {
      const index = Number(child.dataset.chunk)
      if (Number.isInteger(index)) existing.set(index, child)
    }
    if (existing.size) {
      const currentFirst = Math.min(...existing.keys())
      const currentLast = Math.max(...existing.keys())
      for (let i = currentFirst; i < newFirst; i++) existing.get(i)?.remove()
      for (let i = newLast + 1; i <= currentLast; i++) existing.get(i)?.remove()
    }

    const missing: number[] = []
    for (let i = newFirst; i <= newLast; i++) {
      if (!existing.has(i)) missing.push(i)
    }
    const loaded = new Map<number, string>()
    let nextMissing = 0
    async function loadMissing(): Promise<void> {
      while (nextMissing < missing.length) {
        const index = missing[nextMissing++]
        loaded.set(index, await chunkText(index))
      }
    }
    await Promise.all(Array.from({ length: Math.min(6, missing.length) }, loadMissing))
    if (flowEl.value !== flow) return

    // Concurrent completion is unordered. Re-touch in chunk order so the final
    // PageCache eviction result stays identical to the former serial path.
    for (const index of missing) chunkHTML.set(index, loaded.get(index)!)

    let cursor = 0
    while (cursor < missing.length) {
      const start = cursor
      while (cursor + 1 < missing.length && missing[cursor + 1] === missing[cursor] + 1) cursor++
      const end = cursor
      const fragment = document.createDocumentFragment()
      for (let i = start; i <= end; i++) {
        const index = missing[i]
        const section = document.createElement('div')
        section.className = 'rf-chunk'
        section.dataset.chunk = String(index)
        section.innerHTML = loaded.get(index)!
        fragment.appendChild(section)
      }
      let ref: HTMLElement | null = null
      for (let i = missing[end] + 1; i <= newLast; i++) {
        const next = flow.querySelector<HTMLElement>(`.rf-chunk[data-chunk="${i}"]`)
        if (next) {
          ref = next
          break
        }
      }
      flow.insertBefore(fragment, ref)
      cursor++
    }
    first = newFirst
    last = newLast
  }

  // A spine's start chunk remains the layout prefix; only the look-ahead end
  // moves with the target block.
  async function ensureWindowForBlock(block: number): Promise<boolean> {
    const current = manifest.value
    if (!current) return false
    const [newFirst, newLast] = stableWindowRange(current, block, aheadMargin)
    if (last >= 0 && newFirst === first && newLast === last) return false
    await ensureWindow(newFirst, newLast)
    return true
  }

  function lastChunkIndex(): number {
    const current = manifest.value
    return current && current.chunks.length ? current.chunks.length - 1 : -1
  }

  function prefetchSurrounding(block: number) {
    const current = manifest.value
    if (!current) return
    const center = chunkForBlock(current, block)
    if (center < 0) return
    for (let distance = 1; distance <= 2; distance++) {
      for (const index of [center + distance, center - distance]) {
        if (index >= 0 && index < current.chunks.length) void chunkText(index)
      }
    }
  }

  function reset() {
    chunkHTML.clear()
    chunkFlight.clear()
    flowEl.value?.replaceChildren()
    first = 0
    last = -1
  }

  return {
    chunkElement,
    ensureWindow,
    ensureWindowForBlock,
    get first() {
      return first
    },
    get last() {
      return last
    },
    lastChunkIndex,
    prefetchSurrounding,
    reset,
  }
}

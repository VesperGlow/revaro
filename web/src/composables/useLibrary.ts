import { computed, reactive, ref } from 'vue'
import { api } from '../api'
import { buildFolderTree, EMPTY_COUNTS, type LibraryCounts, type LibraryFolderNode, type LibraryItem, type LibraryType } from '../library'

type MediaType = 'book' | 'image' | 'video' | 'audio'
const MEDIA_TYPES: MediaType[] = ['book', 'image', 'video', 'audio']

interface LibraryAllResponse {
  items: Record<MediaType, LibraryItem[]>
  counts: LibraryCounts
}

// 分类栏数据源：一次 /api/library/all 扫描填充四个媒体大类，计数与路径树随之可用。
// 上传/删除后由 app-controller 调用 refresh() 失效重载。
export function useLibrary() {
  const counts = ref<LibraryCounts>({ ...EMPTY_COUNTS })
  const itemsByType = reactive<Record<MediaType, LibraryItem[]>>({ book: [], image: [], video: [], audio: [] })
  const activeType = ref<LibraryType | null>(null)
  const loading = ref(false)
  const error = ref('')
  const durations = reactive<Record<string, number>>({})
  let loaded = false
  let seq = 0

  const items = computed<LibraryItem[]>(() => {
    const type = activeType.value
    return type && type !== 'file' ? itemsByType[type] : []
  })

  const trees = computed<Partial<Record<MediaType, LibraryFolderNode>>>(() => {
    const out: Partial<Record<MediaType, LibraryFolderNode>> = {}
    for (const type of MEDIA_TYPES) if (itemsByType[type].length) out[type] = buildFolderTree(itemsByType[type])
    return out
  })

  async function loadAll(options: { force?: boolean } = {}) {
    if (loaded && !options.force) return
    const request = ++seq
    loading.value = true
    error.value = ''
    try {
      const data = await api<LibraryAllResponse>('/api/library/all')
      if (request !== seq) return
      for (const type of MEDIA_TYPES) {
        itemsByType[type] = data.items[type] || []
        for (const item of itemsByType[type]) if (item.duration_ms) durations[item.id] = item.duration_ms
      }
      counts.value = data.counts
      loaded = true
      ensureDurations(itemsByType.audio)
    } catch (e) {
      if (request === seq) error.value = (e as Error).message
    } finally {
      if (request === seq) loading.value = false
    }
  }

  async function loadType(type: LibraryType, options: { force?: boolean } = {}) {
    activeType.value = type
    if (type === 'file') return
    await loadAll(options)
  }

  // 音乐列表视图按需补齐时长；并发受限，失败静默。
  const durationQueue: LibraryItem[] = []
  let activeDurationFetches = 0
  function ensureDurations(list: LibraryItem[]) {
    for (const item of list) {
      if (durations[item.id] || item.duration_ms) continue
      if (durationQueue.some(queued => queued.id === item.id)) continue
      durationQueue.push(item)
    }
    pumpDurations()
  }

  function pumpDurations() {
    while (activeDurationFetches < 3 && durationQueue.length) {
      const item = durationQueue.shift()!
      activeDurationFetches++
      void api<{ duration: number }>(`/api/files/${item.id}/audio`)
        .then(data => { if (data.duration > 0) { item.duration_ms = Math.round(data.duration * 1000); durations[item.id] = item.duration_ms } })
        .catch(() => { /* 无法探测时保持 --:-- */ })
        .finally(() => { activeDurationFetches--; pumpDurations() })
    }
  }

  async function refresh() {
    await loadAll({ force: true })
  }

  return { counts, itemsByType, items, trees, activeType, loading, error, durations, loadAll, loadType, ensureDurations, refresh }
}

import type { DriveFile } from './api'

// 分类栏的五大类：书架、图片、视频、音乐、文件。
export type LibraryType = 'book' | 'image' | 'video' | 'audio' | 'file'

// 图片/视频共用的两种浏览方式；音乐与文件共用方块/列表两种视图。
export type GalleryMode = 'all' | 'albums'
export type ViewMode = 'grid' | 'list'

export const ROOT_FOLDER_ID = '00000000-0000-0000-0000-000000000000'

export interface LibraryFolderRef {
  id: string
  name: string
}

export interface LibraryItem extends DriveFile {
  folder_path: LibraryFolderRef[]
  duration_ms?: number
}

export interface LibraryCounts {
  book: number
  image: number
  video: number
  audio: number
  file: number
}

export interface LibraryResponse {
  type: LibraryType
  items: LibraryItem[]
  counts: LibraryCounts
}

export const EMPTY_COUNTS: LibraryCounts = { book: 0, image: 0, video: 0, audio: 0, file: 0 }

export interface LibraryCategory {
  type: LibraryType
  label: string
  hint: string
}

export const LIBRARY_CATEGORIES: LibraryCategory[] = [
  { type: 'book', label: '书架', hint: '以封面浏览 EPUB / TXT' },
  { type: 'image', label: '图片', hint: '全部图片与图库分类' },
  { type: 'video', label: '视频', hint: '全部视频与图库分类' },
  { type: 'audio', label: '音乐', hint: '方块或列表浏览音频' },
  { type: 'file', label: '文件', hint: '按目录管理全部文件' },
]

export function isLibraryType(value: string): value is LibraryType {
  return value === 'book' || value === 'image' || value === 'video' || value === 'audio' || value === 'file'
}

// 分类视图左栏「路径」节点的稳定 key：根目录用固定值。
export function folderKey(item: LibraryItem): string {
  return item.folder_path.length ? item.folder_path[item.folder_path.length - 1].id : ROOT_FOLDER_ID
}

export function folderLabel(item: LibraryItem): string {
  if (!item.folder_path.length) return '我的文件'
  return item.folder_path.map(folder => folder.name).filter(Boolean).join(' / ') || '我的文件'
}

export interface LibraryFolderNode {
  id: string
  name: string
  path: string
  count: number
  children: LibraryFolderNode[]
}

// 把每个条目所在的完整路径合并成一棵可展开的树；count 为该节点子树内的条目总数。
export function buildFolderTree(items: LibraryItem[]): LibraryFolderNode {
  const root: LibraryFolderNode = { id: ROOT_FOLDER_ID, name: '我的文件', path: '', count: 0, children: [] }
  const nodes = new Map<string, LibraryFolderNode>([[ROOT_FOLDER_ID, root]])
  for (const item of items) {
    const chain: LibraryFolderNode[] = [root]
    let parent = root
    let path = ''
    for (const folder of item.folder_path) {
      path = path ? `${path} / ${folder.name}` : folder.name
      let node = nodes.get(folder.id)
      if (!node) {
        node = { id: folder.id, name: folder.name, path, count: 0, children: [] }
        nodes.set(folder.id, node)
        parent.children.push(node)
      }
      chain.push(node)
      parent = node
    }
    for (const node of chain) node.count++
  }
  const sortNodes = (node: LibraryFolderNode) => {
    node.children.sort((a, b) => a.name.localeCompare(b.name, 'zh-Hans-CN'))
    node.children.forEach(sortNodes)
  }
  sortNodes(root)
  return root
}

export interface AlbumGroup {
  id: string
  title: string
  path: string
  items: LibraryItem[]
}

// 图库分类：按条目所在目录分组（手机相册的「相簿」），根目录归为一个相簿。
export function groupAlbums(items: LibraryItem[]): AlbumGroup[] {
  const albums = new Map<string, AlbumGroup>()
  for (const item of items) {
    const id = folderKey(item)
    let album = albums.get(id)
    if (!album) {
      const last = item.folder_path[item.folder_path.length - 1]
      album = { id, title: last?.name || '我的文件', path: folderLabel(item), items: [] }
      albums.set(id, album)
    }
    album.items.push(item)
  }
  return [...albums.values()].sort((a, b) => a.title.localeCompare(b.title, 'zh-Hans-CN'))
}

export interface BookSeries {
  key: string
  title: string
  items: LibraryItem[]
  volumes: number[]
}

const chineseDigits: Record<string, number> = { 零: 0, 〇: 0, 一: 1, 二: 2, 两: 2, 三: 3, 四: 4, 五: 5, 六: 6, 七: 7, 八: 8, 九: 9 }

function parseVolumeNumber(value: string): number | null {
  const trimmed = value.trim()
  if (!trimmed) return null
  if (/^\d+$/.test(trimmed)) return Number.parseInt(trimmed, 10)
  if (![...trimmed].every(char => char in chineseDigits || char === '十' || char === '百' || char === '千')) return null
  let total = 0
  let current = 0
  for (const char of trimmed) {
    if (char === '十' || char === '百' || char === '千') {
      const unit = char === '十' ? 10 : char === '百' ? 100 : 1000
      total += (current || 1) * unit
      current = 0
    } else {
      current = chineseDigits[char]
    }
  }
  return total + current
}

const volumePatterns: Array<{ pattern: RegExp; marker: boolean }> = [
  { pattern: /[\s\-_·]*第\s*([0-9零〇一二三四五六七八九十百千两]+)\s*[卷册部集篇季]\s*$/i, marker: true },
  { pattern: /[\s\-_·]*[（([【]\s*([0-9]+)\s*[）)\]】]\s*$/, marker: false },
  { pattern: /[\s\-_·]+(?:vol(?:ume)?|part|book|chapter)\.?\s*([0-9]+)\s*$/i, marker: true },
  { pattern: /[\s\-_·]*[Vv]\s*([0-9]{1,3})\s*$/, marker: false },
  { pattern: /[\s\-_·]+([0-9]{1,3})\s*$/, marker: false },
]

// 去掉书名末尾的卷册标记，返回系列名与卷号（无法识别时卷号为 null）。
export function bookSeriesInfo(name: string): { title: string; volume: number | null } {
  const base = name.replace(/\.[^.]+$/, '').trim()
  for (const { pattern } of volumePatterns) {
    const match = base.match(pattern)
    if (!match) continue
    const volume = parseVolumeNumber(match[1])
    const title = base.slice(0, match.index).replace(/[\s\-_·]+$/, '').trim()
    if (title) return { title, volume }
  }
  return { title: base || name, volume: null }
}

function seriesKey(title: string): string {
  return title.toLowerCase().replace(/[\s\-_·:：]+/g, '')
}

// 同系列书籍归组：只有包含两本及以上时才成为系列，其余保持单本展示。
export function groupBookSeries(items: LibraryItem[]): BookSeries[] {
  const groups = new Map<string, BookSeries>()
  for (const item of items) {
    const info = bookSeriesInfo(item.name)
    const key = seriesKey(info.title) || seriesKey(item.name)
    let group = groups.get(key)
    if (!group) {
      group = { key, title: info.title, items: [], volumes: [] }
      groups.set(key, group)
    }
    group.items.push(item)
    if (info.volume !== null) group.volumes.push(info.volume)
  }
  const result: BookSeries[] = []
  for (const group of groups.values()) {
    group.items.sort((a, b) => {
      const left = bookSeriesInfo(a.name).volume
      const right = bookSeriesInfo(b.name).volume
      if (left !== null && right !== null && left !== right) return left - right
      return a.name.localeCompare(b.name, 'zh-Hans-CN', { numeric: true })
    })
    group.volumes.sort((a, b) => a - b)
    result.push(group)
  }
  return result
}

// 系列内的单本如果不是同一系列，仍按普通书籍展示。
export function isSeries(group: BookSeries): boolean {
  return group.items.length > 1
}

export function sortByUpdatedDesc(items: LibraryItem[]): LibraryItem[] {
  return [...items].sort((a, b) => (b.updated_at || b.created_at).localeCompare(a.updated_at || a.created_at))
}

export function sortByName(items: LibraryItem[]): LibraryItem[] {
  return [...items].sort((a, b) => a.name.localeCompare(b.name, 'zh-Hans-CN', { numeric: true }))
}

export function formatDuration(ms?: number): string {
  if (!ms || ms <= 0) return '--:--'
  const total = Math.round(ms / 1000)
  const hours = Math.floor(total / 3600)
  const minutes = Math.floor((total % 3600) / 60)
  const seconds = total % 60
  if (hours > 0) return `${hours}:${String(minutes).padStart(2, '0')}:${String(seconds).padStart(2, '0')}`
  return `${minutes}:${String(seconds).padStart(2, '0')}`
}

export function libraryViewStorageKey(type: LibraryType, kind: 'gallery' | 'media'): string {
  return `revaro:library:${kind}:${type}`
}

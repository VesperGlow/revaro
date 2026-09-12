import { describe, expect, it } from 'vitest'
import type { DriveFile } from './api'
import { bookSeriesInfo, buildFolderTree, formatDuration, groupAlbums, groupBookSeries, type LibraryItem } from './library'

function item(name: string, folders: Array<[string, string]> = [], extra: Partial<DriveFile> = {}): LibraryItem {
  return {
    id: name + folders.map(folder => folder[0]).join('/'),
    parent_id: folders.length ? folders[folders.length - 1][0] : null,
    name,
    kind: 'file',
    size: 100,
    status: 'ready',
    created_at: '2024-01-01T00:00:00Z',
    updated_at: '2024-01-01T00:00:00Z',
    folder_path: folders.map(([id, folderName]) => ({ id, name: folderName })),
    ...extra,
  }
}

describe('book series detection', () => {
  it('strips volume markers in Chinese and Latin forms', () => {
    expect(bookSeriesInfo('星海拾遗 第03卷.epub')).toEqual({ title: '星海拾遗', volume: 3 })
    expect(bookSeriesInfo('星海拾遗 (2).epub')).toEqual({ title: '星海拾遗', volume: 2 })
    expect(bookSeriesInfo('星海拾遗 Vol.4.epub')).toEqual({ title: '星海拾遗', volume: 4 })
    expect(bookSeriesInfo('星海拾遗 5.epub')).toEqual({ title: '星海拾遗', volume: 5 })
    expect(bookSeriesInfo('星海拾遗 第三卷.epub')).toEqual({ title: '星海拾遗', volume: 3 })
    expect(bookSeriesInfo('独立短篇集.epub')).toEqual({ title: '独立短篇集', volume: null })
  })

  it('groups multiples into a series and orders by volume', () => {
    const series = groupBookSeries([item('星海拾遗 第2卷.epub'), item('星海拾遗 第1卷.epub'), item('独立短篇集.epub')])
    expect(series).toHaveLength(2)
    const grouped = series.find(group => group.items.length === 2)!
    expect(grouped.title).toBe('星海拾遗')
    expect(grouped.items.map(entry => entry.name)).toEqual(['星海拾遗 第1卷.epub', '星海拾遗 第2卷.epub'])
    expect(series.find(group => group.items.length === 1)!.title).toBe('独立短篇集')
  })
})

describe('folder tree', () => {
  it('counts every ancestor once and keeps the root label', () => {
    const tree = buildFolderTree([
      item('a.jpg', [['photos', 'Photos'], ['trips', 'Trips']]),
      item('b.jpg', [['photos', 'Photos']]),
      item('c.jpg'),
    ])
    expect(tree.count).toBe(3)
    const photos = tree.children.find(node => node.id === 'photos')!
    expect(photos.count).toBe(2)
    expect(photos.path).toBe('Photos')
    expect(photos.children[0].count).toBe(1)
    expect(photos.children[0].path).toBe('Photos / Trips')
  })
})

describe('album grouping', () => {
  it('groups by immediate folder and labels the root album', () => {
    const albums = groupAlbums([item('a.jpg', [['photos', 'Photos']]), item('b.jpg', [['photos', 'Photos']]), item('c.jpg')])
    expect(albums.map(album => album.title).sort()).toEqual(['Photos', '我的文件'])
    expect(albums.find(album => album.title === 'Photos')!.items).toHaveLength(2)
  })
})

describe('duration formatting', () => {
  it('renders minutes and hours', () => {
    expect(formatDuration(0)).toBe('--:--')
    expect(formatDuration(59_000)).toBe('0:59')
    expect(formatDuration(3_723_000)).toBe('1:02:03')
  })
})

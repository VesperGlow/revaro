import type { BookProgress, LayoutManifest, LayoutProfile, LayoutStatus } from './types'
import { api } from '../api'

export interface BookInfo {
  id: string
  title: string
  kind: 'epub' | 'txt'
  hasCover: boolean
}

export async function fetchBookInfo(fileId: string): Promise<BookInfo | null> {
  const data = await api<{ format?: string; title?: string; name?: string; cover?: boolean }>(`/api/files/${fileId}/book`)
  return {
    id: fileId,
    title: data.name || data.title || '未命名书籍',
    kind: data.format === 'txt' ? 'txt' : 'epub',
    hasCover: Boolean(data.cover),
  }
}

export async function fetchProgress(fileId: string): Promise<BookProgress> {
  return api<BookProgress>(`/api/files/${fileId}/book/progress`)
}

// saveProgress 写 readingAnchor + profile（跨 layout 稳定的阅读位置）；
// page/total_pages 仅作为旧实现过渡期的显示数据，旧实现删除时一并移除。
export async function saveProgress(fileId: string, progress: BookProgress): Promise<void> {
  await api(`/api/files/${fileId}/book/progress`, {
    method: 'PUT',
    body: JSON.stringify(progress),
  })
}

export async function submitProfile(
  fileId: string,
  profile: LayoutProfile,
  startAnchor: { spine: number; path: number[]; offset: number } | null = null,
): Promise<LayoutStatus> {
  return api<LayoutStatus>(`/api/files/${fileId}/book/layouts`, {
    method: 'POST',
    body: JSON.stringify({ profile, start_anchor: startAnchor }),
  })
}

export async function fetchLayoutStatus(fileId: string, profileId: string): Promise<LayoutStatus> {
  return api<LayoutStatus>(`/api/files/${fileId}/book/layouts/${encodeURIComponent(profileId)}`)
}

export async function fetchManifest(fileId: string, profileId: string): Promise<LayoutManifest> {
  return api<LayoutManifest>(`/api/files/${fileId}/book/layouts/${encodeURIComponent(profileId)}/manifest`)
}

// fetchPageByURL 拉取一页固定 HTML（非 JSON）。URL 来自 manifest 的
// PageMeta.url：(spine, col) 寻址，一经写入永久稳定。
export async function fetchPageByURL(url: string): Promise<string> {
  const response = await fetch(url, { credentials: 'same-origin' })
  if (!response.ok) {
    let message = `页面加载失败 (${response.status})`
    try {
      const payload = (await response.json()) as { error?: { message?: string } }
      message = payload.error?.message || message
    } catch {
      /* ignore */
    }
    throw new Error(message)
  }
  return response.text()
}

// LayoutSuperseded：新一轮排版请求（用户又改了字号）使旧等待作废。
export class LayoutSuperseded extends Error {}

const sleep = (ms: number) => new Promise(resolve => setTimeout(resolve, ms))

// waitForReadable 轮询到「可读」为止：manifest 快照存在且（complete 或
// 已含 anchor 所在页）。渐进式分页下无需等全书生成完——目标章完成即可读。
export async function waitForReadable(
  fileId: string,
  profileId: string,
  anchor: { spine: number; path: number[]; offset: number } | null,
  opts: {
    intervalMs?: number
    onProgress?: (status: LayoutStatus) => void
    aborted?: () => boolean
    pageForAnchor?: (m: LayoutManifest, a: { spine: number; path: number[]; offset: number }) => number
  } = {},
): Promise<LayoutManifest> {
  const interval = opts.intervalMs ?? 400
  for (;;) {
    if (opts.aborted?.()) throw new LayoutSuperseded('layout request superseded')
    const status = await fetchLayoutStatus(fileId, profileId)
    opts.onProgress?.(status)
    if (status.status === 'error') throw new Error(status.error || '排版失败')
    const m = await fetchManifest(fileId, profileId).catch(() => null)
    if (m) {
      if (m.complete) return m
      if (anchor && opts.pageForAnchor) {
        if (opts.pageForAnchor(m, anchor) < m.pages.length) return m
      } else if (m.pages.length > 0) {
        return m
      }
    }
    await sleep(interval)
  }
}

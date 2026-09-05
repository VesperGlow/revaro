import type { BookProgress, FlowManifest } from './types'
import { api } from '../api'

export async function fetchBookTitle(fileId: string): Promise<string> {
  const data = await api<{ title?: string; name?: string }>(`/api/files/${fileId}/book`)
  return data.name || data.title || '未命名书籍'
}

// fetchFlow 拉取连续 reading flow 的 manifest（chunk/spine/TOC 元数据）。
// 服务端 manifest 用 no-cache：总是与当前二进制一致。
export async function fetchFlow(fileId: string): Promise<FlowManifest> {
  return api<FlowManifest>(`/api/files/${fileId}/book/flow`, undefined, 120_000)
}

export async function fetchProgress(fileId: string): Promise<BookProgress> {
  return api<BookProgress>(`/api/files/${fileId}/book/progress`)
}

// saveProgress 写 readingAnchor（跨字号/横竖屏/客户端分页稳定）。
export async function saveProgress(fileId: string, progress: BookProgress): Promise<void> {
  await api(`/api/files/${fileId}/book/progress`, {
    method: 'PUT',
    body: JSON.stringify(progress),
  })
}

// fetchChunk 拉取一个 flow chunk 的 HTML 片段（非 JSON，immutable 缓存）。
export async function fetchChunk(fileId: string, index: number): Promise<string> {
  const response = await fetch(`/api/files/${fileId}/book/flow/chunks/${index}`, { credentials: 'same-origin' })
  if (!response.ok) {
    let message = `内容片段加载失败 (${response.status})`
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

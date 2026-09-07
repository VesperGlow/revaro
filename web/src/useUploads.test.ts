import { afterEach, describe, expect, it, vi } from 'vitest'
import { ref } from 'vue'
import { api } from './api'
import { useUploads } from './composables/useUploads'
import type { UploadTask } from './types'

vi.mock('./api', () => ({ api: vi.fn() }))

afterEach(() => { vi.restoreAllMocks(); vi.unstubAllGlobals(); vi.mocked(api).mockReset() })

function setup(saved: unknown, quotaExceeded = false) {
  const tasks: UploadTask[] = []
  const progress: number[] = []
  vi.stubGlobal('window', globalThis)
  vi.stubGlobal('localStorage', {
    getItem: () => JSON.stringify(saved),
    setItem: () => { if (quotaExceeded) throw new Error('quota exceeded') },
  })
  class UploadRequest {
    upload = { onprogress: (_event: unknown) => {} }
    status = 200
    onload = () => {}
    open() {}
    setRequestHeader() {}
    getResponseHeader() { return 'etag' }
    send(body: Blob) {
      queueMicrotask(() => {
        this.upload.onprogress({ lengthComputable: true, loaded: body.size })
        progress.push(tasks[0].progress)
        this.onload()
      })
    }
  }
  vi.stubGlobal('XMLHttpRequest', UploadRequest)
  const uploads = useUploads({ tasks, currentId: ref('root'), dragActive: ref(false), trashMode: ref(false), fileInput: ref(null), folderInput: ref(null), notify: vi.fn(), openFolder: vi.fn().mockResolvedValue(undefined) })
  return { tasks, progress, uploads }
}

describe('upload reliability', () => {
  it('uploads when saved state is malformed and local storage writes fail', async () => {
    const { tasks, uploads } = setup({}, true)
    vi.mocked(api).mockImplementation(async (path) => {
      if (path === '/api/uploads') return { upload_id: 'new', mode: 'single', url: 'https://s3.test/put' } as never
      return {} as never
    })
    uploads.acceptFiles([new File(['abc'], 'file.txt')])
    await vi.waitFor(() => expect(tasks[0].status).toBe('done'))
    uploads.disposeUploads()
  })

  it('counts acknowledged bytes and keeps resumed multipart progress finite', async () => {
    const file = new File(['abcdefghijkl'], 'file.bin', { lastModified: 123 })
    const { tasks, progress, uploads } = setup([{ uploadId: 'resumed', parentId: 'root', name: file.name, size: file.size, lastModified: file.lastModified }])
    vi.mocked(api).mockImplementation(async (path) => {
      if (path === '/api/uploads/resumed') return { upload_id: 'resumed', mode: 'multipart', part_size: 4, part_count: 3, parts: [{ part_number: 1, etag: 'one' }, { part_number: 2, etag: 'two' }] } as never
      if (path === '/api/uploads/resumed/parts') return { parts: [{ part_number: 3, url: 'https://s3.test/part' }] } as never
      return {} as never
    })
    uploads.acceptFiles([file])
    await vi.waitFor(() => expect(tasks[0].status).toBe('done'))
    expect(progress).toHaveLength(1)
    expect(Number.isFinite(progress[0])).toBe(true)
    expect(progress[0]).toBeGreaterThan(90)
    const completion=vi.mocked(api).mock.calls.find(([path])=>path.endsWith('/complete'))
    expect(completion?.[2]).toBe(0)
    expect(completion?.[1]?.signal).toBeInstanceOf(AbortSignal)
    uploads.disposeUploads()
  })
  it('retains resumable upload state during temporary API failures', async () => {
    const file = new File(['abc'], 'file.bin', { lastModified: 123 })
    const { tasks, uploads } = setup([{ uploadId: 'resumed', parentId: 'root', name: file.name, size: file.size, lastModified: file.lastModified }])
    vi.mocked(api).mockRejectedValue(Object.assign(new Error('unavailable'), { status: 503 }))
    uploads.acceptFiles([file])
    await vi.waitFor(() => expect(tasks[0].status).toBe('failed'))
    expect(tasks[0].uploadId).toBe('resumed')
    expect(api).toHaveBeenCalledTimes(1)
    uploads.disposeUploads()
  })

})

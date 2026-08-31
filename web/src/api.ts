export interface DriveFile { id:string; parent_id:string|null; name:string; kind:'file'|'directory'; size:number; mime_type?:string; etag?:string; status:'pending'|'ready'|'deleting'|'failed'; created_at:string; updated_at:string; deleted_at?:string; restore_parent_id?:string; has_cover?:boolean }

export interface ApiError extends Error { status?:number; data?:unknown }

// 默认 60s 超时：网络挂起时请求会 abort 而不是永久 pending。
// 大文件经 presigned S3 multipart URL 直传，不经过这里。
export async function api<T>(path:string, init:RequestInit = {}, timeoutMs = 60000):Promise<T>{
  const headers = new Headers(init.headers)
  if (init.body && !headers.has('Content-Type')) headers.set('Content-Type','application/json')
  const controller = new AbortController()
  const timer = window.setTimeout(() => controller.abort(), timeoutMs)
	const signal = init.signal ? AbortSignal.any([init.signal, controller.signal]) : controller.signal
  try {
    const response = await fetch(path, { ...init, headers, signal, credentials:'same-origin' })
    if (!response.ok) {
      let message = `请求失败 (${response.status})`
      let payload: unknown = null
      try { payload = await response.json(); message = (payload as {error?:{message?:string}}).error?.message || message } catch { /* ignore */ }
      const error = new Error(message) as ApiError; error.status=response.status; error.data=payload; throw error
    }
    if (response.status === 204) return undefined as T
    return response.json() as Promise<T>
  } finally {
    window.clearTimeout(timer)
  }
}

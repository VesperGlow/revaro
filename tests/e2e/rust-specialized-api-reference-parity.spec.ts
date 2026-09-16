import { readFileSync } from 'node:fs'
import { expect, request as createRequest, test, type APIRequestContext } from '@playwright/test'

const ROOT = '00000000-0000-0000-0000-000000000000'
const MISSING = '0190f8f0-1c2b-7c3d-9e4f-5a6b7c8d9e0f'

function crc32(data: Buffer) {
  let value = 0xffffffff
  for (const byte of data) {
    value ^= byte
    for (let bit = 0; bit < 8; bit += 1) {
      value = (value >>> 1) ^ (value & 1 ? 0xedb88320 : 0)
    }
  }
  return value ^ 0xffffffff
}

function zip(entries: Array<[string, Buffer]>) {
  const local: Buffer[] = []
  const central: Buffer[] = []
  let offset = 0

  for (const [name, data] of entries) {
    const nameBytes = Buffer.from(name)
    const checksum = crc32(data) >>> 0
    const localHeader = Buffer.alloc(30)
    localHeader.writeUInt32LE(0x04034b50, 0)
    localHeader.writeUInt16LE(20, 4)
    localHeader.writeUInt16LE(0x800, 6)
    localHeader.writeUInt32LE(checksum, 14)
    localHeader.writeUInt32LE(data.length, 18)
    localHeader.writeUInt32LE(data.length, 22)
    localHeader.writeUInt16LE(nameBytes.length, 26)
    local.push(Buffer.concat([localHeader, nameBytes, data]))

    const centralHeader = Buffer.alloc(46)
    centralHeader.writeUInt32LE(0x02014b50, 0)
    centralHeader.writeUInt16LE(20, 4)
    centralHeader.writeUInt16LE(20, 6)
    centralHeader.writeUInt16LE(0x800, 8)
    centralHeader.writeUInt32LE(checksum, 16)
    centralHeader.writeUInt32LE(data.length, 20)
    centralHeader.writeUInt32LE(data.length, 24)
    centralHeader.writeUInt16LE(nameBytes.length, 28)
    centralHeader.writeUInt32LE(offset, 42)
    central.push(Buffer.concat([centralHeader, nameBytes]))
    offset += localHeader.length + nameBytes.length + data.length
  }

  const centralDirectory = Buffer.concat(central)
  const end = Buffer.alloc(22)
  end.writeUInt32LE(0x06054b50, 0)
  end.writeUInt16LE(entries.length, 8)
  end.writeUInt16LE(entries.length, 10)
  end.writeUInt32LE(centralDirectory.length, 12)
  end.writeUInt32LE(offset, 16)
  return Buffer.concat([...local, centralDirectory, end])
}

function epub() {
  const container = '<?xml version="1.0" encoding="UTF-8"?><container xmlns="urn:oasis:names:tc:opendocument:xmlns:container" version="1.0"><rootfiles><rootfile full-path="OEBPS/content.opf" media-type="application/oebps-package+xml"/></rootfiles></container>'
  const opf = '<?xml version="1.0" encoding="UTF-8"?><package xmlns="http://www.idpf.org/2007/opf" version="3.0"><metadata xmlns:dc="http://purl.org/dc/elements/1.1/"><dc:title>API 专项验收</dc:title></metadata><manifest><item id="chapter" href="chapter.xhtml" media-type="application/xhtml+xml"/></manifest><spine><itemref idref="chapter"/></spine></package>'
  const chapter = '<html xmlns="http://www.w3.org/1999/xhtml"><body><h1>API 专项验收</h1><p>reader endpoint parity</p></body></html>'
  return zip([
    ['mimetype', Buffer.from('application/epub+zip')],
    ['META-INF/container.xml', Buffer.from(container)],
    ['OEBPS/content.opf', Buffer.from(opf)],
    ['OEBPS/chapter.xhtml', Buffer.from(chapter)],
  ])
}

function wav() {
  const sampleRate = 8_000
  const samples = sampleRate
  const buffer = Buffer.alloc(44 + samples * 2)
  buffer.write('RIFF')
  buffer.writeUInt32LE(36 + samples * 2, 4)
  buffer.write('WAVEfmt ', 8)
  buffer.writeUInt32LE(16, 16)
  buffer.writeUInt16LE(1, 20)
  buffer.writeUInt16LE(1, 22)
  buffer.writeUInt32LE(sampleRate, 24)
  buffer.writeUInt32LE(sampleRate * 2, 28)
  buffer.writeUInt16LE(2, 32)
  buffer.writeUInt16LE(16, 34)
  buffer.write('data', 36)
  buffer.writeUInt32LE(samples * 2, 40)
  return buffer
}

const video = readFileSync(new URL('./fixtures/preview.webm', import.meta.url))

function headers(baseUrl: string, json = false) {
  return json
    ? { Origin: baseUrl, 'Content-Type': 'application/json' }
    : { Origin: baseUrl }
}

async function login(baseUrl: string) {
  const client = await createRequest.newContext({ baseURL: baseUrl })
  const response = await client.post('/api/auth/login', {
    headers: headers(baseUrl, true),
    data: { username: 'admin', password: 'revaro-e2e-password' },
  })
  expect(response.status(), `${baseUrl} 登录失败`).toBe(200)
  return client
}

async function upload(
  client: APIRequestContext,
  baseUrl: string,
  name: string,
  mimeType: string,
  body: Buffer,
) {
  const created = await client.post('/api/uploads', {
    headers: headers(baseUrl, true),
    data: { parent_id: ROOT, name, size: body.length, mime_type: mimeType },
  })
  expect(created.status(), `${baseUrl} 创建 ${name} upload session`).toBe(201)
  const session = await created.json() as {
    upload_id: string
    file_id: string
    url: string
    mode: string
    part_size: number
    part_count: number
  }
  expect(session.mode).toBe('single')
  expect(session.part_count).toBe(0)
  expect(session.url).toBe(`/api/uploads/${session.upload_id}/data`)

  const status = await client.get(`/api/uploads/${session.upload_id}`, { headers: headers(baseUrl) })
  expect(status.status()).toBe(200)
  const statusBody = await status.json() as Record<string, unknown>
  expect(statusBody).toMatchObject({
    upload_id: session.upload_id,
    file_id: session.file_id,
    mode: 'single',
    url: session.url,
    expected_size: body.length,
    mime_type: mimeType,
    status: 'pending',
    parts: [],
  })

  const bytes = await client.put(session.url, {
    headers: { ...headers(baseUrl), 'Content-Type': mimeType },
    data: body,
  })
  expect(bytes.status()).toBe(204)
  expect(bytes.headers()['etag']).toBeTruthy()

  const completed = await client.post(`/api/uploads/${session.upload_id}/complete`, {
    headers: headers(baseUrl, true),
    data: { parts: [] },
  })
  expect(completed.status(), `${baseUrl} 完成 ${name} upload`).toBe(200)
  const file = await completed.json() as Record<string, unknown>
  expect(file).toMatchObject({
    id: session.file_id,
    parent_id: ROOT,
    name,
    kind: 'file',
    size: body.length,
    mime_type: mimeType,
    status: 'ready',
  })
  return { file, uploadId: session.upload_id }
}

async function json(client: APIRequestContext, baseUrl: string, path: string, method = 'GET', data?: unknown) {
  const response = await client.fetch(path, {
    method,
    headers: headers(baseUrl, data !== undefined),
    data,
  })
  const text = await response.text()
  return {
    status: response.status(),
    contentType: response.headers()['content-type']?.split(';')[0],
    value: JSON.parse(text) as Record<string, unknown>,
  }
}

async function requestResult(
  client: APIRequestContext,
  baseUrl: string,
  path: string,
  method = 'GET',
  data?: unknown,
) {
  const response = await client.fetch(path, {
    method,
    headers: headers(baseUrl, data !== undefined),
    data,
  })
  const text = await response.text()
  return {
    status: response.status(),
    contentType: response.headers()['content-type']?.split(';')[0],
    value: text ? JSON.parse(text) as Record<string, unknown> : null,
  }
}

function fileSemantics(file: Record<string, unknown>) {
  return {
    parent_id: file.parent_id,
    name: file.name,
    kind: file.kind,
    size: file.size,
    mime_type: file.mime_type,
    status: file.status,
    has_etag: typeof file.etag === 'string',
    has_content_hash: typeof file.content_hash === 'string',
    hash_algorithm: file.hash_algorithm,
  }
}

function taskSemantics(task: Record<string, unknown>) {
  return {
    type: task.type,
    status: task.status,
    phase: task.phase,
    progress: task.progress,
    speed: task.speed,
    retry_count: task.retry_count,
    max_retries: task.max_retries,
    error: task.error,
    source_type: task.source_type,
    cancel_requested: task.cancel_requested,
    name: task.name,
  }
}

async function waitForUploadTask(client: APIRequestContext, uploadId: string) {
  await expect.poll(async () => {
    const response = await client.get('/api/tasks')
    if (!response.ok()) return null
    const payload = await response.json() as { items?: Array<Record<string, unknown>> }
    const task = (payload.items ?? []).find(item => item.source_id === uploadId)
    return task?.status === 'completed' ? task : null
  }, { timeout: 10_000 }).not.toBeNull()
  const response = await client.get('/api/tasks')
  const payload = await response.json() as { items?: Array<Record<string, unknown>> }
  const task = (payload.items ?? []).find(item => item.source_id === uploadId)
  if (!task?.id) throw new Error(`upload task ${uploadId} is missing`)
  return String(task.id)
}

async function cleanup(client: APIRequestContext, ids: string[], uploadIds: string[]) {
  for (const uploadId of uploadIds) {
    await client.delete(`/api/uploads/${uploadId}`).catch(() => undefined)
  }
  for (const id of ids) {
    await client.delete(`/api/files/${id}`).catch(() => undefined)
    await client.delete(`/api/trash/${id}`).catch(() => undefined)
  }
}

test('旧版专用媒体、阅读器、任务和 upload 成功 API 在 Rust 版保持可达', async () => {
  const oldUrl = process.env.E2E_REFERENCE_URL || 'http://127.0.0.1:18080'
  const newUrl = process.env.E2E_NEW_URL || 'http://127.0.0.1:18084'
  const suffix = crypto.randomUUID().slice(0, 8)
  const fixtures = [
    { name: `api-audio-${suffix}.wav`, mime: 'audio/wav', body: wav() },
    { name: `api-video-${suffix}.webm`, mime: 'video/webm', body: video },
    { name: `api-book-${suffix}.epub`, mime: 'application/epub+zip', body: epub() },
  ] as const
  const oldClient = await login(oldUrl)
  const newClient = await login(newUrl)
  const oldCreated: Array<{ file: Record<string, unknown>; uploadId: string }> = []
  const newCreated: Array<{ file: Record<string, unknown>; uploadId: string }> = []

  try {
    for (const fixture of fixtures) {
      oldCreated.push(await upload(oldClient, oldUrl, fixture.name, fixture.mime, fixture.body))
      newCreated.push(await upload(newClient, newUrl, fixture.name, fixture.mime, fixture.body))
    }

    expect(newCreated.map(({ file }) => fileSemantics(file)))
      .toEqual(oldCreated.map(({ file }) => fileSemantics(file)))

    const [oldAudio, newAudio] = await Promise.all([
      json(oldClient, oldUrl, `/api/files/${oldCreated[0].file.id}/audio`),
      json(newClient, newUrl, `/api/files/${newCreated[0].file.id}/audio`),
    ])
    expect(newAudio.status).toBe(oldAudio.status)
    expect(newAudio.contentType).toBe(oldAudio.contentType)
    expect(newAudio.value).toEqual(oldAudio.value)

    const [oldVideo, newVideo] = await Promise.all([
      json(oldClient, oldUrl, `/api/files/${oldCreated[1].file.id}/video`),
      json(newClient, newUrl, `/api/files/${newCreated[1].file.id}/video`),
    ])
    expect(newVideo.status).toBe(oldVideo.status)
    expect(newVideo.contentType).toBe(oldVideo.contentType)
    expect(newVideo.value).toEqual(oldVideo.value)

    const [oldBook, newBook] = await Promise.all([
      json(oldClient, oldUrl, `/api/files/${oldCreated[2].file.id}/book`),
      json(newClient, newUrl, `/api/files/${newCreated[2].file.id}/book`),
    ])
    expect(newBook.status).toBe(oldBook.status)
    expect(newBook.contentType).toBe(oldBook.contentType)
    expect(newBook.value).toEqual(oldBook.value)

    const [oldReanalyze, newReanalyze] = await Promise.all([
      json(oldClient, oldUrl, `/api/files/${oldCreated[0].file.id}/media/reanalyze`, 'POST'),
      json(newClient, newUrl, `/api/files/${newCreated[0].file.id}/media/reanalyze`, 'POST'),
    ])
    expect(newReanalyze.status).toBe(oldReanalyze.status)
    expect(newReanalyze.contentType).toBe(oldReanalyze.contentType)
    expect(newReanalyze.value).toEqual(oldReanalyze.value)

    const [oldTaskId, newTaskId] = await Promise.all([
      waitForUploadTask(oldClient, oldCreated[0].uploadId),
      waitForUploadTask(newClient, newCreated[0].uploadId),
    ])
    const [oldTask, newTask] = await Promise.all([
      json(oldClient, oldUrl, `/api/tasks/${oldTaskId}`),
      json(newClient, newUrl, `/api/tasks/${newTaskId}`),
    ])
    expect(newTask.status).toBe(oldTask.status)
    expect(newTask.contentType).toBe(oldTask.contentType)
    expect(taskSemantics(newTask.value)).toEqual(taskSemantics(oldTask.value))
  } finally {
    await Promise.all([
      cleanup(oldClient, oldCreated.map(({ file }) => String(file.id)), oldCreated.map(({ uploadId }) => uploadId)),
      cleanup(newClient, newCreated.map(({ file }) => String(file.id)), newCreated.map(({ uploadId }) => uploadId)),
    ])
    await Promise.all([oldClient.dispose(), newClient.dispose()])
  }
})

test('旧版媒体进度 API 的 JSON 解码和文件查找顺序在 Rust 版保持一致', async () => {
  const oldUrl = process.env.E2E_REFERENCE_URL || 'http://127.0.0.1:18080'
  const newUrl = process.env.E2E_NEW_URL || 'http://127.0.0.1:18084'
  const suffix = crypto.randomUUID().slice(0, 8)
  const oldClient = await login(oldUrl)
  const newClient = await login(newUrl)
  let oldCreated: { file: Record<string, unknown>; uploadId: string } | undefined
  let newCreated: { file: Record<string, unknown>; uploadId: string } | undefined

  try {
    ;[oldCreated, newCreated] = await Promise.all([
      upload(oldClient, oldUrl, `media-progress-${suffix}.wav`, 'audio/wav', wav()),
      upload(newClient, newUrl, `media-progress-${suffix}.wav`, 'audio/wav', wav()),
    ])
    const oldId = String(oldCreated.file.id)
    const newId = String(newCreated.file.id)
    const cases: Array<[string, unknown, number, Record<string, unknown>?, boolean?]> = [
      ['缺省字段', {}, 200],
      ['未知字段', { position: 1, duration: 2, extra: true }, 400, { error: { status: 400, message: 'invalid JSON request' } }],
      ['malformed', Buffer.from('{"position":'), 400, { error: { status: 400, message: 'invalid JSON request' } }],
      ['缺失文件 malformed', Buffer.from('{"position":'), 404, { error: { status: 404, message: 'ready media file not found' } }, true],
      ['缺省 position', { duration: 2 }, 200],
      ['显式 null 数值', { position: null, duration: null }, 200],
      ['非法值', { position: 99, duration: 2 }, 400, { error: { status: 400, message: 'media progress values are invalid' } }],
    ]
    for (const [label, data, expectedStatus, expectedError, missing] of cases) {
      const [oldResult, newResult] = await Promise.all([
        json(oldClient, oldUrl, `/api/files/${missing ? MISSING : oldId}/media/progress`, 'PUT', data),
        json(newClient, newUrl, `/api/files/${missing ? MISSING : newId}/media/progress`, 'PUT', data),
      ])
      expect(newResult.status, `Rust ${label} 状态与 reference 不一致`).toBe(oldResult.status)
      expect(newResult.contentType, `Rust ${label} Content-Type 与 reference 不一致`).toBe(oldResult.contentType)
      expect(oldResult.status, `reference ${label} 状态异常`).toBe(expectedStatus)
      if (expectedError) {
        expect(oldResult.value, `reference ${label} 错误 envelope 异常`).toEqual(expectedError)
        expect(newResult.value, `Rust ${label} 错误 envelope 与 reference 不一致`).toEqual(oldResult.value)
      } else {
        expect(newResult.value.position, `Rust ${label} position 与 reference 不一致`).toBe(oldResult.value.position)
        expect(newResult.value.duration, `Rust ${label} duration 与 reference 不一致`).toBe(oldResult.value.duration)
        expect(typeof newResult.value.updated_at, `Rust ${label} updated_at 类型不一致`).toBe(typeof oldResult.value.updated_at)
      }
    }
  } finally {
    await Promise.all([
      oldCreated
        ? cleanup(oldClient, [String(oldCreated.file.id)], [oldCreated.uploadId])
        : Promise.resolve(),
      newCreated
        ? cleanup(newClient, [String(newCreated.file.id)], [newCreated.uploadId])
        : Promise.resolve(),
    ])
    await Promise.all([oldClient.dispose(), newClient.dispose()])
  }
})

test('旧版阅读进度 API 的 JSON 解码和文件查找顺序在 Rust 版保持一致', async () => {
  const oldUrl = process.env.E2E_REFERENCE_URL || 'http://127.0.0.1:18080'
  const newUrl = process.env.E2E_NEW_URL || 'http://127.0.0.1:18084'
  const suffix = crypto.randomUUID().slice(0, 8)
  const oldClient = await login(oldUrl)
  const newClient = await login(newUrl)
  let oldCreated: { file: Record<string, unknown>; uploadId: string } | undefined
  let newCreated: { file: Record<string, unknown>; uploadId: string } | undefined

  try {
    ;[oldCreated, newCreated] = await Promise.all([
      upload(oldClient, oldUrl, `book-progress-${suffix}.epub`, 'application/epub+zip', epub()),
      upload(newClient, newUrl, `book-progress-${suffix}.epub`, 'application/epub+zip', epub()),
    ])
    const oldId = String(oldCreated.file.id)
    const newId = String(newCreated.file.id)
    const cases: Array<[string, unknown, number, Record<string, unknown>?, boolean?]> = [
      ['缺省字段', {}, 204],
      ['未知字段', { anchor: null, extra: true }, 400, { error: { status: 400, message: 'invalid JSON request' } }],
      ['malformed', Buffer.from('{"anchor":'), 400, { error: { status: 400, message: 'invalid JSON request' } }],
      ['缺失文件 malformed', Buffer.from('{"anchor":'), 404, { error: { status: 404, message: 'ready file not found' } }, true],
      ['null', Buffer.from('null'), 204],
    ]
    for (const [label, data, expectedStatus, expectedError, missing] of cases) {
      const [oldResult, newResult] = await Promise.all([
        requestResult(oldClient, oldUrl, `/api/files/${missing ? MISSING : oldId}/book/progress`, 'PUT', data),
        requestResult(newClient, newUrl, `/api/files/${missing ? MISSING : newId}/book/progress`, 'PUT', data),
      ])
      expect(newResult.status, `Rust ${label} 状态与 reference 不一致`).toBe(oldResult.status)
      expect(newResult.contentType, `Rust ${label} Content-Type 与 reference 不一致`).toBe(oldResult.contentType)
      expect(oldResult.status, `reference ${label} 状态异常`).toBe(expectedStatus)
      if (expectedError) {
        expect(oldResult.value, `reference ${label} 错误 envelope 异常`).toEqual(expectedError)
        expect(newResult.value, `Rust ${label} 错误 envelope 与 reference 不一致`).toEqual(oldResult.value)
      } else {
        expect(newResult.value, `Rust ${label} 不应返回响应体`).toBeNull()
        expect(oldResult.value, `reference ${label} 不应返回响应体`).toBeNull()
      }
    }
  } finally {
    await Promise.all([
      oldCreated
        ? cleanup(oldClient, [String(oldCreated.file.id)], [oldCreated.uploadId])
        : Promise.resolve(),
      newCreated
        ? cleanup(newClient, [String(newCreated.file.id)], [newCreated.uploadId])
        : Promise.resolve(),
    ])
    await Promise.all([oldClient.dispose(), newClient.dispose()])
  }
})

import { inflateRawSync } from 'node:zlib'
import { expect, request as createRequest, test, type APIRequestContext } from '@playwright/test'

const ROOT = '00000000-0000-0000-0000-000000000000'
const MISSING = '0190f8f0-1c2b-7c3d-9e4f-5a6b7c8d9e0f'

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

async function createDirectory(client: APIRequestContext, baseUrl: string, name: string) {
  const response = await client.post('/api/directories', {
    headers: headers(baseUrl, true),
    data: { parent_id: ROOT, name },
  })
  expect(response.status(), `${baseUrl} 创建目录失败`).toBe(201)
  const file = await response.json() as { id: string }
  return file.id
}

async function upload(
  client: APIRequestContext,
  baseUrl: string,
  parentId: string,
  name: string,
  body: Buffer,
) {
  const created = await client.post('/api/uploads', {
    headers: headers(baseUrl, true),
    data: { parent_id: parentId, name, size: body.length, mime_type: 'text/plain' },
  })
  expect(created.status(), `${baseUrl} 创建上传会话失败`).toBe(201)
  const session = await created.json() as { upload_id: string; file_id: string; url: string; mode: string }
  expect(session.mode).toBe('single')
  const data = await client.put(session.url, {
    headers: { ...headers(baseUrl), 'Content-Type': 'text/plain' },
    data: body,
  })
  expect(data.status(), `${baseUrl} 上传文件失败`).toBe(204)
  const completed = await client.post(`/api/uploads/${session.upload_id}/complete`, {
    headers: headers(baseUrl, true),
    data: { parts: [] },
  })
  expect(completed.status(), `${baseUrl} 完成上传失败`).toBe(200)
  return { id: session.file_id, uploadId: session.upload_id }
}

async function pendingUpload(client: APIRequestContext, baseUrl: string, name: string) {
  const created = await client.post('/api/uploads', {
    headers: headers(baseUrl, true),
    data: { parent_id: ROOT, name, size: 9, mime_type: 'text/plain' },
  })
  expect(created.status(), `${baseUrl} 创建 pending 上传会话失败`).toBe(201)
  const session = await created.json() as { upload_id: string; file_id: string }
  return { id: session.file_id, uploadId: session.upload_id }
}

type ZipEntry = { name: string; body: Buffer }

// The two ZIP writers use different timestamps and compression metadata. Read
// the central directory so the parity assertion compares the user-visible
// archive entries and bytes rather than incidental container bytes.
function readZipEntries(data: Buffer): ZipEntry[] {
  const signature = Buffer.from([0x50, 0x4b, 0x05, 0x06])
  const end = data.lastIndexOf(signature)
  if (end < 0) throw new Error('ZIP end record not found')
  const count = data.readUInt16LE(end + 10)
  const centralOffset = data.readUInt32LE(end + 16)
  const entries: ZipEntry[] = []
  let cursor = centralOffset
  for (let index = 0; index < count; index += 1) {
    if (data.readUInt32LE(cursor) !== 0x02014b50) throw new Error('ZIP central record not found')
    const method = data.readUInt16LE(cursor + 10)
    const compressedSize = data.readUInt32LE(cursor + 20)
    const nameLength = data.readUInt16LE(cursor + 28)
    const extraLength = data.readUInt16LE(cursor + 30)
    const commentLength = data.readUInt16LE(cursor + 32)
    const localOffset = data.readUInt32LE(cursor + 42)
    const name = data.subarray(cursor + 46, cursor + 46 + nameLength).toString('utf8')
    if (data.readUInt32LE(localOffset) !== 0x04034b50) throw new Error(`ZIP local record missing for ${name}`)
    const localNameLength = data.readUInt16LE(localOffset + 26)
    const localExtraLength = data.readUInt16LE(localOffset + 28)
    const payloadStart = localOffset + 30 + localNameLength + localExtraLength
    const compressed = data.subarray(payloadStart, payloadStart + compressedSize)
    const body = method === 0 ? compressed : method === 8 ? inflateRawSync(compressed) : (() => {
      throw new Error(`unsupported ZIP compression method ${method}`)
    })()
    entries.push({ name, body })
    cursor += 46 + nameLength + extraLength + commentLength
  }
  return entries
}

function comparableZip(entries: ZipEntry[]) {
  return entries.map(entry => ({ name: entry.name, body: entry.body.toString('base64') }))
}

async function responseShape(response: Awaited<ReturnType<APIRequestContext['fetch']>>) {
  const body = await response.body()
  const text = body.toString('utf8')
  let json: unknown = null
  try {
    json = JSON.parse(text)
  } catch {
    // ZIP responses are intentionally compared through their entries below.
  }
  const responseHeaders = response.headers()
  return {
    status: response.status(),
    type: responseHeaders['content-type']?.split(';', 1)[0] ?? '',
    disposition: responseHeaders['content-disposition'] ?? '',
    cacheControl: responseHeaders['cache-control'] ?? '',
    text,
    json,
    body,
  }
}

async function prepare(client: APIRequestContext, baseUrl: string, ids: string[]) {
  const response = await client.post('/api/files/batch-download/prepare', {
    headers: headers(baseUrl, true),
    data: { ids },
  })
  const text = await response.text()
  let json: unknown = null
  try {
    json = JSON.parse(text)
  } catch {
    // Keep the raw body in the assertion for malformed/error responses.
  }
  return { response, text, json }
}

async function prepareBody(client: APIRequestContext, baseUrl: string, data: unknown) {
  const response = await client.post('/api/files/batch-download/prepare', {
    headers: headers(baseUrl, true),
    data,
  })
  const text = await response.text()
  let json: unknown = null
  try {
    json = JSON.parse(text)
  } catch {
    // Keep the raw body in the assertion for malformed/error responses.
  }
  return { response, text, json }
}

async function cleanup(client: APIRequestContext, baseUrl: string, fixture: {
  files: Array<{ id: string; uploadId: string }>
  pending: { id: string; uploadId: string }
  directoryId: string
}) {
  await client.delete(`/api/uploads/${fixture.pending.uploadId}`, { headers: headers(baseUrl) }).catch(() => undefined)
  for (const file of [...fixture.files].reverse()) {
    await client.delete(`/api/files/${file.id}`, { headers: headers(baseUrl) }).catch(() => undefined)
    await client.delete(`/api/trash/${file.id}`, { headers: headers(baseUrl) }).catch(() => undefined)
  }
  await client.delete(`/api/files/${fixture.directoryId}`, { headers: headers(baseUrl) }).catch(() => undefined)
  await client.delete(`/api/trash/${fixture.directoryId}`, { headers: headers(baseUrl) }).catch(() => undefined)
}

async function makeFixture(client: APIRequestContext, baseUrl: string, suffix: string) {
  const directoryId = await createDirectory(client, baseUrl, `batch-reference-${suffix}`)
  const sharedName = `same-name-${suffix}.txt`
  const secondName = `second-name-${suffix}.txt`
  const files = [
    await upload(client, baseUrl, ROOT, sharedName, Buffer.from('root entry\n')),
    await upload(client, baseUrl, directoryId, sharedName, Buffer.from('nested entry\n')),
    await upload(client, baseUrl, ROOT, secondName, Buffer.from('second entry\n')),
  ]
  const pending = await pendingUpload(client, baseUrl, `pending-batch-${suffix}.txt`)
  return { directoryId, files, pending, sharedName, secondName }
}

test('old/new 批量下载完整覆盖 prepare、ZIP 内容、一次性 token 和错误分流', async () => {
  const oldUrl = process.env.E2E_REFERENCE_URL || 'http://127.0.0.1:18080'
  const newUrl = process.env.E2E_NEW_URL || 'http://127.0.0.1:18084'
  const suffix = crypto.randomUUID().slice(0, 8)
  const [oldClient, newClient] = await Promise.all([login(oldUrl), login(newUrl)])
  const [oldFixture, newFixture] = await Promise.all([
    makeFixture(oldClient, oldUrl, suffix),
    makeFixture(newClient, newUrl, suffix),
  ])

  try {
    const oldIds = oldFixture.files.map(file => file.id)
    const newIds = newFixture.files.map(file => file.id)
    const [oldPrepared, newPrepared] = await Promise.all([
      prepare(oldClient, oldUrl, oldIds),
      prepare(newClient, newUrl, newIds),
    ])
    expect(newPrepared.response.status(), 'prepare 状态与 reference 不一致').toBe(oldPrepared.response.status())
    expect(newPrepared.response.headers()['content-type']?.split(';', 1)[0])
      .toBe(oldPrepared.response.headers()['content-type']?.split(';', 1)[0])
    const oldToken = (oldPrepared.json as { token: string }).token
    const newToken = (newPrepared.json as { token: string }).token
    expect(oldToken).toMatch(/^[A-Za-z0-9_-]{43}$/)
    expect(newToken).toMatch(/^[A-Za-z0-9_-]{43}$/)
    expect(newToken).not.toBe(oldToken)

    const [oldZip, newZip] = await Promise.all([
      responseShape(await oldClient.get(`/api/files/batch-download/${oldToken}`)),
      responseShape(await newClient.get(`/api/files/batch-download/${newToken}`)),
    ])
    expect({
      status: newZip.status,
      type: newZip.type,
      disposition: newZip.disposition,
      cacheControl: newZip.cacheControl,
    }, 'ZIP transport 与 reference 不一致').toEqual({
      status: oldZip.status,
      type: oldZip.type,
      disposition: oldZip.disposition,
      cacheControl: oldZip.cacheControl,
    })
    const expectedEntries = [
      { name: oldFixture.sharedName, body: Buffer.from('root entry\n').toString('base64') },
      { name: oldFixture.sharedName.replace(/\.txt$/i, ' (2).txt'), body: Buffer.from('nested entry\n').toString('base64') },
      { name: oldFixture.secondName, body: Buffer.from('second entry\n').toString('base64') },
    ]
    expect(comparableZip(readZipEntries(oldZip.body))).toEqual(expectedEntries)
    expect(comparableZip(readZipEntries(newZip.body)), 'ZIP 文件名或内容与 reference 不一致').toEqual(expectedEntries)

    const oldOneTime = (await prepare(oldClient, oldUrl, [oldIds[0]])).json as { token: string }
    const newOneTime = (await prepare(newClient, newUrl, [newIds[0]])).json as { token: string }
    const unauthenticated = await createRequest.newContext()
    try {
      const [oldUnauthorized, newUnauthorized] = await Promise.all([
        responseShape(await unauthenticated.get(`${oldUrl}/api/files/batch-download/${oldOneTime.token}`)),
        responseShape(await unauthenticated.get(`${newUrl}/api/files/batch-download/${newOneTime.token}`)),
      ])
      expect({ status: newUnauthorized.status, type: newUnauthorized.type, json: newUnauthorized.json })
        .toEqual({ status: oldUnauthorized.status, type: oldUnauthorized.type, json: oldUnauthorized.json })
    } finally {
      await unauthenticated.dispose()
    }
    const [oldFirst, newFirst] = await Promise.all([
      responseShape(await oldClient.get(`/api/files/batch-download/${oldOneTime.token}`)),
      responseShape(await newClient.get(`/api/files/batch-download/${newOneTime.token}`)),
    ])
    expect(oldFirst.status).toBe(200)
    expect(newFirst.status).toBe(200)
    const [oldReplay, newReplay] = await Promise.all([
      responseShape(await oldClient.get(`/api/files/batch-download/${oldOneTime.token}`)),
      responseShape(await newClient.get(`/api/files/batch-download/${newOneTime.token}`)),
    ])
    expect({ status: newReplay.status, type: newReplay.type, json: newReplay.json })
      .toEqual({ status: oldReplay.status, type: oldReplay.type, json: oldReplay.json })

    const cases = [
      { label: 'empty', oldIds: [], newIds: [] },
      { label: 'malformed', oldIds: ['nope'], newIds: ['nope'] },
      { label: 'object-key-shaped', oldIds: [`blobs/${oldIds[0]}`], newIds: [`blobs/${newIds[0]}`] },
      { label: 'duplicate', oldIds: [oldIds[0], oldIds[0]], newIds: [newIds[0], newIds[0]] },
      { label: 'directory', oldIds: [oldFixture.directoryId], newIds: [newFixture.directoryId] },
      { label: 'pending', oldIds: [oldFixture.pending.id], newIds: [newFixture.pending.id] },
      { label: 'missing', oldIds: [MISSING], newIds: [MISSING] },
    ]
    for (const item of cases) {
      const [oldResult, newResult] = await Promise.all([
        prepare(oldClient, oldUrl, item.oldIds),
        prepare(newClient, newUrl, item.newIds),
      ])
      expect({ status: newResult.response.status(), type: newResult.response.headers()['content-type']?.split(';', 1)[0], json: newResult.json }, `${item.label} prepare 与 reference 不一致`)
        .toEqual({ status: oldResult.response.status(), type: oldResult.response.headers()['content-type']?.split(';', 1)[0], json: oldResult.json })
    }
  } finally {
    await Promise.all([
      cleanup(oldClient, oldUrl, oldFixture),
      cleanup(newClient, newUrl, newFixture),
    ])
    await Promise.all([oldClient.dispose(), newClient.dispose()])
  }
})

test('旧版批量下载 prepare 的 JSON 零值和未知字段在 Rust 版保持一致', async () => {
  const oldUrl = process.env.E2E_REFERENCE_URL || 'http://127.0.0.1:18080'
  const newUrl = process.env.E2E_NEW_URL || 'http://127.0.0.1:18084'
  const [oldClient, newClient] = await Promise.all([login(oldUrl), login(newUrl)])
  const cases: Array<[string, unknown, Record<string, unknown>]> = [
    ['缺省字段', {}, { error: { status: 400, message: 'at least one file id is required' } }],
    ['null', { ids: null }, { error: { status: 400, message: 'at least one file id is required' } }],
    ['null item', { ids: [null] }, { error: { status: 400, message: 'invalid file id' } }],
    ['未知字段', { ids: [], extra: true }, { error: { status: 400, message: 'invalid JSON request' } }],
    ['malformed', Buffer.from('{"ids":'), { error: { status: 400, message: 'invalid JSON request' } }],
  ]

  try {
    for (const [label, data, expectedError] of cases) {
      const [oldResult, newResult] = await Promise.all([
        prepareBody(oldClient, oldUrl, data),
        prepareBody(newClient, newUrl, data),
      ])
      expect(newResult.response.status(), `Rust ${label} 状态与 reference 不一致`).toBe(oldResult.response.status())
      expect(newResult.response.headers()['content-type']?.split(';', 1)[0], `Rust ${label} Content-Type 与 reference 不一致`)
        .toBe(oldResult.response.headers()['content-type']?.split(';', 1)[0])
      expect(oldResult.response.status(), `reference ${label} 状态异常`).toBe(400)
      expect(oldResult.json, `reference ${label} 错误 envelope 异常`).toEqual(expectedError)
      expect(newResult.json, `Rust ${label} 错误 envelope 与 reference 不一致`).toEqual(oldResult.json)
    }
  } finally {
    await Promise.all([oldClient.dispose(), newClient.dispose()])
  }
})

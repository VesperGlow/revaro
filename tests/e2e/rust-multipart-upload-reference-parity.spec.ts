import { expect, request as createRequest, test, type APIRequestContext, type APIResponse } from '@playwright/test'

const ROOT = '00000000-0000-0000-0000-000000000000'
const PART_SIZE = 16 << 20

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

async function createMultipart(client: APIRequestContext, baseUrl: string, name: string, size: number) {
  const response = await client.post('/api/uploads', {
    headers: headers(baseUrl, true),
    data: { parent_id: ROOT, name, size, mime_type: 'application/octet-stream' },
  })
  expect(response.status(), `${baseUrl} 创建 multipart session`).toBe(201)
  const session = await response.json() as {
    upload_id: string
    file_id: string
    mode: string
    url: string
    part_size: number
    part_count: number
  }
  expect(session).toMatchObject({ mode: 'multipart', url: '', part_size: PART_SIZE, part_count: 2 })
  return session
}

async function cleanup(client: APIRequestContext, baseUrl: string, sessions: Array<{ upload_id: string; file_id: string }>) {
  for (const session of sessions) {
    await client.delete(`/api/uploads/${session.upload_id}`, { headers: headers(baseUrl) }).catch(() => undefined)
    await client.delete(`/api/files/${session.file_id}`, { headers: headers(baseUrl) }).catch(() => undefined)
    await client.delete(`/api/trash/${session.file_id}`, { headers: headers(baseUrl) }).catch(() => undefined)
  }
}

async function fileSemantics(response: APIResponse) {
  const file = await response.json() as Record<string, unknown>
  return {
    parent_id: file.parent_id,
    name: file.name,
    kind: file.kind,
    size: file.size,
    mime_type: file.mime_type,
    status: file.status,
    etagShape: typeof file.etag === 'string' && /^\d+-[0-9a-f]+$/.test(file.etag),
    content_hash: file.content_hash,
    hash_algorithm: file.hash_algorithm,
  }
}

test('multipart 分片 URL、裸 PUT、ack、complete 和非法 ack 保持 reference', async () => {
  const oldUrl = process.env.E2E_REFERENCE_URL || 'http://127.0.0.1:18080'
  const newUrl = process.env.E2E_NEW_URL || 'http://127.0.0.1:18084'
  const body = Buffer.alloc(PART_SIZE + 1, 0x6d)
  const suffix = crypto.randomUUID().slice(0, 8)
  const oldClient = await login(oldUrl)
  const newClient = await login(newUrl)
  const oldSessions: Array<{ upload_id: string; file_id: string }> = []
  const newSessions: Array<{ upload_id: string; file_id: string }> = []

  try {
    const oldSession = await createMultipart(oldClient, oldUrl, `multipart-${suffix}.bin`, body.length)
    const newSession = await createMultipart(newClient, newUrl, `multipart-${suffix}.bin`, body.length)
    oldSessions.push(oldSession)
    newSessions.push(newSession)

    const [oldStatus, newStatus] = await Promise.all([
      oldClient.get(`/api/uploads/${oldSession.upload_id}`, { headers: headers(oldUrl) }),
      newClient.get(`/api/uploads/${newSession.upload_id}`, { headers: headers(newUrl) }),
    ])
    expect(newStatus.status()).toBe(oldStatus.status())
    expect(await newStatus.json()).toMatchObject({
      mode: 'multipart',
      url: '',
      part_size: PART_SIZE,
      part_count: 2,
      expected_size: body.length,
      parts: [],
    })
    expect(await oldStatus.json()).toMatchObject({
      mode: 'multipart',
      url: '',
      part_size: PART_SIZE,
      part_count: 2,
      expected_size: body.length,
      parts: [],
    })

    const [oldParts, newParts] = await Promise.all([
      oldClient.post(`/api/uploads/${oldSession.upload_id}/parts`, {
        headers: headers(oldUrl, true),
        data: { part_numbers: [1, 2] },
      }),
      newClient.post(`/api/uploads/${newSession.upload_id}/parts`, {
        headers: headers(newUrl, true),
        data: { part_numbers: [1, 2] },
      }),
    ])
    expect(newParts.status()).toBe(oldParts.status())
    const oldPartUrls = await oldParts.json() as { parts: Array<{ part_number: number; url: string }> }
    const newPartUrls = await newParts.json() as { parts: Array<{ part_number: number; url: string }> }
    expect(newPartUrls.parts.map(part => ({ number: part.part_number, suffix: part.url.split('/').slice(-2).join('/') })))
      .toEqual(oldPartUrls.parts.map(part => ({ number: part.part_number, suffix: part.url.split('/').slice(-2).join('/') })))

    const oldEtags: string[] = []
    const newEtags: string[] = []
    for (const [index, part] of [body.subarray(0, PART_SIZE), body.subarray(PART_SIZE)] .entries()) {
      const number = index + 1
      const [oldBytes, newBytes] = await Promise.all([
        oldClient.put(oldPartUrls.parts[index].url, {
          headers: { ...headers(oldUrl), 'Content-Type': 'application/octet-stream' },
          data: part,
        }),
        newClient.put(newPartUrls.parts[index].url, {
          headers: { ...headers(newUrl), 'Content-Type': 'application/octet-stream' },
          data: part,
        }),
      ])
      expect(newBytes.status(), `part ${number} PUT status`).toBe(oldBytes.status())
      oldEtags.push(oldBytes.headers().etag)
      newEtags.push(newBytes.headers().etag)
      expect(oldEtags[index], `old part ${number} ETag`).toMatch(/^\d+-[0-9a-f]+$/)
      expect(newEtags[index], `new part ${number} ETag`).toMatch(/^\d+-[0-9a-f]+$/)

      const [oldAck, newAck] = await Promise.all([
        oldClient.put(`/api/uploads/${oldSession.upload_id}/parts/${number}`, {
          headers: headers(oldUrl, true),
          data: { etag: ` ${oldEtags[index]} `, size: part.length, content_hash: '' },
        }),
        newClient.put(`/api/uploads/${newSession.upload_id}/parts/${number}`, {
          headers: headers(newUrl, true),
          data: { etag: ` ${newEtags[index]} `, size: part.length, content_hash: '' },
        }),
      ])
      expect(newAck.status(), `part ${number} ack status`).toBe(oldAck.status())
    }

    const [oldCompleted, newCompleted] = await Promise.all([
      oldClient.post(`/api/uploads/${oldSession.upload_id}/complete`, {
        headers: headers(oldUrl, true),
        data: { parts: oldEtags.map((etag, index) => ({ part_number: index + 1, etag })) },
      }),
      newClient.post(`/api/uploads/${newSession.upload_id}/complete`, {
        headers: headers(newUrl, true),
        data: { parts: newEtags.map((etag, index) => ({ part_number: index + 1, etag })) },
      }),
    ])
    expect(newCompleted.status()).toBe(oldCompleted.status())
    expect(await fileSemantics(newCompleted)).toEqual(await fileSemantics(oldCompleted))

    const oldInvalid = await createMultipart(oldClient, oldUrl, `invalid-ack-${suffix}.bin`, body.length)
    const newInvalid = await createMultipart(newClient, newUrl, `invalid-ack-${suffix}.bin`, body.length)
    oldSessions.push(oldInvalid)
    newSessions.push(newInvalid)
    const [oldPart, newPart] = await Promise.all([
      oldClient.put(`/api/uploads/${oldInvalid.upload_id}/data/1`, {
        headers: { ...headers(oldUrl), 'Content-Type': 'application/octet-stream' },
        data: body.subarray(0, PART_SIZE),
      }),
      newClient.put(`/api/uploads/${newInvalid.upload_id}/data/1`, {
        headers: { ...headers(newUrl), 'Content-Type': 'application/octet-stream' },
        data: body.subarray(0, PART_SIZE),
      }),
    ])
    const [oldInvalidAck, newInvalidAck] = await Promise.all([
      oldClient.put(`/api/uploads/${oldInvalid.upload_id}/parts/1`, {
        headers: headers(oldUrl, true),
        data: { etag: '', size: body.subarray(0, PART_SIZE).length, content_hash: '' },
      }),
      newClient.put(`/api/uploads/${newInvalid.upload_id}/parts/1`, {
        headers: headers(newUrl, true),
        data: { etag: '', size: body.subarray(0, PART_SIZE).length, content_hash: '' },
      }),
    ])
    expect(newPart.status()).toBe(oldPart.status())
    expect(newInvalidAck.status(), '空 ETag ack 应保持 reference 的 400').toBe(oldInvalidAck.status())
    expect(newInvalidAck.status()).toBe(400)
  } finally {
    await Promise.all([
      cleanup(oldClient, oldUrl, oldSessions),
      cleanup(newClient, newUrl, newSessions),
      oldClient.dispose(),
      newClient.dispose(),
    ])
  }
})

import { expect, request as createRequest, test, type APIRequestContext } from '@playwright/test'
import { readFileSync } from 'node:fs'

const ROOT = '00000000-0000-0000-0000-000000000000'
const videoFixture = readFileSync(new URL('./fixtures/preview.webm', import.meta.url))

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
  const session = await created.json() as { upload_id: string; file_id: string; url: string; mode: string; part_count: number }
  expect(session.mode).toBe('single')
  expect(session.part_count).toBe(0)

  const uploaded = await client.put(session.url, {
    headers: { ...headers(baseUrl), 'Content-Type': mimeType },
    data: body,
  })
  expect(uploaded.status(), `${baseUrl} 上传 ${name}`).toBe(204)

  const completed = await client.post(`/api/uploads/${session.upload_id}/complete`, {
    headers: headers(baseUrl, true),
    data: { parts: [] },
  })
  expect(completed.status(), `${baseUrl} 完成 ${name}`).toBe(200)
  const file = await completed.json() as { id: string }
  return { id: file.id, uploadId: session.upload_id }
}

async function responseShape(response: Awaited<ReturnType<APIRequestContext['get']>>) {
  const responseHeaders = response.headers()
  return {
    status: response.status(),
    type: responseHeaders['content-type'] ?? '',
    disposition: responseHeaders['content-disposition'] ?? '',
    ranges: responseHeaders['accept-ranges'] ?? '',
    range: responseHeaders['content-range'] ?? '',
    length: responseHeaders['content-length'] ?? '',
    etag: responseHeaders.etag ?? '',
    cache: responseHeaders['cache-control'] ?? '',
    referrer: responseHeaders['referrer-policy'] ?? '',
    body: await response.body(),
  }
}

function comparable(shape: Awaited<ReturnType<typeof responseShape>>) {
  const { body: _body, etag, ...value } = shape
  return { ...value, etag: { present: etag !== '', quoted: /^".*"$/.test(etag) } }
}

function errorComparable(shape: Awaited<ReturnType<typeof responseShape>>) {
  const { body, length: _length, ...value } = shape
  return { ...value, json: JSON.parse(body.toString('utf8')) }
}

function thumbnailComparable(shape: Awaited<ReturnType<typeof responseShape>>) {
  const { body: _body, length: _length, etag, ...value } = shape
  return { ...value, has_body: shape.body.length > 0, etag: { present: etag !== '', quoted: /^".*"$/.test(etag) } }
}

async function cleanup(client: APIRequestContext, ids: string[], uploadIds: string[]) {
  for (const uploadId of uploadIds) await client.delete(`/api/uploads/${uploadId}`).catch(() => undefined)
  for (const id of ids) {
    await client.delete(`/api/files/${id}`).catch(() => undefined)
    await client.delete(`/api/trash/${id}`).catch(() => undefined)
  }
}

function tinyPng() {
  return Buffer.from(
    'iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mNk+A8AAQUBAScY42YAAAAASUVORK5CYII=',
    'base64',
  )
}

function tinyWav() {
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

test('old/new preview、Range、鉴权和 thumbnail 响应保持 reference 行为', async () => {
  const oldUrl = process.env.E2E_REFERENCE_URL || 'http://127.0.0.1:18080'
  const newUrl = process.env.E2E_NEW_URL || 'http://127.0.0.1:18084'
  const suffix = crypto.randomUUID().slice(0, 8)
  const fixtures = [
    { name: `preview-${suffix}.txt`, mime: 'text/plain', body: Buffer.from(`preview ${suffix}\n文本\n`) },
    { name: `preview-${suffix}.png`, mime: 'image/png', body: tinyPng() },
    { name: `preview-${suffix}.wav`, mime: 'audio/wav', body: tinyWav() },
    { name: `preview-${suffix}.webm`, mime: 'video/webm', body: videoFixture },
  ] as const
  const clients = await Promise.all([login(oldUrl), login(newUrl)])
  const created: Array<Array<{ id: string; uploadId: string }>> = [[], []]

  try {
    for (const fixture of fixtures) {
      created[0].push(await upload(clients[0], oldUrl, fixture.name, fixture.mime, fixture.body))
      created[1].push(await upload(clients[1], newUrl, fixture.name, fixture.mime, fixture.body))
    }

    for (let index = 0; index < fixtures.length; index += 1) {
      const fixture = fixtures[index]
      const [oldPreview, newPreview] = await Promise.all([
        responseShape(await clients[0].get(`/api/files/${created[0][index].id}/preview`, { headers: headers(oldUrl) })),
        responseShape(await clients[1].get(`/api/files/${created[1][index].id}/preview`, { headers: headers(newUrl) })),
      ])
      if (fixture.mime === 'text/plain') {
        expect(errorComparable(newPreview), `${fixture.name} preview 错误响应与 reference 不一致`).toEqual(errorComparable(oldPreview))
        continue
      }
      expect(comparable(newPreview), `${fixture.name} preview headers/status 与 reference 不一致`).toEqual(comparable(oldPreview))
      expect(newPreview.body.equals(oldPreview.body), `${fixture.name} preview body 与 reference 不一致`).toBe(true)

      const [oldRange, newRange] = await Promise.all([
        responseShape(await clients[0].get(`/api/files/${created[0][index].id}/preview`, {
          headers: { ...headers(oldUrl), Range: 'bytes=2-9' },
        })),
        responseShape(await clients[1].get(`/api/files/${created[1][index].id}/preview`, {
          headers: { ...headers(newUrl), Range: 'bytes=2-9' },
        })),
      ])
      expect(comparable(newRange), `${fixture.name} preview Range 与 reference 不一致`).toEqual(comparable(oldRange))
      expect(newRange.body.equals(oldRange.body), `${fixture.name} preview Range body 与 reference 不一致`).toBe(true)

      const [oldStale, newStale] = await Promise.all([
        responseShape(await clients[0].get(`/api/files/${created[0][index].id}/preview`, {
          headers: { ...headers(oldUrl), Range: 'bytes=0-2', 'If-Range': '"stale"' },
        })),
        responseShape(await clients[1].get(`/api/files/${created[1][index].id}/preview`, {
          headers: { ...headers(newUrl), Range: 'bytes=0-2', 'If-Range': '"stale"' },
        })),
      ])
      expect(comparable(newStale), `${fixture.name} stale If-Range 与 reference 不一致`).toEqual(comparable(oldStale))
      expect(newStale.body.equals(oldStale.body), `${fixture.name} stale If-Range body 与 reference 不一致`).toBe(true)
    }

    const textIndex = 0

    const imageIndex = 1
    const [oldThumbnail, newThumbnail] = await Promise.all([
      responseShape(await clients[0].get(`/api/files/${created[0][imageIndex].id}/thumbnail?v=etag-${suffix}`, { headers: headers(oldUrl) })),
      responseShape(await clients[1].get(`/api/files/${created[1][imageIndex].id}/thumbnail?v=etag-${suffix}`, { headers: headers(newUrl) })),
    ])
    expect(oldThumbnail.status).toBe(200)
    expect(thumbnailComparable(newThumbnail), '图片 thumbnail 响应与 reference 不一致').toEqual(thumbnailComparable(oldThumbnail))
    expect(oldThumbnail.body.subarray(0, 2).equals(Buffer.from([0xff, 0xd8]))).toBe(true)
    expect(newThumbnail.body.subarray(0, 2).equals(Buffer.from([0xff, 0xd8]))).toBe(true)

    const [oldNoThumbnail, newNoThumbnail] = await Promise.all([
      responseShape(await clients[0].get(`/api/files/${created[0][textIndex].id}/thumbnail`, { headers: headers(oldUrl) })),
      responseShape(await clients[1].get(`/api/files/${created[1][textIndex].id}/thumbnail`, { headers: headers(newUrl) })),
    ])
    expect(errorComparable(newNoThumbnail), '无 thumbnail 文件的错误响应与 reference 不一致').toEqual(errorComparable(oldNoThumbnail))

    const oldAnonymous = await createRequest.newContext({ baseURL: oldUrl })
    const newAnonymous = await createRequest.newContext({ baseURL: newUrl })
    try {
      const [oldDenied, newDenied] = await Promise.all([
        responseShape(await oldAnonymous.get(`/api/files/${created[0][imageIndex].id}/preview`, { headers: headers(oldUrl) })),
        responseShape(await newAnonymous.get(`/api/files/${created[1][imageIndex].id}/preview`, { headers: headers(newUrl) })),
      ])
      expect(errorComparable(newDenied), '未鉴权 preview 错误响应与 reference 不一致').toEqual(errorComparable(oldDenied))
    } finally {
      await Promise.all([oldAnonymous.dispose(), newAnonymous.dispose()])
    }
  } finally {
    await Promise.all([
      cleanup(clients[0], created[0].map(item => item.id), created[0].map(item => item.uploadId)),
      cleanup(clients[1], created[1].map(item => item.id), created[1].map(item => item.uploadId)),
    ])
    await Promise.all(clients.map(client => client.dispose()))
  }
})

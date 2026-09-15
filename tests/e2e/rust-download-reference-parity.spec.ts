import { expect, request as createRequest, test, type APIRequestContext } from '@playwright/test'

const ROOT = '00000000-0000-0000-0000-000000000000'

function originHeaders(baseUrl: string, json = false) {
  return json
    ? { Origin: baseUrl, 'Content-Type': 'application/json' }
    : { Origin: baseUrl }
}

async function login(baseUrl: string) {
  const client = await createRequest.newContext({ baseURL: baseUrl })
  const response = await client.post('/api/auth/login', {
    headers: originHeaders(baseUrl, true),
    data: { username: 'admin', password: 'revaro-e2e-password' },
  })
  expect(response.status(), `${baseUrl} 登录失败`).toBe(200)
  return client
}

async function upload(client: APIRequestContext, baseUrl: string, name: string, body: Buffer) {
  const created = await client.post('/api/uploads', {
    headers: originHeaders(baseUrl, true),
    data: { parent_id: ROOT, name, size: body.length, mime_type: 'text/plain' },
  })
  expect(created.status(), `${baseUrl} 创建 upload session`).toBe(201)
  const session = await created.json() as { upload_id: string; file_id: string; url: string; mode: string }
  expect(session.mode).toBe('single')

  const uploaded = await client.put(session.url, {
    headers: { ...originHeaders(baseUrl), 'Content-Type': 'text/plain' },
    data: body,
  })
  expect(uploaded.status(), `${baseUrl} 上传文件`).toBe(204)

  const completed = await client.post(`/api/uploads/${session.upload_id}/complete`, {
    headers: originHeaders(baseUrl, true),
    data: { parts: [] },
  })
  expect(completed.status(), `${baseUrl} 完成上传`).toBe(200)
  const file = await completed.json() as { id: string }
  return { id: file.id, uploadId: session.upload_id }
}

async function responseShape(response: Awaited<ReturnType<APIRequestContext['fetch']>>) {
  const headers = response.headers()
  return {
    status: response.status(),
    type: headers['content-type'] ?? '',
    disposition: headers['content-disposition'] ?? '',
    ranges: headers['accept-ranges'] ?? '',
    range: headers['content-range'] ?? '',
    length: headers['content-length'] ?? '',
    etag: headers.etag ?? '',
    body: await response.body(),
  }
}

function comparable(shape: Awaited<ReturnType<typeof responseShape>>) {
  const { body: _body, etag, type, ...value } = shape
  return {
    ...value,
    type: type.split(';', 1)[0],
    etag: { present: etag !== '', quoted: /^".*"$/.test(etag) },
  }
}

function normalizedMultipart(shape: Awaited<ReturnType<typeof responseShape>>) {
  const boundary = /boundary=([^;]+)/.exec(shape.type)?.[1]
  if (!boundary) throw new Error(`multipart response has no boundary: ${shape.type}`)
  return shape.body.toString('utf8').replaceAll(boundary, 'reference-boundary')
}

async function cleanup(client: APIRequestContext, id: string, uploadId: string) {
  await client.delete(`/api/uploads/${uploadId}`).catch(() => undefined)
  await client.delete(`/api/files/${id}`).catch(() => undefined)
  await client.delete(`/api/trash/${id}`).catch(() => undefined)
}

test('old/new 下载 Range、If-Range、非法范围和 HEAD 保持 reference 行为', async () => {
  const oldUrl = process.env.E2E_REFERENCE_URL || 'http://127.0.0.1:18080'
  const newUrl = process.env.E2E_NEW_URL || 'http://127.0.0.1:18084'
  const suffix = crypto.randomUUID().slice(0, 8)
  const name = `download-range-reference-${suffix}.txt`
  const content = Buffer.from(`0123456789abcdefghijklmnopqrstuvwxyz\n${suffix}\n`, 'utf8')
  const clients = await Promise.all([login(oldUrl), login(newUrl)])
  const created = await Promise.all([
    upload(clients[0], oldUrl, name, content),
    upload(clients[1], newUrl, name, content),
  ])

  try {
    const paths = created.map(item => `/api/files/${item.id}/download`)
    const [oldFull, newFull] = await Promise.all(paths.map(async (path, index) => responseShape(
      await clients[index].fetch(path, { headers: originHeaders(index === 0 ? oldUrl : newUrl) }),
    )))
    expect(comparable(newFull), '完整下载响应与 reference 不一致').toEqual(comparable(oldFull))
    expect(newFull.body.equals(oldFull.body), '完整下载内容与 reference 不一致').toBe(true)

    const cases = [
      { label: 'fixed', range: 'bytes=2-8' },
      { label: 'open-ended', range: 'bytes=9-' },
      { label: 'suffix', range: 'bytes=-7' },
      { label: 'zero-suffix', range: 'bytes=-0' },
      { label: 'malformed', range: 'bytes=not-a-range' },
      { label: 'multi-range', range: 'bytes=0-1,4-6' },
      { label: 'past-end', range: 'bytes=999999-' },
      { label: 'reversed', range: 'bytes=8-2' },
    ] as const

    for (const item of cases) {
      const [oldResponse, newResponse] = await Promise.all(paths.map(async (path, index) => responseShape(
        await clients[index].fetch(path, {
          headers: {
            ...originHeaders(index === 0 ? oldUrl : newUrl),
            Range: item.range,
          },
        }),
      )))
      expect(comparable(newResponse), `${item.label} Range 与 reference 不一致`).toEqual(comparable(oldResponse))
      if (item.label === 'multi-range') {
        expect(normalizedMultipart(newResponse), '多段 Range body 与 reference 不一致').toBe(normalizedMultipart(oldResponse))
      } else {
        expect(newResponse.body.equals(oldResponse.body), `${item.label} Range body 与 reference 不一致`).toBe(true)
      }
    }

    const [oldMatching, newMatching] = await Promise.all(paths.map(async (path, index) => responseShape(
      await clients[index].fetch(path, {
        headers: {
          ...originHeaders(index === 0 ? oldUrl : newUrl),
          Range: 'bytes=3-11',
          'If-Range': index === 0 ? oldFull.etag : newFull.etag,
        },
      }),
    )))
    expect(comparable(newMatching), '匹配 If-Range 与 reference 不一致').toEqual(comparable(oldMatching))
    expect(newMatching.body.equals(oldMatching.body), '匹配 If-Range body 与 reference 不一致').toBe(true)

    const [oldStale, newStale] = await Promise.all(paths.map(async (path, index) => responseShape(
      await clients[index].fetch(path, {
        headers: {
          ...originHeaders(index === 0 ? oldUrl : newUrl),
          Range: 'bytes=3-11',
          'If-Range': '"stale"',
        },
      }),
    )))
    expect(comparable(newStale), '失配 If-Range 与 reference 不一致').toEqual(comparable(oldStale))
    expect(newStale.body.equals(oldStale.body), '失配 If-Range body 与 reference 不一致').toBe(true)

    const [oldHead, newHead] = await Promise.all(paths.map(async (path, index) => responseShape(
      await clients[index].fetch(path, {
        method: 'HEAD',
        headers: originHeaders(index === 0 ? oldUrl : newUrl),
      }),
    )))
    expect(comparable(newHead), '下载 HEAD 与 reference 不一致').toEqual(comparable(oldHead))
    expect(newHead.body.length).toBe(0)
    expect(oldHead.body.length).toBe(0)
  } finally {
    await Promise.all([
      cleanup(clients[0], created[0].id, created[0].uploadId),
      cleanup(clients[1], created[1].id, created[1].uploadId),
    ])
    await Promise.all(clients.map(client => client.dispose()))
  }
})

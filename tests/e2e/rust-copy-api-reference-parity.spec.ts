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

async function createDocument(client: APIRequestContext, baseUrl: string, name: string, content: string) {
  const response = await client.post('/api/documents', {
    headers: headers(baseUrl, true),
    data: { parent_id: ROOT, name, content },
  })
  expect(response.status(), `${baseUrl} 创建 ${name} 失败`).toBe(201)
  return await response.json() as Record<string, unknown>
}

async function copy(client: APIRequestContext, baseUrl: string, id: string, parentId: string) {
  const response = await client.post(`/api/files/${id}/copy`, {
    headers: headers(baseUrl, true),
    data: { parent_id: parentId },
  })
  const text = await response.text()
  let body: unknown
  try {
    body = JSON.parse(text)
  } catch {
    body = text
  }
  return { status: response.status(), type: response.headers()['content-type'] ?? '', body }
}

function fileSemantics(value: unknown) {
  const file = value as Record<string, unknown>
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

async function cleanup(client: APIRequestContext, ids: string[]) {
  for (const id of ids) {
    await client.delete(`/api/files/${id}`).catch(() => undefined)
    await client.delete(`/api/trash/${id}`).catch(() => undefined)
  }
}

test('copy API 的同名后缀、无效目标和错误分流保持 reference 行为', async () => {
  const oldUrl = process.env.E2E_REFERENCE_URL || 'http://127.0.0.1:18080'
  const newUrl = process.env.E2E_NEW_URL || 'http://127.0.0.1:18084'
  const suffix = crypto.randomUUID().slice(0, 8)
  const name = `copy-api-${suffix}.txt`
  const clients = await Promise.all([login(oldUrl), login(newUrl)])
  const urls = [oldUrl, newUrl]
  const ids: string[][] = [[], []]

  try {
    const sources = await Promise.all(clients.map((client, index) => createDocument(
      client,
      urls[index],
      name,
      `copy api parity ${suffix}\n`,
    )))
    const sourceIds = sources.map(source => String(source.id))
    ids[0].push(sourceIds[0])
    ids[1].push(sourceIds[1])

    const copies = [[], []] as Array<Array<{ status: number; type: string; body: unknown }>>
    for (let attempt = 0; attempt < 2; attempt += 1) {
      for (let index = 0; index < clients.length; index += 1) {
        const result = await copy(clients[index], urls[index], sourceIds[index], ROOT)
        expect(result.status, `${urls[index]} copy 第 ${attempt + 1} 次状态异常`).toBe(201)
        copies[index].push(result)
        ids[index].push(String((result.body as Record<string, unknown>).id))
      }
    }

    expect(copies[0].map(result => fileSemantics(result.body)))
      .toEqual(copies[1].map(result => fileSemantics(result.body)))
    expect(copies[0].map(result => (result.body as Record<string, unknown>).name))
      .toEqual([`${name.slice(0, -4)} - 副本.txt`, `${name.slice(0, -4)} - 副本 2.txt`])

    const invalidRequests = [
      [sourceIds[0], MISSING],
      [ROOT, ROOT],
      [MISSING, ROOT],
      [sourceIds[0], sourceIds[0]],
    ] as const
    for (const [source, target] of invalidRequests) {
      const [oldResult, newResult] = await Promise.all([
        copy(clients[0], oldUrl, source, target),
        copy(clients[1], newUrl, source === sourceIds[0] ? sourceIds[1] : source, target === sourceIds[0] ? sourceIds[1] : target),
      ])
      expect(newResult.status, `${source} → ${target} 状态与 reference 不一致`).toBe(oldResult.status)
      expect(newResult.type).toBe(oldResult.type)
      expect(newResult.body).toEqual(oldResult.body)
    }
  } finally {
    await Promise.all([
      cleanup(clients[0], ids[0]),
      cleanup(clients[1], ids[1]),
    ])
    await Promise.all(clients.map(client => client.dispose()))
  }
})

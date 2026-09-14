import { expect, request as createRequest, test, type APIRequestContext } from '@playwright/test'

const ROOT = '00000000-0000-0000-0000-000000000000'

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

async function json(client: APIRequestContext, path: string) {
  const response = await client.get(path)
  expect(response.status(), `${path} 请求失败`).toBe(200)
  return response.json() as Promise<Record<string, unknown>>
}

function keys(value: unknown) {
  return value && typeof value === 'object' && !Array.isArray(value)
    ? Object.keys(value).sort()
    : []
}

function value(value: unknown, key: string) {
  return value && typeof value === 'object' && !Array.isArray(value)
    ? (value as Record<string, unknown>)[key]
    : undefined
}

function assertNumericObjectShape(oldValue: unknown, newValue: unknown, label: string) {
  expect(keys(newValue), `${label} 字段与 reference 不一致`).toEqual(keys(oldValue))
  for (const key of keys(oldValue)) {
    expect(typeof value(newValue, key), `${label}.${key} 类型与 reference 不一致`).toBe('number')
  }
}

function stableItemKeys(value: unknown) {
  if (!Array.isArray(value)) return []
  const fields = new Set<string>()
  for (const item of value) {
    if (!item || typeof item !== 'object' || Array.isArray(item)) continue
    for (const key of Object.keys(item)) {
      if (key !== 'etag') fields.add(key)
    }
  }
  return [...fields].sort()
}

async function remove(client: APIRequestContext, baseUrl: string, id: string) {
  await client.delete(`/api/files/${id}`, { headers: headers(baseUrl) }).catch(() => undefined)
}

test('旧版只读 API 的统计增量、分类 schema 和系统状态 schema 在 Rust 版保持一致', async () => {
  const oldUrl = process.env.E2E_REFERENCE_URL || 'http://127.0.0.1:18080'
  const newUrl = process.env.E2E_NEW_URL || 'http://127.0.0.1:18084'
  const clients = await Promise.all([login(oldUrl), login(newUrl)])
  const urls = [oldUrl, newUrl]
  const ids: string[] = []
  const name = `readonly-api-parity-${crypto.randomUUID().slice(0, 8)}.txt`
  const content = 'readonly API parity\n'

  try {
    const beforeStats = await Promise.all(clients.map(client => json(client, '/api/storage/stats')))
    expect(keys(beforeStats[1])).toEqual(keys(beforeStats[0]))
    expect(typeof beforeStats[0].total_bytes).toBe('number')
    expect(typeof beforeStats[0].file_count).toBe('number')

    const created = await Promise.all(clients.map((client, index) => client.post('/api/documents', {
      headers: headers(urls[index], true),
      data: { parent_id: ROOT, name, content },
    })))
    expect(created.map(response => response.status())).toEqual([201, 201])
    const files = await Promise.all(created.map(response => response.json()))
    ids.push(...files.map(file => String(file.id)))

    const afterStats = await Promise.all(clients.map(client => json(client, '/api/storage/stats')))
    for (const stats of afterStats) {
      expect(typeof stats.total_bytes).toBe('number')
      expect(typeof stats.file_count).toBe('number')
    }
    expect(Number(afterStats[0].total_bytes) - Number(beforeStats[0].total_bytes))
      .toBe(Number(afterStats[1].total_bytes) - Number(beforeStats[1].total_bytes))
    expect(Number(afterStats[0].file_count) - Number(beforeStats[0].file_count))
      .toBe(Number(afterStats[1].file_count) - Number(beforeStats[1].file_count))

    const counts = await Promise.all(clients.map(client => json(client, '/api/library/counts')))
    assertNumericObjectShape(counts[0], counts[1], '/api/library/counts')

    for (const kind of ['book', 'image', 'video', 'audio', 'file']) {
      const results = await Promise.all(clients.map(client => json(client, `/api/library?type=${kind}`)))
      expect(keys(results[1]), `/api/library?type=${kind} 顶层字段不一致`).toEqual(keys(results[0]))
      expect(results[0].type).toBe(kind)
      expect(results[1].type).toBe(kind)
      expect(keys(results[1].counts), `/api/library?type=${kind} counts 字段不一致`).toEqual(keys(results[0].counts))
      const oldItems = results[0].items
      const newItems = results[1].items
      expect(Array.isArray(oldItems)).toBe(true)
      expect(Array.isArray(newItems)).toBe(true)
      if (Array.isArray(oldItems) && oldItems.length && Array.isArray(newItems) && newItems.length) {
        expect(stableItemKeys(newItems), `/api/library?type=${kind} item schema 不一致`)
          .toEqual(stableItemKeys(oldItems))
      }
    }

    const statuses = await Promise.all(clients.map(client => json(client, '/api/system/status')))
    expect(keys(statuses[1])).toEqual(keys(statuses[0]))
    for (const component of ['database', 'storage', 'cache']) {
      const oldComponent = value(statuses[0], component)
      const newComponent = value(statuses[1], component)
      expect(keys(newComponent), `/api/system/status.${component} 字段不一致`).toEqual(keys(oldComponent))
      for (const key of keys(oldComponent)) {
        const oldField = value(oldComponent, key)
        const newField = value(newComponent, key)
        if (typeof oldField === 'number') {
          expect(typeof newField, `/api/system/status.${component}.${key} 类型不一致`).toBe('number')
        } else if (typeof oldField === 'string') {
          expect(typeof newField, `/api/system/status.${component}.${key} 类型不一致`).toBe('string')
        }
      }
    }
  } finally {
    await Promise.all(clients.map((client, index) => Promise.all(ids.map(id => remove(client, urls[index], id)))))
    await Promise.all(clients.map(client => client.dispose()))
  }
})

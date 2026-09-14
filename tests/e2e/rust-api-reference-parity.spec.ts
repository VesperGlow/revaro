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

async function jsonResponse(
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
  let json: unknown = null
  try {
    json = JSON.parse(text)
  } catch {
    // The caller can still compare status and content type for non-JSON APIs.
  }
  return { response, text, json }
}

function objectKeys(value: unknown) {
  return value && typeof value === 'object' && !Array.isArray(value)
    ? Object.keys(value).sort()
    : []
}

function arrayItemKeys(value: unknown) {
  if (!Array.isArray(value)) return []
  const keys = new Set<string>()
  for (const item of value) {
    for (const key of objectKeys(item)) keys.add(key)
  }
  return [...keys].sort()
}

function stableArrayItemKeys(value: unknown) {
  return arrayItemKeys(value).filter(key => key !== 'etag')
}

function objectValue(value: unknown, key: string): unknown {
  return value && typeof value === 'object' && !Array.isArray(value)
    ? (value as Record<string, unknown>)[key]
    : undefined
}

function fileSemantics(value: unknown) {
  const file = value as Record<string, unknown>
  return {
    parent_id: file.parent_id,
    name: file.name,
    kind: file.kind,
    size: file.size,
    mime_type: file.mime_type,
    content_hash: file.content_hash,
    hash_algorithm: file.hash_algorithm,
    status: file.status,
    has_etag: typeof file.etag === 'string',
    has_created_at: typeof file.created_at === 'string',
    has_updated_at: typeof file.updated_at === 'string',
  }
}

async function compareTransport(oldResult: Awaited<ReturnType<typeof jsonResponse>>, newResult: Awaited<ReturnType<typeof jsonResponse>>) {
  expect(newResult.response.status()).toBe(oldResult.response.status())
  expect(newResult.response.headers()['content-type']?.split(';')[0])
    .toBe(oldResult.response.headers()['content-type']?.split(';')[0])
}

async function cleanup(client: APIRequestContext, baseUrl: string, ids: string[]) {
  for (const id of ids) {
    await client.delete(`/api/files/${id}`, { headers: headers(baseUrl) }).catch(() => undefined)
    await client.delete(`/api/trash/${id}`, { headers: headers(baseUrl) }).catch(() => undefined)
  }
}

test('旧版 API 路由、响应字段和错误分流在 Rust 版仍可达', async () => {
  const oldUrl = process.env.E2E_REFERENCE_URL || 'http://127.0.0.1:18080'
  const newUrl = process.env.E2E_NEW_URL || 'http://127.0.0.1:18084'
  const oldClient = await login(oldUrl)
  const newClient = await login(newUrl)

  try {
    const readOnly = [
      '/api/auth/me',
      '/api/auth/totp',
      '/api/storage/stats',
      '/api/library',
      '/api/library?type=book',
      '/api/library/all',
      '/api/library/counts',
      '/api/system/status',
      '/api/tasks',
      `/api/files/${ROOT}`,
      `/api/files/${ROOT}/children`,
      '/api/trash',
      `/api/files/${ROOT}/share`,
    ]

    for (const path of readOnly) {
      const [oldResult, newResult] = await Promise.all([
        jsonResponse(oldClient, oldUrl, path),
        jsonResponse(newClient, newUrl, path),
      ])
      await compareTransport(oldResult, newResult)
      expect(objectKeys(newResult.json), `${path} 顶层字段不一致`).toEqual(objectKeys(oldResult.json))
    }

    const oldSession = await jsonResponse(oldClient, oldUrl, '/api/auth/me')
    const newSession = await jsonResponse(newClient, newUrl, '/api/auth/me')
    expect(newSession.json).toEqual(oldSession.json)

    const oldTotp = await jsonResponse(oldClient, oldUrl, '/api/auth/totp')
    const newTotp = await jsonResponse(newClient, newUrl, '/api/auth/totp')
    expect(newTotp.json).toEqual(oldTotp.json)

    for (const path of ['/api/storage/stats', '/api/library/counts']) {
      const [oldResult, newResult] = await Promise.all([
        jsonResponse(oldClient, oldUrl, path),
        jsonResponse(newClient, newUrl, path),
      ])
      for (const key of objectKeys(oldResult.json)) {
        expect(typeof objectValue(newResult.json, key), `${path}.${key} 类型不一致`).toBe('number')
      }
    }

    const [oldLibrary, newLibrary] = await Promise.all([
      jsonResponse(oldClient, oldUrl, '/api/library'),
      jsonResponse(newClient, newUrl, '/api/library'),
    ])
    expect(objectKeys(newLibrary.json)).toEqual(objectKeys(oldLibrary.json))
    expect(objectKeys(objectValue(newLibrary.json, 'counts')))
      .toEqual(objectKeys(objectValue(oldLibrary.json, 'counts')))
    expect(objectValue(newLibrary.json, 'type')).toBe('file')
    expect(objectValue(oldLibrary.json, 'type')).toBe('file')
    expect(stableArrayItemKeys(objectValue(newLibrary.json, 'items')))
      .toEqual(stableArrayItemKeys(objectValue(oldLibrary.json, 'items')))

    const [oldLibraryAll, newLibraryAll] = await Promise.all([
      jsonResponse(oldClient, oldUrl, '/api/library/all'),
      jsonResponse(newClient, newUrl, '/api/library/all'),
    ])
    expect(objectKeys(objectValue(newLibraryAll.json, 'items')))
      .toEqual(objectKeys(objectValue(oldLibraryAll.json, 'items')))
    const oldBuckets = objectValue(oldLibraryAll.json, 'items') as Record<string, unknown>
    const newBuckets = objectValue(newLibraryAll.json, 'items') as Record<string, unknown>
    for (const bucket of Object.keys(oldBuckets)) {
      expect(Array.isArray(newBuckets[bucket]), `library/all.${bucket} 不是数组`).toBe(true)
      if (Array.isArray(oldBuckets[bucket]) && oldBuckets[bucket].length && Array.isArray(newBuckets[bucket]) && newBuckets[bucket].length) {
        expect(stableArrayItemKeys(newBuckets[bucket])).toEqual(stableArrayItemKeys(oldBuckets[bucket]))
      }
    }

    const [oldRoot, newRoot] = await Promise.all([
      jsonResponse(oldClient, oldUrl, `/api/files/${ROOT}`),
      jsonResponse(newClient, newUrl, `/api/files/${ROOT}`),
    ])
    expect(fileSemantics(objectValue(newRoot.json, 'file'))).toMatchObject({ kind: 'directory', status: 'ready' })
    expect(fileSemantics(objectValue(oldRoot.json, 'file'))).toMatchObject({ kind: 'directory', status: 'ready' })
    expect(objectKeys(objectValue(newRoot.json, 'file'))).toEqual(objectKeys(objectValue(oldRoot.json, 'file')))
    expect(objectKeys(objectValue(newRoot.json, 'breadcrumbs'))).toEqual(objectKeys(objectValue(oldRoot.json, 'breadcrumbs')))

    const missingProbes: Array<[string, string, unknown?]> = [
      [`/api/files/${MISSING}`, 'GET'],
      [`/api/files/${MISSING}/children`, 'GET'],
      [`/api/files/${MISSING}/download`, 'GET'],
      [`/api/files/${MISSING}/preview`, 'GET'],
      [`/api/files/${MISSING}/audio`, 'GET'],
      [`/api/files/${MISSING}/video`, 'GET'],
      [`/api/files/${MISSING}/thumbnail`, 'GET'],
      [`/api/files/${MISSING}/media/progress`, 'GET'],
      [`/api/files/${MISSING}/content`, 'GET'],
      [`/api/files/${MISSING}/book`, 'GET'],
      [`/api/files/${MISSING}/book/progress`, 'GET'],
      [`/api/files/${MISSING}/book/flow`, 'GET'],
      [`/api/files/${MISSING}/book/cover`, 'GET'],
      [`/api/files/${MISSING}/book/assets/0`, 'GET'],
      [`/api/files/${MISSING}/book/flow/chunks/0`, 'GET'],
      [`/api/files/${MISSING}/share`, 'GET'],
      [`/api/files/${MISSING}/media/reanalyze`, 'POST'],
      [`/api/files/${MISSING}/extract`, 'POST'],
      [`/api/tasks/${MISSING}`, 'GET'],
      [`/api/tasks/${MISSING}/cancel`, 'POST'],
      [`/api/tasks/${MISSING}/retry`, 'POST'],
      [`/api/tasks/${MISSING}/input`, 'POST', { password: 'x' }],
      [`/api/uploads/${MISSING}`, 'GET'],
    ]
    for (const [path, method, data] of missingProbes) {
      const [oldResult, newResult] = await Promise.all([
        jsonResponse(oldClient, oldUrl, path, method, data),
        jsonResponse(newClient, newUrl, path, method, data),
      ])
      await compareTransport(oldResult, newResult)
      expect(objectKeys(newResult.json), `${method} ${path} 错误 envelope 不一致`).toEqual(objectKeys(oldResult.json))
      expect(objectValue(newResult.json, 'error')).toEqual(objectValue(oldResult.json, 'error'))
    }

    const [oldUnknown, newUnknown] = await Promise.all([
      jsonResponse(oldClient, oldUrl, '/api/does-not-exist'),
      jsonResponse(newClient, newUrl, '/api/does-not-exist'),
    ])
    await compareTransport(oldUnknown, newUnknown)
    expect(newUnknown.json).toEqual(oldUnknown.json)
  } finally {
    await oldClient.dispose()
    await newClient.dispose()
  }
})

test('旧版文档 API 的创建、读取、保存、下载、分享和回收生命周期保持一致', async () => {
  const oldUrl = process.env.E2E_REFERENCE_URL || 'http://127.0.0.1:18080'
  const newUrl = process.env.E2E_NEW_URL || 'http://127.0.0.1:18084'
  const suffix = crypto.randomUUID().slice(0, 8)
  const name = `api-parity-${suffix}.txt`
  const renamed = `api-parity-renamed-${suffix}.txt`
  const content = `API parity ${suffix}\n第二行\n`
  const updatedContent = `${content}updated\n`
  const clients = await Promise.all([login(oldUrl), login(newUrl)])
  const ids: string[][] = [[], []]

  try {
    const created = await Promise.all(clients.map((client, index) => jsonResponse(
      client,
      index === 0 ? oldUrl : newUrl,
      '/api/documents',
      'POST',
      { parent_id: ROOT, name, content },
    )))
    expect(created[0].response.status()).toBe(201)
    await compareTransport(created[0], created[1])
    for (const [index, result] of created.entries()) {
      const file = result.json as Record<string, unknown>
      expect(fileSemantics(file)).toEqual({
        parent_id: ROOT,
        name,
        kind: 'file',
        size: Buffer.byteLength(content),
        mime_type: 'text/plain; charset=utf-8',
        content_hash: created[0].json && (created[0].json as Record<string, unknown>).content_hash,
        hash_algorithm: 'sha256',
        status: 'ready',
        has_etag: true,
        has_created_at: true,
        has_updated_at: true,
      })
      ids[index].push(String(file.id))
    }
    expect((created[1].json as Record<string, unknown>).content_hash)
      .toBe((created[0].json as Record<string, unknown>).content_hash)

    const read = await Promise.all(clients.map((client, index) => jsonResponse(
      client,
      index === 0 ? oldUrl : newUrl,
      `/api/files/${ids[index][0]}/content`,
    )))
    expect(read[0].json && (read[0].json as Record<string, unknown>).content).toBe(content)
    expect(read[1].json && (read[1].json as Record<string, unknown>).content).toBe(content)
    expect(objectKeys(read[0].json)).toEqual(objectKeys(read[1].json))
    expect(typeof objectValue(read[0].json, 'etag')).toBe('string')
    expect(typeof objectValue(read[1].json, 'etag')).toBe('string')

    const updated = await Promise.all(clients.map((client, index) => jsonResponse(
      client,
      index === 0 ? oldUrl : newUrl,
      `/api/files/${ids[index][0]}/content`,
      'PUT',
      { content: updatedContent, etag: objectValue(read[index].json, 'etag') },
    )))
    expect(updated[0].response.status()).toBe(200)
    await compareTransport(updated[0], updated[1])
    expect(fileSemantics(updated[0].json)).toEqual(fileSemantics(updated[1].json))
    expect(fileSemantics(updated[0].json).size).toBe(Buffer.byteLength(updatedContent))

    const downloads = await Promise.all(clients.map((client, index) => client.get(
      `/api/files/${ids[index][0]}/download`,
      { headers: headers(index === 0 ? oldUrl : newUrl) },
    )))
    expect(downloads[0].status()).toBe(200)
    expect(downloads[1].status()).toBe(200)
    expect(downloads[0].headers()['content-type']).toBe(downloads[1].headers()['content-type'])
    expect(downloads[0].headers()['content-disposition']).toContain(`filename*=UTF-8''${encodeURIComponent(name)}`)
    expect(downloads[1].headers()['content-disposition']).toContain(`filename*=UTF-8''${encodeURIComponent(name)}`)
    expect((await downloads[0].body()).equals(await downloads[1].body())).toBe(true)

    const ranges = await Promise.all(clients.map((client, index) => client.get(
      `/api/files/${ids[index][0]}/download`,
      { headers: { ...headers(index === 0 ? oldUrl : newUrl), Range: 'bytes=2-8' } },
    )))
    expect(ranges[0].status()).toBe(206)
    expect(ranges[1].status()).toBe(206)
    expect(ranges[0].headers()['content-range']).toBe(ranges[1].headers()['content-range'])
    expect((await ranges[0].body()).equals(await ranges[1].body())).toBe(true)

    const preview = await Promise.all(clients.map((client, index) => jsonResponse(
      client,
      index === 0 ? oldUrl : newUrl,
      `/api/files/${ids[index][0]}/preview`,
    )))
    expect(preview[0].response.status()).toBe(415)
    await compareTransport(preview[0], preview[1])
    expect(preview[0].json).toEqual(preview[1].json)

    const share = await Promise.all(clients.map((client, index) => jsonResponse(
      client,
      index === 0 ? oldUrl : newUrl,
      `/api/files/${ids[index][0]}/share`,
      'POST',
    )))
    expect(share[0].response.status()).toBe(201)
    await compareTransport(share[0], share[1])
    expect(objectValue(share[0].json, 'active')).toBe(true)
    expect(objectValue(share[1].json, 'active')).toBe(true)
    expect(new URL(String(objectValue(share[0].json, 'url'))).pathname).toMatch(/^\/s\/.+/)
    expect(new URL(String(objectValue(share[1].json, 'url'))).pathname).toMatch(/^\/s\/.+/)

    const revoke = await Promise.all(clients.map((client, index) => jsonResponse(
      client,
      index === 0 ? oldUrl : newUrl,
      `/api/files/${ids[index][0]}/share`,
      'DELETE',
    )))
    expect(revoke[0].response.status()).toBe(204)
    expect(revoke[1].response.status()).toBe(204)

    const renamedResults = await Promise.all(clients.map((client, index) => jsonResponse(
      client,
      index === 0 ? oldUrl : newUrl,
      `/api/files/${ids[index][0]}`,
      'PATCH',
      { name: renamed },
    )))
    expect(renamedResults[0].response.status()).toBe(200)
    expect(renamedResults[1].response.status()).toBe(200)
    expect(fileSemantics(renamedResults[0].json)).toEqual(fileSemantics(renamedResults[1].json))
    expect(objectValue(renamedResults[0].json, 'name')).toBe(renamed)

    const copies = await Promise.all(clients.map((client, index) => jsonResponse(
      client,
      index === 0 ? oldUrl : newUrl,
      `/api/files/${ids[index][0]}/copy`,
      'POST',
      { parent_id: ROOT },
    )))
    expect(copies[0].response.status()).toBe(201)
    expect(copies[1].response.status()).toBe(201)
    expect(fileSemantics(copies[0].json)).toEqual(fileSemantics(copies[1].json))
    for (const [index, result] of copies.entries()) ids[index].push(String((result.json as Record<string, unknown>).id))

    const remove = await Promise.all(clients.map((client, index) => jsonResponse(
      client,
      index === 0 ? oldUrl : newUrl,
      `/api/files/${ids[index][0]}`,
      'DELETE',
    )))
    expect(remove[0].response.status()).toBe(204)
    expect(remove[1].response.status()).toBe(204)
    const trash = await Promise.all(clients.map((client, index) => jsonResponse(
      client,
      index === 0 ? oldUrl : newUrl,
      '/api/trash',
    )))
    for (const [index, result] of trash.entries()) {
      const items = objectValue(result.json, 'items') as Array<Record<string, unknown>>
      expect(items.some(item => item.id === ids[index][0] && item.name === renamed)).toBe(true)
    }

    const restore = await Promise.all(clients.map((client, index) => jsonResponse(
      client,
      index === 0 ? oldUrl : newUrl,
      `/api/trash/${ids[index][0]}/restore`,
      'POST',
    )))
    expect(restore[0].response.status()).toBe(204)
    expect(restore[1].response.status()).toBe(204)
    const removeAgain = await Promise.all(clients.map((client, index) => jsonResponse(
      client,
      index === 0 ? oldUrl : newUrl,
      `/api/files/${ids[index][0]}`,
      'DELETE',
    )))
    expect(removeAgain[0].response.status()).toBe(204)
    expect(removeAgain[1].response.status()).toBe(204)
    const purge = await Promise.all(clients.map((client, index) => jsonResponse(
      client,
      index === 0 ? oldUrl : newUrl,
      `/api/trash/${ids[index][0]}`,
      'DELETE',
    )))
    expect(purge[0].response.status()).toBe(204)
    expect(purge[1].response.status()).toBe(204)
  } finally {
    await Promise.all(clients.map((client, index) => cleanup(client, index === 0 ? oldUrl : newUrl, ids[index])))
    await Promise.all(clients.map(client => client.dispose()))
  }
})

test('旧版清空回收站 API 的全量删除行为在 Rust 版保持一致', async () => {
  const oldUrl = process.env.E2E_REFERENCE_URL || 'http://127.0.0.1:18080'
  const newUrl = process.env.E2E_NEW_URL || 'http://127.0.0.1:18084'
  const suffix = crypto.randomUUID().slice(0, 8)
  const name = `api-empty-trash-${suffix}.txt`
  const clients = await Promise.all([login(oldUrl), login(newUrl)])
  const ids: string[][] = [[], []]

  try {
    const created = await Promise.all(clients.map((client, index) => jsonResponse(
      client,
      index === 0 ? oldUrl : newUrl,
      '/api/documents',
      'POST',
      { parent_id: ROOT, name, content: `empty trash ${suffix}\n` },
    )))
    for (const [index, result] of created.entries()) {
      expect(result.response.status()).toBe(201)
      ids[index].push(String(objectValue(result.json, 'id')))
    }

    await Promise.all(clients.map((client, index) => client.delete(
      `/api/files/${ids[index][0]}`,
      { headers: headers(index === 0 ? oldUrl : newUrl) },
    )))

    const before = await Promise.all(clients.map((client, index) => jsonResponse(
      client,
      index === 0 ? oldUrl : newUrl,
      '/api/trash',
    )))
    for (const [index, result] of before.entries()) {
      const items = objectValue(result.json, 'items') as Array<Record<string, unknown>>
      expect(items.some(item => item.id === ids[index][0] && item.name === name)).toBe(true)
    }

    const emptied = await Promise.all(clients.map((client, index) => jsonResponse(
      client,
      index === 0 ? oldUrl : newUrl,
      '/api/trash',
      'DELETE',
    )))
    await compareTransport(emptied[0], emptied[1])
    expect(emptied[0].response.status()).toBe(204)

    const after = await Promise.all(clients.map((client, index) => jsonResponse(
      client,
      index === 0 ? oldUrl : newUrl,
      '/api/trash',
    )))
    for (const [index, result] of after.entries()) {
      const items = objectValue(result.json, 'items') as Array<Record<string, unknown>>
      expect(items.some(item => item.id === ids[index][0])).toBe(false)
    }
  } finally {
    await Promise.all(clients.map((client, index) => cleanup(client, index === 0 ? oldUrl : newUrl, ids[index])))
    await Promise.all(clients.map(client => client.dispose()))
  }
})

test('旧版取消未完成上传 API 的状态和可见文件清理在 Rust 版保持一致', async () => {
  const oldUrl = process.env.E2E_REFERENCE_URL || 'http://127.0.0.1:18080'
  const newUrl = process.env.E2E_NEW_URL || 'http://127.0.0.1:18084'
  const suffix = crypto.randomUUID().slice(0, 8)
  const name = `api-abort-upload-${suffix}.bin`
  const clients = await Promise.all([login(oldUrl), login(newUrl)])
  const uploadIds: string[][] = [[], []]
  const fileIds: string[][] = [[], []]

  try {
    const created = await Promise.all(clients.map((client, index) => jsonResponse(
      client,
      index === 0 ? oldUrl : newUrl,
      '/api/uploads',
      'POST',
      { parent_id: ROOT, name, size: 32, mime_type: 'application/octet-stream' },
    )))
    for (const [index, result] of created.entries()) {
      expect(result.response.status()).toBe(201)
      uploadIds[index].push(String(objectValue(result.json, 'upload_id')))
      fileIds[index].push(String(objectValue(result.json, 'file_id')))
    }

    const aborted = await Promise.all(clients.map((client, index) => jsonResponse(
      client,
      index === 0 ? oldUrl : newUrl,
      `/api/uploads/${uploadIds[index][0]}`,
      'DELETE',
    )))
    await compareTransport(aborted[0], aborted[1])
    expect(aborted[0].response.status()).toBe(204)

    const uploadAfter = await Promise.all(clients.map((client, index) => jsonResponse(
      client,
      index === 0 ? oldUrl : newUrl,
      `/api/uploads/${uploadIds[index][0]}`,
    )))
    await compareTransport(uploadAfter[0], uploadAfter[1])
    expect(uploadAfter[0].response.status()).toBe(404)
    expect(objectKeys(uploadAfter[0].json)).toEqual(['error'])
    expect(objectValue(uploadAfter[0].json, 'error')).toEqual(objectValue(uploadAfter[1].json, 'error'))

    const fileAfter = await Promise.all(clients.map((client, index) => jsonResponse(
      client,
      index === 0 ? oldUrl : newUrl,
      `/api/files/${fileIds[index][0]}`,
    )))
    await compareTransport(fileAfter[0], fileAfter[1])
    expect(fileAfter[0].response.status()).toBe(404)
    expect(objectKeys(fileAfter[0].json)).toEqual(['error'])
    expect(objectValue(fileAfter[0].json, 'error')).toEqual(objectValue(fileAfter[1].json, 'error'))
  } finally {
    await Promise.all(clients.map((client, index) => {
      return Promise.all([
        ...uploadIds[index].map(uploadId => client.delete(`/api/uploads/${uploadId}`, { headers: headers(index === 0 ? oldUrl : newUrl) }).catch(() => undefined)),
        ...fileIds[index].map(fileId => client.delete(`/api/files/${fileId}`, { headers: headers(index === 0 ? oldUrl : newUrl) }).catch(() => undefined)),
      ])
    }))
    await Promise.all(clients.map(client => client.dispose()))
  }
})

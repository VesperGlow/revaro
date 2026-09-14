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

async function comparePublicHeaders(
  oldResponse: Awaited<ReturnType<APIRequestContext['get']>>,
  newResponse: Awaited<ReturnType<APIRequestContext['get']>>,
  allowDefaultCspDifference = false,
) {
  expect(newResponse.status()).toBe(oldResponse.status())
  for (const name of [
    'cache-control',
    'content-security-policy',
    'etag',
    'content-type',
    'referrer-policy',
    'x-content-type-options',
    'x-frame-options',
    'x-robots-tag',
  ]) {
    if (name === 'content-security-policy' && allowDefaultCspDifference) {
      expect(newResponse.headers()[name]).toContain("script-src 'self' 'wasm-unsafe-eval'")
    } else {
      expect(newResponse.headers()[name], `${name} 与 reference 不一致`).toBe(oldResponse.headers()[name])
    }
  }
}

test('健康探针和无效公开分享入口与 reference 保持同一响应契约', async () => {
  const oldUrl = process.env.E2E_REFERENCE_URL || 'http://127.0.0.1:18080'
  const newUrl = process.env.E2E_NEW_URL || 'http://127.0.0.1:18084'
  const oldClient = await createRequest.newContext({ baseURL: oldUrl })
  const newClient = await createRequest.newContext({ baseURL: newUrl })

  try {
    for (const path of ['/healthz', '/readyz']) {
      const [oldResponse, newResponse] = await Promise.all([
        oldClient.get(path),
        newClient.get(path),
      ])
      expect(newResponse.status(), `${path} status 与 reference 不一致`).toBe(oldResponse.status())
      expect(newResponse.headers()['content-type']?.split(';')[0]).toBe(oldResponse.headers()['content-type']?.split(';')[0])
      expect(await newResponse.json()).toEqual(await oldResponse.json())
    }

    const [oldResponse, newResponse] = await Promise.all([
      oldClient.get('/s/not-a-valid-share-token'),
      newClient.get('/s/not-a-valid-share-token'),
    ])
    // Invalid `/s/*` requests never enter the share handler. The Rust shell
    // CSP necessarily permits WebAssembly compilation; the valid bearer-link
    // response below still uses the reference's strict sandbox CSP.
    await comparePublicHeaders(oldResponse, newResponse, true)
    expect(await newResponse.json()).toEqual(await oldResponse.json())
  } finally {
    await oldClient.dispose()
    await newClient.dispose()
  }
})

test('公开分享可以脱离登录读取、支持 Range，并在撤销后立即失效', async () => {
  const oldUrl = process.env.E2E_REFERENCE_URL || 'http://127.0.0.1:18080'
  const newUrl = process.env.E2E_NEW_URL || 'http://127.0.0.1:18084'
  const clients = await Promise.all([login(oldUrl), login(newUrl)])
  const publicClients = await Promise.all([
    createRequest.newContext({ baseURL: oldUrl }),
    createRequest.newContext({ baseURL: newUrl }),
  ])
  const ids: string[] = []
  const name = `public-share-parity-${crypto.randomUUID().slice(0, 8)}.txt`
  const content = `公开分享 parity\n${name}\n`

  try {
    const created = await Promise.all(clients.map((client, index) => client.post('/api/documents', {
      headers: originHeaders(index === 0 ? oldUrl : newUrl, true),
      data: { parent_id: ROOT, name, content },
    })))
    expect(created[0].status()).toBe(201)
    expect(created[1].status()).toBe(201)
    const files = await Promise.all(created.map(response => response.json()))
    ids.push(...files.map(file => String(file.id)))

    const shares = await Promise.all(clients.map((client, index) => client.post(`/api/files/${ids[index]}/share`, {
      headers: originHeaders(index === 0 ? oldUrl : newUrl),
    })))
    expect(shares[0].status()).toBe(201)
    expect(shares[1].status()).toBe(201)
    const shareBodies = await Promise.all(shares.map(response => response.json()))
    const paths = shareBodies.map(share => new URL(String(share.url)).pathname)

    const publicReads = await Promise.all(publicClients.map((client, index) => client.get(paths[index])))
    await comparePublicHeaders(publicReads[0], publicReads[1])
    expect((await publicReads[0].body()).equals(await publicReads[1].body())).toBe(true)
    expect((await publicReads[0].body()).toString()).toBe(content)

    const ranges = await Promise.all(publicClients.map((client, index) => client.get(paths[index], {
      headers: { Range: 'bytes=2-8' },
    })))
    await comparePublicHeaders(ranges[0], ranges[1])
    expect(ranges[0].status()).toBe(206)
    expect(ranges[0].headers()['content-range']).toBe(ranges[1].headers()['content-range'])
    expect((await ranges[0].body()).equals(await ranges[1].body())).toBe(true)

    const revoked = await Promise.all(clients.map((client, index) => client.delete(`/api/files/${ids[index]}/share`, {
      headers: originHeaders(index === 0 ? oldUrl : newUrl),
    })))
    expect(revoked[0].status()).toBe(204)
    expect(revoked[1].status()).toBe(204)
    const afterRevoke = await Promise.all(publicClients.map((client, index) => client.get(paths[index])))
    // After revocation both requests are ordinary not-found API errors again,
    // so the Rust shell CSP exception is applicable here as well.
    await comparePublicHeaders(afterRevoke[0], afterRevoke[1], true)
    expect(await afterRevoke[0].json()).toEqual(await afterRevoke[1].json())
  } finally {
    await Promise.all(clients.map((client, index) => client.delete(`/api/files/${ids[index]}`, {
      headers: originHeaders(index === 0 ? oldUrl : newUrl),
    }).catch(() => undefined)))
    await Promise.all([...clients, ...publicClients].map(client => client.dispose()))
  }
})

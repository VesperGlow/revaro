import { expect, test, type Page } from '@playwright/test'
import { login, navigate, openMusicPlayer, uploadFixture, selectMusicMode } from './helpers'

test.use({ reducedMotion: 'reduce' })

function wav() {
  const bytes = Buffer.alloc(44 + 90 * 8000 * 2)
  bytes.write('RIFF'); bytes.writeUInt32LE(bytes.length - 8, 4); bytes.write('WAVEfmt ', 8)
  bytes.writeUInt32LE(16, 16); bytes.writeUInt16LE(1, 20); bytes.writeUInt16LE(1, 22)
  bytes.writeUInt32LE(8000, 24); bytes.writeUInt32LE(16000, 28)
  bytes.writeUInt16LE(2, 32); bytes.writeUInt16LE(16, 34)
  bytes.write('data', 36); bytes.writeUInt32LE(bytes.length - 44, 40)
  return bytes
}

async function fixture(page: Page, run: (file: { id: string, name: string }, headers: Record<string, string>) => Promise<void>) {
  await login(page)
  const headers = { origin: new URL(page.url()).origin }
  const result = await page.request.post('/api/directories', { headers, data: {
    parent_id: '00000000-0000-0000-0000-000000000000', name: `progress-sync-${Date.now()}`,
  } })
  expect(result.status()).toBe(201)
  const directory = await result.json()
  try {
    const file = await uploadFixture(page, directory.id, `${directory.name}.wav`, 'audio/wav', wav())
    await run(file, headers)
  } finally {
    await page.goto('/files')
    await page.request.delete(`/api/files/${directory.id}`, { headers })
    await page.request.delete(`/api/trash/${directory.id}`, { headers })
  }
}

async function open(page: Page, file: { name: string }) {
  await navigate(page, '音乐')
  await page.getByRole('button', { name: `打开 ${file.name}`, exact: true }).click()
  await openMusicPlayer(page)
  await expect(page.getByRole('button', { name: '暂停音乐', exact: true })).toBeVisible()
  await page.getByRole('button', { name: '暂停音乐', exact: true }).click()
}

async function seek(page: Page, position: number) {
  await page.getByLabel('音乐播放进度', { exact: true }).evaluate((input: HTMLInputElement, position) => {
    input.value = String(position)
    input.dispatchEvent(new Event('change', { bubbles: true }))
  }, position)
  await expect.poll(() => page.locator('audio').first().evaluate((audio: HTMLAudioElement) => audio.currentTime)).toBeCloseTo(position, 1)
}

async function saved(page: Page, id: string) { return (await (await page.request.get(`/api/files/${id}/media/progress`)).json()) }
async function position(page: Page) { return page.locator('audio').first().evaluate((audio: HTMLAudioElement) => audio.currentTime) }

async function faults(page: Page) {
  await page.addInitScript(() => {
    const original = window.fetch.bind(window)
    window.fetch = (input, init) => {
      const request = input instanceof Request ? input : new Request(input, init)
      if (localStorage.getItem('test-progress-failure') === '1' && request.method === 'PUT'
        && new URL(request.url).pathname.endsWith('/media/progress')) {
        return Promise.resolve(new Response('{}', { status: 503 }))
      }
      return original(input, init)
    }
  })
}

test('independent devices hand off progress, restore server history and preserve an intentional zero', async ({ page, browser }) => {
  await fixture(page, async file => {
    await open(page, file)
    await seek(page, 20)
    await expect.poll(async () => (await saved(page, file.id)).position).toBe(20)
    const second = await browser.newContext({ baseURL: new URL(page.url()).origin })
    const other = await second.newPage()
    try {
      await login(other)
      await expect.poll(() => other.locator('audio').first().getAttribute('src')).toBe(`/api/files/${file.id}/preview`)
      await expect.poll(() => position(other)).toBeCloseTo(20, 0)
      await expect(other.locator('audio').first()).toHaveJSProperty('paused', true)
      await openMusicPlayer(other)
      await seek(other, 65)
      await expect.poll(async () => (await saved(page, file.id)).position).toBe(65)
      await expect.poll(() => position(page)).toBeCloseTo(65, 1)
      // The local queue session deliberately contains an obsolete position.
      await page.evaluate(id => {
        const key = 'revaro-listening:admin'
        const session = JSON.parse(localStorage.getItem(key)!)
        session.position = 12
        localStorage.setItem(key, JSON.stringify(session))
        localStorage.setItem(`revaro-audio-position:${id}`, '12')
      }, file.id)
      await page.reload()
      await expect.poll(() => position(page)).toBeCloseTo(65, 1)
      await seek(other, 0)
      await expect.poll(async () => (await saved(page, file.id)).position).toBe(0)
      await expect.poll(() => position(page)).toBe(0)
      await page.reload()
      await expect.poll(() => page.locator('audio').first().getAttribute('src')).toBe(`/api/files/${file.id}/preview`)
      await expect.poll(() => position(page)).toBe(0)
      expect((await saved(page, file.id)).completed).toBe(false)
    } finally { await second.close() }
  })
})

test('failed progress writes survive reload and retry with their original causal version', async ({ page }) => {
  await faults(page)
  await fixture(page, async file => {
    await open(page, file)
    await seek(page, 20)
    await expect.poll(async () => (await saved(page, file.id)).position).toBe(20)
    await page.evaluate(() => localStorage.setItem('test-progress-failure', '1'))
    await seek(page, 45)
    await expect.poll(() => page.evaluate(() => Object.keys(localStorage)
      .filter(key => key.startsWith('revaro-media-outbox-v1:'))
      .map(key => JSON.parse(localStorage.getItem(key)!).request.position))).toContain(45)
    expect((await saved(page, file.id)).position).toBe(20)
    await page.reload()
    await expect.poll(() => position(page)).toBeCloseTo(45, 1)
    await page.evaluate(() => {
      localStorage.removeItem('test-progress-failure')
      window.dispatchEvent(new Event('online'))
    })
    await expect.poll(async () => (await saved(page, file.id)).position).toBe(45)
    await expect.poll(() => page.evaluate(() => Object.keys(localStorage)
      .filter(key => key.startsWith('revaro-media-outbox-v1:')).length)).toBe(0)
  })
})

test('a disconnected old device cannot overwrite progress after another device takes over', async ({ page, browser }) => {
  await faults(page)
  await fixture(page, async file => {
    await open(page, file)
    await seek(page, 20)
    await expect.poll(async () => (await saved(page, file.id)).position).toBe(20)
    await page.evaluate(() => localStorage.setItem('test-progress-failure', '1'))
    await seek(page, 45)
    const second = await browser.newContext({ baseURL: new URL(page.url()).origin })
    const other = await second.newPage()
    try {
      await login(other)
      await open(other, file)
      await seek(other, 65)
      await expect.poll(async () => (await saved(page, file.id)).position).toBe(65)
      const remote = await saved(page, file.id)
      await page.evaluate(() => {
        localStorage.removeItem('test-progress-failure')
        window.dispatchEvent(new Event('online'))
      })
      await expect.poll(() => position(page)).toBeCloseTo(65, 1)
      await expect.poll(() => page.evaluate(() => Object.keys(localStorage)
        .filter(key => key.startsWith('revaro-media-outbox-v1:')).length)).toBe(0)
      expect(await saved(page, file.id)).toEqual(remote)
    } finally { await second.close() }
  })
})

test('decoder zero clocks and stale seeked events preserve the timeline and durable progress', async ({ page }) => {
  await fixture(page, async file => {
    await open(page, file)
    await seek(page, 50)
    await expect.poll(async () => (await saved(page, file.id)).position).toBe(50)
    await page.locator('audio').first().evaluate((audio: HTMLAudioElement) => {
      Object.defineProperty(audio, 'currentTime', { configurable: true, get: () => 0 })
      Object.defineProperty(audio, 'readyState', { configurable: true, get: () => 0 })
      for (const event of ['timeupdate', 'seeked', 'pause', 'durationchange']) audio.dispatchEvent(new Event(event))
    })
    await expect(page.getByLabel('音乐播放进度', { exact: true })).toHaveValue('50')
    expect((await saved(page, file.id)).position).toBe(50)
    await page.locator('audio').first().evaluate(audio => {
      delete (audio as any).currentTime
      delete (audio as any).readyState
    })
    await page.reload()
    await expect.poll(() => position(page)).toBeCloseTo(50, 1)
  })
})

test('an unfinished track resumes in its final seconds', async ({ page }) => {
  await fixture(page, async file => {
    await open(page, file)
    await seek(page, 88)
    await expect.poll(async () => (await saved(page, file.id)).position).toBe(88)
    await page.reload()
    await expect.poll(() => position(page)).toBeCloseTo(88, 1)
  })
})

test('a committed write with a lost acknowledgment retries without changing its version', async ({ page }) => {
  await page.addInitScript(() => {
    const original = window.fetch.bind(window)
    window.fetch = async (input, init) => {
      const request = new Request(input, init)
      const response = await original(input, init)
      if (request.method === 'PUT' && new URL(request.url).pathname.endsWith('/media/progress')
        && localStorage.getItem('test-lost-ack') === '1') {
        localStorage.removeItem('test-lost-ack')
        localStorage.setItem('test-committed-ack', await response.clone().text())
        throw new TypeError('response lost after commit')
      }
      return response
    }
  })
  await fixture(page, async file => {
    await open(page, file)
    await seek(page, 20)
    await expect.poll(async () => (await saved(page, file.id)).position).toBe(20)
    const previous = await saved(page, file.id)
    await page.evaluate(({ file, previous }) => {
      localStorage.setItem('test-lost-ack', '1')
      localStorage.setItem('revaro-media-outbox-v1:admin:lost-ack-writer', JSON.stringify({
        file: file.id, request: { position: 47, duration: 90, completed: false,
          sync: { writer: 'lost-ack-writer', sequence: 1, base_revision: previous.revision } },
      }))
    }, { file, previous })
    await page.reload()
    await expect.poll(() => page.evaluate(() => localStorage.getItem('test-committed-ack'))).not.toBe(null)
    const committed = await page.evaluate(() => JSON.parse(localStorage.getItem('test-committed-ack')!))
    await expect.poll(() => page.evaluate(() => Object.keys(localStorage)
      .filter(key => key.startsWith('revaro-media-outbox-v1:')).length)).toBe(0)
    expect(await saved(page, file.id)).toMatchObject({ position: 47, revision: committed.revision })
    await page.reload()
    await expect.poll(() => position(page)).toBeCloseTo(47, 1)
  })
})

test('stopping before a delayed history read completes remains stopped after reload', async ({ page }) => {
  await fixture(page, async (file, headers) => {
    await open(page, file)
    await seek(page, 42)
    await expect.poll(async () => (await saved(page, file.id)).position).toBe(42)
    await page.getByLabel('播放器更多操作', { exact: true }).click()
    await page.getByRole('button', { name: '结束播放', exact: true }).click()
    // Delay the actual history response at the application boundary. WebKit
    // cannot consistently route requests passing through a worker controller.
    await page.evaluate(path => {
      const original = window.fetch.bind(window)
      const state = (window as any).heldProgress = { captured: false, enabled: true, releases: [] as (() => void)[], original }
      window.fetch = async (input, init) => {
        const request = new Request(input, init)
        const response = await original(input, init)
        if (state.enabled && request.method === 'GET' && new URL(request.url).pathname === path) {
          state.captured = true
          await new Promise<void>(resolve => state.releases.push(resolve))
        }
        return response
      }
    }, `/api/files/${file.id}/media/progress`)
    const release = () => page.evaluate(() => {
      const state = (window as any).heldProgress
      state.enabled = false
      for (const resolve of state.releases.splice(0)) resolve()
      window.fetch = state.original
    })
    try {
      await navigate(page, '音乐')
      await page.getByRole('button', { name: `打开 ${file.name}`, exact: true }).click()
      await expect.poll(() => page.evaluate(() => (window as any).heldProgress.captured)).toBe(true)
      await openMusicPlayer(page)
      await page.getByLabel('播放器更多操作', { exact: true }).click()
      await page.getByRole('button', { name: '结束播放', exact: true }).click()
      await expect.poll(async () => (await (await page.request.get('/api/listening/session')).json()).queue.track).toBe(null)
      expect((await saved(page, file.id)).position).toBe(42)
      await release()
      await page.reload()
      await expect(page.locator('.music-orb, .music-dock')).toHaveCount(0)
      expect((await (await page.request.get('/api/listening/session', { headers })).json()).queue.track).toBe(null)
    } finally { await release().catch(() => {}) }
  })
})

test('a new device restores the current track, complete queue and mode and observes a remote stop', async ({ page, browser }) => {
  await fixture(page, async (file, headers) => {
    const parent = (await (await page.request.get(`/api/files/${file.id}`)).json()).file.parent_id
    const next = await uploadFixture(page, parent, `queue-second-${Date.now()}.wav`, 'audio/wav', wav())
    const result = await page.request.post('/api/library/collections', { headers, data: { name: file.name, kind: 'audio' } })
    expect(result.status()).toBe(200)
    const collection = await result.json()
    for (const track of [file, next]) await page.request.put(`/api/library/collections/${collection.id}/items/${track.id}`, { headers })
    const second = await browser.newContext({ baseURL: new URL(page.url()).origin })
    try {
      await navigate(page, '音乐')
      await page.getByLabel('选择集合', { exact: true }).click()
      await page.getByRole('button', { name: `${file.name} · 2`, exact: true }).click()
      await page.getByRole('button', { name: `打开 ${next.name}`, exact: true }).click()
      await openMusicPlayer(page)
      await expect(page.getByRole('button', { name: '暂停音乐', exact: true })).toBeVisible()
      await page.getByRole('button', { name: '暂停音乐', exact: true }).click()
      await seek(page, 35)
      await selectMusicMode(page, 'repeat-all')
      await expect.poll(async () => (await (await page.request.get('/api/listening/session')).json()).queue.mode).toBe('repeat-all')
      const other = await second.newPage()
      await login(other)
      await expect.poll(() => other.locator('audio').first().getAttribute('src')).toBe(`/api/files/${next.id}/preview`)
      await expect.poll(() => position(other)).toBeCloseTo(35, 1)
      await expect(other.locator('audio').first()).toHaveJSProperty('paused', true)
      await openMusicPlayer(other)
      await expect(other.locator('.dock-track small')).toContainText('/ 2 轨')
      await expect(other.locator('.music-dock .playback-mode')).toHaveAttribute('data-playback-mode', 'repeat-all')
      await page.getByLabel('播放器更多操作', { exact: true }).click()
      await page.getByRole('button', { name: '结束播放', exact: true }).click()
      await expect.poll(async () => (await (await page.request.get('/api/listening/session')).json()).queue.track).toBe(null)
      await expect(other.locator('.music-orb, .music-dock')).toHaveCount(0)
      await other.reload()
      await expect(other.locator('.music-orb, .music-dock')).toHaveCount(0)
    } finally {
      await second.close()
      await page.request.delete(`/api/library/collections/${collection.id}`, { headers })
    }
  })
})

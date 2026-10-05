import { expect, test, type Page } from '@playwright/test'
import { login, navigate, enterSelectionMode } from './helpers'

function wav() {
  const data = Buffer.alloc(44 + 16000 * 60)
  data.write('RIFF'); data.writeUInt32LE(data.length - 8, 4); data.write('WAVEfmt ', 8)
  data.writeUInt32LE(16, 16); data.writeUInt16LE(1, 20); data.writeUInt16LE(1, 22)
  data.writeUInt32LE(8000, 24); data.writeUInt32LE(16000, 28)
  data.writeUInt16LE(2, 32); data.writeUInt16LE(16, 34)
  data.write('data', 36); data.writeUInt32LE(data.length - 44, 40)
  return data
}

async function upload(page: Page, parent_id: string, name: string) {
  const headers = { origin: new URL(page.url()).origin }
  const data = wav()
  const created = await page.request.post('/api/uploads', { headers, data: { parent_id, name, mime_type: 'audio/wav', size: data.length } })
  expect(created.status()).toBe(201)
  const session = await created.json()
  expect((await page.request.put(`/api/uploads/${session.upload_id}/data`, { headers, data })).ok()).toBeTruthy()
  const completed = await page.request.post(`/api/uploads/${session.upload_id}/complete`, { headers, data: { parts: [] } })
  expect(completed.ok()).toBeTruthy()
  return completed.json()
}

for (const device of [{ name: 'desktop', width: 1600, hasTouch: false }, { name: 'mobile', width: 390, hasTouch: true }, { name: 'compact', width: 320, hasTouch: true }]) {
  test.describe(device.name, () => {
    test.use({ viewport: { width: device.width, height: 900 }, hasTouch: device.hasTouch })

    test('compact rows preserve playback and provide working per-song menus', async ({ page }) => {
      await login(page)
      const headers = { origin: new URL(page.url()).origin }
      const prefix = `song-row-${Date.now()}`
      const created = await page.request.post('/api/directories', { headers, data: { parent_id: '00000000-0000-0000-0000-000000000000', name: prefix } })
      const { id } = await created.json()
      let collectionId: string | undefined
      try {
        const first = await upload(page, id, `${prefix}-清晨与远方${'很长的曲目标题'.repeat(7)}.wav`)
        const second = await upload(page, id, `${prefix}-夜色.wav`)
        await navigate(page, '音乐')
        const playAll = page.getByRole('button', { name: '播放全部', exact: true })
        const createPlaylist = page.getByRole('button', { name: '新建歌单', exact: true })
        await expect(playAll).toHaveAttribute('title', '播放全部')
        await expect(playAll).toHaveText('')
        await expect(playAll.locator('svg')).toHaveCount(1)
        const playAllBounds = (await playAll.boundingBox())!
        const createBounds = (await createPlaylist.boundingBox())!
        expect([playAllBounds.width, playAllBounds.height]).toEqual([createBounds.width, createBounds.height])
        expect(playAllBounds.width).toBe(playAllBounds.height)
        expect(await playAll.evaluate(element => getComputedStyle(element).backgroundColor))
          .toBe(await page.evaluate(() => {
            const probe = document.createElement('div')
            probe.style.backgroundColor = 'var(--accent)'
            document.querySelector('.content-library')!.append(probe)
            const color = getComputedStyle(probe).backgroundColor
            probe.remove()
            return color
          }))
        const row = page.locator('.song-row').filter({ has: page.getByRole('button', { name: `打开 ${first.name}`, exact: true }) })
        const other = page.locator('.song-row').filter({ has: page.getByRole('button', { name: `打开 ${second.name}`, exact: true }) })
        await expect(row.locator('.song-duration')).toHaveText('1:00')
        await expect(row.locator('.card-info small')).toHaveText('WAV · 本地音乐')
        await expect(row.locator('.song-cover-fallback')).toBeVisible()
        await expect(row).toHaveCSS('height', '68px')
        await expect(other).toHaveCSS('height', '68px')
        await expect(row.locator('.card-preview')).toHaveCSS('width', '46px')
        await expect(row.locator('.card-preview')).toHaveCSS('height', '46px')
        await expect(page.locator('.song-list')).toHaveCSS('border-width', '0px')
        expect((await page.locator('.song-list').boundingBox())!.width).toBeLessThanOrEqual(960)
        expect(await page.evaluate(() => document.documentElement.scrollWidth)).toBe(device.width)
        const title = row.locator('.card-info strong')
        await expect(title).toHaveCSS('text-overflow', 'ellipsis')
        expect(await title.evaluate(element => element.scrollWidth > element.clientWidth)).toBe(true)
        await page.mouse.move(0, 0)
        await expect(row.locator('.song-number')).toHaveCSS('opacity', device.hasTouch ? '0' : '1')
        const titleBounds = await title.boundingBox()
        await row.hover()
        await expect(row.locator('.song-number')).toHaveCSS('opacity', '0')
        await expect(row.locator('.song-play')).toHaveCSS('opacity', '1')
        expect(await title.boundingBox()).toEqual(titleBounds)

        await row.getByRole('button', { name: `播放 ${first.name}`, exact: true }).click()
        await expect(page.getByRole('button', { name: '暂停音乐', exact: true })).toBeVisible()
        await expect(row).toHaveClass(/is-playing/)
        await row.getByRole('button', { name: `暂停 ${first.name}`, exact: true }).click()
        await expect(page.getByRole('button', { name: '播放音乐', exact: true })).toBeVisible()
        const audioCount = await page.locator('audio').count()
        const playerBounds = await page.locator('.music-dock').boundingBox()
        const menu = row.locator('.action-menu')
        await menu.locator('summary').click()
        await expect(menu.locator('.song-menu-panel')).toBeVisible()
        const panelBounds = (await menu.locator('.song-menu-panel').boundingBox())!
        expect(panelBounds.x).toBeGreaterThanOrEqual(0)
        expect(panelBounds.x + panelBounds.width).toBeLessThanOrEqual(device.width)
        await menu.getByRole('button', { name: '收藏曲目', exact: true }).click()
        await expect.poll(async () => (await (await page.request.get(`/api/library/items?q=${prefix}`)).json()).items.find((item: any) => item.file.id === first.id).favorite).toBe(true)
        await menu.locator('summary').click()
        await expect(menu.getByRole('button', { name: '取消收藏', exact: true })).toBeVisible()
        await menu.getByRole('button', { name: '加入歌单', exact: true }).click()
        const dialog = page.getByRole('dialog').filter({ hasText: '加入歌单' })
        await expect(dialog).toBeVisible()
        await dialog.getByRole('button', { name: '＋ 创建新集合', exact: true }).click()
        await page.getByRole('dialog').getByRole('textbox').fill(prefix)
        await page.getByRole('dialog').getByRole('button', { name: '创建', exact: true }).click()
        await expect.poll(async () => (await (await page.request.get('/api/library/collections?kind=audio')).json()).find((collection: any) => collection.name === prefix)?.item_count).toBe(1)
        collectionId = (await (await page.request.get('/api/library/collections?kind=audio')).json()).find((collection: any) => collection.name === prefix).id
        expect(await page.locator('audio').count()).toBe(audioCount)
        expect(await page.locator('.music-dock').boundingBox()).toEqual(playerBounds)

        await menu.locator('summary').click()
        const download = page.waitForEvent('download')
        await menu.getByRole('button', { name: '下载原文件', exact: true }).click()
        expect((await download).suggestedFilename()).toBe(first.name)
        await enterSelectionMode(page)
        await expect(row.locator('.song-play')).toBeDisabled()
        await row.getByRole('checkbox').press('Space')
        await expect(page.getByRole('toolbar', { name: '所选项目操作', exact: true })).toBeVisible()
        await expect(row).toHaveClass(/selected/)
      } finally {
        const stop = page.getByRole('button', { name: '停止音乐', exact: true })
        if (await stop.count()) await stop.click()
        if (collectionId) await page.request.delete(`/api/library/collections/${collectionId}`, { headers })
        await page.request.delete(`/api/files/${id}`, { headers })
        await page.request.delete(`/api/trash/${id}`, { headers })
      }
    })
  })
}

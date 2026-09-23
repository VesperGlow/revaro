import { expect, test } from '@playwright/test'
import { deflateSync } from 'node:zlib'
import { readFileSync } from 'node:fs'
import { login } from './helpers'

function png(width: number, height: number, colours: [number, number, number][]) {
  const rows: Buffer[] = []
  for (let y = 0; y < height; y += 1) {
    const row = Buffer.alloc(1 + width * 3)
    for (let x = 0; x < width; x += 1) {
      const colour = colours[Math.min(colours.length - 1, Math.floor(x * colours.length / width))]
      const offset = 1 + x * 3
      row[offset] = colour[0]
      row[offset + 1] = (colour[1] + Math.floor(y * 20 / height)) % 256
      row[offset + 2] = colour[2]
    }
    rows.push(row)
  }
  const chunk = (type: string, data: Buffer) => {
    const kind = Buffer.from(type)
    const body = Buffer.concat([kind, data])
    const crc = Buffer.alloc(4)
    crc.writeUInt32BE(crc32(body) >>> 0, 0)
    const length = Buffer.alloc(4)
    length.writeUInt32BE(data.length, 0)
    return Buffer.concat([length, body, crc])
  }
  const header = Buffer.alloc(13)
  header.writeUInt32BE(width, 0)
  header.writeUInt32BE(height, 4)
  header[8] = 8
  header[9] = 2
  return Buffer.concat([
    Buffer.from('\x89PNG\r\n\x1a\n', 'binary'),
    chunk('IHDR', header),
    chunk('IDAT', deflateSync(Buffer.concat(rows))),
    chunk('IEND', Buffer.alloc(0)),
  ])
}

function crc32(data: Buffer) {
  let value = 0xffffffff
  for (const byte of data) {
    value ^= byte
    for (let bit = 0; bit < 8; bit += 1) value = (value >>> 1) ^ (value & 1 ? 0xedb88320 : 0)
  }
  return value ^ 0xffffffff
}

function wav() {
  const samples = 8_000 * 4
  const data = Buffer.alloc(44 + samples * 2)
  data.write('RIFF', 0); data.writeUInt32LE(36 + samples * 2, 4); data.write('WAVEfmt ', 8)
  data.writeUInt32LE(16, 16); data.writeUInt16LE(1, 20); data.writeUInt16LE(1, 22)
  data.writeUInt32LE(8_000, 24); data.writeUInt32LE(16_000, 28); data.writeUInt16LE(2, 32)
  data.writeUInt16LE(16, 34); data.write('data', 36); data.writeUInt32LE(samples * 2, 40)
  return data
}

test('Rust bundle serves the media viewer and live transfer dialog', async ({ page }) => {
  const suffix = Date.now().toString(36)
  const target = `rust-e2e-target-${suffix}`
  const first = `rust-e2e-first-${suffix}.png`
  const second = `rust-e2e-second-${suffix}.png`
  const audio = `rust-e2e-audio-${suffix}.wav`
  const video = `rust-e2e-video-${suffix}.webm`
  const videoBytes = readFileSync(new URL('./fixtures/preview.webm', import.meta.url))

  await login(page)
  await page.getByRole('button', { name: '新建文件夹' }).click()
  const create = page.getByRole('dialog')
  await create.locator('input[type=text]').fill(target)
  await create.getByRole('button', { name: '创建' }).click()
  await page.locator('.file-card').filter({ hasText: target }).waitFor()

  await page.locator('input[type=file]').first().setInputFiles([
    { name: first, mimeType: 'image/png', buffer: png(1800, 1100, [[42, 78, 91], [92, 128, 115], [218, 203, 155]]) },
    { name: second, mimeType: 'image/png', buffer: png(900, 1400, [[81, 45, 101], [161, 88, 107], [224, 177, 115]]) },
    { name: audio, mimeType: 'audio/wav', buffer: wav() },
    { name: video, mimeType: 'video/webm', buffer: videoBytes },
  ])
  for (const name of [first, second, audio, video]) {
    await page.locator('.file-card').filter({ hasText: name }).waitFor({ timeout: 20_000 })
  }
  const firstCard = page.locator('.file-card').filter({ hasText: first })
  await firstCard.click()
  await page.locator('.preview-image').waitFor()
  await page.waitForFunction(() => document.querySelector('.preview-image')?.naturalWidth > 0)
  await page.getByRole('button', { name: '实际大小', exact: true }).click()
  await expect(page.locator('.preview-actual-size')).toHaveText('100%')
  await page.getByRole('button', { name: '缩略图', exact: true }).click()
  await expect.poll(() => page.locator('.preview-filmstrip button').count(), { timeout: 10_000 }).toBeGreaterThanOrEqual(2)
  await page.getByRole('button', { name: `查看 ${second}`, exact: true }).click()
  await expect(page.locator('.preview-file-meta strong')).toHaveText(second)
  await page.locator('.preview-commandbar summary').click()
  const downloadPromise = page.waitForEvent('download')
  await page.getByRole('button', { name: '下载', exact: true }).click()
  expect((await downloadPromise).suggestedFilename()).toBe(second)
  await page.locator('.preview-close').click()

  await firstCard.click()
  await page.locator('.preview-commandbar summary').click()
  await page.getByRole('button', { name: '移动', exact: true }).click()
  await page.locator('.move-copy-dialog').waitFor()
  await page.locator('.directory-trigger').click()
  const picker = page.getByRole('region', { name: '选择目标目录' })
  await picker.getByRole('button', { name: target, exact: true }).click()
  await page.waitForFunction(value => document.querySelector('.directory-trigger')?.textContent?.includes(value), target)
  await page.locator('.directory-trigger').click()
  await page.getByRole('dialog').getByRole('button', { name: '移动', exact: true }).click()
  await page.locator('.move-copy-dialog').waitFor({ state: 'detached' })
  await firstCard.waitFor({ state: 'detached' })

  const secondCard = page.locator('.file-card').filter({ hasText: second })
  await secondCard.click()
  await page.locator('.preview-commandbar summary').click()
  await page.getByRole('button', { name: '复制', exact: true }).click()
  await page.locator('.move-copy-dialog').waitFor()
  await page.locator('.directory-trigger').click()
  await page.getByRole('region', { name: '选择目标目录' }).getByRole('button', { name: target, exact: true }).click()
  await page.waitForFunction(value => document.querySelector('.directory-trigger')?.textContent?.includes(value), target)
  await page.locator('.directory-trigger').click()
  await page.getByRole('dialog').getByRole('button', { name: '复制', exact: true }).click()
  await page.locator('.move-copy-dialog').waitFor({ state: 'detached' })
  await secondCard.waitFor()

  await page.locator('.file-card').filter({ hasText: target }).click()
  await page.getByRole('heading', { name: target }).waitFor()
  await page.locator('.file-card').filter({ hasText: first }).waitFor()
  await page.getByRole('button', { name: '我的文件', exact: true }).first().click()
  await page.getByRole('heading', { name: '我的文件' }).waitFor()

  await page.locator('.file-card').filter({ hasText: audio }).click()
  await page.locator('audio').waitFor({ state: 'attached' })
  await page.waitForFunction(() => document.querySelector('audio')?.readyState >= 1)
  await expect(page.locator('.audio-main')).toHaveCount(1)
  await page.getByRole('button', { name: '章节', exact: true }).click()
  await page.locator('.audio-panel').waitFor()
  await page.keyboard.press('Escape')
  await page.locator('.audio-panel').waitFor({ state: 'detached' })
  await page.locator('.preview-close').click()

  await page.locator('.file-card').filter({ hasText: video }).click()
  const videoElement = page.locator('video').last()
  await videoElement.waitFor({ state: 'attached' })
  await page.waitForFunction(() => document.querySelector('video')?.readyState >= 1)
  await expect(page.locator('.video-player-shell')).toHaveCount(1)
  await page.getByLabel('播放设置', { exact: true }).click()
  await page.getByLabel('播放速度', { exact: true }).selectOption('1.5')
  await expect(videoElement).toHaveJSProperty('playbackRate', 1.5)
  await page.getByRole('button', { name: '退出播放', exact: true }).click()
  await page.locator('.preview-modal').waitFor({ state: 'detached' })
})

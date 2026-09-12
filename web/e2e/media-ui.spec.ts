import { test, expect, type Page } from '@playwright/test'
import { readFileSync } from 'node:fs'

const ROOT = '00000000-0000-0000-0000-000000000000'
const files = [
  { id: 'audio-1', name: '山间来信.m4a', mime_type: 'audio/mp4' },
  { id: 'video-1', name: '山间漫步.webm', mime_type: 'video/webm' },
  { id: 'image-1', name: '群山.png', mime_type: 'image/png' },
  { id: 'image-2', name: '远山.png', mime_type: 'image/png' },
].map(file => ({ ...file, parent_id: ROOT, kind: 'file', size: 200000, status: 'ready', created_at: '', updated_at: '' }))
const landscape = (portrait = false) => `<svg xmlns="http://www.w3.org/2000/svg" width="${portrait ? 900 : 1800}" height="${portrait ? 1400 : 1100}" viewBox="0 0 1800 1100"><rect width="1800" height="1100" fill="#c8d7d6"/><circle cx="1320" cy="300" r="110" fill="#eee7d3"/><path d="M0 710 460 200 990 870 1500 400 1800 650V1100H0Z" fill="#809b9d"/><path d="M0 960 650 450 1200 1040 1580 680 1800 800V1100H0Z" fill="#4c6a71"/><path d="M0 990 580 900 1080 1070 1800 970V1100H0Z" fill="#2d4954"/></svg>`
function wav() {
  const length = 8000 * 120, buffer = Buffer.alloc(44 + length * 2)
  buffer.write('RIFF');buffer.writeUInt32LE(36+length*2,4);buffer.write('WAVEfmt ',8);buffer.writeUInt32LE(16,16);buffer.writeUInt16LE(1,20);buffer.writeUInt16LE(1,22);buffer.writeUInt32LE(8000,24);buffer.writeUInt32LE(16000,28);buffer.writeUInt16LE(2,32);buffer.writeUInt16LE(16,34);buffer.write('data',36);buffer.writeUInt32LE(length*2,40)
  return buffer
}
async function mockMedia(page: Page) {
  const sound = wav(), video = readFileSync(new URL('./fixtures/preview.webm', import.meta.url))
  await page.route('**/api/**', async route => {
    const path = new URL(route.request().url()).pathname
    const json = (value: unknown) => route.fulfill({ json: value })
    if (path === '/api/auth/me') return json({ username: 'admin', has_avatar: false })
    if (path === '/api/events' || path === '/api/system/status/stream') return route.fulfill({ contentType: 'text/event-stream', body: '' })
    if (path === `/api/files/${ROOT}/children`) return json({ items: files, total_bytes: 800000, file_count: 4 })
    if (path === `/api/files/${ROOT}`) return json({ file: { id: ROOT, name: '我的文件', kind: 'directory' }, breadcrumbs: [] })
    if (path.endsWith('/media/progress')) return json({ position: 10 })
    if (path === '/api/files/audio-1/audio') return json({ duration: 120, has_cover: true, cover_url: '/api/files/image-1/preview', chapters: [
      {id:1,title:'第一章 · 风从山谷来',start:0,end:40}, {id:2,title:'第二章 · 在林间停留',start:40,end:80}, {id:3,title:'第三章 · 晚风与归途',start:80,end:120},
    ] })
    if (path === '/api/files/video-1/video') return json({ subtitles: [{ id:'zh',label:'简体中文',language:'zh',url:'/api/subtitle.vtt',default:true }] })
    if (path === '/api/subtitle.vtt') return route.fulfill({ contentType:'text/vtt', body:'WEBVTT\n\n00:00:00.000 --> 00:00:30.000\n沿着山间的小路，慢慢走。\n' })
    if (path.endsWith('/thumbnail') || path.startsWith('/api/files/image-')) return route.fulfill({ contentType:'image/svg+xml', body:landscape(path.includes('image-2')) })
    if (path.endsWith('/preview')) {
      const body = path.includes('audio-1') ? sound : video
      const contentType = path.includes('audio-1') ? 'audio/wav' : 'video/webm'
      const range = route.request().headers().range?.match(/bytes=(\d+)-(\d*)/)
      if (range) {
        const start = Number(range[1]), end = Math.min(Number(range[2] || body.length-1),body.length-1)
        return route.fulfill({ status:206,contentType,headers:{'accept-ranges':'bytes','content-range':`bytes ${start}-${end}/${body.length}`},body:body.subarray(start,end+1) })
      }
      return route.fulfill({ contentType, body })
    }
    return json({items:[]})
  })
  await page.goto('/')
  await expect(page.locator('.file-card')).toHaveCount(4)
}
async function open(page:Page,name:string){await page.locator('.file-card').filter({hasText:name}).click();await expect(page.locator('.preview-modal')).toBeVisible()}
async function noOverflow(page:Page){expect(await page.evaluate(()=>document.documentElement.scrollWidth<=innerWidth)).toBe(true)}

for (const width of [1440,390,320]) {
  test(`音频 ${width}px：无字幕时内容居中，章节与秒数跳转可用`, async ({page},testInfo) => {
    await page.setViewportSize({width,height:900})
    await mockMedia(page)
    await open(page,'山间来信.m4a')
    await expect(page.locator('audio')).toHaveJSProperty('readyState',4)
    await expect(page.locator('.audio-panel')).toHaveCount(0)
    await expect(page.locator('.audio-main')).not.toContainText('没有内嵌字幕')
    await page.getByRole('button',{name:'章节',exact:true}).click()
    await page.locator('[data-chapter-index="1"]').click()
    await expect(page.locator('.audio-chapter-current h1')).toHaveText('第二章 · 在林间停留')
    await page.keyboard.press('Escape')
    await expect(page.locator('.audio-panel')).toHaveCount(0)
    await page.locator('audio').evaluate((el:HTMLAudioElement)=>el.pause())
    await page.getByRole('button',{name:'前进30秒'}).click()
    await expect.poll(()=>page.locator('audio').evaluate((el:HTMLAudioElement)=>el.currentTime)).toBeGreaterThanOrEqual(70)
    await page.getByRole('button',{name:'后退15秒'}).click()
    await expect.poll(()=>page.locator('audio').evaluate((el:HTMLAudioElement)=>Math.floor(el.currentTime))).toBe(55)
    const controls=await page.locator('.audio-playback').boundingBox()
    expect(controls!.x).toBeGreaterThanOrEqual(0);expect(controls!.x+controls!.width).toBeLessThanOrEqual(width)
    await noOverflow(page)
    await page.screenshot({path:testInfo.outputPath(`audio-${width}.png`)})
    await page.keyboard.press('Escape')
    await expect(page.locator('.preview-modal')).toHaveCount(0)
    await expect(page.locator('.file-card').filter({hasText:'山间来信.m4a'})).toBeFocused()
  })
}

for (const [name, selector] of [['山间来信.m4a', 'audio'], ['山间漫步.webm', 'video']] as const) {
  test(`不支持的原文件直接报错：${selector}`, async ({page}) => {
    await mockMedia(page)
    const requested:string[]=[]
    page.on('request', request=>requested.push(new URL(request.url()).pathname))
    await page.route('**/api/files/*/preview', route=>route.fulfill({contentType:'application/octet-stream',body:'not decodable media'}))
    await open(page,name)
    await expect.poll(()=>page.locator(selector).evaluate((el:HTMLMediaElement)=>el.error?.code)).toBe(4)
    await expect(page.getByRole('alert')).toContainText('浏览器无法播放')
    expect(requested.some(path=>/hls|fmp4|transcode|audio\/stream/.test(path))).toBe(false)
  })
}

test('图片：实际大小、拖动边界、缩略图与逐层退出', async ({page},testInfo) => {
  await page.setViewportSize({width:1440,height:900});await mockMedia(page);await open(page,'群山.png')
  await expect(page.locator('.preview-image')).toBeVisible()
  await page.getByRole('button',{name:'实际大小',exact:true}).click()
  await expect(page.locator('.preview-actual-size')).toHaveText('100%')
  const naturalSize=await page.locator('.preview-image').boundingBox();expect(naturalSize!.width).toBeCloseTo(1800,0)
  await page.getByRole('button',{name:'适应窗口'}).click()
  const fitted=await page.locator('.preview-image').boundingBox();expect(fitted!.width).toBeLessThan(1440)
  await page.getByRole('button',{name:'缩略图',exact:true}).click()
  await expect(page.locator('.preview-filmstrip button')).toHaveCount(2)
  await page.getByRole('button',{name:'查看 远山.png'}).click()
  await expect(page.locator('.preview-file-meta')).toHaveText('远山.png')
  await page.getByRole('button',{name:'实际大小',exact:true}).click()
  await page.mouse.move(720,400);await page.mouse.down();await page.mouse.move(1300,500);await page.mouse.up()
  const portrait=await page.locator('.preview-image').boundingBox();expect(portrait!.x+portrait!.width/2).toBeCloseTo(720,0)
  await page.getByRole('button',{name:'适应窗口'}).click()
  await page.screenshot({path:testInfo.outputPath('image-desktop.png')})
  await page.locator('.preview-commandbar summary').click()
  await expect(page.getByRole('button',{name:'移动',exact:true})).toBeVisible()
  await page.keyboard.press('Escape');await expect(page.locator('.preview-modal')).toBeVisible();await expect(page.locator('details[open]')).toHaveCount(0)
  await page.locator('.preview-stage').click({position:{x:10,y:10}})
  await expect(page.locator('.preview-commandbar')).toBeHidden()
  await page.keyboard.press('Tab');await expect(page.locator('.preview-commandbar')).toBeVisible()
  await page.keyboard.press('Escape');await expect(page.locator('.preview-modal')).toHaveCount(0)
})

for (const width of [1440,390,320]) {
  test(`视频 ${width}px：时间可见，设置操作期间保持控制条，字幕不跳动`, async ({page},testInfo) => {
    await page.setViewportSize({width,height:844});await mockMedia(page);await open(page,'山间漫步.webm')
    await expect(page.locator('video').last()).toHaveJSProperty('paused',false)
    const shell=page.locator('.video-player-shell'), video=shell.locator('video')
    await shell.hover()
    await expect(page.locator('.video-time')).toBeVisible()
    await expect(page.locator('.video-subtitle-overlay')).toBeVisible()
    const subtitleBefore=await page.locator('.video-subtitle-overlay').boundingBox()
    await page.getByLabel('播放设置',{exact:true}).click()
    await page.getByLabel('播放速度',{exact:true}).selectOption('1.5')
    await expect(video).toHaveJSProperty('playbackRate',1.5)
    await page.mouse.move(0,0);await page.waitForTimeout(3200)
    await expect(page.locator('.video-controls')).toBeVisible()
    await page.keyboard.press('Escape');await expect(shell).toBeVisible()
    await video.focus();await page.mouse.move(0,0);await page.waitForTimeout(3200)
    // Blur the keyboard focus that deliberately holds controls open.
    await page.evaluate(()=>{(document.activeElement as HTMLElement)?.blur()})
    await page.waitForTimeout(3000)
    await expect(page.locator('.video-controls')).toBeHidden()
    const subtitleAfter=await page.locator('.video-subtitle-overlay').boundingBox()
    expect(subtitleAfter!.y).toBeCloseTo(subtitleBefore!.y,0)
    await shell.hover();await expect(page.locator('.video-time')).toBeVisible()
    const row=await page.locator('.video-control-row').boundingBox();expect(row!.x+row!.width).toBeLessThanOrEqual(width)
    await noOverflow(page)
    await page.screenshot({path:testInfo.outputPath(`video-${width}.png`)})
  })
}

test('触屏：轻触视频只切换控制条，图片双指缩放和取消手势不误翻页', async ({browser}) => {
  const context=await browser.newContext({viewport:{width:390,height:844},isMobile:true,hasTouch:true})
  const page=await context.newPage()
  try {
    await mockMedia(page);await open(page,'山间漫步.webm')
    const video=page.locator('.video-player-shell video')
    await expect(video).toHaveJSProperty('paused',false)
    await page.touchscreen.tap(195,400)
    await expect(page.locator('.video-controls')).toBeHidden()
    await expect(video).toHaveJSProperty('paused',false)
    await page.touchscreen.tap(195,400)
    await expect(page.locator('.video-controls')).toBeVisible()
    await expect(video).toHaveJSProperty('paused',false)
    await page.getByRole('button',{name:'退出播放'}).tap()
    await open(page,'群山.png');await expect(page.locator('.preview-image')).toBeVisible()
    const before=parseInt((await page.locator('.preview-actual-size').textContent())!)
    const session=await context.newCDPSession(page)
    await session.send('Input.dispatchTouchEvent',{type:'touchStart',touchPoints:[{x:120,y:400,id:1},{x:270,y:400,id:2}]})
    await session.send('Input.dispatchTouchEvent',{type:'touchMove',touchPoints:[{x:70,y:400,id:1},{x:320,y:400,id:2}]})
    await session.send('Input.dispatchTouchEvent',{type:'touchEnd',touchPoints:[]})
    expect(parseInt((await page.locator('.preview-actual-size').textContent())!)).toBeGreaterThan(before)
    await page.getByRole('button',{name:'适应窗口'}).tap()
    await session.send('Input.dispatchTouchEvent',{type:'touchStart',touchPoints:[{x:300,y:400,id:1}]})
    await session.send('Input.dispatchTouchEvent',{type:'touchMove',touchPoints:[{x:100,y:400,id:1}]})
    await session.send('Input.dispatchTouchEvent',{type:'touchCancel',touchPoints:[]})
    await expect(page.locator('.preview-file-meta')).toHaveText('群山.png')
    await session.send('Input.dispatchTouchEvent',{type:'touchStart',touchPoints:[{x:300,y:400,id:1}]})
    await session.send('Input.dispatchTouchEvent',{type:'touchMove',touchPoints:[{x:100,y:400,id:1}]})
    await session.send('Input.dispatchTouchEvent',{type:'touchEnd',touchPoints:[]})
    await expect(page.locator('.preview-file-meta')).toHaveText('远山.png')
  } finally { await context.close() }
})

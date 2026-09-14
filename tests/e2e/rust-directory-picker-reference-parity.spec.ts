import { expect, test, type Page } from '@playwright/test'

const ROOT = '00000000-0000-0000-0000-000000000000'
const STAMP = '2026-01-01T00:00:00Z'

const root = {
  id: ROOT,
  parent_id: null,
  name: '我的文件',
  kind: 'directory',
  size: 0,
  status: 'ready',
  created_at: STAMP,
  updated_at: STAMP,
  mime_type: '',
}
const source = {
  id: 'compat-source',
  parent_id: ROOT,
  name: '待移动.txt',
  kind: 'file',
  size: 12,
  status: 'ready',
  created_at: STAMP,
  updated_at: STAMP,
  mime_type: 'text/plain',
  etag: 'compat-source-etag',
}
const destination = {
  id: 'compat-destination',
  parent_id: ROOT,
  name: '目标文件夹',
  kind: 'directory',
  size: 0,
  status: 'ready',
  created_at: STAMP,
  updated_at: STAMP,
  mime_type: '',
}

async function mockPicker(page: Page, delayTransfer = false) {
  await page.route('**/api/**', async route => {
    const path = new URL(route.request().url()).pathname
    const json = (value: unknown) => route.fulfill({ json: value })

    if (path === '/api/auth/me') return json({ username: 'admin', has_avatar: false })
    if (path === '/api/events' || path === '/api/system/status/stream') {
      return route.fulfill({ contentType: 'text/event-stream', body: '' })
    }
    if (path === '/api/tasks') return json({ items: [] })
    if (path === '/api/library/all') {
      return json({ items: { book: [], image: [], video: [], audio: [] }, counts: { book: 0, image: 0, video: 0, audio: 0, file: 1 } })
    }
    if (path === '/api/library/counts') return json({ book: 0, image: 0, video: 0, audio: 0, file: 1 })
    if (path === `/api/files/${ROOT}`) return json({ file: root, breadcrumbs: [] })
    if (path === `/api/files/${ROOT}/children`) return json({ items: [source, destination], total_bytes: source.size, file_count: 1 })
    if (path === '/api/files/compat-destination') return json({ file: destination, breadcrumbs: [root] })
    if (path === '/api/files/compat-destination/children') return json({ items: [], total_bytes: 0, file_count: 0 })
    if (delayTransfer && path === '/api/files/compat-source' && route.request().method() === 'PATCH') {
      await new Promise(resolve => setTimeout(resolve, 800))
      return json({})
    }
    return json({ items: [] })
  })
}

async function openPicker(page: Page, baseUrl: string) {
  await page.goto(`${baseUrl}/`)
  await expect(page.getByRole('heading', { name: '我的文件', exact: true })).toBeVisible()
  await page.getByRole('button', { name: '列表', exact: true }).click()
  const row = page.locator('.file-row').filter({ hasText: '待移动.txt' })
  await expect(row).toBeVisible()
  await row.getByRole('button', { name: '选择项目' }).click()
  await page.getByRole('toolbar', { name: '所选项目操作' }).getByRole('button', { name: '移动', exact: true }).click()
  await expect(page.locator('.directory-trigger')).toBeVisible()
}

async function iconGeometry(page: Page, selector: string) {
  return page.locator(selector).evaluateAll(svgs => svgs.map(svg => Array.from(
    svg.querySelectorAll('path,rect,circle,ellipse,line,polyline,polygon'),
  ).map(element => ({
    tag: element.tagName.toLowerCase(),
    attributes: Object.fromEntries(Array.from(element.attributes)
      .filter(attribute => ['d', 'points', 'x', 'y', 'width', 'height', 'rx', 'ry', 'cx', 'cy', 'r', 'x1', 'x2', 'y1', 'y2', 'fill'].includes(attribute.name))
      .sort((left, right) => left.name.localeCompare(right.name))
      .map(attribute => [attribute.name, attribute.value])),
  }))))
}

async function compareIcons(oldPage: Page, newPage: Page, selector: string, label: string) {
  const oldGeometry = await iconGeometry(oldPage, selector)
  const newGeometry = await iconGeometry(newPage, selector)
  expect(newGeometry, `${label} 的 Rust SVG 几何应保持 reference`).toEqual(oldGeometry)
}

async function installEscapeProbe(page: Page) {
  await page.evaluate(() => {
    ;(window as Window & { __pickerEscapeDefaultPrevented?: boolean }).__pickerEscapeDefaultPrevented = undefined
    window.addEventListener('keydown', event => {
      if (event.key === 'Escape') {
        window.setTimeout(() => {
          ;(window as Window & { __pickerEscapeDefaultPrevented?: boolean }).__pickerEscapeDefaultPrevented = event.defaultPrevented
        }, 0)
      }
    }, { capture: true, once: true })
  })
}

async function pickerDisabledMetrics(page: Page) {
  return page.locator('.directory-picker').evaluate(element => ({
    className: element.className,
    opacity: getComputedStyle(element).opacity,
    triggerOpacity: getComputedStyle(element.querySelector('.directory-trigger') as Element).opacity,
    triggerDisabled: (element.querySelector('.directory-trigger') as HTMLButtonElement).disabled,
  }))
}

async function popoverMetrics(page: Page) {
  return page.locator('.directory-popover').evaluate(element => {
    const style = getComputedStyle(element)
    const rect = element.getBoundingClientRect()
    return {
      position: style.position,
      left: style.left,
      top: style.top,
      bottom: style.bottom,
      width: Math.round(rect.width * 100) / 100,
      height: Math.round(rect.height * 100) / 100,
      x: Math.round(rect.x * 100) / 100,
      y: Math.round(rect.y * 100) / 100,
      maxHeight: style.maxHeight,
    }
  })
}

async function clickTriggerAndCaptureTransition(page: Page) {
  return page.evaluate(() => new Promise<{ active: boolean; transitionDuration: string }>(resolve => {
    let finished = false
    const read = () => {
      const element = document.querySelector('.directory-popover')
      if (!element) return null
      const style = getComputedStyle(element)
      const className = element.className
      return {
        active: /directory-flyout-(?:enter|leave)-(?:active|from)|flyout-closed/.test(className)
          || style.opacity !== '1'
          || style.transform !== 'none',
        transitionDuration: style.transitionDuration,
      }
    }
    const finish = () => {
      if (finished) return
      const metrics = read()
      if (!metrics) return
      finished = true
      observer.disconnect()
      resolve(metrics)
    }
    const observer = new MutationObserver(finish)
    observer.observe(document.body, { attributes: true, childList: true, subtree: true })
    const trigger = document.querySelector('.directory-trigger') as HTMLButtonElement | null
    trigger?.click()
    window.setTimeout(finish, 300)
  }))
}

test('移动/复制目录选择器的路径图标和展开关闭行为保持 reference', async ({ browser }) => {
  const oldUrl = process.env.E2E_REFERENCE_URL || 'http://127.0.0.1:18080'
  const newUrl = process.env.E2E_NEW_URL || 'http://127.0.0.1:18084'
  const oldContext = await browser.newContext({ viewport: { width: 1280, height: 900 } })
  const newContext = await browser.newContext({ viewport: { width: 1280, height: 900 } })
  const oldPage = await oldContext.newPage()
  const newPage = await newContext.newPage()

  try {
    await Promise.all([mockPicker(oldPage), mockPicker(newPage)])
    await Promise.all([openPicker(oldPage, oldUrl), openPicker(newPage, newUrl)])

    await compareIcons(oldPage, newPage, '.directory-trigger > svg', '目录选择器触发器')
    const [oldEntrance, newEntrance] = await Promise.all([
      clickTriggerAndCaptureTransition(oldPage),
      clickTriggerAndCaptureTransition(newPage),
    ])
    await expect(oldPage.getByRole('region', { name: '选择目标目录' })).toBeVisible()
    await expect(newPage.getByRole('region', { name: '选择目标目录' })).toBeVisible()
    expect(newEntrance.transitionDuration, 'Rust 目录选择器过渡时长与 reference 不一致')
      .toEqual(oldEntrance.transitionDuration)
    expect(oldEntrance.active, 'reference 目录选择器进入首帧应处于过渡中').toBe(true)
    expect(newEntrance.active, 'Rust 目录选择器进入首帧应处于过渡中').toBe(true)
    await oldPage.waitForTimeout(50)
    await newPage.waitForTimeout(50)
    const oldMetrics = await popoverMetrics(oldPage)
    const newMetrics = await popoverMetrics(newPage)
    expect({ ...newMetrics, y: 0 }, 'Rust 目录选择器 popover 定位与 reference 不一致')
      .toEqual({ ...oldMetrics, y: 0 })
    expect(Math.abs(newMetrics.y - oldMetrics.y), 'Rust 目录选择器 popover 垂直定位超出 reference 亚像素误差')
      .toBeLessThan(1)
    await compareIcons(oldPage, newPage, '.directory-breadcrumbs svg', '目录选择器根路径')
    await compareIcons(oldPage, newPage, '.directory-list > button svg', '目录选择器子目录')

    await oldPage.getByRole('region', { name: '选择目标目录' }).getByRole('button', { name: '目标文件夹', exact: true }).click()
    await newPage.getByRole('region', { name: '选择目标目录' }).getByRole('button', { name: '目标文件夹', exact: true }).click()
    await expect(oldPage.locator('.directory-trigger')).toHaveAttribute('title', /目标文件夹/)
    await expect(newPage.locator('.directory-trigger')).toHaveAttribute('title', /目标文件夹/)
    await compareIcons(oldPage, newPage, '.directory-breadcrumbs svg', '目录选择器深层路径')
    await compareIcons(oldPage, newPage, '.directory-state svg', '目录选择器空目录')

    await Promise.all([installEscapeProbe(oldPage), installEscapeProbe(newPage)])
    await Promise.all([oldPage.keyboard.press('Escape'), newPage.keyboard.press('Escape')])
    await expect(oldPage.getByRole('region', { name: '选择目标目录' })).toHaveCount(0)
    await expect(newPage.getByRole('region', { name: '选择目标目录' })).toHaveCount(0)
    await expect.poll(() => oldPage.evaluate(() => (window as Window & { __pickerEscapeDefaultPrevented?: boolean }).__pickerEscapeDefaultPrevented)).toBe(false)
    await expect.poll(() => newPage.evaluate(() => (window as Window & { __pickerEscapeDefaultPrevented?: boolean }).__pickerEscapeDefaultPrevented)).toBe(false)
  } finally {
    await oldContext.close()
    await newContext.close()
  }
})

test('目录选择器关闭时保留 reference 的退出过渡', async ({ browser }) => {
  const oldUrl = process.env.E2E_REFERENCE_URL || 'http://127.0.0.1:18080'
  const newUrl = process.env.E2E_NEW_URL || 'http://127.0.0.1:18084'
  const oldContext = await browser.newContext({ viewport: { width: 1280, height: 900 } })
  const newContext = await browser.newContext({ viewport: { width: 1280, height: 900 } })
  const oldPage = await oldContext.newPage()
  const newPage = await newContext.newPage()

  try {
    await Promise.all([mockPicker(oldPage), mockPicker(newPage)])
    await Promise.all([openPicker(oldPage, oldUrl), openPicker(newPage, newUrl)])
    await Promise.all([
      oldPage.locator('.directory-trigger').click(),
      newPage.locator('.directory-trigger').click(),
    ])
    await expect(oldPage.getByRole('region', { name: '选择目标目录' })).toBeVisible()
    await expect(newPage.getByRole('region', { name: '选择目标目录' })).toBeVisible()
    await Promise.all([oldPage.waitForTimeout(220), newPage.waitForTimeout(220)])

    const [oldLeave, newLeave] = await Promise.all([
      clickTriggerAndCaptureTransition(oldPage),
      clickTriggerAndCaptureTransition(newPage),
    ])
    expect(oldLeave.transitionDuration, 'reference 目录选择器退出过渡时长探针异常').toBe('0.14s, 0.14s')
    expect(newLeave.transitionDuration, 'Rust 目录选择器退出过渡时长与 reference 不一致')
      .toEqual(oldLeave.transitionDuration)
    expect(oldLeave.active, 'reference 目录选择器退出首帧应处于过渡中').toBe(true)
    expect(newLeave.active, 'Rust 目录选择器退出首帧应处于过渡中').toBe(true)
    await expect(oldPage.getByRole('region', { name: '选择目标目录' })).toHaveCount(0)
    await expect(newPage.getByRole('region', { name: '选择目标目录' })).toHaveCount(0)
  } finally {
    await oldContext.close()
    await newContext.close()
  }
})

test('传输处理中目录选择器保持 reference 的 disabled 外观和控件状态', async ({ browser }) => {
  const oldUrl = process.env.E2E_REFERENCE_URL || 'http://127.0.0.1:18080'
  const newUrl = process.env.E2E_NEW_URL || 'http://127.0.0.1:18084'
  const oldContext = await browser.newContext({ viewport: { width: 1280, height: 900 } })
  const newContext = await browser.newContext({ viewport: { width: 1280, height: 900 } })
  const oldPage = await oldContext.newPage()
  const newPage = await newContext.newPage()

  try {
    await Promise.all([mockPicker(oldPage, true), mockPicker(newPage, true)])
    await Promise.all([openPicker(oldPage, oldUrl), openPicker(newPage, newUrl)])
    await Promise.all([
      oldPage.getByRole('button', { name: '移动', exact: true }).click(),
      newPage.getByRole('button', { name: '移动', exact: true }).click(),
    ])
    await expect(oldPage.getByRole('button', { name: '正在处理…', exact: true })).toBeVisible()
    await expect(newPage.getByRole('button', { name: '正在处理…', exact: true })).toBeVisible()
    expect(await pickerDisabledMetrics(newPage), 'Rust 传输中的目录选择器外观/disabled 状态与 reference 不一致')
      .toEqual(await pickerDisabledMetrics(oldPage))
  } finally {
    await oldContext.close()
    await newContext.close()
  }
})

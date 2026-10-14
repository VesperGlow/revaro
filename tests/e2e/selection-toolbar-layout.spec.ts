import { expect, test } from '@playwright/test'
import { readFileSync } from 'node:fs'

// Exercise the real stylesheet cascade without a running API or book fixtures.
const staticRoot = new URL('../../crates/revaro-web/static/', import.meta.url)
const manifest = readFileSync(new URL('styles.css', staticRoot), 'utf8')
const styles = [...manifest.matchAll(/@import "([^"]+)";/g)]
  .map(match => readFileSync(new URL(match[1], staticRoot), 'utf8')).join('\n')
const icon = '<svg viewBox="0 0 24 24" aria-hidden="true"><path d="M4 12h16"/></svg>'
const mobileSelector = '.selection-mobile-actions>button,.selection-mobile-actions>details>summary'

function action(label: string, disabled = false) {
  return `<button type="button" ${disabled ? 'disabled' : ''}>${icon}<span>${label}</span></button>`
}

function menu(panelClass: string, actions: string[]) {
  return `<details class="action-menu collection-menu"><summary><span>更多</span>${icon}</summary>
    <div class="action-menu-panel ${panelClass}" hidden>${actions.map(label => action(label)).join('')}</div></details>`
}

const scenarios = [
  { name: 'multiple files', read: false, menu: ['复制到', '收藏', '加入书架', '加入歌单'] },
  { name: 'single book', read: true, menu: ['复制到', '分享', '重命名', '取消收藏', '加入书架', '堆叠'] },
  { name: 'single document', read: false, menu: ['编辑文本', '复制到', '分享', '重命名', '收藏'] },
  { name: 'stacked books', read: false, menu: ['收藏', '取消收藏', '加入书架', '堆叠', '移出堆叠'] },
  { name: 'directory', read: false, menu: ['打开', '复制到', '重命名'] },
]

function toolbar(scenario: typeof scenarios[number], trash = false) {
  return `<div class="content-library"><div class="app-shell"><main class="content">
    <div class="selection-toolbar" role="toolbar" aria-label="所选项目操作">
      <button type="button" class="selection-close">取消</button>
      <span class="selection-summary"><b>已选择 9999 项</b><small>已选择 2.0 GB</small></span>
      <div class="selection-action-groups"><div class="selection-actions">
        ${['取消全选', '复制到', '下载 (9999)', '移动', '删除'].map(label => action(label)).join('')}
      </div><div class="selection-management">${action('收藏')}${menu('selection-management-panel', ['取消收藏'])}</div></div>
      <div class="selection-mobile-actions">
        ${action('取消全选')}${trash ? action('恢复') + action('永久删除') :
          action('阅读', !scenario.read) + ['下载', '移动', '删除'].map(label => action(label)).join('') + menu('selection-mobile-panel', scenario.menu)}
      </div>
    </div>
  </main></div></div>`
}

for (const width of [320, 360, 375, 390, 414, 600, 768, 850]) {
  test(`mobile batch toolbar keeps exactly two rows at ${width}px`, async ({ page }) => {
    await page.setViewportSize({ width, height: 900 })
    for (const scenario of scenarios) {
      await page.setContent(`<style>${styles}</style>${toolbar(scenario)}`)
      const bar = page.getByRole('toolbar', { name: '所选项目操作', exact: true })
      const controls = bar.locator(mobileSelector)
      await expect(controls).toHaveCount(6)
      expect(await controls.allTextContents()).toEqual(['取消全选', '阅读', '下载', '移动', '删除', '更多'])
      await expect(bar.locator('.selection-action-groups')).toBeHidden()
      const geometry = await bar.evaluate((element, selector) => {
        const bounds = element.getBoundingClientRect()
        const close = element.querySelector('.selection-close')!.getBoundingClientRect()
        const summary = element.querySelector('.selection-summary')!.getBoundingClientRect()
        return {
          viewportOverflow: document.documentElement.scrollWidth > innerWidth,
          overflow: element.scrollWidth > element.clientWidth,
          left: bounds.left, right: bounds.right, height: bounds.height,
          headerCenterDifference: Math.abs(close.y + close.height / 2 - summary.y - summary.height / 2),
          headerBottom: Math.max(close.bottom, summary.bottom),
          controls: [...element.querySelectorAll<HTMLElement>(selector)].map(control => {
            const rect = control.getBoundingClientRect()
            const graphic = control.querySelector('svg')!.getBoundingClientRect()
            const label = control.querySelector('span')!
            return {
              left: rect.left, right: rect.right, top: rect.top, width: rect.width, height: rect.height,
              iconWidth: graphic.width, iconHeight: graphic.height,
              clippedLabel: label.scrollWidth > label.clientWidth || label.scrollHeight > label.clientHeight,
            }
          }),
        }
      }, mobileSelector)
      expect(geometry.viewportOverflow).toBe(false)
      expect(geometry.overflow).toBe(false)
      expect(geometry.height).toBeLessThanOrEqual(108)
      expect(geometry.headerCenterDifference).toBeLessThanOrEqual(1)
      expect(new Set(geometry.controls.map(control => Math.round(control.top))).size).toBe(1)
      for (const control of geometry.controls) {
        expect(control.left).toBeGreaterThanOrEqual(geometry.left)
        expect(control.right).toBeLessThanOrEqual(geometry.right)
        expect(control.top).toBeGreaterThanOrEqual(geometry.headerBottom)
        expect(control.width).toBeGreaterThanOrEqual(44)
        expect(control.height).toBe(44)
        expect(control.iconWidth).toBe(16)
        expect(control.iconHeight).toBe(16)
        expect(control.clippedLabel).toBe(false)
      }
      const widths = geometry.controls.map(control => control.width)
      expect(Math.max(...widths) - Math.min(...widths)).toBeLessThanOrEqual(.1)
      const read = bar.getByRole('button', { name: '阅读', exact: true })
      if (scenario.read) await expect(read).toBeEnabled()
      else await expect(read).toBeDisabled()
      await controls.last().click()
      const panel = bar.locator('.selection-mobile-panel')
      await panel.evaluate(element => element.removeAttribute('hidden'))
      const box = (await panel.boundingBox())!
      expect(box.x).toBeGreaterThanOrEqual(8)
      expect(box.x + box.width).toBeLessThanOrEqual(width - 8)
      expect((await bar.boundingBox())!.height).toBe(geometry.height)
      for (const label of scenario.menu) await expect(panel.getByRole('button', { name: label, exact: true })).toBeVisible()
    }
    await page.setContent(`<style>${styles}</style>${toolbar(scenarios[0], true)}`)
    await expect(page.locator('.selection-mobile-actions>button')).toHaveCount(3)
    await expect(page.getByRole('button', { name: '恢复', exact: true })).toBeVisible()
    await expect(page.getByRole('button', { name: '永久删除', exact: true })).toBeVisible()
  })
}

for (const width of [851, 1280, 1600, 1920]) {
  test(`desktop batch controls retain the horizontal layout at ${width}px`, async ({ page }) => {
    await page.setViewportSize({ width, height: 900 })
    await page.setContent(`<style>${styles}</style>${toolbar(scenarios[0])}`)
    const bar = page.getByRole('toolbar', { name: '所选项目操作', exact: true })
    await expect(bar.locator('.selection-mobile-actions')).toBeHidden()
    await expect(bar).toHaveCSS('flex-wrap', 'nowrap')
    await expect(bar.locator('.selection-actions')).toHaveCSS('display', 'flex')
    await expect(bar.locator('.selection-management')).toHaveCSS('display', 'flex')
    await expect(bar.locator('.selection-actions>button').first()).toHaveCSS('flex-direction', 'row')
    const centers = await bar.locator('.selection-actions>button').evaluateAll(controls => controls.map(control => {
      const rect = control.getBoundingClientRect()
      return rect.y + rect.height / 2
    }))
    expect(Math.max(...centers) - Math.min(...centers)).toBeLessThanOrEqual(1)
  })
}

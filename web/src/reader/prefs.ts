import type { LayoutProfile, ReaderPrefs } from './types'

// 与共享样式表 @font-face 一致的族名；服务端分页与客户端渲染用同一 WebFont。
export const FONT_FAMILY = 'Revaro Serif'
export const FONT_MIN = 14
export const FONT_MAX = 32
export const LINE_HEIGHTS = [1.4, 1.7, 2.0] as const

const PREFS_KEY = 'revaro-reader-v2-prefs'

export function loadPrefs(): ReaderPrefs {
  const fallback: ReaderPrefs = { fontSize: 19, lineHeight: 1.7, theme: 'light' }
  try {
    const raw = localStorage.getItem(PREFS_KEY)
    if (!raw) return fallback
    const parsed = JSON.parse(raw) as Partial<ReaderPrefs>
    return {
      fontSize: clamp(Number(parsed.fontSize) || fallback.fontSize, FONT_MIN, FONT_MAX),
      lineHeight: (LINE_HEIGHTS as readonly number[]).includes(Number(parsed.lineHeight))
        ? Number(parsed.lineHeight)
        : fallback.lineHeight,
      theme: parsed.theme === 'dark' ? 'dark' : 'light',
    }
  } catch {
    return fallback
  }
}

export function savePrefs(prefs: ReaderPrefs): void {
  try {
    localStorage.setItem(PREFS_KEY, JSON.stringify(prefs))
  } catch {
    /* 隐私模式等场景忽略 */
  }
}

function clamp(value: number, min: number, max: number): number {
  return Math.min(Math.max(value, min), max)
}

// computeMargins 与旧客户端分页逻辑同源的边距推导：布局变化时新旧分页
// 的观感保持一致。
export function computeMargins(width: number, height: number): { top: number; bottom: number; side: number } {
  const MAX_COLUMN = 720
  let side = Math.round(Math.min(Math.max(width * 0.055, 16), 44))
  if (width - 2 * side > MAX_COLUMN) side = Math.round((width - MAX_COLUMN) / 2)
  const mobile = width <= 850
  const top = mobile ? Math.round(Math.min(28, Math.max(16, height * 0.025))) : 60
  const bottom = mobile ? Math.round(Math.min(22, Math.max(12, height * 0.018))) : 24
  return { top, bottom, side }
}

// buildProfile 由视口 + 偏好生成服务端分页参数。参数变化即新 profile；
// 主题不参与（明暗切换用 CSS 变量，永不重排）。
export function buildProfile(width: number, height: number, prefs: ReaderPrefs): LayoutProfile {
  const margins = computeMargins(width, height)
  return {
    viewport_w: width,
    viewport_h: height,
    font_size: prefs.fontSize,
    font_family: FONT_FAMILY,
    line_height: prefs.lineHeight,
    margin_top: margins.top,
    margin_bottom: margins.bottom,
    margin_side: margins.side,
  }
}

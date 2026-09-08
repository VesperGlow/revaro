import type { ReaderPrefs } from './types'

export const FONT_MIN = 14
export const FONT_MAX = 32
export const LINE_HEIGHTS = [1.4, 1.7, 2.0] as const

const PREFS_KEY = 'revaro-reader-prefs'

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

export function clamp(value: number, min: number, max: number): number {
  return Math.min(Math.max(value, min), max)
}

// computeMargins 计算页边距：内容栏宽限制在可读宽度内（桌面宽屏时居中），
// 移动端根据可视高度使用紧凑留白。工具栏刻意覆盖正文，不参与页边距
// 计算，显示或隐藏工具栏时保持同一套 CSS columns 排版。
export function computeMargins(width: number, height: number): { top: number; bottom: number; side: number } {
  const MAX_COLUMN = 720
  let side = Math.round(Math.min(Math.max(width * 0.055, 16), 44))
  if (width - 2 * side > MAX_COLUMN) side = Math.round((width - MAX_COLUMN) / 2)
  const mobile = width <= 850
  const top = mobile ? Math.round(Math.min(28, Math.max(16, height * 0.025))) : 60
  const bottom = mobile ? Math.round(Math.min(22, Math.max(12, height * 0.018))) : 24
  return { top, bottom, side }
}

import { describe, expect, it } from 'vitest'
import { clampImagePan, fitImage, zoomImagePan } from './imageGeometry'

describe('image navigation geometry', () => {
  it('fits portrait and landscape images without enlarging small originals', () => {
    expect(fitImage({ width: 200, height: 100 }, { width: 800, height: 600 })).toEqual({ width: 200, height: 100 })
    expect(fitImage({ width: 1600, height: 800 }, { width: 832, height: 632 })).toEqual({ width: 800, height: 400 })
    expect(fitImage({ width: 400, height: 1600 }, { width: 832, height: 632 })).toEqual({ width: 150, height: 600 })
  })
  it('anchors zoom to the same image pixel under the pointer', () => {
    const before = { x: 30, y: -10 }, pointer = { x: 100, y: 50 }
    const after = zoomImagePan(before, pointer, pointer, 2)
    expect((pointer.x - after.x) / 2).toBe(pointer.x - before.x)
    expect((pointer.y - after.y) / 2).toBe(pointer.y - before.y)
  })
  it('moves the pinch anchor with the fingers', () => {
    expect(zoomImagePan({ x: 0, y: 0 }, { x: 50, y: 40 }, { x: 70, y: 50 }, 2)).toEqual({ x: -30, y: -30 })
  })
  it('keeps letterboxed axes centered and clamps against the actual image edges', () => {
    expect(clampImagePan({ x: 900, y: -900 }, { width: 200, height: 500 }, { width: 800, height: 600 }, 2)).toEqual({ x: 0, y: -200 })
    expect(clampImagePan({ x: 80, y: 100 }, { width: 400, height: 300 }, { width: 800, height: 600 }, 1)).toEqual({ x: 0, y: 0 })
  })
})

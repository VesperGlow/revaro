export interface Point { x: number; y: number }
export interface Size { width: number; height: number }
export function fitImage(image: Size, stage: Size): Size {
  if (image.width <= 0 || image.height <= 0) return { width: 0, height: 0 }
  const ratio = Math.min(1, Math.max(1, stage.width - 32) / image.width, Math.max(1, stage.height - 32) / image.height)
  return { width: image.width * ratio, height: image.height * ratio }
}
export function clampImagePan(pan: Point, image: Size, stage: Size, zoom: number): Point {
  const x = Math.max(0, (image.width * zoom - stage.width) / 2)
  const y = Math.max(0, (image.height * zoom - stage.height) / 2)
  return { x: Math.max(-x, Math.min(x, pan.x)), y: Math.max(-y, Math.min(y, pan.y)) }
}
// Preserve the image point under the mouse or the moving pinch midpoint.
export function zoomImagePan(pan: Point, from: Point, to: Point, ratio: number): Point {
  return { x: to.x - (from.x - pan.x) * ratio, y: to.y - (from.y - pan.y) * ratio }
}

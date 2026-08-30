import { describe, expect, it } from 'vitest'
// @ts-expect-error Vitest runs in Node; the browser build intentionally omits Node globals.
import { readFileSync } from 'node:fs'

const css=readFileSync(new URL('./style.css',import.meta.url),'utf8')

describe('audio subtitle header layout',()=>{
  it('keeps the cue count clear of the floating close button',()=>{
    expect(css).toContain('padding: 0 76px 0 28px')
    expect(css).toContain('flex: 0 0 auto')
    expect(css).toContain('white-space: nowrap')
  })
})

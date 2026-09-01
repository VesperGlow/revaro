import { describe, expect, it } from 'vitest'
// @ts-expect-error Vitest runs in Node; the browser build intentionally omits Node globals.
import { readFileSync } from 'node:fs'

const styleModules=['shell','browser','uploads','dialogs','media','responsive','extras']
const css=styleModules.map(name=>readFileSync(new URL(`./styles/${name}.css`,import.meta.url),'utf8')).join('\n')
const videoPlayer=readFileSync(new URL('./styles/video-player.css',import.meta.url),'utf8')

describe('audio subtitle header layout',()=>{
  it('keeps the cue count clear of the floating close button',()=>{
    expect(css).toContain('padding: 0 76px 0 28px')
    expect(css).toContain('flex: 0 0 auto')
    expect(css).toContain('white-space: nowrap')
  })
})

describe('media control visual composition',()=>{
  it('merges the audio timeline with the content boundary',()=>{
    expect(css).toContain('.audio-preview .audio-playback::before')
    expect(css).toContain('margin-top: -17px')
  })

  it('balances the desktop transport row within the area below the timeline',()=>{
    expect(css).toContain('padding-bottom: max(18px,env(safe-area-inset-bottom,0px))')
    expect(css).toContain('@media (max-width:700px)')
    expect(css).toContain('padding-bottom:max(7px,env(safe-area-inset-bottom,0px))')
  })

  it('keeps subtitles clear of visible desktop controls while preserving mobile spacing',()=>{
    expect(videoPlayer).toContain('.video-subtitle-overlay.raised{bottom:calc(var(--subtitle-image-bottom) + clamp(100px,14%,150px))}')
    expect(videoPlayer).toContain('.video-subtitle-overlay.raised{bottom:calc(var(--subtitle-image-bottom) + 84px)}')
  })
})

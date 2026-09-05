import { describe, expect, it } from 'vitest'
import { readFileSync } from 'node:fs'

const styleModules=['shell','browser','uploads','dialogs','media','responsive','extras']
const css=styleModules.map(name=>readFileSync(new URL(`./styles/${name}.css`,import.meta.url),'utf8')).join('\n')
const videoPlayer=readFileSync(new URL('./styles/video-player.css',import.meta.url),'utf8')
const taskCenter=readFileSync(new URL('./components/TaskCenter.vue',import.meta.url),'utf8')
const reader=readFileSync(new URL('./Reader.vue',import.meta.url),'utf8')
const audioPlayer=readFileSync(new URL('./components/AudioPlayer.vue',import.meta.url),'utf8')
const fullBleedProgress=readFileSync(new URL('./components/FullBleedProgress.vue',import.meta.url),'utf8')

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
    expect(css).toContain('.audio-preview .full-bleed-progress--audio')
  })

  it('keeps the audio progress control full-bleed and safe-area-aware',()=>{
    expect(audioPlayer).toContain("import FullBleedProgress from './FullBleedProgress.vue'")
    expect(css).not.toContain('.audio-track-wrap')
    expect(fullBleedProgress).toContain('height: 44px')
    expect(fullBleedProgress).toContain('env(safe-area-inset-left, 0px)')
    expect(fullBleedProgress).toContain('env(safe-area-inset-right, 0px)')
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

describe('reader chrome layout',()=>{
  it('balances a live circular header progress against the standard back icon',()=>{
    expect(reader).toContain('class="reader-back-icon"')
    expect(reader).not.toContain("from '@lucide/vue'")
    expect(reader).toContain('class="reader-progress-ring"')
    expect(reader).toContain('stroke-dashoffset')
    expect(reader).not.toContain('class="reader-bar-spacer"')
    expect(reader).toContain('Math.round(clamp(percentNow, 0, 100))')
    expect(reader).not.toContain('id="reader-kind"')
    expect(css).toContain('grid-template-columns: 60px minmax(0, 1fr) 60px')
    expect(css).toContain('env(safe-area-inset-top, 0px)')
  })

  it('keeps only three centered controls in the footer',()=>{
    expect(reader).not.toContain('FullBleedProgress')
    expect(reader).not.toContain('id="page-slider"')
    expect(css).toContain('grid-template-columns: repeat(3, 1fr)')
    expect(css).toContain('min-height: 48px')
  })
})

describe('task center flyout',()=>{
  it('keeps its surface opaque and completion actions touchable',()=>{
    expect(css).toContain('z-index: 20')
    expect(taskCenter).toContain('.task-panel{z-index:25;isolation:isolate;background:#fff}')
    expect(taskCenter).toContain('min-height:38px')
    expect(taskCenter).toContain('min-height:40px')
    expect(taskCenter).toContain('清除完成')
    expect(taskCenter).toContain('ChevronDown')
  })
})

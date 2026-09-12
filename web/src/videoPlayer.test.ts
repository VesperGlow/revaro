import {describe,it,expect} from 'vitest'
import {authoritativeSeekTarget,containedVideoInsets,initialSubtitleIndex,mediaElementTimelineTime,setExclusiveSubtitleTrack,shouldContinueMediaClock,shouldHideVideoCursor,shouldSyncMediaClock,subtitleLineClass} from './videoPlayer'

describe('video cursor visibility',()=>{
  const state={playing:true,controlsVisible:false,starting:false,buffering:false,error:''}
  it('hides only during unobstructed playback with hidden controls',()=>{
    expect(shouldHideVideoCursor(state)).toBe(true)
    expect(shouldHideVideoCursor({...state,controlsVisible:true})).toBe(false)
    expect(shouldHideVideoCursor({...state,playing:false})).toBe(false)
    expect(shouldHideVideoCursor({...state,starting:true})).toBe(false)
    expect(shouldHideVideoCursor({...state,buffering:true})).toBe(false)
    expect(shouldHideVideoCursor({...state,error:'decode failed'})).toBe(false)
  })
})

describe('video subtitle timeline and lifecycle',()=>{
  it('shows only the selected track',()=>{
    const first={mode:'disabled' as TextTrackMode},second={mode:'disabled' as TextTrackMode},tracks=[first,second]
    setExclusiveSubtitleTrack(tracks,second);expect(tracks.map(track=>track.mode)).toEqual(['disabled','hidden'])
    setExclusiveSubtitleTrack(tracks,null);expect(tracks.map(track=>track.mode)).toEqual(['disabled','disabled'])
  })
  it('does not give later subtitle lines the global secondary-button class',()=>{
    expect(subtitleLineClass(0)).toBe('')
    expect(subtitleLineClass(1)).toBe('video-subtitle-secondary-line')
    expect(subtitleLineClass(1)).not.toBe('secondary')
  })
  it('anchors subtitles to the contained image through letterboxing',()=>{
    expect(containedVideoInsets(1000,1000,1920,1080)).toEqual({bottom:218.75,horizontal:0})
    expect(containedVideoInsets(1200,600,1080,1920)).toEqual({bottom:0,horizontal:431.25})
    expect(containedVideoInsets(0,600,1920,1080)).toEqual({bottom:0,horizontal:0})
  })
  it('selects disposition defaults without relying on language or title metadata',()=>{
    expect(initialSubtitleIndex([{default:false},{default:true},{forced:true}])).toBe(1)
    expect(initialSubtitleIndex([{forced:false},{forced:true}])).toBe(1)
    expect(initialSubtitleIndex([{},{}])).toBe(0)
  })
})

describe('video seek authority',()=>{
  it('never replaces an explicit zero seek with saved resume progress',()=>{
    expect(authoritativeSeekTarget(0,86,true)).toBe(0)
  })
  it('uses resume progress only before the first explicit seek',()=>{
    expect(authoritativeSeekTarget(0,86,false)).toBe(86)
    expect(authoritativeSeekTarget(80,86,false)).toBe(80)
  })
})

describe('authoritative media element clock',()=>{
  it('tracks continuous native playback from the element',()=>{
    expect(mediaElementTimelineTime(12.25,'direct',0)).toBe(12.25)
  })
  it('suppresses only paused teardown events, never an actively playing clock',()=>{
    expect(shouldSyncMediaClock(true,true)).toBe(false)
    expect(shouldSyncMediaClock(true,false)).toBe(true)
    expect(shouldSyncMediaClock(false,true)).toBe(true)
  })
  it('keeps the sampler alive through a transient pause until teardown',()=>{
    expect(shouldContinueMediaClock(true)).toBe(true)
    expect(shouldContinueMediaClock(false)).toBe(false)
  })
})

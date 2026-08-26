import { afterEach, describe, expect, it, vi } from 'vitest'
import type { VideoFMP4Metadata, VideoFMP4Response } from './types'
import { attachFMP4Stream, bufferedRangesAddedSeconds, createUnifiedVideoPlayer, mseCompatibility, mseFreshRecoveryLimit, mseRecoveryAction, mseStallWatchdogSeconds, mseWatchdogExpired, mseWindowRefillLeadSeconds, setExclusiveSubtitleTrack, shouldHideVideoCursor, subtitleTrackKey, subtitleURLForPlayback } from './videoPlayer'

const metadata=(videoCodec='hevc',audioCodec='aac'):VideoFMP4Metadata=>({
  duration:120,
  video_codec:videoCodec,
  audio_codec:audioCodec,
  video_mime_type:`video/mp4; codecs="${videoCodec==='hevc'?'hvc1.2.4.L120.B0':'avc1.640028'}"`,
  audio_mime_type:`audio/mp4; codecs="${audioCodec==='aac'?'mp4a.40.2':'ec-3'}"`,
  aac_audio_mime_type:'audio/mp4; codecs="mp4a.40.2"',
  mime_type:`video/mp4; codecs="${videoCodec==='hevc'?'hvc1.2.4.L120.B0':'avc1.640028'}, ${audioCodec==='aac'?'mp4a.40.2':'ec-3'}"`,
  aac_mime_type:`video/mp4; codecs="${videoCodec==='hevc'?'hvc1.2.4.L120.B0':'avc1.640028'}, mp4a.40.2"`,
  width:1920,height:1080,bitrate:8_000_000,frame_rate:30,
})

function browserSupports(types:string[],supported=true,powerEfficient=true){
  Object.defineProperty(globalThis,'MediaSource',{configurable:true,value:{isTypeSupported:vi.fn((value:string)=>types.includes(value))}})
  Object.defineProperty(globalThis,'navigator',{configurable:true,value:{mediaCapabilities:{decodingInfo:vi.fn(async()=>({supported,smooth:true,powerEfficient}))}}})
}

afterEach(()=>vi.restoreAllMocks())

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

describe('MSE window refill',()=>{
  it('waits until playback is close to a short window tail',()=>{
    expect(mseWindowRefillLeadSeconds).toBeGreaterThanOrEqual(10)
    expect(mseWindowRefillLeadSeconds).toBeLessThanOrEqual(15)
  })

  it('rebuilds MSE twice before falling back to HLS',()=>{
    expect(mseFreshRecoveryLimit).toBe(2)
    expect(mseRecoveryAction(0)).toBe('fresh-mse')
    expect(mseRecoveryAction(1)).toBe('fresh-mse')
    expect(mseRecoveryAction(2)).toBe('hls')
    expect(mseStallWatchdogSeconds).toBeGreaterThanOrEqual(6)
    expect(mseStallWatchdogSeconds).toBeLessThanOrEqual(10)
    const limit=mseStallWatchdogSeconds*1000
    expect(mseWatchdogExpired(1000,1000,1000+limit-1,false)).toBe(false)
    expect(mseWatchdogExpired(1000,1000,1000+limit,false)).toBe(true)
    expect(mseWatchdogExpired(1000,1000,1000+limit,true)).toBe(false)
    expect(mseWatchdogExpired(1000,5000,1000+limit,false)).toBe(false)
  })

  it('measures actual browser buffer growth without using scheduling timestamps',()=>{
    expect(bufferedRangesAddedSeconds([],[{start:226.8,end:227.3}])).toBeCloseTo(.5)
    expect(bufferedRangesAddedSeconds([{start:226.8,end:227.3}],[{start:226.8,end:227.3}])).toBe(0)
    expect(bufferedRangesAddedSeconds([{start:226.8,end:227.0}],[{start:226.8,end:227.5}])).toBeCloseTo(.5)
  })
})

describe('mseCompatibility',()=>{
  it.each([['hevc','aac'],['h264','aac']])('%s + %s selects video/audio copy',async(video,audio)=>{
    const value=metadata(video,audio)
    browserSupports([value.video_mime_type,value.audio_mime_type!,value.aac_audio_mime_type!,value.mime_type,value.aac_mime_type])
    const result=await mseCompatibility(value)
    expect(result.mode).toBe('copy')
    expect(result.videoSupported).toBe(true)
  })

  it('keeps HEVC and only converts unsupported EAC3 to AAC',async()=>{
    const value=metadata('hevc','eac3')
    browserSupports([value.video_mime_type,value.aac_audio_mime_type!,value.aac_mime_type])
    const result=await mseCompatibility(value)
    expect(result.videoSupported).toBe(true)
    expect(result.audioSupported).toBe(false)
    expect(result.mode).toBe('aac')
  })

  it('uses HLS only when video decoding is unsupported',async()=>{
    const value=metadata('hevc','aac')
    browserSupports([value.audio_mime_type!,value.aac_audio_mime_type!],false)
    const result=await mseCompatibility(value)
    expect(result.mode).toBe('hls')
    expect(result.fallbackReason).toContain('HEVC')
  })

  it('does not enable H.264 fallback for an audio-only capability failure',async()=>{
    const value=metadata('hevc','eac3')
    browserSupports([value.video_mime_type])
    const result=await mseCompatibility(value)
    expect(result.videoSupported).toBe(true)
    expect(result.mode).toBe('error')
  })

  it('does not reject a supported decoder only because it is not power efficient',async()=>{
    const value=metadata('hevc','aac')
    browserSupports([value.video_mime_type,value.audio_mime_type!,value.aac_audio_mime_type!,value.mime_type,value.aac_mime_type],true,false)
    const result=await mseCompatibility(value)
    expect(result.mode).toBe('copy')
    expect(result.powerEfficient).toBe(false)
  })
})

describe('video subtitle timeline and lifecycle',()=>{
  const url='/api/files/video/video/subtitles/embedded-2'

  it('keeps direct and MSE subtitles on the global timeline at saved positions',()=>{
    expect(subtitleURLForPlayback(url,'direct',360)).toBe(url)
    expect(subtitleURLForPlayback(url,'mse',360)).toBe(url)
    expect(subtitleTrackKey('embedded-2','mse',0)).toBe(subtitleTrackKey('embedded-2','mse',360))
  })

  it('does not rebuild the MSE track for forward or backward seeks',()=>{
    const initial=subtitleTrackKey('embedded-2','mse',0)
    expect(subtitleTrackKey('embedded-2','mse',20*60)).toBe(initial)
    expect(subtitleTrackKey('embedded-2','mse',6*60)).toBe(initial)
  })

  it('uses and keys only HLS session offsets',()=>{
    expect(subtitleURLForPlayback(url,'hls',360)).toBe(`${url}?start=360.000`)
    expect(subtitleURLForPlayback(`${url}?lang=zh`,'hls',1200.1259)).toBe(`${url}?lang=zh&start=1200.125`)
    expect(subtitleTrackKey('embedded-2','hls',360)).not.toBe(subtitleTrackKey('embedded-2','hls',1200))
  })

  it('shows only the selected track and disables all tracks when subtitles are closed',()=>{
    const first={mode:'disabled' as TextTrackMode}
    const second={mode:'disabled' as TextTrackMode}
    const tracks=[first,second]
    setExclusiveSubtitleTrack(tracks,second)
    expect(first.mode).toBe('disabled')
    expect(second.mode).toBe('showing')
    setExclusiveSubtitleTrack(tracks,null)
    expect(tracks.map(track=>track.mode)).toEqual(['disabled','disabled'])
  })

  it('leaves the selected track untouched while MSE seeks outside buffered ranges',()=>{
    const selected={mode:'showing' as TextTrackMode}
    const seekOutside=vi.fn(()=>true)
    const element={
      currentTime:360,
      seekable:{length:0,start:()=>0,end:()=>0},
      textTracks:[selected],
    } as unknown as HTMLVideoElement
    const player=createUnifiedVideoPlayer('mse',element,0,()=>{},seekOutside)
    expect(player.seek(20*60)).toBe(true)
    expect(player.seek(6*60)).toBe(true)
    expect(seekOutside).toHaveBeenCalledTimes(2)
    expect(element.textTracks[0]).toBe(selected)
    expect(selected.mode).toBe('showing')
  })

  it('accepts real PTS drift and appends each window init without rebuilding subtitles',async()=>{
    let createdSourceBuffer:FakeSourceBuffer|undefined
    class FakeSourceBuffer extends EventTarget {
      mode:AppendMode='segments'
      updating=false
      timestampOffset=0
      ranges:Array<[number,number]>=[]
      mediaAppendCount=0
      get buffered(){return {length:this.ranges.length,start:(index:number)=>this.ranges[index][0],end:(index:number)=>this.ranges[index][1]} as TimeRanges}
      appendBuffer(data:ArrayBuffer){
        if(new Uint8Array(data)[0]>=4){
          this.mediaAppendCount+=1
          this.ranges=this.mediaAppendCount===1?[[226.8,227.3]]:[[226.8,228.7]]
        }
        queueMicrotask(()=>this.dispatchEvent(new Event('updateend')))
      }
      abort(){}
      remove(){this.ranges=[];queueMicrotask(()=>this.dispatchEvent(new Event('updateend')))}
    }
    class FakeMediaSource extends EventTarget {
      readyState:'closed'|'open'|'ended'='open'
      duration=Number.NaN
      sourceBuffer=new FakeSourceBuffer()
      constructor(){super();createdSourceBuffer=this.sourceBuffer;queueMicrotask(()=>this.dispatchEvent(new Event('sourceopen')))}
      addSourceBuffer(){return this.sourceBuffer as unknown as SourceBuffer}
      removeSourceBuffer(){}
    }
    Object.defineProperty(globalThis,'MediaSource',{configurable:true,value:FakeMediaSource})
    vi.spyOn(URL,'createObjectURL').mockReturnValue('blob:revaro-mse')
    vi.spyOn(URL,'revokeObjectURL').mockImplementation(()=>{})
    const fetchSignals:AbortSignal[]=[]
    let indexRequests=0
    const fetchMock=vi.spyOn(globalThis,'fetch').mockImplementation(async(input,init)=>{
      if(init?.signal)fetchSignals.push(init.signal)
      const requestURL=String(input)
      if(requestURL==='/init-window-a.mp4')return new Response(new Uint8Array([1,2,3]))
      if(requestURL==='/init-window-b.mp4')return new Response(new Uint8Array([2,3,4]))
      if(requestURL.includes('/index.json')){
        indexRequests+=1
        if(indexRequests>1)return Response.json({fragments:[],available_until:229.393,done:true})
        return Response.json({
          fragments:[
            {number:1,start:227.393,duration:1,url:'/fragment-window-a-000001.m4s',init_url:'/init-window-a.mp4',window_start:210,timestamp_offset:0,timing_approximate:true},
            {number:1,start:228.393,duration:1,url:'/fragment-window-b-000001.m4s',init_url:'/init-window-b.mp4',window_start:270,timestamp_offset:0,timing_approximate:true},
          ],available_until:229.393,done:false,
        })
      }
      if(requestURL.includes('window-a'))return new Response(new Uint8Array([4,5,6]))
      return new Response(new Uint8Array([5,6,7]))
    })
    const selected={mode:'showing' as TextTrackMode}
    const events=new EventTarget()
    const element={src:'',currentTime:227.393,readyState:1,networkState:2,paused:false,textTracks:[selected],seekable:{length:0,start:()=>0,end:()=>0},
      addEventListener:events.addEventListener.bind(events),removeEventListener:events.removeEventListener.bind(events),
      load:vi.fn(),pause:vi.fn(),play:vi.fn(async()=>{}),removeAttribute:vi.fn(function(this:{src:string}){this.src=''}),
    } as unknown as HTMLVideoElement
    const response:VideoFMP4Response={
      ...metadata(),duration:400,session_id:'session',init_url:'/init.mp4',index_url:'/index.json',start:0,requested_start:227.393,
      output_audio_codec:'aac',audio_transcoding:false,selected_mode:'mse-copy',
    }
    const onFragment=vi.fn()
    const onFatal=vi.fn()
    const attachment=await attachFMP4Stream({element,response,mimeType:response.mime_type,target:227.393,autoplay:false,onFatal,onFragment})
    await vi.waitFor(()=>expect(onFragment).toHaveBeenCalledTimes(2))
    expect(fetchMock).toHaveBeenCalledWith('/init-window-a.mp4',expect.objectContaining({credentials:'same-origin'}))
    expect(fetchMock).toHaveBeenCalledWith('/init-window-b.mp4',expect.objectContaining({credentials:'same-origin'}))
    expect(fetchMock).not.toHaveBeenCalledWith('/init.mp4',expect.anything())
    expect(fetchMock).toHaveBeenCalledWith('/fragment-window-a-000001.m4s',expect.objectContaining({credentials:'same-origin'}))
    expect(fetchMock).toHaveBeenCalledWith('/fragment-window-b-000001.m4s',expect.objectContaining({credentials:'same-origin'}))
    expect(createdSourceBuffer?.timestampOffset).toBe(0)
    expect(createdSourceBuffer?.buffered.start(0)).toBe(226.8)
    expect(createdSourceBuffer?.buffered.end(0)).toBe(228.7)
    expect(onFatal).not.toHaveBeenCalled()
    expect(element.textTracks[0]).toBe(selected)
    expect(selected.mode).toBe('showing')
    expect(fetchSignals.every(signal=>!signal.aborted)).toBe(true)
    attachment.seek(300)
    expect(fetchSignals.every(signal=>signal.aborted)).toBe(true)
    attachment.destroy()
  })
})

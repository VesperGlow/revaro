import { afterEach, describe, expect, it, vi } from 'vitest'
import type { VideoFMP4Metadata, VideoFMP4Response } from './types'
import { attachFMP4Stream, bufferedRangesAddedSeconds, createUnifiedVideoPlayer, mseCompatibility, mseFreshRecoveryLimit, mseRecoveryAction, mseStallWatchdogSeconds, mseStreamBufferGoalSeconds, mseWatchdogExpired, setExclusiveSubtitleTrack, shouldHideVideoCursor, subtitleTrackKey, subtitleURLForPlayback } from './videoPlayer'

const metadata=(videoCodec='hevc',audioCodec='aac'):VideoFMP4Metadata=>({
  duration:120,video_codec:videoCodec,audio_codec:audioCodec,
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

describe('MSE stdout stream policy',()=>{
  it('uses a bounded browser buffer to apply HTTP backpressure',()=>{
    expect(mseStreamBufferGoalSeconds).toBeGreaterThanOrEqual(30)
    expect(mseStreamBufferGoalSeconds).toBeLessThanOrEqual(60)
  })
  it('rebuilds MSE twice before falling back to HLS',()=>{
    expect(mseFreshRecoveryLimit).toBe(2)
    expect(mseRecoveryAction(0)).toBe('fresh-mse')
    expect(mseRecoveryAction(1)).toBe('fresh-mse')
    expect(mseRecoveryAction(2)).toBe('hls')
    const limit=mseStallWatchdogSeconds*1000
    expect(mseWatchdogExpired(1000,1000,1000+limit-1,false)).toBe(false)
    expect(mseWatchdogExpired(1000,1000,1000+limit,false)).toBe(true)
    expect(mseWatchdogExpired(1000,1000,1000+limit,true)).toBe(false)
  })
  it('measures actual browser buffer growth without estimated fragment timestamps',()=>{
    expect(bufferedRangesAddedSeconds([],[{start:226.8,end:227.3}])).toBeCloseTo(.5)
    expect(bufferedRangesAddedSeconds([{start:226.8,end:227.3}],[{start:226.8,end:227.3}])).toBe(0)
    expect(bufferedRangesAddedSeconds([{start:226.8,end:227.0}],[{start:226.8,end:227.5}])).toBeCloseTo(.5)
  })
})

describe('mseCompatibility',()=>{
  it.each([['hevc','aac'],['h264','aac']])('%s + %s selects video/audio copy',async(video,audio)=>{
    const value=metadata(video,audio)
    browserSupports([value.video_mime_type,value.audio_mime_type!,value.aac_audio_mime_type!,value.mime_type,value.aac_mime_type])
    expect((await mseCompatibility(value)).mode).toBe('copy')
  })
  it('keeps HEVC and only converts unsupported EAC3 to AAC',async()=>{
    const value=metadata('hevc','eac3')
    browserSupports([value.video_mime_type,value.aac_audio_mime_type!,value.aac_mime_type])
    const result=await mseCompatibility(value)
    expect(result.videoSupported).toBe(true);expect(result.audioSupported).toBe(false);expect(result.mode).toBe('aac')
  })
  it('uses HLS only when video decoding is unsupported',async()=>{
    const value=metadata('hevc','aac');browserSupports([value.audio_mime_type!,value.aac_audio_mime_type!],false)
    const result=await mseCompatibility(value)
    expect(result.mode).toBe('hls');expect(result.fallbackReason).toContain('HEVC')
  })
  it('requires support for the exact HEVC MSE codec string',async()=>{
    const value=metadata('hevc','aac')
    browserSupports(['video/mp4; codecs="hvc1"',value.audio_mime_type!,value.aac_audio_mime_type!])
    const result=await mseCompatibility(value)
    expect(result.mode).toBe('hls');expect(result.videoSupported).toBe(false)
  })
  it('does not enable H.264 fallback for an audio-only capability failure',async()=>{
    const value=metadata('hevc','eac3');browserSupports([value.video_mime_type])
    expect((await mseCompatibility(value)).mode).toBe('error')
  })
  it('accepts a supported decoder that is not power efficient',async()=>{
    const value=metadata();browserSupports([value.video_mime_type,value.audio_mime_type!,value.aac_audio_mime_type!,value.mime_type,value.aac_mime_type],true,false)
    const result=await mseCompatibility(value);expect(result.mode).toBe('copy');expect(result.powerEfficient).toBe(false)
  })
})

describe('video subtitle timeline and lifecycle',()=>{
  const url='/api/files/video/video/subtitles/embedded-2'
  it('keeps direct and MSE subtitles on the global timeline at saved positions',()=>{
    expect(subtitleURLForPlayback(url,'direct',360)).toBe(url)
    expect(subtitleURLForPlayback(url,'mse',360)).toBe(url)
    expect(subtitleTrackKey('embedded-2','mse',0)).toBe(subtitleTrackKey('embedded-2','mse',360))
  })
  it('uses and keys only HLS session offsets',()=>{
    expect(subtitleURLForPlayback(url,'hls',360)).toBe(`${url}?start=360.000`)
    expect(subtitleTrackKey('embedded-2','hls',360)).not.toBe(subtitleTrackKey('embedded-2','hls',1200))
  })
  it('shows only the selected track',()=>{
    const first={mode:'disabled' as TextTrackMode},second={mode:'disabled' as TextTrackMode},tracks=[first,second]
    setExclusiveSubtitleTrack(tracks,second);expect(tracks.map(track=>track.mode)).toEqual(['disabled','showing'])
    setExclusiveSubtitleTrack(tracks,null);expect(tracks.map(track=>track.mode)).toEqual(['disabled','disabled'])
  })
  it('hands seeks outside buffered ranges to a new stream session',()=>{
    const selected={mode:'showing' as TextTrackMode},seekOutside=vi.fn(()=>true)
    const element={currentTime:360,seekable:{length:0,start:()=>0,end:()=>0},textTracks:[selected]} as unknown as HTMLVideoElement
    const player=createUnifiedVideoPlayer('mse',element,0,()=>{},seekOutside)
    expect(player.seek(1200)).toBe(true);expect(seekOutside).toHaveBeenCalledWith(1200);expect(selected.mode).toBe('showing')
  })
})

describe('MSE streamed fMP4 attachment',()=>{
  it('appends a ReadableStream, accepts PTS drift, and aborts fetch on seek',async()=>{
    let createdSourceBuffer:FakeSourceBuffer|undefined
    class FakeSourceBuffer extends EventTarget {
      mode:AppendMode='segments';updating=false;timestampOffset=0;ranges:Array<[number,number]>=[]
      get buffered(){return {length:this.ranges.length,start:(i:number)=>this.ranges[i][0],end:(i:number)=>this.ranges[i][1]} as TimeRanges}
      appendBuffer(data:ArrayBuffer){if(new Uint8Array(data)[0]>=4)this.ranges=[[226.8,228.7]];queueMicrotask(()=>this.dispatchEvent(new Event('updateend')))}
      abort(){} remove(){this.ranges=[];queueMicrotask(()=>this.dispatchEvent(new Event('updateend')))}
    }
    class FakeMediaSource extends EventTarget {
      readyState:'closed'|'open'|'ended'='open';duration=Number.NaN;sourceBuffer=new FakeSourceBuffer()
      constructor(){super();createdSourceBuffer=this.sourceBuffer;queueMicrotask(()=>this.dispatchEvent(new Event('sourceopen')))}
      addSourceBuffer(){return this.sourceBuffer as unknown as SourceBuffer} removeSourceBuffer(){} endOfStream(){this.readyState='ended'}
    }
    Object.defineProperty(globalThis,'MediaSource',{configurable:true,value:FakeMediaSource})
    vi.spyOn(URL,'createObjectURL').mockReturnValue('blob:revaro-mse');vi.spyOn(URL,'revokeObjectURL').mockImplementation(()=>{})
    let fetchSignal:AbortSignal|undefined
    const fetchMock=vi.spyOn(globalThis,'fetch').mockImplementation(async(input,init)=>{
      fetchSignal=init?.signal??undefined
      const body=new ReadableStream<Uint8Array>({start(controller){controller.enqueue(new Uint8Array([1,2,3]));controller.enqueue(new Uint8Array([4,5,6]));controller.close()}})
      return new Response(body,{status:200,headers:{'Content-Type':'video/mp4'}})
    })
    const selected={mode:'showing' as TextTrackMode},events=new EventTarget()
    const element={src:'',currentTime:227.393,readyState:1,networkState:2,paused:false,textTracks:[selected],seekable:{length:0,start:()=>0,end:()=>0},
      addEventListener:events.addEventListener.bind(events),removeEventListener:events.removeEventListener.bind(events),load:vi.fn(),pause:vi.fn(),play:vi.fn(async()=>{}),removeAttribute:vi.fn(),
    } as unknown as HTMLVideoElement
    const response:VideoFMP4Response={...metadata(),duration:400,session_id:'session',stream_url:'/api/video/fmp4/session/stream',start:0,requested_start:227.393,output_audio_codec:'aac',audio_transcoding:false,selected_mode:'mse-copy'}
    const onFragment=vi.fn(),onFatal=vi.fn()
    const attachment=await attachFMP4Stream({element,response,mimeType:response.mime_type,target:227.393,autoplay:false,onFatal,onFragment})
    expect(fetchMock).toHaveBeenCalledWith(response.stream_url,expect.objectContaining({credentials:'same-origin',cache:'no-store'}))
    expect(createdSourceBuffer?.timestampOffset).toBe(227.393)
    expect(createdSourceBuffer?.buffered.start(0)).toBe(226.8);expect(createdSourceBuffer?.buffered.end(0)).toBe(228.7)
    expect(onFragment).toHaveBeenCalledTimes(1);expect(onFatal).not.toHaveBeenCalled();expect(selected.mode).toBe('showing')
    expect(fetchSignal?.aborted).toBe(false);expect(attachment.seek(300)).toBe(false);expect(fetchSignal?.aborted).toBe(true)
  })
})

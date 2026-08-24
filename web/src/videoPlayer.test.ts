import { afterEach, describe, expect, it, vi } from 'vitest'
import type { VideoFMP4Metadata, VideoFMP4Response } from './types'
import { attachFMP4Stream, createUnifiedVideoPlayer, mseCompatibility, setExclusiveSubtitleTrack, subtitleTrackKey, subtitleURLForPlayback } from './videoPlayer'

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

  it('leaves the selected track untouched while MSE appends a fragment',async()=>{
    class FakeSourceBuffer extends EventTarget {
      mode:AppendMode='segments'
      updating=false
      buffered={length:0,start:()=>0,end:()=>0} as TimeRanges
      appendBuffer(){queueMicrotask(()=>this.dispatchEvent(new Event('updateend')))}
      abort(){}
      remove(){queueMicrotask(()=>this.dispatchEvent(new Event('updateend')))}
    }
    class FakeMediaSource extends EventTarget {
      readyState:'closed'|'open'|'ended'='open'
      duration=Number.NaN
      sourceBuffer=new FakeSourceBuffer()
      constructor(){super();queueMicrotask(()=>this.dispatchEvent(new Event('sourceopen')))}
      addSourceBuffer(){return this.sourceBuffer as unknown as SourceBuffer}
      removeSourceBuffer(){}
    }
    Object.defineProperty(globalThis,'MediaSource',{configurable:true,value:FakeMediaSource})
    vi.spyOn(URL,'createObjectURL').mockReturnValue('blob:revaro-mse')
    vi.spyOn(URL,'revokeObjectURL').mockImplementation(()=>{})
    const fetchMock=vi.spyOn(globalThis,'fetch').mockImplementation(async input=>{
      const requestURL=String(input)
      if(requestURL.endsWith('/init.mp4'))return new Response(new Uint8Array([1,2,3]))
      if(requestURL.includes('/index.json'))return Response.json({
        fragments:[{number:1,start:360,duration:2,url:'/fragment-000001.m4s'}],available_until:362,done:false,
      })
      return new Response(new Uint8Array([4,5,6]))
    })
    const selected={mode:'showing' as TextTrackMode}
    const element={
      src:'',currentTime:360,textTracks:[selected],seekable:{length:0,start:()=>0,end:()=>0},
      load:vi.fn(),pause:vi.fn(),play:vi.fn(async()=>{}),removeAttribute:vi.fn(function(this:{src:string}){this.src=''}),
    } as unknown as HTMLVideoElement
    const response:VideoFMP4Response={
      ...metadata(),session_id:'session',init_url:'/init.mp4',index_url:'/index.json',start:0,requested_start:360,
      output_audio_codec:'aac',audio_transcoding:false,selected_mode:'mse-copy',
    }
    const onFragment=vi.fn()
    const attachment=await attachFMP4Stream({element,response,mimeType:response.mime_type,target:360,autoplay:false,onFatal:vi.fn(),onFragment})
    expect(onFragment).toHaveBeenCalled()
    expect(fetchMock).toHaveBeenCalledWith('/fragment-000001.m4s',expect.objectContaining({credentials:'same-origin'}))
    expect(element.textTracks[0]).toBe(selected)
    expect(selected.mode).toBe('showing')
    attachment.destroy()
  })
})

import { afterEach, describe, expect, it, vi } from 'vitest'
import type { VideoFMP4Metadata } from './types'
import { mseCompatibility } from './videoPlayer'

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

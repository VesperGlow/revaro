import type { VideoFMP4Index, VideoFMP4Metadata, VideoFMP4Response } from './types'

export const mseWindowRefillLeadSeconds=12
export const mseStallWatchdogSeconds=8
export const mseFreshRecoveryLimit=2

export function mseRecoveryAction(freshFailures:number):'fresh-mse'|'hls'{
  return freshFailures<mseFreshRecoveryLimit?'fresh-mse':'hls'
}

export function mseWatchdogExpired(waitingSince:number,lastBufferGrowth:number,now:number,playheadCovered:boolean):boolean{
  const limit=mseStallWatchdogSeconds*1000
  return !playheadCovered&&waitingSince>0&&now-waitingSince>=limit&&now-lastBufferGrowth>=limit
}

export type VideoPlaybackMode='direct'|'mse'|'hls'

export interface VideoCursorState {
  playing:boolean
  controlsVisible:boolean
  starting:boolean
  buffering:boolean
  error:string|boolean
}

export function shouldHideVideoCursor(state:VideoCursorState):boolean{
  return state.playing&&!state.controlsVisible&&!state.starting&&!state.buffering&&!state.error
}

type MutableTextTrack=Pick<TextTrack,'mode'>

export function subtitleURLForPlayback(url:string,mode:VideoPlaybackMode,streamOffset=0):string{
  if(!url||mode!=='hls'||!Number.isFinite(streamOffset)||streamOffset<=0)return url
  const offset=Math.floor(streamOffset*1000)/1000
  return `${url}${url.includes('?')?'&':'?'}start=${offset.toFixed(3)}`
}

export function subtitleTrackKey(id:string,mode:VideoPlaybackMode,streamOffset=0):string{
  if(mode!=='hls'||!Number.isFinite(streamOffset)||streamOffset<=0)return `global:${id}`
  return `hls:${Math.floor(streamOffset*1000)/1000}:${id}`
}

export function setExclusiveSubtitleTrack<T extends MutableTextTrack>(tracks:ArrayLike<T>,selected:T|null):void{
  for(let index=0;index<tracks.length;index+=1){
    tracks[index].mode=selected&&tracks[index]===selected?'showing':'disabled'
  }
}

export interface UnifiedVideoPlayer {
  readonly mode:VideoPlaybackMode
  readonly offset:number
  play():Promise<void>
  pause():void
  seek(globalTime:number):boolean
  setVolume(value:number,muted:boolean):void
  setSubtitle(track:TextTrack|null):void
  requestFullscreen(container:HTMLElement):Promise<void>
  destroy():void
}

export function createUnifiedVideoPlayer(
  mode:VideoPlaybackMode,
  element:HTMLVideoElement,
  offset=0,
  cleanup:()=>void=()=>{},
  seekOutside?:(globalTime:number)=>boolean,
):UnifiedVideoPlayer {
  return {
    mode,
    offset,
    play:async()=>{await element.play()},
    pause:()=>element.pause(),
    seek:(globalTime:number)=>{
      const local=Math.max(0,globalTime-offset)
      if(mode==='direct'){
        element.currentTime=local
        return true
      }
      const ranges=element.seekable
      for(let index=0;index<ranges.length;index+=1){
        if(local>=ranges.start(index)-.5&&local<=ranges.end(index)+.5){
          element.currentTime=local
          return true
        }
      }
      return seekOutside?.(globalTime)??false
    },
    setVolume:(value:number,muted:boolean)=>{element.volume=value;element.muted=muted},
    setSubtitle:(track:TextTrack|null)=>setExclusiveSubtitleTrack(element.textTracks,track),
    requestFullscreen:async(container:HTMLElement)=>{
      if(document.fullscreenElement)await document.exitFullscreen()
      else try{await container.requestFullscreen({navigationUI:'hide'})}
      catch{(element as HTMLVideoElement&{webkitEnterFullscreen?:()=>void}).webkitEnterFullscreen?.()}
    },
    destroy:cleanup,
  }
}

export interface MSECompatibility {
  videoSupported:boolean
  audioSupported:boolean
  aacAudioSupported:boolean
  combinedCopySupported:boolean
  combinedAACSupported:boolean
  powerEfficient?:boolean
  mode:'copy'|'aac'|'hls'|'error'
  mimeType:string
  fallbackReason:string
}

function supportedMIME(contentType:string,videoCodec:string):string{
  if(!contentType)return ''
  if(MediaSource.isTypeSupported(contentType))return contentType
  if(/^(hevc|h265)$/i.test(videoCodec)){
    const generic=contentType.replace(/hvc1\.[^," ]+/i,'hvc1')
    if(generic!==contentType&&MediaSource.isTypeSupported(generic))return generic
  }
  return ''
}

export async function mseCompatibility(metadata:VideoFMP4Metadata):Promise<MSECompatibility>{
  const unavailable:MSECompatibility={
    videoSupported:false,audioSupported:false,aacAudioSupported:false,
    combinedCopySupported:false,combinedAACSupported:false,mode:'hls',mimeType:'',
    fallbackReason:'浏览器没有 MediaSource Extensions',
  }
  if(typeof MediaSource==='undefined')return unavailable
  const videoMIME=supportedMIME(metadata.video_mime_type,metadata.video_codec)
  const audioSupported=!metadata.audio_codec||Boolean(metadata.audio_mime_type&&MediaSource.isTypeSupported(metadata.audio_mime_type))
  const aacAudioSupported=!metadata.audio_codec||Boolean(metadata.aac_audio_mime_type&&MediaSource.isTypeSupported(metadata.aac_audio_mime_type))
  const copyMIME=supportedMIME(metadata.mime_type,metadata.video_codec)
  const aacMIME=supportedMIME(metadata.aac_mime_type,metadata.video_codec)
  let videoSupported=Boolean(videoMIME)
  let powerEfficient: boolean|undefined
  if(videoSupported&&navigator.mediaCapabilities?.decodingInfo){
    try{
      const result=await navigator.mediaCapabilities.decodingInfo({
        type:'media-source',
        video:{
          contentType:videoMIME,width:Math.max(1,metadata.width),height:Math.max(1,metadata.height),
          bitrate:Math.max(1,metadata.bitrate),framerate:Math.max(1,metadata.frame_rate),
        },
      })
      videoSupported=result.supported
      powerEfficient=result.powerEfficient
    }catch{/* isTypeSupported remains authoritative when MediaCapabilities is incomplete */}
  }
  const result:MSECompatibility={
    videoSupported,audioSupported,aacAudioSupported,
    combinedCopySupported:Boolean(copyMIME),combinedAACSupported:Boolean(aacMIME),powerEfficient,
    mode:'hls',mimeType:'',fallbackReason:'',
  }
  if(!videoSupported){
    result.fallbackReason=`浏览器不支持 ${metadata.video_codec.toUpperCase()} MSE/解码`
  }else if(audioSupported&&copyMIME){
    result.mode='copy';result.mimeType=copyMIME
  }else if(aacAudioSupported&&aacMIME){
    result.mode='aac';result.mimeType=aacMIME
    result.fallbackReason=metadata.audio_codec?`${metadata.audio_codec.toUpperCase()} MSE 不受支持，仅将音频转为 AAC`:''
  }else{
    result.mode='error'
    result.fallbackReason=`MSE 不支持 ${metadata.audio_codec?`${metadata.audio_codec.toUpperCase()} 或 AAC 音频`:'该 fMP4 组合'}`
  }
  return result
}

interface FMP4AttachOptions {
  element:HTMLVideoElement
  response:VideoFMP4Response
  mimeType:string
  target:number
  autoplay:boolean
  onFatal:(reason:string)=>void
  onFragment?:()=>void
}

export interface FMP4Attachment { destroy:()=>void;seek:(globalTime:number)=>boolean }

export async function attachFMP4Stream(options:FMP4AttachOptions):Promise<FMP4Attachment>{
  const {element,response,autoplay,onFatal}=options
  const mediaSource=new MediaSource()
  const objectURL=URL.createObjectURL(mediaSource)
  const lifetimeController=new AbortController()
  let operationController=new AbortController()
  let sourceBuffer:SourceBuffer|null=null
  let disposed=false
  let readySettled=false
  let requestedTarget=Math.max(0,options.target-response.start)
  let seekVersion=0
  let inFlight='none'
  let waitingSince=0
  let lastWatchdogLog=0
  let lastBufferGrowth=Date.now()
  let lastBuffered='[]'
  let watchdogTimer=0
  let watchdogFired=false
  let resolveReady:()=>void=()=>{}
  let rejectReady:(error:Error)=>void=()=>{}
  const ready=new Promise<void>((resolve,reject)=>{resolveReady=resolve;rejectReady=reject})

  const state=()=>({
    currentTime:element.currentTime,readyState:element.readyState,networkState:element.networkState,
    paused:element.paused,buffered:formatTimeRanges(sourceBuffer?.buffered),inFlight,
    sourceBufferUpdating:sourceBuffer?.updating??false,mediaSourceState:mediaSource.readyState,
  })
  const log=(event:string,extra:Record<string,unknown>={})=>console.info('[revaro][mse]',event,{...state(),...extra})
  const recordBuffer=(kind:string,url:string)=>{
    const buffered=formatTimeRanges(sourceBuffer?.buffered)
    if(buffered!==lastBuffered){lastBuffered=buffered;lastBufferGrowth=Date.now()}
    log('append complete',{kind,url})
  }
  const clearWaiting=()=>{waitingSince=0}
  const noteWaiting=(eventName:string)=>{
    if(disposed||!readySettled||element.paused)return
    const now=Date.now()
    if(bufferContains(sourceBuffer?.buffered,element.currentTime,.25)){clearWaiting();return}
    if(!waitingSince){waitingSince=now;log(`${eventName} started`)}
    else if(now-lastWatchdogLog>=2000){lastWatchdogLog=now;log(`${eventName} ongoing`,{duration_ms:now-waitingSince})}
  }
  const inspectWatchdog=()=>{
    if(disposed||watchdogFired||!readySettled||element.paused)return
    const now=Date.now()
    const buffered=formatTimeRanges(sourceBuffer?.buffered)
    if(buffered!==lastBuffered){lastBuffered=buffered;lastBufferGrowth=now}
    if(bufferContains(sourceBuffer?.buffered,element.currentTime,.25)){clearWaiting();return}
    if(!waitingSince&&element.readyState<=2){waitingSince=now;log('watchdog observed unbuffered playhead')}
    if(!waitingSince)return
    const stalledFor=now-waitingSince
    const noGrowthFor=now-lastBufferGrowth
    if(now-lastWatchdogLog>=2000){lastWatchdogLog=now;log('waiting/stalled ongoing',{duration_ms:stalledFor,no_buffer_growth_ms:noGrowthFor})}
    if(!mseWatchdogExpired(waitingSince,lastBufferGrowth,now,false))return
    watchdogFired=true
    const reason=`MSE watchdog: ${Math.round(stalledFor/1000)}s 无可播放缓冲且缓冲没有增长`
    console.warn('[revaro][mse] watchdog recovery',{reason,...state(),waiting_ms:stalledFor,no_buffer_growth_ms:noGrowthFor})
    fail(reason)
  }
  const onWaiting=()=>noteWaiting('waiting')
  const onStalled=()=>noteWaiting('stalled')
  const onPlayable=()=>{if(bufferContains(sourceBuffer?.buffered,element.currentTime,.25))clearWaiting()}

  const fail=(reason:string)=>{
    if(disposed)return
    if(!readySettled){readySettled=true;rejectReady(new Error(reason))}
    else onFatal(reason)
  }
  const destroy=()=>{
    if(disposed)return
    disposed=true;lifetimeController.abort();operationController.abort();globalThis.clearInterval(watchdogTimer)
    element.removeEventListener('waiting',onWaiting);element.removeEventListener('stalled',onStalled)
    element.removeEventListener('playing',onPlayable);element.removeEventListener('canplay',onPlayable);element.removeEventListener('timeupdate',onPlayable)
    try{if(sourceBuffer?.updating)sourceBuffer.abort()}catch{/* already detached */}
    try{if(sourceBuffer&&mediaSource.readyState==='open')mediaSource.removeSourceBuffer(sourceBuffer)}catch{/* already detached */}
    if(element.src===objectURL){element.pause();element.removeAttribute('src');element.load()}
    URL.revokeObjectURL(objectURL)
  }
  const seek=(globalTime:number)=>{
    if(disposed||!Number.isFinite(globalTime))return false
    requestedTarget=Math.max(0,Math.min(response.duration,globalTime-response.start))
    const previousVersion=seekVersion
    seekVersion+=1
    operationController.abort();operationController=new AbortController();waitingSince=Date.now();lastBufferGrowth=Date.now();watchdogFired=false
    log('seek generation changed',{from:previousVersion,to:seekVersion,target:requestedTarget})
    try{if(sourceBuffer?.updating)sourceBuffer.abort()}catch{/* the pump will rebuild this generation */}
    try{element.currentTime=requestedTarget}catch{/* set again after the target fragment is appended */}
    return true
  }

  element.src=objectURL;element.load()
  element.addEventListener('waiting',onWaiting);element.addEventListener('stalled',onStalled)
  element.addEventListener('playing',onPlayable);element.addEventListener('canplay',onPlayable);element.addEventListener('timeupdate',onPlayable)
  mediaSource.addEventListener('sourceopen',()=>{
    if(disposed)return
    try{
      sourceBuffer=mediaSource.addSourceBuffer(options.mimeType)
      sourceBuffer.mode='segments'
      sourceBuffer.addEventListener('error',()=>fail('MSE SourceBuffer 解码 fMP4 失败'))
      log('MediaSource attached',{mimeType:options.mimeType})
      watchdogTimer=globalThis.setInterval(inspectWatchdog,1000) as unknown as number
      void pumpFMP4Fragments(sourceBuffer,mediaSource,options,lifetimeController.signal,()=>operationController.signal,()=>disposed,()=>requestedTarget,()=>seekVersion,value=>{inFlight=value},recordBuffer,log,()=>{
        if(!readySettled){readySettled=true;resolveReady()}
      }).catch(caught=>{
        if(caught instanceof DOMException&&caught.name==='AbortError'&&disposed)return
        fail(caught instanceof Error?caught.message:'MSE fMP4 分片读取失败')
      })
    }catch(caught){fail(caught instanceof Error?caught.message:'无法创建 MSE SourceBuffer')}
  },{once:true})

  await ready
  if(autoplay)void element.play().catch(()=>{})
  return {destroy,seek}
}

async function pumpFMP4Fragments(
  sourceBuffer:SourceBuffer,
  mediaSource:MediaSource,
  options:FMP4AttachOptions,
  lifetimeSignal:AbortSignal,
  operationSignal:()=>AbortSignal,
  isDisposed:()=>boolean,
  target:()=>number,
  version:()=>number,
  setInFlight:(value:string)=>void,
  recordBuffer:(kind:string,url:string)=>void,
  log:(event:string,extra?:Record<string,unknown>)=>void,
  markReady:()=>void,
):Promise<void>{
  if(mediaSource.readyState==='open')try{mediaSource.duration=Math.max(.1,options.response.duration)}catch{/* duration may already be known */}
  let cursor=target()
  let observedVersion=version()
  let currentInitURL=''
  let currentWindowStart=Number.NaN
  let firstMedia=false
  let consecutiveFailures=0
  while(!isDisposed()){
    if(observedVersion!==version()){
      observedVersion=version();cursor=target();currentInitURL='';currentWindowStart=Number.NaN
      const signal=operationSignal()
      setInFlight(`clear buffer for seek generation ${observedVersion}`)
      try{await clearSourceBuffer(sourceBuffer,signal)}
      catch(caught){
        if(signal.aborted){if(lifetimeSignal.aborted)throw new DOMException('Aborted','AbortError');continue}
        throw caught
      }
      setInFlight('none')
      log('seek buffer cleared',{generation:observedVersion,target:cursor})
    }
    const requestVersion=observedVersion
    const signal=operationSignal()
    try{
      const separator=options.response.index_url.includes('?')?'&':'?'
      const indexURL=`${options.response.index_url}${separator}time=${cursor.toFixed(3)}`
      setInFlight(`index ${indexURL}`)
      const response=await fetch(indexURL,{credentials:'same-origin',signal})
      if(!response.ok)throw new Error(`fMP4 分片索引请求失败 (${response.status})`)
      const index=await response.json() as VideoFMP4Index
      if(requestVersion!==version())continue
      if(index.error)throw new Error(`fMP4 remux 已退出：${index.error}`)
      if(!index.fragments.length){
        consecutiveFailures=0
        if(index.done){
          // Keep MediaSource open after the remux completes. Old ranges may be
          // evicted later; a backwards seek must still be able to reappend the
          // cached fragment without constructing another session.
          setInFlight('index complete; waiting for seek')
          await abortableDelay(500,signal)
        }
        continue
      }
      for(const fragment of index.fragments){
        if(requestVersion!==version())break
        const midpoint=fragment.start+Math.min(fragment.duration/2,.25)
        if(!bufferContains(sourceBuffer.buffered,midpoint,.25)){
          const fragmentInitURL=fragment.init_url||currentInitURL
          const timestampOffset=Number.isFinite(fragment.timestamp_offset)?fragment.timestamp_offset:fragment.window_start
          if(!Number.isFinite(timestampOffset))throw new Error('fMP4 分片缺少窗口时间轴偏移')
          if(fragmentInitURL!==currentInitURL&&currentInitURL===''){
            setInFlight(`init ${fragmentInitURL}`)
            const windowInit=await fetchBytes(fragmentInitURL,signal)
            if(requestVersion!==version())break
            sourceBuffer.timestampOffset=timestampOffset
            await appendSourceBuffer(sourceBuffer,windowInit,signal)
            recordBuffer('init',fragmentInitURL)
            currentInitURL=fragmentInitURL
            currentWindowStart=timestampOffset
            log('window init attached',{init_url:fragmentInitURL,window_start:fragment.window_start,timestamp_offset:timestampOffset})
          }else if(fragmentInitURL!==currentInitURL){
            // A session keeps the same codecs and track layout. Reusing the
            // decoder configuration avoids appending a second independent init
            // segment from another FFmpeg invocation to the same SourceBuffer.
            log('window init switched; decoder config reused',{from:currentInitURL,to:fragmentInitURL,window_start:fragment.window_start,timestamp_offset:timestampOffset})
            currentInitURL=fragmentInitURL
          }
          if(currentWindowStart!==timestampOffset){
            sourceBuffer.timestampOffset=timestampOffset
            currentWindowStart=timestampOffset
            log('window timestamp offset changed',{window_start:fragment.window_start,timestamp_offset:timestampOffset})
          }
          setInFlight(`fragment ${fragment.url}`)
          const bytes=await fetchBytes(fragment.url,signal)
          if(requestVersion!==version())break
          await appendSourceBuffer(sourceBuffer,bytes,signal)
          recordBuffer('media',fragment.url)
          if(!bufferContains(sourceBuffer.buffered,midpoint,.25)){
            throw new Error(`MSE append updateend 后没有覆盖目标时间 ${midpoint.toFixed(3)}s`)
          }
        }
        cursor=fragment.start+fragment.duration+.002
        if(!firstMedia){
          firstMedia=true
          options.element.currentTime=target()
          markReady()
        }
        options.onFragment?.()
        await keepMSEBufferBounded(sourceBuffer,options.element,signal,()=>requestVersion!==version())
      }
      consecutiveFailures=0
    }catch(caught){
      if(signal.aborted){
        if(lifetimeSignal.aborted)throw new DOMException('Aborted','AbortError')
        continue
      }
      consecutiveFailures+=1
      if(consecutiveFailures>=4)throw caught
      await abortableDelay(500*consecutiveFailures,signal)
    }finally{
      setInFlight('none')
    }
  }
}

async function fetchBytes(url:string,signal:AbortSignal):Promise<Uint8Array>{
  const response=await fetch(url,{credentials:'same-origin',signal})
  if(!response.ok)throw new Error(`fMP4 分片请求失败 (${response.status})`)
  return new Uint8Array(await response.arrayBuffer())
}

function bufferContains(ranges:TimeRanges|undefined,time:number,tolerance=.05):boolean{
  if(!ranges)return false
  for(let index=0;index<ranges.length;index+=1){
    if(time>=ranges.start(index)-tolerance&&time<=ranges.end(index)+tolerance)return true
  }
  return false
}

function formatTimeRanges(ranges:TimeRanges|undefined):string{
  if(!ranges)return '[]'
  const values:string[]=[]
  for(let index=0;index<ranges.length;index+=1)values.push(`${ranges.start(index).toFixed(3)}-${ranges.end(index).toFixed(3)}`)
  return `[${values.join(', ')}]`
}

function appendSourceBuffer(sourceBuffer:SourceBuffer,data:Uint8Array,signal:AbortSignal):Promise<void>{
  return new Promise((resolve,reject)=>{
    if(signal.aborted){reject(new DOMException('Aborted','AbortError'));return}
    const cleanup=()=>{sourceBuffer.removeEventListener('updateend',done);sourceBuffer.removeEventListener('error',failed);signal.removeEventListener('abort',aborted)}
    const done=()=>{cleanup();resolve()}
    const failed=()=>{cleanup();reject(new Error('MSE 无法追加 fMP4 分片'))}
    const aborted=()=>{cleanup();try{if(sourceBuffer.updating)sourceBuffer.abort()}catch{/* detached */}reject(new DOMException('Aborted','AbortError'))}
    sourceBuffer.addEventListener('updateend',done,{once:true});sourceBuffer.addEventListener('error',failed,{once:true});signal.addEventListener('abort',aborted,{once:true})
    try{sourceBuffer.appendBuffer(data.slice().buffer as ArrayBuffer)}catch(caught){cleanup();reject(caught)}
  })
}

async function clearSourceBuffer(sourceBuffer:SourceBuffer,signal:AbortSignal):Promise<void>{
  if(!sourceBuffer.buffered.length)return
  const start=sourceBuffer.buffered.start(0)
  const end=sourceBuffer.buffered.end(sourceBuffer.buffered.length-1)
  if(end>start)await removeSourceBufferRange(sourceBuffer,start,end,signal)
}

async function keepMSEBufferBounded(sourceBuffer:SourceBuffer,element:HTMLVideoElement,signal:AbortSignal,seekChanged:()=>boolean):Promise<void>{
  if(sourceBuffer.buffered.length&&element.currentTime>70){
    const removeEnd=element.currentTime-60
    if(removeEnd>sourceBuffer.buffered.start(0))await removeSourceBufferRange(sourceBuffer,0,removeEnd,signal)
  }
  // A remux worker is intentionally limited to one short source window. Do
  // not ask the index for the following window until playback is close to the
  // end of the buffered one, otherwise a paused player would still make
  // FFmpeg walk through several windows and read most of a short S3 object.
  while(!seekChanged()&&sourceBuffer.buffered.length&&sourceBuffer.buffered.end(sourceBuffer.buffered.length-1)-element.currentTime>mseWindowRefillLeadSeconds){
    await abortableDelay(350,signal)
  }
}

function removeSourceBufferRange(sourceBuffer:SourceBuffer,start:number,end:number,signal:AbortSignal):Promise<void>{
  return new Promise((resolve,reject)=>{
    const cleanup=()=>{sourceBuffer.removeEventListener('updateend',done);sourceBuffer.removeEventListener('error',failed);signal.removeEventListener('abort',aborted)}
    const done=()=>{cleanup();resolve()};const failed=()=>{cleanup();reject(new Error('MSE 缓存清理失败'))};const aborted=()=>{cleanup();try{if(sourceBuffer.updating)sourceBuffer.abort()}catch{/* detached */}reject(new DOMException('Aborted','AbortError'))}
    sourceBuffer.addEventListener('updateend',done,{once:true});sourceBuffer.addEventListener('error',failed,{once:true});signal.addEventListener('abort',aborted,{once:true})
    try{sourceBuffer.remove(start,end)}catch(caught){cleanup();reject(caught)}
  })
}

function abortableDelay(milliseconds:number,signal:AbortSignal):Promise<void>{
  return new Promise((resolve,reject)=>{
    const timer=window.setTimeout(()=>{signal.removeEventListener('abort',abort);resolve()},milliseconds)
    const abort=()=>{window.clearTimeout(timer);reject(new DOMException('Aborted','AbortError'))}
    signal.addEventListener('abort',abort,{once:true})
  })
}

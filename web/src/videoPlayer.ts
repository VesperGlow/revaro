import type { VideoFMP4Metadata, VideoFMP4Response } from './types'

export const mseStreamBufferGoalSeconds=45
export const mseStallWatchdogSeconds=8
export const mseFreshRecoveryLimit=2

export function mseRecoveryAction(freshFailures:number):'fresh-mse'|'hls'{
  return freshFailures<mseFreshRecoveryLimit?'fresh-mse':'hls'
}

export function mseWatchdogExpired(waitingSince:number,lastBufferGrowth:number,now:number,playheadCovered:boolean):boolean{
  const limit=mseStallWatchdogSeconds*1000
  return !playheadCovered&&waitingSince>0&&now-waitingSince>=limit&&now-lastBufferGrowth>=limit
}

export interface MSEBufferedRange { start:number;end:number }

export function bufferedRangesAddedSeconds(before:MSEBufferedRange[],after:MSEBufferedRange[]):number{
  let added=0
  for(const current of after){
    let uncovered=Math.max(0,current.end-current.start)
    for(const previous of before){
      uncovered-=Math.max(0,Math.min(current.end,previous.end)-Math.max(current.start,previous.start))
    }
    added+=Math.max(0,uncovered)
  }
  return added
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
  for(let index=0;index<tracks.length;index+=1)tracks[index].mode=selected&&tracks[index]===selected?'showing':'disabled'
}

export interface SelectableSubtitle { default?:boolean;forced?:boolean }
export function initialSubtitleIndex(tracks:SelectableSubtitle[]):number{
  if(!tracks.length)return -1
  const preferred=tracks.findIndex(track=>track.default)
  if(preferred>=0)return preferred
  const forced=tracks.findIndex(track=>track.forced)
  return forced>=0?forced:0
}

export function authoritativeSeekTarget(current:number,saved:number,userSeeked:boolean):number{
  if(userSeeked&&Number.isFinite(current))return Math.max(0,current)
  return Number.isFinite(current)&&current>0?current:Math.max(0,Number.isFinite(saved)?saved:0)
}

export function mediaElementTimelineTime(elementTime:number,mode:VideoPlaybackMode,offset:number):number{
  if(!Number.isFinite(elementTime))return 0
  return Math.max(0,elementTime+(mode==='direct'?0:Math.max(0,Number.isFinite(offset)?offset:0)))
}

export function shouldSyncMediaClock(starting:boolean,paused:boolean):boolean{
  return !starting||!paused
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
    mode,offset,
    play:async()=>{await element.play()},
    pause:()=>element.pause(),
    seek:(globalTime:number)=>{
      const local=Math.max(0,globalTime-offset)
      if(mode==='direct'){element.currentTime=local;return true}
      const ranges=element.seekable
      for(let index=0;index<ranges.length;index+=1){
        if(local>=ranges.start(index)-.5&&local<=ranges.end(index)+.5){element.currentTime=local;return true}
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

function supportedMIME(contentType:string):string{
  if(!contentType)return ''
  if(MediaSource.isTypeSupported(contentType))return contentType
  return ''
}

export async function mseCompatibility(metadata:VideoFMP4Metadata):Promise<MSECompatibility>{
  const unavailable:MSECompatibility={
    videoSupported:false,audioSupported:false,aacAudioSupported:false,
    combinedCopySupported:false,combinedAACSupported:false,mode:'hls',mimeType:'',
    fallbackReason:'浏览器没有 MediaSource Extensions',
  }
  if(typeof MediaSource==='undefined')return unavailable
  const videoMIME=supportedMIME(metadata.video_mime_type)
  const audioSupported=!metadata.audio_codec||Boolean(metadata.audio_mime_type&&MediaSource.isTypeSupported(metadata.audio_mime_type))
  const aacAudioSupported=!metadata.audio_codec||Boolean(metadata.aac_audio_mime_type&&MediaSource.isTypeSupported(metadata.aac_audio_mime_type))
  const copyMIME=supportedMIME(metadata.mime_type)
  const aacMIME=supportedMIME(metadata.aac_mime_type)
  let videoSupported=Boolean(videoMIME)
  let powerEfficient:boolean|undefined
  if(videoSupported&&navigator.mediaCapabilities?.decodingInfo){
    try{
      const result=await navigator.mediaCapabilities.decodingInfo({type:'media-source',video:{
        contentType:videoMIME,width:Math.max(1,metadata.width),height:Math.max(1,metadata.height),
        bitrate:Math.max(1,metadata.bitrate),framerate:Math.max(1,metadata.frame_rate),
      }})
      videoSupported=result.supported;powerEfficient=result.powerEfficient
    }catch{/* isTypeSupported remains authoritative when MediaCapabilities is incomplete */}
  }
  const result:MSECompatibility={
    videoSupported,audioSupported,aacAudioSupported,combinedCopySupported:Boolean(copyMIME),
    combinedAACSupported:Boolean(aacMIME),powerEfficient,mode:'hls',mimeType:'',fallbackReason:'',
  }
  if(!videoSupported)result.fallbackReason=`浏览器不支持 ${metadata.video_codec.toUpperCase()} MSE/解码`
  else if(audioSupported&&copyMIME){result.mode='copy';result.mimeType=copyMIME}
  else if(aacAudioSupported&&aacMIME){
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

// attachFMP4Stream consumes one long-lived fetch body. Browser stream
// backpressure stops response reads once enough media is buffered; that stalls
// the server's HTTP write, FFmpeg stdout, the local Range source, and finally
// S3 reads. Abort is propagated through the same chain on seek/close.
export async function attachFMP4Stream(options:FMP4AttachOptions):Promise<FMP4Attachment>{
  const {element,response,autoplay,onFatal}=options
  const mediaSource=new MediaSource()
  const objectURL=URL.createObjectURL(mediaSource)
  const controller=new AbortController()
  let sourceBuffer:SourceBuffer|null=null
  let disposed=false
  let readySettled=false
  let waitingSince=0
  let lastBufferGrowth=Date.now()
  let watchdogTimer=0
  let watchdogFired=false
  let resolveReady:()=>void=()=>{}
  let rejectReady:(reason:Error)=>void=()=>{}
  const ready=new Promise<void>((resolve,reject)=>{resolveReady=resolve;rejectReady=reject})
  const target=Math.max(0,Math.min(response.duration,options.target-response.start))

  const log=(event:string,extra:Record<string,unknown>={})=>console.info('[revaro][mse]',event,{session:response.session_id,target,...extra})
  const cleanup=()=>{
    if(watchdogTimer)globalThis.clearInterval(watchdogTimer)
    element.removeEventListener('waiting',onWaiting);element.removeEventListener('stalled',onWaiting)
    element.removeEventListener('playing',onPlayable);element.removeEventListener('canplay',onPlayable);element.removeEventListener('timeupdate',onPlayable)
    try{if(sourceBuffer?.updating)sourceBuffer.abort()}catch{/* detached */}
    if(mediaSource.readyState==='open'&&sourceBuffer)try{mediaSource.removeSourceBuffer(sourceBuffer)}catch{/* detached */}
    URL.revokeObjectURL(objectURL)
  }
  const destroy=()=>{
    if(disposed)return
    disposed=true;controller.abort();cleanup()
    if(!readySettled){readySettled=true;rejectReady(new DOMException('Aborted','AbortError'))}
  }
  const fail=(reason:string)=>{
    if(disposed)return
    disposed=true;controller.abort();cleanup()
    const error=new Error(reason)
    if(!readySettled){readySettled=true;rejectReady(error)}else onFatal(reason)
  }
  const onWaiting=()=>{if(!waitingSince)waitingSince=Date.now()}
  const onPlayable=()=>{if(bufferContains(sourceBuffer?.buffered,element.currentTime,.1))waitingSince=0}
  const inspectWatchdog=()=>{
    if(disposed||watchdogFired||!sourceBuffer)return
    const now=Date.now(),covered=bufferContains(sourceBuffer.buffered,element.currentTime,.1)
    if(mseWatchdogExpired(waitingSince,lastBufferGrowth,now,covered)){
      watchdogFired=true
      const reason=`MSE watchdog: ${Math.round((now-waitingSince)/1000)}s 无可播放缓冲且流没有增长`
      console.warn('[revaro][mse]',reason,{buffered:formatTimeRanges(sourceBuffer.buffered)})
      fail(reason)
    }
  }

  element.src=objectURL;element.load()
  element.addEventListener('waiting',onWaiting);element.addEventListener('stalled',onWaiting)
  element.addEventListener('playing',onPlayable);element.addEventListener('canplay',onPlayable);element.addEventListener('timeupdate',onPlayable)
  mediaSource.addEventListener('sourceopen',()=>{
    if(disposed)return
    try{
      sourceBuffer=mediaSource.addSourceBuffer(options.mimeType);sourceBuffer.mode='segments'
      sourceBuffer.timestampOffset=Math.max(0,response.requested_start-response.start)
      sourceBuffer.addEventListener('error',()=>fail('MSE SourceBuffer 解码 fMP4 流失败'))
      try{mediaSource.duration=Math.max(.1,response.duration)}catch{/* duration may already be known */}
      watchdogTimer=globalThis.setInterval(inspectWatchdog,1000) as unknown as number
      log('stdout HTTP stream attached',{url:response.stream_url,mime_type:options.mimeType,timestamp_offset:sourceBuffer.timestampOffset})
      void pumpFMP4Stream(sourceBuffer,mediaSource,options,controller.signal,target,()=>disposed,()=>{
        if(!readySettled){readySettled=true;resolveReady()}
      },()=>{lastBufferGrowth=Date.now()}).catch(caught=>{
        if(caught instanceof DOMException&&caught.name==='AbortError'&&disposed)return
        fail(caught instanceof Error?caught.message:'MSE fMP4 流读取失败')
      })
    }catch(caught){fail(caught instanceof Error?caught.message:'无法创建 MSE SourceBuffer')}
  },{once:true})

  await ready
  if(autoplay)void element.play().catch(()=>{})
  return {destroy,seek:()=>{destroy();return false}}
}

async function pumpFMP4Stream(
  sourceBuffer:SourceBuffer,
  mediaSource:MediaSource,
  options:FMP4AttachOptions,
  signal:AbortSignal,
  target:number,
  isDisposed:()=>boolean,
  markReady:()=>void,
  markGrowth:()=>void,
):Promise<void>{
  const response=await fetch(options.response.stream_url,{credentials:'same-origin',cache:'no-store',signal})
  if(!response.ok)throw new Error(`fMP4 流请求失败 (${response.status})`)
  if(!response.body)throw new Error('浏览器未提供 fMP4 ReadableStream')
  const reader=response.body.getReader()
  let ready=false
  try{
    while(!isDisposed()){
      await applyMSEBackpressure(sourceBuffer,options.element,signal)
      const {done,value}=await reader.read()
      if(done)break
      if(!value?.byteLength)continue
      const before=snapshotTimeRanges(sourceBuffer.buffered)
      await appendSourceBuffer(sourceBuffer,value,signal)
      const after=snapshotTimeRanges(sourceBuffer.buffered)
      const growth=bufferedRangesAddedSeconds(before,after)
      if(growth>.001){
        markGrowth();options.onFragment?.()
        if(!ready&&bufferContains(sourceBuffer.buffered,target,.5)){
          ready=true;options.element.currentTime=target;markReady()
        }
      }
    }
  }finally{
    try{reader.releaseLock()}catch{/* already released */}
  }
  if(!ready){
    if(sourceBuffer.buffered.length){options.element.currentTime=Math.min(target,sourceBuffer.buffered.end(sourceBuffer.buffered.length-1));markReady()}
    else throw new Error('FFmpeg fMP4 流在产生可播放缓冲前结束')
  }
  if(mediaSource.readyState==='open'&&!sourceBuffer.updating)try{mediaSource.endOfStream()}catch{/* a seek may detach it */}
}

async function applyMSEBackpressure(sourceBuffer:SourceBuffer,element:HTMLVideoElement,signal:AbortSignal):Promise<void>{
  if(sourceBuffer.buffered.length&&element.currentTime>70){
    const removeEnd=element.currentTime-60
    if(removeEnd>sourceBuffer.buffered.start(0))await removeSourceBufferRange(sourceBuffer,sourceBuffer.buffered.start(0),removeEnd,signal)
  }
  while(sourceBuffer.buffered.length&&sourceBuffer.buffered.end(sourceBuffer.buffered.length-1)-element.currentTime>=mseStreamBufferGoalSeconds){
    await abortableDelay(250,signal)
  }
}

function bufferContains(ranges:TimeRanges|undefined,time:number,tolerance=.05):boolean{
  if(!ranges)return false
  for(let index=0;index<ranges.length;index+=1){
    if(time>=ranges.start(index)-tolerance&&time<=ranges.end(index)+tolerance)return true
  }
  return false
}

function formatTimeRanges(ranges:TimeRanges|undefined):string{
  return `[${snapshotTimeRanges(ranges).map(range=>`${range.start.toFixed(3)}-${range.end.toFixed(3)}`).join(', ')}]`
}

function snapshotTimeRanges(ranges:TimeRanges|undefined):MSEBufferedRange[]{
  if(!ranges)return []
  const values:MSEBufferedRange[]=[]
  for(let index=0;index<ranges.length;index+=1)values.push({start:ranges.start(index),end:ranges.end(index)})
  return values
}

function appendSourceBuffer(sourceBuffer:SourceBuffer,data:Uint8Array,signal:AbortSignal):Promise<void>{
  return new Promise((resolve,reject)=>{
    if(signal.aborted){reject(new DOMException('Aborted','AbortError'));return}
    const cleanup=()=>{sourceBuffer.removeEventListener('updateend',done);sourceBuffer.removeEventListener('error',failed);signal.removeEventListener('abort',aborted)}
    const done=()=>{cleanup();resolve()}
    const failed=()=>{cleanup();reject(new Error('MSE 无法追加 fMP4 流数据'))}
    const aborted=()=>{cleanup();try{if(sourceBuffer.updating)sourceBuffer.abort()}catch{/* detached */}reject(new DOMException('Aborted','AbortError'))}
    sourceBuffer.addEventListener('updateend',done,{once:true});sourceBuffer.addEventListener('error',failed,{once:true});signal.addEventListener('abort',aborted,{once:true})
    try{sourceBuffer.appendBuffer(data.slice().buffer as ArrayBuffer)}catch(caught){cleanup();reject(caught)}
  })
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
    const timer=globalThis.setTimeout(()=>{signal.removeEventListener('abort',abort);resolve()},milliseconds)
    const abort=()=>{globalThis.clearTimeout(timer);reject(new DOMException('Aborted','AbortError'))}
    signal.addEventListener('abort',abort,{once:true})
  })
}

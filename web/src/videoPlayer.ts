import type { VideoFMP4Response } from './types'

export type VideoPlaybackMode='direct'|'mse'|'hls'

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
      return false
    },
    setVolume:(value:number,muted:boolean)=>{element.volume=value;element.muted=muted},
    setSubtitle:(track:TextTrack|null)=>{
      for(let index=0;index<element.textTracks.length;index+=1){
        element.textTracks[index].mode=track&&element.textTracks[index]===track?'showing':'disabled'
      }
    },
    requestFullscreen:async(container:HTMLElement)=>{
      if(document.fullscreenElement)await document.exitFullscreen()
      else try{await container.requestFullscreen({navigationUI:'hide'})}
      catch{(element as HTMLVideoElement&{webkitEnterFullscreen?:()=>void}).webkitEnterFullscreen?.()}
    },
    destroy:cleanup,
  }
}

export async function mseCompatibility(response:VideoFMP4Response):Promise<string>{
  if(typeof MediaSource==='undefined')return '浏览器没有 MediaSource Extensions'
  if(!MediaSource.isTypeSupported(response.mime_type))return `MSE 不支持 ${response.mime_type}`
  const capabilities=navigator.mediaCapabilities
  if(!capabilities?.decodingInfo)return ''
  try{
    const result=await capabilities.decodingInfo({
      type:'media-source',
      video:{
        contentType:response.video_mime_type,
        width:Math.max(1,response.width),height:Math.max(1,response.height),
        bitrate:Math.max(1,response.bitrate),framerate:Math.max(1,response.frame_rate),
      },
    })
    if(!result.supported)return `浏览器解码能力不支持 ${response.video_codec}`
    const mobile=/Android|Mobile|HarmonyOS|HuaweiBrowser/i.test(navigator.userAgent)
    if(mobile&&result.powerEfficient===false)return `移动端没有 ${response.video_codec} 硬件高效解码`
  }catch{/* 浏览器实现不完整时仍以 MediaSource.isTypeSupported 为准 */}
  return ''
}

interface FMP4AttachOptions {
  element:HTMLVideoElement
  response:VideoFMP4Response
  target:number
  autoplay:boolean
  onFatal:(reason:string)=>void
}

interface FMP4Attachment { destroy:()=>void }

export async function attachFMP4Stream(options:FMP4AttachOptions):Promise<FMP4Attachment>{
  const {element,response,autoplay,onFatal}=options
  const mediaSource=new MediaSource()
  const objectURL=URL.createObjectURL(mediaSource)
  const abortController=new AbortController()
  let sourceBuffer:SourceBuffer|null=null
  let disposed=false
  let readySettled=false
  let resolveReady:()=>void=()=>{}
  let rejectReady:(error:Error)=>void=()=>{}
  const ready=new Promise<void>((resolve,reject)=>{resolveReady=resolve;rejectReady=reject})

  const fail=(reason:string)=>{
    if(disposed)return
    if(!readySettled){readySettled=true;rejectReady(new Error(reason))}
    else onFatal(reason)
  }
  const destroy=()=>{
    if(disposed)return
    disposed=true;abortController.abort()
    try{if(sourceBuffer?.updating)sourceBuffer.abort()}catch{/* already detached */}
    try{if(sourceBuffer&&mediaSource.readyState==='open')mediaSource.removeSourceBuffer(sourceBuffer)}catch{/* already detached */}
    if(element.src===objectURL){element.pause();element.removeAttribute('src');element.load()}
    URL.revokeObjectURL(objectURL)
  }

  element.src=objectURL;element.load()
  mediaSource.addEventListener('sourceopen',()=>{
    if(disposed)return
    try{
      sourceBuffer=mediaSource.addSourceBuffer(response.mime_type)
      sourceBuffer.mode='segments'
      sourceBuffer.addEventListener('error',()=>fail('MSE SourceBuffer 解码 fMP4 失败'))
      void pumpFMP4(sourceBuffer,mediaSource,options,abortController.signal,()=>disposed).then(()=>{
        if(!disposed&&mediaSource.readyState==='open'&&!sourceBuffer?.updating)try{mediaSource.endOfStream()}catch{/* final duration remains usable */}
      }).catch(caught=>fail(caught instanceof Error?caught.message:'MSE fMP4 流读取失败'))
    }catch(caught){fail(caught instanceof Error?caught.message:'无法创建 MSE SourceBuffer')}
  },{once:true})
  mediaSource.addEventListener('sourceended',()=>{if(!readySettled)fail('MSE 在首个片段前结束')},{once:true})

  const markReady=()=>{
    if(readySettled)return
    readySettled=true;resolveReady()
  }
  ;(options as FMP4AttachOptions&{markReady?:()=>void}).markReady=markReady
  await ready
  if(autoplay)void element.play().catch(()=>{})
  return {destroy}
}

async function pumpFMP4(
  sourceBuffer:SourceBuffer,
  mediaSource:MediaSource,
  options:FMP4AttachOptions&{markReady?:()=>void},
  signal:AbortSignal,
  isDisposed:()=>boolean,
):Promise<void>{
  const response=await fetch(options.response.stream_url,{credentials:'same-origin',signal})
  if(!response.ok||!response.body)throw new Error(`fMP4 流请求失败 (${response.status})`)
  const reader=response.body.getReader()
  let pending:Uint8Array<ArrayBufferLike>=new Uint8Array(0)
  let initAppended=false
  let mediaAppended=false
  while(!isDisposed()){
    const result=await reader.read()
    if(result.value?.length)pending=concatBytes(pending,result.value)
    for(;;){
      const appendable=appendableMP4Prefix(pending,initAppended)
      if(!appendable.length)break
      const chunk=pending.slice(0,appendable.length)
      pending=pending.slice(appendable.length)
      await appendSourceBuffer(sourceBuffer,chunk,signal)
      if(appendable.hasInit)initAppended=true
      if(appendable.hasMedia){
        mediaAppended=true
        if(mediaSource.readyState==='open'&&(!Number.isFinite(mediaSource.duration)||mediaSource.duration<=0)){
          try{mediaSource.duration=Math.max(.1,options.response.duration-options.response.start)}catch{/* duration will come from media */}
        }
        if(!options.markReady)continue
        const local=Math.max(0,options.target-options.response.start)
        options.element.currentTime=local
        options.markReady();options.markReady=undefined
      }
      await keepMSEBufferBounded(sourceBuffer,options.element,signal)
    }
    if(result.done)break
  }
  if(!mediaAppended)throw new Error('fMP4 没有产生可播放媒体片段')
}

function concatBytes(left:Uint8Array,right:Uint8Array):Uint8Array{
  if(!left.length)return right.slice()
  const merged=new Uint8Array(left.length+right.length);merged.set(left);merged.set(right,left.length);return merged
}

function appendableMP4Prefix(data:Uint8Array,initAppended:boolean):{length:number;hasInit:boolean;hasMedia:boolean}{
  let offset=0
  let initEnd=0
  let mediaEnd=0
  let hasInit=false
  while(offset+8<=data.length){
    const view=new DataView(data.buffer,data.byteOffset+offset,Math.min(16,data.length-offset))
    let size=view.getUint32(0)
    let headerSize=8
    if(size===1){
      if(offset+16>data.length)break
      const high=view.getUint32(8);const low=view.getUint32(12)
      size=high*0x100000000+low;headerSize=16
    }
    if(size===0||size<headerSize||offset+size>data.length)break
    const type=String.fromCharCode(data[offset+4],data[offset+5],data[offset+6],data[offset+7])
    offset+=size
    if(type==='moov'){hasInit=true;initEnd=offset}
    if(type==='mdat'&&(initAppended||hasInit)){mediaEnd=offset}
  }
  if(!initAppended&&mediaEnd)return {length:mediaEnd,hasInit:true,hasMedia:true}
  if(!initAppended&&initEnd)return {length:initEnd,hasInit:true,hasMedia:false}
  if(initAppended&&mediaEnd)return {length:mediaEnd,hasInit:false,hasMedia:true}
  return {length:0,hasInit:false,hasMedia:false}
}

function appendSourceBuffer(sourceBuffer:SourceBuffer,data:Uint8Array,signal:AbortSignal):Promise<void>{
  return new Promise((resolve,reject)=>{
    if(signal.aborted){reject(new DOMException('Aborted','AbortError'));return}
    const cleanup=()=>{sourceBuffer.removeEventListener('updateend',done);sourceBuffer.removeEventListener('error',failed);signal.removeEventListener('abort',aborted)}
    const done=()=>{cleanup();resolve()}
    const failed=()=>{cleanup();reject(new Error('MSE 无法追加 fMP4 片段'))}
    const aborted=()=>{cleanup();reject(new DOMException('Aborted','AbortError'))}
    sourceBuffer.addEventListener('updateend',done,{once:true});sourceBuffer.addEventListener('error',failed,{once:true});signal.addEventListener('abort',aborted,{once:true})
    try{sourceBuffer.appendBuffer(data.slice().buffer as ArrayBuffer)}catch(caught){cleanup();reject(caught)}
  })
}

async function keepMSEBufferBounded(sourceBuffer:SourceBuffer,element:HTMLVideoElement,signal:AbortSignal):Promise<void>{
  if(sourceBuffer.buffered.length&&element.currentTime>70){
    const removeEnd=element.currentTime-60
    if(removeEnd>sourceBuffer.buffered.start(0))await removeSourceBufferRange(sourceBuffer,0,removeEnd,signal)
  }
  while(sourceBuffer.buffered.length&&sourceBuffer.buffered.end(sourceBuffer.buffered.length-1)-element.currentTime>120){
    await abortableDelay(350,signal)
  }
}

function removeSourceBufferRange(sourceBuffer:SourceBuffer,start:number,end:number,signal:AbortSignal):Promise<void>{
  return new Promise((resolve,reject)=>{
    const cleanup=()=>{sourceBuffer.removeEventListener('updateend',done);sourceBuffer.removeEventListener('error',failed);signal.removeEventListener('abort',aborted)}
    const done=()=>{cleanup();resolve()};const failed=()=>{cleanup();reject(new Error('MSE 缓存清理失败'))};const aborted=()=>{cleanup();reject(new DOMException('Aborted','AbortError'))}
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

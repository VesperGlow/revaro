export type VideoPlaybackMode='direct'

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

export interface ContainedVideoInsets { bottom:number;horizontal:number }

/** Insets of the image produced by object-fit: contain inside its element. */
export function containedVideoInsets(containerWidth:number,containerHeight:number,videoWidth:number,videoHeight:number):ContainedVideoInsets{
  if(containerWidth<=0||containerHeight<=0||videoWidth<=0||videoHeight<=0)return {bottom:0,horizontal:0}
  const containerRatio=containerWidth/containerHeight
  const videoRatio=videoWidth/videoHeight
  if(videoRatio>containerRatio)return {bottom:(containerHeight-containerWidth/videoRatio)/2,horizontal:0}
  return {bottom:0,horizontal:(containerWidth-containerHeight*videoRatio)/2}
}

type MutableTextTrack=Pick<TextTrack,'mode'>

export function setExclusiveSubtitleTrack<T extends MutableTextTrack>(tracks:ArrayLike<T>,selected:T|null):void{
  // Revaro renders active cues in its own overlay. Keep the selected track
  // hidden so the browser parses it and emits cuechange without also drawing
  // a second, platform-styled caption (often a black/white background box).
  for(let index=0;index<tracks.length;index+=1)tracks[index].mode=selected&&tracks[index]===selected?'hidden':'disabled'
}

/** Avoid the global `secondary` utility, which is the white rounded button skin. */
export function subtitleLineClass(index:number):string{
  return index>0?'video-subtitle-secondary-line':''
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

export function mediaElementTimelineTime(elementTime:number,_mode:VideoPlaybackMode,_offset:number):number{
  if(!Number.isFinite(elementTime))return 0
  return Math.max(0,elementTime)
}

export function shouldSyncMediaClock(starting:boolean,paused:boolean):boolean{
  return !starting||!paused
}

export function shouldContinueMediaClock(elementPresent:boolean):boolean{
  return elementPresent
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
):UnifiedVideoPlayer {
  return {
    mode,offset,
    play:async()=>{await element.play()},
    pause:()=>element.pause(),
    seek:(globalTime:number)=>{
      const local=Math.max(0,globalTime-offset)
      element.currentTime=local;return true
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

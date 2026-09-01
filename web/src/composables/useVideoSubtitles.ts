import { computed, nextTick, ref, type Ref } from 'vue'
import type { VideoSubtitleTrack } from '../types'
import { containedVideoInsets, setExclusiveSubtitleTrack, subtitleTrackKey, subtitleURLForPlayback, type UnifiedVideoPlayer, type VideoPlaybackMode } from '../videoPlayer'

export function useVideoSubtitles(options:{video:Ref<HTMLVideoElement|null>;directMode:Ref<boolean>;mseMode:Ref<boolean>;prepareMode:Ref<'mse'|'hls'>;streamOffset:Ref<number>;getPlayer:()=>UnifiedVideoPlayer|null}){
  const subtitleElement=ref<HTMLTrackElement|null>(null)
  const subtitles=ref<VideoSubtitleTrack[]>([])
  const activeSubtitle=ref(-1)
  const activeSubtitleLines=ref<string[]>([])
  const subtitlePlacement=ref<'top'|'middle'|'bottom'>('bottom')
  const subtitleImageBottom=ref(0)
  const subtitleImageInset=ref(0)
  let cueTrack:TextTrack|null=null
  const selectedSubtitle=computed(()=>activeSubtitle.value>=0?subtitles.value[activeSubtitle.value]:undefined)
  const subtitlePlaybackMode=computed<VideoPlaybackMode>(()=>options.directMode.value?'direct':options.mseMode.value||options.prepareMode.value==='mse'?'mse':'hls')
  const selectedSubtitleURL=computed(()=>{const track=selectedSubtitle.value;return track?subtitleURLForPlayback(track.url,subtitlePlaybackMode.value,options.streamOffset.value):''})
  const selectedSubtitleKey=computed(()=>selectedSubtitle.value?subtitleTrackKey(selectedSubtitle.value.id,subtitlePlaybackMode.value,options.streamOffset.value):'')
  const subtitleStyle=computed(()=>({'--subtitle-image-bottom':`${subtitleImageBottom.value}px`,'--subtitle-image-inset':`${subtitleImageInset.value}px`} as Record<string,string>))
  function updateSubtitleBounds(){const el=options.video.value;if(!el)return;const bounds=containedVideoInsets(el.clientWidth,el.clientHeight,el.videoWidth,el.videoHeight);subtitleImageBottom.value=bounds.bottom;subtitleImageInset.value=bounds.horizontal}
  function subtitleCueLines(text:string):string[]{const plain=text.replace(/<\/?(?:b|i|u|ruby|rt)(?:\s[^>]*)?>/gi,'').replace(/<v(?:\s[^>]*)?>/gi,'').replace(/<c(?:\.[^\s>]*)*>/gi,'').replace(/<\/[vc]>/gi,'');const decoded=new DOMParser().parseFromString(plain,'text/html').body.textContent||'';return decoded.split(/\r?\n/).map(line=>line.trim()).filter(Boolean)}
  function updateActiveSubtitle(){const cues=cueTrack?.activeCues;const cue=cues?.[0] as VTTCue|undefined;const line=cue&&typeof cue.line==='number'&&!cue.snapToLines?cue.line:100;subtitlePlacement.value=line<=25?'top':line<75?'middle':'bottom';activeSubtitleLines.value=cues?Array.from(cues).flatMap(cue=>subtitleCueLines((cue as VTTCue).text)):[]}
  function bindCueTrack(track:TextTrack|null){if(cueTrack===track){updateActiveSubtitle();return}cueTrack?.removeEventListener('cuechange',updateActiveSubtitle);cueTrack=track;cueTrack?.addEventListener('cuechange',updateActiveSubtitle);updateActiveSubtitle()}
  function applySubtitle(){const tracks=options.video.value?.textTracks;if(!tracks)return;const current=activeSubtitle.value>=0?subtitleElement.value?.track:null;const player=options.getPlayer();if(player)player.setSubtitle(current||null);else setExclusiveSubtitleTrack(tracks,current||null);bindCueTrack(current||null)}
  function disableSubtitleTracks(){const tracks=options.video.value?.textTracks;if(tracks)setExclusiveSubtitleTrack(tracks,null);bindCueTrack(null)}
  async function chooseSubtitle(event:Event){activeSubtitle.value=Number((event.target as HTMLSelectElement).value);if(activeSubtitle.value<0){disableSubtitleTracks();console.info('[revaro] subtitle selected: off');return}await nextTick();applySubtitle();console.info('[revaro] subtitle selected:',selectedSubtitle.value?.id||'none')}
  function onSubtitleLoad(event:Event){if(event.currentTarget!==subtitleElement.value)return;const track=(event.currentTarget as HTMLTrackElement).track;applySubtitle();track.mode='hidden';updateActiveSubtitle();console.info('[revaro] subtitle track loaded');console.info('[revaro] subtitle cues:',track.cues?.length??0);console.info('[revaro] subtitle mode:',track.mode)}
  function onSubtitleError(event:Event){const element=event.currentTarget as HTMLTrackElement,url=selectedSubtitleURL.value;console.error('[revaro] subtitle error','url:',url,'track readyState:',element.readyState);if(!url)return;void fetch(url,{credentials:'same-origin',cache:'no-store'}).then(async response=>{console.error('[revaro] subtitle HTTP status:',response.status,'content-type:',response.headers.get('content-type')||'missing','track readyState:',element.readyState);await response.body?.cancel()}).catch(caught=>console.error('[revaro] subtitle diagnostic request failed:',caught))}
  return {subtitleElement,subtitles,activeSubtitle,activeSubtitleLines,subtitlePlacement,selectedSubtitle,selectedSubtitleURL,selectedSubtitleKey,subtitleStyle,updateSubtitleBounds,applySubtitle,disableSubtitleTracks,chooseSubtitle,onSubtitleLoad,onSubtitleError}
}

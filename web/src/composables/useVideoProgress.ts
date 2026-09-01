import { type Ref } from 'vue'
import { api } from '../api'

export function useVideoProgress(options:{itemId:string;video:Ref<HTMLVideoElement|null>;currentTime:Ref<number>;duration:Ref<number>;directMode:Ref<boolean>}){
 const positionKey=`revaro-video-position:${options.itemId}`
 let progressLoaded=false,restoredPosition=false,serverPosition=0,userSeeked=false
 function savedPosition(){if(serverPosition>0)return serverPosition;const value=Number(localStorage.getItem(positionKey)||0);return Number.isFinite(value)&&value>0?value:0}
 async function loadProgress(){try{const value=await api<{position:number}>(`/api/files/${options.itemId}/media/progress`);serverPosition=Number.isFinite(value.position)?value.position:0}catch{/* 本机进度仍可兜底 */}progressLoaded=true;restoreDirectPosition()}
 function restoreDirectPosition(){const el=options.video.value;if(!progressLoaded||restoredPosition||!options.directMode.value||!el||!options.duration.value)return;restoredPosition=true;const saved=savedPosition();if(saved>0&&saved<options.duration.value-5){el.currentTime=saved;options.currentTime.value=saved}}
 function persistProgress(remote=false){const position=Math.max(0,options.currentTime.value);if(position<=0&&!userSeeked)return;localStorage.setItem(positionKey,String(Math.floor(position)));if(!remote)return;serverPosition=position;void api(`/api/files/${options.itemId}/media/progress`,{method:'PUT',body:JSON.stringify({position,duration:options.duration.value})}).catch(()=>{})}
 function markUserSeeked(){userSeeked=true}
 function didUserSeek(){return userSeeked}
 return {savedPosition,loadProgress,restoreDirectPosition,persistProgress,markUserSeeked,didUserSeek}
}

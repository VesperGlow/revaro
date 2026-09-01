import { api } from './api'

export function formatVideoTime(seconds:number){if(!Number.isFinite(seconds)||seconds<0)return '0:00';const value=Math.floor(seconds),hours=Math.floor(value/3600),minutes=Math.floor(value%3600/60),secs=value%60;return hours?`${hours}:${String(minutes).padStart(2,'0')}:${String(secs).padStart(2,'0')}`:`${minutes}:${String(secs).padStart(2,'0')}`}
export async function releaseFMP4Session(id:string,discard=false,reason=''){if(!id)return;const query=discard?`?discard=1&reason=${encodeURIComponent(reason)}`:'';try{await api(`/api/video/fmp4/${id}${query}`,{method:'DELETE'})}catch{/* 服务端 TTL 仍会兜底 */}}
export function releaseHLSSession(id:string){if(id)void fetch(`/api/video/hls/${id}`,{method:'DELETE',credentials:'same-origin',keepalive:true})}

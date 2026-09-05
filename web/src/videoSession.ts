import { api } from './api'

export async function releaseFMP4Session(id:string,discard=false,reason=''){if(!id)return;const query=discard?`?discard=1&reason=${encodeURIComponent(reason)}`:'';try{await api(`/api/video/fmp4/${id}${query}`,{method:'DELETE'})}catch{/* 服务端 TTL 仍会兜底 */}}
export function releaseHLSSession(id:string){if(id)void fetch(`/api/video/hls/${id}`,{method:'DELETE',credentials:'same-origin',keepalive:true})}

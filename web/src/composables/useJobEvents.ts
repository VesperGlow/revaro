import { onBeforeUnmount } from 'vue'

export function useJobEvents(onChange:()=>void){
  let source:EventSource|null=null
  let fallback=0
  let stopped=false
  const scheduleFallback=()=>{window.clearInterval(fallback);fallback=window.setInterval(()=>{if(!source||source.readyState===EventSource.CLOSED)onChange()},30_000)}
  const connect=()=>{
    if(stopped||source)return
    source=new EventSource('/api/events')
    source.addEventListener('jobs',onChange)
    source.onerror=()=>{source?.close();source=null;scheduleFallback()}
    source.onopen=()=>window.clearInterval(fallback)
  }
  const stop=()=>{stopped=true;source?.close();source=null;window.clearInterval(fallback)}
  onBeforeUnmount(stop)
  return {connect,stop}
}

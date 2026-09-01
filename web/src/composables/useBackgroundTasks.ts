import { ref, type Ref } from 'vue'
import { api } from '../api'
import type { BackgroundTask, UploadTask } from '../types'
import { useJobEvents } from './useJobEvents'

type LocalSession={cancelled:boolean}
export function useBackgroundTasks(deps:{user:Ref<string|null>;tasks:UploadTask[];localUploads:Map<string,LocalSession>;currentId:Ref<string>;notify:(text:string,kind?:'error'|'success')=>void;openFolder:(id:string)=>Promise<void>;cancelUpload:(task:UploadTask)=>Promise<void>;retryUpload:(task:UploadTask)=>Promise<void>}){
 const {user,tasks,localUploads,currentId,notify,openFolder,cancelUpload,retryUpload:retry}=deps
 const backgroundTasks=ref<BackgroundTask[]>([])
 let jobRefreshRunning=false
 let jobRefreshPending=false
 async function refreshJobsFromEvent(){
   if(!user.value)return
   if(jobRefreshRunning){jobRefreshPending=true;return}
   jobRefreshRunning=true
   try{await refreshBackgroundTasks()}
   finally{jobRefreshRunning=false;if(jobRefreshPending){jobRefreshPending=false;void refreshJobsFromEvent()}}
 }
 async function refreshBackgroundTasks(){
   try{
     const before=new Map(backgroundTasks.value.map(task=>[task.id,task.status]))
     const data=await api<{items:BackgroundTask[]}>('/api/tasks')
     backgroundTasks.value=data.items
     let refresh=false
     for(const task of data.items){const old=before.get(task.id);if(old&&!['completed','failed','cancelled'].includes(old)&&['completed','failed','cancelled'].includes(task.status)){if(task.status==='completed'){notify(`「${task.name}」任务完成`,'success');if(['archive_extract','audio_merge','bt','url_download'].includes(task.type))refresh=true}else if(task.status==='failed')notify(task.error||`「${task.name}」任务失败`)}}
     if(refresh)void openFolder(currentId.value)
   }catch{/* SSE 重连后会再次同步 */}
 }
 async function cancelBackgroundTask(task:BackgroundTask){
   if(task.source_type==='upload'){const local=tasks.find(item=>item.uploadId===task.source_id);if(local){await cancelUpload(local);return}}
   if(task.source_type==='audio_merge'){const session=localUploads.get(task.source_id||task.id);if(session)session.cancelled=true}
   try{await api(`/api/tasks/${task.id}/cancel`,{method:'POST'});await refreshBackgroundTasks()}catch(e){notify((e as Error).message)}
 }
 async function retryBackgroundTask(task:BackgroundTask){
   if(task.source_type==='upload'){const local=tasks.find(item=>item.uploadId===task.source_id);if(local){await retry(local);return}}
   try{await api(`/api/tasks/${task.id}/retry`,{method:'POST'});await refreshBackgroundTasks()}catch(e){notify((e as Error).message)}
 }
 const jobEvents=useJobEvents(()=>void refreshJobsFromEvent())
 return {backgroundTasks,refreshJobsFromEvent,refreshBackgroundTasks,cancelBackgroundTask,retryBackgroundTask,jobEvents}
}


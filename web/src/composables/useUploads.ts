import { type Ref } from 'vue'
import { api } from '../api'
import type { ApiError, DriveFile } from '../api'
import type { UploadTask } from '../types'

const FILE_CONCURRENCY=3, MULTIPART_CONCURRENCY=4, PART_URL_BATCH=100, UPLOAD_RETRIES=5
const UPLOAD_RESUME_KEY='revaro.uploads.v1'
export function useUploads(deps:{tasks:UploadTask[];currentId:Ref<string>;dragActive:Ref<boolean>;trashMode:Ref<boolean>;fileInput:Ref<HTMLInputElement|null>;folderInput:Ref<HTMLInputElement|null>;notify:(text:string,kind?:'error'|'success')=>void;openFolder:(id:string)=>Promise<void>}){
 const {tasks,currentId,dragActive,trashMode,fileInput,folderInput,notify,openFolder}=deps
 let activeUploads=0,uploadRefreshTimer=0
 function chooseFiles(){fileInput.value?.click()}
 function chooseFolder(){folderInput.value?.click()}
 interface SavedUpload { uploadId:string; parentId:string; name:string; size:number; lastModified:number }
 function savedUploads():SavedUpload[]{try{return JSON.parse(localStorage.getItem(UPLOAD_RESUME_KEY)||'[]') as SavedUpload[]}catch{return []}}
 function saveUpload(task:UploadTask){if(!task.uploadId)return;const all=savedUploads().filter(x=>x.uploadId!==task.uploadId);all.push({uploadId:task.uploadId,parentId:task.parentId,name:task.file.name,size:task.file.size,lastModified:task.file.lastModified});localStorage.setItem(UPLOAD_RESUME_KEY,JSON.stringify(all))}
 function forgetUpload(id?:string){if(id)localStorage.setItem(UPLOAD_RESUME_KEY,JSON.stringify(savedUploads().filter(x=>x.uploadId!==id)))}
 function queueFiles(files:File[],parentId:string,relativePaths?:Map<File,string>){
   const saved=savedUploads()
   for(const file of files){const resume=saved.find(x=>x.parentId===parentId&&x.name===file.name&&x.size===file.size&&x.lastModified===file.lastModified);tasks.push({id:crypto.randomUUID(),file,parentId,relativePath:relativePaths?.get(file),progress:0,status:'queued',error:'',cancelled:false,uploadId:resume?.uploadId,requests:[]})}
 }
 function acceptFiles(list:FileList|File[]){queueFiles(Array.from(list),currentId.value);pumpQueue()}
 async function ensureUploadDirectory(parentId:string,name:string){
   try{return await api<DriveFile>('/api/directories',{method:'POST',body:JSON.stringify({parent_id:parentId,name})})}
   catch(e){
     if((e as ApiError).status!==409)throw e
     const data=await api<{items:DriveFile[]}>(`/api/files/${parentId}/children`)
     const existing=data.items.find(item=>item.name===name)
     if(existing?.kind==='directory')return existing
     throw new Error(`“${name}”与已有文件重名，无法创建上传目录`,{cause:e})
   }
 }
 async function acceptFolder(list:FileList){
   const files=Array.from(list)
   if(!files.length)return
   const destination=currentId.value
   const relativePaths=new Map<File,string>()
   const fileDirectories=new Map<File,string>()
   const directoryPaths=new Set<string>()
   for(const file of files){
     const raw=(file.webkitRelativePath||file.name).replaceAll('\\','/')
     const parts=raw.split('/').filter(Boolean)
     if(parts.some(part=>part==='.'||part==='..')){notify('文件夹中包含无效路径');return}
     const directory=parts.slice(0,-1).join('/')
     relativePaths.set(file,raw);fileDirectories.set(file,directory)
     for(let depth=1;depth<parts.length;depth++)directoryPaths.add(parts.slice(0,depth).join('/'))
   }
   const folderIds=new Map<string,string>([['',destination]])
   try{
     for(const path of [...directoryPaths].sort((a,b)=>a.split('/').length-b.split('/').length)){
       const split=path.lastIndexOf('/')
       const parentPath=split<0?'':path.slice(0,split)
       const name=split<0?path:path.slice(split+1)
       const parentId=folderIds.get(parentPath)
       if(!parentId)throw new Error(`无法解析目录“${path}”`)
       const folder=await ensureUploadDirectory(parentId,name)
       folderIds.set(path,folder.id)
     }
     for(const file of files){
       const parentId=folderIds.get(fileDirectories.get(file)||'')
       if(!parentId)throw new Error(`无法解析“${relativePaths.get(file)||file.name}”的上传位置`)
       queueFiles([file],parentId,relativePaths)
     }
     await openFolder(currentId.value)
     notify(`已保留目录结构，开始上传 ${files.length} 个文件`,'success')
     pumpQueue()
   }catch(e){notify((e as Error).message)}
 }
 function onDrop(event:DragEvent){dragActive.value=false;if(!trashMode.value&&event.dataTransfer?.files.length)acceptFiles(event.dataTransfer.files)}
 function pumpQueue(){while(activeUploads<FILE_CONCURRENCY){const task=tasks.find(t=>t.status==='queued');if(!task)return;activeUploads++;runUpload(task).finally(()=>{activeUploads--;pumpQueue()})}}
 interface CreatedUpload { upload_id:string; mode:'single'|'multipart'; url?:string; part_size:number; part_count:number; status?:string; parts?:CompletedPart[] }
 interface CompletedPart { part_number:number; etag:string }
 
 async function runUpload(task:UploadTask){
   task.status='uploading';task.error='';task.cancelled=false;task.progress=0
   try{
     let created:CreatedUpload
     if(task.uploadId){
       try{created=await api<CreatedUpload>(`/api/uploads/${task.uploadId}`)}catch{forgetUpload(task.uploadId);task.uploadId=undefined;created=await api<CreatedUpload>('/api/uploads',{method:'POST',body:JSON.stringify({parent_id:task.parentId,name:task.file.name,size:task.file.size,mime_type:task.file.type||'application/octet-stream'})})}
     }else created=await api<CreatedUpload>('/api/uploads',{method:'POST',body:JSON.stringify({parent_id:task.parentId,name:task.file.name,size:task.file.size,mime_type:task.file.type||'application/octet-stream'})})
     task.uploadId=created.upload_id
     saveUpload(task)
     if(task.cancelled){await abortRemote(task);return}
     let parts:CompletedPart[]=[]
     if(created.status==='completed'){
       await api(`/api/uploads/${created.upload_id}/complete`,{method:'POST',body:JSON.stringify({parts:[]})})
     }else if(created.mode==='single'){
       if(!created.url)throw new Error('服务端没有返回上传地址')
       await retrying(()=>xhrPut(created.url!,task.file,task,loaded=>{task.progress=Math.floor(percentage(loaded,task.file.size)*.98)},task.file.type||'application/octet-stream'),task)
     }else{
       parts=await uploadMultipart(task,created)
     }
     if(task.cancelled){await abortRemote(task);return}
     if(created.status!=='completed')await retrying(()=>api(`/api/uploads/${created.upload_id}/complete`,{method:'POST',body:JSON.stringify({parts})},120000),task)
     forgetUpload(task.uploadId);task.progress=100;task.status='done';scheduleUploadRefresh();scheduleAutoClear()
   }catch(e){if(task.cancelled){task.status='cancelled';scheduleAutoClear()}else{task.status='failed';task.error=(e as Error).message}}
 }
 
 async function uploadMultipart(task:UploadTask,created:CreatedUpload):Promise<CompletedPart[]>{
   const existing=new Map((created.parts||[]).map(part=>[part.part_number,part]))
   const numbers=Array.from({length:created.part_count},(_,index)=>index+1).filter(number=>!existing.has(number))
   const sent=new Array(numbers.length).fill(0) as number[]
   const completed=new Array<CompletedPart>(created.part_count)
   for(const part of existing.values())completed[part.part_number-1]=part
   for(let from=0;from<numbers.length;from+=PART_URL_BATCH){
     if(task.cancelled)throw new Error('上传已取消')
     const page=numbers.slice(from,from+PART_URL_BATCH)
     const data=await api<{parts:{part_number:number;url:string}[]}>(`/api/uploads/${created.upload_id}/parts`,{method:'POST',body:JSON.stringify({part_numbers:page})})
     let cursor=0
     const worker=async()=>{
       while(true){
         const localIndex=cursor++
         if(localIndex>=data.parts.length)return
         if(task.cancelled)throw new Error('上传已取消')
         const part=data.parts[localIndex]
         const idx=part.part_number-1
         const start=idx*created.part_size
         const blob=task.file.slice(start,Math.min(task.file.size,start+created.part_size))
         const etag=await retrying(()=>xhrPut(part.url,blob,task,loaded=>{sent[idx]=loaded;task.progress=Math.floor(percentage(sent.reduce((a,x)=>a+x,0),task.file.size)*.98)}),task)
         if(!etag)throw new Error('对象存储没有暴露 ETag，请检查 Bucket CORS 的 ExposeHeaders')
         completed[idx]={part_number:part.part_number,etag}
         await retrying(()=>api(`/api/uploads/${created.upload_id}/parts/${part.part_number}`,{method:'PUT',body:JSON.stringify({etag,size:blob.size})}),task)
       }
     }
     await Promise.all(Array.from({length:Math.min(MULTIPART_CONCURRENCY,data.parts.length)},worker))
   }
   return completed
 }
 
 async function retrying<T>(operation:()=>Promise<T>,task:UploadTask):Promise<T>{
   let last:unknown
   for(let attempt=0;attempt<UPLOAD_RETRIES;attempt++){
     if(task.cancelled)throw new Error('上传已取消')
     try{return await operation()}catch(error){last=error;if(attempt+1<UPLOAD_RETRIES)await new Promise(resolve=>window.setTimeout(resolve,Math.min(8000,500*2**attempt)+Math.random()*250))}
   }
   throw last
 }
 
 function xhrPut(url:string,body:Blob,task:UploadTask,onProgress:(n:number)=>void,contentType?:string):Promise<string>{
   return new Promise((resolve,reject)=>{
     const xhr=new XMLHttpRequest()
     const detach=()=>{task.requests=task.requests.filter(request=>request!==xhr)}
     task.requests.push(xhr)
     xhr.open('PUT',url)
     if(contentType)xhr.setRequestHeader('Content-Type',contentType)
     xhr.upload.onprogress=e=>{if(e.lengthComputable)onProgress(e.loaded)}
     xhr.onload=()=>{
       detach()
       if(xhr.status>=200&&xhr.status<300)resolve(xhr.getResponseHeader('ETag')||'')
       else reject(new Error(`S3 上传失败 (${xhr.status})`))
     }
     xhr.onerror=()=>{detach();reject(new Error('无法连接对象存储，请检查 S3 CORS'))}
     xhr.onabort=()=>{detach();reject(new Error('上传已取消'))}
     xhr.send(body)
   })
 }
 function percentage(done:number,total:number){return total===0?100:Math.min(99,Math.round(done/total*100))}
 async function cancelUpload(task:UploadTask){task.cancelled=true;task.requests.forEach(x=>x.abort());await abortRemote(task);forgetUpload(task.uploadId);task.status='cancelled'}
 async function abortRemote(task:UploadTask){if(task.uploadId){try{await api(`/api/uploads/${task.uploadId}`,{method:'DELETE'})}catch{/* stale cleanup retries later */}}}
 async function retry(task:UploadTask){await abortRemote(task);task.status='queued';task.error='';task.uploadId=undefined;task.requests=[];task.cancelled=false;pumpQueue()}
 // 完成记录保留在上传进度中，等用户主动清除。
 function scheduleAutoClear(){}
 function scheduleUploadRefresh(){window.clearTimeout(uploadRefreshTimer);uploadRefreshTimer=window.setTimeout(()=>void openFolder(currentId.value),250)}
 function disposeUploads(){window.clearTimeout(uploadRefreshTimer);tasks.forEach(task=>task.requests.forEach(request=>request.abort()))}
 return {chooseFiles,chooseFolder,acceptFiles,acceptFolder,onDrop,cancelUpload,retry,scheduleAutoClear,disposeUploads}
}

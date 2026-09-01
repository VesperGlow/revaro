import { computed, reactive, type ComputedRef, type Ref } from 'vue'
import { api } from '../api'
import type { DriveFile } from '../api'
import { isImage, thumbSRC } from '../fileTypes'
import { classifyLocalMergeFile, localNaturalLess, localSubtitlePriority, localMergeTopLevelName, selectLocalCover } from '../localMerge'
import type { AudioMergeFormat, AudioMergeResponse, LocalMergeCreateResponse, LocalMergePick } from '../types'

type LocalUploadSession={id:string;picks:LocalMergePick[];cancelled:boolean}
export function useAudioMerge(deps:{rootId:string;items:Ref<DriveFile[]>;current:Ref<DriveFile|null>;selectedAudioFiles:ComputedRef<DriveFile[]>;canMergeSelectedAudio:ComputedRef<boolean>;currentId:Ref<string>;audioCoverInput:Ref<HTMLInputElement|null>;localMergeInput:Ref<HTMLInputElement|null>;openModal:(name:'audioMerge')=>void;closeModal:()=>void;notify:(text:string,kind?:'error'|'success')=>void;clearSelection:()=>void;refreshBackgroundTasks:()=>Promise<void>}){
 const {rootId:ROOT,items,current,selectedAudioFiles,canMergeSelectedAudio,currentId,audioCoverInput,localMergeInput,openModal,closeModal,notify,clearSelection,refreshBackgroundTasks}=deps
 const audioMerge=reactive({name:'',format:'flac' as AudioMergeFormat,order:[] as DriveFile[],coverData:'',coverFileId:'',coverPreview:'',coverName:'',error:'',busy:false,local:false})
 const localMerge=reactive({picks:[] as LocalMergePick[],dirName:'',order:[] as string[],cover:'',coverPreview:'',name:'',error:'',total:0,busy:false})
 const localUploads=new Map<string,LocalUploadSession>()
 const audioCoverCandidates=computed(()=>items.value.filter(item=>isImage(item)&&item.size<=16*1024*1024&&!(/\.avif$/i.test(item.name)||item.mime_type==='image/avif')))
 const audioMergeSubtitleCount=computed(()=>audioMerge.order.filter(item=>audioSubtitleFor(item)).length)
 function audioMergeExtension(format:AudioMergeFormat){return format==='flac'?'.flac':'.m4a'}
 const audioSubtitleSourceExtensions=new Set(['.mp3','.wav','.flac','.m4a','.aac','.ogg','.opus','.wma','.aiff','.aif','.mka','.webm'])
 function audioSubtitleMatchPriority(audioName:string,subtitleName:string){
   if(!/\.vtt$/i.test(subtitleName))return -1
   const audioTitle=audioName.replace(/\.[^.]+$/,'')
   const subtitleTitle=subtitleName.replace(/\.vtt$/i,'')
   if(subtitleTitle.localeCompare(audioTitle,undefined,{sensitivity:'accent'})===0)return 0
   if(subtitleTitle.localeCompare(audioName,undefined,{sensitivity:'accent'})===0)return 1
   const sourceExtension=subtitleTitle.match(/\.[^.]+$/)?.[0].toLowerCase()||''
   if(audioSubtitleSourceExtensions.has(sourceExtension)&&subtitleTitle.slice(0,-sourceExtension.length).localeCompare(audioTitle,undefined,{sensitivity:'accent'})===0)return 2
   return -1
 }
 function audioSubtitleFor(item:DriveFile){
   return items.value
     .filter(candidate=>candidate.kind==='file'&&candidate.status==='ready')
     .map(candidate=>({candidate,priority:audioSubtitleMatchPriority(item.name,candidate.name)}))
     .filter(match=>match.priority>=0)
     .sort((a,b)=>a.priority-b.priority)[0]?.candidate
 }
 function defaultAudioMergeName(files:DriveFile[],format:AudioMergeFormat){
   const folderName=currentId.value!==ROOT?current.value?.name.trim():''
   const firstName=files[0]?.name.replace(/\.[^.]+$/,'')||'合并音频'
   return `${folderName||`${firstName} 等`}${audioMergeExtension(format)}`
 }
 function showAudioMerge(){
   if(!canMergeSelectedAudio.value)return
   audioMerge.local=false
   audioMerge.format='flac';audioMerge.name=defaultAudioMergeName(selectedAudioFiles.value,audioMerge.format)
   audioMerge.order=[...selectedAudioFiles.value]
   clearAudioCover()
   const suggested=[...audioCoverCandidates.value].sort((a,b)=>coverSuggestionScore(b)-coverSuggestionScore(a))[0]
   if(suggested)selectDirectoryCover(suggested)
   audioMerge.error='';audioMerge.busy=false
   openModal('audioMerge')
 }
 function coverSuggestionScore(item:DriveFile){const name=item.name.toLowerCase();return /(^|[\s_.-])(cover|folder|front|封面)([\s_.-]|$)/i.test(name)?3:/^(00|01)[\s_.-]/.test(name)?2:1}
 function chooseAudioCover(){audioCoverInput.value?.click()}
 function clearAudioCover(){audioMerge.coverData='';audioMerge.coverFileId='';audioMerge.coverPreview='';audioMerge.coverName=''}
 function selectDirectoryCover(item:DriveFile){audioMerge.coverData='';audioMerge.coverFileId=item.id;audioMerge.coverPreview=thumbSRC(item);audioMerge.coverName=item.name;audioMerge.error=''}
 async function setAudioCover(file:File){
   audioMerge.error=''
   if(!['image/jpeg','image/png','image/webp','image/gif'].includes(file.type)){audioMerge.error='封面请选择 JPG、PNG、WebP 或 GIF 图片';return}
   if(file.size>16*1024*1024){audioMerge.error='封面源图片不能超过 16 MiB';return}
   let sourceURL=''
   try{
     sourceURL=URL.createObjectURL(file)
     const image=await new Promise<HTMLImageElement>((resolve,reject)=>{const el=new Image();el.onload=()=>resolve(el);el.onerror=()=>reject(new Error('无法读取封面图片'));el.src=sourceURL})
     const scale=Math.min(1,1200/Math.max(image.naturalWidth,image.naturalHeight))
     const canvas=document.createElement('canvas');canvas.width=Math.max(1,Math.round(image.naturalWidth*scale));canvas.height=Math.max(1,Math.round(image.naturalHeight*scale))
     canvas.getContext('2d')?.drawImage(image,0,0,canvas.width,canvas.height)
     const encode=(quality:number)=>new Promise<Blob>((resolve,reject)=>canvas.toBlob(blob=>blob?resolve(blob):reject(new Error('无法处理封面图片')),'image/jpeg',quality))
     let blob=await encode(.88)
     if(blob.size>2*1024*1024)blob=await encode(.72)
     if(blob.size>2*1024*1024)throw new Error('封面处理后仍超过 2 MiB，请换一张图片')
     const dataURL=await new Promise<string>((resolve,reject)=>{const reader=new FileReader();reader.onload=()=>resolve(String(reader.result));reader.onerror=()=>reject(new Error('无法读取封面图片'));reader.readAsDataURL(blob)})
     audioMerge.coverData=dataURL.slice(dataURL.indexOf(',')+1);audioMerge.coverFileId='';audioMerge.coverPreview=dataURL;audioMerge.coverName=file.name
   }catch(e){audioMerge.error=(e as Error).message}
   finally{if(sourceURL)URL.revokeObjectURL(sourceURL)}
 }
 function setAudioMergeFormat(format:AudioMergeFormat){
   const base=audioMerge.name.replace(/\.(?:flac|m4a)$/i,'')
   audioMerge.format=format;audioMerge.name=base+audioMergeExtension(format);audioMerge.error=''
 }
 function moveAudioMergeInput(index:number,delta:number){
   const target=index+delta
   if(target<0||target>=audioMerge.order.length)return
   const next=[...audioMerge.order]
   ;[next[index],next[target]]=[next[target],next[index]]
   audioMerge.order=next
 }
 async function startAudioMerge(){
   audioMerge.error=''
   let name=audioMerge.name.trim()
   if(!name){audioMerge.error='请输入输出文件名';return}
   const extension=audioMergeExtension(audioMerge.format)
   if(!name.toLowerCase().endsWith(extension))name=name.replace(/\.(?:flac|m4a)$/i,'')+extension
   audioMerge.name=name;audioMerge.busy=true
   try{
     const data=await api<AudioMergeResponse>('/api/audio-merges',{method:'POST',body:JSON.stringify({parent_id:currentId.value,name,format:audioMerge.format,file_ids:audioMerge.order.map(item=>item.id),cover_jpeg:audioMerge.coverData,cover_file_id:audioMerge.coverFileId})})
     clearSelection();closeModal();void refreshBackgroundTasks();notify(`「${data.output_name}」已加入后台队列`,'success')
   }catch(e){audioMerge.error=(e as Error).message}
   finally{audioMerge.busy=false}
 }
 // ── 从本地目录合并：WAV + VTT + 封面 → 无损 ALAC M4A ────────────────────────
 const LOCAL_MERGE_CONCURRENCY = 3
 const LOCAL_MERGE_CHUNK_RETRIES = 3
 interface LocalMergeEntryFile { name:string; size:number; file:File }
 function showLocalAudioMerge(){void pickLocalMergeDir()}
 function chooseLocalMergeDir(){void pickLocalMergeDir()}
 function localSubtitleFor(audioName:string){
   return localMerge.picks
     .filter(pick=>pick.kind==='subtitle')
     .map(pick=>({name:pick.name,priority:localSubtitlePriority(audioName,pick.name)}))
     .filter(match=>match.priority>=0)
     .sort((a,b)=>a.priority-b.priority)[0]?.name
 }
 // pickLocalMergeDir prefers the File System Access API so the chosen folder's
 // audio/vtt/cover files are enumerated and read directly, instead of entering
 // the browser's webkitdirectory "Select Folder to Upload" mode. Browsers
 // without showDirectoryPicker (e.g. Firefox) fall back to that input, whose
 // paths are then flattened by onLocalMergeDir.
 async function pickLocalMergeDir(){
   if(typeof window.showDirectoryPicker === 'function'){
     try{
       const handle=await window.showDirectoryPicker({ mode:'read' })
       const entries:LocalMergeEntryFile[]=[]
       for await (const entry of handle.entries()){
         if(entry[1].kind!=='file')continue
         const fileHandle=entry[1] as FileSystemFileHandle
         const file=await fileHandle.getFile()
         entries.push({ name:entry[0], size:file.size, file })
       }
       if(!entries.length){notify('所选目录里没有文件');return}
       applyLocalMergeEntries(entries, handle.name)
     }catch(e){
       if((e as DOMException)?.name==='AbortError')return
       notify((e as Error).message)
     }
     return
   }
   localMergeInput.value?.click()
 }
 // applyLocalMergeEntries is the single directory-scan → merge pipeline
 // hand-off: classify the picked files, validate them and populate the shared
 // local merge state before opening the audio merge modal. Everything after
 // this point (order, subtitles, cover, ALAC/M4A output, upload and task
 // status) reuses the existing audio merge flow.
 function applyLocalMergeEntries(entries:LocalMergeEntryFile[],dirName:string){
   const picks:LocalMergePick[]=[]
   for(const entry of entries){
     const kind=classifyLocalMergeFile(entry.name)
     if(!kind)continue
     picks.push({file:entry.file,name:entry.name,size:entry.size,kind,preview:kind==='cover'?URL.createObjectURL(entry.file):undefined})
   }
   const audios=picks.filter(pick=>pick.kind==='audio')
   const subtitles=picks.filter(pick=>pick.kind==='subtitle')
   const covers=picks.filter(pick=>pick.kind==='cover')
   if(audios.length<2){notify(`至少需要 2 个音频文件（当前 ${audios.length} 个）`);return}
   if(audios.length>256){notify('音频文件不能超过 256 个');return}
   if(subtitles.length>512||covers.length>64){notify('字幕或封面文件过多');return}
   if(audios.some(audio=>audio.size>64*1024*1024*1024)){notify('单个音频文件不能超过 64 GiB');return}
   if(subtitles.some(vtt=>vtt.size>8*1024*1024)){notify('字幕文件不能超过 8 MiB');return}
   if(covers.some(cover=>cover.size>16*1024*1024)){notify('封面图片不能超过 16 MiB');return}
   const total=picks.reduce((sum,pick)=>sum+pick.size,0)
   if(total>128*1024*1024*1024){notify('所选素材合计超过 128 GiB');return}
   for(const pick of localMerge.picks)if(pick.preview)URL.revokeObjectURL(pick.preview)
   localMerge.picks=picks
   localMerge.dirName=dirName
   localMerge.order=[...audios.map(pick=>pick.name)].sort((a,b)=>localNaturalLess(a,b)?-1:1)
   localMerge.cover=selectLocalCover(covers.map(pick=>pick.name))
   localMerge.coverPreview=localMerge.cover?picks.find(pick=>pick.name===localMerge.cover)?.preview||'':''
   localMerge.name=`${dirName||`${audios[0].name.replace(/\.[^.]+$/,'')} 等`}.m4a`
   localMerge.error='';localMerge.total=total;localMerge.busy=false
   audioMerge.local=true;audioMerge.error=''
   openModal('audioMerge')
 }
 // onLocalMergeDir handles the webkitdirectory fallback input. Its relative
 // paths include the picked folder as the first segment, so top-level files
 // are identified with localMergeTopLevelName instead of a naive "/" check.
 function onLocalMergeDir(list:FileList){
   const files=Array.from(list)
   if(!files.length)return
   const dirName=(files[0].webkitRelativePath||'').replaceAll('\\','/').split('/')[0]||''
   const entries:LocalMergeEntryFile[]=[]
   for(const file of files){
     const relative=(file.webkitRelativePath||file.name).replaceAll('\\','/')
     const name=localMergeTopLevelName(relative)
     if(!name)continue
     entries.push({name,size:file.size,file})
   }
   applyLocalMergeEntries(entries,dirName)
 }
 function selectLocalCoverFile(name:string){localMerge.cover=name;localMerge.coverPreview=localMerge.picks.find(pick=>pick.name===name)?.preview||'';localMerge.error=''}
 function clearLocalCover(){localMerge.cover='';localMerge.coverPreview='';localMerge.error=''}
 function moveLocalMergeInput(index:number,delta:number){
   const target=index+delta
   if(target<0||target>=localMerge.order.length)return
   const next=[...localMerge.order]
   ;[next[index],next[target]]=[next[target],next[index]]
   localMerge.order=next
 }
 function setAudioMergeSource(source:'revaro'|'local'){
   if(source==='local'){audioMerge.local=true;audioMerge.error='';return}
   audioMerge.local=false
   if(!canMergeSelectedAudio.value){audioMerge.error='请先选择至少 2 个音频文件';return}
   audioMerge.format='flac';audioMerge.name=defaultAudioMergeName(selectedAudioFiles.value,audioMerge.format)
   audioMerge.order=[...selectedAudioFiles.value]
   clearAudioCover()
   const suggested=[...audioCoverCandidates.value].sort((a,b)=>coverSuggestionScore(b)-coverSuggestionScore(a))[0]
   if(suggested)selectDirectoryCover(suggested)
   audioMerge.error=''
 }
 function resetLocalMergeState(){
   for(const pick of localMerge.picks)if(pick.preview)URL.revokeObjectURL(pick.preview)
   localMerge.picks=[];localMerge.dirName='';localMerge.order=[];localMerge.cover='';localMerge.coverPreview='';localMerge.name='';localMerge.error='';localMerge.total=0;localMerge.busy=false
 }
 async function startLocalMerge(){
   localMerge.error=''
   let name=localMerge.name.trim()
   if(!name){localMerge.error='请输入输出文件名';return}
   if(!/\.m4a$/i.test(name))name+='.m4a'
   localMerge.name=name
   const picks=localMerge.picks
   const body={parent_id:currentId.value,name,files:picks.map(pick=>({name:pick.name,size:pick.size})),order:[...localMerge.order],cover:localMerge.cover||null}
   localMerge.busy=true
   try{
     const created=await api<LocalMergeCreateResponse>('/api/audio-merges/local',{method:'POST',body:JSON.stringify(body)})
     // 任务立即进入全局任务中心；关闭弹窗不影响后续上传与合并。
     void refreshBackgroundTasks()
     const session:LocalUploadSession={id:created.id,picks,cancelled:false}
     localUploads.set(created.id,session)
     resetLocalMergeState()
     closeModal()
     notify(`「${created.output_name}」已开始上传素材`,'success')
     void uploadLocalMergeChunks(created,session)
   }catch(e){
     localMerge.error=(e as Error).message
     localMerge.busy=false
   }
 }
 async function uploadLocalMergeChunks(created:LocalMergeCreateResponse,session:LocalUploadSession){
   let cursor=0
   const worker=async()=>{
     while(!session.cancelled){
       const fileIndex=cursor++
       if(fileIndex>=created.files.length)return
       const file=created.files[fileIndex]
       const pick=session.picks.find(candidate=>candidate.name===file.name)
       if(!pick)throw new Error(`找不到素材「${file.name}」`)
       for(let chunk=0;chunk<file.chunk_count;chunk++){
         if(session.cancelled)return
         const start=chunk*created.chunk_size
         const blob=pick.file.slice(start,Math.min(pick.size,start+created.chunk_size))
         await putLocalChunkWithRetry(created.id,fileIndex,chunk,blob)
       }
     }
   }
   try{
     const workers=Array.from({length:Math.min(LOCAL_MERGE_CONCURRENCY,created.files.length)},worker)
     await Promise.all(workers)
     if(session.cancelled){await cancelLocalMergeRemote(created.id);return}
     const snapshot=await api<AudioMergeResponse>(`/api/audio-merges/local/${created.id}/complete`,{method:'POST'})
     void refreshBackgroundTasks()
     notify(`「${snapshot.output_name}」素材上传完成，开始后台合并`,'success')
   }catch(e){
     if(session.cancelled){await cancelLocalMergeRemote(created.id);return}
     await cancelLocalMergeRemote(created.id)
     void refreshBackgroundTasks()
     notify(`「${created.output_name}」素材上传失败：${(e as Error).message}`)
   }finally{
     localUploads.delete(created.id)
   }
 }
 async function putLocalChunkWithRetry(jobId:string,fileIndex:number,chunk:number,blob:Blob){
   let lastError:Error|null=null
   for(let attempt=0;attempt<LOCAL_MERGE_CHUNK_RETRIES;attempt++){
     let fatal:Error|null=null
     try{
       const res=await fetch(`/api/audio-merges/local/${jobId}/files/${fileIndex}/chunks/${chunk}`,{method:'POST',headers:{'Content-Type':'application/octet-stream'},body:blob})
       if(res.ok)return
       const detail=await res.json().catch(()=>null)
       const message=(detail as {error?:{message?:string}}|null)?.error?.message||`分块上传被拒绝 (${res.status})`
       if(res.status===429)lastError=new Error(message) // 服务器繁忙：稍后重试
       else if(res.status>=400&&res.status<500)fatal=new Error(message)
       else lastError=new Error(message)
     }catch(e){lastError=e instanceof Error?e:new Error(String(e))}
     if(fatal)throw fatal
     await new Promise(resolve=>setTimeout(resolve,500*(attempt+1)))
   }
   throw lastError||new Error('分块上传失败')
 }
 async function cancelLocalMergeRemote(jobId:string){
   try{await api(`/api/tasks/${jobId}/cancel`,{method:'POST'})}catch{/* 服务端清理失败时稍后由过期清理兜底 */}
 }
 function closeAudioMergeModal(){closeModal()}
 function localFileByName(name:string){return localMerge.picks.find(pick=>pick.name===name)}
 const localCoverCandidates=computed(()=>localMerge.picks.filter(pick=>pick.kind==='cover'))
 
 return {audioMerge,localMerge,localUploads,audioCoverCandidates,audioMergeSubtitleCount,audioMergeExtension,audioSubtitleFor,showAudioMerge,chooseAudioCover,clearAudioCover,selectDirectoryCover,setAudioCover,setAudioMergeFormat,moveAudioMergeInput,startAudioMerge,showLocalAudioMerge,chooseLocalMergeDir,localSubtitleFor,onLocalMergeDir,selectLocalCoverFile,clearLocalCover,moveLocalMergeInput,setAudioMergeSource,startLocalMerge,cancelLocalMergeRemote,closeAudioMergeModal,localFileByName,localCoverCandidates}
}

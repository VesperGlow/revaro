import DOMPurify from 'dompurify'
import { marked } from 'marked'
import { computed, defineAsyncComponent, defineComponent, onBeforeUnmount, onMounted, reactive, ref } from 'vue'
import { api } from './api'
import type { DriveFile } from './api'
import AppDialog from './components/AppDialog.vue'
import AppTopbar from './components/AppTopbar.vue'
import DocumentEditor from './components/DocumentEditor.vue'
import FileBrowserHeader from './components/FileBrowserHeader.vue'
import FileGrid from './components/FileGrid.vue'
import LoginPage from './components/LoginPage.vue'
import MoveCopyDialog from './components/MoveCopyDialog.vue'
import SelectionToolbar from './components/SelectionToolbar.vue'
import ShareDialog from './components/ShareDialog.vue'
import { useAccountSettings } from './composables/useAccountSettings'
import { useAudioMerge } from './composables/useAudioMerge'
import { useBackgroundTasks } from './composables/useBackgroundTasks'
import { useAuthSession } from './composables/useAuthSession'
import { useUploads } from './composables/useUploads'
import { useDialogs } from './composables/useDialogs'
import { isArchive, isAudio, isBook, isEditable, isMedia, isVideo, thumbSRC } from './fileTypes'
import { formatSize } from './format'
import type { ShareResponse, StorageStats, UploadTask } from './types'

export default defineComponent({
  setup(){
    
    const ROOT = '00000000-0000-0000-0000-000000000000'
    
    const user = ref<string|null>(null)
    const hasAvatar = ref(false)
    const avatarVersion = ref(Date.now())
    const checking = ref(true)
    const login = reactive({ username:'admin', password:'', secondFactor:'', totpRequired:false, busy:false, error:'', notice:'' })
    const currentId = ref(ROOT)
    const current = ref<DriveFile|null>(null)
    const items = ref<DriveFile[]>([])
    const breadcrumbs = ref<DriveFile[]>([])
    const loading = ref(false)
    const dragActive = ref(false)
    const toast = reactive({ text:'', kind:'error' as 'error'|'success' })
    const tasks = reactive<UploadTask[]>([])
    const trashMode = ref(false)
    const selected = ref<DriveFile|null>(null)
    const selectedIds = ref<Set<string>>(new Set())
    const moveTargets = ref<DriveFile[]>([])
    const transferMode = ref<'move'|'copy'>('move')
    type ModalName = 'rename'|'move'|'preview'|'share'|'account'|'editor'|'reader'|'audioMerge'
    const modal = ref<ModalName|null>(null)
    const readerFile = ref<DriveFile|null>(null)
    const renameValue = ref('')
    const modalBusy = ref(false)
    const share = reactive({ active:false, url:'', createdAt:'', busy:false, error:'', copied:false })
    const editor = reactive({ isNew:false, readonly:false, fileId:'', name:'', originalName:'', content:'', original:'', etag:'', mode:'edit' as 'edit'|'split'|'preview', busy:false, error:'' })
    const directoryStats = reactive<StorageStats>({ total_bytes:0, file_count:0 })
    const fileInput = ref<HTMLInputElement|null>(null)
    const folderInput = ref<HTMLInputElement|null>(null)
    const localMergeInput = ref<HTMLInputElement|null>(null)
    const avatarInput = ref<HTMLInputElement|null>(null)
    const audioCoverInput = ref<HTMLInputElement|null>(null)
    let toastTimer = 0
    const MediaPreview=defineAsyncComponent(()=>import('./components/MediaPreview.vue'))
    const ReaderView=defineAsyncComponent(()=>import('./Reader.vue'))
    
    const editorDirty = computed(() => editor.content !== editor.original || editor.name !== editor.originalName)
    const editorBytes = computed(() => new Blob([editor.content]).size)
    const editorIsMarkdown = computed(() => /\.(md|markdown)$/i.test(editor.name))
    const renderedMarkdown = computed(() => DOMPurify.sanitize(marked.parse(editor.content, { async:false }) as string))
    const selectedItems = computed(() => items.value.filter(item => selectedIds.value.has(item.id)))
    const selectedBytes = computed(() => selectedItems.value.reduce((total,item) => total+(item.kind==='file'?item.size:0),0))
    const selectedFiles = computed(() => selectedItems.value.filter(item => item.kind==='file'))
    const selectedAudioFiles = computed(() => selectedItems.value.filter(isAudio))
    const canMergeSelectedAudio = computed(() => selectedItems.value.length>=2&&selectedItems.value.length===selectedAudioFiles.value.length)
    const singleSelected = computed(() => selectedItems.value.length===1?selectedItems.value[0]:null)
    
    const {dialog,askDialog,confirmDialog,promptDialog,finishDialog}=useDialogs()
    
    function notify(text:string, kind:'error'|'success'='error') { toast.text=text;toast.kind=kind;window.clearTimeout(toastTimer);toastTimer=window.setTimeout(()=>toast.text='',3600) }
    
    const {account,accountPanel,usernameEditing,usernameSaving,usernameError,usernameInput,avatar,twoFactor,avatarURL,showAccount,startUsernameEdit,cancelUsernameEdit,saveUsername,openAccountPanel,closeAccountPanel,chooseAvatar,uploadAvatar,removeAvatar,savePassword,beginTwoFactorSetup,cancelTwoFactorSetup,enableTwoFactor,regenerateRecoveryCodes,disableTwoFactor,copyRecoveryCodes,downloadRecoveryCodes}=useAccountSettings({user,hasAvatar,avatarVersion,login,items,tasks,modalBusy,avatarInput,notify,openModal,closeModal,confirmDialog})
    const {audioMerge,localMerge,localUploads,audioCoverCandidates,audioMergeSubtitleCount,audioMergeExtension,audioSubtitleFor,showAudioMerge,chooseAudioCover,clearAudioCover,selectDirectoryCover,setAudioCover,setAudioMergeFormat,moveAudioMergeInput,startAudioMerge,showLocalAudioMerge,chooseLocalMergeDir,localSubtitleFor,onLocalMergeDir,selectLocalCoverFile,clearLocalCover,moveLocalMergeInput,setAudioMergeSource,startLocalMerge,cancelLocalMergeRemote,closeAudioMergeModal,localFileByName,localCoverCandidates}=useAudioMerge({rootId:ROOT,items,current,selectedAudioFiles,canMergeSelectedAudio,currentId,audioCoverInput,localMergeInput,openModal,closeModal,notify,clearSelection,refreshBackgroundTasks:()=>refreshBackgroundTasks()})
    const {chooseFiles,chooseFolder,acceptFiles,acceptFolder,onDrop,cancelUpload,retry,scheduleAutoClear,disposeUploads}=useUploads({tasks,currentId,dragActive,trashMode,fileInput,folderInput,notify,openFolder})
    const {backgroundTasks,refreshJobsFromEvent,refreshBackgroundTasks,cancelBackgroundTask,retryBackgroundTask,jobEvents}=useBackgroundTasks({user,tasks,localUploads,currentId,notify,openFolder,cancelUpload,retryUpload:retry})
    const {checkSession,submitLogin,logout}=useAuthSession({user,hasAvatar,checking,login,items,tasks,backgroundTasks,openRoute,openFolder,rootId:ROOT,jobEvents,refreshJobs:refreshJobsFromEvent})
    function filesChanged(event:Event){const el=event.target as HTMLInputElement;if(el.files)acceptFiles(el.files);el.value=''}
    function folderChanged(event:Event){const el=event.target as HTMLInputElement;if(el.files)acceptFolder(el.files);el.value=''}
    function localMergeChanged(event:Event){const el=event.target as HTMLInputElement;if(el.files)onLocalMergeDir(el.files);el.value=''}
    function audioCoverChanged(event:Event){const el=event.target as HTMLInputElement;if(el.files?.[0])void setAudioCover(el.files[0]);el.value=''}
    function avatarChanged(event:Event){const el=event.target as HTMLInputElement;if(el.files?.[0])void uploadAvatar(el.files[0]);el.value=''}
    function blurEventTarget(event:Event){(event.target as HTMLInputElement).blur()}
    
    function openReader(item:DriveFile){readerFile.value=item;openModal('reader');history.replaceState({revaroNav:true},'','/read/'+item.id)}
    // 启动路由：按 URL 恢复到对应文件夹（/f/{id}），再处理阅读器深链。
    async function openRoute(){
      const fm=location.pathname.match(/^\/f\/([^/]+)\/?$/)
      suppressHistory=true
      try{
        if(fm){
          const id=decodeURIComponent(fm[1])
          await openFolder(id)
          if(currentId.value!==id)history.replaceState({revaroNav:true},'','/')
        }else{
          await openFolder(ROOT)
        }
      }finally{suppressHistory=false}
      openDeepLink()
    }
    // 深链 /read/{fileId}：登录后直接打开阅读器。
    async function openDeepLink(){
      const m=location.pathname.match(/^\/read\/([^/]+)\/?$/)
      if(!m)return
      history.replaceState({revaroNav:true},'',folderURL(currentId.value))
      try{
        const data=await api<{file:DriveFile}>(`/api/files/${decodeURIComponent(m[1])}`)
        if(data.file.kind==='file'&&isBook(data.file))openReader(data.file)
      }catch{/* 文件不存在或不可读：留在当前文件夹 */}
    }
    function folderURL(id:string){return id===ROOT?'/':'/f/'+id}
    // 导航请求序号：快速连续切换目录时，只接受最后一次请求的响应，
    // 防止较慢的旧响应覆盖新目录的内容（竞态）。
    let folderSeq=0
    async function openFolder(id:string){
      const seq=++folderSeq
      loading.value=true
      try{
        const [meta,list]=await Promise.all([api<{file:DriveFile;breadcrumbs:DriveFile[]}>(`/api/files/${id}`),api<{items:DriveFile[];total_bytes:number;file_count:number}>(`/api/files/${id}/children`)])
        if(seq!==folderSeq)return
        if(!suppressHistory&&id!==currentId.value){navActions.value.push({kind:'folder',id:currentId.value});window.history.pushState({revaroNav:true},'')}
        trashMode.value=false;currentId.value=id;current.value=meta.file;breadcrumbs.value=meta.breadcrumbs;items.value=list.items;directoryStats.total_bytes=list.total_bytes;directoryStats.file_count=list.file_count;selected.value=null;clearSelection();history.replaceState({revaroNav:true},'',folderURL(id))
      }catch(e){if(seq===folderSeq)notify((e as Error).message)}
      finally{if(seq===folderSeq)loading.value=false}
    }
    
    // 应用内导航历史：每次进入文件夹/打开弹窗都 pushState，系统返回键先关
    // 弹窗、再逐级返回上一屏，而不是直接退出整个应用。
    type NavAction={kind:'folder';id:string}|{kind:'modal-close'}
    const navActions=ref<NavAction[]>([])
    let suppressHistory=false
    let popChain:Promise<void>=Promise.resolve()
    function handlePopState(){
      const action=navActions.value.pop()
      if(!action)return
      if(action.kind==='modal-close'){
        if(modal.value==='reader')history.replaceState({revaroNav:true},'',folderURL(currentId.value))
        modal.value=null;return
      }
      popChain=popChain.then(async()=>{
        suppressHistory=true
        try{await openFolder(action.id)}finally{suppressHistory=false}
      })
    }
    function openModal(name:ModalName){
      if(!modal.value){navActions.value.push({kind:'modal-close'});window.history.pushState({revaroNav:true},'')}
      modal.value=name
    }
    function closeModal(){if(modal.value)window.history.back()}
    function goUp(){const parent=current.value?.parent_id;if(parent)openFolder(parent)}
    async function openTrash(){loading.value=true;try{const data=await api<{items:DriveFile[];total_bytes:number;file_count:number}>('/api/trash');trashMode.value=true;items.value=data.items;directoryStats.total_bytes=data.total_bytes;directoryStats.file_count=data.file_count;current.value=null;breadcrumbs.value=[];selected.value=null;clearSelection()}catch(e){notify((e as Error).message)}finally{loading.value=false}}
    async function createFolder(){const name=await promptDialog({title:'新建文件夹',message:'给这个文件夹起个名字。',placeholder:'文件夹名称',confirmLabel:'创建'});if(!name)return;try{await api('/api/directories',{method:'POST',body:JSON.stringify({parent_id:currentId.value,name})});await openFolder(currentId.value);notify('文件夹已创建','success')}catch(e){notify((e as Error).message)}}
    async function removeSelected(){
      const targets=[...selectedItems.value]
      if(!targets.length)return
      if(!await confirmDialog({title:`移入回收站？`,message:`选中的 ${targets.length} 项会移入回收站，文件夹中的内容也会一起保留。`,confirmLabel:'移入回收站',tone:'danger'}))return
      let removed=0
      const errors:string[]=[]
      for(const item of targets){
        try{await api(`/api/files/${item.id}`,{method:'DELETE'});removed++}
        catch(e){errors.push(`${item.name}：${(e as Error).message}`)}
      }
      await openFolder(currentId.value)
      if(errors.length)notify(`已移入 ${removed} 项，${errors.length} 项失败：${errors[0]}`)
      else notify(`已将 ${removed} 项移入回收站`,'success')
    }
    async function restoreSelected(){for(const item of [...selectedItems.value]){try{await api(`/api/trash/${item.id}/restore`,{method:'POST'})}catch(e){notify(`${item.name}：${(e as Error).message}`);return}}await openTrash();notify('所选项目已恢复','success')}
    async function purgeSelected(){const targets=[...selectedItems.value];if(!targets.length||!await confirmDialog({title:`永久删除 ${targets.length} 项？`,message:'这个操作无法撤销。无引用的数据块会在垃圾回收后清理。',confirmLabel:'永久删除',tone:'danger'}))return;for(const item of targets){try{await api(`/api/trash/${item.id}`,{method:'DELETE'})}catch(e){notify(`${item.name}：${(e as Error).message}`);return}}await openTrash();notify('已永久删除所选项目','success')}
    async function emptyTrash(){if(!items.value.length||!await confirmDialog({title:'清空回收站？',message:`回收站中的 ${items.value.length} 项及其内容都会永久删除，无法恢复。`,confirmLabel:'清空回收站',tone:'danger'}))return;try{await api('/api/trash',{method:'DELETE'});await openTrash();notify('回收站已清空','success')}catch(e){notify((e as Error).message)}}
    function showRename(item:DriveFile){selected.value=item;renameValue.value=item.name;openModal('rename')}
    async function saveRename(){if(!selected.value)return;modalBusy.value=true;try{await api(`/api/files/${selected.value.id}`,{method:'PATCH',body:JSON.stringify({name:renameValue.value})});closeModal();await openFolder(currentId.value);notify('已重命名','success')}catch(e){notify((e as Error).message)}finally{modalBusy.value=false}}
    async function showMove(item:DriveFile){await showMoveTargets([item])}
    async function showMoveSelected(){await showMoveTargets([...selectedItems.value])}
    async function showMoveTargets(targets:DriveFile[]){
      if(!targets.length)return
      transferMode.value='move'
      moveTargets.value=targets;selected.value=targets[0];modalBusy.value=false;openModal('move')
    }
    async function showCopy(item:DriveFile){
      transferMode.value='copy';moveTargets.value=[item];selected.value=item;modalBusy.value=false;openModal('move')
    }
    async function transferTo(parentId:string){
      const targets=[...moveTargets.value]
      if(!targets.length)return
      modalBusy.value=true
      let completed=0
      const errors:string[]=[]
      for(const item of targets){
        try{
          if(transferMode.value==='copy')await api(`/api/files/${item.id}/copy`,{method:'POST',body:JSON.stringify({parent_id:parentId})})
          else await api(`/api/files/${item.id}`,{method:'PATCH',body:JSON.stringify({parent_id:parentId})})
          completed++
        }
        catch(e){errors.push(`${item.name}：${(e as Error).message}`)}
      }
      closeModal();await openFolder(currentId.value);moveTargets.value=[]
      const verb=transferMode.value==='copy'?'复制':'移动'
      if(errors.length)notify(`已${verb} ${completed} 项，${errors.length} 项失败：${errors[0]}`)
      else notify(`已${verb} ${completed} 项`,'success')
      modalBusy.value=false
    }
    function showPreview(item:DriveFile){selected.value=item;openModal('preview')}
    async function showShare(item:DriveFile){selected.value=item;openModal('share');share.active=false;share.url='';share.createdAt='';share.error='';share.copied=false;share.busy=true;try{const data=await api<ShareResponse>(`/api/files/${item.id}/share`);share.active=data.active;share.url=data.url||'';share.createdAt=data.created_at||''}catch(e){share.error=(e as Error).message}finally{share.busy=false}}
    async function createShare(replace=false){if(!selected.value)return;if(replace&&!await confirmDialog({title:'重新生成分享链接？',message:'旧分享链接会立即失效，拿到旧链接的人将无法继续访问。',confirmLabel:'重新生成'}))return;share.busy=true;share.error='';share.copied=false;try{const data=await api<ShareResponse>(`/api/files/${selected.value.id}/share`,{method:'POST'});share.active=data.active;share.url=data.url||'';share.createdAt=data.created_at||''}catch(e){share.error=(e as Error).message}finally{share.busy=false}}
    async function revokeShare(){if(!selected.value||!await confirmDialog({title:'停止分享？',message:'现有公开链接会立即失效。文件本身不会被删除。',confirmLabel:'停止分享',tone:'danger'}))return;share.busy=true;share.error='';try{await api(`/api/files/${selected.value.id}/share`,{method:'DELETE'});share.active=false;share.url='';share.createdAt='';share.copied=false;notify('分享已停止','success')}catch(e){share.error=(e as Error).message}finally{share.busy=false}}
    async function copyShare(){if(!share.url)return;try{await navigator.clipboard.writeText(share.url);share.copied=true;window.setTimeout(()=>share.copied=false,2200)}catch{share.error='复制失败，请手动选择链接复制'}}
    function openItem(item:DriveFile){
      if(item.deleted_at){if(isBook(item))openReader(item);else if(isEditable(item))openEditor(item,true);else if(isMedia(item))showPreview(item);return}
      if(item.kind==='directory')openFolder(item.id);else if(isBook(item)&&(!isEditable(item)||/\.epub$/i.test(item.name)))openReader(item);else if(isEditable(item))openEditor(item);else if(isMedia(item))showPreview(item)
    }
    function newDocument(){selected.value=null;editor.isNew=true;editor.readonly=false;editor.fileId='';editor.name='未命名文档.md';editor.originalName=editor.name;editor.content='';editor.original='';editor.etag='';editor.mode='edit';editor.busy=false;editor.error='';openModal('editor')}
    async function openEditor(item:DriveFile,readonly=false){
      selected.value=item;editor.isNew=false;editor.readonly=readonly;editor.fileId=item.id;editor.name=item.name;editor.originalName=item.name;editor.content='';editor.original='';editor.etag='';editor.mode=readonly&&/\.(md|markdown)$/i.test(item.name)?'preview':'edit';editor.error='';editor.busy=true;openModal('editor')
      try{const data=await api<{content:string;etag:string}>(`/api/files/${item.id}/content`);editor.content=data.content;editor.original=data.content;editor.etag=data.etag||''}
      catch(e){editor.error=(e as Error).message}
      finally{editor.busy=false}
    }
    async function saveDocument(){
      if(editor.readonly)return
      editor.error=''
      if(editor.isNew&&!editor.name.trim()){editor.error='请输入文件名';return}
      if(!/\.(md|markdown|txt|ya?ml|json|toml|ini|conf|log|csv)$/i.test(editor.name)){editor.error='支持 Markdown、TXT、YAML、JSON、TOML、INI、CONF、LOG 和 CSV';return}
      if(editorBytes.value>1024*1024){editor.error='可编辑文档不能超过 1 MiB';return}
      editor.busy=true
      try{
        const saved=editor.isNew
          ?await api<DriveFile>('/api/documents',{method:'POST',body:JSON.stringify({parent_id:currentId.value,name:editor.name.trim(),content:editor.content})})
          :await api<DriveFile>(`/api/files/${editor.fileId}/content`,{method:'PUT',body:JSON.stringify({content:editor.content,etag:editor.etag})})
        editor.isNew=false;editor.fileId=saved.id;editor.name=saved.name;editor.originalName=saved.name;editor.etag=saved.etag||'';editor.original=editor.content;selected.value=saved
        await openFolder(currentId.value);notify('文档已保存','success')
      }catch(e){editor.error=(e as Error).message}
      finally{editor.busy=false}
    }
    async function closeEditor(){if(editorDirty.value&&!await confirmDialog({title:'放弃未保存的修改？',message:'关闭后，本次修改将无法恢复。',confirmLabel:'放弃修改',tone:'danger'}))return;closeModal()}
    function closeBackdrop(){if(modal.value==='editor')void closeEditor();else if(modal.value==='audioMerge')closeAudioMergeModal();else closeModal()}
    function toggleSelection(item:DriveFile){
      const next=new Set(selectedIds.value)
      if(next.has(item.id))next.delete(item.id);else next.add(item.id)
      selectedIds.value=next
    }
    function clearSelection(){selectedIds.value=new Set()}
    function selectAll(){selectedIds.value=selectedItems.value.length===items.value.length?new Set():new Set(items.value.map(item=>item.id))}
    function clearSelectionFromBlank(event:MouseEvent){
      if(!selectedItems.value.length||modal.value)return
      const target=event.target
      if(!(target instanceof Element)||target.closest('button,a,input,textarea,select,[role="toolbar"],.file-card'))return
      clearSelection()
    }
    function download(item:DriveFile){
      const link=document.createElement('a');link.href=`/api/files/${item.id}/download`;link.download=item.name;link.hidden=true;document.body.appendChild(link);link.click();link.remove()
    }
    function downloadSelected(){
      const files=[...selectedFiles.value]
      files.forEach((item,index)=>window.setTimeout(()=>download(item),index*180))
    }
    
    async function extractArchive(item:DriveFile){
      if(!isArchive(item))return
      const confirmed=await confirmDialog({title:'在线解压',message:`将“${item.name}”解压到当前目录中的新文件夹。大压缩包会在后台继续处理。`,confirmLabel:'开始解压'})
      if(!confirmed)return
      try{
        await api(`/api/files/${item.id}/extract`,{method:'POST'})
        void refreshBackgroundTasks()
        clearSelection();notify(`「${item.name}」已加入解压队列`,'success')
      }catch(e){notify((e as Error).message)}
    }
    onMounted(()=>{window.addEventListener('popstate',handlePopState);checkSession().then(()=>{if(user.value){jobEvents.connect();void refreshJobsFromEvent()}})})
    onBeforeUnmount(()=>{window.removeEventListener('popstate',handlePopState);disposeUploads()})
    
    
    return {filesChanged,folderChanged,localMergeChanged,audioCoverChanged,avatarChanged,blurEventTarget,isAudio,isVideo,thumbSRC,formatSize,LoginPage,AppDialog,AppTopbar,DocumentEditor,FileBrowserHeader,FileGrid,MoveCopyDialog,SelectionToolbar,ShareDialog,askDialog,confirmDialog,promptDialog,finishDialog,notify,openReader,checkSession,openRoute,openDeepLink,submitLogin,logout,folderURL,openFolder,handlePopState,openModal,closeModal,goUp,openTrash,createFolder,removeSelected,restoreSelected,purgeSelected,emptyTrash,showRename,saveRename,showMove,showMoveSelected,showMoveTargets,showCopy,transferTo,showPreview,showShare,createShare,revokeShare,copyShare,openItem,newDocument,openEditor,saveDocument,closeEditor,closeBackdrop,toggleSelection,clearSelection,selectAll,clearSelectionFromBlank,download,downloadSelected,extractArchive,ROOT,user,hasAvatar,avatarVersion,checking,login,currentId,current,items,breadcrumbs,loading,dragActive,toast,tasks,trashMode,selected,selectedIds,moveTargets,transferMode,modal,readerFile,renameValue,modalBusy,share,editor,directoryStats,fileInput,folderInput,localMergeInput,avatarInput,audioCoverInput,dialog,MediaPreview,ReaderView,editorDirty,editorBytes,editorIsMarkdown,renderedMarkdown,selectedItems,selectedBytes,selectedFiles,selectedAudioFiles,canMergeSelectedAudio,singleSelected,navActions,account,accountPanel,usernameEditing,usernameSaving,usernameError,usernameInput,avatar,twoFactor,avatarURL,showAccount,startUsernameEdit,cancelUsernameEdit,saveUsername,openAccountPanel,closeAccountPanel,chooseAvatar,uploadAvatar,removeAvatar,savePassword,beginTwoFactorSetup,cancelTwoFactorSetup,enableTwoFactor,regenerateRecoveryCodes,disableTwoFactor,copyRecoveryCodes,downloadRecoveryCodes,audioMerge,localMerge,localUploads,audioCoverCandidates,audioMergeSubtitleCount,audioMergeExtension,audioSubtitleFor,showAudioMerge,chooseAudioCover,clearAudioCover,selectDirectoryCover,setAudioCover,setAudioMergeFormat,moveAudioMergeInput,startAudioMerge,showLocalAudioMerge,chooseLocalMergeDir,localSubtitleFor,onLocalMergeDir,selectLocalCoverFile,clearLocalCover,moveLocalMergeInput,setAudioMergeSource,startLocalMerge,cancelLocalMergeRemote,closeAudioMergeModal,localFileByName,localCoverCandidates,chooseFiles,chooseFolder,acceptFiles,acceptFolder,onDrop,cancelUpload,retry,scheduleAutoClear,disposeUploads,backgroundTasks,refreshJobsFromEvent,refreshBackgroundTasks,cancelBackgroundTask,retryBackgroundTask,jobEvents}
  }
})

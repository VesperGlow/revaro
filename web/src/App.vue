<script setup lang="ts">
import DOMPurify from 'dompurify'
import { marked } from 'marked'
import { computed, nextTick, onBeforeUnmount, onMounted, reactive, ref } from 'vue'
import { api } from './api'
import type { ApiError, DriveFile } from './api'
import AppDialog from './components/AppDialog.vue'
import AppTopbar from './components/AppTopbar.vue'
import FileBrowserHeader from './components/FileBrowserHeader.vue'
import FileGrid from './components/FileGrid.vue'
import FileTable from './components/FileTable.vue'
import MediaPreview from './components/MediaPreview.vue'
import { isArchive, isAudio, isBook, isEditable, isImage, isMedia, isVideo, thumbSRC } from './fileTypes'
import { formatDate, formatSize } from './format'
import ReaderView from './Reader.vue'
import type { ArchiveJob, AudioMergeFormat, AudioMergeResponse, DownloadJob, FolderOption, ProfileResponse, ShareResponse, StorageStats, TOTPRecoveryResponse, TOTPSetupResponse, TOTPStatusResponse, UploadTask } from './types'

const ROOT = '00000000-0000-0000-0000-000000000000'
const FILE_CONCURRENCY = 3
const MULTIPART_CONCURRENCY = 4
const PART_URL_BATCH = 100

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
const folders = ref<FolderOption[]>([])
const modalBusy = ref(false)
const account = reactive({ username:'', currentPassword:'', password:'', confirmPassword:'', error:'' })
const accountPanel = ref<null|'password'|'totp'>(null)
const usernameEditing = ref(false)
const usernameSaving = ref(false)
const usernameError = ref('')
const usernameInput = ref<HTMLInputElement|null>(null)
const avatar = reactive({ busy:false, error:'' })
const twoFactor = reactive({ enabled:false, recoveryRemaining:0, loading:false, busy:false, stage:'idle' as 'idle'|'setup', currentPassword:'', code:'', secret:'', uri:'', qrDataURL:'', recoveryCodes:[] as string[], copied:false, error:'' })
const share = reactive({ active:false, url:'', createdAt:'', busy:false, error:'', copied:false })
const editor = reactive({ isNew:false, readonly:false, fileId:'', name:'', originalName:'', content:'', original:'', etag:'', mode:'edit' as 'edit'|'split'|'preview', busy:false, error:'' })
const audioMerge = reactive({ name:'', format:'flac' as AudioMergeFormat, order:[] as DriveFile[], coverData:'', coverFileId:'', coverPreview:'', coverName:'', error:'', busy:false })
const audioMergeJobs = ref<AudioMergeResponse[]>([])
const downloadJobs = ref<DownloadJob[]>([])
const archiveJobs = ref<ArchiveJob[]>([])
const directoryStats = reactive<StorageStats>({ total_bytes:0, file_count:0 })
const fileInput = ref<HTMLInputElement|null>(null)
const folderInput = ref<HTMLInputElement|null>(null)
const avatarInput = ref<HTMLInputElement|null>(null)
const audioCoverInput = ref<HTMLInputElement|null>(null)
const viewMode = ref<'list'|'grid'>('list')
const dialog = reactive({open:false,title:'',message:'',confirmLabel:'确定',cancelLabel:'取消',tone:'default' as 'default'|'danger',input:false,value:'',placeholder:''})
let dialogResolve:((value:string|boolean|null)=>void)|null=null
let activeUploads = 0
let toastTimer = 0
let audioMergePollTimer = 0
let downloadPollTimer = 0
let archivePollTimer = 0
let uploadRefreshTimer = 0

const editorDirty = computed(() => editor.content !== editor.original || editor.name !== editor.originalName)
const editorBytes = computed(() => new Blob([editor.content]).size)
const editorIsMarkdown = computed(() => /\.(md|markdown)$/i.test(editor.name))
const renderedMarkdown = computed(() => DOMPurify.sanitize(marked.parse(editor.content, { async:false }) as string))
const avatarURL = computed(() => `/api/profile/avatar?v=${avatarVersion.value}`)
const selectedItems = computed(() => items.value.filter(item => selectedIds.value.has(item.id)))
const selectedBytes = computed(() => selectedItems.value.reduce((total,item) => total+(item.kind==='file'?item.size:0),0))
const selectedFiles = computed(() => selectedItems.value.filter(item => item.kind==='file'))
const selectedAudioFiles = computed(() => selectedItems.value.filter(isAudio))
const canMergeSelectedAudio = computed(() => selectedItems.value.length>=2&&selectedItems.value.length===selectedAudioFiles.value.length)
const audioCoverCandidates = computed(() => items.value.filter(item=>isImage(item)&&item.size<=16*1024*1024&&!(/\.avif$/i.test(item.name)||item.mime_type==='image/avif')))
const audioMergeSubtitleCount = computed(() => audioMerge.order.filter(item=>audioSubtitleFor(item)).length)
const singleSelected = computed(() => selectedItems.value.length===1?selectedItems.value[0]:null)

function askDialog(options:{title:string;message:string;confirmLabel?:string;cancelLabel?:string;tone?:'default'|'danger';input?:boolean;value?:string;placeholder?:string}){
  dialog.title=options.title;dialog.message=options.message;dialog.confirmLabel=options.confirmLabel||'确定';dialog.cancelLabel=options.cancelLabel||'取消';dialog.tone=options.tone||'default';dialog.input=!!options.input;dialog.value=options.value||'';dialog.placeholder=options.placeholder||'';dialog.open=true
  return new Promise<string|boolean|null>(resolve=>{dialogResolve=resolve})
}
async function confirmDialog(options:{title:string;message:string;confirmLabel?:string;tone?:'default'|'danger'}){return await askDialog(options)===true}
async function promptDialog(options:{title:string;message:string;value?:string;placeholder?:string;confirmLabel?:string}){const result=await askDialog({...options,input:true});return typeof result==='string'?result:null}
function finishDialog(confirmed:boolean){const resolve=dialogResolve;dialogResolve=null;dialog.open=false;if(!resolve)return;resolve(confirmed?(dialog.input?dialog.value.trim():true):null)}

function notify(text:string, kind:'error'|'success'='error') { toast.text=text;toast.kind=kind;window.clearTimeout(toastTimer);toastTimer=window.setTimeout(()=>toast.text='',3600) }

function openReader(item:DriveFile){readerFile.value=item;openModal('reader');history.replaceState({revaroNav:true},'','/read/'+item.id)}
async function checkSession() {
  try { const me=await api<ProfileResponse>('/api/auth/me');user.value=me.username;hasAvatar.value=me.has_avatar;await openRoute() }
  catch { user.value=null;hasAvatar.value=false }
  finally { checking.value=false }
}
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
async function submitLogin() {
  login.busy=true;login.error='';login.notice=''
  try {
    const me=await api<ProfileResponse>('/api/auth/login',{method:'POST',body:JSON.stringify({username:login.username,password:login.password,second_factor:login.secondFactor})})
    user.value=me.username;hasAvatar.value=me.has_avatar;login.password='';login.secondFactor='';login.totpRequired=false;await openFolder(ROOT);void refreshAudioMergeJobs();void refreshDownloadJobs();void refreshArchiveJobs()
  }catch(e){
    const code=((e as ApiError).data as {error?:{code?:string}}|null)?.error?.code
    if(code==='totp_required'){login.totpRequired=true;login.error='请输入身份验证器验证码或恢复码'}
    else if(code==='invalid_second_factor'){login.totpRequired=true;login.error='验证码或恢复码不正确'}
    else login.error=(e as Error).message
  }
  finally{login.busy=false}
}
async function logout(){await api('/api/auth/logout',{method:'POST'});user.value=null;hasAvatar.value=false;items.value=[];tasks.splice(0);audioMergeJobs.value=[];downloadJobs.value=[];archiveJobs.value=[];window.clearTimeout(audioMergePollTimer);window.clearTimeout(downloadPollTimer);window.clearTimeout(archivePollTimer)}
function showAccount(){account.username=user.value||'';account.currentPassword='';account.password='';account.confirmPassword='';account.error='';accountPanel.value=null;usernameEditing.value=false;usernameSaving.value=false;usernameError.value='';avatar.error='';resetTwoFactor();openModal('account');loadTwoFactorStatus()}
async function startUsernameEdit(){
  if(usernameSaving.value)return
  account.username=user.value||'';usernameError.value='';usernameEditing.value=true
  await nextTick();usernameInput.value?.focus();usernameInput.value?.select()
}
function cancelUsernameEdit(){account.username=user.value||'';usernameError.value='';usernameEditing.value=false}
async function saveUsername(){
  if(!usernameEditing.value||usernameSaving.value)return
  const username=account.username.trim()
  if(!username){usernameError.value='用户名不能为空';await nextTick();usernameInput.value?.focus();return}
  if(username===user.value){account.username=username;usernameEditing.value=false;usernameError.value='';return}
  usernameSaving.value=true;usernameError.value=''
  let failed=false
  try{
    await api('/api/profile/username',{method:'PATCH',body:JSON.stringify({username})})
    account.username=username;user.value=username;login.username=username;usernameEditing.value=false;notify('用户名已保存','success')
  }catch(e){usernameError.value=(e as Error).message;failed=true}
  finally{usernameSaving.value=false}
  if(failed){await nextTick();usernameInput.value?.focus()}
}
function openAccountPanel(panel:'password'|'totp'){
  accountPanel.value=panel
  if(panel==='password'){account.currentPassword='';account.password='';account.confirmPassword='';account.error=''}
  else{twoFactor.currentPassword='';twoFactor.code='';twoFactor.error='';twoFactor.recoveryCodes=[];twoFactor.copied=false;twoFactor.stage='idle'}
}
function closeAccountPanel(){
  if(accountPanel.value==='totp'){cancelTwoFactorSetup();twoFactor.currentPassword='';twoFactor.recoveryCodes=[];twoFactor.copied=false}
  account.currentPassword='';account.password='';account.confirmPassword='';account.error='';accountPanel.value=null
}
function chooseAvatar(){avatarInput.value?.click()}
async function uploadAvatar(file:File){
  avatar.error=''
  if(!['image/jpeg','image/png','image/gif','image/webp'].includes(file.type)){avatar.error='请选择 JPG、PNG、GIF 或 WebP 图片';return}
  if(file.size>2*1024*1024){avatar.error='头像不能超过 2 MiB';return}
  avatar.busy=true
  try{
    const dataURL=await new Promise<string>((resolve,reject)=>{const reader=new FileReader();reader.onload=()=>resolve(String(reader.result));reader.onerror=()=>reject(new Error('无法读取图片'));reader.readAsDataURL(file)})
    await api('/api/profile/avatar',{method:'PUT',body:JSON.stringify({data_url:dataURL})})
    hasAvatar.value=true;avatarVersion.value=Date.now();notify('头像已更新','success')
  }catch(e){avatar.error=(e as Error).message}
  finally{avatar.busy=false}
}
async function removeAvatar(){
  avatar.error='';avatar.busy=true
  try{await api('/api/profile/avatar',{method:'DELETE'});hasAvatar.value=false;avatarVersion.value=Date.now();notify('头像已移除','success')}
  catch(e){avatar.error=(e as Error).message}
  finally{avatar.busy=false}
}
async function savePassword(){
  account.error=''
  if(account.password.length<12){account.error='新密码至少需要 12 个字符';return}
  if(account.password!==account.confirmPassword){account.error='两次输入的新密码不一致';return}
  modalBusy.value=true
  try{
    await api('/api/auth/password',{method:'PATCH',body:JSON.stringify({current_password:account.currentPassword,password:account.password})})
    accountPanel.value=null;closeModal();login.username=account.username;login.password='';login.notice='密码已更新，请重新登录';user.value=null;items.value=[];tasks.splice(0)
  }catch(e){account.error=(e as Error).message}
  finally{modalBusy.value=false}
}

function resetTwoFactor(){twoFactor.enabled=false;twoFactor.recoveryRemaining=0;twoFactor.loading=false;twoFactor.busy=false;twoFactor.stage='idle';twoFactor.currentPassword='';twoFactor.code='';twoFactor.secret='';twoFactor.uri='';twoFactor.qrDataURL='';twoFactor.recoveryCodes=[];twoFactor.copied=false;twoFactor.error=''}
async function loadTwoFactorStatus(){
  twoFactor.loading=true;twoFactor.error=''
  try{const status=await api<TOTPStatusResponse>('/api/auth/totp');twoFactor.enabled=status.enabled;twoFactor.recoveryRemaining=status.recovery_codes}
  catch(e){twoFactor.error=(e as Error).message}
  finally{twoFactor.loading=false}
}
async function beginTwoFactorSetup(){
  twoFactor.error=''
  if(!twoFactor.currentPassword){twoFactor.error='请输入当前密码';return}
  twoFactor.busy=true
  try{
    const setup=await api<TOTPSetupResponse>('/api/auth/totp/setup',{method:'POST',body:JSON.stringify({current_password:twoFactor.currentPassword})})
    twoFactor.secret=setup.secret;twoFactor.uri=setup.uri;twoFactor.qrDataURL=setup.qr_data_url;twoFactor.code='';twoFactor.stage='setup'
  }catch(e){twoFactor.error=(e as Error).message}
  finally{twoFactor.busy=false}
}
function cancelTwoFactorSetup(){twoFactor.stage='idle';twoFactor.code='';twoFactor.secret='';twoFactor.uri='';twoFactor.qrDataURL='';twoFactor.error=''}
async function enableTwoFactor(){
  twoFactor.error=''
  if(!twoFactor.code.trim()){twoFactor.error='请输入身份验证器中的六位验证码';return}
  twoFactor.busy=true
  try{
    const result=await api<TOTPRecoveryResponse>('/api/auth/totp/enable',{method:'POST',body:JSON.stringify({current_password:twoFactor.currentPassword,code:twoFactor.code})})
    twoFactor.enabled=true;twoFactor.recoveryRemaining=result.recovery_codes.length;twoFactor.recoveryCodes=result.recovery_codes;twoFactor.stage='idle';twoFactor.currentPassword='';twoFactor.code='';twoFactor.secret='';twoFactor.uri='';twoFactor.qrDataURL='';notify('两步验证已启用','success')
  }catch(e){twoFactor.error=(e as Error).message}
  finally{twoFactor.busy=false}
}
async function regenerateRecoveryCodes(){
  twoFactor.error=''
  if(!twoFactor.currentPassword||!twoFactor.code.trim()){twoFactor.error='请输入当前密码和验证码或恢复码';return}
  if(!await confirmDialog({title:'重新生成恢复码？',message:'现有恢复码会立即全部失效，请保存新生成的恢复码。',confirmLabel:'重新生成'}))return
  twoFactor.busy=true
  try{
    const result=await api<TOTPRecoveryResponse>('/api/auth/totp/recovery-codes',{method:'POST',body:JSON.stringify({current_password:twoFactor.currentPassword,code:twoFactor.code})})
    twoFactor.recoveryCodes=result.recovery_codes;twoFactor.recoveryRemaining=result.recovery_codes.length;twoFactor.currentPassword='';twoFactor.code='';twoFactor.copied=false;notify('恢复码已重新生成','success')
  }catch(e){twoFactor.error=(e as Error).message}
  finally{twoFactor.busy=false}
}
async function disableTwoFactor(){
  twoFactor.error=''
  if(!twoFactor.currentPassword||!twoFactor.code.trim()){twoFactor.error='请输入当前密码和验证码或恢复码';return}
  if(!await confirmDialog({title:'关闭两步验证？',message:'关闭后，只凭管理员密码即可登录。现有恢复码也会全部失效。',confirmLabel:'关闭验证',tone:'danger'}))return
  twoFactor.busy=true
  try{
    await api('/api/auth/totp',{method:'DELETE',body:JSON.stringify({current_password:twoFactor.currentPassword,code:twoFactor.code})})
    twoFactor.enabled=false;twoFactor.recoveryRemaining=0;twoFactor.currentPassword='';twoFactor.code='';twoFactor.recoveryCodes=[];notify('两步验证已关闭','success')
  }catch(e){twoFactor.error=(e as Error).message}
  finally{twoFactor.busy=false}
}
async function copyRecoveryCodes(){
  try{await navigator.clipboard.writeText(twoFactor.recoveryCodes.join('\n'));twoFactor.copied=true;notify('恢复码已复制','success')}
  catch{twoFactor.error='复制失败，请手动保存恢复码'}
}
function downloadRecoveryCodes(){
  const blob=new Blob([`revaro 恢复码\n生成时间：${new Date().toLocaleString()}\n\n${twoFactor.recoveryCodes.join('\n')}\n`],{type:'text/plain;charset=utf-8'})
  const url=URL.createObjectURL(blob)
  const link=document.createElement('a');link.href=url;link.download='revaro-recovery-codes.txt';link.click()
  window.setTimeout(()=>URL.revokeObjectURL(url),0)
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
async function removeItem(item:DriveFile){if(!await confirmDialog({title:'移入回收站？',message:`「${item.name}」${item.kind==='directory'?'及其中的全部内容':''}会移入回收站，之后仍可恢复。`,confirmLabel:'移入回收站',tone:'danger'}))return;try{await api(`/api/files/${item.id}`,{method:'DELETE'});await openFolder(currentId.value);notify('已移入回收站','success')}catch(e){notify((e as Error).message)}}
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
async function restoreItem(item:DriveFile){try{await api(`/api/trash/${item.id}/restore`,{method:'POST'});await openTrash();notify('已恢复到原位置','success')}catch(e){notify((e as Error).message)}}
async function restoreSelected(){for(const item of [...selectedItems.value]){try{await api(`/api/trash/${item.id}/restore`,{method:'POST'})}catch(e){notify(`${item.name}：${(e as Error).message}`);return}}await openTrash();notify('所选项目已恢复','success')}
async function purgeItem(item:DriveFile){if(!await confirmDialog({title:'永久删除？',message:`「${item.name}」${item.kind==='directory'?'及其中的全部内容':''}将无法恢复，对应的无引用数据块会在垃圾回收后清理。`,confirmLabel:'永久删除',tone:'danger'}))return;try{await api(`/api/trash/${item.id}`,{method:'DELETE'});await openTrash();notify('已永久删除','success')}catch(e){notify((e as Error).message)}}
async function purgeSelected(){const targets=[...selectedItems.value];if(!targets.length||!await confirmDialog({title:`永久删除 ${targets.length} 项？`,message:'这个操作无法撤销。无引用的数据块会在垃圾回收后清理。',confirmLabel:'永久删除',tone:'danger'}))return;for(const item of targets){try{await api(`/api/trash/${item.id}`,{method:'DELETE'})}catch(e){notify(`${item.name}：${(e as Error).message}`);return}}await openTrash();notify('已永久删除所选项目','success')}
async function emptyTrash(){if(!items.value.length||!await confirmDialog({title:'清空回收站？',message:`回收站中的 ${items.value.length} 项及其内容都会永久删除，无法恢复。`,confirmLabel:'清空回收站',tone:'danger'}))return;try{await api('/api/trash',{method:'DELETE'});await openTrash();notify('回收站已清空','success')}catch(e){notify((e as Error).message)}}
function showRename(item:DriveFile){selected.value=item;renameValue.value=item.name;openModal('rename')}
async function saveRename(){if(!selected.value)return;modalBusy.value=true;try{await api(`/api/files/${selected.value.id}`,{method:'PATCH',body:JSON.stringify({name:renameValue.value})});closeModal();await openFolder(currentId.value);notify('已重命名','success')}catch(e){notify((e as Error).message)}finally{modalBusy.value=false}}
async function showMove(item:DriveFile){await showMoveTargets([item])}
async function showMoveSelected(){await showMoveTargets([...selectedItems.value])}
async function showMoveTargets(targets:DriveFile[]){
  if(!targets.length)return
  transferMode.value='move'
  moveTargets.value=targets;selected.value=targets[0];modalBusy.value=true;openModal('move');folders.value=[]
  try{folders.value=await loadFolderTree(new Set(targets.map(item=>item.id)))}catch(e){notify((e as Error).message);closeModal()}finally{modalBusy.value=false}
}
async function showCopy(item:DriveFile){
  transferMode.value='copy';moveTargets.value=[item];selected.value=item;modalBusy.value=true;openModal('move');folders.value=[]
  try{folders.value=await loadFolderTree()}catch(e){notify((e as Error).message);closeModal()}finally{modalBusy.value=false}
}
async function loadFolderTree(excludedIds=new Set<string>()):Promise<FolderOption[]>{
  const result:FolderOption[]=[{id:ROOT,name:'我的文件',depth:0}]
  const queue=[{id:ROOT,depth:0}]
  // 受限并发 BFS：同层目录并行拉取，避免大目录树逐目录串行请求
  while(queue.length){
    const batch=queue.splice(0,8)
    const lists=await Promise.all(batch.map(async node=>({node,data:await api<{items:DriveFile[]}>(`/api/files/${node.id}/children`)})))
    for(const {node,data} of lists){
      for(const child of data.items.filter(x=>x.kind==='directory'&&!excludedIds.has(x.id))){
        result.push({id:child.id,name:child.name,depth:node.depth+1});queue.push({id:child.id,depth:node.depth+1})
      }
    }
  }
  return result
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
function closeBackdrop(){if(modal.value==='editor')void closeEditor();else closeModal()}
function setViewMode(mode:'list'|'grid'){viewMode.value=mode;localStorage.setItem('revaro-view-mode',mode)}
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
  if(!(target instanceof Element)||target.closest('button,a,input,textarea,select,[role="toolbar"],.file-card,.file-row'))return
  clearSelection()
}
function download(item:DriveFile){
  const link=document.createElement('a');link.href=`/api/files/${item.id}/download`;link.download=item.name;link.hidden=true;document.body.appendChild(link);link.click();link.remove()
}
function downloadSelected(){
  const files=[...selectedFiles.value]
  files.forEach((item,index)=>window.setTimeout(()=>download(item),index*180))
}

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
    audioMergeJobs.value=[data,...audioMergeJobs.value.filter(job=>job.id!==data.id)];clearSelection();closeModal();notify(`「${data.output_name}」已加入后台队列`,'success');scheduleAudioMergePoll(350)
  }catch(e){audioMerge.error=(e as Error).message}
  finally{audioMerge.busy=false}
}
function scheduleAudioMergePoll(delay=1000){
  window.clearTimeout(audioMergePollTimer)
  if(audioMergeJobs.value.some(job=>!audioMergeTerminal(job.status)))audioMergePollTimer=window.setTimeout(()=>void refreshAudioMergeJobs(),delay)
}
function audioMergeTerminal(status:AudioMergeResponse['status']){return status==='done'||status==='failed'||status==='cancelled'}
async function refreshAudioMergeJobs(){
  try{
    const previous=new Map(audioMergeJobs.value.map(job=>[job.id,job.status]))
    const data=await api<{items:AudioMergeResponse[]}>('/api/audio-merges')
    audioMergeJobs.value=data.items
    for(const job of data.items){
      const before=previous.get(job.id)
      if(before&&!audioMergeTerminal(before)&&audioMergeTerminal(job.status)){
        if(job.status==='done')notify(`「${job.output_name}」合并完成`,'success')
        else if(job.status==='failed')notify(job.error||`「${job.output_name}」合并失败`)
      }
    }
    scheduleAudioMergePoll()
  }catch{scheduleAudioMergePoll(3000)}
}
async function cancelAudioMerge(job:AudioMergeResponse){
  if(audioMergeTerminal(job.status))return
  try{await api(`/api/audio-merges/${job.id}`,{method:'DELETE'});job.status='cancelling';job.message='正在取消合并';scheduleAudioMergePoll(300)}catch(e){notify((e as Error).message)}
}
async function clearAudioMerges(){
  const finished=audioMergeJobs.value.filter(job=>audioMergeTerminal(job.status))
  await Promise.all(finished.map(job=>api(`/api/audio-merges/${job.id}`,{method:'DELETE'}).catch(()=>undefined)))
  audioMergeJobs.value=audioMergeJobs.value.filter(job=>!audioMergeTerminal(job.status))
}

function downloadTerminal(status:DownloadJob['status']){return status==='done'||status==='failed'||status==='cancelled'}
function scheduleDownloadPoll(delay=1000){
  window.clearTimeout(downloadPollTimer)
  if(downloadJobs.value.some(job=>!downloadTerminal(job.status)&&job.status!=='waiting'&&job.status!=='paused'))downloadPollTimer=window.setTimeout(()=>void refreshDownloadJobs(),delay)
}
async function refreshDownloadJobs(){
  try{
    const previous=new Map(downloadJobs.value.map(job=>[job.id,job.status]))
    const data=await api<{items:DownloadJob[]}>('/api/downloads')
    downloadJobs.value=data.items.filter(job=>job.status!=='done')
    let refreshFolder=false
    for(const job of data.items){
      const before=previous.get(job.id)
      if(before&&before!=='done'&&job.status==='done'){
        notify(`「${job.name}」已下载到网盘`,'success')
        if(job.parent_id===currentId.value)refreshFolder=true
      }else if(before&&!downloadTerminal(before)&&job.status==='failed')notify(job.error||`「${job.name}」下载失败`)
      if(job.status==='done')void api(`/api/downloads/${job.id}`,{method:'DELETE'},30*60*1000).catch(()=>{})
    }
    if(refreshFolder)void openFolder(currentId.value)
    scheduleDownloadPoll()
  }catch{scheduleDownloadPoll(3000)}
}
function downloadsChanged(){void refreshDownloadJobs()}

async function extractArchive(item:DriveFile){
  if(!isArchive(item))return
  const confirmed=await confirmDialog({title:'在线解压',message:`将“${item.name}”解压到当前目录中的新文件夹。大压缩包会在后台继续处理。`,confirmLabel:'开始解压'})
  if(!confirmed)return
  try{
    const job=await api<ArchiveJob>(`/api/files/${item.id}/extract`,{method:'POST'})
    archiveJobs.value=[job,...archiveJobs.value.filter(existing=>existing.id!==job.id)]
    clearSelection();notify(`「${item.name}」已加入解压队列`,'success');scheduleArchivePoll(500)
  }catch(e){notify((e as Error).message)}
}
function archiveTerminal(status:ArchiveJob['status']){return status==='done'||status==='failed'}
function archivePolling(status:ArchiveJob['status']){return !archiveTerminal(status)&&status!=='waiting_password'}
function scheduleArchivePoll(delay=1200){
  window.clearTimeout(archivePollTimer)
  if(archiveJobs.value.some(job=>archivePolling(job.status)))archivePollTimer=window.setTimeout(()=>void refreshArchiveJobs(),delay)
}
async function refreshArchiveJobs(){
  try{
    const previous=new Map(archiveJobs.value.map(job=>[job.id,job.status]))
    const data=await api<{items:ArchiveJob[]}>('/api/archive-jobs')
    archiveJobs.value=data.items
    let refreshFolder=false
    for(const job of data.items){
      const before=previous.get(job.id)
      if(before&&!archiveTerminal(before)&&job.status==='done'){
        notify(`「${job.output_name||job.name}」解压完成`,'success')
        if(job.parent_id===currentId.value)refreshFolder=true
      }else if(before&&!archiveTerminal(before)&&job.status==='failed')notify(job.error||`「${job.name}」解压失败`)
      else if(before&&before!=='waiting_password'&&job.status==='waiting_password')notify(job.error||`「${job.name}」需要解压密码`)
    }
    if(refreshFolder)void openFolder(currentId.value)
    scheduleArchivePoll()
  }catch{window.clearTimeout(archivePollTimer);archivePollTimer=window.setTimeout(()=>void refreshArchiveJobs(),3000)}
}
async function submitArchivePassword(job:ArchiveJob,password:string){
  try{
    const updated=await api<ArchiveJob>(`/api/archive-jobs/${job.id}/password`,{method:'POST',body:JSON.stringify({password})})
    archiveJobs.value=archiveJobs.value.map(existing=>existing.id===updated.id?updated:existing)
    notify(`正在验证「${job.name}」的解压密码`,'success');scheduleArchivePoll(500)
  }catch(e){notify((e as Error).message)}
}
async function clearArchiveJobs(){
  const finished=archiveJobs.value.filter(job=>archiveTerminal(job.status))
  await Promise.all(finished.map(job=>api(`/api/archive-jobs/${job.id}`,{method:'DELETE'}).catch(()=>undefined)))
  archiveJobs.value=archiveJobs.value.filter(job=>!archiveTerminal(job.status))
}

function chooseFiles(){fileInput.value?.click()}
function chooseFolder(){folderInput.value?.click()}
function queueFiles(files:File[],parentId:string,relativePaths?:Map<File,string>){
  for(const file of files)tasks.push({id:crypto.randomUUID(),file,parentId,relativePath:relativePaths?.get(file),progress:0,status:'queued',error:'',cancelled:false,requests:[]})
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
interface CreatedUpload { upload_id:string; mode:'single'|'multipart'; url?:string; part_size:number; part_count:number }
interface CompletedPart { part_number:number; etag:string }

async function runUpload(task:UploadTask){
  task.status='uploading';task.error='';task.cancelled=false;task.progress=0
  try{
    const created=await api<CreatedUpload>('/api/uploads',{method:'POST',body:JSON.stringify({parent_id:task.parentId,name:task.file.name,size:task.file.size,mime_type:task.file.type||'application/octet-stream'})})
    task.uploadId=created.upload_id
    if(task.cancelled){await abortRemote(task);return}
    let parts:CompletedPart[]=[]
    if(created.mode==='single'){
      if(!created.url)throw new Error('服务端没有返回上传地址')
      await xhrPut(created.url,task.file,task,loaded=>{task.progress=Math.floor(percentage(loaded,task.file.size)*.98)},task.file.type||'application/octet-stream')
    }else{
      parts=await uploadMultipart(task,created)
    }
    if(task.cancelled){await abortRemote(task);return}
    await api(`/api/uploads/${created.upload_id}/complete`,{method:'POST',body:JSON.stringify({parts})},120000)
    task.progress=100;task.status='done';scheduleUploadRefresh();scheduleAutoClear()
  }catch(e){if(task.cancelled){task.status='cancelled';scheduleAutoClear()}else{task.status='failed';task.error=(e as Error).message}}
}

async function uploadMultipart(task:UploadTask,created:CreatedUpload):Promise<CompletedPart[]>{
  const numbers=Array.from({length:created.part_count},(_,index)=>index+1)
  const sent=new Array(numbers.length).fill(0) as number[]
  const completed=new Array<CompletedPart>(numbers.length)
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
        const etag=await xhrPut(part.url,blob,task,loaded=>{sent[idx]=loaded;task.progress=Math.floor(percentage(sent.reduce((a,x)=>a+x,0),task.file.size)*.98)})
        if(!etag)throw new Error('对象存储没有暴露 ETag，请检查 Bucket CORS 的 ExposeHeaders')
        completed[idx]={part_number:part.part_number,etag}
      }
    }
    await Promise.all(Array.from({length:Math.min(MULTIPART_CONCURRENCY,data.parts.length)},worker))
  }
  return completed
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
async function cancelUpload(task:UploadTask){task.cancelled=true;task.requests.forEach(x=>x.abort());await abortRemote(task);task.status='cancelled'}
async function abortRemote(task:UploadTask){if(task.uploadId){try{await api(`/api/uploads/${task.uploadId}`,{method:'DELETE'})}catch{/* stale cleanup retries later */}}}
async function retry(task:UploadTask){await abortRemote(task);task.status='queued';task.error='';task.uploadId=undefined;task.requests=[];task.cancelled=false;pumpQueue()}
function clearFinished(){for(let i=tasks.length-1;i>=0;i--)if(['done','cancelled'].includes(tasks[i].status))tasks.splice(i,1)}
// 完成记录保留在上传进度中，等用户主动清除。
function scheduleAutoClear(){}
function scheduleUploadRefresh(){window.clearTimeout(uploadRefreshTimer);uploadRefreshTimer=window.setTimeout(()=>void openFolder(currentId.value),250)}
onMounted(()=>{const saved=localStorage.getItem('revaro-view-mode');if(saved==='list'||saved==='grid')viewMode.value=saved;window.addEventListener('popstate',handlePopState);checkSession().then(()=>{if(user.value){void refreshAudioMergeJobs();void refreshDownloadJobs();void refreshArchiveJobs()}})})
onBeforeUnmount(()=>{window.removeEventListener('popstate',handlePopState);window.clearTimeout(audioMergePollTimer);window.clearTimeout(downloadPollTimer);window.clearTimeout(archivePollTimer);window.clearTimeout(uploadRefreshTimer);tasks.forEach(task=>task.requests.forEach(request=>request.abort()))})
</script>

<template>
  <div v-if="checking" class="splash"><div class="brand-mark"><img class="ui-image" src="/logo.png" alt="" draggable="false"></div><div class="spinner"></div></div>
  <main v-else-if="!user" class="login-page">
    <section class="login-visual"><div class="glow glow-a"></div><div class="glow glow-b"></div><div class="visual-copy"><span class="eyebrow">PRIVATE · DIRECT · YOURS</span><h1>你的文件，<br>安静地待在云上。</h1><p>轻量、自托管，浏览器直连你的 S3。</p></div><div class="revaro-card"><span>☁</span><div><strong>不透明对象存储</strong><small>SQLite 元数据 · 原生 Range</small></div></div></section>
    <section class="login-panel">
      <form class="login-form" @submit.prevent="submitLogin">
        <div class="logo"><span class="brand-mark small"><img class="ui-image" src="/logo.png" alt="" draggable="false"></span><span>revaro</span></div>
        <div><p class="eyebrow dark">WELCOME BACK</p><h2>登录私人空间</h2><p class="muted">首次启动的随机凭据可在容器日志中查看</p></div>
        <label>用户名<input v-model="login.username" autocomplete="username" maxlength="128" required></label>
        <label>密码<input v-model="login.password" type="password" autocomplete="current-password" maxlength="1024" required></label>
        <label v-if="login.totpRequired">验证码或恢复码<input v-model="login.secondFactor" autocomplete="one-time-code" maxlength="128" placeholder="6 位验证码或恢复码" required><small class="login-totp-hint">打开身份验证器，或输入一枚尚未使用的恢复码。</small></label>
        <p v-if="login.notice" class="form-success">{{ login.notice }}</p><p v-if="login.error" class="form-error">{{ login.error }}</p>
        <button class="primary wide" :disabled="login.busy">{{ login.busy ? '正在验证…' : '进入我的网盘' }}</button>
      </form>
    </section>
  </main>

  <div v-else class="app-shell" @dragover.prevent="dragActive=true" @dragleave.self="dragActive=false" @drop.prevent="onDrop">
    <AppTopbar :user="user" :has-avatar="hasAvatar" :avatar-url="avatarURL" :uploads="tasks" :audio-merges="audioMergeJobs" :downloads="downloadJobs" :archive-jobs="archiveJobs" :download-parent-id="trashMode?ROOT:currentId" @home="openFolder(ROOT)" @trash="openTrash" @account="showAccount" @avatar-error="hasAvatar=false" @clear-uploads="clearFinished" @cancel-upload="cancelUpload" @retry-upload="retry" @cancel-audio-merge="cancelAudioMerge" @clear-audio-merges="clearAudioMerges" @clear-archive-jobs="clearArchiveJobs" @archive-password="submitArchivePassword" @downloads-changed="downloadsChanged" />
    <section class="content" @click="clearSelectionFromBlank">
      <FileBrowserHeader :breadcrumbs="breadcrumbs" :current="current" :can-go-up="currentId!==ROOT&&!!current?.parent_id" :item-count="items.length" :total-bytes="directoryStats.total_bytes" :file-count="directoryStats.file_count" :view-mode="viewMode" :trash-mode="trashMode" @open-folder="openFolder" @up="goUp" @set-view="setViewMode" @new-document="newDocument" @create-folder="createFolder" @upload-files="chooseFiles" @upload-folder="chooseFolder" @leave-trash="openFolder(ROOT)" @empty-trash="emptyTrash" />
      <input ref="fileInput" hidden type="file" multiple @change="e=>{const el=e.target as HTMLInputElement;if(el.files)acceptFiles(el.files);el.value=''}">
      <input ref="folderInput" hidden type="file" multiple webkitdirectory @change="e=>{const el=e.target as HTMLInputElement;if(el.files)acceptFolder(el.files);el.value=''}">
      <div v-if="selectedItems.length&&!modal" class="selection-toolbar" role="toolbar" aria-label="所选项目操作">
        <button class="selection-close" title="取消选择" aria-label="取消选择" @click="clearSelection">×</button><span class="selection-summary"><b>{{ selectedItems.length }} 项</b><small>已选择 {{ formatSize(selectedBytes) }}</small></span>
        <div v-if="trashMode" class="selection-actions"><button @click="restoreSelected"><svg viewBox="0 0 24 24" aria-hidden="true"><path d="M3 12a9 9 0 1 0 3-6.7L3 8"/><path d="M3 3v5h5"/></svg><span>恢复</span></button><button class="danger" @click="purgeSelected"><svg viewBox="0 0 24 24" aria-hidden="true"><path d="M4 7h16M9 7V4h6v3m3 0-1 13H7L6 7m4 4v5m4-5v5"/></svg><span>永久删除</span></button></div>
        <div v-else class="selection-actions">
          <button @click="selectAll"><svg viewBox="0 0 24 24" aria-hidden="true"><path d="M4 4h16v16H4z"/><path d="m8 12 3 3 5-6"/></svg><span>{{ selectedItems.length===items.length?'取消全选':'全选' }}</span></button>
          <button v-if="singleSelected&&(singleSelected.kind==='directory'||isEditable(singleSelected)||isMedia(singleSelected)||isBook(singleSelected))" @click="openItem(singleSelected)"><svg viewBox="0 0 24 24" aria-hidden="true"><path v-if="singleSelected.kind==='directory'" d="M3 7h7l2 2h9v9H3z"/><path v-else-if="isEditable(singleSelected)&&!isBook(singleSelected)" d="m4 16-.8 4 4-.8L18.5 7.9l-3.2-3.2L4 16Z"/><path v-else-if="isBook(singleSelected)" d="M12 5c-1.7-1.4-4.2-2-8-2v14c3.8 0 6.3.6 8 2 1.7-1.4 4.2-2 8-2V3c-3.8 0-6.3.6-8 2Zm0 0v14"/><path v-else-if="isImage(singleSelected)" d="M2.5 12s3.5-6 9.5-6 9.5 6 9.5 6-3.5 6-9.5 6-9.5-6-9.5-6Z"/><path v-else d="M8 5v14l11-7Z"/></svg><span>{{ singleSelected.kind==='directory'?'打开':isBook(singleSelected)?'阅读':isEditable(singleSelected)?'编辑文本':isImage(singleSelected)?'预览':'播放' }}</span></button>
          <button v-if="canMergeSelectedAudio" @click="showAudioMerge"><svg viewBox="0 0 24 24" aria-hidden="true"><path d="M4 9v6M8 6v12M12 3v18M16 7v10M20 10v4"/></svg><span>合并音频</span></button>
          <button v-if="singleSelected&&isArchive(singleSelected)" @click="extractArchive(singleSelected)"><svg viewBox="0 0 24 24" aria-hidden="true"><path d="M5 4h14v6H5zM5 14h14v6H5zM12 4v16M9 8h3m-3 4h3m-3 4h3"/></svg><span>在线解压</span></button>
          <button v-if="selectedFiles.length" @click="downloadSelected"><svg viewBox="0 0 24 24" aria-hidden="true"><path d="M12 3v12m0 0 4-4m-4 4-4-4M5 20h14"/></svg><span>下载{{ selectedFiles.length>1?` (${selectedFiles.length})`:'' }}</span></button>
          <button v-if="singleSelected?.kind==='file'" @click="showShare(singleSelected)"><svg viewBox="0 0 24 24" aria-hidden="true"><circle cx="18" cy="5" r="2.5"/><circle cx="6" cy="12" r="2.5"/><circle cx="18" cy="19" r="2.5"/><path d="m8.2 10.8 7.6-4.4M8.2 13.2l7.6 4.4"/></svg><span>分享</span></button>
          <button v-if="singleSelected" @click="showRename(singleSelected)"><svg viewBox="0 0 24 24" aria-hidden="true"><path d="M5 5h14M12 5v14M9 19h6"/></svg><span>重命名</span></button>
          <button @click="showMoveSelected"><svg viewBox="0 0 24 24" aria-hidden="true"><path d="M5 12h14m-5-5 5 5-5 5"/></svg><span>移动</span></button>
          <button class="danger" @click="removeSelected"><svg viewBox="0 0 24 24" aria-hidden="true"><path d="M4 7h16M9 7V4h6v3m3 0-1 13H7L6 7m4 4v5m4-5v5"/></svg><span>删除</span></button>
        </div>
      </div>
      <div v-if="loading" class="state"><div class="spinner"></div><p>正在读取文件…</p></div>
      <div v-else-if="!items.length" class="state empty"><div class="empty-icon">⌁</div><h3>{{ trashMode?'回收站是空的':'这里还是空的' }}</h3><p>{{ trashMode?'删除的项目会先来到这里。':'拖放文件到这里，或新建一篇文档。' }}</p><div v-if="!trashMode" class="empty-actions"><button class="secondary" @click="newDocument">新建文档</button><button class="primary" @click="chooseFiles">上传文件</button></div></div>
      <FileTable v-else-if="viewMode==='list'" :items="items" :selected-ids="selectedIds" :trash-mode="trashMode" @open="openItem" @select="toggleSelection" @select-all="selectAll" @edit="openEditor" @preview="showPreview" @read="openReader" @download="download" @extract="extractArchive" @share="showShare" @rename="showRename" @move="showMove" @remove="removeItem" @restore="restoreItem" @purge="purgeItem" />
      <FileGrid v-else :items="items" :selected-ids="selectedIds" :trash-mode="trashMode" @open="openItem" @select="toggleSelection" />
    </section>

    <div v-if="dragActive&&!trashMode" class="drop-zone"><div><span>↓</span><h2>释放以上传到 {{ current?.name || '我的文件' }}</h2><p>文件将通过 Presigned URL 直传 S3</p></div></div>

    <div v-if="modal" class="modal-backdrop" :class="{previewing:modal==='preview','audio-previewing':modal==='preview'&&!!selected&&isAudio(selected),'video-previewing':modal==='preview'&&!!selected&&isVideo(selected),editing:modal==='editor',reading:modal==='reader'}" @click.self="closeBackdrop">
      <section v-if="modal==='rename'" class="modal"><header><div><p class="eyebrow dark">EDIT</p><h2>重命名</h2></div><button @click="closeModal">×</button></header><label>新名称<input v-model="renameValue" maxlength="1024" @keyup.enter="saveRename"></label><footer><button class="secondary" @click="closeModal">取消</button><button class="primary" :disabled="modalBusy" @click="saveRename">保存</button></footer></section>
      <section v-else-if="modal==='move'" class="modal folder-modal"><header><div><p class="eyebrow dark">{{ transferMode==='copy'?'COPY':'MOVE' }}</p><h2>{{ transferMode==='copy'?'复制到':'移动到' }}</h2><p class="move-target" :title="moveTargets.length===1?moveTargets[0]?.name:undefined">{{ moveTargets.length===1?`「${moveTargets[0]?.name}」`:`${moveTargets.length} 项` }}</p></div><button @click="closeModal">×</button></header><div v-if="modalBusy" class="state small"><div class="spinner"></div></div><div v-else class="folder-list"><button v-for="folder in folders" :key="folder.id" :style="{paddingLeft:`${18+folder.depth*22}px`}" @click="transferTo(folder.id)"><span>▰</span>{{ folder.name }}</button></div></section>
      <section v-else-if="modal==='audioMerge'" class="modal audio-merge-modal">
        <header><div><p class="eyebrow dark">AUDIO MERGE</p><h2>合并音频</h2><p>FLAC / ALAC 真无损，或选择 AAC 节省空间</p></div><button aria-label="关闭" @click="closeModal">×</button></header>
        <div class="audio-merge-layout">
          <section class="merge-settings-panel">
            <fieldset class="merge-format-field">
              <legend>输出格式</legend><div class="merge-format-options">
                <button type="button" :class="{active:audioMerge.format==='flac'}" @click="setAudioMergeFormat('flac')"><span>FLAC</span><strong>无损 · 通用</strong><small>下载母版为 .flac</small></button>
                <button type="button" :class="{active:audioMerge.format==='alac'}" @click="setAudioMergeFormat('alac')"><span>ALAC</span><strong>无损 · Apple</strong><small>下载母版为 .m4a</small></button>
                <button type="button" :class="{active:audioMerge.format==='aac'}" @click="setAudioMergeFormat('aac')"><span>AAC</span><strong>有损 · 192k</strong><small>体积小，直接流播</small></button>
              </div>
            </fieldset>
            <label>输出文件名<input v-model="audioMerge.name" maxlength="1024" :placeholder="`合并音频${audioMergeExtension(audioMerge.format)}`" @keydown.enter.prevent="startAudioMerge"></label>
            <div class="merge-cover-field">
              <strong>封面 <small v-if="audioCoverCandidates.length">已识别当前目录 {{ audioCoverCandidates.length }} 张图片</small></strong>
              <div v-if="audioCoverCandidates.length" class="merge-cover-candidates">
                <button v-for="candidate in audioCoverCandidates" :key="candidate.id" type="button" :class="{active:audioMerge.coverFileId===candidate.id}" :title="candidate.name" @click="selectDirectoryCover(candidate)"><img :src="thumbSRC(candidate)" :alt="candidate.name"><span>{{ candidate.name }}</span></button>
              </div>
              <button type="button" class="merge-cover-picker" @click="chooseAudioCover">
                <img v-if="audioMerge.coverPreview" :src="audioMerge.coverPreview" alt="音频封面预览">
                <span v-else>＋</span>
                <div><b>{{ audioMerge.coverName||'上传其他封面' }}</b><small>{{ audioMerge.coverPreview?'点击可换成本地图片':'没有合适图片时从设备上传' }}</small></div>
              </button>
              <button v-if="audioMerge.coverPreview" type="button" class="merge-cover-remove" @click="clearAudioCover">移除封面</button>
              <input ref="audioCoverInput" hidden type="file" accept="image/jpeg,image/png,image/webp,image/gif" @change="e=>{const el=e.target as HTMLInputElement;if(el.files?.[0])setAudioCover(el.files[0]);el.value=''}">
            </div>
            <p class="lossless-note"><strong>{{ audioMerge.format==='flac'?'字幕说明':'字幕与播放说明' }}</strong><template v-if="audioMerge.format==='flac'">FLAC 不支持内嵌字幕；已识别 {{ audioMergeSubtitleCount }} / {{ audioMerge.order.length }} 个同名 VTT，切换 ALAC 或 AAC 后会自动合并并写入字幕轨。</template><template v-else>将内嵌 {{ audioMergeSubtitleCount }} / {{ audioMerge.order.length }} 个同名 VTT；各段字幕会随音频顺序自动校准时间轴。浏览器无法解码 ALAC 时会临时启动 FFmpeg HLS 兼容流。</template></p>
          </section>
          <section class="merge-order-panel">
            <div class="merge-order-heading"><div><strong>播放顺序</strong><small>每个文件会保留为一个分节</small></div><span>{{ audioMerge.order.length }} 段 · {{ formatSize(audioMerge.order.reduce((sum,item)=>sum+item.size,0)) }}</span></div>
            <div class="merge-order-list">
              <article v-for="(item,index) in audioMerge.order" :key="item.id">
                <b>{{ index+1 }}</b><div><strong :title="item.name">{{ item.name }}</strong><small>{{ formatSize(item.size) }}</small><span v-if="audioSubtitleFor(item)" class="merge-subtitle-match" :class="{disabled:audioMerge.format==='flac'}" :title="audioSubtitleFor(item)?.name"><i>CC</i>{{ audioMerge.format==='flac'?'已找到但 FLAC 不会打包':'将打包' }} · {{ audioSubtitleFor(item)?.name }}</span><span v-else class="merge-subtitle-match missing"><i>CC</i>未找到同名 .vtt</span></div>
                <span class="merge-order-actions"><button :disabled="index===0" title="上移" aria-label="上移" @click="moveAudioMergeInput(index,-1)">↑</button><button :disabled="index===audioMerge.order.length-1" title="下移" aria-label="下移" @click="moveAudioMergeInput(index,1)">↓</button></span>
              </article>
            </div>
          </section>
        </div>
        <p v-if="audioMerge.error" class="form-error merge-error">{{ audioMerge.error }}</p>
        <footer><button class="secondary" @click="closeModal">取消</button><button class="primary" :disabled="audioMerge.busy" @click="startAudioMerge">{{ audioMerge.busy?'正在创建…':'开始合并' }}</button></footer>
      </section>
      <section v-else-if="modal==='account'" class="modal account-modal">
        <header><div><p class="eyebrow dark">PROFILE & SECURITY</p><h2>账户设置</h2></div><button @click="closeModal">×</button></header>
        <div class="account-layout">
          <section class="avatar-settings">
            <div class="avatar-large"><img v-if="hasAvatar" class="ui-image" :src="avatarURL" alt="个人头像" draggable="false"><span v-else>{{ user.slice(0,1).toUpperCase() }}</span></div>
            <h3>个人头像</h3><p>支持 JPG、PNG、GIF 和 WebP，最大 2 MiB。</p>
            <div class="avatar-actions"><button type="button" class="secondary" :disabled="avatar.busy" @click="chooseAvatar">{{ avatar.busy?'处理中…':hasAvatar?'更换头像':'上传头像' }}</button><button v-if="hasAvatar" type="button" class="danger-text" :disabled="avatar.busy" @click="removeAvatar">移除</button></div>
            <input ref="avatarInput" hidden type="file" accept="image/jpeg,image/png,image/gif,image/webp" @change="e=>{const el=e.target as HTMLInputElement;if(el.files?.[0])uploadAvatar(el.files[0]);el.value=''}"><p v-if="avatar.error" class="form-error">{{ avatar.error }}</p>
          </section>
          <div class="account-overview">
            <section class="account-setting-row identity-row">
              <div class="setting-copy">
                <span class="setting-label">用户名</span>
                <div class="username-line">
                  <template v-if="usernameEditing">
                    <input ref="usernameInput" v-model="account.username" class="username-input" autocomplete="username" maxlength="128" aria-label="用户名" :disabled="usernameSaving" @focusout="saveUsername" @keydown.enter.prevent="($event.target as HTMLInputElement).blur()" @keydown.escape.prevent="cancelUsernameEdit">
                    <small v-if="usernameSaving">保存中…</small>
                  </template>
                  <template v-else><strong>{{ account.username }}</strong><button type="button" class="edit-username" aria-label="编辑用户名" @click="startUsernameEdit"><svg viewBox="0 0 24 24" aria-hidden="true"><path d="m4 16-.8 4 4-.8L18.5 7.9l-3.2-3.2L4 16Z"/></svg><span>编辑</span></button></template>
                </div>
                <p v-if="usernameError" class="form-error username-error">{{ usernameError }}</p>
              </div>
              <button type="button" class="secondary password-entry" @click="openAccountPanel('password')">修改密码</button>
            </section>

            <section class="account-setting-row security-row">
              <div class="setting-copy"><div class="setting-title"><span class="setting-label">两步验证</span><span class="security-badge" :class="{enabled:twoFactor.enabled}">{{ twoFactor.enabled?'已启用':'未启用' }}</span></div><p>{{ twoFactor.enabled?`身份验证器已启用，剩余 ${twoFactor.recoveryRemaining} 枚恢复码。`:'使用 TOTP 验证码保护管理员登录。' }}</p></div>
              <button type="button" class="secondary" :disabled="twoFactor.loading" @click="openAccountPanel('totp')">{{ twoFactor.loading?'读取中…':twoFactor.enabled?'管理':'设置' }}</button>
            </section>
            <section class="account-session-row"><div><span class="setting-label">当前会话</span><p>退出这台设备上的 Revaro 账户</p></div><button type="button" @click="logout">退出登录</button></section>
            <p v-if="twoFactor.error&&!accountPanel" class="form-error">{{ twoFactor.error }}</p>
          </div>
        </div>

        <div v-if="accountPanel" class="account-subdialog-backdrop" @click.self="closeAccountPanel">
          <section v-if="accountPanel==='password'" class="modal account-subdialog password-dialog">
            <header><div><p class="eyebrow dark">SECURITY</p><h2>修改密码</h2><p class="subdialog-hint">修改成功后，所有设备都需要使用新密码重新登录。</p></div><button type="button" aria-label="关闭" @click="closeAccountPanel">×</button></header>
            <form @submit.prevent="savePassword">
              <label>当前密码<input v-model="account.currentPassword" type="password" autocomplete="current-password" maxlength="1024" autofocus required></label>
              <label>新密码<input v-model="account.password" type="password" autocomplete="new-password" minlength="12" maxlength="1024" required></label>
              <label>确认新密码<input v-model="account.confirmPassword" type="password" autocomplete="new-password" minlength="12" maxlength="1024" required></label>
              <p v-if="account.error" class="form-error">{{ account.error }}</p>
              <footer><button type="button" class="secondary" @click="closeAccountPanel">取消</button><button class="primary" :disabled="modalBusy">{{ modalBusy?'正在修改…':'修改密码' }}</button></footer>
            </form>
          </section>

          <section v-else class="modal account-subdialog totp-dialog">
            <header><div><p class="eyebrow dark">SECURITY</p><h2>两步验证</h2><p class="subdialog-hint">使用兼容 TOTP 的身份验证器保护管理员登录。</p></div><button type="button" aria-label="关闭" @click="closeAccountPanel">×</button></header>
            <div v-if="twoFactor.loading" class="two-factor-loading"><div class="spinner"></div><span>正在读取安全设置…</span></div>
            <template v-else>
              <section v-if="twoFactor.recoveryCodes.length" class="recovery-panel">
                <div><strong>立即保存恢复码</strong><p>每枚恢复码只能使用一次。关闭窗口后将无法再次查看。</p></div>
                <div class="recovery-grid"><code v-for="code in twoFactor.recoveryCodes" :key="code">{{ code }}</code></div>
                <div class="recovery-actions"><button type="button" class="secondary" @click="copyRecoveryCodes">{{ twoFactor.copied?'已复制':'复制恢复码' }}</button><button type="button" class="secondary" @click="downloadRecoveryCodes">下载文本</button></div>
              </section>
              <template v-if="!twoFactor.enabled">
                <div v-if="twoFactor.stage==='idle'" class="two-factor-idle">
                  <p>启用后，登录时除密码外还需输入身份验证器生成的 6 位验证码。</p>
                  <label>当前密码<input v-model="twoFactor.currentPassword" type="password" autocomplete="current-password" maxlength="1024" placeholder="确认是你本人"></label>
                  <button type="button" class="primary" :disabled="twoFactor.busy" @click="beginTwoFactorSetup">{{ twoFactor.busy?'正在生成…':'开始设置' }}</button>
                </div>
                <div v-else class="totp-enroll">
                  <div class="totp-qr"><img :src="twoFactor.qrDataURL" alt="两步验证二维码"></div>
                  <div class="totp-instructions">
                    <h4>扫描二维码</h4><p>用身份验证器扫描二维码，然后输入应用中显示的验证码完成绑定。</p>
                    <p class="manual-secret">无法扫码？手动输入密钥 <code>{{ twoFactor.secret }}</code></p>
                    <label>6 位验证码<input v-model="twoFactor.code" autocomplete="one-time-code" inputmode="numeric" maxlength="8" placeholder="000000"></label>
                    <div class="two-factor-actions"><button type="button" class="secondary" :disabled="twoFactor.busy" @click="cancelTwoFactorSetup">返回</button><button type="button" class="primary" :disabled="twoFactor.busy" @click="enableTwoFactor">{{ twoFactor.busy?'正在验证…':'启用并生成恢复码' }}</button></div>
                  </div>
                </div>
              </template>
              <div v-else class="two-factor-enabled">
                <p>剩余 <strong>{{ twoFactor.recoveryRemaining }}</strong> 枚恢复码。重新生成或关闭验证前，需要再次确认当前密码和验证码。</p>
                <div class="two-factor-fields"><label>当前密码<input v-model="twoFactor.currentPassword" type="password" autocomplete="current-password" maxlength="1024"></label><label>验证码或恢复码<input v-model="twoFactor.code" autocomplete="one-time-code" maxlength="128"></label></div>
                <div class="two-factor-actions"><button type="button" class="secondary" :disabled="twoFactor.busy" @click="regenerateRecoveryCodes">重新生成恢复码</button><button type="button" class="danger-button" :disabled="twoFactor.busy" @click="disableTwoFactor">关闭两步验证</button></div>
              </div>
            </template>
            <p v-if="twoFactor.error" class="form-error two-factor-error">{{ twoFactor.error }}</p>
          </section>
        </div>
      </section>
      <section v-else-if="modal==='editor'" class="document-editor">
        <header class="editor-header">
          <div class="editor-title"><span>▤</span><div><input v-if="editor.isNew" v-model="editor.name" aria-label="文档文件名" maxlength="1024"><strong v-else :title="editor.name">{{ editor.name }}</strong><small>{{ editor.isNew?'保存在当前文件夹':editor.readonly?'回收站只读预览':'文本编辑器' }}</small></div><span class="editor-meta"><b>{{ editorBytes.toLocaleString() }} 字节</b><span> · UTF-8 · 最大 1 MiB</span></span></div>
          <div v-if="editorIsMarkdown&&!editor.readonly" class="editor-tabs" role="group" aria-label="编辑器视图"><button :class="{active:editor.mode==='edit'}" @click="editor.mode='edit'">编辑</button><button :class="{active:editor.mode==='split'}" @click="editor.mode='split'">分栏</button><button :class="{active:editor.mode==='preview'}" @click="editor.mode='preview'">预览</button></div>
          <div class="editor-actions"><span v-if="editor.error" class="editor-header-message error">{{ editor.error }}</span><span v-else-if="editor.readonly" class="editor-header-message">只读</span><template v-if="!editor.readonly"><span v-if="editor.isNew||editorDirty" class="unsaved-dot">未保存</span><button class="primary" :disabled="editor.busy||(!editor.isNew&&!editorDirty)" @click="saveDocument">{{ editor.busy?'保存中…':'保存' }}</button></template><button class="editor-close" aria-label="关闭编辑器" @click="closeEditor">×</button></div>
        </header>
        <div v-if="editor.busy&&!editor.content" class="state editor-loading"><div class="spinner"></div><p>正在打开文档…</p></div>
        <div v-else class="editor-workspace" :class="[`mode-${editor.mode}`,{markdown:editorIsMarkdown}]">
          <textarea v-if="editor.mode!=='preview'" v-model="editor.content" :readonly="editor.readonly" autofocus spellcheck="false" aria-label="文档内容" @keydown.ctrl.s.prevent="saveDocument" @keydown.meta.s.prevent="saveDocument"></textarea>
          <article v-if="editorIsMarkdown&&editor.mode!=='edit'" class="markdown-preview" v-html="renderedMarkdown"></article>
        </div>
      </section>
      <section v-else-if="modal==='share'" class="modal share-modal">
        <header><div class="share-title"><span>↗</span><div><h2>分享文件</h2><p :title="selected?.name">{{ selected?.name }}</p></div></div><button @click="closeModal">×</button></header>
        <div v-if="share.busy" class="state small"><div class="spinner"></div><p>正在准备分享…</p></div>
        <template v-else-if="share.active">
          <p class="share-description">任何拿到链接的人都能直接读取该文件。重新生成或停止分享后，旧链接立即失效。</p>
          <div class="share-link"><input :value="share.url" aria-label="分享链接" readonly @focus="($event.target as HTMLInputElement).select()"><button type="button" class="primary" @click="copyShare">{{ share.copied?'已复制':'复制链接' }}</button></div>
          <p v-if="share.createdAt" class="share-created">公开链接 · 创建于 {{ formatDate(share.createdAt) }}</p>
          <p v-if="share.error" class="form-error">{{ share.error }}</p>
          <footer class="share-footer"><button class="danger-text" :disabled="share.busy" @click="revokeShare">停止分享</button><button class="secondary" :disabled="share.busy" @click="createShare(true)">重新生成链接</button></footer>
        </template>
        <template v-else><p class="share-description">创建后，无需登录即可通过链接读取这个文件。你可以随时重新生成或停止分享。</p><button class="primary share-create" :disabled="share.busy" @click="createShare(false)">创建公开链接</button><p v-if="share.error" class="form-error">{{ share.error }}</p></template>
      </section>
      <MediaPreview v-else-if="modal==='preview'&&selected" :selected="selected" :items="items" @close="closeModal" @change="selected=$event" @download="download" @move="showMove" @copy="showCopy" />
    </div>
    <ReaderView v-if="modal==='reader'&&readerFile" :file="readerFile" @close="closeModal" />
    <AppDialog v-if="dialog.open" :title="dialog.title" :message="dialog.message" :confirm-label="dialog.confirmLabel" :cancel-label="dialog.cancelLabel" :tone="dialog.tone" :input="dialog.input" :value="dialog.value" :placeholder="dialog.placeholder" @update:value="dialog.value=$event" @confirm="finishDialog(true)" @cancel="finishDialog(false)" />
    <div v-if="toast.text" class="toast" :class="toast.kind">{{ toast.text }}</div>
  </div>
</template>

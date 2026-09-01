import type { Ref } from 'vue'
import { api } from '../api'
import type { ApiError, DriveFile } from '../api'
import type { BackgroundTask, ProfileResponse, UploadTask } from '../types'

type Login={username:string;password:string;secondFactor:string;totpRequired:boolean;busy:boolean;error:string;notice:string}
export function useAuthSession(deps:{user:Ref<string|null>;hasAvatar:Ref<boolean>;checking:Ref<boolean>;login:Login;items:Ref<DriveFile[]>;tasks:UploadTask[];backgroundTasks:Ref<BackgroundTask[]>;openRoute:()=>Promise<void>;openFolder:(id:string)=>Promise<void>;rootId:string;jobEvents:{connect:()=>void;stop:()=>void};refreshJobs:()=>Promise<void>}){
 const {user,hasAvatar,checking,login,items,tasks,backgroundTasks,openRoute,openFolder,rootId,jobEvents,refreshJobs}=deps
 async function checkSession(){try{const me=await api<ProfileResponse>('/api/auth/me');user.value=me.username;hasAvatar.value=me.has_avatar;await openRoute()}catch{user.value=null;hasAvatar.value=false}finally{checking.value=false}}
 async function submitLogin(){login.busy=true;login.error='';login.notice='';try{const me=await api<ProfileResponse>('/api/auth/login',{method:'POST',body:JSON.stringify({username:login.username,password:login.password,second_factor:login.secondFactor})});user.value=me.username;hasAvatar.value=me.has_avatar;login.password='';login.secondFactor='';login.totpRequired=false;await openFolder(rootId);jobEvents.connect();void refreshJobs()}catch(e){const code=((e as ApiError).data as {error?:{code?:string}}|null)?.error?.code;if(code==='totp_required'){login.totpRequired=true;login.error='请输入身份验证器验证码或恢复码'}else if(code==='invalid_second_factor'){login.totpRequired=true;login.error='验证码或恢复码不正确'}else login.error=(e as Error).message}finally{login.busy=false}}
 async function logout(){jobEvents.stop();await api('/api/auth/logout',{method:'POST'});user.value=null;hasAvatar.value=false;items.value=[];tasks.splice(0);backgroundTasks.value=[]}
 return {checkSession,submitLogin,logout}
}


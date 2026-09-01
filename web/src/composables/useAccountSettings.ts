import { computed, nextTick, reactive, ref, type Ref } from 'vue'
import { api } from '../api'
import type { DriveFile } from '../api'
import type { TOTPRecoveryResponse, TOTPSetupResponse, TOTPStatusResponse, UploadTask } from '../types'

type LoginState={username:string;password:string;secondFactor:string;totpRequired:boolean;busy:boolean;error:string;notice:string}
export function useAccountSettings(deps:{user:Ref<string|null>;hasAvatar:Ref<boolean>;avatarVersion:Ref<number>;login:LoginState;items:Ref<DriveFile[]>;tasks:UploadTask[];modalBusy:Ref<boolean>;avatarInput:Ref<HTMLInputElement|null>;notify:(text:string,kind?:'error'|'success')=>void;openModal:(name:'account')=>void;closeModal:()=>void;confirmDialog:(options:{title:string;message:string;confirmLabel?:string;tone?:'default'|'danger'})=>Promise<boolean>}){
  const {user,hasAvatar,avatarVersion,login,items,tasks,modalBusy,avatarInput,notify,openModal,closeModal,confirmDialog}=deps
  const account = reactive({ username:'', currentPassword:'', password:'', confirmPassword:'', error:'' })
  const accountPanel = ref<null|'password'|'totp'>(null)
  const usernameEditing = ref(false)
  const usernameSaving = ref(false)
  const usernameError = ref('')
  const usernameInput = ref<HTMLInputElement|null>(null)
  const avatar = reactive({ busy:false, error:'' })
  const twoFactor = reactive({ enabled:false, recoveryRemaining:0, loading:false, busy:false, stage:'idle' as 'idle'|'setup', currentPassword:'', code:'', secret:'', uri:'', qrDataURL:'', recoveryCodes:[] as string[], copied:false, error:'' })
  const avatarURL=computed(()=>`/api/profile/avatar?v=${avatarVersion.value}`)
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
  
  return {account,accountPanel,usernameEditing,usernameSaving,usernameError,usernameInput,avatar,twoFactor,avatarURL,showAccount,startUsernameEdit,cancelUsernameEdit,saveUsername,openAccountPanel,closeAccountPanel,chooseAvatar,uploadAvatar,removeAvatar,savePassword,beginTwoFactorSetup,cancelTwoFactorSetup,enableTwoFactor,regenerateRecoveryCodes,disableTwoFactor,copyRecoveryCodes,downloadRecoveryCodes}
}


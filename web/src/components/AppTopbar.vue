<script setup lang="ts">
import { computed, nextTick, onBeforeUnmount, onMounted, ref } from 'vue'
import { Activity, Settings, Trash2 } from '@lucide/vue'
import { isActiveTaskStatus } from '../taskStatus'
import type { BackgroundTask } from '../types'
import TaskCenter from './TaskCenter.vue'
import SystemStatus from './SystemStatus.vue'

const props=defineProps<{
  user:string
  hasAvatar:boolean
  avatarUrl:string
  tasks:BackgroundTask[]
  downloadParentId:string
}>()

const emit=defineEmits<{
  home:[]
  trash:[]
  account:[]
  avatarError:[]
  cancelTask:[task:BackgroundTask]
  retryTask:[task:BackgroundTask]
  tasksChanged:[]
}>()

const mobile=ref(false)
const mobileAccountMenu=ref<HTMLDetailsElement|null>(null)
const mobileTaskCenter=ref<InstanceType<typeof TaskCenter>|null>(null)
const activeTasks=computed(()=>props.tasks.filter(task=>isActiveTaskStatus(task.status)))
const failedTasks=computed(()=>props.tasks.filter(task=>task.status==='failed'))
let mediaQuery:MediaQueryList|null=null
function updateMobile(){mobile.value=!!mediaQuery?.matches;if(!mobile.value&&mobileAccountMenu.value)mobileAccountMenu.value.open=false}
function closeMobileMenu(){if(mobileAccountMenu.value)mobileAccountMenu.value.open=false}
function closeMobileMenuOutside(event:PointerEvent){const target=event.target;if(mobileAccountMenu.value?.open&&target instanceof Node&&!mobileAccountMenu.value.contains(target))closeMobileMenu()}
function closeMobileMenuEscape(event:KeyboardEvent){if(event.key==='Escape'&&mobileAccountMenu.value?.open){closeMobileMenu();mobileAccountMenu.value.querySelector<HTMLElement>('summary')?.focus()}}
function openMobileTaskCenter(){
  closeMobileMenu()
  nextTick(()=>mobileTaskCenter.value?.openCenter())
}
function openMobileTrash(){closeMobileMenu();emit('trash')}
function openMobileAccount(){closeMobileMenu();emit('account')}
onMounted(()=>{mediaQuery=window.matchMedia('(max-width:850px)');updateMobile();mediaQuery.addEventListener('change',updateMobile);document.addEventListener('pointerdown',closeMobileMenuOutside);document.addEventListener('keydown',closeMobileMenuEscape)})
onBeforeUnmount(()=>{mediaQuery?.removeEventListener('change',updateMobile);document.removeEventListener('pointerdown',closeMobileMenuOutside);document.removeEventListener('keydown',closeMobileMenuEscape)})
</script>

<template>
  <header class="topbar">
    <button class="logo brand-button" title="回到我的文件" aria-label="回到我的文件" @click="$emit('home')"><img class="brand-logo" src="/revaro-logo.svg" alt="" aria-hidden="true"></button>
    <div class="top-actions">
      <template v-if="!mobile">
        <TaskCenter :tasks="tasks" :parent-id="downloadParentId" @changed="$emit('tasksChanged')" @cancel="$emit('cancelTask',$event)" @retry="$emit('retryTask',$event)" />
        <SystemStatus />
        <button class="trash-button" title="回收站" aria-label="打开回收站" @click="$emit('trash')"><Trash2 aria-hidden="true" /></button>
      </template>
      <button v-if="!mobile" class="account-button" title="打开账户设置" @click="$emit('account')">
        <span class="avatar-badge"><img v-if="hasAvatar" class="ui-image" :src="avatarUrl" alt="个人头像" draggable="false" @error="$emit('avatarError')"><template v-else>{{ user.slice(0,1).toUpperCase() }}</template></span>
        <span class="account-copy"><b>{{ user }}</b><small>账户设置</small></span>
      </button>
      <SystemStatus v-if="mobile" />
      <details v-if="mobile" ref="mobileAccountMenu" class="mobile-account-menu">
        <summary title="账户与工具" aria-label="打开账户与工具菜单"><span class="avatar-badge"><img v-if="hasAvatar" class="ui-image" :src="avatarUrl" alt="个人头像" draggable="false" @error="$emit('avatarError')"><template v-else>{{ user.slice(0,1).toUpperCase() }}</template></span><i v-if="failedTasks.length" class="task-menu-badge failed" aria-label="存在失败任务">!</i><i v-else-if="activeTasks.length" class="task-menu-badge active" :aria-label="`${activeTasks.length} 个活动任务`"></i></summary>
        <section>
          <button class="mobile-tool-item" @click="openMobileTaskCenter"><span class="mobile-task-icon"><Activity aria-hidden="true" /></span><b>任务中心</b><small v-if="failedTasks.length" class="failed">{{ failedTasks.length }} 项失败</small><small v-else-if="activeTasks.length">{{ activeTasks.length }} 项活动</small></button>
          <button class="mobile-trash" @click="openMobileTrash"><span class="trash-button"><Trash2 aria-hidden="true" /></span><b>回收站</b></button>
          <hr>
          <button class="mobile-tool-item" @click="openMobileAccount"><span class="mobile-task-icon"><Settings aria-hidden="true" /></span><b>账户设置</b></button>
        </section>
      </details>
      <TaskCenter v-if="mobile" ref="mobileTaskCenter" hide-trigger :tasks="tasks" :parent-id="downloadParentId" @changed="$emit('tasksChanged')" @cancel="$emit('cancelTask',$event)" @retry="$emit('retryTask',$event)" />
    </div>
  </header>
</template>

<style scoped>
.brand-logo{display:block;width:auto;height:28px}.brand-button{min-width:0}
.trash-button{display:grid;place-items:center;width:44px;height:44px;padding:0;border:0;border-radius:50%;background:transparent;color:#64748b}.trash-button:hover{background:#f1f5f9;color:#334155}.trash-button svg{width:20px;height:20px;fill:none;stroke:currentColor;stroke-width:1.7;stroke-linecap:round;stroke-linejoin:round}.mobile-account-menu{position:relative}.mobile-account-menu>summary{position:relative;display:grid;width:40px;height:40px;place-items:center;cursor:pointer;list-style:none}.mobile-account-menu>summary::-webkit-details-marker{display:none}.mobile-account-menu>summary .avatar-badge{width:34px;height:34px}.task-menu-badge{position:absolute;top:1px;right:0;width:9px;height:9px;border:2px solid #fff;border-radius:50%;background:#2563eb}.task-menu-badge.failed{display:grid;place-items:center;width:15px;height:15px;border-width:1px;background:#dc2626;color:#fff;font-size:9px;font-style:normal;font-weight:900;line-height:1}.mobile-account-menu>section{position:fixed;z-index:44;top:64px;right:max(10px,env(safe-area-inset-right,0px));display:grid;width:min(210px,calc(100vw - 20px));padding:7px;border:1px solid #dfe6ee;border-radius:15px;background:#fff;box-shadow:0 22px 60px #0f172a2e}.mobile-account-menu hr{width:calc(100% - 12px);margin:5px 6px;border:0;border-top:1px solid #e8edf3}.mobile-tool-item,.mobile-trash{display:flex;align-items:center;min-height:46px;padding:3px 9px;border:0;border-radius:10px;background:#fff;color:#34475e;gap:9px}.mobile-tool-item:hover,.mobile-trash:hover{background:#f4f7fb}.mobile-tool-item{width:100%;cursor:pointer;text-align:left}.mobile-tool-item b,.mobile-trash>b{font-size:13px;font-weight:700}.mobile-tool-item small{margin-left:auto;color:#64748b;font-size:11px}.mobile-tool-item small.failed{color:#dc2626;font-weight:750}.mobile-task-icon{display:grid;width:34px;height:34px;place-items:center;color:#3d5f7e}.mobile-task-icon svg{width:19px;height:19px;fill:none;stroke:currentColor;stroke-width:2;stroke-linecap:round;stroke-linejoin:round}.mobile-trash{width:100%;cursor:pointer}.mobile-trash .trash-button{width:34px;height:34px}.mobile-trash b{font-weight:700}
@media(max-width:850px){.topbar{padding-right:max(14px,env(safe-area-inset-right,0px));padding-left:max(14px,env(safe-area-inset-left,0px))}.brand-logo{height:24px}.brand-button{flex:0 0 auto;white-space:nowrap}.top-actions{flex:0 0 auto;gap:5px}.top-actions :deep(.system-status>summary){width:38px;height:40px}.mobile-account-menu{flex:0 0 auto}}
@media(max-width:430px){.topbar{padding-right:max(12px,env(safe-area-inset-right,0px));padding-left:max(12px,env(safe-area-inset-left,0px))}.top-actions{gap:4px}.account-button{margin-left:0}}
@media(max-width:350px){.topbar{padding-right:max(8px,env(safe-area-inset-right,0px));padding-left:max(8px,env(safe-area-inset-left,0px))}.brand-logo{height:22px}.brand-button{padding-right:2px;padding-left:2px}.top-actions{gap:2px}}
@media(max-width:850px){.mobile-account-menu>section{top:calc(64px + env(safe-area-inset-top,0px));width:min(210px,calc(100vw - 20px - env(safe-area-inset-left,0px) - env(safe-area-inset-right,0px)))}.mobile-tool-item{min-width:0}.mobile-tool-item b{min-width:0;overflow:hidden;text-overflow:ellipsis;white-space:nowrap}}
</style>

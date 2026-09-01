<script setup lang="ts">
import { onBeforeUnmount, onMounted, ref } from 'vue'
import { Menu, Trash2 } from 'lucide-vue-next'
import type { BackgroundTask } from '../types'
import TaskCenter from './TaskCenter.vue'

defineProps<{
  user:string
  hasAvatar:boolean
  avatarUrl:string
  tasks:BackgroundTask[]
  downloadParentId:string
}>()

defineEmits<{
  home:[]
  trash:[]
  account:[]
  avatarError:[]
  cancelTask:[task:BackgroundTask]
  retryTask:[task:BackgroundTask]
  tasksChanged:[]
}>()

const mobile=ref(false)
const mobileMenu=ref<HTMLDetailsElement|null>(null)
let mediaQuery:MediaQueryList|null=null
function updateMobile(){mobile.value=!!mediaQuery?.matches;if(!mobile.value&&mobileMenu.value)mobileMenu.value.open=false}
function closeMobileMenu(event:PointerEvent){const target=event.target;if(mobileMenu.value?.open&&target instanceof Node&&!mobileMenu.value.contains(target))mobileMenu.value.open=false}
onMounted(()=>{mediaQuery=window.matchMedia('(max-width:850px)');updateMobile();mediaQuery.addEventListener('change',updateMobile);document.addEventListener('pointerdown',closeMobileMenu)})
onBeforeUnmount(()=>{mediaQuery?.removeEventListener('change',updateMobile);document.removeEventListener('pointerdown',closeMobileMenu)})
</script>

<template>
  <header class="topbar">
    <button class="logo brand-button" title="回到我的文件" aria-label="回到我的文件" @click="$emit('home')"><img class="brand-logo" src="/revaro-logo.svg" alt="" aria-hidden="true"></button>
    <div class="top-actions">
      <template v-if="!mobile">
        <TaskCenter :tasks="tasks" :parent-id="downloadParentId" @changed="$emit('tasksChanged')" @cancel="$emit('cancelTask',$event)" @retry="$emit('retryTask',$event)" />
        <button class="trash-button" title="回收站" aria-label="打开回收站" @click="$emit('trash')"><Trash2 aria-hidden="true" /></button>
      </template>
      <button class="account-button" title="打开账户设置" @click="$emit('account')">
        <span class="avatar-badge"><img v-if="hasAvatar" class="ui-image" :src="avatarUrl" alt="个人头像" draggable="false" @error="$emit('avatarError')"><template v-else>{{ user.slice(0,1).toUpperCase() }}</template></span>
        <span class="account-copy"><b>{{ user }}</b><small>账户设置</small></span>
      </button>
      <details v-if="mobile" ref="mobileMenu" class="mobile-tool-menu">
        <summary title="任务与工具" aria-label="打开任务与工具菜单"><Menu aria-hidden="true" /></summary>
        <section>
          <div class="mobile-tool-item"><TaskCenter :tasks="tasks" :parent-id="downloadParentId" @changed="$emit('tasksChanged')" @cancel="$emit('cancelTask',$event)" @retry="$emit('retryTask',$event)" /><span>任务中心</span></div>
          <button class="mobile-trash" @click="mobileMenu?.removeAttribute('open');$emit('trash')"><span class="trash-button"><Trash2 aria-hidden="true" /></span><b>回收站</b></button>
        </section>
      </details>
    </div>
  </header>
</template>

<style scoped>
.brand-logo{display:block;width:auto;height:28px}.brand-button{min-width:0}
.trash-button{display:grid;place-items:center;width:44px;height:44px;padding:0;border:0;border-radius:50%;background:transparent;color:#64748b}.trash-button:hover{background:#f1f5f9;color:#334155}.trash-button svg{width:20px;height:20px;fill:none;stroke:currentColor;stroke-width:1.7;stroke-linecap:round;stroke-linejoin:round}.mobile-tool-menu{position:relative}.mobile-tool-menu>summary{display:grid;place-items:center;width:40px;height:40px;border-radius:11px;color:#52657b;cursor:pointer;list-style:none}.mobile-tool-menu>summary::-webkit-details-marker{display:none}.mobile-tool-menu>summary:hover{background:#f1f5f9}.mobile-tool-menu>summary svg{width:23px;height:23px;fill:none;stroke:currentColor;stroke-width:1.9;stroke-linecap:round}.mobile-tool-menu>section{position:fixed;z-index:44;top:64px;right:10px;display:grid;width:min(250px,calc(100vw - 20px));padding:8px;border:1px solid #dfe6ee;border-radius:16px;background:#fff;box-shadow:0 22px 60px #0f172a2e}.mobile-tool-item,.mobile-trash{display:flex;align-items:center;min-height:52px;padding:4px 10px;border:0;border-radius:11px;background:#fff;color:#34475e;gap:12px}.mobile-tool-item:hover,.mobile-trash:hover{background:#f4f7fb}.mobile-tool-item>span,.mobile-trash>b{font-size:13px;font-weight:700}.mobile-trash{width:100%;cursor:pointer}.mobile-trash .trash-button{width:40px;height:40px}.mobile-trash b{font-weight:700}
@media(max-width:850px){.topbar{padding-right:max(14px,env(safe-area-inset-right,0px));padding-left:max(14px,env(safe-area-inset-left,0px))}.brand-logo{height:24px}.brand-button{flex:0 0 auto;white-space:nowrap}.top-actions{flex:0 0 auto;gap:6px}.account-copy{display:none}.account-button{flex:0 0 auto}.mobile-tool-menu{flex:0 0 auto}}
@media(max-width:430px){.topbar{padding-right:max(12px,env(safe-area-inset-right,0px));padding-left:max(12px,env(safe-area-inset-left,0px))}.top-actions{gap:4px}.account-button{margin-left:0}}
@media(max-width:350px){.topbar{padding-right:max(8px,env(safe-area-inset-right,0px));padding-left:max(8px,env(safe-area-inset-left,0px))}.brand-logo{height:22px}.brand-button{padding-right:2px;padding-left:2px}.top-actions{gap:2px}}
@media(max-width:850px){.mobile-tool-menu>section{top:calc(64px + env(safe-area-inset-top,0px));right:max(10px,env(safe-area-inset-right,0px));width:min(250px,calc(100vw - 20px - env(safe-area-inset-left,0px) - env(safe-area-inset-right,0px)))}.mobile-tool-item{min-width:0}.mobile-tool-item>span{min-width:0;overflow:hidden;text-overflow:ellipsis;white-space:nowrap}}
</style>

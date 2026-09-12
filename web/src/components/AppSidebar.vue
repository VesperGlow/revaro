<script setup lang="ts">
import { computed, onBeforeUnmount, onMounted, ref, watch } from 'vue'
import { BookOpen, ChevronLeft, ChevronRight, Film, FolderClosed, Image, Music, PanelLeftClose, PanelLeftOpen, Trash2 } from '@lucide/vue'
import { LIBRARY_CATEGORIES, type LibraryCounts, type LibraryFolderNode, type LibraryType } from '../library'
import SidebarFileTree from './SidebarFileTree.vue'
import SidebarPathTree from './SidebarPathTree.vue'

const props=defineProps<{
  section:LibraryType
  collapsed:boolean
  mobileOpen:boolean
  counts:LibraryCounts
  trees:Partial<Record<LibraryType,LibraryFolderNode>>
  activeFolderId:string|null
  currentFolderId:string
  reloadToken:number
}>()
const emit=defineEmits<{
  'select-category':[type:LibraryType]
  'select-folder':[id:string|null]
  'navigate-directory':[id:string]
  'toggle-collapse':[]
  'toggle-mobile':[]
  'close-mobile':[]
  'open-trash':[]
}>()

const icons={book:BookOpen,image:Image,video:Film,audio:Music,file:FolderClosed}
const EXPANDED_KEY='revaro:sidebar:expanded'
const MOBILE_QUERY='(max-width: 850px)'

// 窄屏用浮动抽屉：抽屉里始终显示图标+文字的完整布局，桌面才使用图标轨道。
const mobile=ref(typeof window!=='undefined'&&window.matchMedia(MOBILE_QUERY).matches)
const rail=computed(()=>props.collapsed&&!mobile.value)
let mediaQuery:MediaQueryList|null=null
function updateMobile(){mobile.value=!!mediaQuery?.matches}
function onKeydown(event:KeyboardEvent){if(event.key==='Escape'&&props.mobileOpen)emit('close-mobile')}
onMounted(()=>{
  mediaQuery=window.matchMedia(MOBILE_QUERY)
  updateMobile()
  mediaQuery.addEventListener('change',updateMobile)
  document.addEventListener('keydown',onKeydown)
})
onBeforeUnmount(()=>{
  mediaQuery?.removeEventListener('change',updateMobile)
  document.removeEventListener('keydown',onKeydown)
})

// 手风琴：同一时间只展开一个分类的路径树，打开新分类时自动收起旧的。
function readExpanded():LibraryType|null{
  try{
    const raw=localStorage.getItem(EXPANDED_KEY)
    if(!raw)return null
    return LIBRARY_CATEGORIES.some(category=>category.type===raw)?raw as LibraryType:null
  }catch{return null}
}
const expanded=ref<LibraryType|null>(readExpanded())
watch(expanded,value=>{try{ if(value)localStorage.setItem(EXPANDED_KEY,value); else localStorage.removeItem(EXPANDED_KEY) }catch{/* 隐私模式下忽略 */}})

function isExpanded(type:LibraryType){return expanded.value===type}
function toggleExpand(type:LibraryType){expanded.value=expanded.value===type?null:type}
function selectCategory(type:LibraryType){
  expanded.value=type
  emit('select-category',type)
  emit('close-mobile')
}
function chooseFolder(id:string|null){emit('select-folder',id)}
function navigateDirectory(id:string){emit('navigate-directory',id)}
const categoryHint=computed(()=>Object.fromEntries(LIBRARY_CATEGORIES.map(category=>[category.type,category.hint])))
</script>

<template>
  <div class="sidebar-backdrop" :class="{open:mobileOpen}" aria-hidden="true" @click="emit('close-mobile')"></div>
  <button v-if="mobile" type="button" class="sidebar-handle" :class="{open:mobileOpen}" :aria-expanded="mobileOpen" :aria-label="mobileOpen?'收起分类栏':'展开分类栏'" :title="mobileOpen?'收起分类栏':'展开分类栏'" @click="emit('toggle-mobile')">
    <ChevronLeft v-if="mobileOpen" aria-hidden="true" /><ChevronRight v-else aria-hidden="true" />
  </button>

  <aside class="app-sidebar" :class="{collapsed:rail,'mobile-open':mobileOpen}" aria-label="分类导航">
    <div class="sidebar-head">
      <button type="button" class="sidebar-collapse" :title="rail?'展开分类栏':'收起分类栏'" :aria-label="rail?'展开分类栏':'收起分类栏'" :aria-expanded="!rail" @click="emit('toggle-collapse')">
        <PanelLeftOpen v-if="rail" aria-hidden="true" /><PanelLeftClose v-else aria-hidden="true" />
        <span v-if="!rail">收起分类栏</span>
      </button>
    </div>

    <nav class="sidebar-nav">
      <section v-for="category in LIBRARY_CATEGORIES" :key="category.type" class="sidebar-category" :class="{active:section===category.type&&!rail}">
        <div class="category-row">
          <button type="button" class="category-main" :data-category="category.type" :class="{active:section===category.type}" :title="rail?category.label:categoryHint[category.type]" :aria-current="section===category.type?'page':undefined" @click="selectCategory(category.type)">
            <span class="category-icon"><component :is="icons[category.type]" aria-hidden="true" /></span>
            <span v-if="!rail" class="category-label">{{ category.label }}</span>
            <span v-if="!rail&&counts[category.type]" class="category-count">{{ counts[category.type] }}</span>
          </button>
          <button v-if="!rail" type="button" class="category-expand" :aria-expanded="isExpanded(category.type)" :aria-label="`${isExpanded(category.type)?'收起':'展开'}${category.label}路径`" :title="isExpanded(category.type)?'收起路径':'展开路径'" @click.stop="toggleExpand(category.type)"><ChevronRight :class="{open:isExpanded(category.type)}" aria-hidden="true" /></button>
        </div>
        <div v-if="!rail&&isExpanded(category.type)" class="category-paths">
          <SidebarPathTree v-if="category.type!=='file'&&trees[category.type]" :node="trees[category.type]!" :active-folder-id="activeFolderId" :depth="0" @select="chooseFolder" />
          <p v-else-if="category.type!=='file'" class="path-loading">还没有{{ category.label }}内容</p>
          <SidebarFileTree v-else :current-id="currentFolderId" :reload-token="reloadToken" @navigate="navigateDirectory" />
        </div>
      </section>
    </nav>

    <div class="sidebar-foot">
      <button type="button" class="category-main trash-entry" title="回收站" @click="emit('open-trash')">
        <span class="category-icon"><Trash2 aria-hidden="true" /></span>
        <span v-if="!rail" class="category-label">回收站</span>
      </button>
    </div>
  </aside>
</template>

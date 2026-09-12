<script setup lang="ts">
import { computed } from 'vue'
import type { DriveFile } from '../api'
import { formatSize } from '../format'
import { groupBookSeries, type LibraryItem } from '../library'
import BookCover from './BookCover.vue'

const props=defineProps<{items:LibraryItem[]}>()
const emit=defineEmits<{open:[item:DriveFile]}>()

const series=computed(()=>groupBookSeries(props.items))

// 统一固定尺寸只作为不可见的裁切容器：封面本身铺满该区域，组成一个封面块。
// 单本时一张封面铺满；系列时多本封面横向错位并共同填满整块区域：
// 第一本完整占左侧 FIRST_WIDTH，其余封面依次右移，最后一本的右缘正好落在
// 容器右边界（step = 剩余宽度 / 封面数量），因此无论多少本都填满且允许裁切。
const FIRST_WIDTH=58
const FAN_SCALE=1.06

function fanMetrics(count:number){
  const fans=Math.max(0,count-1)
  const step=fans<=0?0:(100-FIRST_WIDTH)/fans
  const maxAngle=fans<=1?0:Math.min(2,7/fans)
  return {fans,step,maxAngle}
}
function mainStyle(){return {width:`${FIRST_WIDTH}%`,zIndex:'3000'}}
function fanStyle(index:number,count:number){
  const {fans,step,maxAngle}=fanMetrics(count)
  const angle=fans<=1?0:(index-(fans-1)/2)*(2*maxAngle/Math.max(fans-1,1))
  return {
    left:`${((index+1)*step).toFixed(3)}%`,
    width:`${FIRST_WIDTH}%`,
    zIndex:String(2999-index),
    transform:`rotate(${angle.toFixed(3)}deg) scale(${FAN_SCALE})`,
  }
}
function bookSize(item:DriveFile){return formatSize(item.size)}
</script>

<template>
  <div class="book-shelf">
    <template v-for="group in series" :key="group.key">
      <article v-if="group.items.length>1" class="shelf-card series-card">
        <div class="series-stage">
          <button type="button" class="series-cover series-cover-main" :style="mainStyle()" :title="`阅读 ${group.items[0].name}`" @click="emit('open',group.items[0])">
            <BookCover :item="group.items[0]" show-title />
          </button>
          <button v-for="(book,index) in group.items.slice(1)" :key="book.id" type="button" class="series-cover series-cover-fan" :style="fanStyle(index,group.items.length)" :title="`阅读 ${book.name}`" @click="emit('open',book)">
            <BookCover :item="book" />
          </button>
          <span class="series-badge">{{ group.items.length }} 本</span>
        </div>
        <div class="shelf-meta"><strong :title="group.title">{{ group.title }}</strong><small>同系列 · {{ group.items.length }} 本</small></div>
      </article>

      <article v-else class="shelf-card" :title="`阅读 ${group.items[0].name}`" role="button" tabindex="0" @click="emit('open',group.items[0])" @keydown.enter.prevent="emit('open',group.items[0])">
        <div class="shelf-cover"><BookCover :item="group.items[0]" show-title /></div>
        <div class="shelf-meta"><strong :title="group.title">{{ group.title }}</strong><small>{{ bookSize(group.items[0]) }}</small></div>
      </article>
    </template>
  </div>
</template>

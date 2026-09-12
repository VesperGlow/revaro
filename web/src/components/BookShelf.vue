<script setup lang="ts">
import { computed } from 'vue'
import type { DriveFile } from '../api'
import { formatSize } from '../format'
import { groupBookSeries, type LibraryItem } from '../library'
import BookCover from './BookCover.vue'

const props=defineProps<{items:LibraryItem[]}>()
const emit=defineEmits<{open:[item:DriveFile]}>()

const series=computed(()=>groupBookSeries(props.items))

// 系列卡片始终是固定的方形尺寸：所有封面绝对定位，卡片高度只由宽度决定，
// 与书籍数量无关。第一本完整展示在左侧，其余封面依次向右错位展开。
const FIRST_WIDTH=52
const FAN_WIDTH=38
const FAN_START=46
const RIGHT_MARGIN=4

// 数量越多，偏移步长、旋转与下沉幅度越小；步长按可用宽度精确计算，
// 因此无论多少本都保证全部封面收在卡片内。
function fanMetrics(count:number){
  const fans=Math.max(0,count-1)
  const step=fans<=1?0:Math.min(22,Math.max(0,(100-FAN_START-FAN_WIDTH-RIGHT_MARGIN)/(fans-1)))
  const rotation=Math.min(2.8,9/Math.max(fans,1))
  const drift=Math.min(1.6,6/Math.max(fans,1))
  const shrink=Math.min(0.02,0.12/Math.max(fans,1))
  return {fans,step,rotation,drift,shrink}
}
function mainStyle(count:number){return {width:`${FIRST_WIDTH}%`,zIndex:String(Math.min(count,900))}}
function fanStyle(index:number,count:number){
  const {fans,step,rotation,drift,shrink}=fanMetrics(count)
  const offset=index-(fans-1)/2
  return {
    left:`${(FAN_START+index*step).toFixed(3)}%`,
    width:`${FAN_WIDTH}%`,
    zIndex:String(fans-index),
    transform:`translateY(-50%) translateY(${(index*drift).toFixed(3)}%) rotate(${(offset*rotation).toFixed(3)}deg) scale(${(1-index*shrink).toFixed(4)})`,
  }
}
function bookSize(item:DriveFile){return formatSize(item.size)}
</script>

<template>
  <div class="book-shelf">
    <template v-for="group in series" :key="group.key">
      <article v-if="group.items.length>1" class="shelf-card series-card">
        <div class="series-stage">
          <button type="button" class="series-cover series-cover-main" :style="mainStyle(group.items.length)" :title="`阅读 ${group.items[0].name}`" @click="emit('open',group.items[0])">
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

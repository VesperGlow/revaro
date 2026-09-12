<script setup lang="ts">
import { computed, reactive } from 'vue'
import type { DriveFile } from '../api'
import { formatSize } from '../format'
import { groupBookSeries, type LibraryItem } from '../library'
import BookCover from './BookCover.vue'

const props=defineProps<{items:LibraryItem[]}>()
const emit=defineEmits<{open:[item:DriveFile]}>()

const MAX_FAN=6
const series=computed(()=>groupBookSeries(props.items))
const expanded=reactive<Record<string,boolean>>({})
function toggle(key:string){expanded[key]=!expanded[key]}

function fanItems(group:{items:LibraryItem[]}){return group.items.slice(1,1+MAX_FAN)}
function hiddenCount(group:{items:LibraryItem[]}){return Math.max(0,group.items.length-1-MAX_FAN)}
function fanStyle(key:string,index:number,total:number){
  const open=!!expanded[key]
  const spread=open?Math.min(52,180/Math.max(total,1)):11
  return {'--x':`${index*spread}px`,'--r':`${(index-(total-1)/2)*(open?5.5:1.6)}deg`,'--z':String(total-index)}
}
function bookSize(item:DriveFile){return formatSize(item.size)}
</script>

<template>
  <div class="book-shelf">
    <template v-for="group in series" :key="group.key">
      <article v-if="group.items.length>1" class="shelf-card series-card" :class="{expanded:!!expanded[group.key]}">
        <div class="series-stage">
          <button type="button" class="series-main" :title="`阅读 ${group.items[0].name}`" @click="emit('open',group.items[0])">
            <BookCover :item="group.items[0]" show-title />
          </button>
          <div class="series-fan">
            <button v-for="(book,index) in fanItems(group)" :key="book.id" type="button" class="fan-card" :style="fanStyle(group.key,index,fanItems(group).length)" :title="`阅读 ${book.name}`" @click.stop="emit('open',book)">
              <BookCover :item="book" />
            </button>
            <span v-if="hiddenCount(group)" class="fan-more">+{{ hiddenCount(group) }}</span>
          </div>
          <span class="series-badge">{{ group.items.length }} 本</span>
          <button type="button" class="series-toggle" :aria-expanded="!!expanded[group.key]" @click.stop="toggle(group.key)">{{ expanded[group.key]?'收起':'展开' }}</button>
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

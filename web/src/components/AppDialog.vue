<script setup lang="ts">
import { nextTick, onMounted, ref } from 'vue'
import { CircleAlert, Trash2 } from '@lucide/vue'

const props=defineProps<{
  title:string
  message:string
  confirmLabel:string
  cancelLabel:string
  tone:'default'|'danger'
  input:boolean
  value:string
  placeholder?:string
}>()

const emit=defineEmits<{confirm:[];cancel:[];'update:value':[value:string]}>()
const inputElement=ref<HTMLInputElement|null>(null)

onMounted(()=>nextTick(()=>inputElement.value?.focus()))
function submit(){if(!props.input||props.value.trim())emit('confirm')}
</script>

<template>
  <div class="dialog-backdrop" role="presentation" @click.self="$emit('cancel')">
    <section class="app-dialog" role="dialog" aria-modal="true" :aria-labelledby="'dialog-title'" @keydown.esc="$emit('cancel')">
      <div class="dialog-icon" :class="tone">
        <Trash2 v-if="tone==='danger'" aria-hidden="true" />
        <CircleAlert v-else aria-hidden="true" />
      </div>
      <div class="dialog-copy"><h2 id="dialog-title">{{ title }}</h2><p>{{ message }}</p></div>
      <input v-if="input" ref="inputElement" :value="value" :placeholder="placeholder" maxlength="1024" @input="$emit('update:value',($event.target as HTMLInputElement).value)" @keyup.enter="submit">
      <footer><button class="secondary" @click="$emit('cancel')">{{ cancelLabel }}</button><button class="dialog-confirm" :class="tone" :disabled="input&&!value.trim()" @click="submit">{{ confirmLabel }}</button></footer>
    </section>
  </div>
</template>

<style scoped>
.dialog-backdrop{position:fixed;z-index:80;inset:0;display:grid;place-items:center;padding:20px;background:#0f172a73;backdrop-filter:blur(5px)}
.app-dialog{display:grid;grid-template-columns:46px 1fr;width:min(440px,100%);padding:24px;border:1px solid #e6ebf1;border-radius:19px;background:#fff;box-shadow:0 30px 90px #0f172a4d}
.dialog-icon{display:grid;place-items:center;width:38px;height:38px;border-radius:12px;background:#eaf2ff;color:#2563eb}.dialog-icon.danger{background:#feecec;color:#dc2626}.dialog-icon svg{width:21px;height:21px;fill:none;stroke:currentColor;stroke-width:1.8;stroke-linecap:round;stroke-linejoin:round}
.dialog-copy h2{margin:2px 0 7px;color:#172033;font-size:18px}.dialog-copy p{margin:0;color:#66758a;font-size:13px;line-height:1.65;white-space:pre-line}
input{grid-column:1/-1;width:100%;margin-top:20px;padding:12px 13px;border:1px solid #d9e1eb;border-radius:11px;outline:none}input:focus{border-color:#60a5fa;box-shadow:0 0 0 3px #60a5fa24}
footer{grid-column:1/-1;display:flex;justify-content:flex-end;gap:10px;margin-top:24px}button{min-height:39px;padding:0 16px;border-radius:10px;font-weight:750}.dialog-confirm{border:0;background:#2563eb;color:#fff}.dialog-confirm.danger{background:#dc2626}.dialog-confirm:disabled{opacity:.5}
</style>

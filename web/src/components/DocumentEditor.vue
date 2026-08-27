<script setup lang="ts">
defineProps<{isNew:boolean;readonly:boolean;name:string;content:string;mode:'edit'|'split'|'preview';busy:boolean;error:string;dirty:boolean;bytes:number;markdown:boolean;renderedMarkdown:string}>()
defineEmits<{close:[];save:[];'update:name':[value:string];'update:content':[value:string];'update:mode':[value:'edit'|'split'|'preview']}>()
</script>

<template>
  <section class="document-editor">
    <header class="editor-header">
      <div class="editor-title"><span>▤</span><div><input v-if="isNew" :value="name" aria-label="文档文件名" maxlength="1024" @input="$emit('update:name',($event.target as HTMLInputElement).value)"><strong v-else :title="name">{{ name }}</strong><small>{{ isNew?'保存在当前文件夹':readonly?'回收站只读预览':'文本编辑器' }}</small></div><span class="editor-meta"><b>{{ bytes.toLocaleString() }} 字节</b><span> · UTF-8 · 最大 1 MiB</span></span></div>
      <div v-if="markdown&&!readonly" class="editor-tabs" role="group" aria-label="编辑器视图"><button :class="{active:mode==='edit'}" @click="$emit('update:mode','edit')">编辑</button><button :class="{active:mode==='split'}" @click="$emit('update:mode','split')">分栏</button><button :class="{active:mode==='preview'}" @click="$emit('update:mode','preview')">预览</button></div>
      <div class="editor-actions"><span v-if="error" class="editor-header-message error">{{ error }}</span><span v-else-if="readonly" class="editor-header-message">只读</span><template v-if="!readonly"><span v-if="isNew||dirty" class="unsaved-dot">未保存</span><button class="primary" :disabled="busy||(!isNew&&!dirty)" @click="$emit('save')">{{ busy?'保存中…':'保存' }}</button></template><button class="editor-close" aria-label="关闭编辑器" @click="$emit('close')">×</button></div>
    </header>
    <div v-if="busy&&!content" class="state editor-loading"><div class="spinner"></div><p>正在打开文档…</p></div>
    <div v-else class="editor-workspace" :class="[`mode-${mode}`,{markdown}]">
      <textarea v-if="mode!=='preview'" :value="content" :readonly="readonly" autofocus spellcheck="false" aria-label="文档内容" @input="$emit('update:content',($event.target as HTMLTextAreaElement).value)" @keydown.ctrl.s.prevent="$emit('save')" @keydown.meta.s.prevent="$emit('save')"></textarea>
      <article v-if="markdown&&mode!=='edit'" class="markdown-preview" v-html="renderedMarkdown"></article>
    </div>
  </section>
</template>

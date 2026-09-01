<script setup lang="ts">
defineProps<{login:{username:string;password:string;secondFactor:string;totpRequired:boolean;busy:boolean;error:string;notice:string}}>()
defineEmits<{submit:[];username:[value:string];password:[value:string];secondFactor:[value:string]}>()
</script>

<template>
  <main class="login-page">
    <section class="login-visual"><div class="glow glow-a"></div><div class="glow glow-b"></div><div class="visual-copy"><span class="eyebrow">PRIVATE · DIRECT · YOURS</span><h1>你的文件，<br>安静地待在云上。</h1><p>轻量、自托管，浏览器直连你的 S3。</p></div><div class="revaro-card"><span>☁</span><div><strong>不透明对象存储</strong><small>SQLite 元数据 · 原生 Range</small></div></div></section>
    <section class="login-panel">
      <form class="login-form" @submit.prevent="$emit('submit')">
        <div class="logo"><span class="brand-mark small"><img class="ui-image" src="/logo.png" alt="" draggable="false"></span><span>revaro</span></div>
        <div><p class="eyebrow dark">WELCOME BACK</p><h2>登录私人空间</h2><p class="muted">首次启动的随机凭据可在容器日志中查看</p></div>
        <label>用户名<input :value="login.username" autocomplete="username" maxlength="128" required @input="$emit('username',($event.target as HTMLInputElement).value)"></label>
        <label>密码<input :value="login.password" type="password" autocomplete="current-password" maxlength="1024" required @input="$emit('password',($event.target as HTMLInputElement).value)"></label>
        <label v-if="login.totpRequired">验证码或恢复码<input :value="login.secondFactor" autocomplete="one-time-code" maxlength="128" placeholder="6 位验证码或恢复码" required @input="$emit('secondFactor',($event.target as HTMLInputElement).value)"><small class="login-totp-hint">打开身份验证器，或输入一枚尚未使用的恢复码。</small></label>
        <p v-if="login.notice" class="form-success">{{ login.notice }}</p><p v-if="login.error" class="form-error">{{ login.error }}</p>
        <button class="primary wide" :disabled="login.busy">{{ login.busy ? '正在验证…' : '进入我的网盘' }}</button>
      </form>
    </section>
  </main>
</template>

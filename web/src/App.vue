<script lang="ts" src="./app-controller.ts"></script>


<template>
  <div v-if="checking" class="splash"><div class="brand-mark"><img class="ui-image" src="/logo.png" alt="" draggable="false"></div><div class="spinner"></div></div>
  <LoginPage v-else-if="!user" :login="login" @username="login.username=$event" @password="login.password=$event" @second-factor="login.secondFactor=$event" @submit="submitLogin" />

  <div v-else class="app-shell" @dragover.prevent="dragActive=true" @dragleave.self="dragActive=false" @drop.prevent="onDrop">
    <AppTopbar :user="user" :has-avatar="hasAvatar" :avatar-url="avatarURL" :tasks="backgroundTasks" @home="openFolder(ROOT)" @trash="openTrash" @account="showAccount" @avatar-error="hasAvatar=false" @cancel-task="cancelBackgroundTask" @retry-task="retryBackgroundTask" @tasks-changed="refreshBackgroundTasks" />
    <section class="content" @click="clearSelectionFromBlank">
      <FileBrowserHeader :breadcrumbs="breadcrumbs" :current="current" :item-count="items.length" :total-bytes="directoryStats.total_bytes" :file-count="directoryStats.file_count" :trash-mode="trashMode" @open-folder="openFolder" @new-document="newDocument" @create-folder="createFolder" @upload-files="chooseFiles" @upload-folder="chooseFolder" @leave-trash="openFolder(ROOT)" @empty-trash="emptyTrash" />
      <input ref="fileInput" hidden type="file" multiple @change="filesChanged">
      <input ref="folderInput" hidden type="file" multiple webkitdirectory @change="folderChanged">
      <SelectionToolbar v-if="selectedItems.length&&!modal" :selected-items="selectedItems" :selected-bytes="selectedBytes" :selected-files="selectedFiles" :single-selected="singleSelected" :item-count="items.length" :trash-mode="trashMode" @clear="clearSelection" @restore="restoreSelected" @purge="purgeSelected" @select-all="selectAll" @open="openItem" @extract="extractArchive" @download="downloadSelected" @share="showShare" @rename="showRename" @move="showMoveSelected" @remove="removeSelected" />
      <div v-if="loading" class="state"><div class="spinner"></div><p>正在读取文件…</p></div>
      <div v-else-if="!items.length" class="state empty"><div class="empty-icon">⌁</div><h3>{{ trashMode?'回收站是空的':'这里还是空的' }}</h3><p>{{ trashMode?'删除的项目会先来到这里。':'拖放文件到这里，或新建一篇文档。' }}</p><div v-if="!trashMode" class="empty-actions"><button class="secondary" @click="newDocument">新建文档</button><button class="primary" @click="chooseFiles">上传文件</button></div></div>
      <FileGrid v-else :items="items" :selected-ids="selectedIds" :trash-mode="trashMode" @open="openItem" @select="toggleSelection" />
    </section>

    <div v-if="dragActive&&!trashMode" class="drop-zone"><div><span>↓</span><h2>释放以上传到 {{ current?.name || '我的文件' }}</h2><p>文件将保存到服务器本地磁盘</p></div></div>

    <div v-if="modal" class="modal-backdrop" :class="{previewing:modal==='preview','audio-previewing':modal==='preview'&&!!selected&&isAudio(selected),'video-previewing':modal==='preview'&&!!selected&&isVideo(selected),editing:modal==='editor',reading:modal==='reader',accounting:modal==='account'}" @click.self="closeBackdrop">
      <section v-if="modal==='rename'" class="modal"><header><div><p class="eyebrow dark">EDIT</p><h2>重命名</h2></div><button @click="closeModal">×</button></header><label>新名称<input v-model="renameValue" maxlength="1024" @keyup.enter="saveRename"></label><footer><button class="secondary" @click="closeModal">取消</button><button class="primary" :disabled="modalBusy" @click="saveRename">保存</button></footer></section>
      <MoveCopyDialog v-else-if="modal==='move'" :mode="transferMode" :targets="moveTargets" :initial-id="currentId" :busy="modalBusy" @close="closeModal" @select="transferTo" />
      <section v-else-if="modal==='account'" class="modal account-modal">
        <header><div><h2>账户设置</h2></div><button @click="closeModal">×</button></header>
        <div class="account-layout">
          <section class="avatar-settings">
            <div class="avatar-large"><img v-if="hasAvatar" class="ui-image" :src="avatarURL" alt="个人头像" draggable="false"><span v-else>{{ user.slice(0,1).toUpperCase() }}</span></div>
            <h3>个人头像</h3><p>支持 JPG、PNG、GIF 和 WebP，最大 2 MiB。</p>
            <div class="avatar-actions"><button type="button" class="secondary" :disabled="avatar.busy" @click="chooseAvatar">{{ avatar.busy?'处理中…':hasAvatar?'更换头像':'上传头像' }}</button><button v-if="hasAvatar" type="button" class="danger-text" :disabled="avatar.busy" @click="removeAvatar">移除</button></div>
            <input ref="avatarInput" hidden type="file" accept="image/jpeg,image/png,image/gif,image/webp" @change="avatarChanged"><p v-if="avatar.error" class="form-error">{{ avatar.error }}</p>
          </section>
          <div class="account-overview">
            <section class="account-setting-row identity-row">
              <div class="setting-copy">
                <span class="setting-label">用户名</span>
                <div class="username-line">
                  <template v-if="usernameEditing">
                    <input ref="usernameInput" v-model="account.username" class="username-input" autocomplete="username" maxlength="128" aria-label="用户名" :disabled="usernameSaving" @focusout="saveUsername" @keydown.enter.prevent="blurEventTarget" @keydown.escape.prevent="cancelUsernameEdit">
                    <small v-if="usernameSaving">保存中…</small>
                  </template>
                  <template v-else><strong>{{ account.username }}</strong><button type="button" class="edit-username" aria-label="编辑用户名" @click="startUsernameEdit"><svg viewBox="0 0 24 24" aria-hidden="true"><path d="m4 16-.8 4 4-.8L18.5 7.9l-3.2-3.2L4 16Z"/></svg><span>编辑</span></button></template>
                </div>
                <p v-if="usernameError" class="form-error username-error">{{ usernameError }}</p>
              </div>
              <button type="button" class="secondary password-entry" @click="openAccountPanel('password')">修改密码</button>
            </section>

            <section class="account-setting-row security-row">
              <div class="setting-copy"><div class="setting-title"><span class="setting-label">两步验证</span><span class="security-badge" :class="{enabled:twoFactor.enabled}">{{ twoFactor.enabled?'已启用':'未启用' }}</span></div><p>{{ twoFactor.enabled?`身份验证器已启用，剩余 ${twoFactor.recoveryRemaining} 枚恢复码。`:'使用 TOTP 验证码保护管理员登录。' }}</p></div>
              <button type="button" class="secondary" :disabled="twoFactor.loading" @click="openAccountPanel('totp')">{{ twoFactor.loading?'读取中…':twoFactor.enabled?'管理':'设置' }}</button>
            </section>
            <section class="account-session-row"><div><span class="setting-label">当前会话</span><p>退出这台设备上的 Revaro 账户</p></div><button type="button" @click="logout">退出登录</button></section>
            <p v-if="twoFactor.error&&!accountPanel" class="form-error">{{ twoFactor.error }}</p>
          </div>
        </div>

        <div v-if="accountPanel" class="account-subdialog-backdrop" @click.self="closeAccountPanel">
          <section v-if="accountPanel==='password'" class="modal account-subdialog password-dialog">
            <header><div><p class="eyebrow dark">SECURITY</p><h2>修改密码</h2><p class="subdialog-hint">修改成功后，所有设备都需要使用新密码重新登录。</p></div><button type="button" aria-label="关闭" @click="closeAccountPanel">×</button></header>
            <form @submit.prevent="savePassword">
              <label>当前密码<input v-model="account.currentPassword" type="password" autocomplete="current-password" maxlength="1024" autofocus required></label>
              <label>新密码<input v-model="account.password" type="password" autocomplete="new-password" minlength="12" maxlength="1024" required></label>
              <label>确认新密码<input v-model="account.confirmPassword" type="password" autocomplete="new-password" minlength="12" maxlength="1024" required></label>
              <p v-if="account.error" class="form-error">{{ account.error }}</p>
              <footer><button type="button" class="secondary" @click="closeAccountPanel">取消</button><button class="primary" :disabled="modalBusy">{{ modalBusy?'正在修改…':'修改密码' }}</button></footer>
            </form>
          </section>

          <section v-else class="modal account-subdialog totp-dialog">
            <header><div><p class="eyebrow dark">SECURITY</p><h2>两步验证</h2><p class="subdialog-hint">使用兼容 TOTP 的身份验证器保护管理员登录。</p></div><button type="button" aria-label="关闭" @click="closeAccountPanel">×</button></header>
            <div v-if="twoFactor.loading" class="two-factor-loading"><div class="spinner"></div><span>正在读取安全设置…</span></div>
            <template v-else>
              <section v-if="twoFactor.recoveryCodes.length" class="recovery-panel">
                <div><strong>立即保存恢复码</strong><p>每枚恢复码只能使用一次。关闭窗口后将无法再次查看。</p></div>
                <div class="recovery-grid"><code v-for="code in twoFactor.recoveryCodes" :key="code">{{ code }}</code></div>
                <div class="recovery-actions"><button type="button" class="secondary" @click="copyRecoveryCodes">{{ twoFactor.copied?'已复制':'复制恢复码' }}</button><button type="button" class="secondary" @click="downloadRecoveryCodes">下载文本</button></div>
              </section>
              <template v-if="!twoFactor.enabled">
                <div v-if="twoFactor.stage==='idle'" class="two-factor-idle">
                  <p>启用后，登录时除密码外还需输入身份验证器生成的 6 位验证码。</p>
                  <label>当前密码<input v-model="twoFactor.currentPassword" type="password" autocomplete="current-password" maxlength="1024" placeholder="确认是你本人"></label>
                  <button type="button" class="primary" :disabled="twoFactor.busy" @click="beginTwoFactorSetup">{{ twoFactor.busy?'正在生成…':'开始设置' }}</button>
                </div>
                <div v-else class="totp-enroll">
                  <div class="totp-qr"><img :src="twoFactor.qrDataURL" alt="两步验证二维码"></div>
                  <div class="totp-instructions">
                    <h4>扫描二维码</h4><p>用身份验证器扫描二维码，然后输入应用中显示的验证码完成绑定。</p>
                    <p class="manual-secret">无法扫码？手动输入密钥 <code>{{ twoFactor.secret }}</code></p>
                    <label>6 位验证码<input v-model="twoFactor.code" autocomplete="one-time-code" inputmode="numeric" maxlength="8" placeholder="000000"></label>
                    <div class="two-factor-actions"><button type="button" class="secondary" :disabled="twoFactor.busy" @click="cancelTwoFactorSetup">返回</button><button type="button" class="primary" :disabled="twoFactor.busy" @click="enableTwoFactor">{{ twoFactor.busy?'正在验证…':'启用并生成恢复码' }}</button></div>
                  </div>
                </div>
              </template>
              <div v-else class="two-factor-enabled">
                <p>剩余 <strong>{{ twoFactor.recoveryRemaining }}</strong> 枚恢复码。重新生成或关闭验证前，需要再次确认当前密码和验证码。</p>
                <div class="two-factor-fields"><label>当前密码<input v-model="twoFactor.currentPassword" type="password" autocomplete="current-password" maxlength="1024"></label><label>验证码或恢复码<input v-model="twoFactor.code" autocomplete="one-time-code" maxlength="128"></label></div>
                <div class="two-factor-actions"><button type="button" class="secondary" :disabled="twoFactor.busy" @click="regenerateRecoveryCodes">重新生成恢复码</button><button type="button" class="danger-button" :disabled="twoFactor.busy" @click="disableTwoFactor">关闭两步验证</button></div>
              </div>
            </template>
            <p v-if="twoFactor.error" class="form-error two-factor-error">{{ twoFactor.error }}</p>
          </section>
        </div>
      </section>
      <DocumentEditor v-else-if="modal==='editor'" :is-new="editor.isNew" :readonly="editor.readonly" :name="editor.name" :content="editor.content" :mode="editor.mode" :busy="editor.busy" :error="editor.error" :dirty="editorDirty" :bytes="editorBytes" :markdown="editorIsMarkdown" :rendered-markdown="renderedMarkdown" @update:name="editor.name=$event" @update:content="editor.content=$event" @update:mode="editor.mode=$event" @save="saveDocument" @close="closeEditor" />
      <ShareDialog v-else-if="modal==='share'" :file="selected" :active="share.active" :url="share.url" :created-at="share.createdAt" :busy="share.busy" :error="share.error" :copied="share.copied" @close="closeModal" @copy="copyShare" @revoke="revokeShare" @create="createShare" />
      <MediaPreview v-else-if="modal==='preview'&&selected" :selected="selected" :items="items" @close="closeModal" @change="selected=$event" @download="download" @move="showMove" @copy="showCopy" />
    </div>
    <Reader v-if="modal==='reader'&&readerFile" :file="readerFile" @close="closeModal" />
    <AppDialog v-if="dialog.open" :title="dialog.title" :message="dialog.message" :confirm-label="dialog.confirmLabel" :cancel-label="dialog.cancelLabel" :tone="dialog.tone" :input="dialog.input" :value="dialog.value" :placeholder="dialog.placeholder" @update:value="dialog.value=$event" @confirm="finishDialog(true)" @cancel="finishDialog(false)" />
    <div v-if="toast.text" class="toast" :class="toast.kind">{{ toast.text }}</div>
  </div>
</template>

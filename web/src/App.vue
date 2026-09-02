<script lang="ts" src="./app-controller.ts"></script>


<template>
  <div v-if="checking" class="splash"><div class="brand-mark"><img class="ui-image" src="/logo.png" alt="" draggable="false"></div><div class="spinner"></div></div>
  <LoginPage v-else-if="!user" :login="login" @username="login.username=$event" @password="login.password=$event" @second-factor="login.secondFactor=$event" @submit="submitLogin" />

  <div v-else class="app-shell" @dragover.prevent="dragActive=true" @dragleave.self="dragActive=false" @drop.prevent="onDrop">
    <AppTopbar :user="user" :has-avatar="hasAvatar" :avatar-url="avatarURL" :tasks="backgroundTasks" :download-parent-id="trashMode?ROOT:currentId" @home="openFolder(ROOT)" @trash="openTrash" @account="showAccount" @avatar-error="hasAvatar=false" @cancel-task="cancelBackgroundTask" @retry-task="retryBackgroundTask" @tasks-changed="refreshBackgroundTasks" />
    <section class="content" @click="clearSelectionFromBlank">
      <FileBrowserHeader :breadcrumbs="breadcrumbs" :current="current" :item-count="items.length" :total-bytes="directoryStats.total_bytes" :file-count="directoryStats.file_count" :trash-mode="trashMode" @open-folder="openFolder" @new-document="newDocument" @create-folder="createFolder" @upload-files="chooseFiles" @upload-folder="chooseFolder" @local-audio-merge="showLocalAudioMerge" @leave-trash="openFolder(ROOT)" @empty-trash="emptyTrash" />
      <input ref="fileInput" hidden type="file" multiple @change="filesChanged">
      <input ref="folderInput" hidden type="file" multiple webkitdirectory @change="folderChanged">
      <input ref="localMergeInput" hidden type="file" multiple webkitdirectory @change="localMergeChanged">
      <SelectionToolbar v-if="selectedItems.length&&!modal" :selected-items="selectedItems" :selected-bytes="selectedBytes" :selected-files="selectedFiles" :single-selected="singleSelected" :item-count="items.length" :trash-mode="trashMode" :can-merge-audio="canMergeSelectedAudio" @clear="clearSelection" @restore="restoreSelected" @purge="purgeSelected" @select-all="selectAll" @open="openItem" @merge-audio="showAudioMerge" @extract="extractArchive" @download="downloadSelected" @share="showShare" @rename="showRename" @move="showMoveSelected" @remove="removeSelected" />
      <div v-if="loading" class="state"><div class="spinner"></div><p>正在读取文件…</p></div>
      <div v-else-if="!items.length" class="state empty"><div class="empty-icon">⌁</div><h3>{{ trashMode?'回收站是空的':'这里还是空的' }}</h3><p>{{ trashMode?'删除的项目会先来到这里。':'拖放文件到这里，或新建一篇文档。' }}</p><div v-if="!trashMode" class="empty-actions"><button class="secondary" @click="newDocument">新建文档</button><button class="primary" @click="chooseFiles">上传文件</button></div></div>
      <FileGrid v-else :items="items" :selected-ids="selectedIds" :trash-mode="trashMode" @open="openItem" @select="toggleSelection" />
    </section>

    <div v-if="dragActive&&!trashMode" class="drop-zone"><div><span>↓</span><h2>释放以上传到 {{ current?.name || '我的文件' }}</h2><p>文件将通过 Presigned URL 直传 S3</p></div></div>

    <div v-if="modal" class="modal-backdrop" :class="{previewing:modal==='preview','audio-previewing':modal==='preview'&&!!selected&&isAudio(selected),'video-previewing':modal==='preview'&&!!selected&&isVideo(selected),editing:modal==='editor',reading:modal==='reader',accounting:modal==='account'}" @click.self="closeBackdrop">
      <section v-if="modal==='rename'" class="modal"><header><div><p class="eyebrow dark">EDIT</p><h2>重命名</h2></div><button @click="closeModal">×</button></header><label>新名称<input v-model="renameValue" maxlength="1024" @keyup.enter="saveRename"></label><footer><button class="secondary" @click="closeModal">取消</button><button class="primary" :disabled="modalBusy" @click="saveRename">保存</button></footer></section>
      <MoveCopyDialog v-else-if="modal==='move'" :mode="transferMode" :targets="moveTargets" :initial-id="currentId" :busy="modalBusy" @close="closeModal" @select="transferTo" />
      <section v-else-if="modal==='audioMerge'" class="modal audio-merge-modal">
        <header><div><p class="eyebrow dark">AUDIO MERGE</p><h2>合并音频</h2><p>{{ audioMerge.local?'从电脑目录上传素材，输出固定为无损 ALAC M4A':'FLAC / ALAC 真无损，或选择 AAC 节省空间' }}</p></div><button aria-label="关闭" @click="closeAudioMergeModal">×</button></header>
        <div class="merge-source-tabs" role="tablist" aria-label="合并来源">
          <button type="button" :class="{active:!audioMerge.local}" :disabled="localMerge.busy" @click="setAudioMergeSource('revaro')"><span>☁</span>从 Revaro 合并</button>
          <button type="button" :class="{active:audioMerge.local}" :disabled="localMerge.busy" @click="setAudioMergeSource('local')"><span>♬</span>从本地目录合并</button>
        </div>
        <template v-if="!audioMerge.local">
          <div class="audio-merge-layout">
            <section class="merge-settings-panel">
              <fieldset class="merge-format-field">
                <legend>输出格式</legend><div class="merge-format-options">
                  <button type="button" :class="{active:audioMerge.format==='flac'}" @click="setAudioMergeFormat('flac')"><span>FLAC</span><strong>无损 · 通用</strong><small>下载母版为 .flac</small></button>
                  <button type="button" :class="{active:audioMerge.format==='alac'}" @click="setAudioMergeFormat('alac')"><span>ALAC</span><strong>无损 · Apple</strong><small>下载母版为 .m4a</small></button>
                  <button type="button" :class="{active:audioMerge.format==='aac'}" @click="setAudioMergeFormat('aac')"><span>AAC</span><strong>有损 · 192k</strong><small>体积小，直接流播</small></button>
                </div>
              </fieldset>
              <label>输出文件名<input v-model="audioMerge.name" maxlength="1024" :placeholder="`合并音频${audioMergeExtension(audioMerge.format)}`" @keydown.enter.prevent="startAudioMerge"></label>
              <div class="merge-cover-field">
                <strong>封面 <small v-if="audioCoverCandidates.length">已识别当前目录 {{ audioCoverCandidates.length }} 张图片</small></strong>
                <div v-if="audioCoverCandidates.length" class="merge-cover-candidates">
                  <button v-for="candidate in audioCoverCandidates" :key="candidate.id" type="button" :class="{active:audioMerge.coverFileId===candidate.id}" :title="candidate.name" @click="selectDirectoryCover(candidate)"><img :src="thumbSRC(candidate)" :alt="candidate.name"><span>{{ candidate.name }}</span></button>
                </div>
                <button type="button" class="merge-cover-picker" @click="chooseAudioCover">
                  <img v-if="audioMerge.coverPreview" :src="audioMerge.coverPreview" alt="音频封面预览">
                  <span v-else>＋</span>
                  <div><b>{{ audioMerge.coverName||'上传其他封面' }}</b><small>{{ audioMerge.coverPreview?'点击可换成本地图片':'没有合适图片时从设备上传' }}</small></div>
                </button>
                <button v-if="audioMerge.coverPreview" type="button" class="merge-cover-remove" @click="clearAudioCover">移除封面</button>
                <input ref="audioCoverInput" hidden type="file" accept="image/jpeg,image/png,image/webp,image/gif" @change="audioCoverChanged">
              </div>
              <p class="lossless-note"><strong>{{ audioMerge.format==='flac'?'字幕说明':'字幕与播放说明' }}</strong><template v-if="audioMerge.format==='flac'">FLAC 不支持内嵌字幕；已识别 {{ audioMergeSubtitleCount }} / {{ audioMerge.order.length }} 个同名 VTT，切换 ALAC 或 AAC 后会自动合并并写入字幕轨。</template><template v-else>将内嵌 {{ audioMergeSubtitleCount }} / {{ audioMerge.order.length }} 个同名 VTT；各段字幕会随音频顺序自动校准时间轴。浏览器无法解码 ALAC 时会临时启动 FFmpeg HLS 兼容流。</template></p>
            </section>
            <section class="merge-order-panel">
              <div class="merge-order-heading"><div><strong>播放顺序</strong><small>每个文件会保留为一个分节</small></div><span>{{ audioMerge.order.length }} 段 · {{ formatSize(audioMerge.order.reduce((sum,item)=>sum+item.size,0)) }}</span></div>
              <div class="merge-order-list">
                <article v-for="(item,index) in audioMerge.order" :key="item.id">
                  <b>{{ index+1 }}</b><div><strong :title="item.name">{{ item.name }}</strong><small>{{ formatSize(item.size) }}</small><span v-if="audioSubtitleFor(item)" class="merge-subtitle-match" :class="{disabled:audioMerge.format==='flac'}" :title="audioSubtitleFor(item)?.name"><i>CC</i>{{ audioMerge.format==='flac'?'已找到但 FLAC 不会打包':'将打包' }} · {{ audioSubtitleFor(item)?.name }}</span><span v-else class="merge-subtitle-match missing"><i>CC</i>未找到同名 .vtt</span></div>
                  <span class="merge-order-actions"><button :disabled="index===0" title="上移" aria-label="上移" @click="moveAudioMergeInput(index,-1)">↑</button><button :disabled="index===audioMerge.order.length-1" title="下移" aria-label="下移" @click="moveAudioMergeInput(index,1)">↓</button></span>
                </article>
              </div>
            </section>
          </div>
          <p v-if="audioMerge.error" class="form-error merge-error">{{ audioMerge.error }}</p>
          <footer><button class="secondary" @click="closeModal">取消</button><button class="primary" :disabled="audioMerge.busy" @click="startAudioMerge">{{ audioMerge.busy?'正在创建…':'开始合并' }}</button></footer>
        </template>
        <template v-else>
          <div v-if="!localMerge.picks.length" class="local-merge-picker">
            <div class="local-merge-picker-icon">♬</div>
            <h3>选择电脑上的音频目录</h3>
            <p>自动识别 WAV 音频、同名 VTT 字幕和封面图片，按自然顺序合并为无损 ALAC M4A。素材只暂存在服务器本地工作区，源文件不会进入对象存储。</p>
            <button type="button" class="primary" @click="chooseLocalMergeDir">选择音频目录…</button>
          </div>
          <div v-else class="local-merge-body">
            <div class="local-merge-dir"><span :title="localMerge.dirName||'本地目录'">▰ {{ localMerge.dirName||'本地目录' }}</span><button type="button" class="secondary" :disabled="localMerge.busy" @click="chooseLocalMergeDir">更换目录</button></div>
            <label>输出文件名（固定 ALAC 无损 .m4a）<input v-model="localMerge.name" maxlength="1024" placeholder="合并音频.m4a" :disabled="localMerge.busy" @keydown.enter.prevent="startLocalMerge"></label>
            <section class="merge-order-panel">
              <div class="merge-order-heading"><div><strong>播放顺序</strong><small>WAV 已按自然排序，每个文件保留为一个分节</small></div><span>{{ localMerge.order.length }} 段 · {{ formatSize(localMerge.total) }}</span></div>
              <div class="merge-order-list">
                <article v-for="(name,index) in localMerge.order" :key="name">
                  <b>{{ index+1 }}</b><div>
                    <strong :title="name">{{ name }}</strong><small>{{ formatSize(localFileByName(name)?.size||0) }}</small>
                    <span v-if="localSubtitleFor(name)" class="merge-subtitle-match" :title="localSubtitleFor(name)"><i>CC</i>将打包 · {{ localSubtitleFor(name) }}</span>
                    <span v-else class="merge-subtitle-match missing"><i>CC</i>未找到同名 .vtt</span>
                  </div>
                  <span class="merge-order-actions"><button :disabled="index===0||localMerge.busy" title="上移" aria-label="上移" @click="moveLocalMergeInput(index,-1)">↑</button><button :disabled="index===localMerge.order.length-1||localMerge.busy" title="下移" aria-label="下移" @click="moveLocalMergeInput(index,1)">↓</button></span>
                </article>
              </div>
            </section>
            <div class="merge-cover-field">
              <strong>封面 <small>已识别 {{ localCoverCandidates.length }} 张图片</small></strong>
              <div v-if="localCoverCandidates.length" class="merge-cover-candidates">
                <button v-for="candidate in localCoverCandidates" :key="candidate.name" type="button" :class="{active:localMerge.cover===candidate.name}" :title="candidate.name" :disabled="localMerge.busy" @click="selectLocalCoverFile(candidate.name)"><img :src="candidate.preview" :alt="candidate.name"><span>{{ candidate.name }}</span></button>
              </div>
              <p v-if="localMerge.coverPreview" class="local-cover-preview"><img :src="localMerge.coverPreview" alt="已选封面预览"><button v-if="localMerge.cover" type="button" class="merge-cover-remove" :disabled="localMerge.busy" @click="clearLocalCover">不使用封面</button></p>
              <p v-else class="local-cover-none">未选择封面{{ localCoverCandidates.length?`（${localCoverCandidates.length} 张图片均未命名为 cover / folder / front / album 等）`:'' }}</p>
            </div>
          </div>
          <p v-if="localMerge.error" class="form-error merge-error">{{ localMerge.error }}</p>
          <footer>
            <button class="secondary" @click="closeModal">取消</button>
            <button class="primary" :disabled="localMerge.busy||!localMerge.picks.length" @click="startLocalMerge">{{ localMerge.busy?'正在创建…':'上传并合并' }}</button>
          </footer>
        </template>
      </section>
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
    <ReaderView v-if="modal==='reader'&&readerFile" :file="readerFile" @close="closeModal" />
    <AppDialog v-if="dialog.open" :title="dialog.title" :message="dialog.message" :confirm-label="dialog.confirmLabel" :cancel-label="dialog.cancelLabel" :tone="dialog.tone" :input="dialog.input" :value="dialog.value" :placeholder="dialog.placeholder" @update:value="dialog.value=$event" @confirm="finishDialog(true)" @cancel="finishDialog(false)" />
    <div v-if="toast.text" class="toast" :class="toast.kind">{{ toast.text }}</div>
  </div>
</template>

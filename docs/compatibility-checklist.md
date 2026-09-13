# Rust 迁移兼容性恢复清单

本清单的规范是 Rust 迁移前最后一个已确认正常的生产实现，而不是当前 Rust 实现的现状。任何旧版已有入口、接口、页面、交互、状态、文案、图标或视觉层级，都必须先在旧版确认，再在新版以相同操作验证；只有旧版行为、新版行为、差异、修复和回归验证全部有记录，项目才算完成。

本阶段不新增产品功能。对旧版已有行为，只做兼容恢复；旧版明显的安全缺陷可以修复，但不得改变正常用户路径。

## 0. 状态约定

- `[ ]` 尚未完成旧版/新版双向验证。
- `[R]` 已确认 Rust 版回退，待恢复。
- `[P]` 已恢复并通过自动化和实际浏览器验证。
- `[B]` 测试基础设施或测试选择器异常，不能作为功能通过/失败结论；仍需另行手工验证。
- 每个条目都要补充证据：旧版操作结果、新版操作结果、差异、代码位置、测试命令、浏览器验证结果。
- “旧版确认”可以由旧版源码、旧版运行时和旧版 E2E 共同构成；不能仅凭当前页面推断旧版没有某项功能。

## 1. 基线和双版本运行证据

### 1.1 Git 基线

- `[P]` Rust 迁移开始 commit：`c514a74`（`refactor(rust): 建立 Cargo workspace 与前后端共享 core crate`）。该 commit 的父 commit 是旧技术栈仍完整存在的 `3a18bde0cb3278db37fc4e98f1f86297897774c`。
- `[P]` reference implementation：`3a18bde`（`refactor(ui): 精简移动端分类抽屉为一级入口`，2026-09-12），即迁移启动前最后一个旧版链路 tip；包含完整 `cmd/server`、`internal`、`data-plane` 和 `web`。
- `[P]` 当前 Rust main：`e9b6202`（`docs(migration): record green CI publish`，2026-09-13）。
- `[P]` 初始工作区在本清单创建前干净；本清单必须先独立提交，再进入功能恢复提交。

### 1.2 隔离运行实例

- `[P]` old worktree：`/tmp/revaro-old`，detached `3a18bde`，服务 `http://127.0.0.1:18080`。
- `[P]` new worktree：`/tmp/revaro-new`，detached `e9b6202`，服务 `http://127.0.0.1:18081`。
- `[P]` old/new 使用不同的 `APP_DATA_DIR`、`APP_WORK_DIR`、端口和管理员 cookie；不得交叉使用数据库、上传临时文件或任务队列。
- `[P]` 两个实例均 `GET /healthz` 返回 200；old 使用迁移前 Go server + data-plane，new 使用 Rust server + Rust wasm bundle。
- `[B]` Docker/Compose 由于环境没有 `/var/run/docker.sock` 无法启动；已切换为本机构建、独立进程和 Chromium 实测，不因此跳过浏览器验证。
- `[P]` old 前端 `npm ci && npm run build`、data-plane release build、Go server build 均通过。
- `[P]` new `cargo build --locked --release -p revaro-server`、`cargo xtask web-build` 均通过。
- `[P]` new 当前已有 smoke E2E：`tests/e2e` 3/3 通过（Rust media、TXT/EPUB reader）。
- `[B]` old 原有 E2E：36/38 通过；`web/e2e/files.spec.ts` 的两个用例在等待 `.file-card` 的“选择项目”按钮时超时，虽然上传本身完成，故标为旧测试选择器/运行时异常，不把它当作 Rust parity 结论。必须在清单对应的手工选择、多选、ZIP 条目中继续验证。
- `[P]` 已保存初始浏览器截图：`/tmp/revaro-old-initial.png`、`/tmp/revaro-new-initial.png`；后续每个模块保存同一 viewport、同一数据状态的 old/new 截图或 trace。

### 1.3 每个条目的固定验收顺序

1. 从旧源码反向清点入口、组件、API 调用和状态。
2. 在 old 实例使用真实点击、输入、键盘、触摸、拖放和空白区域操作确认结果。
3. 在 new 实例以相同数据、相同 viewport、相同操作重做；记录 DOM、网络、截图、控制台错误和结果。
4. 明确差异后恢复实现，不以“功能类似”作为通过标准。
5. 对逻辑模块补充 Rust 单元/集成测试、浏览器 E2E 或回归用例，并运行 `cargo xtask check`、clippy、test、wasm build。
6. 再次逐操作对照 old/new，只有完整链路通过才标 `[P]`，并为该模块单独提交。

## 2. 启动、认证和全局壳层

| 状态 | 条目 | 旧版规范与验收点 | 当前 Rust 初检 |
|---|---|---|---|
| `[ ]` | 启动画面 | 首屏 splash、logo、spinner、加载到登录/主界面的时序、网络慢和失败状态一致；启动过程不闪出错误主界面。 | 待双版本逐帧/逐状态确认 |
| `[ ]` | 登录 | 用户名/密码输入、回车提交、按钮 loading/disabled、错误文案、焦点、密码可见性（如有）、重复提交和网络错误一致。 | `LoginView` 已存在，完整行为待验证 |
| `[ ]` | TOTP 登录 | 需要二次验证时的输入、回退、错误、重试、恢复码路径和 session 建立一致。 | API/页面 caller 待补齐 |
| `[ ]` | 会话检查 | `/api/auth/me`、刷新页面、已过期 cookie、401 后回登录页且不遗留旧数据。 | 初检仅有 session fetch |
| `[R]` | 账户入口 | 顶栏账户按钮应打开“账户设置”而不是直接退出登录；用户名、头像、菜单文案和层级一致。 | 当前按钮 title/文案为“退出登录”，点击触发 logout |
| `[ ]` | 账户设置 | 账户资料、用户名修改、头像读取/上传/删除、密码修改、TOTP 状态/setup/enable/recovery/delete、成功/失败/取消/关闭行为一致。 | 页面模块缺失，API caller 不完整 |
| `[ ]` | 退出登录 | 只在账户设置或移动端工具菜单的明确“退出登录”动作触发；成功后清空 session/任务/页面状态并回登录页。 | 当前误绑到账户入口，待拆分 |
| `[ ]` | 全局错误/Toast | 成功、失败、权限过期、冲突、网络断开、复制剪贴板失败的 toast 文案、颜色、时长、关闭方式和堆叠顺序一致。 | 待验证 |
| `[ ]` | 全局键盘 | Escape 关闭当前最内层弹窗/菜单，Enter 提交可提交表单，Tab 焦点不越界；浏览器后退的 modal/folder 语义一致。 | 待验证 |

## 3. 顶栏、任务中心和系统状态

| 状态 | 条目 | 旧版规范与验收点 | 当前 Rust 初检 |
|---|---|---|---|
| `[ ]` | Logo/回到根目录 | 桌面和移动端 logo 图标、`回到我的文件` aria/title、点击后路径、active 状态一致。 | 初检有简化 logo，待对照 |
| `[R]` | 任务中心入口 | 顶栏独立任务中心图标/summary，入口位置、图标、数量/状态提示、点击展开和再次点击关闭一致；不能被上传入口替换。 | 当前任务中心入口缺失/被上传摘要占用 |
| `[ ]` | 任务面板分组 | 活跃、已完成/已取消、失败分组；上传/归档解压/字幕任务标签、进度、状态中文文案、平均进度和空状态一致。 | Rust `TaskController` 有基础 API，面板 parity 待恢复 |
| `[ ]` | 任务操作 | 取消、重试、清除已完成、归档密码输入、任务详情、失败错误、超过四项时“显示更多”、任务流实时更新一致。 | caller/面板待恢复 |
| `[ ]` | 任务面板交互 | 面板不被背景遮挡、点击面板不关闭、点空白关闭、Esc 关闭、点击入口切换、loading/error/empty 一致。 | 待验证 |
| `[R]` | 系统状态入口 | 在线/状态球可点击；`aria-label=打开系统状态`、title=`系统状态`、颜色/ok 状态和位置一致。 | 已确认当前入口不可点击/无旧版面板 |
| `[ ]` | 系统状态面板 | EventSource `/api/system/status/stream` 更新状态；DB、存储、缓存三张纵向卡片，状态 badge、详情/错误/加载一致；旧版没有“任务/清理队列/备份”伪卡片和刷新按钮。 | server API 存在，Rust UI/caller 缺失 |
| `[ ]` | 系统状态关闭 | 点空白、Esc、重复点击、401/断线/重连/服务异常状态一致，关闭后 SSE 清理。 | 待恢复 |
| `[ ]` | 回收站入口 | 顶栏回收站图标、title=`回收站`、aria、点击进入 trash 路由、数量/空状态和返回根目录一致。 | 基础入口存在，视觉和全局位置待对照 |
| `[ ]` | 移动端顶栏 | 状态球仍可用；头像/工具菜单包含任务中心、回收站、账户设置、退出登录等旧版项目；不出现旧版明确禁止的 `打开任务与工具菜单` 旧入口；遮罩/外部点击/Esc 一致。 | 当前移动端完整行为待验证 |

## 4. 侧栏、分类入口和路径树

| 状态 | 条目 | 旧版规范与验收点 | 当前 Rust 初检 |
|---|---|---|---|
| `[R]` | 五个一级分类 | 侧栏入口顺序、图标和文案为：书架、图片、视频、音乐、文件；每项 active/current、点击路由和返回行为一致。 | 当前只显示“我的文件” |
| `[ ]` | 分类数据 | 分类数量、空状态、刷新/loading/error、书籍/图片/视频/音乐/普通文件各自对应 `/api/library` 视图一致。 | library caller/UI 缺失 |
| `[ ]` | 分类路径 | 分类主项和展开控制、路径树/文件树、当前路径高亮、展开/收起、加载/空/错误、点击文件夹进入对应分类路径一致。 | Sidebar path/tree 模块缺失 |
| `[ ]` | 分类持久化 | `revaro:sidebar:collapsed`、`revaro:sidebar:expanded` 的值、恢复时机和坏值处理一致。 | 待验证 |
| `[ ]` | 桌面侧栏折叠 | 折叠 rail、展开按钮、tooltip/aria、内容宽度/动画、刷新后恢复、当前页仍可识别一致。 | 当前简化 |
| `[ ]` | 移动端分类抽屉 | 宽度 `min(300px,78vw)`；只显示一级入口（书/图/影/音/文件/回收站），不显示树、数量或 chevron；50px 行高；浮动 handle、backdrop、点击空白、Esc、打开/关闭跟随一致，内容不位移。 | 旧版 commit `3a18bde` 明确记录；Rust 待逐项恢复 |
| `[ ]` | 侧栏图标 | Lucide 风格、stroke、大小、对齐、active/hover/disabled 颜色和五类具体图标与旧版一致，不用“看起来相似”的替代图标。 | 已知部分图标不一致 |
| `[ ]` | 回收站 footer | 桌面/移动端位置、图标、active、点击和 trash empty 状态一致。 | 基础入口存在，需完整验证 |

## 5. 文件浏览、路由和全局内容区

| 状态 | 条目 | 旧版规范与验收点 | 当前 Rust 初检 |
|---|---|---|---|
| `[R]` | 根目录内容头 | `我的文件` 标题、当前路径 nav、项目数/文件数/大小三项 metadata 的文案、间距和层级一致。 | 基本内容存在但头部简化 |
| `[R]` | 面包屑 | `当前路径` nav、根和各级名称、Lucide chevron-right 分隔、当前项样式、点击中间级、超长路径横向滚动、键盘/触摸行为一致。 | 当前使用退化的 literal `/` 分隔 |
| `[ ]` | 文件夹路由 | `/`、`/f/{id}`、`/library/{book|image|video|audio|file}`、分类下 `/f/{folder}` 的地址、刷新、直接打开、无效 id、权限错误和回退一致。 | 当前有部分 folder/trash 状态，分类路由缺失 |
| `[ ]` | 深链接 | `/read/{fileId}` 打开旧版阅读器；媒体/文件深链接、登录后回到目标、无效深链接错误/返回一致。 | reader 基本存在，完整路由待验 |
| `[ ]` | 浏览器历史 | 文件夹进入 pushState；返回/前进恢复文件夹/分类；先关闭 modal 再回退页面；stale request 不覆盖新路径。 | 待对照 |
| `[ ]` | 网格/列表切换 | 默认值、按钮图标/tooltip/active、内容布局、滚动、刷新后状态和移动端响应式行为一致。 | 基础切换存在 |
| `[ ]` | loading/empty/error | 首次加载、切换路径、网络失败、空根、空分类、空回收站、重试按钮、旧内容保留策略和文案一致。 | 待验证 |
| `[ ]` | 拖放 | 桌面拖入文件/文件夹、拖动经过/离开/放下、overlay、非法目标、重复文件、取消和上传结果一致。 | 上传控制器有基础实现，UI 状态待验证 |
| `[ ]` | 响应式布局 | 桌面、平板、390px 手机宽度下内容区、侧栏、顶栏、工具栏、对话框和滚动容器的宽高/层级一致。 | 待对照截图 |

## 6. 文件项、图标和选择/操作菜单

| 状态 | 条目 | 旧版规范与验收点 | 当前 Rust 初检 |
|---|---|---|---|
| `[ ]` | 文件卡/行 | 文件名、大小、类型、更新时间、目录/媒体/文档标识、thumbnail/cover、fallback 和截断规则一致；方块与列表都验证。 | Rust 有 FileTile/rows 基础，视觉待对照 |
| `[R]` | 图标系统 | 文件夹、文本文档、EPUB、图片、音频、视频、归档、未知文件的旧版图标路径、stroke、颜色、尺寸、背景和状态叠加一致。 | 已确认部分图标与旧版不同 |
| `[ ]` | hover/active/disabled | 卡片 hover、键盘 focus、选中 active、不可用、loading、任务中覆盖层、错误状态和 pointer 行为一致。 | 待验证 |
| `[ ]` | 选择入口 | 每个可选 item 始终有旧版语义的 `选择项目` 控件；点击不打开项目，选中后工具栏更新；取消选择/全选/跨页状态一致。 | 初检有空 selection buttons，但语义/行为待对照 |
| `[ ]` | 触摸选择 | 480ms 长按选中；选择模式下轻触切换；普通轻触仍打开项目；滚动不误选；触摸反馈一致。 | 当前 FileTile 未完整保留旧版 long-press 语义 |
| `[ ]` | 右键/更多菜单 | 文件/文件夹右键或 more 入口、菜单锚点、菜单项顺序、点空白关闭、Esc、边缘翻转和 item disabled 状态一致。 | 待验证 |
| `[ ]` | 打开动作 | 目录进入；可编辑文本进入 editor；EPUB 进入 reader；图片/音频/视频进入 preview；未知类型下载/预览策略、回收站只读行为一致。 | Rust 有部分 open logic，完整矩阵待验 |
| `[ ]` | SelectionToolbar | 选中计数/总大小、清除、全选、打开、下载、分享、重命名、移动、删除、恢复、永久删除、归档解压等按钮的出现条件和文案一致。 | 当前缺少旧版 open/extract/download/share 等动作 |

## 7. 上传入口、队列和任务联动

| 状态 | 条目 | 旧版规范与验收点 | 当前 Rust 初检 |
|---|---|---|---|
| `[R]` | 上传入口 | 文件浏览头部独立上传菜单，含“上传文件”“上传文件夹”；桌面按钮、移动端下拉、图标、点击外部/Esc 关闭一致。 | 当前头部缺少旧版上传菜单，顶栏简化为“上传”摘要 |
| `[ ]` | 文件选择 | 单/多文件选择、文件夹选择、取消、空选择、同名文件、路径/相对目录保留、浏览器能力差异一致。 | 待验证 |
| `[ ]` | 拖放上传 | 文件/目录拖放、目标目录、overlay、非法文件、重复上传和完成后列表刷新一致。 | 基础 controller 存在 |
| `[ ]` | 创建 upload | `POST /api/uploads` 的 chunk/single 模式、大小、类型、目标目录、断点信息和错误处理一致。 | API caller 部分存在 |
| `[ ]` | 上传进度 | 单文件/多文件进度、速度、剩余时间、并发、pending/uploading/completing/completed/failed/cancelled 状态和文案一致。 | Rust queue 有实现，需逐操作比较 |
| `[ ]` | 上传队列 | 队列面板的展开/收起、排序、显示更多、取消、重试、失败原因、完成清理和与任务中心的分工一致。 | 待恢复 parity |
| `[ ]` | 断点续传 | 刷新/关闭后使用 `revaro.uploads.v1` 恢复；分片获取、记录、complete、abort 和过期记录清理一致。 | controller/API 存在，持久化和 UI 待验 |
| `[ ]` | 上传完成 | 列表/分类/统计刷新，任务中心更新，toast，当前路径和重复文件结果一致。 | 待验证 |

## 8. 新建、重命名、移动、复制、删除和回收站

| 状态 | 条目 | 旧版规范与验收点 | 当前 Rust 初检 |
|---|---|---|---|
| `[ ]` | 新建文件夹 | 入口、输入聚焦、空名/非法名/冲突、Enter/Esc、loading、成功刷新和错误文案一致。 | 基础入口存在 |
| `[R]` | 新建文档 | 桌面直接入口和创建菜单中的“新建文档”、默认名 `未命名文档.md`、创建后进入 editor、取消/失败一致。 | 当前只有“新建文件夹” |
| `[ ]` | 重命名 | 单选条件、输入初值/扩展名规则、冲突、空白、Enter/Esc、PATCH 结果和列表更新一致。 | 基础 API/UI 部分存在 |
| `[ ]` | 移动 | DirectoryPicker 面包屑、实时目录浏览、加载/错误/空、排除自身/子目录、目标选中、确认/取消/冲突和 PATCH 结果一致。 | 简化 dialog，需对照 |
| `[ ]` | 复制 | 目标选择、目录/文件、同名处理、任务或立即结果、完成刷新和错误一致。 | API 存在，UI 行为待验 |
| `[ ]` | 删除 | 确认文案、单项/多项、目录、取消、loading、移入回收站、selection 清理和列表刷新一致。 | 基础实现待验 |
| `[ ]` | 回收站查看 | 列表/网格、原路径/删除时间/大小、空状态、打开限制、恢复/永久删除入口一致。 | 基础 trash fetch 存在 |
| `[ ]` | 恢复 | 单项/多项恢复、原位置可用/冲突、成功/失败文案、刷新和 selection 一致。 | 基础 API/UI 待验 |
| `[ ]` | 永久删除 | 单项确认、清空回收站确认、不可恢复警告、loading/失败/成功及列表更新一致。 | 基础 API/UI 待验 |
| `[ ]` | 对话框通用行为 | backdrop、Esc、焦点、按钮顺序、危险色、空输入 disabled、提交中禁用和错误保留输入一致。 | `AppDialog` parity 待恢复 |

## 9. 文本文档查看与编辑器

| 状态 | 条目 | 旧版规范与验收点 | 当前 Rust 初检 |
|---|---|---|---|
| `[R]` | 编辑器入口 | md/markdown/txt/yaml/yml/json/toml/ini/conf/log/csv 等旧版可编辑扩展名，点击文件打开 editor；回收站内容只读。 | 当前文本编辑器消失 |
| `[ ]` | 新文档编辑 | 默认文件名、初始内容、editor modal/页面尺寸、关闭、保存、创建失败和成功返回一致。 | `/api/documents` caller 缺失 |
| `[ ]` | 读取 | `/content`、编码/大文件错误、loading/error、只读提示、滚动和文本保持一致。 | API caller 缺失 |
| `[ ]` | 编辑模式 | textarea、编辑/分栏/预览 tabs，Markdown `marked` + DOMPurify 结果、光标/滚动、预览错误和非 Markdown 隐藏 tabs 一致。 | 模块缺失 |
| `[ ]` | 保存 | PUT content、etag/冲突、busy/disabled、成功 toast、列表 metadata、关闭后刷新和失败重试一致。 | server route 有，UI 缺失 |
| `[ ]` | 未保存关闭 | dirty 检测、关闭/浏览器后退确认、取消返回编辑、确认丢弃、Esc/backdrop 行为一致。 | 模块缺失 |
| `[ ]` | 编辑器视觉 | 标题、文件名、工具栏、图标、按钮文案、编辑区字体/行高、readonly 和错误层级与旧版一致。 | 待恢复 |

## 10. EPUB/TXT 阅读器

| 状态 | 条目 | 旧版规范与验收点 | 当前 Rust 初检 |
|---|---|---|---|
| `[ ]` | TXT 打开 | `/read/{id}`、加载、分页/分栏、返回、书名、实时进度、刷新/深链恢复一致。 | Rust smoke 有 TXT，需逐项 old/new |
| `[ ]` | EPUB 打开 | manifest/flow/chunk、封面、章节、样式、图片/assets、首屏和错误回退一致。 | Rust smoke 有 EPUB，需逐项 old/new |
| `[ ]` | 顶栏 | 返回按钮、居中标题、进度 ring/文字、沉浸式工具显隐、工具不导致正文重排一致。 | 基础 reader 存在 |
| `[ ]` | 翻页 | 上一页/下一页、中心区域、键盘左右/空格、边界不崩、连续翻页无跳页、横竖屏重排位置保持一致。 | 待完整回归 |
| `[ ]` | 目录 | 底栏进入 TOC drawer、父子目录、文本 locator、fragment、未加载 chunk 自动加载、跳转后 readingAnchor 一致。 | 待完整回归 |
| `[ ]` | 阅读设置 | 字号、行距、主题、背景、沉浸式模式、弹层覆盖正文、图标居中、纯客户端重排零 chunk 请求一致。 | 待完整回归 |
| `[ ]` | 进度和缓存 | `revaro-reader-prefs`、服务端 `/book/progress`、anchor、manifest/chunk L2 cache、重开零重复请求、版本变化重取一致。 | Rust reader/cache 存在，待对照 |
| `[ ]` | 阅读器响应式 | 桌面/窄屏/触摸、手势与滚动冲突、旋转、空白点击和 drawer 关闭一致。 | 待验证 |

旧版 `web/e2e/reader-flow.spec.ts` 和 `reader-real-epub.spec.ts` 的全部行为场景都属于本节，不得只以当前 3 个 smoke case 通过代替：包括窗口预取、目录锚点、回退到开头、跨 spine、父级目录不回弹、随机 seek 稳定、页边界、旋转、图片章节定位、缓存复开和视觉覆盖层。

## 11. 图片、音频和视频查看器

| 状态 | 条目 | 旧版规范与验收点 | 当前 Rust 初检 |
|---|---|---|---|
| `[ ]` | 图片查看 | `/preview`、loading/error、画廊上一张/下一张、缩略图、计数、实际大小/适应窗口、放大缩小、双击、滚轮、拖动边界、stage 点击显隐 chrome、键盘 `←/→/+/-/0/1` 一致。 | Rust media 基础存在 |
| `[ ]` | 图片触摸 | 双指缩放、拖动、手势取消不误翻页、边界限制、旋转/重排状态保持一致。 | 待验证 |
| `[ ]` | 图片更多菜单 | 下载、移动、复制、信息等 menu 的位置、点击外部/Esc、loading/error 和返回行为一致。 | 待验证 |
| `[ ]` | 音频播放器 | `/audio` 元数据、封面 fallback、章节、上一/下一章、时间跳转、进度、播放/暂停、loading/error/retry、一首/多首行为一致。 | Rust audio 基础存在 |
| `[ ]` | 音频持久化 | `revaro-audio-volume`、`revaro-audio-muted`、`revaro-audio-position:{id}`，音量滑块、静音、键盘操作和刷新恢复一致。 | 待验证 |
| `[ ]` | 视频播放器 | Range/直接 preview、poster thumbnail、播放/暂停、进度拖动与 seek preview/commit、时间显示、音量/静音、速度、全屏、控制条显隐和自动隐藏一致。 | Rust video 基础存在 |
| `[ ]` | 视频字幕 | `/video` metadata、VTT subtitle、选择/关闭字幕、字幕不抖动、加载/解析错误和移动端布局一致。 | 待验证 |
| `[ ]` | 视频持久化 | `revaro-video-volume`、`revaro-video-rate`、`revaro-video-position:{id}`，刷新/重开恢复准确且无错误跳 seek。 | 待验证 |
| `[ ]` | 媒体操作 | 播放器设置/更多中的下载、移动、复制、信息、reanalyze（旧版入口若出现）、关闭/返回和任务刷新一致。 | 待验证 |
| `[ ]` | 不支持/损坏媒体 | unsupported 原文件直接显示旧版错误而不是空白；重试、返回、控制条、错误文案/图标一致。 | 待验证 |

## 12. 下载、Range、预览、分享和归档任务

| 状态 | 条目 | 旧版规范与验收点 | 当前 Rust 初检 |
|---|---|---|---|
| `[ ]` | 单文件下载 | `/api/files/{id}/download`、文件名、Content-Disposition、下载菜单/按钮、loading/error 和回收站策略一致。 | 当前 UI 只有部分 direct link |
| `[ ]` | Range 下载/播放 | bytes range、206/416、Content-Range、HEAD/缓存/大文件、音视频 seek 及断点行为与旧版一致。 | server tests 有，浏览器需验证 |
| `[ ]` | 预览 | `/preview` content type、图片/音频/视频/文本行为、鉴权、缓存、错误、thumbnail fallback 一致。 | caller 分散，待矩阵验证 |
| `[ ]` | 多选 ZIP | 选择多个文件后一次 prepare、进度/任务、一次性 token 下载、CSP `frame` 约束和失败处理一致。 | 当前 selection 缺少动作 |
| `[ ]` | 归档解压 | 支持格式、密码输入任务、冲突/错误、取消、任务中心、完成刷新和安全路径行为一致。 | Rust UI/API caller 缺失或不完整 |
| `[ ]` | 分享读取 | 分享状态读取、已存在/不存在、过期/权限、链接显示、复制失败和关闭一致。 | server route 有，UI 缺失 |
| `[ ]` | 分享创建 | 单文件创建链接、复制 URL、成功/失败、按钮 loading/disabled、公开页面行为和文案一致。 | UI 缺失 |
| `[ ]` | 分享撤销 | 二次确认（如旧版有）、DELETE、成功/失败、状态刷新、旧链接失效一致。 | UI 缺失 |
| `[ ]` | 公开分享页 | `GET /s/{token}` 的文件信息、下载/预览、过期/无效 token、响应头和移动端布局一致。 | 待 old/new 实测 |

## 13. 全部旧版 API、调用方和 Rust 版调用覆盖

下面按旧版 `internal/server/server.go` 的认证路由登记，逐条追踪“旧版前端调用方 → 当前 Rust handler/API → 当前 Rust UI caller”。“后端已迁移”不等于通过；必须确认当前页面实际触发调用并且用户可完成旧版操作。

| 状态 | 旧版 API | 旧版调用方/用途 | Rust handler/API 与当前 caller 初检 |
|---|---|---|---|
| `[ ]` | `GET /healthz` | 启动/监控 | handler 存在；两实例 200，需纳入部署验收 |
| `[ ]` | `GET /readyz` | 就绪检查 | handler/响应和未就绪语义待验证 |
| `[ ]` | `GET /s/{token}` | 公开分享页 | handler 存在；Rust/浏览器全链路待验 |
| `[ ]` | `POST /api/auth/login` | LoginPage | Rust `login()` 存在；表单/TOTP/错误待验 |
| `[ ]` | `POST /api/auth/logout` | 顶栏账户菜单明确退出 | Rust `logout()` 存在；当前被错误绑定到账户入口 |
| `[ ]` | `GET /api/auth/me` | App 启动/刷新 session | Rust `fetch_session()` 存在；401/回跳待验 |
| `[ ]` | `PATCH /api/auth/credentials` | 账户设置凭据 | Rust UI/API caller 缺失 |
| `[ ]` | `PATCH /api/auth/password` | 账户设置改密码 | Rust UI/API caller 缺失 |
| `[ ]` | `GET /api/auth/totp` | 账户设置 TOTP 状态 | Rust UI/API caller 缺失 |
| `[ ]` | `POST /api/auth/totp/setup` | 账户设置 setup | Rust UI/API caller 缺失 |
| `[ ]` | `POST /api/auth/totp/enable` | 账户设置启用 | Rust UI/API caller 缺失 |
| `[ ]` | `POST /api/auth/totp/recovery-codes` | 账户设置恢复码 | Rust UI/API caller 缺失 |
| `[ ]` | `DELETE /api/auth/totp` | 账户设置关闭 TOTP | Rust UI/API caller 缺失 |
| `[ ]` | `GET /api/profile/avatar` | 顶栏/账户设置头像 | Rust UI/API caller 缺失 |
| `[ ]` | `PUT /api/profile/avatar` | 账户设置上传头像 | Rust UI/API caller 缺失 |
| `[ ]` | `DELETE /api/profile/avatar` | 账户设置删除头像 | Rust UI/API caller 缺失 |
| `[ ]` | `PATCH /api/profile/username` | 账户设置改用户名 | Rust UI/API caller 缺失 |
| `[ ]` | `GET /api/storage/stats` | 全局/账户/存储信息 | Rust UI/API caller 缺失 |
| `[ ]` | `GET /api/library` | 书架/图片/视频/音乐/文件分类 | Rust server route/type 有；Rust UI caller 缺失 |
| `[ ]` | `GET /api/library/all` | 分类全量/系列/相册等 | Rust server route/type 有；Rust UI caller 缺失 |
| `[ ]` | `GET /api/library/counts` | 侧栏分类数量 | Rust server route/type 有；Rust UI caller 缺失 |
| `[ ]` | `GET /api/system/status` | 系统状态初次读取 | Rust type/handler 有；UI caller 缺失 |
| `[ ]` | `GET /api/system/status/stream` | 系统状态 SSE | Rust handler 有；UI/EventSource caller 缺失 |
| `[ ]` | `GET /api/events` | 任务实时 SSE | Rust `TaskController` 有基础连接；面板联动待验 |
| `[ ]` | `GET /api/tasks` | 任务中心初始/刷新 | Rust `fetch_tasks()` 有；完整 UI/详情待验 |
| `[ ]` | `GET /api/tasks/{id}` | 任务详情/归档等待 | Rust UI caller 缺失或待确认 |
| `[ ]` | `POST /api/tasks/{id}/cancel` | 任务中心取消 | Rust `cancel_task()` 有；按钮/状态待验 |
| `[ ]` | `POST /api/tasks/{id}/retry` | 任务中心重试 | Rust `retry_task()` 有；按钮/状态待验 |
| `[ ]` | `POST /api/tasks/{id}/input` | 密码/用户输入 | Rust `input_task()` 有；归档交互待验 |
| `[ ]` | `DELETE /api/tasks/{id}` | 清除完成任务 | Rust `delete_task()` 有；分组/确认待验 |
| `[ ]` | `GET /api/files/{id}` | 文件详情/进入目录 | Rust `fetch_file()` 有 |
| `[ ]` | `GET /api/files/{id}/children` | 文件夹内容/目录选择器 | Rust `fetch_children()` 有；DirectoryPicker caller 待恢复 |
| `[ ]` | `GET /api/files/{id}/download` | 单文件/媒体下载 | server route 有；UI 调用不完整 |
| `[ ]` | `POST /api/files/batch-download/prepare` | 多选 ZIP | server route 有；Rust UI/API caller 缺失 |
| `[ ]` | `GET /api/files/batch-download/{token}` | ZIP token 下载 | server route 有；Rust UI caller 缺失 |
| `[ ]` | `GET /api/files/{id}/preview` | 图片/音频/视频/文件预览 | media 组件部分使用；完整入口/Range 待验 |
| `[ ]` | `GET /api/files/{id}/audio` | 音频 metadata/章节 | Rust `fetch_audio()` 有 |
| `[ ]` | `GET /api/files/{id}/video` | 视频 metadata/subtitles | Rust `fetch_video()` 有 |
| `[ ]` | `POST /api/files/{id}/media/reanalyze` | 媒体重新分析 | server route 有；Rust UI caller 待确认 |
| `[ ]` | `GET /api/files/{id}/video/subtitles/{subtitle}` | 视频字幕文件 | server route 有；VideoPlayer caller 待确认 |
| `[ ]` | `GET /api/files/{id}/media/progress` | 音视频进度恢复 | Rust `fetch_media_progress()` 有 |
| `[ ]` | `PUT /api/files/{id}/media/progress` | 音视频进度保存 | Rust `save_media_progress()` 有 |
| `[ ]` | `GET /api/files/{id}/content` | 文本编辑器读取 | server route 有；Rust UI/API caller 缺失 |
| `[ ]` | `PUT /api/files/{id}/content` | 文本编辑器保存/etag | server route 有；Rust UI/API caller 缺失 |
| `[ ]` | `GET /api/files/{id}/book` | EPUB/TXT metadata | Rust `fetch_book()` 有 |
| `[ ]` | `GET /api/files/{id}/book/assets/{index}` | EPUB 资源 | reader caller 待逐请求验证 |
| `[ ]` | `GET /api/files/{id}/book/cover` | EPUB cover | reader/UI fallback 待验 |
| `[ ]` | `GET /api/files/{id}/book/progress` | reader progress | Rust `fetch_book_progress()` 有 |
| `[ ]` | `PUT /api/files/{id}/book/progress` | reader progress save | Rust `save_book_progress()` 有 |
| `[ ]` | `GET /api/files/{id}/book/flow` | reader manifest/flow | Rust `fetch_book_flow()` 有 |
| `[ ]` | `GET /api/files/{id}/book/flow/chunks/{index}` | reader window/cache | Rust `fetch_book_chunk()` 有 |
| `[ ]` | `GET /api/files/{id}/thumbnail` | 卡片/视频 poster/cover | Rust UI 部分直接构造 URL，fallback 待验 |
| `[ ]` | `GET /api/files/{id}/share` | 分享状态 | server route 有；Rust UI/API caller 缺失 |
| `[ ]` | `POST /api/files/{id}/share` | 创建分享 | server route 有；Rust UI/API caller 缺失 |
| `[ ]` | `DELETE /api/files/{id}/share` | 撤销分享 | server route 有；Rust UI/API caller 缺失 |
| `[ ]` | `POST /api/directories` | 新建文件夹 | Rust `create_directory()` 有；完整表单待验 |
| `[ ]` | `POST /api/documents` | 新建文档 | Rust core/server route 有；Rust UI/API caller 缺失 |
| `[ ]` | `PATCH /api/files/{id}` | 重命名/移动 | Rust `patch_file()` 有；所有入口待验 |
| `[ ]` | `POST /api/files/{id}/copy` | 复制 | Rust `copy_file()` 有；DirectoryPicker caller 待验 |
| `[ ]` | `POST /api/files/{id}/extract` | 归档解压 | server route 有；Rust UI/API caller 缺失或待确认 |
| `[ ]` | `DELETE /api/files/{id}` | 移入回收站 | Rust `delete_file()` 有 |
| `[ ]` | `GET /api/trash` | 回收站列表 | Rust `fetch_trash()` 有 |
| `[ ]` | `DELETE /api/trash` | 清空回收站 | Rust `empty_trash()` 有 |
| `[ ]` | `POST /api/trash/{id}/restore` | 恢复 | Rust `restore_file()` 有 |
| `[ ]` | `DELETE /api/trash/{id}` | 永久删除 | Rust `purge_file()` 有 |
| `[ ]` | `POST /api/uploads` | 创建上传 | Rust `create_upload()` 有 |
| `[ ]` | `GET /api/uploads/{id}` | 上传状态/断点恢复 | Rust `fetch_upload()` 有 |
| `[ ]` | `PUT /api/uploads/{id}/data` | 单请求上传 | Rust API/controller caller 待验 |
| `[ ]` | `PUT /api/uploads/{id}/data/{part}` | 分片上传 | Rust `upload_part()` 有 |
| `[ ]` | `POST /api/uploads/{id}/parts` | 获取分片 URL | Rust `request_upload_parts()` 有 |
| `[ ]` | `PUT /api/uploads/{id}/parts/{part}` | 记录分片 | Rust `record_upload_part()` 有 |
| `[ ]` | `POST /api/uploads/{id}/complete` | 完成上传 | Rust `complete_upload()` 有 |
| `[ ]` | `DELETE /api/uploads/{id}` | 取消/中止上传 | Rust `abort_upload()` 有 |

## 14. 旧版页面/组件反向清点

以下旧版组件均必须有 Rust 等价入口或明确证明其行为已由等价组件覆盖，不得因为 Rust 版合并为单文件就从清单移除：

- `[ ]` `App.vue`：启动、认证、路由、history、拖放、modal stack、toast、reader/media 分流。
- `[ ]` `AppTopbar.vue`、`AppSidebar.vue`、`TaskCenter.vue`、`SystemStatus.vue`：全局壳层、任务、状态、分类导航。
- `[ ]` `LoginPage.vue`：登录、TOTP、错误和 loading。
- `[ ]` `FileBrowserHeader.vue`、`SelectionToolbar.vue`：路径、统计、视图、新建、上传、选择后动作。
- `[ ]` `FileGrid.vue`、`FileRows.vue`、`FileCard.vue`：两种布局、图标、缩略图、选择和 touch。
- `[ ]` `LibraryView.vue`、`BookShelf.vue`、`GalleryGrid.vue`、`BookCover.vue`：书架、系列、图片/视频 gallery、相册、音乐/文件分类。
- `[ ]` `DocumentEditor.vue`：文本查看/编辑/Markdown 分栏预览/保存/冲突。
- `[ ]` `MoveCopyDialog.vue`、`DirectoryPicker.vue`：移动/复制目标选择和排除规则。
- `[ ]` `AppDialog.vue`：确认/输入通用弹窗行为。
- `[ ]` `ShareDialog.vue`：创建、复制、撤销分享。
- `[ ]` `MediaPreview.vue`、`PreviewMenu.vue`、`AudioPlayer.vue`、`VideoPlayer.vue`、`VideoControls.vue`、`VideoStatusOverlay.vue`、`FullBleedProgress.vue`：三类媒体完整控制与状态。
- `[ ]` `Reader.vue`、`reader_cache`：TXT/EPUB 阅读、目录、分页、设置、进度和缓存。
- `[ ]` `SidebarPathTree.vue`、`SidebarFileTree.vue`、`SidebarDirectoryNode.vue`：桌面路径树与移动端隐藏规则。
- `[ ]` `ServiceCard.vue`、`StatusBadge.vue`、`Icon` 集合：系统状态卡片、状态 badge、全套图标。

## 15. 文案、图标、视觉层级和状态矩阵

所有下列状态都必须在 old/new 同一操作点比较截图和可访问性树：

- `[ ]` 正常、hover、focus、active、选中、disabled、loading、上传中、保存中、播放中、暂停、错误、重试、空列表、无权限、过期、断线、恢复中。
- `[ ]` 顶栏、侧栏、内容头、breadcrumb、工具栏、卡片/行、菜单、drawer、dialog、reader、media preview 的 z-index、遮罩透明度、点击命中区域和滚动归属。
- `[ ]` 所有中文文案、标点、大小写、数字格式、文件大小/时长格式、空状态、错误状态和按钮顺序。
- `[ ]` 所有旧版图标的语义、具体形状、尺寸、stroke/fill、颜色、对齐、tooltip/title/aria-label；重点核对任务中心、系统状态、账户设置、回收站、分类五图标、面包屑 chevron、列表/方块、更多、播放控制。
- `[ ]` 桌面键盘操作、移动端触摸/长按、点击空白、点击遮罩、Esc、浏览器 back/forward、焦点回收。
- `[ ]` 390×844 触摸 viewport、窄屏横向溢出、虚拟键盘/输入框、媒体/阅读器旋转和安全区域。
- `[ ]` CSP、控制台错误、无重复 SSE、EventSource/Fetch 取消、内存/监听器清理。

## 16. 自动化和提交门槛

- `[ ]` 为每个已恢复模块增加或更新 Rust unit/integration tests，以及对应 Chromium old/new parity E2E；测试必须操作用户入口，不只直接调用 API。
- `[ ]` old `web/e2e` 全部可执行；已知两个选择器异常要修正测试夹具或另建等价手工用例后再判定文件模块。
- `[ ]` new parity suite 覆盖旧版 `auth-status.spec.ts`、`files.spec.ts`、`library-ui.spec.ts`、`media-ui.spec.ts`、`mobile.spec.ts`、`reader-flow.spec.ts`、`reader-real-epub.spec.ts` 的行为集合。
- `[ ]` 每个逻辑完整模块单独提交，提交说明包含 checklist ID、旧版证据、新版证据和验证命令；不要把无关功能混入恢复提交。
- `[ ]` 每个模块提交前通过：`cargo xtask check`、`cargo clippy --workspace --all-targets --all-features -- -D warnings`、`cargo test --workspace`、`cargo xtask web-build`（或项目约定的等价 wasm build）。
- `[ ]` 最终只在本文件所有旧版功能条目均为 `[P]` 后，才宣布 Rust 迁移兼容恢复完成。

## 17. 模块提交记录

| 模块 | Checklist 范围 | commit | 自动测试 | old/new 浏览器证据 | 状态 |
|---|---|---|---|---|---|
| 基线与清单 | 1 | 待提交 | 构建/healthz/E2E 基线已记录 | `/tmp/revaro-old-initial.png`、`/tmp/revaro-new-initial.png` | 进行中 |
| 全局导航与 UI | 2–4 | 待提交 | 待执行 | 待生成 | 未开始 |
| 文件浏览与选择 | 5–6 | 待提交 | 待执行 | 待生成 | 未开始 |
| 上传与任务 | 7、3 | 待提交 | 待执行 | 待生成 | 未开始 |
| CRUD 与回收站 | 8 | 待提交 | 待执行 | 待生成 | 未开始 |
| 文档编辑器 | 9 | 待提交 | 待执行 | 待生成 | 未开始 |
| 阅读器 | 10 | 待提交 | 待执行 | 待生成 | 未开始 |
| 媒体 | 11 | 待提交 | 待执行 | 待生成 | 未开始 |
| 下载/分享/归档 | 12 | 待提交 | 待执行 | 待生成 | 未开始 |
| 全量 API caller 与最终视觉回归 | 13–16 | 待提交 | 待执行 | 待生成 | 未开始 |

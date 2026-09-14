# Rust 迁移兼容性恢复清单

本清单的规范是 Rust 迁移前最后一个已确认正常的生产实现，而不是当前 Rust 实现的现状。任何旧版已有入口、接口、页面、交互、状态、文案、图标或视觉层级，都必须先在旧版确认，再在新版以相同操作验证；只有旧版行为、新版行为、差异、修复和回归验证全部有记录，项目才算完成。

本阶段不新增产品功能。对旧版已有行为，只做兼容恢复；旧版明显的安全缺陷可以修复，但不得改变正常用户路径。

## 0. 状态约定

- `[ ]` 尚未完成旧版/新版双向验证。
- `[R]` 已确认 Rust 版回退，待恢复。
- `[P]` 已恢复并通过自动化和实际浏览器验证；若唯一差异是本清单明确记录的旧版安全缺陷修复，也标为 `[P]`，不得把该例外隐藏在“功能类似”描述里。
- `[B]` 测试基础设施或测试选择器异常，不能作为功能通过/失败结论；仍需另行手工验证。
- 每个条目都要补充证据：旧版操作结果、新版操作结果、差异、代码位置、测试命令、浏览器验证结果。
- “旧版确认”可以由旧版源码、旧版运行时和旧版 E2E 共同构成；不能仅凭当前页面推断旧版没有某项功能。

## 1. 基线和双版本运行证据

### 1.1 Git 基线

- `[P]` Rust 迁移开始 commit：`c514a74`（`refactor(rust): 建立 Cargo workspace 与前后端共享 core crate`）。该 commit 的父 commit 是旧技术栈仍完整存在的 `3a18bde0cb3278db37fc4e98f1f86297897774c`。
- `[P]` reference implementation：`3a18bde`（`refactor(ui): 精简移动端分类抽屉为一级入口`，2026-09-12），即迁移启动前最后一个旧版链路 tip；包含完整 `cmd/server`、`internal`、`data-plane` 和 `web`。
- `[P]` 当前 Rust main：`e9b6202`（`docs(migration): record green CI publish`，2026-09-13）。
- `[P]` 当前已验证的兼容恢复工作树行为提交：`83f7738`；上面的 `e9b6202` 保留为恢复开始时的 Rust 基线，后续每个逻辑模块均以独立提交推进。
- `[P]` 初始工作区在本清单创建前干净；本清单必须先独立提交，再进入功能恢复提交。

### 1.2 隔离运行实例

- `[P]` old worktree：`/tmp/revaro-old`，detached `3a18bde`，服务 `http://127.0.0.1:18080`。
- `[P]` new 基线 worktree：`/tmp/revaro-new`，detached `e9b6202`；当前恢复中的 new 实际运行实例从 `/config/revaro` 当前工作树构建，服务 `http://127.0.0.1:18084`（`18082`、`18083` 仅保留早期对照记录；Playwright 已在 `821769c` 将未显式传入的双版本 `E2E_NEW_URL` 默认固定到 `18084`，本轮全套仍显式使用该地址）。
- `[P]` old/new 使用不同的 `APP_DATA_DIR`、`APP_WORK_DIR`、端口和管理员 cookie；不得交叉使用数据库、上传临时文件或任务队列。
- `[P]` 两个实例均 `GET /healthz` 返回 200；old 使用迁移前 Go server + data-plane，new 使用 Rust server + Rust wasm bundle。
- `[B]` Docker/Compose 由于环境没有 `/var/run/docker.sock` 无法启动；已切换为本机构建、独立进程和 Chromium 实测，不因此跳过浏览器验证。
- `[P]` old 前端 `npm ci && npm run build`、data-plane release build、Go server build 均通过。
- `[P]` new `cargo build --locked --release -p revaro-server`、`cargo xtask web-build` 均通过。
- `[P]` new 当前已有 smoke E2E：基础 `tests/e2e` 3/3 通过（Rust media、TXT/EPUB reader）；迁移恢复期间的对照 suite 另见 1.4。
- `[P]` old/new 原有 E2E：非真实 EPUB 场景两版各 37/37 通过；三本真实 EPUB 的连续翻页/windowSync 回退场景 old 1/1（约 1.5 分钟）、new 1/1（约 3.3 分钟）通过。旧版生产网格确实没有选择控件；列表选择和多选/ZIP 已由独立 old/new 操作覆盖。
- `[P]` 已保存初始浏览器截图：`/tmp/revaro-old-initial.png`、`/tmp/revaro-new-initial.png`；后续每个模块保存同一 viewport、同一数据状态的 old/new 截图或 trace。

### 1.3 每个条目的固定验收顺序

1. 从旧源码反向清点入口、组件、API 调用和状态。
2. 在 old 实例使用真实点击、输入、键盘、触摸、拖放和空白区域操作确认结果。
3. 在 new 实例以相同数据、相同 viewport、相同操作重做；记录 DOM、网络、截图、控制台错误和结果。
4. 明确差异后恢复实现，不以“功能类似”作为通过标准。
5. 对逻辑模块补充 Rust 单元/集成测试、浏览器 E2E 或回归用例，并运行 `cargo xtask check`、clippy、test、wasm build。
6. 再次逐操作对照 old/new，只有完整链路通过才标 `[P]`，并为该模块单独提交。

### 1.4 已完成的双版本探针记录

以下记录只覆盖已经实际执行过的窄行为，不替代后续各模块的完整验收：

- `2026-09-13`，old `18080` / new `18082`，Chromium 1440×900：任务中心、系统状态、账户设置、回收站入口、侧栏五类入口、侧栏折叠/展开逐项点击；两版均得到同一入口顺序和可见状态。系统状态均显示数据库、网盘存储使用量、服务端缓存三张卡片。
- `2026-09-13`，old `18080` / new `18082`，Chromium 390×844：移动分类抽屉均为六个一级入口且无目录树；打开后 backdrop 存在，点击右侧空白关闭；账户工具菜单、任务中心/回收站/账户设置三项、新建菜单、上传菜单的文案和 Escape 关闭行为一致。
- `2026-09-13`，old/new 均切换列表视图并通过真实 `input[type=file]` 上传两个 TXT：两版均显示行选择控件，单选显示“全选/阅读/下载/分享/重命名/移动/删除”，双选显示“下载 (2)/移动/删除”。旧版默认方块视图没有 `card-select`；列表选择和多选/ZIP 已按 reference 的实际入口验证，不能据此虚构网格选择功能。
- 上述探针使用 `chromium.launch({ args: ["--disable-http-cache"] })`、`serviceWorkers: "block"` 和带随机查询参数的页面，避免 WASM/静态资源缓存掩盖差异；当前证据截图保存在 `/tmp/revaro-old-global-parity.png`、`/tmp/revaro-new-global-parity.png`、`/tmp/revaro-old-mobile-parity.png`、`/tmp/revaro-new-mobile-parity.png`。
- `2026-09-14`，old `18080` / new `18082` 串行运行 parity E2E：最新共享集合 new 46/46、old 43/43（old 排除 3 个仅验证 Rust bundle 的标题）；覆盖账户、认证/TOTP 分支、文件操作/分享/归档/回收站键盘路径、空状态/error toast、视图偏好、分类/书架/图库/移动抽屉、媒体、面包屑/历史、任务中心、上传和 download/preview/Range。old/new 不共用 Playwright 输出目录。
- `2026-09-14`，old `18080` / new `18082`：旧版 `reader-flow.spec.ts` 的 17 个窗口预取、目录锚点、分页、旋转、缓存和视觉场景，以及真实 EPUB 场景，均在两版通过；认证/状态/移动端基础场景两版也通过。
- `2026-09-14`，old `18080` / new `18083`：任务中心 parity 集合两版均 9/9 通过；在原有等待密码、活跃/完成/取消/失败/不可重试、显示更多、清除完成和桌面/移动切换之外，实际确认活动进度条保留 `12.5%` 小数宽度、失败条固定 `100%`、状态 class、空名称已知/未知类型回退与原始 tooltip、Escape 后 summary 焦点、请求未完成时按钮仍可操作，以及清除完成的 `Promise.all` 并发删除。Rust 初始版的进度取整、失败条非满格、名称 tooltip、busy/“清除中…”/“继续中…”和串行删除已恢复；保留 401 自动退出的安全强化。
- `2026-09-14`，old `18080` / new `18082`：侧栏持久化与移动抽屉定向集合两版均 2/2 通过；折叠 rail 和分类手风琴刷新后恢复，移动端隐藏桌面控件、遮罩关闭和六个一级入口一致。
- `2026-09-14`，old `18080` / new `18082`：旧版 `e2e/auth-status.spec.ts` 两项均通过，状态 SSE 三卡、纵向布局、无伪卡片、Esc/空白关闭和未登录 401 响应一致。
- `2026-09-14`，old `18080` / new `18082`：真实 TXT 深链接 `/read/{id}` 均回到根目录且不打开阅读器；这是 reference 运行时现状（旧源码虽有 `openDeepLink` 意图），当前 Rust 未引入额外差异，暂不把旧版自身缺陷冒充 Rust 回退。
- `2026-09-14`，old `18080` / new `18082`：已登录页面中途把目录 children 请求改为 401 时，old 保留壳层并显示 `session expired` toast，new 回到登录页；Rust 保留这一安全边界，避免过期 session 下继续展示旧数据，属于允许的安全强化，正常成功路径不变。
- `2026-09-14`，old `18080` / new `18084`：以同一 mock 目录实际触发 children 401，old/new 分别确认上述结果；`rust-feedback-reference-parity.spec.ts` old/new 1/1，测试提交 `2005d99`。该用例保持为安全例外证据，不把两版不同结果标记为 parity PASS。
- `2026-09-14`，old `18080` / new `18083`：无效 `/f/compatibility-folder-that-does-not-exist` 登录后均回到 `/`，加载“我的文件”，不留下错误状态；另以 mock API 实际打开 `/library/book`、`/library/image`、`/library/video`、`/library/audio/f/compatibility-route-folder`、`/library/file` 和 `/f/compatibility-route-folder`，两版页面与规范 URL 一致。
- `2026-09-14`，old `18080` / new `18083`：实际创建目录并点击进入后，两版均将 `/` → `/f/{id}` 写入应用内 history；浏览器后退逐级回到根目录。再次前进时两版均只恢复 `/f/{id}` URL、不重放目录请求，这是 reference 的现运行时行为，已用同一用例明确记录而不把它误判为 Rust 差异。
- `2026-09-14`，old `18080` / new `18083`：实际打开账户设置后浏览器后退，两版均先关闭账户弹层、保留“我的文件”页面和 `/` URL；弹层 history 语义已加入 parity 用例。
- `2026-09-14`，old `18080` / new `18084`：同一 mock 根目录实际逐项点击目录、TXT、EPUB、图片、音频、视频和未知文件；目录进入、文本编辑器、阅读器、三类媒体预览及未知文件无动作的分流结果与 pathname 均对照一致，并逐项用浏览器后退关闭弹层。Rust 初始 Reader 清理时错误覆盖了 popstate 已恢复的根路径，已移除该路径恢复；`rust-open-item-reference-parity.spec.ts` old/new 1/1，修复提交 `4b5c1a0`，验证提交 `478969a`。
- `2026-09-14`，old `18080` / new `18084`：分享弹层上打开停止分享确认框后实际按浏览器后退；reference 会关闭 share modal、保留外置 `AppDialog`，Rust 初始 popstate 额外清除了该 dialog。已恢复 reference 的外置弹层生命周期，`rust-modal-history-reference-parity.spec.ts` old/new 1/1，修复提交 `55ef117`，验证提交 `a7c669e`。
- `2026-09-14`，old `18080` / new `18083`：将 `POST /api/directories` 同时模拟为 409，旧版关闭新建文件夹弹窗并显示错误 toast；Rust 初始行为把错误留在弹窗内，已恢复为关闭弹窗 + toast。两版回归均通过；分享二次确认错误仍按分享层单独验证。
- `2026-09-14`，old `18080` / new `18083`：新建文件夹成功后再触发一次根目录刷新，两版均保留“文件夹已创建”成功 toast；Rust 初始目录/回收站刷新会清空全局反馈，已移除该非 reference 行为。old/new `rust-actions-parity-ui.spec.ts` 的目录刷新用例通过。
- `2026-09-14`，old `18080` / new `18083`：列表选中一项后滚动到顶部并真实点击内容区左上空白，旧版和 Rust 版均清除选择工具栏（`rust-file-interaction-parity.spec.ts` 1/1 each）；文件行、按钮和工具栏仍由过滤规则排除，不会误清除。
- `2026-09-14`，old `18080` / new `18083`：实际聚焦生产方块文件卡后按 Space，旧版 `FileCard.vue` 的 `.prevent` 使页面保持 `scrollY=150`；Rust 初始版滚到 `574`，已恢复无条件 `prevent_default`（仅在可选择时切换选择），old/new `rust-file-interaction-parity.spec.ts` 均 1/1。
- `2026-09-14`，old `18080` / new `18083`：实际上传无封面 EPUB 并读取浏览器 DOM，旧版书籍图标 `path.icon-detail` 的 `d` 与 Rust 初始版仅一处几何字符串不同（`-13 1` vs `-13-1`）；已恢复 reference 路径，old/new 书籍图标几何用例均 1/1。
- `2026-09-14`，old `18080` / new `18083`：实际上传无封面 EPUB、等待缩略图失败后检查卡片 class，旧版只保留 `file-card book-tile fallback-tile`；Rust 初始版错误地同时保留 `preview-tile`，已让缩略图失败状态联动外层 class。另实际上传 WebM，旧版视频卡为 `preview-tile`、Rust 初始版漏标，已恢复；对应 file-interaction 用例两版各 2/2。
- `2026-09-14`，old `18080` / new `18083`，390×844：实际聚焦媒体库图片方块卡和音乐列表行按 Space，旧版均阻止页面滚动；Rust 初始 `LibraryCard`/`LibraryRow` 缺少对应键盘处理，已恢复无条件 `prevent_default`。old/new `rust-library-ui.spec.ts` 定向用例均 1/1。
- `2026-09-14`，old `18080` / new `18083`：实际对照 Logo 回根、系统状态面板点空白/Escape/重复点击关闭、桌面与移动端侧栏回收站 footer。Rust 初始移动 footer 点击时错误关闭抽屉，已移除额外关闭；尺寸、路径和其余状态均与 old 一致，`rust-navigation-parity.spec.ts` old/new 定向用例均 1/1。
- `2026-09-14`，old `18080` / new `18083`：模拟 `/api/library/all` 首次 503、再次刷新延迟返回合法图片条目，旧版错误态、重试 loading、恢复内容和刷新图标路径已逐项对照；Rust 初始 `RefreshCw` 几何不同，已恢复四段 reference path，`rust-library-ui.spec.ts` old/new 定向用例均 1/1。
- `2026-09-14`，old `18080` / new `18084`：同一 mock 数据实际逐项点击分类路径树；根/节点计数、首层默认展开、子路径展开、过滤后的卡片数、active 行和回到“全部位置”均一致，新增 `rust-sidebar-tree-reference-parity.spec.ts` old/new 1/1，提交 `65ead82`。同一 mock 目录实际打开“文件”树：old `/children` 已返回目录，但 DOM 只留下未解析的 `<sidebardirectorynode>`（旧 `SidebarFileTree.vue` 未导入该组件），new 渲染有效目录行并导航到 `/f/dir-archive`。这是 reference 自身缺失组件注册的运行时缺陷，不把 new 的有效目录树降级成缺陷；该差异已由同一测试显式锁定，递归文件树条目仍保持未完成，需在兼容范围决策中显式处理。
- `2026-09-14`，old `18080` / new `18084`：模拟 `/api/library/all` 首次 503、刷新后延迟 250ms 成功，实际比较分类错误态、路径提示、刷新 loading、恢复后的标题/卡片和请求次数；old/new `rust-library-reference-parity.spec.ts` 2/2 通过，提交 `cf15e55`。分类数量、旧快照保留和全量错误矩阵仍按分类数据条目继续收口。
- `2026-09-14`，old `18080` / new `18083`：`rust-icon-reference-parity.spec.ts` 在两个独立浏览器上下文中用同一 mock 数据逐项读取实际 DOM；顶栏任务、系统状态三张服务卡、五类侧栏/路径树、回收站、折叠、文件视图、新建/上传、任务取消/密码/重试/完成展开，以及移动端抽屉和账户工具入口的 SVG 几何均一致。Rust 初始版本中任务取消、密码、重试、媒体控制、状态卡、文件操作等多个 Lucide 几何差异已按 old `@lucide/vue` 1.41.0 恢复；任务中心“展开其余/收起”箭头也恢复，媒体/文件项全类型图标仍待继续覆盖。
- `2026-09-14`，old `18080` / new `18083`：`rust-directory-picker-reference-parity.spec.ts` 在两个独立浏览器上下文中从列表行实际打开“移动”入口，逐项对照目录选择器触发器、根路径、子目录、深层路径和空目录状态的 SVG 几何，并实际点击目标目录、按 Escape 关闭；两版均 1/1。Rust 初始目录选择器的 ChevronRight 方向错误且缺少 reference 的 stroke/fill 属性，已恢复旧版 Lucide 几何；`rust-actions-parity-ui.spec.ts` old/new 各 9/9，移动/复制的排除、冲突和完整结果矩阵仍待验。
- `2026-09-14`，old `18080` / new `18083`：在目录选择器打开后从 window capture 延迟读取 Escape 的 `defaultPrevented`；old 为 `false`，Rust 初始版为 `true`。已恢复 document capture 阶段的 `stopPropagation`、不调用 `prevent_default` 及对应 listener 生命周期，old/new `rust-directory-picker-reference-parity.spec.ts` 各 1/1，提交 `9ff563b`。
- `2026-09-14`，old `18080` / new `18083`：传输请求延迟期间实际点击“移动”并读取 picker；old 外层为 `class="directory-picker disabled"`、opacity `0.65`，触发器自身 opacity `1`，Rust 初始版缺少外层 class 且把 opacity 放在按钮上。已恢复外层 disabled class/opacity 与 cursor，picker 两个 old/new 场景各 2/2，提交 `d0421c5`。
- `2026-09-14`，old `18080` / new `18083`：实际打开目录选择器后读取进入首帧、固定定位和关闭首帧；old 的 `.directory-flyout-enter/leave-*` 为 `opacity/transform` 过渡 140ms，Rust 初始版直接挂载到最终态并即时卸载。已接入等价的 `flyout-closed` class、DOM 挂载后的定位和延迟卸载/定时器清理；进入/定位、独立退出、传输 disabled 三个场景 old/new 共 3/3，提交 `3198ff8`。
- `2026-09-14`，old `18080` / new `18083`：保留两个列表选择后延迟 `/api/files/batch-download/prepare`，实际读取通知文案、Toast CSS 和点击命中结果；old 文案为“正在准备 2 个文件…”，Rust 初始版为“正在准备批量下载…”，且 Rust `pointer-events:none` 会让点击 Toast 穿透并清空选择。已恢复数量文案和 old 的自身命中区域，`rust-feedback-reference-parity.spec.ts` old/new 1/1，提交 `a0e7f8e`；断线、剪贴板失败、堆叠和完整时限矩阵仍待验。
- `2026-09-14`，old `18080` / new `18083`：桌面与 390×844 实际读取侧栏所有分类行的初始/active/hover computed style、计数、路径展开箭头、折叠 rail、移动抽屉和过渡完成后的几何；两版一致。Rust 额外提供回收站 `aria-label`，属于不改变用户路径的无障碍增强；`rust-sidebar-state-reference-parity.spec.ts` old/new 1/1，提交 `6710996`。文件树竞态、完整路径树和全文件类型图标仍待验。
- `2026-09-14`，old `18080` / new `18083`：实际打开“新建文件夹”通用确认弹窗，聚焦输入框后按 Escape，并在 window bubble 读取默认事件；old 关闭弹窗且 `defaultPrevented=false`，Rust 初始版虽关闭但错误为 `true`。已移除 `ActionDialog` 多余的 `prevent_default`；`rust-dialog-keyboard-reference-parity.spec.ts` old/new 1/1，提交 `c0a5fd9`。
- `2026-09-14`，old `18080` / new `18084`：同一 mock 文件实际打开“重命名”，比较初始名称、输入/按钮状态、弹窗文案和焦点；old 打开后因选择工具栏卸载而焦点回到 `body`，Rust 初始版因保留工具栏并自动聚焦而落在输入框/操作按钮。已让通用 dialog 出现时卸载选择工具栏，并移除 Rust 重命名专属 autofocus；旧版语义上缺少的 `role=dialog` 与显式 `button type` 作为无障碍/防误提交增强保留。`rust-rename-dialog-reference-parity.spec.ts` old/new 1/1，提交 `0e90830`。
- `2026-09-14`，old `18080` / new `18084`：实际打开已有分享链接，分别取消和确认“重新生成链接”，并让 POST 延迟后返回 500；两版取消均保留 active 分享层，确认后子弹窗立即关闭，失败回到分享层显示同一错误。`rust-share-dialog-reference-parity.spec.ts` 二级确认场景 old/new 1/1，提交 `39b4055`。
- `2026-09-14`，old `18080` / new `18083`，390×844：实际打开文件头的新建/上传下拉，逐项比较初始/展开 DOM、summary/popover/首项尺寸与视觉层级、首项 hover、点击空白关闭，以及点击“新建文档”后的 editor 入口和菜单关闭；old/new `rust-file-header-menu-reference-parity.spec.ts` 各 1/1，未发现 Rust 行为差异，提交 `6828692`。
- `2026-09-14`，old `18080` / new `18083`：实际对文件卡执行右键并读取冒泡事件的 `defaultPrevented`，两版均阻止浏览器原生菜单；随后实际打开图片预览“更多操作”，比较菜单初始/展开尺寸、定位、层级、hover、空白关闭、Escape 关闭和 summary 焦点恢复，两版均一致。`rust-preview-menu-reference-parity.spec.ts` old/new 1/1，提交 `8f51320`；音频音量/视频字幕与播放设置菜单仍待完整状态矩阵。
- `2026-09-14`，old `18080` / new `18083`：列表模式实际选择目录、TXT、EPUB、图片、ZIP、未知文件，逐项比较所选摘要、按钮出现条件/顺序、中文文案和 SVG 路径；再比较 TXT+图片多选。old/new 均一致，工具栏关闭后选择清理也一致；`rust-selection-toolbar-reference-parity.spec.ts` old/new 各 1/1，提交 `6c9e46a`。移动端布局及回收站恢复/永久删除分支仍待验。
- `2026-09-14`，old `18080` / new `18083`，390×844 触摸 viewport：实际进入列表、选择 TXT 并读取移动端工具栏布局，再打开账户工具菜单进入回收站，选择已删除 TXT；old/new 的移动工具栏几何和回收站“恢复/永久删除”分支一致。`rust-selection-toolbar-reference-parity.spec.ts` old/new 各 1/1，提交 `e26855f`。
- `2026-09-14`，old `18080` / new `18083`：实际打开账户设置后比较用户名编辑入口和会话区；旧版入口为 `svg + span`，Rust 初始版只有 `span`，且对应 hover 图标未命中。已恢复旧版铅笔 path、14px 尺寸和统一 icon helper；old/new DOM、geometry、hover、编辑聚焦和 Escape 取消均 1/1，`rust-account-reference-parity.spec.ts`，提交 `a6ac08e`。
- `2026-09-14`，old `18080` / new `18083`：用同一 mock TOTP 数据实际完成“账户设置 → 两步验证设置 → 启用 → 下载文本”，读取浏览器下载文件逐字比较文件名、时间行、恢复码顺序和换行；old 使用默认 `Date.toLocaleString()`，Rust 初始版使用 ISO 时间戳，已恢复浏览器本地化格式。`rust-account-download-reference-parity.spec.ts` old/new 1/1，账户相关两项合计 2/2，提交 `80cf6c3`。
- `2026-09-14`，old `18080` / new `18084`：同一延迟 `POST /api/auth/totp/setup` 实际点击“开始设置”，在 loading 中点击 TOTP 子弹窗空白；old 会关闭子弹窗，Rust 初始版因 `totp_busy` 限制仍停留。已恢复遮罩关闭行为，`rust-account-reference-parity.spec.ts`（用户名入口 + TOTP loading）old/new 2/2，提交 `077e678`。
- `2026-09-14`，old `18080` / new `18084`：同一延迟移动 `PATCH /api/files/{id}` 实际点击“移动”后，在请求 pending 中点击遮罩；old 会立即关闭外层传输弹窗，Rust 初始版因 `transfer_busy` 限制仍停留。已移除传输遮罩和外层关闭回调的 busy 阻断；`rust-transfer-dialog-reference-parity.spec.ts` old/new 1/1，提交 `8bbbd22`。
- `2026-09-14`，old `18080` / new `18084`：同一真实文件夹选择在两个独立上下文中上传，记录“已保留目录结构，开始上传 2 个文件”反馈事件，并逐页进入根目录、nested 目录核对两个 TXT；old/new `rust-upload-parity.spec.ts` 文件夹双实例场景 1/1，完整上传集合 5/5，提交 `255349d`。原测试只等待 3.6 秒可见 toast，在高数据量工作树下会误报，已改为记录实际出现事件。
- `2026-09-14`，old `18080` / new `18084`：对同一普通 TXT 上传的真实 PUT 请求延迟 5 秒，确认旧版在字节传输期间任务中心仍不显示该 upload 任务（服务端创建记录但不发 jobs 事件），完成后才显示“上传 / 已完成 / 100%”；Rust 初始版创建任务时额外发事件，错误显示“排队中”并暴露取消按钮。已移除创建阶段事件，恢复旧版通知时机；`rust-upload-parity.spec.ts` old/new 双上下文用例通过，上传模块现 6/6，修复提交 `cb277b7`。
- `2026-09-14`，old `18080` / new `18084`：同一普通 TXT 的真实 PUT 请求连续返回 503，old/new 均重试恰好 5 次；上传会话仍为 `pending`、持久化任务仍为 `queued/uploading/0%`，任务中心仍不出现该文件，清理 DELETE 后无残留。该“失败但无可见本地队列”的结果是旧版实际行为，不虚构新队列 UI；`rust-upload-parity.spec.ts` old/new 1/1，提交 `b907728`。
- `2026-09-14`，old `18080` / new `18084`：真实 `input[type=file]` 先提交空 FileList，再一次选择两个同名 TXT；两版空选择均不发 `POST /api/uploads`，重复选择均得到 `[201,409]`，目录最终只保留一个 ready 文件。`rust-upload-parity.spec.ts` old/new 1/1，提交 `4910f65`。
- `2026-09-15`，old `18080` / new `18084`：实际从 shell 派发可取消的 `dragleave`，old 的 `@dragleave.self` 冒泡到 window 时 `defaultPrevented=false`；Rust 初始版额外调用 `prevent_default()`，已移除。随后以同一真实上传去掉单文件 PUT 响应的 `ETag`，old 仍完成并生成 ready 文件，Rust 初始版在前端拒绝空 ETag；已恢复旧版“单文件不要求 ETag、multipart 分片仍要求校验”的分流。`rust-upload-parity.spec.ts` 上传集合现 15/15，提交 `338d387`。
- `2026-09-15`，old `18080` / new `18084`：实际以 `z-tree` 后 `a-tree` 的文件选择顺序上传文件夹；old 同层目录请求保持 `z-tree → a-tree`，Rust 初始版因 `BTreeSet` 改成字典序，已恢复旧版稳定的“只按深度排序、同层保持插入顺序”。同一用例还确认重复目录只创建一次，old/new 1/1。
- `2026-09-15`，old `18080` / new `18084`：实际让单文件 PUT 返回 409，old 重试 5 次；Rust 初始版仅请求 1 次，已恢复旧版对所有失败上传操作的 5 次重试及退避。实际刷新后从任务中心取消无本地 `File` 句柄的 upload，old 调用 `POST /api/tasks/{id}/cancel`，Rust 初始版错误直删 upload session，已改为与旧版相同的任务取消 fallback；两个场景 old/new 均 1/1。
- `2026-09-15`，old `18080` / new `18084`：mock 16 MiB+ 文件实际比较 multipart 的分片 URL 批次、裸 PUT 的 `Content-Type`、record/complete 字段；另以 `revaro.uploads.v1` 加一个坏记录实际刷新恢复，只上传缺失分片，并保留旧版恢复分片的 `size/content_hash` 字段。Rust 初始版的 multipart `Content-Type`、空 `content_hash`、camelCase resume 解析及 complete 字段均已对齐；old/new multipart 与 resume 各 1/1。
- `2026-09-14`，old `18080` / new `18084`：同一延迟密码 `PATCH /api/auth/password` 实际提交修改，先关闭密码子弹窗，再点击账户外层遮罩；old 会关闭账户弹层，Rust 初始版因外层 `password_busy/totp_busy` 限制仍停留。已移除账户外层 busy 阻断，保留提交按钮 disabled/loading；`rust-account-reference-parity.spec.ts` old/new 3/3，新增场景提交 `1acb307`。
- `2026-09-14`，old `18080` / new `18083`：实际触发成功和 409 错误 Toast，比较文案、`toast success/error` class、无额外 role、定位/颜色/padding/命中区域、最新通知交互及 3.6 秒消失；Rust 初始版缺少 error class 且额外带 `role=status`，已按 reference 恢复。`rust-feedback-reference-parity.spec.ts` old/new 各 2/2，提交 `253e92d`；断线、剪贴板失败、堆叠等业务来源仍待验。
- `2026-09-14`，old `18080` / new `18083`：同一 mock 媒体库逐项切换书架、图片/视频图库、音乐方块/列表；双页面对照实际暴露 Rust 单本 EPUB 标题仍带“第1卷”、图片切到视频时图库模式被重置、分类卡/音频行缺少旧版 `contextmenu.prevent` 三处差异。已改为使用分组标题、父级共享首个图库模式和持久化 key，并恢复分类卡/行右键默认事件语义。旧版原始 `library-ui.spec.ts` 4/4、Rust `rust-library-ui.spec.ts` 8/8、`rust-library-reference-parity.spec.ts` old/new 双上下文 1/1，`cargo xtask check` 通过；提交 `1ffae0e`。
- `2026-09-14`，old `18080` / new `18083`，390×844：`rust-breadcrumb-layout-reference-parity.spec.ts` 先实际暴露 Rust 面包屑额外 `span` 导致每个路径项都获得首/末项移动端 margin（old 1/1 对照失败），随后移除包装并恢复 direct `button`/`ChevronRight` 子节点；修复后 old/new DOM 层级、每项 margin 和深层横向位置均 1/1，并追加中间级点击、Enter、触摸点击三条导航结果对照，整组现为 3/3。导航全套仍保留在 `[ ]` 直到 stale request/完整键盘状态矩阵完成。
- `2026-09-14`，old `18080` / new `18083`：实际点击媒体分类和路径树展开控件后读取 SVG computed transform，旧版分类/路径箭头均为 `matrix(0, 1, -1, 0, 0, 0)`，Rust 初始版为 `none`；已恢复动态展开态的 90° 旋转，`rust-icon-reference-parity.spec.ts` old/new 各 1/1。
- `2026-09-15`，old `18080` / new `18084`：实际向两版预置非法 `revaro:library:media:audio=corrupted`，旧版按 `usePersistentMode` 回退到方块并令“方块” `active/aria-pressed=true`；Rust 初始版只渲染方块内容但两个按钮均未 active，已增加允许值过滤恢复默认状态。`rust-library-reference-parity.spec.ts` 分类集合 old/new 3/3，修复提交 `46daf7e`。
- `2026-09-15`，old `18080` / new `18084`：实际让 `/api/library/all` 缺失 `audio` bucket；旧版因 `data.items[type] || []` 进入正常“这里还没有音乐内容”空态，Rust 初始版因强制反序列化进入“读取失败”。现已让四个 bucket 对缺失或 `null` 均按空数组解析，并以 old/new 双版本用例确认空态一致；核心测试覆盖两种 JSON 形态，修复提交 `b79879d`。
- `2026-09-15`，old `18080` / new `18084`：实际以同一 mock 走“列表选择归档 → 在线解压确认 → POST extract → 任务中心等待密码 → 提交密码”，确认两版确认框 DOM/class、即时关闭、队列 toast、任务分组、密码输入和 running 状态刷新一致；期间暴露 Rust 默认确认按钮漏掉旧版 `default` class，已恢复。`rust-archive-reference-parity.spec.ts` old/new 1/1，修复提交 `7b278b7`。
- `2026-09-15`，old `18080` / new `18084`：以同一实际 WAV、WebM、EPUB 分别走 upload session 创建、pending 查询、字节 PUT、complete，再查询 audio/video/book metadata、reanalyze 和完成 upload task 详情；old/new 成功状态与响应字段一致。探针发现 Rust complete 返回 ready 文件时漏掉 `etag`，已在 single/multipart 共用事务中恢复写入；修复后 `rust-specialized-api-reference-parity.spec.ts` old/new 1/1。
- `2026-09-15`，old `18080` / new `18084`：以 16 MiB+1 的真实 multipart body 逐项执行分片 URL 批次、裸 PUT、带空白 ETag 的分片 ack、complete，并比较独立对象存储下可稳定断言的 ETag 形状、文件状态、hash 与完成结果；随后对空 ETag ack 做错误分支对照，发现 Rust 初始版错误返回 204，已恢复 reference 的 trim/非空 ETag、size 和 content-hash 长度校验（400）。`rust-multipart-upload-reference-parity.spec.ts` old/new 1/1；修复提交 `e5a1b21`。
- `2026-09-14`，old `18080` / new `18083`：将创建目录 POST 延迟 800ms，old 点击“创建”后通用确认弹窗立即移除，Rust 初始版停留在“处理中…”直到请求完成；已恢复旧版同步关闭/后台等待语义，重命名弹窗仍按旧版保留保存中状态，`rust-actions-parity-ui.spec.ts` old/new 各 10/10。
- `2026-09-14`，old `18080` / new `18083`：反向对照 `SelectionToolbar.vue` 的打开按钮分流，旧版对同时满足 editable/book 的 `.txt` 显示书本“阅读”图标，Rust 初始版错误显示编辑图标；已按旧版条件顺序恢复，`rust-selection-toolbar-icon-reference-parity.spec.ts` old/new 各 1/1。另实际点击列表行“移动”后，旧版选择工具栏立即隐藏而 Rust 初始版仍显示；已让媒体预览、阅读器、编辑器、移动/复制、分享和账户弹层按旧版隐藏工具栏，`rust-actions-parity-ui.spec.ts` old/new 各 10/10。
- `2026-09-14`，old `18080` / new `18083`：任务中心以两个活动任务的 1%/2% 原始进度实际对照，旧版先求平均再四舍五入为 2%，Rust 初始版逐项取整并整数除法显示 1%；已恢复 reference 聚合顺序。随后以 12.5%/失败 42% fixture 对照进度条 raw width、终态满格和 status class，`rust-task-center-parity.spec.ts` old/new 各 9/9。
- `2026-09-14`，old `18080` / new `18083`：任务中心请求延迟期间实际读取旧版 DOM，旧版仍显示“还没有后台任务”，Rust 初始版错误显示“正在读取任务…”；已恢复旧版的空任务 fallback，并保留请求完成后的分组行为。相同集合还确认空名称 fallback、Escape 焦点、请求中按钮状态与并发清理时序，`rust-task-center-parity.spec.ts` old/new 各 9/9。
- `2026-09-14`，old `18080` / new `18083`：同一 mock 任务和三张状态卡实际读取 badge 的 class、尺寸、padding 与文字；旧版顶栏任务 badge 和服务卡 badge 均为 `size-sm`，Rust 初始版分别过大或缺少尺寸 class；已恢复 `size-sm`。缓存命中率用 2/3 暴露旧版 `Math.round` 与 Rust 初始整数除法的 67%/66% 差异，也已恢复。`rust-global-ui-reference-parity.spec.ts` old/new 各 1/1，聚合导航/任务/图标集合 old/new 各 15/15。
- `2026-09-14`，old `18080` / new `18083`：系统状态 mock 首帧、非法 SSE JSON、`critical` 状态、桌面/390px 面板几何、空白/Escape 关闭和 summary 焦点逐项读取；另以首个 EventSource 响应结束验证 1s 断线重连，再在退出登录后等待 1.3s 验证没有新连接。old/new `rust-global-ui-reference-parity.spec.ts` 串行各 5/5；Rust 初始版曾把所有非 `degraded` 状态压成 `ok`，并在 SystemStatus Escape 错误阻止默认事件，已按 reference 恢复 raw 状态 class 与键盘语义，提交 `16b70e3`。
- `2026-09-14`，old `18080` / new `18083`，390×844：实际打开移动端账户与工具菜单后按 Escape，并在 window 阶段捕获 `defaultPrevented`；old 为 `false`、菜单关闭且焦点回到 summary，Rust 初始版为 `true`，已移除额外阻止。`rust-navigation-parity.spec.ts` old/new 各 1/1，提交 `ce4d34c`。
- `2026-09-14`，old `18080` / new `18083`，390×844：实际打开移动端分类抽屉后按 Escape，并在 window 阶段捕获 `defaultPrevented`；old 为 `false`、抽屉关闭，Rust 初始版为 `true`，已移除额外阻止。`rust-navigation-parity.spec.ts` old/new 各 1/1，提交 `b4147c6`。
- `2026-09-14`，old `18080` / new `18083`，390×844：实际分别打开新建、上传下拉菜单后按 Escape，并在 window 阶段捕获 `defaultPrevented`；两个版本均关闭菜单且 old 为 `false`，Rust 初始版为 `true`，已恢复旧版键盘语义。`rust-navigation-parity.spec.ts` old/new 各 1/1，提交 `75426a5`。
- `2026-09-14`，old `18080` / new `18083`：分类切换实际记录 `/api/library/all` 请求次数；旧版首次加载后在书架、图库、视频、音乐、文件分类间复用快照，只有明确 Refresh 才重新读取。Rust 初始版每次分类切换都重新请求；已恢复缓存视图与 force refresh 分流，并保持 force refresh 失败后旧缓存仍可供后续分类切换复用。`rust-library-ui.spec.ts` old/new 各 8/8，相关提交为 `d068eb8`、`bb6edba`。
- `2026-09-14`，old `18080` / new `18083`：选中列表文件后发起延迟且返回 500 的目录导航，旧版在 loading 和失败后均保留原列表与选择工具栏，Rust 初始版立即清掉选择；已将清空时机移到成功导航分支。old/new `rust-navigation-parity.spec.ts` 定向用例各 1/1，修复提交 `db5b963`。
- `2026-09-14`，old `18080` / new `18084`：同一 mock 根目录让“慢目录”请求延迟 700ms、“快目录”请求延迟 30ms，并在同一浏览器任务中连续触发两次点击；old/new 最终均停在快目录，迟到的慢目录响应未覆盖标题、URL 或路径。`rust-navigation-parity.spec.ts` old/new 1/1，提交 `226daf1`。
- `2026-09-14`，old `18080` / new `18083`，390×844：向长路径面包屑注入 `scrollTo` 探针，旧版每次当前路径变化均调用 `{left: scrollWidth, behavior: "smooth"}`，Rust 初始版只写 `scrollLeft`、没有平滑调用。已恢复 `ScrollToOptions` 平滑显露，`rust-breadcrumb-layout-reference-parity.spec.ts` old/new 的 DOM/边距与平滑调用各通过，修复提交 `3040995`。
- `2026-09-14`，old `18080` / new `18083`：侧栏媒体库根节点 tooltip 实际为 `title="我的文件"`，Rust 初始版为空；已按旧版在 path 为空时回退到节点名称。`rust-library-ui.spec.ts` old/new 完整用例各 8/8，修复提交 `9d4ea2b`。
- `2026-09-14`，old `18080` / new `18083`：实际登录后注销并卸载认证壳层，旧版会移除顶栏/侧栏各自注册的两个 `matchMedia` change 监听，Rust 初始 helper 永久保留、移除数为 0；已恢复组件生命周期清理，`rust-navigation-parity.spec.ts` old/new 各 10/10。
- `2026-09-14`，old `18080` / new `18083`：媒体预览实际打开图片并展开胶卷，旧版缩略图地址始终为 `/thumbnail?v=<etag>`，Rust 初始版漏掉版本参数；同时从预览根节点按 Tab，旧版先聚焦“更多操作”原生 `summary`，Rust 初始焦点循环漏选该节点。已恢复带编码 etag 的缩略图 URL 和旧版焦点候选规则；`rust-media-parity-ui.spec.ts` old/new 全部 13/13，新增焦点与 URL 断言，代码提交 `b84ce18`。
- `2026-09-14`，old `18080` / new `18083`：按旧版 reader-flow reference 逐页、逐目录项、逐次翻页运行 17 项；两版均 17/17 通过。覆盖稳定窗口、热路径零重复请求、字号/行距客户端重排、跨 spine、父级/随机 TOC、未加载 chunk、连续翻页、旋转、图片 NavAnchor、无 fragment 回退、L2 重开/版本变化和阅读器视觉覆盖层。L2 用例仅在每次测试开头清理浏览器 Cache Storage，并等待异步请求完成，确保共享 Chromium 进程不会把上一次测试的缓存当作 reference 初始设备状态。
- `2026-09-14`，old `18080` / new `18083`：真实上传 EPUB old/new 各 1/1；DOM 中全书 `data-block` 均为连续唯一的 `0…37`，翻页后的页码和无障碍文案均为 `14` / `阅读进度 14.0%`，TOC Escape 关闭后焦点回到 `#toc-button`。Rust 曾在每个 spine 内重复注入 global block 编号，已由 `ed13571` 恢复旧版全书编号后通过。
- `2026-09-14`，old `18080` / new `18083`：反向对照 reader 的文本 fragment 二分定位、媒体 visual start、点击点/可见块回退、DOM 文本进度计算、TOC 导航深度保护、windowSync 取消/恢复、Tab 候选和键盘 Enter 行为；Rust reader/cache 与真实 EPUB 验收代码提交 `a47dc50`，old/new reader-flow 各 17/17，真实 EPUB 各 1/1。
- `2026-09-14`，old `18080` / new `18083`：文档编辑器专用探针先实际暴露三处 Rust 回退：新文档扩展名校验先 trim 导致尾随空格被错误接受，校验错误时保存按钮消失，回收站 YAML 等非 Markdown 可编辑文件被错误设为 Preview；另以延迟 1.2 秒的目录 children 响应验证旧版保存成功 toast 必须等待刷新完成。`2f9eb7b` 恢复原始输入校验、错误时保留保存操作、只读 Markdown 分流和刷新完成后的反馈时序；修复后 `rust-editor-reference-parity.spec.ts` old/new 各 3/3，覆盖尾随空格错误、YAML/Markdown 回收站只读状态和保存刷新时序。
- `2026-09-15`，old `18080` / new `18084`：在 1440×900 与 390×844 实际打开同一 Markdown，逐项比较 editor/backdrop/header/title/icon/meta/tabs/actions/close/workspace/textarea 的几何、计算样式、无障碍文案和 Edit/Split/Preview 三种模式；两版稳定状态均一致。对照包含 tab 过渡收敛后的最终状态，`rust-editor-visual-reference-parity.spec.ts` old/new 1/1；编辑器新增的 semantic aria/id 不改变 reference 视觉层级。提交 `dc0b82d`。
- `2026-09-14`，old `18080` / new `18083`：实际向 `.app-shell` 触发拖入后，再从 `.content-head` 派发冒泡 `dragleave`；旧版因 Vue `.self` 只在离开 shell 本身时关闭覆盖层，Rust 初始版错误地被子元素事件关闭。已恢复相同 target/currentTarget 语义；shell 离开和回收站禁止拖放也在 `rust-upload-parity.spec.ts` old/new 验证，拖拽修复提交 `0d9d993`。
- `2026-09-14`，old `18080` / new `18083`：对真实文件夹选择拦截当前目录 children 并延迟 1.5 秒；旧版成功反馈只在 `openFolder(currentId)` 完成后出现，Rust 初始版在刷新请求完成前就显示。已加入可等待的目录刷新完成通知，保持旧版“刷新 → 成功 toast → pump queue”顺序；`rust-upload-parity.spec.ts` old/new 时序用例各 1/1，修复提交 `5eaa9da`。
- `2026-09-14`，反向核对旧 `useUploads.ts` 的单/分片进度公式：Rust 初始版直接对 98% 比例四舍五入，且 complete 前额外写入 99%；旧版则先对文件百分比四舍五入并封顶 99，再乘 `0.98` 向下取整，complete 前不越过该值。已恢复边界（1/3、1/7、100%、越界）并通过 `cargo test -p revaro-web` 49/49，修复提交 `3d90ae5`。
- `2026-09-14`，old `18080` / new `18083`：旧版原始 `e2e/auth-status.spec.ts`、`mobile.spec.ts`、`library-ui.spec.ts`、`files.spec.ts`、`media-ui.spec.ts`、`reader-flow.spec.ts` 分别为 2/2、1/1、4/4、3/3、10/10、17/17；两版均通过。三本真实 EPUB 原始 `reader-real-epub.spec.ts` old 1/1（约 1.5 分钟）、new 1/1（约 3.3 分钟）；完整 reference 行为集合已可在两隔离实例执行。
- `2026-09-14`，old `18080` / new `18083`：同一 mock 根目录在 1440×900 与 390×844 实际读取文件浏览头，逐项比较标题/统计、方块/列表按钮文字、active、`aria-pressed`、断点显示和布局；两版切换结果一致，`rust-global-ui-reference-parity.spec.ts` 该集合 6/6，文件视图偏好刷新用例 old/new 各 1/1，提交 `93ae4cf`。
- `2026-09-14`，old `18080` / new `18083`：实际创建唯一目录并移入回收站，聚焦生产网格卡后按 Enter；旧版 `FileCard.vue` 的 `.prevent` 即使回收站目录不可打开也会将 `defaultPrevented` 设为 `true`，Rust 初始版只在可打开分支阻止事件、结果为 `false`。已让 FileTile/FileRow 先无条件阻止 Enter，再按旧版条件决定是否打开；两版定向 `rust-file-interaction-parity.spec.ts` 各 1/1，且均不弹出打开弹窗，提交 `31a8ce7`。
- `2026-09-14`，old `18080` / new `18083`：实际创建名为 `compat-folder-<uuid>.epub` 的目录并读取文件卡；旧版目标目录不请求 thumbnail、使用 `folder-tile fallback-tile` 和文件夹 SVG，Rust 初始版误请求该目录 thumbnail。已将主文件浏览的 EPUB 预览、class 和 icon 谓词收紧为 `kind=file`，并保留非 ready EPUB 的旧版类型语义；old/new 定向用例各 1/1，Rust `cargo test -p revaro-web` 51/51、WASM check 通过，提交 `3d50af0`。
- `2026-09-14`，old `18080` / new `18083`：同一 mock 文件矩阵在 1440×1000 实际切换方块/列表视图；旧版列表更新时间按浏览器本地时区显示（`1月1日 08:00`），Rust 初始 formatter 固定 UTC（`1月1日 00:00`）。已让 wasm formatter 使用 browser `Date` 本地 getters，保留 native UTC 单测；目录、目录 `.epub`、TXT、EPUB、图片、音频（带/不带封面）、视频、归档、未知、pending/failed 共 12 类的卡片/行矩阵 old/new 1/1，提交 `33a4052`。
- `2026-09-14`，old `18080` / new `18083`，1440×1000：同一文件 fixture 实际读取方块卡的正常、图片预览 hover、键盘 focus/focus-visible 和 fallback 状态，以及列表行的正常、hover、focus、selected、selected-hover、pending、failed 状态；逐项比较状态 class、背景/边框/圆角/阴影/变换/透明度/光标、预览伪元素和行选择控件的 computed style，old/new `rust-file-card-state-reference-parity.spec.ts` 1/1，无差异。文件卡/行完整 loading、disabled 和触摸长按仍待验。
- `2026-09-14`，old `18080` / new `18083`，1440×900：同一 mock 壳层让 Chromium 从页面起点连续按 Tab 24 次，实际比较顶栏、侧栏、路径树、文件浏览头和文件项的焦点落点；old/new `rust-global-focus-reference-parity.spec.ts` 1/1，未出现隐藏节点或焦点落回 body。Rust 额外的任务中心/账户/回收站 aria-label 不改变焦点顺序，保留为无障碍增强；弹窗、抽屉、编辑器、分享、媒体和阅读器内部的焦点边界仍待验。
- `2026-09-14`，old `18080` / new `18084`：同一 mock 数据在 1440×900 实际打开根目录、切换列表/方块、进入回收站并返回；比较内容头标题、统计文案、按钮状态、卡片/行名称/元信息和回收站头部。390×844 另以空目录与 children 500 错误分别确认空态、错误 toast 和页面结构；两版均通过 `rust-file-browser-reference-parity.spec.ts` 2/2。
- `2026-09-14`，old `18080` / new `18084`：实际以同一 active 分享链接点击复制，旧版只将按钮改为“已复制”、不产生全局 toast；Rust 初始版额外显示“分享链接已复制”，已移除通知并保留复制状态/失败回显。`rust-share-dialog-reference-parity.spec.ts` 复制场景 old/new 1/1，修复提交 `94ad574`。
- `2026-09-14`，old `18080` / new `18084`：对移动请求注入同一 `PATCH /api/files/{id}` 500（`move failed`），旧版全局反馈为“已移动 0 项，1 项失败：传输中的文件.txt：move failed”，Rust 初始版为“部分项目失败”。已恢复失败数量和首项错误的 reference 文案，`rust-transfer-dialog-reference-parity.spec.ts` old/new 2/2，修复提交 `f953af8`。
- `2026-09-14`，old `18080` / new `18084`：分别让分享链接和 TOTP 恢复码的 `navigator.clipboard.writeText` 失败；两版均不产生全局 toast，只在各自弹窗保留“复制失败，请手动…”错误，按钮仍可用。`rust-share-dialog-reference-parity.spec.ts` 4/4、`rust-account-download-reference-parity.spec.ts` 2/2；账户下载断言同时修正为比较本地化时间格式和静态内容，排除并行生成时刻秒数的偶然差异，测试提交 `c1ce934`。
- `2026-09-14`，old `18080` / new `18084`：深层面包屑布局用例曾在平滑滚动尚未结束时读取位置，重复运行出现 0/1/6px 瞬时偏移；两版父容器和最终计算样式一致。测试现等待两版均到达 `scrollWidth - clientWidth` 的最终位置后再比较，390×844 深层 DOM/首末 margin/横向位置、smooth 显露、中间级 click/Enter/tap 稳定 old/new 各 3/3，布局首项另重复 5 次全通过；测试稳定性提交 `5dd791a`。
- `2026-09-14`，old `18080` / new `18084`：实际让新建目录请求分别断网、返回 403 和连续返回两条不同 409；old/new 的 transport 文案（`Failed to fetch`）、403 错误 class/CSS/时限、单槽位最新通知和从替换时刻重新计时均一致，`rust-feedback-reference-parity.spec.ts` old/new 5/5。Rust 初始 transport toast 多展示 `TypeError: ` 前缀，已在 API 边界恢复旧版可见文案，提交 `3657c79`；401 会话边界的安全强化另见前述记录。
- `2026-09-14`，old `18080` / new `18084`：同一双选删除 fixture 让首项 DELETE 返回 500、后项成功；旧版仍尝试后项，刷新列表并显示“已移入 1 项，1 项失败：删除失败.txt：delete failed”，Rust 初始版首错即停且只显示 `delete failed`。已恢复继续处理、刷新/清选择和精确反馈；同一用例另验证重命名尾随空格原样送入 PATCH，`rust-crud-reference-parity.spec.ts` old/new 2/2，修复提交 `a46b845`。
- `2026-09-14`，old `18080` / new `18084`：新建文件夹实际输入空格并按 Enter，old/new 均保持弹窗、禁用创建且不发 POST；取消后重新提交同一 409，均立即关闭确认框并显示 `folder already exists`。重命名同一 409 时两版均保留原输入、弹窗和可重试的保存按钮；`rust-crud-reference-parity.spec.ts` old/new 4/4，测试提交 `f82e7f8`。
- `2026-09-14`，old `18080` / new `18084`：回收站恢复 409 两版均保留项目、选择和文件名错误；永久删除 409 时旧版文案为 `回收站失败.txt：purge conflict`，Rust 初始版丢失文件名。已恢复逐项永久删除失败上下文，回收站冲突用例 old/new 2/2，`cargo xtask check` 通过，修复提交 `8e72809`。
- `2026-09-14`，old `18080` / new `18084`：真实打开非空回收站，清空确认先取消再重新确认；两版取消均不发 DELETE，模拟 500 后均立即关闭确认框、显示 `empty trash failed`、保留项目且按钮仍可用。`rust-crud-reference-parity.spec.ts` old/new 1/1，提交 `1abeb30`。
- `2026-09-14`，old `18080` / new `18084`：普通文件上传时序用例首轮完整套件在慢的初始 `/api/tasks` 快照下出现一次非确定性可见任务，单独重跑未复现；已让 old/new 先完成初始任务快照再开始上传，避免把合法快照竞态误报为通知事件。修正后的定向用例通过，提交 `8172184`；随后显式指定 new 端口的完整套件已 142/142 通过。
- `2026-09-14`，old `18080` / new `18084`：逐个实际打开旧版全部 11 个可编辑扩展名 `md/markdown/txt/yaml/yml/json/toml/ini/conf/log/csv`，比较内容、文本编辑器标签、Markdown 分栏入口和保存禁用状态；另以延迟 `/content`、未保存关闭取消、Ctrl+S 和 409 ETag 冲突比较 loading、重试入口与错误文案。新增 old/new 双上下文场景 2/2，提交 `0027c57`；编辑器完整视觉、编码/大文件和 browser-back 矩阵仍未完成。
- `2026-09-14`，new Rust parity 全套首轮 140 项为 139 通过、1 项失败；该命令漏传 `E2E_NEW_URL`，上传用例实际访问了陈旧的 `18083` 实例，失败不是当前 Rust 业务结论。另一次未显式指定端口的 142 项运行还受到同一实例及过渡首帧取样影响，均不作为最终结果。
- `2026-09-14`，为避免上述误连陈旧实例，`tests/e2e/playwright.config.ts` 已将未设置 `E2E_NEW_URL` 时的默认值统一设为当前 Rust `18084`；未传环境变量的分类 history 双版本用例已实际验证 1/1，提交 `821769c`。完整 suite 仍要求在清单中显式记录 old/new 端口。
- `2026-09-14`，old `18080` / new `18084`：目录选择器 parity 用例改为在两个实际页面监听 DOM 过渡 class 后采样进入/退出首帧，并对浏览器 fixed 定位的亚像素垂直差异使用 `<1px` 容差；没有修改产品实现。定向 3/3 通过，提交 `82cd1b3`。
- `2026-09-14`，old `18080` / new `18084`：显式设置 `E2E_BASE_URL`、`E2E_REFERENCE_URL`、`E2E_NEW_URL`，串行运行 `rust-*.spec.ts` 完整双版本 parity suite，142/142 通过（约 5.9 分钟）；这是加入分类 history 用例前的全套结果，后续 143/143 见下一条。不等同于清单所有条目已完成，未覆盖的条目仍按下方状态继续收口。
- `2026-09-14`，old `18080` / new `18084`：在同一 mock 数据下实际执行“图片分类 → 照片路径筛选 → 书架 → 浏览器后退 → 再后退 → 前进”，比较 URL、标题、筛选文案、卡片和普通目录内容；old/new `rust-library-history-reference-parity.spec.ts` 通过。旧版前进后的结果是 URL 回到 `/library/image`，内容仍停留在“我的文件”根目录（history action 已耗尽）；Rust 保持该 reference 现行为，不擅自引入新的前进栈模型。新增后显式端口完整 suite 为 143/143 通过，提交 `697239f`。
- `2026-09-14`，Rust 工作树此前执行 `cargo fmt --all && cargo xtask check` 通过：workspace unit/integration/doc tests、clippy `-D warnings`、WASM target check 均通过；最新 download 兼容修复另执行 `cargo test -p revaro-server file_routes --lib`（22/22）和 `cargo xtask web-build`，并用新 bundle 完成 reader 4/4 与 old 共享 reader 2/2。
- `2026-09-14`，old `18080` / new `18084`：对同一 mock 文件/回收站分别把目录或回收站刷新响应延迟 700ms，实际点击新建、删除、移动、重命名、恢复、永久删除和清空；old 均在 refresh 完成前不显示成功 toast，Rust 初始版会立即反馈，且 extract 分支错误刷新当前目录。已让 Rust 等待对应 `FolderLoadRequest`/`TrashLoadRequest` 完成后再反馈，并恢复 extract 只刷新任务中心；`rust-mutation-feedback-order-reference-parity.spec.ts` old/new 7/7，`cargo fmt --all -- --check`、`cargo xtask check`、`cargo xtask web-build` 均通过，显式端口完整 suite 150/150 通过。修复提交 `83f7738`。
- `2026-09-14`，old `18080` / new `18084`：实际打开图片预览和 EPUB 阅读器，比较根节点初始焦点、Tab/Shift+Tab 首尾环绕、更多菜单/目录抽屉 Escape、第二次 Escape 关闭预览、文件卡焦点恢复和 `body` overflow 生命周期；old 的 `usePreviewDialog` 只对媒体/阅读器启用焦点 trap，账户、编辑器、分享及普通确认弹窗保持非 trap 语义。`rust-overlay-focus-reference-parity.spec.ts` old/new 2/2 通过，提交 `c238c02`。
- `2026-09-14`，old `18080` / new `18084`：在 overlay focus 用例加入后的显式双版本完整 `rust-*.spec.ts` suite 中，152/152 通过（约 5.9 分钟）；包含账户、全局壳层、分类、文件浏览、上传、CRUD、编辑器、阅读器、媒体、分享、任务中心和移动端已有 parity 用例。此前同一套件出现过一次文件夹上传结果采样抖动，单项连续 3 次及本次完整重跑均通过，最终结果以本次 152/152 为准。
- `2026-09-14`，old `18080` / new `18084`：账户外层与密码子面板、编辑器未保存确认、分享 active 面板的 Escape/焦点边界实际对照；两版账户/分享面板均保持打开且不阻止 Escape 默认事件，编辑器确认弹窗从真实聚焦按钮按 Escape 后关闭并保留编辑器。相关账户、编辑器、分享 spec 定向 12/12 通过，提交 `4940aa2`。
- `2026-09-14`，old `18080` / new `18084`：从旧版全部 `notify` 调用点反向核对 Rust `Feedback` 调用方，并实际验证后台任务完成/失败、分享重生成/停止分享、放弃编辑时已有 toast 的来源语义；重生成成功 old 为 0 个全局 toast、Rust 初始版为 1 个，已抑制该分支反馈，同时避免放弃编辑清空已有 toast。新增定向通知用例全部通过，修复提交 `31ca8e5`、`25c966f`；所有 parity spec 的 new fallback 已统一为当前 Rust `18084`，提交 `eb617a8`。
- `2026-09-14`，old `18080` / new `18084`：Toast 来源修复及 4 个新增来源用例后的显式双版本完整 `rust-*.spec.ts` suite，156/156 通过（约 6.0 分钟）。
- `2026-09-14`，old `18080` / new `18084`：Reader 路径清理修复及普通文件打开分流用例后的显式双版本完整 `rust-*.spec.ts` suite，160/160 通过（约 6.3 分钟）；Reader/导航定向集合 18/18、`cargo xtask check` 通过。该结果只说明现有自动化集合无回归，不代表下方尚未收口的兼容性条目已完成。
- `2026-09-15`，old `18080` / new `18084`：媒体双版本浏览器集合 14/14；实际验证图片首次打开不预加载相邻原图、切换时才预加载、缩略图 `?v=<etag>` 失败后只回退一次到绝对原图 URL、图片控制/手势、音频章节/持久化、视频字幕/全屏/自动隐藏和三种移动宽度。Rust 初始版的 effect 首次挂载误触发相邻预加载，且 fallback 地址比较/写入语义与旧版不一致，已按旧版 watch 与 `new URL(..., location.origin)` 恢复；old/new 同一 `rust-media-parity-ui.spec.ts` 均 14/14。
- `2026-09-15`，old `18080` / new `18084`：给视频文件注入含空格和斜杠的 etag，实际打开预览并比较 `<video poster>`；旧版为 `/api/files/{id}/thumbnail?v=poster%20v%2F1`，Rust 初始版漏掉版本参数。已恢复与旧版 `thumbSRC` 相同的 URL 编码规则，`rust-video-poster-reference-parity.spec.ts` old/new 1/1，修复提交 `dcfc9c1`。
- `2026-09-15`，old `18080` / new `18084`：实际暂停音频、清空本地位置、点击“前进 30 秒”、等待 `timeupdate`/500ms 防抖后关闭预览；旧版 seek 后仍不立即写本地位置，Rust 初始版因 `user_seeked` 分支立即写入。已恢复旧版的防抖时序，并对照关闭时各发送一次远端保存，`rust-media-parity-ui.spec.ts` old/new 1/1，修复提交 `f1671c9`。
- `2026-09-15`，old `18080` / new `18084`：实际暂停视频、清空本地位置、用自定义进度条 seek 到 20 秒、等待 `timeupdate`/600ms 防抖后关闭预览；旧版 seek 后不立即写本地位置，Rust 初始版在 `seek_to` 内立即写入。已恢复旧版节流边界，并对照关闭时各发送一次远端保存，`rust-media-parity-ui.spec.ts` old/new 1/1，修复提交 `b625b1c`。
- `2026-09-15`，old `18080` / new `18084`：实际延迟视频 preview 请求并采样初始状态（两版均无 loading 遮罩、中心播放按钮隐藏、控制按钮为“暂停”），再注入含 `&nbsp;`、十六进制 entity、标签和换行的 VTT cue；Rust 初始版保留 entity 字面量，已按旧版 DOM `textContent` 解码，慢响应与字幕用例 old/new 均 1/1，修复提交 `22f320e`。
- `2026-09-15`，old `18080` / new `18084`：实际在图片“实际大小”后于同一鼠标坐标滚轮放大并比较 computed transform/图像边界；旧版缩放后仍以鼠标位置为锚点，Rust 初始版把目标点误设为原点。已恢复 pointer-preserving wheel zoom，`rust-media-parity-ui.spec.ts` old/new 1/1，修复提交 `7119137`。
- `2026-09-15`，old `18080` / new `18084`：实际暂停音频和视频、清理暂停产生的普通进度请求后关闭预览，并在浏览器 `fetch` 层读取最终 `PUT /media/progress` 的 Request；旧版卸载路径使用 `credentials: same-origin`、JSON header 和 `keepalive: true`，Rust 初始版由普通 gloo 请求发送 `keepalive: false`。已恢复旧版的“先本地保存、再 fire-and-forget keepalive 保存最终位置”语义，音频/视频双版本结果均为 `[true]`，`rust-media-parity-ui.spec.ts` 完整 20/20，修复提交 `25a2194`。
- `2026-09-15`，old `18080` / new `18084`：向书架 `/api/library/all` 注入 `紧凑系列1.epub`、`紧凑系列2.epub`。旧版实际将两项分别渲染为单本书卡；Rust 初始版在书名分组探测中对中文前缀做非字符边界切片并触发 WASM panic，页面进入读取失败态。已改用边界安全的关键字匹配并保留旧版要求分隔符的分组规则；`rust-library-reference-parity.spec.ts` old/new 4/4，`cargo xtask check`、clippy、workspace test、WASM build 均通过，修复提交 `38ca433`。
- `2026-09-15`，old `18080` / new `18084`：实际打开同一份 Markdown，再向 textarea 输入完全相同的原内容；两版均不显示“未保存”标记且保存按钮保持禁用，确认 dirty 必须按内容差异而不是按 input 事件置位。`rust-editor-reference-parity.spec.ts` old/new 1/1，测试提交 `fbfb583`。
- `2026-09-15`，old `18080` / new `18084`：实际在同一份 Markdown 上切换编辑、分栏和预览三种模式，比较 active tab、textarea/预览可见性、Unicode UTF-8 字节数、GFM 标题/列表/任务项/表格/链接/图片/删除线/下划线，以及主动清理的 HTML；两版结果一致且均不生成 `script`/`onclick` 节点。`rust-editor-reference-parity.spec.ts` old/new 1/1，测试提交 `65fa5c1`。
- `2026-09-15`，old `18080` / new `18084`，390×844 触摸上下文：实际完成视频点按、保持播放、退出，再对图片做双指放大、单指取消和完整横向手势；两版均保持同一控制条/播放状态、缩放增量、取消不切图和完成手势切到下一张。`rust-media-parity-ui.spec.ts` old/new 1/1，测试提交 `df134c1`。
- `2026-09-15`，old `18080` / new `18084`：分别让音频和视频原文件 preview 返回不可解码的 `application/octet-stream`；两版均使用“浏览器无法播放此原始格式，请下载后使用本地播放器打开”及视频“重新尝试”入口，并且不请求 HLS/fMP4/transcode/audio-stream。`rust-media-parity-ui.spec.ts` old/new 1/1，测试提交 `710e434`。
- `2026-09-15`，old `18080` / new `18084`：桌面和 390×844 移动端逐项实际点击顶栏任务中心、在线状态、账户设置、回收站，以及移动账户/工具菜单中的任务、账户、回收站；两版均按 reference 打开/关闭对应面板，账户入口不误触发退出，移动菜单点击后正确关闭并完成目标跳转。`rust-global-ui-reference-parity.spec.ts` old/new 完整分流 1/1，测试提交 `f752758`。
- `2026-09-15`，old `18080` / new `18084`：按旧 server route registry、old `web/src` 调用点和 Rust route/caller 逐项反向清点；未发现旧版主 UI 调用方在 Rust 端无对应实现。新增 API 双实例探针比较认证、存储、library、状态、任务、根目录/children、回收站、分享及所有专用缺失分流；另以同名同内容文档实际完成创建、读取、保存、完整/Range 下载、preview、分享/撤销、重命名、复制、删除、恢复和 purge 生命周期，均 old/new 1/1。探针发现并恢复 `GET /api/uploads/{id}` 缺失时 old 的 `upload not found`（Rust 原为 `pending upload not found`），上传其他操作仍保留 pending 文案；服务端单测通过，修复提交 `da88991`，API E2E 提交 `eac64a8`。live 数据中历史文件可能省略可选 `etag`，对照只忽略该字段，其余契约字段仍严格比较。
- `2026-09-15`，old `18080` / new `18084`：重建 new 后串行复跑全局键盘/弹层集合 27/27，以及账户、确认操作、传输、编辑器、分享和操作菜单集合 28/28；实际覆盖桌面 24 步 Tab 顺序、媒体/阅读器焦点陷阱与恢复、顶栏/状态/任务/侧栏/下拉/确认/账户/传输/编辑器/分享的 Escape、主要表单 Enter、空白关闭和浏览器后退。old/new 均通过，未把 401 安全强化差异当作兼容通过依据。
- `2026-09-15`，old `18080` / new `18084`：注入状态 SSE 与任务 SSE 的 `EventSource` 构造失败；旧版状态面板仍显示“正在获取状态…”，不产生错误卡或 Toast，Rust 初始版错误显示“状态流不可用”卡。已移除构造失败时的额外可见反馈/轮询分支，保留成功建立后的正常断线重连；`rust-event-stream-construction-reference-parity.spec.ts` old/new 1/1，修复提交 `f52e986`，测试提交 `d83d5a4`。
- `2026-09-15`，old `18080` / new `18084`：受控 mock reader API 下直接打开 `/read/{id}`，两版都先把路径恢复到 `/`、不打开 Reader；反向核对旧 `openRoute` 后确认这是 reference 运行时先 `openFolder(ROOT)` 改写 pathname 后再读取深链的已知缺陷。已把现象固化为 `rust-deep-link-reference-parity.spec.ts` old/new 1/1（测试提交 `e5393ca`），不把“两个版本都坏”误标成 Reader 深链功能 PASS。
- `2026-09-15`，old `18080` / new `18084`：重建 new 后串行复跑分类/侧栏集合 16/16；五类分类实际切换到书架、图片、视频、音乐、文件，比较数量、标题、路径树、卡片/行、系列/图库/音乐视图、空态、503→重试 loading→恢复、缺失/null bucket、非法视图偏好、分类 history 和路径过滤；old/new 均通过。文件目录树仍保留 reference 未注册组件的已知运行时差异，不把该条目提前标 PASS。
- `2026-09-15`，old `18080` / new `18084`：在同一 mock 根目录实际进入子目录，并延迟两版的 `/api/files/{id}/children` 响应；两版在目录切换期间都显示“正在读取文件…”，响应放行后都进入相同空目录完成态。根目录、列表/方块、回收站、空目录和读取失败结构的文件浏览集合 `rust-file-browser-reference-parity.spec.ts` old/new 3/3 通过，测试提交 `6cdeec9`。
- `2026-09-15`，old `18080` / new `18084`：重建 new 后实际复跑文件项双版本集合 7/7（类型/状态、头部菜单、更多菜单与右键、选择工具栏）；另分别在 old 与 new 运行 `rust-file-interaction-parity.spec.ts` 8/8，选择模式、空白清选、TXT/EPUB/视频图标、回收站目录 Enter 和目录 `.epub` 边界均通过，未发现新的 Rust 差异。
- `2026-09-15`，old `18080` / new `18084`：在 1440、1024、851、850、390、320px 六个 viewport 用同一 mock 根目录逐项读取实际布局；断点可见性、侧栏/内容区/顶栏/文件头/网格几何、桌面/移动入口和 body 横向溢出均一致，`rust-responsive-layout-reference-parity.spec.ts` old/new 1/1，测试提交 `2714f85`。弹窗、上传队列、选择工具栏和媒体/阅读器的响应式子项仍按各自条目验收。
- `2026-09-15`，old `18080` / new `18084`：在 390×844 同时切换方块→列表、刷新恢复偏好、再切回方块，逐次比较实际行/卡数量、壳层几何、入口可见性和横向溢出；`rust-responsive-layout-reference-parity.spec.ts` 第二项 old/new 1/1，测试提交 `120ad12`。
- `2026-09-15`，old `18080` / new `18084`：以 1440×900、40 个实际 mock 文件逐项切换方块/列表，比较 active/`aria-pressed`/tooltip/icon、可见项数量、长页面滚动位置和切换后的存储值；再把两版 localStorage 设为非法值重载，确认均显示默认方块且 old 保留非法原值。Rust 初始 `Effect` 在首次渲染时错误写入 `grid`，已恢复 old watcher 仅在用户切换后写入的时序；`rust-file-view-state-reference-parity.spec.ts` old/new 1/1。
- `2026-09-15`，old `18080` / new `18084`：在 390×844 对同一 mock 实际执行根目录完成、空根、进入目录 loading→空完成、目录 children 500、回收站 500；失败时两版均保留原目录内容、结束 loading、只显示同文案 Toast，不渲染错误卡/重试按钮。分类页 503→重试→恢复由分类对照集合覆盖；`rust-file-loading-state-reference-parity.spec.ts` 2 tests old/new 通过。
- `2026-09-15`，old `18080` / new `18084`：以当前重建后的 Rust bundle 重新串行执行完整上传对照集合；old 15/15、new 15/15。实际覆盖普通/文件夹选择、空选择、同名冲突、相对路径和同层创建顺序、拖放 overlay/非法回收站目标/`dragleave` 默认事件、刷新完成后的成功反馈、传输中任务中心隐藏、503/409 五次重试、单文件无 ETag、multipart 分片与 `revaro.uploads.v1` 缺失分片恢复、刷新后任务取消入口；未把独立本地队列不可见的内部取消/并发/过期清理误记为已覆盖。
- `2026-09-15`，当前 Rust URL `18084`、reference URL `18080`：以当前 Rust URL 运行 `tests/e2e` 全部 191 项、1 worker，191/191 通过（约 7.2 分钟）；其中双版本用例仍在同一测试中分别操作 old/new。将同一集合错误地以 old 作为全局 `E2E_BASE_URL` 运行时，两个名称为 “Rust bundle” 的 smoke 用例命中的是旧版 DOM selector（旧版输入没有显式 `type=text`、旧版编辑器标题没有 `#editor-title`），以及一个并发上传全量运行时序 flake；三项分别在 new URL（5/5）和上传双版本单独运行（1/1）通过，不作为 Rust 回退。
- `2026-09-15`，old `18080` / new `18084`：新增公开分享/API 探针，实际比较 `/healthz`、`/readyz`、无效 token，随后在两实例创建同名同内容 TXT，使用无 cookie 客户端读取公开链接、Range 206，并在撤销后确认链接立即 404；逐项核对安全响应头和内容。发现 Rust 通用文件流额外发送 `ETag`，而 reference 公开分享查询未填该字段，已仅在 `/s/{token}` 兼容性分支移除；old/new 2/2，测试仍保留 Rust shell CSP 所需的 `wasm-unsafe-eval` 例外。
- `2026-09-15`，old `18080` / new `18084`：以同名同内容 TXT 在两实例实际比较 `/api/storage/stats` 的 bytes/file 增量，并逐项请求 `/api/library` 五种 type、`/api/library/counts`、`/api/system/status`，核对顶层/嵌套字段和类型；old/new 只读 API schema 与增量一致，`rust-readonly-api-reference-parity.spec.ts` 1/1。

## 2. 启动、认证和全局壳层

| 状态 | 条目 | 旧版规范与验收点 | 当前 Rust 初检 |
|---|---|---|---|
| `[P]` | 启动画面 | 首屏 splash、logo、spinner、加载到登录/主界面的时序、网络慢和失败状态一致；启动过程不闪出错误主界面。 | old/new `rust-auth-parity.spec.ts` 延迟 session 期间均显示 splash，随后登录失败状态一致 |
| `[P]` | 登录 | 用户名/密码输入、回车提交、按钮 loading/disabled、错误文案、焦点、密码可见性（如有）、重复提交和网络错误一致。 | old/new `rust-auth-parity.spec.ts` 的慢响应、Enter 提交和失败状态均通过 |
| `[P]` | TOTP 登录 | 需要二次验证时的输入、回退、错误、重试、恢复码路径和 session 建立一致。 | old/new `rust-auth-parity.spec.ts` 的二次输入、错误保留和重试分支均通过 |
| `[P]` | 会话检查 | `/api/auth/me`、刷新页面、已过期 cookie、401 后回登录页且不遗留旧数据。 | 初始/刷新过期 cookie 两版均回登录；中途 401 old 保留壳层+toast，new 回登录以清除过期 session 下的旧数据，记录为安全强化例外 |
| `[P]` | 账户入口 | 顶栏账户按钮应打开“账户设置”而不是直接退出登录；用户名、头像、菜单文案和层级一致。 | old/new 桌面实际点击均打开账户设置；移动端工具菜单入口已对照，退出动作仍在独立条目验证 |
| `[P]` | 账户设置 | 账户资料、用户名修改、头像读取/上传/删除、密码修改、TOTP 状态/setup/enable/recovery/delete、成功/失败/取消/关闭行为一致。 | old/new `rust-account-parity.spec.ts` 2/2 与 `rust-password-parity.spec.ts` 1/1 通过，覆盖头像、用户名、密码、TOTP 全链路和错误/关闭；`rust-account-reference-parity.spec.ts` 恢复用户名编辑铅笔入口的 DOM/geometry/hover，补齐 TOTP setup loading 时点击子弹窗空白关闭，以及密码提交 pending 时关闭子弹窗后点击账户外层遮罩；`rust-account-download-reference-parity.spec.ts` 对照恢复码下载本地化时间格式/静态内容，并验证复制失败局部错误与无 toast |
| `[P]` | 退出登录 | 只在账户设置或移动端工具菜单的明确“退出登录”动作触发；成功后清空 session/任务/页面状态并回登录页。 | old/new 明确点击账户设置内“退出登录”后回登录页；账户入口本身不会退出 |
| `[P]` | 全局错误/Toast | 成功、失败、权限过期、冲突、网络断开、复制剪贴板失败的 toast 文案、颜色、时长、关闭方式和堆叠顺序一致。 | 成功/409 错误文案、`toast success/error` class、无额外 role、CSS、命中区域、批量下载数量文案、CRUD/回收站/移动成功反馈的刷新后时序和 3.6 秒时限已 old/new 对照；分享/TOTP 剪贴板失败、后台任务完成/失败、分享重生成不通知/停止分享通知、放弃编辑保留已有 toast、EventSource 构造失败 pending 语义均已 old/new 对照（`rust-feedback-reference-parity.spec.ts` 15/15、`rust-event-stream-construction-reference-parity.spec.ts` 1/1）；中途 401 为明确记录的安全强化例外 |
| `[P]` | 全局键盘 | Escape 关闭当前最内层弹窗/菜单，Enter 提交可提交表单，Tab 焦点不越界；浏览器后退的 modal/folder 语义一致。 | old/new 全局键盘集合 27/27、账户/确认/传输/编辑器/分享/操作集合 28/28；桌面 24 步 Tab、媒体/阅读器 trap、顶栏/状态/任务/侧栏/下拉/确认/账户/传输/编辑器/分享 Escape、主要 Enter、空白关闭和浏览器后退均实际对照 |

## 3. 顶栏、任务中心和系统状态

| 状态 | 条目 | 旧版规范与验收点 | 当前 Rust 初检 |
|---|---|---|---|
| `[P]` | Logo/回到根目录 | 桌面和移动端 logo 图标、`回到我的文件` aria/title、点击后路径、active 状态一致。 | old/new 桌面、移动端从分类/回收站点击 Logo 均回根；按钮尺寸和 title/aria 已在 `rust-navigation-parity.spec.ts` 对照 |
| `[P]` | 任务中心入口 | 顶栏独立任务中心图标/summary，入口位置、图标、数量/状态提示、点击展开和再次点击关闭一致；不能被上传入口替换。 | old/new 实际点击 summary 均展开任务面板；空状态、点击空白和 Escape 已对照 |
| `[P]` | 任务面板分组 | 活跃、已完成/已取消、失败分组；上传/归档解压/字幕任务标签、进度、状态中文文案、平均进度和空状态一致。 | old/new `rust-task-center-parity.spec.ts` 覆盖 waiting/active/completed/cancelled/failed、不可重试、完成空态、上传/归档标签、显示更多及原始小数进度先平均再四舍五入 |
| `[P]` | 任务操作 | 取消、重试、清除已完成、归档密码输入、任务详情、失败错误、超过四项时“显示更多”、任务流实时更新一致。 | old/new 定向 9/9：取消、重试、继续输入密码、清除完成、空白/Escape/入口关闭、分数/名称 fallback、请求中按钮和并发清理均通过；任务详情入口在 reference 无独立页面，归档行即输入入口 |
| `[P]` | 任务面板交互 | 面板不被背景遮挡、点击面板不关闭、点空白关闭、Esc 关闭、点击入口切换、loading/error/empty 一致。 | old/new mock、延迟初始读取、空状态、Escape 焦点和请求中状态均验证；桌面/移动端切换不会重复拉取或断开共享 SSE，`rust-task-center-parity.spec.ts` old/new 各 9/9 |
| `[P]` | 系统状态入口 | 在线/状态球可点击；`aria-label=打开系统状态`、title=`系统状态`、颜色/ok 状态和位置一致。 | old/new 实际点击均展开状态面板；aria/title、ok 状态和三卡布局已对照 |
| `[P]` | 系统状态面板 | EventSource `/api/system/status/stream` 更新状态；DB、存储、缓存三张纵向卡片，状态 badge、详情/错误/加载一致；旧版没有“任务/清理队列/备份”伪卡片和刷新按钮。 | old/new `rust-global-ui-reference-parity.spec.ts` 状态集合串行各 5/5；三卡文案/桌面与 390px 几何、真实首帧、无伪卡片、非法 SSE 数据、critical 顶层 class 和缓存命中率四舍五入均一致 |
| `[P]` | 系统状态关闭 | 点空白、Esc、重复点击、401/断线/重连/服务异常状态一致，关闭后 SSE 清理。 | old/new 状态集合各 5/5；点空白、重复点击、Esc 后 summary 焦点与 `defaultPrevented=false`、首帧断线后 1s 重连、退出登录时 EventSource/重连定时器清理均实际验证 |
| `[P]` | 回收站入口 | 顶栏回收站图标、title=`回收站`、aria、点击进入 trash 路由、数量/空状态和返回根目录一致。 | old/new 桌面顶栏与侧栏 footer、移动 footer 均可进入回收站；空态/返回根和尺寸已对照，入口按 reference 保持根 URL |
| `[P]` | 移动端顶栏 | 状态球仍可用；头像/工具菜单包含旧版实际项目：任务中心、回收站、账户设置；不出现旧版明确禁止的 `打开任务与工具菜单` 旧入口；遮罩/外部点击/Esc 一致，退出登录仍从账户设置进入。 | old/new 390×844 实测状态球、工具菜单三项、任务中心跳转、外部点击和 Escape 均通过；旧版工具菜单本身没有独立退出项 |

## 4. 侧栏、分类入口和路径树

| 状态 | 条目 | 旧版规范与验收点 | 当前 Rust 初检 |
|---|---|---|---|
| `[P]` | 五个一级分类 | 侧栏入口顺序、图标和文案为：书架、图片、视频、音乐、文件；每项 active/current、点击路由和返回行为一致。 | old/new `rust-library-ui.spec.ts` 与导航定向用例确认顺序、文案、active/current、点击切换和返回 |
| `[P]` | 分类数据 | 分类数量、空状态、刷新/loading/error、书籍/图片/视频/音乐/普通文件各自对应 `/api/library` 视图一致。 | old/new 分类/侧栏集合 16/16；实际切换五类并比较数量、标题、路径树、卡片/行、系列/图库/音乐视图、空态、503→重试 loading→恢复、缺失/null bucket、非法音乐视图偏好和分类 history；API `/api/library`、`/api/library/all`、`/api/library/counts` 的传输/顶层字段也已双实例核对 |
| `[ ]` | 分类路径 | 分类主项和展开控制、路径树/文件树、当前路径高亮、展开/收起、加载/空/错误、点击文件夹进入对应分类路径一致。 | 多级媒体路径树计数、默认展开、展开/过滤、active、展开箭头旋转、根节点 tooltip、空路径提示和分类失败/重试 loading old/new 已由 `rust-sidebar-tree-reference-parity.spec.ts`、`rust-library-reference-parity.spec.ts` 对照；文件目录树已实际记录 old 未注册组件、new 有效递归导航，切换竞态仍待稳定场景裁定 |
| `[P]` | 分类持久化 | `revaro:sidebar:collapsed`、`revaro:sidebar:expanded` 的值、恢复时机和坏值处理一致。 | old/new `rust-navigation-parity.spec.ts` 刷新后分别恢复折叠和 book 手风琴；坏值均回默认状态 |
| `[P]` | 桌面侧栏折叠 | 折叠 rail、展开按钮、tooltip/aria、内容宽度/动画、刷新后恢复、当前页仍可识别一致。 | old/new `rust-navigation-parity.spec.ts` 实测 rail、`aria-expanded`、刷新恢复、展开恢复和移动端不复用 rail |
| `[P]` | 移动端分类抽屉 | 宽度 `min(300px,78vw)`；只显示一级入口（书/图/影/音/文件/回收站），不显示树、数量或 chevron；50px 行高；浮动 handle、backdrop、点击空白、Esc、打开/关闭跟随一致，内容不位移。 | old/new 390×844 实际打开、检查六个入口/无目录树、点 backdrop、Escape、重复开关；`rust-library-ui.spec.ts` 3/3 |
| `[ ]` | 侧栏图标 | Lucide 风格、stroke、大小、对齐、active/hover/disabled 颜色和五类具体图标与旧版一致，不用“看起来相似”的替代图标。 | `rust-icon-reference-parity.spec.ts` 与 `rust-sidebar-state-reference-parity.spec.ts` 已在 old/new 浏览器逐项比对侧栏、路径、折叠、回收站和移动抽屉 geometry/active/hover；disabled、完整文件类型图标和路径树竞态仍待验 |
| `[P]` | 侧栏 active/hover/折叠/移动状态 | 分类行 active/hover、计数、展开箭头、桌面 rail 与 390×844 抽屉的布局、动画完成后的尺寸和点击状态一致。 | old/new `rust-sidebar-state-reference-parity.spec.ts` 各 1/1；保留 Rust 额外回收站 `aria-label` 作为无障碍增强 |
| `[P]` | 回收站 footer | 桌面/移动端位置、图标、active、点击和 trash empty 状态一致。 | old/new 桌面尺寸、移动端 footer 点击、回收站空态和移动抽屉保持打开的 reference 语义已实测 |

## 5. 文件浏览、路由和全局内容区

| 状态 | 条目 | 旧版规范与验收点 | 当前 Rust 初检 |
|---|---|---|---|
| `[P]` | 根目录内容头 | `我的文件` 标题、当前路径 nav、项目数/文件数/大小三项 metadata 的文案、间距和层级一致。 | old/new 1440×900 实际比较根目录与回收站内容头、统计文案、动作按钮和返回路径；390×844 空态/错误态结构也逐项比较，`rust-file-browser-reference-parity.spec.ts` 2/2 |
| `[P]` | 面包屑 | `当前路径` nav、根和各级名称、Lucide chevron-right 分隔、当前项样式、点击中间级、超长路径横向滚动、键盘/触摸行为一致。 | old/new 深层路径实际创建并打开，移动端横向滚动、browser back、点击根、`scrollTo({behavior:"smooth"})`、DOM 层级和首末项 margin，以及中间级 click/Enter/tap 均已对照；`rust-breadcrumb-layout-reference-parity.spec.ts` 3/3 |
| `[P]` | 文件夹路由 | `/`、`/f/{id}`、`/library/{book|image|video|audio|file}`、分类下 `/f/{folder}` 的地址、刷新、直接打开、无效 id、权限错误和回退一致。 | old/new 直达浏览器用例覆盖五类分类、分类路径、文件夹路径和无效 `/f/{id}`；无效地址均回根并加载默认页面 |
| `[ ]` | 深链接 | `/read/{fileId}` 打开旧版阅读器；媒体/文件深链接、登录后回到目标、无效深链接错误/返回一致。 | old/new 真实 TXT 与受控 reader mock 均实际回根且不打开阅读器；已确认旧源码存在但 reference 运行时因 pathname 改写顺序未触发，`rust-deep-link-reference-parity.spec.ts` old/new 1/1 仅记录现象，不把共同缺陷标为功能 PASS；是否修复旧版意图仍需单独决定 |
| `[ ]` | 浏览器历史 | 文件夹进入 pushState；返回/前进恢复文件夹/分类；先关闭 modal 再回退页面；stale request 不覆盖新路径。 | old/new 已实际覆盖目录进入、后退、前进 URL 现象、账户弹层后退关闭、普通文件/EPUB/媒体弹层后退关闭并恢复当前文件夹、分享确认框叠加时关闭外层 modal 但保留 reference 外置 dialog、分类筛选后的分类切换/后退/前进，以及慢/快目录响应竞态不覆盖最后一次导航；分类 history 已由 `rust-library-history-reference-parity.spec.ts` 补齐，其它弹层组合仍待验证 |
| `[P]` | 网格/列表切换 | 默认值、按钮图标/tooltip/active、内容布局、滚动、刷新后状态和移动端响应式行为一致。 | old/new 1440×900 以 40 个实际 mock 文件比较两种布局的 active/`aria-pressed`/tooltip/icon、可见项、长页面滚动位置、切换后的 localStorage 和非法偏好回退；`rust-file-view-state-reference-parity.spec.ts` old/new 1/1。已有 390×844 切换/刷新/切回和六档断点布局用例继续覆盖移动端 |
| `[P]` | 文件浏览头菜单状态 | 新建/上传 `<details>` 的初始关闭、summary、popover 定位/尺寸/视觉层级、首项 hover、点击空白关闭和菜单动作后的关闭行为一致；移动端与桌面入口按 reference 呈现。 | old/new 390×844 实测新建/上传两菜单的初始/展开/hover/外部关闭及“新建文档”打开 editor；`rust-file-header-menu-reference-parity.spec.ts` 各 1/1，提交 `6828692` |
| `[P]` | loading/empty/error | 首次加载、切换路径、网络失败、空根、空分类、空回收站、重试按钮、旧内容保留策略和文案一致。 | old/new 390×844 实际对照根目录 loading、空根、目录 loading→完成、目录失败、回收站失败；普通文件/回收站失败保留旧内容并只显示 Toast、不出现错误卡/重试按钮；分类 503→重试→恢复由 `rust-library-reference-parity.spec.ts` 覆盖；`rust-file-loading-state-reference-parity.spec.ts` 2 tests、`rust-file-browser-reference-parity.spec.ts`、`rust-navigation-parity.spec.ts` 均通过 |
| `[ ]` | 拖放 | 桌面拖入文件/文件夹、拖动经过/离开/放下、overlay、非法目标、重复文件、取消和上传结果一致。 | 上传控制器有基础实现，UI 状态待验证 |
| `[ ]` | 响应式布局 | 桌面、平板、390px 手机宽度下内容区、侧栏、顶栏、工具栏、对话框和滚动容器的宽高/层级一致。 | old/new `rust-responsive-layout-reference-parity.spec.ts` 已在 1440/1024/851/850/390/320px 对照基础壳层几何、断点入口可见性、网格列和 body 横向溢出，1/1 通过；对话框、上传/选择工具栏、媒体/阅读器和滚动容器完整矩阵仍待验 |

## 6. 文件项、图标和选择/操作菜单

| 状态 | 条目 | 旧版规范与验收点 | 当前 Rust 初检 |
|---|---|---|---|
| `[ ]` | 文件卡/行 | 文件名、大小、类型、更新时间、目录/媒体/文档标识、thumbnail/cover、fallback 和截断规则一致；方块与列表都验证。回收站目录按 Enter 仍阻止默认事件但不打开；目录名带 `.epub` 仍按目录渲染；列表日期使用浏览器本地时区。 | Rust 有 FileTile/rows 基础；old/new 已实际通过类型/状态 7/7 双版本集合及 old/new 各 8/8 交互集合，覆盖 Enter、`.epub` 目录边界、日期时区、12 类卡/行矩阵以及正常/hover/focus/selected/pending/failed 状态；完整 loading/disabled/触摸状态仍待验 |
| `[ ]` | 图标系统 | 文件夹、文本文档、EPUB、图片、音频、视频、归档、未知文件的旧版图标路径、stroke、颜色、尺寸、背景和状态叠加一致。 | 全局 Lucide 几何已在 old/new 浏览器入口中逐项修复并覆盖任务/状态/菜单/媒体控制关键集合；账户用户名编辑铅笔已恢复；文件项 12 类 preview/fallback 图标路径和节点已矩阵对照，颜色/状态叠加和截图级视觉仍待验 |
| `[ ]` | hover/active/disabled | 卡片 hover、键盘 focus、选中 active、不可用、loading、任务中覆盖层、错误状态和 pointer 行为一致。 | 方块正常/预览 hover/focus/fallback、列表正常/hover/focus/selected/selected-hover/pending/failed 的 computed style、伪元素和选择控件已 old/new 1/1；文件项双版本 7/7、交互 old/new 各 8/8 另确认 Space/Enter、右键和选择 pointer；完整 loading/disabled、长按及任务覆盖层仍待验 |
| `[P]` | 选择入口 | 旧版生产路径只在列表行提供 `选择项目` 控件；点击不打开项目，选中后工具栏更新，取消选择/全选和跨项状态一致；默认方块网格没有选择控件；内容空白点击清除选择，文件行/按钮/工具栏点击不误清除。 | old/new `rust-file-interaction-parity.spec.ts`、actions parity 实测列表显式选择、清除、空白点击和选择模式；旧版 `FileGrid` 的 `selectable` 未开启 |
| `[P]` | 触摸选择 | 旧版生产路径为列表显式选择按钮；进入选择模式后轻触行切换选择，普通轻触打开项目；旧版 tile 的 480ms 长按函数因生产网格 `selectable=false` 不可达，不作为用户行为。 | old/new 390×844 实际验证选择按钮、选择模式轻触不打开编辑器；未将不可达长按代码迁入 Rust |
| `[ ]` | 右键/更多菜单 | 文件/文件夹右键或 more 入口、菜单锚点、菜单项顺序、点空白关闭、Esc、边缘翻转和 item disabled 状态一致。 | old/new 文件卡右键均阻止原生菜单；图片预览 more 的锚点、菜单项、空白/Escape 关闭、hover 和焦点已对照；边缘翻转、disabled 及所有操作结果仍待验 |
| `[ ]` | 打开动作 | 目录进入；可编辑文本进入 editor；EPUB 进入 reader；图片/音频/视频进入 preview；未知类型下载/预览策略、回收站只读行为一致。 | old/new 1440×900 同一 mock 根目录已实际覆盖目录、TXT、EPUB、图片、音频、视频、未知文件的点击分流、pathname 和浏览器后退关闭；`rust-open-item-reference-parity.spec.ts` 1/1；回收站只读、网格/列表键盘、损坏/不支持文件和完整媒体打开状态仍待验 |
| `[ ]` | SelectionToolbar | 选中计数/总大小、清除、全选、打开、下载、分享、重命名、移动、删除、恢复、永久删除、归档解压等按钮的出现条件和文案一致。 | old/new 列表实际覆盖目录、TXT、EPUB、图片、ZIP、未知及 TXT+图片多选的按钮分流、摘要、文案和图标路径；390×844 移动端布局及回收站恢复/永久删除已对照；`.txt` 阅读分流及弹层隐藏已对照，disabled/完整状态矩阵仍待验 |

## 7. 上传入口、队列和任务联动

| 状态 | 条目 | 旧版规范与验收点 | 当前 Rust 初检 |
|---|---|---|---|
| `[P]` | 上传入口 | 文件浏览头部独立上传菜单，含“上传文件”“上传文件夹”；桌面按钮、移动端下拉、图标、popover 层级、hover、点击外部/Esc 关闭和菜单动作一致。 | old/new 桌面与移动端均实际展开同一菜单；文案、说明、图标/层级、首项 hover、外部关闭、Escape 和菜单动作已对照；`rust-file-header-menu-reference-parity.spec.ts` 各 1/1 |
| `[ ]` | 文件选择 | 单/多文件选择、文件夹选择、取消、空选择、同名文件、路径/相对目录保留、浏览器能力差异一致。 | 当前重建后的 old/new 完整上传集合均 15/15：空选择无请求、同名双选 `[201,409]`、单文件完成、文件夹相对路径/同层顺序和坏 resume 记录旁的有效记录均已实际验证；取消选择和浏览器能力差异仍待验 |
| `[ ]` | 拖放上传 | 文件/目录拖放、目标目录、overlay、非法文件、重复上传和完成后列表刷新一致。 | 当前重建后的 old/new 完整集合 15/15；已覆盖文件夹相对路径/同层顺序、shell/子元素 `dragleave` overlay、回收站禁止拖放、默认事件语义、刷新后反馈时序、普通上传通知时机、503/409 重试和单文件无 ETag 完成；非法文件、完整进度/取消矩阵仍待验 |
| `[ ]` | 创建 upload | `POST /api/uploads` 的 chunk/single 模式、大小、类型、目标目录、断点信息和错误处理一致。 | old/new 实际核对 single/multipart 创建体、目标目录、大小/MIME、重复冲突和 multipart 分片批次；异常响应、边界参数及完整 `POST` 错误路径仍待验 |
| `[ ]` | 上传进度 | 单文件/多文件进度、速度、剩余时间、并发、pending/uploading/completing/completed/failed/cancelled 状态和文案一致。 | 旧版进度公式、重试退避和 multipart 裸 PUT 语义已与 Rust 对齐；任务中心传输中隐藏、完成后 100% 和连续失败状态已 old/new 对照；并发、完整进度/取消状态矩阵仍待验 |
| `[ ]` | 上传队列 | 队列面板的展开/收起、排序、显示更多、取消、重试、失败原因、完成清理和与任务中心的分工一致。 | old/new 完整集合 15/15 确认 reference 的可见分工：上传进行中和连续失败的本地任务均不出现在任务中心，完成后才进入完成通知；刷新后无本地句柄的任务中心取消 fallback 已对照。独立本地队列无可见面板；并发、失败/重试和本地取消内部状态矩阵仍待验 |
| `[ ]` | 断点续传 | 刷新/关闭后使用 `revaro.uploads.v1` 恢复；分片获取、记录、complete、abort 和过期记录清理一致。 | old/new 完整集合 15/15 实际验证 camelCase resume 记录、坏记录过滤、已确认分片跳过、缺失分片重传、record/complete 恢复字段；关闭/abort、过期清理和全部已确认分片状态仍待验 |
| `[ ]` | 上传完成 | 列表/分类/统计刷新，任务中心更新，toast，当前路径和重复文件结果一致。 | old/new 完整集合 15/15；文件夹 toast、根目录刷新、嵌套目录和两个文件结果、普通文件“传输中隐藏/完成后通知”、重复文件 `[201,409]`、single/multipart complete 已对照；分类、完整失败和所有刷新分支仍待验 |

## 8. 新建、重命名、移动、复制、删除和回收站

| 状态 | 条目 | 旧版规范与验收点 | 当前 Rust 初检 |
|---|---|---|---|
| `[ ]` | 新建文件夹 | 入口、输入聚焦、空名/非法名/冲突、Enter/Esc、loading、成功刷新和错误文案一致。 | old/new 已实际验证空白输入 disabled 且不发请求、取消、409 关闭弹窗 + error toast，以及延迟 children 刷新完成后才显示“文件夹已创建”（`rust-crud-reference-parity.spec.ts`、`rust-mutation-feedback-order-reference-parity.spec.ts`）；非法名、loading 和完整成功矩阵仍待收口 |
| `[ ]` | 新建文档 | 桌面直接入口和创建菜单中的“新建文档”、默认名 `未命名文档.md`、创建后进入 editor、取消/失败一致。 | old/new 已验证创建菜单实际动作、默认名、进入 editor、菜单立即关闭和保存重开；取消、失败仍待收口 |
| `[ ]` | 重命名 | 单选条件、输入初值/扩展名规则、冲突、空白、Enter/Esc、PATCH 结果和列表更新一致。 | old/new 已实际对照初始名称、输入/按钮状态、文案、焦点及弹窗时选择工具栏卸载；尾随空格原样进入 PATCH；409 冲突保留输入/弹窗并恢复可重试状态；延迟 children 刷新完成后才显示成功反馈（`rust-crud-reference-parity.spec.ts`、`rust-mutation-feedback-order-reference-parity.spec.ts`）；空名、Enter/Esc、保存中和完整 PATCH 结果矩阵仍待收口 |
| `[ ]` | 移动 | DirectoryPicker 面包屑、实时目录浏览、加载/错误/空、排除自身/子目录、目标选中、确认/取消/冲突和 PATCH 结果一致。 | old/new 触发器、面板定位/DOM、140ms 进入/退出过渡、路径图标几何、实际移动和清理、PATCH pending 时点击遮罩关闭，以及延迟 children 刷新完成后才显示成功反馈已对照；排除子目录/冲突/错误仍待验 |
| `[P]` | 目录选择器浮层定位与过渡 | 打开后 nextTick 定位；popover 在窗口边缘的 fixed/top-bottom/max-height 选择一致；进入/退出 opacity、transform、140ms 时序和关闭后的卸载一致。 | old/new 实际比较进入首帧、50ms 定位、独立退出首帧及 140ms 后卸载；测试监听实际 DOM 过渡 class 并容忍 `<1px` 浏览器亚像素差异；`rust-directory-picker-reference-parity.spec.ts` old/new 3/3，修复提交 `3198ff8`，验证提交 `82cd1b3` |
| `[ ]` | 复制 | 目标选择、目录/文件、同名处理、任务或立即结果、完成刷新和错误一致。 | old/new 媒体更多菜单实际复制并验证原文件保留；普通文件、同名和失败仍待验 |
| `[ ]` | 删除 | 确认文案、单项/多项、目录、取消、loading、移入回收站、selection 清理和列表刷新一致。 | old/new 多选删除确认文案、单文件清理链路已对照；同一首项失败/后项成功 fixture 已确认继续处理、刷新清选择和“成功数/失败数/首项错误”反馈；延迟 children 刷新完成后才显示成功反馈；目录、取消/loading、401 和完整失败矩阵仍待验 |
| `[ ]` | 回收站查看 | 列表/网格、原路径/删除时间/大小、空状态、打开限制、恢复/永久删除入口一致。 | old/new 空回收站、列表行元信息、TXT 键盘打开分流已对照；完整 grid/只读矩阵仍待验 |
| `[ ]` | 恢复 | 单项/多项恢复、原位置可用/冲突、成功/失败文案、刷新和 selection 一致。 | old/new 直接恢复和清理已实际验证；单项 409 冲突保留项目/选择并显示文件名错误；延迟回收站刷新完成后才显示成功反馈；多选、原位置冲突和完整失败矩阵仍待验 |
| `[ ]` | 永久删除 | 单项确认、清空回收站确认、不可恢复警告、loading/失败/成功及列表更新一致。 | old/new 永久删除确认、清理链路和 409 失败文件名文案，以及清空回收站取消/500 后弹窗、列表和按钮状态已对照；延迟回收站刷新完成后才显示永久删除/清空成功反馈；loading 和多项/401 矩阵仍待验 |
| `[ ]` | 对话框通用行为 | backdrop、Esc、焦点、按钮顺序、危险色、空输入 disabled、提交中禁用和错误保留输入一致。 | 新建操作的空值、Esc（含 `defaultPrevented=false`）、backdrop、disabled、延迟请求立即关闭及 API 失败关闭/toast 已 old/new 验证；重命名打开时选择工具栏卸载/焦点回退、分享二级确认取消/提交关闭/错误回显、传输 PATCH pending 时遮罩关闭、账户密码 pending 时关闭子弹窗后外层遮罩关闭已对照；浏览器后退时分享外层 modal 关闭而外置确认框保留也已对照；分享弹窗 loading 期间关闭按钮/遮罩可用性已恢复并对照，其他确认框错误和焦点回收仍待验 |

## 9. 文本文档查看与编辑器

| 状态 | 条目 | 旧版规范与验收点 | 当前 Rust 初检 |
|---|---|---|---|
| `[ ]` | 编辑器入口 | md/markdown/txt/yaml/yml/json/toml/ini/conf/log/csv 等旧版可编辑扩展名，点击文件打开 editor；回收站内容只读。 | old/new 已逐个验证 11 个扩展名从列表文件行进入 editor，并验证内容、Markdown tabs、保存禁用；回收站 YAML/Markdown 只读分流和 trash TXT 键盘进入 reader 也已验证；网格/完整回收站矩阵仍待验 |
| `[ ]` | 新文档编辑 | 默认文件名、初始内容、editor modal/页面尺寸、关闭、保存、创建失败和成功返回一致。 | old/new 已验证默认名、编辑、保存、重开、尾随空格扩展名错误和错误后保存按钮保留；创建取消/冲突/失败全矩阵仍待收口 |
| `[ ]` | 读取 | `/content`、编码/大文件错误、loading/error、只读提示、滚动和文本保持一致。 | Rust 已有 `/content` caller；old/new 已验证真实 TXT 读取、YAML/Markdown 只读内容和提示，并以延迟 `/content` 实际比较 loading；编码/大文件/error/滚动全矩阵仍待验 |
| `[ ]` | 编辑模式 | textarea、编辑/分栏/预览 tabs，Markdown 的 GFM 元素与主动 HTML 清理结果、光标/滚动、预览错误和非 Markdown 隐藏 tabs 一致。reference 使用 `marked` + DOMPurify；Rust 使用 `pulldown-cmark` + `ammonia` 对齐可见结果。 | old/new 已逐个验证 11 个扩展名的 textarea 内容、编辑/分栏/预览 tabs、Unicode UTF-8 字节数，以及 GFM 标题/列表/任务项/表格/删除线/链接/图片/下划线和安全 HTML 清理结果；光标/滚动、复杂 Markdown 错误仍待验 |
| `[ ]` | 保存 | PUT content、etag/冲突、busy/disabled、成功 toast、列表 metadata、关闭后刷新和失败重试一致。 | old/new 已以 Ctrl+S 实际触发保存、409 ETag 冲突后保留 editor/错误/可重试按钮，并验证成功 toast 必须等待目录刷新、持久化重开；busy/普通请求失败和 metadata 全矩阵仍待验 |
| `[ ]` | 未保存关闭 | dirty 检测、关闭/浏览器后退确认、取消返回编辑、确认丢弃、Esc/backdrop 行为一致。 | old/new 已实际验证编辑后关闭弹出确认、取消返回编辑、冲突后仍可放弃；同值 input 仍保持 clean；干净编辑器的 browser-back 关闭已由 `rust-open-item-reference-parity.spec.ts` 对照，dirty browser-back、Esc/backdrop 和 readonly 组合矩阵仍待验 |
| `[P]` | 编辑器视觉 | 标题、文件名、工具栏、图标、按钮文案、编辑区字体/行高、readonly 和错误层级与旧版一致。 | old/new 1440×900 与 390×844 实际打开同一 Markdown，逐项比较 editor/backdrop/header/title/icon/meta/tabs/actions/close/workspace/textarea 的几何与 computed style，并验证 Edit/Split/Preview 稳定状态；`rust-editor-visual-reference-parity.spec.ts` old/new 1/1，semantic aria/id 增强不改变视觉 |

## 10. EPUB/TXT 阅读器

| 状态 | 条目 | 旧版规范与验收点 | 当前 Rust 初检 |
|---|---|---|---|
| `[ ]` | TXT 打开 | `/read/{id}`、加载、分页/分栏、返回、书名、实时进度、刷新/深链恢复一致。 | old/new 17项 reader-flow 与真实 TXT 链路已通过；仍需把条目证据拆到各子场景 |
| `[ ]` | EPUB 打开 | manifest/flow/chunk、封面、章节、样式、图片/assets、首屏和错误回退一致。 | old/new 真实 EPUB 1/1、reader-flow 17/17 和普通文件打开分流/浏览器后退 1/1 已通过；损坏/错误回退和逐项截图仍待验 |
| `[P]` | 顶栏 | 返回按钮、居中标题、进度 ring/文字、沉浸式工具显隐、工具不导致正文重排一致。 | old/new reader-flow 的顶栏、标题截断、ring、沉浸式隐藏和恢复均通过；真实 EPUB 另验证页码及 `阅读进度 14.0%` |
| `[ ]` | 翻页 | 上一页/下一页、中心区域、键盘左右/空格、边界不崩、连续翻页无跳页、横竖屏重排位置保持一致。 | old/new reader-flow 已通过点击翻页、键盘/空格、边界、连续无跳页和旋转；触摸翻页/取消、完整键盘状态仍待独立矩阵 |
| `[ ]` | 目录 | 底栏进入 TOC drawer、父子目录、文本 locator、fragment、未加载 chunk 自动加载、跳转后 readingAnchor 一致。 | old/new reader-flow 已通过文本 locator、fragment、未加载 chunk、媒体 NavAnchor、无 fragment、父级/随机跳转和 Escape 焦点；空目录、复杂层级视觉和完整错误状态仍待验 |
| `[ ]` | 阅读设置 | 字号、行距、主题、背景、沉浸式模式、弹层覆盖正文、图标居中、纯客户端重排零 chunk 请求一致。 | old/new reader-flow 已通过字号/行距零 chunk、主题、覆盖层几何和工具显隐；偏好跨刷新、边界 disabled、触摸/键盘完整状态仍待验 |
| `[ ]` | 进度和缓存 | `revaro-reader-prefs`、服务端 `/book/progress`、anchor、manifest/chunk L2 cache、重开零重复请求、版本变化重取一致。 | old/new reader-flow 17/17、真实 EPUB 1/1；Rust 已恢复全局 `data-block`、14/14.0% 标签、版本/指纹变更清理和重开零 chunk，偏好/断网/失败/并发保存矩阵仍待验 |
| `[ ]` | 阅读器响应式 | 桌面/窄屏/触摸、手势与滚动冲突、旋转、空白点击和 drawer 关闭一致。 | old/new 已通过 390×844 视觉、旋转重排、drawer/scrim/Escape；实际触摸手势、pointer cancel、safe-area 和滚动冲突仍待验 |

旧版 `web/e2e/reader-flow.spec.ts` 和 `reader-real-epub.spec.ts` 的全部行为场景都属于本节，不得只以当前 3 个 smoke case 通过代替：包括窗口预取、目录锚点、回退到开头、跨 spine、父级目录不回弹、随机 seek 稳定、页边界、旋转、图片章节定位、缓存复开和视觉覆盖层。

## 11. 图片、音频和视频查看器

| 状态 | 条目 | 旧版规范与验收点 | 当前 Rust 初检 |
|---|---|---|---|
| `[ ]` | 图片查看 | `/preview`、loading/error、画廊上一张/下一张、缩略图、计数、实际大小/适应窗口、放大缩小、双击、滚轮、拖动边界、stage 点击显隐 chrome、键盘 `←/→/+/-/0/1` 一致。 | old/new media parity 14/14 已覆盖首次打开不预加载、切换时预加载、controls、带 `etag` 版本参数的 thumb、thumb 失败绝对 URL fallback、从根节点 Tab 进入菜单、thumb/menu、退出、桌面/390/320 宽度；滚轮鼠标锚点另由同 spec old/new 1/1 验证；完整键盘/边界矩阵仍待验 |
| `[ ]` | 图片触摸 | 双指缩放、拖动、手势取消不误翻页、边界限制、旋转/重排状态保持一致。 | old/new 390×844 触摸上下文已实际验证双指缩放增加百分比、单指 `touchcancel` 不切换图片、完整横向手势切换到下一张；边界限制、旋转/safe-area 和滚动冲突仍待验 |
| `[ ]` | 图片更多菜单 | 下载、移动、复制、信息等 menu 的位置、点击外部/Esc、loading/error 和返回行为一致。 | old/new 实际对照更多入口、下载/移动/复制/信息项、popover 几何与 hover、空白/Escape 和焦点恢复；点击各动作后的 loading/error/返回仍待验 |
| `[ ]` | 音频播放器 | `/audio` 元数据、封面 fallback、章节、上一/下一章、时间跳转、进度、播放/暂停、loading/error/retry、一首/多首行为一致。 | old/new media parity 已覆盖章节标识、controls、桌面/移动宽度；seek/关闭进度保存时序及关闭时 `keepalive` 最终保存另由 `rust-media-parity-ui.spec.ts` old/new 1/1 验证；播放状态、错误重试和完整持久化矩阵仍待验 |
| `[ ]` | 音频持久化 | `revaro-audio-volume`、`revaro-audio-muted`、`revaro-audio-position:{id}`，音量滑块、静音、键盘操作和刷新恢复一致。 | old/new 实际对照暂停后 seek 的 500ms 本地防抖、关闭预览的远端 PUT、音量/静音/位置初始恢复；卸载最终 PUT 已恢复 `credentials: same-origin`、JSON header、`keepalive: true`（提交 `25a2194`）；连续播放写入、进度接口错误/断线和刷新恢复完整矩阵仍待验，时序修复提交 `f1671c9` |
| `[ ]` | 视频播放器 | Range/直接 preview、poster thumbnail、播放/暂停、进度拖动与 seek preview/commit、时间显示、音量/静音、速度、全屏、控制条显隐和自动隐藏一致。 | old/new media parity 已覆盖 poster、controls、速度 Escape、桌面/390/320、touch；poster 的 etag 版本参数和特殊字符编码另由 `rust-video-poster-reference-parity.spec.ts` old/new 1/1 验证；seek/关闭保存时序及卸载 `keepalive` 语义另由 `rust-media-parity-ui.spec.ts` old/new 1/1 验证；Range/全屏/seek commit 完整矩阵仍待验 |
| `[ ]` | 视频字幕 | `/video` metadata、VTT subtitle、选择/关闭字幕、字幕不抖动、加载/解析错误和移动端布局一致。 | old/new media parity 已覆盖默认字幕、选择/关闭、移动端布局与 cue 定位；延迟 preview 初始状态及复杂 cue entity/换行/二级行 class 另由 `rust-media-parity-ui.spec.ts` old/new 各 1/1 验证；track 加载/解析错误和完整字幕矩阵仍待验 |
| `[ ]` | 视频持久化 | `revaro-video-volume`、`revaro-video-rate`、`revaro-video-position:{id}`，刷新/重开恢复准确且无错误跳 seek。 | old/new 实际对照 `revaro-video-volume`/rate、服务端/本地位置初始恢复，以及自定义 seek 的 600ms 本地防抖和关闭预览远端 PUT；卸载最终 PUT 已恢复 `credentials: same-origin`、JSON header、`keepalive: true`（提交 `25a2194`）；连续播放、接口错误/断线和刷新恢复完整矩阵仍待验，时序修复提交 `b625b1c` |
| `[ ]` | 媒体操作 | 播放器设置/更多中的下载、移动、复制、信息、reanalyze（旧版入口若出现）、关闭/返回和任务刷新一致。 | 图片更多菜单的入口、项目和关闭语义已 old/new 对照；音频音量、视频字幕/播放设置及各动作结果、错误和任务刷新仍待验 |
| `[ ]` | 不支持/损坏媒体 | unsupported 原文件直接显示旧版错误而不是空白；重试、返回、控制条、错误文案/图标一致。 | old/new 对照不可解码 audio/video 原文件：错误文案、video“重新尝试”、返回关闭和无转码请求一致；重试请求、加载失败、损坏图片和完整状态矩阵仍待验 |

## 12. 下载、Range、预览、分享和归档任务

| 状态 | 条目 | 旧版规范与验收点 | 当前 Rust 初检 |
|---|---|---|---|
| `[P]` | 单文件下载 | `/api/files/{id}/download`、文件名、Content-Disposition、下载菜单/按钮、loading/error 和回收站策略一致。 | old/new 真实 UI 上传后触发下载并核对文件名、Content-Disposition、ETag；`rust-download-parity.spec.ts` 通过 |
| `[P]` | Range 下载/播放 | bytes range、206/416、Content-Range、HEAD/缓存/大文件、音视频 seek 及断点行为与旧版一致。 | old/new 实际请求并核对 open-ended/suffix 206、invalid 416、Content-Range、Content-Length、Go 版 416 正文；HEAD/大文件/媒体 seek 仍是子项待补 |
| `[ ]` | 预览 | `/preview` content type、图片/音频/视频/文本行为、鉴权、缓存、错误、thumbnail fallback 一致。 | caller 分散，待矩阵验证 |
| `[ ]` | 多选 ZIP | 选择多个文件后一次 prepare、进度/任务、一次性 token 下载、CSP `frame` 约束和失败处理一致。 | old/new 已验证列表多选、一次 prepare、ZIP 下载和 frame CSP；任务/进度、一次性 token 重放和失败处理仍待验 |
| `[ ]` | 归档解压 | 支持格式、密码输入任务、冲突/错误、取消、任务中心、完成刷新和安全路径行为一致。 | old/new 已实际走列表选择、在线解压确认、任务中心等待密码和输入后状态刷新；Rust 真实 ZIP 完成链路与 server 安全路径测试已有；冲突/错误/取消和完整完成矩阵仍待验，`rust-archive-reference-parity.spec.ts` old/new 1/1 |
| `[P]` | 分享读取 | 分享状态读取、已存在/不存在、过期/权限、链接显示、复制失败和关闭一致。 | old/new action parity 实际打开 ShareDialog 并读取 inactive/active 状态；共享 test 通过 |
| `[P]` | 分享创建 | 单文件创建链接、复制 URL、成功/失败、按钮 loading/disabled、公开页面行为和文案一致。 | old/new `rust-actions-parity-ui.spec.ts` 实测创建、复制、重生成和公开读取；两版通过 |
| `[P]` | 分享撤销 | 二次确认（如旧版有）、DELETE、成功/失败、状态刷新、旧链接失效一致。 | old/new action parity 实测停止分享确认、DELETE、状态回到创建入口和旧链接 404 |
| `[P]` | 公开分享页 | `GET /s/{token}` 的文件信息、下载/预览、过期/无效 token、响应头和移动端布局一致。 | 旧版实际行为是受安全响应头保护的原始文件流而非 HTML 页面；old/new 实测无 cookie 读取、文本 attachment、Range 206、`no-store`/CSP/robots/referrer headers、短 token 404；公开流按 reference 不发送 `ETag`，移动端无额外页面布局（`rust-public-share-reference-parity.spec.ts` 2/2） |

## 13. 全部旧版 API、调用方和 Rust 版调用覆盖

下面按旧版 `internal/server/server.go` 的认证路由登记，逐条追踪“旧版前端调用方 → 当前 Rust handler/API → 当前 Rust UI caller”。“后端已迁移”不等于通过；必须确认当前页面实际触发调用并且用户可完成旧版操作。

| 状态 | 旧版 API | 旧版调用方/用途 | Rust handler/API 与当前 caller 初检 |
|---|---|---|---|
| `[P]` | `GET /healthz` | 启动/监控 | old/new 实例均实际请求并返回相同 `200 {"status":"ok"}`；`rust-public-share-reference-parity.spec.ts` |
| `[P]` | `GET /readyz` | 就绪检查 | Rust 现已同时 ping SQLite 与本地对象存储；old/new 实例均返回 `{"status":"ready"}`，存储根缺失单测返回 503 `object storage unavailable` |
| `[P]` | `GET /s/{token}` | 公开分享页 | old/new 创建同名同内容文档并生成分享；无 cookie 读取完整流、Range、响应安全头和撤销后的 404 均实际对照；公开流 `ETag` 与 reference 同为缺省；`rust-public-share-reference-parity.spec.ts` 2/2 |
| `[P]` | `POST /api/auth/login` | LoginPage | Rust `login()` 由登录页调用；old/new 实际登录、TOTP、Enter、loading/错误和 API 登录探针均通过 |
| `[P]` | `POST /api/auth/logout` | 顶栏账户菜单明确退出 | Rust `logout()` 由账户设置的明确退出按钮调用；old/new 登录回跳已验证 |
| `[P]` | `GET /api/auth/me` | App 启动/刷新 session | Rust `fetch_session()` 由启动壳层调用；old/new 刷新/过期 cookie/中途 401 分流已对照，API 会话字段也已核对 |
| `[P]` | `PATCH /api/auth/credentials` | 账户设置凭据 | old UI 没有直接 caller（用户名/密码分拆为下列两个接口）；Rust handler 保留旧 API 兼容 |
| `[P]` | `PATCH /api/auth/password` | 账户设置改密码 | Rust `change_password()` 由 `AccountSettings` 调用；old/new 成功和错误路径已验证 |
| `[P]` | `GET /api/auth/totp` | 账户设置 TOTP 状态 | Rust `fetch_totp_status()` 由 `AccountSettings` 调用；old/new 状态面板已验证 |
| `[P]` | `POST /api/auth/totp/setup` | 账户设置 setup | Rust `begin_totp_setup()` 由 `AccountSettings` 调用；old/new 错误与成功 setup 已验证 |
| `[P]` | `POST /api/auth/totp/enable` | 账户设置启用 | Rust `enable_totp()` 由 `AccountSettings` 调用；old/new 启用链路已验证 |
| `[P]` | `POST /api/auth/totp/recovery-codes` | 账户设置恢复码 | Rust `regenerate_totp_recovery_codes()` 由 `AccountSettings` 调用；old/new 重生成已验证 |
| `[P]` | `DELETE /api/auth/totp` | 账户设置关闭 TOTP | Rust `disable_totp()` 由 `AccountSettings` 调用；old/new 关闭链路已验证 |
| `[P]` | `GET /api/profile/avatar` | 顶栏/账户设置头像 | Rust 顶栏 URL 与账户面板实际加载；old/new 头像成功/删除已验证 |
| `[P]` | `PUT /api/profile/avatar` | 账户设置上传头像 | Rust `update_avatar()` 由 `AccountSettings` 调用；old/new 类型校验和成功上传已验证 |
| `[P]` | `DELETE /api/profile/avatar` | 账户设置删除头像 | Rust `delete_avatar()` 由 `AccountSettings` 调用；old/new 删除已验证 |
| `[P]` | `PATCH /api/profile/username` | 账户设置改用户名 | Rust `change_username()` 由 `AccountSettings` 调用；old/new 保存和空值错误已验证 |
| `[P]` | `GET /api/storage/stats` | 全局/账户/存储信息 | old UI 没有直接调用方；Rust handler 保留，系统状态使用 `/system/status/stream` 的存储数据；old/new 同名文档前后统计增量及字段/类型均一致，`rust-readonly-api-reference-parity.spec.ts` |
| `[P]` | `GET /api/library` | 书架/图片/视频/音乐/文件分类 | old UI 实际统一调用 `/api/library/all`；Rust route 保留，old/new 五种 `type` 均实际请求并核对顶层、counts 和非空 item schema；`rust-readonly-api-reference-parity.spec.ts` |
| `[P]` | `GET /api/library/all` | 分类全量/系列/相册等 | Rust `fetch_library_all()` 由 `FileBrowser`/`LibraryView` 调用；old/new 分类、书架、图库和排序已验证 |
| `[P]` | `GET /api/library/counts` | 侧栏分类数量 | old `useLibrary` 从 `/api/library/all` 取得 counts；Rust UI 同样复用 all，old/new 独立 endpoint 字段和数值类型已实际核对 |
| `[P]` | `GET /api/system/status` | 系统状态初次读取 | old UI 没有直接调用方；两版 UI 都使用 SSE 首帧，handler 保留；old/new 直读响应及 database/storage/cache 嵌套 schema 已实际核对 |
| `[P]` | `GET /api/system/status/stream` | 系统状态 SSE | Rust `SystemStatus` 直接建立 EventSource；old/new 三卡首帧、非法数据、critical/异常状态透传、断线重连和卸载清理已验证 |
| `[P]` | `GET /api/events` | 任务实时 SSE | Rust `TaskController` 直接建立 EventSource；old/new 任务面板刷新/关闭已验证 |
| `[P]` | `GET /api/tasks` | 任务中心初始/刷新 | Rust `fetch_tasks()` 由 `TaskController` 调用；old/new 分组、空态和操作已验证 |
| `[P]` | `GET /api/tasks/{id}` | 任务详情/归档等待 | old UI 没有直接详情页 caller；old/new 以实际完成的 upload task 查询详情，状态、类型、进度、错误和 source 字段一致；归档输入仍走 `/input`，`rust-specialized-api-reference-parity.spec.ts` |
| `[P]` | `POST /api/tasks/{id}/cancel` | 任务中心取消 | Rust `cancel_task()` 由任务按钮调用；old/new mock 任务取消已验证 |
| `[P]` | `POST /api/tasks/{id}/retry` | 任务中心重试 | Rust `retry_task()` 由任务按钮调用；old/new retry 和 max-retry 隐藏已验证 |
| `[P]` | `POST /api/tasks/{id}/input` | 密码/用户输入 | Rust `submit_task_input()` 由密码弹窗调用；old/new 归档等待输入已验证 |
| `[P]` | `DELETE /api/tasks/{id}` | 清除完成任务 | Rust `delete_task()` 由任务面板调用；old/new 清除完成项已验证 |
| `[P]` | `GET /api/files/{id}` | 文件详情/进入目录 | Rust `fetch_file()` 由浏览器、面包屑和 DirectoryPicker 调用；导航对照已验证 |
| `[P]` | `GET /api/files/{id}/children` | 文件夹内容/目录选择器 | Rust `fetch_children()` 由浏览器、侧栏文件树、DirectoryPicker 和上传队列调用；old/new 目录导航已验证 |
| `[P]` | `GET /api/files/{id}/download` | 单文件/媒体下载 | Rust direct URL 与 `download_file()` 调用；old/new 单文件及 Range 已验证 |
| `[P]` | `POST /api/files/batch-download/prepare` | 多选 ZIP | Rust `prepare_batch_download()` 由 SelectionToolbar 调用；old/new 多选一次 ZIP 已验证 |
| `[P]` | `GET /api/files/batch-download/{token}` | ZIP token 下载 | Rust 通过隐藏 anchor 下载 token；old/new 建议文件名和无 iframe/CSP 已验证 |
| `[P]` | `GET /api/files/{id}/preview` | 图片/音频/视频/文件预览 | Rust media 组件 direct URL；old/new 图片/音频/视频及不支持文本 preview/Range 已验证 |
| `[P]` | `GET /api/files/{id}/audio` | 音频 metadata/章节 | old/new 实际上传同一 WAV 后查询成功响应，duration、cover、章节和 JSON 传输一致；Rust `fetch_audio()` 有；`rust-specialized-api-reference-parity.spec.ts` |
| `[P]` | `GET /api/files/{id}/video` | 视频 metadata/subtitles | old/new 实际上传同一 WebM 后查询成功响应，字幕数组和 JSON 传输一致；Rust `fetch_video()` 有；`rust-specialized-api-reference-parity.spec.ts` |
| `[P]` | `POST /api/files/{id}/media/reanalyze` | 媒体重新分析 | old UI 没有稳定可见入口，但 old/new 对同一实际 WAV 执行 reanalyze，均返回 ready/字幕数量；Rust route 保留；`rust-specialized-api-reference-parity.spec.ts` |
| `[P]` | `GET /api/files/{id}/video/subtitles/{subtitle}` | 视频字幕文件 | Rust `VideoPlayer` 的 `<track src>` 直接调用；old/new 字幕加载 fixture 已验证 |
| `[P]` | `GET /api/files/{id}/media/progress` | 音视频进度恢复 | Rust `fetch_media_progress()` 由 Audio/VideoPlayer 调用；old/new 存储探针已验证 |
| `[P]` | `PUT /api/files/{id}/media/progress` | 音视频进度保存 | Rust 普通定时路径由 `save_media_progress()` 调用，预览卸载路径由 `save_media_progress_keepalive()` 直接构造同源 keepalive 请求；old/new 存储探针与 Request.keepalive 已验证 |
| `[P]` | `GET /api/files/{id}/content` | 文本编辑器读取 | Rust `fetch_document()` 由 `DocumentEditor` 流程调用；old/new TXT/Markdown 读取已验证 |
| `[P]` | `PUT /api/files/{id}/content` | 文本编辑器保存/etag | Rust `update_document()` 由 `DocumentEditor` 调用；old/new 保存和 Markdown 重开已验证，冲突子项仍待验 |
| `[P]` | `GET /api/files/{id}/book` | EPUB/TXT metadata | old/new 实际上传同一 EPUB 后查询成功响应，format/title/name/cover/TOC 一致；Rust `fetch_book()` 有；`rust-specialized-api-reference-parity.spec.ts` |
| `[P]` | `GET /api/files/{id}/book/assets/{index}` | EPUB 资源 | Rust reader flow `<img>/<object>` URL 由 `ReaderView` 生成；old/new 真实 EPUB 资源场景已通过 |
| `[P]` | `GET /api/files/{id}/book/cover` | EPUB cover | Rust reader cover URL/fallback 由 `ReaderView` 调用；old/new 真实 EPUB 已通过 |
| `[P]` | `GET /api/files/{id}/book/progress` | reader progress | Rust `fetch_book_progress()` 由 reader 启动调用；old/new reader-flow 已验证 |
| `[P]` | `PUT /api/files/{id}/book/progress` | reader progress save | Rust `save_book_progress()` 由 reader 翻页/关闭调用；old/new reader-flow 已验证 |
| `[P]` | `GET /api/files/{id}/book/flow` | reader manifest/flow | Rust `fetch_book_flow()` 由 reader 启动调用；old/new reader-flow/EPUB 已验证 |
| `[P]` | `GET /api/files/{id}/book/flow/chunks/{index}` | reader window/cache | Rust `fetch_book_chunk()` 由窗口预取/cache 调用；old/new reader-flow 已验证 |
| `[P]` | `GET /api/files/{id}/thumbnail` | 卡片/视频 poster/cover | Rust file cards/library/video/audio 构造带 etag URL；old/new library/media 已验证 |
| `[P]` | `GET /api/files/{id}/share` | 分享状态 | Rust `fetch_share()` 由 `ShareDialog` 调用；old/new 分享读取已验证 |
| `[P]` | `POST /api/files/{id}/share` | 创建分享 | Rust `create_share()` 由 `ShareDialog` 调用；old/new 创建/重新生成已验证 |
| `[P]` | `DELETE /api/files/{id}/share` | 撤销分享 | Rust `revoke_share()` 由 `ShareDialog` 调用；old/new 撤销及公开链接失效已验证 |
| `[P]` | `POST /api/directories` | 新建文件夹 | Rust `create_directory()` 由 header/empty state 调用；old/new 新建和输入行为已验证 |
| `[P]` | `POST /api/documents` | 新建文档 | Rust `create_document()` 由 `DocumentEditor` 新文档流程调用；old/new 默认名/创建/保存已验证 |
| `[P]` | `PATCH /api/files/{id}` | 重命名/移动 | Rust `patch_file()` 由 rename/transfer 调用；old/new 实际重命名和移动已验证 |
| `[P]` | `POST /api/files/{id}/copy` | 复制 | Rust `copy_file()` 由 transfer dialog 调用；old/new 实际复制已验证 |
| `[P]` | `POST /api/files/{id}/extract` | 归档解压 | Rust `extract_archive()` 由 SelectionToolbar 调用；old/new 实际确认请求入口、即时反馈和任务中心后续状态，真实 ZIP 任务也已有验证 |
| `[P]` | `DELETE /api/files/{id}` | 移入回收站 | Rust `delete_file()` 有；API 文档生命周期 old/new 均实际验证删除后入 trash |
| `[P]` | `GET /api/trash` | 回收站列表 | Rust `fetch_trash()` 有；API 文档生命周期 old/new 均实际验证列表包含目标项 |
| `[ ]` | `DELETE /api/trash` | 清空回收站 | Rust `empty_trash()` 有；清空的反馈/错误链路另由 UI parity 覆盖，实际 API 成功/失败矩阵仍未完 |
| `[P]` | `POST /api/trash/{id}/restore` | 恢复 | Rust `restore_file()` 有；API 文档生命周期 old/new 均实际验证恢复 |
| `[P]` | `DELETE /api/trash/{id}` | 永久删除 | Rust `purge_file()` 有；API 文档生命周期 old/new 均实际验证 purge |
| `[P]` | `POST /api/uploads` | 创建上传 | old/new 实际创建同一 WAV/WebM/EPUB 的 single session，模式、part size/count、目标 URL 和 expiry 字段一致；Rust `create_upload()` 有；`rust-specialized-api-reference-parity.spec.ts` |
| `[P]` | `GET /api/uploads/{id}` | 上传状态/断点恢复 | old/new 实际读取 pending session，file/upload/size/MIME/status/parts 字段一致；Rust `fetch_upload()` 有；`rust-specialized-api-reference-parity.spec.ts` |
| `[P]` | `PUT /api/uploads/{id}/data` | 单请求上传 | old/new 实际 PUT 同一媒体字节，均返回 204 + ETag；Rust upload controller caller 已实际跑通；`rust-specialized-api-reference-parity.spec.ts` |
| `[P]` | `PUT /api/uploads/{id}/data/{part}` | 分片上传 | old/new 以 16 MiB+1 body 实际裸 PUT 两个分片，状态码、`Content-Type` 和 ETag 形状一致；Rust `upload_part()` 有；`rust-multipart-upload-reference-parity.spec.ts` |
| `[P]` | `POST /api/uploads/{id}/parts` | 获取分片 URL | old/new 实际请求 `[1,2]` 批次，返回的 part 编号和 URL 尾段一致；Rust `request_upload_parts()` 有；`rust-multipart-upload-reference-parity.spec.ts` |
| `[P]` | `PUT /api/uploads/{id}/parts/{part}` | 记录分片 | old/new 实际接受带空白的有效 ETag 并 trim；空 ETag 均返回 400，size/content-hash 校验分支一致；Rust `record_upload_part()` 有；`rust-multipart-upload-reference-parity.spec.ts` |
| `[P]` | `POST /api/uploads/{id}/complete` | 完成上传 | old/new 实际完成三类文件，ready 文件对象、content hash 和 ETag 一致；此前 Rust 漏写 ETag 已恢复；`rust-specialized-api-reference-parity.spec.ts` |
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
| 基线与清单 | 1 | `068b9bb`、`821769c`（E2E new 默认端口） | healthz、old/new 构建和基线记录已完成；双版本测试未显式传 URL 时默认命中当前 Rust `18084` | `/tmp/revaro-old-initial.png`、`/tmp/revaro-new-initial.png` | 已建立，仍持续追加证据 |
| 全局导航与 UI | 2–4 | `d18556d`（实现）、`d257696`（E2E）、`ba16ddb`（路由）、`db5b963`（失败导航选择状态）、`9d4ea2b`（根节点 tooltip）、`226daf1`（stale navigation parity）、`697239f`（分类 history parity）、`f752758`（顶栏入口完整分流）、`f52e986`（EventSource 构造失败语义）、`d83d5a4`（事件流探针） | 认证、账户、任务、状态、移动抽屉、分类入口/直达路由、空态、Logo、回收站 footer 和关键入口 old/new 已通过；桌面/390×844 顶栏完整分流、浏览器后退/弹层 history、失败导航保留旧内容/选择、根节点 tooltip、慢/快目录响应竞态及筛选后分类 history、全局键盘 27/27 + 账户/确认/传输/编辑器/分享/操作 28/28、Toast 来源与 EventSource 构造失败 pending 语义均已追加；分类路径、深链接、浏览器历史组合和完整状态矩阵仍未完 | `/tmp/revaro-old-global-parity.png`、`/tmp/revaro-new-global-parity.png`、移动端同名截图、导航 trace、`/tmp/revaro-history-*`、`/tmp/revaro-modal-history-*` | 局部 PASS |
| 媒体/阅读器焦点生命周期 | 10–11、15 | `c238c02` | `rust-overlay-focus-reference-parity.spec.ts` old/new 各 2/2 | 实际比较图片预览、EPUB 阅读器的初始焦点、Tab/Shift+Tab 环绕、菜单/目录 Escape、二次 Escape 关闭、文件卡焦点恢复和 body overflow；仅确认 old 同样启用 trap 的媒体/阅读器，账户/编辑器/分享/普通弹窗仍待完整键盘矩阵 | 局部 PASS |
| 通用弹层嵌套键盘边界 | 1.4、2、5–7、10–11 | `4940aa2` | `rust-account-reference-parity.spec.ts`、`rust-editor-reference-parity.spec.ts`、`rust-share-dialog-reference-parity.spec.ts` 定向 12/12 | old/new 实际比较账户外层/密码子面板 Escape 与焦点、编辑器未保存确认 Escape、分享 active Escape；保留 reference 的非 trap 和默认事件语义 | 局部 PASS |
| 全局图标与任务中心控件 | 3–4、6、11、15 | `e329690`（`icons.rs` geometry、路径/音频 fallback、任务展开箭头、old/new DOM E2E） | `rust-icon-reference-parity.spec.ts` 双上下文实际比较全局入口、状态卡、菜单、任务操作、路径和移动端图标；媒体/文件项全类型与完整状态矩阵未完 | old/new icon parity trace；old package source 对照记录 | 局部 PASS |
| 目录选择器图标、展开控件、Escape、disabled 与 flyout 语义 | 8、15 | `d528aed`、`9ff563b`、`d0421c5`、`3198ff8`、`82cd1b3` | `rust-directory-picker-reference-parity.spec.ts` old/new 各 3/3；`rust-actions-parity-ui.spec.ts` old/new 各 9/9 | old/new 实际打开移动目标选择器，比较触发器、面包屑、子目录、深层路径、空目录图标、目标点击/Escape 默认事件、传输中 disabled class/opacity/按钮状态、进入/退出过渡和卸载时序；首帧探针监听真实 DOM 过渡 class，定位比较容忍 `<1px` 浏览器亚像素误差 | 局部 PASS |
| 面包屑 DOM、平滑显露与移动端布局 | 5、15 | `2e2221d`、`3040995`、`da5321c`、`5dd791a` | `rust-breadcrumb-layout-reference-parity.spec.ts` old/new 各 3/3；布局首项重复 5 次通过；`rust-navigation-parity.spec.ts` old/new 各 9/9 | 390×844 深层路径实际比较 direct 子节点、首末 margin、最终横向位置、`scrollTo` smooth options、中间级点击/Enter/触摸、点击根和浏览器后退；测试等待 smooth 动画收敛，避免瞬时采样误报 | 局部 PASS |
| 壳层响应式监听生命周期 | 2、15 | `9dc5204` | `rust-navigation-parity.spec.ts` old/new 各 10/10 | 实际注销卸载认证壳层，拦截 `MediaQueryList` add/remove，确认顶栏/侧栏监听均被释放；完整断线/重连清理仍未完 | 局部 PASS |
| 桌面全局 Tab 焦点顺序 | 2–6、15 | `2451445` | `rust-global-focus-reference-parity.spec.ts` old/new 双上下文 1/1 | 1440×900 实际连续按 Tab 24 次，比较顶栏、侧栏、路径树、文件头和文件项焦点落点；额外 aria-label 只作为无障碍增强保留，媒体/阅读器 trap 与账户/编辑器/分享非 trap 边界已有独立证据，完整状态矩阵仍未完 | 局部 PASS |
| 侧栏展开箭头状态 | 4、15 | `317d1bd` | `rust-icon-reference-parity.spec.ts` old/new 各 1/1 | 实际点击分类和路径树展开控件，比较 SVG transform；分类/路径箭头均与 old 的 90° 旋转一致 | 局部 PASS |
| 通用确认弹窗时序 | 8、15 | `045247f` | `rust-actions-parity-ui.spec.ts` old/new 各 10/10 | 延迟创建请求下实际点击确认，比较弹窗即时关闭和后台结果；重命名保存中语义单独保留 | 局部 PASS |
| 重命名弹窗与选择工具栏层级 | 8、15 | `0e90830`、`a46b845`、`f82e7f8` | `rust-rename-dialog-reference-parity.spec.ts` old/new 双上下文 1/1；`rust-crud-reference-parity.spec.ts` old/new 4/4；`cargo xtask check` 通过 | 实际打开重命名，对照初始名称、输入/按钮状态、文案、焦点和弹窗打开后选择工具栏卸载；尾随空格原样送入 PATCH；409 冲突保留输入/弹窗并恢复可重试状态；Rust 移除专属 autofocus 并恢复旧版层级时序，语义 role/type 增强保留；空名/Enter/Esc/保存中仍未完 | 局部 PASS |
| 选择工具栏分流与弹层可见性 | 6、8、15 | `f898067` | `rust-selection-toolbar-icon-reference-parity.spec.ts` old/new 各 1/1；`rust-actions-parity-ui.spec.ts` old/new 各 10/10 | 实际选择 `.txt` 对照阅读图标几何；实际打开移动弹层对照选择工具栏立即隐藏；完整文件类型/disabled 矩阵未完 | 局部 PASS |
| 任务中心与操作时序 | 3、15 | `5c69720`、`2d9f584`、`d05c438` | `rust-task-center-parity.spec.ts` old/new 各 9/9 | 两个活动任务原始进度先平均再四舍五入；小数条宽、失败满格、名称 fallback、Escape 焦点、请求中按钮和 `Promise.all` 删除均实际对照；延迟初始请求仍显示 reference 空任务文案 | 局部 PASS |
| 系统状态 SSE 与全局键盘语义 | 3、13、15 | `16b70e3` | `rust-global-ui-reference-parity.spec.ts` old/new 各 5/5 | 实际对照三卡首帧、非法数据、critical 状态 class、桌面/移动几何、空白/Escape（含默认事件和焦点）、断线重连以及退出登录后的 EventSource/定时器清理 | 局部 PASS |
| 移动端工具菜单键盘语义 | 2、3、15 | `ce4d34c` | `rust-navigation-parity.spec.ts` old/new 各 1/1 | 390×844 实际打开账户与工具菜单，比较 Escape 的关闭、summary 焦点和 window 阶段 `defaultPrevented=false` | 局部 PASS |
| 移动端分类抽屉键盘语义 | 2、4、15 | `b4147c6` | `rust-navigation-parity.spec.ts` old/new 各 1/1 | 390×844 实际打开分类抽屉，比较 Escape 的关闭结果与 window 阶段 `defaultPrevented=false` | 局部 PASS |
| 文件浏览头下拉键盘语义 | 5、7、15 | `75426a5` | `rust-navigation-parity.spec.ts` old/new 各 1/1 | 390×844 实际分别打开新建/上传菜单，比较 Escape 关闭结果与 window 阶段 `defaultPrevented=false` | 局部 PASS |
| 顶栏/状态 badge 与命中率 | 3、4、15 | `8f5356f`、`1108947` | `rust-global-ui-reference-parity.spec.ts` old/new 各 1/1；聚合导航/任务/图标集合 old/new 各 15/15 | 同一 mock 数据逐项比较任务 header、服务卡 badge 的 class/尺寸/padding/文字，并用 2/3 fixture 验证 67% 四舍五入；系统状态异常 class/重连已由独立模块覆盖 | 局部 PASS |
| 文件浏览头视图与断点状态 | 5、15 | `93ae4cf`、本模块视图偏好修复 | `rust-global-ui-reference-parity.spec.ts` old/new 双上下文 6/6；`rust-responsive-layout-reference-parity.spec.ts` old/new 2/2；`rust-file-view-state-reference-parity.spec.ts` old/new 1/1 | 1440/390px 实际比较标题/统计、方块/列表 active、`aria-pressed`、断点可见性、布局、长页面滚动、切换后的存储值和非法偏好回退；Rust 初始首次 Effect 覆写 localStorage 的差异已恢复 | PASS |
| 根目录/回收站内容头与空错误态 | 5、15 | `50b8f3c`、本模块 loading/error 对照 | `rust-file-browser-reference-parity.spec.ts` old/new 2/2；`rust-file-loading-state-reference-parity.spec.ts` old/new 2 tests | 1440×900 实际比较根目录卡片、列表行、统计和回收站返回；390×844 比较空根、目录 loading→空完成、children 500、回收站 500 的旧内容保留、Toast 和无重试错误卡 | PASS |
| 文件项键盘、类型边界与日期格式 | 6、15 | `31a8ce7`、`3d50af0`、`33a4052` | `rust-file-interaction-parity.spec.ts` old/new 定向各 2/2；`rust-file-card-reference-parity.spec.ts` old/new 矩阵 1/1；`cargo test -p revaro-web` 51/51；WASM/web build | 实际验证回收站目录 Enter、目录名 `.epub` 的 thumbnail/fallback、12 类卡/行节点及浏览器本地时区日期 | 局部 PASS |
| 文件卡/行状态视觉 | 6、15 | `aa96e6a` | `rust-file-card-state-reference-parity.spec.ts` old/new 双上下文 1/1 | 1440×1000 实际比较方块正常/hover/focus/fallback、列表正常/hover/focus/selected/selected-hover/pending/failed 的状态 class、computed style、预览伪元素和选择控件；完整 loading/disabled/触摸状态未完 | 局部 PASS |
| 媒体库快照与 force refresh | 4、5、15 | `d068eb8`、`bb6edba`、`1ffae0e`、`697239f`、`cf15e55`、`46daf7e`、`b79879d`、`38ca433` | `rust-library-ui.spec.ts` old/new 各 8/8；`rust-library-reference-parity.spec.ts` old/new 各 4/4；`rust-library-history-reference-parity.spec.ts` old/new 各 1/1；书架中文前缀回归 fixture old/new 4/4；`revaro-core` 111/111 | 实际切换分类只请求一次 `/api/library/all`，显式 Refresh 才重新读取；首次 503、重试 loading/恢复和请求次数、refresh 失败后继续切换仍复用旧快照；书架单本分组标题、中文前缀书名安全解析、图库模式跨图片/视频切换、媒体分类内容和右键默认事件、筛选后分类切换与浏览器返回/前进结果、非法音乐视图偏好回退、缺失或 null bucket 的正常空态也已双版本对照；完整分类错误/数量矩阵未完 | 局部 PASS |
| 就绪探针 | 1、13 | `938a60a` | Rust router 单测：DB 正常、对象存储失败；old/new 实例实际响应一致 | `/readyz` old/new 200 对照 | PASS |
| 文件浏览与选择 | 5–6 | `d18556d`（实现）、`d257696`（E2E）、`1937d06`、`83ec6c0`、`8e59b85`、`4ba891f`（逐项 parity） | 面包屑/历史、列表选择、文件图标、打开分流和操作菜单已有 old/new 用例；方块卡与媒体库卡 Space、EPUB 书籍图标几何、EPUB fallback class、视频 preview class、媒体库刷新图标/失败重试和多级分类路径已追加验证；hover/长按/全部类型未完 | `/tmp/revaro-old-global-parity.png`、`/tmp/revaro-new-global-parity.png`、file-card/library parity trace | 局部 PASS |
| 普通文件打开分流与阅读器路由生命周期 | 5–6、9–11、15 | `4b5c1a0`（Reader 路径恢复）、`478969a`（双版本打开分流） | `rust-open-item-reference-parity.spec.ts` old/new 1/1 | 同一 mock 根目录实际点击目录、TXT、EPUB、图片、音频、视频和未知文件，比较 overlay、pathname、Reader 标题、编辑器内容，并用浏览器后退逐项关闭；修复 Rust Reader cleanup 覆盖文件夹 URL 的回退 | 局部 PASS |
| 失败导航状态保留 | 5、6、15 | `db5b963`、`226daf1` | `rust-navigation-parity.spec.ts` old/new 定向用例各 1/1 | 列表已有选择时发起延迟 500 导航并返回 500，实际比较 loading/失败后的旧列表、选择工具栏和错误 toast；另以慢/快目录双击确认 stale response 不覆盖最后一次路径；成功导航清空选择，分类 history/完整状态矩阵仍未完 | old/new navigation parity trace | 局部 PASS |
| 上传与任务 | 7、3 | `3beac64`（server）、`d18556d`（web）、`d257696`（E2E）、`0d9d993`（拖拽覆盖层）、`5eaa9da`（文件夹刷新时序）、`3d90ae5`（进度取整）、`255349d`（文件夹 old/new 双实例）、`cb277b7`（任务通知时机）、`b907728`（失败重试 parity）、`4910f65`（文件选择 parity）、`338d387`（请求/断点/取消入口 parity）、`e5a1b21`（multipart ack 校验） | 上传入口、目录上传、任务中心分组/取消/重试/归档输入和完成刷新已有 old/new 用例；拖拽 `.self` 语义、文件夹刷新后成功反馈、旧版进度边界、同一文件夹的 old/new toast/嵌套目录/同层请求顺序、普通上传传输中隐藏/完成后通知、连续 503/409 的 5 次重试、空选择/同名重复、单文件无 ETag、multipart URL/裸 PUT/ack/complete、camelCase 断点恢复和刷新后任务中心取消入口均已追加；断点关闭/abort、并发、完整进度/本地取消矩阵未完 | parity Playwright trace、`rust-upload-parity.spec.ts` old/new 完整 15/15、`rust-multipart-upload-reference-parity.spec.ts` old/new 1/1；`cargo fmt/check/clippy`、release build、`cargo xtask web-build` 通过 | 局部 PASS |
| CRUD 与回收站 | 8 | `d18556d`（实现）、`d257696`（E2E）、`f953af8`（移动失败反馈）、`a46b845`（删除/重命名 parity）、`f82e7f8`（CRUD conflict parity）、`8e72809`（purge error context）、`1abeb30`（empty trash parity）、`83f7738`（成功反馈时序） | 新建、重命名、移动、复制、删除、恢复、永久删除主链路已 old/new 实测；新建 API 失败时弹窗关闭/toast、空白输入不发请求、409 关闭确认框、移动 PATCH 失败数量与首项错误文案、删除多选继续处理/刷新清选择、重命名原始空白输入和 409 保留输入/可重试、回收站恢复/永久删除 409 的项目/选择/文件名错误上下文、清空回收站取消/500 后弹窗列表状态已追加；7 个成功分支已确认等待 reference 的目录/回收站刷新再反馈；目录/401/完整 loading 失败矩阵仍未完 | parity Playwright trace、`/tmp/revaro-dialog-error-*`、`rust-transfer-dialog-reference-parity.spec.ts`、`rust-crud-reference-parity.spec.ts`、`rust-mutation-feedback-order-reference-parity.spec.ts`（old/new 7/7） | 局部 PASS |
| CRUD/传输刷新—反馈时序 | 8、15 | `83f7738` | 同一 700ms 延迟 mock 实际覆盖新建、删除、移动、重命名、恢复、永久删除、清空回收站；old/new 在刷新完成前均无成功 Toast，完成后文案与列表状态一致。Rust 使用可完成的 folder/trash refresh request，并恢复 extract 只刷新任务中心；`cargo fmt --all -- --check`、`cargo xtask check`、`cargo xtask web-build` 与最新显式端口完整 suite 160/160 均通过 | `rust-mutation-feedback-order-reference-parity.spec.ts` old/new 7/7 | PASS |
| 文档编辑器 | 9 | `d18556d`（实现）、`d257696`（E2E）、`2f9eb7b`（editor reverse parity）、`0027c57`（extension/conflict parity）、`fbfb583`（same-value dirty parity）、`65fa5c1`（Markdown mode/security parity）、`dc0b82d`（visual parity） | TXT/Markdown 新建、读取、GFM 预览/HTML 清理、保存、dirty discard、尾随空格校验、错误保留保存、回收站 YAML/Markdown 只读分流和刷新反馈时序已 old/new 实测；新增 11 扩展名入口、延迟 loading、Ctrl+S、ETag 冲突、未保存取消、同值 input dirty 语义、编辑/分栏/预览和安全渲染均已对照；1440×900/390×844 编辑器几何与 computed style 视觉矩阵已通过；编码/大文件、browser-back 和失败矩阵未完 | `rust-editor-reference-parity.spec.ts` 8/8；`rust-editor-visual-reference-parity.spec.ts` old/new 1/1；其中 old/new 双上下文 视觉 1/1，Rust 单实例 bundle 场景 3/3 | 局部 PASS |
| 阅读器 | 10 | `14084bf`（core）、`d18556d`（web）、`ed13571`（全局 block）、`a47dc50`（定位/进度/缓存/导航 E2E） | old/new reference reader-flow 各 17/17；真实上传 EPUB 各 1/1；全局 block 0…37、14/14.0% 进度文案、TOC Escape 焦点、L2 同版本零请求/版本变化重取已实测；触摸/错误/偏好和完整 UI 状态矩阵仍未完 | reader-flow trace、real EPUB trace、`rust-reader-ui.spec.ts` | 局部 PASS |
| 媒体 | 11 | `d18556d`（实现）、`d257696`（E2E）、`b84ce18`（thumb/focus parity）、`dcfc9c1`（video poster etag parity）、`f1671c9`（audio seek persistence timing）、`b625b1c`（video seek persistence timing）、`22f320e`（subtitle text parity）、`7119137`（image wheel anchor）、`25a2194`（卸载 keepalive 进度保存）、`df134c1`（touch gesture parity）、`710e434`（unsupported parity） | 图片/音频/视频桌面/窄屏/触摸、字幕、存储、全屏主链路、缩略图版本参数和预览 Tab 首焦点已有 old/new 实测；视频 poster 的 etag 编码、音频/视频 seek 与关闭保存时序、复杂字幕 entity/换行、图片滚轮锚点、音视频卸载时 `keepalive: true` 最终进度保存、390×844 触摸手势和不可解码原文件错误分流已单独 old/new 验证；损坏/seek 边界仍未完 | `rust-media-parity-ui.spec.ts` 完整 22/22、`rust-video-poster-reference-parity.spec.ts` | 局部 PASS |
| 下载/分享/归档 | 12 | `ff43716`（Range）、`d18556d`（UI）、`d257696`（E2E）、`3967289`（公开分享 transport E2E）、`7b278b7`（归档入口双版本） | 单文件、ZIP、分享生命周期、公开分享安全 headers/Range/无效 token、归档入口/任务密码/状态刷新、preview/206/416 已 old/new 实测；HEAD/大文件/媒体 seek 与视觉状态仍未完 | download/share/action parity trace；公开分享 old/new 追加断言；`rust-archive-reference-parity.spec.ts` old/new 1/1 | 局部 PASS |
| 全局通知与批量下载反馈 | 2、12、15 | `a0e7f8e`、`31ca8e5`、`25c966f`、`eb617a8` | `rust-feedback-reference-parity.spec.ts` old/new 来源用例 7/7；`rust-share-dialog-reference-parity.spec.ts` 重生成/停止分享 1/1；`rust-editor-reference-parity.spec.ts` 放弃编辑 1/1；workspace `cargo xtask check` 通过 | 延迟批量下载、任务完成/失败、分享重生成/停止分享、放弃编辑保留已有 toast 均实际比较 old/new；Rust 初始重生成额外通知与 discard 清空通知已恢复；所有双版本 new fallback 已锁定当前 Rust 18084 | 局部 PASS |
| 文件浏览头新建/上传菜单 | 5、7、8、15 | `6828692` | `rust-file-header-menu-reference-parity.spec.ts` old/new 各 1/1 | 390×844 实际比较新建/上传菜单初始关闭、展开、summary/popover/首项尺寸与层级、hover、空白关闭及“新建文档”动作后的 editor/菜单状态 | 局部 PASS |
| 媒体预览更多菜单与右键语义 | 6、11、15 | `8f51320` | `rust-preview-menu-reference-parity.spec.ts` old/new 各 1/1 | 实际比较文件卡右键默认事件、图片预览更多菜单项/几何/hover、空白与 Escape 关闭、summary 焦点恢复；音频/视频菜单全状态仍待验 | 局部 PASS |
| 分享弹窗 loading/active/二级确认/复制状态 | 2、12、15 | `01c48cb`、`39b4055`、`94ad574`、`c1ce934`、`31ca8e5`、`4940aa2` | `rust-share-dialog-reference-parity.spec.ts` old/new 双上下文 5/5；`cargo xtask check` 通过 | 延迟读取/创建请求期间实际比较可关闭 loading、遮罩、重新打开、active 链接输入、按钮状态、尺寸和文案；二级确认取消/提交关闭/错误回显、复制成功/失败局部状态、active Escape、重生成成功不产生全局 toast、停止分享产生成功 toast 均已对照；分享非 trap 边界已补齐，完整视觉状态矩阵仍未完 | 局部 PASS |
| 选择工具栏文件类型分流 | 6、8、15 | `6c9e46a` | `rust-selection-toolbar-reference-parity.spec.ts` old/new 各 1/1 | 列表实际选择目录、TXT、EPUB、图片、ZIP、未知和双选，比较按钮顺序/文案/图标路径/摘要及关闭后清理；移动端与回收站分支仍待验 | 局部 PASS |
| 选择工具栏移动端与回收站状态 | 6、8、15 | `e26855f` | `rust-selection-toolbar-reference-parity.spec.ts` old/new 各 1/1 | 390×844 实际比较移动端工具栏几何以及回收站已删除 TXT 的“恢复/永久删除”分支；完整 disabled/loading/失败状态仍待验 | 局部 PASS |
| 账户设置用户名编辑入口 | 2、15 | `a6ac08e` | `rust-account-reference-parity.spec.ts` old/new 各 1/1；`cargo xtask check` 通过 | 实际比较用户名编辑按钮的 SVG/路径/14px geometry、会话区结构、hover 颜色、输入聚焦和 Escape 取消；初始 Rust 图标缺失已恢复 | 局部 PASS |
| 账户 TOTP loading 遮罩行为 | 2、8、15 | `077e678` | `rust-account-reference-parity.spec.ts` old/new 双上下文 2/2；`cargo xtask check` 通过 | 延迟 setup 请求期间实际点击子弹窗空白，比较 old/new 的关闭结果；Rust 初始 busy 限制已移除，恢复 reference 可关闭语义 | 局部 PASS |
| 账户密码 loading 外层遮罩行为 | 2、8、15 | `1acb307` | `rust-account-reference-parity.spec.ts` old/new 双上下文 3/3；`cargo xtask check` 通过 | 延迟密码 PATCH 期间实际提交、关闭子弹窗并点击账户外层遮罩，比较账户弹层卸载；Rust 初始外层 busy 限制已移除，按钮 disabled/loading 仍保留 | 局部 PASS |
| 移动/复制 loading、失败与成功反馈 | 8、15 | `8bbbd22`、`f953af8`、`83f7738` | `rust-transfer-dialog-reference-parity.spec.ts` old/new 双上下文 2/2；`rust-mutation-feedback-order-reference-parity.spec.ts` old/new 7/7；`cargo xtask check` 通过 | 延迟移动 PATCH 请求期间实际点击 old/new 遮罩，比较弹窗卸载结果；另以同一 500 响应比较“已移动 0 项，1 项失败：文件：move failed”文案；再以 700ms children 延迟确认成功 toast 等待目录刷新；Rust 初始 busy 限制、失败数量文案和成功反馈时序均已恢复；目标排除、冲突、复制成功和完整错误矩阵仍未完 | 局部 PASS |
| 删除多选与重命名输入边界 | 8、15 | `a46b845`、`f82e7f8`、`8e72809` | `rust-crud-reference-parity.spec.ts` old/new 6/6；`cargo xtask check` 通过 | 双选首项失败/后项成功继续 DELETE、刷新列表、清选择并显示精确反馈；rename 尾随空格原样送入 PATCH；新建文件夹空白/取消/409、rename 409 输入保留/可重试、trash restore/purge 409 的项目/选择/文件名文案均已对照；目录/401/空名/Enter/Esc/loading 等矩阵仍未完 | 局部 PASS |
| 账户设置恢复码下载与剪贴板错误 | 2、15 | `80cf6c3`、`c1ce934` | `rust-account-download-reference-parity.spec.ts` old/new 各 2/2；`cargo xtask check` 通过 | 实际完成 TOTP 启用并读取下载文件；文件名、本地化时间格式、恢复码顺序和换行与 reference 一致；另注入剪贴板失败，比较局部错误、按钮状态和无全局 toast；初始 Rust ISO 时间格式已恢复 | 局部 PASS |
| 全局 Toast 严重级别与时序 | 2、15 | `253e92d`、`c1ce934`、`3657c79`、`83f7738`、`31ca8e5`、`25c966f`、`eb617a8` | `rust-feedback-reference-parity.spec.ts` old/new 7/7；分享/TOTP 剪贴板失败 old/new 各 1/1；分享重生成/停止分享 old/new 1/1；放弃编辑保留 toast old/new 1/1；CRUD/回收站/移动成功反馈 old/new 7/7；`cargo xtask check` 通过 | 实际比较成功/409 错误 Toast 的文案、class、role、CSS/命中区域、最新通知和 3.6 秒消失；局部剪贴板失败不会误发全局 toast；断网/403/连续不同错误、后台任务完成/失败、分享重生成/停止分享和 editor discard 保留语义均已对照；初始 Rust 的 error class/额外 role/transport `TypeError:`/重生成额外 toast/discard 清空 toast 已恢复，401 会话安全边界、完整逐调用点状态矩阵仍未完 | 局部 PASS |
| 侧栏状态与响应式交互 | 4、15 | `6710996`、`65ead82` | `rust-sidebar-state-reference-parity.spec.ts` old/new 1/1；`rust-sidebar-tree-reference-parity.spec.ts` old/new 2/2 | 桌面 active/hover、路径展开、折叠 rail、390×844 移动抽屉及过渡完成后的尺寸/颜色/布局实际对照；分类树递归展开/计数/过滤/active/根路径已双版本对照；文件树实际暴露 reference 未注册 `SidebarDirectoryNode`，Rust 保留可用递归导航 | 局部 PASS |
| 通用弹窗 Escape 语义 | 2、8、15 | `c0a5fd9` | `rust-dialog-keyboard-reference-parity.spec.ts` old/new 1/1；WASM/web build 通过 | 实际打开新建文件夹弹窗、聚焦输入、按 Escape，对照关闭结果和 window bubble 的 `defaultPrevented=false` | 局部 PASS |
| 弹层 history 与外置确认框生命周期 | 5、8、15 | `55ef117`、`a7c669e` | `rust-modal-history-reference-parity.spec.ts` old/new 1/1 | 实际在分享弹层上打开停止分享确认框后按浏览器后退，比较 share modal 与外置 AppDialog 的卸载/保留结果和 pathname；Rust 初始额外清除 dialog，已恢复 reference 行为 | 局部 PASS |
| 分类媒体视图与文件项交互 | 4–6、15 | `1ffae0e` | old 原始 `library-ui.spec.ts` 4/4；Rust `rust-library-ui.spec.ts` 8/8；双版本 `rust-library-reference-parity.spec.ts` 1/1；`cargo xtask check` | 同一 fixture 实际比较书架/图库/音乐标题、分组、卡片/行、视图切换、图片到视频的会话状态延续及分类卡/音频行 `contextmenu.prevent`；Rust 初始三处差异均已恢复 | 局部 PASS |
| API 路由与文档生命周期 | 13、15–16 | `da88991`、`eac64a8` | `rust-api-reference-parity.spec.ts` old/new 2/2；`rust-specialized-api-reference-parity.spec.ts` old/new 1/1；服务端 upload error 单测 1/1；`cargo fmt`、workspace check 已通过 | 旧 route registry、old/new caller 反向清点；认证/存储/library/status/tasks/文件/回收站/分享缺失矩阵及文档创建→回收生命周期实际双实例对照；专用媒体/阅读器成功 API、upload session 和任务详情实际对照；Rust upload complete 漏写 ETag 已恢复 | 局部 PASS |
| 全量 API caller 与最终视觉回归 | 13–16 | 待提交 | API matrix 已完成第一轮 route/caller 反向登记并新增双实例探针；全量状态、无障碍、响应式、CSP/监听器审计未完 | 待补齐 | 未完成 |

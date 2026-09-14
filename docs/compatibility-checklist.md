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
- `[P]` 当前兼容恢复工作树 HEAD：`bb6edba`；上面的 `e9b6202` 保留为恢复开始时的 Rust 基线，后续每个逻辑模块均以独立提交推进。
- `[P]` 初始工作区在本清单创建前干净；本清单必须先独立提交，再进入功能恢复提交。

### 1.2 隔离运行实例

- `[P]` old worktree：`/tmp/revaro-old`，detached `3a18bde`，服务 `http://127.0.0.1:18080`。
- `[P]` new 基线 worktree：`/tmp/revaro-new`，detached `e9b6202`；当前恢复中的 new 实际运行实例从 `/config/revaro` 当前工作树构建，服务 `http://127.0.0.1:18083`（`18082` 仅保留早期对照记录）。
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
- `2026-09-14`，old `18080` / new `18082`：任务中心/导航/文件交互定向集合两版均 7/7 通过；覆盖等待密码、活跃/完成/取消/失败/不可重试、显示更多、取消、重试、清除完成、桌面/移动切换、列表选择、面包屑和回收站返回。
- `2026-09-14`，old `18080` / new `18082`：侧栏持久化与移动抽屉定向集合两版均 2/2 通过；折叠 rail 和分类手风琴刷新后恢复，移动端隐藏桌面控件、遮罩关闭和六个一级入口一致。
- `2026-09-14`，old `18080` / new `18082`：旧版 `e2e/auth-status.spec.ts` 两项均通过，状态 SSE 三卡、纵向布局、无伪卡片、Esc/空白关闭和未登录 401 响应一致。
- `2026-09-14`，old `18080` / new `18082`：真实 TXT 深链接 `/read/{id}` 均回到根目录且不打开阅读器；这是 reference 运行时现状（旧源码虽有 `openDeepLink` 意图），当前 Rust 未引入额外差异，暂不把旧版自身缺陷冒充 Rust 回退。
- `2026-09-14`，old `18080` / new `18082`：已登录页面中途把目录 children 请求改为 401 时，old 保留壳层并显示 `session expired` toast，new 回到登录页；Rust 保留这一安全边界，避免过期 session 下继续展示旧数据，属于允许的安全强化，正常成功路径不变。
- `2026-09-14`，old `18080` / new `18083`：无效 `/f/compatibility-folder-that-does-not-exist` 登录后均回到 `/`，加载“我的文件”，不留下错误状态；另以 mock API 实际打开 `/library/book`、`/library/image`、`/library/video`、`/library/audio/f/compatibility-route-folder`、`/library/file` 和 `/f/compatibility-route-folder`，两版页面与规范 URL 一致。
- `2026-09-14`，old `18080` / new `18083`：实际创建目录并点击进入后，两版均将 `/` → `/f/{id}` 写入应用内 history；浏览器后退逐级回到根目录。再次前进时两版均只恢复 `/f/{id}` URL、不重放目录请求，这是 reference 的现运行时行为，已用同一用例明确记录而不把它误判为 Rust 差异。
- `2026-09-14`，old `18080` / new `18083`：实际打开账户设置后浏览器后退，两版均先关闭账户弹层、保留“我的文件”页面和 `/` URL；弹层 history 语义已加入 parity 用例。
- `2026-09-14`，old `18080` / new `18083`：将 `POST /api/directories` 同时模拟为 409，旧版关闭新建文件夹弹窗并显示错误 toast；Rust 初始行为把错误留在弹窗内，已恢复为关闭弹窗 + toast。两版回归均通过；分享二次确认错误仍按分享层单独验证。
- `2026-09-14`，old `18080` / new `18083`：新建文件夹成功后再触发一次根目录刷新，两版均保留“文件夹已创建”成功 toast；Rust 初始目录/回收站刷新会清空全局反馈，已移除该非 reference 行为。old/new `rust-actions-parity-ui.spec.ts` 的目录刷新用例通过。
- `2026-09-14`，old `18080` / new `18083`：列表选中一项后滚动到顶部并真实点击内容区左上空白，旧版和 Rust 版均清除选择工具栏（`rust-file-interaction-parity.spec.ts` 1/1 each）；文件行、按钮和工具栏仍由过滤规则排除，不会误清除。
- `2026-09-14`，old `18080` / new `18083`：实际聚焦生产方块文件卡后按 Space，旧版 `FileCard.vue` 的 `.prevent` 使页面保持 `scrollY=150`；Rust 初始版滚到 `574`，已恢复无条件 `prevent_default`（仅在可选择时切换选择），old/new `rust-file-interaction-parity.spec.ts` 均 1/1。
- `2026-09-14`，old `18080` / new `18083`：实际上传无封面 EPUB 并读取浏览器 DOM，旧版书籍图标 `path.icon-detail` 的 `d` 与 Rust 初始版仅一处几何字符串不同（`-13 1` vs `-13-1`）；已恢复 reference 路径，old/new 书籍图标几何用例均 1/1。
- `2026-09-14`，old `18080` / new `18083`：实际上传无封面 EPUB、等待缩略图失败后检查卡片 class，旧版只保留 `file-card book-tile fallback-tile`；Rust 初始版错误地同时保留 `preview-tile`，已让缩略图失败状态联动外层 class。另实际上传 WebM，旧版视频卡为 `preview-tile`、Rust 初始版漏标，已恢复；对应 file-interaction 用例两版各 2/2。
- `2026-09-14`，old `18080` / new `18083`，390×844：实际聚焦媒体库图片方块卡和音乐列表行按 Space，旧版均阻止页面滚动；Rust 初始 `LibraryCard`/`LibraryRow` 缺少对应键盘处理，已恢复无条件 `prevent_default`。old/new `rust-library-ui.spec.ts` 定向用例均 1/1。
- `2026-09-14`，old `18080` / new `18083`：实际对照 Logo 回根、系统状态面板点空白/Escape/重复点击关闭、桌面与移动端侧栏回收站 footer。Rust 初始移动 footer 点击时错误关闭抽屉，已移除额外关闭；尺寸、路径和其余状态均与 old 一致，`rust-navigation-parity.spec.ts` old/new 定向用例均 1/1。
- `2026-09-14`，old `18080` / new `18083`：模拟 `/api/library/all` 首次 503、再次刷新延迟返回合法图片条目，旧版错误态、重试 loading、恢复内容和刷新图标路径已逐项对照；Rust 初始 `RefreshCw` 几何不同，已恢复四段 reference path，`rust-library-ui.spec.ts` old/new 定向用例均 1/1。
- `2026-09-14`，old `18080` / new `18083`：模拟四张图片分布在根目录、两级 `归档 / 旅行` 和 `归档 / 工作`，逐项点击分类路径树；根/节点计数、首层默认展开、子路径展开、过滤后的卡片数、active 行和回到“全部位置”均一致。空分类另验证“还没有图片内容”路径提示、空态文案和“上传文件”入口，old/new 各 1/1。文件目录树在媒体分类切换后的 old 运行中出现旧版自身异步加载竞态（old 未显示子目录、new 显示），未将其伪记为 Rust 已通过，仍需用稳定真实目录场景单独裁定。
- `2026-09-14`，old `18080` / new `18083`：`rust-icon-reference-parity.spec.ts` 在两个独立浏览器上下文中用同一 mock 数据逐项读取实际 DOM；顶栏任务、系统状态三张服务卡、五类侧栏/路径树、回收站、折叠、文件视图、新建/上传、任务取消/密码/重试/完成展开，以及移动端抽屉和账户工具入口的 SVG 几何均一致。Rust 初始版本中任务取消、密码、重试、媒体控制、状态卡、文件操作等多个 Lucide 几何差异已按 old `@lucide/vue` 1.41.0 恢复；任务中心“展开其余/收起”箭头也恢复，媒体/文件项全类型图标仍待继续覆盖。
- `2026-09-14`，old `18080` / new `18083`：`rust-directory-picker-reference-parity.spec.ts` 在两个独立浏览器上下文中从列表行实际打开“移动”入口，逐项对照目录选择器触发器、根路径、子目录、深层路径和空目录状态的 SVG 几何，并实际点击目标目录、按 Escape 关闭；两版均 1/1。Rust 初始目录选择器的 ChevronRight 方向错误且缺少 reference 的 stroke/fill 属性，已恢复旧版 Lucide 几何；`rust-actions-parity-ui.spec.ts` old/new 各 9/9，移动/复制的排除、冲突和完整结果矩阵仍待验。
- `2026-09-14`，old `18080` / new `18083`，390×844：`rust-breadcrumb-layout-reference-parity.spec.ts` 先实际暴露 Rust 面包屑额外 `span` 导致每个路径项都获得首/末项移动端 margin（old 1/1 对照失败），随后移除包装并恢复 direct `button`/`ChevronRight` 子节点；修复后 old/new DOM 层级、每项 margin 和深层横向位置均 1/1，导航全套仍保留在 `[ ]` 直到中间级/键盘/触摸矩阵完成。
- `2026-09-14`，old `18080` / new `18083`：实际点击媒体分类和路径树展开控件后读取 SVG computed transform，旧版分类/路径箭头均为 `matrix(0, 1, -1, 0, 0, 0)`，Rust 初始版为 `none`；已恢复动态展开态的 90° 旋转，`rust-icon-reference-parity.spec.ts` old/new 各 1/1。
- `2026-09-14`，old `18080` / new `18083`：将创建目录 POST 延迟 800ms，old 点击“创建”后通用确认弹窗立即移除，Rust 初始版停留在“处理中…”直到请求完成；已恢复旧版同步关闭/后台等待语义，重命名弹窗仍按旧版保留保存中状态，`rust-actions-parity-ui.spec.ts` old/new 各 10/10。
- `2026-09-14`，old `18080` / new `18083`：反向对照 `SelectionToolbar.vue` 的打开按钮分流，旧版对同时满足 editable/book 的 `.txt` 显示书本“阅读”图标，Rust 初始版错误显示编辑图标；已按旧版条件顺序恢复，`rust-selection-toolbar-icon-reference-parity.spec.ts` old/new 各 1/1。另实际点击列表行“移动”后，旧版选择工具栏立即隐藏而 Rust 初始版仍显示；已让媒体预览、阅读器、编辑器、移动/复制、分享和账户弹层按旧版隐藏工具栏，`rust-actions-parity-ui.spec.ts` old/new 各 10/10。
- `2026-09-14`，old `18080` / new `18083`：任务中心以两个活动任务的 1%/2% 原始进度实际对照，旧版先求平均再四舍五入为 2%，Rust 初始版逐项取整并整数除法显示 1%；已恢复 reference 聚合顺序，`rust-task-center-parity.spec.ts` old/new 各 4/4。
- `2026-09-14`，old `18080` / new `18083`：任务中心请求延迟期间实际读取旧版 DOM，旧版仍显示“还没有后台任务”，Rust 初始版错误显示“正在读取任务…”；已恢复旧版的空任务 fallback，并保留请求完成后的分组行为，`rust-task-center-parity.spec.ts` old/new 各 5/5。
- `2026-09-14`，old `18080` / new `18083`：同一 mock 任务和三张状态卡实际读取 badge 的 class、尺寸、padding 与文字；旧版顶栏任务 badge 和服务卡 badge 均为 `size-sm`，Rust 初始版分别过大或缺少尺寸 class；已恢复 `size-sm`。缓存命中率用 2/3 暴露旧版 `Math.round` 与 Rust 初始整数除法的 67%/66% 差异，也已恢复。`rust-global-ui-reference-parity.spec.ts` old/new 各 1/1，聚合导航/任务/图标集合 old/new 各 15/15。
- `2026-09-14`，old `18080` / new `18083`：分类切换实际记录 `/api/library/all` 请求次数；旧版首次加载后在书架、图库、视频、音乐、文件分类间复用快照，只有明确 Refresh 才重新读取。Rust 初始版每次分类切换都重新请求；已恢复缓存视图与 force refresh 分流，并保持 force refresh 失败后旧缓存仍可供后续分类切换复用。`rust-library-ui.spec.ts` old/new 各 8/8，相关提交为 `d068eb8`、`bb6edba`。
- `2026-09-14`，old `18080` / new `18083`：选中列表文件后发起延迟且返回 500 的目录导航，旧版在 loading 和失败后均保留原列表与选择工具栏，Rust 初始版立即清掉选择；已将清空时机移到成功导航分支。old/new `rust-navigation-parity.spec.ts` 定向用例各 1/1，修复提交 `db5b963`。
- `2026-09-14`，old `18080` / new `18083`：侧栏媒体库根节点 tooltip 实际为 `title="我的文件"`，Rust 初始版为空；已按旧版在 path 为空时回退到节点名称。`rust-library-ui.spec.ts` old/new 完整用例各 8/8，修复提交 `9d4ea2b`。
- `2026-09-14`，old `18080` / new `18083`：实际登录后注销并卸载认证壳层，旧版会移除顶栏/侧栏各自注册的两个 `matchMedia` change 监听，Rust 初始 helper 永久保留、移除数为 0；已恢复组件生命周期清理，`rust-navigation-parity.spec.ts` old/new 各 10/10。
- `2026-09-14`，old `18080` / new `18083`：媒体预览实际打开图片并展开胶卷，旧版缩略图地址始终为 `/thumbnail?v=<etag>`，Rust 初始版漏掉版本参数；同时从预览根节点按 Tab，旧版先聚焦“更多操作”原生 `summary`，Rust 初始焦点循环漏选该节点。已恢复带编码 etag 的缩略图 URL 和旧版焦点候选规则；`rust-media-parity-ui.spec.ts` old/new 全部 13/13，新增焦点与 URL 断言，代码提交 `b84ce18`。
- `2026-09-14`，old `18080` / new `18083`：按旧版 reader-flow reference 逐页、逐目录项、逐次翻页运行 17 项；两版均 17/17 通过。覆盖稳定窗口、热路径零重复请求、字号/行距客户端重排、跨 spine、父级/随机 TOC、未加载 chunk、连续翻页、旋转、图片 NavAnchor、无 fragment 回退、L2 重开/版本变化和阅读器视觉覆盖层。L2 用例仅在每次测试开头清理浏览器 Cache Storage，并等待异步请求完成，确保共享 Chromium 进程不会把上一次测试的缓存当作 reference 初始设备状态。
- `2026-09-14`，old `18080` / new `18083`：真实上传 EPUB old/new 各 1/1；DOM 中全书 `data-block` 均为连续唯一的 `0…37`，翻页后的页码和无障碍文案均为 `14` / `阅读进度 14.0%`，TOC Escape 关闭后焦点回到 `#toc-button`。Rust 曾在每个 spine 内重复注入 global block 编号，已由 `ed13571` 恢复旧版全书编号后通过。
- `2026-09-14`，old `18080` / new `18083`：反向对照 reader 的文本 fragment 二分定位、媒体 visual start、点击点/可见块回退、DOM 文本进度计算、TOC 导航深度保护、windowSync 取消/恢复、Tab 候选和键盘 Enter 行为；Rust reader/cache 与真实 EPUB 验收代码提交 `a47dc50`，old/new reader-flow 各 17/17，真实 EPUB 各 1/1。
- `2026-09-14`，old `18080` / new `18083`：文档编辑器专用探针先实际暴露三处 Rust 回退：新文档扩展名校验先 trim 导致尾随空格被错误接受，校验错误时保存按钮消失，回收站 YAML 等非 Markdown 可编辑文件被错误设为 Preview；另以延迟 1.2 秒的目录 children 响应验证旧版保存成功 toast 必须等待刷新完成。`2f9eb7b` 恢复原始输入校验、错误时保留保存操作、只读 Markdown 分流和刷新完成后的反馈时序；修复后 `rust-editor-reference-parity.spec.ts` old/new 各 3/3，覆盖尾随空格错误、YAML/Markdown 回收站只读状态和保存刷新时序。
- `2026-09-14`，old `18080` / new `18083`：旧版原始 `e2e/auth-status.spec.ts`、`mobile.spec.ts`、`library-ui.spec.ts`、`files.spec.ts`、`media-ui.spec.ts`、`reader-flow.spec.ts` 分别为 2/2、1/1、4/4、3/3、10/10、17/17；两版均通过。三本真实 EPUB 原始 `reader-real-epub.spec.ts` old 1/1（约 1.5 分钟）、new 1/1（约 3.3 分钟）；完整 reference 行为集合已可在两隔离实例执行。
- `2026-09-14`，Rust 工作树此前执行 `cargo fmt --all && cargo xtask check` 通过：workspace unit/integration/doc tests、clippy `-D warnings`、WASM target check 均通过；最新 download 兼容修复另执行 `cargo test -p revaro-server file_routes --lib`（22/22）和 `cargo xtask web-build`，并用新 bundle 完成 reader 4/4 与 old 共享 reader 2/2。

## 2. 启动、认证和全局壳层

| 状态 | 条目 | 旧版规范与验收点 | 当前 Rust 初检 |
|---|---|---|---|
| `[P]` | 启动画面 | 首屏 splash、logo、spinner、加载到登录/主界面的时序、网络慢和失败状态一致；启动过程不闪出错误主界面。 | old/new `rust-auth-parity.spec.ts` 延迟 session 期间均显示 splash，随后登录失败状态一致 |
| `[P]` | 登录 | 用户名/密码输入、回车提交、按钮 loading/disabled、错误文案、焦点、密码可见性（如有）、重复提交和网络错误一致。 | old/new `rust-auth-parity.spec.ts` 的慢响应、Enter 提交和失败状态均通过 |
| `[P]` | TOTP 登录 | 需要二次验证时的输入、回退、错误、重试、恢复码路径和 session 建立一致。 | old/new `rust-auth-parity.spec.ts` 的二次输入、错误保留和重试分支均通过 |
| `[P]` | 会话检查 | `/api/auth/me`、刷新页面、已过期 cookie、401 后回登录页且不遗留旧数据。 | 初始/刷新过期 cookie 两版均回登录；中途 401 old 保留壳层+toast，new 回登录以清除过期 session 下的旧数据，记录为安全强化例外 |
| `[P]` | 账户入口 | 顶栏账户按钮应打开“账户设置”而不是直接退出登录；用户名、头像、菜单文案和层级一致。 | old/new 桌面实际点击均打开账户设置；移动端工具菜单入口已对照，退出动作仍在独立条目验证 |
| `[P]` | 账户设置 | 账户资料、用户名修改、头像读取/上传/删除、密码修改、TOTP 状态/setup/enable/recovery/delete、成功/失败/取消/关闭行为一致。 | old/new `rust-account-parity.spec.ts` 2/2 与 `rust-password-parity.spec.ts` 1/1 通过，覆盖头像、用户名、密码、TOTP 全链路和错误/关闭 |
| `[P]` | 退出登录 | 只在账户设置或移动端工具菜单的明确“退出登录”动作触发；成功后清空 session/任务/页面状态并回登录页。 | old/new 明确点击账户设置内“退出登录”后回登录页；账户入口本身不会退出 |
| `[ ]` | 全局错误/Toast | 成功、失败、权限过期、冲突、网络断开、复制剪贴板失败的 toast 文案、颜色、时长、关闭方式和堆叠顺序一致。 | 成功 toast 颜色/时限、目录错误 toast 和操作失败已对照；权限过期、断线、剪贴板失败及堆叠顺序仍待验证 |
| `[ ]` | 全局键盘 | Escape 关闭当前最内层弹窗/菜单，Enter 提交可提交表单，Tab 焦点不越界；浏览器后退的 modal/folder 语义一致。 | 顶栏/状态/任务/侧栏/内容菜单的 Escape、主要 Enter 和弹层 history 已有 old/new 用例；Tab 焦点边界及完整叠层顺序仍待验 |

## 3. 顶栏、任务中心和系统状态

| 状态 | 条目 | 旧版规范与验收点 | 当前 Rust 初检 |
|---|---|---|---|
| `[P]` | Logo/回到根目录 | 桌面和移动端 logo 图标、`回到我的文件` aria/title、点击后路径、active 状态一致。 | old/new 桌面、移动端从分类/回收站点击 Logo 均回根；按钮尺寸和 title/aria 已在 `rust-navigation-parity.spec.ts` 对照 |
| `[P]` | 任务中心入口 | 顶栏独立任务中心图标/summary，入口位置、图标、数量/状态提示、点击展开和再次点击关闭一致；不能被上传入口替换。 | old/new 实际点击 summary 均展开任务面板；空状态、点击空白和 Escape 已对照 |
| `[P]` | 任务面板分组 | 活跃、已完成/已取消、失败分组；上传/归档解压/字幕任务标签、进度、状态中文文案、平均进度和空状态一致。 | old/new `rust-task-center-parity.spec.ts` 覆盖 waiting/active/completed/cancelled/failed、不可重试、完成空态、上传/归档标签、显示更多及原始小数进度先平均再四舍五入 |
| `[P]` | 任务操作 | 取消、重试、清除已完成、归档密码输入、任务详情、失败错误、超过四项时“显示更多”、任务流实时更新一致。 | old/new 定向 7/7：取消、重试、继续输入密码、清除完成、空白/Escape/入口关闭均通过；任务详情入口在 reference 无独立页面，归档行即输入入口 |
| `[P]` | 任务面板交互 | 面板不被背景遮挡、点击面板不关闭、点空白关闭、Esc 关闭、点击入口切换、loading/error/empty 一致。 | old/new mock、延迟初始读取和空状态均验证；桌面/移动端切换不会重复拉取或断开共享 SSE，`rust-task-center-parity.spec.ts` old/new 各 5/5 |
| `[P]` | 系统状态入口 | 在线/状态球可点击；`aria-label=打开系统状态`、title=`系统状态`、颜色/ok 状态和位置一致。 | old/new 实际点击均展开状态面板；aria/title、ok 状态和三卡布局已对照 |
| `[P]` | 系统状态面板 | EventSource `/api/system/status/stream` 更新状态；DB、存储、缓存三张纵向卡片，状态 badge、详情/错误/加载一致；旧版没有“任务/清理队列/备份”伪卡片和刷新按钮。 | old `e2e/auth-status.spec.ts` against old/new 2/2；三卡文案/纵向布局/真实首帧、无伪卡片、SSE 入口和缓存命中率四舍五入均一致 |
| `[P]` | 系统状态关闭 | 点空白、Esc、重复点击、401/断线/重连/服务异常状态一致，关闭后 SSE 清理。 | old/new 实际点空白、Esc、重复点击均关闭；首帧、SSE 生命周期已在 `auth-status` 对照，401/断线/重连异常矩阵仍待验证 |
| `[P]` | 回收站入口 | 顶栏回收站图标、title=`回收站`、aria、点击进入 trash 路由、数量/空状态和返回根目录一致。 | old/new 桌面顶栏与侧栏 footer、移动 footer 均可进入回收站；空态/返回根和尺寸已对照，入口按 reference 保持根 URL |
| `[P]` | 移动端顶栏 | 状态球仍可用；头像/工具菜单包含旧版实际项目：任务中心、回收站、账户设置；不出现旧版明确禁止的 `打开任务与工具菜单` 旧入口；遮罩/外部点击/Esc 一致，退出登录仍从账户设置进入。 | old/new 390×844 实测状态球、工具菜单三项、任务中心跳转、外部点击和 Escape 均通过；旧版工具菜单本身没有独立退出项 |

## 4. 侧栏、分类入口和路径树

| 状态 | 条目 | 旧版规范与验收点 | 当前 Rust 初检 |
|---|---|---|---|
| `[P]` | 五个一级分类 | 侧栏入口顺序、图标和文案为：书架、图片、视频、音乐、文件；每项 active/current、点击路由和返回行为一致。 | old/new `rust-library-ui.spec.ts` 与导航定向用例确认顺序、文案、active/current、点击切换和返回 |
| `[ ]` | 分类数据 | 分类数量、空状态、刷新/loading/error、书籍/图片/视频/音乐/普通文件各自对应 `/api/library` 视图一致。 | `/api/library/all`、五类有数据视图、分类 503 → 重试 loading → 恢复内容、空分类路径/空态、首次快照缓存与显式刷新已在 old/new 对照；各类数量、旧内容保留和完整错误矩阵仍待验 |
| `[ ]` | 分类路径 | 分类主项和展开控制、路径树/文件树、当前路径高亮、展开/收起、加载/空/错误、点击文件夹进入对应分类路径一致。 | 多级媒体路径树计数、默认展开、展开/过滤、active、展开箭头旋转、根节点 tooltip 和空路径提示 old/new 已对照；文件目录树加载/递归及旧版切换竞态仍待稳定场景裁定 |
| `[P]` | 分类持久化 | `revaro:sidebar:collapsed`、`revaro:sidebar:expanded` 的值、恢复时机和坏值处理一致。 | old/new `rust-navigation-parity.spec.ts` 刷新后分别恢复折叠和 book 手风琴；坏值均回默认状态 |
| `[P]` | 桌面侧栏折叠 | 折叠 rail、展开按钮、tooltip/aria、内容宽度/动画、刷新后恢复、当前页仍可识别一致。 | old/new `rust-navigation-parity.spec.ts` 实测 rail、`aria-expanded`、刷新恢复、展开恢复和移动端不复用 rail |
| `[P]` | 移动端分类抽屉 | 宽度 `min(300px,78vw)`；只显示一级入口（书/图/影/音/文件/回收站），不显示树、数量或 chevron；50px 行高；浮动 handle、backdrop、点击空白、Esc、打开/关闭跟随一致，内容不位移。 | old/new 390×844 实际打开、检查六个入口/无目录树、点 backdrop、Escape、重复开关；`rust-library-ui.spec.ts` 3/3 |
| `[ ]` | 侧栏图标 | Lucide 风格、stroke、大小、对齐、active/hover/disabled 颜色和五类具体图标与旧版一致，不用“看起来相似”的替代图标。 | `rust-icon-reference-parity.spec.ts` 已在 old/new 浏览器逐项比对侧栏、路径、折叠、回收站和移动抽屉 geometry；active/hover/disabled 全状态及全部文件类型仍待验 |
| `[P]` | 回收站 footer | 桌面/移动端位置、图标、active、点击和 trash empty 状态一致。 | old/new 桌面尺寸、移动端 footer 点击、回收站空态和移动抽屉保持打开的 reference 语义已实测 |

## 5. 文件浏览、路由和全局内容区

| 状态 | 条目 | 旧版规范与验收点 | 当前 Rust 初检 |
|---|---|---|---|
| `[ ]` | 根目录内容头 | `我的文件` 标题、当前路径 nav、项目数/文件数/大小三项 metadata 的文案、间距和层级一致。 | old/new 首屏层级已恢复；同一 fixture 下统计、空态和刷新仍需验证 |
| `[ ]` | 面包屑 | `当前路径` nav、根和各级名称、Lucide chevron-right 分隔、当前项样式、点击中间级、超长路径横向滚动、键盘/触摸行为一致。 | old/new 深层路径实际创建并打开，移动端横向滚动、browser back、点击根、smooth-scroll、DOM 层级和首末项 margin 已对照；中间级、键盘/触摸全矩阵仍待验 |
| `[P]` | 文件夹路由 | `/`、`/f/{id}`、`/library/{book|image|video|audio|file}`、分类下 `/f/{folder}` 的地址、刷新、直接打开、无效 id、权限错误和回退一致。 | old/new 直达浏览器用例覆盖五类分类、分类路径、文件夹路径和无效 `/f/{id}`；无效地址均回根并加载默认页面 |
| `[ ]` | 深链接 | `/read/{fileId}` 打开旧版阅读器；媒体/文件深链接、登录后回到目标、无效深链接错误/返回一致。 | old/new 真实 TXT `/read/{id}` 均实际回根且不打开阅读器，已确认是 reference 运行时缺陷；需单独决定是否恢复源码意图，当前不新增偏离旧版的行为 |
| `[ ]` | 浏览器历史 | 文件夹进入 pushState；返回/前进恢复文件夹/分类；先关闭 modal 再回退页面；stale request 不覆盖新路径。 | old/new 已实际覆盖目录进入、后退、前进 URL 现象和账户弹层后退关闭；分类历史、stale request 和完整 modal stack 仍待验证 |
| `[ ]` | 网格/列表切换 | 默认值、按钮图标/tooltip/active、内容布局、滚动、刷新后状态和移动端响应式行为一致。 | 基础切换存在 |
| `[ ]` | loading/empty/error | 首次加载、切换路径、网络失败、空根、空分类、空回收站、重试按钮、旧内容保留策略和文案一致。 | 空根/空回收站文案、模拟读取失败 toast，以及失败导航中旧内容/选择工具栏保留策略已 old/new 对照；首次 loading、重试和完整旧内容保留矩阵仍待验 |
| `[ ]` | 拖放 | 桌面拖入文件/文件夹、拖动经过/离开/放下、overlay、非法目标、重复文件、取消和上传结果一致。 | 上传控制器有基础实现，UI 状态待验证 |
| `[ ]` | 响应式布局 | 桌面、平板、390px 手机宽度下内容区、侧栏、顶栏、工具栏、对话框和滚动容器的宽高/层级一致。 | 待对照截图 |

## 6. 文件项、图标和选择/操作菜单

| 状态 | 条目 | 旧版规范与验收点 | 当前 Rust 初检 |
|---|---|---|---|
| `[ ]` | 文件卡/行 | 文件名、大小、类型、更新时间、目录/媒体/文档标识、thumbnail/cover、fallback 和截断规则一致；方块与列表都验证。 | Rust 有 FileTile/rows 基础，视觉待对照 |
| `[ ]` | 图标系统 | 文件夹、文本文档、EPUB、图片、音频、视频、归档、未知文件的旧版图标路径、stroke、颜色、尺寸、背景和状态叠加一致。 | 全局 Lucide 几何已在 old/new 浏览器入口中逐项修复并覆盖任务/状态/菜单/媒体控制关键集合；文件项各类型、fallback、颜色和状态叠加仍待同一 fixture 截图对照 |
| `[ ]` | hover/active/disabled | 卡片 hover、键盘 focus、选中 active、不可用、loading、任务中覆盖层、错误状态和 pointer 行为一致。 | 待验证 |
| `[P]` | 选择入口 | 旧版生产路径只在列表行提供 `选择项目` 控件；点击不打开项目，选中后工具栏更新，取消选择/全选和跨项状态一致；默认方块网格没有选择控件；内容空白点击清除选择，文件行/按钮/工具栏点击不误清除。 | old/new `rust-file-interaction-parity.spec.ts`、actions parity 实测列表显式选择、清除、空白点击和选择模式；旧版 `FileGrid` 的 `selectable` 未开启 |
| `[P]` | 触摸选择 | 旧版生产路径为列表显式选择按钮；进入选择模式后轻触行切换选择，普通轻触打开项目；旧版 tile 的 480ms 长按函数因生产网格 `selectable=false` 不可达，不作为用户行为。 | old/new 390×844 实际验证选择按钮、选择模式轻触不打开编辑器；未将不可达长按代码迁入 Rust |
| `[ ]` | 右键/更多菜单 | 文件/文件夹右键或 more 入口、菜单锚点、菜单项顺序、点空白关闭、Esc、边缘翻转和 item disabled 状态一致。 | 待验证 |
| `[ ]` | 打开动作 | 目录进入；可编辑文本进入 editor；EPUB 进入 reader；图片/音频/视频进入 preview；未知类型下载/预览策略、回收站只读行为一致。 | Rust 有部分 open logic，完整矩阵待验 |
| `[ ]` | SelectionToolbar | 选中计数/总大小、清除、全选、打开、下载、分享、重命名、移动、删除、恢复、永久删除、归档解压等按钮的出现条件和文案一致。 | `.txt` 同时 editable/book 时的阅读图标、打开移动/复制/预览/阅读器/编辑器/分享/账户弹层时隐藏工具栏已 old/new 对照；完整出现条件、disabled、计数/总大小和所有文件类型矩阵仍待验证 |

## 7. 上传入口、队列和任务联动

| 状态 | 条目 | 旧版规范与验收点 | 当前 Rust 初检 |
|---|---|---|---|
| `[P]` | 上传入口 | 文件浏览头部独立上传菜单，含“上传文件”“上传文件夹”；桌面按钮、移动端下拉、图标、点击外部/Esc 关闭一致。 | old/new 桌面与移动端均实际展开同一菜单；“上传文件/上传文件夹”及说明文案、Escape 关闭已对照 |
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
| `[ ]` | 新建文档 | 桌面直接入口和创建菜单中的“新建文档”、默认名 `未命名文档.md`、创建后进入 editor、取消/失败一致。 | old/new 已验证创建菜单、默认名、进入 editor、立即关闭和保存重开；取消、失败仍待收口 |
| `[ ]` | 重命名 | 单选条件、输入初值/扩展名规则、冲突、空白、Enter/Esc、PATCH 结果和列表更新一致。 | 基础 API/UI 部分存在 |
| `[ ]` | 移动 | DirectoryPicker 面包屑、实时目录浏览、加载/错误/空、排除自身/子目录、目标选中、确认/取消/冲突和 PATCH 结果一致。 | old/new 触发器、面板定位/DOM、路径图标几何、实际移动和清理已对照；排除子目录/冲突/错误仍待验 |
| `[ ]` | 复制 | 目标选择、目录/文件、同名处理、任务或立即结果、完成刷新和错误一致。 | old/new 媒体更多菜单实际复制并验证原文件保留；普通文件、同名和失败仍待验 |
| `[ ]` | 删除 | 确认文案、单项/多项、目录、取消、loading、移入回收站、selection 清理和列表刷新一致。 | old/new 多选删除确认文案、单文件清理链路已对照；目录、取消/loading/失败仍待验 |
| `[ ]` | 回收站查看 | 列表/网格、原路径/删除时间/大小、空状态、打开限制、恢复/永久删除入口一致。 | old/new 空回收站、列表行元信息、TXT 键盘打开分流已对照；完整 grid/只读矩阵仍待验 |
| `[ ]` | 恢复 | 单项/多项恢复、原位置可用/冲突、成功/失败文案、刷新和 selection 一致。 | old/new 直接恢复和清理已实际验证；冲突、失败、多选仍待验 |
| `[ ]` | 永久删除 | 单项确认、清空回收站确认、不可恢复警告、loading/失败/成功及列表更新一致。 | old/new 永久删除确认、清理链路已对照；清空回收站、失败/loading仍待验 |
| `[ ]` | 对话框通用行为 | backdrop、Esc、焦点、按钮顺序、危险色、空输入 disabled、提交中禁用和错误保留输入一致。 | 新建操作的空值、Esc、backdrop、disabled、延迟请求立即关闭及 API 失败关闭/toast 已 old/new 验证；其他确认框错误、焦点回收和分享子确认仍待验 |

## 9. 文本文档查看与编辑器

| 状态 | 条目 | 旧版规范与验收点 | 当前 Rust 初检 |
|---|---|---|---|
| `[ ]` | 编辑器入口 | md/markdown/txt/yaml/yml/json/toml/ini/conf/log/csv 等旧版可编辑扩展名，点击文件打开 editor；回收站内容只读。 | old/new 已验证 Markdown 新文档、保存及再次打开、回收站 YAML/Markdown 只读分流和 trash TXT 键盘进入 reader；全部扩展名、网格/列表和回收站完整矩阵仍待验 |
| `[ ]` | 新文档编辑 | 默认文件名、初始内容、editor modal/页面尺寸、关闭、保存、创建失败和成功返回一致。 | old/new 已验证默认名、编辑、保存、重开、尾随空格扩展名错误和错误后保存按钮保留；创建取消/冲突/失败全矩阵仍待收口 |
| `[ ]` | 读取 | `/content`、编码/大文件错误、loading/error、只读提示、滚动和文本保持一致。 | Rust 已有 `/content` caller；old/new 已验证真实 TXT 读取、YAML/Markdown 只读内容和提示，编码/大文件/loading/error/滚动全矩阵仍待验 |
| `[ ]` | 编辑模式 | textarea、编辑/分栏/预览 tabs，Markdown 的 GFM 元素与主动 HTML 清理结果、光标/滚动、预览错误和非 Markdown 隐藏 tabs 一致。reference 使用 `marked` + DOMPurify；Rust 使用 `pulldown-cmark` + `ammonia` 对齐可见结果。 | old/new 已验证编辑/分栏/预览、GFM 标题/列表/任务项/表格/删除线/安全 HTML、主动 HTML 清理、非 Markdown 无 tabs 和错误后仍可保存；光标/滚动、复杂 Markdown 错误仍待验 |
| `[ ]` | 保存 | PUT content、etag/冲突、busy/disabled、成功 toast、列表 metadata、关闭后刷新和失败重试一致。 | old/new 已验证保存按钮、错误后重试入口、成功 toast 必须等待目录刷新、持久化重开；etag 冲突、busy/请求失败和 metadata 全矩阵仍待验 |
| `[ ]` | 未保存关闭 | dirty 检测、关闭/浏览器后退确认、取消返回编辑、确认丢弃、Esc/backdrop 行为一致。 | Rust 已有 dirty、确认对话框和 backdrop/Esc 路径；browser-back、取消后继续编辑和 readonly 矩阵仍待验 |
| `[ ]` | 编辑器视觉 | 标题、文件名、工具栏、图标、按钮文案、编辑区字体/行高、readonly 和错误层级与旧版一致。 | 待恢复 |

## 10. EPUB/TXT 阅读器

| 状态 | 条目 | 旧版规范与验收点 | 当前 Rust 初检 |
|---|---|---|---|
| `[ ]` | TXT 打开 | `/read/{id}`、加载、分页/分栏、返回、书名、实时进度、刷新/深链恢复一致。 | old/new 17项 reader-flow 与真实 TXT 链路已通过；仍需把条目证据拆到各子场景 |
| `[ ]` | EPUB 打开 | manifest/flow/chunk、封面、章节、样式、图片/assets、首屏和错误回退一致。 | old/new 真实 EPUB 1/1、reader-flow 17/17 已通过；损坏/错误回退和逐项截图仍待验 |
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
| `[ ]` | 图片查看 | `/preview`、loading/error、画廊上一张/下一张、缩略图、计数、实际大小/适应窗口、放大缩小、双击、滚轮、拖动边界、stage 点击显隐 chrome、键盘 `←/→/+/-/0/1` 一致。 | old/new media parity 已覆盖 controls、带 `etag` 版本参数的 thumb、从根节点 Tab 进入菜单、thumb/menu、退出、桌面/390/320 宽度；完整键盘/滚轮/边界矩阵仍待验 |
| `[ ]` | 图片触摸 | 双指缩放、拖动、手势取消不误翻页、边界限制、旋转/重排状态保持一致。 | 待验证 |
| `[ ]` | 图片更多菜单 | 下载、移动、复制、信息等 menu 的位置、点击外部/Esc、loading/error 和返回行为一致。 | 待验证 |
| `[ ]` | 音频播放器 | `/audio` 元数据、封面 fallback、章节、上一/下一章、时间跳转、进度、播放/暂停、loading/error/retry、一首/多首行为一致。 | old/new media parity 已覆盖章节标识、controls、桌面/移动宽度；播放状态、错误重试和持久化仍待验 |
| `[ ]` | 音频持久化 | `revaro-audio-volume`、`revaro-audio-muted`、`revaro-audio-position:{id}`，音量滑块、静音、键盘操作和刷新恢复一致。 | 待验证 |
| `[ ]` | 视频播放器 | Range/直接 preview、poster thumbnail、播放/暂停、进度拖动与 seek preview/commit、时间显示、音量/静音、速度、全屏、控制条显隐和自动隐藏一致。 | old/new media parity 已覆盖 poster、controls、速度 Escape、桌面/390/320、touch；Range/全屏/seek commit 仍待验 |
| `[ ]` | 视频字幕 | `/video` metadata、VTT subtitle、选择/关闭字幕、字幕不抖动、加载/解析错误和移动端布局一致。 | 待验证 |
| `[ ]` | 视频持久化 | `revaro-video-volume`、`revaro-video-rate`、`revaro-video-position:{id}`，刷新/重开恢复准确且无错误跳 seek。 | 待验证 |
| `[ ]` | 媒体操作 | 播放器设置/更多中的下载、移动、复制、信息、reanalyze（旧版入口若出现）、关闭/返回和任务刷新一致。 | 待验证 |
| `[ ]` | 不支持/损坏媒体 | unsupported 原文件直接显示旧版错误而不是空白；重试、返回、控制条、错误文案/图标一致。 | 待验证 |

## 12. 下载、Range、预览、分享和归档任务

| 状态 | 条目 | 旧版规范与验收点 | 当前 Rust 初检 |
|---|---|---|---|
| `[P]` | 单文件下载 | `/api/files/{id}/download`、文件名、Content-Disposition、下载菜单/按钮、loading/error 和回收站策略一致。 | old/new 真实 UI 上传后触发下载并核对文件名、Content-Disposition、ETag；`rust-download-parity.spec.ts` 通过 |
| `[P]` | Range 下载/播放 | bytes range、206/416、Content-Range、HEAD/缓存/大文件、音视频 seek 及断点行为与旧版一致。 | old/new 实际请求并核对 open-ended/suffix 206、invalid 416、Content-Range、Content-Length、Go 版 416 正文；HEAD/大文件/媒体 seek 仍是子项待补 |
| `[ ]` | 预览 | `/preview` content type、图片/音频/视频/文本行为、鉴权、缓存、错误、thumbnail fallback 一致。 | caller 分散，待矩阵验证 |
| `[ ]` | 多选 ZIP | 选择多个文件后一次 prepare、进度/任务、一次性 token 下载、CSP `frame` 约束和失败处理一致。 | old/new 已验证列表多选、一次 prepare、ZIP 下载和 frame CSP；任务/进度、一次性 token 重放和失败处理仍待验 |
| `[ ]` | 归档解压 | 支持格式、密码输入任务、冲突/错误、取消、任务中心、完成刷新和安全路径行为一致。 | Rust UI/API caller 缺失或不完整 |
| `[P]` | 分享读取 | 分享状态读取、已存在/不存在、过期/权限、链接显示、复制失败和关闭一致。 | old/new action parity 实际打开 ShareDialog 并读取 inactive/active 状态；共享 test 通过 |
| `[P]` | 分享创建 | 单文件创建链接、复制 URL、成功/失败、按钮 loading/disabled、公开页面行为和文案一致。 | old/new `rust-actions-parity-ui.spec.ts` 实测创建、复制、重生成和公开读取；两版通过 |
| `[P]` | 分享撤销 | 二次确认（如旧版有）、DELETE、成功/失败、状态刷新、旧链接失效一致。 | old/new action parity 实测停止分享确认、DELETE、状态回到创建入口和旧链接 404 |
| `[P]` | 公开分享页 | `GET /s/{token}` 的文件信息、下载/预览、过期/无效 token、响应头和移动端布局一致。 | 旧版实际行为是受安全响应头保护的原始文件流而非 HTML 页面；old/new 实测无 cookie 读取、文本 attachment、Range 206、`no-store`/CSP/robots/referrer headers、短 token 404；移动端无额外页面布局 |

## 13. 全部旧版 API、调用方和 Rust 版调用覆盖

下面按旧版 `internal/server/server.go` 的认证路由登记，逐条追踪“旧版前端调用方 → 当前 Rust handler/API → 当前 Rust UI caller”。“后端已迁移”不等于通过；必须确认当前页面实际触发调用并且用户可完成旧版操作。

| 状态 | 旧版 API | 旧版调用方/用途 | Rust handler/API 与当前 caller 初检 |
|---|---|---|---|
| `[ ]` | `GET /healthz` | 启动/监控 | handler 存在；两实例 200，需纳入部署验收 |
| `[P]` | `GET /readyz` | 就绪检查 | Rust 现已同时 ping SQLite 与本地对象存储；old/new 实例均返回 `{"status":"ready"}`，存储根缺失单测返回 503 `object storage unavailable` |
| `[ ]` | `GET /s/{token}` | 公开分享页 | handler 存在；Rust/浏览器全链路待验 |
| `[ ]` | `POST /api/auth/login` | LoginPage | Rust `login()` 存在；表单/TOTP/错误待验 |
| `[P]` | `POST /api/auth/logout` | 顶栏账户菜单明确退出 | Rust `logout()` 由账户设置的明确退出按钮调用；old/new 登录回跳已验证 |
| `[ ]` | `GET /api/auth/me` | App 启动/刷新 session | Rust `fetch_session()` 存在；401/回跳待验 |
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
| `[ ]` | `GET /api/storage/stats` | 全局/账户/存储信息 | old UI 没有直接调用方；Rust handler 保留，系统状态使用 `/system/status/stream` 的存储数据；响应语义待单独核对 |
| `[ ]` | `GET /api/library` | 书架/图片/视频/音乐/文件分类 | old UI 实际统一调用 `/api/library/all`；Rust route 保留，需确认兼容 API 无额外调用方 |
| `[P]` | `GET /api/library/all` | 分类全量/系列/相册等 | Rust `fetch_library_all()` 由 `FileBrowser`/`LibraryView` 调用；old/new 分类、书架、图库和排序已验证 |
| `[ ]` | `GET /api/library/counts` | 侧栏分类数量 | old `useLibrary` 从 `/api/library/all` 取得 counts；Rust响应字段保留，当前 UI 同样复用 all，独立 endpoint 待核对 |
| `[ ]` | `GET /api/system/status` | 系统状态初次读取 | old UI 没有直接调用方；两版 UI 都使用 SSE 首帧，handler 保留，直读错误/字段待验 |
| `[P]` | `GET /api/system/status/stream` | 系统状态 SSE | Rust `SystemStatus` 直接建立 EventSource；old/new 三卡首帧、入口和关闭已验证 |
| `[P]` | `GET /api/events` | 任务实时 SSE | Rust `TaskController` 直接建立 EventSource；old/new 任务面板刷新/关闭已验证 |
| `[P]` | `GET /api/tasks` | 任务中心初始/刷新 | Rust `fetch_tasks()` 由 `TaskController` 调用；old/new 分组、空态和操作已验证 |
| `[ ]` | `GET /api/tasks/{id}` | 任务详情/归档等待 | old UI 没有直接详情页 caller；Rust route 若保留仅供兼容，归档输入走 `/input`；需单独核对响应语义 |
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
| `[ ]` | `GET /api/files/{id}/audio` | 音频 metadata/章节 | Rust `fetch_audio()` 有 |
| `[ ]` | `GET /api/files/{id}/video` | 视频 metadata/subtitles | Rust `fetch_video()` 有 |
| `[ ]` | `POST /api/files/{id}/media/reanalyze` | 媒体重新分析 | Rust server route 保留；old UI 没有稳定可见入口，需确认媒体更多菜单是否在该 reference commit 出现 |
| `[P]` | `GET /api/files/{id}/video/subtitles/{subtitle}` | 视频字幕文件 | Rust `VideoPlayer` 的 `<track src>` 直接调用；old/new 字幕加载 fixture 已验证 |
| `[P]` | `GET /api/files/{id}/media/progress` | 音视频进度恢复 | Rust `fetch_media_progress()` 由 Audio/VideoPlayer 调用；old/new 存储探针已验证 |
| `[P]` | `PUT /api/files/{id}/media/progress` | 音视频进度保存 | Rust `save_media_progress()` 由 Audio/VideoPlayer 调用；old/new 存储探针已验证 |
| `[P]` | `GET /api/files/{id}/content` | 文本编辑器读取 | Rust `fetch_document()` 由 `DocumentEditor` 流程调用；old/new TXT/Markdown 读取已验证 |
| `[P]` | `PUT /api/files/{id}/content` | 文本编辑器保存/etag | Rust `update_document()` 由 `DocumentEditor` 调用；old/new 保存和 Markdown 重开已验证，冲突子项仍待验 |
| `[ ]` | `GET /api/files/{id}/book` | EPUB/TXT metadata | Rust `fetch_book()` 有 |
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
| `[P]` | `POST /api/files/{id}/extract` | 归档解压 | Rust `extract_archive()` 由 SelectionToolbar 调用；old/new 真实 ZIP 任务已验证 |
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
| 基线与清单 | 1 | `068b9bb` | healthz、old/new 构建和基线记录已完成 | `/tmp/revaro-old-initial.png`、`/tmp/revaro-new-initial.png` | 已建立，仍持续追加证据 |
| 全局导航与 UI | 2–4 | `d18556d`（实现）、`d257696`（E2E）、`ba16ddb`（路由）、`db5b963`（失败导航选择状态）、`9d4ea2b`（根节点 tooltip） | 认证、账户、任务、状态、移动抽屉、分类入口/直达路由、空态、Logo、回收站 footer 和关键入口 old/new 已通过；浏览器后退/弹层 history、失败导航保留旧内容/选择和根节点 tooltip 已追加；全局错误/键盘和完整状态矩阵未完 | `/tmp/revaro-old-global-parity.png`、`/tmp/revaro-new-global-parity.png`、移动端同名截图、导航 trace、`/tmp/revaro-history-*`、`/tmp/revaro-modal-history-*` | 局部 PASS |
| 全局图标与任务中心控件 | 3–4、6、11、15 | `e329690`（`icons.rs` geometry、路径/音频 fallback、任务展开箭头、old/new DOM E2E） | `rust-icon-reference-parity.spec.ts` 双上下文实际比较全局入口、状态卡、菜单、任务操作、路径和移动端图标；媒体/文件项全类型与完整状态矩阵未完 | old/new icon parity trace；old package source 对照记录 | 局部 PASS |
| 目录选择器图标与展开控件 | 8、15 | `d528aed` | `rust-directory-picker-reference-parity.spec.ts` old/new 各 1/1；`rust-actions-parity-ui.spec.ts` old/new 各 9/9 | old/new 实际打开移动目标选择器，比较触发器、面包屑、子目录、深层路径、空目录图标并验证点击目标/Escape 关闭 | 局部 PASS |
| 面包屑 DOM 与移动端布局 | 5、15 | `2e2221d` | `rust-breadcrumb-layout-reference-parity.spec.ts` old/new 各 1/1；`rust-navigation-parity.spec.ts` old/new 各 9/9 | 390×844 深层路径实际比较 direct 子节点、首末 margin、横向位置、点击根和浏览器后退 | 局部 PASS |
| 壳层响应式监听生命周期 | 2、15 | `9dc5204` | `rust-navigation-parity.spec.ts` old/new 各 10/10 | 实际注销卸载认证壳层，拦截 `MediaQueryList` add/remove，确认顶栏/侧栏监听均被释放；完整断线/重连清理仍未完 | 局部 PASS |
| 侧栏展开箭头状态 | 4、15 | `317d1bd` | `rust-icon-reference-parity.spec.ts` old/new 各 1/1 | 实际点击分类和路径树展开控件，比较 SVG transform；分类/路径箭头均与 old 的 90° 旋转一致 | 局部 PASS |
| 通用确认弹窗时序 | 8、15 | `045247f` | `rust-actions-parity-ui.spec.ts` old/new 各 10/10 | 延迟创建请求下实际点击确认，比较弹窗即时关闭和后台结果；重命名保存中语义单独保留 | 局部 PASS |
| 选择工具栏分流与弹层可见性 | 6、8、15 | `f898067` | `rust-selection-toolbar-icon-reference-parity.spec.ts` old/new 各 1/1；`rust-actions-parity-ui.spec.ts` old/new 各 10/10 | 实际选择 `.txt` 对照阅读图标几何；实际打开移动弹层对照选择工具栏立即隐藏；完整文件类型/disabled 矩阵未完 | 局部 PASS |
| 任务中心平均进度与空态 | 3、15 | `5c69720`、`2d9f584` | `rust-task-center-parity.spec.ts` old/new 各 5/5 | 两个活动任务原始进度实际比较 header 汇总，恢复旧版先平均再四舍五入；延迟初始请求仍显示 reference 空任务文案，其它任务状态矩阵已有覆盖 | 局部 PASS |
| 顶栏/状态 badge 与命中率 | 3、4、15 | `8f5356f`、`1108947` | `rust-global-ui-reference-parity.spec.ts` old/new 各 1/1；聚合导航/任务/图标集合 old/new 各 15/15 | 同一 mock 数据逐项比较任务 header、服务卡 badge 的 class/尺寸/padding/文字，并用 2/3 fixture 验证 67% 四舍五入；旧版状态异常矩阵仍未完 | 局部 PASS |
| 媒体库快照与 force refresh | 4、5、15 | `d068eb8`、`bb6edba` | `rust-library-ui.spec.ts` old/new 各 8/8 | 实际切换分类只请求一次 `/api/library/all`，显式 Refresh 才重新读取；refresh 失败后继续切换仍复用旧快照；完整分类错误/数量矩阵未完 | 局部 PASS |
| 就绪探针 | 1、13 | `938a60a` | Rust router 单测：DB 正常、对象存储失败；old/new 实例实际响应一致 | `/readyz` old/new 200 对照 | PASS |
| 文件浏览与选择 | 5–6 | `d18556d`（实现）、`d257696`（E2E）、`1937d06`、`83ec6c0`、`8e59b85`、`4ba891f`（逐项 parity） | 面包屑/历史、列表选择、文件图标、打开分流和操作菜单已有 old/new 用例；方块卡与媒体库卡 Space、EPUB 书籍图标几何、EPUB fallback class、视频 preview class、媒体库刷新图标/失败重试和多级分类路径已追加验证；hover/长按/全部类型未完 | `/tmp/revaro-old-global-parity.png`、`/tmp/revaro-new-global-parity.png`、file-card/library parity trace | 局部 PASS |
| 失败导航状态保留 | 5、6、15 | `db5b963` | `rust-navigation-parity.spec.ts` old/new 定向用例各 1/1 | 列表已有选择时发起延迟 500 导航并返回 500，实际比较 loading/失败后的旧列表、选择工具栏和错误 toast；成功导航清空选择，完整 stale request 矩阵仍未完 | old/new navigation parity trace | 局部 PASS |
| 上传与任务 | 7、3 | `3beac64`（server）、`d18556d`（web）、`d257696`（E2E） | 上传入口、目录上传、任务中心分组/取消/重试/归档输入和完成刷新已有 old/new 用例；断点续传完整 UI 未完 | parity Playwright trace 与任务/上传测试结果 | 局部 PASS |
| CRUD 与回收站 | 8 | `d18556d`（实现）、`d257696`（E2E）、待提交 dialog failure parity | 新建、重命名、移动、复制、删除、恢复、永久删除主链路已 old/new 实测；新建 API 失败时弹窗关闭/toast 已追加；冲突/失败/清空矩阵未完 | parity Playwright trace、`/tmp/revaro-dialog-error-*` | 局部 PASS |
| 文档编辑器 | 9 | `d18556d`（实现）、`d257696`（E2E）、`2f9eb7b`（editor reverse parity） | TXT/Markdown 新建、读取、GFM 预览/HTML 清理、保存、dirty discard、尾随空格校验、错误保留保存、回收站 YAML/Markdown 只读分流和刷新反馈时序已 old/new 实测；etag 冲突/全部扩展名/完整 loading 与视觉矩阵未完 | `rust-editor-reference-parity.spec.ts` old/new 各 3/3、reader/editor parity trace | 局部 PASS |
| 阅读器 | 10 | `14084bf`（core）、`d18556d`（web）、`ed13571`（全局 block）、`a47dc50`（定位/进度/缓存/导航 E2E） | old/new reference reader-flow 各 17/17；真实上传 EPUB 各 1/1；全局 block 0…37、14/14.0% 进度文案、TOC Escape 焦点、L2 同版本零请求/版本变化重取已实测；触摸/错误/偏好和完整 UI 状态矩阵仍未完 | reader-flow trace、real EPUB trace、`rust-reader-ui.spec.ts` | 局部 PASS |
| 媒体 | 11 | `d18556d`（实现）、`d257696`（E2E）、`b84ce18`（thumb/focus parity） | 图片/音频/视频桌面/窄屏/触摸、字幕、存储、全屏主链路、缩略图版本参数和预览 Tab 首焦点已有 old/new 实测；损坏/seek 边界仍未完 | media parity trace | 局部 PASS |
| 下载/分享/归档 | 12 | `ff43716`（Range）、`d18556d`（UI）、`d257696`（E2E）、`3967289`（公开分享 transport E2E） | 单文件、ZIP、分享生命周期、公开分享安全 headers/Range/无效 token、归档任务、preview/206/416 已 old/new 实测；HEAD/大文件/媒体 seek 与视觉状态仍未完 | download/share/action parity trace；公开分享 old/new 追加断言 | 局部 PASS |
| 全量 API caller 与最终视觉回归 | 13–16 | 待提交 | API matrix 已反向登记并修正 caller 记录；全量状态、无障碍、响应式、CSP/监听器审计未完 | 待补齐 | 未完成 |

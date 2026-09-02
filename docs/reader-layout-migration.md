# 阅读器迁移方案：服务端固定分页 + 客户端三页窗口

目标：把 Revaro EPUB 阅读器从「客户端实时跨 spine 分栏分页」迁移为「服务端按
设备阅读配置生成固定 Page HTML/DOM fragment + 客户端三页窗口纯 transform 动画」。
本文先梳理现有架构，再给出最小风险的分阶段方案。

**迁移策略（已调整）**：新阅读器完成并验证后**直接替换**旧阅读器，不保留永久
fallback，也不长期维护双实现。开发阶段旧 reader 暂时保留用于对照并保证现有
功能可用；服务端分页、新三页窗口、阅读进度、字号/旋转重排等核心功能全部验证
通过后，切换新 reader 为唯一实现，并删除旧 reader 代码、旧分页逻辑、旧 API
与无用兼容分支。

**术语约定**：内部位置统一称为 **locator / readingAnchor**，不再称为 CFI。

## 1. 现有架构梳理

### 1.1 服务端（Go）

- 解析管线 `internal/reader/`：
  - EPUB：zip 解包 → OPF/spine → 每章白名单清洗为 `Chapter.HTML`（剥离
    `script/style/iframe/事件属性/内联 style`），注入 `data-source-path`、
    `data-frag-ids`，内嵌图片改写为 `/api/files/{id}/book/assets/{n}?v={ETag}`
    （不可变、长缓存）；目录（nav + NCX 回退）、封面、图片资产表一并产出。
  - TXT：编码探测（UTF-8/GBK）＋“第…章”标题识别（UTF-16 offset 目录）。
  - 解析结果按 object key（内容哈希）LRU 缓存（3 本 / 96 MiB），天然不可变。
- HTTP `internal/server/book.go`：
  - `GET /api/files/{id}/book`（元数据）、`/book/content`（整本 JSON：chapters+toc
    或 text+toc）、`/book/assets/{n}`、`/book/cover`；
  - `GET/PUT /book/progress`：`{page, total_pages}`，存 SQLite `settings` 表。
- 不含任何服务端排版：**没有** headless Chromium，Docker 镜像（bookworm-slim、
  read_only rootfs、cap_drop ALL、非 root）也没装；**没有**字体抽取（只抽图片）。
- 可复用的基建：DB 任务系统（`tasks.go`）、作业事件扇出（`jobs.go`）、对象存储
  前缀 + GC（`object_manager.go`/`cache_manager.go`/`cleanup_manager.go`）、媒体缓存。

### 1.2 客户端（web/src/reader/ + reader-app.js）

- `Reader.vue` 静态骨架 + `reader-app.js` 命令式 DOM 接管（VesperGlow/reader 移植版）。
- 布局：整本书进 DOM，每章一个 `.book-content` 分栏段（`column-width` +
  `column-gap` + `column-fill:auto`），JS 量 `scrollWidth` 得出每段栏数；
  page = (section, column)。
- 翻页：段内 `translateX(-col*width)`；跨段用 WAAPI 双层 260ms 动画；触摸跟手、
  热区、键盘、进度条。
- 进度：page/totalPages 百分比；打开时按比例恢复；防抖保存 + 15s 心跳 + 失焦 flush。
- 偏好：字号（CSS 变量 `--reader-font-size`，14–32px）、明暗主题，localStorage；
  改字号/旋转 → 客户端即时重排，按比例回位。
- 打开时先把全书图片 load 完再首次分页（保证图片尺寸稳定）。

### 1.3 对迁移有利的既有事实

1. 清洗后的章节 HTML **已经是服务端分页器的理想输入**：解析器完全不用动。
2. 现有 CSS 语义（多栏、图片高度上限、`break-inside`）可以原样搬进 Chromium，
   服务端分页结果与现版视觉一致，老阅读器作为 fallback 不会“两套观感”。
3. 图片已是共享不可变 URL；主题用 CSS 变量，**主题不应进入 layout profile**，
   明暗切换永不触发重排。
4. 后台任务、对象存储前缀 GC、进度接口的兼容扩展点都已存在。

## 2. 目标架构（要点）

- **Layout profile**：`sha256(viewportW, viewportH, fontSize, fontFamily,
  lineHeight, margins, bookContentHash)` → profileID。主题/亮度不参与。
- **服务端分页**：headless Chromium 把整本清洗后的 spine 装进一个文档，用与
  客户端相同的多栏 CSS 排版；用 `caretRangeFromPoint` 在每道栏边界二分定位文本
  位置，再用 DOM Range（`cloneContents`）把内容切成**合法、闭合的 HTML
  fragment**，一页一个对象；manifest 记录每页 start/end readingAnchor。
- **固定 Page HTML**：每页 fragment 的容器用内联样式锁定 viewport 尺寸、字体、
  字号、行距、边距等全部排版参数，页面显示不依赖任何外部 CSS 状态；
  `@font-face` 与共享样式表按 profile 共享；服务端分页与客户端渲染使用
  **同一份 WebFont**（同一字体文件、同一 URL），保证分页结果逐像素一致。
- **产物**：`layouts/{contentHash}/{profileID}/manifest.json` +
  `pages/{n}.html`，存对象存储，不可变、可 GC；共享样式表一个 profile 一份；
  图片/字体走既有不可变资产 URL。
- **客户端**：只保留 上一页/当前页/下一页 三个页面节点，邻页预取，transform
  动画复用现版 WAAPI 思路；进度 = 当前页 start readingAnchor，页码只是 UI 显示值。
- **布局切换**：改字号/旋转 → 旧 profile 继续可读；后台生成新 profile；
  manifest 就绪后按当前 readingAnchor 映射到新页码，无缝换页。
- **收尾**：核心功能验证通过后，新 reader 成为唯一实现，旧 reader 与旧分页
  逻辑、旧 API、兼容分支全部删除。

## 3. 需要拍板的设计决策

| 决策点 | 建议 | 理由 / 代价 |
|---|---|---|
| 位置标识 | **locator / readingAnchor**：`{spineIndex, nodePath, textOffset}`（对清洗后 DOM 计算），全文统一术语，不再称 CFI | 清洗器已重写 DOM，对原始 spine 计算规范 EPUBCFI 无意义；readingAnchor 跨 profile 稳定即可，实现成本低一个量级 |
| 分页算法 | 沿用 CSS 多栏 + Range 切片，不做「图片页」 | 与现版视觉一致，规避打印分页（`@page`）的生态差异；图超高的场景按现版规则降级 |
| Chromium 形态 | chromedp 内嵌进程（单一实例、串行任务队列、超时/内存上限、`--no-sandbox --headless=new`） | 单用户自托管场景最简单；需改 Dockerfile（+300~400MB）；不新增 sidecar 服务 |
| 生成粒度 | 默认**整本一次 pass**；「当前 readingAnchor 周边窗口先就绪」作为 Phase 4 优化 | 一次 pass 只开一个页面、测完所有 section；千页书约数秒到十几秒，期间旧 layout 始终可读 |
| 主题 | 永不进 profile，共享样式表用 CSS 变量 | 明暗切换零重排 |
| 字体 | **服务端与客户端共用同一 WebFont**（应用内自托管同一字体文件/URL）；Phase 1 起随应用打包 | 字体不一致会导致两套分页结果不一致、切换时文字跳动；CJK 字体体积大，见风险表 |
| 进度 API | 扩展 `bookProgress` 增加可选 `anchor`/`profile` 字段，保留 `page/total_pages` 至旧实现删除 | 过渡期新旧并行互不破坏；旧实现删除时一并删掉 `page/total_pages` |

**明确不做**：不生成页面图片（要求即 HTML fragment）；不为所有设备预先生成全部
profile（按需生成）；不把整本书再塞进客户端 DOM；不长期维护双实现（旧 reader
仅开发期对照，验证后删除）。

## 4. 分阶段迁移（每阶段独立可回滚、无用户可见回归）

### Phase 0 — 契约与开关（无行为变化）
- 进度负载增加可选 `anchor`、`profile` 字段（向后兼容，旧字段照存照读，直至
  旧实现删除）。
- 新增 `GET /api/files/{id}/book/layouts/capabilities`：报告布局引擎可用性
  （镜像是否带 Chromium）。
- 新增 `internal/reader/layout` 包骨架：profile 定义与哈希、manifest 结构、
  readingAnchor 结构。
- 风险：无。产出：契约先行，后面各阶段并行开发不被接口返工。

### Phase 1 — 服务端分页器（隐藏能力，只验证不启用）
- chromedp pager：装整本 spine → 同一份 CSS 多栏排版 → 栏边界
  caretRangeFromPoint 二分 → Range 切片出每页 fragment → 写 manifest + pages
  到对象存储；串行队列复用 `tasks.go`/`jobs.go` 模式；超时、重试、单页大小上限。
- 校验：对样例书生成 layout，重点验证 `Range.cloneContents()` 对 ruby、复杂
  inline、图片、列表、表格等内容的分页一致性（回渲染断言：文本恰好一次不重
  不漏、每页不溢出、HTML 闭合），并与现版客户端分页边界做对照记录白名单差异。
- 提供调试入口（隐藏 CLI/端点）手动生成与检查产物。
- 风险：内存（Chromium 单实例 + 页级对象流式写出）；时间（大书生成慢，但不
  影响任何线上路径）。缓解：上限、超时、失败即不暴露。

### Phase 2 — 客户端 v2 阅读器（opt-in，平行模块）
- 新增 `reader-app-v2.js`（或 Vue 组件）：三页窗口（prev/current/next）、
  manifest 驱动、邻页预取、transform 动画、复用现有热区/滑动/键盘/目录交互。
- 开关：localStorage opt-in；开发期任何失败/超时**自动回落旧阅读器**（仅过渡
  手段，不作为长期维护的兼容分支）。
- 此阶段 v2 仍可用 page 进度（manifest 页码 ↔ 现有进度字段），先打通渲染。
- 风险：与老阅读器共享骨架 CSS，注意样式隔离（v2 页面容器用自己的类前缀）。

### Phase 3 — readingAnchor 进度 + 无缝布局切换
- 翻页/停止时写 `{anchor, profile}`；恢复时 anchor → 页码（manifest 内二分）。
- 字号/旋转变更：保留旧 profile 继续读；防抖合并连续的调整请求（滑动字号只
  生成最终值）；后台生成新 profile，就绪后 anchor 映射换页并动画过渡；在途生成
  可取消、以最新 profile 为准。
- 进度条页码 = manifest 派生的显示值；服务端 `page` 字段仅作过渡期数据。
- 风险：anchor 跨 profile 漂移（清洗后 DOM 不变 + 同一套文本索引，理论稳定；
  用样例书做回归断言）；生成完成前用户已翻多页（切换时按**最新** anchor 映射，
  不是发起时的）。

### Phase 4 — 加固与成本控制
- 增量/渐进生成：先切当前 CFI 周边窗口页面并提前发布 manifest 子集，其余后台
  补齐（可选优化，不做也不阻塞）。
- 预取策略、页面缓存头、profile 老化回收（挂到既有 GC）、Chromium 崩溃自愈。
- Docker：默认镜像带 Chromium；提供 `-lite` 变体（无 Chromium，仅旧阅读器，
  供资源受限部署过渡使用）。
- 状态面：`system/status` 增加 layout 引擎状态；e2e 覆盖 v2 与回落路径。

### Phase 5 — 唯一实现切换与旧代码删除
- 服务端分页、三页窗口、readingAnchor 进度、字号/旋转重排全部验证通过后：
  - 新 reader 成为**唯一**实现，移除 opt-in 开关；
  - 删除旧 reader 代码（`reader-app.js`、`reader/` 旧模块、旧骨架 CSS）、旧
    客户端分页逻辑（分栏测量/translateX 跨段翻页）；
  - 删除旧 API 与兼容分支：`bookProgress` 的 `page/total_pages` 字段与相关
    校验、按比例恢复逻辑；
  - 更新 README/文档与 e2e 测试。

## 5. 主要风险与缓解

| 风险 | 缓解 |
|---|---|
| Chromium 内存/耗时失控 | 单实例串行队列、单 pass 超时、流式写页对象、失败回退旧阅读器 |
| 服务端与 fallback 客户端分页不一致 | 同一份 CSS 与同一套字体；Phase 1 快照对比建立差异白名单 |
| CJK 断行/字体缺字导致跨设备不一致 | 应用内自托管同一 WebFont（Phase 1 起打包），服务端分页与客户端渲染用同一字体文件；字体缺失时 profile 显式记录并降级 |
| 大书/复杂 EPUB（超宽表格、巨图、RTL）切片异常 | 单页降级策略；开发期按书回落旧阅读器（仅过渡） |
| 存储膨胀（每页 30~60KB HTML，千页书每 profile 数十 MB） | 对象存储 + GC 老化回收，单用户规模可接受；页面只存 fragment，样式/字体共享 |
| 切换瞬间进度错位 | 切换一律按切换时的最新 anchor 重映射；写进度带 profile 版本 |
| 安全面扩大（服务端跑渲染引擎） | 只渲染白名单清洗 HTML；禁外网、只放行本服务资产 URL；非 root + no-sandbox 容器 |

## 6. 阶段化保证

- 解析管线、资产分发、目录接口在迁移全程不动；
- Phase 0–4 期间旧 reader 保持可用用于对照与兜底（仅过渡）；Phase 5 验证通过
  后一次性切换并删除旧实现，不留双实现维护负担；
- 每个 Phase 都有独立开关与回滚点，任何一阶段中止，系统状态 = 该阶段起点。

## 7. 实施状态（Phase 0/1 已完成）

### Phase 0 — 契约与开关 ✅
- `internal/reader/layout/` 包骨架：`Profile`（规范化 + 校验 + 内容哈希派生
  `profileID`）、`Anchor`（readingAnchor，全书全序 `Compare`）、`Manifest`
  （`PageForAnchor` 二分查页、`AnchorForPage`）。
- 进度 API 扩展：`bookProgress` 增加可选 `anchor`/`profile`，旧 `page/total_pages`
  照常工作；非法锚点/非法 profileID 拒绝写入，读取时丢弃损坏锚点。
- `GET /api/files/{id}/book/layouts/capabilities`：Chromium 探测（
  `REVARO_CHROME_BIN` → PATH → Playwright/Puppeteer 缓存），结果缓存。
- 术语：代码与文档统一 locator / readingAnchor，无 CFI 字样。

### Phase 1 — 服务端分页器 ✅（隐藏能力）
- `Pager`（chromedp，单会话）：整本 spine 装入同一文档，与旧客户端同源的多栏
  CSS（共享样式表 `reader.css`，分页与页面展示共用）；步进索引（文本 caret +
  元素边界 caret）+ 栏边界二分（取「最后一个 fits」的 caret，保护
  break-inside:avoid 元素不被撕开）+ `Range.cloneContents()` 切片。
- 固定 Page HTML：每页 `.revaro-page` 内联锁定 viewport/字号/行距/边距/字体，
  与共享样式表 + `@font-face` 组合后不依赖任何外部 CSS 状态。
- 共享 WebFont：`Noto Serif SC`（OFL）子集化 woff2（CJK URO 全覆盖 + 标点 +
  ASCII，6.16MB）内嵌二进制；服务端分页经 Fetch 拦截注入同一文件，客户端走
  `/api/reader/fonts/revaro-serif.woff2`（immutable 缓存）——两端同字体。
- Fetch 拦截：wrapper 文档、字体、内嵌图片全部内存满足，Chromium 零真实网络；
  非 root + `--no-sandbox`，独立临时 profile 目录。
- 产物：`layouts/{bookObjectKey}/{profileID}/manifest.json` + `pages/{n}.html`
  对象存储（不可变、不被现有 GC 触碰）；`profileID` 可由客户端确定性计算。
- 隐藏调试端点：`POST /api/files/{id}/book/layouts`（幂等）、`GET .../layouts`
  （列表）、`.../layouts/{profile}`（状态）、`.../manifest`、`.../pages/{n}`；
  `GET /api/reader.css`。

### Phase 1 验证结果 ✅（真实 Chromium 140 集成测试）
- **切片一致性**（`TestPaginateSlicesContentExactly`）：ruby、复杂 inline
  （嵌套 b/i/span）、图片（小图 + 带题注大图）、嵌套列表、表格、pre、
  blockquote、跨章边界全量覆盖：
  - 逐章拼接页文本与原文**逐字符相等**（不重不漏）；
  - `<img>/<table>/<ruby>/<li>` 计数与来源一致，不可断元素不跨页撕裂；
  - 每页回渲染 `scroll <= client`（无溢出；修复了 margin 穿透锁定容器的问题，
    见 `reader.css` 的 `display: flow-root`）；
  - 回渲染文本与页 HTML 文本一致。
- **锚点稳定性**（`TestPaginateAnchorStableAcrossProfiles`）：同 profile 两次
  分页页锚点完全一致（确定性）；文本位置锚点在 3 组不同 profile（viewport/
  字号/行距各异）中都落在包含该文本的页区间内（阅读位置不漂移）——旧
  layout → 新 layout 切换的映射基础。
- **TXT 通道**：UTF-16 语义分段（与旧客户端一致）+ 文本连续性。
- **端到端**（`TestReaderLayoutGenerationEndToEnd`）：POST 提交 → 轮询 →
  manifest → 页面对象 → 列表 → 幂等重提交 → 非法输入拒绝，Chromium 冷启动
  约 0.6s。

### 已知限制（Phase 1 范围，后续阶段处理）
- 产物对象暂不老化回收（GC 不扫 `layouts/` 前缀，安全但会累积，Phase 4 挂
  book 删除钩子）。
- 单本整次生成（一次 pass 全量写页）；渐进生成与页预取策略在 Phase 4。
- Docker 镜像尚未装 Chromium（`-lite`/默认镜像拆分与 `REVARO_CHROME_BIN`
  编排在 Phase 4）；开发/CI 用 Playwright 缓存的 chrome-headless-shell。
- 未与旧客户端做逐页边界快照对照：以更强的「回渲染自洽」验证替代，后续
  Phase 2 客户端落地时可补对照。

### Phase 2/3 完成 ✅ — 客户端三页窗口 + readingAnchor 进度 + 无缝切换
- `web/src/Reader.vue`（新实现，替换旧组件）：**严格三页窗口**（prev/current/
  next 三个槽位），零客户端分页；前后页预取进 `PageCache`（容量 12，LRU），
  翻页热路径只做 track 的合成层 transform（`animateTrack`，260ms）；动画中
  连点累加排队，滑动拖拽跟随同一 transform。
- 交互全保留：热区/键盘/滑动/目录（manifest.TOC 直接跳页）/进度条/字号
  （14–32）/行距（1.4/1.7/2.0）/明暗主题（CSS 变量，永不重排）。
- 进度 = readingAnchor + profile（`bookProgress` 的 page/total_pages 字段已
  删除），防抖保存 + 失焦/隐藏落盘。
- **无缝重排**：字号/行距/旋转 → 新 profile 后台生成（600ms 防抖合并连续
  调整），期间旧 layout 照常翻页；manifest 就绪后当前 anchor → 新页码映射，
  清空页缓存并强制重取后切换（页缓存按 profile 隔离）。
- 页缓存/预取/锚点查页等纯逻辑有 vitest 单元测试；
  `web/e2e/reader-v2.spec.ts`（`playwright.v2.config.ts`：vite dev + 路由
  mock）做浏览器级验证：三页窗口不膨胀、每次翻页只新增 1 个预取请求且已
  渲染页零重复请求、目录/进度条跳页、字号变更期间旧 layout 可读且切换后
  锚点位置不漂移、按 anchor 恢复进度——5 个用例全部通过。

### Phase 5 完成 ✅ — 唯一实现切换与旧代码删除
- `Reader.vue` 已替换为新实现，App 门控（`revaro-reader-v2` 开关）移除；
- 删除旧 reader 代码：`reader-app.js`、`reader-app.d.ts`、`reader/`（旧
  segments/navigation/progress/preferences/api 模块）、旧 `Reader.vue`；
- 删除旧客户端分页逻辑与样式（`.book-content`、`.reader-page`、
  `.reader-zones`、`.book-segments` 等规则）；
- 删除旧 API：`GET /api/files/{id}/book/content`；`bookProgress` 移除
  `page/total_pages` 字段与按比例恢复逻辑。
- 开发期旧 reader 保留的过渡开关与 mock 均已清理。

### 剩余事项（Phase 4 加固，不影响已交付功能）
- ~~Docker 镜像装 Chromium~~：发布镜像已内置 Chromium（`REVARO_CHROME_BIN`
  预设 `/usr/bin/chromium`，podman 构建验证可执行、`--version` 正常、非 root
  下 headless 可启动）；
- `layouts/` 产物随书删除钩子（目前依赖 GC 的孤儿清理，间隔 1h）；
- 可选 `-lite` 无 Chromium 变体（资源受限部署）。

### Phase 4 完成 ✅ — 渐进式分页、实例复用、缓存 GC、方向预取与统计
- **渐进式分页（不整本同步生成）**：`POST /layouts` 带 `start_anchor` 时按
  螺旋顺序生成（`spineOrder`：目标章 → ±1 → ±2…）；每完成一章立即写页面
  对象（`(spine, col)` 寻址，一经写入永久稳定）并发布 manifest **快照**
  （`complete=false`，页数组按 spine 顺序用前缀和装配全局页码）。客户端
  `waitForReadable` 在「快照含当前位置」即开始渲染，不等全书完成；后台
  800ms 轮询新快照，页码漂移按当前 anchor 重映射（页缓存以 URL 为键，
  跨快照命中，不重新下载、阅读不漂移）。
- **Chromium 实例复用/受控队列**（`internal/reader/layout/scheduler.go`）：
  进程内单浏览器（独立 tab 串行执行，任务取消只关 tab）、单槽位串行队列、
  `context.AfterFunc` 联动取消；Fetch 拦截挂 tab 级 + 原子 resolver 指针；
  任务间确定性经 `TestSchedulerReusesBrowser` 验证（同 PID、逐字节一致）。
  新提交让旧任务在下一个章边界中止（`layoutGen` 最新优先）。
- **Layout 缓存 GC**：`CollectGarbage` 新增 `layouts/` 回收——已删除书的
  产物（blob 不在引用集合）立即清理；其余按 `LAYOUT_CACHE_TTL`（默认
  720h）与 `LAYOUT_CACHE_CAPACITY`（默认 1 GiB）淘汰最旧对象；渐进快照
  manifest 用 `no-cache`，完成态 manifest 才 immutable。
- **按阅读方向智能预取**：前行 `[p+1,p+2,p+3,p-1]`、后退镜像（向后看 3 页
  另一侧最后），只碰当前快照内已生成的页；e2e 断言后退时请求顺序恰为
  [7,6,5] 且不取对侧页。
- **统计**：`system/status` 新增 `reader` 段（引擎版本、队列长度、运行中
  作业、累计页数/字节、Chromium 进程 RSS、最近任务耗时）；调度器
  `SchedulerStats` 与作业 phase/progress（`spines_done/spines_total/pages/
  complete`）供 UI 展示。
- 验证：Go 全量测试（含 `TestReaderLayoutProgressiveOrder`、
  `TestLayoutCacheGC`、`TestSpineOrderSpiral`、调度器复用）与浏览器级
  e2e 7 用例全绿；新增用例覆盖「渐进式首开目标章先可读、快照增长后滑块
  上限增长且阅读位置不漂移」。

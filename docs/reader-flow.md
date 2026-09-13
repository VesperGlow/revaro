# 阅读器架构：轻量混合（服务端 reading flow + 客户端 CSS columns 分页）

本文记录 Revaro 阅读器当前架构（取代旧的“服务端 Chromium 固定分页”方案，
相关迁移过程见 git 历史）。

## 1. 设计目标与取舍

旧架构让服务端用 headless Chromium 按每个设备阅读配置（viewport/字号/行距）
把书切成一页页固定 HTML，客户端只做三页窗口变换。它把“分页”这种本就依赖
浏览器排版引擎的工作搬去服务端，代价是：

- 镜像 +300~400 MB（Chromium），还要维护单实例调度、Fetch 拦截、profile 缓存；
- 任何字号/横竖屏变化都要服务端重新生成（渐进式生成的复杂度很高）；
- 生成产物按 (profile × 内容) 双重膨胀，缓存与 GC 复杂。

新架构回到轻量混合模型：

| 层 | 职责 |
|---|---|
| **服务端（Rust）** | EPUB 解包、spine/TOC 解析、XHTML 白名单清洗、图片固有尺寸/宽高比提取、资产分发、locator（readingAnchor）；把整本书拼成一条**连续、标准化、可缓存的 reading flow**，再按**自然 DOM 边界**切成适量 chunk（chunk 不是页面，不与 viewport/字号绑定） |
| **客户端（浏览器）** | 只加载当前位置附近少量 chunk，在统一 DOM/样式环境下用**浏览器原生 CSS columns** 做最终分页；翻页热路径只做合成层 transform；字号/行距/旋转变化**只在客户端重新分页**，服务端零参与 |

**明确不做**：服务端排版（Chromium/chromedp、layout profile、固定 Page
HTML、渐进式分页任务）已全部删除；服务端不再知道任何设备/字号参数；不为
“两端一致”自托管一份字体（客户端用自己的字体栈排版，不存在两端分页一致
性问题）。

## 2. reading flow 与 chunk

- **flow**：全书内容排成一个连续排版流。EPUB 的每个 spine 清洗结果按序拼入；
  TXT 每个“第…章”分段按序拼入。图片与文字始终处于同一排版上下文——跨 spine
  内容无缝接排，不再以 spine 为分页/翻页边界。
- **内容块（block）**：每个 spine 的清洗 DOM 顶层元素是一个 block（TXT 为
  行对齐的文本块，保留 pre-wrap 连续排版观感）。block 在全书统一编号
  （`data-block="N"`），chunk 边界只允许出现在 block 之间。
- **chunk**：按文本量（约 7k UTF-16 码元）与 HTML 体积把连续 block 分组，
  一个 chunk 不跨 block。客户端加载当前 spine 的稳定排版前缀到当前位置，
  并向阅读方向额外预取 3 个 chunk；跨 spine 后才释放旧前缀。
- **manifest**：flow 的产物清单：`version`（flow 格式版本）、`format`、
  `total_chars`、`spines`（每章块区间）、`chunks`（索引/块区间/文本量）、
  `toc`（每条目录 → 目标 block）。纯函数、确定性（同一本书永远生成相同产物）。
- **缓存**：产物写对象存储 `flows/{blobKey}/f{version}/`（manifest 最后写，
  保证一致性）；manifest 响应 no-cache（随二进制版本走），chunk 内容不可变、
  长缓存。GC 按孤儿/TTL/容量回收（`FLOW_CACHE_TTL` / `FLOW_CACHE_CAPACITY`）。

HTTP 面：

- `GET /api/files/{id}/book/flow` → manifest
- `GET /api/files/{id}/book/flow/chunks/{n}` → chunk HTML 片段
- `GET/PUT /api/files/{id}/book/progress` → `{anchor}`（readingAnchor）

旧端点（layouts/capabilities/manifest/pages、`/api/reader.css`、共享字体）已
删除；`bookProgress` 不再持久化 profile。

## 3. readingAnchor（locator）

`Anchor{spine, block, path, offset}`：

- `block`：全书统一编号（即 `data-block`）；
- `path`：从 block 元素出发的 childNodes 下标链（到承载 offset 的文本节点）；
- `offset`：文本节点内 UTF-16 偏移；`-1` 表示元素边界位置（块起点用空 path +
  `-1`）。

锚点只依赖清洗后内容本身（与客户端怎么排版无关），因此跨字号、横竖屏、跨
客户端分页稳定。旧格式（`{spine, path, offset}`，path[0] 为章内块号）在读取
时自动迁移（无 `block` 字段即旧格式）。

客户端在每次翻页后读取当前屏顶的文本位置（`caretRangeFromPoint`），把顶行
字符记成 readingAnchor 并防抖保存；恢复/目录跳转按 anchor → 块 → 所在栏
定位。字号/旋转等变化在客户端重排后按同一 anchor 回到新布局中对应栏。

## 4. 目录导航（Stable NavAnchor，v4）

TOC 每条目录在服务端构建期解析为导航 locator，两套语义分离：

- **文本目标**（绝大多数条目）：manifest 保存实际文本节点的稳定 DOM path
  （`text_path`，相对块元素的 childNodes 下标链）+ 首个可见字符的 UTF-16
  偏移（`text_offset`）。客户端加载目标 chunk 后解析真实 Text node，用
  collapsed caret Range 的 rect 计算栏。不再向 DOM 注入空 inline 标记——
  空 inline 在 column/page break 处可能停留在上一栏，而真实文本已进入下一
  栏，会导致目录稳定跳到目标前一页。
- **媒体目标**（img/svg/video）：清洗期把 `data-rv-anchor="<id>"` 绑到媒体
  元素上，客户端按真实元素 rect 定栏。
- **回退链**：文本 locator 解析失败（节点缺失/偏移越界/无布局盒）→ 媒体
  NavAnchor → `source_fragment` 块内精确定位（visualStart）→ 块起点。

## 5. 稳定分页边界（窗口虚拟化的前提）

chunk 按体量切分、不是 page boundary。如果 CSS columns 以「任意已加载
chunk」为排版原点，窗口增删会改变后续所有 page break（相位漂移）：目录落
点偏移、继续阅读时排版跳动。因此：

- **服务端**在每个 spine 起始块（全书首块除外）注入 `data-spine-start`，
  客户端 CSS 对其施加 `break-before: column`——spine 起点强制成为
  column/page boundary（TXT 章节等价处理）。
- **客户端**窗口恒为 `[当前 spine 起始块所在 chunk, 当前 chunk + AHEAD]`：
  spine 内阅读时排版前缀（spine 起始 chunk → 当前 chunk）永远保留，只有
  进入下一 spine 后才释放上一 spine 的 chunk；向后翻越 spine 边界时允许
  原始扩张一个 chunk（强制分栏保证当前 spine 的 page boundary 不变，绝对
  栏号由 topAnchor 重对齐吸收）。
- 窗口增删后当前 readingAnchor 的视觉页与后续 page boundary 不变；随机
  TOC seek、连续翻页、窗口滑动、横竖屏、字号变化均有回归测试
  （早期 Vue 工程中的 route-mock 用例已随旧工程删除；当前真实行为测试见下文）。
- 后续如需更激进的虚拟化（不保留整个 spine 前缀），必须基于已测量并缓存
  的真实 page boundary，而不是字符 chunk 边界。

## 6. 客户端分页与窗口

- 容器：`.rf-flow.revaro-content` 是一棵连续多栏容器（绝对定位、宽=屏宽、
  高=阅读区高、`column-width`/`column-gap`/`column-fill:auto`、字号/行距/
  图片上限等以 CSS 变量注入）。栏距恒等于 2×侧边距 → 栏宽 + 栏距 = 屏宽，
  因此每“页”恰好平移一屏宽（纯 transform）。
- 窗口：已加载 chunk 是 `[c0, c1]` 的连续区间；`measureCols()` 用
  `scrollWidth / 屏宽` 得到栏数。窗口滑动只增量 append/remove chunk；
  `windowSync` 与翻页互斥（`syncing`/`turnBusy` 排队重放），重起源期间
  不得按陈旧 `cols`/`currentCol` 计算落点。
- 交互：左右热区、方向键/PageUp/PageDown/空格、移动端跟手滑动（快速轻扫或
  拖过 1/4 屏判定）；工具栏、目录抽屉、进度条（按全文 UTF-16 比例的 0–1000
  滑块）、字号 14–32 / 行距 / 明暗主题沿用旧骨架样式。

## 7. 缓存架构（Rust 服务端 + 浏览器 Cache Storage）

服务端（`revaro-server`）：

- 解析后的书由 `ReaderRuntime` 的有界 LRU 保存；按对象键加锁，避免并发首次
  打开重复解析。
- flow manifest/chunk 以 `flows/{object_key}/f{version}/` 持久化到本地对象存储，
  manifest 最后写入；缺失 chunk 会触发单飞重建。`FLOW_CACHE_TTL` 和
  `FLOW_CACHE_CAPACITY` 由后台回收逻辑约束。
- 媒体派生结果使用 `MediaRuntime` 的并发限制、按源版本的锁和有界字幕缓存；
  原文件 Range 直接从本地对象读取，不经过远程对象服务或 HLS 工作区。
- `LocalStore` 的临时文件、fsync、原子替换和只创建派生对象语义保证崩溃与并发
  下不会暴露半成品。

客户端（`crates/revaro-web/src/components/reader_cache.rs`）：

- 内存 `ChunkCache` 是 L1；manifest 用 `localStorage` 快速恢复，HTML chunk
  用 Cache Storage 作为 L2，避免把大段 HTML 放进同步存储。
- chunk 键同时包含文件对象键、flow 版本、源指纹和布局/目录指纹；同一文件 ID
  被替换或目录元数据改变时不会误读旧内容。
- 浏览器端在把 manifest 或 chunk 放进 DOM 前验证版本、范围、总量、连续性和
  TOC 目标；旧 generation 的异步请求不会写入当前窗口。

## 8. 实施状态

- Rust：`crates/revaro-reader` 负责 EPUB/TXT 解析和 flow；
  `crates/revaro-server/src/reader_routes.rs` 负责 manifest/chunk 端点、幂等
  构建、自愈和进度；`crates/revaro-web/src/components/reader.rs` 负责 CSS
  columns、窗口、导航、偏好和生命周期；`reader_cache.rs` 负责浏览器缓存。
- 测试：`revaro-reader` 与 `revaro-server` 覆盖 flow 不变量、locator 往返、
  spine 边界、TXT 连续性、端点契约、并发幂等和 chunk 自愈；`revaro-web` 覆盖
  纯逻辑与 manifest 校验；`tests/e2e/rust-reader-ui.spec.ts` 覆盖真实上传、
  首屏、翻页、目录、重排、进度重开、缓存和深链。

已知取舍：

- 客户端分页的列号/页数是“已加载窗口”内的显示量，跨书进度用文本百分比表示，
  不承诺与任何服务端页码一致；
- 恢复/重排定位以“anchor 所在栏”为粒度（栏界随布局变化，重排后可见栏顶
  内容块可前后移动数块，anchor 内容本身仍在当前栏内）；
- 长 spine（单文件巨著）的排版前缀会随阅读位置线性增长，这是第一阶段的
  明确取舍（见第 5 节）；后续性能基准应迁到 Rust/独立浏览器测试 harness，
  不得把旧 Vue 开发服务器作为生产依赖；
- 旧格式进度只对 spine 0 精确迁移（spine > 0 时块号换算会偏到全书起点附近），
  由于旧格式仅存在于开发期，未做进一步兼容。

## 9. 移动端性能与镜像体积优化（随架构保留）

- **will-change 生命周期**：`.rf-flow` 不常驻 `will-change: transform`，只在
  跟手拖动/翻页 WAAPI 动画期间由 JS 临时开启、动画结束即释放——避免 Android
  Chrome 在大图旁把整层文字常驻合成导致轻微发糊与掉帧。
- **增量窗口**：阅读窗口滑动不再清空重建最多 8 个 chunk，改为增量 DOM 更新：
  只 append 新 chunk、remove 已出窗口的最旧 chunk，保留 chunk 的子树不动
  （避免大规模 CSS columns reflow）。窗口常驻约 6 个 chunk
  （身后 2 + 前方 3），PageCache 容量 24 保持不变——翻页热路径仍零网络。
- **精简镜像**：发布镜像不安装 Debian 完整 ffmpeg，而由 Dockerfile 的 multi-stage
  `ffmpeg` 生成与 Rust `revaro-media` ABI 匹配的 shared libraries；运行层只
  COPY `revaro`、Leptos bundle 和少量 `.so`，没有 `ffmpeg`/`ffprobe` 命令、
  独立 data-plane 或 Go/Node 构建产物。

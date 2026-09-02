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
| **服务端（Go）** | EPUB 解包、spine/TOC 解析、XHTML 白名单清洗、图片固有尺寸/宽高比提取、资产分发、locator（readingAnchor）；把整本书拼成一条**连续、标准化、可缓存的 reading flow**，再按**自然 DOM 边界**切成适量 chunk（chunk 不是页面，不与 viewport/字号绑定） |
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
  一个 chunk 不跨 block。客户端按“当前位置为中心”加载一个小的连续窗口
  （前后留余量、上限 8 个 chunk），阅读方向向前时窗口滑动。
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

## 4. 客户端分页与窗口

- 容器：`.rf-flow.revaro-content` 是一棵连续多栏容器（绝对定位、宽=屏宽、
  高=阅读区高、`column-width`/`column-gap`/`column-fill:auto`、字号/行距/
  图片上限等以 CSS 变量注入）。栏距恒等于 2×侧边距 → 栏宽 + 栏距 = 屏宽，
  因此每“页”恰好平移一屏宽（纯 transform）。
- 窗口：已加载 chunk 是 `[c0, c1]` 的连续区间；`measureCols()` 用
  `scrollWidth / 屏宽` 得到栏数。前进到窗口边缘时先预取下一 chunk（后台，
  不在翻页热路径），窗口过宽时从身后丢弃（重排后按顶行 anchor 对位，视觉
  不跳）。
- 交互：左右热区、方向键/PageUp/PageDown/空格、移动端跟手滑动（快速轻扫或
  拖过 1/4 屏判定）；工具栏、目录抽屉、进度条（按全文 UTF-16 比例的 0–1000
  滑块）、字号 14–32 / 行距 / 明暗主题沿用旧骨架样式。

## 5. 实施状态

- Go：`internal/reader/flow`（Anchor/Manifest/构建+chunk 纯函数）、对象键；
  `internal/server/reader_flow.go`（manifest/chunk 端点、单飞构建、GC）；
  删除 `internal/reader/layout`（chromedp 分页器）、`reader_layout.go`、
  `reader_layout_test.go`；config 移除 `REVARO_CHROME_BIN`/LAYOUT_*，新增
  `FLOW_CACHE_TTL`/`FLOW_CACHE_CAPACITY`；Docker 镜像不再装 Chromium。
- Web：`web/src/reader/{types,api,flow,cache,prefs}` 重写；`Reader.vue` 为
  窗口化 chunk + CSS columns 阅读器；样式随前端打包（不再请求 `/api/reader.css`）。
- 测试：Go（flow 构建不变量、TXT 连续性、目录映射、服务端端点契约、旧端点
  移除、GC）；Web vitest（纯 helper）；Playwright route-mock e2e（窗口预取、
  热路径零网络、字号重排零请求、anchor 进度恢复）。

已知取舍：

- 客户端分页的列号/页数是“已加载窗口”内的显示量，跨书进度用文本百分比表示，
  不承诺与任何服务端页码一致；
- 恢复定位以“anchor 所在栏”为粒度（重排后 anchor 可能位于该栏中部，属于
  一到几行的精度）；
- 旧格式进度只对 spine 0 精确迁移（spine > 0 时块号换算会偏到全书起点附近），
  由于旧格式仅存在于开发期，未做进一步兼容。

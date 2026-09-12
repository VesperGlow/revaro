# Revaro：前后端统一 Rust 迁移

本文是迁移的总入口。两份代码审计见
[audit-backend.md](audit-backend.md) 与 [audit-frontend.md](audit-frontend.md)。

## 1. 审计结论（重要）

任务描述里写的是「后端继续使用 Rust」。**审计发现这与仓库现状不符**：

| 部分 | 迁移前实际技术栈 | 规模（非测试） |
|---|---|---|
| 后端 | **Go**（chi + `database/sql` + `modernc.org/sqlite`） | ~13,500 行，36 个测试文件 / 157 个测试 |
| 媒体/压缩包 | Rust（`data-plane/`，axum，独立进程） | ~1,420 行 |
| 前端 | **Vue 3 + TypeScript**（Vite） | ~5,850 行 TS/Vue + 4,550 行 CSS |
| 构建 | npm + Vite（前端）、Go（后端）、Cargo（data-plane） | 三套工具链 |

后端并不是 Rust，而是 Go；Rust 只覆盖媒体与解压。因此「后端继续使用 Rust」
在本仓库中只能理解为**把 Go 后端移植到 Rust**——这正是本迁移的主体工作量。

另外两个影响方案的事实：

* 本环境**没有安装 Go 工具链**，无法编译或运行原后端。因此迁移以
  Go 源码 + Go 测试（157 个，作为行为规范）+ 审计文档为准，而不是靠
  对拍运行结果。
* Go 后端是**进程内外的唯一 HTTP 服务**：Rust data-plane 只监听回环并
  依赖 Go 生成的一次性 token。统一到 Rust 后这层进程边界可以直接消除。

## 2. 目标形态

一个 Cargo workspace、一套共享类型、一套构建流程。

```
Cargo.toml                  # workspace 根（members = crates/*, xtask）
rust-toolchain.toml         # 固定 1.98 + wasm32-unknown-unknown
.cargo/config.toml          # cargo xtask 别名
crates/
  revaro-core/              # 共享：领域模型、API 契约、错误模型、校验、分类规则
  revaro-server/            # Axum 后端（bin: revaro）
  revaro-web/               # Leptos 前端（CSR → wasm32）
  revaro-media/             # 媒体/压缩包引擎（由 data-plane 改造，in-process）
  revaro-reader/            # EPUB/TXT 解析与 reading flow 生成
xtask/                      # 构建编排（cargo xtask ...）
```

### 关键设计决定

1. **`revaro-core` 同时编译到 native 与 `wasm32-unknown-unknown`。**
   它因此不依赖 tokio、rusqlite、文件系统或任何原生设施。共享的是
   *模型与规则*，不是实现。
2. **媒体引擎改为进程内库。** 原 data-plane 是独立进程 + 回环 + bearer
   token。合并进 `revaro-server` 后，进程监督、token 校验、data-plane
   客户端那一整套（约 300 行 Go）消失。
3. **前端 CSR（client-side rendering）。** 产品核心是长生命周期的交互状态
   （阅读器分页、媒体播放与进度、上传队列、任务中心），SSR/hydration
   只会增加复杂度而不带来收益。
4. **`APP_WEB_DIR` 取代 `go:embed`。** 服务端从磁盘读前端产物，
   `cargo build -p revaro-server` 不再依赖「前端必须先构建」。这是对
   Go 版本 `go:embed` 强制构建顺序的简化。
5. **保留既有 SQLite schema 与 `objects/` 布局。** 001/002 迁移原样保留，
   `blobs/`、`thumbs/`、`flows/`、`profile/avatar`、`.multipart/` 键格式不变，
   不做数据迁移。
6. **保留既有 HTTP 契约。** 路由、状态码、`{error:{status,code,message}}`
   信封、`omitempty` 语义、字段名全部按原样复刻，并由 `revaro-core` 的
   测试固定。

## 3. 分阶段计划

每个阶段结束时都必须：可编译、测试通过、可运行、可部署。

### ✅ 阶段 0 — 审计
产出 `audit-backend.md`、`audit-frontend.md`。完成。

### ✅ 阶段 1 — 工作区与共享类型（本次完成）
* 建立 Cargo workspace、工具链固定、`cargo xtask` 构建编排。
* `revaro-core`：领域模型、API 契约、错误模型、校验、文件分类、
  object key 规则、时间戳表示、SHA-256。
* `revaro-server` 骨架：配置（含 `TRUSTED_PROXIES` CIDR）、错误→响应映射、
  安全响应头、Origin 守卫、SPA 静态服务、`/healthz`、`/readyz`、
  `/api/*` JSON 404。
* `revaro-web` 骨架：Leptos CSR 外壳，调用 `/healthz` 并用共享的
  `revaro_core::api::Health` 解析响应。
* `data-plane/` 暂以 `workspace.exclude` 保持独立，Go 后端继续可用。

验收：`cargo test`、`cargo clippy -D warnings`、`cargo fmt --check`、
`cargo xtask web-build` 全绿；`revaro` 二进制可启动并正确响应上述路由。

### 阶段 2 — 后端基础设施
`revaro-server` 内按 Go 包顺序移植，每个模块带 Go 测试对应的 Rust 测试：
1. **`database`（已完成）**：连接池、WAL、busy timeout、嵌入式迁移
   （001/002 原样引用）、`schema_migrations`、目录/文件权限。
2. `storage`：`objects/` 本地存储、临时文件 + fsync + 原子 rename、
   分片上传目录、只读句柄、前缀扫描、cleanup 队列。
3. `ids` / `auth`：UUIDv4、Argon2id 口令、session cookie、TOTP
   （AES-GCM 密钥、恢复码、重放步数）、限流。
4. `config` 补齐 `MEDIA_*`/`FLOW_*` 语义与启动自检。

### 阶段 3 — 文件与上传
`files`（浏览、面包屑、移动、复制、删除、回收站）、`documents`、
`uploads`（single/multipart、幂等 complete、ETag 组装、499 取消）、
`trash`、`batch-download`（ZIP 流式）、`share`。

### 阶段 4 — 后台系统
`tasks` / `task_manager` / `jobs`(SSE) / `cleanup_manager` / `object_manager`、
`cache`（L1/L2 字节 LRU、优先级与软配额、singleflight、磁盘 `.meta` 格式）、
`system_status`。

### 阶段 5 — 阅读器
`revaro-reader`：EPUB 解析与白名单清洗、TXT 分章、reading flow 生成
（确定性 chunk、`data-block` 编号、UTF-16 偏移、TOC 目标）、flow 缓存。

### 阶段 6 — 媒体
`revaro-media`：由 `data-plane` 改造为库（probe、缩略图、音频封面、
字幕提取、压缩包解压），并接入 `revaro-server`。

### 阶段 7 — 前端
Leptos 按模块渐进替换 Vue：外壳/登录 → 文件浏览 → 上传与任务中心 →
媒体播放 → 阅读器（最高风险项）。CSS 约 98.5% 可原样复用。

### 阶段 8 — 收尾
删除 `web/`（npm 链）、`internal/`、`cmd/`、`go.mod`；重写 Dockerfile 与
CI；更新 README 与 docs。

## 4. 已记录的风险与遗留问题

来自两份审计，迁移时必须逐条处理：

* **reading flow 需要逐字节一致的输出**——Go 侧 EPUB 目录/封面选择遍历
  map，本身是非确定的，与「同一本书永远生成相同产物」的文档承诺矛盾。
  移植时应改为确定性排序。
* **`data-source-path` / `data-frag-ids` 使用 Go `%q` 而非 HTML 转义**，
  存在属性注入面。
* **`originGuard` 拒绝任何缺少 `Origin` 的写请求**（包括登录），
  非浏览器客户端会得到 403。
* **`object_cleanup` 没有重试上限/死信**；`tasks.retry_count` 只在启动或
  手动重试时生效，没有周期性心跳/租约回收。
* **TOTP 恢复码使用无盐 SHA-256**；`ChangeUsername` 会保留 session 但改变
  其身份。
* **`Local` 的 ETag 是 size+mtime 而非内容哈希**。
* `docs/reader-flow.md` 已过时（提到 S3/HLS，实际是纯本地、无 HLS）。
* 前端 `reader-real-epub.spec.ts` 里有一条**故意失败**的断言
  （`windowSync` 不得把内容向后移动）。

## 5. 构建与检查

```sh
cargo xtask web-build     # 构建 Leptos 前端到 dist/web
cargo xtask check         # fmt + clippy + 全量测试 + wasm 类型检查
cargo xtask build         # release 服务端 + 前端产物
cargo test --workspace    # 仅测试
```

`wasm-bindgen` CLI 版本必须与 workspace 中固定的 `wasm-bindgen` 完全一致
（当前 `0.2.128`）：

```sh
cargo install wasm-bindgen-cli --version 0.2.128
```

CI 新增 `rust` job，用 `cargo xtask check` 校验整个 workspace；
`cargo xtask check` 会先比对 wasm-bindgen 的固定版本，防止 CLI 与 crate
的私有 ABI 版本漂移。

## 6. 功能清单（用于逐项核对「不回退」）

目标描述里列举了「阅读器、媒体播放、文件管理、搜索、回收站、任务系统」。
按源码核对后的真实情况如下——**其中「搜索」并不存在**，因此没有需要保留的
搜索功能，迁移中不应凭空新增：

| 功能 | 迁移前是否存在 | 实现位置 |
|---|---|---|
| 文件管理（浏览/新建/重命名/移动/复制/删除/下载） | ✅ | `internal/server/server_files.go` |
| 上传（单请求 + 分片续传 + 幂等完成） | ✅ | `server_uploads.go`、`upload_content.go` |
| 回收站（列出/还原/清空/彻底删除/保留期） | ✅ | `server_files.go`，`TRASH_RETENTION` |
| 任务系统（列表/取消/重试/输入 + SSE 事件） | ✅ | `tasks.go`、`task_manager.go`、`jobs.go` |
| 分享链接（公开 `/s/{token}`） | ✅ | `server_stream_share.go` |
| 文本编辑（≤1 MiB 白名单扩展名） | ✅ | `server_files.go` |
| 阅读器（EPUB/TXT + reading flow） | ✅ | `internal/reader` |
| 媒体播放（原文件 Range、字幕、进度） | ✅ | `server_stream_share.go`、`video_media.go`、`media_progress.go` |
| 缩略图与音频封面 | ✅ | `thumb.go` |
| 压缩包解压 | ✅ | `archive.go` + Rust data-plane |
| 批量下载（流式 ZIP） | ✅ | `download_batch.go` |
| 系统状态（含 SSE 流） | ✅ | `system_status.go` |
| TOTP 两步验证与恢复码 | ✅ | `internal/auth/totp.go` |
| **搜索** | ❌ **不存在** | 无端点、无 UI，仅在注释/定位逻辑中出现同名词 |

## 7. 前端 CSS 级联顺序（契约，勿凭猜测）

样式表的加载顺序是**有语义的**：多个文件对同一选择器竞争，顺序决定胜出者
（例如 `.app-shell` 的 grid 列宽由 shell.css 定义后又被覆盖）。权威顺序取自
`web/src/main.ts` 与 `web/src/style.css` 的 import 序列：

| # | 文件 | 来源 |
|---|---|---|
| 1 | `styles/shell.css` | `style.css` 内 `@import` |
| 2 | `styles/browser.css` | 同上 |
| 3 | `styles/uploads.css` | 同上 |
| 4 | `styles/dialogs.css` | 同上 |
| 5 | `styles/media.css` | 同上 |
| 6 | `styles/responsive.css` | 同上 |
| 7 | `styles/library.css` | 同上 |
| 8 | `account.css` | `main.ts` |
| 9 | `ui.css` | `main.ts`（`:root` 令牌的权威定义在这里，覆盖 responsive.css） |
| 10 | `styles/selection-toolbar.css` | `main.ts` |
| 11 | `styles/share-dialog.css` | `main.ts` |
| 12 | `styles/document-editor.css` | `main.ts` |
| 13 | `styles/reader-flow.css` | `main.ts` |
| 14 | `styles/reader-chrome.css` | `main.ts` |
| 15 | `styles/video-player.css` | `VideoPlayer.vue` 的 `<style src>`，随组件加载，故排在最后 |

移植时必须以单个聚合文件（`@import` 或按序拼接）复现这 15 项顺序，并用测试
固定，避免后续编辑悄悄改变级联结果。

## 8. 进度日志


| 阶段 | 状态 | 提交 |
|---|---|---|
| 0 审计 | ✅ | 后端/前端审计文档 |
| 1 工作区 + 共享类型 | ✅ | `refactor(rust): 建立 Cargo workspace 与前后端共享 core crate` |
| 2a 数据库层 | ✅ | `refactor(rust): 移植 SQLite 数据库层并接入服务启动` |
| 2b 对象存储 | ✅ | `refactor(rust): 移植本地对象存储并接入启动与就绪检查` |
| 2c 阅读器解析 | 🚧 | `crates/revaro-reader`（EPUB/TXT 解析与白名单清洗） |
| 2d 认证与会话 | 🚧 | `revaro-server::auth` + `/api/auth/*` |
| CI 覆盖 | ✅ | `build(ci): 新增 Rust workspace 检查任务…` |

阶段 2b 验收：171 个测试通过（core 89 + server 82 + xtask 5）；实测启动
自动创建 `objects/` 并在日志中确认就绪；对象存储测试覆盖原子写入无残留、
尺寸不匹配回滚、符号链接拒读、路径穿越拒绝、7 个并发写者不留撕裂对象、
分片顺序拼装与 ETag 校验、过期清理不误伤进行中的上传。

阶段 1 验收：128 个测试通过、`cargo fmt --check` 与
`clippy -D warnings` 干净、`cargo xtask web-build` 产出 192 KB wasm
包、实测二进制正确响应 `/healthz`、`/readyz`、`/api/*` JSON 404、
SPA 回退与路径穿越防护。

阶段 2a 验收：141 个测试通过；实测启动自动建库并应用迁移 1+2，
13 张表齐全；`directory_stats` 递归触发器、`object_cleanup` 触发器与
外键约束均有测试覆盖。

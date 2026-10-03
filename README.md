# Revaro

自托管的个人内容库，把阅读、音乐和图库放在同一个应用里。Rust 管理账户、文件目录和 SQLite 元数据，原始文件以 UUID blob 存储在服务器硬盘；`revaro-media` 在进程内提供媒体信息和缩略图。

首页展示继续阅读、最近播放和新加入的图片。阅读书库支持 EPUB/TXT；音乐库提供歌曲列表、歌单和跨页面持续播放；图库提供图片墙、相册和大图浏览。三个内容库支持全局文件名搜索、收藏和分页，通过文件 ID 共用原文件。已有文件会在升级时自动分类，新上传的内容在上传完成后进入相应内容库。

文件管理保留上传、文件夹整理、分片续传、下载、分享、回收站、文本编辑及视频播放。五个页面共用一个顶栏，桌面中间以 SVG 图标切换首页、阅读、音乐、图库和文件，移动端使用同一组底部导航。此版本尚未提供作者/歌手/专辑标签、EXIF 日期分组、批注或刷新后的音乐队列恢复。

## 本地体验新版

```sh
./scripts/preview.sh
# 另开终端，导入原创演示书籍、合成音频和图片（可选）
python3 scripts/seed-preview.py
```

访问 `http://localhost:8081`，账户 `admin`，预览密码 `revaro-preview-2026`。脚本使用 `data/content-preview/` 中的独立数据库、对象及缓存；静态资源放在 `dist/content-preview-web/`。可通过 `PREVIEW_PORT`、`PREVIEW_ROOT` 和 `PREVIEW_PASSWORD` 自定义预览配置。已构建后可用 `./scripts/preview.sh --no-build` 启动。

## 部署

复制 `.env.example` 为 `.env`，填写访问地址后启动：

```sh
docker compose up -d
```

默认访问 `http://localhost:8080`。首次启动时，若未设置 `ADMIN_PASSWORD`，系统生成随机管理员密码并写入容器 `/data/initial-admin-credentials`，文件权限为 0600。登录后请在账户设置中修改凭据，并删除该凭据文件。支持 TOTP 两步验证与恢复码。

镜像为 `ghcr.io/vesperglow/revaro:latest`，以 UID/GID 10001 运行。默认仅将端口映射到主机回环地址；公网使用时通过反向代理提供 HTTPS，并将 `APP_BASE_URL` 设为实际访问地址。`TRUSTED_PROXIES` 只填写受信代理的 CIDR。

本地目录：

- `/data/revaro.db`：SQLite 元数据。
- `/objects/blobs/<UUID>`：普通文件内容。
- `/objects/profile/`、`thumbs/` 及阅读对象：持久化附属内容。
- `/objects/.multipart/`：正在上传的分片。
- `/caches/`：可重建的媒体和阅读缓存。

`/data`、`/objects`、`/caches` 是三个平级目录。务必持久化 `/data` 和 `/objects`；`/caches` 可清空重建。数据库和文件均只保存在本机。提供停服备份/恢复命令（见下文），也可以自行备份数据库和对象卷；定时备份由宿主机调度。

Compose 默认创建三个独立的命名卷；若要把文件实际放到另一块磁盘，应将 `revaro-objects` 卷映射到宿主机该磁盘上的目录，并确保 UID/GID 10001 可写。仅分成三个命名卷不保证它们位于不同磁盘。

已有安装从单卷布局升级时，先停服并备份旧的 `revaro-data` 卷，再把旧卷中的 `objects/` 内容复制到新的 `revaro-objects` 卷：

```sh
docker compose stop revaro
docker compose run --rm --no-deps --user 0 --entrypoint sh revaro -ec '
  test -d /data/objects
  test -z "$(find /objects -mindepth 1 -maxdepth 1 -print -quit)"
  cp -a /data/objects/. /objects/
  chown -R 10001:10001 /objects
'
docker compose up -d
```

确认文件能正常浏览、下载后，旧的 `/data/objects` 才可清理。旧的 `/data/work`、`/objects/.work` 和 `/work` 仅用于临时文件或缓存，无需迁移；确认旧实例已停止后即可清理。请勿在复制前启动新布局，否则新对象卷可能已有内容，迁移命令会拒绝覆盖。

## 播放

浏览器直接读取原文件，支持 HTTP Range、拖动定位、音量、倍速及进度同步。播放能力取决于浏览器支持的容器和编解码器；无法解码时可下载到本地播放器。服务端保留媒体探测和封面/缩略图生成。

## 配置

| 变量 | 默认值 | 用途 |
|---|---|---|
| `APP_ADDR` | `:8080` | 应用监听地址 |
| `APP_DATA_DIR` | `/data` | 数据库和配置目录 |
| `APP_OBJECTS_DIR` | `/objects` | 持久对象和上传分片根目录 |
| `APP_CACHES_DIR` | `/caches` | 可重建的媒体和阅读缓存目录 |
| `APP_BASE_URL` | `http://localhost:8080`（Compose 会随 `APP_PORT` 推导） | 公网访问地址、同源检查和分享链接 |
| `COOKIE_SECURE` | 随 HTTPS 地址启用 | Cookie Secure |
| `UPLOAD_EXPIRES` | `24h` | 上传会话有效期 |
| `UPLOAD_IDLE_TIMEOUT` | `60s` | 上传请求无字节进展超时 |
| `UPLOAD_REQUEST_TIMEOUT` | `24h` | 单次请求总超时，分片分别计时 |
| `UPLOAD_MIN_FREE_BYTES` | `67108864` | 接收上传后需保留的磁盘空间（64 MiB） |
| `TRASH_RETENTION` | `720h` | 回收站保留时间 |
| `GC_INTERVAL` | `1h` | 孤立文件回收间隔，0 关闭 |
| `MEDIA_CACHE_CAPACITY` | `2147483648` | 本地工作缓存容量（字节） |
| `FLOW_CACHE_TTL` | `720h` | 阅读派生对象回收时间 |
| `FLOW_CACHE_CAPACITY` | `1073741824` | 阅读派生对象容量 |


## 文件与恢复

文件浏览支持文件名包含搜索：在“我的文件”根目录搜索全盘，进入文件夹后只搜索当前目录的直接子项；清空关键词恢复目录列表。前后端按目录自动确定范围，不再提供范围切换。顶栏放大镜从原位向左展开为紧凑搜索栏，默认约 208px，随输入文字宽度适度增长，并受视口和导航可用空间限制；右侧按钮保持原位。Enter 搜索，×、Esc 或点击空白处收起并保留输入。内容区保留路径、摘要和名称/大小/时间排序；单页连续滚动，每次追加 100 项。顶栏新建、上传和任务中心共用按钮对齐的弹层定位。选中目录可递归复制或下载 ZIP；复制最多 5,000 项，ZIP 最多 1,000 个文件、5,000 个节点、128 层和 1 TiB。

分享对话框可设置 1 小时、1 天、7 天、30 天或永久有效期；重新生成会使旧链接失效。账户设置里的“分享管理与运行状态”直接展示公开链接总览与撤销，以及磁盘可用空间、内存缓存、磁盘缓存、活动任务的简洁摘要；可刷新状态或清理本机阅读缓存。状态加载时使用固定高度骨架占位，失败提示也保留相同高度。没有公开链接时只显示“暂无公开链接”，分页仅在需要时出现。

文本编辑器的“版本历史”保留最近 20 次保存前内容，可查看及恢复。恢复会保留当前内容并检查 ETag，文件永久删除时一并清理历史。升级前的旧内容不会自动恢复成历史版本。

浏览器阅读缓存最多 32 份清单、256 个 HTML chunk、32 MiB，有效期 7 天；账户页可清理，退出登录及改密码时也会清理。服务端 EPUB flow 格式升级到 v5，旧派生数据会重新生成。

停服后，以与服务相同的环境变量运行（数据库与对象目录必须对应同一个实例）：

```sh
revaro backup /backups/revaro-2026-10-02
revaro restore /backups/revaro-2026-10-02
revaro reset-admin
```

`backup` 的目标目录必须不存在，且位于数据/对象目录之外；它生成 SQLite 一致性快照、完整对象副本和 SHA-256 校验清单。`restore` 会先验证校验值与数据库完整性，并要求目标没有数据库、对象目录为空。恢复不需要复制 `/caches`。

`reset-admin` 清除所有会话和 TOTP，生成随机密码并写入 `/data/admin-recovery-credentials-<UUID>`（0600），日志只给出路径。`ADMIN_USERNAME` 非空时用该用户名，否则恢复为 `admin`。读取后登录修改密码并删除凭据文件。服务及这些命令共享进程锁，运行中的实例会拒绝离线操作。

Docker 中可在停服后使用 `docker compose run --rm --no-deps revaro reset-admin`。备份/恢复需要额外将备份目录挂载到容器，并确保 UID/GID 10001 可以写入。

负数保留时间/超时会阻止启动；`TRASH_RETENTION=0` 关闭自动清空，`GC_INTERVAL=0` 关闭孤立对象扫描。阅读派生数据有独立维护任务。`APP_BASE_URL` 必须是无用户名、路径、查询和片段的 HTTP(S) 站点地址。

## 开发与检查

Rust 工作区：

```sh
cargo xtask web-build     # 构建 Leptos 前端到 dist/web
cargo xtask check         # fmt + clippy + 全量测试 + wasm 类型检查
cargo xtask build         # release 服务端 + 前端产物
```

`wasm-bindgen` CLI 版本必须与 workspace 固定的版本一致
（`cargo install wasm-bindgen-cli --version 0.2.128`）。

浏览器行为测试使用 `tests/e2e/` 中仅包含 Playwright 的 npm 包；它不参与生产
构建或运行。CI 会对 Rust 镜像执行 `cargo xtask build`，并在真实容器中运行 Rust
媒体与阅读器 E2E。

Rust 本地编译需要 FFmpeg 开发库、clang 和 cmake；Dockerfile 包含完整构建环境。生产 FFmpeg 只保留媒体读取所需库，不包含转码命令或编码器。

`main` 推送会触发 GitHub Actions：Rust workspace 检查、依赖扫描、镜像构建和 Chromium E2E 通过后发布 GHCR 镜像。浏览器测试使用 `compose.e2e.yml` 启动全新本地存储服务。

媒体与存储边界见 [data-plane.md](docs/data-plane.md)。

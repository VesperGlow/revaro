# Revaro

轻量、自托管的本地文件管理器。Go 管理账户、文件目录和 SQLite 元数据，普通文件以 UUID blob 存储在服务器硬盘；Rust 提供媒体信息、缩略图、字幕提取和压缩包解压。

支持文件及文件夹上传、大文件分片续传、下载、分享、回收站、文本编辑、EPUB/TXT 阅读、原文件音视频播放、字幕和播放进度。不提供离线下载、音频合并或音视频格式转换。

## 部署

复制 `.env.example` 为 `.env`，填写访问地址后启动：

```sh
docker compose up -d
```

默认访问 `http://localhost:8080`。首次启动时，若未设置 `ADMIN_PASSWORD`，系统生成随机管理员密码并写入容器 `/data/initial-admin-credentials`，文件权限为 0600。登录后请在账户设置中修改凭据，并删除该凭据文件。支持 TOTP 两步验证与恢复码。

镜像为 `ghcr.io/vesperglow/revaro:latest`，以 UID/GID 10001 运行。默认仅将端口映射到主机回环地址；公网使用时通过反向代理提供 HTTPS，并将 `APP_BASE_URL` 设为实际访问地址。`TRUSTED_PROXIES` 只填写受信代理的 CIDR。

本地目录：

- `/data/revaro.db`：SQLite 元数据。
- `/data/objects/blobs/<UUID>`：普通文件内容。
- `/data/objects/profile/`、`thumbs/` 及阅读对象：持久化附属内容。
- `/data/objects/.multipart/`：正在上传的分片。
- `/data/work/`：解压和可回收缓存。

这是面向全新安装的本地产品，使用空的数据目录启动，不提供旧版本迁移或兼容逻辑。务必持久化整个 `/data`；数据库和文件均只保存在本机。应用不提供自动数据库备份，请在停服后自行备份完整数据卷。

## 播放

浏览器直接读取原文件，支持 HTTP Range、拖动定位、音量、倍速、字幕及进度同步。播放能力取决于浏览器支持的容器和编解码器；无法解码时可下载到本地播放器。服务端不再做视频或音频转码、兼容流封装或音频合并。保留媒体探测、封面/缩略图和 WebVTT 字幕提取。

## 配置

| 变量 | 默认值 | 用途 |
|---|---|---|
| `APP_ADDR` | `:8080` | 应用监听地址 |
| `APP_DATA_DIR` | `/data` | 数据库和本地对象根目录 |
| `APP_WORK_DIR` | `/work`，镜像内为 `/data/work` | 临时工作目录 |
| `APP_BASE_URL` | `http://localhost:8080` | 公网访问地址、同源检查和分享链接 |
| `COOKIE_SECURE` | 随 HTTPS 地址启用 | Cookie Secure |
| `UPLOAD_EXPIRES` | `24h` | 上传会话有效期 |
| `TRASH_RETENTION` | `720h` | 回收站保留时间 |
| `GC_INTERVAL` | `1h` | 孤立文件回收间隔，0 关闭 |
| `MEDIA_CACHE_CAPACITY` | `2147483648` | 本地工作缓存容量（字节） |
| `FLOW_CACHE_TTL` | `720h` | 阅读派生对象回收时间 |
| `FLOW_CACHE_CAPACITY` | `1073741824` | 阅读派生对象容量 |


## 开发与检查

```sh
cd web
npm ci
npm test
npm run build
npm run lint
cd ..
go test -race ./...
go vet ./...
cd data-plane
cargo fmt --check
cargo clippy --locked --all-targets -- -D warnings
cargo test --locked
```

Rust 本地编译需要 FFmpeg 开发库、clang、cmake 和 libarchive 相关构建依赖；Dockerfile 包含完整构建环境。生产 FFmpeg 只保留媒体读取所需库，不包含转码命令或编码器。

`main` 推送会触发 GitHub Actions：前后端测试、依赖扫描、镜像构建和 Chromium E2E 通过后发布 GHCR 镜像。浏览器测试使用 `compose.e2e.yml` 启动全新本地存储服务。

架构细节见 [data-plane.md](docs/data-plane.md)。

# CI 与镜像构建

当前发布目标为 `linux/amd64`。`rust` job 运行 workspace 的格式检查、
`clippy -D warnings`、完整测试、wasm32 类型检查、Rust 依赖审计和 release
构建，以及 10 万文件、8 并发、600 次请求的独立负载检查（各接口 p95 不超过
2 秒），并验证突发过载、分片续传、存储空间不足和强制终止后的恢复。
所有 Rust 原生依赖由 runner 安装。该 job 只编译负载测试所需的 release 服务端，
前端 release 包和与 workspace 锁定版本相同的 wasm-bindgen CLI 由 Docker 构建负责。

`container` job 与 Rust job 并行构建同一个 Rust Dockerfile，并启动真实
容器执行全部 Chromium E2E、Firefox 和 WebKit 启动/草稿恢复/菜单/账户回归，以及 160 轮
编辑器和菜单资源检查。生成长音频测试夹具的 FFmpeg 命令只安装在测试 runner。
浏览器测试位于独立的 `tests/e2e/` npm
包，只包含 Playwright；`.dockerignore` 和 Dockerfile 都保证它不进入生产镜像。

Rust job 使用固定提交版本的 [rust-cache](https://github.com/Swatinem/rust-cache)
缓存依赖的编译产物与 registry，缓存键包含工具链、Cargo 配置和依赖清单。
只有 main 写入缓存，PR 可以读取 main 的缓存。CI 原生检查关闭调试符号和增量文件，
控制磁盘与缓存体积；release 优化配置保持不变。Docker 检查后保留编译结果，
不再执行 `cargo clean`，后续构建可复用 xtask 和已有依赖。
前端 release 包只构建一次。两个 job 都成功才允许发布，发布继续复用通过验收的镜像。
Firefox 不使用其不支持的移动设备模拟；WebKit 额外覆盖触屏账户界面。
头像重复读取的垃圾回收测量使用 Chromium CDP，其余浏览器不执行该测量。

生产 Compose 的 `APP_BASE_URL` 默认值会从 `APP_PORT` 推导；留空时例如
`APP_PORT=18081` 会得到 `http://localhost:18081`，显式设置公网地址则保持原值。
`COOKIE_SECURE` 默认保持为空，由 Rust 按 `APP_BASE_URL` 自动决定 HTTPS Cookie
属性。CI 在构建镜像前用 `docker compose config --format json` 同时断言两种
基址和两种情况下的空 Cookie 覆盖值，防止宿主端口调整后 Origin 守卫拒绝登录，
或示例配置意外关闭 HTTPS Cookie 安全属性。

同一配置检查还固定生产 Compose 的部署边界：80/443 公网端口、`/data`、`/objects` 和 `/caches` 三个独立卷、只读根
文件系统、`/tmp` tmpfs、`no-new-privileges`、丢弃全部 capabilities、非缓存健康
检查和 `/data`、`/objects`、`/caches`、`/opt/revaro/web` 路径。配置回归会在镜像构建前失败。

容器 job 是 `revaro-image-amd64-v3` BuildKit 缓存的唯一写入者。主分支和版本标签
在 E2E 通过后还会把已加载的 amd64 镜像导出为保留 1 天的 artifact；发布 job 下载
并加载这个 artifact，只重新打标签后推送 GHCR，因此发布的镜像就是刚刚通过容器
验收的那一个。PR 不上传镜像 artifact，也不会进入发布 job。主分支不会在缓存
导出期间取消，PR 仍会取消过时运行。正式发布仍由 CI Docker runner 完成。
2026-10-09 的加固验证在独立本地 Rust 进程上执行；当前开发环境没有可用的
Docker daemon，因此本轮改动的真实容器构建、容器运行和宿主机低端口检查
仍需由 CI 验证，不能用本地测试结果代替。具体记录见
[个人使用耐用性验收](resilience-2026-10-09.md)。

容器 job 在启动 E2E 前还会检查最终运行层的边界：镜像默认使用 UID/GID 10001
的 `revaro` 用户，并在镜像内确认不存在 Go、Node 或 npm 可执行文件。这样旧实现
或旧构建工具链若被意外复制进生产层，会在浏览器验收前直接失败。

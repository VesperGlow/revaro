# CI 与镜像构建

当前发布目标为 `linux/amd64`。`rust` job 运行 workspace 的格式检查、
`clippy -D warnings`、完整测试、wasm32 类型检查、Rust 依赖审计和 release
构建；所有 Rust 原生依赖由 runner 安装，wasm-bindgen 使用与 workspace 锁定
版本相同的 CLI。

`container` job 等待 Rust 质量门通过后构建同一个 Rust Dockerfile，并启动真实
容器执行 Chromium 媒体和阅读器 E2E。浏览器测试位于独立的 `tests/e2e/` npm
包，只包含 Playwright；`.dockerignore` 和 Dockerfile 都保证它不进入生产镜像。

生产 Compose 的 `APP_BASE_URL` 默认值会从 `APP_PORT` 推导；留空时例如
`APP_PORT=18081` 会得到 `http://localhost:18081`，显式设置公网地址则保持原值。
`COOKIE_SECURE` 默认保持为空，由 Rust 按 `APP_BASE_URL` 自动决定 HTTPS Cookie
属性。CI 在构建镜像前用 `docker compose config --format json` 同时断言两种
基址和两种情况下的空 Cookie 覆盖值，防止宿主端口调整后 Origin 守卫拒绝登录，
或示例配置意外关闭 HTTPS Cookie 安全属性。

容器 job 是 `revaro-image-amd64-v3` BuildKit 缓存的唯一写入者。主分支和版本标签
在 E2E 通过后还会把已加载的 amd64 镜像导出为保留 1 天的 artifact；发布 job 下载
并加载这个 artifact，只重新打标签后推送 GHCR，因此发布的镜像就是刚刚通过容器
验收的那一个。PR 不上传镜像 artifact，也不会进入发布 job。主分支不会在缓存
导出期间取消，PR 仍会取消过时运行。正式发布仍由 CI Docker runner 完成；当前
开发环境的 Docker service 无法启动，但已用 Buildah 的 host-userns 路径完成等价
镜像构建和容器 E2E。

容器 job 在启动 E2E 前还会检查最终运行层的边界：镜像默认使用 UID/GID 10001
的 `revaro` 用户，并在镜像内确认不存在 Go、Node 或 npm 可执行文件。这样旧实现
或旧构建工具链若被意外复制进生产层，会在浏览器验收前直接失败。

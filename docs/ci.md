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

容器 job 是 `revaro-image-amd64-v3` BuildKit 缓存的唯一写入者；发布 job 只读取
该缓存并推送经过验证的 GHCR 镜像。主分支不会在缓存导出期间取消，PR 仍会取消
过时运行。正式发布仍由 CI Docker runner 完成；当前开发环境的 Docker service
无法启动，但已用 Buildah 的 host-userns 路径完成等价镜像构建和容器 E2E。

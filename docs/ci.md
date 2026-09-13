# CI 与镜像构建

当前发布目标为 `linux/amd64`。`rust` job 运行 workspace 的格式检查、
`clippy -D warnings`、完整测试、wasm32 类型检查、Rust 依赖审计和 release
构建；所有 Rust 原生依赖由 runner 安装，wasm-bindgen 使用与 workspace 锁定
版本相同的 CLI。

`container` job 等待 Rust 质量门通过后构建同一个 Rust Dockerfile，并启动真实
容器执行 Chromium 媒体和阅读器 E2E。浏览器测试位于独立的 `tests/e2e/` npm
包，只包含 Playwright；`.dockerignore` 和 Dockerfile 都保证它不进入生产镜像。

容器 job 是 `revaro-image-amd64-v3` BuildKit 缓存的唯一写入者；发布 job 只读取
该缓存并推送经过验证的 GHCR 镜像。主分支不会在缓存导出期间取消，PR 仍会取消
过时运行。Docker daemon 不可用的本地环境只能运行等价的 Cargo 检查和真实 Rust
进程 E2E，不能声称本地完成镜像构建。

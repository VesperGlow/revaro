# CI 与镜像构建

当前只构建 `linux/amd64`。Rust release 的 LTO、codegen-units 和 strip 配置保持不变。

## 构建与验收门槛

- `rust` job 运行格式检查、`clippy -D warnings`、完整 Rust 测试、wasm32 类型检查、依赖审计、低端口权限验证，以及 10 万文件、8 并发、600 次请求的 release 负载与断流恢复检查。Rust 原生依赖由 runner 安装，继续使用固定版本的 rust-cache。
- `container` job 构建一次 amd64 镜像，检查 Compose 配置与运行层边界，再上传 `revaro-built-image`。Docker 默认仍执行 `cargo xtask check`；只有 CI 传入 `REVARO_RUN_CHECKS=0`，避免与 Rust job 重复编译、执行整套检查。
- `e2e` job 从同一个 artifact 加载镜像。全部 Chromium 测试分成 4 片，Firefox 和 WebKit 的原有回归集合各 1 片，共 6 个并行 runner；每片独立启动容器和数据库，Playwright 的单 worker 设置保留，避免账户、播放队列和进度相互干扰。每片只安装它实际使用的浏览器，并启动虚拟音频输出。编辑器/菜单的 160 轮资源检查只在 Chromium 第 1 片运行一次。
- `publish` 必须等待 Rust、镜像构建和全部浏览器分片成功，才下载同一个镜像、重新打标签并推送 GHCR。上传待测 artifact 不等于发布；PR 可以运行相同分片但不会发布。没有在 publish 中重建镜像。

每片失败诊断使用独立 artifact 名称，保留 HTML 报告、截图、视频、trace 和容器日志。一个分片失败不会取消其他分片，便于一次收齐回归结果。Firefox 继续排除不支持的移动账户模拟；三引擎均包含 native-file-loading。

## 依赖缓存与耗时

Docker 使用固定版本 cargo-chef 0.1.78：planner 从 Cargo 清单、锁文件和 target 信息生成 recipe，独立层预编译服务端 release、wasm32 release 和 xtask 依赖；应用源文件在这些层之后复制。源代码改变时仍重新编译应用，但不使整个依赖构建层失效。[cargo-chef 官方说明](https://github.com/LukeMathWalker/cargo-chef)

应用检查与构建在同一个 RUN 中完成，把二进制和 Web 包复制到 `/artifacts`，随后移除该次构建的 target。因此不会再为每个提交导出一层巨大的应用编译中间文件；之前的依赖层仍保留。BuildKit 使用 `revaro-image-amd64-v4` 的 GHA `mode=max` 缓存，同时读取 v3 作为回退，复用未变动的 FFmpeg、系统依赖与工具链层。只有 main 的镜像 job 写入 v4；PR 和标签读取缓存。切换缓存结构后的第一次构建需要预热新的依赖层，不能用它代表后续命中依赖层的耗时。主分支构建继续不中途取消；PR 可以取消过时运行。

2026-10-10 的 [CI #330](https://github.com/VesperGlow/revaro/actions/runs/38056876338) 实测：

| 阶段 | 耗时 | 结果 |
| --- | ---: | --- |
| Rust job（含 release 负载检查） | 4 分 55 秒 | 通过 |
| Docker 内重复 workspace check | 4 分 15 秒 | 通过；新流程由 Rust job 独立把关 |
| Docker release 构建 | 8 分 16 秒 | 通过；新增稳定依赖层缓存 |
| BuildKit 缓存导出 | 5 分 28 秒 | 通过；限制每提交中间文件层 |
| 单 runner Chromium | 18 分 37 秒 | 131 通过、6 失败、2 重试后通过、1 跳过；新流程分 4 片 |

此次失败来自媒体/图片已采用原生加载，而部分旧测试只对 Worker 请求注入延迟或 404，注入实际未发生。修正故障入口，并保留暂停、恢复、封面回退与布局断言。单曲循环用结束事件和实际播放状态验证，不要求浏览器额外发送新请求。

新流程的总耗时和缓存命中后的加速幅度必须以新 CI 的时间戳为准，未完成测量前不承诺固定分钟数。当前沙箱禁止创建本地 TCP/UDP socket，真实容器、浏览器与 QUIC 换地址回归由 CI 运行；本地静态检查和逻辑单测不替代这些结果。

2026-10-10 的 [CI #331](https://github.com/VesperGlow/revaro/actions/runs/38062770859) 已验证新的流程：Rust job 5 分 1 秒、镜像 job 11 分 54 秒（包括首次 cargo-chef 依赖层预热），全部 Chromium 分片和 Firefox 通过。镜像构建相较 #330 的 18 分 34 秒缩短；BuildKit 缓存导出从 5 分 28 秒降至约 1 分 21 秒。浏览器各自运行，未再把所有浏览器耗时串在镜像 job 后。

该轮仍失败于 WebKit 的 1 MiB 断流视频场景：两次尝试均停在约 0.14 秒，正常高码率场景等 31 项通过，另有 1 项跳过；publish 因验收失败被阻止。因此这不是一次完整成功发布，也不代表公网性能验收通过。针对原生恢复逻辑的修正保留 `waiting/stalled` 后最后一次 `progress` 的后续检查，用播放时钟和 `HAVE_FUTURE_DATA` 区分可播放缓冲与停滞，覆盖零秒启动；仍保留每个源最多两次恢复、暂停/跳转取消及当前位置恢复。新增 4 项逻辑测试，其中 3 项在旧实现上复现缺陷，修正后全部 42 项传输测试通过。WebKit 真实断流复测待下一轮 CI；故障时同时把媒体状态、事件和 Range 传输记录写入 job 日志，便于直接定位。

生产 Compose 的 `APP_BASE_URL` 默认值会从 `APP_PORT` 推导；留空时例如
`APP_PORT=18081` 会得到 `http://localhost:18081`，显式设置公网地址则保持原值。
`COOKIE_SECURE` 默认保持为空，由 Rust 按 `APP_BASE_URL` 自动决定 HTTPS Cookie
属性。CI 在构建镜像前用 `docker compose config --format json` 同时断言两种
基址和两种情况下的空 Cookie 覆盖值，防止宿主端口调整后 Origin 守卫拒绝登录，
或示例配置意外关闭 HTTPS Cookie 安全属性。

同一配置检查还固定生产 Compose 的部署边界：80/443 公网端口、`/data`、`/objects` 和 `/caches` 三个独立卷、只读根
文件系统、`/tmp` tmpfs、`no-new-privileges`、丢弃全部 capabilities、非缓存健康
检查和 `/data`、`/objects`、`/caches`、`/opt/revaro/web` 路径。配置回归会在镜像构建前失败。

容器 job 在启动 E2E 前还会检查最终运行层的边界：镜像默认使用 UID/GID 10001
的 `revaro` 用户，并在镜像内确认不存在 Go、Node 或 npm 可执行文件。这样旧实现
或旧构建工具链若被意外复制进生产层，会在浏览器验收前直接失败。

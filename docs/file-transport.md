# 文件传输与国际线路恢复

传输策略按请求大小和线路状态生效，不按文件扩展名或 MIME 类型分支。

| 入口 | 统一实现 |
| --- | --- |
| 任意原文件下载、可预览文件、公开链接 `/s/{token}` | Service Worker → `transport-core.js`；服务端 `transfer::serve_reader` |
| 缩略图、书籍封面/附件、阅读流/章节 | 同一 Worker；服务端 `transfer::serve_bytes` |
| 编辑器内容、历史版本、文件信息 | 同一 Worker；生成的 JSON 通过共享字节响应添加 Range/ETag |
| 批量 ZIP | 用户绑定票据；一次生成磁盘缓存文件，复用共享 Range 响应 |
| 所有非空文件上传 | UI 仅选择/切片/展示；`transport::put_blob` → 同一个 JS 核心 |
| 上传创建、状态和提交 | Worker 统一恢复；创建必须有幂等键，提交可安全重复 |

Service Worker 在 WASM 应用挂载之前激活并接管页面，因此原生 `<img>`、音视频、文档读取、下载链接和 Fetch 都经过共享层。下载链接使用普通导航并由 Content-Disposition 启动保存，避免 Chromium 的 download 属性绕过 Worker。公开链接首次访问也先加载无需登录的轻量引导页，激活共享层后再交付原文件；公开字节响应保留强 ETag，同时维持 no-store。需现代浏览器、HTTPS 或 localhost；不安全的远程 HTTP 会明确显示初始化错误。支持标准 Service Worker、Fetch、ReadableStream、AbortController、WebCrypto，不依赖私有浏览器扩展。

下载流预留一个块的队列容量，确保浏览器原生附件导航开始消费响应；Worker 的 fetch 事件保持到流结束或取消，避免只发送响应头便结束任务。

## 下载与读取

先请求 `Range: bytes=0-0`，确认当前访问权限、完整长度和强 ETag。可变目录列表、播放进度等元数据在同一个缓冲请求中原子读取，仍复用共同的停滞与重试策略，避免读目录时把多个变化快照拼在一起。文件内容随后按 512 KiB 窗口发送独立 Range 请求，默认 3 路、限制在 2～4 路；观察到 HTTP/3 时用 4 路。输出按原字节顺序流式交给消费者，预取窗口有界，不将整个大文件拼成 Blob。

每块都发送 `If-Range` 与 `If-Match`，严格验证 206、Content-Range、总长度和 ETag。连接中断保留该块已收到的前缀，下次只请求剩余区间。成功块不因其他块失败而重新下载。文件版本改变时，在输出首块前可重新探测；输出开始后停止旧流，避免拼接不同版本的数据。

已完成块保存在 CacheStorage，最多 256 块（默认约 128 MiB），缓存不可用或配额不足时继续网络传输。重新打开相同资源会重新认证/核验 ETag，再复用仍保留的块；浏览器原生下载也可以从自己的保存偏移重新发出 Range。`no-store` 响应不持久保存，退出登录清空传输缓存；过期会话/撤销分享不能读取缓存中的旧数据。缓存有界，不能保证任意超大文件的每块长期保存。

请求头等待默认 15 秒，响应体 8 秒无字节进展会取消并重连；20 秒进展窗口低于 8 KiB 也触发恢复。HTTP/3 下响应体停滞阈值缩短至约 5.2 秒。每次恢复最多 10 次尝试，400 ms 起指数退避、50%～100% 随机抖动，上限 8 秒，并尊重最多 60 秒的 Retry-After。403/404/409/412 等永久失败不会循环重试。显式取消会中断请求、排队和退避。

≤256 KiB 的资源可在 1.2 秒后补发一次请求；HTTP/3 下约 720 ms。以完整、验证成功的响应决定胜者，再取消慢请求。每个运行上下文最多同时执行 2 个 hedge，避免大量缩略图引发补发风暴；请求准入上限通常为 12，HTTP/3 下为 16。

## 上传

所有非空文件，无论类型和大小，都创建 multipart 会话。默认块大小 1 MiB，最多 10,000 块；超大文件增大块尺寸，空文件保留空对象提交。旧会话保留原先的几何参数。为兼容现有 API 客户端，单块 multipart 仍可 PUT 到会话的 `/data`；新 UI 使用编号块地址。

每个块使用 WebCrypto SHA-256，随 PUT 发送 `X-Content-SHA256`。服务端边写边哈希，在原子发布和数据库确认前验证；响应携带 ETag 与相同 SHA-256，客户端再次核对。失败/停滞仅重试该块，丢失确认时服务器比较已接收内容并保持原 ETag。已经确认的块不会重传。

重新选择文件时先读取持久会话状态，再对本地已确认块计算 SHA-256 与服务端确认比较，避免同名、同大小、同修改时间的不同文件导致错误续传。缺少哈希的旧确认会重新校验该块。成功提交幂等，丢失提交响应不会创建重复文件。浏览器重启后需要重新选择本地文件，以重新取得 File 访问权限。

## HTTP/3 与 HTTP/2 回退

浏览器原生 QUIC 管理拥塞、ACK、重传和协议协商。标准 Fetch API 不允许应用设置 Hy2 Brutal 拥塞控制、UDP FEC 或选择具体 HTTP 版本。本实现提供应用层补偿：并行独立 Range、保留字节前缀、缩短停滞阈值、受限 hedge 与快速局部重连；这些策略也适用于 HTTP/1.1 和 HTTP/2。协议识别使用可用的 Resource Timing `nextHopProtocol`，不可用时使用保守默认值。

真正可控的 HTTP/2 回退需要独立 authority，示例：

```env
APP_BASE_URL=https://files.example.com
APP_HTTP2_BASE_URL=https://files.example.com:8443
```

在同一个主机的另一个 TLS 端口提供 **仅 HTTP/2/HTTP/1.1** 的反向代理，不发布 HTTP/3 Alt-Svc。客户端在连续连接失败或停滞后自动切换该入口，保持 5 分钟，之后再次尝试主入口。配置会拒绝其他主机、降级 HTTP 和相同 authority，以保留 host-only 会话 Cookie 与 TLS 安全边界。浏览器兼容的凭证 CORS、Range/校验头和 CSP 已由服务端按该配置开启。

例如使用支持 HTTP/3 的 nginx 构建（证书路径按部署替换）：

```nginx
server {
    listen 443 ssl;
    listen 443 quic reuseport;
    http2 on;
    server_name files.example.com;
    ssl_certificate /etc/ssl/files/fullchain.pem;
    ssl_certificate_key /etc/ssl/files/privkey.pem;
    add_header Alt-Svc 'h3=":443"; ma=300' always;
    location / {
        proxy_pass http://127.0.0.1:8080;
        proxy_set_header Host $host;
        proxy_set_header X-Forwarded-Proto https;
        proxy_buffering off;
        proxy_request_buffering off;
        client_max_body_size 0;
    }
}
server {
    listen 8443 ssl;
    http2 on;
    server_name files.example.com;
    ssl_certificate /etc/ssl/files/fullchain.pem;
    ssl_certificate_key /etc/ssl/files/privkey.pem;
    # 不在此监听 QUIC，不添加 Alt-Svc；避免共用开启了 QUIC 的全局模板。
    location / {
        proxy_pass http://127.0.0.1:8080;
        proxy_set_header Host $host;
        proxy_set_header X-Forwarded-Proto https;
        proxy_buffering off;
        proxy_request_buffering off;
        client_max_body_size 0;
    }
}
```

需要开放 TCP 443/8443 与 UDP 443。未配置独立入口时浏览器负责自己的协议回退；应用继续重试，但不能保证强制从 HTTP/3 转成 HTTP/2。同主机不同端口仍可能共享物理瓶颈，回退用于避开 QUIC/UDP 路径故障。

协议依据：[HTTP Range 与 If-Range](https://www.rfc-editor.org/rfc/rfc9110.html#section-14)、[Fetch 标准](https://fetch.spec.whatwg.org/)、[Service Workers](https://www.w3.org/TR/service-workers/)、[nginx HTTP/3 模块](https://nginx.org/en/docs/http/ngx_http_v3_module.html)。

## 批量 ZIP 与验证

ZIP 先生成到 `/caches/batch-downloads/` 并原子发布，再使用相同的 Range 引擎下载。票据绑定用户，24 小时闲置有效，可多次续传；服务端进程重启需重新准备票据。生成任务受并发与磁盘预留控制，客户端断开不会解除生成锁或重复覆盖同一个临时文件。过期文件在临时存储清理任务中回收。相比原先即刻输出的 ZIP，首次开始传输前需要完成归档，并占用临时磁盘空间。

```sh
node --test tests/transport/transport.test.mjs
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo xtask web-check
```

故障注入覆盖任意 MIME 类型、响应体停滞/局部断流、约 15% 请求失败、成功块跨引擎复用、ETag 变化、hedge 取消、上传确认丢失、SHA-256 发布前验证、取消和备用 authority 切换。请求失败注入不能等价证明 5%～15% UDP/TCP **数据包** 丢失下的实测吞吐；真实 HTTP/3/HTTP/2 协商与丢包性能需要在启用 TLS/QUIC 的部署上进行隔离网络测试。

后续已增加可选原生 Quinn HTTP/3 与 `standard` / `aggressive` 拥塞控制，并用隔离 tc netem 实测 0/5/10/15% 包丢失、短暂断流和 UDP 故障回退；参数、完整异常样本与三种浏览器兼容性见 [QUIC 传输说明](quic-transport.md)。浏览器 Worker 由同一 transport core 生成 classic 脚本，支持 Firefox；上传适配器继续 import 原始 ESM core。

2026-10-06 合并版本验证：`cargo xtask check` 覆盖格式、原生 Clippy、467 项 Rust 测试及 WASM 类型检查；9 项 Node 传输故障测试通过。CI 的 Chromium 场景分阶段验证，63 项通过，1 项需要额外大型 FLAC 实例样本而跳过；覆盖真实断流代理、首次公开下载、任意文件类型、批量 ZIP、上传确认丢失、失败块补传、编辑器、阅读器、音频章节、逐行台词和桌面/移动端界面。测试在 BrowserContext 上拦截 Worker 实际请求，所有类型的测试夹具共用分块上传与 SHA-256 校验。本地已有实例同步加载了界面修正，仍为 HTTP localhost，未启用 QUIC 或独立 TLS HTTP/2 入口。

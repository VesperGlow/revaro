# 文件传输与国际线路恢复

架构分析、分阶段方案与验收记录见 [文件传输重构](file-transport-refactor.md)。统一 HTTP 契约与文件访问接口，同时按消费方式选择读取策略。

| 入口 | 统一实现与策略 |
| --- | --- |
| 原生 audio/video、img（包括缩略图、封面、书内附件） | 共享策略选择原生加载，Worker 不拦截；浏览器管理连续 Range、缓存、背压与取消 |
| 显式原文件/内容/历史版本/阅读流块 Fetch | 同一核心的有界 Range 恢复；受 Worker 控制时在 Worker 中运行，失效时在文档中运行 |
| 普通文件、公开分享 `/s/{token}`、批量 ZIP 下载导航 | 现有 Worker 的有界恢复；Worker 不可用的公开分享直接使用同一 HTTP 文件入口；不拼接整个文件为 Blob |
| 目录列表、文件信息、播放/阅读进度与编辑等可变 API | 文档内 bufferedRequest 原子读取/写入；Worker 原生转发，避免二次恢复 |
| 上传 | UI 切片与展示；put_blob → 同一 JS 核心，XHR 保留原生上传进度 |

服务端原文件读取使用可替换的 `FileAccess.open`，返回同一版本的 reader/size/ETag；本地文件的元数据取自打开的句柄。HTTP 层的 `serve_reader`/`serve_bytes` 共用 200/206/304/412/416、单区间、多区间、Content-Range、Content-Length 与强 ETag 契约。正文按下游需求读取，单次读缓冲 64 KiB；原文件完整响应也限于打开时捕获的长度。零长度后缀和空对象的 Range 返回带 `bytes */size` 的 416；未知 Range 单位忽略并返回完整 200。

所有私有文件、派生图片与阅读资源统一使用 `private, no-cache`：可保存字节，但每次 HTTP 缓存复用前重新鉴权、校验 ETag。公开分享保留更严格的 no-store。退出登录清理应用块缓存；浏览器 HTTP 缓存的复用仍需通过服务端鉴权。

## 原生播放与图片

原生 audio/video/img 不额外请求 0-0 探测，不把浏览器的开放区间拆成小窗口，也不补发应用层 hedge。Worker 不接管这些请求，浏览器直接管理 HTTP 条件响应、连续流和缓存合并；文档管理的控制 API 仍显式转发 Fetch 以兼容 WebKit。HTTP/2、HTTP/3 连接复用、流控、重传与协议回退由浏览器管理。

播放时设置 `preload=auto` 连续预缓冲；拖动视频进度条仅更新显示，提交时才 seek。下一首隐藏音频保留 `preload=none`；已取消无法被浏览器原生 HTTP 缓存复用的 Worker 分块预取，避免同一引言再次下载。图片按 DOM 使用需求加载。所有内容继续使用原文件。

## 显式读取与流式下载

冷启动从头读取先用最多 64 KiB 首段确认权限、长度与强 ETag，同时交付内容。再次打开、跳转或带 If-Range 时先用 0-0 核验。后续窗口按吞吐调整为 64～512 KiB，默认 3 路，低吞吐 2 路、HTTP/3 4 路。输出有序、有背压，预取有界。电子书继续按阅读器所需章节/流块取数。

每个数据窗口携带 If-Range/If-Match，验证 206、总长、区间与 Content-Length。断流保留收到的前缀，仅重取余下区间；文件变更停止旧流。错误区间、错误长度和无法按解码后偏移续传的编码 Range 立即失败，避免重复无效请求。multipart、条件验证及非 ETag 的 If-Range 交给原始 HTTP 请求处理。后端忽略 Range 返回 200 时，将同一个响应流交付消费者，不整文件缓冲或重复下载。

窗口拆成对齐 64 KiB 块，内存上限 8 MiB，磁盘约 128 MiB；同版本并发读取共享在途数据，只在最后一个消费者取消时中断。慢缓存写入和配额不足不阻塞正文。重新认证后才复用块；no-store 不保存内容。任意超大文件不能保证永久缓存。

恢复策略保留请求头/正文停滞截止时间、有界指数退避、Retry-After、备用 HTTPS authority、SHA-256 上传确认与受限 hedge。400/403/404/409/412 等永久错误不循环重试。原生媒体由浏览器负责网络恢复，不套用分块请求规则。少数原生引擎断流后会一直停在 NETWORK_LOADING：共享播放观察器只在已开始播放、缓冲耗尽且时钟停滞后等待 3 秒，再用原生 load 恢复当前位置；同一来源连续失败最多恢复两次，数据继续到达、暂停、跳转、切换来源或解码错误都不会触发此恢复。

## 并发与监控

显式请求与上传共享核心准入策略，播放缓冲不足时限制后台并发；文档与 Worker 接收同一播放压力。原生消费保留浏览器优先级调度。服务端 TCP/QUIC 共用响应许可：登录文件最多 12 路、后台最多 4 路，公开分享保留独立 8 路，许可直到正文结束或取消才释放。

`transportState().metrics` 提供网络请求、重试、应用缓存命中/字节、显式读取回退为原生 Fetch 的请求与网络失败计数。服务端 DEBUG `file_transfer` 日志记录 method、Range、status、等待响应头时间、实际交付正文的字节、耗时和 completed/cancelled/failed。原生 HTTP 缓存与协议通过 Resource Timing 和浏览器/代理测量，避免为监控读取或复制正文。服务端日志中的字节表示交给 HTTP 栈的字节，真实线路字节由代理或浏览器测量。

## 上传

所有非空文件，无论类型和大小，都创建 multipart 会话。默认块大小 1 MiB，最多 10,000 块；超大文件增大块尺寸，空文件保留空对象提交。旧会话保留原先的几何参数。为兼容现有 API 客户端，单块 multipart 仍可 PUT 到会话的 `/data`；新 UI 使用编号块地址。

每个块使用 WebCrypto SHA-256，随 PUT 发送 `X-Content-SHA256`。服务端边写边哈希，在原子发布和数据库确认前验证；响应携带 ETag 与相同 SHA-256，客户端再次核对。失败/停滞仅重试该块，丢失确认时服务器比较已接收内容并保持原 ETag。已经确认的块不会重传。

重新选择文件时先读取持久会话状态，再对本地已确认块计算 SHA-256 与服务端确认比较，避免同名、同大小、同修改时间的不同文件导致错误续传。缺少哈希的旧确认会重新校验该块。成功提交幂等，丢失提交响应不会创建重复文件。浏览器重启后需要重新选择本地文件，以重新取得 File 访问权限。

## HTTP/3 与 HTTP/2 回退

浏览器原生 QUIC 管理拥塞、ACK、重传和协议协商。标准 Fetch API 不允许应用设置 Hy2 Brutal 拥塞控制、UDP FEC 或选择具体 HTTP 版本。本实现提供应用层补偿：并行独立 Range、保留字节前缀、缩短停滞阈值、受限 hedge 与快速局部重连；这些策略也适用于 HTTP/1.1 和 HTTP/2。协议识别使用可用的 Resource Timing `nextHopProtocol`，不可用时使用保守默认值。

默认公网部署由 Revaro 自身直接提供 TCP/UDP 443，浏览器在 UDP 或 HTTP/3 握手不可用时正常回退同一入口的 TCP HTTP/2；显式读取与下载的共享恢复策略适用于所有文件类型；原生媒体由浏览器管理协议与网络恢复。

如需应用层明确选择 HTTP/2，可启用 Revaro 的独立 TCP TLS listener，无需任何反向代理：

```env
APP_DOMAIN=files.example.com
APP_HTTP2_ADDR=0.0.0.0:8444
APP_HTTP2_BASE_URL=https://files.example.com:8444
```

开放额外 TCP 8444。该 listener 复用同一 ACME 证书，仅配置 HTTP/2 与 HTTP/1.1，不发布 Alt-Svc。客户端分别记录主入口与备用入口的失败和成功；主入口停滞或累计两次连接失败时切换备用，保持 60 秒后重新尝试主入口，备用成功不会清掉主入口的失败记录。备用失败则允许返回主入口。配置要求相同 hostname 和不同 HTTPS authority，保留 host-only Cookie；服务端支持凭证 CORS、Priority、Range/校验头和 CSP。标准 Fetch 不能指定同一 authority 的 HTTP 版本；不配置额外端口时由浏览器完成协议回退。

部署与 ACME 生命周期见 [公网入口部署](public-ingress.md)，标准 HTTP/3 拥塞控制见 [QUIC 传输说明](quic-transport.md)。协议依据：[HTTP Range 与 If-Range](https://www.rfc-editor.org/rfc/rfc9110.html#section-14)、[Fetch 标准](https://fetch.spec.whatwg.org/)、[Service Workers](https://www.w3.org/TR/service-workers/)。

## 批量 ZIP 与验证

ZIP 先生成到 `/caches/batch-downloads/` 并原子发布，再使用相同的 Range 引擎下载。票据绑定用户，24 小时闲置有效，可多次续传；服务端进程重启需重新准备票据。生成任务受并发与磁盘预留控制，客户端断开不会解除生成锁或重复覆盖同一个临时文件。过期文件在临时存储清理任务中回收。相比原先即刻输出的 ZIP，首次开始传输前需要完成归档，并占用临时磁盘空间。

```sh
node --test tests/transport/*.test.mjs
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo xtask web-check
```

故障注入覆盖任意 MIME 类型、响应体停滞/局部断流、约 15% 请求失败、成功块跨引擎复用、ETag 变化、hedge 取消、上传确认丢失、SHA-256 发布前验证、取消和备用 authority 切换。请求失败注入不能等价证明 5%～15% UDP/TCP **数据包** 丢失下的实测吞吐；真实 HTTP/3/HTTP/2 协商与丢包性能需要在启用 TLS/QUIC 的部署上进行隔离网络测试。

此前分块恢复策略的 22 项 Node 回归，覆盖冷启动合并首段、按吞吐调整窗口、慢缓存写入、非对齐跳转复用、并发读取独立取消、退出登录期间的延迟探测和缓存读取、前台优先准入、备用入口抢先响应、下一首的缓冲门槛和低缓冲取消。CI 会执行所有传输测试文件，并验证无 Worker 的 API 恢复、keepalive 和上传幂等策略。

`tests/transport/peak-benchmark.mjs` 用共享 1 Mbps、80 ms 延迟、各在途流公平分配带宽的模型对比首段和混合负载；不模拟真实拥塞算法、丢包或浏览器解码。对比基线为 `ca09c35`，[本次模型结果](peak-transport-benchmark.json)：大文件首次交付从 12347 ms 降到 587 ms；12 个后台请求下的前台 2 KiB 读取从 6152 ms 降到 131 ms。此模型只证明调度和首段设计的效果，不能外推为真实晚高峰性能保证。可复现命令：

```sh
git show ca09c35:crates/revaro-web/static/transport-core.js > /tmp/revaro-transport-before.mjs
node tests/transport/peak-benchmark.mjs /tmp/revaro-transport-before.mjs
```

后续已增加可选原生 Quinn HTTP/3 与 `standard` / `aggressive` 拥塞控制，并用隔离 tc netem 实测 0/5/10/15% 包丢失、短暂断流和 UDP 故障回退；参数、完整异常样本与三种浏览器兼容性见 [QUIC 传输说明](quic-transport.md)。浏览器 Worker 由同一 transport core 生成 classic 脚本，支持 Firefox；上传适配器继续 import 原始 ESM core。

2026-10-06 合并版本验证：`cargo xtask check` 覆盖格式、原生 Clippy、467 项 Rust 测试及 WASM 类型检查；9 项 Node 传输故障测试通过。CI 的 Chromium 场景分阶段验证，63 项通过，1 项需要额外大型 FLAC 实例样本而跳过；覆盖真实断流代理、首次公开下载、任意文件类型、批量 ZIP、上传确认丢失、失败块补传、编辑器、阅读器、音频章节、逐行台词和桌面/移动端界面。测试在 BrowserContext 上拦截 Worker 实际请求，所有类型的测试夹具共用分块上传与 SHA-256 校验。本地已有实例同步加载了界面修正，仍为 HTTP localhost，未启用 QUIC 或独立 TLS HTTP/2 入口。

2026-10-08 晚高峰相关改动验证：`cargo xtask check` 通过格式、Clippy、480 项 Rust 测试及 WASM 类型检查；22 项 Node 传输测试通过，发布模式 Web 构建通过。独立 localhost 实例执行 CI 浏览器场景并增加长 WAV/FLAC 跳转场景，共 90 项通过；需要额外导入大型 FLAC 与外部字幕的 1 项跳过。一个选择场景因测试从仓库根目录运行而无法写入截图，按 CI 的 `tests/e2e` 工作目录重跑后通过。此验证未测量真实公网晚高峰线路。

2026-10-10 文件访问重构验证：500 项 Rust、38 项 Node、40 项相关浏览器测试全部通过，格式、Clippy、WASM 检查与发布 Web 构建通过。原生媒体和图片直接使用浏览器 HTTP；恢复式下载与按需阅读继续复用共享核心。分析、逐项验收、12.16 Mbps 视频实播与断流、长音频跳转、随机并发读取及缓存结果见 [重构记录](file-transport-refactor.md) 和 [JSON 测量](file-transport-measurements.json)。此次使用隔离 localhost 实例与 HTTP 代理，WebKit 为 Linux WPE 引擎；未把该测量外推为公网 QUIC 或实际 Safari 的性能结果。

## 播放进度与多设备会话

音视频进度携带服务器 revision、唯一 writer、会话内 sequence 和明确 completed 标记。相同 writer 的旧序号返回实际已保存结果；新 writer 只能以已观察到的版本取得写入权。音乐队列和进度在同一 SQLite 事务中提交，旧设备恢复连接时无法覆盖其他设备接管的队列或位置。队列也可独立提交，使读取历史期间的结束播放不依赖尚未初始化的媒体时钟。

待提交写入先按账号记入浏览器 localStorage，网络失败按原始版本重试，刷新后恢复；409 冲突终止旧会话的重试，不把陈旧记录重新包装成新版本。成功确认仅删除已确认序号及更旧的记录。账号切换清除内存重试任务，保留各账号自己的磁盘记录。后台页面暂停执行或设备离线期间无法即时接收其他设备状态，恢复在线、前台和定期轮询后同步；清除浏览器存储会删除该设备尚未提交的记录。

`progress-sync.spec.ts` 使用独立浏览器上下文验证接管、原生时钟瞬时归零、显式零进度、末尾未完成恢复、失败写入刷新恢复、确认丢失幂等重试、延迟历史期间结束播放，以及完整队列与播放模式跨设备恢复。CI 在 Chromium、Firefox、WebKit 上执行这些场景。

CI 为原生播放测试启动 PulseAudio 虚拟输出，避免无音频设备的 Linux runner 导致 Firefox `NS_ERROR_DOM_MEDIA_MEDIASINK_ERR`；仍通过真实媒体解码与播放时钟验证进度。[PulseAudio null sink 文档](https://wiki.freedesktop.org/www/Software/PulseAudio/Documentation/User/Modules/#module-null-sink)说明该输出使用系统时钟。

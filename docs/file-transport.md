# 文件传输与国际线路恢复

传输策略按请求大小和线路状态生效，不按文件扩展名或 MIME 类型分支。

| 入口 | 统一实现 |
| --- | --- |
| 任意原文件下载、可预览文件、公开链接 `/s/{token}` | Service Worker → `transport-core.js`；服务端 `transfer::serve_reader` |
| 缩略图、书籍封面/附件、阅读流/章节 | 同一 Worker；服务端 `transfer::serve_bytes` |
| 编辑器内容、历史版本 | 同一 Worker；不可变内容使用 Range/ETag |
| 文件信息、目录列表、播放会话与进度等可变 API | 文档内 `transport-core.js` 缓冲请求；不依赖 Worker 存活 |
| 批量 ZIP | 用户绑定票据；一次生成磁盘缓存文件，复用共享 Range 响应 |
| 所有非空文件上传 | UI 仅选择/切片/展示；`transport::put_blob` → 同一个 JS 核心 |
| 上传创建、状态和提交 | 文档内共享核心统一恢复；创建必须有幂等键，提交可安全重复 |

应用挂载前先安装文档内 API 恢复层，同时激活 Service Worker，让原生 `<img>`、音视频、文件读取和下载链接经过共享核心。可变 API 直接在文档中运行同一缓冲恢复策略，避免 Worker 休眠、更换或浏览器多上下文故障阻塞目录和进度同步。Worker 注册失败或 8 秒仍未接管时，应用继续启动，显式文件 Fetch 使用文档内 Range 核心，原生媒体使用服务器原生 Range。下载链接使用普通导航并由 Content-Disposition 启动保存，避免 Chromium 的 download 属性绕过 Worker。公开链接首次访问也先加载无需登录的轻量引导页，激活共享层后再交付原文件；公开字节响应保留强 ETag，同时维持 no-store。完整 Worker 功能需现代浏览器、HTTPS 或 localhost；Worker 不可用时保留上述降级路径。支持标准 Service Worker、Fetch、ReadableStream、AbortController、WebCrypto，不依赖私有浏览器扩展。

下载流预留一个块的队列容量，确保浏览器原生附件导航开始消费响应；Worker 的 fetch 事件保持到流结束或取消，避免只发送响应头便结束任务。

## 下载与读取

首次从头读取时，用最多 64 KiB 的首段同时确认访问权限、完整长度、强 ETag 并交付数据；小文件一次请求即可完成。再次打开、跳转或带 If-Range 的请求先用 `Range: bytes=0-0` 重新核验权限和版本。可变目录列表、播放进度等元数据在同一个缓冲请求中原子读取，避免拼接不同快照。后续 Range 窗口按近期吞吐动态调整为 64～512 KiB，目标约半秒数据；默认 3 路，低吞吐用 2 路，HTTP/3 用 4 路。输出按原字节顺序交付，预取有界，不将整个大文件拼成 Blob。

每块都发送 `If-Range` 与 `If-Match`，严格验证 206、Content-Range、总长度和 ETag。连接中断保留该块已收到的前缀，下次只请求剩余区间。成功块不因其他块失败而重新下载。文件版本改变时，在输出首块前可重新探测；输出开始后停止旧流，避免拼接不同版本的数据。

网络窗口拆成对齐的 64 KiB 缓存块，使相邻跳转、预取和正式播放复用同一块。同版本同 URL 的并发读取共享在途请求；仅当所有读取者都取消时才中断该请求。内存缓存上限 8 MiB，CacheStorage 最多 2048 项（包含少量资源描述，内容约 128 MiB）；磁盘写入异步执行，待写数据上限 8 MiB，慢写入与配额不足不阻塞内容交付。重新打开先重新认证/核验 ETag；`no-store` 不保存内容和描述，退出登录清空缓存。缓存有界，不能保证任意超大文件长期保存。

请求头等待默认 15 秒；配置备用入口后，主入口默认请求最多等 3 秒再恢复，上传提交保留独立的长超时。响应体通常 8 秒无进展会重连；配置备用入口后的正在播放媒体缩短至 2.5 秒。HTTP/3 下对应阈值再乘 0.65。20 秒内不足 8 KiB 也触发恢复。每次恢复最多尝试 10 次，400 ms 起指数退避，50%～100% 随机抖动，上限 8 秒，尊重最多 60 秒 Retry-After。403/404/409/412 等永久失败不循环重试。显式取消中断请求、排队和退避。请求和读流的截止时间由共享核心自行结束，不依赖浏览器及时完成 Fetch、read 或 cancel；原生取消失效也不会卡住重试或保留准入名额。

前台首段和 ≤256 KiB 的前台窗口可在 1.2 秒后补发一次请求，HTTP/3 下约 720 ms；有备用入口时优先在那里补发。完整且验证成功的响应获胜，再取消慢请求。每个上下文最多 2 次并发补发；播放缓冲不足、有排队请求、缩略图和批量下载时关闭补发，控制拥塞时的额外流量。

## 播放与混合负载

客户端共享准入队列按播放、前台读取、缩略图、后台下载/上传排序，同时发送标准 `Priority` 请求头。通常最多 12 路，HTTP/3 最多 16 路，后台至少留出 4 路前台容量。正在播放的音视频缓冲不足 15 秒时，总上限降至 8，后台最多 2 路，后台每个流只预取 1 个窗口；已在途请求正常完成，后续请求让出容量。上传适配器与 Worker 分别拥有队列，同时接收播放状态。

服务端对登录后的文件 GET/HEAD 最多保留 12 个响应体，其中后台最多 4 个；许可直到正文结束或连接取消才释放。公开分享继续使用原有独立的 8 路配额和超额 429，上传继续使用原有服务端限制。这是并发与排队控制，不能替代线路带宽或协议层拥塞控制。

当前音视频开始播放后切换为 `preload=auto`。下一首的隐藏音频保持 `preload=none`，由 Worker 在当前缓冲达到 20 秒时预取下一首最多 256 KiB，同时最多预取两首；缓冲不足、暂停、跳转或队列改变会取消预取。顺序和列表循环预取确定的下一首，随机播放不猜测下一首，单曲循环复用当前缓存。预取只去掉内部优先级参数后作为缓存键，正式播放仍先核验权限和 ETag。所有内容继续使用原文件，不生成预览尺寸或低码率副本。

## 上传

所有非空文件，无论类型和大小，都创建 multipart 会话。默认块大小 1 MiB，最多 10,000 块；超大文件增大块尺寸，空文件保留空对象提交。旧会话保留原先的几何参数。为兼容现有 API 客户端，单块 multipart 仍可 PUT 到会话的 `/data`；新 UI 使用编号块地址。

每个块使用 WebCrypto SHA-256，随 PUT 发送 `X-Content-SHA256`。服务端边写边哈希，在原子发布和数据库确认前验证；响应携带 ETag 与相同 SHA-256，客户端再次核对。失败/停滞仅重试该块，丢失确认时服务器比较已接收内容并保持原 ETag。已经确认的块不会重传。

重新选择文件时先读取持久会话状态，再对本地已确认块计算 SHA-256 与服务端确认比较，避免同名、同大小、同修改时间的不同文件导致错误续传。缺少哈希的旧确认会重新校验该块。成功提交幂等，丢失提交响应不会创建重复文件。浏览器重启后需要重新选择本地文件，以重新取得 File 访问权限。

## HTTP/3 与 HTTP/2 回退

浏览器原生 QUIC 管理拥塞、ACK、重传和协议协商。标准 Fetch API 不允许应用设置 Hy2 Brutal 拥塞控制、UDP FEC 或选择具体 HTTP 版本。本实现提供应用层补偿：并行独立 Range、保留字节前缀、缩短停滞阈值、受限 hedge 与快速局部重连；这些策略也适用于 HTTP/1.1 和 HTTP/2。协议识别使用可用的 Resource Timing `nextHopProtocol`，不可用时使用保守默认值。

默认公网部署由 Revaro 自身直接提供 TCP/UDP 443，浏览器在 UDP 或 HTTP/3 握手不可用时正常回退同一入口的 TCP HTTP/2；共享 transport 的 Range、重试与停滞恢复继续适用于所有文件类型。

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

晚高峰相关策略还有 22 项 Node 回归，覆盖冷启动合并首段、按吞吐调整窗口、慢缓存写入、非对齐跳转复用、并发读取独立取消、退出登录期间的延迟探测和缓存读取、前台优先准入、备用入口抢先响应、下一首的缓冲门槛和低缓冲取消。CI 会执行所有传输测试文件，并验证无 Worker 的 API 恢复、keepalive 和上传幂等策略。

`tests/transport/peak-benchmark.mjs` 用共享 1 Mbps、80 ms 延迟、各在途流公平分配带宽的模型对比首段和混合负载；不模拟真实拥塞算法、丢包或浏览器解码。对比基线为 `ca09c35`，[本次模型结果](peak-transport-benchmark.json)：大文件首次交付从 12347 ms 降到 587 ms；12 个后台请求下的前台 2 KiB 读取从 6152 ms 降到 131 ms。此模型只证明调度和首段设计的效果，不能外推为真实晚高峰性能保证。可复现命令：

```sh
git show ca09c35:crates/revaro-web/static/transport-core.js > /tmp/revaro-transport-before.mjs
node tests/transport/peak-benchmark.mjs /tmp/revaro-transport-before.mjs
```

后续已增加可选原生 Quinn HTTP/3 与 `standard` / `aggressive` 拥塞控制，并用隔离 tc netem 实测 0/5/10/15% 包丢失、短暂断流和 UDP 故障回退；参数、完整异常样本与三种浏览器兼容性见 [QUIC 传输说明](quic-transport.md)。浏览器 Worker 由同一 transport core 生成 classic 脚本，支持 Firefox；上传适配器继续 import 原始 ESM core。

2026-10-06 合并版本验证：`cargo xtask check` 覆盖格式、原生 Clippy、467 项 Rust 测试及 WASM 类型检查；9 项 Node 传输故障测试通过。CI 的 Chromium 场景分阶段验证，63 项通过，1 项需要额外大型 FLAC 实例样本而跳过；覆盖真实断流代理、首次公开下载、任意文件类型、批量 ZIP、上传确认丢失、失败块补传、编辑器、阅读器、音频章节、逐行台词和桌面/移动端界面。测试在 BrowserContext 上拦截 Worker 实际请求，所有类型的测试夹具共用分块上传与 SHA-256 校验。本地已有实例同步加载了界面修正，仍为 HTTP localhost，未启用 QUIC 或独立 TLS HTTP/2 入口。

2026-10-08 晚高峰相关改动验证：`cargo xtask check` 通过格式、Clippy、480 项 Rust 测试及 WASM 类型检查；22 项 Node 传输测试通过，发布模式 Web 构建通过。独立 localhost 实例执行 CI 浏览器场景并增加长 WAV/FLAC 跳转场景，共 90 项通过；需要额外导入大型 FLAC 与外部字幕的 1 项跳过。一个选择场景因测试从仓库根目录运行而无法写入截图，按 CI 的 `tests/e2e` 工作目录重跑后通过。此验证未测量真实公网晚高峰线路。

## 播放进度与多设备会话

音视频进度携带服务器 revision、唯一 writer、会话内 sequence 和明确 completed 标记。相同 writer 的旧序号返回实际已保存结果；新 writer 只能以已观察到的版本取得写入权。音乐队列和进度在同一 SQLite 事务中提交，旧设备恢复连接时无法覆盖其他设备接管的队列或位置。队列也可独立提交，使读取历史期间的结束播放不依赖尚未初始化的媒体时钟。

待提交写入先按账号记入浏览器 localStorage，网络失败按原始版本重试，刷新后恢复；409 冲突终止旧会话的重试，不把陈旧记录重新包装成新版本。成功确认仅删除已确认序号及更旧的记录。账号切换清除内存重试任务，保留各账号自己的磁盘记录。后台页面暂停执行或设备离线期间无法即时接收其他设备状态，恢复在线、前台和定期轮询后同步；清除浏览器存储会删除该设备尚未提交的记录。

`progress-sync.spec.ts` 使用独立浏览器上下文验证接管、原生时钟瞬时归零、显式零进度、末尾未完成恢复、失败写入刷新恢复、确认丢失幂等重试、延迟历史期间结束播放，以及完整队列与播放模式跨设备恢复。CI 在 Chromium、Firefox、WebKit 上执行这些场景。

# 文件访问与流式传输重构（2026-10-10）

## 当前路径与问题

| 内容 | 浏览器读取 | 服务端字节来源 | 当前问题 |
| --- | --- | --- | --- |
| 音视频 | 原生 audio/video → Worker → fileResponse | serve_file → LocalStore → serve_reader | 原生连续 Range 被拆成 64～512 KiB 请求；每次 seek 另发 0-0 探测；吞吐按所有文件的近期请求估算，高码率视频也受小窗口限制 |
| 图片、封面 | 原生 img → 同一 Worker | 原文件或 serve_bytes | 小图片额外探测与分块；绕开浏览器自己的 HTTP 缓存 |
| EPUB/TXT | 按章节/流块 fetch，附件用 img | 解析/派生缓存 → serve_bytes | 按需读取合理；读取策略、Worker 与文档重复列出文件路由 |
| 编辑器、历史版本 | 显式 fetch | 有界 JSON → file_resources → serve_bytes | middleware 对所有 /api/files GET 都缓冲和哈希，包含可变列表、进度等控制 API |
| 普通文件、公开分享、ZIP | 下载导航 → Worker | 原文件/磁盘归档 → serve_reader | 有界恢复与续传已有，但 Range 不支持的后端可能被整文件缓冲；须保留下载的取消和背压 |
| 上传 | XHR 进度适配 → transport-core | multipart 原子提交 | 与读取共用重试、排队；保留哈希、幂等与断点续传 |

其他风险：LocalStore.open_object 在打开文件前取 size/ETag，原子替换并发时可能描述另一个句柄；HTTP 层虽有 ReadSeek，原文件路由仍直接依赖本地 File；缺少统一正文生命周期监控。现有测试主要证明分块恢复，不能证明高码率视频的实际连续播放性能。

## 统一方案

统一文件访问接口与 HTTP 契约，不要求每一种消费方式使用相同的分块策略。

- 服务端：FileAccess.open 返回同一版本的 reader/size/ETag；所有原文件、派生资源、历史内容与归档继续共用 serve_reader/serve_bytes。取消时丢弃正文释放读句柄和许可；读流只随下游需求读取。存储提供者负责按偏移读取，不在 HTTP 层暴露路径。
- 客户端：transport-core 维护唯一资源路由与策略选择。原生 audio/video/img 请求绕过 Worker，直接交给浏览器，不探测、不应用层切片、不补发 hedge。浏览器管理 Range、HTTP 缓存、连接复用与 HTTP/2、HTTP/3 协商；播放时 preload=auto 连续预缓冲。
- 原生媒体异常恢复仍调用浏览器 load/play：仅在开始播放后缓冲耗尽、时钟停滞且没有继续收到数据时恢复当前位置，同一来源连续失败最多两次。共享观察器保留暂停、跳转和来源切换的用户意图，不把正常预缓冲或解码错误当成可重试网络请求。
- 显式读取、阅读器与下载保留共享 Range 恢复、认证后缓存、并发请求合并、取消与有限预取。元数据与控制写入仍使用原子的 bufferedRequest；非幂等写入不自动重放。
- 现有 Worker 仅保留显式读取/下载的缓存与恢复职责、以及文档请求的必要原生转发，不新增 Worker/协议。取消无法被原生播放复用的 Worker 下一首分块预取，避免重复下载。
- 监控：共享客户端请求/重试/缓存计数，原生内容由浏览器 Resource Timing 与统一服务端正文日志观测；状态、字节数、完成/取消、正文耗时均基于实际消费。

协议契约遵循 [RFC 9110](https://www.rfc-editor.org/rfc/rfc9110.html#section-14)；native 策略由浏览器直接处理 200/206/304/416、If-Range、ETag 与 multipart，恢复策略必须验证区间、总长和强校验器，不能拼接不同版本。

## 分阶段落地与验收

1. **恢复原生媒体能力**：合并客户端路由，移除原生媒体/图片的主动切片与无效预取；保留共享恢复策略。测试大响应不缓冲、单个浏览器请求不放大、原始条件头/取消、真实高码率播放与跳转。
2. **存储与服务端契约**：引入可替换读取提供者、句柄元数据一致性、正文生命周期监控；限制派生资源 middleware 范围。用替代内存后端与大对象验证 seek、200/206/416、许可释放与背压。
3. **恢复路径校正**：审查 Range 忽略/多区间/条件请求、无效响应重试、缓存失效及权限；确认普通下载、阅读器按需读取、上传续传与播放/阅读进度保持。
4. **性能和回归验收**：覆盖随机跳转、高码率视频、并发读取、弱网恢复、缓存命中，记录请求数、首字节、实际传输量、重复字节、缓冲/播放时钟与取消。运行 Node、相关 Rust 和真实浏览器回归。只有每项要求都有当前证据后才完成目标。

原有 file-transport.md 中的分块说明仅适用于恢复策略。此前 QUIC/网络模型数据保留，但不用于证明本次原生播放改动。

## 逐项验收依据

| 要求 | 落地内容 | 验证入口 |
| --- | --- | --- |
| 1. 读取路径审查 | 上述入口表明确原文件、派生资源、可变控制 API 与消费策略；移除重复路由表达式和无效预取 | 本文分析；transport-core 的 isFileResource/usesNativeFileLoading |
| 2. 统一访问与 HTTP 流 | FileAccess、同句柄元数据、共用 serve_reader/serve_bytes；64 KiB 读缓冲；完整 200 限定版本长度；416/ETag 和条件响应一致 | file_access.rs 的非本地提供者测试；transfer.rs 的 8 GiB 偏移、背压、取消和生成资源测试；file_routes 协议测试 |
| 3. 鉴权、缓存、调度、错误与监控 | 私有资源每次复用重新验证；恢复路径共享准入/错误处理/缓存；服务端统一正文许可与生命周期日志，原生资源不用监控性重取 | Node 并发、缓存失效、永久错误、许可清理测试；浏览器图片会话失效测试；DEBUG file_transfer 记录 |
| 4. 浏览器原生能力 | 原生媒体/图片的 Worker fetch 事件不 respondWith；沿用浏览器 HTTP 和现有 TCP/QUIC 入口 | native-loading 的 32 GiB 未缓冲响应测试；playback-prefetch 的零 Worker 请求断言；真实浏览器播放 |
| 5. 保留消费策略 | 音视频 preload=auto；图片按 DOM 加载；电子书按章节/流块；下载为有界流 | native-file-loading、audio-range-seeking、rust-reader-ui、file-transport 浏览器测试 |
| 6. 视频频繁请求与缓冲 | 移除媒体分块、0-0 探测和 hedge；拖动不提交 seek；原生断流停滞有限恢复当前位置 | 12.16 Mbps VP9 实播和三次跳转；1 MiB 后断流；native-recovery 的取消意图与重试预算测试 |
| 7. 后端扩展与既有能力 | 原文件读提供者可替换；现有本地写入/解析/派生流程保留；进度与上传确认语义保留 | 内存提供者经原文件交付入口；三引擎 progress-sync；阅读恢复、下载 SHA-256 与上传确认丢失测试 |
| 8. 性能测试 | 8 GiB 随机/并发读取、32 GiB 原生流、真实高码率/长音频、代理断流及缓存验证纳入 CI | tests/transport/*.test.mjs；CI 三引擎 native-file-loading；[测量记录](file-transport-measurements.json) |

未来远程后端可以用原生 Range 实现 AsyncRead/AsyncSeek 并返回稳定的强 ETag。此次替换点覆盖原文件读取；上传、解析引擎与派生文件生成仍使用 LocalStore，扩展时需另行提供对应生命周期实现。

浏览器允许为媒体索引、跳转或恢复发出短区间、重叠区间。验收约束是取消应用层额外的媒体探测、切片和重复预取，并保持真实播放推进；不强行改写浏览器合法请求。WPE WebKit 的私有图片可重新取回 200，Chromium/Firefox 的条件缓存复用单独验证；三种路径均必须拒绝失效会话。

## 性能测量

四个阶段已完成：`cargo xtask check` 通过格式、Clippy、500 项 Rust 测试和 WASM 检查；38 项 Node 测试通过，发布 Web 构建通过。当前构建的相关浏览器回归共 40 项通过（Chromium 18、Firefox 11、WPE WebKit 11），无失败、跳过或重试后通过项；覆盖原生加载、断流恢复、图片鉴权、长音频跳转、下载/上传续传、无 Worker 分享、电子书阅读与多设备进度。没有把未执行的完整浏览器 CI 套件计入此结果。

8 GiB 虚拟对象执行 16 个随机块的 64 次重叠并发读取，再进行 16 次缓存读取。一次断流发生在 32 KiB 前缀之后，只重取剩余部分。共 97 个请求、1,048,656 字节（1 MiB 数据及 80 字节鉴权探测），后台最高并发 2，暖缓存命中 16 次，没有重传已成功的数据。此测试使用 4 ms 响应延迟模型，不模拟真实 TCP/QUIC 拥塞。

原生视频为实际解码的 16 秒 VP9/WebM，24,313,565 字节、平均 12.16 Mbps；代理每流限速 4 MiB/s，首块额外延迟 60 ms。每个场景验证正常播放、仅预览拖动不跳转、三次前后跳转、恢复播放和退出。断流场景在一次响应交付约 1 MiB 后关闭连接。

| 引擎 | 正常 / 断流请求数 | 正常 / 断流播放到第 3 秒耗时 | 应用额外 0-0 探测 |
| --- | --- | --- | --- |
| Chromium 154 | 7 / 8 | 3.634 / 3.608 秒 | 0 |
| Firefox 141 | 6 / 7 | 4.097 / 4.081 秒 | 0 |
| WPE WebKit 26 | 7 / 13 | 3.833 / 10.171 秒 | 0 |

“播放到第 3 秒”包含真实播放的三秒，不是首帧延迟。WebKit 断流样本包含原生加载停滞后的有限恢复等待。长 WAV/FLAC 场景在下载完整文件之前跳转至 2400 秒，再返回 100 秒，分别仅交付约 1.16 MB / 1.53 MB，而原文件约 57.68 MB / 58.24 MB。

完整请求数、字节数、响应头与首字节时间、取消数量、缓存及验收结果见 [JSON 测量记录](file-transport-measurements.json)。代理测量的是交付的 HTTP 正文字节，不是网络确认或 TLS 开销；这些 localhost 结果不证明公网晚高峰吞吐、HTTP/3 协商或 macOS/iOS Safari 性能。

复现时使用独立数据目录的实例，设置 `E2E_BASE_URL` 指向它；浏览器和 FFmpeg 按 CI 安装。测试报告保留每个原生场景的 JSON 附件。

```sh
cargo xtask check
node --test tests/transport/*.test.mjs
cargo xtask web-build
cd tests/e2e
npx playwright test native-file-loading.spec.ts audio-range-seeking.spec.ts file-transport.spec.ts progress-sync.spec.ts rust-reader-ui.spec.ts
E2E_BROWSER=firefox npx playwright test native-file-loading.spec.ts progress-sync.spec.ts
E2E_BROWSER=webkit npx playwright test native-file-loading.spec.ts progress-sync.spec.ts
```

# Revaro 公网 QUIC / HTTP/3 性能验收

2026-10-10，入口 `https://revaro-2.0721.ing`，实测覆盖北京时间约 **20:40–21:18**。晚高峰按北京时间 **18:00–24:00**；本次所有性能样本均在这一窗口内采集，逐次 UTC / 北京时间见结果 JSON。本次结论是：**公网 H3 协商和 Range 正确性通过；当前 H3 高码率播放、随机跳转和断网恢复未通过，仍需要针对性优化。**

本次实际优化仅为网关防火墙补放 UDP 443。Nginx 转发配置、Revaro 容器、QUIC 功能与拥塞控制配置均保留。没有部署新架构或把此前 localhost 数据加入公网对比。

## 已核实的部署链路和修复

```mermaid
flowchart LR
    C[公网测试客户端] -->|TLS over TCP / H2| G[62.84.164.14\nNginx stream 四层转发]
    C -->|QUIC over UDP / H3| G
    G -->|转发原 TCP 字节或 UDP 数据报| R[88.214.26.196:443\nRevaro 原生 TLS / QUIC 入口]
```

网关 Nginx 1.26.3 的 `/etc/nginx/stream.d/revaro.conf`：TCP 443 → 后端 TCP 443；`listen 443 udp reuseport` → 后端 UDP 443。TCP / UDP 空闲超时分别为 1 小时 / 10 分钟。没有配置 stream 限速；不是 HTTP `proxy_pass` 链路，故 HTTP `proxy_buffering` 和 HTTP 分块大小在此网关不适用。stream 默认的 16 KiB 读缓冲用于转发，不等待整份文件。[Nginx stream 官方文档](https://nginx.org/en/docs/stream/ngx_stream_proxy_module.html)

后端实际 TCP/UDP 443 均由 `/usr/local/bin/revaro`，PID 29172 监听。容器为 `ghcr.io/vesperglow/revaro:latest`，启动日志为 `mode=Aggressive target_mbps=None max_mbps=250 global_max_mbps=1000`。因此 QUIC 是客户端到 Revaro 的端到端连接，经过网关 UDP 转发；网关没有终止 QUIC，也没有将 H3 转成 HTTP/1.1 上游。后端拥塞控制日志能够对应到网关转发出的 UDP 端口，确认原生控制器确实参与了公网传输。

修复前响应已广告 `Alt-Svc: h3=":443"; ma=300`，但严格 H3 三次握手均超时；自动模式实际回退为 H2。同一客户端访问公网 H3 对照站成功，直连后端公网地址也成功。网关 nftables 的 input 默认 drop，只允许 TCP 22/80/443，缺少 UDP 443 放行，已证实为协商失败原因。

2026-10-10 **20:59:46 北京时间**：向运行规则追加 `inet filter input udp dport 443 accept`，同时写入 `/etc/nftables.conf`，语法检查通过；没有重载整个防火墙规则集。原文件备份：`/etc/nftables.conf.revaro-quic-20261010T125946Z.bak`。随后严格 curl H3 返回 200，证书验证成功；Chromium 154 通过 Alt-Svc 自然从 H2 切至 H3，未使用强制 QUIC 或忽略证书参数。[curl 严格 H3 与自动回退说明](https://curl.se/docs/http3.html)

## 测量方法与边界

- 当前客户端公网出口为 `157.254.234.140`；所在地、运营商及住宅/移动网络属性未核实。使用真实公网 DNS / TLS / TCP / UDP 路径，不使用 localhost、SSH 数据隧道或本地假代理替代。
- 下载使用现有 1,554,290,405 字节文件；H2 / H3 各 3 次 8 MiB 有界 Range、各 8 次固定种子的随机 256 KiB Range，以及有意限时取消的连续开放 Range。每次 curl 新建连接，交替协议顺序；吞吐按实际接收正文 / 完整请求耗时计算，包含连接建立和首字节等待。
- 视频为现有 52,679,755 字节 MP4：H.264 / AAC、1280×720、29.443 秒，平均 **14.31 Mbps**。每个条件使用全新 Chromium 配置和空 Worker 缓存；关闭浏览器 HTTP 缓存，先完成同样的协议预热，再测 `src` 设置到首个解码帧。该首帧值不含登录、页面启动或首次 Alt-Svc 学习耗时。
- `deployed` 条件从公网加载实际部署的 transport-client / Worker，然后使用受控原生 video 元素。`native-control` 只在测试浏览器内绕过 Worker。均读取同一实际文件；没有加载完整应用 UI，也没有写入播放进度、上传或删除用户文件。
- 每个协议与加载条件，主测试各 2 次播放/跳转、2 次断网恢复；先播放 8 秒，再依次跳转到时长的 81%、22%、64%、41%，记录 seeked 与新解码帧。失败和超时保留，不只统计成功请求。
- 页面与 Worker 的 CDP 同时观察；只统计真正到网络的请求，排除 Worker 合成响应及未触网的页面取消事件。传输字节来自实际 dataReceived，取消请求接收到的前缀也计入；不用 Content-Length 代替收到的字节。
- 断网恢复是在真实公网访问中模拟浏览器离线 **3 秒**，恢复后须连续推进播放时钟至少 **5 秒**，观察上限 20 秒。它不是实测移动切网或固定丢包率；短暂推进时钟后再次卡住不会算恢复成功。
- 现网 Worker 的两协议均观察到 `ERR_INTERNET_DISCONNECTED`，确认请求受到故障注入。原生 H3 对照只有取消记录，无法独立确认活跃 QUIC 流被中断，其“断网恢复”因果结论标为未测。现网 H3 在断网前也已处于低缓冲状态，表中反映的是相同启动时序下的播放恢复表现，不是排除播放瓶颈后的纯重连延迟。

## HTTP/2 与 HTTP/3 对比

以下为修复后的公网入口，主视频条件为**现网 Worker**。

| 指标 | HTTP/2 | HTTP/3 |
| --- | ---: | ---: |
| 严格协议协商 | 3/3 成功 | 3/3 成功 |
| 8 MiB Range 吞吐中位数，各 3 次 | 36.28 Mbps | 14.17 Mbps |
| 同组 Range 首字节中位数 | 443 ms | 314 ms |
| 随机 256 KiB Range 首字节中位数，各 8 次 | 434 ms | 304 ms |
| 随机 Range 完整请求中位数 | 1,039 ms | 856 ms |
| 初始 8 秒开放 Range 的接收吞吐 | 162.38 Mbps | 18.22 Mbps |
| 视频首个解码帧中位数，各 2 次 | 1.85 秒 | 2.54 秒 |
| 四次跳转流程完成 | 2/2 | 1/2；另一轮跳转超过 20 秒 |
| 已完成跳转的新解码帧中位数 | 167 ms，8 次 | 9,195 ms，5 次 |
| 已完成跳转最大延迟 | 824 ms | 12,046 ms；另有超时，未纳入该最大值 |
| 整个播放/跳转流程网络请求数 | 155 / 158 | 317 / 334 |
| 正常播放 waiting 通知 | 0 / 0 | 一轮未完整记录；另一轮 7 次 |
| 3 秒断网后持续恢复 | 2/2；恢复后约 9.0 / 9.8 秒开始持续播放 | 0/2；20 秒内未观察到连续 5 秒播放 |
| 断网测试 waiting 通知 | 3 / 3 | 4 / 4 |

H3 完成的随机小范围请求更快，但这不代表高码率播放更好。H3 只有 5/8 个计划跳转产生了成功帧，其余包括超时后的未执行项，成功跳转中位数不能代表失败样本。H3 流程耗时更长且一轮未完成，流程总请求数不是相同时间或相同完成字节量下的效率比较。

为比较相同观察窗口，另各做一次精确记录：

| 播放开始后 8 秒观察窗口，现网 Worker | HTTP/2 | HTTP/3 |
| --- | ---: | ---: |
| 播放时钟前进 | 7.98 秒 | 2.66 秒 |
| 缓冲次数 / 时间 | 0 / 0 秒 | 3 / 5.34 秒 |
| 从启动至窗口结束的实际网络请求数 | 144 | 113 |
| 同窗口接收正文 | 47.44 MB | 6.83 MB |

H3 的窗口请求更少是伴随接收和播放进度下降出现的，不能算请求优化。窗口从 play Promise 兑现且首帧已解码后等待 8 秒；字节/请求包含此前启动读取。此表每格仅一次补充样本；完整主测试和失败记录均保留。

连续 **30 秒** Range 各补测一次：[原始记录](../tests/quic/results/2026-10-10-public/continuous-30s.json)。H2 收到 **512.56 MB**，请求平均 **136.61 Mbps**，第 5–24 秒吞吐中位数 **148.83 Mbps**，最大正文间隔 **0.28 秒**；H3 收到 **87.23 MB**，平均 **23.25 Mbps**，同期中位数 **25.41 Mbps**，最大正文间隔 **1.51 秒**。均实际增量接收，按计划取消，未宣称整文件下载完成。H3 长流平均吞吐高于该视频平均码率，但其短时停顿、媒体实际读取和跳转仍不能通过验收，不能只用长流平均值替代播放结果。

6 个有界 Range 与 16 个随机 Range 的 206、Content-Range、正文长度全部正确。同一范围的跨协议 SHA-256 一致。这证明本次抽样范围的协议与数据一致性，不等同于完整大文件的独立源文件校验。

## 分块、重复读取与 Nginx 瓶颈

公网 core / Worker 的哈希与后端容器文件一致，仍是旧分块版本，未包含工作区上一轮原生媒体绕过改动。主测试现网 Worker 的 H3 请求有约 68%–69% 的有界范围不大于 64 KiB；补充失败样本中为 308/314 个请求。范围大小含 1 字节探测，不能把每个小请求都直接当错误，但数量和频繁回退到小窗口值得优化。

相同 Range 被再次发出，可能包括取消后的重试及探测；它不等于正文被完整重复下载。按实际接收前缀与 Content-Range 位置求并集，主测试现网 Worker 的重复正文约为：H2 每轮 **0.42–1.31 MB**，H3 每轮 **0.50–0.92 MB**。这里不统计 QUIC / TCP 内部重传。

原生加载对照的 H2 首帧约 1.50 秒、请求数 30/55，少于现网 Worker；但实际接收约 **90.3/97.0 MB**，其中约 **37.6/44.3 MB** 为已收到偏移的重复正文。该文件上，减少应用分块请求并没有同时降低字节量。关闭浏览器 HTTP 缓存、MP4 文件布局和浏览器媒体缓冲策略都会限制该对照的外推，不能据此保证原生改动对所有媒体更快。

原生 H3 主测试两轮都发生跳转超时；模拟离线后的观察窗口也未连续播放，但因活跃流中断未得到独立确认，原生 H3 的故障恢复因果结论为**未测**。补充一次原生 H3 完成了跳转，但最初 8 秒仍有一次约 2.45 秒缓冲。故 H3 正常播放/跳转问题也存在于绕过 Worker 的路径，单纯部署原生加载改动不足以证明问题解决。

对公网后端 `88.214.26.196` 直连时，保留域名、SNI、认证和证书校验：H2 / H3 各 3 次 8 MiB 中位数 **36.25 / 13.95 Mbps**，8 秒连续 Range 为 **121.71 / 18.87 Mbps**。没有观察到绕过网关就消除 H3 低吞吐的现象。两台机器读取时的 UDP InErrors / RcvbufErrors / SndbufErrors 均为 0，Nginx 也没有配置 HTTP 响应缓冲或 stream 限速；目前没有依据扩大代理缓冲。

后端日志确认 Aggressive 自动目标实际变化，并出现 RTT 从约 145 ms 升到 500–1,300 ms、部分连接触发 Cubic 回退。统计包含正常播放和离线故障注入期间的连接；不能把累计 lost_bytes 或最近一轮 loss_ppm 当作某段物理网络的实际丢包率。现有证据支持继续核查 QUIC 容量估计、排队、长流取消/重新读取与公网 UDP 路径，尚不足以确定唯一原因或保证某个参数修改有效。

另有实际 MTU 线索：网关 eth0 的 MTU 为 1500，但到本测试出口的路由缓存显示 **MTU 1280**；两次采集之间系统 IPv4 FragOKs 从 61,760 增至 106,986，增加 **45,226**，FragCreates 相应增加 90,452。UDP 缓冲错误仍为 0。它提示本出口路径存在分片负担，但这些是系统级计数，没有逐流抓包或 MTU 上限对照，不能据此断言全部分片属于本次 QUIC，或将其判定为唯一瓶颈。Nginx UDP 中转两侧的有效 MTU 应列为下一轮重点；本轮没有凭这一线索修改现网 MTU。

## 验收结论与下一步

公网入口可用性问题已用最小防火墙修改解决，H2/H3 Range 合同与持续正文输出得到真实公网验证。**H3 性能验收暂不通过，需要继续优化；当前不能声称 H3 优于 H2。** H2 在此视频上稳定，现网 Worker 的 H2 断网恢复仍有约 9–10 秒延迟。

后续应优先做保留现有 QUIC 功能的短期、可回退对照：验证 1280 MTU 路径经过 UDP 中转后的报文/分片表现，并在相同公网路径比较 Aggressive 自动模式与标准 Cubic，关联 RTT、发送预算、正文进度和取消流；同时复核已有原生加载版本部署后的请求/字节/恢复表现。当前不凭猜测修改 QUIC 目标、Nginx 缓冲或重新构建传输架构。

尚未实测：中国住宅和移动运营商、多地客户端、多晚高峰及非高峰差异、真实移动切网、按链路实测丢包率、4K 或高于 14.31 Mbps 的视频、完整应用播放器 UI，以及其他浏览器/设备。当前只证明本出口在北京时间晚高峰内访问真实公网的结果，不代表中国晚高峰用户的总体体验。

## 原始记录与复现

- [部署、控制器日志、修复及文件哈希](../tests/quic/results/2026-10-10-public/deployment.json)
- [修复前协议和 Range](../tests/quic/results/2026-10-10-public/network.json)
- [修复后 H2/H3 Range 对照](../tests/quic/results/2026-10-10-public/network-after-fix.json)
- [公网后端直连诊断](../tests/quic/results/2026-10-10-public/network-origin-direct.json)
- [主浏览器测量，含失败](../tests/quic/results/2026-10-10-public/browser.json)
- [H3 补充状态记录](../tests/quic/results/2026-10-10-public/browser-h3-diagnostic.json) / [H2 相同窗口记录](../tests/quic/results/2026-10-10-public/browser-h2-diagnostic.json)
- [汇总 JSON](../tests/quic/results/2026-10-10-public/summary.json)
- [结果一致性、实际协议、只读请求及部署复核](../tests/quic/results/2026-10-10-public/validation.json)

公网工具拒绝非公开 HTTPS 目标。认证 Cookie / storage state 从私有文件读取，结果不保存密码、Cookie、TOTP、文件名或文件正文。

```sh
python3 tests/quic/public-network.py \
  --url https://revaro-2.0721.ing/ \
  --file-url https://revaro-2.0721.ing/api/files/FILE_ID/download \
  --cookie-jar /private/session.cookies \
  --output /tmp/revaro-public/network.json

REVARO_PUBLIC_URL=https://revaro-2.0721.ing \
REVARO_PUBLIC_VIDEO_ID=VIDEO_ID \
REVARO_PUBLIC_STORAGE_STATE=/private/storage-state.json \
REVARO_PUBLIC_RESULTS=/tmp/revaro-public/browser.json \
node tests/quic/public-browser.mjs

python3 tests/quic/summarize-public.py tests/quic/results/2026-10-10-public
```

`--connect-address 88.214.26.196` 仅用于明确标记的公网源站直连诊断，不替代域名网关路径。Browser 支持用 `REVARO_PUBLIC_MODES`、`REVARO_PUBLIC_PROTOCOLS`、`REVARO_PUBLIC_SCENARIOS` 缩小复测范围；H3 未协商时明确跳过，绝不把 H2 回退标为 H3。

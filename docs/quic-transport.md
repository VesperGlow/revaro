# 可选的标准 HTTP/3 拥塞控制

`crates/revaro-server/src/quic/` 使用 Quinn 0.11 的 `Controller` / `ControllerFactory`，通过 upstream h3/h3-quinn 接收普通 HTTP/3 请求，再交给现有 Axum Router。没有 Hysteria 握手、代理封装、自定义 QUIC 帧、FEC 帧或浏览器插件。Chrome/Edge/Firefox 使用自己的标准 HTTP/3 栈。

拥塞控制与 pacing 位于 HTTP 流之下，覆盖所有请求和文件类型，也包括控制包、重传与静态资源。文件资源仍遵循[共享传输层](file-transport.md)的 Range / ETag 契约；音视频和图片使用浏览器原生加载，显式读取、下载与上传保留共享的有界恢复、校验和续传。

这是服务端**发送方向**的控制器，主要改善下载。标准浏览器的上传发送拥塞控制由浏览器决定，服务端不能把它改成 Brutal；上传继续通过统一的 SHA-256 分块与续传抵抗断流。

## 连接迁移与发送等待

服务端允许 Quinn 进行连接迁移，保留其路径验证和反放大机制。同一客户端 NAT 换源端口或切换地址后，可以继续使用原 HTTP/3 连接；它不需要靠重新握手恢复。应用每 250 ms 检查当前远端地址并同步 pacing 的目的地址登记，保留发送状态和已有预算，移除不再被其他连接使用的旧地址；若旧地址还有连接，不复制它们的突发 credits。每个新请求的 ConnectInfo 也取当前地址。[Quinn migration 接口](https://docs.rs/quinn/0.11.12/quinn/struct.ServerConfig.html#method.migration)

已清理连接的驱动仍可能发送关闭包，迁移中的新目的地址也可能暂未登记。这些路径同样会遇到全局或未知 peer 的限速。每次因 pacing 返回 WouldBlock 都记录当前驱动的唤醒时间，后续 poll_writable 等待 timer；不再要求目的地址仍在 peers 中，避免可写内核 socket 触发 Quinn 的立即重试循环。[Quinn AsyncUdpSocket 契约](https://docs.rs/quinn/0.11.12/quinn/trait.AsyncUdpSocket.html#tymethod.try_send)

回归覆盖未知/已清理 peer 的 Pending 与定时唤醒、迁移预算与登记清理，以及同一个 H3 客户端在换源 IP/端口后继续读取 Range 和验证器。socket 回归属于协议正确性测试，不证明公网切网延迟或弱网吞吐改善；公网性能结论仍见[公网验收](public-quic-acceptance.md)。

## 模式与安全边界

默认不启用原生 QUIC；配置 `APP_QUIC_ADDR` 才创建 UDP endpoint。启用后默认 `aggressive + auto`，根据实际 ACK 吞吐与持续排队反馈动态探测 target；不再要求固定低目标。`standard` 的 upstream Cubic 实现完整保留，设置 `QUIC_CC_MODE=standard` 可用于兼容、诊断与手动回退。`QUIC_TARGET_MBPS=12` 等数字仍可固定目标。自动估计的是当前连接可获得的发送容量，不保证在任意链路上达到物理线路标称带宽。

aggressive 策略：

- 每个有效采样汇总 ACK/声明丢失的字节，保留 5 秒样本，至少 50 MTU 才估算 loss；发送预算为 `target / (1 - loss)`，受补偿倍率、单 peer 和全 endpoint 上限限制。迟到 ACK 和伪重传会使声明丢包估计偏保守，但不能突破硬限速。
- auto 从 4 Mbps 起步（受发送上限限制），采样间隔为 500 ms 或两个 RTT 中较大者，最长 1 秒，避免容量骤降导致 RTT 拉长后迟迟不调整。按整段 ACK 字节/时间估算实际交付速率，以同期真实 UDP 发送速率限制 ACK 压缩造成的高估；空闲或应用供给不足时不提高 target。启动阶段逐样本最多提高 50%，稳定阶段最多提高 8%；4 Mbps 是探测起点，不是长期限速。
- 两个连续有效样本出现持续排队（RTT 比最小 RTT 增加超过 50% 或 50 ms）或交付速率低于 target 的 70%，才向实际 ACK 吞吐收敛，持续排队每次最多下降 20%；实际交付不足 target 一半的明显容量下降，每次最多下降 50%。单次 RTT 抖动和普通随机丢包不会直接乘性缩小窗口。固定 target 仍从 25% 在约 2 秒内升到目标。窗口按两倍 BDP 计算，RTT 使用 10 ms～2 s 边界，并受最大窗口限制。
- Quinn 自带 pacing 由窗口/RTT 推导，Controller 接口没有独立的实际 pacing-rate setter。因此增加 `AsyncUdpSocket` token bucket，同时限制每个 UDP peer 和整个 endpoint，包含重传与 IPv6/UDP 头预算。Tokio timer 通过每个连接独立的 poller 唤醒，不忙等，也不会假装发送成功。
- 突发最多积累 4 ms 的预算，至少两个 MTU、绝不超过 64 KiB；长时间空闲不能积攒无限 credits。关闭发送 GSO，MTU discovery 上限 1452，确保每个 datagram 都单独通过预算检查。未知 peer 共用 1 Mbps 握手预算，Quinn 的地址验证与反放大机制保留。
- persistent congestion 或 ECN 信号立即交给 Cubic；persistent congestion 重置到两个 MTU。连续两个采样期 loss ≥35%，或一次 ≥60%，也转回 Cubic并停止损失补偿。上限不足时只能降低实际吞吐，不能越界发送。
- 路径恢复后，至少 5 秒冷却、有足量 ACK、loss <25% 才允许重新探测（auto 从安全起点，固定模式从 25% target）。第二次冷却延长至 10 秒；每条连接最多两次恢复探测，第三次进入 Cubic 后保持 Cubic。持续故障或重复拥塞不能无限重启 aggressive。
- QUIC idle timeout 30 秒、握手 timeout 10 秒、5 秒 keepalive；每个 HTTP/3 流的发送停滞 8 秒只 reset 该流，现有 Range 层补取失败块。大响应拆成至多 16 KiB 的发送片段，按进度计时。正常路由的上传时限、鉴权、Origin guard、磁盘预留与并发限制仍生效。TLS 0-RTT 保持关闭，避免写请求重放。

Controller 只决定本地窗口和发送预算；QUIC 丢包检测、PTO、重传、ACK、TLS 与 HTTP/3/QPACK 均由原库处理。[Quinn Controller](https://docs.rs/quinn/0.11.12/quinn/congestion/trait.Controller.html)、[UDP adapter](https://docs.rs/quinn/0.11.12/quinn/trait.AsyncUdpSocket.html)、[RFC 9002 persistent congestion/pacing](https://www.rfc-editor.org/rfc/rfc9002.html#section-7.6)。有限损失补偿参考 [Hysteria Brutal 的目标/ACK-rate 思路](https://github.com/apernet/hysteria/blob/master/core/internal/congestion/brutal/brutal.go)，实现没有引入该项目的协议或传输封装。

## 配置

带宽单位为十进制 Mbps；发送预算包含 QUIC 与保守 UDP/IP 开销，文件有效吞吐通常略低于目标。

| 变量 | 默认值 | 含义/范围 |
| --- | --- | --- |
| `APP_TLS_ADDR` | 空 | 原生 HTTPS TCP 地址，例如 `0.0.0.0:8443` |
| `APP_TLS_CERT`, `APP_TLS_KEY` | 空 | 启用 TLS 时必须设置的 PEM fullchain 与私钥路径 |
| `APP_QUIC_ADDR` | 空 | QUIC UDP 地址，例如 `0.0.0.0:8443`，需要原生 TLS |
| `APP_HTTP2_ADDR` | 空 | 原生 HTTP/2 专用 TCP 地址；该入口不会广告 Alt-Svc |
| `APP_HTTP2_BASE_URL` | 空 | 同 hostname、不同 HTTPS 端口的回退 URL |
| `QUIC_CC_MODE` | `aggressive` | `standard` / `aggressive` |
| `QUIC_TARGET_MBPS` | `auto` | 空或 `auto` 动态估算；数字 1～1000 固定 target；standard 不使用 target |
| `QUIC_MAX_MBPS` | 250 | 每 peer 最大发送预算，1～1000 |
| `QUIC_GLOBAL_MAX_MBPS` | 1000 | 全 endpoint 共享硬上限，1～1000 |
| `QUIC_MAX_COMPENSATION_PERCENT` | 125 | 最大补偿倍率，100～200（125 即 1.25×） |
| `QUIC_MAX_WINDOW_MIB` | 8 | 窗口上限，1～64 MiB |
| `QUIC_MAX_CONNECTIONS` | 32 | 全 endpoint 同时连接上限，1～128 |

固定 aggressive 要求 `target ≤ peer max ≤ global max`；auto 始终受 `peer max ≤ global max` 硬上限约束。默认上限是管理预算，既不是固定 target，也不是保证的线路容量。原生监听地址不接受端口 0；公网 URL 与内部监听端口可以因 NAT 映射而不同。`APP_QUIC_PUBLIC_PORT` 默认从公网 URL 推导，Alt-Svc 公布公网 UDP 端口。

公网部署现在通过 `APP_DOMAIN` 自动启用 ACME、TCP 80/443 与 UDP 443，无需反向代理；容器内也是 80/443，可使用宿主机网络。见 [公网入口部署](public-ingress.md)。以下手工 PEM 示例用于兼容与诊断：

```dotenv
APP_BASE_URL=https://files.example.com:8443
APP_TLS_ADDR=0.0.0.0:8443
APP_TLS_CERT=/tls/fullchain.pem
APP_TLS_KEY=/tls/privkey.pem
APP_QUIC_ADDR=0.0.0.0:8443
APP_HTTP2_ADDR=0.0.0.0:8444
APP_HTTP2_BASE_URL=https://files.example.com:8444
QUIC_CC_MODE=aggressive
QUIC_TARGET_MBPS=auto
QUIC_MAX_MBPS=250
QUIC_GLOBAL_MAX_MBPS=1000
QUIC_MAX_COMPENSATION_PERCENT=125
```

开放 TCP/UDP 8443 与 TCP 8444，使用浏览器信任的正式证书。主 HTTPS 入口发送 `Alt-Svc: h3=":8443"; ma=300`，回退入口只配置 `h2` / `http/1.1` ALPN。已有 `APP_ADDR` HTTP listener 继续用于内网与健康检查。

Compose 可使用 `compose.quic.yml` override；设置示例中的两个 HTTPS URL，以及 `REVARO_TLS_DIR`，确保容器 uid 10001 可读取证书，然后：

```sh
docker compose -f compose.local.yml -f compose.quic.yml up -d
```

如果 nginx/CDN 终止 QUIC，实际拥塞控制由代理决定，Revaro 的自定义 Controller 不会作用于那条连接。需要让 UDP QUIC 直接到 Quinn，或者做 UDP 透传；只给上游 HTTP 加配置不能改变代理的 QUIC sender。独立 HTTP/2 authority 配合共享层的 retry/停滞检测避开 QUIC/UDP 故障，浏览器自身也能回退主入口的 TCP。CORS、凭据和 Resource Timing 只向配置的同 hostname 主 HTTPS origin 开放。

## 测量与复现

Rust 测试覆盖补偿上限、持久拥塞、有限恢复探测、多个 peer 的共享 pacing 上限与清理，以及真实 TLS/QUIC socket 上鉴权、校验上传、重复 part、四种任意 MIME 的 Range/ETag 契约。Classic Worker 在 web-build 中从同一份 transport-core 与 Worker 入口生成，兼容没有 module Service Worker 的 Firefox；浏览器源码中仍只有一份恢复策略。

`tests/quic/netem.py` 只在 `unshare -Urn` 创建的隔离 namespace 中改变 loopback qdisc，拒绝操作原网络命名空间。使用真实 Revaro 进程、标准 curl HTTP/3、64 MiB 任意二进制对象、SHA-256 校验上传、逐块前缀校验；每个样本重启测试 endpoint，防止取消下载后的旧 PTO/字节竞争下一组线路。每秒有效吞吐、尾部静默、进度最大间隔、tc 原始 counters、故障恢复时间均记录到 JSON。定时终止的前缀测量不等价于整文件完成测试。

```sh
cargo build -p revaro-server
python3 tests/quic/netem.py --output /tmp/revaro-quic-results \
  --seconds 20 --repeats 3 --recovery --browser
```

依赖 Linux 用户 namespace、ip/tc、OpenSSL、支持 HTTP3 的 curl；`--browser` 还需要 tests/e2e 的 Playwright 与 Chromium。默认脚本使用 auto、peer max=250 / global max=1000；历史基线可追加 `--target 12 --max-mbps 20 --global-max-mbps 40` 复现。`--line-mbps` 与 `--payload-mib` 可改变线路及对象大小，`--capacity-change` 在第 5 秒把容量降低到 1/4、第 12 秒恢复。模拟 20 Mbps、每方向 60±10 ms 延迟（约 120 ms RTT），0/5/10/15% 随机包丢失，compensation=1.25×。额外恢复测试在第 5～7 秒施加 100% loss。浏览器整文件测试使用同一 Worker 下载 PDF、ZIP、文本和任意二进制，验证整文件 SHA-256，并在任意二进制下载中短暂断流。

本地测试用的自签名证书需要仅在测试 profile 中信任。Chromium/Edge 的测试可使用 unknown-root/force-QUIC flags；它们只解决临时证书信任，发送的仍是标准 HTTP/3。生产用可信证书，不需要这些 flags。Firefox 可在独立 NSS profile 中导入测试 CA，经普通 Alt-Svc 协商。

## 2026-10-06 固定 target 基线（修改默认值前）

这组历史测试使用固定 target=12 Mbps、peer max=20 Mbps，不能代表当前 auto 默认。原始数据在 [`tests/quic/results/2026-10-06/`](../tests/quic/results/2026-10-06/netem.json)。Linux 6.12、debug 服务端、curl 8.14.1 HTTP/3；每组 3 个独立 20 秒样本，表中为第 4～20 秒逐秒有效吞吐的中位数。提前 reset 的样本保留其后零吞吐，**没有剔除失败样本**。tc delay 会产生重排序，协议声明丢失也包含部分伪丢包；表中 loss 是 qdisc 配置，不是声称精确的端到端测得 loss。

| netem loss | standard Mbps（最小～最大） | aggressive Mbps（最小～最大） | 单流 reset：standard / aggressive |
| --- | --- | --- | --- |
| 0% | 18.23（18.19～18.28） | 11.06（11.01～11.28） | 0/3、0/3 |
| 5% | 0.36（0.33～0.38） | 11.26（11.25～11.30） | 0/3、0/3 |
| 10% | 0.27（0.22～0.27） | 11.24（0.01～11.26） | 0/3、1/3 |
| 15% | 0.15（0.08～0.20） | 11.09（11.08～11.19） | 2/3、0/3 |

reset 是 8 秒发送停滞保护触发的标准流取消，测试的原始 curl 单流不自动续传；不能把中位数解释成每条连接都始终稳定。应用通过已完成 Range 块缓存和失败块重试继续恢复。

在 5% 基础丢包、第 5～7 秒完全断流的额外样本中：standard 恢复首个字节耗时 0.80 秒，aggressive 耗时 1.24 秒（均从恢复线路起算）；aggressive 在第 15 秒重新达到约 11.36 Mbps，即线路恢复后约 8 秒回到原吞吐。日志确认先因 persistent congestion 进入 Cubic，再在 ACK/冷却条件满足后进行受限恢复探测。该机制把初版永久保持 Cubic 的低速尾部修复为可恢复的发送预算。

浏览器整文件验证：Chromium 154.0.8037.57、Microsoft Edge 154.0.4258.62、Firefox 141.0 均以标准 h3 完成 PDF、ZIP、文本和任意二进制的校验上传/整文件下载。Chromium 的额外 netem 测试在 15% loss 下完成前三个 1 MiB 文件；6 MiB 任意二进制下载期间再施加 2 秒完全断流，14.28 秒完成、最大应用进度间隔 5.95 秒、完整 SHA-256 匹配。持续阻断 UDP 后，共享 transport 在 15.68 秒内切到独立入口，Resource Timing 验证为 `h2`；不存在依赖修改浏览器协议才能恢复的服务端实现。

这组固定 target 基线表明，12 Mbps 预算会限制零丢包线路吞吐；因此当前默认改为 aggressive 的自动带宽模式。最大补偿仍为 **1.25×**，突发仍受 4 ms/64 KiB 边界限制，窗口 8 MiB、连接 32 条。standard 完整保留以便手动对照和回退。

这些结果证明该模拟环境下的行为，不能替代真实国际路径、多用户竞争、不同 RTT/带宽与无线损失模型的测量。原有 HTTP localhost 实例已更新二进制和共享 Worker，保持现有 URL 与数据；它没有 TLS/UDP 配置，因此未开启 QUIC。测试用隔离 HTTPS/H3/H2 endpoint 与生产实例分开。

当时验证：`cargo xtask check`（fmt、原生 Clippy `-D warnings`、462 项 Rust 测试、WASM 类型检查）、9 项 Node 恢复测试、已有实例的 Chromium 全局恢复 E2E 均通过。Compose 的默认配置与可选 TLS/UDP override 已验证可解析；当前环境没有 Docker daemon，未重建运行中的容器。


## 2026-10-06 默认 aggressive + auto 验证

当前默认采用自动 target；单 peer / 总发送硬上限改为 250 / 1000 Mbps，补偿仍为 1.25×。`QUIC_TARGET_MBPS=auto` 或空值会随 ACK 吞吐、发送供给和持续排队探测，数字保留固定目标。`QUIC_CC_MODE=standard` 保留完整 Cubic 实现；原生监听与独立 HTTP/2 回退配置没有变化。

原始结果在 [auto netem JSON](../tests/quic/results/2026-10-06-auto/netem.json)。同样是 debug 服务端、20 Mbps、约 120 ms RTT、每方向 10 ms jitter、每组 3 次独立 25 秒；表为第 4～25 秒平均吞吐的中位数，失败流结束后的零吞吐仍计入。

| netem loss | standard Mbps（最小～最大） | aggressive auto Mbps（最小～最大） | reset：standard / auto |
| --- | --- | --- | --- |
| 0% | 18.29（18.26～18.29） | 17.99（17.98～18.14） | 0/3、0/3 |
| 5% | 0.38（0.38～0.40） | 17.85（17.83～17.92） | 0/3、0/3 |
| 10% | 0.15（0.07～0.28） | 17.25（15.33～18.00） | 2/3、0/3 |
| 15% | 0.06（0.04～0.18） | 16.82（16.65～16.92） | 3/3、0/3 |

额外的 [200 Mbps 验证](../tests/quic/results/2026-10-06-auto/high-bandwidth.json)使用约 120 ms RTT、无额外 jitter / 随机 loss、256 MiB 任意二进制对象、5000 packet qdisc limit，每种模式 3 次 20 秒前缀传输。auto 第 4～20 秒平均吞吐中位数为 **119.73 Mbps**（115.97～123.18），结束时动态 target 约 185～231 Mbps，证明没有固定 10/12/50 Mbps target 限制。实际有效吞吐仍受应用供给、客户端流控、协议开销和运行环境影响；该环境下未达到标称 200 Mbps。standard 为 12.76 Mbps（11.30～15.97）；即使 qdisc 未配置 loss，QUIC 仍声明少量 packet loss，因此不能将此结果解释为理想无损网络上所有 Cubic 实现的上限。

5% loss 下，第 5～7 秒完全断流的 raw 单流样本，在恢复后 **1.26 秒**重新接收字节，随后触发 8 秒停滞保护并 reset。保留该失败样本；原始 curl 不执行应用的 Range 重试。另一容量变化样本在第 5 秒将线路从 20 降到 5 Mbps、第 12 秒恢复，控制器因高 loss 进入 Cubic、经健康 ACK 和冷却重新探测，末尾第 21～25 秒有效吞吐平均 **16.46 Mbps**。它不会因旧 target 无限发送，也不会永远固定在降低后的预算。

[Chromium 整文件验证](../tests/quic/results/2026-10-06-auto/chromium-netem.json)：15% loss 下 PDF / ZIP / 文本各 1 MiB 完成，分别约 1.23 / 1.35 / 1.12 秒；6 MiB 任意二进制再遭遇 2 秒断流，**17.81 秒**完成，最大应用进度间隔 **11.18 秒**，全部 SHA-256 匹配。该间隔仍有改进空间，但共享 Range 层完成了恢复。持续阻断 UDP 后，**15.76 秒**切到独立 HTTP/2 authority，Resource Timing 为 `h2`。本轮没有修改 HTTP/3 wire protocol；前一轮 Chrome/Edge/Firefox 的标准 h3 互通结果仍保留在固定 target 基线中。

复现当前默认参数：

```sh
python3 tests/quic/netem.py --output /tmp/revaro-auto20 \
  --seconds 25 --repeats 3 --recovery --capacity-change --browser
python3 tests/quic/netem.py --output /tmp/revaro-auto200 \
  --seconds 20 --repeats 3 --loss 0 --line-mbps 200 --jitter-ms 0 --payload-mib 256
```

[验证记录](../tests/quic/results/2026-10-06-auto/validation.json)：467 项 Rust 测试、原生 Clippy、fmt、WASM 类型检查、9 项 Node 恢复测试和已有实例的文件传输 E2E 均通过。新增测试覆盖默认 aggressive/auto、保留 standard/固定 target、临时 RTT 初始值替换、压缩 ACK、应用供给不足、容量下降、重新探测及硬上限。本地已有 HTTP 实例已更新并通过 ready 检查，仍使用原来的 URL 与数据；未配置 TLS/UDP，因此本次没有将该 HTTP 入口变成 QUIC 入口。

合并音频/界面后的发布验证还覆盖 CI 的 64 个 Chromium 场景：分阶段验证 63 项通过，1 项因未配置外部大型 FLAC 样本而跳过。修正了集合菜单与主音频元素定位、手机卡片比例、上传面板避让，以及与 Worker/统一分块上传接口对应的故障注入；标准 HTTP/3 与拥塞控制实现保持上述验证版本。详细场景记录见同一 validation JSON。

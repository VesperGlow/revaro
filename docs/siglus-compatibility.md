# Siglus 本地兼容性适配

这是最初用于验证兼容性的独立浏览器原型；运行时已整理进 Revaro 的 SiglusEngine `/g/` 播放器，生产分享通过同源 ZIP Range 和 manifest 复用这里的 VFS 适配。原型中的本地文件夹导入界面仍不属于生产流程。

上游为 [xmoezzz/siglus_rs](https://github.com/xmoezzz/siglus_rs)，固定版本 `5f358ef1360b0439e92aed2b749e8f25d8721954`，许可证为 MPL-2.0。修改保存在 [web-compat.patch](../scripts/siglus/web-compat.patch)，没有包含游戏资源或游戏密钥。

## 修改范围

- WASM 语音解码使用现有解码器，避免调用浏览器不支持的原生线程。
- 保留 Gameexe 的游戏逻辑分辨率，等比适配画布，并用同一视口反向换算输入坐标。
- 浏览器渲染器申请显卡实际支持的纹理尺寸，不使用 WebGL2 兼容档位默认的 2048 上限。窗口超过设备实际限制时，仅等比缩小内部绘制缓冲，CSS 仍铺满窗口；渲染视口与输入坐标保持一致。
- 在独立探测画布上选择可用的 WebGPU 或 WebGL2，不提前绑定游戏画布；两种后端都不可用时提示启用浏览器图形加速。WGPU 0.20 的 WebGPU glue 只在浏览器已移除旧参数时省略 `maxInterStageShaderComponents`，保留浏览器原生的插值变量限制。
- 当两种图形 API 都不可用时，播放器切换到 Canvas2D 软件后端。CPU 合成器覆盖普通 2D 背景、立绘、透明度、缩放、裁剪、色调和常见混合模式；视频帧与高级 3D/擦除效果仍在补齐，浏览器不需要启用硬件图形加速即可进入普通剧情。
- 软件精灵的锚点、源裁剪和变换复用 GPU 路径的 `render_math` / `render_plan`，不额外减去半张图片尺寸。首屏回归在用户交互前检查整个游戏帧，防止启动背景只剩左上角四分之一；仅检查 canvas 尺寸不能检出这个错误。
- 浏览器字体目录通过 ZIP VFS 枚举，包含游戏的 `font/`、`fonts/`、`dat/` 和根目录。配置字体优先；缺少的字形按字符回退到包内其他字体，并缓存解析结果。金辉汉化包内的 `dat/font03.otf` 可补齐内置日文字体缺少的简体字，无需把游戏字体复制进播放器公开资源或修改 ZIP。
- 鼠标按下、松开分别交给游戏帧处理；日志接收的按键和鼠标操作会同时消费其释放事件，防止日志关闭后穿透到剧情。
- 失焦时清除输入状态，不通过合成松开事件触发剧情、确认或菜单操作。
- 读档初始化清除菜单遗留、且原生存档不序列化的临时隐藏开关；剧情脚本自己的可见性仍由存档恢复。隐藏消息窗时，鼠标恢复操作会消费这次点击，避免同时推进剧情。
- 浏览器使用动画帧请求继续绘制，避免 `ControlFlow::Poll` 持续调度任务。
- 存档、缩略图和文件操作复用上游 `game_fs` 接口，原型写入 IndexedDB，不写回游戏 ZIP。
- MPEG、OMV 改为随播放解码，保留当前帧、预读帧及解码器的小队列，退出视频时释放缓存。WMV 保留上游路径。
- 将 CPAL 的固定 AudioContext 检测表达式替换为直接检测；遇到其他动态表达式会报错，不放宽 CSP 来允许 `eval`。
- 固定 CPAL 0.15.3 的 WebAudio 流生命周期：销毁前标记流已退出，已排队的定时器和 `onended` 回调直接返回，避免继续操作关闭的 AudioContext 并触发 WASM `unreachable`。构建脚本校验原始 crate 的 SHA-256，再应用独立补丁，不修改全局 Cargo 缓存。
- 可选诊断接口仅在 `__siglusProbeUi === true` 时输出状态，采用 `&self` 读取，不调用会消费等待事件的 `SceneVm::is_blocked()`。

原型页面另外约束画布的 CSS 尺寸，防止 winit 写入的内联固定尺寸在窗口缩小、开发者工具打开时造成页面溢出；不使用全局缩放。

## 构建

需要 Rust 1.98.0、`wasm32-unknown-unknown` 目标和 wasm-bindgen 0.2.111。在干净的上游检出中切换到上述固定版本，然后运行：

```bash
SIGLUS_WASM_BINDGEN=/path/to/wasm-bindgen \
  bash scripts/siglus/build-player.sh /path/to/siglus_rs /path/to/output
```

脚本检查上游版本，应用补丁，构建 WASM，适配 glue，并输出许可证、来源版本和 SHA-256 清单。重复运行时会检查补丁是否已经应用。输出目录不会自动成为 Revaro 的公开资源目录。

CPAL 的源码由 `prepare-cpal.py` 从本地 Cargo crate 缓存或固定下载地址取得，校验 Cargo.lock 原始校验和后解包到上游检出的 `vendor/cpal`，应用 `cpal-webaudio-lifecycle.patch`。其余平台的音频后端保持原来的实现。

本地预览工具（仅绑定回环地址，不是生产用游戏服务器）：

```bash
python3 scripts/siglus/serve-preview.py \
  --archive /path/to/game.zip \
  --runtime-dir /path/to/output \
  --key-config /path/to/key.toml
```

`--key-config` 可以省略；需要专用密钥的游戏必须自行提供。原版使用默认页面，当前汉化格式测试使用 `/?profile=chinese`。预览工具支持保留 ZIP 中的单个顶层游戏目录，并且不读取 ZIP 自带的 Windows 存档。

## 原型与限制

本地原型地址为 `http://localhost:18081/?profile=chinese`。实测资源来自 Downloads 中的 `金色Loveriche.zip`。生产 `/g/` 分享使用同样的 `scene.chs`、`gameexe.chs` 和 `.g01` 路径映射，资源只读，不重命名或修改 ZIP 内容。该配置不能推广为所有汉化补丁的自动识别规则。

ZIP 服务使用受控 HEAD、Range/206 文件读取，不在服务器解压或缓存游戏。浏览器按 ZIP 条目读取、解压并检查 CRC，资源 LRU 上限为 64 MiB；存档沿用原型既有的游戏与语言配置命名空间，排除 ZIP 内携带的 Windows 存档。

仍需注意：

- VFS 目前一次读取一个完整资源；DEFLATE 条目和大语音包的同步读取、解压仍可能暂停主线程。
- 视频的压缩文件与音轨 PCM 仍会整项读取或解码。MPEG/OMV 的画面缓存改进不代表总内存与视频长度无关；WMV 仍有整段画面解码风险。
- 长 OP/ED、所有视频编码变体、持续音画同步、完整路线和移动设备尚不能视为兼容通过。
- 游戏专用密钥与汉化资源选择目前由本地配置提供，尚未形成 Revaro 分享创建流程。
- 浏览器软件 WebGPU 的渲染速度会影响动画、测试等待和输入反馈；不能据此推断硬件加速设备的帧率。
- 本次测试中，文字仍在显示、界面切换未结束时点击 SAVE 有一次未进入菜单；等文字显示完后再次点击成功。不能将这类过渡阶段的输入全部认定为兼容通过。

## 2026-10-25 实测结果

在本地 Chromium、软件 WebGPU 和这个汉化包中验证了：

| 场景 | 结果 |
| --- | --- |
| 读取既有第一槽存档 | 中文剧情与消息窗直接恢复，不再需要额外按键显示 |
| 滚轮打开日志，右键或 Esc 关闭 | 回到原对话，消息窗不隐藏，正文不推进 |
| 日志方向键、Enter 返回 | 返回原对话，释放事件不推进正文 |
| 正常剧情右键 | 隐藏消息窗；再次右键恢复，正文保持不变 |
| Space | 中文对话继续推进 |
| 新建第二槽、缩略图、刷新后读取 | IndexedDB 写入完成，刷新后重新读取并恢复正文；第一槽保留 |
| 失焦 | Ctrl 和鼠标按住状态清除 |
| 390px 窄屏与模拟触控 | 无页面溢出；触控按下、松开进入引擎，并推进对话。未测试真实手机 |
| 开场 MPEG 自然播放结束 | 本样本保留画面峰值 2 帧、16,711,680 字节；退出后画面缓存归零 |
| 系统音频输出 | 约 6 秒采样中存在非静音输出；不代表所有语音和长视频同步已验证 |
| 新鲜会话的 ZIP 读取 | 90 次资源请求均为 206，共读取约 39 MB 压缩条目；未下载完整 8.58 GB ZIP |

HEAD 返回长度且无正文；有效 Range 返回 206，无效区间返回 416，缺失区间返回 400，匿名资源请求返回 403。新增预览工具也检查了静态 WASM MIME、隔离响应头及 ZIP 元数据与实测实例的一致性。

实际测试记录、截图与构建产物保存在 `data/galgame-local/siglus-research/`，该目录不进入版本控制。`compat-regression.json` 记录了过早点击 SAVE 的失败尝试；`compat-followup.json` 和 `compat-persistence.json` 分别记录了后续存档、触控以及刷新读取等通过结果，没有将失败尝试改记为成功。

## Revaro 分享配置与启动

`/g/` 播放页打开后自动读取 ZIP 目录并初始化引擎，画布占满页面，不显示启动按钮或顶栏。启动时临时显示加载提示，失败时显示具体错误；引擎异步报告的 `host init` 错误也会显示，避免留下无提示的黑屏。

分享的 `entry` 决定加载 `Scene.pck` 或 `scene.chs`，URL 查询参数不能切换它。需要 `key.toml` 的游戏可以把配置放入 ZIP 根目录，或在管理端分享对话框的“游戏配置”中填写。后一种配置保存到该分享的 `key_config` 字段，并随有效的分享索引传给浏览器；它优先于 ZIP 和浏览器里自动生成的配置。配置不写入公共播放器或游戏 ZIP，也不包含在仓库中。重新生成同一文件的链接时保留配置，显式清空可以移除；撤销、过期和文件回收后无法通过旧链接读取。

浏览器可能限制未交互页面的声音自动播放。游戏仍自动初始化，首次点击画面或按键会恢复该页面的音频上下文，不增加单独的启动按钮。

## 关闭音频流后的启动崩溃回归

`tests/e2e/siglus-real-lifecycle.spec.ts` 是可选的真实游戏测试，通过 `SIGLUS_REAL_GAME_URL` 指向本地 `/g/` 分享运行。测试保留真实 WASM、游戏资源和音频后端，仅让关闭后的 `createBufferSource()` 严格抛出 `InvalidStateError`，以复现部分浏览器的行为。

2026-10-25 本地验证：旧 WASM 在启动时触发 CPAL panic，最终错误被覆盖为 `unreachable`；加入流销毁标记后，Chromium、Firefox 均通过自动启动与 30 次剧情推进，已关闭上下文访问次数为零，检测到非静音 PCM，浏览器与引擎错误为空。这不代表完整路线或用户的 Windows Edge 环境已经逐一验证。

```bash
SIGLUS_REAL_GAME_URL=http://127.0.0.1:18083/g/kinkoi \
PLAYWRIGHT_EXECUTABLE_PATH=/usr/bin/chromium \
tests/e2e/node_modules/.bin/playwright test \
  --config=tests/e2e/playwright.config.ts siglus-real-lifecycle.spec.ts
```

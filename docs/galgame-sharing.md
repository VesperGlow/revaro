# Galgame 网页分享（Kirikiri2）

上传完整游戏 ZIP，在文件的「分享」对话框选择「Galgame 分享」，引擎为 Kirikiri2，填写入口 XP3（例如 `data.xp3`，本机《纸上魔法使》汉化入口为 `dtcn.xp3`），创建链接后直接访问 `/g/{token}`。游戏由当前 Revaro 进程提供，浏览器执行 WASM；不解包、不复制分享文件，不需要额外游戏服务器或游戏域名。

普通分享仍使用 `/s/`，接口参数和旧链接行为保持兼容。每个文件仍只有一条分享记录，切换分享类型或重新生成随机链接会让原链接失效。自定义后缀继续全局唯一。

## 部署

本机构建需要匹配的 `wasm-bindgen-cli 0.2.128`（原 Dockerfile 已安装）。按原流程执行 `cargo xtask web-build` 和 `cargo build -p revaro-server --release`。播放器已随 `crates/revaro-web/static/galgame/` 一起打包，无 CDN 或运行时下载步骤；原 Docker 构建的静态资源复制步骤会包含这些文件。确保部署完整 `dist/web/galgame/`，并将 `APP_WEB_DIR` 指向构建后的目录。服务启动自动执行数据库迁移 018；部署前按原运维流程备份 SQLite 数据库。

浏览器必须访问可信 HTTPS，开发时可以使用 `http://localhost:端口`。`APP_BASE_URL` 必须与浏览器地址的源一致，供密码解锁 POST 的 Origin 检查使用；HTTPS 部署使用安全 Cookie。反向代理应原样转发 Range、If-Range、HEAD 及 206/416，禁用 `/g/` 的代理缓存和响应缓冲，不压缩 ZIP，也不要覆盖应用的 COOP/COEP/CORP/CSP。若使用流量防护，避免将正常的频繁小 Range 当作下载攻击。

`/g/`、`/galgame/` 单独设置 `COOP: same-origin`、`COEP: require-corp`、`CORP: same-origin`，其余页面的隔离策略保持不变。播放器脚本是同源外部文件，沿用全站 CSP，无内联脚本、`unsafe-eval`、第三方分析或 PWA 注册。缺少静态资源时返回错误，绝不以 SPA HTML 代替 WASM/JS。

## 访问与存储

- 创建、读取管理状态、撤销分享仍要求管理员登录，并沿用原有效期、文件 ready/回收站校验。
- 当前原系统并没有分享密码功能；本次仅对 Galgame 新增可选密码，复用现有 Argon2id、KDF 并发控制、Origin 检查和登录限速实现，使用独立的游戏密码限速计数。普通分享的公开访问方式未更改。
- 密码授权 Cookie 为 HttpOnly、SameSite=Strict，路径限定到单个分享，12 小时有效，服务端每次资源请求重新检查密码授权、分享状态、有效期和文件状态。更新密码或更换令牌后原授权失效。已传到浏览器的游戏字节无法远程撤回。
- ZIP 通过 `GET|HEAD /g/{token}/archive` 从现有 `FileAccess` 层流式读取；复用强 ETag、Range、206/416。GET 必须带有效 Range，忽略 Range 或过期 If-Range 不会触发整包下载。单个请求占用原有公共分享并发槽直到传输完成或取消。
- 播放器仅从已授权的 `/g/{token}/manifest` 获取入口、资源地址和存档标识，URL 查询参数不能替换游戏资源或存档空间。不能借 `/s/{game_token}` 绕过游戏密码访问 ZIP。
- IndexedDB 存档按文件 ID + 原存储对象标识隔离，重新生成分享链接仍可复用该游戏的存档；不同上传文件有独立存档。不会访问 ZIP 之外的 Windows savedata，也不把存档写回 Revaro；若上传 ZIP 本身含有存档，首次运行时引擎仍可能读取归档中的存档。浏览器尽力申请持久化，用户仍可能拒绝或清理数据。
- 页面没有注册游戏 Service Worker，现有 Revaro 传输 Worker 对 `/g/` 不做缓存或重试。远程 ZIP 不回退到整包 Blob。压缩条目的临时解压发生在浏览器 OPFS，服务器不做解包和游戏缓存。

## ZIP 与引擎限制

推荐 ZIP_STORED（仅存储、不再压缩）打包全部资源，XP3 本身通常已压缩。ZIP deflate 条目第一次读取时仍需要把**整个该条目**下载、流式解压到浏览器 OPFS，之后才能随机读；一个数 GB 的 deflate XP3 不具有细粒度网络按需加载效果。本集成没有宣称普通压缩 ZIP 的任意内部偏移都可直接 Range 解压。

播放器沿用上游 ZIP/ZIP64 解析器。入口为去掉共同顶层目录后的相对 XP3 路径；必须存在且完整匹配。加密 ZIP、不支持的压缩方法、分卷、奇异文件名编码、损坏归档和非 Kirikiri2 引擎不在已验证范围内。创建接口校验 ready ZIP 文件、引擎和入口路径的格式，归档实际内容与入口是否存在由浏览器读取索引后验证，创建成功不代表游戏兼容。

上游主要面向 Chromium 137+，实际还要求 JSPI（`WebAssembly.Suspending/promising`）、SIMD、原生 WASM exceptions、共享内存、WebGL；ZIP deflate 需要 OPFS 和 `DecompressionStream('deflate-raw')`。Firefox/Safari、移动实机、低内存设备尚未验证；Chromium 横屏触控仿真与 DPR 2 已检查。不能仅凭支持 WebAssembly 推断可以运行。音频自动播放可能需要一次用户交互。

沿用上游同源引擎单例锁，同一站点一次只运行一个游戏标签页。即使浏览器存档已隔离，也没有扩展为多引擎并发执行。

原生 DLL、特殊插件、加密 XP3、视频格式、汉化修改、多窗口界面等兼容性取决于上游实现；本次不运行 Windows EXE，也没有把本机现成的游戏专用脚本补丁塞进通用播放器。实际验证结果记录在本次测试说明中，未通关验证。

上游来源、版本、改动和许可证见 `static/galgame/kirikiri2/SOURCE.md`、`ENGINE-LICENSE.txt` 与 `SHA256SUMS.json`。

## API

```http
POST /api/files/{file_id}/share
Origin: https://revaro.example
Content-Type: application/json

{"share_type":"galgame","engine":"kirikiri2","entry":"data.xp3","expires_in_seconds":604800,"password":"optional-password"}
```

`password` 省略保留当前游戏密码，空字符串移除密码；UI 留空表示保留。返回 `url: https://revaro.example/g/{token}`。管理 GET、DELETE 和 `/api/shares` 仍沿用原接口。普通分享省略新增字段即可。

```http
POST /g/{token}/unlock
Origin: https://revaro.example
Content-Type: application/json

{"password":"optional-password"}
```

成功设置分享专用 Cookie；未经授权的 manifest 和资源 HEAD/GET 均返回 401，失效或被撤销的分享返回 404。密码不进入 URL。

## 本次验证记录（2026-10-24）

- `cargo test -p revaro-server -p revaro-core`：402 项通过；最终资源响应头调整后重新运行 3 项 Galgame 路由测试通过（包含单/多 Range、HEAD、416、过期 If-Range、密码、到期/删除/撤销、存档命名空间）。
- `node --test tests/transport/*.test.mjs`：50 项通过。
- Chromium 浏览器：新增 3 项 Galgame 分享测试通过，涵盖 ZIP 原上传接口、分享对话框、密码门禁、受控 Range、缺少 Range/入口时报错且不整包回退；另有原普通分享有效期与撤销测试通过。合成 ZIP 只用于接口/索引测试，不是可玩游戏兼容性测试。
- `cargo xtask web-check`、`cargo xtask web-build`、`cargo fmt --all -- --check`、后端/共享类型 `cargo clippy --all-targets -- -D warnings` 通过。额外执行的 WASM 目标前端 Clippy 因现有代码中的 330 个诊断未通过，例如 `app.rs` 的 redundant_locals；诊断位置未落在本次新增行上。本次没有顺带重构这些代码。
- 使用原 multipart 上传接口导入本机现有《纸上魔法使》ZIP（3,071,473,515 字节），入口 `dtcn.xp3`，在 Chromium 155 中实际启动中文标题、选择读者预览、进入第一章并显示中文正文。启动测量 67 个游戏资源 GET 均为 206，传输 21,816,022 字节（约 20.8 MiB），另有 HEAD 请求；未整包下载，未请求外部域名，未记录页面脚本异常。
- 实际执行游戏内 SAVE，确认 IndexedDB 写入；刷新页面后在 LOAD 中选回第一槽，恢复到保存时相同的书房背景、夜子对白「现实，铅字织成的纸香世界。」。测试截图和存档清单位于本机 `data/galgame-local/`，不纳入源码分发。
- 本机预览使用隔离的 `data/galgame-local/` 数据库、对象目录及专用浏览器配置，不修改原游戏目录或原 Revaro 数据目录。静态播放器不包含该游戏、汉化脚本或个人存档。

这不是通关或全插件兼容性结论。音频输出、全部视频、后续分支、移动端没有在此次集成验证中验收。原生窗口式确认框和滚轮现在由下述通用输入适配处理；没有移植本机独立播放器中的游戏专用脚本补丁。游戏自己绘制的图层式弹窗、复杂嵌套窗口仍需逐游戏验证。


## 输入与弹窗适配（2026-10-24）

播放器的 `input.js` 只在游戏画布、原生确认窗的显示画布和触控快捷按钮内处理输入。普通 `/s/` 和 Revaro 管理页面不加载这段代码。

- 右键：引擎原生模态窗存在时发送一次 Esc，普通剧情、历史、设置和存读档界面交给原生右键处理。不调用浏览器 history，不打开画布右键菜单。
- 滚轮：通过已有原生 `TVPWindowLayer::onMouseScroll` 入口同时送到 Window 和 DrawDevice/Layer，保留游戏自己的打开日志、翻阅和到底返回规则。队列有上限，窗口切换后丢弃过期事件；不会同时滚动网页。Ctrl/Meta+滚轮保留浏览器缩放。
- 键盘：原生 Esc、方向键、Space 映射不变，Enter 的 Cocos→VK_RETURN 丢失映射已修复。Esc/Enter/Space 不重复发送按住自动重复事件；失焦时释放按键及鼠标状态，并通过原生释放入口立即清除按下标记，避免后台 SDL 队列延后导致按键卡住。F11 和浏览器组合快捷键不进入游戏。
- 弹窗：从引擎读取真实模态标记及画布逻辑尺寸，在最后一帧父窗口上居中显示原确认窗，按比例放大至原逻辑尺寸的至多 1.5 倍，并受可视区域限制。鼠标与触控使用同一裁剪矩形的逆映射；不改变引擎画布尺寸、全局页面缩放或游戏资源。

这份固定版本的 WASM 只增加既有滚轮/按键释放函数的导出，并修正一个键码查找表字节，没有改写函数体。重建工具严格校验原版本；升级引擎必须同步验证 ABI，不能把这些布局偏移套到其他版本。详细来源及 ABI 见播放器的 SOURCE.md。运行 `python3 -m unittest discover -s tests/galgame -v` 可审计重建结果与原模块的一致性。

真实游戏测试可指定 `E2E_GALGAME_URL=http://localhost:18080/g/kamimaho-local`，运行 `tests/e2e/galgame-input-real.spec.ts`。该测试的菜单坐标属于本机《纸上魔法使》验证样本，播放器实现不含这些坐标或游戏名。没有提供真实游戏时测试会明确跳过。

Enter、方向键最终行为仍取决于游戏本身；例如本次样本的确认窗没有实现 Enter 自动确认，修复后按键能够到达 VK_RETURN，但仍需点击原生确认按钮。不会用截图识别或固定坐标替游戏执行确认。


本轮实际验证结果：

- Chromium 155 的真实游戏场景：设置与 LOAD 菜单右键返回；SAVE/LOAD、返回标题及退出确认窗右键和 Esc 取消；弹窗背景点击不穿透；确认/取消按钮的逆映射点击；保存第一槽、刷新后 LOAD 恢复正文。原确认窗逻辑尺寸 574×175，在 1280×720 中居中显示为 861×262.5。
- 推进约 20 次正文后，上滚打开长日志，继续上滚到较旧记录，下滚到底由游戏关闭日志并回到正文；实际累计 32 个原生滚轮事件，无页面脚本异常。没有用键盘替代滚轮。
- 900×500 与 400×800 改变视口后检查居中、比例和边界；窄屏仍受游戏原图文字大小限制，横屏更适合阅读。
- Chromium 触控仿真：844×390、DPR 2，原生画布 1688×780；确认窗 776.47×236.72，中心正确；单点进入菜单、长按右键取消、点击原取消按钮、触控返回/日志按钮通过，无页面异常。修复了合成触摸按钮缺少先行 mousemove 导致 SDL 使用旧指针位置的问题。该结果不等同于 Android/iOS 实机验收。
- Enter、Space、四个方向键检查原生键状态；按住 Esc 的重复 keydown 不穿透关闭第二层界面，失焦释放按键。游戏未实现的键盘动作继续保持原样。
- 3 项固定版本 WASM/JS 适配审计、3 项分享路由测试、4 项普通/Galgame 分享浏览器回归测试通过；桌面真实游戏存读档/输入用例与 DPR 2 触控用例共 2 项通过。后端 Clippy、格式检查和播放器 JS 语法检查通过。

使用这个适配器升级 WASM 时必须同步验证导出入口和原生布局元数据。它不接受任意版本 WASM，也没有宣称适配所有游戏的自定义 UI、嵌套弹窗、视频覆盖或特殊插件。

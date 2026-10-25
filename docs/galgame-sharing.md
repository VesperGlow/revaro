# Galgame 网页分享（Kirikiri2 / SiglusEngine）

上传包含全部游戏资源的 ZIP，在文件的「分享」中选择「Galgame 分享（完整 ZIP）」。程序读取 ZIP 目录并检测 Kirikiri2 的 XP3 或 SiglusEngine 的 `Scene.pck` / `scene.chs` 入口，用户点击入口展开选项，选中后自动收起，不必输入或猜文件名。只有一个候选时自动选中；多个候选时等待用户选择，已有分享保留原入口。入口列表不代表每个资源包都能独立启动，也不自动判断汉化、原版或补丁版本。本机《纸上魔法使》的汉化入口为 `dtcn.xp3`；SiglusEngine 汉化包通常选择 `scene.chs`。

自定义地址显示为固定的站点前缀与独立后缀输入框；只有后缀带输入边框，留空生成随机链接。后缀支持 1–64 位英文字母、数字、`-`、`_`，区分大小写。

打开 `/g/{token}` 直接加载游戏，无密码、验证页面或浏览器能力预检。实际加载失败仍显示错误与重新加载按钮。普通 `/s/` 分享保持原行为；每个文件仍只有一条分享记录，切换类型、更换后缀或重新生成会使旧链接失效。有效期、撤销、文件删除/回收站与 ready 状态仍由服务器逐请求处理。

## 部署与迁移

执行 `cargo xtask web-build`、`cargo build -p revaro-server --release`，部署完整 `dist/web/galgame/`，将 `APP_WEB_DIR` 指向构建后的目录。需要匹配的 `wasm-bindgen-cli 0.2.128`。Kirikiri2 和 SiglusEngine 播放器、WASM、字体和 JSZip 随 Revaro 发布，不依赖 CDN、独立游戏服务器或每游戏域名。

启动自动执行迁移 019，移除原 Galgame 密码字段；原有游戏分享也改为直接访问。管理员管理文件与分享仍需原网盘登录。按原部署流程备份 SQLite 数据库。

浏览器仍需可信 HTTPS（开发可用 localhost）、JSPI、共享 WASM 内存、SIMD、WASM exceptions 与可用 WebGL。网页移除预检并不改变引擎的运行条件；虚拟桌面没有可用 GPU 时需配置软件渲染。本机 Downloads 中的独立游戏浏览器启动入口使用 SwiftShader，不修改原浏览器设置，两个浏览器配置的存档互相独立。

`/g/` 与 `/galgame/` 单独使用 `COOP: same-origin`、`COEP: require-corp`、`CORP: same-origin`，其余页面不变。反向代理须原样转发 Range、If-Range、HEAD、206/416，禁用游戏 ZIP 压缩、代理缓存与响应缓冲，不覆盖隔离头和 CSP。页面不使用内联脚本、第三方统计或游戏 Service Worker；静态资源缺失不会回退到 SPA HTML。

## 资源、入口与存档

- 管理端 `GET /api/files/{id}/game-entries` 经原存储读取器获取 ZIP 目录，不解压、复制或整包读取游戏；使用阻塞工作线程桥接异步 seek/read，目录累计读取预算 8 MiB、最多 65,536 个条目并沿用 ZIP 并发槽。支持 ZIP64。返回路径与 VLFS 一样去掉唯一共同顶层目录，不读取 XP3 内部脚本。
- 资源从 `/g/{token}/archive` 经原 `FileAccess` 流式读取。GET 必须带有效 Range，HEAD 返回元数据；未被接受的 Range 或过期 If-Range 不触发整包下载。分享失效立即停止后续资源访问，不会把游戏缓存在服务器内存。
- 推荐 ZIP_STORED（仅存储）打包，XP3 通常已有压缩。deflate XP3 首次读取仍须下载整个该条目，并在浏览器 OPFS 解压；服务器不解包。
- 游戏和入口由服务器 manifest 指定，查询参数不能替换资源或存档空间。Galgame 令牌不能通过普通 `/s/` 下载接口读取。
- IndexedDB 存档按文件 ID 与原对象标识隔离；更换分享链接可继续使用同一浏览器的存档。重新上传是不同文件身份。不会访问 Windows savedata 或写回上传 ZIP。页面不清理 Revaro 的其他缓存、Worker 或存档。
- 同源引擎使用 Web Lock，一次运行一个游戏标签页。

## API

```http
GET /api/files/{file_id}/game-entries
```

管理员登录后返回 `{"entries":["data.xp3","dtcn.xp3"],"engines":["kirikiri2"]}`，或包含 SiglusEngine 入口及 `siglus`。仅表示可供选择的路径，不会从名称推断哪个版本正确。

```http
POST /api/files/{file_id}/share
Origin: https://revaro.example
Content-Type: application/json

{"share_type":"galgame","engine":"kirikiri2","entry":"dtcn.xp3","expires_in_seconds":604800,"custom_slug":"my-game"}
```

返回 `/g/my-game`。管理 GET、DELETE 与 `/api/shares` 沿用原接口，普通分享省略新增字段。已移除密码参数、密码授权 Cookie 与 `/unlock` 流程。

## 输入及兼容性

输入适配仅作用于播放器：普通场景右键、滚轮、方向键、Esc、Enter 与 Space 交给原游戏处理；原生模态窗右键发送一次 Esc，失焦释放按键。原生确认窗按比例居中放大，点击和触控用同一矩形做逆映射，不全局缩放页面。触控提供单击、长按返回、拖动日志和快捷按钮。

固定 WASM 导出既有滚轮/释放函数、修正 Enter 查找表字节，并移除自动识别文本编码时的两处全局缓存写入。BOM、编码探测顺序和游戏主动指定编码的行为保留，使 GBK 与 Shift-JIS 混用的脚本可逐文件识别。来源、C++ 补丁、许可证、ABI 与可复现适配方法见播放器 `SOURCE.md`、`ENGINE-LICENSE.txt`、`SHA256SUMS.json` 和 `scripts/kirikiri/`。升级引擎需同步适配 ABI。

自定义图层弹窗、复杂嵌套窗口、插件、加密资源及全部音视频兼容性取决于上游。SiglusEngine 使用独立的 `siglus_rs` WASM 播放器，并按 ZIP 条目 Range 读取；汉化资源通过 `scene.chs`、`gameexe.chs` 与 `.g01` 覆盖文件映射到引擎路径。窄屏受游戏原始文字大小限制；移动实机、其他浏览器和完整通关未验收。此前的 Chromium 实际游玩记录不代表所有游戏兼容。

《车轮之国 向日葵的少女》本机所供汉化版本入口为 `akcn.xp3`。实测编码修复后通过原先 `HistoryLayer.tjs` 的字符转换错误，但仍停在游戏自身的光盘检查。Web 引擎尚无光驱卷标查找能力，当前 ZIP 不能认定可玩；未验证其标题、中文剧情、存读档或音视频。

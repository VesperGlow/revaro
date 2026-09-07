# Revaro 1.0 发布前审计

审计日期：2026-09-07。检查覆盖 Go 控制面、Rust 数据面、Vue 前端、SQLite 迁移、S3、BT/直链、媒体、阅读器、任务、缓存、部署和 CI。本轮后续修复增加了字幕副本所必需的迁移 019；没有增加产品功能或替换现有架构。

结论：已修复本次实际复现的对象误删、原生进程崩溃和 BT 视频副本丢失字幕等问题。没有发现尚未修复的确定性代码发布阻塞。本地验证结果见下表；**生产 Docker 镜像的最终验收仍是发布门槛**，本环境没有可连接的 Docker daemon，不能把本地二进制验证等同于生产镜像验证。下列已知限制不建议在发布收尾阶段通过大范围重构解决。

## 已发现并修复

| 问题 | 证据及处理 |
| --- | --- |
| BT 优化视频副本丢失播放及字幕元数据 | 新增回归在修复前复现。迁移 019 仅去掉派生对象键的全局唯一约束，保留每文件主键、外键级联和数据检查。复制事务为新 file_id 创建独立 playback/subtitles 记录，共享 S3 内容键；完整继承轨索引、语言、标题、默认/强制标记、内封/外挂提取对象键、大小和 ETag，以及播放 MIME、时长和编解码器。ingests 仍表示原 BT 任务，不伪造导入记录。删除任一文件只级联其自身关联，现有引用扫描保护仍被其他文件引用的对象。 |
| 混合声道/采样率音频合并触发 Rust 进程 SIGSEGV | 真实 MinIO 媒体验收复现；GDB 定位到 `av_audio_fifo_write`。本机 libav 7 环境中，绑定的 sink 参数 setter 忽略了设置失败，单声道帧进入立体声 FIFO。改为显式 `aformat` 参数并在 FIFO 入口检查格式、声道布局和采样率；新增混合输入回归。修改后完整媒体验收通过。 |
| 上传完成与取消/过期清理竞态，返回成功但对象已被删除 | 新增受控交错回归，修改前复现。按 upload ID 串行化完成和取消；过期扫描在取得锁后重新检查状态。取消先提交元数据，再清理对象；删除 pending 文件和任务取消也走同一路径。锁等待可取消，空闲锁条目会移除。 |
| 文件或上传记录已删除，完成接口仍误报成功 | 修改前回归复现。`finalizeUpload` 检查两个 UPDATE 的受影响行数，任何一个不成立即回滚。创建 multipart 后所有元数据失败路径均尝试 AbortMultipart，包括 BeginTx 失败。 |
| 延迟删除/BT 回滚误删已发布文件及字幕 | 修改前普通引用对象删除回归复现；补充 BT 发布及字幕引用测试。延迟清理和 BT 回滚检查引用，引用集合纳入 `web_media_playback`、`web_media_subtitles`。BT 发布事务检查任务仍处于 importing。 |
| 创建/移动/复制过程与删除交错，产生不可达文件或失去并发保护 | 创建目录、文档、上传和复制时，在插入语句中检查父目录仍可用；移动的环检测和更新处于同一事务；复制在事务内重新读取源文件。父目录在存储请求期间被删除的回归通过。pending 文件删除使用状态条件，避免删除刚完成的 ready 文件。 |
| 文档编辑保留旧 SHA-256，复制丢失完整性字段 | 修改前回归复现。文档创建和编辑校验实际存储内容并保存新哈希；复制保留已有 hash/algorithm。编辑使用原对象键作为原子更新条件，并检查 ready/非删除状态。 |
| 大文件完成校验被浏览器两分钟固定超时反复取消 | 服务端完成操作流式重读全对象，但前端原先仅给 120 秒。只对完成校验取消固定超时，保留 AbortSignal 和用户取消；假时钟测试验证超过三分钟后仍可等待且可显式取消。反向代理的独立超时仍需生产验证。 |
| 上传续传和清理的前端边界问题 | localStorage 内容非数组会抛异常，写入失败会打断上传；已增加校验和降级。续传进度纳入已确认分片。临时 API 故障保留原会话，只有 404 才重建。分片 worker 全部结束后才报告失败，组件销毁停止校验、在途上传和队列继续启动。相关单测通过。 |
| URL 任务重试在数据库报错时解引用空 Result | 先判断 Exec 错误再读取 RowsAffected；使用 SQLite 触发器注入写入失败，确认返回受控错误而非 panic。 |
| 直链下载大小不一致时遗漏对象回滚 | 补上 `discardBlob`，复用既有失败清理队列。 |
| Compose 健康检查依赖的 wget 未装入生产镜像 | 对照 Compose 与 Dockerfile 确认；运行镜像增加 wget。实际镜像健康状态待 CI 验收。 |
| E2E 固定名字导致复跑失败或假阳性 | 第二轮在相同测试数据库复跑时，trace 显示创建目录和移动均命中重名 409；旧文件还可能掩盖上传失败。上传/移动/批量下载用例改用唯一名称，恢复断言只针对本次文件。 |
| 文档仍描述已移除实现和 API | 修正任务查询/取消 API、上传分片确认与完整性校验、BT 本地工作区容量要求、凭据文件读取方式，以及 MKV 外挂字幕确实调用 ffmpeg CLI 的说明。 |

关键实现：

- [上传生命周期与对象引用](../internal/server/server_uploads.go)
- [文件发布、编辑和复制](../internal/server/server_files.go)
- [字幕副本迁移](../internal/database/migrations/019_web_media_copy_references.sql)、[旧数据库升级回归](../internal/database/web_media_copy_test.go)、[BT 副本 HTTP/SQLite/S3 回归](../internal/server/web_media_copy_test.go)
- [延迟清理](../internal/server/server_stream_share.go)、[BT 导入](../internal/server/download_import.go)
- [音频滤镜与回归](../data-plane/src/media_audio.rs)、[原生 FIFO 边界](../data-plane/src/audio_fifo.rs)
- [浏览器上传](../web/src/composables/useUploads.ts)、[上传回归](../web/src/useUploads.test.ts)、[超时回归](../web/src/api.test.ts)

## 实际验证

| 验证 | 结果与边界 |
| --- | --- |
| `go test -race -count=1 -json ./...`（配置真实数据面） | 241 个测试/子测试通过、10 个跳过，没有失败或竞态报告。 |
| BT 副本单元与集成回归 | 0/1/3 条字幕轨、重复复制和复制副本；删除原文件或副本后执行清理、删除原 BT 任务、关闭并重新打开 SQLite 和 Server，再次查询 API 并读取字幕字节；真实 Rust/MinIO 变体验证签名 URL。播放记录写入、第二条字幕写入及 Commit 故障均回滚，没有半成品；旧数据库升级失败也能完整回滚并重试。 |
| 真实 Go ↔ Rust ↔ MinIO 生命周期 | 配置 `DATA_PLANE_TEST_ADDR/TOKEN` 后单独运行 `TestDataPlaneS3Lifecycle`，通过；覆盖 20 MiB 存储、Range/seek、列举、签名和删除。最终全量 race 也配置真实数据面，该测试及两项新 S3 副本子测试均实际执行。 |
| `go vet ./...`、`CGO_ENABLED=0 go build -trimpath ./cmd/server` | 通过。 |
| 前端 Vitest | 12 个测试文件、87 项测试通过。 |
| 前端 ESLint、vue-tsc、Vite 生产构建 | 通过。 |
| Rust fmt、clippy `--all-targets -- -D warnings`、`cargo test --locked`、`cargo build --locked` | 通过；14 项测试，包括真实本地 BT seeder/client 的 tail seek 与 fastresume。 |
| `data-plane/tests/real-integration.sh` | 修复音频崩溃后通过。使用真实 MinIO 和媒体样本，覆盖 H.264/HEVC/VP9、AAC/Opus/FLAC、fMP4、HLS seek、十分钟音画同步、混合音频/单声道/5.1、章节、封面、字幕、多段上传和解压取消/重启。仅在临时副本中修改监听端口，避免占用已有服务。 |
| Chromium Playwright | 最终复验 **23 项全部通过**；在保留旧测试数据的环境中验证了唯一名称修复。本次字幕修复后完整重跑仍为 23 项通过。包含真实文件上传、ZIP、回收站、移动端、阅读器窗口/缓存，以及环境中三本真实 EPUB。阅读器大部分用例使用受控 API mock，不能全部当作真实后端联调。 |
| 依赖审计 | `npm audit --omit=dev` 0 漏洞；cargo-audit 成功；govulncheck 可达符号及导入包均为 0 漏洞。模块层另报告 x/crypto 中 3 项未被本项目导入/调用的 SSH/OpenPGP 通告，未为此扩大依赖升级范围。 |
| Go 可达性分析 | `deadcode -test ./cmd/server ./internal/...` 删除前报告 10 项；删除后输出为空。 |
| `git diff --check` | 通过。 |

本机 Go 为 1.27.1，libavcodec/libavutil 为 61/59；生产 Dockerfile 使用 Go 1.26 和自编译 FFmpeg 5.1。原生回归已纳入 Dockerfile 现有 cargo test 门禁，但该门禁尚未在本环境构建镜像执行。

另外 10 个 Go 媒体测试仍会跳过：测试工厂固定使用不实现 MediaEngine 的 mock，`requireMediaEngine` 会跳过它们。Rust 单测和真实媒体验收补充了部分覆盖，但不能把那些 Go HTTP/PCM 精确性用例记为通过。建议后续接通真实数据面测试工厂。

字幕“启用/关闭”的当前选择只存在于前端组件状态，数据库并无对应字段；本次保留全部已有持久化轨道状态，不引入新设置。迁移保留已有记录，不根据对象键推测并补写旧版本已经缺失元数据的历史副本。

本轮详细日志和临时样本索引保存在 `/tmp/revaro-final-audit/`，不加入仓库。本次复验日志以 `bt-` 开头；S3 副本回归与存储集成共用 `DATA_PLANE_TEST_ADDR`、`DATA_PLANE_TEST_TOKEN`（未配置时明确跳过）。复验入口为仓库现有 Go/npm/cargo 命令、`web/e2e` 和 `data-plane/tests/real-integration.sh`。

## 仍存在，但不建议现在扩大修改

1. **`derived/media/` 孤儿回收不完整。** 周期 GC 扫描 blobs、thumbs、flows；BT 正常回滚会清理派生产物，但永久删除文件后的派生对象可能长期留在 S3。建议另做包含在途 BT 导入保护的回收验证，不在此次收尾中增加未经充分验证的批量删除路径。S3 AbortMultipart 失败后也没有独立持久化 upload ID 的重试队列，仍依赖数据面启动时的过期 multipart 扫描；持续运行期间的遗留分片费用需要另行监控。
2. **SHA-256 尚未覆盖全部历史和 BT 入库路径。** 历史数据按迁移设计允许 NULL hash；BT 导入依赖 piece 校验和返回的对象信息，`commitImported` 未填充逐文件 SHA-256。本次没有对历史对象做全量回读或改变 BT 协议，也不应把现有完整性字段宣传为全库审计完成。
3. **备份和存储容量边界需要生产演练。** 自动备份是 SQLite 一致性快照，恢复历史已删除内容仍需要匹配时间点的 S3 备份/版本；BT 使用本地磁盘，不能以媒体缓存容量推算 BT 占用。已有文档写明恢复匹配要求，本次纠正了“不需要完整本地磁盘”的旧 BT 说明。
4. **长时间和故障组合覆盖仍不足。** 1 TiB/弱网、代理超时、S3 持续故障下任务重试与清理交错、磁盘耗尽和断电恢复没有完整实测。取消/提交的局部回归不能证明整个任务系统在所有重启和重试交错下具有 exactly-once 语义。不建议为此在最后阶段重写任务系统；应补充生产条件的故障注入。
5. **签名 URL 与不可变内容约定。** 单对象 Presigned PUT 在过期前仍是写能力；目前没有条件写/对象版本绑定。服务端完整性检查不等于撤销此前发出的写 URL。本次没有改变 S3 签名协议或引入对象版本迁移。

## 已确认删除的遗留代码

结合 Go 入口、测试可达性分析和调用搜索，已删除：

- 缓存旧包装 `Manager.evictDisk`、`Manager.memoryUsage`。
- 旧 HLS 签名源包装 `startMediaHLSSource`。
- 未使用错误输出 `problemError`、旧任务槽等待 `waitForJobSlot`。
- 旧并行辅助 `parallel`、内容读取包装 `readFileWithLimit`。
- Go 侧旧分片发送 `DataPlane.uploadPart`、分片大小计算 `storeBlobPartSize`，及三项不再使用的 storeBlob 常量。
- 未使用测试辅助 `mockStorage.putBlock`。
- 另经搜索确认无调用的 `failUpload`、旧 `maxBlocksPerRequest`/`maxCompleteBody` 常量，以及前端空实现 `scheduleAutoClear` 和它的无效导出/调用。

未删除历史 SQL 迁移、reader 旧锚点转换、durable task 恢复适配和 HLS 后备路径：这些仍承担升级、恢复或测试接口的职责，不能因名字含 legacy/compat 就安全删除。

## 发布门槛

**BT 字幕副本这一代码阻塞已解除；生产验收门槛仍待同一代码提交的 CI 通过，暂不作无条件的 1.0 放行结论。** 当前不能连接 Docker daemon，因此尚未验证最终镜像的 FFmpeg 5.1 ABI/运行依赖、非 root UID、只读文件系统、tmpfs、Compose 健康检查和停机清理。

按用户要求，在本地验证通过后推送 main 触发现有 CI；publish 依赖 quality 与 container 两个作业成功，不绕过生产验收门槛。包含本次修改的提交必须通过这两个作业，再用实际部署参数验证上传/播放和停止重启。尤其要确认新音频回归在自编译 FFmpeg 下通过、容器变为 healthy，以及反向代理不会中断大文件完成校验。这属于验收门槛，不需要继续重构架构。

# Revaro 冗余代码与精简审查（2026-10-04）

审查对象是基线 `372795c` 加上当前工作区中已完成的四项功能修复。随后按这份审查完成了八项整理；此前的四项功能修复继续保留。以下先记录实施结果，后面保留原审查依据；原审查中的行号和“建议”描述对应整理前源码。

## 八项实施结果

| 项目 | 已完成的改动 |
|---|---|
| 1. 依赖 | 删除 server 的 `tower-http`、media 的无用 `uuid` 开发依赖及失效日志过滤项；删除七项无调用的 `web-sys` feature；锁文件减少五个包 |
| 2. 样式 | 删除旧连接状态灯、动画及任务列表样式，共 86 行；更新样式加载说明，保留原层叠顺序 |
| 3. HTTP | GET 与其他 JSON 请求共用发送和解码逻辑；统一 `RequestError` 与登录错误，保留 TOTP 判断和登录传输错误文案；删除完整转发的 timeout helper |
| 4. Serde | 新增内部 `serde_helpers::null_default`，替代 20 份同模式 helper；保留缺字段/null 兼容、chunk 的 `-1` 缺省值及历史 anchor 迁移 |
| 5. 小函数 | 删除重复文件查询、未使用的 `read_asset`、恒 true 的重试判断及重复图标；缩略图 URL 集中到 `components/resource_url.rs` |
| 6. 播放器 | 新增 `components/playback.rs`，共享进度读取、恢复状态、过期响应检查、保存、音量偏好及两种计时策略；保留各播放器的零位置、结束重播、本地回退和 keepalive 策略 |
| 7. 书库 SQL | 新增 `library_routes/query.rs`，使用候选行 CTE/JOIN 共用筛选、计数与分页，通过具名 `Candidate` 映射附加字段；保留纳秒活动排序、ETag 检查及系列分组 |
| 8. 大组件 | 阅读器拆为 loading/layout/anchors/navigation/persistence/paging；文件页拆为 loading/actions/navigation/downloads/tiles；内容库拆为 loading/collections/cards/home；清理 268 处无意义的自身赋值绑定 |

### 规模变化

统计以整理前工作区快照为起点，包含新模块，避免把移动代码算成删除。

| 主组件 | 整理前 | 整理后 |
|---|---:|---:|
| `components/file_browser.rs` | 2,988 行 | 1,713 行 |
| `components/reader.rs` | 3,059 行 | 1,166 行 |
| `components/content_shell.rs` | 1,030 行 | 575 行 |

`crates/` 下 Rust 源码总计 51,995 → 51,991 行，减少 5,116 字节；CSS 减少 86 行、1,487 字节。锁文件 382 → 377 个包。模块拆分增加了明确的控制器接口，因此整体行数变化很小，主要收益是重复实现减少、职责与状态生命周期更容易检查。未测量 release 产物体积或 SQL 性能，不能据此声称运行更快。

### 最终验证

- `cargo xtask check` 通过：格式检查、native Clippy（`-D warnings`）、428 项 Rust 测试、WASM all-targets 类型检查。
- `cargo xtask web-build --debug` 与 `cargo build -p revaro-server` 均通过。
- 47 项不同的 Chromium 浏览器测试通过：原 CI 组 21 项、内容库与书籍交互 8 项、界面与上传 17 项，以及新增完整音频预览回归 1 项。
- 浏览器验证覆盖音乐延迟元数据与过期响应、音频预览的服务端/本地续播与结束重播、视频、阅读器目录与进度、分页搜索、文件移动复制、回收站、TOTP 和多个移动端尺寸。
- 新增音频用例首次运行时误用了普通文件页入口（该入口打开音乐底栏）。修正为回收站的完整音频预览入口后通过；该用例显式提供进度响应，因真实进度 API 排除回收站文件。
- `git diff --check` 通过。验证使用三个独立临时数据目录，没有使用项目现有用户数据。

日志：`/tmp/revaro-slim-check.log`、`/tmp/revaro-slim-build.log`；浏览器日志和报告保存在 `/tmp/revaro-slim-e2e-esn9pbzu/`，修正后的音频回归日志为 `audio-tests.log`。

## 结论与顺序

主要问题是旧功能残留和相同基础逻辑分散在多个模块。建议先处理依赖、无用样式、HTTP 响应解码、Serde helper 和完全相同的小函数，再分步整理播放器、书库 SQL 和大组件。

以下内容是维护性改进，不代表每一处重复都会导致运行错误。模块拆分带来的收益主要是可读性和修改范围缩小，不能直接等同于性能提升。

## 1. 无用依赖及浏览器 API 配置

- `crates/revaro-server/Cargo.toml:42` 的 `tower-http` 没有生产或测试调用。源码里唯一的 `tower_http` 字符串是 `main.rs:179` 的日志过滤项，静态文件、请求限制与安全中间件均使用项目自己的实现。
- `crates/revaro-media/Cargo.toml:19` 的开发依赖 `uuid` 没有测试调用。
- `crates/revaro-web/Cargo.toml:70` 起的 `HtmlTrackElement`、`TextTrack`、`TextTrackCueList`、`TextTrackList`、`TextTrackMode`、`VttCue` 是字幕 API 残留；`MessageEvent` 也没有应用调用。

建议移除两项无用依赖、七项不再使用的 `web-sys` feature，并顺便清理失效日志过滤项。删除开发依赖 `uuid` 不会移除工作区其他模块实际使用的 UUID 支持。

验证：在独立源码副本中完成这些 manifest 删减，运行以下两项均通过：

```sh
cargo check --offline --workspace --all-targets
cargo check --offline -p revaro-web --target wasm32-unknown-unknown
```

临时锁文件从 382 个包减少至 377 个包，移除了 `tower-http`、`async-compression`、`compression-codecs`、`compression-core`、`http-range-header`。这是依赖图减少量，尚未测量 release 二进制或 WASM 体积变化。

## 2. 已退出界面的样式仍被加载

- `crates/revaro-web/static/styles/shell.css:253` 的 `.connection`、`.connection i`，及该文件移动端的 `.connection` 规则。
- `crates/revaro-web/static/styles/browser.css:31` 至连接状态灯的两个动画结束位置，保留了对应状态灯及其动画。
- `crates/revaro-web/static/styles/shell.css:356` 的 `.task-list`、`.task-list article`、`.task-icon`。

当前 Rust 组件、静态入口和启动脚本均没有引用这些类。`ui.css:11` 的历史清理说明已经提到旧任务图标，但 `shell.css` 中同类规则仍然存在。

建议先删除这些明确没有界面调用的规则，再更新 `logic/stylesheet.rs` 的说明。其他层叠覆盖需要按选择器和媒体查询核对；仅凭“重复出现”不能判定可删。上述样式删减尚未进行浏览器视觉验证。

## 3. HTTP 客户端的请求与解码重复

位置：`crates/revaro-web/src/api.rs:776`、`:806`、`:836`、`:880`，以及登录请求 `:640`。

- `get_json_with_request` 和 `send_json` 的发送、成功响应解码、失败映射行为相同，前者还内联复制了一份 `decode_request_error`。
- `api_request_timeout` 只是完整转发给参数相同的 `api_request_with_timeout`，没有额外策略。
- `LoginError` 与 `RequestError` 的字段相同；登录函数又复制了相同的错误信封解码。

建议 GET 和其他 JSON 请求共用 `send_json`，保留一个有超时及取消参数的 builder helper，并统一错误信封解码。登录的 TOTP 判断可以作为错误类型的方法保留；合并时需要保留当前登录与普通请求的传输错误文案差异。

具体 endpoint 的强类型 wrapper 仍有价值，它们可以明确请求/返回类型。收益来自合并发送及解码实现。

## 4. 共享类型的 null 兼容 helper 重复

位置：`crates/revaro-core/src/api.rs:19`、`model.rs:283`、`media.rs:8`、`reader.rs:18`。

这四个模块合计有 20 份同一模式的实现：反序列化 `Option<T>`，遇到 null 返回 `T::default()`。类型分别是字符串、数字、布尔值、数组和时间戳，其中部分函数仅名字不同。

建议放到一个内部 Serde 工具模块：

```rust
pub(crate) fn null_default<'de, D, T>(deserializer: D) -> Result<T, D::Error>
where
    D: serde::Deserializer<'de>,
    T: serde::Deserialize<'de> + Default,
{
    Ok(Option::<T>::deserialize(deserializer)?.unwrap_or_default())
}
```

调用字段继续保留 `#[serde(default)]` 和 null 兼容。`reader.rs:47` 的 chunk 缺省值是 `-1`，以及旧 anchor 迁移、枚举响应容错等规则，仍应单独保留。已有缺字段/null/非法类型测试适合验证这次整理。

## 5. 完全重复或没有实际意义的小函数

| 位置 | 现状 | 建议 |
|---|---|---|
| `revaro-server/src/file_routes.rs:194`、`:211` | `lookup_file_for_commit` 和 `lookup_file_any` 的签名、SQL、解码及错误映射完全相同 | 上传和创建文档调用统一使用 `lookup_file_any` |
| `revaro-server/src/web.rs:209` | `read_asset` 在仓库中没有任何调用，注释所称的测试也没有使用它 | 删除函数和仅为它引入的 `AsyncReadExt` |
| `revaro-web/src/components/uploads.rs:1380` | `retryable` 忽略参数并恒返回 true；唯一调用中的 `!retryable(&error)` 永远为 false | 删除 helper 和恒假的分支，保留原重试次数、取消和最终错误处理 |
| `revaro-web/src/components/file_browser.rs:2839`、`media.rs:850`、`video.rs:898` | 三份 `thumbnail_url` 完全相同 | 统一到浏览器侧资源 URL helper，保留 ETag 编码和版本参数 |
| `revaro-web/src/components/icons.rs:73`、`:149` | `close_square` 和 `x` 产生相同 SVG | 调用方统一使用 `x`，删除重复实现及旧任务注释 |

## 6. 三种播放器重复管理相同生命周期

位置：`crates/revaro-web/src/components/audio.rs:99`、`video.rs:117`、`music_player.rs:53`、`:148`。

音频预览、视频预览和音乐底栏分别实现了进度读取、元数据就绪、恢复、保存、音量偏好和结束清理。音频/视频还分别维护相同的 `safe_duration` 和 `clear_timer`；视频的保存调度函数与音频组件内闭包做的是同类工作。

音乐底栏刚增加的文件身份检查、响应修订号和恢复前写入门控，说明这部分生命周期值得集中管理。建议先抽出：

1. 时间数值处理、保存位置到恢复位置的计算、音量偏好读取。
2. 两种明确的计时策略：重置等待的本地保存、已有计时器时不重复安排的远端保存。
3. 当前文件身份、恢复状态、过期响应检查与进度持久化。

章节、视频全屏/手势和音乐队列仍由各组件管理。各组件目前还存在主动跳到零、接近结束时重新开始、localStorage 回退、keepalive 保存等策略差异，抽取时应明确参数和状态，而不是直接替换成同一套默认行为。

需要结合现有媒体浏览器测试和新增音乐竞态回归验证，分步处理。

## 7. 书库查询可以减少重复子查询并改善可读性

位置：`crates/revaro-server/src/library_routes.rs:93`。

同一次列表查询多次关联读取 `library_items`、`library_state`、阅读进度和媒体元数据，并在字段、分组和排序里重复插入 series/最近活动表达式。结果读取依赖 `row.get(15)` 至 `row.get(23)` 的列序号，字段调整比较难检查。

建议建立字段清晰的候选行 CTE/适当的 JOIN，把状态、匹配当前 ETag 的元数据和进度集中映射到具名行结构，再分别处理普通分页和系列分组分页。SQL 使用多行字符串，映射移入具名函数。

这属于设计整理，尚未比较查询计划或性能。需要保持当前纳秒精度的活动排序、缺历史排除、ETag 一致性、集合位置和系列组分页行为，并运行现有对应回归。

## 8. 大组件可以按职责拆分

- `crates/revaro-web/src/components/file_browser.rs`：2,988 行，包含加载、文件操作、预览、对话框、下载及路由协调。
- `crates/revaro-web/src/components/reader.rs`：3,059 行，包含阅读 UI、DOM anchor/布局测量、导航、翻页和进度保存。
- `crates/revaro-web/src/components/content_shell.rs`：1,030 行，包含导航、内容库加载、集合管理、批量操作、卡片和首页。

建议先沿现有函数边界移动：阅读器拆 DOM anchor/布局/持久化，文件页拆加载与操作控制器、卡片和下载 helper，内容库拆列表控制器、集合管理和卡片/首页视图。先保持状态所有权和挂载生命周期，再逐步调整接口；纯移动文件主要改善阅读和后续维护，不保证总行数减少。

## 原审查验证范围（实施前）

进行了源码调用检索、相同函数体比对和 manifest/样式引用核对。仅依赖及浏览器 feature 删减在临时副本中通过了 native all-targets 与 WASM 类型检查；其他建议尚未实施或重新运行浏览器测试。

验证日志：`/tmp/revaro-redundancy-native.log`、`/tmp/revaro-redundancy-wasm.log`。临时副本已删除，当前工作区保留此前的四项功能修复。

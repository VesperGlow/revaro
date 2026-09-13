# Rust server 与媒体边界

Revaro 现在由一个 `revaro` 进程提供 HTTP 服务。`revaro-server` 负责认证、
SQLite 事务、本地对象存储、任务状态和响应语义；`revaro-media` 是同一进程
内的阻塞媒体库，负责 FFmpeg 探测/缩略图/字幕转换和有界归档解压。

## 数据与文件

所有应用数据留在本地磁盘：

- SQLite 元数据位于 `APP_DATA_DIR/revaro.db`；
- 原文件和不可变派生对象位于 `APP_DATA_DIR/objects`；
- 解压临时目录、字幕缓存和可回收工作文件位于 `APP_WORK_DIR`。

对象提交在内容、大小、完整性哈希和 SQLite 事务都成功后才对外可见。上传的
取消与完成相互串行；永久删除在同一个事务中排入 blob、缩略图和阅读 flow 的
清理队列。清理失败会保留队列项，供重试和下次启动恢复；孤立对象扫描只作为
上传中止和晚到派生缓存的兜底回收。

对象键先经过共享校验，再由 `LocalStore` 解析到对象根目录下的相对路径。读取
拒绝符号链接、目录和路径穿越；写入使用同目录临时文件、`fsync` 和原子替换，
不可变派生对象采用只创建语义，避免并发生成互相覆盖。

## 媒体与归档

服务端先完成认证、文件查找和对象句柄校验，再把已经打开的本地对象交给
`MediaEngine` 或 `ArchiveEngine`。阻塞操作运行在专用 worker 上，并受媒体并发
信号量、归档单槽位和取消令牌限制；请求断开与优雅停机都会停止对应工作。

媒体能力包括：

- FFmpeg 容器、音视频流、章节和内嵌字幕探测；
- 视频首帧、音频内嵌封面和图片/EPUB 缩略图生成；
- 外置 SRT/ASS 与内嵌字幕转 WebVTT；
- `libarchive` 有界读取、路径规范化、条目/展开大小限制、密码等待、取消和
  结果导入。

服务器直接提供原文件 Range 播放和下载，不做转码、HLS、音视频合并或远程对象
存储签名。生产镜像只携带运行所需的 FFmpeg shared libraries；没有独立的
data-plane 可执行文件，也没有回环 bearer token 或子进程监督协议。

## 构建与运行

`cargo xtask build` 先生成 `wasm32` Leptos bundle，再构建 release `revaro`
二进制。Dockerfile 在与运行层匹配的 FFmpeg 前缀上运行 workspace 检查和发布
构建，最终镜像只复制 `revaro`、`dist/web` 和必要的 native libraries。

容器以 UID/GID 10001 运行，`/data` 是唯一持久卷；`APP_WEB_DIR` 指向镜像内的
`/opt/revaro/web`。Compose 的只读根文件系统、`/tmp` tmpfs、无额外 capability
和 `/readyz` healthcheck 与单进程模型一致。

# Rust server、媒体与本地存储

Revaro 由一个 `revaro` 进程提供 HTTP 服务。`revaro-server` 负责认证、SQLite
事务、本地对象存储、上传任务和响应语义；`revaro-media` 在同一进程内负责媒体
探测与缩略图生成。

## 数据与文件

容器中的 `/data`、`/objects` 和 `/caches` 是平级目录，分别由独立卷挂载：

- `/data/revaro.db` 保存 SQLite 元数据和账户配置，适合放在高 I/O 存储上；
- `/objects` 保存原文件、上传分片和持久化派生对象；
- `/caches` 保存可重建的媒体、阅读缓存和应用临时工作文件。

数据库和对象卷都需要备份；缓存卷可以清空。Compose 的三个命名卷默认可能仍
落在宿主机的同一块磁盘上，物理分离时应将对象卷绑定到目标磁盘目录。

对象提交在内容、大小、完整性哈希和 SQLite 事务都成功后才对外可见。上传的
取消与完成相互串行；永久删除在同一个事务中排入 blob、缩略图和阅读 flow 的
清理队列。清理失败会保留队列项，供重试和下次启动恢复；孤立对象扫描只作为
上传中止和晚到派生缓存的兜底回收。

对象键先经过共享校验，再由 `LocalStore` 解析到对象根目录下的相对路径。读取
拒绝符号链接、目录和路径穿越；写入使用同目录临时文件、`fsync` 和原子替换，
不可变派生对象采用只创建语义，避免并发生成互相覆盖。

## 媒体

服务端先完成认证、文件查找和对象句柄校验，再把已经打开的本地对象交给
`MediaEngine`。阻塞探测和缩略图生成受媒体并发限制与取消令牌控制。

媒体能力包括 FFmpeg 容器、音视频流与章节探测，以及视频首帧、音频内嵌封面和
图片/EPUB 缩略图生成。服务器直接提供原文件 Range 播放和下载，不做转码、
HLS、音视频合并、字幕转换或归档解压。

## 构建与运行

`cargo xtask build` 先生成 `wasm32` Leptos bundle，再构建 release `revaro`
二进制。Dockerfile 在与运行层匹配的 FFmpeg 前缀上运行 workspace 检查和发布
构建，最终镜像只复制 `revaro`、`dist/web` 和必要的 native libraries。

容器以 UID/GID 10001 运行；`APP_WEB_DIR` 指向镜像内的 `/opt/revaro/web`。
Compose 使用只读根文件系统、`/tmp` tmpfs、无额外 capability 和 `/readyz`
healthcheck。

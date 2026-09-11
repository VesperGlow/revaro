# 从 S3 文件存储迁移到本地磁盘

此版本将普通文件、头像、缩略图和阅读派生文件放在 `APP_DATA_DIR/objects`，默认是 `/data/objects`。数据库继续位于 `/data/revaro.db`。文件名和目录由 SQLite 管理；磁盘路径使用原有对象键，例如 `objects/blobs/<UUID>`，重命名文件不改磁盘路径。

## 已有安装

1. 停止旧服务，保留原数据库及数据卷的完整副本。代码备份在远端 `backup` 分支；代码分支不包含用户文件或数据库。
2. 保留原数据库，并将旧 S3 bucket 的对象按原键完整复制到数据卷的 `objects/` 子目录。不要改 UUID，不要只复制数据库。包括 `blobs/`、`profile/`、`thumbs/`、阅读缓存对象以及旧音频播放文件。
3. 确认目标是 Revaro 使用的持久化数据卷，容器 UID/GID `10001:10001` 对该目录有读写权限，且磁盘容量足够。
4. 启动新版本。启动时会逐个核对数据库中原文件和音频流的对象是否存在、大小是否一致；缺失时会报具体对象键并停止启动，先补齐文件再启动。
5. 验证上传、下载、播放、分享和回收站恢复。保留源 S3 对象与旧数据库副本，确认迁移成功后再按自己的保留策略处理。

可使用已有 AWS CLI 配置进行一次性复制（将示例路径、bucket 和 endpoint 替换为真实值）：

```sh
aws --endpoint-url https://your-s3-endpoint s3 sync \
  s3://your-bucket/ /absolute/path/to/revaro-data/objects/ \
  --exclude 'revaro-backups/*'
```

不要添加 `--delete`；不要在旧服务仍写入时做最终同步。AWS 官方 endpoint 可省略 `--endpoint-url`。这只是迁移命令，应用本身没有 S3 文件读写或迁移接口。若原来使用 Compose 自带 MinIO，先用旧版本 Compose 停止 Revaro、保持 MinIO 可读，完成复制后再切换 Compose；新 Compose 不再启动 MinIO。

旧 S3 未完成上传无法续传，本次数据库迁移会清理其 pending 文件记录。已完成文件不改键、不删除。新版本的本地分片上传可以在服务重启后继续；合并、离线下载和转码任务不再恢复。历史迁移文件与已完成任务记录保留，便于旧数据库升级。

## S3 数据库备份

S3 只接收 `revaro-backups/database/revaro-db-<UTC时间>.sqlite`。设置 `S3_BUCKET`、访问凭据及可选 endpoint 后，默认启用备份；可显式设置 `BACKUP_ENABLED=false` 关闭。默认每 24 小时检查一次，保留 14 个快照。快照由 SQLite `VACUUM INTO` 生成，上传失败不影响本地文件服务。

数据库快照不包含本地 blob。灾难恢复需要匹配的数据库和 `objects/` 备份，请同时备份持久化数据卷。单个 S3 数据库快照上限为 5 GiB；超过上限会记录备份失败，文件服务继续运行。

## 回滚

使用 `backup` 分支对应镜像，并恢复升级前的数据库副本和原 S3 配置。不要让旧镜像直接读已迁移的数据库；本次升级重命名了上传表字段。新版本运行后上传的文件仅在本地，回滚前需要另行保存这些新增文件。

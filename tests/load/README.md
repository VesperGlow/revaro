# 个人使用耐用性检查

所有测试数据均可丢弃。负载脚本自行启动服务，使用临时目录和空闲回环端口；
浏览器脚本需要连接专用测试实例，创建并清理自己的夹具目录。
不要将浏览器脚本连接到真实个人资料库。

## 服务端负载与恢复

需要 Python 3 和已构建的 Rust 服务端，不依赖 Python 第三方包。

```sh
cargo build -p revaro-server --release
python3 tests/load/resilience.py --binary target/release/revaro \
  --files 100000 --requests 600 --workers 8 --max-p95-ms 2000 \
  --output /tmp/revaro-resilience-load.json
```

默认创建 100 个目录和 10 万条图片元数据，混合访问目录列表、图片库、最近打开
首页、全盘名称搜索、分享列表和健康检查。各接口 p95 必须在指定预算内，
请求不能失败，返回的数量和分页必须正确。规模夹具不生成图片实体。

随后让 192 个请求同时到达，要求只返回成功或带 `Retry-After: 2` 的 503，
压力结束后原会话仍可使用。可用 `--overload-workers 0` 单独关闭此阶段。

真实数据阶段上传 8 MiB 文件，在收到第一片确认后强制终止服务，重启后继续
原会话；提高服务的磁盘空间准入门槛，验证写入返回 507 且已确认分片仍在。
恢复门槛后重传、完成上传，8 并发下载 16 次并逐一校验 SHA-256，再次强制
终止和重启，验证文件字节、SQLite 完整性及外键。这里没有填满宿主机磁盘，
也没有模拟设备掉电或文件系统故障。

同一真实文件还用于离线备份和恢复演练：运行中的服务必须拒绝备份；停服后
备份、恢复到空目录，再登录读取原文件并核对 SHA-256。覆盖已有数据库或
恢复被篡改的对象必须失败，损坏校验失败不能写入目标数据库和对象目录。

退出码非零即验收失败。JSON 包含二进制指纹、请求延迟、恢复结果及子进程
峰值 RSS（含服务进程和离线管理命令）；输出目录需要事先存在。异常阶段的
结果可能仅包含已完成的部分。

## 浏览器重复操作

专用实例须包含 release 前端。安装 `tests/e2e` 的依赖和 Chromium 后运行：

```sh
cd tests/e2e
npm ci --ignore-scripts
npx playwright install chromium
cd ../..
E2E_BASE_URL=http://127.0.0.1:18080 \
  SOAK_OUTPUT=/tmp/revaro-browser-soak.json node tests/load/browser-soak.cjs
```

默认测试账户为 `admin` / `revaro-e2e-password`，可用 `E2E_USERNAME` 和
`E2E_PASSWORD` 覆盖。本机 Chromium 可通过 `PLAYWRIGHT_EXECUTABLE_PATH` 指定。

脚本执行 10 轮预热和两组各 75 轮操作：打开文档、输入 256 KiB 未保存内容、
关闭并放弃修改，再打开和关闭两层菜单。输入通过 DOM 值和 `input` 事件模拟
粘贴，随后走应用原有的草稿与关闭逻辑。每阶段回收可回收对象，记录 JS 堆、
WASM 内存、DOM 数量和页面异常。

最后 75 轮的 JS 堆和 WASM 增长分别必须小于 16 MiB，DOM 增长不得超过 50，
页面异常必须为零。这是有界重复操作检查，不能代替多日运行或全部用户操作
路径的内存检查。

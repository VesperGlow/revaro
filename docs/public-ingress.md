# Revaro 直接公网入口

Revaro 自身处理 ACME 与 HTTPS，只提供自己的应用、文件及 API，不转发到其他服务。容器内与公网均使用 TCP 80、TCP 443、UDP 443；`compose.host.yml` 使用宿主机网络，不经过端口映射。

## 配置与启动

复制 `.env.example` 为 `.env`，至少填写：

```dotenv
APP_DOMAIN=files.example.com
ACME_EMAIL=admin@example.com
ACME_STAGING=false
QUIC_CC_MODE=aggressive
QUIC_TARGET_MBPS=auto
```

将域名 A/AAAA 指向服务器，开放防火墙和云安全组的 TCP 80/443、UDP 443。若配置 AAAA，IPv6 必须同样可达；当前默认 `0.0.0.0` 监听 IPv4，IPv6 部署可显式设置 `APP_ADDR=[::]:80`、`APP_TLS_ADDR=[::]:443`、`APP_QUIC_ADDR=[::]:443`。不要让不可达的 AAAA 影响 ACME 验证。HTTP-01 必须可从公网 TCP 80 访问，NAT 场景需把该端口转发到本机。[Let's Encrypt HTTP-01](https://letsencrypt.org/docs/challenge-types/)

默认桥接部署：

```sh
docker compose up -d
# 或在完成下述 rootless 主机设置后：
podman-compose up -d
```

域名模式自动推导 `APP_BASE_URL=https://$APP_DOMAIN`、HTTP 80、TCP TLS 443 和 UDP QUIC 443。Compose 中的监听也是这三个端口。`APP_BASE_URL` 若显式填写，必须是该域名的 HTTPS origin；手工 PEM 与 `APP_DOMAIN` 不能同时启用。直接运行二进制时自动读取工作目录 `.env`，进程环境变量优先，且不会修改进程环境。

## rootless Podman 与宿主机网络

rootless 进程绑定低端口受宿主机内核限制。管理员在宿主机执行一次：

```sh
sudo sh -c 'printf "%s\n" "net.ipv4.ip_unprivileged_port_start=80" > /etc/sysctl.d/90-revaro-low-ports.conf'
sudo sysctl --system
```

随后以普通用户启动：

```sh
podman-compose -f compose.host.yml up -d
podman-compose -f compose.host.yml logs -f revaro
```

`compose.host.yml` 是独立文件，不能叠加到默认 Compose；宿主机网络不配置端口映射，也不尝试修改宿主机 sysctl。容器仍使用 UID/GID 10001、只读根文件系统、`cap_drop: ALL` 和 `no-new-privileges`，无需 privileged 或 root 容器。默认桥接 Compose 额外将网络命名空间内的低端口起点设为 80，宿主机的设置负责允许 rootless 发布公网低端口。端口必须没有其他服务占用。[Podman rootless 限制](https://github.com/containers/podman/blob/main/rootless.md)

这项 sysctl 允许宿主机其他普通用户绑定 80 及以上端口；保持原有用户权限管理。要在注销后持续运行 rootless 服务，可按宿主机的 systemd 用户服务策略启用 linger。命名卷由 Podman 管理 UID 映射；如使用宿主机 bind mount，须按用户命名空间映射设置目录所有权，不能直接假定宿主机 UID 10001。

## 证书生命周期

- HTTP 80 的 `/.well-known/acme-challenge/{token}` 返回当前 HTTP-01 key authorization；不存在的 token 返回 404。其他请求 308 跳转固定配置的 HTTPS origin，保留路径、查询和方法，不信任 Host 或转发头。
- TCP 443 使用 rustls，ALPN 为 `h2`、`http/1.1`；UDP 443 使用原有 Quinn 与标准 `h3`。TCP 响应公布 `Alt-Svc: h3=":443"; ma=300`。若有 NAT，`APP_QUIC_PUBLIC_PORT` 可指定公网 UDP 端口。
- `rustls-acme` 的持续状态流负责申请、退避重试和在证书生命周期约三分之二时续期。证书解析器由 TCP 与 QUIC 共享；续期后新连接使用新证书，已有连接继续传输，不重启 listener，也不改变 HTTP/3 协议。[rustls-acme](https://docs.rs/rustls-acme/0.15.4/rustls_acme/)
- `/data/acme` 存储账户密钥和证书/私钥链，目录权限 0700、文件 0600。临时文件先同步到磁盘，再原子替换并同步目录。缓存按域名/联系人与 CA 地址隔离，测试和生产证书不会混用。应持久化整个 `/data` 卷；缓存不可写会导致启动失败。
- 首次申请完成前 HTTPS 握手暂不可用；没有临时自签名生产证书。TLS 健康检查等待证书就绪，启动宽限 300 秒。已有缓存无需 CA 在线即可恢复服务，后台续期失败保留正在使用的证书并继续重试。
- `ACME_STAGING=true` 用于部署试验，证书不受浏览器信任；正式启用时改为 false 并重启。私有 CA 可设置 HTTPS `ACME_DIRECTORY_URL` 与 `ACME_CA_FILE`，仅改变 ACME 客户端的信任根，不关闭 TLS 校验。挂载额外 CA 文件时需确保容器 UID 可读。

## QUIC、共享传输与回退

默认保留 `aggressive + auto`、pacing、有限丢包补偿、每连接及全局上限和严重拥塞时 Cubic 回退。`QUIC_CC_MODE=standard` 可手动切换为 upstream Cubic。配置和实测见 [QUIC 传输说明](quic-transport.md)。所有文件共用相同 Router、Range/ETag、分块上传和浏览器 resilient transport，没有新增按文件类型分类的恢复逻辑。

UDP 不可达或 HTTP/3 握手失败时，浏览器通过相同 TCP 443 回退 HTTP/2。需要应用层固定选择备用 HTTP/2 authority 时，可以配置 `APP_HTTP2_ADDR` 和同域名的 `APP_HTTP2_BASE_URL`；该原生额外 listener 使用同一个 ACME 证书并不公布 Alt-Svc，桥接部署需自行增加对应 TCP 映射。见 [共享传输与回退](file-transport.md)。

本地 HTTP 开发仍可使用 `compose.local.yml`。现有手工 PEM、独立 HTTP/2 端口和 `compose.quic.yml` 诊断配置保留：`docker compose -f compose.local.yml -f compose.quic.yml up -d`，不要同时设置 `APP_DOMAIN`。

## 验证

```sh
cargo test -p revaro-server --test public_ingress
cargo test -p revaro-server --test quic_transport
cargo xtask check
# 已设置宿主机低端口 sysctl 且 80/443 空闲时，以普通用户直接实测：
REVARO_INGRESS_TEST_PUBLIC_PORTS=1 cargo test -p revaro-server --test public_ingress
```

`public_ingress` 使用本地受信任 CA 与真实 HTTPS ACME/JWS 请求，验证 HTTP-01 key authorization、临时签发错误自动恢复、续期时 HTTP/1.1/HTTP/2/HTTP/3 证书切换、旧 HTTP/3 连接继续读取、私有缓存和 CA 离线后重启。`quic_transport` 保持真实 HTTP/3 路由上的任意 MIME 上传、Range、ETag 和校验覆盖。真实公网 Let's Encrypt 签发仍取决于部署域名、DNS 与入站端口可达。

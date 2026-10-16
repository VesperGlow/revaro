# HTTP 服务与外部反向代理

Revaro 只提供普通 HTTP，默认监听 `127.0.0.1:8080`。用 `APP_ADDR=IP:端口` 修改监听地址，IPv6 使用 `[::1]:8080`。HTTPS、HTTP/2、HTTP/3、证书及 HTTP 到 HTTPS 的重定向全部由外部 Caddy/Nginx 管理。Revaro 不创建 UDP 或额外的公网监听器。

## 应用配置

宿主机运行或使用 `compose.host.yml` 时：

```dotenv
APP_ADDR=127.0.0.1:8080
APP_BASE_URL=https://files.example.com
TRUSTED_PROXIES=127.0.0.1/32,::1/128
```

`APP_BASE_URL` 必须与浏览器访问的 origin（协议、域名、端口）一致；登录和上传等写请求的 `Origin` 校验、分享链接均使用这个固定地址，不使用请求的 Host 或转发 Host 构造链接。代理应原样转发 URI、查询参数、Host、Origin 和 Cookie。应用不会向客户端发布内部 HTTP 地址，也不会把健康检查重定向到公网地址。

`COOKIE_SECURE` 留空时根据 `APP_BASE_URL` 自动设置。可信代理的单值 `X-Forwarded-Proto: https` 也会启用 Secure Cookie 和 HTTPS 安全响应头。会话 Cookie 保持 `HttpOnly; SameSite=Lax; Path=/`，不设置 Domain；退出登录和修改密码使用相同的安全属性清除 Cookie。公网部署必须使用 HTTPS，浏览器的 Service Worker 和 SHA-256 上传校验需要安全上下文；本地开发可用 localhost。

`TRUSTED_PROXIES` 默认为空。只填写直接代理及代理链所用的具体 IP/CIDR：

- 只有 socket 对端属于这些网段时，才读取 `X-Forwarded-For`、`X-Real-IP` 和 `X-Forwarded-Proto`。
- `X-Forwarded-For` 从右向左找第一个不可信地址；遇到无效地址停止，不跨过无效节点接受更左侧的值。没有此头时才使用单个 `X-Real-IP`。
- 边缘代理应覆盖外部传入的客户端 IP 及协议头，`X-Forwarded-Proto` 必须是单个 `http` 或 `https`。不要把所有客户端网段都列为可信代理。
- 桥接容器内看到的对端可能是网桥网关或代理容器地址，应按实际对端配置，不能直接照抄宿主机的 loopback 网段。Rootless 网络可能重写源 IP，真实客户端 IP 需要由可信 HTTP 代理提供。

## Compose 与 Quadlet

```sh
cp .env.example .env
# 在 .env 中设置 APP_BASE_URL 和 TRUSTED_PROXIES
docker compose up -d
# 或使用宿主机网络，默认只监听宿主机 loopback
podman-compose -f compose.host.yml up -d
```

默认 Compose 在容器内监听 `0.0.0.0:8080`，仅发布宿主机 `127.0.0.1:8080`。`APP_BIND_IP` 和 `APP_PORT` 控制宿主机发布地址与端口，容器内仍用 8080；`compose.local.yml` 提供相同的 HTTP 配置。宿主机网络使用 `APP_ADDR` 控制监听地址，修改端口时同时把 `APP_BASE_URL` 设置为实际访问地址。若代理运行在同一个容器网络，可直接代理 `http://revaro:8080`，仅允许代理访问该网络。

Rootless Podman Quadlet 示例，保存为 `~/.config/containers/systemd/revaro.container`：

```ini
[Unit]
Description=Revaro HTTP service
After=network-online.target
Wants=network-online.target

[Container]
Image=ghcr.io/vesperglow/revaro:latest
ContainerName=revaro
Network=host
Environment=APP_ADDR=127.0.0.1:8080
Environment=APP_BASE_URL=https://files.example.com
Environment=TRUSTED_PROXIES=127.0.0.1/32,::1/128
Volume=revaro-data.volume:/data
Volume=revaro-objects.volume:/objects
Volume=revaro-caches.volume:/caches
ReadOnly=true
Tmpfs=/tmp:rw,size=1g,mode=1777
NoNewPrivileges=true
DropCapability=all

[Service]
Restart=always

[Install]
WantedBy=default.target
```

同时创建三个对应的 `.volume` 文件（例如 `revaro-data.volume`，内容为 `[Volume]`）。执行 `systemctl --user daemon-reload` 和 `systemctl --user start revaro.service`。不需要低端口 sysctl、证书挂载或网络 capabilities；镜像只声明 HTTP 8080。

## Caddy

宿主机上的 Caddyfile：

```caddyfile
files.example.com {
    reverse_proxy 127.0.0.1:8080 {
        flush_interval -1
    }
}
```

Caddy 默认向上游提供 Host、客户端 IP 和原始协议头，并支持 WebSocket 升级。上游为普通 HTTP；公网协议协商和证书由 Caddy 配置负责。`flush_interval -1` 及时交付流式响应。参见 [Caddy reverse_proxy 官方文档](https://caddyserver.com/docs/caddyfile/directives/reverse_proxy)。

## Nginx

以下 `map` 放在 `http` 上下文，`location` 放在已经配置好公网 HTTPS 与证书的 `server` 内。HTTP 到 HTTPS 的重定向也在外部代理配置中完成。

```nginx
map $http_upgrade $revaro_connection {
    default upgrade;
    ''      '';
}

# 在外部 HTTPS server 块内：
location / {
    proxy_pass http://127.0.0.1:8080;
    proxy_http_version 1.1;
    proxy_set_header Host $http_host;
    proxy_set_header Origin $http_origin;
    proxy_set_header X-Real-IP $remote_addr;
    proxy_set_header X-Forwarded-For $remote_addr;
    proxy_set_header X-Forwarded-Proto $scheme;
    proxy_set_header Upgrade $http_upgrade;
    proxy_set_header Connection $revaro_connection;
    proxy_request_buffering off;
    proxy_buffering off;
    proxy_cache off;
    proxy_redirect off;
    proxy_read_timeout 3600s;
    proxy_send_timeout 3600s;
    client_body_timeout 3600s;
    client_max_body_size 0;
}
```

该示例是直接接收客户端的单层代理，覆盖客户端提供的转发头。多层代理需在边缘清洗这些头，再配置各层可信网段。不要重写 Origin 到内部 HTTP 地址，不要改写 Cookie 的 Domain/Path 或删除 Secure 属性。

默认转发 Range、If-Range、ETag 和条件请求头；关闭缓存及缓冲，保留 206、304、416、Content-Range、Accept-Ranges 和原始正文。代理不要压缩或转换文件 Range 响应，不要用额外切片层改变字节偏移。上传请求直接流向应用，保留 SHA-256 确认、分片重试和断点续传；应用自身的超时、大小与并发限制仍生效。WebSocket 使用 HTTP/1.1 Upgrade 转发，无须应用的额外监听端口。参见 [Nginx proxy 模块](https://nginx.org/en/docs/http/ngx_http_proxy_module.html) 与 [WebSocket 代理说明](https://nginx.org/en/docs/http/websocket.html)。

## 部署验证

1. 访问内部 `http://127.0.0.1:8080/readyz`，应返回 200 JSON，没有重定向。
2. 用公网 HTTPS 地址登录，确认会话 Cookie 的安全属性、分享链接的域名与协议正确，写请求没有 Origin 错误。
3. 上传后暂停/继续、下载续传、音视频播放及跳转，检查 Range 的 206、Content-Range 与 ETag；验证 `If-Range` 失效后按标准返回完整正文。
4. 查看代理访问日志和登录限流行为，确认转发客户端 IP 与实际客户端一致。直接访问后端时伪造转发头不能影响身份。

应用数据目录、SQLite、对象文件和上传分片格式均保持不变。升级只需替换服务程序及前端并调整代理和环境配置，无须数据库或对象迁移；既有数据目录中的历史文件不会被应用主动删除或读取为网络配置。

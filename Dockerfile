# syntax=docker/dockerfile:1.7
FROM node:24-alpine AS web
WORKDIR /src
COPY web/package.json web/package-lock.json ./web/
RUN cd web && npm ci
# 精确复制源文件，绝不携带宿主 node_modules（平台相关原生绑定会覆盖
# 上面 npm ci 安装的 Linux 版本，导致构建失败或产物损坏）
COPY web/index.html web/vite.config.ts web/tsconfig.json ./web/
COPY web/src ./web/src
COPY web/public ./web/public
COPY internal/webui ./internal/webui
RUN cd web && npm run build

FROM golang:1.26-alpine AS backend
WORKDIR /src
COPY go.mod go.sum ./
RUN go mod download
COPY . .
COPY --from=web /src/internal/webui/dist ./internal/webui/dist
RUN CGO_ENABLED=0 GOOS=linux go build -trimpath -ldflags="-s -w" -o /out/revaro ./cmd/server

FROM rust:1.98-bookworm AS dataplane-base
RUN apt-get update && DEBIAN_FRONTEND=noninteractive apt-get install -y --no-install-recommends \
    clang cmake pkg-config ffmpeg zlib1g-dev libbz2-dev liblzma-dev libzstd-dev liblz4-dev \
    libssl-dev libxml2-dev libacl1-dev libavcodec-dev libavformat-dev libavutil-dev libavfilter-dev libswresample-dev libswscale-dev \
    && rm -rf /var/lib/apt/lists/*
WORKDIR /src/data-plane
COPY data-plane/Cargo.toml data-plane/Cargo.lock ./
COPY data-plane/src ./src

FROM dataplane-base AS dataplane
RUN cargo build --locked --release && cp target/release/revaro-data-plane /out-revaro-data-plane

FROM debian:bookworm-slim
# chromium：服务端固定分页的排版引擎（headless，--no-sandbox 由分页器注入；
# 非 root + cap_drop:ALL 容器内无用户命名空间，见 internal/reader/layout）。
# fonts-noto-cjk：webfont 子集之外的 CJK 字符回退，与服务端分页观感一致。
RUN apt-get update && DEBIAN_FRONTEND=noninteractive apt-get install -y --no-install-recommends \
    ca-certificates tzdata ffmpeg libavcodec59 libavformat59 libavutil57 libavfilter8 libswresample4 libswscale6 \
    chromium fonts-noto-cjk \
    && rm -rf /var/lib/apt/lists/* \
    && groupadd --system --gid 10001 revaro \
    && useradd --system --uid 10001 --gid revaro --no-create-home revaro \
    && mkdir -p /data/.cache /data/work && chown -R revaro:revaro /data
COPY --from=backend /out/revaro /usr/local/bin/revaro
COPY --from=dataplane /out-revaro-data-plane /usr/local/bin/revaro-data-plane
ENV HOME=/data \
    XDG_CACHE_HOME=/data/.cache \
    APP_WORK_DIR=/data/work \
    REVARO_CHROME_BIN=/usr/bin/chromium
USER revaro
VOLUME ["/data"]
EXPOSE 8080 51413/tcp 51413/udp
ENTRYPOINT ["/usr/local/bin/revaro"]

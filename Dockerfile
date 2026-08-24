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

FROM golang:1.25.13-alpine AS backend
WORKDIR /src
COPY go.mod go.sum ./
RUN go mod download
COPY . .
COPY --from=web /src/internal/webui/dist ./internal/webui/dist
RUN CGO_ENABLED=0 GOOS=linux go build -trimpath -ldflags="-s -w" -o /out/revaro ./cmd/server

FROM alpine:3.22
RUN apk add --no-cache ca-certificates tzdata ffmpeg 7zip \
    && addgroup -S -g 10001 revaro \
    && adduser -S -D -H -u 10001 -G revaro revaro \
    && mkdir -p /data \
    && chown revaro:revaro /data
COPY --from=backend /out/revaro /usr/local/bin/revaro
USER revaro
VOLUME ["/data"]
EXPOSE 8080 51413/tcp 51413/udp
ENTRYPOINT ["/usr/local/bin/revaro"]

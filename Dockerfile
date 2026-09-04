# syntax=docker/dockerfile:1.7
FROM node:26-alpine AS web
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

FROM golang:1.27-alpine AS backend
WORKDIR /src
COPY go.mod go.sum ./
RUN go mod download
# 仅复制 Go 源码：README / docs / workflow / data-plane 等与 Go 构建无关的
# 改动不再使本层失效；前端产物由 web 阶段在下一步提供
COPY cmd ./cmd
COPY internal ./internal
COPY --from=web /src/internal/webui/dist ./internal/webui/dist
RUN CGO_ENABLED=0 GOOS=linux go build -trimpath -ldflags="-s -w" -o /out/revaro ./cmd/server

# ---- 精简 FFmpeg 5.1（libav* .59，与 bookworm 数据平面 soname/ABI 一致）----
# Revaro 只在本进程内用 libav（ffmpeg-next）做 probe/thumbnail/fmp4/HLS/音频
# 合并，并用 ffmpeg CLI 做 MKV 外挂字幕 remux。外部库只保留 x264（H.264 编码）
# 与 x265（HEVC 测试片源），两者静态内链；其余全部用 FFmpeg 内建 codec/format/
# filter/protocol。因此运行层不再安装 Debian 完整 ffmpeg 及其依赖树
# （Mesa/libLLVM/libmfx/flite/codec2/libx265… 数百 MB），只带这些 .so 与两个
# 命令行工具。
FROM debian:bookworm-slim AS ffmpeg
ARG FFMPEG_VERSION=5.1.10
ARG FFMPEG_SHA256=392306d6fc45dab0e9e0ea55381e071842e83a2fb31d320aeda40477a7766293
ARG X264_COMMIT=0480cb05fa188d37ae87e8f4fd8f1aea3711f7ee
ARG X264_SHA256=d0967a1348c85dfde363bb52610403be898171493100561efa0dd05d5fd1ae50
ARG X265_COMMIT=419182243fb2e2dfbe91dfc45a51778cf704f849
ARG X265_SHA256=d82fccba4c302b9873ba09d021f9e71bfb85996d90ff4d6a8ba69bcf13dd90b9
RUN apt-get -o Acquire::Retries=5 update \
    && DEBIAN_FRONTEND=noninteractive apt-get -o Acquire::Retries=5 install -y --no-install-recommends \
    build-essential curl ca-certificates xz-utils nasm cmake pkg-config \
    && rm -rf /var/lib/apt/lists/*
WORKDIR /src
RUN curl --retry 5 --retry-all-errors --connect-timeout 30 -fsSL \
      -o ffmpeg.tar.xz https://ffmpeg.org/releases/ffmpeg-${FFMPEG_VERSION}.tar.xz \
    && echo "${FFMPEG_SHA256}  ffmpeg.tar.xz" | sha256sum -c - \
    && curl --retry 5 --retry-all-errors --connect-timeout 30 -fsSL \
      -o x264.tar.gz "https://code.videolan.org/videolan/x264/-/archive/${X264_COMMIT}/x264-${X264_COMMIT}.tar.gz" \
    && echo "${X264_SHA256}  x264.tar.gz" | sha256sum -c - \
    && curl --retry 5 --retry-all-errors --connect-timeout 30 -fsSL \
      -o x265.tar.gz "https://github.com/videolan/x265/archive/${X265_COMMIT}.tar.gz" \
    && echo "${X265_SHA256}  x265.tar.gz" | sha256sum -c - \
    && mkdir -p x264 x265 ffmpeg \
    && tar -xf x264.tar.gz -C x264 --strip-components=1 \
    && tar -xf x265.tar.gz -C x265 --strip-components=1 \
    && tar -xf ffmpeg.tar.xz -C ffmpeg --strip-components=1
# x264（静态，PIC：供链接进 libavcodec.so）
WORKDIR /src/x264
RUN ./configure --prefix=/opt/revaro/x264 --disable-cli --enable-static --enable-pic --disable-opencl \
    && make -j"$(nproc)" && make install
# x265（静态，8bit）
WORKDIR /src/x265/source
RUN cmake -S . -B build \
      -DCMAKE_INSTALL_PREFIX=/opt/revaro/x265 -DCMAKE_BUILD_TYPE=Release \
      -DENABLE_SHARED=OFF -DENABLE_CLI=OFF \
    && cmake --build build -j"$(nproc)" && cmake --install build
# FFmpeg：共享库 + ffmpeg/ffprobe；其余组件全走内建实现
WORKDIR /src/ffmpeg
ENV PKG_CONFIG_PATH=/opt/revaro/x264/lib/pkgconfig:/opt/revaro/x265/lib/pkgconfig
RUN ./configure \
      --prefix=/opt/revaro/ffmpeg \
      --disable-autodetect \
      --disable-doc --disable-debug --disable-ffplay --disable-network --disable-postproc \
      --enable-gpl --enable-libx264 --enable-libx265 \
      --enable-avdevice --enable-shared --enable-pthreads \
      --extra-cflags="-I/opt/revaro/x264/include -I/opt/revaro/x265/include" \
      --extra-ldflags="-L/opt/revaro/x264/lib -L/opt/revaro/x265/lib" \
      --extra-libs="-lstdc++ -lm -lpthread -ldl" \
    && make -j"$(nproc)" && make install \
    && rm -rf /src

# ---- Rust 数据平面（编译与测试都针对上面的精简 libav，与运行层一致）----
FROM rust:1.98-bookworm AS dataplane-base
RUN apt-get -o Acquire::Retries=5 update \
    && DEBIAN_FRONTEND=noninteractive apt-get -o Acquire::Retries=5 install -y --no-install-recommends \
    clang cmake pkg-config zlib1g-dev libbz2-dev liblzma-dev libzstd-dev liblz4-dev \
    libssl-dev libxml2-dev libacl1-dev \
    && rm -rf /var/lib/apt/lists/*
# rustfmt / clippy 组件预装进基础层：CI 检查阶段不再每次临时下载组件
RUN rustup component add rustfmt clippy
# 用 /opt 的 ffmpeg 前缀替换 Debian 完整 libav*-dev / ffmpeg 包
COPY --from=ffmpeg /opt/revaro/ffmpeg /opt/revaro/ffmpeg
ENV PATH=/opt/revaro/ffmpeg/bin:$PATH \
    PKG_CONFIG_PATH=/opt/revaro/ffmpeg/lib/pkgconfig \
    LD_LIBRARY_PATH=/opt/revaro/ffmpeg/lib \
    CARGO_NET_RETRY=10 \
    CARGO_HTTP_TIMEOUT=120
WORKDIR /src/data-plane
COPY data-plane/Cargo.toml data-plane/Cargo.lock ./
# 依赖桩编译层：用空 main 先把全部第三方依赖编译进 target/（release +
# debug/测试 profile 含 dev-deps），随后删除 crate 自身的产物与指纹——
# 下游只能依据真实源码重编 revaro crate，从结构上排除 cargo 依据 mtime
# 把桩二进制误判为"已最新"的可能。Cargo.lock 与工具链不变时该层长期
# 命中；同一 RUN 内清理 registry 解包内容，使该层只保留编译产物。
FROM dataplane-base AS dataplane-src
RUN mkdir src \
    && echo 'fn main() {}' > src/main.rs \
    && CARGO_INCREMENTAL=0 cargo build --locked --release \
    && CARGO_INCREMENTAL=0 cargo test --locked --no-run \
    && rm -rf src \
        target/release/revaro-data-plane target/debug/revaro-data-plane \
        target/release/.fingerprint/revaro-data-plane-* target/debug/.fingerprint/revaro-data-plane-* \
        target/release/deps/revaro_data_plane-* target/debug/deps/revaro_data_plane-* \
        "$CARGO_HOME/registry/src"
COPY data-plane/src ./src

# 把 Rust 的格式、lint 和测试放进生产镜像依赖链。CI 只构建一次完整镜像：
# 检查通过后才会生成 release 二进制，且源码未变化时整层直接命中缓存。
FROM dataplane-src AS dataplane-checked
RUN cargo fmt --check \
    && CARGO_INCREMENTAL=0 cargo clippy --locked --all-targets -- -D warnings \
    && CARGO_INCREMENTAL=0 cargo test --locked

FROM dataplane-checked AS dataplane
RUN CARGO_INCREMENTAL=0 cargo build --locked --release \
    && cp target/release/revaro-data-plane /out-revaro-data-plane

FROM debian:bookworm-slim
# 运行依赖最小化：TLS 证书 / 时区 + libstdc++/libgcc（静态内链 x265 需要）
# + 精简 FFmpeg；不安装 Debian 完整 ffmpeg 及其依赖树。
RUN apt-get -o Acquire::Retries=5 update \
    && DEBIAN_FRONTEND=noninteractive apt-get -o Acquire::Retries=5 install -y --no-install-recommends \
    ca-certificates tzdata libstdc++6 libgcc-s1 libxml2 \
    && rm -rf /var/lib/apt/lists/* \
    && groupadd --system --gid 10001 revaro \
    && useradd --system --uid 10001 --gid revaro --no-create-home revaro \
    && mkdir -p /data/.cache /data/work && chown -R revaro:revaro /data
COPY --from=backend /out/revaro /usr/local/bin/revaro
COPY --from=dataplane /out-revaro-data-plane /usr/local/bin/revaro-data-plane
# 精简 FFmpeg：ffmpeg/ffprobe + 共享库（ldconfig 让 revaro-data-plane 找到 .so）
COPY --from=ffmpeg /opt/revaro/ffmpeg/bin/ffmpeg /opt/revaro/ffmpeg/bin/ffprobe /usr/local/bin/
COPY --from=ffmpeg /opt/revaro/ffmpeg/lib/libav*.so* /usr/local/lib/
COPY --from=ffmpeg /opt/revaro/ffmpeg/lib/libsw*.so* /usr/local/lib/
RUN strip --strip-unneeded /usr/local/bin/ffmpeg /usr/local/bin/ffprobe /usr/local/lib/libav*.so* /usr/local/lib/libsw*.so* 2>/dev/null || true \
    && ldconfig
ENV HOME=/data \
    XDG_CACHE_HOME=/data/.cache \
    APP_WORK_DIR=/data/work
USER revaro
VOLUME ["/data"]
EXPOSE 8080 51413/tcp 51413/udp
ENTRYPOINT ["/usr/local/bin/revaro"]

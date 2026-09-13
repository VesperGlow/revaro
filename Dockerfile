# syntax=docker/dockerfile:1.7

# ---- Minimal FFmpeg shared libraries for native media inspection ----
FROM debian:bookworm-slim AS ffmpeg
ARG FFMPEG_VERSION=5.1.10
ARG FFMPEG_SHA256=392306d6fc45dab0e9e0ea55381e071842e83a2fb31d320aeda40477a7766293
RUN apt-get -o Acquire::Retries=5 update \
    && DEBIAN_FRONTEND=noninteractive apt-get -o Acquire::Retries=5 install -y --no-install-recommends \
    build-essential curl ca-certificates xz-utils nasm cmake pkg-config \
    && rm -rf /var/lib/apt/lists/*
WORKDIR /src
RUN curl --retry 5 --retry-all-errors --connect-timeout 30 -fsSL \
      -o ffmpeg.tar.xz https://ffmpeg.org/releases/ffmpeg-${FFMPEG_VERSION}.tar.xz \
    && echo "${FFMPEG_SHA256}  ffmpeg.tar.xz" | sha256sum -c - \
    && mkdir -p ffmpeg \
    && tar -xf ffmpeg.tar.xz -C ffmpeg --strip-components=1
WORKDIR /src/ffmpeg
RUN ./configure \
      --prefix=/opt/revaro/ffmpeg \
      --disable-autodetect \
      --disable-doc --disable-debug --disable-ffplay --disable-network --disable-postproc \
      --disable-programs --disable-encoders --disable-muxers \
      --enable-avdevice --enable-shared --enable-pthreads \
    && make -j"$(nproc)" && make install \
    && rm -rf /src

# ---- Rust workspace build and verification ----
FROM rust:1.98-bookworm AS rust-base
RUN apt-get -o Acquire::Retries=5 update \
    && DEBIAN_FRONTEND=noninteractive apt-get -o Acquire::Retries=5 install -y --no-install-recommends \
    ca-certificates curl clang cmake pkg-config \
    zlib1g-dev libbz2-dev liblzma-dev libzstd-dev liblz4-dev \
    libssl-dev libxml2-dev libacl1-dev \
    && rm -rf /var/lib/apt/lists/* \
    && rustup target add wasm32-unknown-unknown \
    && rustup component add rustfmt clippy

# Build and runtime use the same FFmpeg sonames. The image is intentionally
# amd64-only, matching the supported publication target in CI.
COPY --from=ffmpeg /opt/revaro/ffmpeg /opt/revaro/ffmpeg
ENV PATH=/opt/revaro/ffmpeg/bin:$PATH \
    PKG_CONFIG_PATH=/opt/revaro/ffmpeg/lib/pkgconfig \
    LD_LIBRARY_PATH=/opt/revaro/ffmpeg/lib \
    CARGO_NET_RETRY=10 \
    CARGO_HTTP_TIMEOUT=120

# xtask invokes wasm-bindgen after compiling the Leptos client. Download the
# pinned static CLI instead of adding it to the workspace dependency graph.
ARG WASM_BINDGEN_VERSION=0.2.128
RUN curl --retry 5 --retry-all-errors --connect-timeout 30 -fsSL \
      -o /tmp/wasm-bindgen.tar.gz \
      "https://github.com/wasm-bindgen/wasm-bindgen/releases/download/${WASM_BINDGEN_VERSION}/wasm-bindgen-${WASM_BINDGEN_VERSION}-x86_64-unknown-linux-musl.tar.gz" \
    && tar -xzf /tmp/wasm-bindgen.tar.gz -C /tmp \
    && install -m 0755 \
      "/tmp/wasm-bindgen-${WASM_BINDGEN_VERSION}-x86_64-unknown-linux-musl/wasm-bindgen" \
      /usr/local/bin/wasm-bindgen \
    && rm -rf /tmp/wasm-bindgen* \
    && wasm-bindgen --version

WORKDIR /src
COPY rust-toolchain.toml Cargo.toml Cargo.lock ./
COPY .cargo ./.cargo
COPY crates ./crates
COPY xtask ./xtask

# Keep the image build itself a release gate. The same workspace check is also
# run by the Rust CI job, while this layer guarantees that a publishable image
# cannot be assembled from a source tree that fails its own checks.
FROM rust-base AS rust-checked
RUN CARGO_INCREMENTAL=0 cargo xtask check

FROM rust-checked AS rust-build
RUN CARGO_INCREMENTAL=0 cargo xtask build

# ---- Runtime ----
FROM debian:bookworm-slim
# libarchive2-sys links its bounded static archive engine against these system
# libraries; FFmpeg itself is copied below as the only media-specific runtime.
RUN apt-get -o Acquire::Retries=5 update \
    && DEBIAN_FRONTEND=noninteractive apt-get -o Acquire::Retries=5 install -y --no-install-recommends \
    ca-certificates tzdata wget libstdc++6 libgcc-s1 \
    libxml2 libssl3 libacl1 zlib1g libbz2-1.0 liblzma5 libzstd1 liblz4-1 \
    && rm -rf /var/lib/apt/lists/* \
    && groupadd --system --gid 10001 revaro \
    && useradd --system --uid 10001 --gid revaro --no-create-home revaro \
    && mkdir -p /opt/revaro/web /data/.cache /data/work \
    && chown -R revaro:revaro /data

COPY --from=rust-build /src/target/release/revaro /usr/local/bin/revaro
COPY --from=rust-build /src/dist/web /opt/revaro/web
# Shared media libraries only; no conversion binaries or sidecar process.
COPY --from=ffmpeg /opt/revaro/ffmpeg/lib/libav*.so* /usr/local/lib/
COPY --from=ffmpeg /opt/revaro/ffmpeg/lib/libsw*.so* /usr/local/lib/
RUN strip --strip-unneeded /usr/local/lib/libav*.so* /usr/local/lib/libsw*.so* 2>/dev/null || true \
    && ldconfig

ENV HOME=/data \
    XDG_CACHE_HOME=/data/.cache \
    APP_WORK_DIR=/data/work \
    APP_WEB_DIR=/opt/revaro/web
WORKDIR /data
USER revaro
VOLUME ["/data"]
EXPOSE 8080
ENTRYPOINT ["/usr/local/bin/revaro"]

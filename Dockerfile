# syntax=docker/dockerfile:1.7

# ---- Minimal FFmpeg shared libraries for native media inspection ----
FROM debian:bookworm-slim AS ffmpeg
ARG FFMPEG_VERSION=5.1.10
ARG FFMPEG_SHA256=392306d6fc45dab0e9e0ea55381e071842e83a2fb31d320aeda40477a7766293
RUN apt-get -o Acquire::Retries=5 update \
    && DEBIAN_FRONTEND=noninteractive apt-get -o Acquire::Retries=5 install -y --no-install-recommends \
    build-essential curl ca-certificates xz-utils nasm cmake pkg-config zlib1g-dev \
    && rm -rf /var/lib/apt/lists/*
WORKDIR /src
RUN curl --retry 5 --retry-all-errors --connect-timeout 30 -fsSL \
      -o ffmpeg.tar.xz https://ffmpeg.org/releases/ffmpeg-${FFMPEG_VERSION}.tar.xz \
    && echo "${FFMPEG_SHA256}  ffmpeg.tar.xz" | sha256sum -c - \
    && mkdir -p ffmpeg \
    && tar -xf ffmpeg.tar.xz -C ffmpeg --strip-components=1
WORKDIR /src/ffmpeg
# PNG artwork in FLAC needs zlib even when external library autodetection is off.
RUN ./configure \
      --prefix=/opt/revaro/ffmpeg \
      --disable-autodetect \
      --disable-doc --disable-debug --disable-ffplay --disable-network --disable-postproc \
      --disable-programs --disable-encoders --disable-muxers \
      --enable-zlib --enable-decoder=png,mjpeg \
      --enable-avdevice --enable-shared --enable-pthreads \
    && make -j"$(nproc)" && make install \
    && rm -rf /src

# ---- Rust workspace build and verification ----
FROM rust:1.99-bookworm AS rust-base
RUN apt-get -o Acquire::Retries=5 update \
    && DEBIAN_FRONTEND=noninteractive apt-get -o Acquire::Retries=5 install -y --no-install-recommends \
    ca-certificates curl clang cmake pkg-config \
    zlib1g-dev libbz2-dev liblzma-dev libzstd-dev liblz4-dev \
    libssl-dev \
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
    CARGO_HTTP_TIMEOUT=120 \
    CARGO_INCREMENTAL=0 \
    CARGO_PROFILE_DEV_DEBUG=0 \
    CARGO_PROFILE_TEST_DEBUG=0

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

# Stable dependency layers survive changes to application sources. Install the
# pinned build-only tool once; it never enters the runtime image.
RUN cargo install cargo-chef --locked --version 0.1.78

FROM rust-base AS rust-planner
COPY crates ./crates
COPY xtask ./xtask
RUN cargo chef prepare --recipe-path recipe.json

FROM rust-base AS rust-dependencies
COPY --from=rust-planner /src/recipe.json ./recipe.json
RUN cargo chef cook --locked --release --package revaro-server --recipe-path recipe.json \
    && cargo chef cook --locked --release --package revaro-web --target wasm32-unknown-unknown --recipe-path recipe.json \
    && cargo chef cook --locked --package xtask --recipe-path recipe.json

FROM rust-dependencies AS rust-build
COPY crates ./crates
COPY xtask ./xtask
# Standalone builds retain their check gate. CI performs the same complete
# checks in its Rust job and gates publication on that job, avoiding a second
# compilation and test run inside Docker.
ARG REVARO_RUN_CHECKS=1
RUN case "$REVARO_RUN_CHECKS" in \
      1) cargo xtask check ;; \
      0) ;; \
      *) echo 'REVARO_RUN_CHECKS must be 0 or 1' >&2; exit 1 ;; \
    esac \
    && cargo xtask build \
    && mkdir -p /artifacts \
    && cp target/release/revaro /artifacts/revaro \
    && cp -a dist/web /artifacts/web \
    && rm -rf target
# Do not export another multi-GB layer of per-commit compiler intermediates.
# The unchanged rust-dependencies layer retains the reusable compiled crates.

# ---- Runtime ----
FROM debian:bookworm-slim
# FFmpeg itself is copied below as the only media-specific runtime.
RUN apt-get -o Acquire::Retries=5 update \
    && DEBIAN_FRONTEND=noninteractive apt-get -o Acquire::Retries=5 install -y --no-install-recommends \
    ca-certificates tzdata wget libstdc++6 libgcc-s1 \
    libssl3 zlib1g libbz2-1.0 liblzma5 libzstd1 liblz4-1 \
    && rm -rf /var/lib/apt/lists/* \
    && groupadd --system --gid 10001 revaro \
    && useradd --system --uid 10001 --gid revaro --no-create-home revaro \
    && mkdir -p /opt/revaro/web /data /objects /caches/.cache \
    && chown -R revaro:revaro /data /objects /caches

COPY --from=rust-build /artifacts/revaro /usr/local/bin/revaro
COPY --from=rust-build /artifacts/web /opt/revaro/web
# Shared media libraries only; no conversion binaries or sidecar process.
COPY --from=ffmpeg /opt/revaro/ffmpeg/lib/libav*.so* /usr/local/lib/
COPY --from=ffmpeg /opt/revaro/ffmpeg/lib/libsw*.so* /usr/local/lib/
RUN strip --strip-unneeded /usr/local/lib/libav*.so* /usr/local/lib/libsw*.so* 2>/dev/null || true \
    && ldconfig

ENV HOME=/data \
    XDG_CACHE_HOME=/caches/.cache \
    APP_OBJECTS_DIR=/objects \
    APP_CACHES_DIR=/caches \
    APP_WEB_DIR=/opt/revaro/web
WORKDIR /data
USER revaro
VOLUME ["/data", "/objects", "/caches"]
EXPOSE 80/tcp 443/tcp 443/udp 8080/tcp
ENTRYPOINT ["/usr/local/bin/revaro"]

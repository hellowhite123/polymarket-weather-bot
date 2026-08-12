# ============================================================================
# Polymarket Rust 执行层 — 多阶段构建 (清华源 + rsproxy 加速)
#
# build context: 项目根目录 (.)
#   docker compose build polymorph
#   或手动: docker build -t ai-weather-polymorph -f polymarket-rust/Dockerfile .
#
# Stage 1: builder — Rust 1.85 编译 + protoc
# Stage 2: runtime — 最小运行时镜像
# ============================================================================

# ── Stage 1: 编译 ──
FROM rust:1.85-slim-bookworm AS builder

# ── apt 镜像源 → 清华源 ──
RUN sed -i 's|deb.debian.org|mirrors.tuna.tsinghua.edu.cn|g; s|security.debian.org|mirrors.tuna.tsinghua.edu.cn|g' \
    /etc/apt/sources.list.d/debian.sources 2>/dev/null || \
  sed -i 's|deb.debian.org|mirrors.tuna.tsinghua.edu.cn|g; s|security.debian.org|mirrors.tuna.tsinghua.edu.cn|g' \
    /etc/apt/sources.list

# protoc (tonic-build 依赖) + OpenSSL (reqwest/ethers TLS)
RUN apt-get update && apt-get install -y --no-install-recommends \
    protobuf-compiler \
    pkg-config \
    libssl-dev \
    ca-certificates \
    && rm -rf /var/lib/apt/lists/*

# ── Cargo 镜像源 → rsproxy (清华源加速 crates.io) ──
# 使用 sparse 协议（cargo 1.68+），更稳定、更快
ENV CARGO_REGISTRIES_CRATES_IO_PROTOCOL=sparse
RUN mkdir -p /usr/local/cargo && \
    cat > /usr/local/cargo/config.toml <<'EOF'
[source.crates-io]
replace-with = 'rsproxy-sparse'

[source.rsproxy]
registry = "https://rsproxy.cn/crates.io-index"

[source.rsproxy-sparse]
registry = "sparse+https://rsproxy.cn/index/"

[registries.rsproxy]
index = "https://rsproxy.cn/crates.io-index"

[net]
git-fetch-with-cli = true
EOF

WORKDIR /build

# ── 依赖缓存层 (改动源码不会触发重编) ──

# Cargo.toml / Cargo.lock (来自 polymarket-rust/polymarket-rust/)
COPY polymarket-rust/polymarket-rust/Cargo.toml \
     polymarket-rust/polymarket-rust/Cargo.lock* \
     ./

# build.rs (tonic-build 编译 proto)
COPY polymarket-rust/polymarket-rust/build.rs ./

# proto 文件 → build.rs 期望的 ./proto/ 位置
RUN mkdir -p proto
COPY proto/polymarket.proto proto/

# 创建占位 src 让 cargo 解析依赖树
RUN mkdir -p src/bin && \
    echo "fn main() {}" > src/main.rs && \
    echo "fn main() {}" > src/bin/server.rs

# 编译依赖 (核心缓存层) — 使用国内镜像后通常 30s 内完成
RUN cargo build --release --bin server 2>&1; \
    rm -rf src

# ── 源码编译层 ──

COPY polymarket-rust/polymarket-rust/src/ src/

RUN cargo build --release --bin server

# ── Stage 2: 运行时 ──
FROM debian:bookworm-slim

# ── apt 镜像源 → 清华源 ──
RUN sed -i 's|deb.debian.org|mirrors.tuna.tsinghua.edu.cn|g; s|security.debian.org|mirrors.tuna.tsinghua.edu.cn|g' \
    /etc/apt/sources.list.d/debian.sources 2>/dev/null || \
  sed -i 's|deb.debian.org|mirrors.tuna.tsinghua.edu.cn|g; s|security.debian.org|mirrors.tuna.tsinghua.edu.cn|g' \
    /etc/apt/sources.list

RUN apt-get update && apt-get install -y --no-install-recommends \
    ca-certificates \
    libssl3 \
    netcat-openbsd \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /app

COPY --from=builder /build/target/release/server /app/polymorph

RUN mkdir -p /app/data /app/wallets

EXPOSE 50051

ENV RUST_LOG=info
ENV GRPC_ADDR=0.0.0.0:50051
ENV CHAIN_ID=137
ENV CLOB_HOST=https://clob.polymarket.com

ENTRYPOINT ["/app/polymorph"]
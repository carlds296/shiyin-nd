# 网页版镜像：多阶段构建
#   阶段一 rust:1-bookworm 里交叉编译 x86_64-unknown-linux-musl 静态二进制
#   阶段二 alpine:3.20 + python3 跑 web.py，体积小、无 glibc 依赖
#
# 关键点：CC 设为 musl-gcc，但【不要】覆盖 linker（交给 Rust 自带的 rust-lld）。
# 早期版本用 .cargo/config.toml 把 linker 指到 Ubuntu 的 musl-gcc，
# 与 Rust 内置 musl std 不匹配，产物一启动就段错误（连 --help 都崩）。

FROM rust:1-bookworm AS builder

RUN apt-get update \
 && apt-get install -y --no-install-recommends musl-tools \
 && rm -rf /var/lib/apt/lists/*

RUN rustup target add x86_64-unknown-linux-musl

WORKDIR /src
COPY Cargo.toml Cargo.lock ./
COPY src ./src

ENV CC_x86_64_unknown_linux_musl=musl-gcc
RUN cargo build --release --target x86_64-unknown-linux-musl --bin shiyin-nd

FROM alpine:3.20

RUN apk add --no-cache python3 ca-certificates tzdata

COPY --from=builder /src/target/x86_64-unknown-linux-musl/release/shiyin-nd /usr/local/bin/shiyin-nd
COPY web.py /app/web.py

# 容器常以非 root uid 运行（如 1000:10），web.py 必须对所有人可读可执行，
# 否则会报 [Errno 13] Permission denied
RUN chmod 755 /usr/local/bin/shiyin-nd /app /app/web.py \
 && mkdir -p /music /config

ENV SHIYIN_BIN=/usr/local/bin/shiyin-nd \
    SHIYIN_CONFIG_DIR=/config \
    SHIYIN_MUSIC_DIR=/music \
    WEB_PORT=8090 \
    PYTHONUNBUFFERED=1

EXPOSE 8090
WORKDIR /music
CMD ["python3", "/app/web.py"]

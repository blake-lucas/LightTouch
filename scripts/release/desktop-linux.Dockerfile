# The desktop app and firmwarekit for Linux, built against glibc 2.31 (Debian 11) so they run on the same systems
# as the engine and the AppImage (glibc 2.31+). See scripts/release/build-desktop-linux.sh.
FROM debian:bullseye-slim
# Debian 11 is past its end of life: its packages are on archive.debian.org.
RUN printf 'deb http://archive.debian.org/debian bullseye main\ndeb http://archive.debian.org/debian-security bullseye-security main\n' \
        > /etc/apt/sources.list && rm -f /etc/apt/sources.list.d/* \
    && apt-get update && apt-get install -y --no-install-recommends \
        ca-certificates curl build-essential pkg-config perl make libasound2-dev \
    && rm -rf /var/lib/apt/lists/*
ENV RUSTUP_HOME=/opt/rustup CARGO_HOME=/opt/cargo PATH=/opt/cargo/bin:$PATH
ARG RUST_VERSION=1.99.1
RUN curl -sSf https://sh.rustup.rs | sh -s -- -y --profile minimal --default-toolchain $RUST_VERSION \
    && chmod -R a+rwX /opt/cargo /opt/rustup

# Build image for build-tarball-ubuntu.sh: Ubuntu 22.04 (glibc 2.35) + build dependencies + Rust.
FROM docker.io/library/ubuntu:22.04
ARG RUST_TOOLCHAIN
ENV DEBIAN_FRONTEND=noninteractive RUSTUP_HOME=/opt/rustup CARGO_HOME=/opt/cargo PATH=/opt/cargo/bin:$PATH
# Rootless podman without subuid ranges maps only root: apt cannot drop to _apt and chown to other ids fails.
RUN echo 'APT::Sandbox::User "root";' > /etc/apt/apt.conf.d/99root
RUN mv /bin/chown /bin/chown.real && printf '#!/bin/sh\n/bin/chown.real "$@" || true\n' > /bin/chown && chmod +x /bin/chown \
    && apt-get update && apt-get install -y --no-install-recommends \
      build-essential pkg-config curl ca-certificates git clang \
      libasound2-dev libxkbcommon-dev libxkbcommon-x11-dev libxcb1-dev libwayland-dev \
      libfontconfig-dev libfreetype-dev \
    && mv /bin/chown.real /bin/chown && rm -rf /var/lib/apt/lists/*
RUN curl -fsSL https://sh.rustup.rs | sh -s -- -y --profile minimal -c rustfmt -c clippy --default-toolchain "$RUST_TOOLCHAIN"

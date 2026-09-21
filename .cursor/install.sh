#!/usr/bin/env bash
# Idempotent Cloud Agent bootstrap for the fc-gpui Rust workspace.
# Installs the Linux native libraries needed to build and run the GPUI
# framework (X11 + Wayland + wgpu GL/Vulkan), then warms the Cargo build.
set -euo pipefail

SUDO=""
if [ "$(id -u)" -ne 0 ]; then
  SUDO="sudo"
fi

export DEBIAN_FRONTEND=noninteractive

# Native dependencies mirror the CI matrix in
# .github/workflows/gpui-feature-matrix.yml. `libstdc++-14-dev` is required
# because the base image's `cc` is clang, which selects the GCC 14 toolchain
# directory when locating `libstdc++.so` at link time.
$SUDO apt-get update
$SUDO apt-get install --yes \
  build-essential \
  g++ \
  libstdc++-14-dev \
  libwayland-dev \
  libxcb1-dev \
  libxkbcommon-dev \
  libxkbcommon-x11-dev \
  mesa-vulkan-drivers \
  libegl1 \
  libgles2 \
  libgl1-mesa-dri \
  pkg-config \
  xvfb

# Fetch and pre-build against the committed lockfile so agents start hot.
cargo fetch --locked
cargo build --workspace --lib --tests --locked

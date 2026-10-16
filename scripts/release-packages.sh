#!/usr/bin/env bash
# Single source of truth for the workspace release crates, in topological
# order (dependencies before dependents). This is both the safe order for
# sequential `cargo publish` and the archive verification order.
# Sourced by scripts/verify-release-archives.sh and scripts/publish-release.sh.
RELEASE_PACKAGES=(
    fc-gpui-collections
    fc-gpui-perf
    fc-gpui-util-macros
    fc-gpui-util
    fc-gpui-http-client
    fc-gpui-macros
    fc-gpui-derive-refineable
    fc-gpui-refineable
    fc-gpui-semantic-version
    fc-gpui-sum-tree
    fc-gpui-media
    fc-gpui-core
    fc-gpui-wgpu
    fc-gpui-linux
    fc-gpui-macos
    fc-gpui-windows
    fc-gpui-platform
    fc-gpui
)
readonly RELEASE_PACKAGES

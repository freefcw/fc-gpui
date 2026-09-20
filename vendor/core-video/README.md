# vendor/core-video

Temporary workspace patch of crates.io [`core-video` 0.5.2](https://crates.io/crates/core-video/0.5.2)
(registry checksum `139679cc63eb9504bdbe37e37874b0247136177655f0008588781e90863afa62`).

Upstream: [rust-media/apple-media-rs](https://github.com/rust-media/apple-media-rs) (`core-video`),
MIT OR Apache-2.0.

## Why this exists

`fc-gpui-core`, `fc-gpui-macos`, and `fc-gpui-media` use this crate on macOS
for `CVImageBuffer` / `CVPixelBuffer` (screen-capture frames, `Surface`,
Metal `CVMetalTextureCache`). They do **not** use `core_video::display_link`.

Registry `core-video` 0.5.2 (and current 0.6.1) still *declares* legacy
`block` 0.1 as a required dependency, but the only `use block` is in
`src/display_link.rs`, already gated by the `display-link` feature. Cargo.lock
is target-agnostic, so that unused required dep kept `block` after #43
dropped the scap/cocoa graph.

Zed still depends on `core-video` for `CVPixelBuffer` / Metal textures and
uses `objc2-core-video` only for `PlatformScreenCaptureFrame`. There is no
upstream SHA that dropped `block` on this path. This is a local adaptation.

## Delta vs registry

- Sources are the 0.5.2 tree.
- `block` is optional: `display-link = ["dep:block"]`.
- `publish = false`. Root `[patch.crates-io]` points here.
- Workspace manifests request `default-features = false, features = ["link"]`
  so this crate's Block-based `CVDisplayLink` is not compiled. fc-gpui frame
  pacing stays in `crates/gpui-macos/src/mac/display_link.rs`.

Published `fc-gpui-*` crates still depend on registry `core-video 0.5.2`.
Downstream lockfiles without this patch still see `block`.

Remove this patch when a published `core-video` feature-gates `block`, or
when fc-gpui migrates these types to `objc2-core-video` without the
rust-media crate.

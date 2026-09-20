# vendor/core-graphics2

Temporary workspace patch of crates.io [`core-graphics2` 0.5.2](https://crates.io/crates/core-graphics2/0.5.2)
(registry checksum `4416167a69126e617f8d0a214af0e3c1dbdeffcb100ddf72dcd1a1ac9893c146`).

Upstream: [rust-media/apple-media-rs](https://github.com/rust-media/apple-media-rs)
(`core-graphics` package in that repo), MIT OR Apache-2.0.

## Why this exists

`core-video` 0.5.2 depends on this crate with `features = ["display"]`
(`default-features = false`) for `CGColorSpace` / `CGRect` / `CGSize` used
by `CVImageBuffer`. The only `use block` in this crate is
`src/display_stream.rs`, already gated by `display-stream`. Registry 0.5.2
still hard-depends on `block`, so the lockfile kept it even when
`display-stream` is off.

fc-gpui does not use `CGDisplayStream`. This is a local adaptation; there is
no Zed SHA.

## Delta vs registry

- Sources are the 0.5.2 tree.
- `block` is optional: `display-stream` enables `dep:block`.
- `publish = false`. Root `[patch.crates-io]` points here.

Remove this patch when a published `core-graphics2` feature-gates `block`,
or when `core-video` is no longer in this workspace.

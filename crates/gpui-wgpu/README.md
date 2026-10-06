# FC GPUI WGPU

Internal WGPU renderer crate for [fc-gpui](https://crates.io/crates/fc-gpui).

This package owns the WGPU context, atlas, surface renderer, shader, resource-budget normalization, and device-loss recovery used by the Linux and FreeBSD backends. Applications should normally depend on `fc-gpui` rather than this crate directly.

Licensed under Apache-2.0.

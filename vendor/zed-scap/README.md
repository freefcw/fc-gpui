# vendor/zed-scap

Temporary workspace patch of crates.io [`zed-scap` 0.0.8-zed](https://crates.io/crates/zed-scap/0.0.8-zed)
(registry checksum `b6b338d705ae33a43ca00287c11129303a7a0aa57b101b72a1c08c863f698ac8`).

## Why this exists

`fc-gpui` never compiles `zed-scap` on macOS. Linux / Windows / FreeBSD optional
`screen-capture` uses it; macOS capture is `objc2-screen-capture-kit` in
`fc-gpui-macos`. Registry `zed-scap` still *declares* macOS `cocoa` / `objc` /
`screencapturekit` / `tao-core-video-sys`, so Cargo.lock retained those crates
for every target.

Zed has not dropped that graph: workspace pin
`scap = { git = "https://github.com/zed-industries/scap", rev = "4afea48c3b002197176fb19cd0f9b180dd36eaac" }`
and crates.io `0.0.8-zed` both still list those macOS deps. There is no upstream
SHA to trail.

## Delta vs registry

- Linux / Windows / FreeBSD sources and features (`x11`, `wayland`) are unchanged.
- macOS sources are kept so the crate matches 0.0.8-zed, but the macOS-only
  dependencies are omitted. This workspace does not depend on `zed-scap` under
  `cfg(target_os = "macos")`, so those modules are not compiled here.
- `publish = false`. Root `[patch.crates-io]` points here. Published
  `fc-gpui-core` / `fc-gpui-windows` still depend on registry `zed-scap 0.0.8-zed`;
  downstream lockfiles without this patch still see the cocoa/objc graph.

Remove this patch when a published `zed-scap` no longer declares those macOS
crates, or when Linux/Windows capture no longer needs `zed-scap`.

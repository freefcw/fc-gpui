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
- `sysinfo` is omitted for the same reason: its only call site
  (`utils/mac/mod.rs::is_supported`) belongs to the uncompiled macOS module.
  Registry scap's `sysinfo 0.31` dragged the retired `windows 0.57` chain into
  this workspace's lockfile on every target.
- `windows` is bumped to 0.62 and `rand` to 0.9 so scap's own requirements
  resolve to the workspace's generations. Note: `windows 0.61` still reaches
  this lockfile through `windows-capture 1.4.4` (and `rand 0.8` through
  `oo7`); scap just no longer contributes those requirements. Call sites
  (`HWND`/`HMONITOR` handles, `rand::random`) are unaffected.
- `windows-capture` is capped `<1.5` (matching `fc-gpui-windows`):
  `windows-capture 1.5` changes `Settings::new`'s arity and no longer compiles
  registry scap; the proper fix is a port to windows-capture 2.x.
- `publish = false`. Root `[patch.crates-io]` points here. Published
  `fc-gpui-core` / `fc-gpui-windows` still depend on registry `zed-scap 0.0.8-zed`;
  downstream lockfiles without this patch still see the cocoa/objc graph.

Remove this patch when a published `zed-scap` no longer declares those macOS
crates, or when Linux/Windows capture no longer needs `zed-scap`.

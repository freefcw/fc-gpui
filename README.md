# FC GPUI

[![Crates.io](https://img.shields.io/crates/v/fc-gpui.svg)](https://crates.io/crates/fc-gpui)
[![License](https://img.shields.io/crates/l/fc-gpui.svg)](LICENSE-APACHE)

> `fc-gpui` 0.11.1 is published on crates.io.

A GPU-accelerated UI framework for Rust, forked from [Zed's GPUI](https://github.com/zed-industries/zed). fc-gpui extends the original framework with daemon-mode capabilities, system tray integration, global hotkeys, native notifications, and more — making it suitable for background apps, menu bar utilities, and overlay tools.

## Getting Started

`fc-gpui` 0.11.1 is published on crates.io. The unrelated `adabraka-gpui` registry crate is
the upstream [Augani](https://github.com/Augani/adabraka-gpui) crate at `0.5.1` and does not
carry this fork's releases. Depend on the crates.io package:

```toml
fc-gpui = "0.11.1"
```

Building requires the toolchain pinned in [`rust-toolchain.toml`](rust-toolchain.toml): Rust
`1.97.1` with edition 2024.

To trim image decoder footprint, disable default features and list only the platform and image
formats you need:

```toml
fc-gpui = { version = "0.11.1", default-features = false, features = [
    "font-kit",
    "wayland",
    "x11",
    "image-format-png",
    "image-format-jpeg",
    "image-format-webp",
] }
```

The crate now exposes `image-format-*` features that map directly to `image` crate decoders, plus
`image-rayon` for parallel decoding. SVG rendering remains available separately via `resvg`.

### Import Naming

The published package is `fc-gpui`, but the library target keeps the upstream `gpui` namespace,
so a plain dependency imports as `gpui` and Zed-derived code compiles unchanged:

```rust
use gpui::*;
```

Renaming the dependency is also supported — the GPUI macros (derives, `register_action!`,
`#[gpui::test]`) follow the dependency key, covered by
[`tests/downstream-gpui-alias`](tests/downstream-gpui-alias):

```toml
[dependencies]
gpui = { package = "fc-gpui", version = "0.11.1" }
```

The utility crates (`fc-gpui-util`, `fc-gpui-http-client`, …) import under their matching
`fc_gpui_*` library names.

### Macro Dependency Override

Macro paths are resolved automatically for normal `fc-gpui` dependencies and ordinary
Cargo aliases. If one package directly lists both `fc-gpui` and `fc-gpui-core`, or
uses an alias that is indistinguishable from the normalized package name, select the intended
crate once in the consuming package's `Cargo.toml`:

```toml
[package.metadata.gpui-macros]
crate = "core_ui"
```

The value must be the Rust dependency identifier visible to that package. It applies consistently
to derive macros, `register_action!`, and `#[gpui::test]`. An ambiguous direct dependency graph
without this override fails with an actionable macro error instead of guessing a crate.

## Platform Support

| Feature | macOS | Linux (X11) | Linux (Wayland) | Windows |
|---|---|---|---|---|
| GPU-accelerated rendering | Metal | Vulkan/OpenGL | Vulkan/OpenGL | DirectX |
| System tray icon & menu | Yes | Yes (DBus/SNI) | Yes (DBus/SNI) | Yes (Shell_NotifyIcon) |
| Tray menu actions | Yes | Yes | Yes | Yes |
| Global hotkeys | Yes | Yes (XGrabKey) | No | Yes (RegisterHotKey) |
| Native notifications | Yes (UNUserNotification) | Yes (notify-rust) | Yes (notify-rust) | Yes (Shell balloon) |
| Overlay windows (always-on-top) | Yes | Yes | Yes (`WindowKind::LayerShell`) | Yes |
| Click-through windows | Yes | Yes (Shape ext) | Yes (wl_region) | Yes (WS_EX_TRANSPARENT) |
| Window show/hide | Yes | Yes | Yes | Yes |
| Auto-launch at login | Yes (SMAppService) | Yes (XDG autostart) | Yes (XDG autostart) | Yes (Registry) |
| Single instance lock | Yes (Unix socket) | Yes (Unix socket) | Yes (Unix socket) | Yes (Named mutex) |
| Focused window info | Yes (Accessibility) | Yes (EWMH) | No | Yes (Win32) |
| Permission queries | Yes (Accessibility, Mic) | No | No | No |
| Daemon mode (no dock icon) | Yes | Yes | Yes | Yes |

Before publishing a release, validate these claims on real operating systems with the
[0.9 desktop release smoke checklist](docs/release/0.9-desktop-smoke-checklist.md).

### Rendering Backends

Linux X11 and Wayland now use the internal wgpu renderer ported from Zed's current GPUI stack.
The old Blade renderer and its `macos-blade` compatibility feature have been removed. The default
macOS path continues to use Metal and Windows continues to use DirectX.

This migration is internal to the `fc-gpui` crate. Downstream applications should keep using
`gpui::Application::new()` and the existing `x11`/`wayland` features. See
[`docs/wgpu-migration.md`](docs/wgpu-migration.md) for implementation notes and verification
status.

### Wayland Layer Shell

Create a layer-shell surface with `WindowKind::LayerShell`. The Wayland backend requires
`wlr-layer-shell`; opening the window fails with `LayerShellNotSupportedError` when the selected
backend or compositor does not support that protocol. Compositors older than wlr-layer-shell
version 5 cannot select `exclusive_edge`, so that option is ignored with a warning. A width or
height of zero is passed through to let the compositor choose that dimension.

Applications upgrading from `0.6.x` should follow the
[layer-shell migration and implementation provenance guide](docs/layer-shell-migration.md).

```rust
use gpui::{
    Bounds, WindowBounds, WindowKind, WindowOptions,
    layer_shell::{Anchor, KeyboardInteractivity, LayerShellOptions}, point, px, size,
};

let popup_bounds = Bounds::new(point(px(1520.0), px(820.0)), size(px(320.0), px(220.0)));

let options = WindowOptions {
    window_bounds: Some(WindowBounds::Windowed(popup_bounds)),
    kind: WindowKind::LayerShell(LayerShellOptions {
        namespace: "gpui-popup".to_string(),
        anchor: Anchor::TOP | Anchor::RIGHT,
        margin: Some((px(24.0), px(24.0), px(0.0), px(0.0))),
        keyboard_interactivity: KeyboardInteractivity::None,
        ..Default::default()
    }),
    ..WindowOptions::default()
};
```

Automated coverage for this path focuses on protocol-independent behavior such as layer configure
sizing and protocol enum mapping. Run:

```sh
cargo test -p fc-gpui-core --lib --features test-support layer_shell
cargo check -p fc-gpui --example layer_shell --features wayland
```

## Features

### Core UI Framework
- Hybrid immediate/retained mode rendering
- GPU-accelerated with Metal, Vulkan, OpenGL, and DirectX backends
- Tailwind-style layout and styling API
- Entity-based state management
- Declarative views with the `Render` trait
- Low-level `Element` API for custom rendering
- Async executor integrated with the platform event loop
- Action system for keyboard shortcuts
- Test framework with `#[gpui::test]`
- **Resource profiles** — tune cache sizes and GPU allocations for different app types (desktop, utility, minimal)

### Daemon & Background App Support
- **System tray** — icon, tooltip, and nested menus with action callbacks
- **Global hotkeys** — register system-wide keyboard shortcuts
- **Native notifications** — OS-level notifications on all platforms
- **Overlay windows** — always-on-top transparent windows
- **Click-through windows** — mouse events pass through to windows below
- **Window show/hide** — programmatic visibility control
- **Auto-launch** — register your app to start at login
- **Single instance** — prevent multiple copies with activation signaling
- **Keep alive without windows** — apps stay alive with no visible windows via `QuitMode::Explicit`, set through `Application::with_quit_mode` or `App::set_quit_mode`
- **Focused window info** — query which window the user is focused on
- **Permission status** — check accessibility and microphone permissions
- **In-app toast notifications** — stackable, auto-dismissing toast component (deprecated; use the [fc-ui](https://github.com/freefcw/fc-ui) toast component instead)

## Quick Example

```rust
use gpui::{App, Application, TrayMenuItem};

fn main() {
    Application::new().run(|cx: &mut App| {
        cx.set_quit_mode(gpui::QuitMode::Explicit);
        cx.set_tray_tooltip("My App");

        cx.set_tray_menu(vec![
            TrayMenuItem::Action {
                label: "Settings".into(),
                id: "settings".into(),
            },
            TrayMenuItem::Separator,
            TrayMenuItem::Action {
                label: "Quit".into(),
                id: "quit".into(),
            },
        ]);

        cx.on_tray_menu_action(|id, cx| match id.as_ref() {
            "quit" => cx.quit(),
            _ => {}
        });
    });
}
```

See [`crates/gpui-compat/examples/daemon_app.rs`](crates/gpui-compat/examples/daemon_app.rs) for a full example with overlay windows, settings window, global hotkeys, and notifications.

### Resource Profiles

For lightweight applications (tray icons, status bars, small popups), you can reduce memory usage by selecting an appropriate resource profile:

```rust
use gpui::{Application, AppProfile};

// Minimal profile for tray icons and status bars
Application::new()
    .with_resource_profile(AppProfile::Minimal)
    .run(|cx| {
        // ... your app logic
    });

// Utility profile for settings panels and dialogs
Application::new()
    .with_resource_profile(AppProfile::Utility)
    .run(|cx| {
        // ... your app logic
    });
```

See [`docs/resource-profiles.md`](docs/resource-profiles.md) for detailed guidance on choosing and tuning resource profiles.

## Ecosystem

- [fc-ui](https://github.com/freefcw/fc-ui) — a component library built on `fc-gpui`,
  published as [`fc-ui`](https://crates.io/crates/fc-ui) on crates.io: 85+ components, a theme
  system, an animation framework, and layout utilities. `fc-gpui` provides the renderer and the
  platform capabilities (windows, tray, hotkeys, notifications, daemon mode); `fc-ui` provides
  ready-made components on top of it. Feature requests that can be built by composing `div()`
  and `Styled` belong in fc-ui; changes that need to touch shaders or platform APIs belong here.

## Dependencies

### macOS
- Xcode with macOS components
- Xcode command line tools: `xcode-select --install`

### Linux
- For X11: `libxcb`, `libxkbcommon`
- For Wayland: `libwayland-client`, `libxkbcommon`
- GPU backend: wgpu with Vulkan/OpenGL support
- D-Bus (for system tray via StatusNotifierItem)

### Windows
- Visual Studio Build Tools with C++ workload
- Windows SDK

## License

Apache-2.0

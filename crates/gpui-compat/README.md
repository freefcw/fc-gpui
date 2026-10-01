# Adabraka GPUI

A GPU-accelerated UI framework for Rust, forked from [Zed's GPUI](https://github.com/zed-industries/zed). Adabraka GPUI extends the original framework with daemon-mode capabilities, system tray integration, global hotkeys, native notifications, and more — making it suitable for background apps, menu bar utilities, and overlay tools.

## Getting Started

`fc-gpui` 0.9.0 is published on crates.io. This 0.9.1 release is not published yet. The
unrelated `adabraka-gpui` registry crate is the upstream
[Augani](https://github.com/Augani/adabraka-gpui) crate at `0.5.1` and does not carry this
fork's releases. Depend on the `v0.9.1` tag:

```toml
fc-gpui = { git = "https://github.com/freefcw/fc-gpui.git", tag = "v0.9.1" }
```

Building requires the toolchain pinned in
[`rust-toolchain.toml`](https://github.com/freefcw/fc-gpui/blob/main/rust-toolchain.toml):
Rust `1.97.1` with edition 2024.

To keep the build small, disable default features and opt into only the image formats you need:

```toml
fc-gpui = { git = "https://github.com/freefcw/fc-gpui.git", tag = "v0.9.1", default-features = false, features = [
    "font-kit",
    "wayland",
    "x11",
    "image-format-png",
    "image-format-jpeg",
    "image-format-webp",
] }
```

`image-format-*` features map to the underlying `image` crate decoder features. `image-rayon`
re-enables parallel decoding. SVG support is handled separately through `resvg`.

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

### Platform Support

| Feature | macOS | Linux (X11) | Linux (Wayland) | Windows |
|---|---|---|---|---|
| GPU-accelerated rendering | Metal | Vulkan/OpenGL | Vulkan/OpenGL | DirectX |
| System tray icon & menu | Yes | Yes (DBus/SNI) | Yes (DBus/SNI) | Yes (Shell_NotifyIcon) |
| Tray menu actions | Yes | Yes | Yes | Yes |
| Global hotkeys | Yes | Yes (XGrabKey) | No (protocol limitation) | Yes (RegisterHotKey) |
| Native notifications | Yes (UNUserNotification) | Yes (notify-rust) | Yes (notify-rust) | Yes (Shell balloon) |
| Overlay windows (always-on-top) | Yes | Yes | Yes (`WindowKind::LayerShell`) | Yes |
| Click-through windows | Yes | Yes (Shape ext) | Yes (wl_region) | Yes (WS_EX_TRANSPARENT) |
| Window show/hide | Yes | Yes | Yes | Yes |
| Auto-launch at login | Yes (SMAppService) | Yes (XDG autostart) | Yes (XDG autostart) | Yes (Registry) |
| Single instance lock | Yes (Unix socket) | Yes (Unix socket) | Yes (Unix socket) | Yes (Named mutex) |
| Focused window info | Yes (Accessibility) | Yes (EWMH) | No | Yes (Win32) |
| Permission queries | Yes (Accessibility, Mic) | No | No | No |
| Daemon mode (no dock icon) | Yes (Accessory policy) | Yes | Yes | Yes |

### Rendering Backends

Linux X11 and Wayland use the internal wgpu renderer ported from Zed's current GPUI backend. Blade is
no longer part of the crate. The default macOS renderer is Metal and Windows uses DirectX.

Downstream applications continue to use only `fc-gpui`; the renderer package is an internal
implementation dependency and does not change the `Application::new()` entry point. See
[the WGPU migration notes](https://github.com/freefcw/fc-gpui/blob/main/docs/wgpu-migration.md) for details.

### Accessibility

Adabraka GPUI exposes AccessKit-backed roles, labels, values, focus, and action handlers on
elements. Native adapters connect the generated accessibility tree to macOS Accessibility,
Linux AT-SPI, and Windows UI Automation.

Native adapters are enabled by the default `accessibility` feature. Disable default features to
exclude their platform dependencies; the core accessibility API remains available but inactive.

```sh
cargo run -p fc-gpui --example a11y
```

See [`examples/a11y.rs`](examples/a11y.rs) for focus navigation, accessible actions, numeric
values, switches, and list metadata.

### Wayland Layer Shell

Create a layer-shell surface with `WindowKind::LayerShell`. The Wayland backend requires
`wlr-layer-shell`; opening the window fails with `LayerShellNotSupportedError` when the selected
backend or compositor does not support that protocol. Compositors older than wlr-layer-shell
version 5 cannot select `exclusive_edge`, so that option is ignored with a warning. A width or
height of zero is passed through to let the compositor choose that dimension.

Applications upgrading from `0.6.x` should follow the
[layer-shell migration and implementation provenance guide](https://github.com/freefcw/fc-gpui/blob/main/docs/layer-shell-migration.md).

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

The layer-shell regression tests avoid requiring a real compositor. They cover configure sizing
and protocol enum mapping:

```sh
cargo test -p fc-gpui-core --lib --features test-support layer_shell
cargo check -p fc-gpui --example layer_shell --features wayland
```

## Features

### Core UI Framework (inherited from GPUI)
- Hybrid immediate/retained mode rendering
- GPU-accelerated with Metal, Vulkan, OpenGL, and DirectX backends
- Tailwind-style layout and styling API
- Entity-based state management
- Declarative views with the `Render` trait
- Low-level `Element` API for custom rendering
- Async executor integrated with the platform event loop
- Action system for keyboard shortcuts
- AccessKit accessibility tree and native platform adapters
- Test framework with `#[gpui::test]`

### Daemon & Background App Support (new in Adabraka)
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
- **In-app toast notifications** — stackable, auto-dismissing toast component

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

## Dependencies

### macOS

- [Xcode](https://apps.apple.com/us/app/xcode/id497799835?mt=12) with macOS components
- Xcode command line tools: `xcode-select --install`

### Linux

- Standard development packages for your distro
- For X11: `libxcb`, `libxkbcommon`
- For Wayland: `libwayland-client`, `libxkbcommon`
- GPU backend: wgpu with Vulkan/OpenGL support
- D-Bus (for system tray via StatusNotifierItem)

### Windows

- Visual Studio Build Tools with C++ workload
- Windows SDK

## License

Apache-2.0

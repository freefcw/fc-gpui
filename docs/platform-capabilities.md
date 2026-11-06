# Platform capability matrix

This matrix records the behavior currently implemented by the in-tree GPUI
platforms. `implemented` means the backend performs the operation,
`fallback` means GPUI deliberately supplies a portable behavior, `unsupported`
means the operation is unavailable, and `no-op` means the request is accepted
but has no platform effect. Selected fallible `Platform` operations report
`UnsupportedPlatformOperation`, which callers can identify by downcasting;
the raw-window-handle APIs instead report `HandleError::NotSupported`.
A source link is the authority when this document and the code disagree.

## Application and window operations

| Capability | macOS | Linux Wayland | Linux X11 | Linux headless | Windows |
| --- | --- | --- | --- | --- | --- |
| Application run/quit/restart | implemented | implemented | implemented | test/headless | implemented |
| Activate, hide, hide other apps | implemented | no-op/log | no-op/log | no-op/default | default no-op where unsupported |
| Native window handle | implemented | implemented | implemented | unsupported | implemented |
| Native display handle | implemented | implemented | implemented | unsupported | implemented |
| `on_reopen` | callback can fire | accepted, never fired | accepted, never fired | accepted, never fired | accepted, never fired |
| File prompts | native | xdg-desktop-portal | xdg-desktop-portal | `Ok(None)` without windowing features; otherwise portal path | native |
| Window prompt | native | GPUI fallback when native prompt is absent | GPUI fallback when native prompt is absent | GPUI fallback | native |
| Screen capture | feature/platform dependent | feature/platform dependent | feature/platform dependent | unsupported | feature/platform dependent |
| `PaintSurface` | Metal/CoreVideo | unavailable | unavailable | unavailable | unavailable |

The generic defaults and unsupported error type live in
[`crates/gpui/src/platform.rs`](../crates/gpui/src/platform.rs). Native handle
implementations are in the platform window modules, for example
[`gpui-linux/x11/window.rs`](../crates/gpui-linux/src/linux/x11/window.rs) and
[`gpui-windows/window.rs`](../crates/gpui-windows/src/windows/window.rs).

Linux file prompts are selected by the compiled `wayland`/`x11` features, not
by the active client. Without either feature, open/save prompts resolve to
`Ok(None)`, the same value used for cancellation. With either feature enabled,
even a headless client follows the portal path; headless mode does not guarantee
that file prompts are unavailable or suppressed. Portal failures return errors,
without a GPUI file-picker fallback. See
[`gpui-linux/platform.rs`](../crates/gpui-linux/src/linux/platform.rs).

## Input and text

| Capability | macOS | Linux Wayland/X11 | Windows |
| --- | --- | --- | --- |
| Clipboard and text input | implemented | implemented by backend | implemented |
| IME caret/candidate positioning | implemented | backend-specific | implemented through IMM updates |
| Global hotkeys | native | backend-specific; unsupported operations return errors | native |
| Accessibility | feature-gated | feature-gated | feature-gated |

Windows IME coordinates are converted in
[`gpui-windows/ime_position.rs`](../crates/gpui-windows/src/ime_position.rs) and
applied from [`windows/window.rs`](../crates/gpui-windows/src/windows/window.rs).
The coordinate conversion has unit tests; candidate-window behavior still
needs an interactive Windows test with a CJK IME and more than one DPI scale.
**Native IME validation is pending:** no Windows test host is currently available.
Follow the [Windows IME validation steps and result template](windows-ime-validation.md)
before marking this capability runtime-verified.

## Contract rules

1. Add a row here whenever a platform method gains or loses a real
   implementation, fallback, or error result.
2. Link the implementation and state feature gates explicitly; do not infer
   support from a trait method existing.
3. Keep `on_reopen`/`on_open_urls` marked as accepted-but-never-fired on
   platforms that do not emit those events.
4. A unit test proves portable logic only. Native window handles, IME
   placement, compositor presentation, and screen capture require the matching
   CI runner or an interactive host test.

## Verification coverage

- Core library: `cargo test -p fc-gpui-core --lib --features test-support,runtime_shaders`.
- Windows pure logic: `cargo test -p fc-gpui-windows --lib --locked`.
- Linux X11: an Xvfb real-visual job is configured in
  [the feature-matrix workflow](../.github/workflows/gpui-feature-matrix.yml).
  The Wayland-specific feature jobs run compile checks; they do not validate
  presentation on a real Wayland compositor, which remains unverified.
- Windows visual behavior: the Windows real-visual CI job. It does not prove
  native IME behavior; that requires the interactive CJK IME procedure above.
  macOS unit tests cannot validate those native paths.

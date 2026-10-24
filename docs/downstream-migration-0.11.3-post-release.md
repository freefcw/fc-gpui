# Downstream migration notes for 0.12.0

The `0.12.0` release contains public API changes after the `0.11.3` release.
Applications that stay on the `0.11.3` packages do not need these changes until
they upgrade.

## Rename `ScrollWheelEvent`

Rename imports and event types from `ScrollWheelEvent` to `ScrollEvent`:

```rust
use gpui::{ScrollEvent, Window};

window.on_mouse_event(|event: &ScrollEvent, _phase, _window, _cx| {
    // Handle event.delta and event.touch_phase.
});
```

The `PlatformInput::ScrollWheel` variant keeps its existing name. Only the
public event type is renamed.

## Implement `Platform::platform_info`

Custom platform implementations must provide runtime platform information:

```rust
use gpui::{OperatingSystem, PlatformInfo, PlatformTarget};

fn platform_info(&self) -> PlatformInfo {
    PlatformInfo {
        os: OperatingSystem::Linux,
        target: PlatformTarget::Native,
    }
}
```

Use `PlatformTarget::Web` when the application runs in a browser. The host OS
and delivery target are separate values because a browser application can run
on a different host OS from the build target.

Application code can read the value through `cx.platform_info()`.

## Pass `App` to secondary-modifier queries

The secondary modifier is a runtime platform concept. Pass the current app
context when querying it:

```rust
if event.modifiers.secondary(cx) {
    // Command on macOS, Control on other native platforms.
}

let secondary = Modifiers::secondary_key(cx);
```

This is required for code that uses `Modifiers::secondary` or
`Modifiers::secondary_key` directly.

## Use atomic credential creation on macOS

The new macOS-only method creates an item only when the same URL and username
are absent:

```rust
#[cfg(target_os = "macos")]
let created = cx
    .write_credentials_if_not_exists(url, username, password)
    .await?;
```

`created` is `true` when the item was created and `false` when an existing item
was preserved. Existing `write_credentials`, `read_credentials`, and
`delete_credentials` calls remain unchanged.

## Other changes

Taffy is now pinned to `0.15.0`. Downstream applications should not depend on
GPUI's internal Taffy types; applications that directly pin or use Taffy
alongside GPUI should verify their own layout code after upgrading.

The renderer, font-cache, focus-cleanup, and macOS thread-marker changes do not
require downstream source changes.

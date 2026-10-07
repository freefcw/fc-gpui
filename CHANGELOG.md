# Changelog

## Unreleased

### Added

- **`keyring` feature gates Linux credentials storage** — `fc-gpui`, `fc-gpui-platform`,
  and `fc-gpui-linux` gain a `keyring` feature (on by default) that controls the `oo7`
  Secret Service integration behind `write_credentials` / `read_credentials` /
  `delete_credentials`. Disabling it drops `oo7` and its unique transitive dependencies;
  the three credentials methods then return an error explaining the feature is off.
  Default behavior is unchanged.
- **Weekly dependency audit CI** — a scheduled workflow runs `cargo audit`, `cargo deny`
  (licenses, advisories, bans, sources; policy in the new `deny.toml`), and
  `cargo machete` against the workspace.

### Changes

- **Unused dependencies removed** — `fc-gpui-core` dev-deps `env_logger`,
  `pretty_assertions`, `unicode-segmentation`; `fc-gpui-util` dev-dep `indoc`;
  stale `rand` pins in the downstream compatibility fixtures.
- **`deny.toml` licensing fix** — allow `NCSA` (`libfuzzer-sys`, reachable only
  under `image-format-avif`) so `cargo deny check licenses` passes; drop the
  unmatched `Unicode-DFS-2016` / `CDLA-Permissive-2.0` allowances.

### Breaking changes

- **`Animation` gains start delay and finite repeat** — the `oneshot: bool` field is replaced by a
  `repeat: AnimationRepeat` enum (`Once`, `Forever`, `Times(n)`), alongside new `delay: Duration`
  field and `with_delay` / `repeat_n` / `with_repeat` builders (`repeat()` keeps its infinite-loop
  meaning). This replaces the downstream "animate with `repeat()` forever, then cancel via a
  timer" workaround: an animation can now wait before starting and run an exact number of passes
  before holding its final value. While the delay elapses the element holds `easing(0)` and
  re-renders are driven by a background timer instead of animation frames; `Times(0)` is clamped
  to one pass. In animation chains (`with_animations`) each animation's delay counts from the
  moment that animation begins.

## 0.10.0 (2026-10-06)

### Breaking changes

- **Utility library target names now match the published package names** — the eleven support
  crates drop their custom `adabraka_*` `[lib]` names: `fc-gpui-util` → `fc_gpui_util`,
  `fc-gpui-collections` → `fc_gpui_collections`, `fc-gpui-http-client` → `fc_gpui_http_client`,
  and so on. This only affects consumers that depend on those packages directly with the
  default dependency key; in-tree and renamed dependencies are unchanged.
- **The framework's import identity is unchanged** — `fc-gpui` still imports as `gpui`,
  `fc-gpui-core` as `gpui_core`, and the platform backends as `gpui_*`. Existing
  `use gpui::*` / `use gpui_core::*` code compiles without changes, and renamed dependencies
  keep working with the GPUI macros (see `tests/downstream-gpui-alias` and the sibling
  compatibility fixtures).
- **`adabraka_perf` binary renamed to `fc-gpui-perf`** — removing the custom `[[bin]]` name
  derives the executable from the package name; adjust scripts that invoke the old command.
- **Package metadata drops the Adabraka branding** — crate descriptions and the authors field
  now read `fc-gpui` / `FC GPUI Contributors`; no functional change.

### Fixes

- **macOS tray clicks report the real button** — `handleTrayPanelClick:` no longer synthesizes
  `LeftClick` for every interaction. `NSApp.currentEvent` is classified while the click is being
  delivered, so `TrayIconEvent::{RightClick, DoubleClick}` now reach `on_tray_icon_event` /
  `on_tray_icon_click_event`. Right-click-to-open-menu works without platform workarounds.
- **macOS tray clicks no longer require panel mode as a silent prerequisite** — the status item
  button's click handler is wired at tray creation and re-wired on every state transition, so
  icon-click callbacks fire whenever no menu is attached. Registering a callback plus attaching a
  menu without panel mode (the ambiguous combination, either registration order) now logs a
  one-time warning instead of silently never delivering clicks.
- **`WindowOptions::show` / `focus` are honored on Linux X11** — the facade now only calls
  `PlatformWindow::map_window` when `show` is set, so windows created with `show: false` stay
  hidden until `show_window()`, matching macOS. X11 consumes `focus` by requesting
  `_NET_ACTIVE_WINDOW` right after mapping, matching the macOS `focus && show` →
  `makeKeyAndOrderFront` behavior. Downstream no longer needs per-window
  `show_window()`/`activate_window()` fixups after creation.
- **Resize notifications are no longer dropped on re-entrancy** — the facade's `on_resize` /
  `on_moved` callbacks retry on the foreground executor when they fire synchronously while the
  app is already borrowed (the exact path taken by programmatic `setFrame:`-based resizes), so
  `bounds_changed` always runs instead of being swallowed by `log_err`.

- **GitHub archive extraction** — `fc-gpui-http-client` 0.5.2 keeps a PAX size header on
  the entry it describes. A size header placed before a GNU long name no longer changes the
  length of the following file. Unix symlink extraction is covered by a test.
- **Windows archive builds** — `fc-gpui-http-client` depends on `async-std` with the `unstable`
  feature on Windows only, so `async-tar` 0.6 can call `symlink_file`. This dependency is
  required for that build. Windows CI does not create links without `SeCreateSymbolicLinkPrivilege`.

### Improvements

- **Dependency refresh: renderer and accessibility stacks** — upgrades `wgpu` to 30.0.1
  (surface `color_space: Auto` and `Queue::present` keep 29-era behavior; adapter limits
  request skips the new anti-fingerprinting bucketing), `taffy` to 0.14.0 (measured nodes
  delegate back to taffy's leaf algorithm, preserving 0.13 clamping and scrollable-overflow
  semantics), and the accesskit family to `accesskit` 0.25.1 / `accesskit_macos` 0.27.1 /
  `accesskit_unix` 0.24.0 / `accesskit_windows` 0.35.1 (deprecated `Tree` alias → `TreeInfo`).
- **Dependency refresh: utilities** — `git2` 0.21 (clears RUSTSEC-2026-0183/0184),
  `resvg`/`usvg` 0.48, `dirs` 7, `ctor` 1.0, `strum` 0.28, `etagere` 0.3, `itertools` 0.15,
  `sha2` 0.11 (hex digest encoding no longer relies on the removed `LowerHex` impl),
  `take-until` 0.3, and `async_zip` 0.0.19 including the migration from the deprecated
  `read::seek` module to `read1`. Lockfile-wide `cargo update` applied 205 semver-compatible
  bumps.
- **Smaller production dependency graph** — `num_cpus` and `backtrace` are replaced by
  `std::thread::available_parallelism` / `std::backtrace`, and `postage` (unmaintained since
  2022) moves behind the `test-support` feature, which is the only code that uses it.
- **Workspace-level dependency management** — a `[workspace.dependencies]` table now pins the
  ~30 dependencies shared across crates, and member crates inherit via `workspace = true`, so
  shared versions stay in lockstep; `[workspace.package]` metadata (authors, license, homepage,
  repository) is inherited the same way.
- **Profiler test isolation** — the executor timing test identifies its own task's timings via
  a dedicated spawn-site probe file instead of asserting on global profiler state, fixing
  interference from unrelated tests that poll tasks while the profiler is enabled.

- **`Window::resize_anchored(size, anchor)`** — new API (also on `PlatformWindow`, with a
  default fallback to `resize`) that resizes without AppKit animation while keeping the given
  `ResizeAnchor` corner/edge fixed on screen, e.g. `ResizeAnchor::TopLeft` grows a tray popup
  down-right under the menu-bar icon. Implemented on macOS via synchronous
  `setFrame:display:animate:NO`; Linux/Windows fall back to `resize` for now. This is a fork
  capability Zed upstream does not have.
- **`trim_gpu_caches` on Linux** — `App::trim_gpu_caches` now trims every open window's renderer
  in addition to platform-level shared caches. The WGPU renderer shrinks its grown instance
  buffer back to the profile's initial size (mirroring the Metal idle-pool trim), so tray-class
  resident apps reclaim GPU memory on Linux too, not just macOS.
- **Tray callback delivery semantics documented** — `on_tray_icon_event` /
  `on_tray_icon_click_event` doc comments now state the macOS next-main-queue-turn delivery, the
  click-kind mapping, and the Linux host caveats.

- **Dependency refresh for 0.10** — migrates `derive_more` 0.99 to 2.1 with the minimal
  feature set and regression coverage for the derived geometry and font-weight arithmetic,
  and moves the Linux portal client to `ashpd` 0.12 (`zbus` 5).
  The public `gpui::ctor` re-export is now `ctor` 0.4.3: annotate startup functions with
  `unsafe fn`. Existing safe `fn` items still compile because `ctor` silences that deprecation
  by default.
  macOS screen-capture frameworks (`objc2-core-media`, `objc2-screen-capture-kit`) are now
  optional behind the existing `screen-capture` feature, and `objc2-app-kit` /
  `objc2-foundation` no longer pull unused default features.
- **Build and bench tooling** — updates `bindgen` to 0.73.2, `cbindgen` to 0.29.4, and the
  Criterion benches to 0.8.2.
- **`stacksafe` 1.0** — drops the transitive `proc-macro-error2` crate, whose code Rust has
  flagged for rejection in a future version.

### Downstream workarounds removable

Standing section: lists upstream fixes that let downstream packages delete their own
workarounds. Check here before maintaining a local workaround registry.

- **This release**:
  - Direct AppKit `setFrame:display:animate:NO` calls for anchored, non-animated popup resizes
    (e.g. BananaTray `src/platform/popup_window.rs`) can be replaced by
    `window.resize_anchored(size, ResizeAnchor::TopLeft)`; resize/moved events now survive being
    triggered from inside a window update.
  - Per-window `show_window()` / `activate_window()` fixups after opening windows on Linux X11
    can be deleted; pass `show`/`focus` in `WindowOptions` instead.
  - macOS tray right-click discrimination no longer needs left-click-only UI workarounds;
    handle `TrayIconEvent::RightClick` directly. The implicit `set_tray_panel_mode(true)`
    prerequisite call is only needed when a menu is attached.

## 0.9.2 (2026-10-02)

### Fixes

- **Clippy-clean shaped-line tests** — remove a redundant clone from the adjusted-length cursor regression test.

## 0.9.1 (2026-10-02)

### Breaking changes

- **`ShapedLine::cursor` returns `Option<ShapedLineCursor>`** — lines whose length was adjusted
  with `ShapedLine::with_len` (e.g. invisible-character rendering) cannot be split, so `cursor`
  now returns `None` for them instead of panicking.

- **Published package names moved to the `fc-gpui*` family** — the 18 release crates are renamed
  on crates.io. Cargo target names are untouched, so `use gpui::*`, `use gpui_core::*`, and the
  internal `collections`/`util`/`refineable` imports are unchanged; only the `package` you write
  in `Cargo.toml` changes.

  | Old package | New package |
  | --- | --- |
  | `adabraka-gpui` | `fc-gpui` |
  | `adabraka-gpui-core` | `fc-gpui-core` |
  | `adabraka-gpui-wgpu` | `fc-gpui-wgpu` |
  | `adabraka-gpui-linux` | `fc-gpui-linux` |
  | `adabraka-gpui-macos` | `fc-gpui-macos` |
  | `adabraka-gpui-windows` | `fc-gpui-windows` |
  | `adabraka-gpui-platform` | `fc-gpui-platform` |
  | `adabraka-gpui-macros` | `fc-gpui-macros` |
  | `adabraka_util` | `fc-gpui-util` |
  | `adabraka_util_macros` | `fc-gpui-util-macros` |
  | `adabraka_collections` | `fc-gpui-collections` |
  | `adabraka_semantic_version` | `fc-gpui-semantic-version` |
  | `adabraka_derive_refineable` | `fc-gpui-derive-refineable` |
  | `adabraka_refineable` | `fc-gpui-refineable` |
  | `adabraka_sum_tree` | `fc-gpui-sum-tree` |
  | `adabraka_http_client` | `fc-gpui-http-client` |
  | `adabraka_media` | `fc-gpui-media` |
  | `adabraka_perf` | `fc-gpui-perf` |

  Downstream packages that rename the facade dependency keep working; the GPUI macros now probe
  `fc-gpui` and `fc-gpui-core` instead of the old names, and the
  `[package.metadata.gpui-macros] crate = "…"` override is unchanged.

- **`App::fetch_asset` returns `Option<A::Output>`** — the asset cache now owns loads: pending
  loads and completed results stay cached until `App::remove_asset`, and `Window::use_asset` /
  `ImageCacheItem` subscribe the current view to completion. `ImageCacheItem` changed from a public
  enum (`Loading`/`Loaded`) to an owning struct with `get` / `use_image`; dropping an entry cancels
  its pending load.
- **`Platform::restart` takes restart arguments** — `restart(binary_path, arguments)`;
  `Application::with_restart_arguments` configures them. `Platform` also gains a required
  `prevent_idle_sleep` method, and `PlatformWindow` gains required `visibility` /
  `on_visibility_change` methods. Custom out-of-tree platform backends must implement the new
  methods.
- **`KeyDownEvent::prefer_character_input` and `KeystrokeEvent::input_preference`** — both structs
  gained public fields; struct literals and exhaustive matches need updating.
- **`LineWrapper::wrap_line` takes an `IndentAdjustment`** — controls continuation-line indentation
  (`NoIndent` / `SameIndent` (default) / `ExtraColumns(n)`).

### Fixes

- **Windows character input preference is actually computed** — `KeyDownEvent::prefer_character_input`
  was hardcoded `false` on every platform, so the `InputPreference::CharacterInput` dispatch path
  never engaged for real keyboard input. Windows now ports upstream zed-industries/zed#41259's
  `should_prefer_character_input` heuristic (ToUnicode with/without Ctrl/Alt/Win): keystrokes whose
  modifiers change the produced character, such as AltGr or Ctrl+Alt combinations on international
  layouts, prefer text input over key bindings when the focused input accepts text.
- **macOS deferred simple-fullscreen toggle after window close** — a window closed before the
  deferred toggle task ran no longer panics in `native_window_ptr`.
- **macOS surface instance stride** — surface instances in the Metal instance buffer are strided by
  `SurfaceBounds`, matching what the shader reads and what the CPU writes; multiple surfaces in one
  frame no longer overlap.
- **Pending-input timeout resume checks focus** — resuming a paused timeout no longer applies to
  pending input left over from a previous focus, matching the pause path.
- **Line paint layer bounds tolerate stale wrap boundaries** — a wrap boundary that indexes
  outside its layout no longer panics paint; it is skipped, merging the row with its neighbor.

- **macOS clipboard tests under cargo's worker-thread harness** — `test_clipboard` no longer
  constructs `MacPlatform` on a libtest worker, where `MainThreadMarker::new` is `None`. Production
  `MacPlatform::new` still panics off the AppKit main thread.

### Improvements

- **`App::on_missing_glyphs`** — subscribe to grapheme clusters that exhausted font fallback.
  The core text system installs its reporter into the platform text system at construction; the
  Linux (cosmic-text) backend reports glyphs that shape to glyph id 0, while macOS and Windows do
  not detect missing glyphs yet. Intended for dynamic font installation: register a covering font
  with `TextSystem::add_fonts`, which invalidates cached layouts so the text is shaped again. One
  registration per application; reports are deduplicated and bounded.
- **`Svg::data` cache key includes the byte length** — two distinct SVG documents must collide on
  both the 64-bit hash and the length before they alias the same sprite cache entry.

- **Window visibility** — `Window::visibility` / `Window::is_visible` and
  `Window::observe_window_visibility` / `Context::observe_window_visibility` report whether the
  platform is presenting the window's frames (`WindowVisibility`). macOS uses
  `NSWindow.occlusionState`; Windows uses `WS_VISIBLE` plus the minimized state; Wayland uses the
  xdg-shell v6 `suspended` toplevel state; X11 uses mapped state plus `VisibilityNotify`; headless
  windows are `Hidden`.
- **`App::prevent_idle_sleep` / `ActivityGuard`** — holds an OS activity that prevents idle sleep
  until the guard is dropped (macOS `NSActivityOptions::UserInitiated`, Windows power requests, the
  freedesktop inhibit portal on Linux). `BackgroundExecutor::prevent_app_nap` guards against App
  Nap-style throttling without blocking system sleep.
- **Pausable pending-input timeout for which-key UI** — `Window::pending_input` reports pending
  multi-stroke input with its timeout status, and `Window::set_pending_input_timeout_paused` lets
  an entity pause the flush while its UI is open; releasing the entity resumes automatically.
- **Standalone modifier keystrokes** — releasing a lone modifier (shift/ctrl/alt/cmd/fn) dispatches
  a recognized keystroke to interceptors and keystroke observers; modifier presses alone still
  produce no keystroke.
- **Dynamic font installation invalidation** — `TextSystem::add_fonts` clears cached font
  resolution and bumps a font generation that invalidates per-window and cross-window line-layout
  caches, so newly installed fonts shape existing text. The core also grows a bounded,
  deduplicated missing-glyph reporting channel and the Linux (cosmic-text) text system learns to
  detect graphemes that exhaust fallback; the application-facing subscription is available through
  `App::on_missing_glyphs`.
- **`ShapedLine::split_at` / `ShapedLineCursor`** — split a shaped line at a UTF-8 byte boundary
  preserving glyphs, decorations, and shaping metadata; the cursor splits increasing boundaries
  incrementally in linear time for byte-ordered lines and falls back to `split_at` for visually
  reordered ones.
- **Restart arguments** — `Platform::restart` forwards extra arguments on all platforms (macOS via
  `open --args`, Linux via argv, Windows via a quoted `Start-Process` argument list passed through
  the environment instead of script interpolation).

- **Workspace lockfile no longer records legacy `objc` or `block`** — a workspace-only
  `[patch.crates-io]` for `zed-scap` omits unused macOS cocoa/`objc` deps (Linux/Windows/FreeBSD
  capture still uses the 0.0.8-zed API). Residual `block` from `core-video` / `core-graphics2` is
  gated behind unused `display-link` / `display-stream` features (`default-features = false` plus
  `link` on `core-video`). Downstream lockfiles without these patches still see registry `objc` /
  `block`.

- **`MacWindowState::native_window` is `Option<Retained<NSWindow>>`** — typed AppKit replaces most
  raw `id` `msg_send!` for frame, screen, tabbing, fullscreen, IME, and dock progress. Drop takes
  the `Retained` to unwind the window-ivar `Arc` cycle. A few private or deprecated selectors stay
  as objc2 `msg_send!` (`_zoomFill:`, nil tabbing identifier). Outbound `start_external_drag`
  remains deferred.

- **`fc-gpui-media` drops leftover `objc`** — bindgen generates C bindings instead of an
  Objective-C path; Core Video return and pixel-format constants come from the `core-video` crate.
  Public media API names are unchanged.

- **`fc-gpui-macos` drops the `objc` crate** — leftover `window.rs` / `platform.rs` `msg_send!`
  sites (clipboard RTF, notifications, scroller style, recent documents, paths, dispatcher and
  display-link main-thread checks, keyboard, window lifecycle) use typed objc2. A few private
  `msg_send!` leftovers remain.

- **macOS Metal renderer on `objc2-metal`** — the gfx-rs `metal` crate and `ConcreteBlock`
  presentable completed handlers are replaced by `objc2-metal`, `CAMetalLayer`, and `block2`. The
  `metal` package is gone from the workspace lockfile.

- **macOS AppKit classes via objc2 `define_class!`** — `GPUIApplication` /
  `GPUIApplicationDelegate` and `GPUIWindow` / `GPUIPanel` / `GPUIView` / `BlurredView` plus the
  window-state archiver and unarchiver (in `window_classes.rs`) replace legacy `ClassDecl` + ivars.

- **macOS screen-capture stream on objc2** — SCStream start/stop, sample/output handlers, and the
  remaining stream `ClassDecl` / `ConcreteBlock` path use objc2 `define_class!` and `block2`
  `RcBlock`. CI's macOS visual smoke compiles `fc-gpui-macos` with `screen-capture`. Exactly-once
  `Ended` / `Cancelled` / `Failed` callbacks are unchanged.

- **macOS leftover objc2 islands** — biometric LocalAuthentication, SMAppService auto-launch,
  `os_info`, focused-window lookup, network-path monitor, media keys, tray menus, `open_url` /
  `reveal_path`, and thermal-state handlers move from leftover `objc` / `ConcreteBlock` call sites
  to typed objc2 + `block2`.

- **macOS simple fullscreen restore bounds** — `window_bounds()` reports the pre-simple-fullscreen frame so a restart does not reopen the window maximized.

- **`Window::toggle_simple_fullscreen` / `is_simple_fullscreen`** — macOS-only borderless fullscreen that covers the menu bar and notch without a Mission Control space. No-op / `false` on other platforms. This crate has no `fullscreen_mode` setting; apps choose when to call the API.

- **`Window::native_window_state` / `restore_native_window_state`** — encode and replay an opaque native restorable-state blob. On macOS this is AppKit window restoration (frame and Space); other platforms return `None` / no-op.

- **macOS titlebar Fill honors tiled-window margins** — when System Settings maps double-click to Fill, GPUI prefers AppKit's private `_zoomFill:` so "Tiled windows have margins" applies, and falls back to `zoom:` if that selector is missing.

- **GitHub release request timeout** — GitHub release list and tag lookups attach a 10-second `RequestTimeout` covering the full response body, so a stalled GitHub connection cannot hang callers indefinitely. Client implementations that honor the extension apply it; this workspace has no `reqwest_client` crate.

- **Windows Git Bash / PowerShell discovery** — Git Bash lookup uses `GIT_INSTALL_ROOT` and walks up from `git` so a `mingw64\bin` PATH prefix (launching from Git Bash) still finds `bin\bash.exe`. PowerShell discovery also checks .NET global tools, MSIX package family names, and Windows PowerShell; `get_powershell()` returns `None` if none are found, and `get_windows_system_shell` then falls back to `SystemRoot\System32\cmd.exe`. Windows app restart uses a discovered PowerShell instead of a hard-coded `powershell.exe`.

- **Opt-in `increase_open_file_limit` on Unix** — `adabraka_util::increase_open_file_limit()` raises the process soft `RLIMIT_NOFILE` toward 65,536 (10,240 on macOS) without changing the hard limit. GPUI does not call this from `Application::new` or any other startup path; apps that need it must call it themselves.

- **`Window::focus_lost_restore_target`** — while focus-lost listeners run, returns the closest ancestor of the previously focused element that can still receive focus. Returns `None` at other times, or when no such ancestor exists.

- **`on_file_drop_exit` for inbound file-drag leave** — `div().on_file_drop_exit(...)` runs when a platform file drag leaves the window while the element is hovered. This is a window-local `FileDropEvent::Exited`, not notification that an outbound drag session ended.

- **`all_font_names` lists only real families** — `TextSystem::all_font_names` no longer appends the hardcoded fallback stack or `.SystemUIFont`. Suggestions include platform fonts and fonts registered with `add_fonts`. Virtual aliases such as `.SystemUIFont` still resolve; they are just omitted from the list.

- **Malformed `${...}` shell variables no longer panic** — `ShellKind::to_cmd_variable` and `to_powershell_variable` use `strip_suffix('}')` and pass through malformed references such as `${` instead of slicing off the last byte.

- **HoverListenerMode for keyboard-modality hover listeners** — `div().hover_listener_mode(HoverListenerMode::InputModalityIndependent)` keeps `on_hover` hit-testing after keyboard input (menus that pause on hover). Default `InputModalityAware` still clears hover on key press. Hover styles and tooltips are unchanged.

- **Layout padding snap, SVG data/ExactSize, and test-window handles** — snap recomputed
  padding to the device-pixel grid before `clamp_scroll` so fractional rem sizes do not grow
  phantom scrollbars; `svg().data(bytes)` renders from raw SVG bytes; `SvgSize::ExactSize` and
  `render_parsed` rasterize at exact device-pixel dimensions while the scale-factor path stays 2×;
  `TestWindow` returns `HandleError::NotSupported` instead of panicking on raw handles;
  `Hitbox::is_hovered_at` hit-tests a position against the last frame; `Window::painted_quads`
  exposes scene quads under `test-support`.

- **Windows mixed-DPI placement and COLR emoji clear** — place new windows using the target
  monitor's DPI instead of `GetDpiForWindow` after `CW_USEDEFAULT`; clear the COLR emoji D3D
  render target before compositing layers so uncovered texels do not keep DXGI pool garbage.

- **Linux X11/Wayland correctness ports** — enable `wayland-backend`'s `log` feature so connection
  errors cannot panic when stderr is gone; write a trailing NUL on X11 `WM_CLASS`; clear the ICCCM
  urgency hint when a window becomes active; reject a null XKB context instead of using it; drop the
  X11 client `RefMut` before `WM_DELETE_WINDOW` close callbacks; drain x11rb-buffered events after
  foreground idle work and stop forcing `SetInputFocus` (keep `_NET_ACTIVE_WINDOW`) so the first map
  is not blank.

- **Every dependency now resolves from crates.io** — `fc-gpui-wgpu` depends on the published
  `wgpu 29.0.4` instead of the `zed-industries/wgpu` `v29` branch, whose only two commits ahead of
  the upstream `v29` release (the XCB EGL display handle and an examples README edit) both landed
  in 29.0.4. The root `[patch.crates-io]` override for `windows-capture` is replaced by a
  `>=1.3.6, <1.5` requirement on `fc-gpui-core`'s `screen-capture` feature, which keeps
  `zed-scap 0.0.8-zed` on the five-argument `Settings::new` for downstream builds too instead of
  only inside this workspace. Windows capture source titles lose Zed's `Monitor::name` display
  detection fix, which is not in any published `windows-capture`; `zed-scap` falls back to the GDI
  device name when `name()` fails.

## 0.9.0 (2026-08-30)

The 0.9 development line intentionally contains the public API and Cargo feature changes below.
Applications upgrading from 0.8.x should follow the migration mappings in this section.

### Breaking changes

- **`Window::blur` and `Window::disable_focus` now take `&mut App`** — pending
  keystrokes and keybinding-indicator observers are cleared when a window is
  blurred. Call `window.blur(cx)` / `window.disable_focus(cx)` instead of the
  previous no-argument forms.
- **`GpuResourceBudget` requires `instance_buffer_max_size`** — struct literals that listed only
  atlas and initial instance-buffer sizes will not compile. Use `GpuResourceBudget::new(atlas,
  initial)` or `..GpuResourceBudget::default()` (256 MiB ceiling, the previous implicit Zed cap).
  `AppProfile::{Desktop,Utility,Minimal}` already fill profile-specific maxima.
- **`set_keep_alive_without_windows` replaced by `QuitMode`** — the boolean keep-alive flag is
  gone; use `App::set_quit_mode` / `Application::with_quit_mode` with `QuitMode::Explicit`
  (previous `true`) or `QuitMode::LastWindowClosed` (previous `false`) instead. The quit
  decision is now owned by the core application rather than each platform backend; platforms
  only receive `Platform::set_quit_mode` for platform-visible side effects (macOS
  `ActivationPolicy`). Behavior change on macOS: `QuitMode::Default` now keeps the app alive
  after the last window closes, following the macOS convention — previously the default quit
  on last window close on every platform. Other platforms are unchanged (`Default` still
  quits when the last window closes).
- **Removed pass-through dependency features** — implementation-dependency features that enabled
  no additional code are no longer public: `backtrace`, `rand`, `bitflags`, and `scap` on
  `adabraka-gpui-core`, plus the Linux backend dependency names (`as-raw-xcb-connection`,
  `ashpd`, `bytemuck`,
  `calloop-wayland-source`, `cosmic-text`, `filedescriptor`, `open`, `wayland-backend`,
  `wayland-client`, `wayland-cursor`, `wayland-protocols`, `wayland-protocols-plasma`,
  `wayland-protocols-wlr`, `wayland-scanner`, `x11-clipboard`, `x11rb`, `xim`, and
  `xkbcommon`) across `adabraka-gpui`, `adabraka-gpui-platform`, and `adabraka-gpui-linux`.
  Enable the corresponding semantic features instead: `leak-detection`, `test-support`,
  `wayland`, `x11`, `screen-capture`, or `perf-enabled`. The `wgpu` feature remains a no-op
  marker on `adabraka-gpui` and `adabraka-gpui-platform` for headless compile checks; the old
  `adabraka-gpui-linux/wgpu` feature is removed, and the Linux WGPU backend is enabled by the
  `wayland`/`x11` aggregates.

  | Removed feature | Replacement |
  | --- | --- |
  | `backtrace` | `leak-detection` |
  | `scap` | `screen-capture` |
  | Linux Wayland dependency names | `wayland` |
  | Linux X11 dependency names | `x11` |
  | `rand` on `adabraka-gpui-core` | `test-support` |

### Improvements

- **Spring animations** — `SpringConfig`, `SpringAnimation`, and `AnimationExt::with_spring`
  simulate interruptible physical motion. Overlay, HUD, and daemon surfaces can retarget a
  spring without restarting it. Duration-based easing can overshoot via `sampled_easing`.
- **Animation max FPS** — `Animation::with_max_fps` schedules timer-driven redraws instead of
  requesting a frame on every vsync, useful for looping HUD/daemon decorations.
- **Configurable inactive-window FPS** — `WindowOptions::inactive_frame_interval` controls the
  minimum interval between animation frames while a window is unfocused (`None` disables the
  throttle). The default remains ~30 FPS.
- **`LineLayout::{split_at, paint, paint_background}`** — callers can hold `Arc<LineLayout>`
  and their own decoration runs without allocating a large `ShapedLine`.
- **`container_query` element** — `container_query(|size, window, cx| ...)` builds a subtree from
  the size the element was assigned during layout. It fills its parent by default and is sized
  only by its style and the space the parent offers, so the contents cannot influence it.
- **Sticky-axis scrolling** — `restrict_scroll_to_axis` now locks a precise trackpad gesture to the
  axis it starts on and unlocks only when the opposite axis is strong enough. Line-based wheel
  remapping and `allow_concurrent_scroll` keep their previous semantics.
- **`App::set_window_appearance`** — force light or dark independently of the OS setting; `None`
  clears the override and follows the system again. macOS sets `NSApplication.appearance` so the
  native chrome matches; other platforms store the override and report it from
  `App::window_appearance`.
- **Wrapping keeps closing punctuation** — UAX #14 LB13 closing marks (`)`, `]`, `}`, quotes, `»`,
  `…`) count as word characters, so a wrapped line can no longer start with one. URL-glue
  characters (`/`, `?`, `&`, `=`) are unchanged.
- **Windows Restart Manager** — `WM_QUERYENDSESSION` / `WM_ENDSESSION` shut the app down
  cleanly so installers can replace binaries. `Platform::on_quit` now reports whether
  shutdown ran synchronously.

- **Renderer GPU budgets moved off `WindowParams`** — `atlas_initial_size` and
  `instance_buffer_initial_size` are renderer memory policy, not per-window parameters. Platforms
  now receive them through `Platform::configure_gpu_resources` (driven by
  `Application::with_resource_profile`) and supply them to renderers at window creation, so
  `WindowParams` no longer carries renderer resource policy.
- **Batched wgpu instance uploads** — the Linux wgpu renderer uploads a frame's instances once at
  frame start instead of per batch, sizing and growing the buffer from `GpuResourceBudget`
  watermarks so `Minimal` and `Utility` are not pinned to the 256 MiB desktop ceiling. A frame
  whose instances exceed the profile or device cap now fails instead of presenting an unrecorded
  swapchain image, and `GpuResourceBudget::new(atlas, initial)` keeps a two-field construction
  path for callers that do not want to pick a maximum.
- **Demand-driven Wayland frame loop** — the render loop parks when idle rather than committing
  empty heartbeat frames, and wakes through `schedule_frame` when a window still needs work.
  Hidden windows no longer attach buffers or schedule frames, so a refresh cannot remap a hidden
  XDG or layer-shell surface, and the Linux event loop is skipped entirely when the app has
  already quit instead of blocking forever in `dispatch`.
- **Linux font cache prewarming** — `TextSystem::prewarm_fonts` warms the cosmic-text font-match
  cache for the requested fonts so shaping does not pay that cost on the hot path.
- **Feature contract checks** — `scripts/verify-features.sh` compiles the public feature
  combinations used by CI and the docs runbooks, and pins every published crate's public
  feature list against a whitelist so accidental additions or removals fail in CI.
- **Async benchmark feature** — `adabraka-gpui-core` exposes the `bench` feature used to compile
  the production-like async task completion benchmark without enabling it for normal builds.
- **Taffy 0.13** — the layout engine pin moves from `0.12.2` to `0.13.0`. Public `Styled`
  builder signatures are unchanged (`flex_grow` / `flex_shrink` remain parameterless helpers
  that set `1.0`). Post-bump list/text cache fixes and auto-sized window-root fill were already
  present; this release adds the content-sized grid-column regression coverage that lands with
  the Zed 0.13 bump.

## Utility crates 0.6.0 (2026-08-30)

### Breaking changes

- **`adabraka_util` feature cleanup** — the dependency-name features `git2`, `rand`, and
  `util_macros` are removed. Use the semantic `test-support` feature when the test helpers are
  required.
- **`adabraka_util_macros` feature cleanup** — the dependency-name feature `perf` is removed. Use
  `perf-enabled` instead.

## 0.8.1 (2026-07-28)

### Fixes

- **Windows test compilation** — avoids importing the crate-wide `test` name into the draw
  coordinator unit-test module, removing an ambiguous `#[test]` attribute on Windows.
- **Windows visual snapshots** — aligns the DirectX HLSL `Quad` layout with the Rust scene struct's
  explicit transform padding so offscreen readback renders scene content instead of a fully
  transparent image.

## 0.8.0 (2026-07-28)

### Breaking changes

- **Permission capability reporting** — `PermissionStatus` now includes `Unavailable`, and
  accessibility and microphone permission requests return `PermissionRequestStatus`. Unsupported
  platforms no longer report permissions as granted.
- **Default image decoder set** — default builds now enable GIF, JPEG, PNG, and WebP. AVIF, BMP,
  DDS, EXR, farbfeld, HDR, ICO, PNM, QOI, TGA, and TIFF remain available through their individual
  `image-format-*` features but must be enabled explicitly.

### Features

- **Screen-capture lifecycle** — callers can opt into exactly-once `Ended`, `Cancelled`, or `Failed`
  notifications for streams that start successfully. Existing `ScreenCaptureSource::stream`
  implementations remain source-compatible.
- **Accessibility contracts and testing** — elements can expose form-control properties, ARIA
  descriptions, and keyboard shortcuts; test platforms can activate accessibility, dispatch
  assistive actions, and inspect a stable debug tree.
- **Cross-platform visual artifacts** — Linux WGPU and Windows DirectX backends can render scenes to
  images for visual tests, with native window sizing and offscreen Windows rendering handled by the
  test platform.
- **Window and image behavior** — applications can request attention for a specific window, update
  layer-shell exclusive zones at runtime, honor EXIF orientation for static images, and size
  auto-dimensioned window roots from the platform-provided viewport.

### Fixes

- **GPUI macro dependency resolution** — aligns `adabraka-gpui-macros` with the `0.8` release line,
  resolves renamed facade/core dependencies, and lets mixed direct dependency graphs select the
  intended crate through `[package.metadata.gpui-macros]`.
- **macOS screen capture** — releases native setup, startup-failure, and frame-callback resources
  so repeated screen-sharing attempts do not accumulate them.
- **Wayland clipboard/selection serial** — selection ownership now uses a dedicated press-derived
  serial instead of the largest serial across all kinds, preventing IME serials from poisoning
  clipboard ownership on Mutter/kWin after prolonged input-method use.
- **Rendering and event safety** — fixes truncated-text measurement reuse, nested deferred draws,
  reentrant Windows draws, arena clearing during draws, prompt click-through, and Windows text and
  atlas edge cases.
- **Platform lifecycle hardening** — retains delayed Linux tasks during shutdown, enables the
  Windows security APIs required by single-instance support, and deduplicates Wayland IME cursor
  commits.

### Improvements

- **Internal crate split** — separates core, platform selection, Linux, macOS, Windows, and WGPU
  implementations while preserving `adabraka-gpui` as the downstream compatibility facade.
- **Linux/WGPU resource profiles** — new windows now apply
  `GpuResourceBudget::instance_buffer_initial_size`, including after GPU device recovery, while
  clamping the allocation to renderer and device limits.
- **Rendering and scheduling efficiency** — reduces border-only quad overdraw, shares macOS display
  links per display, uses the Win32 thread pool for Windows tasks, and upgrades Taffy to `0.12.2`.
- **Release verification** — the canonical migration script verifies all eight package archives and
  runs semantic API comparison for the published public facade with a registry library baseline.

## 0.7.0 (2026-07-11)

### Breaking changes

- **Layer-shell API (`0.7.0`)** — layer-shell windows now use
  `WindowOptions.kind = WindowKind::LayerShell(LayerShellOptions { ... })`. This removes
  `WindowOptions::layer_shell`, `LayerShellProtocolPreference`,
  `LayerShellOptions::tray_panel`, and `LayerShellOptions::from_window_bounds`.
  `WindowKind::Overlay` no longer implicitly creates a layer-shell surface. The Wayland backend
  now requires `wlr-layer-shell`, while X11 and unsupported compositors return
  `LayerShellNotSupportedError`. See the
  [migration and implementation provenance guide](docs/layer-shell-migration.md).

### Features

- **App resource profiles** — new `AppResourceProfile` / `AppProfile` system lets
  callers tune internal cache sizes and GPU atlas allocation at startup via
  `Application::new().with_resource_profile(AppProfile::Minimal)`. Three presets
  are provided (`Desktop`, `Utility`, `Minimal`) plus a `Custom` variant for
  fine-grained control. The global line-layout cache now uses a configurable
  watermark-based eviction strategy instead of the previous fixed 50%-drop
  approach, which is significantly smoother for small caches.

### Fixes

- **Linux/WGPU rendering** — aligned the WGPU shader's `Quad`/`Background` layout with the Rust `repr(C)` scene structs so that borders, rounded corners, gradients, separators, and toggle tracks no longer drop intermittently on Linux (`fix(wgpu): align Quad/Background ABI with Rust scene struct`).
- **Multi-stop gradients** — `interpolate_multi_stop` now clamps `stop_count` to `[2, 4]` and protects every divisor with `max(p_high - p_low, 1e-6)`, so degenerate or collapsed stops no longer produce NaN/Inf that blank out a quad. Mirrored across WGSL, Metal, and HLSL.

### Behavior changes

- **Linux/WGPU sRGB gradients** — gradients with `ColorSpace::Srgb` now mix in the same color space as macOS/Windows. The previous WGPU path applied an extra `linear_to_srgba` / `srgba_to_linear` round-trip in vertex/fragment, producing visibly different gradients on Linux. After this change Linux gradients look slightly brighter and more saturated at the midpoint, matching macOS and Windows. Oklab gradients are unchanged.

## 0.6.2 (2026-04-30)

### Improvements

- **Cursor behavior** — restore cursor to Arrow style when window loses focus

### Documentation

- Added batch 2 evaluation report documenting architectural constraints
- Added immediate actions completion report

## 0.6.1 (2026-04-30)

### Fixes

- **Anchored element positioning** — fixed size calculation with negative coordinates that caused context menus and popups to appear at wrong locations (synced from Zed `b38194198b`)
- **GIF rendering stability** — fixed out-of-bounds panic when replacing a GIF with one that has fewer frames (synced from Zed `749fcfdfd8`)

### Documentation

- Added comprehensive Zed sync documentation in `docs/sync/`:
  - Complete mapping between Zed and Adabraka GPUI repositories
  - Technical sync guide with code-level instructions
  - Quick reference for daily sync operations
  - Analysis of 100+ mergeable updates from Zed (2024-2026)

## 0.5.1 (2026-02-17)

### Performance

- **DirectX pipeline state caching** — skip redundant `set_pipeline_state` calls when consecutive batches use the same pipeline, saving ~6 D3D11 API calls per batch in text-heavy UIs
- **Cross-window text layout cache** — global LRU cache on `TextSystem` prevents re-shaping text that another window already shaped

## 0.5.0 (2026-02-15)

### Desktop Platform Features

Added 15 new cross-platform capabilities to the `Platform` trait, with implementations for macOS, Windows, and Linux:

**System Integration**
- **System power events** — subscribe to suspend/resume/lock/unlock/shutdown notifications (macOS: `NSWorkspace`, Windows: `WM_POWERBROADCAST`, Linux: stub for D-Bus logind)
- **Power save blocker** — prevent display sleep or app suspension (macOS: `IOPMAssertionCreateWithName`, Windows: `SetThreadExecutionState`, Linux: `dbus-send` screensaver inhibit + `systemd-inhibit`)
- **System idle time** — query time since last user input (macOS: `CGEventSourceSecondsSinceLastEventType`, Windows: `GetLastInputInfo`, Linux: X11 screensaver extension)
- **Network status** — query online/offline connectivity (macOS: `NWPathMonitor`, Windows: `INetworkListManager`, Linux: `/sys/class/net/*/operstate`)
- **OS info** — query OS name, version, arch, locale, hostname
- **Biometric authentication** — Touch ID (macOS), Windows Hello, with availability detection

**Window Management**
- **User attention** — request/cancel taskbar attention (macOS: dock bounce, Windows: `FlashWindowEx`, Linux: X11 EWMH `_NET_WM_STATE_DEMANDS_ATTENTION`)
- **Progress bar** — set taskbar/dock progress state (macOS: `NSDockTile`, Windows: `ITaskbarList3`)
- **Window state save/restore** — `WindowState` struct for persisting window bounds, display, and fullscreen state
- **Window positioner** — `WindowPosition` enum for semantic positioning (center, tray-relative, corners)

**UI & Input**
- **Native dialogs** — modal alert/confirm dialogs with customizable buttons (macOS: `NSAlert`, Windows: `TaskDialogIndirect`, Linux: `zenity`/`kdialog`)
- **Context menus** — show native context menus at a position (macOS: `NSMenu`, Windows: `TrackPopupMenu`)
- **Media keys** — intercept play/pause/stop/next/previous hardware keys (macOS: `MPRemoteCommandCenter`, Windows: `WM_APPCOMMAND`, Linux: XF86 keysym interception)
- **Dock badge** — set dock icon badge label (macOS only)

**App API**
- New `App` and `Window` convenience methods for all platform features
- `app.os_info()`, `app.network_status()`, `app.start_power_save_blocker()`, `app.show_dialog()`, etc.
- `window.set_progress_bar()`, `window.request_user_attention()`, etc.

### Improvements

- **Tray panel mode** — position windows relative to the tray icon
- **Menu icons** — support icon data on tray menu items
- **Global hotkey normalization** — consistent key string handling across platforms
- **Platform features demo** — new `platform_features_demo` example exercising all new APIs

### Fixes

- Pin `core-text` to `=21.0.0` to prevent `core-graphics` version conflict on macOS
- Resolve all clippy warnings across the codebase
- Fix safety and correctness issues in platform feature implementations

## 0.4.1 (2026-02-14)

- Documentation and README updates for crates.io

## 0.4.0 (2026-02-14)

### Initial Murmur Extensions

- System tray with icon, tooltip, and menu
- Global hotkeys with platform-native registration
- Overlay windows (always-on-top, click-through)
- Window show/hide toggling
- Active window and focused window info queries
- Accessibility and microphone permission checks (macOS)
- Auto-launch at login
- Single-instance enforcement
- Desktop notifications
- Toast component for in-app notifications
- Keep-alive-without-windows daemon mode
- Element transforms (rotate, scale, transform-origin)
- Multi-stop, radial, and conic gradients
- Per-element blend modes

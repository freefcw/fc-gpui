# Goal Document: Platform Contract Hardening

The current cross-platform behavior matrix is maintained in
[`docs/platform-capabilities.md`](../platform-capabilities.md). Update it when
this plan changes a backend capability or fallback contract.

## Go / No-Go
- **Judgment**: Go
- **Reason**: The three changes have bounded code paths, concrete static evidence, and focused test seams. They can be implemented without the deferred Wayland frame-transaction redesign.

## Target Outcome

Unsupported platform permissions cannot look granted, screen-capture consumers can observe a stream's terminal state, and Linux WGPU renderers receive the configured initial instance-buffer budget.

## Goal Definition
- **Type**: technical / quality
- **Boundary**: permission defaults in `Platform`, screen-capture lifecycle contracts and in-tree implementations, WGPU window construction, resource-profile documentation, and focused tests.
- **Non-goals**:
  - Split `Platform` into smaller traits.
  - Redesign Wayland frame completion.
  - Add new OS permission implementations.
  - Change renderer growth policy beyond applying the initial budget.
- **Deferred work**:
  - End-to-end capture tests against a real compositor or macOS ScreenCaptureKit.
  - A broader platform capability matrix for unrelated APIs.
- **Verification rule**: New focused tests fail on the current behavior, pass after the implementation, the relevant GPUI test suite passes, and every changed Rust file passes rustfmt.
- **Evidence source**: Rust unit tests, compile-time call-site coverage, targeted Cargo tests, and source review of the WGPU construction chain.
- **Pass criteria**:
  - Default permission checks and requests report `Unavailable`; supported requests report `Requested`.
  - A caller opting into screen-capture termination receives exactly one terminal status for test, scap, and macOS paths where applicable.
  - A custom profile's instance-buffer initial size reaches both Linux WGPU constructors and is normalized against device constraints.
- **Confidence note**: Unit tests cover public contract behavior and pure capacity normalization. Native capture and GPU behavior still require platform runtime validation.
- **Judgment owner**: Focused tests and the repository's GPUI test target.

## Current State
- `Platform` defaults accessibility and microphone permissions to success despite no implementation.
- `ScreenCaptureSource::stream` had no runtime terminal-state channel; scap logged errors and stopped.
- `WindowParams` carries atlas size but not the instance-buffer budget; WGPU hard-codes 2 MiB.
- The worktree is clean before this change.

## Priority Rationale
- Permission false positives can drive incorrect product behavior immediately, so they are first.
- The screen-capture lifecycle contract is next because it establishes recoverable error semantics before backend behavior changes.
- WGPU budget propagation is isolated and can reuse the existing profile-to-window parameter flow.

## Assumptions and Open Decisions

| Item | Status | Impact | Owner / Next step |
|---|---|---|---|
| Adding explicit unavailable/requested permission results is acceptable for this pre-1.0 public API | assumed | Downstream exhaustive matches may need an update; callers may choose to inspect new request returns | Validate in this workspace; document in final summary |
| Existing `ScreenCaptureSource` callers and implementations should remain source-compatible | confirmed by in-repo search | Prevents an unnecessary broad API break | Keep `stream` as a compatibility wrapper; legacy implementations explicitly reject unsupported termination notification instead of silently ignoring it |
| WGPU must apply the field rather than merely document it unsupported | confirmed by requested scope | Makes profile tuning observable on Linux | Thread it through `WindowParams` and both WGPU constructors |

## Phases

### Phase 1: Lock Unsupported Permission Semantics
- **Purpose**: Prevent unimplemented permissions from being interpreted as granted.
- **Entry condition**: Existing default and macOS permission implementations are understood.
- **Phase rules**:
  - Preserve macOS granted/denied behavior.
  - Keep existing statement-style permission request call sites compatible while returning an explicit request status.
- **Todos**:
  - [x] Add focused tests for default permission status and microphone callback behavior.
    - **Surface**: `platform/test/platform.rs`
    - **Proof**: Tests fail against the current `Granted`/`true` defaults.
    - **Depends on**: none
  - [x] Add an explicit unavailable status and use it in default platform implementations.
    - **Surface**: `platform.rs`
    - **Proof**: Focused tests pass; macOS implementation remains explicit.
    - **Depends on**: failing tests
- **Exit proof**: Unsupported test platform behavior cannot report permission success.
- **Stop condition**: A repository consumer requires unsupported permissions to remain `Granted`.

### Phase 2: Expose Screen-Capture Termination
- **Purpose**: Let consumers distinguish a started stream from its later cancellation, completion, or failure.
- **Entry condition**: The legacy stream API and scap/macOS callback paths are understood.
- **Phase rules**:
  - Keep `stream` source-compatible.
  - Use one terminal notification at most; startup failures remain in the existing result receiver.
  - Do not overload per-frame callbacks with lifecycle events.
- **Todos**:
  - [x] Add a failing test for an opt-in terminal callback on `TestScreenCaptureSource`.
    - **Surface**: `platform/test/platform.rs`
    - **Proof**: The new API is absent or cannot report a test termination.
    - **Depends on**: none
  - [x] Add `ScreenCaptureStreamTermination` and `stream_with_termination`.
    - **Surface**: `platform.rs`, platform implementations
    - **Proof**: Test source delivers one explicit terminal outcome; scap and macOS route runtime termination through the callback.
    - **Depends on**: failing test
- **Exit proof**: A caller can opt in to an independent terminal event without changing its existing frame callback.
- **Stop condition**: Native API ownership or callback threading makes a safe macOS implementation impossible without a separate design decision.

### Phase 3: Apply WGPU Instance-Buffer Budget
- **Purpose**: Make `GpuResourceBudget::instance_buffer_initial_size` effective for Linux WGPU windows.
- **Entry condition**: Existing atlas-size propagation and WGPU recovery paths are understood.
- **Phase rules**:
  - Default desktop behavior stays 2 MiB.
  - Clamp invalid or oversized requested values to a safe device-supported allocation.
  - Preserve the requested budget through device recovery.
- **Todos**:
  - [x] Add failing pure tests for configured capacity normalization.
    - **Surface**: `resource_profile.rs`
    - **Proof**: Test references missing normalization behavior.
    - **Depends on**: none
  - [x] Carry the field through `WindowParams`, Wayland, X11, WGPU construction, and recovery.
    - **Surface**: `window.rs`, `platform.rs`, Linux windows, WGPU renderer
    - **Proof**: The window propagation test passes, both Linux constructor call sites are covered by source review, and Linux CI remains the final platform compile proof.
    - **Depends on**: failing test
  - [x] Update profile documentation.
    - **Surface**: `docs/resource-profiles.md`
    - **Proof**: Custom profile example and field description match actual behavior.
    - **Depends on**: implementation
- **Exit proof**: A custom initial capacity is no longer silently replaced by 2 MiB on WGPU.
- **Stop condition**: Device-limit or recovery semantics require a product-level choice beyond safe clamping.

## Dry-Run Findings
- The screen-capture API is public and has no in-repository consumer, so an additive method protects external callers while enabling the new contract.
- WGPU initialization and device recovery share a private constructor; the requested size must be retained separately from the current grown capacity.
- Native macOS capture cannot be exercised in the current focused unit tests; implementation must remain small and preserve Objective-C ownership behavior.

## Final Validation
- `cargo test -p fc-gpui --lib --features screen-capture -- --test-threads=1`
- Run the focused permission, stream-drop, exactly-once, and macOS error-mapping tests independently.
- Run rustfmt in check mode for every changed Rust file.
- Compile the Linux `screen-capture` feature path in Linux CI; local cross-compilation requires a Linux C toolchain.

## First Execution Step

Add behavior-focused tests that demonstrate the current false permission success, absent capture termination API, and missing WGPU capacity normalization.

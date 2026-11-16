# fc-ksni

Fork of crates.io [`ksni` 0.3.7](https://crates.io/crates/ksni/0.3.7)
(registry checksum `1bcc1cb43797140f74b711f1c4a49f48a7a7429f211c5fb78264e7bcd1e99caf`,
upstream <https://github.com/iovxw/ksni>). The license is the upstream
[Unlicense](UNLICENSE).

`fc-gpui-linux` depends on this crate by path and version. A workspace
`[patch.crates-io]` would not be copied into the published `fc-gpui-linux`
manifest, so downstream users would resolve unpatched `ksni` and fail to
compile the Ayatana label API. This package name is what gets published.

The default features are `async-io` and `blocking`. Upstream ksni defaults to
`tokio`, which cannot be combined with `async-io`. This crate is a workspace
member, so every target builds it, and `zbus` needs one of those runtimes.

Publish `fc-ksni` 0.3.7 before `fc-gpui-linux` 0.13.0. It is a workspace
member so `cargo package` can include it in the same invocation as
`fc-gpui-linux`. Until that publish, the archive check resolves the packaged
copy with `[patch.crates-io]`.

## Delta vs ksni 0.3.7

Search the sources for `fc-gpui:`. The patch adds three Status Notifier Item
members that libayatana-appindicator already uses and upstream ksni does not:

- property `XAyatanaLabel` (`Tray::x_ayatana_label`, default empty)
- property `XAyatanaLabelGuide` (`Tray::x_ayatana_label_guide`, default empty)
- signal `XAyatanaNewLabel(label, guide)` when either value changes

`PropertiesChanged` carries the same two properties. `Title` and `ToolTip` are
unchanged: they stay the application name / tooltip, not the visible label.

Three upstream unit tests open a session bus. They return immediately when
that bus is absent, which is the usual case on a GitHub-hosted runner.
Everything else matches ksni 0.3.7. The crate is rustfmt-clean under the
workspace toolchain.

# FC GPUI Core

Internal core crate for [fc-gpui](https://crates.io/crates/fc-gpui).

This package contains GPUI's application state, element system, rendering contracts, test runtime, and the desktop backends that are being extracted during the upstream-aligned crate migration. Most applications should depend on `fc-gpui`, which preserves the stable `gpui` crate name and constructs the appropriate platform backend.

Direct use is intended for fc-gpui platform and renderer crates. Its low-level backend contracts may evolve together with those crates.

Licensed under Apache-2.0.

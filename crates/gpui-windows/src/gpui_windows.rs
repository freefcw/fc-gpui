#![doc = include_str!("../README.md")]
// False positives in platform-specific state handling.
#![cfg_attr(target_os = "windows", allow(unused_mut))]

mod mouse_move_cache;

#[cfg(target_os = "windows")]
#[path = "windows/mod.rs"]
mod backend;

#[cfg(target_os = "windows")]
pub(crate) use backend::*;

#[cfg(target_os = "windows")]
pub(crate) use gpui::*;

#[cfg(target_os = "windows")]
use std::rc::Rc;

/// Returns the default Windows platform implementation for this process.
#[cfg(target_os = "windows")]
pub fn current_platform(_headless: bool) -> Rc<dyn gpui::Platform> {
    Rc::new(
        WindowsPlatform::new()
            .inspect_err(|error| show_error("Failed to launch", error.to_string()))
            .unwrap(),
    )
}

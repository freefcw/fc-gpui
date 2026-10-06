//! Platform composition helpers for fc-gpui.
//!
//! This crate is an internal migration boundary. The published `fc-gpui`
//! package remains the compatibility entry point for ordinary downstream users.

use std::rc::Rc;

pub use gpui::Platform;

/// Returns a background executor for the current platform.
pub fn background_executor() -> gpui::BackgroundExecutor {
    current_platform(true).background_executor()
}

/// Builds an application using the current graphical platform.
pub fn application() -> gpui::Application {
    gpui::Application::with_platform(current_platform(false))
}

/// Builds an application using the current platform in headless mode.
pub fn headless() -> gpui::Application {
    gpui::Application::with_platform(current_platform(true))
}

/// Returns the current platform implementation.
#[cfg(any(target_os = "linux", target_os = "freebsd"))]
pub fn current_platform(headless: bool) -> Rc<dyn Platform> {
    gpui_linux::current_platform(headless)
}

/// Returns the current platform implementation.
#[cfg(target_os = "macos")]
pub fn current_platform(headless: bool) -> Rc<dyn Platform> {
    gpui_macos::current_platform(headless)
}

/// Returns the current platform implementation.
#[cfg(target_os = "windows")]
pub fn current_platform(headless: bool) -> Rc<dyn Platform> {
    gpui_windows::current_platform(headless)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn construction_entry_points_have_stable_types() {
        let _: fn() -> gpui::Application = application;
        let _: fn() -> gpui::Application = headless;
        let _: fn() -> gpui::BackgroundExecutor = background_executor;
        let _: fn(bool) -> Rc<dyn Platform> = current_platform;
    }
}

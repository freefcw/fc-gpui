#![doc = include_str!("../README.md")]

extern crate self as gpui;

use std::{path::PathBuf, rc::Rc, sync::Arc};

pub use gpui_core::*;

/// Returns a background executor for the current platform.
pub fn background_executor() -> BackgroundExecutor {
    gpui_platform::background_executor()
}

/// Returns the current desktop platform implementation.
pub fn current_platform(headless: bool) -> Rc<dyn Platform> {
    gpui_platform::current_platform(headless)
}

/// Real-renderer visual test context composed with the current desktop platform.
#[cfg(all(
    feature = "test-support",
    any(
        target_os = "macos",
        target_os = "linux",
        target_os = "freebsd",
        target_os = "windows"
    )
))]
pub struct RealVisualTestContext(gpui_core::RealVisualTestContext);

#[cfg(all(
    feature = "test-support",
    any(
        target_os = "macos",
        target_os = "linux",
        target_os = "freebsd",
        target_os = "windows"
    )
))]
impl RealVisualTestContext {
    /// Creates a context when the current platform supports real visual capture.
    pub fn new_if_supported() -> Option<Self> {
        let capabilities = VisualTestCapabilities::detect();
        (capabilities.real_renderer && capabilities.screenshot_capture).then(Self::new)
    }

    /// Creates a context with the default empty asset source.
    pub fn new() -> Self {
        Self(gpui_core::RealVisualTestContext::with_platform(
            gpui_platform::current_platform(false),
        ))
    }

    /// Creates a context with a custom asset source.
    pub fn with_asset_source(asset_source: Arc<dyn AssetSource>) -> Self {
        Self(
            gpui_core::RealVisualTestContext::with_platform_and_asset_source(
                gpui_platform::current_platform(false),
                asset_source,
            ),
        )
    }

    /// Starts the real platform run loop and invokes the callback after launch.
    pub fn run<F>(self, on_finish_launching: F)
    where
        F: 'static + FnOnce(&mut Self),
    {
        let platform = self.0.visual_test_platform();
        let mut cx = Some(self);
        platform.run(Box::new(move || {
            if let Some(mut cx) = cx.take() {
                on_finish_launching(&mut cx);
            }
        }));
    }
}

#[cfg(all(
    feature = "test-support",
    any(
        target_os = "macos",
        target_os = "linux",
        target_os = "freebsd",
        target_os = "windows"
    )
))]
impl std::ops::Deref for RealVisualTestContext {
    type Target = gpui_core::RealVisualTestContext;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

#[cfg(all(
    feature = "test-support",
    any(
        target_os = "macos",
        target_os = "linux",
        target_os = "freebsd",
        target_os = "windows"
    )
))]
impl std::ops::DerefMut for RealVisualTestContext {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0
    }
}

/// Configures and launches a GPUI application.
///
/// This compatibility wrapper keeps platform selection in the published
/// package while all application state and callback types remain core types.
pub struct Application(gpui_core::Application);

impl Application {
    /// Builds an application using the current desktop platform.
    // `new` selects and initializes the current desktop backend; there is no
    // backend-independent default application to construct.
    #[allow(clippy::new_without_default)]
    pub fn new() -> Self {
        #[cfg(any(test, feature = "test-support"))]
        log::info!("GPUI was compiled in test mode");

        Self(gpui_platform::application())
    }

    /// Builds an application using the current platform in headless mode.
    pub fn headless() -> Self {
        Self(gpui_platform::headless())
    }

    /// Builds an application with a caller-provided platform implementation.
    pub fn with_platform(platform: Rc<dyn Platform>) -> Self {
        Self(gpui_core::Application::with_platform(platform))
    }

    /// Builds this app with accessibility integration forcibly disabled.
    pub fn inaccessible(self) -> Self {
        Self(self.0.inaccessible())
    }

    /// Builds this app with the given asset source.
    pub fn with_assets(self, asset_source: impl AssetSource) -> Self {
        Self(self.0.with_assets(asset_source))
    }

    /// Sets the arguments passed to the application binary on restart.
    pub fn with_restart_arguments(self, arguments: Vec<std::ffi::OsString>) -> Self {
        Self(self.0.with_restart_arguments(arguments))
    }

    /// Sets the HTTP client for the application.
    pub fn with_http_client(self, http_client: Arc<dyn http_client::HttpClient>) -> Self {
        Self(self.0.with_http_client(http_client))
    }

    /// Sets the resource profile for the application.
    pub fn with_resource_profile(self, profile: impl Into<AppResourceProfile>) -> Self {
        Self(self.0.with_resource_profile(profile))
    }

    /// Configures when the application should automatically quit.
    /// By default, [`QuitMode::Default`] is used.
    pub fn with_quit_mode(self, mode: QuitMode) -> Self {
        Self(self.0.with_quit_mode(mode))
    }

    /// Starts the application.
    pub fn run<F>(self, on_finish_launching: F)
    where
        F: 'static + FnOnce(&mut App),
    {
        self.0.run(on_finish_launching)
    }

    /// Starts an application whose run loop is driven by an embedder.
    pub fn run_embedded<F>(self, on_finish_launching: F) -> ApplicationHandle
    where
        F: 'static + FnOnce(&mut App),
    {
        self.0.run_embedded(on_finish_launching)
    }

    /// Registers a handler for platform open-URL requests.
    pub fn on_open_urls<F>(&self, callback: F) -> &Self
    where
        F: 'static + FnMut(Vec<String>),
    {
        self.0.on_open_urls(callback);
        self
    }

    /// Registers a handler for reopening an already-running application.
    pub fn on_reopen<F>(&self, callback: F) -> &Self
    where
        F: 'static + FnMut(&mut App),
    {
        self.0.on_reopen(callback);
        self
    }

    /// Returns the application's background executor.
    pub fn background_executor(&self) -> BackgroundExecutor {
        self.0.background_executor()
    }

    /// Returns the application's foreground executor.
    pub fn foreground_executor(&self) -> ForegroundExecutor {
        self.0.foreground_executor()
    }

    /// Returns the application's text system.
    pub fn text_system(&self) -> Arc<TextSystem> {
        self.0.text_system()
    }

    /// Returns the bundle path for an auxiliary executable.
    pub fn path_for_auxiliary_executable(&self, name: &str) -> Result<PathBuf> {
        self.0.path_for_auxiliary_executable(name)
    }
}

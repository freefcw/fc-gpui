#[cfg(feature = "screen-capture")]
use crate::ScreenCaptureSource;
use crate::{
    Action, ActivityGuard, AnyWindowHandle, BackgroundExecutor, ClipboardItem, CursorStyle,
    ForegroundExecutor, GpuResourceBudget, Keymap, Menu, MenuItem, OwnedMenu, PathPromptOptions,
    Platform, PlatformDisplay, PlatformKeyboardLayout, PlatformKeyboardMapper, PlatformTextSystem,
    PlatformWindow, RendererCacheStats, Task, TestDispatcher, WindowAppearance, WindowParams,
};
use anyhow::Result;
use futures::channel::oneshot;
use parking_lot::Mutex;
use rand::{SeedableRng, rngs::StdRng};
use std::{
    path::{Path, PathBuf},
    rc::Rc,
    sync::Arc,
};

/// Test platform that combines native rendering with deterministic app tasks.
#[doc(hidden)]
pub struct VisualTestPlatform {
    dispatcher: TestDispatcher,
    background_executor: BackgroundExecutor,
    foreground_executor: ForegroundExecutor,
    platform: Rc<dyn Platform>,
    clipboard: Mutex<Option<ClipboardItem>>,
}

impl VisualTestPlatform {
    pub(crate) fn new(platform: Rc<dyn Platform>, seed: u64) -> Self {
        let dispatcher = TestDispatcher::new(StdRng::seed_from_u64(seed));
        let dispatcher = Arc::new(dispatcher);
        let background_executor = BackgroundExecutor::new(dispatcher.clone());
        let foreground_executor = ForegroundExecutor::new(dispatcher.clone());

        Self {
            dispatcher: (*dispatcher).clone(),
            background_executor,
            foreground_executor,
            platform,
            clipboard: Mutex::new(None),
        }
    }

    pub(crate) fn dispatcher(&self) -> &TestDispatcher {
        &self.dispatcher
    }
}

impl Platform for VisualTestPlatform {
    fn background_executor(&self) -> BackgroundExecutor {
        self.background_executor.clone()
    }

    fn foreground_executor(&self) -> ForegroundExecutor {
        self.foreground_executor.clone()
    }

    fn text_system(&self) -> Arc<dyn PlatformTextSystem> {
        self.platform.text_system()
    }

    fn run(&self, on_finish_launching: Box<dyn 'static + FnOnce()>) {
        self.platform.run(on_finish_launching);
    }

    fn quit(&self) {
        self.platform.quit();
    }

    fn restart(&self, _binary_path: Option<PathBuf>) {}

    fn activate(&self, _ignoring_other_apps: bool) {}

    fn hide(&self) {}

    fn hide_other_apps(&self) {}

    fn unhide_other_apps(&self) {}

    fn displays(&self) -> Vec<Rc<dyn PlatformDisplay>> {
        self.platform.displays()
    }

    fn primary_display(&self) -> Option<Rc<dyn PlatformDisplay>> {
        self.platform.primary_display()
    }

    fn active_window(&self) -> Option<AnyWindowHandle> {
        self.platform.active_window()
    }

    fn window_stack(&self) -> Option<Vec<AnyWindowHandle>> {
        self.platform.window_stack()
    }

    #[cfg(feature = "screen-capture")]
    fn is_screen_capture_supported(&self) -> bool {
        self.platform.is_screen_capture_supported()
    }

    #[cfg(feature = "screen-capture")]
    fn screen_capture_sources(
        &self,
    ) -> oneshot::Receiver<Result<Vec<Rc<dyn ScreenCaptureSource>>>> {
        self.platform.screen_capture_sources()
    }

    fn open_window(
        &self,
        handle: AnyWindowHandle,
        options: WindowParams,
    ) -> Result<Box<dyn PlatformWindow>> {
        self.platform.open_window(handle, options)
    }

    fn trim_renderer_caches(&self) {
        self.platform.trim_renderer_caches();
    }

    fn configure_gpu_resources(&self, gpu: &GpuResourceBudget) {
        self.platform.configure_gpu_resources(gpu);
    }

    fn renderer_cache_stats(&self) -> RendererCacheStats {
        self.platform.renderer_cache_stats()
    }

    fn window_appearance(&self) -> WindowAppearance {
        self.platform.window_appearance()
    }

    fn set_window_appearance(&self, appearance: Option<WindowAppearance>) {
        self.platform.set_window_appearance(appearance);
    }

    fn open_url(&self, url: &str) {
        self.platform.open_url(url);
    }

    fn on_open_urls(&self, _callback: Box<dyn FnMut(Vec<String>)>) {}

    fn register_url_scheme(&self, _url: &str) -> Task<Result<()>> {
        Task::ready(Ok(()))
    }

    fn prompt_for_paths(
        &self,
        _options: PathPromptOptions,
    ) -> oneshot::Receiver<Result<Option<Vec<PathBuf>>>> {
        let (tx, rx) = oneshot::channel();
        tx.send(Ok(None)).ok();
        rx
    }

    fn prompt_for_new_path(
        &self,
        _directory: &Path,
        _suggested_name: Option<&str>,
    ) -> oneshot::Receiver<Result<Option<PathBuf>>> {
        let (tx, rx) = oneshot::channel();
        tx.send(Ok(None)).ok();
        rx
    }

    fn can_select_mixed_files_and_dirs(&self) -> bool {
        self.platform.can_select_mixed_files_and_dirs()
    }

    fn reveal_path(&self, path: &Path) {
        self.platform.reveal_path(path);
    }

    fn open_with_system(&self, path: &Path) {
        self.platform.open_with_system(path);
    }

    fn on_quit(&self, _callback: Box<dyn FnMut() -> bool>) {}

    fn on_reopen(&self, _callback: Box<dyn FnMut()>) {}

    fn set_menus(&self, _menus: Vec<Menu>, _keymap: &Keymap) {}

    fn get_menus(&self) -> Option<Vec<OwnedMenu>> {
        None
    }

    fn set_dock_menu(&self, _menu: Vec<MenuItem>, _keymap: &Keymap) {}

    fn on_app_menu_action(&self, _callback: Box<dyn FnMut(&dyn Action)>) {}

    fn on_will_open_app_menu(&self, _callback: Box<dyn FnMut()>) {}

    fn on_validate_app_menu_command(&self, _callback: Box<dyn FnMut(&dyn Action) -> bool>) {}

    fn app_path(&self) -> Result<PathBuf> {
        self.platform.app_path()
    }

    fn path_for_auxiliary_executable(&self, name: &str) -> Result<PathBuf> {
        self.platform.path_for_auxiliary_executable(name)
    }

    fn set_cursor_style(&self, style: CursorStyle) {
        self.platform.set_cursor_style(style);
    }

    fn should_auto_hide_scrollbars(&self) -> bool {
        self.platform.should_auto_hide_scrollbars()
    }

    fn read_from_clipboard(&self) -> Option<ClipboardItem> {
        self.clipboard.lock().clone()
    }

    fn write_to_clipboard(&self, item: ClipboardItem) {
        *self.clipboard.lock() = Some(item);
    }

    #[cfg(any(target_os = "linux", target_os = "freebsd"))]
    fn read_from_primary(&self) -> Option<ClipboardItem> {
        self.clipboard.lock().clone()
    }

    #[cfg(any(target_os = "linux", target_os = "freebsd"))]
    fn write_to_primary(&self, item: ClipboardItem) {
        *self.clipboard.lock() = Some(item);
    }

    fn write_credentials(&self, _url: &str, _username: &str, _password: &[u8]) -> Task<Result<()>> {
        Task::ready(Ok(()))
    }

    fn read_credentials(&self, _url: &str) -> Task<Result<Option<(String, Vec<u8>)>>> {
        Task::ready(Ok(None))
    }

    fn delete_credentials(&self, _url: &str) -> Task<Result<()>> {
        Task::ready(Ok(()))
    }

    fn keyboard_layout(&self) -> Box<dyn PlatformKeyboardLayout> {
        self.platform.keyboard_layout()
    }

    fn keyboard_mapper(&self) -> Rc<dyn PlatformKeyboardMapper> {
        self.platform.keyboard_mapper()
    }

    fn on_keyboard_layout_change(&self, _callback: Box<dyn FnMut()>) {}

    fn set_quit_mode(&self, mode: crate::QuitMode) {
        self.platform.set_quit_mode(mode);
    }

    fn os_info(&self) -> crate::OsInfo {
        self.platform.os_info()
    }

    fn biometric_status(&self) -> crate::BiometricStatus {
        self.platform.biometric_status()
    }

    fn authenticate_biometric(&self, reason: &str, callback: Box<dyn FnOnce(bool) + Send>) {
        self.platform.authenticate_biometric(reason, callback);
    }

    fn prevent_idle_sleep(&self, reason: &str) -> Task<Result<ActivityGuard>> {
        Task::ready(Err(anyhow::anyhow!(
            "Idle sleep prevention for {reason:?} is not supported in visual tests"
        )))
    }
}

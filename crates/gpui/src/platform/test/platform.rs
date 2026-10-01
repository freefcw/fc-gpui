use crate::{
    ActivityGuard, AnyWindowHandle, BackgroundExecutor, ClipboardItem, CursorStyle, DevicePixels,
    DummyKeyboardMapper, ForegroundExecutor, GpuResourceBudget, Keymap, NoopTextSystem, Platform,
    PlatformDisplay, PlatformKeyboardLayout, PlatformKeyboardMapper, PlatformTextSystem,
    PromptButton, QuitMode, ScreenCaptureFrame, ScreenCaptureSource, ScreenCaptureStream,
    ScreenCaptureStreamTermination, ScreenCaptureTerminationCallback, SourceMetadata, Task,
    TestDisplay, TestWindow, TrayIconClickEvent, TrayIconEvent, TrayIconRenderingMode,
    WindowAppearance, WindowParams, size,
};
use anyhow::Result;
use collections::VecDeque;
use futures::channel::oneshot;
use parking_lot::Mutex;
use std::{
    cell::{Cell, RefCell},
    path::{Path, PathBuf},
    rc::{Rc, Weak},
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};
#[cfg(target_os = "windows")]
use windows::Win32::{
    Graphics::Imaging::{CLSID_WICImagingFactory, IWICImagingFactory},
    System::Com::{CLSCTX_INPROC_SERVER, CoCreateInstance},
};

/// TestPlatform implements the Platform trait for use in tests.
pub(crate) struct TestPlatform {
    background_executor: BackgroundExecutor,
    foreground_executor: ForegroundExecutor,

    pub(crate) active_window: RefCell<Option<TestWindow>>,
    active_display: Rc<dyn PlatformDisplay>,
    active_cursor: Mutex<CursorStyle>,
    current_clipboard_item: Mutex<Option<ClipboardItem>>,
    pub(crate) gpu_resource_budget: Mutex<GpuResourceBudget>,
    pub(crate) did_quit: Mutex<bool>,
    pub(crate) last_quit_mode: Mutex<Option<QuitMode>>,
    next_window_map_error: Mutex<Option<String>>,
    #[cfg(any(target_os = "linux", target_os = "freebsd"))]
    current_primary_item: Mutex<Option<ClipboardItem>>,
    pub(crate) prompts: RefCell<TestPrompts>,
    screen_capture_sources: RefCell<Vec<TestScreenCaptureSource>>,
    tray_icon: Mutex<Option<Vec<u8>>>,
    tray_icon_rendering_mode: Mutex<TrayIconRenderingMode>,
    tray_icon_event_callback: RefCell<Option<Box<dyn FnMut(TrayIconEvent)>>>,
    tray_icon_click_event_callback: RefCell<Option<Box<dyn FnMut(TrayIconClickEvent)>>>,
    pub opened_url: RefCell<Option<String>>,
    appearance_override: Mutex<Option<WindowAppearance>>,
    pub text_system: Arc<dyn PlatformTextSystem>,
    idle_sleep_prevention_count: Arc<AtomicUsize>,
    idle_sleep_prevention_delay: Cell<Duration>,
    idle_sleep_prevention_fails: Cell<bool>,
    #[cfg(target_os = "windows")]
    bitmap_factory: std::mem::ManuallyDrop<IWICImagingFactory>,
    weak: Weak<Self>,
}

#[derive(Clone)]
/// A fake screen capture source, used for testing.
pub struct TestScreenCaptureSource {
    active_termination: Arc<Mutex<Option<Arc<TestStreamTerminationNotifier>>>>,
}

/// A fake screen capture stream, used for testing.
pub struct TestScreenCaptureStream {}

struct ActiveTestScreenCaptureStream {
    termination: Arc<TestStreamTerminationNotifier>,
}

struct TestStreamTerminationNotifier {
    callback: Mutex<Option<ScreenCaptureTerminationCallback>>,
}

impl TestStreamTerminationNotifier {
    fn new(callback: ScreenCaptureTerminationCallback) -> Self {
        Self {
            callback: Mutex::new(Some(callback)),
        }
    }

    fn notify(&self, termination: ScreenCaptureStreamTermination) {
        let callback = self.callback.lock().take();
        if let Some(callback) = callback {
            callback(termination);
        }
    }
}

impl ScreenCaptureSource for TestScreenCaptureSource {
    fn metadata(&self) -> Result<SourceMetadata> {
        Ok(SourceMetadata {
            id: 0,
            is_main: None,
            label: None,
            resolution: size(DevicePixels(1), DevicePixels(1)),
        })
    }

    fn stream(
        &self,
        _foreground_executor: &ForegroundExecutor,
        _frame_callback: Box<dyn Fn(ScreenCaptureFrame) + Send>,
    ) -> oneshot::Receiver<Result<Box<dyn ScreenCaptureStream>>> {
        self.start_stream()
    }

    fn stream_with_termination(
        &self,
        _foreground_executor: &ForegroundExecutor,
        _frame_callback: Box<dyn Fn(ScreenCaptureFrame) + Send>,
        termination_callback: ScreenCaptureTerminationCallback,
    ) -> oneshot::Receiver<Result<Box<dyn ScreenCaptureStream>>> {
        let termination = Arc::new(TestStreamTerminationNotifier::new(termination_callback));
        self.replace_active_termination(termination.clone());
        self.start_stream_with_termination(termination)
    }
}

impl ScreenCaptureStream for TestScreenCaptureStream {
    fn metadata(&self) -> Result<SourceMetadata> {
        TestScreenCaptureSource::new().metadata()
    }
}

impl ScreenCaptureStream for ActiveTestScreenCaptureStream {
    fn metadata(&self) -> Result<SourceMetadata> {
        TestScreenCaptureSource::new().metadata()
    }
}

impl Drop for ActiveTestScreenCaptureStream {
    fn drop(&mut self) {
        self.termination
            .notify(ScreenCaptureStreamTermination::Cancelled);
    }
}

struct TestPrompt {
    msg: String,
    detail: Option<String>,
    answers: Vec<String>,
    tx: oneshot::Sender<usize>,
}

#[derive(Default)]
pub(crate) struct TestPrompts {
    multiple_choice: VecDeque<TestPrompt>,
    new_path: VecDeque<(PathBuf, oneshot::Sender<Result<Option<PathBuf>>>)>,
}

impl TestPlatform {
    pub fn new(executor: BackgroundExecutor, foreground_executor: ForegroundExecutor) -> Rc<Self> {
        Self::with_text_system(executor, foreground_executor, Arc::new(NoopTextSystem))
    }

    pub(crate) fn with_text_system(
        executor: BackgroundExecutor,
        foreground_executor: ForegroundExecutor,
        text_system: Arc<dyn PlatformTextSystem>,
    ) -> Rc<Self> {
        #[cfg(target_os = "windows")]
        let bitmap_factory = unsafe {
            windows::Win32::System::Ole::OleInitialize(None)
                .expect("unable to initialize Windows OLE");
            std::mem::ManuallyDrop::new(
                CoCreateInstance(&CLSID_WICImagingFactory, None, CLSCTX_INPROC_SERVER)
                    .expect("Error creating bitmap factory."),
            )
        };

        Rc::new_cyclic(|weak| TestPlatform {
            background_executor: executor,
            foreground_executor,
            prompts: Default::default(),
            screen_capture_sources: Default::default(),
            active_cursor: Default::default(),
            active_display: Rc::new(TestDisplay::new()),
            active_window: Default::default(),
            current_clipboard_item: Mutex::new(None),
            gpu_resource_budget: Mutex::new(crate::AppResourceProfile::default().gpu),
            did_quit: Mutex::new(false),
            last_quit_mode: Mutex::new(None),
            next_window_map_error: Mutex::new(None),
            #[cfg(any(target_os = "linux", target_os = "freebsd"))]
            current_primary_item: Mutex::new(None),
            weak: weak.clone(),
            tray_icon: Mutex::new(None),
            tray_icon_rendering_mode: Mutex::new(TrayIconRenderingMode::default()),
            tray_icon_event_callback: Default::default(),
            tray_icon_click_event_callback: Default::default(),
            opened_url: Default::default(),
            appearance_override: Mutex::new(None),
            idle_sleep_prevention_count: Arc::new(AtomicUsize::new(0)),
            idle_sleep_prevention_delay: Cell::new(Duration::ZERO),
            idle_sleep_prevention_fails: Cell::new(false),
            #[cfg(target_os = "windows")]
            bitmap_factory,
            text_system,
        })
    }

    pub(crate) fn simulate_new_path_selection(
        &self,
        select_path: impl FnOnce(&std::path::Path) -> Option<std::path::PathBuf>,
    ) {
        let (path, tx) = self
            .prompts
            .borrow_mut()
            .new_path
            .pop_front()
            .expect("no pending new path prompt");
        self.background_executor().set_waiting_hint(None);
        tx.send(Ok(select_path(&path))).ok();
    }

    #[track_caller]
    pub(crate) fn simulate_prompt_answer(&self, response: &str) {
        let prompt = self
            .prompts
            .borrow_mut()
            .multiple_choice
            .pop_front()
            .expect("no pending multiple choice prompt");
        self.background_executor().set_waiting_hint(None);
        let Some(ix) = prompt.answers.iter().position(|a| a == response) else {
            panic!(
                "PROMPT: {}\n{:?}\n{:?}\nCannot respond with {}",
                prompt.msg, prompt.detail, prompt.answers, response
            )
        };
        prompt.tx.send(ix).ok();
    }

    pub(crate) fn has_pending_prompt(&self) -> bool {
        !self.prompts.borrow().multiple_choice.is_empty()
    }

    pub(crate) fn pending_prompt(&self) -> Option<(String, String)> {
        let prompts = self.prompts.borrow();
        let prompt = prompts.multiple_choice.front()?;
        Some((
            prompt.msg.clone(),
            prompt.detail.clone().unwrap_or_default(),
        ))
    }

    pub(crate) fn set_screen_capture_sources(&self, sources: Vec<TestScreenCaptureSource>) {
        *self.screen_capture_sources.borrow_mut() = sources;
    }

    #[cfg(test)]
    pub(crate) fn fail_next_window_map(&self, message: impl Into<String>) {
        *self.next_window_map_error.lock() = Some(message.into());
    }

    pub(crate) fn prompt(
        &self,
        msg: &str,
        detail: Option<&str>,
        answers: &[PromptButton],
    ) -> oneshot::Receiver<usize> {
        let (tx, rx) = oneshot::channel();
        let answers: Vec<String> = answers.iter().map(|s| s.label().to_string()).collect();
        self.background_executor()
            .set_waiting_hint(Some(format!("PROMPT: {:?} {:?}", msg, detail)));
        self.prompts
            .borrow_mut()
            .multiple_choice
            .push_back(TestPrompt {
                msg: msg.to_string(),
                detail: detail.map(|s| s.to_string()),
                answers,
                tx,
            });
        rx
    }

    pub(crate) fn set_active_window(&self, window: Option<TestWindow>) {
        let executor = self.foreground_executor();
        let previous_window = self.active_window.borrow_mut().take();
        self.active_window.borrow_mut().clone_from(&window);

        executor
            .spawn(async move {
                if let Some(previous_window) = previous_window {
                    if let Some(window) = window.as_ref()
                        && Rc::ptr_eq(&previous_window.0, &window.0)
                    {
                        return;
                    }
                    previous_window.simulate_active_status_change(false);
                }
                if let Some(window) = window {
                    window.simulate_active_status_change(true);
                }
            })
            .detach();
    }

    pub(crate) fn did_prompt_for_new_path(&self) -> bool {
        !self.prompts.borrow().new_path.is_empty()
    }

    pub(crate) fn active_idle_sleep_preventions(&self) -> usize {
        self.idle_sleep_prevention_count.load(Ordering::SeqCst)
    }

    pub(crate) fn set_idle_sleep_prevention_delay(&self, delay: Duration) {
        self.idle_sleep_prevention_delay.set(delay);
    }

    pub(crate) fn set_idle_sleep_prevention_fails(&self, fails: bool) {
        self.idle_sleep_prevention_fails.set(fails);
    }

    pub(crate) fn tray_icon(&self) -> Option<Vec<u8>> {
        self.tray_icon.lock().clone()
    }

    pub(crate) fn tray_icon_rendering_mode(&self) -> TrayIconRenderingMode {
        *self.tray_icon_rendering_mode.lock()
    }

    pub(crate) fn simulate_tray_icon_click_event(&self, event: TrayIconClickEvent) {
        let mut event_callback = self.tray_icon_event_callback.borrow_mut().take();
        let mut click_callback = self.tray_icon_click_event_callback.borrow_mut().take();

        if let Some(callback) = event_callback.as_mut() {
            callback(event.kind.clone());
        }

        if let Some(callback) = click_callback.as_mut() {
            callback(event);
        }

        if let Some(callback) = event_callback {
            self.tray_icon_event_callback
                .borrow_mut()
                .get_or_insert(callback);
        }
        if let Some(callback) = click_callback {
            self.tray_icon_click_event_callback
                .borrow_mut()
                .get_or_insert(callback);
        }
    }
}

impl Platform for TestPlatform {
    fn configure_gpu_resources(&self, gpu: &GpuResourceBudget) {
        *self.gpu_resource_budget.lock() = gpu.clone();
    }

    fn set_quit_mode(&self, mode: QuitMode) {
        *self.last_quit_mode.lock() = Some(mode);
    }

    fn background_executor(&self) -> BackgroundExecutor {
        self.background_executor.clone()
    }

    fn foreground_executor(&self) -> ForegroundExecutor {
        self.foreground_executor.clone()
    }

    fn text_system(&self) -> Arc<dyn PlatformTextSystem> {
        self.text_system.clone()
    }

    fn prevent_idle_sleep(&self, reason: &str) -> Task<Result<ActivityGuard>> {
        let count = self.idle_sleep_prevention_count.clone();
        let fails = self.idle_sleep_prevention_fails.get();
        let reason = reason.to_owned();
        let acquire = move || {
            if fails {
                anyhow::bail!("Idle sleep prevention for {reason:?} is set to fail in this test");
            }
            count.fetch_add(1, Ordering::SeqCst);
            Ok(ActivityGuard::new(move || {
                count.fetch_sub(1, Ordering::SeqCst);
            }))
        };

        let delay = self.idle_sleep_prevention_delay.get();
        if delay.is_zero() {
            Task::ready(acquire())
        } else {
            let delay = self.background_executor.timer(delay);
            self.foreground_executor.spawn(async move {
                delay.await;
                acquire()
            })
        }
    }

    fn keyboard_layout(&self) -> Box<dyn PlatformKeyboardLayout> {
        Box::new(TestKeyboardLayout)
    }

    fn keyboard_mapper(&self) -> Rc<dyn PlatformKeyboardMapper> {
        Rc::new(DummyKeyboardMapper)
    }

    fn on_keyboard_layout_change(&self, _: Box<dyn FnMut()>) {}

    fn run(&self, _on_finish_launching: Box<dyn FnOnce()>) {
        unimplemented!()
    }

    fn quit(&self) {
        *self.did_quit.lock() = true;
    }

    fn restart(&self, _: Option<PathBuf>) {
        //
    }

    fn activate(&self, _ignoring_other_apps: bool) {
        //
    }

    fn hide(&self) {
        unimplemented!()
    }

    fn hide_other_apps(&self) {
        unimplemented!()
    }

    fn unhide_other_apps(&self) {
        unimplemented!()
    }

    fn displays(&self) -> Vec<std::rc::Rc<dyn crate::PlatformDisplay>> {
        vec![self.active_display.clone()]
    }

    fn primary_display(&self) -> Option<std::rc::Rc<dyn crate::PlatformDisplay>> {
        Some(self.active_display.clone())
    }

    #[cfg(feature = "screen-capture")]
    fn is_screen_capture_supported(&self) -> bool {
        true
    }

    #[cfg(feature = "screen-capture")]
    fn screen_capture_sources(
        &self,
    ) -> oneshot::Receiver<Result<Vec<Rc<dyn ScreenCaptureSource>>>> {
        let (mut tx, rx) = oneshot::channel();
        tx.send(Ok(self
            .screen_capture_sources
            .borrow()
            .iter()
            .map(|source| Rc::new(source.clone()) as Rc<dyn ScreenCaptureSource>)
            .collect()))
            .ok();
        rx
    }

    fn active_window(&self) -> Option<crate::AnyWindowHandle> {
        self.active_window
            .borrow()
            .as_ref()
            .map(|window| window.0.lock().handle)
    }

    fn open_window(
        &self,
        handle: AnyWindowHandle,
        params: WindowParams,
    ) -> anyhow::Result<Box<dyn crate::PlatformWindow>> {
        let window = TestWindow::new(
            handle,
            params,
            self.weak.clone(),
            self.active_display.clone(),
            self.next_window_map_error.lock().take(),
        );
        Ok(Box::new(window))
    }

    fn window_appearance(&self) -> WindowAppearance {
        self.appearance_override
            .lock()
            .unwrap_or(WindowAppearance::Light)
    }

    fn set_window_appearance(&self, appearance: Option<WindowAppearance>) {
        *self.appearance_override.lock() = appearance;
    }

    fn set_tray_icon(&self, icon: Option<&[u8]>) {
        *self.tray_icon.lock() = icon.map(Vec::from);
    }

    fn set_tray_icon_rendering_mode(&self, rendering_mode: TrayIconRenderingMode) {
        *self.tray_icon_rendering_mode.lock() = rendering_mode;
    }

    fn on_tray_icon_event(&self, callback: Box<dyn FnMut(TrayIconEvent)>) {
        *self.tray_icon_event_callback.borrow_mut() = Some(callback);
    }

    fn on_tray_icon_click_event(&self, callback: Box<dyn FnMut(TrayIconClickEvent)>) {
        *self.tray_icon_click_event_callback.borrow_mut() = Some(callback);
    }

    fn open_url(&self, url: &str) {
        *self.opened_url.borrow_mut() = Some(url.to_string())
    }

    fn on_open_urls(&self, _callback: Box<dyn FnMut(Vec<String>)>) {
        unimplemented!()
    }

    fn prompt_for_paths(
        &self,
        _options: crate::PathPromptOptions,
    ) -> oneshot::Receiver<Result<Option<Vec<std::path::PathBuf>>>> {
        unimplemented!()
    }

    fn prompt_for_new_path(
        &self,
        directory: &std::path::Path,
        _suggested_name: Option<&str>,
    ) -> oneshot::Receiver<Result<Option<std::path::PathBuf>>> {
        let (tx, rx) = oneshot::channel();
        self.background_executor()
            .set_waiting_hint(Some(format!("PROMPT FOR PATH: {:?}", directory)));
        self.prompts
            .borrow_mut()
            .new_path
            .push_back((directory.to_path_buf(), tx));
        rx
    }

    fn can_select_mixed_files_and_dirs(&self) -> bool {
        true
    }

    fn reveal_path(&self, _path: &std::path::Path) {
        unimplemented!()
    }

    fn on_quit(&self, _callback: Box<dyn FnMut() -> bool>) {}

    fn on_reopen(&self, _callback: Box<dyn FnMut()>) {
        unimplemented!()
    }

    fn set_menus(&self, _menus: Vec<crate::Menu>, _keymap: &Keymap) {}
    fn set_dock_menu(&self, _menu: Vec<crate::MenuItem>, _keymap: &Keymap) {}

    fn add_recent_document(&self, _paths: &Path) {}

    fn on_app_menu_action(&self, _callback: Box<dyn FnMut(&dyn crate::Action)>) {}

    fn on_will_open_app_menu(&self, _callback: Box<dyn FnMut()>) {}

    fn on_validate_app_menu_command(&self, _callback: Box<dyn FnMut(&dyn crate::Action) -> bool>) {}

    fn app_path(&self) -> Result<std::path::PathBuf> {
        unimplemented!()
    }

    fn path_for_auxiliary_executable(&self, _name: &str) -> Result<std::path::PathBuf> {
        unimplemented!()
    }

    fn set_cursor_style(&self, style: crate::CursorStyle) {
        *self.active_cursor.lock() = style;
    }

    fn should_auto_hide_scrollbars(&self) -> bool {
        false
    }

    #[cfg(any(target_os = "linux", target_os = "freebsd"))]
    fn write_to_primary(&self, item: ClipboardItem) {
        *self.current_primary_item.lock() = Some(item);
    }

    fn write_to_clipboard(&self, item: ClipboardItem) {
        *self.current_clipboard_item.lock() = Some(item);
    }

    #[cfg(any(target_os = "linux", target_os = "freebsd"))]
    fn read_from_primary(&self) -> Option<ClipboardItem> {
        self.current_primary_item.lock().clone()
    }

    fn read_from_clipboard(&self) -> Option<ClipboardItem> {
        self.current_clipboard_item.lock().clone()
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

    fn register_url_scheme(&self, _: &str) -> Task<anyhow::Result<()>> {
        unimplemented!()
    }

    fn open_with_system(&self, _path: &Path) {
        unimplemented!()
    }
}

impl TestScreenCaptureSource {
    /// Create a fake screen capture source, used for testing.
    pub fn new() -> Self {
        Self {
            active_termination: Default::default(),
        }
    }

    /// Completes the active test stream with a terminal state.
    pub fn finish_stream(&self, termination: ScreenCaptureStreamTermination) {
        let active = self.active_termination.lock().take();
        if let Some(active) = active {
            active.notify(termination);
        }
    }

    fn start_stream(&self) -> oneshot::Receiver<Result<Box<dyn ScreenCaptureStream>>> {
        let (mut tx, rx) = oneshot::channel();
        tx.send(Ok(
            Box::new(TestScreenCaptureStream {}) as Box<dyn ScreenCaptureStream>
        ))
        .ok();
        rx
    }

    fn start_stream_with_termination(
        &self,
        termination: Arc<TestStreamTerminationNotifier>,
    ) -> oneshot::Receiver<Result<Box<dyn ScreenCaptureStream>>> {
        let (mut tx, rx) = oneshot::channel();
        tx.send(Ok(
            Box::new(ActiveTestScreenCaptureStream { termination }) as Box<dyn ScreenCaptureStream>
        ))
        .ok();
        rx
    }

    fn replace_active_termination(&self, termination: Arc<TestStreamTerminationNotifier>) {
        let previous = self.active_termination.lock().replace(termination);
        if let Some(previous) = previous {
            previous.notify(ScreenCaptureStreamTermination::Cancelled);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{PermissionRequestStatus, PermissionStatus, TestDispatcher};
    use rand::{SeedableRng, rngs::StdRng};
    use std::sync::{Arc, mpsc};
    #[cfg(feature = "screen-capture")]
    use std::time::Duration;

    fn test_platform() -> Rc<TestPlatform> {
        let dispatcher = Arc::new(TestDispatcher::new(StdRng::seed_from_u64(0)));
        TestPlatform::new(
            BackgroundExecutor::new(dispatcher.clone()),
            ForegroundExecutor::new(dispatcher),
        )
    }

    #[test]
    fn unsupported_permissions_do_not_report_success() {
        let platform = test_platform();

        assert_eq!(
            platform.accessibility_status(),
            PermissionStatus::Unavailable
        );
        assert_eq!(platform.microphone_status(), PermissionStatus::Unavailable);

        assert_eq!(
            platform.request_accessibility_permission(),
            PermissionRequestStatus::Unavailable
        );

        let (sender, receiver) = mpsc::channel();
        let request_status = platform.request_microphone_permission(Box::new(move |granted| {
            sender.send(granted).unwrap();
        }));

        assert_eq!(request_status, PermissionRequestStatus::Unavailable);
        assert!(!receiver.recv().unwrap());
    }

    #[cfg(feature = "screen-capture")]
    #[test]
    fn legacy_screen_capture_source_rejects_unsupported_termination_notification() {
        struct LegacySource;

        impl ScreenCaptureSource for LegacySource {
            fn metadata(&self) -> Result<SourceMetadata> {
                TestScreenCaptureSource::new().metadata()
            }

            fn stream(
                &self,
                _foreground_executor: &ForegroundExecutor,
                _frame_callback: Box<dyn Fn(ScreenCaptureFrame) + Send>,
            ) -> oneshot::Receiver<Result<Box<dyn ScreenCaptureStream>>> {
                TestScreenCaptureSource::new().start_stream()
            }
        }

        let dispatcher = Arc::new(TestDispatcher::new(StdRng::seed_from_u64(0)));
        let foreground_executor = ForegroundExecutor::new(dispatcher);
        let stream = LegacySource.stream_with_termination(
            &foreground_executor,
            Box::new(|_| {}),
            Box::new(|_| {}),
        );

        let error = match smol::block_on(stream).unwrap() {
            Ok(_) => panic!("legacy source unexpectedly accepted termination notification"),
            Err(error) => error,
        };
        assert!(
            error
                .to_string()
                .contains("runtime termination notification is not supported")
        );

        let stream = LegacySource.stream(&foreground_executor, Box::new(|_| {}));
        assert!(smol::block_on(stream).unwrap().is_ok());
    }

    #[cfg(feature = "screen-capture")]
    #[test]
    fn screen_capture_source_reports_its_first_terminal_status() {
        let source = TestScreenCaptureSource::new();
        let dispatcher = Arc::new(TestDispatcher::new(StdRng::seed_from_u64(0)));
        let foreground_executor = ForegroundExecutor::new(dispatcher);
        let (sender, receiver) = mpsc::channel();

        let stream = source.stream_with_termination(
            &foreground_executor,
            Box::new(|_| {}),
            Box::new(move |termination| sender.send(termination).unwrap()),
        );
        let stream = smol::block_on(stream).unwrap().unwrap();

        source.finish_stream(ScreenCaptureStreamTermination::Failed(
            "capture device disconnected".into(),
        ));
        source.finish_stream(ScreenCaptureStreamTermination::Ended);
        drop(stream);

        assert!(matches!(
            receiver.recv().unwrap(),
            ScreenCaptureStreamTermination::Failed(message) if message == "capture device disconnected"
        ));
        assert!(matches!(
            receiver.try_recv(),
            Err(mpsc::TryRecvError::Disconnected)
        ));
    }

    #[cfg(feature = "screen-capture")]
    #[test]
    fn dropping_screen_capture_stream_reports_cancelled_once() {
        let source = TestScreenCaptureSource::new();
        let dispatcher = Arc::new(TestDispatcher::new(StdRng::seed_from_u64(0)));
        let foreground_executor = ForegroundExecutor::new(dispatcher);
        let (sender, receiver) = mpsc::channel();

        let stream = source.stream_with_termination(
            &foreground_executor,
            Box::new(|_| {}),
            Box::new(move |termination| sender.send(termination).unwrap()),
        );
        let stream = smol::block_on(stream).unwrap().unwrap();

        drop(stream);
        assert_eq!(
            receiver.recv_timeout(Duration::from_millis(100)),
            Ok(ScreenCaptureStreamTermination::Cancelled)
        );

        source.finish_stream(ScreenCaptureStreamTermination::Ended);
        assert!(matches!(
            receiver.try_recv(),
            Err(mpsc::TryRecvError::Disconnected)
        ));
    }

    #[cfg(feature = "screen-capture")]
    #[test]
    fn replacing_a_screen_capture_stream_keeps_callback_ownership_isolated() {
        let source = TestScreenCaptureSource::new();
        let dispatcher = Arc::new(TestDispatcher::new(StdRng::seed_from_u64(0)));
        let foreground_executor = ForegroundExecutor::new(dispatcher);
        let (first_sender, first_receiver) = mpsc::channel();
        let (second_sender, second_receiver) = mpsc::channel();

        let first_stream = source.stream_with_termination(
            &foreground_executor,
            Box::new(|_| {}),
            Box::new(move |termination| first_sender.send(termination).unwrap()),
        );
        let first_stream = smol::block_on(first_stream).unwrap().unwrap();

        let second_stream = source.stream_with_termination(
            &foreground_executor,
            Box::new(|_| {}),
            Box::new(move |termination| second_sender.send(termination).unwrap()),
        );
        let second_stream = smol::block_on(second_stream).unwrap().unwrap();

        assert_eq!(
            first_receiver.recv_timeout(Duration::from_millis(100)),
            Ok(ScreenCaptureStreamTermination::Cancelled)
        );
        drop(first_stream);
        assert!(matches!(
            second_receiver.try_recv(),
            Err(mpsc::TryRecvError::Empty)
        ));

        source.finish_stream(ScreenCaptureStreamTermination::Ended);
        assert_eq!(
            second_receiver.recv_timeout(Duration::from_millis(100)),
            Ok(ScreenCaptureStreamTermination::Ended)
        );
        drop(second_stream);
        assert!(matches!(
            second_receiver.try_recv(),
            Err(mpsc::TryRecvError::Disconnected)
        ));
    }
}

#[cfg(target_os = "windows")]
impl Drop for TestPlatform {
    fn drop(&mut self) {
        unsafe {
            std::mem::ManuallyDrop::drop(&mut self.bitmap_factory);
            windows::Win32::System::Ole::OleUninitialize();
        }
    }
}

struct TestKeyboardLayout;

impl PlatformKeyboardLayout for TestKeyboardLayout {
    fn id(&self) -> &str {
        "zed.keyboard.example"
    }

    fn name(&self) -> &str {
        "zed.keyboard.example"
    }
}

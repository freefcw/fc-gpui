mod window;

use std::rc::Rc;

use gpui::{
    AnyWindowHandle, ClipboardItem, CursorStyle, DisplayId, GraphicalEnvironment, PlatformDisplay,
    PlatformKeyboardLayout, PlatformWindow, WindowParams,
};

use super::{LinuxClient, LinuxKeyboardLayout};
pub(crate) use window::{HeadlessDisplay, HeadlessWindow};

/// The state of a `LinuxPlatform` with no display server.
///
/// It reports one fake display and opens [`HeadlessWindow`]s, which lay out and handle input but
/// draw nothing.
pub(crate) struct HeadlessConnection {
    display: Rc<dyn PlatformDisplay>,
    /// Cloned into every open window, so its count tells whether windows are open.
    window_lease: Rc<()>,
}

impl HeadlessConnection {
    pub(crate) fn new() -> Self {
        Self {
            display: Rc::new(HeadlessDisplay::new()),
            window_lease: Rc::new(()),
        }
    }
}

impl LinuxClient for HeadlessConnection {
    fn compositor_name(&self) -> &'static str {
        "headless"
    }

    fn keyboard_layout(&self) -> Box<dyn PlatformKeyboardLayout> {
        Box::new(LinuxKeyboardLayout::new("unknown".into()))
    }

    fn displays(&self) -> Vec<Rc<dyn PlatformDisplay>> {
        vec![self.display.clone()]
    }

    fn display(&self, id: DisplayId) -> Option<Rc<dyn PlatformDisplay>> {
        (self.display.id() == id).then(|| self.display.clone())
    }

    fn primary_display(&self) -> Option<Rc<dyn PlatformDisplay>> {
        Some(self.display.clone())
    }

    fn open_window(
        &self,
        _handle: AnyWindowHandle,
        params: WindowParams,
    ) -> anyhow::Result<Box<dyn PlatformWindow>> {
        Ok(Box::new(HeadlessWindow::new(
            params,
            self.display.clone(),
            self.window_lease.clone(),
        )))
    }

    fn set_cursor_style(&self, _style: CursorStyle) {}

    fn open_uri(&self, _uri: &str) {}

    fn reveal_path(&self, _path: std::path::PathBuf) {}

    fn write_to_primary(&self, _item: ClipboardItem) {}

    fn write_to_clipboard(&self, _item: ClipboardItem) {}

    fn read_from_primary(&self) -> Option<ClipboardItem> {
        None
    }

    fn read_from_clipboard(&self) -> Option<ClipboardItem> {
        None
    }

    fn active_window(&self) -> Option<AnyWindowHandle> {
        None
    }

    fn window_stack(&self) -> Option<Vec<AnyWindowHandle>> {
        None
    }

    fn has_windows(&self) -> bool {
        Rc::strong_count(&self.window_lease) > 1
    }

    fn graphical_environment(&self) -> Option<GraphicalEnvironment> {
        None
    }

    #[cfg(feature = "screen-capture")]
    fn is_screen_capture_supported(&self) -> bool {
        false
    }

    #[cfg(feature = "screen-capture")]
    fn screen_capture_sources(
        &self,
    ) -> futures::channel::oneshot::Receiver<anyhow::Result<Vec<Rc<dyn gpui::ScreenCaptureSource>>>>
    {
        let (sender, receiver) = futures::channel::oneshot::channel();
        sender
            .send(Err(anyhow::anyhow!(
                "Headless mode does not support screen capture."
            )))
            .ok();
        receiver
    }
}

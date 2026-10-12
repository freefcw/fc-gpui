mod window;

use std::rc::Rc;

use gpui::{
    AnyWindowHandle, ClipboardItem, CursorStyle, DisplayId, GraphicalEnvironment, PlatformDisplay,
    PlatformKeyboardLayout, PlatformWindow, WindowParams,
};

use super::{LinuxClient, LinuxKeyboardLayout};
pub(crate) use window::HeadlessWindow;

/// The state of a `LinuxPlatform` with no display server.
///
/// It reports no displays and opens [`HeadlessWindow`]s, which lay out and handle input but draw
/// nothing.
pub(crate) struct HeadlessConnection {
    /// Cloned into every open window, so its count tells whether windows are open.
    window_lease: Rc<()>,
}

impl HeadlessConnection {
    pub(crate) fn new() -> Self {
        Self {
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
        Vec::new()
    }

    fn display(&self, _id: DisplayId) -> Option<Rc<dyn PlatformDisplay>> {
        None
    }

    fn primary_display(&self) -> Option<Rc<dyn PlatformDisplay>> {
        None
    }

    fn open_window(
        &self,
        _handle: AnyWindowHandle,
        params: WindowParams,
    ) -> anyhow::Result<Box<dyn PlatformWindow>> {
        Ok(Box::new(HeadlessWindow::new(
            params,
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

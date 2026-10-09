use std::{path::PathBuf, rc::Rc};

use gpui::{
    AnyWindowHandle, AttentionType, ClipboardItem, CursorStyle, FocusedWindowInfo,
    GraphicalEnvironment, Keystroke, PlatformDisplay, PlatformKeyboardLayout, PlatformWindow,
    TrayIconRenderingMode, TrayMenuItem, WindowParams, WindowingModes,
};

#[cfg(feature = "wayland")]
use super::WaylandClient;
#[cfg(feature = "x11")]
use super::X11Connection;
use super::{HeadlessConnection, LinuxClient};

/// Returns the display server `environment` selects among the windowed modes in `modes`:
/// Wayland, then X11.
#[cfg_attr(
    not(any(feature = "wayland", feature = "x11")),
    allow(
        unused_variables,
        reason = "no windowed modes without a display backend"
    )
)]
pub(crate) fn select_backend(
    modes: WindowingModes,
    environment: &GraphicalEnvironment,
) -> Option<Backend> {
    let is_set =
        |value: &Option<std::ffi::OsString>| value.as_ref().is_some_and(|value| !value.is_empty());
    #[cfg(feature = "wayland")]
    if modes.contains(WindowingModes::WAYLAND) && is_set(&environment.wayland_display) {
        return Some(Backend::Wayland);
    }
    #[cfg(feature = "x11")]
    if modes.contains(WindowingModes::X11) && is_set(&environment.x11_display) {
        return Some(Backend::X11);
    }
    None
}

/// A display server that [`DisplayConnection`] can connect to.
#[derive(Clone, Copy)]
pub(crate) enum Backend {
    #[cfg(feature = "wayland")]
    Wayland,
    #[cfg(feature = "x11")]
    X11,
}

/// The display server a `LinuxPlatform` is connected to.
pub(crate) enum DisplayConnection {
    Headless(HeadlessConnection),
    #[cfg(feature = "wayland")]
    Wayland(WaylandClient),
    #[cfg(feature = "x11")]
    X11(X11Connection),
}

macro_rules! dispatch {
    ($self:expr, $connection:ident => $connected:expr, headless => $headless:expr) => {
        dispatch!($self, $connection => $connected, headless(_) => $headless)
    };
    ($self:expr, $connection:ident => $connected:expr, headless($state:pat) => $headless:expr) => {
        match $self {
            DisplayConnection::Headless($state) => $headless,
            #[cfg(feature = "wayland")]
            DisplayConnection::Wayland($connection) => $connected,
            #[cfg(feature = "x11")]
            DisplayConnection::X11($connection) => $connected,
        }
    };
}

#[cfg_attr(
    not(any(feature = "wayland", feature = "x11")),
    allow(
        unused_variables,
        reason = "only headless mode exists without a display backend"
    )
)]
impl DisplayConnection {
    pub(crate) fn is_headless(&self) -> bool {
        matches!(self, DisplayConnection::Headless(_))
    }

    pub(crate) fn compositor_name(&self) -> &'static str {
        dispatch!(self, connection => connection.compositor_name(), headless => "headless")
    }

    pub(crate) fn has_windows(&self) -> bool {
        dispatch!(self, connection => connection.has_windows(), headless(connection) => connection.has_windows())
    }

    pub(crate) fn graphical_environment(&self) -> Option<GraphicalEnvironment> {
        dispatch!(self, connection => connection.graphical_environment(), headless => None)
    }

    pub(crate) fn keyboard_layout(&self) -> Box<dyn PlatformKeyboardLayout> {
        dispatch!(self, connection => connection.keyboard_layout(), headless(connection) => connection.keyboard_layout())
    }

    pub(crate) fn displays(&self) -> Vec<Rc<dyn PlatformDisplay>> {
        dispatch!(self, connection => connection.displays(), headless(connection) => connection.displays())
    }

    pub(crate) fn primary_display(&self) -> Option<Rc<dyn PlatformDisplay>> {
        dispatch!(
            self,
            connection => connection.primary_display(),
            headless(connection) => connection.primary_display()
        )
    }

    pub(crate) fn open_window(
        &self,
        handle: AnyWindowHandle,
        params: WindowParams,
    ) -> anyhow::Result<Box<dyn PlatformWindow>> {
        dispatch!(
            self,
            connection => connection.open_window(handle, params),
            headless(connection) => connection.open_window(handle, params)
        )
    }

    pub(crate) fn active_window(&self) -> Option<AnyWindowHandle> {
        dispatch!(self, connection => connection.active_window(), headless(connection) => connection.active_window())
    }

    pub(crate) fn window_stack(&self) -> Option<Vec<AnyWindowHandle>> {
        dispatch!(self, connection => connection.window_stack(), headless(connection) => connection.window_stack())
    }

    pub(crate) fn set_cursor_style(&self, style: CursorStyle) {
        dispatch!(self, connection => connection.set_cursor_style(style), headless(connection) => connection.set_cursor_style(style))
    }

    pub(crate) fn open_uri(&self, uri: &str) {
        dispatch!(self, connection => connection.open_uri(uri), headless(connection) => connection.open_uri(uri))
    }

    pub(crate) fn reveal_path(&self, path: PathBuf) {
        dispatch!(self, connection => connection.reveal_path(path), headless(connection) => connection.reveal_path(path))
    }

    pub(crate) fn write_to_primary(&self, item: ClipboardItem) {
        dispatch!(self, connection => connection.write_to_primary(item), headless(connection) => connection.write_to_primary(item))
    }

    pub(crate) fn write_to_clipboard(&self, item: ClipboardItem) {
        dispatch!(self, connection => connection.write_to_clipboard(item), headless(connection) => connection.write_to_clipboard(item))
    }

    pub(crate) fn read_from_primary(&self) -> Option<ClipboardItem> {
        dispatch!(self, connection => connection.read_from_primary(), headless(connection) => connection.read_from_primary())
    }

    pub(crate) fn read_from_clipboard(&self) -> Option<ClipboardItem> {
        dispatch!(self, connection => connection.read_from_clipboard(), headless(connection) => connection.read_from_clipboard())
    }

    pub(crate) fn focused_window_info(&self) -> Option<FocusedWindowInfo> {
        dispatch!(self, connection => connection.focused_window_info(), headless(connection) => connection.focused_window_info())
    }

    pub(crate) fn set_tray_icon(&self, icon: Option<&[u8]>) {
        dispatch!(self, connection => connection.set_tray_icon(icon), headless(connection) => connection.set_tray_icon(icon))
    }

    pub(crate) fn set_tray_icon_rendering_mode(&self, rendering_mode: TrayIconRenderingMode) {
        dispatch!(
            self,
            connection => connection.set_tray_icon_rendering_mode(rendering_mode),
            headless(connection) => connection.set_tray_icon_rendering_mode(rendering_mode)
        )
    }

    pub(crate) fn set_tray_menu(&self, menu: Vec<TrayMenuItem>) {
        dispatch!(self, connection => connection.set_tray_menu(menu), headless(connection) => connection.set_tray_menu(menu))
    }

    pub(crate) fn set_tray_tooltip(&self, tooltip: &str) {
        dispatch!(self, connection => connection.set_tray_tooltip(tooltip), headless(connection) => connection.set_tray_tooltip(tooltip))
    }

    pub(crate) fn set_tray_panel_mode(&self, enabled: bool) {
        dispatch!(self, connection => connection.set_tray_panel_mode(enabled), headless(connection) => connection.set_tray_panel_mode(enabled))
    }

    pub(crate) fn register_global_hotkey(
        &self,
        id: u32,
        keystroke: &Keystroke,
    ) -> anyhow::Result<()> {
        dispatch!(
            self,
            connection => connection.register_global_hotkey(id, keystroke),
            headless(connection) => connection.register_global_hotkey(id, keystroke)
        )
    }

    pub(crate) fn unregister_global_hotkey(&self, id: u32) {
        dispatch!(self, connection => connection.unregister_global_hotkey(id), headless(connection) => connection.unregister_global_hotkey(id))
    }

    pub(crate) fn system_idle_time(&self) -> Option<std::time::Duration> {
        dispatch!(self, connection => connection.system_idle_time(), headless(connection) => connection.system_idle_time())
    }

    pub(crate) fn request_user_attention(
        &self,
        level: AttentionType,
        handle: Option<AnyWindowHandle>,
    ) {
        dispatch!(
            self,
            connection => connection.request_user_attention(level, handle),
            headless(connection) => connection.request_user_attention(level, handle)
        )
    }

    pub(crate) fn cancel_user_attention(&self, handle: Option<AnyWindowHandle>) {
        dispatch!(
            self,
            connection => connection.cancel_user_attention(handle),
            headless(connection) => connection.cancel_user_attention(handle)
        )
    }

    #[cfg(any(feature = "wayland", feature = "x11"))]
    pub(crate) fn window_identifier(
        &self,
    ) -> futures::future::BoxFuture<'static, Option<ashpd::WindowIdentifier>> {
        dispatch!(
            self,
            connection => Box::pin(connection.window_identifier()),
            headless => Box::pin(std::future::ready(None))
        )
    }

    #[cfg(feature = "screen-capture")]
    pub(crate) fn is_screen_capture_supported(&self) -> bool {
        dispatch!(
            self,
            connection => connection.is_screen_capture_supported(),
            headless(connection) => connection.is_screen_capture_supported()
        )
    }

    #[cfg(feature = "screen-capture")]
    pub(crate) fn screen_capture_sources(
        &self,
    ) -> futures::channel::oneshot::Receiver<anyhow::Result<Vec<Rc<dyn gpui::ScreenCaptureSource>>>>
    {
        dispatch!(
            self,
            connection => connection.screen_capture_sources(),
            headless(connection) => connection.screen_capture_sources()
        )
    }
}

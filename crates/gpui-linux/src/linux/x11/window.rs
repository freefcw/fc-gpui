use anyhow::{Context as _, anyhow};
use x11rb::connection::RequestConnection;

use crate::linux::X11ClientStatePtr;
use gpui::{
    AnyWindowHandle, Bounds, Decorations, DevicePixels, ForegroundExecutor, GpuResourceBudget,
    GpuSpecs, Modifiers, Pixels, PlatformAtlas, PlatformDisplay, PlatformInput,
    PlatformInputHandler, PlatformWindow, Point, PromptButton, PromptLevel, RequestFrameOptions,
    ResizeEdge, ScaledPixels, Scene, Size, Tiling, WindowAppearance, WindowBackgroundAppearance,
    WindowBounds, WindowControlArea, WindowDecorations, WindowKind, WindowParams, WindowVisibility,
    px, size,
};
use gpui_wgpu::{GpuContext, WgpuRenderer, WgpuSurfaceConfig};

use raw_window_handle as rwh;
use util::{ResultExt, maybe};
use x11rb::{
    connection::Connection,
    cookie::{Cookie, VoidCookie},
    errors::ConnectionError,
    properties::{WmHints, WmSizeHints, WmSizeHintsSpecification},
    protocol::{
        sync,
        xinput::{self, ConnectionExt as _},
        xproto::{self, ClientMessageEvent, ConnectionExt, TranslateCoordinatesReply},
    },
    wrapper::ConnectionExt as _,
    xcb_ffi::XCBConnection,
};

use std::{
    cell::RefCell, ffi::c_void, fmt::Display, mem::size_of, num::NonZeroU32, ops::Div,
    ptr::NonNull, rc::Rc, sync::Arc,
};

use super::{X11Display, XINPUT_ALL_DEVICE_GROUPS, XINPUT_ALL_DEVICES};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum X11WindowType {
    Normal,
    Notification,
    Dialog,
    Dock,
}

x11rb::atom_manager! {
    pub XcbAtoms: AtomsCookie {
        XA_ATOM,
        XdndAware,
        XdndStatus,
        XdndEnter,
        XdndLeave,
        XdndPosition,
        XdndSelection,
        XdndDrop,
        XdndFinished,
        XdndTypeList,
        XdndActionCopy,
        TextUriList: b"text/uri-list",
        UTF8_STRING,
        TEXT,
        STRING,
        TEXT_PLAIN_UTF8: b"text/plain;charset=utf-8",
        TEXT_PLAIN: b"text/plain",
        XDND_DATA,
        WM_PROTOCOLS,
        WM_DELETE_WINDOW,
        WM_CHANGE_STATE,
        WM_TRANSIENT_FOR,
        _NET_WM_PID,
        _NET_WM_NAME,
        _NET_WM_ICON,
        _NET_WM_STATE,
        _NET_WM_STATE_MAXIMIZED_VERT,
        _NET_WM_STATE_MAXIMIZED_HORZ,
        _NET_WM_STATE_FULLSCREEN,
        _NET_WM_STATE_HIDDEN,
        _NET_WM_STATE_FOCUSED,
        _NET_WM_STATE_ABOVE,
        _NET_ACTIVE_WINDOW,
        _NET_WM_SYNC_REQUEST,
        _NET_WM_SYNC_REQUEST_COUNTER,
        _NET_WM_BYPASS_COMPOSITOR,
        _NET_WM_MOVERESIZE,
        _NET_WM_WINDOW_TYPE,
        _NET_WM_WINDOW_TYPE_NORMAL,
        _NET_WM_WINDOW_TYPE_NOTIFICATION,
        _NET_WM_WINDOW_TYPE_DIALOG,
        _NET_WM_WINDOW_TYPE_DOCK,
        _NET_WM_SYNC,
        _NET_WM_STATE_DEMANDS_ATTENTION,
        _NET_SUPPORTED,
        _MOTIF_WM_HINTS,
        _GTK_SHOW_WINDOW_MENU,
        _GTK_FRAME_EXTENTS,
        _GTK_EDGE_CONSTRAINTS,
        _NET_CLIENT_LIST_STACKING,
    }
}

fn window_type_for_kind(kind: &WindowKind) -> X11WindowType {
    match kind {
        WindowKind::Normal => X11WindowType::Normal,
        WindowKind::PopUp => X11WindowType::Notification,
        WindowKind::Floating => X11WindowType::Dialog,
        WindowKind::Overlay => X11WindowType::Dock,
        #[cfg(feature = "wayland")]
        WindowKind::LayerShell(_) => {
            unreachable!("layer-shell windows are rejected before X11 window creation")
        }
    }
}

fn ensure_window_kind_supported(_kind: &WindowKind) -> anyhow::Result<()> {
    #[cfg(feature = "wayland")]
    if matches!(_kind, WindowKind::LayerShell(_)) {
        return Err(gpui::layer_shell::LayerShellNotSupportedError.into());
    }

    Ok(())
}

fn query_render_extent(
    xcb: &Rc<XCBConnection>,
    x_window: xproto::Window,
) -> anyhow::Result<Size<DevicePixels>> {
    let reply = get_reply(|| "X11 GetGeometry failed.", xcb.get_geometry(x_window))?;
    Ok(size(
        DevicePixels(reply.width as i32),
        DevicePixels(reply.height as i32),
    ))
}

fn resize_edge_to_moveresize(edge: ResizeEdge) -> u32 {
    match edge {
        ResizeEdge::TopLeft => 0,
        ResizeEdge::Top => 1,
        ResizeEdge::TopRight => 2,
        ResizeEdge::Right => 3,
        ResizeEdge::BottomRight => 4,
        ResizeEdge::Bottom => 5,
        ResizeEdge::BottomLeft => 6,
        ResizeEdge::Left => 7,
    }
}

#[derive(Debug)]
struct EdgeConstraints {
    top_tiled: bool,
    #[allow(dead_code)]
    top_resizable: bool,

    right_tiled: bool,
    #[allow(dead_code)]
    right_resizable: bool,

    bottom_tiled: bool,
    #[allow(dead_code)]
    bottom_resizable: bool,

    left_tiled: bool,
    #[allow(dead_code)]
    left_resizable: bool,
}

impl EdgeConstraints {
    fn from_atom(atom: u32) -> Self {
        EdgeConstraints {
            top_tiled: (atom & (1 << 0)) != 0,
            top_resizable: (atom & (1 << 1)) != 0,
            right_tiled: (atom & (1 << 2)) != 0,
            right_resizable: (atom & (1 << 3)) != 0,
            bottom_tiled: (atom & (1 << 4)) != 0,
            bottom_resizable: (atom & (1 << 5)) != 0,
            left_tiled: (atom & (1 << 6)) != 0,
            left_resizable: (atom & (1 << 7)) != 0,
        }
    }

    fn to_tiling(&self) -> Tiling {
        Tiling {
            top: self.top_tiled,
            right: self.right_tiled,
            bottom: self.bottom_tiled,
            left: self.left_tiled,
        }
    }
}

#[derive(Copy, Clone, Debug)]
struct Visual {
    id: xproto::Visualid,
    colormap: u32,
    depth: u8,
}

struct VisualSet {
    inherit: Visual,
    opaque: Option<Visual>,
    transparent: Option<Visual>,
    root: u32,
    black_pixel: u32,
}

fn find_visuals(xcb: &XCBConnection, screen_index: usize) -> VisualSet {
    let screen = &xcb.setup().roots[screen_index];
    let mut set = VisualSet {
        inherit: Visual {
            id: screen.root_visual,
            colormap: screen.default_colormap,
            depth: screen.root_depth,
        },
        opaque: None,
        transparent: None,
        root: screen.root,
        black_pixel: screen.black_pixel,
    };

    for depth_info in screen.allowed_depths.iter() {
        for visual_type in depth_info.visuals.iter() {
            let visual = Visual {
                id: visual_type.visual_id,
                colormap: 0,
                depth: depth_info.depth,
            };
            log::debug!(
                "Visual id: {}, class: {:?}, depth: {}, bits_per_value: {}, masks: 0x{:x} 0x{:x} 0x{:x}",
                visual_type.visual_id,
                visual_type.class,
                depth_info.depth,
                visual_type.bits_per_rgb_value,
                visual_type.red_mask,
                visual_type.green_mask,
                visual_type.blue_mask,
            );

            if (
                visual_type.red_mask,
                visual_type.green_mask,
                visual_type.blue_mask,
            ) != (0xFF0000, 0xFF00, 0xFF)
            {
                continue;
            }
            let color_mask = visual_type.red_mask | visual_type.green_mask | visual_type.blue_mask;
            let alpha_mask = color_mask as usize ^ ((1usize << depth_info.depth) - 1);

            if alpha_mask == 0 {
                if set.opaque.is_none() {
                    set.opaque = Some(visual);
                }
            } else {
                if set.transparent.is_none() {
                    set.transparent = Some(visual);
                }
            }
        }
    }

    set
}

#[derive(Clone, Debug)]
struct RawWindow {
    connection: *mut c_void,
    screen_id: usize,
    window_id: u32,
    visual_id: u32,
}

unsafe impl Send for RawWindow {}
unsafe impl Sync for RawWindow {}

#[derive(Default)]
pub struct Callbacks {
    request_frame: Option<Box<dyn FnMut(RequestFrameOptions)>>,
    input: Option<Box<dyn FnMut(PlatformInput) -> gpui::DispatchEventResult>>,
    active_status_change: Option<Box<dyn FnMut(bool)>>,
    visibility_change: Option<Box<dyn FnMut(WindowVisibility)>>,
    hovered_status_change: Option<Box<dyn FnMut(bool)>>,
    resize: Option<Box<dyn FnMut(Size<Pixels>, f32)>>,
    moved: Option<Box<dyn FnMut()>>,
    should_close: Option<Box<dyn FnMut() -> bool>>,
    close: Option<Box<dyn FnOnce()>>,
    appearance_changed: Option<Box<dyn FnMut()>>,
}

pub struct X11WindowState {
    pub destroyed: bool,
    client: X11ClientStatePtr,
    executor: ForegroundExecutor,
    atoms: XcbAtoms,
    x_root_window: xproto::Window,
    x_screen_index: usize,
    visual_id: u32,
    pub(crate) counter_id: sync::Counter,
    pub(crate) last_sync_counter: Option<sync::Int64>,
    bounds: Bounds<Pixels>,
    scale_factor: f32,
    renderer: WgpuRenderer,
    display: Rc<dyn PlatformDisplay>,
    input_handler: Option<PlatformInputHandler>,
    appearance: WindowAppearance,
    background_appearance: WindowBackgroundAppearance,
    maximized_vertical: bool,
    maximized_horizontal: bool,
    hidden: bool,
    /// `WindowParams::focus` captured at creation; consumed by the first map
    /// operation, which requests `_NET_ACTIVE_WINDOW` right after mapping.
    pending_focus: bool,
    active: bool,
    /// Owned by the client's `WindowRef`, which combines the mapped state with
    /// `VisibilityNotify`; this is the last value it reported.
    visibility: WindowVisibility,
    hovered: bool,
    force_render_after_recovery: bool,
    fullscreen: bool,
    is_resizable: bool,
    client_side_decorations_supported: bool,
    decorations: WindowDecorations,
    edge_constraints: Option<EdgeConstraints>,
    pub handle: AnyWindowHandle,
    last_insets: [u32; 4],
    #[cfg(feature = "accessibility")]
    accesskit_adapter: Option<accesskit_unix::Adapter>,
}

impl X11WindowState {
    fn is_transparent(&self) -> bool {
        self.background_appearance != WindowBackgroundAppearance::Opaque
    }
}

fn window_type_atom(kind: &WindowKind, atoms: &XcbAtoms) -> xproto::Atom {
    match window_type_for_kind(kind) {
        X11WindowType::Normal => atoms._NET_WM_WINDOW_TYPE_NORMAL,
        X11WindowType::Notification => atoms._NET_WM_WINDOW_TYPE_NOTIFICATION,
        X11WindowType::Dialog => atoms._NET_WM_WINDOW_TYPE_DIALOG,
        X11WindowType::Dock => atoms._NET_WM_WINDOW_TYPE_DOCK,
    }
}

fn normal_size_hints(params: &WindowParams) -> Option<WmSizeHints> {
    let mut size_hints = WmSizeHints::new();

    if let Some(size) = params.window_min_size {
        size_hints.min_size = Some((f32::from(size.width) as i32, f32::from(size.height) as i32));
    }

    if !params.is_resizable {
        let size = params.bounds.size;
        let fixed_size = (f32::from(size.width) as i32, f32::from(size.height) as i32);
        size_hints.size = Some((
            WmSizeHintsSpecification::ProgramSpecified,
            fixed_size.0,
            fixed_size.1,
        ));
        size_hints.min_size = Some(fixed_size);
        size_hints.max_size = Some(fixed_size);
    }

    (size_hints.size.is_some() || size_hints.min_size.is_some() || size_hints.max_size.is_some())
        .then_some(size_hints)
}

fn motif_hints_data(decorations: WindowDecorations, is_resizable: bool) -> [u32; 5] {
    const MWM_HINTS_FUNCTIONS: u32 = 1 << 0;
    const MWM_HINTS_DECORATIONS: u32 = 1 << 1;
    const MWM_FUNC_ALL: u32 = 1 << 0;
    const MWM_FUNC_MOVE: u32 = 1 << 2;
    const MWM_FUNC_MINIMIZE: u32 = 1 << 3;
    const MWM_FUNC_CLOSE: u32 = 1 << 5;

    let flags = MWM_HINTS_FUNCTIONS | MWM_HINTS_DECORATIONS;
    let functions = if is_resizable {
        MWM_FUNC_ALL
    } else {
        MWM_FUNC_MOVE | MWM_FUNC_MINIMIZE | MWM_FUNC_CLOSE
    };
    let decorations = match decorations {
        WindowDecorations::Server => 1,
        WindowDecorations::Client => 0,
    };

    [flags, functions, decorations, 0, 0]
}

#[derive(Clone)]
pub(crate) struct X11WindowStatePtr {
    pub state: Rc<RefCell<X11WindowState>>,
    pub(crate) callbacks: Rc<RefCell<Callbacks>>,
    xcb: Rc<XCBConnection>,
    pub(crate) x_window: xproto::Window,
}

impl rwh::HasWindowHandle for RawWindow {
    fn window_handle(&self) -> Result<rwh::WindowHandle<'_>, rwh::HandleError> {
        let Some(non_zero) = NonZeroU32::new(self.window_id) else {
            log::error!("RawWindow.window_id zero when getting window handle.");
            return Err(rwh::HandleError::Unavailable);
        };
        let mut handle = rwh::XcbWindowHandle::new(non_zero);
        handle.visual_id = NonZeroU32::new(self.visual_id);
        Ok(unsafe { rwh::WindowHandle::borrow_raw(handle.into()) })
    }
}
impl rwh::HasDisplayHandle for RawWindow {
    fn display_handle(&self) -> Result<rwh::DisplayHandle<'_>, rwh::HandleError> {
        let Some(non_zero) = NonNull::new(self.connection) else {
            log::error!("Null RawWindow.connection when getting display handle.");
            return Err(rwh::HandleError::Unavailable);
        };
        let handle = rwh::XcbDisplayHandle::new(Some(non_zero), self.screen_id as i32);
        Ok(unsafe { rwh::DisplayHandle::borrow_raw(handle.into()) })
    }
}

impl rwh::HasWindowHandle for X11Window {
    fn window_handle(&self) -> Result<rwh::WindowHandle<'_>, rwh::HandleError> {
        unimplemented!()
    }
}
impl rwh::HasDisplayHandle for X11Window {
    fn display_handle(&self) -> Result<rwh::DisplayHandle<'_>, rwh::HandleError> {
        unimplemented!()
    }
}

pub(crate) fn xcb_flush(xcb: &XCBConnection) {
    xcb.flush()
        .map_err(handle_connection_error)
        .context("X11 flush failed")
        .log_err();
}

pub(crate) fn check_reply<E, F, C>(
    failure_context: F,
    result: Result<VoidCookie<'_, C>, ConnectionError>,
) -> anyhow::Result<()>
where
    E: Display + Send + Sync + 'static,
    F: FnOnce() -> E,
    C: RequestConnection,
{
    result
        .map_err(handle_connection_error)
        .and_then(|response| response.check().map_err(|reply_error| anyhow!(reply_error)))
        .with_context(failure_context)
}

pub(crate) fn get_reply<E, F, C, O>(
    failure_context: F,
    result: Result<Cookie<'_, C, O>, ConnectionError>,
) -> anyhow::Result<O>
where
    E: Display + Send + Sync + 'static,
    F: FnOnce() -> E,
    C: RequestConnection,
    O: x11rb::x11_utils::TryParse,
{
    result
        .map_err(handle_connection_error)
        .and_then(|response| response.reply().map_err(|reply_error| anyhow!(reply_error)))
        .with_context(failure_context)
}

/// Sets or clears the ICCCM WM_HINTS urgency flag, preserving the other hints.
///
/// Clearing when the flag isn't set is skipped: writing it back would create a
/// WM_HINTS property on windows that never requested attention, and would add an
/// X round trip to every window state change.
fn set_wm_hints_urgency(xcb: &XCBConnection, x_window: xproto::Window, urgent: bool) {
    let mut hints = WmHints::new();
    match WmHints::get(xcb, x_window) {
        Ok(cookie) => match cookie.reply() {
            Ok(Some(existing_hints)) => hints = existing_hints,
            Ok(None) => {}
            Err(error) => {
                if !urgent {
                    return;
                }
                log::debug!("failed to read X11 WM_HINTS before setting urgency: {error}")
            }
        },
        Err(error) => {
            if !urgent {
                return;
            }
            log::debug!("failed to request X11 WM_HINTS before setting urgency: {error}")
        }
    }

    if !urgent && !hints.urgent {
        return;
    }

    hints.urgent = urgent;
    check_reply(
        || "X11 ChangeProperty for WM_HINTS urgency failed.",
        hints.set(xcb, x_window),
    )
    .log_err();
    xcb_flush(xcb);
}

/// Convert X11 connection errors to `anyhow::Error` and panic for unrecoverable errors.
pub(crate) fn handle_connection_error(err: ConnectionError) -> anyhow::Error {
    match err {
        ConnectionError::UnknownError => anyhow!("X11 connection: Unknown error"),
        ConnectionError::UnsupportedExtension => anyhow!("X11 connection: Unsupported extension"),
        ConnectionError::MaximumRequestLengthExceeded => {
            anyhow!("X11 connection: Maximum request length exceeded")
        }
        ConnectionError::FdPassingFailed => {
            panic!("X11 connection: File descriptor passing failed")
        }
        ConnectionError::ParseError(parse_error) => {
            anyhow!(parse_error).context("Parse error in X11 response")
        }
        ConnectionError::InsufficientMemory => panic!("X11 connection: Insufficient memory"),
        ConnectionError::IoError(err) => anyhow!(err).context("X11 connection: IOError"),
        _ => anyhow!(err),
    }
}

impl X11WindowState {
    pub fn new(
        handle: AnyWindowHandle,
        client: X11ClientStatePtr,
        executor: ForegroundExecutor,
        gpu_context: GpuContext,
        params: WindowParams,
        gpu_resource_budget: &GpuResourceBudget,
        xcb: &Rc<XCBConnection>,
        client_side_decorations_supported: bool,
        x_main_screen_index: usize,
        x_window: xproto::Window,
        atoms: &XcbAtoms,
        scale_factor: f32,
        appearance: WindowAppearance,
        supports_xinput_gestures: bool,
        parent_window: Option<xproto::Window>,
    ) -> anyhow::Result<Self> {
        ensure_window_kind_supported(&params.kind)?;

        let x_screen_index = params
            .display_id
            .map_or(x_main_screen_index, gpui::DisplayId::index);

        let visual_set = find_visuals(xcb, x_screen_index);

        let visual = match visual_set.transparent {
            Some(visual) => visual,
            None => {
                log::warn!("Unable to find a transparent visual",);
                visual_set.inherit
            }
        };
        log::info!("Using {:?}", visual);

        let colormap = if visual.colormap != 0 {
            visual.colormap
        } else {
            let id = xcb.generate_id()?;
            log::info!("Creating colormap {}", id);
            check_reply(
                || format!("X11 CreateColormap failed. id: {}", id),
                xcb.create_colormap(xproto::ColormapAlloc::NONE, id, visual_set.root, visual.id),
            )?;
            id
        };

        let win_aux = xproto::CreateWindowAux::new()
            // https://stackoverflow.com/questions/43218127/x11-xlib-xcb-creating-a-window-requires-border-pixel-if-specifying-colormap-wh
            .border_pixel(visual_set.black_pixel)
            .colormap(colormap)
            .event_mask(
                xproto::EventMask::EXPOSURE
                    | xproto::EventMask::STRUCTURE_NOTIFY
                    | xproto::EventMask::FOCUS_CHANGE
                    | xproto::EventMask::KEY_PRESS
                    | xproto::EventMask::KEY_RELEASE
                    | xproto::EventMask::PROPERTY_CHANGE
                    | xproto::EventMask::VISIBILITY_CHANGE,
            );

        let mut bounds = params.bounds.to_device_pixels(scale_factor);
        if bounds.size.width.0 == 0 || bounds.size.height.0 == 0 {
            log::warn!(
                "Window bounds contain a zero value. height={}, width={}. Falling back to defaults.",
                bounds.size.height.0,
                bounds.size.width.0
            );
            bounds.size.width = 800.into();
            bounds.size.height = 600.into();
        }

        check_reply(
            || {
                format!(
                    "X11 CreateWindow failed. depth: {}, x_window: {}, visual_set.root: {}, bounds.origin.x.0: {}, bounds.origin.y.0: {}, bounds.size.width.0: {}, bounds.size.height.0: {}",
                    visual.depth,
                    x_window,
                    visual_set.root,
                    bounds.origin.x.0 + 2,
                    bounds.origin.y.0,
                    bounds.size.width.0,
                    bounds.size.height.0
                )
            },
            xcb.create_window(
                visual.depth,
                x_window,
                visual_set.root,
                (bounds.origin.x.0 + 2) as i16,
                bounds.origin.y.0 as i16,
                bounds.size.width.0 as u16,
                bounds.size.height.0 as u16,
                0,
                xproto::WindowClass::INPUT_OUTPUT,
                visual.id,
                &win_aux,
            ),
        )?;

        // Collect errors during setup, so that window can be destroyed on failure.
        let setup_result = maybe!({
            let pid = std::process::id();
            check_reply(
                || "X11 ChangeProperty for _NET_WM_PID failed.",
                xcb.change_property32(
                    xproto::PropMode::REPLACE,
                    x_window,
                    atoms._NET_WM_PID,
                    xproto::AtomEnum::CARDINAL,
                    &[pid],
                ),
            )?;

            if let Some(size_hints) = normal_size_hints(&params) {
                check_reply(
                    || {
                        format!(
                            "X11 change of WM_SIZE_HINTS failed. size_hints: {:?}",
                            size_hints
                        )
                    },
                    size_hints.set_normal_hints(xcb, x_window),
                )?;
            }

            let reply = get_reply(|| "X11 GetGeometry failed.", xcb.get_geometry(x_window))?;
            if reply.x == 0 && reply.y == 0 {
                bounds.origin.x.0 += 2;
                // Work around a bug where our rendered content appears
                // outside the window bounds when opened at the default position
                // (14px, 49px on X + Gnome + Ubuntu 22).
                let x = bounds.origin.x.0;
                let y = bounds.origin.y.0;
                check_reply(
                    || format!("X11 ConfigureWindow failed. x: {}, y: {}", x, y),
                    xcb.configure_window(x_window, &xproto::ConfigureWindowAux::new().x(x).y(y)),
                )?;
            }
            if let Some(titlebar) = params.titlebar
                && let Some(title) = titlebar.title
            {
                check_reply(
                    || "X11 ChangeProperty8 on window title failed.",
                    xcb.change_property8(
                        xproto::PropMode::REPLACE,
                        x_window,
                        xproto::AtomEnum::WM_NAME,
                        xproto::AtomEnum::STRING,
                        title.as_bytes(),
                    ),
                )?;
            }

            let window_type = window_type_atom(&params.kind, atoms);
            check_reply(
                || "X11 ChangeProperty32 setting window type failed.",
                xcb.change_property32(
                    xproto::PropMode::REPLACE,
                    x_window,
                    atoms._NET_WM_WINDOW_TYPE,
                    xproto::AtomEnum::ATOM,
                    &[window_type],
                ),
            )?;

            if params.kind == WindowKind::Floating {
                if let Some(parent_window) = parent_window {
                    // WM_TRANSIENT_FOR hint indicating the main application window. For floating windows, we set
                    // a parent window (WM_TRANSIENT_FOR) such that the window manager knows where to
                    // place the floating window in relation to the main window.
                    // https://specifications.freedesktop.org/wm-spec/1.4/ar01s05.html
                    check_reply(
                        || "X11 ChangeProperty32 setting WM_TRANSIENT_FOR for floating window failed.",
                        xcb.change_property32(
                            xproto::PropMode::REPLACE,
                            x_window,
                            atoms.WM_TRANSIENT_FOR,
                            xproto::AtomEnum::WINDOW,
                            &[parent_window],
                        ),
                    )?;
                }
            } else if params.kind == WindowKind::Overlay {
                check_reply(
                    || "X11 ChangeProperty32 setting _NET_WM_STATE_ABOVE for overlay failed.",
                    xcb.change_property32(
                        xproto::PropMode::REPLACE,
                        x_window,
                        atoms._NET_WM_STATE,
                        xproto::AtomEnum::ATOM,
                        &[atoms._NET_WM_STATE_ABOVE],
                    ),
                )?;
            }

            if params.mouse_passthrough {
                use x11rb::protocol::shape;
                check_reply(
                    || "X11 shape::rectangles for mouse passthrough failed.",
                    shape::rectangles(
                        xcb.as_ref(),
                        shape::SO::SET,
                        shape::SK::INPUT,
                        xproto::ClipOrdering::UNSORTED,
                        x_window,
                        0,
                        0,
                        &[],
                    ),
                )?;
            }

            check_reply(
                || "X11 ChangeProperty32 setting protocols failed.",
                xcb.change_property32(
                    xproto::PropMode::REPLACE,
                    x_window,
                    atoms.WM_PROTOCOLS,
                    xproto::AtomEnum::ATOM,
                    &[atoms.WM_DELETE_WINDOW, atoms._NET_WM_SYNC_REQUEST],
                ),
            )?;

            get_reply(
                || "X11 sync protocol initialize failed.",
                sync::initialize(xcb, 3, 1),
            )?;
            let sync_request_counter = xcb.generate_id()?;
            check_reply(
                || "X11 sync CreateCounter failed.",
                sync::create_counter(xcb, sync_request_counter, sync::Int64 { lo: 0, hi: 0 }),
            )?;

            check_reply(
                || "X11 ChangeProperty32 setting sync request counter failed.",
                xcb.change_property32(
                    xproto::PropMode::REPLACE,
                    x_window,
                    atoms._NET_WM_SYNC_REQUEST_COUNTER,
                    xproto::AtomEnum::CARDINAL,
                    &[sync_request_counter],
                ),
            )?;

            let mut xi_event_mask = xinput::XIEventMask::MOTION
                | xinput::XIEventMask::BUTTON_PRESS
                | xinput::XIEventMask::BUTTON_RELEASE
                | xinput::XIEventMask::ENTER
                | xinput::XIEventMask::LEAVE;
            if supports_xinput_gestures {
                xi_event_mask |=
                    xinput::XIEventMask::from(1u32 << xinput::GESTURE_PINCH_BEGIN_EVENT)
                        | xinput::XIEventMask::from(1u32 << xinput::GESTURE_PINCH_UPDATE_EVENT)
                        | xinput::XIEventMask::from(1u32 << xinput::GESTURE_PINCH_END_EVENT);
            }

            check_reply(
                || "X11 XiSelectEvents failed.",
                xcb.xinput_xi_select_events(
                    x_window,
                    &[xinput::EventMask {
                        deviceid: XINPUT_ALL_DEVICE_GROUPS,
                        mask: vec![xi_event_mask],
                    }],
                ),
            )?;

            check_reply(
                || "X11 XiSelectEvents for device changes failed.",
                xcb.xinput_xi_select_events(
                    x_window,
                    &[xinput::EventMask {
                        deviceid: XINPUT_ALL_DEVICES,
                        mask: vec![
                            xinput::XIEventMask::HIERARCHY | xinput::XIEventMask::DEVICE_CHANGED,
                        ],
                    }],
                ),
            )?;

            xcb_flush(xcb);

            let renderer = {
                let raw_window = RawWindow {
                    connection: as_raw_xcb_connection::AsRawXcbConnection::as_raw_xcb_connection(
                        xcb,
                    ) as *mut _,
                    screen_id: x_screen_index,
                    window_id: x_window,
                    visual_id: visual.id,
                };
                let config = WgpuSurfaceConfig {
                    // Note: this has to be done after the GPU init, or otherwise
                    // the sizes are immediately invalidated.
                    size: query_render_extent(xcb, x_window)?,
                    // We set it to transparent by default, even if we have client-side
                    // decorations, since those seem to work on X11 even without `true` here.
                    // If the window appearance changes, then the renderer will get updated
                    // too
                    transparent: false,
                    preferred_present_mode: None,
                };
                WgpuRenderer::new(gpu_context, &raw_window, config, None, gpu_resource_budget)?
            };

            let display = Rc::new(X11Display::new(xcb, scale_factor, x_screen_index)?);

            Ok(Self {
                client,
                executor,
                display,
                x_root_window: visual_set.root,
                x_screen_index,
                visual_id: visual.id,
                bounds: bounds.to_pixels(scale_factor),
                scale_factor,
                renderer,
                atoms: *atoms,
                input_handler: None,
                active: false,
                // The window is not mapped until the client sees `MapNotify`.
                visibility: WindowVisibility::Hidden,
                hovered: false,
                force_render_after_recovery: false,
                fullscreen: false,
                is_resizable: params.is_resizable,
                maximized_vertical: false,
                maximized_horizontal: false,
                // An X11 window is not mapped until the facade honors
                // WindowOptions::show. Keep the platform visibility state in
                // sync with that initial mapping decision.
                hidden: !params.show,
                pending_focus: params.focus,
                appearance,
                handle,
                background_appearance: WindowBackgroundAppearance::Opaque,
                destroyed: false,
                client_side_decorations_supported,
                decorations: WindowDecorations::Server,
                last_insets: [0, 0, 0, 0],
                edge_constraints: None,
                #[cfg(feature = "accessibility")]
                accesskit_adapter: None,
                counter_id: sync_request_counter,
                last_sync_counter: None,
            })
        });

        if setup_result.is_err() {
            check_reply(
                || "X11 DestroyWindow failed while cleaning it up after setup failure.",
                xcb.destroy_window(x_window),
            )?;
            xcb_flush(xcb);
        }

        setup_result
    }

    fn content_size(&self) -> Size<Pixels> {
        let size = self.renderer.viewport_size();
        Size {
            width: px(size.width.0 as f32),
            height: px(size.height.0 as f32),
        }
    }
}

pub(crate) struct X11Window(pub X11WindowStatePtr);

impl Drop for X11Window {
    fn drop(&mut self) {
        let (executor, client_ptr) = {
            let mut state = self.0.state.borrow_mut();
            // Stop routing late X11 events as soon as GPUI begins tearing down this window.
            state.destroyed = true;
            state.renderer.destroy();
            (state.executor.clone(), state.client.clone())
        };

        maybe!({
            check_reply(
                || "X11 DestroyWindow failure.",
                self.0.xcb.destroy_window(self.0.x_window),
            )?;
            xcb_flush(&self.0.xcb);

            anyhow::Ok(())
        })
        .log_err();

        // The Rust window is being dropped regardless of the X server result, so keep
        // the client-side window table consistent even if destroy_window failed.
        let this_ptr = self.0.clone();
        executor
            .spawn(async move {
                this_ptr.close();
                client_ptr.drop_window(this_ptr.x_window);
            })
            .detach();
    }
}

enum WmHintPropertyState {
    // Remove = 0,
    // Add = 1,
    Toggle = 2,
}

impl X11Window {
    pub fn new(
        handle: AnyWindowHandle,
        client: X11ClientStatePtr,
        executor: ForegroundExecutor,
        gpu_context: GpuContext,
        params: WindowParams,
        gpu_resource_budget: &GpuResourceBudget,
        xcb: &Rc<XCBConnection>,
        client_side_decorations_supported: bool,
        x_main_screen_index: usize,
        x_window: xproto::Window,
        atoms: &XcbAtoms,
        scale_factor: f32,
        appearance: WindowAppearance,
        supports_xinput_gestures: bool,
        parent_window: Option<xproto::Window>,
    ) -> anyhow::Result<Self> {
        let icon = params.icon.clone();
        let ptr = X11WindowStatePtr {
            state: Rc::new(RefCell::new(X11WindowState::new(
                handle,
                client,
                executor,
                gpu_context,
                params,
                gpu_resource_budget,
                xcb,
                client_side_decorations_supported,
                x_main_screen_index,
                x_window,
                atoms,
                scale_factor,
                appearance,
                supports_xinput_gestures,
                parent_window,
            )?)),
            callbacks: Rc::new(RefCell::new(Callbacks::default())),
            xcb: xcb.clone(),
            x_window,
        };

        let state = ptr.state.borrow_mut();
        ptr.set_wm_properties(state)?;

        let window = Self(ptr);
        if let Some(icon) = icon {
            window.set_window_icon(Some(icon));
        }

        Ok(window)
    }

    fn set_wm_hints<C: Display + Send + Sync + 'static, F: FnOnce() -> C>(
        &self,
        failure_context: F,
        wm_hint_property_state: WmHintPropertyState,
        prop1: u32,
        prop2: u32,
    ) -> anyhow::Result<()> {
        let state = self.0.state.borrow();
        let message = ClientMessageEvent::new(
            32,
            self.0.x_window,
            state.atoms._NET_WM_STATE,
            [wm_hint_property_state as u32, prop1, prop2, 1, 0],
        );
        check_reply(
            failure_context,
            self.0.xcb.send_event(
                false,
                state.x_root_window,
                xproto::EventMask::SUBSTRUCTURE_REDIRECT | xproto::EventMask::SUBSTRUCTURE_NOTIFY,
                message,
            ),
        )?;
        xcb_flush(&self.0.xcb);
        Ok(())
    }

    fn get_root_position(
        &self,
        position: Point<Pixels>,
    ) -> anyhow::Result<TranslateCoordinatesReply> {
        let state = self.0.state.borrow();
        get_reply(
            || "X11 TranslateCoordinates failed.",
            self.0.xcb.translate_coordinates(
                self.0.x_window,
                state.x_root_window,
                (f32::from(position.x) * state.scale_factor) as i16,
                (f32::from(position.y) * state.scale_factor) as i16,
            ),
        )
    }

    fn send_moveresize(&self, flag: u32) -> anyhow::Result<()> {
        let state = self.0.state.borrow();

        check_reply(
            || "X11 UngrabPointer before move/resize of window failed.",
            self.0.xcb.ungrab_pointer(x11rb::CURRENT_TIME),
        )?;

        let pointer = get_reply(
            || "X11 QueryPointer before move/resize of window failed.",
            self.0.xcb.query_pointer(self.0.x_window),
        )?;
        let message = ClientMessageEvent::new(
            32,
            self.0.x_window,
            state.atoms._NET_WM_MOVERESIZE,
            [
                pointer.root_x as u32,
                pointer.root_y as u32,
                flag,
                0, // Left mouse button
                0,
            ],
        );
        check_reply(
            || "X11 SendEvent to move/resize window failed.",
            self.0.xcb.send_event(
                false,
                state.x_root_window,
                xproto::EventMask::SUBSTRUCTURE_REDIRECT | xproto::EventMask::SUBSTRUCTURE_NOTIFY,
                message,
            ),
        )?;

        xcb_flush(&self.0.xcb);
        Ok(())
    }

    fn take_pending_focus(&self) -> bool {
        let mut state = self.0.state.borrow_mut();
        let pending = state.pending_focus;
        state.pending_focus = false;
        pending
    }
}

impl X11WindowStatePtr {
    pub fn should_close(&self) -> bool {
        let mut cb = self.callbacks.borrow_mut();
        if let Some(mut should_close) = cb.should_close.take() {
            let result = (should_close)();
            cb.should_close = Some(should_close);
            result
        } else {
            true
        }
    }

    pub fn property_notify(&self, event: xproto::PropertyNotifyEvent) -> anyhow::Result<()> {
        let mut state = self.state.borrow_mut();
        if event.atom == state.atoms._NET_WM_STATE {
            self.set_wm_properties(state)?;
        } else if event.atom == state.atoms._GTK_EDGE_CONSTRAINTS {
            self.set_edge_constraints(state)?;
        }
        Ok(())
    }

    fn set_edge_constraints(
        &self,
        mut state: std::cell::RefMut<X11WindowState>,
    ) -> anyhow::Result<()> {
        let reply = get_reply(
            || "X11 GetProperty for _GTK_EDGE_CONSTRAINTS failed.",
            self.xcb.get_property(
                false,
                self.x_window,
                state.atoms._GTK_EDGE_CONSTRAINTS,
                xproto::AtomEnum::CARDINAL,
                0,
                4,
            ),
        )?;

        if reply.value_len != 0 {
            if let Ok(bytes) = reply.value[0..4].try_into() {
                let atom = u32::from_ne_bytes(bytes);
                let edge_constraints = EdgeConstraints::from_atom(atom);
                state.edge_constraints.replace(edge_constraints);
            } else {
                log::error!("Failed to parse GTK_EDGE_CONSTRAINTS");
            }
        }

        Ok(())
    }

    fn set_wm_properties(
        &self,
        mut state: std::cell::RefMut<X11WindowState>,
    ) -> anyhow::Result<()> {
        let reply = get_reply(
            || "X11 GetProperty for _NET_WM_STATE failed.",
            self.xcb.get_property(
                false,
                self.x_window,
                state.atoms._NET_WM_STATE,
                xproto::AtomEnum::ATOM,
                0,
                u32::MAX,
            ),
        )?;

        let atoms = reply
            .value
            .chunks_exact(4)
            .map(|chunk| u32::from_ne_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]));

        let was_active = state.active;
        state.active = false;
        state.fullscreen = false;
        state.maximized_vertical = false;
        state.maximized_horizontal = false;
        state.hidden = false;

        for atom in atoms {
            if atom == state.atoms._NET_WM_STATE_FOCUSED {
                state.active = true;
            } else if atom == state.atoms._NET_WM_STATE_FULLSCREEN {
                state.fullscreen = true;
            } else if atom == state.atoms._NET_WM_STATE_MAXIMIZED_VERT {
                state.maximized_vertical = true;
            } else if atom == state.atoms._NET_WM_STATE_MAXIMIZED_HORZ {
                state.maximized_horizontal = true;
            } else if atom == state.atoms._NET_WM_STATE_HIDDEN {
                state.hidden = true;
            }
        }

        // The urgency hint has no withdrawal signal of its own; ICCCM leaves that to
        // the client, and focus is the conventional means for the user to zero it.
        // _NET_WM_STATE_FOCUSED is optional; FocusIn also clears via set_active(true).
        if state.active && !was_active {
            set_wm_hints_urgency(&self.xcb, self.x_window, false);
        }

        Ok(())
    }

    pub fn close(&self) {
        {
            // A close callback removes the GPUI window synchronously; drop this borrow
            // before invoking it because the callback may re-enter window state.
            let mut state = self.state.borrow_mut();
            state.destroyed = true;
        }

        let mut callbacks = self.callbacks.borrow_mut();
        if let Some(fun) = callbacks.close.take() {
            fun()
        }
    }

    pub fn refresh(&self, request_frame_options: RequestFrameOptions) {
        let force_render_after_recovery = {
            let mut state = self.state.borrow_mut();
            std::mem::take(&mut state.force_render_after_recovery)
        };

        let mut cb = self.callbacks.borrow_mut();
        if let Some(ref mut fun) = cb.request_frame {
            fun(RequestFrameOptions {
                force_render: request_frame_options.force_render || force_render_after_recovery,
                ..request_frame_options
            });
        }
    }

    pub fn handle_input(&self, input: PlatformInput) {
        if let Some(ref mut fun) = self.callbacks.borrow_mut().input
            && !fun(input.clone()).propagate
        {
            return;
        }
        if let PlatformInput::KeyDown(event) = input {
            // only allow shift modifier when inserting text
            if event.keystroke.modifiers.is_subset_of(&Modifiers::shift()) {
                let mut state = self.state.borrow_mut();
                if let Some(mut input_handler) = state.input_handler.take() {
                    if let Some(key_char) = &event.keystroke.key_char {
                        drop(state);
                        input_handler.replace_text_in_range(None, key_char);
                        state = self.state.borrow_mut();
                    }
                    state.input_handler = Some(input_handler);
                }
            }
        }
    }

    pub fn handle_ime_commit(&self, text: String) {
        let mut state = self.state.borrow_mut();
        if let Some(mut input_handler) = state.input_handler.take() {
            drop(state);
            input_handler.replace_text_in_range(None, &text);
            let mut state = self.state.borrow_mut();
            state.input_handler = Some(input_handler);
        }
    }

    pub fn handle_ime_preedit(&self, text: String) {
        let mut state = self.state.borrow_mut();
        if let Some(mut input_handler) = state.input_handler.take() {
            drop(state);
            input_handler.replace_and_mark_text_in_range(None, &text, None);
            let mut state = self.state.borrow_mut();
            state.input_handler = Some(input_handler);
        }
    }

    pub fn handle_ime_unmark(&self) {
        let mut state = self.state.borrow_mut();
        if let Some(mut input_handler) = state.input_handler.take() {
            drop(state);
            input_handler.unmark_text();
            let mut state = self.state.borrow_mut();
            state.input_handler = Some(input_handler);
        }
    }

    pub fn handle_ime_delete(&self) {
        let mut state = self.state.borrow_mut();
        if let Some(mut input_handler) = state.input_handler.take() {
            drop(state);
            if let Some(marked) = input_handler.marked_text_range() {
                input_handler.replace_text_in_range(Some(marked), "");
            }
            let mut state = self.state.borrow_mut();
            state.input_handler = Some(input_handler);
        }
    }

    pub fn get_ime_area(&self) -> Option<Bounds<ScaledPixels>> {
        let mut state = self.state.borrow_mut();
        let scale_factor = state.scale_factor;
        let mut bounds: Option<Bounds<Pixels>> = None;
        if let Some(mut input_handler) = state.input_handler.take() {
            drop(state);
            if let Some(selection) = input_handler.selected_text_range(true) {
                bounds = input_handler.bounds_for_range(selection.range);
            }
            let mut state = self.state.borrow_mut();
            state.input_handler = Some(input_handler);
        };
        bounds.map(|b| b.scale(scale_factor))
    }

    pub fn set_bounds(&self, bounds: Bounds<i32>) -> anyhow::Result<()> {
        let mut resize_args = None;
        let is_resize;
        {
            let mut state = self.state.borrow_mut();
            let bounds = bounds.map(|f| px(f as f32 / state.scale_factor));

            is_resize = bounds.size.width != state.bounds.size.width
                || bounds.size.height != state.bounds.size.height;

            // If it's a resize event (only width/height changed), we ignore `bounds.origin`
            // because it contains wrong values.
            if is_resize {
                state.bounds.size = bounds.size;
            } else {
                state.bounds = bounds;
            }

            let gpu_size = query_render_extent(&self.xcb, self.x_window)?;
            if true {
                state.renderer.update_drawable_size(gpu_size);
                resize_args = Some((state.content_size(), state.scale_factor));
            }
            if let Some(value) = state.last_sync_counter.take() {
                check_reply(
                    || "X11 sync SetCounter failed.",
                    sync::set_counter(&self.xcb, state.counter_id, value),
                )?;
            }
        }

        let mut callbacks = self.callbacks.borrow_mut();
        if let Some((content_size, scale_factor)) = resize_args
            && let Some(ref mut fun) = callbacks.resize
        {
            fun(content_size, scale_factor)
        }

        if !is_resize && let Some(ref mut fun) = callbacks.moved {
            fun();
        }

        Ok(())
    }

    pub fn set_visibility(&self, visibility: WindowVisibility) {
        if std::mem::replace(&mut self.state.borrow_mut().visibility, visibility) == visibility {
            return;
        }
        let callback = self.callbacks.borrow_mut().visibility_change.take();
        if let Some(mut fun) = callback {
            fun(visibility);
            self.callbacks.borrow_mut().visibility_change = Some(fun);
        }
    }

    pub fn set_active(&self, focus: bool) {
        if focus {
            // FocusIn is the ICCCM-level focus signal. Do not rely only on
            // _NET_WM_STATE_FOCUSED, which some window managers never set.
            set_wm_hints_urgency(&self.xcb, self.x_window, false);
        }
        if let Some(ref mut fun) = self.callbacks.borrow_mut().active_status_change {
            fun(focus);
        }
        #[cfg(feature = "accessibility")]
        {
            if let Some(adapter) = self.state.borrow_mut().accesskit_adapter.as_mut() {
                adapter.update_window_focus_state(focus);
            }
        }
    }

    pub fn set_hovered(&self, focus: bool) {
        if let Some(ref mut fun) = self.callbacks.borrow_mut().hovered_status_change {
            fun(focus);
        }
    }

    pub fn set_appearance(&mut self, appearance: WindowAppearance) {
        let mut state = self.state.borrow_mut();
        state.appearance = appearance;
        let is_transparent = state.is_transparent();
        state.renderer.update_transparency(is_transparent);
        state.appearance = appearance;
        drop(state);
        let mut callbacks = self.callbacks.borrow_mut();
        if let Some(ref mut fun) = callbacks.appearance_changed {
            (fun)()
        }
    }
}

impl PlatformWindow for X11Window {
    fn bounds(&self) -> Bounds<Pixels> {
        self.0.state.borrow().bounds
    }

    fn is_maximized(&self) -> bool {
        let state = self.0.state.borrow();

        // A maximized window that gets minimized will still retain its maximized state.
        !state.hidden && state.maximized_vertical && state.maximized_horizontal
    }

    fn window_bounds(&self) -> WindowBounds {
        let state = self.0.state.borrow();
        if self.is_maximized() {
            WindowBounds::Maximized(state.bounds)
        } else {
            WindowBounds::Windowed(state.bounds)
        }
    }

    fn inner_window_bounds(&self) -> WindowBounds {
        let state = self.0.state.borrow();
        if self.is_maximized() {
            WindowBounds::Maximized(state.bounds)
        } else {
            let mut bounds = state.bounds;
            let [left, right, top, bottom] = state.last_insets;

            let [left, right, top, bottom] = [
                px((left as f32) / state.scale_factor),
                px((right as f32) / state.scale_factor),
                px((top as f32) / state.scale_factor),
                px((bottom as f32) / state.scale_factor),
            ];

            bounds.origin.x += left;
            bounds.origin.y += top;
            bounds.size.width -= left + right;
            bounds.size.height -= top + bottom;

            WindowBounds::Windowed(bounds)
        }
    }

    fn content_size(&self) -> Size<Pixels> {
        // We divide by the scale factor here because this value is queried to determine how much to draw,
        // but it will be multiplied later by the scale to adjust for scaling.
        let state = self.0.state.borrow();
        state
            .content_size()
            .map(|size| size.div(state.scale_factor))
    }

    fn resize(&mut self, size: Size<Pixels>) {
        let state = self.0.state.borrow();
        let size = size.to_device_pixels(state.scale_factor);
        let width = size.width.0 as u32;
        let height = size.height.0 as u32;

        check_reply(
            || {
                format!(
                    "X11 ConfigureWindow failed. width: {}, height: {}",
                    width, height
                )
            },
            self.0.xcb.configure_window(
                self.0.x_window,
                &xproto::ConfigureWindowAux::new()
                    .width(width)
                    .height(height),
            ),
        )
        .log_err();
        xcb_flush(&self.0.xcb);
    }

    fn scale_factor(&self) -> f32 {
        self.0.state.borrow().scale_factor
    }

    fn appearance(&self) -> WindowAppearance {
        self.0.state.borrow().appearance
    }

    fn display(&self) -> Option<Rc<dyn PlatformDisplay>> {
        Some(self.0.state.borrow().display.clone())
    }

    fn mouse_position(&self) -> Point<Pixels> {
        get_reply(
            || "X11 QueryPointer failed.",
            self.0.xcb.query_pointer(self.0.x_window),
        )
        .log_err()
        .map_or(Point::new(Pixels::ZERO, Pixels::ZERO), |reply| {
            let scale_factor = self.0.state.borrow().scale_factor;
            Point::new(
                px(reply.win_x as f32 / scale_factor),
                px(reply.win_y as f32 / scale_factor),
            )
        })
    }

    fn modifiers(&self) -> Modifiers {
        self.0
            .state
            .borrow()
            .client
            .0
            .upgrade()
            .map(|ref_cell| ref_cell.borrow().modifiers)
            .unwrap_or_default()
    }

    fn capslock(&self) -> gpui::Capslock {
        self.0
            .state
            .borrow()
            .client
            .0
            .upgrade()
            .map(|ref_cell| ref_cell.borrow().capslock)
            .unwrap_or_default()
    }

    fn set_input_handler(&mut self, input_handler: PlatformInputHandler) {
        self.0.state.borrow_mut().input_handler = Some(input_handler);
    }

    fn take_input_handler(&mut self) -> Option<PlatformInputHandler> {
        self.0.state.borrow_mut().input_handler.take()
    }

    fn prompt(
        &self,
        _level: PromptLevel,
        _msg: &str,
        _detail: Option<&str>,
        _answers: &[PromptButton],
    ) -> Option<futures::channel::oneshot::Receiver<usize>> {
        None
    }

    fn activate(&self) {
        let data = [1, xproto::Time::CURRENT_TIME.into(), 0, 0, 0];
        let message = xproto::ClientMessageEvent::new(
            32,
            self.0.x_window,
            self.0.state.borrow().atoms._NET_ACTIVE_WINDOW,
            data,
        );
        self.0
            .xcb
            .send_event(
                false,
                self.0.state.borrow().x_root_window,
                xproto::EventMask::SUBSTRUCTURE_REDIRECT | xproto::EventMask::SUBSTRUCTURE_NOTIFY,
                message,
            )
            .log_err();
        xcb_flush(&self.0.xcb);
    }

    fn request_attention(&self) {
        if self.is_active() {
            return;
        }

        set_wm_hints_urgency(&self.0.xcb, self.0.x_window, true);
    }

    fn is_active(&self) -> bool {
        self.0.state.borrow().active
    }

    fn visibility(&self) -> WindowVisibility {
        self.0.state.borrow().visibility
    }

    fn is_hovered(&self) -> bool {
        self.0.state.borrow().hovered
    }

    fn set_title(&mut self, title: &str) {
        check_reply(
            || "X11 ChangeProperty8 on WM_NAME failed.",
            self.0.xcb.change_property8(
                xproto::PropMode::REPLACE,
                self.0.x_window,
                xproto::AtomEnum::WM_NAME,
                xproto::AtomEnum::STRING,
                title.as_bytes(),
            ),
        )
        .log_err();

        check_reply(
            || "X11 ChangeProperty8 on _NET_WM_NAME failed.",
            self.0.xcb.change_property8(
                xproto::PropMode::REPLACE,
                self.0.x_window,
                self.0.state.borrow().atoms._NET_WM_NAME,
                self.0.state.borrow().atoms.UTF8_STRING,
                title.as_bytes(),
            ),
        )
        .log_err();
        xcb_flush(&self.0.xcb);
    }

    fn set_app_id(&mut self, app_id: &str) {
        let mut data = Vec::with_capacity(app_id.len() * 2 + 2);
        data.extend(app_id.bytes()); // instance https://unix.stackexchange.com/a/494170
        data.push(b'\0');
        data.extend(app_id.bytes()); // class
        data.push(b'\0');

        check_reply(
            || "X11 ChangeProperty8 for WM_CLASS failed.",
            self.0.xcb.change_property8(
                xproto::PropMode::REPLACE,
                self.0.x_window,
                xproto::AtomEnum::WM_CLASS,
                xproto::AtomEnum::STRING,
                &data,
            ),
        )
        .log_err();
    }

    fn map_window(&mut self) -> anyhow::Result<()> {
        check_reply(
            || "X11 MapWindow failed.",
            self.0.xcb.map_window(self.0.x_window),
        )?;
        // `WindowParams::focus`: request activation right after mapping, so
        // the WM processes both with the same flush, mirroring the macOS
        // `focus && show` → `makeKeyAndOrderFront` behavior.
        if self.take_pending_focus() {
            self.activate();
        }
        Ok(())
    }

    fn trim_renderer_caches(&self) {
        self.0.state.borrow_mut().renderer.trim();
    }

    fn set_background_appearance(&self, background_appearance: WindowBackgroundAppearance) {
        let mut state = self.0.state.borrow_mut();
        state.background_appearance = background_appearance;
        let transparent = state.is_transparent();
        state.renderer.update_transparency(transparent);
    }

    fn minimize(&self) {
        let state = self.0.state.borrow();
        const WINDOW_ICONIC_STATE: u32 = 3;
        let message = ClientMessageEvent::new(
            32,
            self.0.x_window,
            state.atoms.WM_CHANGE_STATE,
            [WINDOW_ICONIC_STATE, 0, 0, 0, 0],
        );
        check_reply(
            || "X11 SendEvent to minimize window failed.",
            self.0.xcb.send_event(
                false,
                state.x_root_window,
                xproto::EventMask::SUBSTRUCTURE_REDIRECT | xproto::EventMask::SUBSTRUCTURE_NOTIFY,
                message,
            ),
        )
        .log_err();
    }

    fn zoom(&self) {
        let state = self.0.state.borrow();
        self.set_wm_hints(
            || "X11 SendEvent to maximize a window failed.",
            WmHintPropertyState::Toggle,
            state.atoms._NET_WM_STATE_MAXIMIZED_VERT,
            state.atoms._NET_WM_STATE_MAXIMIZED_HORZ,
        )
        .log_err();
    }

    fn toggle_fullscreen(&self) {
        let state = self.0.state.borrow();
        self.set_wm_hints(
            || "X11 SendEvent to fullscreen a window failed.",
            WmHintPropertyState::Toggle,
            state.atoms._NET_WM_STATE_FULLSCREEN,
            xproto::AtomEnum::NONE.into(),
        )
        .log_err();
    }

    fn is_fullscreen(&self) -> bool {
        self.0.state.borrow().fullscreen
    }

    fn on_request_frame(&self, callback: Box<dyn FnMut(RequestFrameOptions)>) {
        self.0.callbacks.borrow_mut().request_frame = Some(callback);
    }

    fn on_input(&self, callback: Box<dyn FnMut(PlatformInput) -> gpui::DispatchEventResult>) {
        self.0.callbacks.borrow_mut().input = Some(callback);
    }

    fn on_active_status_change(&self, callback: Box<dyn FnMut(bool)>) {
        self.0.callbacks.borrow_mut().active_status_change = Some(callback);
    }

    fn on_visibility_change(&self, callback: Box<dyn FnMut(WindowVisibility)>) {
        self.0.callbacks.borrow_mut().visibility_change = Some(callback);
    }

    fn on_hover_status_change(&self, callback: Box<dyn FnMut(bool)>) {
        self.0.callbacks.borrow_mut().hovered_status_change = Some(callback);
    }

    fn on_resize(&self, callback: Box<dyn FnMut(Size<Pixels>, f32)>) {
        self.0.callbacks.borrow_mut().resize = Some(callback);
    }

    fn on_moved(&self, callback: Box<dyn FnMut()>) {
        self.0.callbacks.borrow_mut().moved = Some(callback);
    }

    fn on_should_close(&self, callback: Box<dyn FnMut() -> bool>) {
        self.0.callbacks.borrow_mut().should_close = Some(callback);
    }

    fn on_close(&self, callback: Box<dyn FnOnce()>) {
        self.0.callbacks.borrow_mut().close = Some(callback);
    }

    fn on_hit_test_window_control(&self, _callback: Box<dyn FnMut() -> Option<WindowControlArea>>) {
    }

    fn on_appearance_changed(&self, callback: Box<dyn FnMut()>) {
        self.0.callbacks.borrow_mut().appearance_changed = Some(callback);
    }

    fn draw(&self, scene: &Scene) {
        let mut inner = self.0.state.borrow_mut();

        if inner.renderer.device_lost() {
            let raw_window = RawWindow {
                connection: as_raw_xcb_connection::AsRawXcbConnection::as_raw_xcb_connection(
                    &*self.0.xcb,
                ) as *mut _,
                screen_id: inner.x_screen_index,
                window_id: self.0.x_window,
                visual_id: inner.visual_id,
            };
            if let Err(err) = inner.renderer.recover(&raw_window) {
                log::warn!("GPU recovery failed, will retry on next frame: {err}");
            }

            inner.force_render_after_recovery = true;
            return;
        }

        inner.renderer.draw(scene);

        if inner.renderer.needs_redraw() {
            inner.force_render_after_recovery = true;
        }
    }

    #[cfg(any(test, feature = "test-support"))]
    fn render_to_image(&self, scene: &Scene) -> anyhow::Result<image::RgbaImage> {
        self.0
            .state
            .borrow_mut()
            .renderer
            .render_scene_to_image(scene)
    }

    fn sprite_atlas(&self) -> Arc<dyn PlatformAtlas> {
        let inner = self.0.state.borrow();
        inner.renderer.sprite_atlas().clone()
    }

    fn show_window_menu(&self, position: Point<Pixels>) {
        let state = self.0.state.borrow();

        check_reply(
            || "X11 UngrabPointer failed.",
            self.0.xcb.ungrab_pointer(x11rb::CURRENT_TIME),
        )
        .log_err();

        let Some(coords) = self.get_root_position(position).log_err() else {
            return;
        };
        let message = ClientMessageEvent::new(
            32,
            self.0.x_window,
            state.atoms._GTK_SHOW_WINDOW_MENU,
            [
                XINPUT_ALL_DEVICE_GROUPS as u32,
                coords.dst_x as u32,
                coords.dst_y as u32,
                0,
                0,
            ],
        );
        check_reply(
            || "X11 SendEvent to show window menu failed.",
            self.0.xcb.send_event(
                false,
                state.x_root_window,
                xproto::EventMask::SUBSTRUCTURE_REDIRECT | xproto::EventMask::SUBSTRUCTURE_NOTIFY,
                message,
            ),
        )
        .log_err();
    }

    fn start_window_move(&self) {
        const MOVERESIZE_MOVE: u32 = 8;
        self.send_moveresize(MOVERESIZE_MOVE).log_err();
    }

    fn start_window_resize(&self, edge: ResizeEdge) {
        if !self.0.state.borrow().is_resizable {
            return;
        }

        self.send_moveresize(resize_edge_to_moveresize(edge))
            .log_err();
    }

    fn window_decorations(&self) -> gpui::Decorations {
        let state = self.0.state.borrow();

        // Client window decorations require compositor support
        if !state.client_side_decorations_supported {
            return Decorations::Server;
        }

        match state.decorations {
            WindowDecorations::Server => Decorations::Server,
            WindowDecorations::Client => {
                let tiling = if state.fullscreen {
                    Tiling::tiled()
                } else if let Some(edge_constraints) = &state.edge_constraints {
                    edge_constraints.to_tiling()
                } else {
                    // https://source.chromium.org/chromium/chromium/src/+/main:ui/ozone/platform/x11/x11_window.cc;l=2519;drc=1f14cc876cc5bf899d13284a12c451498219bb2d
                    Tiling {
                        top: state.maximized_vertical,
                        bottom: state.maximized_vertical,
                        left: state.maximized_horizontal,
                        right: state.maximized_horizontal,
                    }
                };
                Decorations::Client { tiling }
            }
        }
    }

    fn set_client_inset(&self, inset: Pixels) {
        let mut state = self.0.state.borrow_mut();

        let dp = (f32::from(inset) * state.scale_factor) as u32;

        let insets = if state.fullscreen {
            [0, 0, 0, 0]
        } else if let Some(edge_constraints) = &state.edge_constraints {
            let left = if edge_constraints.left_tiled { 0 } else { dp };
            let top = if edge_constraints.top_tiled { 0 } else { dp };
            let right = if edge_constraints.right_tiled { 0 } else { dp };
            let bottom = if edge_constraints.bottom_tiled { 0 } else { dp };

            [left, right, top, bottom]
        } else {
            let (left, right) = if state.maximized_horizontal {
                (0, 0)
            } else {
                (dp, dp)
            };
            let (top, bottom) = if state.maximized_vertical {
                (0, 0)
            } else {
                (dp, dp)
            };
            [left, right, top, bottom]
        };

        if state.last_insets != insets {
            state.last_insets = insets;

            check_reply(
                || "X11 ChangeProperty for _GTK_FRAME_EXTENTS failed.",
                self.0.xcb.change_property(
                    xproto::PropMode::REPLACE,
                    self.0.x_window,
                    state.atoms._GTK_FRAME_EXTENTS,
                    xproto::AtomEnum::CARDINAL,
                    size_of::<u32>() as u8 * 8,
                    4,
                    bytemuck::cast_slice::<u32, u8>(&insets),
                ),
            )
            .log_err();
        }
    }

    fn request_decorations(&self, mut decorations: gpui::WindowDecorations) {
        let mut state = self.0.state.borrow_mut();

        if matches!(decorations, gpui::WindowDecorations::Client)
            && !state.client_side_decorations_supported
        {
            log::info!(
                "x11: no compositor present, falling back to server-side window decorations"
            );
            decorations = gpui::WindowDecorations::Server;
        }

        let hints_data = motif_hints_data(decorations, state.is_resizable);

        let success = check_reply(
            || "X11 ChangeProperty for _MOTIF_WM_HINTS failed.",
            self.0.xcb.change_property(
                xproto::PropMode::REPLACE,
                self.0.x_window,
                state.atoms._MOTIF_WM_HINTS,
                state.atoms._MOTIF_WM_HINTS,
                size_of::<u32>() as u8 * 8,
                5,
                bytemuck::cast_slice::<u32, u8>(&hints_data),
            ),
        )
        .log_err();

        let Some(()) = success else {
            return;
        };

        match decorations {
            WindowDecorations::Server => {
                state.decorations = WindowDecorations::Server;
                let is_transparent = state.is_transparent();
                state.renderer.update_transparency(is_transparent);
            }
            WindowDecorations::Client => {
                state.decorations = WindowDecorations::Client;
                let is_transparent = state.is_transparent();
                state.renderer.update_transparency(is_transparent);
            }
        }

        drop(state);
        let mut callbacks = self.0.callbacks.borrow_mut();
        if let Some(appearance_changed) = callbacks.appearance_changed.as_mut() {
            appearance_changed();
        }
    }

    fn update_ime_position(&self, bounds: Bounds<Pixels>) {
        let mut state = self.0.state.borrow_mut();
        let client = state.client.clone();
        drop(state);
        client.update_ime_position(bounds);
    }

    fn gpu_specs(&self) -> Option<GpuSpecs> {
        self.0.state.borrow().renderer.gpu_specs().into()
    }

    fn play_system_bell(&self) {
        self.0.xcb.bell(0).log_err();
        xcb_flush(&self.0.xcb);
    }

    fn show(&self) {
        self.0.xcb.map_window(self.0.x_window).log_err();
        xcb_flush(&self.0.xcb);
        self.0.state.borrow_mut().hidden = false;
        if self.take_pending_focus() {
            self.activate();
        }
    }

    fn hide(&self) {
        self.0.xcb.unmap_window(self.0.x_window).log_err();
        xcb_flush(&self.0.xcb);
        self.0.state.borrow_mut().hidden = true;
    }

    fn is_visible(&self) -> bool {
        !self.0.state.borrow().hidden
    }

    fn set_mouse_passthrough(&self, passthrough: bool) {
        use x11rb::protocol::shape;
        if passthrough {
            shape::rectangles(
                self.0.xcb.as_ref(),
                shape::SO::SET,
                shape::SK::INPUT,
                xproto::ClipOrdering::UNSORTED,
                self.0.x_window,
                0,
                0,
                &[],
            )
            .log_err();
        } else {
            shape::mask(
                self.0.xcb.as_ref(),
                shape::SO::SET,
                shape::SK::INPUT,
                self.0.x_window,
                0,
                0,
                x11rb::NONE,
            )
            .log_err();
        }
        xcb_flush(&self.0.xcb);
    }

    fn set_window_icon(&self, icon: Option<image::RgbaImage>) {
        let Some(image) = icon else { return };

        let width = image.width();
        let height = image.height();
        let property_size = 2 + (width * height) as usize;
        let mut property_data: Vec<u32> = Vec::with_capacity(property_size);

        property_data.push(width);
        property_data.push(height);

        // _NET_WM_ICON expects each pixel as a 32-bit ARGB CARDINAL,
        // i.e. the integer value (A << 24) | (R << 16) | (G << 8) | B,
        // independent of host endianness.
        for pixel in image.pixels() {
            let [r, g, b, a] = pixel.0;
            property_data
                .push(((a as u32) << 24) | ((r as u32) << 16) | ((g as u32) << 8) | (b as u32));
        }

        check_reply(
            || "X11 set window icon failed.",
            self.0.xcb.change_property32(
                xproto::PropMode::REPLACE,
                self.0.x_window,
                self.0.state.borrow().atoms._NET_WM_ICON,
                xproto::AtomEnum::CARDINAL,
                &property_data,
            ),
        )
        .log_err();
        xcb_flush(&self.0.xcb);
    }

    #[cfg(feature = "accessibility")]
    fn a11y_init(&self, callbacks: gpui::A11yCallbacks) {
        let activation_handler = A11yActivationHandler {
            callback: callbacks.activation,
        };
        let action_handler = A11yActionHandler(callbacks.action);
        let deactivation_handler = A11yDeactivationHandler {
            callback: callbacks.deactivation,
        };

        let adapter =
            accesskit_unix::Adapter::new(activation_handler, action_handler, deactivation_handler);

        self.0.state.borrow_mut().accesskit_adapter = Some(adapter);
    }

    #[cfg(feature = "accessibility")]
    fn a11y_tree_update(&self, tree_update: accesskit::TreeUpdate) {
        let mut state = self.0.state.borrow_mut();
        if let Some(adapter) = state.accesskit_adapter.as_mut() {
            adapter.update_if_active(|| tree_update);
        }
    }

    #[cfg(feature = "accessibility")]
    fn a11y_update_window_bounds(&self) {
        let mut state = self.0.state.borrow_mut();
        let scale = state.scale_factor;
        let bounds = state.bounds;
        let [left, right, top, bottom] = state.last_insets;

        let x = f32::from(bounds.origin.x);
        let y = f32::from(bounds.origin.y);
        let width = f32::from(bounds.size.width);
        let height = f32::from(bounds.size.height);

        let outer = accesskit::Rect {
            x0: (x * scale) as f64,
            y0: (y * scale) as f64,
            x1: ((x + width) * scale) as f64,
            y1: ((y + height) * scale) as f64,
        };

        let inner = accesskit::Rect {
            x0: (x * scale) as f64 + left as f64,
            y0: (y * scale) as f64 + top as f64,
            x1: ((x + width) * scale) as f64 - right as f64,
            y1: ((y + height) * scale) as f64 - bottom as f64,
        };

        if let Some(adapter) = state.accesskit_adapter.as_mut() {
            adapter.set_root_window_bounds(outer, inner);
        }
    }
}

#[cfg(feature = "accessibility")]
struct A11yActivationHandler {
    callback: Box<dyn Fn() -> Option<accesskit::TreeUpdate> + Send + 'static>,
}

#[cfg(feature = "accessibility")]
impl accesskit::ActivationHandler for A11yActivationHandler {
    fn request_initial_tree(&mut self) -> Option<accesskit::TreeUpdate> {
        (self.callback)()
    }
}

#[cfg(feature = "accessibility")]
struct A11yActionHandler(Box<dyn Fn(accesskit::ActionRequest) + Send + 'static>);

#[cfg(feature = "accessibility")]
impl accesskit::ActionHandler for A11yActionHandler {
    fn do_action(&mut self, request: accesskit::ActionRequest) {
        (self.0)(request);
    }
}

#[cfg(feature = "accessibility")]
struct A11yDeactivationHandler {
    callback: Box<dyn Fn() + Send + 'static>,
}

#[cfg(feature = "accessibility")]
impl accesskit::DeactivationHandler for A11yDeactivationHandler {
    fn deactivate_accessibility(&mut self) {
        (self.callback)();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::point;

    fn window_params(is_resizable: bool, window_min_size: Option<Size<Pixels>>) -> WindowParams {
        WindowParams {
            bounds: Bounds::new(point(px(10.0), px(20.0)), size(px(320.0), px(240.0))),
            titlebar: None,
            kind: WindowKind::Normal,
            is_movable: true,
            app_owns_titlebar_drag: false,
            is_resizable,
            is_minimizable: true,
            focus: true,
            app_id: None,
            show: true,
            display_id: None,
            window_min_size,
            #[cfg(target_os = "macos")]
            tabbing_identifier: None,
            mouse_passthrough: false,
            icon: None,
        }
    }

    #[test]
    fn window_kind_maps_to_ewmh_window_type() {
        assert_eq!(
            window_type_for_kind(&WindowKind::Normal),
            X11WindowType::Normal
        );
        assert_eq!(
            window_type_for_kind(&WindowKind::PopUp),
            X11WindowType::Notification
        );
        assert_eq!(
            window_type_for_kind(&WindowKind::Floating),
            X11WindowType::Dialog
        );
        assert_eq!(
            window_type_for_kind(&WindowKind::Overlay),
            X11WindowType::Dock
        );
    }

    #[cfg(feature = "wayland")]
    #[test]
    fn layer_shell_window_kind_is_rejected() {
        let error = ensure_window_kind_supported(&WindowKind::LayerShell(
            gpui::layer_shell::LayerShellOptions::default(),
        ))
        .expect_err("X11 must reject Wayland layer-shell windows");

        assert!(
            error
                .downcast_ref::<gpui::layer_shell::LayerShellNotSupportedError>()
                .is_some()
        );
    }

    #[test]
    fn resizable_window_without_min_size_has_no_size_hints() {
        assert!(normal_size_hints(&window_params(true, None)).is_none());
    }

    #[test]
    fn min_size_becomes_normal_size_hint() {
        let hints = normal_size_hints(&window_params(true, Some(size(px(120.0), px(80.0)))))
            .expect("min size should produce size hints");

        assert_eq!(hints.min_size, Some((120, 80)));
        assert_eq!(hints.max_size, None);
    }

    #[test]
    fn non_resizable_window_uses_fixed_size_hints() {
        let hints = normal_size_hints(&window_params(false, None))
            .expect("fixed size should produce size hints");

        assert_eq!(hints.min_size, Some((320, 240)));
        assert_eq!(hints.max_size, Some((320, 240)));
        let Some((WmSizeHintsSpecification::ProgramSpecified, width, height)) = hints.size else {
            panic!("fixed windows should use program-specified size hints");
        };
        assert_eq!((width, height), (320, 240));
    }

    #[test]
    fn motif_hints_preserve_close_move_minimize_for_fixed_size_windows() {
        const MWM_HINTS_FUNCTIONS: u32 = 1 << 0;
        const MWM_HINTS_DECORATIONS: u32 = 1 << 1;
        const MWM_FUNC_MOVE: u32 = 1 << 2;
        const MWM_FUNC_MINIMIZE: u32 = 1 << 3;
        const MWM_FUNC_CLOSE: u32 = 1 << 5;

        assert_eq!(
            motif_hints_data(WindowDecorations::Client, false),
            [
                MWM_HINTS_FUNCTIONS | MWM_HINTS_DECORATIONS,
                MWM_FUNC_MOVE | MWM_FUNC_MINIMIZE | MWM_FUNC_CLOSE,
                0,
                0,
                0,
            ]
        );
    }

    #[test]
    fn motif_hints_allow_all_functions_for_resizable_windows() {
        const MWM_HINTS_FUNCTIONS: u32 = 1 << 0;
        const MWM_HINTS_DECORATIONS: u32 = 1 << 1;
        const MWM_FUNC_ALL: u32 = 1 << 0;

        assert_eq!(
            motif_hints_data(WindowDecorations::Server, true),
            [
                MWM_HINTS_FUNCTIONS | MWM_HINTS_DECORATIONS,
                MWM_FUNC_ALL,
                1,
                0,
                0,
            ]
        );
    }
}

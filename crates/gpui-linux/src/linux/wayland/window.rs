use std::{
    cell::{Cell, Ref, RefCell, RefMut},
    ffi::c_void,
    ptr::NonNull,
    rc::Rc,
    sync::Arc,
};

use calloop::ping::Ping;
use collections::HashMap;
use futures::channel::oneshot::Receiver;

use raw_window_handle as rwh;
use wayland_backend::client::ObjectId;
use wayland_client::WEnum;
use wayland_client::{
    Proxy,
    protocol::{wl_callback, wl_output, wl_surface},
};
use wayland_protocols::wp::viewporter::client::wp_viewport;
use wayland_protocols::xdg::decoration::zv1::client::zxdg_toplevel_decoration_v1;
use wayland_protocols::xdg::shell::client::xdg_surface;
use wayland_protocols::xdg::shell::client::xdg_toplevel::{self};
use wayland_protocols::{
    wp::fractional_scale::v1::client::wp_fractional_scale_v1,
    xdg::shell::client::xdg_toplevel::XdgToplevel,
};
use wayland_protocols_plasma::blur::client::org_kde_kwin_blur;
use wayland_protocols_wlr::layer_shell::v1::client::{zwlr_layer_shell_v1, zwlr_layer_surface_v1};

use crate::linux::wayland::{display::WaylandDisplay, serial::SerialKind};
use crate::linux::{Globals, Output, WaylandClientStatePtr};
use gpui::layer_shell::{
    Anchor, KeyboardInteractivity, Layer, LayerShellNotSupportedError, LayerShellOptions,
};
use gpui::{
    AnyWindowHandle, Bounds, Capslock, Decorations, DisplayId, GpuResourceBudget, GpuSpecs,
    Modifiers, Pixels, PlatformAtlas, PlatformDisplay, PlatformInput, PlatformInputHandler,
    PlatformWindow, Point, PromptButton, PromptLevel, RequestFrameOptions, ResizeEdge, Scene, Size,
    Tiling, WindowAppearance, WindowBackgroundAppearance, WindowBounds, WindowControlArea,
    WindowControls, WindowDecorations, WindowKind, WindowParams, px, size,
};
use gpui_wgpu::{GpuContext, WgpuRenderer, WgpuSurfaceConfig, wgpu};

#[derive(Default)]
pub(crate) struct Callbacks {
    request_frame: Option<Box<dyn FnMut(RequestFrameOptions)>>,
    input: Option<Box<dyn FnMut(gpui::PlatformInput) -> gpui::DispatchEventResult>>,
    active_status_change: Option<Box<dyn FnMut(bool)>>,
    hover_status_change: Option<Box<dyn FnMut(bool)>>,
    resize: Option<Box<dyn FnMut(Size<Pixels>, f32)>>,
    moved: Option<Box<dyn FnMut()>>,
    should_close: Option<Box<dyn FnMut() -> bool>>,
    close: Option<Box<dyn FnOnce()>>,
    appearance_changed: Option<Box<dyn FnMut()>>,
}

#[derive(Clone, Debug)]
struct RawWindow {
    window: *mut c_void,
    display: *mut c_void,
}

unsafe impl Send for RawWindow {}
unsafe impl Sync for RawWindow {}

impl rwh::HasWindowHandle for RawWindow {
    fn window_handle(&self) -> Result<rwh::WindowHandle<'_>, rwh::HandleError> {
        let window = NonNull::new(self.window).unwrap();
        let handle = rwh::WaylandWindowHandle::new(window);
        Ok(unsafe { rwh::WindowHandle::borrow_raw(handle.into()) })
    }
}
impl rwh::HasDisplayHandle for RawWindow {
    fn display_handle(&self) -> Result<rwh::DisplayHandle<'_>, rwh::HandleError> {
        let display = NonNull::new(self.display).unwrap();
        let handle = rwh::WaylandDisplayHandle::new(display);
        Ok(unsafe { rwh::DisplayHandle::borrow_raw(handle.into()) })
    }
}

#[derive(Debug)]
struct InProgressConfigure {
    size: Option<Size<Pixels>>,
    fullscreen: bool,
    maximized: bool,
    resizing: bool,
    tiling: Tiling,
}

pub struct WaylandWindowState {
    role: WaylandWindowRole,
    pub surface: wl_surface::WlSurface,
    app_id: Option<String>,
    appearance: WindowAppearance,
    blur: Option<org_kde_kwin_blur::OrgKdeKwinBlur>,
    viewport: Option<wp_viewport::WpViewport>,
    outputs: HashMap<ObjectId, Output>,
    display: Option<(ObjectId, Output)>,
    globals: Globals,
    renderer: WgpuRenderer,
    bounds: Bounds<Pixels>,
    scale: f32,
    input_handler: Option<PlatformInputHandler>,
    decorations: WindowDecorations,
    background_appearance: WindowBackgroundAppearance,
    fullscreen: bool,
    maximized: bool,
    tiling: Tiling,
    window_bounds: Bounds<Pixels>,
    client: WaylandClientStatePtr,
    handle: AnyWindowHandle,
    active: bool,
    hovered: bool,
    redraw_requested: bool,
    presentation: PresentationState,
    pending_frame_callback: Option<wl_callback::WlCallback>,
    in_progress_configure: Option<InProgressConfigure>,
    resize_throttle: bool,
    in_progress_window_controls: Option<WindowControls>,
    window_controls: WindowControls,
    client_inset: Option<Pixels>,
    visible: bool,
    #[cfg(feature = "accessibility")]
    accesskit_adapter: Option<accesskit_unix::Adapter>,
}

pub enum WaylandWindowRole {
    XdgToplevel {
        xdg_surface: xdg_surface::XdgSurface,
        toplevel: xdg_toplevel::XdgToplevel,
        decoration: Option<zxdg_toplevel_decoration_v1::ZxdgToplevelDecorationV1>,
    },
    LayerShell {
        layer_surface: zwlr_layer_surface_v1::ZwlrLayerSurfaceV1,
        options: LayerShellOptions,
        configured: bool,
    },
}

impl WaylandWindowRole {
    fn toplevel(&self) -> Option<xdg_toplevel::XdgToplevel> {
        match self {
            WaylandWindowRole::XdgToplevel { toplevel, .. } => Some(toplevel.clone()),
            WaylandWindowRole::LayerShell { .. } => None,
        }
    }

    fn is_layer_shell(&self) -> bool {
        matches!(self, WaylandWindowRole::LayerShell { .. })
    }

    fn set_exclusive_zone(&self, zone: i32) -> bool {
        let WaylandWindowRole::LayerShell { layer_surface, .. } = self else {
            return false;
        };
        layer_surface.set_exclusive_zone(zone);
        true
    }

    fn set_exclusive_edge(&self, edge: Anchor) -> bool {
        let WaylandWindowRole::LayerShell {
            layer_surface,
            options,
            ..
        } = self
        else {
            return false;
        };
        apply_exclusive_edge(layer_surface, options.anchor, edge)
    }
}

#[derive(Clone)]
pub struct WaylandWindowStatePtr {
    state: Rc<RefCell<WaylandWindowState>>,
    callbacks: Rc<RefCell<Callbacks>>,
    frame_loop: Rc<Cell<FrameLoop>>,
    frame_ping: Ping,
}

impl WaylandWindowState {
    pub(crate) fn new(
        handle: AnyWindowHandle,
        surface: wl_surface::WlSurface,
        role: WaylandWindowRole,
        appearance: WindowAppearance,
        viewport: Option<wp_viewport::WpViewport>,
        client: WaylandClientStatePtr,
        globals: Globals,
        gpu_context: GpuContext,
        options: WindowParams,
        gpu_resource_budget: &GpuResourceBudget,
    ) -> anyhow::Result<Self> {
        let renderer = {
            let raw_window = RawWindow {
                window: surface.id().as_ptr().cast::<c_void>(),
                display: surface
                    .backend()
                    .upgrade()
                    .unwrap()
                    .display_ptr()
                    .cast::<c_void>(),
            };
            let config = WgpuSurfaceConfig {
                size: options.bounds.to_device_pixels(1.0).size,
                transparent: true,
                // Prefer Mailbox on Wayland to avoid blocking the event loop on FIFO stalls.
                preferred_present_mode: Some(wgpu::PresentMode::Mailbox),
            };
            WgpuRenderer::new(gpu_context, &raw_window, config, None, gpu_resource_budget)?
        };

        Ok(Self {
            role,
            surface,
            app_id: options.app_id,
            blur: None,
            viewport,
            globals,
            outputs: HashMap::default(),
            display: None,
            renderer,
            bounds: options.bounds,
            scale: 1.0,
            input_handler: None,
            decorations: WindowDecorations::Client,
            background_appearance: WindowBackgroundAppearance::Opaque,
            fullscreen: false,
            maximized: false,
            tiling: Tiling::default(),
            window_bounds: options.bounds,
            in_progress_configure: None,
            resize_throttle: false,
            client,
            appearance,
            handle,
            active: false,
            hovered: false,
            redraw_requested: false,
            presentation: PresentationState::Unpresented,
            pending_frame_callback: None,
            in_progress_window_controls: None,
            window_controls: WindowControls::default(),
            client_inset: None,
            visible: true,
            #[cfg(feature = "accessibility")]
            accesskit_adapter: None,
        })
    }

    pub fn is_transparent(&self) -> bool {
        self.decorations == WindowDecorations::Client
            || self.background_appearance != WindowBackgroundAppearance::Opaque
    }

    pub fn primary_output_scale(&mut self) -> i32 {
        let mut scale = 1;
        let mut current_output = self.display.take();
        for (id, output) in self.outputs.iter() {
            if let Some((_, output_data)) = &current_output {
                if output.scale > output_data.scale {
                    current_output = Some((id.clone(), output.clone()));
                }
            } else {
                current_output = Some((id.clone(), output.clone()));
            }
            scale = scale.max(output.scale);
        }
        self.display = current_output;
        scale
    }

    pub fn inset(&self) -> Pixels {
        match self.decorations {
            WindowDecorations::Server => px(0.0),
            WindowDecorations::Client => self.client_inset.unwrap_or(px(0.0)),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PresentationState {
    Unpresented,
    Presented,
    RetryBeforeFirstPresent,
    RetryAfterPresent,
}

impl PresentationState {
    fn requires_presentation(self) -> bool {
        matches!(
            self,
            Self::RetryBeforeFirstPresent | Self::RetryAfterPresent
        )
    }

    fn failed(self) -> Self {
        match self {
            Self::Unpresented | Self::RetryBeforeFirstPresent => Self::RetryBeforeFirstPresent,
            Self::Presented | Self::RetryAfterPresent => Self::RetryAfterPresent,
        }
    }

    /// After the first successful present, pace retries with compositor frame
    /// callbacks so an occluded surface does not keep polling. Before the first
    /// present a callback may never arrive, so those retries use the timer.
    fn compositor_paced_retry(self) -> bool {
        self == Self::RetryAfterPresent
    }
}

#[cfg(test)]
mod presentation_state_tests {
    use super::PresentationState;

    #[test]
    fn failure_tracks_whether_the_surface_has_presented() {
        assert_eq!(
            PresentationState::Unpresented.failed(),
            PresentationState::RetryBeforeFirstPresent
        );
        assert_eq!(
            PresentationState::RetryBeforeFirstPresent.failed(),
            PresentationState::RetryBeforeFirstPresent
        );
        assert_eq!(
            PresentationState::Presented.failed(),
            PresentationState::RetryAfterPresent
        );
        assert_eq!(
            PresentationState::RetryAfterPresent.failed(),
            PresentationState::RetryAfterPresent
        );
    }

    #[test]
    fn only_retry_states_require_presentation() {
        assert!(!PresentationState::Unpresented.requires_presentation());
        assert!(!PresentationState::Presented.requires_presentation());
        assert!(PresentationState::RetryBeforeFirstPresent.requires_presentation());
        assert!(PresentationState::RetryAfterPresent.requires_presentation());
    }

    #[test]
    fn failed_present_after_a_successful_frame_uses_compositor_pacing() {
        // frame() sets FrameLoop::Ticking before complete_frame, so the retry
        // policy must follow presentation state rather than PresentationFailed.
        let after_failed_present = PresentationState::Presented.failed();
        assert_eq!(after_failed_present, PresentationState::RetryAfterPresent);
        assert!(after_failed_present.compositor_paced_retry());
        assert!(PresentationState::RetryAfterPresent.compositor_paced_retry());
        assert!(!PresentationState::RetryBeforeFirstPresent.compositor_paced_retry());
        assert!(!PresentationState::Unpresented.compositor_paced_retry());
        assert!(!PresentationState::Presented.compositor_paced_retry());
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FrameLoop {
    Unconfigured,
    Ticking,
    RescheduleRequested,
    PresentationFailed,
    AwaitingCallback,
    Scheduled,
    RetryScheduled,
    Parked,
}

pub(crate) struct WaylandWindow(pub WaylandWindowStatePtr);
pub enum ImeInput {
    InsertText(String),
    SetMarkedText(String),
    UnmarkText,
    DeleteText,
}

impl Drop for WaylandWindow {
    fn drop(&mut self) {
        self.0.frame_loop.set(FrameLoop::Parked);

        let mut state = self.0.state.borrow_mut();
        let surface_id = state.surface.id();
        let client = state.client.clone();

        state.renderer.destroy();
        if let Some(blur) = &state.blur {
            blur.release();
        }
        match &state.role {
            WaylandWindowRole::XdgToplevel {
                xdg_surface,
                toplevel,
                decoration,
            } => {
                if let Some(decoration) = decoration {
                    decoration.destroy();
                }
                toplevel.destroy();
                xdg_surface.destroy();
            }
            WaylandWindowRole::LayerShell { layer_surface, .. } => {
                layer_surface.destroy();
            }
        }
        if let Some(viewport) = &state.viewport {
            viewport.destroy();
        }
        state.surface.destroy();

        let state_ptr = self.0.clone();
        state
            .globals
            .executor
            .spawn(async move {
                state_ptr.close();
                client.drop_window(&surface_id)
            })
            .detach();
        drop(state);
    }
}

impl WaylandWindow {
    fn borrow(&self) -> Ref<'_, WaylandWindowState> {
        self.0.state.borrow()
    }

    fn borrow_mut(&self) -> RefMut<'_, WaylandWindowState> {
        self.0.state.borrow_mut()
    }

    pub fn new(
        handle: AnyWindowHandle,
        globals: Globals,
        gpu_context: GpuContext,
        client: WaylandClientStatePtr,
        params: WindowParams,
        gpu_resource_budget: &GpuResourceBudget,
        appearance: WindowAppearance,
        parent: Option<XdgToplevel>,
        outputs: Vec<Output>,
    ) -> anyhow::Result<(Self, ObjectId)> {
        let surface = globals.compositor.create_surface(&globals.qh, ());
        let role = create_window_role(&surface, &globals, &params, parent, &outputs)?;

        if let Some(fractional_scale_manager) = globals.fractional_scale_manager.as_ref() {
            fractional_scale_manager.get_fractional_scale(&surface, &globals.qh, surface.id());
        }

        let viewport = globals
            .viewporter
            .as_ref()
            .map(|viewporter| viewporter.get_viewport(&surface, &globals.qh, ()));

        let mouse_passthrough = params.mouse_passthrough;
        let frame_ping = globals.frame_ping.clone();

        let this = Self(WaylandWindowStatePtr {
            state: Rc::new(RefCell::new(WaylandWindowState::new(
                handle,
                surface.clone(),
                role,
                appearance,
                viewport,
                client,
                globals,
                gpu_context,
                params,
                gpu_resource_budget,
            )?)),
            callbacks: Rc::new(RefCell::new(Callbacks::default())),
            frame_loop: Rc::new(Cell::new(FrameLoop::Unconfigured)),
            frame_ping,
        });

        if mouse_passthrough {
            this.set_mouse_passthrough(true);
        }

        // Kick things off
        surface.commit();

        Ok((this, surface.id()))
    }
}

fn create_window_role(
    surface: &wl_surface::WlSurface,
    globals: &Globals,
    params: &WindowParams,
    parent: Option<XdgToplevel>,
    outputs: &[Output],
) -> anyhow::Result<WaylandWindowRole> {
    if let WindowKind::LayerShell(options) = &params.kind {
        return create_layer_shell_role(surface, globals, params, options, outputs);
    }

    Ok(create_xdg_toplevel_role(surface, globals, params, parent))
}

fn create_layer_shell_role(
    surface: &wl_surface::WlSurface,
    globals: &Globals,
    params: &WindowParams,
    options: &LayerShellOptions,
    outputs: &[Output],
) -> anyhow::Result<WaylandWindowRole> {
    let layer_shell = globals
        .layer_shell
        .as_ref()
        .ok_or(LayerShellNotSupportedError)?;
    let output = output_for_layer_shell(params, outputs);
    let layer_surface = layer_shell.get_layer_surface(
        surface,
        output.as_ref(),
        wayland_layer(options.layer),
        options.namespace.to_string(),
        &globals.qh,
        surface.id(),
    );

    configure_layer_shell_surface(&layer_surface, options, params.bounds.size);
    Ok(WaylandWindowRole::LayerShell {
        layer_surface,
        options: options.clone(),
        configured: false,
    })
}

fn output_for_layer_shell(
    params: &WindowParams,
    outputs: &[Output],
) -> Option<wl_output::WlOutput> {
    params.display_id.and_then(|display_id| {
        outputs
            .iter()
            .find(|output| DisplayId::from(output.id.protocol_id()) == display_id)
            .map(|output| output.output.clone())
    })
}

fn create_xdg_toplevel_role(
    surface: &wl_surface::WlSurface,
    globals: &Globals,
    params: &WindowParams,
    parent: Option<XdgToplevel>,
) -> WaylandWindowRole {
    let xdg_surface = globals
        .wm_base
        .get_xdg_surface(surface, &globals.qh, surface.id());
    let toplevel = xdg_surface.get_toplevel(&globals.qh, surface.id());

    if let Some(app_id) = &params.app_id {
        toplevel.set_app_id(app_id.clone());
    }

    if params.kind == WindowKind::Floating || params.kind == WindowKind::Overlay {
        toplevel.set_parent(parent.as_ref());
    }

    if params.kind == WindowKind::Overlay {
        log::warn!(
            "Wayland: WindowKind::Overlay does not support true always-on-top without \
             layer-shell support; falling back to xdg_toplevel."
        );
    }

    if let Some(size) = params.window_min_size {
        toplevel.set_min_size(f32::from(size.width) as i32, f32::from(size.height) as i32);
    }

    let decoration = globals
        .decoration_manager
        .as_ref()
        .map(|decoration_manager| {
            decoration_manager.get_toplevel_decoration(&toplevel, &globals.qh, surface.id())
        });

    WaylandWindowRole::XdgToplevel {
        xdg_surface,
        toplevel,
        decoration,
    }
}

fn layer_surface_request_size(size: Size<Pixels>) -> (u32, u32) {
    (f32::from(size.width) as u32, f32::from(size.height) as u32)
}

fn configure_layer_shell_surface(
    layer_surface: &zwlr_layer_surface_v1::ZwlrLayerSurfaceV1,
    options: &LayerShellOptions,
    size: Size<Pixels>,
) {
    let (width, height) = layer_surface_request_size(size);
    layer_surface.set_size(width, height);
    layer_surface.set_anchor(wayland_anchor(options.anchor));
    if let Some((top, right, bottom, left)) = options.margin {
        layer_surface.set_margin(
            f32::from(top) as i32,
            f32::from(right) as i32,
            f32::from(bottom) as i32,
            f32::from(left) as i32,
        );
    }
    if let Some(exclusive_zone) = options.exclusive_zone {
        layer_surface.set_exclusive_zone(f32::from(exclusive_zone) as i32);
    }
    layer_surface.set_keyboard_interactivity(wayland_keyboard_interactivity(
        options.keyboard_interactivity,
    ));

    if let Some(exclusive_edge) = options.exclusive_edge {
        apply_exclusive_edge(layer_surface, options.anchor, exclusive_edge);
    }
}

fn exclusive_edge_is_valid(anchor: Anchor, edge: Anchor) -> bool {
    edge.bits().count_ones() == 1 && anchor.contains(edge)
}

fn apply_exclusive_edge(
    layer_surface: &zwlr_layer_surface_v1::ZwlrLayerSurfaceV1,
    anchor: Anchor,
    edge: Anchor,
) -> bool {
    if !layer_shell_supports_exclusive_edge(layer_surface.version()) {
        log::warn!(
            "Wayland: wlr-layer-shell v{} does not support selecting an exclusive edge; the compositor will infer it from the anchors.",
            layer_surface.version()
        );
        return false;
    }
    if !exclusive_edge_is_valid(anchor, edge) {
        log::warn!(
            "Wayland: ignoring exclusive edge {edge:?}; it must be one edge contained in anchor {anchor:?}"
        );
        return false;
    }

    layer_surface.set_exclusive_edge(wayland_anchor(edge));
    true
}

fn layer_shell_supports_exclusive_edge(version: u32) -> bool {
    version >= zwlr_layer_surface_v1::REQ_SET_EXCLUSIVE_EDGE_SINCE
}

fn wayland_layer(layer: Layer) -> zwlr_layer_shell_v1::Layer {
    match layer {
        Layer::Background => zwlr_layer_shell_v1::Layer::Background,
        Layer::Bottom => zwlr_layer_shell_v1::Layer::Bottom,
        Layer::Top => zwlr_layer_shell_v1::Layer::Top,
        Layer::Overlay => zwlr_layer_shell_v1::Layer::Overlay,
    }
}

fn wayland_anchor(anchor: Anchor) -> zwlr_layer_surface_v1::Anchor {
    zwlr_layer_surface_v1::Anchor::from_bits_truncate(anchor.bits())
}

fn wayland_keyboard_interactivity(
    interactivity: KeyboardInteractivity,
) -> zwlr_layer_surface_v1::KeyboardInteractivity {
    match interactivity {
        KeyboardInteractivity::None => zwlr_layer_surface_v1::KeyboardInteractivity::None,
        KeyboardInteractivity::OnDemand => zwlr_layer_surface_v1::KeyboardInteractivity::OnDemand,
        KeyboardInteractivity::Exclusive => zwlr_layer_surface_v1::KeyboardInteractivity::Exclusive,
    }
}

fn layer_configure_size(current_size: Size<Pixels>, width: i32, height: i32) -> Size<Pixels> {
    match (width, height) {
        (0, 0) => current_size,
        (0, height) => size(current_size.width, px(height as f32)),
        (width, 0) => size(px(width as f32), current_size.height),
        (width, height) => size(px(width as f32), px(height as f32)),
    }
}

impl WaylandWindowStatePtr {
    pub fn handle(&self) -> AnyWindowHandle {
        self.state.borrow().handle
    }

    pub fn surface(&self) -> wl_surface::WlSurface {
        self.state.borrow().surface.clone()
    }

    pub fn toplevel(&self) -> Option<xdg_toplevel::XdgToplevel> {
        self.state.borrow().role.toplevel()
    }

    pub fn ptr_eq(&self, other: &Self) -> bool {
        Rc::ptr_eq(&self.state, &other.state)
    }

    pub fn frame(&self) {
        if !self.state.borrow().visible {
            self.state.borrow_mut().redraw_requested = true;
            self.frame_loop.set(FrameLoop::Parked);
            return;
        }

        self.frame_loop.set(FrameLoop::Ticking);
        let mut state = self.state.borrow_mut();
        state.resize_throttle = false;
        // GPUI may throttle this tick without calling draw, so leave the request
        // latched until a draw actually reaches the renderer.
        let force_render = state.redraw_requested;
        let require_presentation = state.presentation.requires_presentation();
        drop(state);

        let mut callbacks = self.callbacks.borrow_mut();
        let Some(request_frame_callback) = callbacks.request_frame.as_mut() else {
            self.frame_loop.set(FrameLoop::Parked);
            return;
        };
        request_frame_callback(RequestFrameOptions {
            force_render,
            require_presentation,
        });
        self.update_ime_enabled();
        drop(callbacks);

        self.complete_frame();
    }

    fn complete_frame(&self) {
        let mut state = self.state.borrow_mut();
        if !state.visible {
            self.frame_loop.set(FrameLoop::Parked);
            return;
        }
        if is_unconfigured_layer_shell(&state.role) {
            self.frame_loop.set(FrameLoop::Unconfigured);
            return;
        }

        let frame_loop = self.frame_loop.get();
        if frame_loop == FrameLoop::AwaitingCallback {
            return;
        }

        if state.presentation.requires_presentation() {
            // Before the first present, or when throttling skipped draw, a
            // callback may never arrive. After a successful present, pace
            // retries with the compositor so an occluded window does not keep
            // polling at 60 Hz. This depends on presentation state because
            // frame() has already set Ticking.
            if state.presentation.compositor_paced_retry() {
                if state.pending_frame_callback.is_none() {
                    let callback = state.surface.frame(&state.globals.qh, state.surface.id());
                    state.pending_frame_callback = Some(callback);
                }
                state.surface.commit();
                self.frame_loop.set(FrameLoop::AwaitingCallback);
                return;
            }

            self.frame_loop.set(FrameLoop::RetryScheduled);
            let surface_id = state.surface.id();
            let client = state.client.clone();
            drop(state);
            client.schedule_frame_retry(&surface_id);
            return;
        }

        if frame_loop == FrameLoop::RescheduleRequested || state.redraw_requested {
            self.frame_loop.set(FrameLoop::RetryScheduled);
            let surface_id = state.surface.id();
            let client = state.client.clone();
            drop(state);
            client.schedule_frame_retry(&surface_id);
            return;
        }

        self.frame_loop.set(FrameLoop::Parked);
    }

    pub fn frame_callback_fired(&self) {
        // Another wl_surface commit may have carried this callback while a retry
        // timer owned the render-loop wakeup.
        self.state.borrow_mut().pending_frame_callback = None;
        if self.frame_loop.get() == FrameLoop::AwaitingCallback {
            self.frame();
        }
    }

    pub fn scheduled_frame_fired(&self) {
        if self.frame_loop.get() == FrameLoop::Scheduled {
            self.frame();
        }
    }

    pub fn retry_timer_fired(&self) {
        if self.frame_loop.get() == FrameLoop::RetryScheduled {
            self.frame();
        }
    }

    pub fn is_configured(&self) -> bool {
        self.frame_loop.get() != FrameLoop::Unconfigured
    }

    pub fn schedule_frame(&self) {
        if !self.state.borrow().visible {
            return;
        }
        match self.frame_loop.get() {
            FrameLoop::Parked => {
                self.frame_loop.set(FrameLoop::Scheduled);
                self.frame_ping.ping();
            }
            FrameLoop::Ticking => {
                self.frame_loop.set(FrameLoop::RescheduleRequested);
            }
            // A wake is already armed: a ping or retry timer is in flight, or a
            // presented buffer guarantees a compositor frame callback.
            _ => {}
        }
    }

    fn request_redraw(&self) {
        self.state.borrow_mut().redraw_requested = true;
        self.schedule_frame();
    }

    fn update_ime_enabled(&self) {
        let mut state = self.state.borrow_mut();
        if !state.active {
            return;
        }
        let client = state.client.clone();
        let ime_enabled = state
            .input_handler
            .as_mut()
            .map(|input_handler| input_handler.query_accepts_text_input())
            .unwrap_or(false);
        drop(state);

        let Some(ime_enabled) = required_ime_state_change(client.ime_enabled(), ime_enabled) else {
            return;
        };

        if ime_enabled {
            client.enable_ime();
        } else {
            client.disable_ime();
        }
    }

    pub fn handle_xdg_surface_event(&self, event: xdg_surface::Event) {
        if let xdg_surface::Event::Configure { serial } = event {
            {
                let mut state = self.state.borrow_mut();
                if let Some(window_controls) = state.in_progress_window_controls.take() {
                    state.window_controls = window_controls;

                    drop(state);
                    let mut callbacks = self.callbacks.borrow_mut();
                    if let Some(appearance_changed) = callbacks.appearance_changed.as_mut() {
                        appearance_changed();
                    }
                }
            }
            {
                let mut state = self.state.borrow_mut();

                if let Some(mut configure) = state.in_progress_configure.take() {
                    let got_unmaximized = state.maximized && !configure.maximized;
                    state.fullscreen = configure.fullscreen;
                    state.maximized = configure.maximized;
                    state.tiling = configure.tiling;
                    // Limit interactive resizes to once per vblank
                    if configure.resizing && state.resize_throttle {
                        return;
                    } else if configure.resizing {
                        state.resize_throttle = true;
                    }
                    if !configure.fullscreen && !configure.maximized {
                        configure.size = if got_unmaximized {
                            Some(state.window_bounds.size)
                        } else {
                            compute_outer_size(state.inset(), configure.size, state.tiling)
                        };
                        if let Some(size) = configure.size {
                            state.window_bounds = Bounds {
                                origin: Point::default(),
                                size,
                            };
                        }
                    }
                    drop(state);
                    if let Some(size) = configure.size {
                        self.resize(size);
                    }
                }
            }
            let state = self.state.borrow_mut();
            let xdg_surface = match &state.role {
                WaylandWindowRole::XdgToplevel { xdg_surface, .. } => xdg_surface.clone(),
                WaylandWindowRole::LayerShell { .. } => return,
            };
            xdg_surface.ack_configure(serial);

            let window_geometry = inset_by_tiling(
                state.bounds.map_origin(|_| px(0.0)),
                state.inset(),
                state.tiling,
            )
            .map(|v| f32::from(v) as i32)
            .map_size(|v| if v <= 0 { 1 } else { v });

            xdg_surface.set_window_geometry(
                window_geometry.origin.x,
                window_geometry.origin.y,
                window_geometry.size.width,
                window_geometry.size.height,
            );

            let initial_configure = !self.is_configured();
            drop(state);
            if initial_configure {
                self.frame();
            } else {
                self.request_redraw();
            }
        }
    }

    pub fn handle_toplevel_decoration_event(&self, event: zxdg_toplevel_decoration_v1::Event) {
        if let zxdg_toplevel_decoration_v1::Event::Configure { mode } = event {
            match mode {
                WEnum::Value(zxdg_toplevel_decoration_v1::Mode::ServerSide) => {
                    self.state.borrow_mut().decorations = WindowDecorations::Server;
                    if let Some(mut appearance_changed) =
                        self.callbacks.borrow_mut().appearance_changed.as_mut()
                    {
                        appearance_changed();
                    }
                }
                WEnum::Value(zxdg_toplevel_decoration_v1::Mode::ClientSide) => {
                    self.state.borrow_mut().decorations = WindowDecorations::Client;
                    // Update background to be transparent
                    if let Some(mut appearance_changed) =
                        self.callbacks.borrow_mut().appearance_changed.as_mut()
                    {
                        appearance_changed();
                    }
                }
                WEnum::Value(_) => {
                    log::warn!("Unknown decoration mode");
                    return;
                }
                WEnum::Unknown(v) => {
                    log::warn!("Unknown decoration mode: {}", v);
                    return;
                }
            }
            update_window(self.state.borrow_mut());
            self.request_redraw();
        }
    }

    pub fn handle_wlr_layer_surface_event(&self, event: zwlr_layer_surface_v1::Event) -> bool {
        match event {
            zwlr_layer_surface_v1::Event::Configure {
                serial,
                width,
                height,
            } => {
                let mut state = self.state.borrow_mut();
                let size = layer_configure_size(state.bounds.size, width as i32, height as i32);
                let layer_surface = match &mut state.role {
                    WaylandWindowRole::LayerShell {
                        layer_surface,
                        configured,
                        ..
                    } => {
                        *configured = true;
                        layer_surface.clone()
                    }
                    WaylandWindowRole::XdgToplevel { .. } => return false,
                };
                layer_surface.ack_configure(serial);
                drop(state);
                self.resize(size);
                let initial_configure = !self.is_configured();
                if initial_configure {
                    self.frame();
                } else {
                    self.request_redraw();
                }
                false
            }
            zwlr_layer_surface_v1::Event::Closed => true,
            _ => false,
        }
    }

    pub fn handle_fractional_scale_event(&self, event: wp_fractional_scale_v1::Event) {
        if let wp_fractional_scale_v1::Event::PreferredScale { scale } = event {
            self.rescale(scale as f32 / 120.0);
            self.request_redraw();
        }
    }

    pub fn handle_toplevel_event(&self, event: xdg_toplevel::Event) -> bool {
        match event {
            xdg_toplevel::Event::Configure {
                width,
                height,
                states,
            } => {
                let mut size = if width == 0 || height == 0 {
                    None
                } else {
                    Some(size(px(width as f32), px(height as f32)))
                };

                let states = extract_states::<xdg_toplevel::State>(&states);

                let mut tiling = Tiling::default();
                let mut fullscreen = false;
                let mut maximized = false;
                let mut resizing = false;

                for state in states {
                    match state {
                        xdg_toplevel::State::Maximized => {
                            maximized = true;
                        }
                        xdg_toplevel::State::Fullscreen => {
                            fullscreen = true;
                        }
                        xdg_toplevel::State::Resizing => resizing = true,
                        xdg_toplevel::State::TiledTop => {
                            tiling.top = true;
                        }
                        xdg_toplevel::State::TiledLeft => {
                            tiling.left = true;
                        }
                        xdg_toplevel::State::TiledRight => {
                            tiling.right = true;
                        }
                        xdg_toplevel::State::TiledBottom => {
                            tiling.bottom = true;
                        }
                        _ => {
                            // noop
                        }
                    }
                }

                if fullscreen || maximized {
                    tiling = Tiling::tiled();
                }

                let mut state = self.state.borrow_mut();
                state.in_progress_configure = Some(InProgressConfigure {
                    size,
                    fullscreen,
                    maximized,
                    resizing,
                    tiling,
                });

                false
            }
            xdg_toplevel::Event::Close => {
                let mut cb = self.callbacks.borrow_mut();
                if let Some(mut should_close) = cb.should_close.take() {
                    let result = (should_close)();
                    cb.should_close = Some(should_close);
                    if result {
                        drop(cb);
                        self.close();
                    }
                    result
                } else {
                    true
                }
            }
            xdg_toplevel::Event::WmCapabilities { capabilities } => {
                let mut window_controls = WindowControls::default();

                let states = extract_states::<xdg_toplevel::WmCapabilities>(&capabilities);

                for state in states {
                    match state {
                        xdg_toplevel::WmCapabilities::Maximize => {
                            window_controls.maximize = true;
                        }
                        xdg_toplevel::WmCapabilities::Minimize => {
                            window_controls.minimize = true;
                        }
                        xdg_toplevel::WmCapabilities::Fullscreen => {
                            window_controls.fullscreen = true;
                        }
                        xdg_toplevel::WmCapabilities::WindowMenu => {
                            window_controls.window_menu = true;
                        }
                        _ => {}
                    }
                }

                let mut state = self.state.borrow_mut();
                state.in_progress_window_controls = Some(window_controls);
                false
            }
            _ => false,
        }
    }

    #[allow(clippy::mutable_key_type)]
    pub fn handle_surface_event(
        &self,
        event: wl_surface::Event,
        outputs: HashMap<ObjectId, Output>,
    ) {
        let mut state = self.state.borrow_mut();

        match event {
            wl_surface::Event::Enter { output } => {
                let id = output.id();

                let Some(output) = outputs.get(&id) else {
                    return;
                };

                state.outputs.insert(id, output.clone());

                let scale = state.primary_output_scale();

                // We use `PreferredBufferScale` instead to set the scale if it's available
                if state.surface.version() < wl_surface::EVT_PREFERRED_BUFFER_SCALE_SINCE {
                    state.surface.set_buffer_scale(scale);
                    drop(state);
                    self.rescale(scale as f32);
                } else {
                    drop(state);
                }
                self.request_redraw();
            }
            wl_surface::Event::Leave { output } => {
                state.outputs.remove(&output.id());

                let scale = state.primary_output_scale();

                // We use `PreferredBufferScale` instead to set the scale if it's available
                if state.surface.version() < wl_surface::EVT_PREFERRED_BUFFER_SCALE_SINCE {
                    state.surface.set_buffer_scale(scale);
                    drop(state);
                    self.rescale(scale as f32);
                } else {
                    drop(state);
                }
                self.request_redraw();
            }
            wl_surface::Event::PreferredBufferScale { factor } => {
                // We use `WpFractionalScale` instead to set the scale if it's available
                if state.globals.fractional_scale_manager.is_none() {
                    state.surface.set_buffer_scale(factor);
                    drop(state);
                    self.rescale(factor as f32);
                    self.request_redraw();
                }
            }
            _ => {}
        }
    }

    pub fn handle_ime(&self, ime: ImeInput) {
        let mut state = self.state.borrow_mut();
        if let Some(mut input_handler) = state.input_handler.take() {
            drop(state);
            match ime {
                ImeInput::InsertText(text) => {
                    input_handler.replace_text_in_range(None, &text);
                }
                ImeInput::SetMarkedText(text) => {
                    input_handler.replace_and_mark_text_in_range(None, &text, None);
                }
                ImeInput::UnmarkText => {
                    input_handler.unmark_text();
                }
                ImeInput::DeleteText => {
                    if let Some(marked) = input_handler.marked_text_range() {
                        input_handler.replace_text_in_range(Some(marked), "");
                    }
                }
            }
            self.state.borrow_mut().input_handler = Some(input_handler);
        }
    }

    pub fn get_ime_area(&self) -> Option<Bounds<Pixels>> {
        let mut state = self.state.borrow_mut();
        let mut bounds: Option<Bounds<Pixels>> = None;
        if let Some(mut input_handler) = state.input_handler.take() {
            drop(state);
            bounds = input_handler.ime_candidate_bounds();
            self.state.borrow_mut().input_handler = Some(input_handler);
        }
        bounds
    }

    pub fn set_size_and_scale(&self, size: Option<Size<Pixels>>, scale: Option<f32>) {
        let (size, scale) = {
            let mut state = self.state.borrow_mut();
            if size.is_none_or(|size| size == state.bounds.size)
                && scale.is_none_or(|scale| scale == state.scale)
            {
                return;
            }
            if let Some(size) = size {
                state.bounds.size = size;
            }
            if let Some(scale) = scale {
                state.scale = scale;
            }
            let device_bounds = state.bounds.to_device_pixels(state.scale);
            state.renderer.update_drawable_size(device_bounds.size);
            (state.bounds.size, state.scale)
        };

        if let Some(ref mut fun) = self.callbacks.borrow_mut().resize {
            fun(size, scale);
        }

        {
            let state = self.state.borrow();
            if let Some(viewport) = &state.viewport {
                viewport
                    .set_destination(f32::from(size.width) as i32, f32::from(size.height) as i32);
            }
        }
    }

    pub fn resize(&self, size: Size<Pixels>) {
        self.set_size_and_scale(Some(size), None);
    }

    pub fn rescale(&self, scale: f32) {
        self.set_size_and_scale(None, Some(scale));
    }

    pub fn close(&self) {
        let mut callbacks = self.callbacks.borrow_mut();
        if let Some(fun) = callbacks.close.take() {
            fun()
        }
    }

    pub fn handle_input(&self, input: PlatformInput) {
        if let Some(ref mut fun) = self.callbacks.borrow_mut().input
            && !fun(input.clone()).propagate
        {
            return;
        }
        if let PlatformInput::KeyDown(event) = input
            && event.keystroke.modifiers.is_subset_of(&Modifiers::shift())
            && let Some(key_char) = &event.keystroke.key_char
        {
            let mut state = self.state.borrow_mut();
            if let Some(mut input_handler) = state.input_handler.take() {
                drop(state);
                input_handler.replace_text_in_range(None, key_char);
                self.state.borrow_mut().input_handler = Some(input_handler);
            }
        }
    }

    pub fn set_focused(&self, focus: bool) {
        self.state.borrow_mut().active = focus;
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
        if let Some(ref mut fun) = self.callbacks.borrow_mut().hover_status_change {
            fun(focus);
        }
    }

    pub fn set_appearance(&mut self, appearance: WindowAppearance) {
        self.state.borrow_mut().appearance = appearance;

        let mut callbacks = self.callbacks.borrow_mut();
        if let Some(ref mut fun) = callbacks.appearance_changed {
            (fun)()
        }
    }

    pub fn primary_output_scale(&self) -> i32 {
        self.state.borrow_mut().primary_output_scale()
    }
}

fn required_ime_state_change(current: Option<bool>, desired: bool) -> Option<bool> {
    (current != Some(desired)).then_some(desired)
}

fn extract_states<'a, S: TryFrom<u32> + 'a>(states: &'a [u8]) -> impl Iterator<Item = S> + 'a
where
    <S as TryFrom<u32>>::Error: 'a,
{
    states
        .chunks_exact(4)
        .flat_map(TryInto::<[u8; 4]>::try_into)
        .map(u32::from_ne_bytes)
        .flat_map(S::try_from)
}

impl rwh::HasWindowHandle for WaylandWindow {
    fn window_handle(&self) -> Result<rwh::WindowHandle<'_>, rwh::HandleError> {
        let surface = self.0.surface().id().as_ptr() as *mut libc::c_void;
        let c_ptr = NonNull::new(surface).ok_or(rwh::HandleError::Unavailable)?;
        let handle = rwh::WaylandWindowHandle::new(c_ptr);
        let raw_handle = rwh::RawWindowHandle::Wayland(handle);
        Ok(unsafe { rwh::WindowHandle::borrow_raw(raw_handle) })
    }
}

impl rwh::HasDisplayHandle for WaylandWindow {
    fn display_handle(&self) -> Result<rwh::DisplayHandle<'_>, rwh::HandleError> {
        let display = self
            .0
            .surface()
            .backend()
            .upgrade()
            .ok_or(rwh::HandleError::Unavailable)?
            .display_ptr() as *mut libc::c_void;

        let c_ptr = NonNull::new(display).ok_or(rwh::HandleError::Unavailable)?;
        let handle = rwh::WaylandDisplayHandle::new(c_ptr);
        let raw_handle = rwh::RawDisplayHandle::Wayland(handle);
        Ok(unsafe { rwh::DisplayHandle::borrow_raw(raw_handle) })
    }
}

impl PlatformWindow for WaylandWindow {
    fn bounds(&self) -> Bounds<Pixels> {
        self.borrow().bounds
    }

    fn is_maximized(&self) -> bool {
        self.borrow().maximized
    }

    fn window_bounds(&self) -> WindowBounds {
        let state = self.borrow();
        if state.fullscreen {
            WindowBounds::Fullscreen(state.window_bounds)
        } else if state.maximized {
            WindowBounds::Maximized(state.window_bounds)
        } else {
            drop(state);
            WindowBounds::Windowed(self.bounds())
        }
    }

    fn inner_window_bounds(&self) -> WindowBounds {
        let state = self.borrow();
        if state.fullscreen {
            WindowBounds::Fullscreen(state.window_bounds)
        } else if state.maximized {
            WindowBounds::Maximized(state.window_bounds)
        } else {
            let inset = state.inset();
            drop(state);
            WindowBounds::Windowed(self.bounds().inset(inset))
        }
    }

    fn content_size(&self) -> Size<Pixels> {
        self.borrow().bounds.size
    }

    fn resize(&mut self, size: Size<Pixels>) {
        let state = self.borrow();
        let state_ptr = self.0.clone();
        let dp_size = size.to_device_pixels(self.scale_factor());

        match &state.role {
            WaylandWindowRole::XdgToplevel { xdg_surface, .. } => {
                xdg_surface.set_window_geometry(
                    f32::from(state.bounds.origin.x) as i32,
                    f32::from(state.bounds.origin.y) as i32,
                    dp_size.width.0,
                    dp_size.height.0,
                );
            }
            WaylandWindowRole::LayerShell { layer_surface, .. } => {
                if state.visible {
                    let (width, height) = layer_surface_request_size(size);
                    layer_surface.set_size(width, height);
                }
            }
        }

        state
            .globals
            .executor
            .spawn(async move { state_ptr.resize(size) })
            .detach();
    }

    fn scale_factor(&self) -> f32 {
        self.borrow().scale
    }

    fn appearance(&self) -> WindowAppearance {
        self.borrow().appearance
    }

    fn display(&self) -> Option<Rc<dyn PlatformDisplay>> {
        let state = self.borrow();
        state.display.as_ref().map(|(id, display)| {
            Rc::new(WaylandDisplay {
                id: id.clone(),
                name: display.name.clone(),
                bounds: display.bounds.to_pixels(state.scale),
            }) as Rc<dyn PlatformDisplay>
        })
    }

    fn mouse_position(&self) -> Point<Pixels> {
        self.borrow()
            .client
            .get_client()
            .borrow()
            .mouse_location
            .unwrap_or_default()
    }

    fn modifiers(&self) -> Modifiers {
        self.borrow().client.get_client().borrow().modifiers
    }

    fn capslock(&self) -> Capslock {
        self.borrow().client.get_client().borrow().capslock
    }

    fn set_input_handler(&mut self, input_handler: PlatformInputHandler) {
        self.borrow_mut().input_handler = Some(input_handler);
    }

    fn take_input_handler(&mut self) -> Option<PlatformInputHandler> {
        self.borrow_mut().input_handler.take()
    }

    fn prompt(
        &self,
        _level: PromptLevel,
        _msg: &str,
        _detail: Option<&str>,
        _answers: &[PromptButton],
    ) -> Option<Receiver<usize>> {
        None
    }

    fn activate(&self) {
        // Try to request an activation token. Even though the activation is likely going to be rejected,
        // KWin and Mutter can use the app_id to visually indicate we're requesting attention.
        let state = self.borrow();
        if let (Some(activation), Some(app_id)) = (&state.globals.activation, state.app_id.clone())
        {
            state.client.set_pending_activation(state.surface.id());
            let token = activation.get_activation_token(&state.globals.qh, ());
            // The serial isn't exactly important here, since the activation is probably going to be rejected anyway.
            let serial = state.client.get_serial(SerialKind::MousePress);
            token.set_app_id(app_id);
            token.set_serial(serial.as_raw(), &state.globals.seat);
            token.set_surface(&state.surface);
            token.commit();
        }
    }

    fn is_active(&self) -> bool {
        self.borrow().active
    }

    fn is_hovered(&self) -> bool {
        self.borrow().hovered
    }

    fn set_title(&mut self, title: &str) {
        let state = self.borrow();
        if let Some(toplevel) = state.role.toplevel() {
            toplevel.set_title(title.to_string());
        }
    }

    fn set_app_id(&mut self, app_id: &str) {
        let mut state = self.borrow_mut();
        match &state.role {
            WaylandWindowRole::XdgToplevel { toplevel, .. } => {
                toplevel.set_app_id(app_id.to_owned());
            }
            WaylandWindowRole::LayerShell { .. } => {}
        }
        state.app_id = Some(app_id.to_owned());
    }

    fn set_background_appearance(&self, background_appearance: WindowBackgroundAppearance) {
        let mut state = self.borrow_mut();
        if state.background_appearance == background_appearance {
            return;
        }
        state.background_appearance = background_appearance;
        update_window(state);
        self.0.request_redraw();
    }

    fn minimize(&self) {
        if let Some(toplevel) = self.borrow().role.toplevel() {
            toplevel.set_minimized();
        }
    }

    fn zoom(&self) {
        let state = self.borrow();
        let Some(toplevel) = state.role.toplevel() else {
            return;
        };
        if !state.maximized {
            toplevel.set_maximized();
        } else {
            toplevel.unset_maximized();
        }
    }

    fn toggle_fullscreen(&self) {
        let mut state = self.borrow_mut();
        let Some(toplevel) = state.role.toplevel() else {
            return;
        };
        if !state.fullscreen {
            toplevel.set_fullscreen(None);
        } else {
            toplevel.unset_fullscreen();
        }
    }

    fn is_fullscreen(&self) -> bool {
        self.borrow().fullscreen
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

    fn on_hover_status_change(&self, callback: Box<dyn FnMut(bool)>) {
        self.0.callbacks.borrow_mut().hover_status_change = Some(callback);
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
        let mut state = self.borrow_mut();

        if !state.visible || is_unconfigured_layer_shell(&state.role) {
            state.redraw_requested = true;
            return;
        }

        if state.renderer.device_lost() {
            let raw_window = RawWindow {
                window: state.surface.id().as_ptr().cast::<c_void>(),
                display: state
                    .surface
                    .backend()
                    .upgrade()
                    .unwrap()
                    .display_ptr()
                    .cast::<c_void>(),
            };
            if let Err(err) = state.renderer.recover(&raw_window) {
                log::warn!("GPU recovery failed, will retry on next frame: {err}");
            }

            state.redraw_requested = true;
            return;
        }

        // Surface state changed during this GPUI tick is included in this presentation.
        state.redraw_requested = false;
        if state.pending_frame_callback.is_none() {
            let callback = state.surface.frame(&state.globals.qh, state.surface.id());
            state.pending_frame_callback = Some(callback);
        }
        if state.renderer.draw(scene) {
            state.presentation = PresentationState::Presented;
            self.0.frame_loop.set(FrameLoop::AwaitingCallback);
        } else {
            state.presentation = state.presentation.failed();
            self.0.frame_loop.set(FrameLoop::PresentationFailed);
        }

        if state.renderer.needs_redraw() {
            state.redraw_requested = true;
        }
    }

    fn schedule_frame(&self) {
        self.0.schedule_frame();
    }

    #[cfg(any(test, feature = "test-support"))]
    fn render_to_image(&self, scene: &Scene) -> anyhow::Result<image::RgbaImage> {
        self.borrow_mut().renderer.render_scene_to_image(scene)
    }

    fn sprite_atlas(&self) -> Arc<dyn PlatformAtlas> {
        let state = self.borrow();
        state.renderer.sprite_atlas().clone()
    }

    fn show_window_menu(&self, position: Point<Pixels>) {
        let state = self.borrow();
        let Some(toplevel) = state.role.toplevel() else {
            return;
        };
        let serial = state.client.get_serial(SerialKind::MousePress);
        toplevel.show_window_menu(
            &state.globals.seat,
            serial.as_raw(),
            f32::from(position.x) as i32,
            f32::from(position.y) as i32,
        );
    }

    fn start_window_move(&self) {
        let state = self.borrow();
        let Some(toplevel) = state.role.toplevel() else {
            return;
        };
        let serial = state.client.get_serial(SerialKind::MousePress);
        toplevel._move(&state.globals.seat, serial.as_raw());
    }

    fn start_window_resize(&self, edge: gpui::ResizeEdge) {
        let state = self.borrow();
        let Some(toplevel) = state.role.toplevel() else {
            return;
        };
        toplevel.resize(
            &state.globals.seat,
            state.client.get_serial(SerialKind::MousePress).as_raw(),
            resize_edge_to_xdg(edge),
        )
    }

    fn window_decorations(&self) -> Decorations {
        let state = self.borrow();
        if state.role.is_layer_shell() {
            return Decorations::Client {
                tiling: Tiling::default(),
            };
        }
        match state.decorations {
            WindowDecorations::Server => Decorations::Server,
            WindowDecorations::Client => Decorations::Client {
                tiling: state.tiling,
            },
        }
    }

    fn request_decorations(&self, decorations: WindowDecorations) {
        let mut state = self.borrow_mut();
        state.decorations = decorations;
        let decoration = match &state.role {
            WaylandWindowRole::XdgToplevel { decoration, .. } => decoration.clone(),
            WaylandWindowRole::LayerShell { .. } => None,
        };
        if let Some(decoration) = decoration {
            decoration.set_mode(window_decorations_to_xdg(decorations));
            update_window(state);
        } else {
            drop(state);
        }
        self.0.request_redraw();
    }

    fn window_controls(&self) -> WindowControls {
        self.borrow().window_controls
    }

    fn set_client_inset(&self, inset: Pixels) {
        let mut state = self.borrow_mut();
        if Some(inset) != state.client_inset {
            state.client_inset = Some(inset);
            update_window(state);
            self.0.request_redraw();
        }
    }

    fn update_ime_position(&self, bounds: Bounds<Pixels>) {
        let state = self.borrow();
        if !state.active {
            return;
        }
        state.client.update_ime_position(bounds);
    }

    fn gpu_specs(&self) -> Option<GpuSpecs> {
        self.borrow().renderer.gpu_specs().into()
    }

    fn play_system_bell(&self) {
        let state = self.borrow();
        if let Some(bell) = state.globals.system_bell.as_ref() {
            bell.ring(Some(&state.surface));
        }
    }

    fn show(&self) {
        let mut state = self.borrow_mut();
        state.visible = true;
        let size = state.bounds.size;
        let remapping_layer_shell = matches!(&state.role, WaylandWindowRole::LayerShell { .. });
        match &mut state.role {
            WaylandWindowRole::LayerShell {
                configured,
                layer_surface,
                options,
            } => {
                *configured = false;
                configure_layer_shell_surface(layer_surface, options, size);
            }
            WaylandWindowRole::XdgToplevel { .. } => {}
        }
        // Commit so the compositor sends a configure. Do not request a fake
        // frame callback; the demand-driven loop waits for configure or a ping.
        state.surface.commit();
        drop(state);
        if remapping_layer_shell {
            self.0.frame_loop.set(FrameLoop::Unconfigured);
        } else {
            self.0.request_redraw();
        }
    }

    fn hide(&self) {
        let mut state = self.borrow_mut();
        state.visible = false;
        self.0.frame_loop.set(FrameLoop::Parked);
        if is_unconfigured_layer_shell(&state.role) {
            return;
        }
        state.surface.attach(None, 0, 0);
        state.surface.commit();
    }

    fn is_visible(&self) -> bool {
        self.borrow().visible
    }

    fn set_exclusive_zone(&self, zone: Pixels) {
        let state = self.borrow();
        if state.role.set_exclusive_zone(f32::from(zone) as i32) {
            state.surface.commit();
        }
    }

    fn set_exclusive_edge(&self, edge: Anchor) {
        let state = self.borrow();
        if state.role.set_exclusive_edge(edge) {
            state.surface.commit();
        }
    }

    fn set_input_region(&self, region: Option<&[Bounds<Pixels>]>) {
        let state = self.borrow();
        match region {
            // No region means the whole surface receives input.
            None => state.surface.set_input_region(None),
            // A region restricts input to its rectangles. An empty region
            // receives no input at all.
            Some(rects) => {
                let wl_region = state
                    .globals
                    .compositor
                    .create_region(&state.globals.qh, ());
                for rect in rects {
                    let rect = rect.map(|pixels| f32::from(pixels) as i32);
                    wl_region.add(
                        rect.origin.x,
                        rect.origin.y,
                        rect.size.width,
                        rect.size.height,
                    );
                }
                state.surface.set_input_region(Some(&wl_region));
                wl_region.destroy();
            }
        }

        // Commit so the new input region applies immediately. Otherwise it
        // waits for the next frame, which could be the very click we want to
        // allow passing through.
        if !is_unconfigured_layer_shell(&state.role) {
            state.surface.commit();
        }
    }

    fn set_mouse_passthrough(&self, passthrough: bool) {
        let state = self.borrow();
        if passthrough {
            let region = state
                .globals
                .compositor
                .create_region(&state.globals.qh, ());
            state.surface.set_input_region(Some(&region));
            region.destroy();
        } else {
            state.surface.set_input_region(None);
        }
        if !is_unconfigured_layer_shell(&state.role) {
            state.surface.commit();
        }
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

        self.borrow_mut().accesskit_adapter = Some(adapter);
    }

    #[cfg(feature = "accessibility")]
    fn a11y_tree_update(&self, tree_update: accesskit::TreeUpdate) {
        let mut state = self.borrow_mut();
        if let Some(adapter) = state.accesskit_adapter.as_mut() {
            adapter.update_if_active(|| tree_update);
        }
    }

    #[cfg(feature = "accessibility")]
    fn a11y_update_window_bounds(&self) {
        // Wayland does not expose absolute window positions.
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

fn is_unconfigured_layer_shell(role: &WaylandWindowRole) -> bool {
    matches!(
        role,
        WaylandWindowRole::LayerShell {
            configured: false,
            ..
        }
    )
}

fn update_window(mut state: RefMut<WaylandWindowState>) {
    let opaque = !state.is_transparent();

    state.renderer.update_transparency(!opaque);
    let mut opaque_area = state.window_bounds.map(|v| f32::from(v) as i32);
    opaque_area.inset(f32::from(state.inset()) as i32);

    let region = state
        .globals
        .compositor
        .create_region(&state.globals.qh, ());
    region.add(
        opaque_area.origin.x,
        opaque_area.origin.y,
        opaque_area.size.width,
        opaque_area.size.height,
    );

    // Note that rounded corners make this rectangle API hard to work with.
    // As this is common when using CSD, let's just disable this API.
    if state.background_appearance == WindowBackgroundAppearance::Opaque
        && state.decorations == WindowDecorations::Server
    {
        // Promise the compositor that this region of the window surface
        // contains no transparent pixels. This allows the compositor to skip
        // updating whatever is behind the surface for better performance.
        state.surface.set_opaque_region(Some(&region));
    } else {
        state.surface.set_opaque_region(None);
    }

    if let Some(ref blur_manager) = state.globals.blur_manager {
        if state.background_appearance == WindowBackgroundAppearance::Blurred {
            if state.blur.is_none() {
                let blur = blur_manager.create(&state.surface, &state.globals.qh, ());
                state.blur = Some(blur);
            }
            state.blur.as_ref().unwrap().commit();
        } else {
            // It probably doesn't hurt to clear the blur for opaque windows
            blur_manager.unset(&state.surface);
            if let Some(b) = state.blur.take() {
                b.release()
            }
        }
    }

    region.destroy();
}

fn window_decorations_to_xdg(decorations: WindowDecorations) -> zxdg_toplevel_decoration_v1::Mode {
    match decorations {
        WindowDecorations::Client => zxdg_toplevel_decoration_v1::Mode::ClientSide,
        WindowDecorations::Server => zxdg_toplevel_decoration_v1::Mode::ServerSide,
    }
}

fn resize_edge_to_xdg(edge: ResizeEdge) -> xdg_toplevel::ResizeEdge {
    match edge {
        ResizeEdge::Top => xdg_toplevel::ResizeEdge::Top,
        ResizeEdge::TopRight => xdg_toplevel::ResizeEdge::TopRight,
        ResizeEdge::Right => xdg_toplevel::ResizeEdge::Right,
        ResizeEdge::BottomRight => xdg_toplevel::ResizeEdge::BottomRight,
        ResizeEdge::Bottom => xdg_toplevel::ResizeEdge::Bottom,
        ResizeEdge::BottomLeft => xdg_toplevel::ResizeEdge::BottomLeft,
        ResizeEdge::Left => xdg_toplevel::ResizeEdge::Left,
        ResizeEdge::TopLeft => xdg_toplevel::ResizeEdge::TopLeft,
    }
}

/// The configuration event is in terms of the window geometry, which we are constantly
/// updating to account for the client decorations. But that's not the area we want to render
/// to, due to our intrusize CSD. So, here we calculate the 'actual' size, by adding back in the insets
fn compute_outer_size(
    inset: Pixels,
    new_size: Option<Size<Pixels>>,
    tiling: Tiling,
) -> Option<Size<Pixels>> {
    new_size.map(|mut new_size| {
        if !tiling.top {
            new_size.height += inset;
        }
        if !tiling.bottom {
            new_size.height += inset;
        }
        if !tiling.left {
            new_size.width += inset;
        }
        if !tiling.right {
            new_size.width += inset;
        }

        new_size
    })
}

fn inset_by_tiling(mut bounds: Bounds<Pixels>, inset: Pixels, tiling: Tiling) -> Bounds<Pixels> {
    if !tiling.top {
        bounds.origin.y += inset;
        bounds.size.height -= inset;
    }
    if !tiling.bottom {
        bounds.size.height -= inset;
    }
    if !tiling.left {
        bounds.origin.x += inset;
        bounds.size.width -= inset;
    }
    if !tiling.right {
        bounds.size.width -= inset;
    }

    bounds
}

#[cfg(test)]
mod layer_shell_tests {
    use super::*;

    #[test]
    fn layer_surface_size_preserves_zero_dimensions() {
        assert_eq!(
            layer_surface_request_size(size(px(0.0), px(240.0))),
            (0, 240)
        );
        assert_eq!(
            layer_surface_request_size(size(px(320.0), px(0.0))),
            (320, 0)
        );
        assert_eq!(layer_surface_request_size(size(px(0.0), px(0.0))), (0, 0));
    }

    #[test]
    fn layer_configure_size_keeps_current_zero_dimensions() {
        let current_size = size(px(320.0), px(240.0));

        assert_eq!(layer_configure_size(current_size, 0, 0), current_size);
        assert_eq!(
            layer_configure_size(current_size, 640, 0),
            size(px(640.0), px(240.0))
        );
        assert_eq!(
            layer_configure_size(current_size, 0, 480),
            size(px(320.0), px(480.0))
        );
        assert_eq!(
            layer_configure_size(current_size, 640, 480),
            size(px(640.0), px(480.0))
        );
    }

    #[test]
    fn layer_keyboard_interactivity_maps_to_wlr_protocol() {
        assert_eq!(
            wayland_keyboard_interactivity(KeyboardInteractivity::None),
            zwlr_layer_surface_v1::KeyboardInteractivity::None
        );
        assert_eq!(
            wayland_keyboard_interactivity(KeyboardInteractivity::OnDemand),
            zwlr_layer_surface_v1::KeyboardInteractivity::OnDemand
        );
        assert_eq!(
            wayland_keyboard_interactivity(KeyboardInteractivity::Exclusive),
            zwlr_layer_surface_v1::KeyboardInteractivity::Exclusive
        );
    }

    #[test]
    fn layer_values_map_to_wlr_protocol() {
        assert_eq!(
            wayland_layer(Layer::Background),
            zwlr_layer_shell_v1::Layer::Background
        );
        assert_eq!(
            wayland_layer(Layer::Bottom),
            zwlr_layer_shell_v1::Layer::Bottom
        );
        assert_eq!(wayland_layer(Layer::Top), zwlr_layer_shell_v1::Layer::Top);
        assert_eq!(
            wayland_layer(Layer::Overlay),
            zwlr_layer_shell_v1::Layer::Overlay
        );
    }

    #[test]
    fn layer_anchor_bits_map_to_wlr_protocol() {
        use gpui::layer_shell::Anchor;

        assert_eq!(
            wayland_anchor(Anchor::TOP),
            zwlr_layer_surface_v1::Anchor::Top
        );
        assert_eq!(
            wayland_anchor(Anchor::BOTTOM),
            zwlr_layer_surface_v1::Anchor::Bottom
        );
        assert_eq!(
            wayland_anchor(Anchor::LEFT),
            zwlr_layer_surface_v1::Anchor::Left
        );
        assert_eq!(
            wayland_anchor(Anchor::RIGHT),
            zwlr_layer_surface_v1::Anchor::Right
        );
        assert_eq!(
            wayland_anchor(Anchor::TOP | Anchor::BOTTOM | Anchor::LEFT | Anchor::RIGHT),
            zwlr_layer_surface_v1::Anchor::Top
                | zwlr_layer_surface_v1::Anchor::Bottom
                | zwlr_layer_surface_v1::Anchor::Left
                | zwlr_layer_surface_v1::Anchor::Right
        );
    }

    #[test]
    fn exclusive_edge_must_be_one_of_the_surface_anchors() {
        let anchor = Anchor::TOP | Anchor::LEFT;

        assert!(exclusive_edge_is_valid(anchor, Anchor::TOP));
        assert!(exclusive_edge_is_valid(anchor, Anchor::LEFT));
        assert!(!exclusive_edge_is_valid(anchor, Anchor::RIGHT));
        assert!(!exclusive_edge_is_valid(anchor, Anchor::empty()));
        assert!(!exclusive_edge_is_valid(anchor, Anchor::TOP | Anchor::LEFT));
    }

    #[test]
    fn exclusive_edge_requires_wlr_layer_shell_v5() {
        let required_version = zwlr_layer_surface_v1::REQ_SET_EXCLUSIVE_EDGE_SINCE;

        assert!(!layer_shell_supports_exclusive_edge(required_version - 1));
        assert!(layer_shell_supports_exclusive_edge(required_version));
        assert!(layer_shell_supports_exclusive_edge(required_version + 1));
    }
}

#[cfg(test)]
mod ime_tests {
    use super::required_ime_state_change;

    #[test]
    fn requests_initial_ime_state() {
        assert_eq!(required_ime_state_change(None, true), Some(true));
        assert_eq!(required_ime_state_change(None, false), Some(false));
    }

    #[test]
    fn requests_ime_state_switches() {
        assert_eq!(required_ime_state_change(Some(false), true), Some(true));
        assert_eq!(required_ime_state_change(Some(true), false), Some(false));
    }

    #[test]
    fn skips_redundant_ime_state_updates() {
        assert_eq!(required_ime_state_change(Some(true), true), None);
        assert_eq!(required_ime_state_change(Some(false), false), None);
    }
}

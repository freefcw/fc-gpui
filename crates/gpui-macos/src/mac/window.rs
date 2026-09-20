use super::{MacDisplay, display_id_for_ns_screen, renderer};
use crate::{
    AnyWindowHandle, Bounds, Capslock, CursorStyle, DevicePixels, ForegroundExecutor, KeyDownEvent,
    Keystroke, Modifiers, Pixels, PlatformAtlas, PlatformDisplay, PlatformInput,
    PlatformInputHandler, PlatformWindow, Point, PromptButton, PromptLevel, RequestFrameOptions,
    SharedString, Size, SystemWindowTab, WindowAppearance, WindowBackgroundAppearance,
    WindowBounds, WindowControlArea, WindowFrameSource, WindowKind, WindowParams,
    dispatch_get_main_queue, dispatch_sys::dispatch_async_f, point, px, size,
};
use block2::RcBlock;
use futures::channel::oneshot;
use objc2::encode::{Encode, Encoding, RefEncode};
use objc2::rc::Retained;
use objc2::runtime::{AnyObject, ProtocolObject};
use objc2::{
    AnyThread, ClassType, DefinedClass, MainThreadMarker, MainThreadOnly, Message, msg_send,
};
use objc2_app_kit::{
    NSAlert, NSAlertStyle, NSAppearanceCustomization, NSApplication,
    NSApplicationPresentationOptions, NSAutoresizingMaskOptions, NSBeep, NSButton as Objc2NSButton,
    NSColor, NSCursor, NSEvent, NSEventModifierFlags, NSNormalWindowLevel, NSPopUpMenuWindowLevel,
    NSProgressIndicator, NSProgressIndicatorStyle, NSScreen, NSStatusWindowLevel,
    NSTextInputContext, NSTrackingArea, NSTrackingAreaOptions, NSView,
    NSViewLayerContentsRedrawPolicy, NSWindow, NSWindowAnimationBehavior, NSWindowButton,
    NSWindowCollectionBehavior, NSWindowOcclusionState, NSWindowOrderingMode, NSWindowStyleMask,
    NSWindowTitleVisibility,
};
use objc2_foundation::{
    NSArray, NSAutoreleasePool, NSData, NSDictionary, NSKeyedArchiver, NSNumber, NSObjectProtocol,
    NSPoint as Objc2NSPoint, NSProcessInfo, NSRect as Objc2NSRect, NSSize as Objc2NSSize, NSString,
    NSUserDefaults,
};
use parking_lot::Mutex;
use raw_window_handle as rwh;
use std::{
    cell::Cell,
    ffi::c_void,
    ptr::NonNull,
    rc::Rc,
    sync::{Arc, Once, Weak},
};
use util::ResultExt;

type ObjcId = *mut AnyObject;

#[allow(non_camel_case_types)]
type id = ObjcId;

#[allow(non_camel_case_types)]
type NSUInteger = usize;

#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(super) struct NSPoint {
    pub x: f64,
    pub y: f64,
}

impl NSPoint {
    fn new(x: f64, y: f64) -> Self {
        Self { x, y }
    }
}

unsafe impl Encode for NSPoint {
    const ENCODING: Encoding = Encoding::Struct("CGPoint", &[f64::ENCODING, f64::ENCODING]);
}

unsafe impl RefEncode for NSPoint {
    const ENCODING_REF: Encoding = Encoding::Pointer(&Self::ENCODING);
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(super) struct NSSize {
    pub width: f64,
    pub height: f64,
}

impl NSSize {
    fn new(width: f64, height: f64) -> Self {
        Self { width, height }
    }
}

unsafe impl Encode for NSSize {
    const ENCODING: Encoding = Encoding::Struct("CGSize", &[f64::ENCODING, f64::ENCODING]);
}

unsafe impl RefEncode for NSSize {
    const ENCODING_REF: Encoding = Encoding::Pointer(&Self::ENCODING);
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(super) struct NSRect {
    pub origin: NSPoint,
    pub size: NSSize,
}

impl NSRect {
    fn new(origin: NSPoint, size: NSSize) -> Self {
        Self { origin, size }
    }
}

unsafe impl Encode for NSRect {
    const ENCODING: Encoding = Encoding::Struct("CGRect", &[NSPoint::ENCODING, NSSize::ENCODING]);
}

unsafe impl RefEncode for NSRect {
    const ENCODING_REF: Encoding = Encoding::Pointer(&Self::ENCODING);
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(crate) struct NSOperatingSystemVersion {
    pub major_version: NSUInteger,
    pub minor_version: NSUInteger,
    pub patch_version: NSUInteger,
}

impl NSOperatingSystemVersion {
    pub(crate) fn new(
        major_version: NSUInteger,
        minor_version: NSUInteger,
        patch_version: NSUInteger,
    ) -> Self {
        Self {
            major_version,
            minor_version,
            patch_version,
        }
    }
}

impl From<NSSize> for Size<Pixels> {
    fn from(value: NSSize) -> Self {
        Size {
            width: px(value.width as f32),
            height: px(value.height as f32),
        }
    }
}

impl From<NSRect> for Size<Pixels> {
    fn from(rect: NSRect) -> Self {
        Size::<Pixels>::from(rect.size)
    }
}

static RESTORES_WORKSPACE_AT_LAUNCH_DEFAULT: Once = Once::new();

#[path = "window_classes.rs"]
mod window_classes;
use window_classes::{
    BlurredView, GPUIPanel, GPUIView, GPUIWindow, GPUIWindowStateArchiverDelegate,
    GPUIWindowStateKeyedUnarchiver, assign_panel_state, assign_view_state, assign_window_state,
    gpui_window_from_ns, retain_window_state,
};

#[derive(PartialEq)]
pub enum UserTabbingPreference {
    Never,
    Always,
    InFullScreen,
}

pub(super) fn from_objc_rect(rect: Objc2NSRect) -> NSRect {
    NSRect::new(
        NSPoint::new(rect.origin.x, rect.origin.y),
        NSSize::new(rect.size.width, rect.size.height),
    )
}

fn to_objc_rect(rect: NSRect) -> Objc2NSRect {
    Objc2NSRect::new(
        Objc2NSPoint::new(rect.origin.x, rect.origin.y),
        Objc2NSSize::new(rect.size.width, rect.size.height),
    )
}

fn to_objc_point(point: NSPoint) -> Objc2NSPoint {
    Objc2NSPoint::new(point.x, point.y)
}

fn from_objc_point(point: Objc2NSPoint) -> NSPoint {
    NSPoint::new(point.x, point.y)
}

fn native_window_ptr(window: &NSWindow) -> *mut NSWindow {
    window as *const NSWindow as *mut NSWindow
}

unsafe fn native_window_from_ptr<'a>(ptr: *mut NSWindow) -> &'a NSWindow {
    unsafe { &*ptr }
}

fn ns_window_from_created(created: &CreatedWindow) -> Retained<NSWindow> {
    match created {
        CreatedWindow::Window(window) => Retained::into_super(window.retain()),
        CreatedWindow::Panel(panel) => Retained::into_super(Retained::into_super(panel.retain())),
    }
}

fn set_tabbing_identifier(window: &NSWindow, tabbing_identifier: Option<&str>) {
    if let Some(tabbing_identifier) = tabbing_identifier {
        window.setTabbingIdentifier(&NSString::from_str(tabbing_identifier));
    } else {
        // Typed setter does not accept nil; AppKit treats a nil identifier as "no grouping".
        let _: () = unsafe { msg_send![window, setTabbingIdentifier: None::<&NSString>] };
    }
}

fn as_ns_view<'a>(view: NonNull<AnyObject>) -> &'a NSView {
    unsafe { view.cast::<NSView>().as_ref() }
}

fn as_any_object(object: &impl AsRef<AnyObject>) -> &AnyObject {
    object.as_ref()
}

fn add_popup_tracking_area(view: &NSView) {
    let tracking_area = unsafe {
        NSTrackingArea::initWithRect_options_owner_userInfo(
            NSTrackingArea::alloc(),
            Objc2NSRect::new(Objc2NSPoint::new(0.0, 0.0), Objc2NSSize::new(0.0, 0.0)),
            NSTrackingAreaOptions::MouseEnteredAndExited
                | NSTrackingAreaOptions::MouseMoved
                | NSTrackingAreaOptions::ActiveAlways
                | NSTrackingAreaOptions::InVisibleRect,
            Some(as_any_object(view)),
            None,
        )
    };
    view.addTrackingArea(&tracking_area);
}

fn global_domain_string(key: &str) -> String {
    let defaults = NSUserDefaults::standardUserDefaults();
    let domain = NSString::from_str("NSGlobalDomain");
    let key = NSString::from_str(key);
    defaults
        .persistentDomainForName(&domain)
        .and_then(|dict| dict.objectForKey(&key))
        .and_then(|value| value.downcast::<NSString>().ok())
        .map(|value| value.to_string())
        .unwrap_or_default()
}

fn ns_event_modifier_flags() -> NSEventModifierFlags {
    // `+[NSEvent modifierFlags]` collides with the instance method of the same
    // name, so objc2 does not expose a unique typed class method here.
    unsafe { msg_send![NSEvent::class(), modifierFlags] }
}

enum CreatedWindow {
    Window(Retained<GPUIWindow>),
    Panel(Retained<GPUIPanel>),
}

fn convert_mouse_position(position: NSPoint, window_height: Pixels) -> Point<Pixels> {
    point(
        px(position.x as f32),
        // macOS screen coordinates are relative to bottom left
        window_height - px(position.y as f32),
    )
}

// State captured when entering simple (borderless) fullscreen, used to restore
// the window on exit.
struct SimpleFullscreenState {
    frame: Objc2NSRect,
    bounds: Bounds<Pixels>,
    style_mask: NSWindowStyleMask,
}

enum SimpleFullscreenPlan {
    Enter { screen_frame: Objc2NSRect },
    Exit(SimpleFullscreenState),
}

struct SimpleFullscreenAppState {
    window_count: usize,
    saved_presentation_options: NSApplicationPresentationOptions,
}

static SIMPLE_FULLSCREEN_APP_STATE: Mutex<Option<SimpleFullscreenAppState>> = Mutex::new(None);

fn shared_ns_application() -> Retained<NSApplication> {
    unsafe { NSApplication::sharedApplication(MainThreadMarker::new_unchecked()) }
}

fn push_simple_fullscreen_presentation_options() {
    let mut app_state = SIMPLE_FULLSCREEN_APP_STATE.lock();
    match app_state.as_mut() {
        Some(app_state) => app_state.window_count += 1,
        None => {
            let app = shared_ns_application();
            let saved_presentation_options = app.presentationOptions();
            app.setPresentationOptions(
                NSApplicationPresentationOptions::AutoHideDock
                    | NSApplicationPresentationOptions::AutoHideMenuBar,
            );
            *app_state = Some(SimpleFullscreenAppState {
                window_count: 1,
                saved_presentation_options,
            });
        }
    }
}

fn pop_simple_fullscreen_presentation_options() {
    let mut app_state = SIMPLE_FULLSCREEN_APP_STATE.lock();
    if let Some(state) = app_state.as_mut() {
        state.window_count = state.window_count.saturating_sub(1);
        if state.window_count == 0 {
            let app = shared_ns_application();
            app.setPresentationOptions(state.saved_presentation_options);
            *app_state = None;
        }
    }
}

fn apply_simple_fullscreen_plan(
    native_window: &NSWindow,
    native_view: &NSView,
    plan: SimpleFullscreenPlan,
) {
    match plan {
        SimpleFullscreenPlan::Exit(saved) => {
            pop_simple_fullscreen_presentation_options();
            native_window.setStyleMask(saved.style_mask);
            native_window.setFrame_display(saved.frame, true);
        }
        SimpleFullscreenPlan::Enter { screen_frame } => {
            push_simple_fullscreen_presentation_options();
            native_window.setStyleMask(NSWindowStyleMask::Borderless);
            native_window.setFrame_display(screen_frame, true);
        }
    }

    // Changing the style mask makes AppKit resign the window's key status and
    // first responder, so keyboard input stops reaching the editor. Re-make the
    // window key and restore the GPUI view as first responder.
    native_window.makeKeyAndOrderFront(None);
    let _: bool = native_window.makeFirstResponder(Some(native_view));
}

struct MacWindowState {
    self_ref: Weak<Mutex<MacWindowState>>,
    handle: AnyWindowHandle,
    executor: ForegroundExecutor,
    /// Taken on `MacWindow` drop so the window-ivar `Arc` cycle can unwind after `close`.
    native_window: Option<Retained<NSWindow>>,
    native_view: NonNull<AnyObject>,
    blurred_view: Option<id>,
    cursor_style: CursorStyle,
    cursor_hidden: bool,
    frame_source: Option<WindowFrameSource>,
    renderer: renderer::Renderer,
    request_frame_callback: Option<Box<dyn FnMut(RequestFrameOptions)>>,
    event_callback: Option<Box<dyn FnMut(PlatformInput) -> crate::DispatchEventResult>>,
    activate_callback: Option<Box<dyn FnMut(bool)>>,
    resize_callback: Option<Box<dyn FnMut(Size<Pixels>, f32)>>,
    moved_callback: Option<Box<dyn FnMut()>>,
    should_close_callback: Option<Box<dyn FnMut() -> bool>>,
    close_callback: Option<Box<dyn FnOnce()>>,
    appearance_changed_callback: Option<Box<dyn FnMut()>>,
    input_handler: Option<PlatformInputHandler>,
    last_key_equivalent: Option<KeyDownEvent>,
    synthetic_drag_counter: usize,
    traffic_light_position: Option<Point<Pixels>>,
    transparent_titlebar: bool,
    previous_modifiers_changed_event: Option<PlatformInput>,
    keystroke_for_do_command: Option<Keystroke>,
    do_command_handled: Option<bool>,
    external_files_dragged: bool,
    // Whether the next left-mouse click is also the focusing click.
    first_mouse: bool,
    app_owns_titlebar_drag: bool,
    fullscreen_restore_bounds: Bounds<Pixels>,
    simple_fullscreen_state: Option<SimpleFullscreenState>,
    move_tab_to_new_window_callback: Option<Box<dyn FnMut()>>,
    merge_all_windows_callback: Option<Box<dyn FnMut()>>,
    select_next_tab_callback: Option<Box<dyn FnMut()>>,
    select_previous_tab_callback: Option<Box<dyn FnMut()>>,
    toggle_tab_bar_callback: Option<Box<dyn FnMut()>>,
    activated_least_once: bool,
    is_closing: bool,
    #[cfg(feature = "accessibility")]
    accesskit_adapter: Option<accesskit_macos::SubclassingAdapter>,
}

struct FrameRequestContext {
    window_state: Weak<Mutex<MacWindowState>>,
}

unsafe extern "C" fn drop_frame_request_context(context: *mut c_void) {
    unsafe {
        drop(Box::from_raw(context as *mut FrameRequestContext));
    }
}

impl MacWindowState {
    fn native_window(&self) -> &NSWindow {
        self.native_window
            .as_deref()
            .expect("MacWindowState.native_window already taken")
    }

    fn native_window_ptr(&self) -> *mut NSWindow {
        native_window_ptr(self.native_window())
    }

    fn begin_close(&mut self) {
        self.is_closing = true;
        self.request_frame_callback.take();
        self.stop_display_link();
        // Drop clears the AppKit delegate before asynchronously sending `close`, so
        // `close_window` may never run. Release here so both explicit close and Rust
        // Drop restore presentation options. `take()` makes a second pop a no-op.
        if self.simple_fullscreen_state.take().is_some() {
            pop_simple_fullscreen_presentation_options();
        }
    }

    fn move_traffic_light(&self) {
        if let Some(traffic_light_position) = self.traffic_light_position {
            if self.is_fullscreen() {
                // Moving traffic lights while fullscreen doesn't work,
                // see https://github.com/zed-industries/zed/issues/4712
                return;
            }

            let titlebar_height = self.titlebar_height();
            let window = self.native_window();
            let Some(close_button) = window.standardWindowButton(NSWindowButton::CloseButton)
            else {
                return;
            };
            let Some(min_button) = window.standardWindowButton(NSWindowButton::MiniaturizeButton)
            else {
                return;
            };
            let Some(zoom_button) = window.standardWindowButton(NSWindowButton::ZoomButton) else {
                return;
            };

            let mut close_button_frame = from_objc_rect(close_button.frame());
            let mut min_button_frame = from_objc_rect(min_button.frame());
            let mut zoom_button_frame = from_objc_rect(zoom_button.frame());
            let mut origin = point(
                traffic_light_position.x,
                titlebar_height
                    - traffic_light_position.y
                    - px(close_button_frame.size.height as f32),
            );
            let button_spacing =
                px((min_button_frame.origin.x - close_button_frame.origin.x) as f32);

            close_button_frame.origin = NSPoint::new(origin.x.into(), origin.y.into());
            close_button.setFrame(to_objc_rect(close_button_frame));
            origin.x += button_spacing;

            min_button_frame.origin = NSPoint::new(origin.x.into(), origin.y.into());
            min_button.setFrame(to_objc_rect(min_button_frame));
            origin.x += button_spacing;

            zoom_button_frame.origin = NSPoint::new(origin.x.into(), origin.y.into());
            zoom_button.setFrame(to_objc_rect(zoom_button_frame));
        }
    }

    fn start_display_link(&mut self) {
        self.stop_display_link();
        if self.is_closing || self.request_frame_callback.is_none() {
            return;
        }
        if !self
            .native_window()
            .occlusionState()
            .contains(NSWindowOcclusionState::Visible)
        {
            return;
        }
        let Some(screen) = self.native_window().screen() else {
            // AppKit can temporarily report no screen while displays are being reconfigured.
            return;
        };
        let display_id = display_id_for_ns_screen(&screen);

        self.frame_source
            .get_or_insert_with(|| {
                let context = Box::new(FrameRequestContext {
                    window_state: self.self_ref.clone(),
                });
                WindowFrameSource::new(
                    Box::into_raw(context).cast::<c_void>(),
                    step,
                    drop_frame_request_context,
                )
            })
            .start(display_id.index() as u32)
            .log_err();
    }

    fn stop_display_link(&mut self) {
        if let Some(frame_source) = self.frame_source.as_mut() {
            frame_source.stop();
        }
    }

    fn is_maximized(&self) -> bool {
        let bounds = self.bounds();
        let Some(screen) = self.native_window().screen() else {
            return false;
        };
        let screen_size: Size<Pixels> = from_objc_rect(screen.visibleFrame()).into();
        bounds.size == screen_size
    }

    fn is_fullscreen(&self) -> bool {
        self.native_window()
            .styleMask()
            .contains(NSWindowStyleMask::FullScreen)
    }

    fn toggle_simple_fullscreen(&mut self) -> Option<SimpleFullscreenPlan> {
        // If the window is in native fullscreen, simple fullscreen would conflict
        // with AppKit's own fullscreen handling, so ignore the request.
        if self.is_fullscreen() {
            return None;
        }

        if let Some(saved) = self.simple_fullscreen_state.take() {
            Some(SimpleFullscreenPlan::Exit(saved))
        } else {
            let screen = self.native_window().screen()?;
            let screen_frame = screen.frame();
            let bounds = self.bounds();

            self.simple_fullscreen_state = Some(SimpleFullscreenState {
                frame: self.native_window().frame(),
                bounds,
                style_mask: self.native_window().styleMask(),
            });

            Some(SimpleFullscreenPlan::Enter { screen_frame })
        }
    }

    fn bounds(&self) -> Bounds<Pixels> {
        let mut window_frame = from_objc_rect(self.native_window().frame());
        let Some(screen) = self.native_window().screen() else {
            return Bounds::new(point(px(0.), px(0.)), crate::DEFAULT_WINDOW_SIZE);
        };
        let screen_frame = from_objc_rect(screen.frame());

        // Flip the y coordinate to be top-left origin
        window_frame.origin.y =
            screen_frame.size.height - window_frame.origin.y - window_frame.size.height;

        Bounds::new(
            point(
                px((window_frame.origin.x - screen_frame.origin.x) as f32),
                px((window_frame.origin.y + screen_frame.origin.y) as f32),
            ),
            size(
                px(window_frame.size.width as f32),
                px(window_frame.size.height as f32),
            ),
        )
    }

    fn content_size(&self) -> Size<Pixels> {
        let Some(content_view) = self.native_window().contentView() else {
            return size(px(0.), px(0.));
        };
        let frame = from_objc_rect(content_view.frame());
        size(px(frame.size.width as f32), px(frame.size.height as f32))
    }

    fn scale_factor(&self) -> f32 {
        get_scale_factor(self.native_window())
    }

    fn titlebar_height(&self) -> Pixels {
        let frame = self.native_window().frame();
        let content_layout_rect = self.native_window().contentLayoutRect();
        px((frame.size.height - content_layout_rect.size.height) as f32)
    }

    fn window_bounds(&self) -> WindowBounds {
        if self.is_fullscreen() {
            WindowBounds::Fullscreen(self.fullscreen_restore_bounds)
        } else if let Some(state) = &self.simple_fullscreen_state {
            WindowBounds::Windowed(state.bounds)
        } else {
            WindowBounds::Windowed(self.bounds())
        }
    }
}

unsafe impl Send for MacWindowState {}

pub(crate) struct MacWindow(Arc<Mutex<MacWindowState>>, MainThreadMarker);

impl MacWindow {
    pub fn open(
        handle: AnyWindowHandle,
        WindowParams {
            bounds,
            titlebar,
            kind,
            is_movable,
            app_owns_titlebar_drag,
            is_resizable,
            is_minimizable,
            focus,
            show,
            display_id,
            window_min_size,
            tabbing_identifier,
            mouse_passthrough,
            icon: _,
            app_id: _app_id,
        }: WindowParams,
        executor: ForegroundExecutor,
        renderer_context: renderer::Context,
        atlas_initial_size: Size<DevicePixels>,
        marker: MainThreadMarker,
    ) -> Self {
        unsafe {
            let pool = NSAutoreleasePool::new();

            let allows_automatic_window_tabbing = tabbing_identifier.is_some();
            NSWindow::setAllowsAutomaticWindowTabbing(allows_automatic_window_tabbing, marker);

            let mut style_mask;
            if let Some(titlebar) = titlebar.as_ref() {
                style_mask = NSWindowStyleMask::Closable | NSWindowStyleMask::Titled;

                if is_resizable {
                    style_mask |= NSWindowStyleMask::Resizable;
                }

                if is_minimizable {
                    style_mask |= NSWindowStyleMask::Miniaturizable;
                }

                if titlebar.appears_transparent {
                    style_mask |= NSWindowStyleMask::FullSizeContentView;
                }
            } else {
                style_mask = NSWindowStyleMask::Titled | NSWindowStyleMask::FullSizeContentView;
            }

            let is_panel = matches!(&kind, WindowKind::PopUp | WindowKind::Overlay);
            if is_panel {
                style_mask |= NSWindowStyleMask::NonactivatingPanel;
            }

            let display = display_id
                .and_then(MacDisplay::find_by_id)
                .unwrap_or_else(MacDisplay::primary);

            let screens = NSScreen::screens(marker);
            let mut target_screen: Option<Retained<NSScreen>> = None;
            let mut selected_screen_frame = None;

            for i in 0..screens.len() {
                let screen = screens.objectAtIndex(i);
                let display_id = display_id_for_ns_screen(&screen);
                let frame = from_objc_rect(screen.frame());
                if display_id == display.id() {
                    selected_screen_frame = Some(frame);
                    target_screen = Some(screen);
                }
            }

            let screen_frame = selected_screen_frame.unwrap_or_else(|| {
                let screen = NSScreen::mainScreen(marker);
                target_screen = screen.clone();
                screen
                    .as_ref()
                    .map(|screen| from_objc_rect(screen.frame()))
                    .unwrap_or_default()
            });

            let window_rect = NSRect::new(
                NSPoint::new(
                    screen_frame.origin.x + f64::from(bounds.origin.x),
                    screen_frame.origin.y
                        + f64::from(display.bounds().size.height - bounds.origin.y),
                ),
                NSSize::new(f64::from(bounds.size.width), f64::from(bounds.size.height)),
            );

            let screen = target_screen.as_deref();
            let content_rect = to_objc_rect(window_rect);
            let created = if is_panel {
                CreatedWindow::Panel(GPUIPanel::new(marker, content_rect, style_mask, screen))
            } else {
                CreatedWindow::Window(GPUIWindow::new(marker, content_rect, style_mask, screen))
            };
            let native_window = ns_window_from_created(&created);
            let filename_type = NSString::from_str("NSFilenamesPboardType");
            native_window.registerForDraggedTypes(&NSArray::from_retained_slice(&[filename_type]));
            native_window.setReleasedWhenClosed(false);

            let content_view = native_window
                .contentView()
                .expect("NSWindow contentView after init");
            let native_view_retained = GPUIView::with_frame(marker, content_view.bounds());
            let native_view_ptr = Retained::as_ptr(&native_view_retained) as *mut AnyObject;
            assert!(!native_view_ptr.is_null());

            let mut window = Self(
                Arc::new_cyclic(|self_ref| {
                    Mutex::new(MacWindowState {
                        self_ref: self_ref.clone(),
                        handle,
                        executor,
                        native_window: Some(native_window.retain()),
                        native_view: NonNull::new_unchecked(native_view_ptr),
                        blurred_view: None,
                        cursor_style: CursorStyle::Arrow,
                        cursor_hidden: false,
                        frame_source: None,
                        renderer: renderer::new_renderer(
                            renderer_context,
                            Retained::as_ptr(&native_window) as *mut _,
                            native_view_ptr as *mut _,
                            bounds.size.map(f32::from),
                            false,
                            atlas_initial_size,
                        ),
                        request_frame_callback: None,
                        event_callback: None,
                        activate_callback: None,
                        resize_callback: None,
                        moved_callback: None,
                        should_close_callback: None,
                        close_callback: None,
                        appearance_changed_callback: None,
                        input_handler: None,
                        last_key_equivalent: None,
                        synthetic_drag_counter: 0,
                        traffic_light_position: titlebar
                            .as_ref()
                            .and_then(|titlebar| titlebar.traffic_light_position),
                        transparent_titlebar: titlebar
                            .as_ref()
                            .is_none_or(|titlebar| titlebar.appears_transparent),
                        previous_modifiers_changed_event: None,
                        keystroke_for_do_command: None,
                        do_command_handled: None,
                        external_files_dragged: false,
                        first_mouse: false,
                        app_owns_titlebar_drag,
                        fullscreen_restore_bounds: Bounds::default(),
                        simple_fullscreen_state: None,
                        move_tab_to_new_window_callback: None,
                        merge_all_windows_callback: None,
                        select_next_tab_callback: None,
                        select_previous_tab_callback: None,
                        toggle_tab_bar_callback: None,
                        activated_least_once: false,
                        is_closing: false,
                        #[cfg(feature = "accessibility")]
                        accesskit_adapter: None,
                    })
                }),
                marker,
            );

            match &created {
                CreatedWindow::Window(gpui_window) => {
                    assign_window_state(gpui_window, &window.0);
                    gpui_window.setDelegate(Some(ProtocolObject::from_ref(&**gpui_window)));
                }
                CreatedWindow::Panel(gpui_panel) => {
                    assign_panel_state(gpui_panel, &window.0);
                    gpui_panel.setDelegate(Some(ProtocolObject::from_ref(&**gpui_panel)));
                }
            }
            assign_view_state(&native_view_retained, &window.0);

            if let Some(title) = titlebar
                .as_ref()
                .and_then(|t| t.title.as_ref().map(AsRef::as_ref))
            {
                window.set_title(title);
            }

            native_window.setMovable(is_movable);

            if let Some(window_min_size) = window_min_size {
                native_window.setContentMinSize(Objc2NSSize::new(
                    window_min_size.width.to_f64(),
                    window_min_size.height.to_f64(),
                ));
            }

            if titlebar.is_none_or(|titlebar| titlebar.appears_transparent) {
                native_window.setTitlebarAppearsTransparent(true);
                native_window.setTitleVisibility(NSWindowTitleVisibility::Hidden);
            }

            native_view_retained.setAutoresizingMask(
                NSAutoresizingMaskOptions::ViewWidthSizable
                    | NSAutoresizingMaskOptions::ViewHeightSizable,
            );
            // Deprecated OpenGL setter; objc2-app-kit 0.3.2 has no typed binding.
            let _: () = msg_send![
                &*native_view_retained,
                setWantsBestResolutionOpenGLSurface: true
            ];

            // From winit crate: On Mojave, views automatically become layer-backed shortly after
            // being added to a native_window. Changing the layer-backedness of a view breaks the
            // association between the view and its associated OpenGL context. To work around this,
            // on we explicitly make the view layer-backed up front so that AppKit doesn't do it
            // itself and break the association with its context.
            native_view_retained.setWantsLayer(true);
            native_view_retained
                .setLayerContentsRedrawPolicy(NSViewLayerContentsRedrawPolicy::DuringViewResize);

            content_view.addSubview(&native_view_retained);
            let _: bool = native_window.makeFirstResponder(Some(&*native_view_retained));

            match &kind {
                WindowKind::Normal | WindowKind::Floating => {
                    native_window.setLevel(NSNormalWindowLevel);
                    native_window.setAcceptsMouseMovedEvents(true);
                    set_tabbing_identifier(native_window.as_ref(), tabbing_identifier.as_deref());
                }
                WindowKind::PopUp => {
                    // Use a tracking area to allow receiving MouseMoved events even when
                    // the window or application aren't active, which is often the case
                    // e.g. for notification windows.
                    add_popup_tracking_area(&native_view_retained);

                    native_window.setLevel(NSPopUpMenuWindowLevel);
                    native_window.setAnimationBehavior(NSWindowAnimationBehavior::UtilityWindow);
                    native_window.setCollectionBehavior(
                        NSWindowCollectionBehavior::CanJoinAllSpaces
                            | NSWindowCollectionBehavior::FullScreenAuxiliary,
                    );
                }
                WindowKind::Overlay => {
                    add_popup_tracking_area(&native_view_retained);

                    native_window.setLevel(NSStatusWindowLevel);
                    native_window.setAnimationBehavior(NSWindowAnimationBehavior::UtilityWindow);
                    native_window.setCollectionBehavior(
                        NSWindowCollectionBehavior::CanJoinAllSpaces
                            | NSWindowCollectionBehavior::Stationary
                            | NSWindowCollectionBehavior::FullScreenAuxiliary,
                    );
                }
            }

            drop(native_view_retained);

            let app = shared_ns_application();
            if allows_automatic_window_tabbing
                && let Some(main_window) = app.mainWindow()
                && !std::ptr::eq(
                    Retained::as_ptr(&main_window),
                    Retained::as_ptr(&native_window),
                )
            {
                let main_window_is_fullscreen = main_window
                    .styleMask()
                    .contains(NSWindowStyleMask::FullScreen);
                let user_tabbing_preference = Self::get_user_tabbing_preference()
                    .unwrap_or(UserTabbingPreference::InFullScreen);
                let should_add_as_tab = user_tabbing_preference == UserTabbingPreference::Always
                    || user_tabbing_preference == UserTabbingPreference::InFullScreen
                        && main_window_is_fullscreen;

                if should_add_as_tab {
                    let main_window_can_tab =
                        main_window.respondsToSelector(objc2::sel!(addTabbedWindow:ordered:));
                    let main_window_visible = main_window.isVisible();

                    if main_window_can_tab && main_window_visible {
                        main_window
                            .addTabbedWindow_ordered(&native_window, NSWindowOrderingMode::Above);

                        // Ensure the window is visible immediately after adding the tab, since the tab bar is updated with a new entry at this point.
                        // Note: Calling orderFront here can break fullscreen mode (makes fullscreen windows exit fullscreen), so only do this if the main window is not fullscreen.
                        if !main_window_is_fullscreen {
                            native_window.orderFront(None);
                        }
                    }
                }
            }

            if mouse_passthrough {
                native_window.setIgnoresMouseEvents(true);
            }

            if focus && show {
                native_window.makeKeyAndOrderFront(None);
            } else if show {
                native_window.orderFront(None);
            }

            // Set the initial position of the window to the specified origin.
            // Although we already specified the position using `initWithContentRect_styleMask_backing_defer_screen_`,
            // the window position might be incorrect if the main screen (the screen that contains the window that has focus)
            //  is different from the primary screen.
            native_window.setFrameTopLeftPoint(to_objc_point(window_rect.origin));
            window.0.lock().move_traffic_light();

            pool.drain();

            window
        }
    }

    pub fn active_window() -> Option<AnyWindowHandle> {
        let app = shared_ns_application();
        let main_window = app.mainWindow()?;
        let window = gpui_window_from_ns(&main_window)?;
        let handle = retain_window_state(window.ivars().state.get())
            .lock()
            .handle;
        Some(handle)
    }

    pub fn ordered_windows() -> Vec<AnyWindowHandle> {
        let app = shared_ns_application();
        let mut window_handles = Vec::new();
        for window in app.orderedWindows().iter() {
            if let Some(window) = gpui_window_from_ns(&window) {
                let handle = retain_window_state(window.ivars().state.get())
                    .lock()
                    .handle;
                window_handles.push(handle);
            }
        }
        window_handles
    }

    pub fn get_user_tabbing_preference() -> Option<UserTabbingPreference> {
        match global_domain_string("AppleWindowTabbingMode").as_str() {
            "manual" => Some(UserTabbingPreference::Never),
            "always" => Some(UserTabbingPreference::Always),
            _ => Some(UserTabbingPreference::InFullScreen),
        }
    }
}

impl Drop for MacWindow {
    fn drop(&mut self) {
        let mut this = self.0.lock();
        // Must run before `setDelegate: nil` so simple-fullscreen presentation
        // options are popped even when the later async `close` skips `close_window`.
        this.begin_close();
        this.frame_source.take();
        this.renderer.destroy();
        let window = this
            .native_window
            .take()
            .expect("MacWindowState.native_window already taken");
        window.setDelegate(None);
        this.input_handler.take();
        let window_ptr = Retained::into_raw(window);
        this.executor
            .spawn(async move {
                unsafe {
                    let window = native_window_from_ptr(window_ptr);
                    window.close();
                    let _: () = msg_send![window_ptr, autorelease];
                }
            })
            .detach();
    }
}

impl PlatformWindow for MacWindow {
    fn bounds(&self) -> Bounds<Pixels> {
        self.0.as_ref().lock().bounds()
    }

    fn window_bounds(&self) -> WindowBounds {
        self.0.as_ref().lock().window_bounds()
    }

    fn is_maximized(&self) -> bool {
        self.0.as_ref().lock().is_maximized()
    }

    fn content_size(&self) -> Size<Pixels> {
        self.0.as_ref().lock().content_size()
    }

    fn resize(&mut self, size: Size<Pixels>) {
        let this = self.0.lock();
        let window = this.native_window_ptr();
        this.executor
            .spawn(async move {
                unsafe {
                    native_window_from_ptr(window).setContentSize(Objc2NSSize::new(
                        f64::from(size.width),
                        f64::from(size.height),
                    ));
                }
            })
            .detach();
    }

    fn merge_all_windows(&self) {
        let native_window = self.0.lock().native_window_ptr();
        unsafe extern "C" fn merge_windows_async(context: *mut std::ffi::c_void) {
            unsafe {
                native_window_from_ptr(context.cast()).mergeAllWindows(None);
            }
        }

        unsafe {
            dispatch_async_f(
                dispatch_get_main_queue(),
                native_window.cast(),
                Some(merge_windows_async),
            );
        }
    }

    fn move_tab_to_new_window(&self) {
        let native_window = self.0.lock().native_window_ptr();
        unsafe extern "C" fn move_tab_async(context: *mut std::ffi::c_void) {
            unsafe {
                let native_window = native_window_from_ptr(context.cast());
                native_window.moveTabToNewWindow(None);
                native_window.makeKeyAndOrderFront(None);
            }
        }

        unsafe {
            dispatch_async_f(
                dispatch_get_main_queue(),
                native_window.cast(),
                Some(move_tab_async),
            );
        }
    }

    fn toggle_window_tab_overview(&self) {
        self.0.lock().native_window().toggleTabOverview(None);
    }

    fn set_tabbing_identifier(&self, tabbing_identifier: Option<String>) {
        let this = self.0.lock();
        NSWindow::setAllowsAutomaticWindowTabbing(tabbing_identifier.is_some(), self.1);
        set_tabbing_identifier(this.native_window(), tabbing_identifier.as_deref());
    }

    fn native_window_state(&self) -> Option<Vec<u8>> {
        let native_window = {
            let state = self.0.lock();
            if state.is_fullscreen() || state.simple_fullscreen_state.is_some() {
                return None;
            }
            state.native_window().retain()
        };
        // SAFETY: `native_window` is a live `NSWindow` retained by this window's state, and the
        // selectors below are AppKit/Foundation methods sent with their documented signatures. The
        // archived bytes are copied into an owned `Vec` before the objects we allocated are
        // released, so no pointer into Objective-C memory escapes this block.
        unsafe {
            let archiver =
                NSKeyedArchiver::initRequiringSecureCoding(NSKeyedArchiver::alloc(), true);
            let delegate = GPUIWindowStateArchiverDelegate::new();
            archiver.setDelegate(Some(ProtocolObject::from_ref(&*delegate)));
            native_window.encodeRestorableStateWithCoder(archiver.as_super());
            archiver.finishEncoding();
            // The archiver holds a weak reference to its delegate; clear it before the delegate
            // is released below.
            archiver.setDelegate(None);

            let data = archiver.encodedData();
            let bytes = data.to_vec();
            if bytes.is_empty() {
                log::warn!("the archiver produced no data for the native window state");
                None
            } else {
                Some(bytes)
            }
        }
    }

    fn restore_native_window_state(&self, state: &[u8]) {
        if state.is_empty() {
            return;
        }
        let native_window = self.0.lock().native_window().retain();
        // SAFETY: `native_window` is a live `NSWindow` retained by this window's state. The NSData,
        // NSKeyedUnarchiver and `restoreStateWithCoder:` selectors are sent with their documented
        // signatures, and the `NSData` only borrows `state` for the duration of this synchronous
        // call (it is consumed before `state` could be freed).
        unsafe {
            // On macOS < 15 the `NSWindowRestoresWorkspaceAtLaunch` user default controls whether
            // the window is restored to its original Space. On macOS 15+ that default is broken
            // (FB15644170), and the `_windowRestorationOptions` override on our unarchiver subclass
            // handles it instead.
            if !is_macos_version_at_least(NSOperatingSystemVersion::new(15, 0, 0)) {
                RESTORES_WORKSPACE_AT_LAUNCH_DEFAULT.call_once(|| {
                    let defaults = NSUserDefaults::standardUserDefaults();
                    let key = NSString::from_str("NSWindowRestoresWorkspaceAtLaunch");
                    let yes_value = NSNumber::numberWithBool(true);
                    let yes_obj: &AnyObject = yes_value.as_ref();
                    let dict =
                        NSDictionary::<NSString, AnyObject>::from_slices(&[&*key], &[yes_obj]);
                    defaults.registerDefaults(&dict);
                });
            }

            let ns_data = NSData::with_bytes(state);
            let unarchiver = match GPUIWindowStateKeyedUnarchiver::for_reading_from_data(&ns_data) {
                Ok(unarchiver) => unarchiver,
                Err(error) => {
                    log::warn!(
                        "failed to unarchive the native window state: {}",
                        error.localizedDescription()
                    );
                    return;
                }
            };
            native_window.restoreStateWithCoder(unarchiver.as_super());
            if let Some(error) = unarchiver.error() {
                log::warn!(
                    "failed to restore the native window state: {}",
                    error.localizedDescription()
                );
            }
        }
    }

    fn scale_factor(&self) -> f32 {
        self.0.as_ref().lock().scale_factor()
    }

    fn appearance(&self) -> WindowAppearance {
        super::window_appearance::from_ns_appearance(
            &self.0.lock().native_window().effectiveAppearance(),
        )
    }

    fn display(&self) -> Option<Rc<dyn PlatformDisplay>> {
        let screen = self.0.lock().native_window().screen()?;
        Some(Rc::new(MacDisplay(
            display_id_for_ns_screen(&screen).index() as u32,
        )))
    }

    fn mouse_position(&self) -> Point<Pixels> {
        let position = from_objc_point(
            self.0
                .lock()
                .native_window()
                .mouseLocationOutsideOfEventStream(),
        );
        convert_mouse_position(position, self.content_size().height)
    }

    fn modifiers(&self) -> Modifiers {
        let modifiers = ns_event_modifier_flags();

        let control = modifiers.contains(NSEventModifierFlags::Control);
        let alt = modifiers.contains(NSEventModifierFlags::Option);
        let shift = modifiers.contains(NSEventModifierFlags::Shift);
        let command = modifiers.contains(NSEventModifierFlags::Command);
        let function = modifiers.contains(NSEventModifierFlags::Function);

        Modifiers {
            control,
            alt,
            shift,
            platform: command,
            function,
        }
    }

    fn capslock(&self) -> Capslock {
        let modifiers = ns_event_modifier_flags();

        Capslock {
            on: modifiers.contains(NSEventModifierFlags::CapsLock),
        }
    }

    fn set_input_handler(&mut self, input_handler: PlatformInputHandler) {
        self.0.as_ref().lock().input_handler = Some(input_handler);
    }

    fn take_input_handler(&mut self) -> Option<PlatformInputHandler> {
        self.0.as_ref().lock().input_handler.take()
    }

    fn prompt(
        &self,
        level: PromptLevel,
        msg: &str,
        detail: Option<&str>,
        answers: &[PromptButton],
    ) -> Option<oneshot::Receiver<usize>> {
        use objc2_foundation::{NSInteger, NSString};

        // NSAlert's first button keeps Return and Cancel keeps Escape, but the keyboard
        // focus (and therefore Space) defaults to Cancel, leaving the middle button of
        // prompts like "Save / Don't Save / Cancel" unreachable from the keyboard.
        let initial_focus_ix = answers
            .iter()
            .enumerate()
            .rev()
            .find(|(_, label)| !label.is_cancel())
            .map(|(ix, _)| ix)
            .filter(|&ix| ix > 0);

        let alert = NSAlert::new(self.1);
        alert.setAlertStyle(match level {
            PromptLevel::Critical => NSAlertStyle::Critical,
            PromptLevel::Warning => NSAlertStyle::Warning,
            PromptLevel::Info => NSAlertStyle::Informational,
        });
        let message = NSString::from_str(msg);
        alert.setMessageText(message.as_ref());

        if let Some(detail) = detail {
            let detail_text = NSString::from_str(detail);
            alert.setInformativeText(detail_text.as_ref());
        }

        let mut initial_focus_button: Option<Retained<Objc2NSButton>> = None;
        for (ix, answer) in answers.iter().enumerate() {
            let title = NSString::from_str(answer.label());
            let button = alert.addButtonWithTitle(&title);
            button.setTag(ix as NSInteger);

            if answer.is_cancel() {
                if let Some(key) = core::char::from_u32(super::events::ESCAPE_KEY) {
                    let key = NSString::from_str(&key.to_string());
                    button.setKeyEquivalent(&key);
                }
            } else if Some(ix) == initial_focus_ix {
                initial_focus_button = Some(button);
            }
        }

        if let Some(button) = initial_focus_button {
            alert.window().setInitialFirstResponder(Some(&button));
        }

        let (done_tx, done_rx) = oneshot::channel();
        let done_tx = Cell::new(Some(done_tx));

        let block = RcBlock::new(move |answer: NSInteger| {
            if let Some(done_tx) = done_tx.take() {
                let _ = done_tx.send(answer.try_into().unwrap());
            }
        });

        let lock = self.0.lock();
        let native_window = lock.native_window_ptr();
        let executor = lock.executor.clone();
        executor
            .spawn(async move {
                let sheet_window = unsafe { native_window_from_ptr(native_window) };
                alert.beginSheetModalForWindow_completionHandler(sheet_window, Some(&block));
            })
            .detach();

        Some(done_rx)
    }

    fn activate(&self) {
        let window = self.0.lock().native_window_ptr();
        let executor = self.0.lock().executor.clone();
        executor
            .spawn(async move {
                unsafe { native_window_from_ptr(window) }.makeKeyAndOrderFront(None);
            })
            .detach();
    }

    fn request_attention(&self) {
        if self.is_active() {
            return;
        }

        let executor = self.0.lock().executor.clone();
        executor
            .spawn(async move {
                super::dock::request_user_attention(crate::AttentionType::Informational);
            })
            .detach();
    }

    fn is_active(&self) -> bool {
        self.0.lock().native_window().isKeyWindow()
    }

    // is_hovered is unused on macOS. See Window::is_window_hovered.
    fn is_hovered(&self) -> bool {
        false
    }

    fn set_title(&mut self, title: &str) {
        let app = shared_ns_application();
        let window = self.0.lock().native_window().retain();
        let title = NSString::from_str(title);
        // Deprecated NSApplication window-menu API; objc2-app-kit 0.3.2 has no typed binding.
        let _: () = unsafe {
            msg_send![
                &*app,
                changeWindowsItem: &*window,
                title: &*title,
                filename: false
            ]
        };
        window.setTitle(&title);
        self.0.lock().move_traffic_light();
    }

    fn get_title(&self) -> String {
        self.0.lock().native_window().title().to_string()
    }

    fn set_app_id(&mut self, _app_id: &str) {}

    fn set_background_appearance(&self, background_appearance: WindowBackgroundAppearance) {
        let mut this = self.0.as_ref().lock();

        let opaque = background_appearance == WindowBackgroundAppearance::Opaque;
        this.renderer.update_transparency(!opaque);

        this.native_window().setOpaque(opaque);
        let alpha = if opaque { 1.0 } else { 0.0001 };
        let background_color = NSColor::colorWithSRGBRed_green_blue_alpha(0.0, 0.0, 0.0, alpha);
        this.native_window()
            .setBackgroundColor(Some(&background_color));

        if background_appearance != WindowBackgroundAppearance::Blurred {
            if let Some(blur_view) = this.blurred_view.take() {
                if let Some(view) = unsafe { blur_view.cast::<NSView>().as_ref() } {
                    view.removeFromSuperview();
                }
            }
        } else if this.blurred_view.is_none() {
            let Some(content_view) = this.native_window().contentView() else {
                return;
            };
            let blur = BlurredView::with_frame(self.1, content_view.bounds());
            blur.setAutoresizingMask(
                NSAutoresizingMaskOptions::ViewWidthSizable
                    | NSAutoresizingMaskOptions::ViewHeightSizable,
            );
            content_view.addSubview_positioned_relativeTo(&blur, NSWindowOrderingMode::Below, None);
            this.blurred_view = Some(Retained::as_ptr(&blur) as id);
            drop(blur);
        }
    }

    fn set_edited(&mut self, edited: bool) {
        self.0.lock().native_window().setDocumentEdited(edited);

        // Changing the document edited state resets the traffic light position,
        // so we have to move it again.
        self.0.lock().move_traffic_light();
    }

    fn show_character_palette(&self) {
        let this = self.0.lock();
        let window = this.native_window_ptr();
        this.executor
            .spawn(async move {
                let app = shared_ns_application();
                app.orderFrontCharacterPalette(Some(as_any_object(unsafe {
                    native_window_from_ptr(window)
                })));
            })
            .detach();
    }

    fn minimize(&self) {
        self.0.lock().native_window().miniaturize(None);
    }

    fn zoom(&self) {
        let this = self.0.lock();
        let window = this.native_window_ptr();
        this.executor
            .spawn(async move {
                unsafe { native_window_from_ptr(window) }.zoom(None);
            })
            .detach();
    }

    fn toggle_fullscreen(&self) {
        let this = self.0.lock();
        let window = this.native_window_ptr();
        this.executor
            .spawn(async move {
                unsafe { native_window_from_ptr(window) }.toggleFullScreen(None);
            })
            .detach();
    }

    fn toggle_simple_fullscreen(&self) {
        let state = self.0.clone();
        let executor = {
            let this = self.0.lock();
            this.executor.clone()
        };
        executor
            .spawn(async move {
                let (native_window, native_view, plan) = {
                    let mut lock = state.lock();
                    (
                        lock.native_window_ptr(),
                        lock.native_view,
                        lock.toggle_simple_fullscreen(),
                    )
                };
                if let Some(plan) = plan {
                    apply_simple_fullscreen_plan(
                        unsafe { native_window_from_ptr(native_window) },
                        as_ns_view(native_view),
                        plan,
                    );
                }
            })
            .detach();
    }

    fn is_simple_fullscreen(&self) -> bool {
        self.0.lock().simple_fullscreen_state.is_some()
    }

    fn is_fullscreen(&self) -> bool {
        self.0.lock().is_fullscreen()
    }

    fn on_request_frame(&self, callback: Box<dyn FnMut(RequestFrameOptions)>) {
        let mut lock = self.0.as_ref().lock();
        lock.request_frame_callback = Some(callback);
        lock.start_display_link();
    }

    fn on_input(&self, callback: Box<dyn FnMut(PlatformInput) -> crate::DispatchEventResult>) {
        self.0.as_ref().lock().event_callback = Some(callback);
    }

    fn on_active_status_change(&self, callback: Box<dyn FnMut(bool)>) {
        self.0.as_ref().lock().activate_callback = Some(callback);
    }

    fn on_hover_status_change(&self, _: Box<dyn FnMut(bool)>) {}

    fn on_resize(&self, callback: Box<dyn FnMut(Size<Pixels>, f32)>) {
        self.0.as_ref().lock().resize_callback = Some(callback);
    }

    fn on_moved(&self, callback: Box<dyn FnMut()>) {
        self.0.as_ref().lock().moved_callback = Some(callback);
    }

    fn on_should_close(&self, callback: Box<dyn FnMut() -> bool>) {
        self.0.as_ref().lock().should_close_callback = Some(callback);
    }

    fn on_close(&self, callback: Box<dyn FnOnce()>) {
        self.0.as_ref().lock().close_callback = Some(callback);
    }

    fn on_hit_test_window_control(&self, _callback: Box<dyn FnMut() -> Option<WindowControlArea>>) {
    }

    fn on_appearance_changed(&self, callback: Box<dyn FnMut()>) {
        self.0.lock().appearance_changed_callback = Some(callback);
    }

    fn tabbed_windows(&self) -> Option<Vec<SystemWindowTab>> {
        let windows = self.0.lock().native_window().tabbedWindows()?;
        let mut result = Vec::new();
        for window in windows.iter() {
            if let Some(gpui_window) = gpui_window_from_ns(&window) {
                let handle = retain_window_state(gpui_window.ivars().state.get())
                    .lock()
                    .handle;
                let title = SharedString::from(window.title().to_string());
                result.push(SystemWindowTab::new(title, handle));
            }
        }
        Some(result)
    }

    fn tab_bar_visible(&self) -> bool {
        self.0
            .lock()
            .native_window()
            .tabGroup()
            .is_some_and(|tab_group| tab_group.isTabBarVisible())
    }

    fn on_move_tab_to_new_window(&self, callback: Box<dyn FnMut()>) {
        self.0.as_ref().lock().move_tab_to_new_window_callback = Some(callback);
    }

    fn on_merge_all_windows(&self, callback: Box<dyn FnMut()>) {
        self.0.as_ref().lock().merge_all_windows_callback = Some(callback);
    }

    fn on_select_next_tab(&self, callback: Box<dyn FnMut()>) {
        self.0.as_ref().lock().select_next_tab_callback = Some(callback);
    }

    fn on_select_previous_tab(&self, callback: Box<dyn FnMut()>) {
        self.0.as_ref().lock().select_previous_tab_callback = Some(callback);
    }

    fn on_toggle_tab_bar(&self, callback: Box<dyn FnMut()>) {
        self.0.as_ref().lock().toggle_tab_bar_callback = Some(callback);
    }

    fn draw(&self, scene: &crate::Scene) {
        let mut this = self.0.lock();
        this.renderer.draw(scene);
    }

    #[cfg(any(test, feature = "test-support"))]
    fn render_to_image(&self, scene: &crate::Scene) -> anyhow::Result<image::RgbaImage> {
        let mut this = self.0.lock();
        let size = this.content_size().to_device_pixels(this.scale_factor());
        this.renderer.render_scene_to_image(scene, size)
    }

    fn sprite_atlas(&self) -> Arc<dyn PlatformAtlas> {
        self.0.lock().renderer.sprite_atlas().clone()
    }

    fn gpu_specs(&self) -> Option<crate::GpuSpecs> {
        None
    }

    fn update_ime_position(&self, _bounds: Bounds<Pixels>) {
        let executor = self.0.lock().executor.clone();
        executor
            .spawn(async move {
                if let Some(input_context) = NSTextInputContext::currentInputContext(unsafe {
                    MainThreadMarker::new_unchecked()
                }) {
                    input_context.invalidateCharacterCoordinates();
                }
            })
            .detach()
    }

    fn play_system_bell(&self) {
        NSBeep()
    }

    #[cfg(feature = "accessibility")]
    fn a11y_init(&self, callbacks: crate::A11yCallbacks) {
        let mut lock = self.0.lock();
        let activation_handler = A11yActivationHandler {
            callback: callbacks.activation,
        };
        let action_handler = A11yActionHandler(callbacks.action);
        let adapter = unsafe {
            accesskit_macos::SubclassingAdapter::for_window(
                lock.native_window_ptr() as *mut c_void,
                activation_handler,
                action_handler,
            )
        };
        lock.accesskit_adapter = Some(adapter);
    }

    #[cfg(feature = "accessibility")]
    fn a11y_tree_update(&self, tree_update: accesskit::TreeUpdate) {
        let events = {
            let mut lock = self.0.lock();
            lock.accesskit_adapter
                .as_mut()
                .and_then(|adapter| adapter.update_if_active(|| tree_update))
        };
        if let Some(events) = events {
            events.raise();
        }
    }

    #[cfg(feature = "accessibility")]
    fn a11y_update_window_bounds(&self) {
        // macOS tracks window bounds automatically through NSAccessibility.
    }

    fn show(&self) {
        self.0.lock().native_window().makeKeyAndOrderFront(None);
    }

    fn hide(&self) {
        self.0.lock().native_window().orderOut(None);
    }

    fn is_visible(&self) -> bool {
        self.0.lock().native_window().isVisible()
    }

    fn set_mouse_passthrough(&self, passthrough: bool) {
        self.0
            .lock()
            .native_window()
            .setIgnoresMouseEvents(passthrough);
    }

    fn titlebar_double_click(&self) {
        let this = self.0.lock();
        if this.simple_fullscreen_state.is_some() {
            return;
        }
        let window = this.native_window_ptr();
        this.executor
            .spawn(async move {
                let action_str = global_domain_string("AppleActionOnDoubleClick");
                let window = unsafe { native_window_from_ptr(window) };
                match action_str.as_str() {
                    "None" => {
                        // "Do Nothing" selected, so do no action
                    }
                    "Minimize" => {
                        window.miniaturize(None);
                    }
                    "Maximize" => {
                        window.zoom(None);
                    }
                    "Fill" => {
                        // Unlike `zoom:`, AppKit's private Fill action honors the system's
                        // "Tiled windows have margins" setting.
                        if window.respondsToSelector(objc2::sel!(_zoomFill:)) {
                            let _: () = unsafe { msg_send![window, _zoomFill: None::<&AnyObject>] };
                        } else {
                            window.zoom(None);
                        }
                    }
                    _ => {
                        window.zoom(None);
                    }
                }
            })
            .detach();
    }

    fn set_progress_bar(&self, state: crate::ProgressBarState) {
        let app = shared_ns_application();
        let dock_tile = app.dockTile();
        let indicator_frame =
            Objc2NSRect::new(Objc2NSPoint::new(0.0, 0.0), Objc2NSSize::new(140.0, 140.0));
        match state {
            crate::ProgressBarState::None => {
                dock_tile.setContentView(None);
                dock_tile.setBadgeLabel(None);
                dock_tile.display();
            }
            crate::ProgressBarState::Indeterminate => {
                let indicator = NSProgressIndicator::initWithFrame(
                    NSProgressIndicator::alloc(self.1),
                    indicator_frame,
                );
                indicator.setStyle(NSProgressIndicatorStyle::Bar);
                indicator.setIndeterminate(true);
                unsafe { indicator.startAnimation(None) };
                dock_tile.setContentView(Some(&indicator));
                dock_tile.display();
            }
            crate::ProgressBarState::Normal(pct)
            | crate::ProgressBarState::Error(pct)
            | crate::ProgressBarState::Paused(pct) => {
                let indicator = NSProgressIndicator::initWithFrame(
                    NSProgressIndicator::alloc(self.1),
                    indicator_frame,
                );
                indicator.setStyle(NSProgressIndicatorStyle::Bar);
                indicator.setIndeterminate(false);
                indicator.setMinValue(0.0);
                indicator.setMaxValue(100.0);
                indicator.setDoubleValue(pct * 100.0);
                dock_tile.setContentView(Some(&indicator));
                dock_tile.display();
            }
        }
    }

    fn set_cursor_style(&self, style: CursorStyle) {
        let mut state = self.0.lock();
        if state.cursor_style != style {
            // If transitioning away from hidden cursor, unhide immediately rather than
            // waiting for resetCursorRects, so other windows/apps aren't affected.
            if state.cursor_hidden && !matches!(style, CursorStyle::None) {
                NSCursor::unhide();
                state.cursor_hidden = false;
            }
            state.cursor_style = style;
            let native_window = state.native_window_ptr();
            let native_view = state.native_view;
            drop(state);
            unsafe { native_window_from_ptr(native_window) }
                .invalidateCursorRectsForView(as_ns_view(native_view));
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

impl rwh::HasWindowHandle for MacWindow {
    fn window_handle(&self) -> Result<rwh::WindowHandle<'_>, rwh::HandleError> {
        // SAFETY: The AppKitWindowHandle is a wrapper around a pointer to an NSView
        unsafe {
            Ok(rwh::WindowHandle::borrow_raw(rwh::RawWindowHandle::AppKit(
                rwh::AppKitWindowHandle::new(self.0.lock().native_view.cast()),
            )))
        }
    }
}

impl rwh::HasDisplayHandle for MacWindow {
    fn display_handle(&self) -> Result<rwh::DisplayHandle<'_>, rwh::HandleError> {
        // SAFETY: This is a no-op on macOS
        unsafe {
            Ok(rwh::DisplayHandle::borrow_raw(
                rwh::AppKitDisplayHandle::new().into(),
            ))
        }
    }
}

pub(crate) fn is_macos_version_at_least(version: NSOperatingSystemVersion) -> bool {
    NSProcessInfo::processInfo().isOperatingSystemAtLeastVersion(
        objc2_foundation::NSOperatingSystemVersion {
            majorVersion: version.major_version as isize,
            minorVersion: version.minor_version as isize,
            patchVersion: version.patch_version as isize,
        },
    )
}

fn get_scale_factor(native_window: &NSWindow) -> f32 {
    let factor = native_window
        .screen()
        .map(|screen| screen.backingScaleFactor() as f32)
        .unwrap_or(2.0);

    // We are not certain what triggers this, but it seems that sometimes
    // this method would return 0 (https://github.com/zed-industries/zed/issues/6412)
    // It seems most likely that this would happen if the window has no screen
    // (if it is off-screen), though we'd expect to see viewDidChangeBackingProperties before
    // it was rendered for real.
    // Regardless, attempt to avoid the issue here.
    if factor == 0.0 { 2. } else { factor }
}

unsafe extern "C" fn step(context: *mut c_void) {
    let context = unsafe { &*(context as *const FrameRequestContext) };
    let Some(window_state) = context.window_state.upgrade() else {
        return;
    };
    let mut lock = window_state.lock();

    if let Some(mut callback) = lock.request_frame_callback.take() {
        drop(lock);
        callback(Default::default());
        let mut lock = window_state.lock();
        if !lock.is_closing {
            lock.request_frame_callback = Some(callback);
        }
    }
}

fn titlebar_move_rect(bounds: NSRect, app_owns_titlebar_drag: bool) -> NSRect {
    if app_owns_titlebar_drag {
        bounds
    } else {
        NSRect::new(NSPoint::new(0., 0.), NSSize::new(0., 0.))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn app_owned_titlebar_claims_the_content_view_for_window_moves() {
        let bounds = NSRect::new(NSPoint::new(10., 20.), NSSize::new(300., 200.));
        let claimed = titlebar_move_rect(bounds, true);
        assert_eq!(claimed.origin.x, 10.);
        assert_eq!(claimed.origin.y, 20.);
        assert_eq!(claimed.size.width, 300.);
        assert_eq!(claimed.size.height, 200.);

        let native = titlebar_move_rect(bounds, false);
        assert_eq!(native.origin.x, 0.);
        assert_eq!(native.origin.y, 0.);
        assert_eq!(native.size.width, 0.);
        assert_eq!(native.size.height, 0.);
    }
}

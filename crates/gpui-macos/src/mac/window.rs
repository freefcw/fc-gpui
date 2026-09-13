use super::{
    BoolExt, MacDisplay, NSRange, NSStringExt, display_id_for_screen, ns_string, renderer,
};
use crate::{
    AnyWindowHandle, Bounds, Capslock, CursorStyle, DevicePixels, ExternalPaths, FileDropEvent,
    ForegroundExecutor, KeyDownEvent, Keystroke, Modifiers, ModifiersChangedEvent, MouseButton,
    MouseDownEvent, MouseMoveEvent, MouseUpEvent, Pixels, PlatformAtlas, PlatformDisplay,
    PlatformInput, PlatformInputHandler, PlatformWindow, Point, PromptButton, PromptLevel,
    RequestFrameOptions, SharedString, Size, SystemWindowTab, Timer, WindowAppearance,
    WindowBackgroundAppearance, WindowBounds, WindowControlArea, WindowFrameSource, WindowKind,
    WindowParams, dispatch_get_main_queue, dispatch_sys::dispatch_async_f, point, px, size,
};
use block2::RcBlock;
use core_graphics::display::{CGPoint, CGRect};
use ctor::ctor;
use futures::channel::oneshot;
use objc::{
    class,
    declare::ClassDecl,
    msg_send,
    runtime::{BOOL, Class, NO, Object, Protocol, Sel, YES},
    sel, sel_impl,
};
use objc2::{MainThreadMarker, rc::Retained};
use objc2_app_kit::{
    NSAlert, NSAlertStyle, NSAutoresizingMaskOptions, NSBackingStoreType, NSBeep,
    NSButton as Objc2NSButton, NSEventModifierFlags, NSVisualEffectMaterial, NSVisualEffectState,
    NSWindow as Objc2NSWindow, NSWindowButton, NSWindowCollectionBehavior, NSWindowOcclusionState,
    NSWindowOrderingMode, NSWindowStyleMask, NSWindowTitleVisibility,
};
use parking_lot::Mutex;
use raw_window_handle as rwh;
use smallvec::SmallVec;
use std::{
    cell::Cell,
    ffi::c_void,
    mem,
    ops::Range,
    path::PathBuf,
    ptr::{self, NonNull},
    rc::Rc,
    sync::{Arc, Once, Weak},
    time::Duration,
};
use util::ResultExt;

type ObjcId = *mut Object;

#[allow(non_camel_case_types)]
type id = ObjcId;

#[allow(non_upper_case_globals)]
const nil: ObjcId = ptr::null_mut();

#[allow(non_camel_case_types)]
type NSInteger = isize;

#[allow(non_camel_case_types)]
type NSUInteger = usize;

#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq)]
struct NSPoint {
    pub x: f64,
    pub y: f64,
}

impl NSPoint {
    fn new(x: f64, y: f64) -> Self {
        Self { x, y }
    }
}

unsafe impl objc::Encode for NSPoint {
    fn encode() -> objc::Encoding {
        let encoding = format!(
            "{{CGPoint={}{}}}",
            f64::encode().as_str(),
            f64::encode().as_str()
        );
        unsafe { objc::Encoding::from_str(&encoding) }
    }
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq)]
struct NSSize {
    pub width: f64,
    pub height: f64,
}

impl NSSize {
    fn new(width: f64, height: f64) -> Self {
        Self { width, height }
    }
}

unsafe impl objc::Encode for NSSize {
    fn encode() -> objc::Encoding {
        let encoding = format!(
            "{{CGSize={}{}}}",
            f64::encode().as_str(),
            f64::encode().as_str()
        );
        unsafe { objc::Encoding::from_str(&encoding) }
    }
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq)]
struct NSRect {
    pub origin: NSPoint,
    pub size: NSSize,
}

impl NSRect {
    fn new(origin: NSPoint, size: NSSize) -> Self {
        Self { origin, size }
    }
}

unsafe impl objc::Encode for NSRect {
    fn encode() -> objc::Encoding {
        let encoding = format!(
            "{{CGRect={}{}}}",
            NSPoint::encode().as_str(),
            NSSize::encode().as_str()
        );
        unsafe { objc::Encoding::from_str(&encoding) }
    }
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

unsafe impl objc::Encode for NSOperatingSystemVersion {
    fn encode() -> objc::Encoding {
        let encoding = format!(
            "{{NSOperatingSystemVersion={}{}{}}}",
            NSUInteger::encode().as_str(),
            NSUInteger::encode().as_str(),
            NSUInteger::encode().as_str()
        );
        unsafe { objc::Encoding::from_str(&encoding) }
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

const WINDOW_STATE_IVAR: &str = "windowState";

static RESTORES_WORKSPACE_AT_LAUNCH_DEFAULT: Once = Once::new();

static mut WINDOW_CLASS: *const Class = ptr::null();
static mut PANEL_CLASS: *const Class = ptr::null();
static mut VIEW_CLASS: *const Class = ptr::null();
static mut BLURRED_VIEW_CLASS: *const Class = ptr::null();
static mut WINDOW_STATE_ARCHIVER_DELEGATE_CLASS: *const Class = ptr::null();
static mut WINDOW_STATE_UNARCHIVER_CLASS: *const Class = ptr::null();

#[allow(non_upper_case_globals)]
const NSNormalWindowLevel: NSInteger = 0;
#[allow(non_upper_case_globals)]
const NSPopUpWindowLevel: NSInteger = 101;
#[allow(non_upper_case_globals)]
const NSTrackingMouseEnteredAndExited: NSUInteger = 0x01;
#[allow(non_upper_case_globals)]
const NSTrackingMouseMoved: NSUInteger = 0x02;
#[allow(non_upper_case_globals)]
const NSTrackingActiveAlways: NSUInteger = 0x80;
#[allow(non_upper_case_globals)]
const NSTrackingInVisibleRect: NSUInteger = 0x200;
#[allow(non_upper_case_globals)]
const NSWindowAnimationBehaviorUtilityWindow: NSInteger = 4;
#[allow(non_upper_case_globals)]
const NSViewLayerContentsRedrawDuringViewResize: NSInteger = 2;
// https://developer.apple.com/documentation/appkit/nsdragoperation
type NSDragOperation = NSUInteger;
#[allow(non_upper_case_globals)]
const NSDragOperationNone: NSDragOperation = 0;
#[allow(non_upper_case_globals)]
const NSDragOperationCopy: NSDragOperation = 1;
#[derive(PartialEq)]
pub enum UserTabbingPreference {
    Never,
    Always,
    InFullScreen,
}

unsafe fn autorelease(object: id) -> id {
    unsafe { msg_send![object, autorelease] }
}

unsafe fn array_count(array: id) -> NSUInteger {
    unsafe { msg_send![array, count] }
}

unsafe fn array_object_at_index(array: id, index: NSUInteger) -> id {
    unsafe { msg_send![array, objectAtIndex: index] }
}

unsafe fn filenames_pboard_type() -> id {
    unsafe { ns_string("NSFilenamesPboardType") }
}

unsafe fn screen_backing_scale_factor(screen: id) -> f64 {
    unsafe { msg_send![screen, backingScaleFactor] }
}

unsafe fn screen_device_description(screen: id) -> id {
    unsafe { msg_send![screen, deviceDescription] }
}

unsafe fn screen_frame(screen: id) -> NSRect {
    unsafe { msg_send![screen, frame] }
}

unsafe fn screen_visible_frame(screen: id) -> NSRect {
    unsafe { msg_send![screen, visibleFrame] }
}

unsafe fn shared_application() -> id {
    unsafe { msg_send![class!(NSApplication), sharedApplication] }
}

unsafe fn user_defaults() -> id {
    unsafe { msg_send![class!(NSUserDefaults), standardUserDefaults] }
}

unsafe fn view_bounds(view: id) -> NSRect {
    unsafe { msg_send![view, bounds] }
}

unsafe fn view_frame(view: id) -> NSRect {
    unsafe { msg_send![view, frame] }
}

unsafe fn view_init_with_frame(view: id, frame: NSRect) -> id {
    unsafe { msg_send![view, initWithFrame: frame] }
}

unsafe fn window_content_view(window: id) -> id {
    unsafe { msg_send![window, contentView] }
}

unsafe fn window_frame(window: id) -> NSRect {
    unsafe { msg_send![window, frame] }
}

unsafe fn window_occlusion_state(window: id) -> NSWindowOcclusionState {
    unsafe { msg_send![window, occlusionState] }
}

unsafe fn window_screen(window: id) -> id {
    unsafe { msg_send![window, screen] }
}

unsafe fn window_style_mask(window: id) -> NSWindowStyleMask {
    unsafe { msg_send![window, styleMask] }
}

#[ctor]
unsafe fn build_classes() {
    unsafe {
        WINDOW_CLASS = build_window_class("GPUIWindow", class!(NSWindow));
        PANEL_CLASS = build_window_class("GPUIPanel", class!(NSPanel));
        VIEW_CLASS = {
            let mut decl = ClassDecl::new("GPUIView", class!(NSView)).unwrap();
            decl.add_ivar::<*mut c_void>(WINDOW_STATE_IVAR);
            unsafe {
                decl.add_method(sel!(dealloc), dealloc_view as extern "C" fn(&Object, Sel));

                decl.add_method(
                    sel!(performKeyEquivalent:),
                    handle_key_equivalent as extern "C" fn(&Object, Sel, id) -> BOOL,
                );
                decl.add_method(
                    sel!(keyDown:),
                    handle_key_down as extern "C" fn(&Object, Sel, id),
                );
                decl.add_method(
                    sel!(keyUp:),
                    handle_key_up as extern "C" fn(&Object, Sel, id),
                );
                decl.add_method(
                    sel!(mouseDown:),
                    handle_view_event as extern "C" fn(&Object, Sel, id),
                );
                decl.add_method(
                    sel!(mouseUp:),
                    handle_view_event as extern "C" fn(&Object, Sel, id),
                );
                decl.add_method(
                    sel!(rightMouseDown:),
                    handle_view_event as extern "C" fn(&Object, Sel, id),
                );
                decl.add_method(
                    sel!(rightMouseUp:),
                    handle_view_event as extern "C" fn(&Object, Sel, id),
                );
                decl.add_method(
                    sel!(otherMouseDown:),
                    handle_view_event as extern "C" fn(&Object, Sel, id),
                );
                decl.add_method(
                    sel!(otherMouseUp:),
                    handle_view_event as extern "C" fn(&Object, Sel, id),
                );
                decl.add_method(
                    sel!(mouseMoved:),
                    handle_view_event as extern "C" fn(&Object, Sel, id),
                );
                decl.add_method(
                    sel!(resetCursorRects),
                    reset_cursor_rects as extern "C" fn(&Object, Sel),
                );
                decl.add_method(
                    sel!(mouseExited:),
                    handle_view_event as extern "C" fn(&Object, Sel, id),
                );
                decl.add_method(
                    sel!(mouseDragged:),
                    handle_view_event as extern "C" fn(&Object, Sel, id),
                );
                decl.add_method(
                    sel!(scrollWheel:),
                    handle_view_event as extern "C" fn(&Object, Sel, id),
                );
                decl.add_method(
                    sel!(swipeWithEvent:),
                    handle_view_event as extern "C" fn(&Object, Sel, id),
                );
                decl.add_method(
                    sel!(flagsChanged:),
                    handle_view_event as extern "C" fn(&Object, Sel, id),
                );

                decl.add_method(
                    sel!(makeBackingLayer),
                    make_backing_layer as extern "C" fn(&Object, Sel) -> id,
                );

                decl.add_protocol(Protocol::get("CALayerDelegate").unwrap());
                decl.add_method(
                    sel!(viewDidChangeBackingProperties),
                    view_did_change_backing_properties as extern "C" fn(&Object, Sel),
                );
                decl.add_method(
                    sel!(setFrameSize:),
                    set_frame_size as extern "C" fn(&Object, Sel, NSSize),
                );
                decl.add_method(
                    sel!(displayLayer:),
                    display_layer as extern "C" fn(&Object, Sel, id),
                );

                decl.add_protocol(Protocol::get("NSTextInputClient").unwrap());
                decl.add_method(
                    sel!(validAttributesForMarkedText),
                    valid_attributes_for_marked_text as extern "C" fn(&Object, Sel) -> id,
                );
                decl.add_method(
                    sel!(hasMarkedText),
                    has_marked_text as extern "C" fn(&Object, Sel) -> BOOL,
                );
                decl.add_method(
                    sel!(markedRange),
                    marked_range as extern "C" fn(&Object, Sel) -> NSRange,
                );
                decl.add_method(
                    sel!(selectedRange),
                    selected_range as extern "C" fn(&Object, Sel) -> NSRange,
                );
                decl.add_method(
                    sel!(firstRectForCharacterRange:actualRange:),
                    first_rect_for_character_range
                        as extern "C" fn(&Object, Sel, NSRange, id) -> NSRect,
                );
                decl.add_method(
                    sel!(insertText:replacementRange:),
                    insert_text as extern "C" fn(&Object, Sel, id, NSRange),
                );
                decl.add_method(
                    sel!(setMarkedText:selectedRange:replacementRange:),
                    set_marked_text as extern "C" fn(&Object, Sel, id, NSRange, NSRange),
                );
                decl.add_method(sel!(unmarkText), unmark_text as extern "C" fn(&Object, Sel));
                decl.add_method(
                    sel!(attributedSubstringForProposedRange:actualRange:),
                    attributed_substring_for_proposed_range
                        as extern "C" fn(&Object, Sel, NSRange, *mut c_void) -> id,
                );
                decl.add_method(
                    sel!(viewDidChangeEffectiveAppearance),
                    view_did_change_effective_appearance as extern "C" fn(&Object, Sel),
                );

                // Suppress beep on keystrokes with modifier keys.
                decl.add_method(
                    sel!(doCommandBySelector:),
                    do_command_by_selector as extern "C" fn(&Object, Sel, Sel),
                );

                decl.add_method(
                    sel!(acceptsFirstMouse:),
                    accepts_first_mouse as extern "C" fn(&Object, Sel, id) -> BOOL,
                );

                decl.add_method(
                    sel!(_opaqueRectForWindowMoveWhenInTitlebar),
                    opaque_rect_for_window_move_when_in_titlebar
                        as extern "C" fn(&Object, Sel) -> NSRect,
                );

                decl.add_method(
                    sel!(characterIndexForPoint:),
                    character_index_for_point as extern "C" fn(&Object, Sel, NSPoint) -> u64,
                );
            }
            decl.register()
        };
        BLURRED_VIEW_CLASS = {
            let mut decl = ClassDecl::new("BlurredView", class!(NSVisualEffectView)).unwrap();
            unsafe {
                decl.add_method(
                    sel!(initWithFrame:),
                    blurred_view_init_with_frame as extern "C" fn(&Object, Sel, NSRect) -> id,
                );
                decl.add_method(
                    sel!(updateLayer),
                    blurred_view_update_layer as extern "C" fn(&Object, Sel),
                );
                decl.register()
            }
        };
        WINDOW_STATE_ARCHIVER_DELEGATE_CLASS = {
            let mut decl =
                ClassDecl::new("GPUIWindowStateArchiverDelegate", class!(NSObject)).unwrap();
            decl.add_method(
                sel!(archiver:willEncodeObject:),
                window_state_archiver_will_encode_object
                    as extern "C" fn(&Object, Sel, id, id) -> id,
            );
            decl.register()
        };
        WINDOW_STATE_UNARCHIVER_CLASS = {
            let mut decl =
                ClassDecl::new("GPUIWindowStateKeyedUnarchiver", class!(NSKeyedUnarchiver))
                    .unwrap();
            decl.add_method(
                sel!(_windowRestorationOptions),
                window_state_unarchiver_restoration_options as extern "C" fn(&Object, Sel) -> id,
            );
            decl.register()
        };
    }
}

// NSKeyedArchiverDelegate callback that skips objects which don't adopt `NSSecureCoding`
// (the window itself and its NSView hierarchy), so encoding the window's restorable state
// succeeds. AppKit still encodes the window frame and its persistent window-management
// identifier, which is what the Space restoration on relaunch keys off.
extern "C" fn window_state_archiver_will_encode_object(
    _this: &Object,
    _sel: Sel,
    _archiver: id,
    object: id,
) -> id {
    // SAFETY: `object` is whatever AppKit hands the delegate during archiving; we only send it
    // `isKindOfClass:` with valid class arguments, which is safe for any Objective-C object.
    unsafe {
        if object.is_null() {
            return object;
        }
        let is_view: BOOL = msg_send![object, isKindOfClass: class!(NSView)];
        let is_window: BOOL = msg_send![object, isKindOfClass: class!(NSWindow)];
        if is_view == YES || is_window == YES {
            nil
        } else {
            object
        }
    }
}

// Override of the private `_windowRestorationOptions` on our NSKeyedUnarchiver subclass.
// Returning a default-initialized `NSWindowRestorationOptions` tells AppKit to restore the
// window to its original Space. This is the macOS 15+ path (FB15644170: the
// `NSWindowRestoresWorkspaceAtLaunch` user default no longer works there).
extern "C" fn window_state_unarchiver_restoration_options(_this: &Object, _sel: Sel) -> id {
    if !is_macos_version_at_least(NSOperatingSystemVersion::new(15, 0, 0)) {
        return nil;
    }
    // SAFETY: we look the class up by name and only send it `alloc`/`init`/`autorelease`, all of
    // which have the standard `-> id` signature. Returning `nil` when the class is absent is valid.
    unsafe {
        match Class::get("NSWindowRestorationOptions") {
            Some(class) => {
                let options: id = msg_send![class, alloc];
                let options: id = msg_send![options, init];
                msg_send![options, autorelease]
            }
            None => nil,
        }
    }
}

fn convert_mouse_position(position: NSPoint, window_height: Pixels) -> Point<Pixels> {
    point(
        px(position.x as f32),
        // macOS screen coordinates are relative to bottom left
        window_height - px(position.y as f32),
    )
}

unsafe fn build_window_class(name: &'static str, superclass: &Class) -> *const Class {
    unsafe {
        let mut decl = ClassDecl::new(name, superclass).unwrap();
        decl.add_ivar::<*mut c_void>(WINDOW_STATE_IVAR);
        decl.add_method(sel!(dealloc), dealloc_window as extern "C" fn(&Object, Sel));

        decl.add_method(
            sel!(canBecomeMainWindow),
            yes as extern "C" fn(&Object, Sel) -> BOOL,
        );
        decl.add_method(
            sel!(canBecomeKeyWindow),
            yes as extern "C" fn(&Object, Sel) -> BOOL,
        );
        decl.add_method(
            sel!(windowDidResize:),
            window_did_resize as extern "C" fn(&Object, Sel, id),
        );
        decl.add_method(
            sel!(windowDidChangeOcclusionState:),
            window_did_change_occlusion_state as extern "C" fn(&Object, Sel, id),
        );
        decl.add_method(
            sel!(windowWillEnterFullScreen:),
            window_will_enter_fullscreen as extern "C" fn(&Object, Sel, id),
        );
        decl.add_method(
            sel!(windowWillExitFullScreen:),
            window_will_exit_fullscreen as extern "C" fn(&Object, Sel, id),
        );
        decl.add_method(
            sel!(windowDidMove:),
            window_did_move as extern "C" fn(&Object, Sel, id),
        );
        decl.add_method(
            sel!(windowDidChangeScreen:),
            window_did_change_screen as extern "C" fn(&Object, Sel, id),
        );
        decl.add_method(
            sel!(windowDidBecomeKey:),
            window_did_change_key_status as extern "C" fn(&Object, Sel, id),
        );
        decl.add_method(
            sel!(windowDidResignKey:),
            window_did_change_key_status as extern "C" fn(&Object, Sel, id),
        );
        decl.add_method(
            sel!(windowShouldClose:),
            window_should_close as extern "C" fn(&Object, Sel, id) -> BOOL,
        );

        decl.add_method(sel!(close), close_window as extern "C" fn(&Object, Sel));

        decl.add_method(
            sel!(draggingEntered:),
            dragging_entered as extern "C" fn(&Object, Sel, id) -> NSDragOperation,
        );
        decl.add_method(
            sel!(draggingUpdated:),
            dragging_updated as extern "C" fn(&Object, Sel, id) -> NSDragOperation,
        );
        decl.add_method(
            sel!(draggingExited:),
            dragging_exited as extern "C" fn(&Object, Sel, id),
        );
        decl.add_method(
            sel!(performDragOperation:),
            perform_drag_operation as extern "C" fn(&Object, Sel, id) -> BOOL,
        );
        decl.add_method(
            sel!(concludeDragOperation:),
            conclude_drag_operation as extern "C" fn(&Object, Sel, id),
        );

        decl.add_method(
            sel!(addTitlebarAccessoryViewController:),
            add_titlebar_accessory_view_controller as extern "C" fn(&Object, Sel, id),
        );

        decl.add_method(
            sel!(moveTabToNewWindow:),
            move_tab_to_new_window as extern "C" fn(&Object, Sel, id),
        );

        decl.add_method(
            sel!(mergeAllWindows:),
            merge_all_windows as extern "C" fn(&Object, Sel, id),
        );

        decl.add_method(
            sel!(selectNextTab:),
            select_next_tab as extern "C" fn(&Object, Sel, id),
        );

        decl.add_method(
            sel!(selectPreviousTab:),
            select_previous_tab as extern "C" fn(&Object, Sel, id),
        );

        decl.add_method(
            sel!(toggleTabBar:),
            toggle_tab_bar as extern "C" fn(&Object, Sel, id),
        );

        decl.register()
    }
}

// `NSApplicationPresentationOptions` bits (see `NSApplication.PresentationOptions`).
const NS_APPLICATION_PRESENTATION_AUTO_HIDE_DOCK: NSUInteger = 1 << 0;
const NS_APPLICATION_PRESENTATION_AUTO_HIDE_MENU_BAR: NSUInteger = 1 << 2;

// State captured when entering simple (borderless) fullscreen, used to restore
// the window on exit.
struct SimpleFullscreenState {
    frame: NSRect,
    style_mask: NSWindowStyleMask,
}

enum SimpleFullscreenPlan {
    Enter { screen_frame: NSRect },
    Exit(SimpleFullscreenState),
}

struct SimpleFullscreenAppState {
    window_count: usize,
    saved_presentation_options: NSUInteger,
}

static SIMPLE_FULLSCREEN_APP_STATE: Mutex<Option<SimpleFullscreenAppState>> = Mutex::new(None);

unsafe fn push_simple_fullscreen_presentation_options() {
    let mut app_state = SIMPLE_FULLSCREEN_APP_STATE.lock();
    match app_state.as_mut() {
        Some(app_state) => app_state.window_count += 1,
        None => unsafe {
            let app = shared_application();
            let saved_presentation_options: NSUInteger = msg_send![app, presentationOptions];
            let _: () = msg_send![
                app,
                setPresentationOptions: NS_APPLICATION_PRESENTATION_AUTO_HIDE_DOCK
                    | NS_APPLICATION_PRESENTATION_AUTO_HIDE_MENU_BAR
            ];
            *app_state = Some(SimpleFullscreenAppState {
                window_count: 1,
                saved_presentation_options,
            });
        },
    }
}

unsafe fn pop_simple_fullscreen_presentation_options() {
    let mut app_state = SIMPLE_FULLSCREEN_APP_STATE.lock();
    if let Some(state) = app_state.as_mut() {
        state.window_count = state.window_count.saturating_sub(1);
        if state.window_count == 0 {
            unsafe {
                let app = shared_application();
                let _: () = msg_send![
                    app,
                    setPresentationOptions: state.saved_presentation_options
                ];
            }
            *app_state = None;
        }
    }
}

unsafe fn apply_simple_fullscreen_plan(
    native_window: id,
    native_view: id,
    plan: SimpleFullscreenPlan,
) {
    unsafe {
        match plan {
            SimpleFullscreenPlan::Exit(saved) => {
                pop_simple_fullscreen_presentation_options();
                let _: () = msg_send![native_window, setStyleMask: saved.style_mask];
                let _: () = msg_send![native_window, setFrame: saved.frame display: YES];
            }
            SimpleFullscreenPlan::Enter { screen_frame } => {
                push_simple_fullscreen_presentation_options();
                let _: () = msg_send![native_window, setStyleMask: NSWindowStyleMask::Borderless];
                let _: () = msg_send![native_window, setFrame: screen_frame display: YES];
            }
        }

        // Changing the style mask makes AppKit resign the window's key status and
        // first responder, so keyboard input stops reaching the editor. Re-make the
        // window key and restore the GPUI view as first responder.
        let _: () = msg_send![native_window, makeKeyAndOrderFront: nil];
        let _: () = msg_send![native_window, makeFirstResponder: native_view];
    }
}

struct MacWindowState {
    self_ref: Weak<Mutex<MacWindowState>>,
    handle: AnyWindowHandle,
    executor: ForegroundExecutor,
    native_window: id,
    native_view: NonNull<Object>,
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
    fn begin_close(&mut self) {
        self.is_closing = true;
        self.request_frame_callback.take();
        self.stop_display_link();
    }

    fn move_traffic_light(&self) {
        if let Some(traffic_light_position) = self.traffic_light_position {
            if self.is_fullscreen() {
                // Moving traffic lights while fullscreen doesn't work,
                // see https://github.com/zed-industries/zed/issues/4712
                return;
            }

            let titlebar_height = self.titlebar_height();

            unsafe {
                let close_button: id = msg_send![
                    self.native_window,
                    standardWindowButton: NSWindowButton::CloseButton
                ];
                let min_button: id = msg_send![
                    self.native_window,
                    standardWindowButton: NSWindowButton::MiniaturizeButton
                ];
                let zoom_button: id = msg_send![
                    self.native_window,
                    standardWindowButton: NSWindowButton::ZoomButton
                ];

                let mut close_button_frame: CGRect = msg_send![close_button, frame];
                let mut min_button_frame: CGRect = msg_send![min_button, frame];
                let mut zoom_button_frame: CGRect = msg_send![zoom_button, frame];
                let mut origin = point(
                    traffic_light_position.x,
                    titlebar_height
                        - traffic_light_position.y
                        - px(close_button_frame.size.height as f32),
                );
                let button_spacing =
                    px((min_button_frame.origin.x - close_button_frame.origin.x) as f32);

                close_button_frame.origin = CGPoint::new(origin.x.into(), origin.y.into());
                let _: () = msg_send![close_button, setFrame: close_button_frame];
                origin.x += button_spacing;

                min_button_frame.origin = CGPoint::new(origin.x.into(), origin.y.into());
                let _: () = msg_send![min_button, setFrame: min_button_frame];
                origin.x += button_spacing;

                zoom_button_frame.origin = CGPoint::new(origin.x.into(), origin.y.into());
                let _: () = msg_send![zoom_button, setFrame: zoom_button_frame];
                origin.x += button_spacing;
            }
        }
    }

    fn start_display_link(&mut self) {
        self.stop_display_link();
        if self.is_closing || self.request_frame_callback.is_none() {
            return;
        }
        unsafe {
            if !window_occlusion_state(self.native_window).contains(NSWindowOcclusionState::Visible)
            {
                return;
            }
        }
        let Some(display_id) =
            (unsafe { display_id_for_screen(window_screen(self.native_window)) })
        else {
            // AppKit can temporarily report no screen while displays are being reconfigured.
            return;
        };

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
        unsafe {
            let bounds = self.bounds();
            let screen_size = screen_visible_frame(window_screen(self.native_window)).into();
            bounds.size == screen_size
        }
    }

    fn is_fullscreen(&self) -> bool {
        unsafe {
            let style_mask = window_style_mask(self.native_window);
            style_mask.contains(NSWindowStyleMask::FullScreen)
        }
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
            let screen = unsafe { window_screen(self.native_window) };
            if screen == nil {
                return None;
            }
            let screen_frame = unsafe { screen_frame(screen) };

            self.simple_fullscreen_state = Some(SimpleFullscreenState {
                frame: unsafe { window_frame(self.native_window) },
                style_mask: unsafe { window_style_mask(self.native_window) },
            });

            Some(SimpleFullscreenPlan::Enter { screen_frame })
        }
    }

    fn bounds(&self) -> Bounds<Pixels> {
        let mut window_frame = unsafe { window_frame(self.native_window) };
        let screen = unsafe { window_screen(self.native_window) };
        if screen == nil {
            return Bounds::new(point(px(0.), px(0.)), crate::DEFAULT_WINDOW_SIZE);
        }
        let screen_frame = unsafe { screen_frame(screen) };

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
        let NSSize { width, height, .. } =
            unsafe { view_frame(window_content_view(self.native_window)) }.size;
        size(px(width as f32), px(height as f32))
    }

    fn scale_factor(&self) -> f32 {
        get_scale_factor(self.native_window)
    }

    fn titlebar_height(&self) -> Pixels {
        unsafe {
            let frame = window_frame(self.native_window);
            let content_layout_rect: CGRect = msg_send![self.native_window, contentLayoutRect];
            px((frame.size.height - content_layout_rect.size.height) as f32)
        }
    }

    fn window_bounds(&self) -> WindowBounds {
        if self.is_fullscreen() {
            WindowBounds::Fullscreen(self.fullscreen_restore_bounds)
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
            let pool: id = msg_send![class!(NSAutoreleasePool), new];

            let allows_automatic_window_tabbing = tabbing_identifier.is_some();
            if allows_automatic_window_tabbing {
                let () = msg_send![class!(NSWindow), setAllowsAutomaticWindowTabbing: YES];
            } else {
                let () = msg_send![class!(NSWindow), setAllowsAutomaticWindowTabbing: NO];
            }

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

            let native_window: id = match &kind {
                WindowKind::Normal | WindowKind::Floating => msg_send![WINDOW_CLASS, alloc],
                WindowKind::PopUp | WindowKind::Overlay => {
                    style_mask |= NSWindowStyleMask::NonactivatingPanel;
                    msg_send![PANEL_CLASS, alloc]
                }
            };

            let display = display_id
                .and_then(MacDisplay::find_by_id)
                .unwrap_or_else(MacDisplay::primary);

            let mut target_screen = nil;
            let mut selected_screen_frame = None;

            let screens: id = msg_send![class!(NSScreen), screens];
            let count = array_count(screens);
            for i in 0..count {
                let screen = array_object_at_index(screens, i);
                let Some(display_id) = display_id_for_screen(screen) else {
                    continue;
                };
                let frame = screen_frame(screen);
                if display_id == display.id() {
                    selected_screen_frame = Some(frame);
                    target_screen = screen;
                }
            }

            let screen_frame = selected_screen_frame.unwrap_or_else(|| {
                let screen: id = msg_send![class!(NSScreen), mainScreen];
                target_screen = screen;
                screen_frame(screen)
            });

            let window_rect = NSRect::new(
                NSPoint::new(
                    screen_frame.origin.x + f64::from(bounds.origin.x),
                    screen_frame.origin.y
                        + f64::from(display.bounds().size.height - bounds.origin.y),
                ),
                NSSize::new(f64::from(bounds.size.width), f64::from(bounds.size.height)),
            );

            let native_window: id = msg_send![
                native_window,
                initWithContentRect: window_rect
                styleMask: style_mask
                backing: NSBackingStoreType::Buffered
                defer: NO
                screen: target_screen
            ];
            assert!(!native_window.is_null());
            let dragged_types: id =
                msg_send![class!(NSArray), arrayWithObject: filenames_pboard_type()];
            let () = msg_send![
                native_window,
                registerForDraggedTypes: dragged_types
            ];
            let () = msg_send![
                native_window,
                setReleasedWhenClosed: NO
            ];

            let content_view = window_content_view(native_window);
            let native_view: id = msg_send![VIEW_CLASS, alloc];
            let native_view = view_init_with_frame(native_view, view_bounds(content_view));
            assert!(!native_view.is_null());

            let mut window = Self(
                Arc::new_cyclic(|self_ref| {
                    Mutex::new(MacWindowState {
                        self_ref: self_ref.clone(),
                        handle,
                        executor,
                        native_window,
                        native_view: NonNull::new_unchecked(native_view),
                        blurred_view: None,
                        cursor_style: CursorStyle::Arrow,
                        cursor_hidden: false,
                        frame_source: None,
                        renderer: renderer::new_renderer(
                            renderer_context,
                            native_window as *mut _,
                            native_view as *mut _,
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

            (*native_window).set_ivar(
                WINDOW_STATE_IVAR,
                Arc::into_raw(window.0.clone()) as *const c_void,
            );
            let _: () = msg_send![native_window, setDelegate: native_window];
            (*native_view).set_ivar(
                WINDOW_STATE_IVAR,
                Arc::into_raw(window.0.clone()) as *const c_void,
            );

            if let Some(title) = titlebar
                .as_ref()
                .and_then(|t| t.title.as_ref().map(AsRef::as_ref))
            {
                window.set_title(title);
            }

            let _: () = msg_send![native_window, setMovable: is_movable as BOOL];

            if let Some(window_min_size) = window_min_size {
                let _: () = msg_send![native_window, setContentMinSize: NSSize {
                    width: window_min_size.width.to_f64(),
                    height: window_min_size.height.to_f64(),
                }];
            }

            if titlebar.is_none_or(|titlebar| titlebar.appears_transparent) {
                let _: () = msg_send![native_window, setTitlebarAppearsTransparent: YES];
                let _: () =
                    msg_send![native_window, setTitleVisibility: NSWindowTitleVisibility::Hidden];
            }

            let _: () = msg_send![
                native_view,
                setAutoresizingMask:
                    NSAutoresizingMaskOptions::ViewWidthSizable
                        | NSAutoresizingMaskOptions::ViewHeightSizable
            ];
            let _: () = msg_send![native_view, setWantsBestResolutionOpenGLSurface: YES];

            // From winit crate: On Mojave, views automatically become layer-backed shortly after
            // being added to a native_window. Changing the layer-backedness of a view breaks the
            // association between the view and its associated OpenGL context. To work around this,
            // on we explicitly make the view layer-backed up front so that AppKit doesn't do it
            // itself and break the association with its context.
            let _: () = msg_send![native_view, setWantsLayer: YES];
            let _: () = msg_send![
            native_view,
            setLayerContentsRedrawPolicy: NSViewLayerContentsRedrawDuringViewResize
            ];

            let _: () = msg_send![content_view, addSubview: autorelease(native_view)];
            let _: () = msg_send![native_window, makeFirstResponder: native_view];

            match &kind {
                WindowKind::Normal | WindowKind::Floating => {
                    let _: () = msg_send![native_window, setLevel: NSNormalWindowLevel];
                    let _: () = msg_send![native_window, setAcceptsMouseMovedEvents: YES];

                    if let Some(tabbing_identifier) = tabbing_identifier {
                        let tabbing_id = ns_string(tabbing_identifier.as_str());
                        let _: () = msg_send![native_window, setTabbingIdentifier: tabbing_id];
                    } else {
                        let _: () = msg_send![native_window, setTabbingIdentifier:nil];
                    }
                }
                WindowKind::PopUp => {
                    // Use a tracking area to allow receiving MouseMoved events even when
                    // the window or application aren't active, which is often the case
                    // e.g. for notification windows.
                    let tracking_area: id = msg_send![class!(NSTrackingArea), alloc];
                    let _: () = msg_send![
                        tracking_area,
                        initWithRect: NSRect::new(NSPoint::new(0., 0.), NSSize::new(0., 0.))
                        options: NSTrackingMouseEnteredAndExited | NSTrackingMouseMoved | NSTrackingActiveAlways | NSTrackingInVisibleRect
                        owner: native_view
                        userInfo: nil
                    ];
                    let _: () = msg_send![native_view, addTrackingArea: autorelease(tracking_area)];

                    let _: () = msg_send![native_window, setLevel: NSPopUpWindowLevel];
                    let _: () = msg_send![
                        native_window,
                        setAnimationBehavior: NSWindowAnimationBehaviorUtilityWindow
                    ];
                    let _: () = msg_send![
                        native_window,
                        setCollectionBehavior:
                            NSWindowCollectionBehavior::CanJoinAllSpaces
                                | NSWindowCollectionBehavior::FullScreenAuxiliary
                    ];
                }
                WindowKind::Overlay => {
                    let tracking_area: id = msg_send![class!(NSTrackingArea), alloc];
                    let _: () = msg_send![
                        tracking_area,
                        initWithRect: NSRect::new(NSPoint::new(0., 0.), NSSize::new(0., 0.))
                        options: NSTrackingMouseEnteredAndExited | NSTrackingMouseMoved | NSTrackingActiveAlways | NSTrackingInVisibleRect
                        owner: native_view
                        userInfo: nil
                    ];
                    let _: () = msg_send![native_view, addTrackingArea: autorelease(tracking_area)];

                    let _: () = msg_send![native_window, setLevel: 25_isize];
                    let _: () = msg_send![
                        native_window,
                        setAnimationBehavior: NSWindowAnimationBehaviorUtilityWindow
                    ];
                    let _: () = msg_send![
                        native_window,
                        setCollectionBehavior:
                            NSWindowCollectionBehavior::CanJoinAllSpaces
                                | NSWindowCollectionBehavior::Stationary
                                | NSWindowCollectionBehavior::FullScreenAuxiliary
                    ];
                }
            }

            let app = shared_application();
            let main_window: id = msg_send![app, mainWindow];
            if allows_automatic_window_tabbing
                && !main_window.is_null()
                && main_window != native_window
            {
                let main_window_is_fullscreen =
                    window_style_mask(main_window).contains(NSWindowStyleMask::FullScreen);
                let user_tabbing_preference = Self::get_user_tabbing_preference()
                    .unwrap_or(UserTabbingPreference::InFullScreen);
                let should_add_as_tab = user_tabbing_preference == UserTabbingPreference::Always
                    || user_tabbing_preference == UserTabbingPreference::InFullScreen
                        && main_window_is_fullscreen;

                if should_add_as_tab {
                    let main_window_can_tab: BOOL =
                        msg_send![main_window, respondsToSelector: sel!(addTabbedWindow:ordered:)];
                    let main_window_visible: BOOL = msg_send![main_window, isVisible];

                    if main_window_can_tab == YES && main_window_visible == YES {
                        let _: () = msg_send![
                            main_window,
                            addTabbedWindow: native_window
                            ordered: NSWindowOrderingMode::Above
                        ];

                        // Ensure the window is visible immediately after adding the tab, since the tab bar is updated with a new entry at this point.
                        // Note: Calling orderFront here can break fullscreen mode (makes fullscreen windows exit fullscreen), so only do this if the main window is not fullscreen.
                        if !main_window_is_fullscreen {
                            let _: () = msg_send![native_window, orderFront: nil];
                        }
                    }
                }
            }

            if mouse_passthrough {
                let _: () = msg_send![native_window, setIgnoresMouseEvents: YES];
            }

            if focus && show {
                let _: () = msg_send![native_window, makeKeyAndOrderFront: nil];
            } else if show {
                let _: () = msg_send![native_window, orderFront: nil];
            }

            // Set the initial position of the window to the specified origin.
            // Although we already specified the position using `initWithContentRect_styleMask_backing_defer_screen_`,
            // the window position might be incorrect if the main screen (the screen that contains the window that has focus)
            //  is different from the primary screen.
            let _: () = msg_send![native_window, setFrameTopLeftPoint: window_rect.origin];
            window.0.lock().move_traffic_light();

            let _: () = msg_send![pool, drain];

            window
        }
    }

    pub fn active_window() -> Option<AnyWindowHandle> {
        unsafe {
            let app = shared_application();
            let main_window: id = msg_send![app, mainWindow];
            if main_window.is_null() {
                return None;
            }

            if msg_send![main_window, isKindOfClass: WINDOW_CLASS] {
                let handle = get_window_state(&*main_window).lock().handle;
                Some(handle)
            } else {
                None
            }
        }
    }

    pub fn ordered_windows() -> Vec<AnyWindowHandle> {
        unsafe {
            let app = shared_application();
            let windows: id = msg_send![app, orderedWindows];
            let count: NSUInteger = msg_send![windows, count];

            let mut window_handles = Vec::new();
            for i in 0..count {
                let window: id = msg_send![windows, objectAtIndex:i];
                if msg_send![window, isKindOfClass: WINDOW_CLASS] {
                    let handle = get_window_state(&*window).lock().handle;
                    window_handles.push(handle);
                }
            }

            window_handles
        }
    }

    pub fn get_user_tabbing_preference() -> Option<UserTabbingPreference> {
        unsafe {
            let defaults = user_defaults();
            let domain = ns_string("NSGlobalDomain");
            let key = ns_string("AppleWindowTabbingMode");

            let dict: id = msg_send![defaults, persistentDomainForName: domain];
            let value: id = if !dict.is_null() {
                msg_send![dict, objectForKey: key]
            } else {
                nil
            };

            let value_str = if !value.is_null() {
                value.to_str().to_string()
            } else {
                String::new()
            };

            match value_str.as_ref() {
                "manual" => Some(UserTabbingPreference::Never),
                "always" => Some(UserTabbingPreference::Always),
                _ => Some(UserTabbingPreference::InFullScreen),
            }
        }
    }
}

impl Drop for MacWindow {
    fn drop(&mut self) {
        let mut this = self.0.lock();
        let window = this.native_window;
        this.begin_close();
        this.frame_source.take();
        this.renderer.destroy();
        unsafe {
            let _: () = msg_send![this.native_window, setDelegate: nil];
        }
        this.input_handler.take();
        this.executor
            .spawn(async move {
                unsafe {
                    let _: () = msg_send![window, close];
                    autorelease(window);
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
        let window = this.native_window;
        this.executor
            .spawn(async move {
                unsafe {
                    let _: () = msg_send![window, setContentSize: NSSize {
                        width: f64::from(size.width),
                        height: f64::from(size.height),
                    }];
                }
            })
            .detach();
    }

    fn merge_all_windows(&self) {
        let native_window = self.0.lock().native_window;
        unsafe extern "C" fn merge_windows_async(context: *mut std::ffi::c_void) {
            let native_window = context as id;
            let _: () = msg_send![native_window, mergeAllWindows:nil];
        }

        unsafe {
            dispatch_async_f(
                dispatch_get_main_queue(),
                native_window as *mut std::ffi::c_void,
                Some(merge_windows_async),
            );
        }
    }

    fn move_tab_to_new_window(&self) {
        let native_window = self.0.lock().native_window;
        unsafe extern "C" fn move_tab_async(context: *mut std::ffi::c_void) {
            let native_window = context as id;
            let _: () = msg_send![native_window, moveTabToNewWindow:nil];
            let _: () = msg_send![native_window, makeKeyAndOrderFront: nil];
        }

        unsafe {
            dispatch_async_f(
                dispatch_get_main_queue(),
                native_window as *mut std::ffi::c_void,
                Some(move_tab_async),
            );
        }
    }

    fn toggle_window_tab_overview(&self) {
        let native_window = self.0.lock().native_window;
        unsafe {
            let _: () = msg_send![native_window, toggleTabOverview:nil];
        }
    }

    fn set_tabbing_identifier(&self, tabbing_identifier: Option<String>) {
        let native_window = self.0.lock().native_window;
        unsafe {
            let allows_automatic_window_tabbing = tabbing_identifier.is_some();
            if allows_automatic_window_tabbing {
                let () = msg_send![class!(NSWindow), setAllowsAutomaticWindowTabbing: YES];
            } else {
                let () = msg_send![class!(NSWindow), setAllowsAutomaticWindowTabbing: NO];
            }

            if let Some(tabbing_identifier) = tabbing_identifier {
                let tabbing_id = ns_string(tabbing_identifier.as_str());
                let _: () = msg_send![native_window, setTabbingIdentifier: tabbing_id];
            } else {
                let _: () = msg_send![native_window, setTabbingIdentifier:nil];
            }
        }
    }

    fn native_window_state(&self) -> Option<Vec<u8>> {
        let native_window = {
            let state = self.0.lock();
            if state.is_fullscreen() || state.simple_fullscreen_state.is_some() {
                return None;
            }
            state.native_window
        };
        // SAFETY: `native_window` is a live `NSWindow` retained by this window's state, and the
        // selectors below are AppKit/Foundation methods sent with their documented signatures. The
        // archived bytes are copied into an owned `Vec` before the objects we allocated are
        // released, so no pointer into Objective-C memory escapes this block.
        unsafe {
            let archiver: id = msg_send![class!(NSKeyedArchiver), alloc];
            let archiver: id = msg_send![archiver, initRequiringSecureCoding: YES];
            if archiver.is_null() {
                log::warn!("failed to create an archiver for the native window state");
                return None;
            }
            let delegate: id = msg_send![WINDOW_STATE_ARCHIVER_DELEGATE_CLASS, new];
            let _: () = msg_send![archiver, setDelegate: delegate];
            let _: () = msg_send![native_window, encodeRestorableStateWithCoder: archiver];
            let _: () = msg_send![archiver, finishEncoding];
            // The archiver holds a weak reference to its delegate; clear it before the delegate
            // is released below.
            let _: () = msg_send![archiver, setDelegate: nil];

            let data: id = msg_send![archiver, encodedData];
            let bytes: *const u8 = if data.is_null() {
                ptr::null()
            } else {
                let bytes: *const c_void = msg_send![data, bytes];
                bytes as *const u8
            };
            let state = if bytes.is_null() {
                log::warn!("the archiver produced no data for the native window state");
                None
            } else {
                let length: NSUInteger = msg_send![data, length];
                Some(std::slice::from_raw_parts(bytes, length).to_vec())
            };

            let _: () = msg_send![delegate, release];
            let _: () = msg_send![archiver, release];
            state
        }
    }

    fn restore_native_window_state(&self, state: &[u8]) {
        if state.is_empty() {
            return;
        }
        let native_window = self.0.lock().native_window;
        // SAFETY: `native_window` is a live `NSWindow` retained by this window's state. The NSData,
        // NSKeyedUnarchiver and `restoreStateWithCoder:` selectors are sent with their documented
        // signatures, and the `NSData` only borrows `state` for the duration of this synchronous
        // call (it is consumed before `state` could be freed).
        unsafe {
            let data: id = msg_send![
                class!(NSData),
                dataWithBytes: state.as_ptr() as *const c_void
                length: state.len() as NSUInteger
            ];
            if data.is_null() {
                log::warn!(
                    "failed to wrap {} bytes of native window state",
                    state.len()
                );
                return;
            }

            // On macOS < 15 the `NSWindowRestoresWorkspaceAtLaunch` user default controls whether
            // the window is restored to its original Space. On macOS 15+ that default is broken
            // (FB15644170), and the `_windowRestorationOptions` override on our unarchiver subclass
            // handles it instead.
            if !is_macos_version_at_least(NSOperatingSystemVersion::new(15, 0, 0)) {
                RESTORES_WORKSPACE_AT_LAUNCH_DEFAULT.call_once(|| {
                    let defaults = user_defaults();
                    let key = ns_string("NSWindowRestoresWorkspaceAtLaunch");
                    let yes_value: id = msg_send![class!(NSNumber), numberWithBool: YES];
                    let dict: id = msg_send![
                        class!(NSDictionary),
                        dictionaryWithObject: yes_value
                        forKey: key
                    ];
                    let _: () = msg_send![defaults, registerDefaults: dict];
                });
            }

            let unarchiver: id = msg_send![WINDOW_STATE_UNARCHIVER_CLASS, alloc];
            let mut error: id = nil;
            let unarchiver: id =
                msg_send![unarchiver, initForReadingFromData: data error: &mut error];
            if unarchiver.is_null() {
                log::warn!(
                    "failed to unarchive the native window state: {}",
                    ns_error_description(error)
                );
                return;
            }
            let _: () = msg_send![native_window, restoreStateWithCoder: unarchiver];
            let error: id = msg_send![unarchiver, error];
            if !error.is_null() {
                log::warn!(
                    "failed to restore the native window state: {}",
                    ns_error_description(error)
                );
            }
            let _: () = msg_send![unarchiver, release];
        }
    }

    fn scale_factor(&self) -> f32 {
        self.0.as_ref().lock().scale_factor()
    }

    fn appearance(&self) -> WindowAppearance {
        unsafe {
            let appearance: id = msg_send![self.0.lock().native_window, effectiveAppearance];
            super::window_appearance::from_native(appearance)
        }
    }

    fn display(&self) -> Option<Rc<dyn PlatformDisplay>> {
        unsafe {
            let screen = window_screen(self.0.lock().native_window);
            if screen.is_null() {
                return None;
            }
            let device_description = screen_device_description(screen);
            let screen_number: id =
                msg_send![device_description, objectForKey: ns_string("NSScreenNumber")];

            let screen_number: u32 = msg_send![screen_number, unsignedIntValue];

            Some(Rc::new(MacDisplay(screen_number)))
        }
    }

    fn mouse_position(&self) -> Point<Pixels> {
        let position: NSPoint = unsafe {
            msg_send![
                self.0.lock().native_window,
                mouseLocationOutsideOfEventStream
            ]
        };
        convert_mouse_position(position, self.content_size().height)
    }

    fn modifiers(&self) -> Modifiers {
        unsafe {
            let modifiers: NSEventModifierFlags = msg_send![class!(NSEvent), modifierFlags];

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
    }

    fn capslock(&self) -> Capslock {
        unsafe {
            let modifiers: NSEventModifierFlags = msg_send![class!(NSEvent), modifierFlags];

            Capslock {
                on: modifiers.contains(NSEventModifierFlags::CapsLock),
            }
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
        let native_window = lock.native_window;
        let executor = lock.executor.clone();
        executor
            .spawn(async move {
                // SAFETY: `native_window` is an Objective-C `NSWindow` pointer
                // owned by the platform window; bridge it into objc2.
                let sheet_window: &Objc2NSWindow =
                    unsafe { &*(native_window as *const Objc2NSWindow) };

                alert.beginSheetModalForWindow_completionHandler(sheet_window, Some(&block));
            })
            .detach();

        Some(done_rx)
    }

    fn activate(&self) {
        let window = self.0.lock().native_window;
        let executor = self.0.lock().executor.clone();
        executor
            .spawn(async move {
                unsafe {
                    let _: () = msg_send![window, makeKeyAndOrderFront: nil];
                }
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
        unsafe {
            let is_key_window: BOOL = msg_send![self.0.lock().native_window, isKeyWindow];
            is_key_window == YES
        }
    }

    // is_hovered is unused on macOS. See Window::is_window_hovered.
    fn is_hovered(&self) -> bool {
        false
    }

    fn set_title(&mut self, title: &str) {
        unsafe {
            let app = shared_application();
            let window = self.0.lock().native_window;
            let title = ns_string(title);
            let _: () = msg_send![app, changeWindowsItem:window title:title filename:false];
            let _: () = msg_send![window, setTitle: title];
            self.0.lock().move_traffic_light();
        }
    }

    fn get_title(&self) -> String {
        unsafe {
            let title: id = msg_send![self.0.lock().native_window, title];
            if title.is_null() {
                "".to_string()
            } else {
                title.to_str().to_string()
            }
        }
    }

    fn set_app_id(&mut self, _app_id: &str) {}

    fn set_background_appearance(&self, background_appearance: WindowBackgroundAppearance) {
        let mut this = self.0.as_ref().lock();

        let opaque = background_appearance == WindowBackgroundAppearance::Opaque;
        this.renderer.update_transparency(!opaque);

        unsafe {
            let _: () = msg_send![this.native_window, setOpaque: opaque as BOOL];
            let background_color: id = if opaque {
                msg_send![class!(NSColor), colorWithSRGBRed: 0f64 green: 0f64 blue: 0f64 alpha: 1f64]
            } else {
                // Not using `+[NSColor clearColor]` to avoid broken shadow.
                msg_send![class!(NSColor), colorWithSRGBRed: 0f64 green: 0f64 blue: 0f64 alpha: 0.0001]
            };
            let _: () = msg_send![this.native_window, setBackgroundColor: background_color];

            if background_appearance != WindowBackgroundAppearance::Blurred {
                if let Some(blur_view) = this.blurred_view {
                    let _: () = msg_send![blur_view, removeFromSuperview];
                    this.blurred_view = None;
                }
            } else if this.blurred_view.is_none() {
                let content_view = window_content_view(this.native_window);
                let frame = view_bounds(content_view);
                let mut blur_view: id = msg_send![BLURRED_VIEW_CLASS, alloc];
                blur_view = view_init_with_frame(blur_view, frame);
                let _: () = msg_send![
                    blur_view,
                    setAutoresizingMask:
                        NSAutoresizingMaskOptions::ViewWidthSizable
                            | NSAutoresizingMaskOptions::ViewHeightSizable
                ];

                let _: () = msg_send![
                    content_view,
                    addSubview: blur_view
                    positioned: NSWindowOrderingMode::Below
                    relativeTo: nil
                ];
                this.blurred_view = Some(autorelease(blur_view));
            }
        }
    }

    fn set_edited(&mut self, edited: bool) {
        unsafe {
            let window = self.0.lock().native_window;
            msg_send![window, setDocumentEdited: edited as BOOL]
        }

        // Changing the document edited state resets the traffic light position,
        // so we have to move it again.
        self.0.lock().move_traffic_light();
    }

    fn show_character_palette(&self) {
        let this = self.0.lock();
        let window = this.native_window;
        this.executor
            .spawn(async move {
                unsafe {
                    let app = shared_application();
                    let _: () = msg_send![app, orderFrontCharacterPalette: window];
                }
            })
            .detach();
    }

    fn minimize(&self) {
        let window = self.0.lock().native_window;
        unsafe {
            let _: () = msg_send![window, miniaturize: nil];
        }
    }

    fn zoom(&self) {
        let this = self.0.lock();
        let window = this.native_window;
        this.executor
            .spawn(async move {
                unsafe {
                    let _: () = msg_send![window, zoom: nil];
                }
            })
            .detach();
    }

    fn toggle_fullscreen(&self) {
        let this = self.0.lock();
        let window = this.native_window;
        this.executor
            .spawn(async move {
                unsafe {
                    let _: () = msg_send![window, toggleFullScreen: nil];
                }
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
                        lock.native_window,
                        lock.native_view.as_ptr() as id,
                        lock.toggle_simple_fullscreen(),
                    )
                };
                if let Some(plan) = plan {
                    unsafe { apply_simple_fullscreen_plan(native_window, native_view, plan) };
                }
            })
            .detach();
    }

    fn is_simple_fullscreen(&self) -> bool {
        self.0.lock().simple_fullscreen_state.is_some()
    }

    fn is_fullscreen(&self) -> bool {
        let this = self.0.lock();
        let window = this.native_window;

        unsafe { window_style_mask(window).contains(NSWindowStyleMask::FullScreen) }
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
        unsafe {
            let windows: id = msg_send![self.0.lock().native_window, tabbedWindows];
            if windows.is_null() {
                return None;
            }

            let count: NSUInteger = msg_send![windows, count];
            let mut result = Vec::new();
            for i in 0..count {
                let window: id = msg_send![windows, objectAtIndex:i];
                if msg_send![window, isKindOfClass: WINDOW_CLASS] {
                    let handle = get_window_state(&*window).lock().handle;
                    let title: id = msg_send![window, title];
                    let title = SharedString::from(title.to_str().to_string());

                    result.push(SystemWindowTab::new(title, handle));
                }
            }

            Some(result)
        }
    }

    fn tab_bar_visible(&self) -> bool {
        unsafe {
            let tab_group: id = msg_send![self.0.lock().native_window, tabGroup];
            if tab_group.is_null() {
                false
            } else {
                let tab_bar_visible: BOOL = msg_send![tab_group, isTabBarVisible];
                tab_bar_visible == YES
            }
        }
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
                unsafe {
                    let input_context: id =
                        msg_send![class!(NSTextInputContext), currentInputContext];
                    if input_context.is_null() {
                        return;
                    }
                    let _: () = msg_send![input_context, invalidateCharacterCoordinates];
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
                lock.native_window as *mut c_void,
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
        unsafe {
            let _: () = msg_send![self.0.lock().native_window, makeKeyAndOrderFront: nil];
        }
    }

    fn hide(&self) {
        unsafe {
            let _: () = msg_send![self.0.lock().native_window, orderOut: nil];
        }
    }

    fn is_visible(&self) -> bool {
        unsafe { msg_send![self.0.lock().native_window, isVisible] }
    }

    fn set_mouse_passthrough(&self, passthrough: bool) {
        unsafe {
            let _: () =
                msg_send![self.0.lock().native_window, setIgnoresMouseEvents: passthrough as BOOL];
        }
    }

    fn titlebar_double_click(&self) {
        let this = self.0.lock();
        if this.simple_fullscreen_state.is_some() {
            return;
        }
        let window = this.native_window;
        this.executor
            .spawn(async move {
                unsafe {
                    let defaults = user_defaults();
                    let domain = ns_string("NSGlobalDomain");
                    let key = ns_string("AppleActionOnDoubleClick");

                    let dict: id = msg_send![defaults, persistentDomainForName: domain];
                    let action: id = if !dict.is_null() {
                        msg_send![dict, objectForKey: key]
                    } else {
                        nil
                    };

                    let action_str = if !action.is_null() {
                        action.to_str().to_string()
                    } else {
                        String::new()
                    };

                    match action_str.as_ref() {
                        "None" => {
                            // "Do Nothing" selected, so do no action
                        }
                        "Minimize" => {
                            let _: () = msg_send![window, miniaturize: nil];
                        }
                        "Maximize" => {
                            let _: () = msg_send![window, zoom: nil];
                        }
                        "Fill" => {
                            // Unlike `zoom:`, AppKit's private Fill action honors the system's
                            // "Tiled windows have margins" setting.
                            let responds_to_zoom_fill: BOOL =
                                msg_send![window, respondsToSelector: sel!(_zoomFill:)];
                            if responds_to_zoom_fill == YES {
                                let _: () = msg_send![window, _zoomFill: nil];
                            } else {
                                let _: () = msg_send![window, zoom: nil];
                            }
                        }
                        _ => {
                            let _: () = msg_send![window, zoom: nil];
                        }
                    }
                }
            })
            .detach();
    }

    fn set_progress_bar(&self, state: crate::ProgressBarState) {
        unsafe {
            let app: id = msg_send![class!(NSApplication), sharedApplication];
            let dock_tile: id = msg_send![app, dockTile];
            if dock_tile == nil {
                return;
            }

            match state {
                crate::ProgressBarState::None => {
                    let _: () = msg_send![dock_tile, setContentView: nil];
                    let _: () = msg_send![dock_tile, setBadgeLabel: nil];
                    let _: () = msg_send![dock_tile, display];
                }
                crate::ProgressBarState::Indeterminate => {
                    let indicator: id = msg_send![class!(NSProgressIndicator), alloc];
                    let frame = NSRect {
                        origin: NSPoint::new(0.0, 0.0),
                        size: NSSize::new(140.0, 140.0),
                    };
                    let indicator: id = msg_send![indicator, initWithFrame: frame];
                    let _: () = msg_send![indicator, setStyle: 0i64]; // NSProgressIndicatorBarStyle
                    let _: () = msg_send![indicator, setIndeterminate: YES];
                    let _: () = msg_send![indicator, startAnimation: nil];
                    let _: () = msg_send![dock_tile, setContentView: indicator];
                    let _: () = msg_send![indicator, release];
                    let _: () = msg_send![dock_tile, display];
                }
                crate::ProgressBarState::Normal(pct)
                | crate::ProgressBarState::Error(pct)
                | crate::ProgressBarState::Paused(pct) => {
                    let indicator: id = msg_send![class!(NSProgressIndicator), alloc];
                    let frame = NSRect {
                        origin: NSPoint::new(0.0, 0.0),
                        size: NSSize::new(140.0, 140.0),
                    };
                    let indicator: id = msg_send![indicator, initWithFrame: frame];
                    let _: () = msg_send![indicator, setStyle: 0i64]; // NSProgressIndicatorBarStyle
                    let _: () = msg_send![indicator, setIndeterminate: NO];
                    let _: () = msg_send![indicator, setMinValue: 0.0f64];
                    let _: () = msg_send![indicator, setMaxValue: 100.0f64];
                    let _: () = msg_send![indicator, setDoubleValue: pct * 100.0];
                    let _: () = msg_send![dock_tile, setContentView: indicator];
                    let _: () = msg_send![indicator, release];
                    let _: () = msg_send![dock_tile, display];
                }
            }
        }
    }

    fn set_cursor_style(&self, style: CursorStyle) {
        let mut state = self.0.lock();
        if state.cursor_style != style {
            // If transitioning away from hidden cursor, unhide immediately rather than
            // waiting for resetCursorRects, so other windows/apps aren't affected.
            if state.cursor_hidden && !matches!(style, CursorStyle::None) {
                unsafe {
                    let _: () = msg_send![class!(NSCursor), unhide];
                }
                state.cursor_hidden = false;
            }
            state.cursor_style = style;
            let native_window = state.native_window;
            let native_view = state.native_view.as_ptr();
            drop(state);
            unsafe {
                let _: () = msg_send![native_window, invalidateCursorRectsForView: native_view];
            }
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

fn get_scale_factor(native_window: id) -> f32 {
    let factor = unsafe {
        let screen = window_screen(native_window);
        if screen.is_null() {
            return 2.0;
        }
        screen_backing_scale_factor(screen) as f32
    };

    // We are not certain what triggers this, but it seems that sometimes
    // this method would return 0 (https://github.com/zed-industries/zed/issues/6412)
    // It seems most likely that this would happen if the window has no screen
    // (if it is off-screen), though we'd expect to see viewDidChangeBackingProperties before
    // it was rendered for real.
    // Regardless, attempt to avoid the issue here.
    if factor == 0.0 { 2. } else { factor }
}

unsafe fn get_window_state(object: &Object) -> Arc<Mutex<MacWindowState>> {
    unsafe {
        let raw: *mut c_void = *object.get_ivar(WINDOW_STATE_IVAR);
        let rc1 = Arc::from_raw(raw as *mut Mutex<MacWindowState>);
        let rc2 = rc1.clone();
        mem::forget(rc1);
        rc2
    }
}

unsafe fn drop_window_state(object: &Object) {
    unsafe {
        let raw: *mut c_void = *object.get_ivar(WINDOW_STATE_IVAR);
        Arc::from_raw(raw as *mut Mutex<MacWindowState>);
    }
}

extern "C" fn yes(_: &Object, _: Sel) -> BOOL {
    YES
}

extern "C" fn dealloc_window(this: &Object, _: Sel) {
    unsafe {
        drop_window_state(this);
        let _: () = msg_send![super(this, class!(NSWindow)), dealloc];
    }
}

extern "C" fn dealloc_view(this: &Object, _: Sel) {
    unsafe {
        // If the cursor was hidden by this view, restore it before dealloc to prevent
        // NSCursor's global hide count from leaking.
        let window_state = get_window_state(this);
        let mut state = window_state.lock();
        if state.cursor_hidden {
            let _: () = msg_send![class!(NSCursor), unhide];
            state.cursor_hidden = false;
        }
        drop(state);
        drop_window_state(this);
        let _: () = msg_send![super(this, class!(NSView)), dealloc];
    }
}

extern "C" fn reset_cursor_rects(this: &Object, _: Sel) {
    // SAFETY: AppKit invokes cursor-rect updates on the main thread for GPUIView instances,
    // whose WINDOW_STATE_IVAR is initialized when the view is created. The cursor registered
    // below is a valid NSCursor.
    unsafe {
        let _: () = msg_send![super(this, class!(NSView)), resetCursorRects];

        let window_state = get_window_state(this);
        let cursor_style;
        let cursor_hidden;

        {
            let mut window_state = window_state.lock();

            if matches!(window_state.cursor_style, CursorStyle::None) {
                if !window_state.cursor_hidden {
                    let _: () = msg_send![class!(NSCursor), hide];
                    window_state.cursor_hidden = true;
                }
                return;
            }

            cursor_style = window_state.cursor_style;
            cursor_hidden = window_state.cursor_hidden;
        };

        let cursor: id = match cursor_style {
            CursorStyle::Arrow => msg_send![class!(NSCursor), arrowCursor],
            CursorStyle::IBeam => msg_send![class!(NSCursor), IBeamCursor],
            CursorStyle::Crosshair => msg_send![class!(NSCursor), crosshairCursor],
            CursorStyle::ClosedHand => msg_send![class!(NSCursor), closedHandCursor],
            CursorStyle::OpenHand => msg_send![class!(NSCursor), openHandCursor],
            CursorStyle::PointingHand => msg_send![class!(NSCursor), pointingHandCursor],
            CursorStyle::ResizeLeftRight => msg_send![class!(NSCursor), resizeLeftRightCursor],
            CursorStyle::ResizeUpDown => msg_send![class!(NSCursor), resizeUpDownCursor],
            CursorStyle::ResizeLeft => msg_send![class!(NSCursor), resizeLeftCursor],
            CursorStyle::ResizeRight => msg_send![class!(NSCursor), resizeRightCursor],
            CursorStyle::ResizeColumn => msg_send![class!(NSCursor), resizeLeftRightCursor],
            CursorStyle::ResizeRow => msg_send![class!(NSCursor), resizeUpDownCursor],
            CursorStyle::ResizeUp => msg_send![class!(NSCursor), resizeUpCursor],
            CursorStyle::ResizeDown => msg_send![class!(NSCursor), resizeDownCursor],

            // Undocumented, private class methods:
            // https://stackoverflow.com/questions/27242353/cocoa-predefined-resize-mouse-cursor
            CursorStyle::ResizeUpLeftDownRight => {
                msg_send![class!(NSCursor), _windowResizeNorthWestSouthEastCursor]
            }
            CursorStyle::ResizeUpRightDownLeft => {
                msg_send![class!(NSCursor), _windowResizeNorthEastSouthWestCursor]
            }

            CursorStyle::IBeamCursorForVerticalLayout => {
                msg_send![class!(NSCursor), IBeamCursorForVerticalLayout]
            }
            CursorStyle::OperationNotAllowed => {
                msg_send![class!(NSCursor), operationNotAllowedCursor]
            }
            CursorStyle::DragLink => msg_send![class!(NSCursor), dragLinkCursor],
            CursorStyle::DragCopy => msg_send![class!(NSCursor), dragCopyCursor],
            CursorStyle::ContextualMenu => msg_send![class!(NSCursor), contextualMenuCursor],
            CursorStyle::None => unreachable!(),
        };

        if cursor_hidden {
            let _: () = msg_send![class!(NSCursor), unhide];
            window_state.lock().cursor_hidden = false;
        }

        let bounds: NSRect = msg_send![this, bounds];
        let _: () = msg_send![this, addCursorRect: bounds cursor: cursor];
    }
}

extern "C" fn handle_key_equivalent(this: &Object, _: Sel, native_event: id) -> BOOL {
    handle_key_event(this, native_event, true)
}

extern "C" fn handle_key_down(this: &Object, _: Sel, native_event: id) {
    handle_key_event(this, native_event, false);
}

extern "C" fn handle_key_up(this: &Object, _: Sel, native_event: id) {
    handle_key_event(this, native_event, false);
}

// Things to test if you're modifying this method:
//  U.S. layout:
//   - The IME consumes characters like 'j' and 'k', which makes paging through `less` in
//     the terminal behave incorrectly by default. This behavior should be patched by our
//     IME integration
//   - `alt-t` should open the tasks menu
//   - In vim mode, this keybinding should work:
//     ```
//        {
//          "context": "Editor && vim_mode == insert",
//          "bindings": {"j j": "vim::NormalBefore"}
//        }
//     ```
//     and typing 'j k' in insert mode with this keybinding should insert the two characters
//  Brazilian layout:
//   - `" space` should create an unmarked quote
//   - `" backspace` should delete the marked quote
//   - `" "`should create an unmarked quote and a second marked quote
//   - `" up` should insert a quote, unmark it, and move up one line
//   - `" cmd-down` should insert a quote, unmark it, and move to the end of the file
//   - `cmd-ctrl-space` and clicking on an emoji should type it
//  Czech (QWERTY) layout:
//   - in vim mode `option-4`  should go to end of line (same as $)
//  Japanese (Romaji) layout:
//   - type `a i left down up enter enter` should create an unmarked text "愛"
extern "C" fn handle_key_event(this: &Object, native_event: id, key_equivalent: bool) -> BOOL {
    let window_state = unsafe { get_window_state(this) };
    let mut lock = window_state.as_ref().lock();

    let window_height = lock.content_size().height;
    let event =
        unsafe { super::events::platform_input_from_native(native_event, Some(window_height)) };

    let Some(event) = event else {
        return NO;
    };

    let run_callback = |event: PlatformInput| -> BOOL {
        let mut callback = window_state.as_ref().lock().event_callback.take();
        let handled: BOOL = if let Some(callback) = callback.as_mut() {
            !callback(event).propagate as BOOL
        } else {
            NO
        };
        window_state.as_ref().lock().event_callback = callback;
        handled
    };

    match event {
        PlatformInput::KeyDown(mut key_down_event) => {
            // For certain keystrokes, macOS will first dispatch a "key equivalent" event.
            // If that event isn't handled, it will then dispatch a "key down" event. GPUI
            // makes no distinction between these two types of events, so we need to ignore
            // the "key down" event if we've already just processed its "key equivalent" version.
            if key_equivalent {
                lock.last_key_equivalent = Some(key_down_event.clone());
            } else if lock.last_key_equivalent.take().as_ref() == Some(&key_down_event) {
                return NO;
            }

            drop(lock);

            let is_composing =
                with_input_handler(this, |input_handler| input_handler.marked_text_range())
                    .flatten()
                    .is_some();

            // If we're composing, send the key to the input handler first;
            // otherwise we only send to the input handler if we don't have a matching binding.
            // The input handler may call `do_command_by_selector` if it doesn't know how to handle
            // a key. If it does so, it will return YES so we won't send the key twice.
            // We also do this for non-printing keys (like arrow keys and escape) as the IME menu
            // may need them even if there is no marked text;
            // however we skip keys with control or the input handler adds control-characters to the buffer.
            // and keys with function, as the input handler swallows them.
            if is_composing
                || (key_down_event.keystroke.key_char.is_none()
                    && !key_down_event.keystroke.modifiers.control
                    && !key_down_event.keystroke.modifiers.function)
            {
                {
                    let mut lock = window_state.as_ref().lock();
                    lock.keystroke_for_do_command = Some(key_down_event.keystroke.clone());
                    lock.do_command_handled.take();
                    drop(lock);
                }

                let handled: BOOL = unsafe {
                    let input_context: id = msg_send![this, inputContext];
                    msg_send![input_context, handleEvent: native_event]
                };
                window_state.as_ref().lock().keystroke_for_do_command.take();
                if let Some(handled) = window_state.as_ref().lock().do_command_handled.take() {
                    return handled as BOOL;
                } else if handled == YES {
                    return YES;
                }

                let handled = run_callback(PlatformInput::KeyDown(key_down_event));
                return handled;
            }

            let handled = run_callback(PlatformInput::KeyDown(key_down_event.clone()));
            if handled == YES {
                return YES;
            }

            if key_down_event.is_held
                && let Some(key_char) = key_down_event.keystroke.key_char.as_ref()
            {
                let handled = with_input_handler(this, |input_handler| {
                    if !input_handler.apple_press_and_hold_enabled() {
                        input_handler.replace_text_in_range(None, key_char);
                        return YES;
                    }
                    NO
                });
                if handled == Some(YES) {
                    return YES;
                }
            }

            // Don't send key equivalents to the input handler,
            // or macOS shortcuts like cmd-` will stop working.
            if key_equivalent {
                return NO;
            }

            unsafe {
                let input_context: id = msg_send![this, inputContext];
                msg_send![input_context, handleEvent: native_event]
            }
        }

        PlatformInput::KeyUp(_) => {
            drop(lock);
            run_callback(event)
        }

        _ => NO,
    }
}

extern "C" fn handle_view_event(this: &Object, _: Sel, native_event: id) {
    let window_state = unsafe { get_window_state(this) };
    let weak_window_state = Arc::downgrade(&window_state);
    let mut lock = window_state.as_ref().lock();
    let window_height = lock.content_size().height;
    let event =
        unsafe { super::events::platform_input_from_native(native_event, Some(window_height)) };

    if let Some(mut event) = event {
        match &mut event {
            PlatformInput::MouseDown(
                event @ MouseDownEvent {
                    button: MouseButton::Left,
                    modifiers: Modifiers { control: true, .. },
                    ..
                },
            ) => {
                // On mac, a ctrl-left click should be handled as a right click.
                *event = MouseDownEvent {
                    button: MouseButton::Right,
                    modifiers: Modifiers {
                        control: false,
                        ..event.modifiers
                    },
                    click_count: 1,
                    ..*event
                };
            }

            // Handles focusing click.
            PlatformInput::MouseDown(
                event @ MouseDownEvent {
                    button: MouseButton::Left,
                    ..
                },
            ) if (lock.first_mouse) => {
                *event = MouseDownEvent {
                    first_mouse: true,
                    ..*event
                };
                lock.first_mouse = false;
            }

            // Because we map a ctrl-left_down to a right_down -> right_up let's ignore
            // the ctrl-left_up to avoid having a mismatch in button down/up events if the
            // user is still holding ctrl when releasing the left mouse button
            PlatformInput::MouseUp(
                event @ MouseUpEvent {
                    button: MouseButton::Left,
                    modifiers: Modifiers { control: true, .. },
                    ..
                },
            ) => {
                *event = MouseUpEvent {
                    button: MouseButton::Right,
                    modifiers: Modifiers {
                        control: false,
                        ..event.modifiers
                    },
                    click_count: 1,
                    ..*event
                };
            }

            _ => {}
        };

        match &event {
            PlatformInput::MouseDown(_) => {
                drop(lock);
                unsafe {
                    let input_context: id = msg_send![this, inputContext];
                    msg_send![input_context, handleEvent: native_event]
                }
                lock = window_state.as_ref().lock();
            }
            PlatformInput::MouseMove(
                event @ MouseMoveEvent {
                    pressed_button: Some(_),
                    ..
                },
            ) => {
                // Synthetic drag is used for selecting long buffer contents while buffer is being scrolled.
                // External file drag and drop is able to emit its own synthetic mouse events which will conflict
                // with these ones.
                if !lock.external_files_dragged {
                    lock.synthetic_drag_counter += 1;
                    let executor = lock.executor.clone();
                    executor
                        .spawn(synthetic_drag(
                            weak_window_state,
                            lock.synthetic_drag_counter,
                            event.clone(),
                        ))
                        .detach();
                }
            }

            PlatformInput::MouseUp(MouseUpEvent { .. }) => {
                lock.synthetic_drag_counter += 1;
            }

            PlatformInput::ModifiersChanged(ModifiersChangedEvent {
                modifiers,
                capslock,
            }) => {
                // Only raise modifiers changed event when they have actually changed
                if let Some(PlatformInput::ModifiersChanged(ModifiersChangedEvent {
                    modifiers: prev_modifiers,
                    capslock: prev_capslock,
                })) = &lock.previous_modifiers_changed_event
                    && prev_modifiers == modifiers
                    && prev_capslock == capslock
                {
                    return;
                }

                lock.previous_modifiers_changed_event = Some(event.clone());
            }

            _ => {}
        }

        if let Some(mut callback) = lock.event_callback.take() {
            drop(lock);
            callback(event);
            window_state.lock().event_callback = Some(callback);
        }
    }
}

extern "C" fn window_did_change_occlusion_state(this: &Object, _: Sel, _: id) {
    let window_state = unsafe { get_window_state(this) };
    let lock = &mut *window_state.lock();
    unsafe {
        if window_occlusion_state(lock.native_window).contains(NSWindowOcclusionState::Visible) {
            lock.move_traffic_light();
            lock.start_display_link();
        } else {
            lock.stop_display_link();
        }
    }
}

extern "C" fn window_did_resize(this: &Object, _: Sel, _: id) {
    let window_state = unsafe { get_window_state(this) };
    window_state.as_ref().lock().move_traffic_light();
}

extern "C" fn window_will_enter_fullscreen(this: &Object, _: Sel, _: id) {
    let window_state = unsafe { get_window_state(this) };
    let mut lock = window_state.as_ref().lock();
    lock.fullscreen_restore_bounds = lock.bounds();

    let min_version = NSOperatingSystemVersion::new(15, 3, 0);

    if is_macos_version_at_least(min_version) {
        unsafe {
            let _: () = msg_send![lock.native_window, setTitlebarAppearsTransparent: NO];
        }
    }
}

extern "C" fn window_will_exit_fullscreen(this: &Object, _: Sel, _: id) {
    let window_state = unsafe { get_window_state(this) };
    let mut lock = window_state.as_ref().lock();

    let min_version = NSOperatingSystemVersion::new(15, 3, 0);

    if is_macos_version_at_least(min_version) && lock.transparent_titlebar {
        unsafe {
            let _: () = msg_send![lock.native_window, setTitlebarAppearsTransparent: YES];
        }
    }
}

pub(crate) fn is_macos_version_at_least(version: NSOperatingSystemVersion) -> bool {
    unsafe {
        let process_info: id = msg_send![class!(NSProcessInfo), processInfo];
        msg_send![process_info, isOperatingSystemAtLeastVersion: version]
    }
}

fn ns_error_description(error: id) -> String {
    if error.is_null() {
        return "unknown error".to_owned();
    }
    unsafe {
        let description: id = msg_send![error, localizedDescription];
        description.to_str().to_owned()
    }
}

extern "C" fn window_did_move(this: &Object, _: Sel, _: id) {
    let window_state = unsafe { get_window_state(this) };
    let mut lock = window_state.as_ref().lock();
    if let Some(mut callback) = lock.moved_callback.take() {
        drop(lock);
        callback();
        window_state.lock().moved_callback = Some(callback);
    }
}

extern "C" fn window_did_change_screen(this: &Object, _: Sel, _: id) {
    let window_state = unsafe { get_window_state(this) };
    let mut lock = window_state.as_ref().lock();
    lock.start_display_link();
}

extern "C" fn window_did_change_key_status(this: &Object, selector: Sel, _: id) {
    let window_state = unsafe { get_window_state(this) };
    let mut lock = window_state.lock();
    let is_active = unsafe {
        let is_key_window: BOOL = msg_send![lock.native_window, isKeyWindow];
        is_key_window == YES
    };

    // When opening a pop-up while the application isn't active, Cocoa sends a spurious
    // `windowDidBecomeKey` message to the previous key window even though that window
    // isn't actually key. This causes a bug if the application is later activated while
    // the pop-up is still open, making it impossible to activate the previous key window
    // even if the pop-up gets closed. The only way to activate it again is to de-activate
    // the app and re-activate it, which is a pretty bad UX.
    // The following code detects the spurious event and invokes `resignKeyWindow`:
    // in theory, we're not supposed to invoke this method manually but it balances out
    // the spurious `becomeKeyWindow` event and helps us work around that bug.
    if selector == sel!(windowDidBecomeKey:) && !is_active {
        unsafe {
            let _: () = msg_send![lock.native_window, resignKeyWindow];
            return;
        }
    }

    let executor = lock.executor.clone();
    drop(lock);

    #[cfg(feature = "accessibility")]
    {
        let a11y_events = {
            let mut lock = window_state.lock();
            lock.accesskit_adapter
                .as_mut()
                .and_then(|adapter| adapter.update_view_focus_state(is_active))
        };
        if let Some(events) = a11y_events {
            events.raise();
        }
    }

    // When a window becomes active, trigger an immediate synchronous frame request to prevent
    // tab flicker when switching between windows in native tabs mode.
    //
    // This is only done on subsequent activations (not the first) to ensure the initial focus
    // path is properly established. Without this guard, the focus state would remain unset until
    // the first mouse click, causing keybindings to be non-functional.
    if selector == sel!(windowDidBecomeKey:) && is_active {
        let window_state = unsafe { get_window_state(this) };
        let mut lock = window_state.lock();

        if lock.activated_least_once {
            if let Some(mut callback) = lock.request_frame_callback.take() {
                lock.renderer.set_presents_with_transaction(true);
                lock.stop_display_link();
                drop(lock);
                callback(Default::default());

                let mut lock = window_state.lock();
                if !lock.is_closing {
                    lock.request_frame_callback = Some(callback);
                    lock.renderer.set_presents_with_transaction(false);
                    lock.start_display_link();
                }
            }
        } else {
            lock.activated_least_once = true;
        }
    }

    executor
        .spawn(async move {
            let mut lock = window_state.as_ref().lock();
            if is_active {
                lock.move_traffic_light();
            }

            if let Some(mut callback) = lock.activate_callback.take() {
                drop(lock);
                callback(is_active);
                window_state.lock().activate_callback = Some(callback);
            };
        })
        .detach();
}

extern "C" fn window_should_close(this: &Object, _: Sel, _: id) -> BOOL {
    let window_state = unsafe { get_window_state(this) };
    let mut lock = window_state.as_ref().lock();
    if let Some(mut callback) = lock.should_close_callback.take() {
        drop(lock);
        let should_close = callback();
        window_state.lock().should_close_callback = Some(callback);
        should_close as BOOL
    } else {
        YES
    }
}

extern "C" fn close_window(this: &Object, _: Sel) {
    unsafe {
        let (close_callback, simple_fullscreen_state) = {
            let window_state = get_window_state(this);
            let mut lock = window_state.as_ref().lock();
            lock.begin_close();
            (
                lock.close_callback.take(),
                lock.simple_fullscreen_state.take(),
            )
        };

        if simple_fullscreen_state.is_some() {
            pop_simple_fullscreen_presentation_options();
        }

        if let Some(callback) = close_callback {
            callback();
        }

        let _: () = msg_send![super(this, class!(NSWindow)), close];
    }
}

extern "C" fn make_backing_layer(this: &Object, _: Sel) -> id {
    let window_state = unsafe { get_window_state(this) };
    let window_state = window_state.as_ref().lock();
    window_state.renderer.layer_ptr() as id
}

extern "C" fn view_did_change_backing_properties(this: &Object, _: Sel) {
    let window_state = unsafe { get_window_state(this) };
    let mut lock = window_state.as_ref().lock();

    let scale_factor = lock.scale_factor();
    let size = lock.content_size();
    let drawable_size = size.to_device_pixels(scale_factor);
    unsafe {
        let _: () = msg_send![
            lock.renderer.layer(),
            setContentsScale: scale_factor as f64
        ];
    }

    lock.renderer.update_drawable_size(drawable_size);

    if let Some(mut callback) = lock.resize_callback.take() {
        let content_size = lock.content_size();
        let scale_factor = lock.scale_factor();
        drop(lock);
        callback(content_size, scale_factor);
        window_state.as_ref().lock().resize_callback = Some(callback);
    };
}

extern "C" fn set_frame_size(this: &Object, _: Sel, size: NSSize) {
    let window_state = unsafe { get_window_state(this) };
    let mut lock = window_state.as_ref().lock();

    let new_size = Size::<Pixels>::from(size);
    let old_size = unsafe {
        let old_frame: NSRect = msg_send![this, frame];
        Size::<Pixels>::from(old_frame.size)
    };

    if old_size == new_size {
        return;
    }

    unsafe {
        let _: () = msg_send![super(this, class!(NSView)), setFrameSize: size];
    }

    let scale_factor = lock.scale_factor();
    let drawable_size = new_size.to_device_pixels(scale_factor);
    lock.renderer.update_drawable_size(drawable_size);

    if let Some(mut callback) = lock.resize_callback.take() {
        let content_size = lock.content_size();
        let scale_factor = lock.scale_factor();
        drop(lock);
        callback(content_size, scale_factor);
        window_state.lock().resize_callback = Some(callback);
    };
}

extern "C" fn display_layer(this: &Object, _: Sel, _: id) {
    let window_state = unsafe { get_window_state(this) };
    let mut lock = window_state.lock();
    if let Some(mut callback) = lock.request_frame_callback.take() {
        lock.renderer.set_presents_with_transaction(true);
        lock.stop_display_link();
        drop(lock);
        callback(Default::default());

        let mut lock = window_state.lock();
        if !lock.is_closing {
            lock.request_frame_callback = Some(callback);
            lock.renderer.set_presents_with_transaction(false);
            lock.start_display_link();
        }
    }
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

extern "C" fn valid_attributes_for_marked_text(_: &Object, _: Sel) -> id {
    unsafe { msg_send![class!(NSArray), array] }
}

extern "C" fn has_marked_text(this: &Object, _: Sel) -> BOOL {
    let has_marked_text_result =
        with_input_handler(this, |input_handler| input_handler.marked_text_range()).flatten();

    has_marked_text_result.is_some() as BOOL
}

extern "C" fn marked_range(this: &Object, _: Sel) -> NSRange {
    let marked_range_result =
        with_input_handler(this, |input_handler| input_handler.marked_text_range()).flatten();

    marked_range_result.map_or(NSRange::invalid(), |range| range.into())
}

extern "C" fn selected_range(this: &Object, _: Sel) -> NSRange {
    let selected_range_result = with_input_handler(this, |input_handler| {
        input_handler.selected_text_range(false)
    })
    .flatten();

    selected_range_result.map_or(NSRange::invalid(), |selection| selection.range.into())
}

extern "C" fn first_rect_for_character_range(
    this: &Object,
    _: Sel,
    range: NSRange,
    _: id,
) -> NSRect {
    let frame = get_frame(this);
    with_input_handler(this, |input_handler| {
        input_handler.bounds_for_range(range.to_range()?)
    })
    .flatten()
    .map_or(
        NSRect::new(NSPoint::new(0., 0.), NSSize::new(0., 0.)),
        |bounds| {
            NSRect::new(
                NSPoint::new(
                    frame.origin.x + f64::from(bounds.origin.x),
                    frame.origin.y + frame.size.height
                        - f64::from(bounds.origin.y)
                        - f64::from(bounds.size.height),
                ),
                NSSize::new(f64::from(bounds.size.width), f64::from(bounds.size.height)),
            )
        },
    )
}

fn get_frame(this: &Object) -> NSRect {
    unsafe {
        let state = get_window_state(this);
        let lock = state.lock();
        let mut frame = window_frame(lock.native_window);
        let content_layout_rect: CGRect = msg_send![lock.native_window, contentLayoutRect];
        let style_mask = window_style_mask(lock.native_window);
        if !style_mask.contains(NSWindowStyleMask::FullSizeContentView) {
            frame.origin.y -= frame.size.height - content_layout_rect.size.height;
        }
        frame
    }
}

extern "C" fn insert_text(this: &Object, _: Sel, text: id, replacement_range: NSRange) {
    unsafe {
        let is_attributed_string: BOOL =
            msg_send![text, isKindOfClass: [class!(NSAttributedString)]];
        let text: id = if is_attributed_string == YES {
            msg_send![text, string]
        } else {
            text
        };

        let text = text.to_str();
        let replacement_range = replacement_range.to_range();
        with_input_handler(this, |input_handler| {
            input_handler.replace_text_in_range(replacement_range, text)
        });
    }
}

extern "C" fn set_marked_text(
    this: &Object,
    _: Sel,
    text: id,
    selected_range: NSRange,
    replacement_range: NSRange,
) {
    unsafe {
        let is_attributed_string: BOOL =
            msg_send![text, isKindOfClass: [class!(NSAttributedString)]];
        let text: id = if is_attributed_string == YES {
            msg_send![text, string]
        } else {
            text
        };
        let selected_range = selected_range.to_range();
        let replacement_range = replacement_range.to_range();
        let text = text.to_str();
        with_input_handler(this, |input_handler| {
            input_handler.replace_and_mark_text_in_range(replacement_range, text, selected_range)
        });
    }
}
extern "C" fn unmark_text(this: &Object, _: Sel) {
    with_input_handler(this, |input_handler| input_handler.unmark_text());
}

extern "C" fn attributed_substring_for_proposed_range(
    this: &Object,
    _: Sel,
    range: NSRange,
    actual_range: *mut c_void,
) -> id {
    with_input_handler(this, |input_handler| {
        let range = range.to_range()?;
        if range.is_empty() {
            return None;
        }
        let mut adjusted: Option<Range<usize>> = None;

        let selected_text = input_handler.text_for_range(range.clone(), &mut adjusted)?;
        if let Some(adjusted) = adjusted
            && adjusted != range
        {
            unsafe { (actual_range as *mut NSRange).write(NSRange::from(adjusted)) };
        }
        unsafe {
            let string: id = msg_send![class!(NSAttributedString), alloc];
            let string: id = msg_send![string, initWithString: ns_string(&selected_text)];
            Some(string)
        }
    })
    .flatten()
    .unwrap_or(nil)
}

// We ignore which selector it asks us to do because the user may have
// bound the shortcut to something else.
extern "C" fn do_command_by_selector(this: &Object, _: Sel, _: Sel) {
    let state = unsafe { get_window_state(this) };
    let mut lock = state.as_ref().lock();
    let keystroke = lock.keystroke_for_do_command.take();
    let mut event_callback = lock.event_callback.take();
    drop(lock);

    if let Some((keystroke, mut callback)) = keystroke.zip(event_callback.as_mut()) {
        let handled = (callback)(PlatformInput::KeyDown(KeyDownEvent {
            keystroke,
            is_held: false,
        }));
        state.as_ref().lock().do_command_handled = Some(!handled.propagate);
    }

    state.as_ref().lock().event_callback = event_callback;
}

extern "C" fn view_did_change_effective_appearance(this: &Object, _: Sel) {
    unsafe {
        let state = get_window_state(this);
        let mut lock = state.as_ref().lock();
        if let Some(mut callback) = lock.appearance_changed_callback.take() {
            drop(lock);
            callback();
            state.lock().appearance_changed_callback = Some(callback);
        }
    }
}

extern "C" fn accepts_first_mouse(this: &Object, _: Sel, _: id) -> BOOL {
    let window_state = unsafe { get_window_state(this) };
    let mut lock = window_state.as_ref().lock();
    lock.first_mouse = true;
    YES
}

fn titlebar_move_rect(bounds: NSRect, app_owns_titlebar_drag: bool) -> NSRect {
    if app_owns_titlebar_drag {
        bounds
    } else {
        NSRect::new(NSPoint::new(0., 0.), NSSize::new(0., 0.))
    }
}

extern "C" fn opaque_rect_for_window_move_when_in_titlebar(this: &Object, _: Sel) -> NSRect {
    let window_state = unsafe { get_window_state(this) };
    let app_owns_titlebar_drag = window_state.as_ref().lock().app_owns_titlebar_drag;
    let bounds = unsafe { msg_send![this, bounds] };
    titlebar_move_rect(bounds, app_owns_titlebar_drag)
}

extern "C" fn character_index_for_point(this: &Object, _: Sel, position: NSPoint) -> u64 {
    let position = screen_point_to_gpui_point(this, position);
    with_input_handler(this, |input_handler| {
        input_handler.character_index_for_point(position)
    })
    .flatten()
    .map(|index| index as u64)
    .unwrap_or(usize::MAX as u64)
}

fn screen_point_to_gpui_point(this: &Object, position: NSPoint) -> Point<Pixels> {
    let frame = get_frame(this);
    let window_x = position.x - frame.origin.x;
    let window_y = frame.size.height - (position.y - frame.origin.y);

    point(px(window_x as f32), px(window_y as f32))
}

extern "C" fn dragging_entered(this: &Object, _: Sel, dragging_info: id) -> NSDragOperation {
    let window_state = unsafe { get_window_state(this) };
    let position = drag_event_position(&window_state, dragging_info);
    let paths = external_paths_from_event(dragging_info);
    if let Some(event) =
        paths.map(|paths| PlatformInput::FileDrop(FileDropEvent::Entered { position, paths }))
        && send_new_event(&window_state, event)
    {
        window_state.lock().external_files_dragged = true;
        return NSDragOperationCopy;
    }
    NSDragOperationNone
}

extern "C" fn dragging_updated(this: &Object, _: Sel, dragging_info: id) -> NSDragOperation {
    let window_state = unsafe { get_window_state(this) };
    let position = drag_event_position(&window_state, dragging_info);
    if send_new_event(
        &window_state,
        PlatformInput::FileDrop(FileDropEvent::Pending { position }),
    ) {
        NSDragOperationCopy
    } else {
        NSDragOperationNone
    }
}

extern "C" fn dragging_exited(this: &Object, _: Sel, _: id) {
    let window_state = unsafe { get_window_state(this) };
    send_new_event(
        &window_state,
        PlatformInput::FileDrop(FileDropEvent::Exited),
    );
    window_state.lock().external_files_dragged = false;
}

extern "C" fn perform_drag_operation(this: &Object, _: Sel, dragging_info: id) -> BOOL {
    let window_state = unsafe { get_window_state(this) };
    let position = drag_event_position(&window_state, dragging_info);
    send_new_event(
        &window_state,
        PlatformInput::FileDrop(FileDropEvent::Submit { position }),
    )
    .to_objc()
}

fn external_paths_from_event(dragging_info: *mut Object) -> Option<ExternalPaths> {
    let mut paths = SmallVec::new();
    let pasteboard: id = unsafe { msg_send![dragging_info, draggingPasteboard] };
    let filenames: id =
        unsafe { msg_send![pasteboard, propertyListForType: filenames_pboard_type()] };
    if filenames == nil {
        return None;
    }
    let count = unsafe { array_count(filenames) };
    for i in 0..count {
        let file = unsafe { array_object_at_index(filenames, i) };
        let path = unsafe { file.to_str().to_owned() };
        paths.push(PathBuf::from(path))
    }
    Some(ExternalPaths(paths))
}

extern "C" fn conclude_drag_operation(this: &Object, _: Sel, _: id) {
    let window_state = unsafe { get_window_state(this) };
    send_new_event(
        &window_state,
        PlatformInput::FileDrop(FileDropEvent::Exited),
    );
}

async fn synthetic_drag(
    window_state: Weak<Mutex<MacWindowState>>,
    drag_id: usize,
    event: MouseMoveEvent,
) {
    loop {
        Timer::after(Duration::from_millis(16)).await;
        if let Some(window_state) = window_state.upgrade() {
            let mut lock = window_state.lock();
            if lock.synthetic_drag_counter == drag_id {
                if let Some(mut callback) = lock.event_callback.take() {
                    drop(lock);
                    callback(PlatformInput::MouseMove(event.clone()));
                    window_state.lock().event_callback = Some(callback);
                }
            } else {
                break;
            }
        }
    }
}

fn send_new_event(window_state_lock: &Mutex<MacWindowState>, e: PlatformInput) -> bool {
    let window_state = window_state_lock.lock().event_callback.take();
    if let Some(mut callback) = window_state {
        callback(e);
        window_state_lock.lock().event_callback = Some(callback);
        true
    } else {
        false
    }
}

fn drag_event_position(window_state: &Mutex<MacWindowState>, dragging_info: id) -> Point<Pixels> {
    let drag_location: NSPoint = unsafe { msg_send![dragging_info, draggingLocation] };
    convert_mouse_position(drag_location, window_state.lock().content_size().height)
}

fn with_input_handler<F, R>(window: &Object, f: F) -> Option<R>
where
    F: FnOnce(&mut PlatformInputHandler) -> R,
{
    let window_state = unsafe { get_window_state(window) };
    let mut lock = window_state.as_ref().lock();
    if let Some(mut input_handler) = lock.input_handler.take() {
        drop(lock);
        let result = f(&mut input_handler);
        window_state.lock().input_handler = Some(input_handler);
        Some(result)
    } else {
        None
    }
}

extern "C" fn blurred_view_init_with_frame(this: &Object, _: Sel, frame: NSRect) -> id {
    unsafe {
        let view: id = msg_send![super(this, class!(NSVisualEffectView)), initWithFrame: frame];
        // Use a colorless semantic material. The default value `AppearanceBased`, though not
        // manually set, is deprecated.
        let _: () = msg_send![view, setMaterial: NSVisualEffectMaterial::Selection];
        let _: () = msg_send![view, setState: NSVisualEffectState::Active];
        view
    }
}

extern "C" fn blurred_view_update_layer(this: &Object, _: Sel) {
    unsafe {
        let _: () = msg_send![super(this, class!(NSVisualEffectView)), updateLayer];
        let layer: id = msg_send![this, layer];
        if !layer.is_null() {
            remove_layer_background(layer);
        }
    }
}

unsafe fn remove_layer_background(layer: id) {
    unsafe {
        let _: () = msg_send![layer, setBackgroundColor:nil];

        let class_name: id = msg_send![layer, className];
        let is_chameleon_layer: BOOL =
            msg_send![class_name, isEqualToString: ns_string("CAChameleonLayer")];
        if is_chameleon_layer == YES {
            // Remove the desktop tinting effect.
            let _: () = msg_send![layer, setHidden: YES];
            return;
        }

        let filters: id = msg_send![layer, filters];
        if !filters.is_null() {
            // Remove the increased saturation.
            // The effect of a `CAFilter` or `CIFilter` is determined by its name, and the
            // `description` reflects its name and some parameters. Currently `NSVisualEffectView`
            // uses a `CAFilter` named "colorSaturate". If one day they switch to `CIFilter`, the
            // `description` will still contain "Saturat" ("... inputSaturation = ...").
            let test_string = ns_string("Saturat");
            let count = array_count(filters);
            for i in 0..count {
                let filter = array_object_at_index(filters, i);
                let description: id = msg_send![filter, description];
                let hit: BOOL = msg_send![description, containsString: test_string];
                if hit == NO {
                    continue;
                }

                let all_indices = NSRange {
                    location: 0,
                    length: count as usize,
                };
                let indices: id = msg_send![class!(NSMutableIndexSet), indexSet];
                let _: () = msg_send![indices, addIndexesInRange: all_indices];
                let _: () = msg_send![indices, removeIndex:i];
                let filtered: id = msg_send![filters, objectsAtIndexes: indices];
                let _: () = msg_send![layer, setFilters: filtered];
                break;
            }
        }

        let sublayers: id = msg_send![layer, sublayers];
        if !sublayers.is_null() {
            let count = array_count(sublayers);
            for i in 0..count {
                let sublayer = array_object_at_index(sublayers, i);
                remove_layer_background(sublayer);
            }
        }
    }
}

extern "C" fn add_titlebar_accessory_view_controller(this: &Object, _: Sel, view_controller: id) {
    unsafe {
        let _: () = msg_send![super(this, class!(NSWindow)), addTitlebarAccessoryViewController: view_controller];

        // Hide the native tab bar and set its height to 0, since we render our own.
        let accessory_view: id = msg_send![view_controller, view];
        let _: () = msg_send![accessory_view, setHidden: YES];
        let mut frame: NSRect = msg_send![accessory_view, frame];
        frame.size.height = 0.0;
        let _: () = msg_send![accessory_view, setFrame: frame];
    }
}

extern "C" fn move_tab_to_new_window(this: &Object, _: Sel, _: id) {
    unsafe {
        let _: () = msg_send![super(this, class!(NSWindow)), moveTabToNewWindow:nil];

        let window_state = get_window_state(this);
        let mut lock = window_state.as_ref().lock();
        if let Some(mut callback) = lock.move_tab_to_new_window_callback.take() {
            drop(lock);
            callback();
            window_state.lock().move_tab_to_new_window_callback = Some(callback);
        }
    }
}

extern "C" fn merge_all_windows(this: &Object, _: Sel, _: id) {
    unsafe {
        let _: () = msg_send![super(this, class!(NSWindow)), mergeAllWindows:nil];

        let window_state = get_window_state(this);
        let mut lock = window_state.as_ref().lock();
        if let Some(mut callback) = lock.merge_all_windows_callback.take() {
            drop(lock);
            callback();
            window_state.lock().merge_all_windows_callback = Some(callback);
        }
    }
}

extern "C" fn select_next_tab(this: &Object, _sel: Sel, _id: id) {
    let window_state = unsafe { get_window_state(this) };
    let mut lock = window_state.as_ref().lock();
    if let Some(mut callback) = lock.select_next_tab_callback.take() {
        drop(lock);
        callback();
        window_state.lock().select_next_tab_callback = Some(callback);
    }
}

extern "C" fn select_previous_tab(this: &Object, _sel: Sel, _id: id) {
    let window_state = unsafe { get_window_state(this) };
    let mut lock = window_state.as_ref().lock();
    if let Some(mut callback) = lock.select_previous_tab_callback.take() {
        drop(lock);
        callback();
        window_state.lock().select_previous_tab_callback = Some(callback);
    }
}

extern "C" fn toggle_tab_bar(this: &Object, _sel: Sel, _id: id) {
    unsafe {
        let _: () = msg_send![super(this, class!(NSWindow)), toggleTabBar:nil];

        let window_state = get_window_state(this);
        let mut lock = window_state.as_ref().lock();
        lock.move_traffic_light();

        if let Some(mut callback) = lock.toggle_tab_bar_callback.take() {
            drop(lock);
            callback();
            window_state.lock().toggle_tab_bar_callback = Some(callback);
        }
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

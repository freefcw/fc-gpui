//! objc2 `define_class!` replacements for the macOS window/view ClassDecl
//! types that used to live in `window.rs`.
//!
//! Zed's `crates/gpui_macos/src/window.rs` still registers these classes with
//! legacy `objc` `ClassDecl`. This module is a local adaptation in the same
//! objc2 style as `platform.rs` / `screen_capture.rs`.

use super::super::events::platform_input_from_native;
use super::{
    MacWindowState, NSOperatingSystemVersion, NSPoint, NSRange, NSRect, NSSize, NSStringExt,
    convert_mouse_position, is_macos_version_at_least, ns_string, titlebar_move_rect,
};
use crate::{
    CursorStyle, ExternalPaths, FileDropEvent, KeyDownEvent, Modifiers, ModifiersChangedEvent,
    MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent, Pixels, PlatformInput,
    PlatformInputHandler, Point, Size, Timer, point, px,
};
use objc::{
    class, msg_send,
    runtime::{BOOL, NO, Object, YES},
};
use objc2::rc::{Allocated, Retained};
use objc2::runtime::{AnyClass, AnyObject, ProtocolObject, Sel};
use objc2::{AnyThread, ClassType, DefinedClass, MainThreadMarker, MainThreadOnly, define_class};
use objc2_app_kit::{
    NSCursor, NSDragOperation, NSDraggingDestination, NSDraggingInfo, NSEvent, NSPanel, NSScreen,
    NSTextInputClient, NSView, NSVisualEffectMaterial, NSVisualEffectState, NSVisualEffectView,
    NSWindow, NSWindowDelegate, NSWindowOcclusionState, NSWindowStyleMask,
};
use objc2_foundation::{
    NSArray, NSAttributedString, NSAttributedStringKey, NSData, NSError, NSNotification,
    NSObjectProtocol, NSPoint as Objc2NSPoint, NSRange as Objc2NSRange, NSRangePointer,
    NSRect as Objc2NSRect, NSSize as Objc2NSSize, NSString, NSUInteger,
};
use objc2_quartz_core::{CALayer, CALayerDelegate};
use parking_lot::Mutex;
use smallvec::SmallVec;
use std::{
    cell::Cell,
    mem,
    ops::Range,
    path::PathBuf,
    ptr,
    sync::{Arc, Weak},
    time::Duration,
};

type ObjcId = *mut Object;

#[allow(non_camel_case_types)]
type id = ObjcId;

#[allow(non_upper_case_globals)]
const nil: ObjcId = ptr::null_mut();

pub(super) struct WindowIvars {
    pub(super) state: Cell<*const Mutex<MacWindowState>>,
}

impl Drop for WindowIvars {
    fn drop(&mut self) {
        drop_retained_window_state(self.state.get());
    }
}

pub(super) struct ViewIvars {
    pub(super) state: Cell<*const Mutex<MacWindowState>>,
}

impl Drop for ViewIvars {
    fn drop(&mut self) {
        let raw = self.state.get();
        if raw.is_null() {
            return;
        }
        let window_state = unsafe { Arc::from_raw(raw) };
        let mut state = window_state.lock();
        if state.cursor_hidden {
            let _: () = unsafe { objc2::msg_send![NSCursor::class(), unhide] };
            state.cursor_hidden = false;
        }
    }
}

macro_rules! define_gpui_ns_window {
    ($ty:ident, $super:ty, $name:literal) => {
        define_class!(
            // SAFETY: `$super` can be subclassed and `$ty` does not implement
            // `Drop`. Ivars hold a raw `Arc` pointer whose ownership is released
            // when the ivars are dropped.
            #[unsafe(super($super))]
            #[thread_kind = MainThreadOnly]
            #[name = $name]
            #[ivars = WindowIvars]
            pub(super) struct $ty;

            unsafe impl NSObjectProtocol for $ty {}

            impl $ty {
                #[unsafe(method(canBecomeMainWindow))]
                fn can_become_main_window(&self) -> bool {
                    true
                }

                #[unsafe(method(canBecomeKeyWindow))]
                fn can_become_key_window(&self) -> bool {
                    true
                }

                #[unsafe(method(close))]
                fn close(&self) {
                    close_gpui_window(self);
                    unsafe { objc2::msg_send![super(self), close] }
                }

                #[unsafe(method(addTitlebarAccessoryViewController:))]
                fn add_titlebar_accessory_view_controller(&self, view_controller: &AnyObject) {
                    unsafe {
                        objc2::msg_send![super(self), addTitlebarAccessoryViewController: view_controller]
                    };
                    hide_titlebar_accessory(view_controller);
                }

                #[unsafe(method(moveTabToNewWindow:))]
                fn move_tab_to_new_window(&self, _sender: Option<&AnyObject>) {
                    let sender: Option<&AnyObject> = None;
                    unsafe { objc2::msg_send![super(self), moveTabToNewWindow: sender] };
                    invoke_move_tab_to_new_window(self);
                }

                #[unsafe(method(mergeAllWindows:))]
                fn merge_all_windows(&self, _sender: Option<&AnyObject>) {
                    let sender: Option<&AnyObject> = None;
                    unsafe { objc2::msg_send![super(self), mergeAllWindows: sender] };
                    invoke_merge_all_windows(self);
                }

                #[unsafe(method(selectNextTab:))]
                fn select_next_tab(&self, _sender: Option<&AnyObject>) {
                    invoke_select_next_tab(self);
                }

                #[unsafe(method(selectPreviousTab:))]
                fn select_previous_tab(&self, _sender: Option<&AnyObject>) {
                    invoke_select_previous_tab(self);
                }

                #[unsafe(method(toggleTabBar:))]
                fn toggle_tab_bar(&self, _sender: Option<&AnyObject>) {
                    let sender: Option<&AnyObject> = None;
                    unsafe { objc2::msg_send![super(self), toggleTabBar: sender] };
                    invoke_toggle_tab_bar(self);
                }
            }

            unsafe impl NSWindowDelegate for $ty {
                #[unsafe(method(windowDidResize:))]
                fn window_did_resize(&self, _notification: &NSNotification) {
                    window_ivars_state(self.ivars()).lock().move_traffic_light();
                }

                #[unsafe(method(windowDidChangeOcclusionState:))]
                fn window_did_change_occlusion_state(&self, _notification: &NSNotification) {
                    handle_window_did_change_occlusion_state(self);
                }

                #[unsafe(method(windowWillEnterFullScreen:))]
                fn window_will_enter_full_screen(&self, _notification: &NSNotification) {
                    handle_window_will_enter_fullscreen(self);
                }

                #[unsafe(method(windowWillExitFullScreen:))]
                fn window_will_exit_full_screen(&self, _notification: &NSNotification) {
                    handle_window_will_exit_fullscreen(self);
                }

                #[unsafe(method(windowDidMove:))]
                fn window_did_move(&self, _notification: &NSNotification) {
                    handle_window_did_move(self);
                }

                #[unsafe(method(windowDidChangeScreen:))]
                fn window_did_change_screen(&self, _notification: &NSNotification) {
                    window_ivars_state(self.ivars()).lock().start_display_link();
                }

                #[unsafe(method(windowDidBecomeKey:))]
                fn window_did_become_key(&self, _notification: &NSNotification) {
                    handle_window_did_change_key_status(self, true);
                }

                #[unsafe(method(windowDidResignKey:))]
                fn window_did_resign_key(&self, _notification: &NSNotification) {
                    handle_window_did_change_key_status(self, false);
                }

                #[unsafe(method(windowShouldClose:))]
                fn window_should_close(&self, _sender: &NSWindow) -> bool {
                    handle_window_should_close(self)
                }
            }

            unsafe impl NSDraggingDestination for $ty {
                #[unsafe(method(draggingEntered:))]
                fn dragging_entered(
                    &self,
                    sender: &ProtocolObject<dyn NSDraggingInfo>,
                ) -> NSDragOperation {
                    handle_dragging_entered(self, sender)
                }

                #[unsafe(method(draggingUpdated:))]
                fn dragging_updated(
                    &self,
                    sender: &ProtocolObject<dyn NSDraggingInfo>,
                ) -> NSDragOperation {
                    handle_dragging_updated(self, sender)
                }

                #[unsafe(method(draggingExited:))]
                fn dragging_exited(&self, _sender: Option<&ProtocolObject<dyn NSDraggingInfo>>) {
                    handle_dragging_exited(self);
                }

                #[unsafe(method(performDragOperation:))]
                fn perform_drag_operation(
                    &self,
                    sender: &ProtocolObject<dyn NSDraggingInfo>,
                ) -> bool {
                    handle_perform_drag_operation(self, sender)
                }

                #[unsafe(method(concludeDragOperation:))]
                fn conclude_drag_operation(
                    &self,
                    _sender: Option<&ProtocolObject<dyn NSDraggingInfo>>,
                ) {
                    handle_conclude_drag_operation(self);
                }
            }
        );
    };
}

define_gpui_ns_window!(GPUIWindow, NSWindow, "GPUIWindow");
define_gpui_ns_window!(GPUIPanel, NSPanel, "GPUIPanel");

define_class!(
    // SAFETY: `NSView` can be subclassed and `GPUIView` does not implement
    // `Drop`. Ivars hold a raw `Arc` pointer whose ownership is released when
    // the ivars are dropped (and the cursor hide count is restored).
    #[unsafe(super(NSView))]
    #[thread_kind = MainThreadOnly]
    #[name = "GPUIView"]
    #[ivars = ViewIvars]
    pub(super) struct GPUIView;

    unsafe impl NSObjectProtocol for GPUIView {}

    impl GPUIView {
        #[unsafe(method(performKeyEquivalent:))]
        fn perform_key_equivalent(&self, event: &NSEvent) -> bool {
            handle_key_event(self, event, true)
        }

        #[unsafe(method(keyDown:))]
        fn key_down(&self, event: &NSEvent) {
            handle_key_event(self, event, false);
        }

        #[unsafe(method(keyUp:))]
        fn key_up(&self, event: &NSEvent) {
            handle_key_event(self, event, false);
        }

        #[unsafe(method(mouseDown:))]
        fn mouse_down(&self, event: &NSEvent) {
            handle_view_event(self, event);
        }

        #[unsafe(method(mouseUp:))]
        fn mouse_up(&self, event: &NSEvent) {
            handle_view_event(self, event);
        }

        #[unsafe(method(rightMouseDown:))]
        fn right_mouse_down(&self, event: &NSEvent) {
            handle_view_event(self, event);
        }

        #[unsafe(method(rightMouseUp:))]
        fn right_mouse_up(&self, event: &NSEvent) {
            handle_view_event(self, event);
        }

        #[unsafe(method(otherMouseDown:))]
        fn other_mouse_down(&self, event: &NSEvent) {
            handle_view_event(self, event);
        }

        #[unsafe(method(otherMouseUp:))]
        fn other_mouse_up(&self, event: &NSEvent) {
            handle_view_event(self, event);
        }

        #[unsafe(method(mouseMoved:))]
        fn mouse_moved(&self, event: &NSEvent) {
            handle_view_event(self, event);
        }

        #[unsafe(method(resetCursorRects))]
        fn reset_cursor_rects(&self) {
            unsafe { objc2::msg_send![super(self), resetCursorRects] };
            handle_reset_cursor_rects(self);
        }

        #[unsafe(method(mouseExited:))]
        fn mouse_exited(&self, event: &NSEvent) {
            handle_view_event(self, event);
        }

        #[unsafe(method(mouseDragged:))]
        fn mouse_dragged(&self, event: &NSEvent) {
            handle_view_event(self, event);
        }

        #[unsafe(method(scrollWheel:))]
        fn scroll_wheel(&self, event: &NSEvent) {
            handle_view_event(self, event);
        }

        #[unsafe(method(swipeWithEvent:))]
        fn swipe_with_event(&self, event: &NSEvent) {
            handle_view_event(self, event);
        }

        #[unsafe(method(flagsChanged:))]
        fn flags_changed(&self, event: &NSEvent) {
            handle_view_event(self, event);
        }

        #[unsafe(method_id(makeBackingLayer))]
        fn make_backing_layer(&self) -> Retained<CALayer> {
            let ptr = view_state(self)
                .lock()
                .renderer
                .layer_ptr()
                .cast::<CALayer>();
            unsafe { Retained::retain(ptr).expect("CAMetalLayer backing layer") }
        }

        #[unsafe(method(viewDidChangeBackingProperties))]
        fn view_did_change_backing_properties(&self) {
            handle_view_did_change_backing_properties(self);
        }

        #[unsafe(method(setFrameSize:))]
        fn set_frame_size(&self, size: Objc2NSSize) {
            if !frame_size_changed(self, size) {
                return;
            }
            unsafe { objc2::msg_send![super(self), setFrameSize: size] };
            finish_set_frame_size(self, size);
        }

        #[unsafe(method(viewDidChangeEffectiveAppearance))]
        fn view_did_change_effective_appearance(&self) {
            handle_view_did_change_effective_appearance(self);
        }

        #[unsafe(method(acceptsFirstMouse:))]
        fn accepts_first_mouse(&self, _event: Option<&NSEvent>) -> bool {
            view_state(self).lock().first_mouse = true;
            true
        }

        #[unsafe(method(_opaqueRectForWindowMoveWhenInTitlebar))]
        fn opaque_rect_for_window_move_when_in_titlebar(&self) -> Objc2NSRect {
            let app_owns_titlebar_drag = view_state(self).lock().app_owns_titlebar_drag;
            to_objc_rect(titlebar_move_rect(
                from_objc_rect(self.bounds()),
                app_owns_titlebar_drag,
            ))
        }
    }

    unsafe impl CALayerDelegate for GPUIView {
        #[unsafe(method(displayLayer:))]
        fn display_layer(&self, _layer: &CALayer) {
            handle_display_layer(self);
        }
    }

    unsafe impl NSTextInputClient for GPUIView {
        #[unsafe(method_id(validAttributesForMarkedText))]
        fn valid_attributes_for_marked_text(&self) -> Retained<NSArray<NSAttributedStringKey>> {
            NSArray::<NSAttributedStringKey>::from_slice(&[])
        }

        #[unsafe(method(hasMarkedText))]
        fn has_marked_text(&self) -> bool {
            with_input_handler(self, |input_handler| input_handler.marked_text_range())
                .flatten()
                .is_some()
        }

        #[unsafe(method(markedRange))]
        fn marked_range(&self) -> Objc2NSRange {
            with_input_handler(self, |input_handler| input_handler.marked_text_range())
                .flatten()
                .map_or(invalid_objc_range(), to_objc_range)
        }

        #[unsafe(method(selectedRange))]
        fn selected_range(&self) -> Objc2NSRange {
            with_input_handler(self, |input_handler| {
                input_handler.selected_text_range(false)
            })
            .flatten()
            .map_or(invalid_objc_range(), |selection| {
                to_objc_range(selection.range)
            })
        }

        #[unsafe(method(firstRectForCharacterRange:actualRange:))]
        unsafe fn first_rect_for_character_range_actual_range(
            &self,
            range: Objc2NSRange,
            _actual_range: NSRangePointer,
        ) -> Objc2NSRect {
            to_objc_rect(first_rect_for_character_range(self, range))
        }

        #[unsafe(method(insertText:replacementRange:))]
        unsafe fn insert_text_replacement_range(
            &self,
            string: &AnyObject,
            replacement_range: Objc2NSRange,
        ) {
            insert_text(self, string, replacement_range);
        }

        #[unsafe(method(setMarkedText:selectedRange:replacementRange:))]
        unsafe fn set_marked_text_selected_range_replacement_range(
            &self,
            string: &AnyObject,
            selected_range: Objc2NSRange,
            replacement_range: Objc2NSRange,
        ) {
            set_marked_text(self, string, selected_range, replacement_range);
        }

        #[unsafe(method(unmarkText))]
        fn unmark_text(&self) {
            with_input_handler(self, |input_handler| input_handler.unmark_text());
        }

        #[unsafe(method_id(attributedSubstringForProposedRange:actualRange:))]
        unsafe fn attributed_substring_for_proposed_range_actual_range(
            &self,
            range: Objc2NSRange,
            actual_range: NSRangePointer,
        ) -> Option<Retained<NSAttributedString>> {
            attributed_substring_for_proposed_range(self, range, actual_range)
        }

        #[unsafe(method(doCommandBySelector:))]
        unsafe fn do_command_by_selector(&self, _selector: Sel) {
            handle_do_command_by_selector(self);
        }

        #[unsafe(method(characterIndexForPoint:))]
        fn character_index_for_point(&self, point: Objc2NSPoint) -> NSUInteger {
            let position = screen_point_to_gpui_point(self, from_objc_point(point));
            with_input_handler(self, |input_handler| {
                input_handler.character_index_for_point(position)
            })
            .flatten()
            .unwrap_or(usize::MAX)
        }
    }
);

define_class!(
    // SAFETY: `NSVisualEffectView` can be subclassed and `BlurredView` does not
    // implement `Drop`.
    #[unsafe(super(NSVisualEffectView))]
    #[thread_kind = MainThreadOnly]
    #[name = "BlurredView"]
    pub(super) struct BlurredView;

    impl BlurredView {
        #[unsafe(method_id(initWithFrame:))]
        fn init_with_frame(this: Allocated<Self>, frame: Objc2NSRect) -> Option<Retained<Self>> {
            let this: Option<Retained<Self>> =
                unsafe { objc2::msg_send![super(this), initWithFrame: frame] };
            let Some(this) = this else {
                return None;
            };
            this.setMaterial(NSVisualEffectMaterial::Selection);
            this.setState(NSVisualEffectState::Active);
            Some(this)
        }

        #[unsafe(method(updateLayer))]
        fn update_layer(&self) {
            let _: () = unsafe { objc2::msg_send![super(self), updateLayer] };
            if let Some(layer) = self.layer() {
                remove_layer_background(&layer);
            }
        }
    }
);

struct ArchiverDelegateIvars;

define_class!(
    // SAFETY: `NSObject` has no subclassing requirements and
    // `GPUIWindowStateArchiverDelegate` does not implement `Drop`.
    #[unsafe(super(objc2_foundation::NSObject))]
    #[name = "GPUIWindowStateArchiverDelegate"]
    #[ivars = ArchiverDelegateIvars]
    pub(super) struct GPUIWindowStateArchiverDelegate;

    unsafe impl NSObjectProtocol for GPUIWindowStateArchiverDelegate {}

    unsafe impl objc2_foundation::NSKeyedArchiverDelegate for GPUIWindowStateArchiverDelegate {
        #[unsafe(method_id(archiver:willEncodeObject:))]
        unsafe fn archiver_will_encode_object(
            &self,
            _archiver: &objc2_foundation::NSKeyedArchiver,
            object: &AnyObject,
        ) -> Option<Retained<AnyObject>> {
            if object.is_kind_of::<NSView>() || object.is_kind_of::<NSWindow>() {
                None
            } else {
                Some(object.retain())
            }
        }
    }
);

define_class!(
    // SAFETY: `NSKeyedUnarchiver` can be subclassed and
    // `GPUIWindowStateKeyedUnarchiver` does not implement `Drop`.
    #[unsafe(super(objc2_foundation::NSKeyedUnarchiver))]
    #[name = "GPUIWindowStateKeyedUnarchiver"]
    pub(super) struct GPUIWindowStateKeyedUnarchiver;

    impl GPUIWindowStateKeyedUnarchiver {
        #[unsafe(method_id(_windowRestorationOptions))]
        fn window_restoration_options(&self) -> Option<Retained<AnyObject>> {
            if !is_macos_version_at_least(NSOperatingSystemVersion::new(15, 0, 0)) {
                return None;
            }
            let Some(class) = AnyClass::get(c"NSWindowRestorationOptions") else {
                return None;
            };
            let options: Option<Retained<AnyObject>> = unsafe { objc2::msg_send![class, new] };
            options
        }
    }
);

impl GPUIWindow {
    pub(super) fn new(
        mtm: MainThreadMarker,
        content_rect: Objc2NSRect,
        style_mask: NSWindowStyleMask,
        screen: Option<&NSScreen>,
    ) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(WindowIvars {
            state: Cell::new(ptr::null()),
        });
        unsafe {
            objc2::msg_send![
                super(this),
                initWithContentRect: content_rect,
                styleMask: style_mask,
                backing: objc2_app_kit::NSBackingStoreType::Buffered,
                defer: false,
                screen: screen,
            ]
        }
    }
}

impl GPUIPanel {
    pub(super) fn new(
        mtm: MainThreadMarker,
        content_rect: Objc2NSRect,
        style_mask: NSWindowStyleMask,
        screen: Option<&NSScreen>,
    ) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(WindowIvars {
            state: Cell::new(ptr::null()),
        });
        unsafe {
            objc2::msg_send![
                super(this),
                initWithContentRect: content_rect,
                styleMask: style_mask,
                backing: objc2_app_kit::NSBackingStoreType::Buffered,
                defer: false,
                screen: screen,
            ]
        }
    }
}

impl GPUIView {
    pub(super) fn with_frame(mtm: MainThreadMarker, frame: Objc2NSRect) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(ViewIvars {
            state: Cell::new(ptr::null()),
        });
        unsafe { objc2::msg_send![super(this), initWithFrame: frame] }
    }
}

impl BlurredView {
    pub(super) fn with_frame(mtm: MainThreadMarker, frame: Objc2NSRect) -> Retained<Self> {
        let this = Self::alloc(mtm);
        let view: Option<Retained<Self>> = unsafe { objc2::msg_send![this, initWithFrame: frame] };
        view.expect("BlurredView initWithFrame returned nil")
    }
}

impl GPUIWindowStateArchiverDelegate {
    pub(super) fn new() -> Retained<Self> {
        let this = Self::alloc().set_ivars(ArchiverDelegateIvars);
        unsafe { objc2::msg_send![super(this), init] }
    }
}

impl GPUIWindowStateKeyedUnarchiver {
    pub(super) fn for_reading_from_data(
        data: &NSData,
    ) -> Result<Retained<Self>, Retained<NSError>> {
        let this = Self::alloc();
        unsafe { objc2::msg_send![this, initForReadingFromData: data, error: _] }
    }
}

pub(super) fn retain_window_state(raw: *const Mutex<MacWindowState>) -> Arc<Mutex<MacWindowState>> {
    assert!(!raw.is_null(), "windowState ivar is null");
    let rc1 = unsafe { Arc::from_raw(raw) };
    let rc2 = rc1.clone();
    mem::forget(rc1);
    rc2
}

fn drop_retained_window_state(raw: *const Mutex<MacWindowState>) {
    if raw.is_null() {
        return;
    }
    unsafe {
        drop(Arc::from_raw(raw));
    }
}

pub(super) fn assign_window_state(window: &GPUIWindow, state: &Arc<Mutex<MacWindowState>>) {
    window
        .ivars()
        .state
        .set(Arc::into_raw(state.clone()) as *const Mutex<MacWindowState>);
}

pub(super) fn assign_panel_state(panel: &GPUIPanel, state: &Arc<Mutex<MacWindowState>>) {
    panel
        .ivars()
        .state
        .set(Arc::into_raw(state.clone()) as *const Mutex<MacWindowState>);
}

pub(super) fn assign_view_state(view: &GPUIView, state: &Arc<Mutex<MacWindowState>>) {
    view.ivars()
        .state
        .set(Arc::into_raw(state.clone()) as *const Mutex<MacWindowState>);
}

pub(super) unsafe fn gpui_window_from_id<'a>(ptr: id) -> Option<&'a GPUIWindow> {
    unsafe {
        let object = ptr.cast::<AnyObject>().as_ref()?;
        if object.is_kind_of::<GPUIWindow>() {
            Some(&*ptr.cast::<GPUIWindow>())
        } else {
            None
        }
    }
}

fn window_ivars_state(ivars: &WindowIvars) -> Arc<Mutex<MacWindowState>> {
    retain_window_state(ivars.state.get())
}

fn view_state(view: &GPUIView) -> Arc<Mutex<MacWindowState>> {
    retain_window_state(view.ivars().state.get())
}

fn as_legacy_event(event: &NSEvent) -> id {
    event as *const NSEvent as id
}

fn to_objc_rect(rect: NSRect) -> Objc2NSRect {
    Objc2NSRect::new(
        Objc2NSPoint::new(rect.origin.x, rect.origin.y),
        Objc2NSSize::new(rect.size.width, rect.size.height),
    )
}

fn from_objc_rect(rect: Objc2NSRect) -> NSRect {
    NSRect::new(
        NSPoint::new(rect.origin.x, rect.origin.y),
        NSSize::new(rect.size.width, rect.size.height),
    )
}

fn from_objc_point(point: Objc2NSPoint) -> NSPoint {
    NSPoint::new(point.x, point.y)
}

fn to_objc_range(range: Range<usize>) -> Objc2NSRange {
    Objc2NSRange {
        location: range.start,
        length: range.len(),
    }
}

fn from_objc_range(range: Objc2NSRange) -> NSRange {
    NSRange {
        location: range.location,
        length: range.length,
    }
}

fn invalid_objc_range() -> Objc2NSRange {
    let range = NSRange::invalid();
    Objc2NSRange {
        location: range.location,
        length: range.length,
    }
}

trait HasWindowIvars {
    fn window_ivars(&self) -> &WindowIvars;
}

impl HasWindowIvars for GPUIWindow {
    fn window_ivars(&self) -> &WindowIvars {
        self.ivars()
    }
}

impl HasWindowIvars for GPUIPanel {
    fn window_ivars(&self) -> &WindowIvars {
        self.ivars()
    }
}

fn close_gpui_window(this: &impl HasWindowIvars) {
    let close_callback = {
        let window_state = window_ivars_state(this.window_ivars());
        let mut lock = window_state.lock();
        lock.begin_close();
        lock.close_callback.take()
    };
    if let Some(callback) = close_callback {
        callback();
    }
}

fn hide_titlebar_accessory(view_controller: &AnyObject) {
    unsafe {
        let accessory_view: id = msg_send![view_controller as *const AnyObject as id, view];
        let _: () = msg_send![accessory_view, setHidden: YES];
        let mut frame: NSRect = msg_send![accessory_view, frame];
        frame.size.height = 0.0;
        let _: () = msg_send![accessory_view, setFrame: frame];
    }
}

fn invoke_move_tab_to_new_window(this: &impl HasWindowIvars) {
    let window_state = window_ivars_state(this.window_ivars());
    let mut lock = window_state.lock();
    if let Some(mut callback) = lock.move_tab_to_new_window_callback.take() {
        drop(lock);
        callback();
        window_state.lock().move_tab_to_new_window_callback = Some(callback);
    }
}

fn invoke_merge_all_windows(this: &impl HasWindowIvars) {
    let window_state = window_ivars_state(this.window_ivars());
    let mut lock = window_state.lock();
    if let Some(mut callback) = lock.merge_all_windows_callback.take() {
        drop(lock);
        callback();
        window_state.lock().merge_all_windows_callback = Some(callback);
    }
}

fn invoke_select_next_tab(this: &impl HasWindowIvars) {
    let window_state = window_ivars_state(this.window_ivars());
    let mut lock = window_state.lock();
    if let Some(mut callback) = lock.select_next_tab_callback.take() {
        drop(lock);
        callback();
        window_state.lock().select_next_tab_callback = Some(callback);
    }
}

fn invoke_select_previous_tab(this: &impl HasWindowIvars) {
    let window_state = window_ivars_state(this.window_ivars());
    let mut lock = window_state.lock();
    if let Some(mut callback) = lock.select_previous_tab_callback.take() {
        drop(lock);
        callback();
        window_state.lock().select_previous_tab_callback = Some(callback);
    }
}

fn invoke_toggle_tab_bar(this: &impl HasWindowIvars) {
    let window_state = window_ivars_state(this.window_ivars());
    let mut lock = window_state.lock();
    lock.move_traffic_light();
    if let Some(mut callback) = lock.toggle_tab_bar_callback.take() {
        drop(lock);
        callback();
        window_state.lock().toggle_tab_bar_callback = Some(callback);
    }
}

fn handle_window_did_change_occlusion_state(this: &impl HasWindowIvars) {
    let window_state = window_ivars_state(this.window_ivars());
    let lock = &mut *window_state.lock();
    unsafe {
        if super::window_occlusion_state(lock.native_window)
            .contains(NSWindowOcclusionState::Visible)
        {
            lock.move_traffic_light();
            lock.start_display_link();
        } else {
            lock.stop_display_link();
        }
    }
}

fn handle_window_will_enter_fullscreen(this: &impl HasWindowIvars) {
    let window_state = window_ivars_state(this.window_ivars());
    let mut lock = window_state.lock();
    lock.fullscreen_restore_bounds = lock.bounds();

    if is_macos_version_at_least(NSOperatingSystemVersion::new(15, 3, 0)) {
        unsafe {
            let _: () = msg_send![lock.native_window, setTitlebarAppearsTransparent: NO];
        }
    }
}

fn handle_window_will_exit_fullscreen(this: &impl HasWindowIvars) {
    let window_state = window_ivars_state(this.window_ivars());
    let mut lock = window_state.lock();
    if is_macos_version_at_least(NSOperatingSystemVersion::new(15, 3, 0))
        && lock.transparent_titlebar
    {
        unsafe {
            let _: () = msg_send![lock.native_window, setTitlebarAppearsTransparent: YES];
        }
    }
}

fn handle_window_did_move(this: &impl HasWindowIvars) {
    let window_state = window_ivars_state(this.window_ivars());
    let mut lock = window_state.lock();
    if let Some(mut callback) = lock.moved_callback.take() {
        drop(lock);
        callback();
        window_state.lock().moved_callback = Some(callback);
    }
}

fn handle_window_did_change_key_status(this: &impl HasWindowIvars, became_key: bool) {
    let window_state = window_ivars_state(this.window_ivars());
    let mut lock = window_state.lock();
    let is_active = unsafe {
        let is_key_window: BOOL = msg_send![lock.native_window, isKeyWindow];
        is_key_window == YES
    };

    if became_key && !is_active {
        unsafe {
            let _: () = msg_send![lock.native_window, resignKeyWindow];
        }
        return;
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

    if became_key && is_active {
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
            let mut lock = window_state.lock();
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

fn handle_window_should_close(this: &impl HasWindowIvars) -> bool {
    let window_state = window_ivars_state(this.window_ivars());
    let mut lock = window_state.lock();
    if let Some(mut callback) = lock.should_close_callback.take() {
        drop(lock);
        let should_close = callback();
        window_state.lock().should_close_callback = Some(callback);
        should_close
    } else {
        true
    }
}

fn handle_dragging_entered(
    this: &impl HasWindowIvars,
    sender: &ProtocolObject<dyn NSDraggingInfo>,
) -> NSDragOperation {
    let window_state = window_ivars_state(this.window_ivars());
    let position = drag_event_position(&window_state, sender);
    let paths = external_paths_from_event(sender);
    if let Some(event) =
        paths.map(|paths| PlatformInput::FileDrop(FileDropEvent::Entered { position, paths }))
        && send_new_event(&window_state, event)
    {
        window_state.lock().external_files_dragged = true;
        NSDragOperation::Copy
    } else {
        NSDragOperation::empty()
    }
}

fn handle_dragging_updated(
    this: &impl HasWindowIvars,
    sender: &ProtocolObject<dyn NSDraggingInfo>,
) -> NSDragOperation {
    let window_state = window_ivars_state(this.window_ivars());
    let position = drag_event_position(&window_state, sender);
    if send_new_event(
        &window_state,
        PlatformInput::FileDrop(FileDropEvent::Pending { position }),
    ) {
        NSDragOperation::Copy
    } else {
        NSDragOperation::empty()
    }
}

fn handle_dragging_exited(this: &impl HasWindowIvars) {
    let window_state = window_ivars_state(this.window_ivars());
    send_new_event(
        &window_state,
        PlatformInput::FileDrop(FileDropEvent::Exited),
    );
    window_state.lock().external_files_dragged = false;
}

fn handle_perform_drag_operation(
    this: &impl HasWindowIvars,
    sender: &ProtocolObject<dyn NSDraggingInfo>,
) -> bool {
    let window_state = window_ivars_state(this.window_ivars());
    let position = drag_event_position(&window_state, sender);
    send_new_event(
        &window_state,
        PlatformInput::FileDrop(FileDropEvent::Submit { position }),
    )
}

fn handle_conclude_drag_operation(this: &impl HasWindowIvars) {
    let window_state = window_ivars_state(this.window_ivars());
    send_new_event(
        &window_state,
        PlatformInput::FileDrop(FileDropEvent::Exited),
    );
}

fn handle_reset_cursor_rects(this: &GPUIView) {
    let window_state = view_state(this);
    let cursor_style;
    let cursor_hidden;

    {
        let mut window_state = window_state.lock();
        if matches!(window_state.cursor_style, CursorStyle::None) {
            if !window_state.cursor_hidden {
                let _: () = unsafe { objc2::msg_send![NSCursor::class(), hide] };
                window_state.cursor_hidden = true;
            }
            return;
        }

        cursor_style = window_state.cursor_style;
        cursor_hidden = window_state.cursor_hidden;
    };

    let cursor = cursor_for_style(cursor_style);

    if cursor_hidden {
        let _: () = unsafe { objc2::msg_send![NSCursor::class(), unhide] };
        window_state.lock().cursor_hidden = false;
    }

    this.addCursorRect_cursor(this.bounds(), &cursor);
}

fn cursor_for_style(cursor_style: CursorStyle) -> Retained<NSCursor> {
    match cursor_style {
        CursorStyle::Arrow => NSCursor::arrowCursor(),
        CursorStyle::IBeam => NSCursor::IBeamCursor(),
        CursorStyle::Crosshair => NSCursor::crosshairCursor(),
        CursorStyle::ClosedHand => NSCursor::closedHandCursor(),
        CursorStyle::OpenHand => NSCursor::openHandCursor(),
        CursorStyle::PointingHand => NSCursor::pointingHandCursor(),
        CursorStyle::ResizeLeftRight => NSCursor::resizeLeftRightCursor(),
        CursorStyle::ResizeUpDown => NSCursor::resizeUpDownCursor(),
        CursorStyle::ResizeLeft => NSCursor::resizeLeftCursor(),
        CursorStyle::ResizeRight => NSCursor::resizeRightCursor(),
        CursorStyle::ResizeColumn => NSCursor::resizeLeftRightCursor(),
        CursorStyle::ResizeRow => NSCursor::resizeUpDownCursor(),
        CursorStyle::ResizeUp => NSCursor::resizeUpCursor(),
        CursorStyle::ResizeDown => NSCursor::resizeDownCursor(),
        CursorStyle::ResizeUpLeftDownRight => unsafe {
            objc2::msg_send![NSCursor::class(), _windowResizeNorthWestSouthEastCursor]
        },
        CursorStyle::ResizeUpRightDownLeft => unsafe {
            objc2::msg_send![NSCursor::class(), _windowResizeNorthEastSouthWestCursor]
        },
        CursorStyle::IBeamCursorForVerticalLayout => NSCursor::IBeamCursorForVerticalLayout(),
        CursorStyle::OperationNotAllowed => NSCursor::operationNotAllowedCursor(),
        CursorStyle::DragLink => NSCursor::dragLinkCursor(),
        CursorStyle::DragCopy => NSCursor::dragCopyCursor(),
        CursorStyle::ContextualMenu => NSCursor::contextualMenuCursor(),
        CursorStyle::None => unreachable!(),
    }
}

fn handle_key_event(this: &GPUIView, native_event: &NSEvent, key_equivalent: bool) -> bool {
    let window_state = view_state(this);
    let mut lock = window_state.lock();

    let window_height = lock.content_size().height;
    let event =
        unsafe { platform_input_from_native(as_legacy_event(native_event), Some(window_height)) };

    let Some(event) = event else {
        return false;
    };

    let run_callback = |event: PlatformInput| -> bool {
        let mut callback = window_state.lock().event_callback.take();
        let handled = if let Some(callback) = callback.as_mut() {
            !callback(event).propagate
        } else {
            false
        };
        window_state.lock().event_callback = callback;
        handled
    };

    match event {
        PlatformInput::KeyDown(mut key_down_event) => {
            if key_equivalent {
                lock.last_key_equivalent = Some(key_down_event.clone());
            } else if lock.last_key_equivalent.take().as_ref() == Some(&key_down_event) {
                return false;
            }

            drop(lock);

            let is_composing =
                with_input_handler(this, |input_handler| input_handler.marked_text_range())
                    .flatten()
                    .is_some();

            if is_composing
                || (key_down_event.keystroke.key_char.is_none()
                    && !key_down_event.keystroke.modifiers.control
                    && !key_down_event.keystroke.modifiers.function)
            {
                {
                    let mut lock = window_state.lock();
                    lock.keystroke_for_do_command = Some(key_down_event.keystroke.clone());
                    lock.do_command_handled.take();
                    drop(lock);
                }

                let handled = handle_event_with_input_context(this, native_event);
                window_state.lock().keystroke_for_do_command.take();
                if let Some(handled) = window_state.lock().do_command_handled.take() {
                    return handled;
                } else if handled {
                    return true;
                }

                return run_callback(PlatformInput::KeyDown(key_down_event));
            }

            let handled = run_callback(PlatformInput::KeyDown(key_down_event.clone()));
            if handled {
                return true;
            }

            if key_down_event.is_held
                && let Some(key_char) = key_down_event.keystroke.key_char.as_ref()
            {
                let handled = with_input_handler(this, |input_handler| {
                    if !input_handler.apple_press_and_hold_enabled() {
                        input_handler.replace_text_in_range(None, key_char);
                        return true;
                    }
                    false
                });
                if handled == Some(true) {
                    return true;
                }
            }

            if key_equivalent {
                return false;
            }

            handle_event_with_input_context(this, native_event)
        }

        PlatformInput::KeyUp(_) => {
            drop(lock);
            run_callback(event)
        }

        _ => false,
    }
}

fn handle_event_with_input_context(this: &GPUIView, event: &NSEvent) -> bool {
    this.inputContext()
        .is_some_and(|context| context.handleEvent(event))
}

fn handle_view_event(this: &GPUIView, native_event: &NSEvent) {
    let window_state = view_state(this);
    let weak_window_state = Arc::downgrade(&window_state);
    let mut lock = window_state.lock();
    let window_height = lock.content_size().height;
    let event =
        unsafe { platform_input_from_native(as_legacy_event(native_event), Some(window_height)) };

    if let Some(mut event) = event {
        match &mut event {
            PlatformInput::MouseDown(
                event @ MouseDownEvent {
                    button: MouseButton::Left,
                    modifiers: Modifiers { control: true, .. },
                    ..
                },
            ) => {
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
                handle_event_with_input_context(this, native_event);
                lock = window_state.lock();
            }
            PlatformInput::MouseMove(
                event @ MouseMoveEvent {
                    pressed_button: Some(_),
                    ..
                },
            ) => {
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

fn handle_view_did_change_backing_properties(this: &GPUIView) {
    let window_state = view_state(this);
    let mut lock = window_state.lock();

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
        window_state.lock().resize_callback = Some(callback);
    };
}

fn frame_size_changed(this: &GPUIView, size: Objc2NSSize) -> bool {
    let new_size = Size::<Pixels>::from(from_objc_size(size));
    let old_size = Size::<Pixels>::from(from_objc_rect(this.frame()).size);
    old_size != new_size
}

fn finish_set_frame_size(this: &GPUIView, size: Objc2NSSize) {
    let window_state = view_state(this);
    let mut lock = window_state.lock();
    let new_size = Size::<Pixels>::from(from_objc_size(size));

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

fn from_objc_size(size: Objc2NSSize) -> NSSize {
    NSSize::new(size.width, size.height)
}

fn handle_display_layer(this: &GPUIView) {
    let window_state = view_state(this);
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

fn handle_view_did_change_effective_appearance(this: &GPUIView) {
    let state = view_state(this);
    let mut lock = state.lock();
    if let Some(mut callback) = lock.appearance_changed_callback.take() {
        drop(lock);
        callback();
        state.lock().appearance_changed_callback = Some(callback);
    }
}

fn first_rect_for_character_range(this: &GPUIView, range: Objc2NSRange) -> NSRect {
    let frame = get_frame(this);
    with_input_handler(this, |input_handler| {
        input_handler.bounds_for_range(from_objc_range(range).to_range()?)
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

fn get_frame(this: &GPUIView) -> NSRect {
    unsafe {
        let state = view_state(this);
        let lock = state.lock();
        let mut frame = super::window_frame(lock.native_window);
        let content_layout_rect: core_graphics::display::CGRect =
            msg_send![lock.native_window, contentLayoutRect];
        let style_mask = super::window_style_mask(lock.native_window);
        if !style_mask.contains(NSWindowStyleMask::FullSizeContentView) {
            frame.origin.y -= frame.size.height - content_layout_rect.size.height;
        }
        frame
    }
}

fn insert_text(this: &GPUIView, text: &AnyObject, replacement_range: Objc2NSRange) {
    let Some(text) = nsstring_from_insert_text(text) else {
        return;
    };
    let replacement_range = from_objc_range(replacement_range).to_range();
    with_input_handler(this, |input_handler| {
        input_handler.replace_text_in_range(replacement_range, &text)
    });
}

fn set_marked_text(
    this: &GPUIView,
    text: &AnyObject,
    selected_range: Objc2NSRange,
    replacement_range: Objc2NSRange,
) {
    let Some(text) = nsstring_from_insert_text(text) else {
        return;
    };
    let selected_range = from_objc_range(selected_range).to_range();
    let replacement_range = from_objc_range(replacement_range).to_range();
    with_input_handler(this, |input_handler| {
        input_handler.replace_and_mark_text_in_range(replacement_range, &text, selected_range)
    });
}

fn nsstring_from_insert_text(text: &AnyObject) -> Option<String> {
    if let Some(attributed) = text.downcast_ref::<NSAttributedString>() {
        Some(attributed.string().to_string())
    } else if let Some(string) = text.downcast_ref::<NSString>() {
        Some(string.to_string())
    } else {
        None
    }
}

fn attributed_substring_for_proposed_range(
    this: &GPUIView,
    range: Objc2NSRange,
    actual_range: NSRangePointer,
) -> Option<Retained<NSAttributedString>> {
    with_input_handler(this, |input_handler| {
        let range = from_objc_range(range).to_range()?;
        if range.is_empty() {
            return None;
        }
        let mut adjusted: Option<Range<usize>> = None;

        let selected_text = input_handler.text_for_range(range.clone(), &mut adjusted)?;
        if let Some(adjusted) = adjusted
            && adjusted != range
            && !actual_range.is_null()
        {
            unsafe {
                actual_range.write(to_objc_range(adjusted));
            }
        }
        Some(NSAttributedString::from_nsstring(&NSString::from_str(
            &selected_text,
        )))
    })
    .flatten()
}

fn handle_do_command_by_selector(this: &GPUIView) {
    let state = view_state(this);
    let mut lock = state.lock();
    let keystroke = lock.keystroke_for_do_command.take();
    let mut event_callback = lock.event_callback.take();
    drop(lock);

    if let Some((keystroke, mut callback)) = keystroke.zip(event_callback.as_mut()) {
        let handled = (callback)(PlatformInput::KeyDown(KeyDownEvent {
            keystroke,
            is_held: false,
        }));
        state.lock().do_command_handled = Some(!handled.propagate);
    }

    state.lock().event_callback = event_callback;
}

fn screen_point_to_gpui_point(this: &GPUIView, position: NSPoint) -> Point<Pixels> {
    let frame = get_frame(this);
    let window_x = position.x - frame.origin.x;
    let window_y = frame.size.height - (position.y - frame.origin.y);

    point(px(window_x as f32), px(window_y as f32))
}

fn external_paths_from_event(
    dragging_info: &ProtocolObject<dyn NSDraggingInfo>,
) -> Option<ExternalPaths> {
    let mut paths = SmallVec::new();
    let pasteboard = dragging_info.draggingPasteboard();
    let filenames_type = unsafe { ns_string("NSFilenamesPboardType") };
    let filenames: id = unsafe {
        msg_send![
            Retained::as_ptr(&pasteboard) as id,
            propertyListForType: filenames_type
        ]
    };
    if filenames == nil {
        return None;
    }
    let count = unsafe { super::array_count(filenames) };
    for i in 0..count {
        let file = unsafe { super::array_object_at_index(filenames, i) };
        let path = unsafe { file.to_str().to_owned() };
        paths.push(PathBuf::from(path))
    }
    Some(ExternalPaths(paths))
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

fn drag_event_position(
    window_state: &Mutex<MacWindowState>,
    dragging_info: &ProtocolObject<dyn NSDraggingInfo>,
) -> Point<Pixels> {
    let drag_location = from_objc_point(dragging_info.draggingLocation());
    convert_mouse_position(drag_location, window_state.lock().content_size().height)
}

fn with_input_handler<F, R>(window: &GPUIView, f: F) -> Option<R>
where
    F: FnOnce(&mut PlatformInputHandler) -> R,
{
    let window_state = view_state(window);
    let mut lock = window_state.lock();
    if let Some(mut input_handler) = lock.input_handler.take() {
        drop(lock);
        let result = f(&mut input_handler);
        window_state.lock().input_handler = Some(input_handler);
        Some(result)
    } else {
        None
    }
}

fn remove_layer_background(layer: &CALayer) {
    unsafe {
        let layer = layer as *const CALayer as id;
        let _: () = msg_send![layer, setBackgroundColor:nil];

        let class_name: id = msg_send![layer, className];
        let is_chameleon_layer: BOOL =
            msg_send![class_name, isEqualToString: ns_string("CAChameleonLayer")];
        if is_chameleon_layer == YES {
            let _: () = msg_send![layer, setHidden: YES];
            return;
        }

        let filters: id = msg_send![layer, filters];
        if !filters.is_null() {
            let test_string = ns_string("Saturat");
            let count = super::array_count(filters);
            for i in 0..count {
                let filter = super::array_object_at_index(filters, i);
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
            let count = super::array_count(sublayers);
            for i in 0..count {
                let sublayer = super::array_object_at_index(sublayers, i);
                if let Some(sublayer) = sublayer.cast::<CALayer>().as_ref() {
                    remove_layer_background(sublayer);
                }
            }
        }
    }
}

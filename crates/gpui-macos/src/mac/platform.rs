use super::tray::MacTray;
use super::{
    MacKeyboardLayout, MacKeyboardMapper, events::key_to_native, global_hotkey::NativeHotkeyMapper,
    global_point_to_native_screen_point, renderer,
};
use crate::{
    Action, AnyWindowHandle, BackgroundExecutor, ClipboardItem, ClipboardString, CursorStyle,
    DevicePixels, ForegroundExecutor, GpuResourceBudget, Image, ImageFormat, KeyContext, Keymap,
    MacDispatcher, MacDisplay, MacWindow, Menu, MenuItem, OsMenu, OwnedMenu, PathPromptOptions,
    Platform, PlatformDisplay, PlatformKeyboardLayout, PlatformKeyboardMapper, PlatformTextSystem,
    PlatformWindow, QuitMode, RendererCacheStats, Result, SemanticVersion, SharedString, Size,
    SystemMenuType, Task, ThermalState, TrayAnchor, TrayIconClickEvent, TrayIconEvent,
    TrayIconRenderingMode, TrayMenuItem, WindowAppearance, WindowParams,
};
use anyhow::{Context as _, anyhow};
use block2::RcBlock;
use core_foundation::{
    base::{CFRelease, CFType, CFTypeRef, OSStatus, TCFType},
    boolean::CFBoolean,
    data::CFData,
    dictionary::{CFDictionary, CFDictionaryRef, CFMutableDictionary},
    runloop::CFRunLoopRun,
    string::{CFString, CFStringRef},
};
use futures::channel::oneshot;
use itertools::Itertools;
use objc2::rc::{Allocated, Retained};
use objc2::runtime::{AnyObject, ProtocolObject};
use objc2::{AnyThread, ClassType, DefinedClass, MainThreadMarker, MainThreadOnly, define_class};
use objc2_app_kit::{
    NSApplication as Objc2NSApplication,
    NSApplicationActivationPolicy as Objc2NSApplicationActivationPolicy, NSApplicationDelegate,
    NSAttributedStringAppKitDocumentFormats, NSDocumentController, NSEvent as Objc2NSEvent,
    NSEventMask as Objc2NSEventMask, NSEventModifierFlags as Objc2NSEventModifierFlags,
    NSImage as Objc2NSImage, NSMenu as Objc2NSMenu, NSMenuDelegate, NSMenuItem as Objc2NSMenuItem,
    NSMenuItemValidation, NSModalResponse as Objc2NSModalResponse,
    NSModalResponseOK as Objc2NSModalResponseOK, NSOpenPanel as Objc2NSOpenPanel,
    NSPasteboard as Objc2NSPasteboard, NSPasteboardType,
    NSPasteboardTypePNG as Objc2NSPasteboardTypePNG,
    NSPasteboardTypeRTF as Objc2NSPasteboardTypeRTF,
    NSPasteboardTypeRTFD as Objc2NSPasteboardTypeRTFD,
    NSPasteboardTypeString as Objc2NSPasteboardTypeString,
    NSPasteboardTypeTIFF as Objc2NSPasteboardTypeTIFF, NSSavePanel as Objc2NSSavePanel, NSScroller,
    NSScrollerStyle, NSTextInputContextKeyboardSelectionDidChangeNotification,
    NSWorkspace as Objc2NSWorkspace, NSWorkspaceDidWakeNotification,
    NSWorkspaceSessionDidBecomeActiveNotification, NSWorkspaceSessionDidResignActiveNotification,
    NSWorkspaceWillPowerOffNotification, NSWorkspaceWillSleepNotification,
};
use objc2_foundation::{
    NSArray, NSAttributedString as Objc2NSAttributedString,
    NSAutoreleasePool as Objc2NSAutoreleasePool, NSBundle as Objc2NSBundle, NSData as Objc2NSData,
    NSDictionary, NSMutableAttributedString, NSNotification, NSNotificationCenter,
    NSObjectProtocol, NSProcessInfo as Objc2NSProcessInfo,
    NSProcessInfoThermalState as Objc2NSProcessInfoThermalState,
    NSProcessInfoThermalStateDidChangeNotification, NSRange as Objc2NSRange, NSSize as Objc2NSSize,
    NSString as Objc2NSString, NSURL as Objc2NSURL, NSUserDefaults,
};
use objc2_user_notifications::{
    UNMutableNotificationContent, UNNotificationRequest, UNUserNotificationCenter,
};
use parking_lot::Mutex;
use std::{
    cell::Cell,
    convert::TryInto,
    ffi::{CStr, OsStr, c_void},
    os::unix::ffi::OsStrExt,
    path::{Path, PathBuf},
    process::Command,
    ptr,
    rc::Rc,
    sync::{Arc, OnceLock},
};
use strum::IntoEnumIterator;
use util::ResultExt;

type ObjcId = *mut AnyObject;

#[allow(non_camel_case_types)]
type id = ObjcId;

/// `NX_SUBTYPE_AUX_CONTROL_BUTTONS` — media keys arrive as system-defined
/// events with this subtype. The numeric value collides with
/// `NSEventSubtype::ScreenChanged`.
const NX_SUBTYPE_AUX_CONTROL_BUTTONS: i16 = 8;

fn main_thread_marker() -> MainThreadMarker {
    unsafe { MainThreadMarker::new_unchecked() }
}

struct ApplicationIvars {
    platform: Cell<*const MacPlatform>,
}

define_class!(
    // SAFETY: `NSApplication` can be subclassed and `GPUIApplication` does not
    // implement `Drop`. `init` initializes ivars before AppKit's singleton
    // `sharedApplication` returns the instance.
    #[unsafe(super(objc2_app_kit::NSApplication))]
    #[thread_kind = MainThreadOnly]
    #[name = "GPUIApplication"]
    #[ivars = ApplicationIvars]
    struct GPUIApplication;

    impl GPUIApplication {
        #[unsafe(method_id(init))]
        fn init(this: Allocated<Self>) -> Option<Retained<Self>> {
            let this = this.set_ivars(ApplicationIvars {
                platform: Cell::new(ptr::null()),
            });
            unsafe { objc2::msg_send![super(this), init] }
        }
    }
);

struct DelegateIvars {
    platform: Cell<*const MacPlatform>,
}

define_class!(
    // SAFETY: `NSResponder` can be subclassed and `GPUIApplicationDelegate`
    // does not implement `Drop`.
    #[unsafe(super(objc2_app_kit::NSResponder))]
    #[thread_kind = MainThreadOnly]
    #[name = "GPUIApplicationDelegate"]
    #[ivars = DelegateIvars]
    struct GPUIApplicationDelegate;

    unsafe impl NSObjectProtocol for GPUIApplicationDelegate {}

    unsafe impl NSApplicationDelegate for GPUIApplicationDelegate {
        #[unsafe(method(applicationWillFinishLaunching:))]
        fn application_will_finish_launching(&self, _notification: &NSNotification) {
            disable_autofill_heuristic_controller();
        }

        #[unsafe(method(applicationDidFinishLaunching:))]
        fn application_did_finish_launching(&self, _notification: &NSNotification) {
            register_launch_observers(self);

            let platform = delegate_platform(self);
            let callback = platform.0.lock().finish_launching.take();
            if let Some(callback) = callback {
                callback();
            }
        }

        #[unsafe(method(applicationShouldHandleReopen:hasVisibleWindows:))]
        fn application_should_handle_reopen_has_visible_windows(
            &self,
            _sender: &Objc2NSApplication,
            has_visible_windows: bool,
        ) -> bool {
            if !has_visible_windows {
                let platform = delegate_platform(self);
                let mut lock = platform.0.lock();
                if let Some(mut callback) = lock.reopen.take() {
                    drop(lock);
                    callback();
                    platform.0.lock().reopen.get_or_insert(callback);
                }
            }
            true
        }

        #[unsafe(method(applicationWillTerminate:))]
        fn application_will_terminate(&self, _notification: &NSNotification) {
            let platform = delegate_platform(self);
            let mut lock = platform.0.lock();
            if let Some(mut callback) = lock.quit.take() {
                drop(lock);
                callback();
                platform.0.lock().quit.get_or_insert(callback);
            }
        }

        #[unsafe(method_id(applicationDockMenu:))]
        fn application_dock_menu(
            &self,
            _sender: &Objc2NSApplication,
        ) -> Option<Retained<Objc2NSMenu>> {
            let state = delegate_platform(self).0.lock();
            match state.dock_menu {
                Some(ptr) => unsafe { Retained::retain(ptr as *mut Objc2NSMenu) },
                None => None,
            }
        }

        #[unsafe(method(application:openURLs:))]
        fn application_open_urls(
            &self,
            _application: &Objc2NSApplication,
            urls: &NSArray<Objc2NSURL>,
        ) {
            let urls = urls
                .iter()
                .filter_map(|url| match url.absoluteString() {
                    Some(string) => Some(string.to_string()),
                    None => {
                        log::error!("error converting path to string: missing absoluteString");
                        None
                    }
                })
                .collect::<Vec<_>>();
            let platform = delegate_platform(self);
            let mut lock = platform.0.lock();
            if let Some(mut callback) = lock.open_urls.take() {
                drop(lock);
                callback(urls);
                platform.0.lock().open_urls.get_or_insert(callback);
            }
        }

        #[unsafe(method(applicationShouldTerminateAfterLastWindowClosed:))]
        fn application_should_terminate_after_last_window_closed(
            &self,
            _sender: &Objc2NSApplication,
        ) -> bool {
            // Lifecycle decisions are owned by the core application via `QuitMode`;
            // the platform layer always defers to that and never auto-terminates on
            // window close. Core will call `Platform::quit` when the mode requires it.
            false
        }
    }

    unsafe impl NSMenuDelegate for GPUIApplicationDelegate {
        #[unsafe(method(menuWillOpen:))]
        fn menu_will_open(&self, _menu: &Objc2NSMenu) {
            let platform = delegate_platform(self);
            let mut lock = platform.0.lock();
            if let Some(mut callback) = lock.will_open_menu.take() {
                drop(lock);
                callback();
                platform.0.lock().will_open_menu.get_or_insert(callback);
            }
        }
    }

    unsafe impl NSMenuItemValidation for GPUIApplicationDelegate {
        #[unsafe(method(validateMenuItem:))]
        fn validate_menu_item(&self, item: &Objc2NSMenuItem) -> bool {
            let mut result = false;
            let platform = delegate_platform(self);
            let mut lock = platform.0.lock();
            if let Some(mut callback) = lock.validate_menu_command.take() {
                let index = item.tag() as usize;
                if let Some(action) = lock.menu_actions.get(index) {
                    let action = action.boxed_clone();
                    drop(lock);
                    result = callback(action.as_ref());
                }
                platform
                    .0
                    .lock()
                    .validate_menu_command
                    .get_or_insert(callback);
            }
            result
        }
    }

    impl GPUIApplicationDelegate {
        #[unsafe(method(handleGPUIMenuItem:))]
        fn handle_gpui_menu_item(&self, item: &Objc2NSMenuItem) {
            dispatch_menu_item(self, item);
        }

        #[unsafe(method(cut:))]
        fn cut(&self, item: &Objc2NSMenuItem) {
            dispatch_menu_item(self, item);
        }

        #[unsafe(method(copy:))]
        fn copy(&self, item: &Objc2NSMenuItem) {
            dispatch_menu_item(self, item);
        }

        #[unsafe(method(paste:))]
        fn paste(&self, item: &Objc2NSMenuItem) {
            dispatch_menu_item(self, item);
        }

        #[unsafe(method(selectAll:))]
        fn select_all(&self, item: &Objc2NSMenuItem) {
            dispatch_menu_item(self, item);
        }

        #[unsafe(method(undo:))]
        fn undo(&self, item: &Objc2NSMenuItem) {
            dispatch_menu_item(self, item);
        }

        #[unsafe(method(redo:))]
        fn redo(&self, item: &Objc2NSMenuItem) {
            dispatch_menu_item(self, item);
        }

        #[unsafe(method(handleTrayMenuItem:))]
        fn handle_tray_menu_item(&self, item: &Objc2NSMenuItem) {
            let Some(id) = represented_string(item) else {
                return;
            };
            let platform = delegate_platform(self) as *const MacPlatform;

            use super::dispatcher::{dispatch_get_main_queue, dispatch_sys::dispatch_async_f};

            struct TrayActionCtx {
                platform: *const MacPlatform,
                id: SharedString,
            }

            let ctx = Box::into_raw(Box::new(TrayActionCtx { platform, id }));

            unsafe extern "C" fn invoke(ctx_ptr: *mut c_void) {
                let ctx = unsafe { Box::from_raw(ctx_ptr as *mut TrayActionCtx) };
                let platform = unsafe { &*ctx.platform };
                let mut lock = platform.0.lock();
                if let Some(mut callback) = lock.tray_menu_callback.take() {
                    drop(lock);
                    callback(ctx.id);
                    platform.0.lock().tray_menu_callback.get_or_insert(callback);
                }
            }

            unsafe {
                dispatch_async_f(dispatch_get_main_queue(), ctx as *mut c_void, Some(invoke));
            }
        }

        #[unsafe(method(handleTrayPanelClick:))]
        fn handle_tray_panel_click(&self, _sender: &AnyObject) {
            let platform_ptr = delegate_platform(self) as *const MacPlatform;

            use super::dispatcher::{dispatch_get_main_queue, dispatch_sys::dispatch_async_f};

            unsafe extern "C" fn invoke(ctx_ptr: *mut c_void) {
                let platform = unsafe { &*(ctx_ptr as *const MacPlatform) };
                let mut lock = platform.0.lock();
                let mut event_callback = lock.tray_icon_callback.take();
                let mut click_callback = lock.tray_icon_click_callback.take();
                drop(lock);

                let event = TrayIconClickEvent::new(TrayIconEvent::LeftClick);
                if let Some(ref mut callback) = event_callback {
                    callback(event.kind.clone());
                }
                if let Some(ref mut callback) = click_callback {
                    callback(event);
                }

                let mut lock = platform.0.lock();
                if let Some(callback) = event_callback {
                    lock.tray_icon_callback.get_or_insert(callback);
                }
                if let Some(callback) = click_callback {
                    lock.tray_icon_click_callback.get_or_insert(callback);
                }
            }

            unsafe {
                dispatch_async_f(
                    dispatch_get_main_queue(),
                    platform_ptr as *mut c_void,
                    Some(invoke),
                );
            }
        }

        #[unsafe(method(handleContextMenuItem:))]
        fn handle_context_menu_item(&self, item: &Objc2NSMenuItem) {
            let Some(id) = represented_string(item) else {
                return;
            };
            let platform = delegate_platform(self);
            let mut lock = platform.0.lock();
            if let Some(mut callback) = lock.context_menu_callback.take() {
                drop(lock);
                callback(id);
                platform
                    .0
                    .lock()
                    .context_menu_callback
                    .get_or_insert(callback);
            }
        }

        #[unsafe(method(onKeyboardLayoutChange:))]
        fn on_keyboard_layout_change(&self, _notification: &NSNotification) {
            let platform = delegate_platform(self);
            let mut lock = platform.0.lock();
            let keyboard_layout = MacKeyboardLayout::new();
            lock.keyboard_mapper = Rc::new(MacKeyboardMapper::new(keyboard_layout.id()));
            lock.global_hotkey_mapper = NativeHotkeyMapper::new();
            if !lock.global_hotkey_registrations.is_empty() {
                reregister_global_hotkeys_for_current_layout(&mut lock);
            }
            if let Some(mut callback) = lock.on_keyboard_layout_change.take() {
                drop(lock);
                callback();
                platform
                    .0
                    .lock()
                    .on_keyboard_layout_change
                    .get_or_insert(callback);
            }
        }

        #[unsafe(method(onThermalStateChange:))]
        fn on_thermal_state_change(&self, _notification: &NSNotification) {
            let platform_ptr = delegate_platform(self) as *const MacPlatform;

            use super::dispatcher::{dispatch_get_main_queue, dispatch_sys::dispatch_async_f};

            unsafe extern "C" fn invoke(context: *mut c_void) {
                let platform = unsafe { &*(context as *const MacPlatform) };
                let mut lock = platform.0.lock();
                if let Some(mut callback) = lock.on_thermal_state_change.take() {
                    drop(lock);
                    callback();
                    platform
                        .0
                        .lock()
                        .on_thermal_state_change
                        .get_or_insert(callback);
                }
            }

            unsafe {
                dispatch_async_f(
                    dispatch_get_main_queue(),
                    platform_ptr as *mut c_void,
                    Some(invoke),
                );
            }
        }

        #[unsafe(method(handleSystemPowerEvent:))]
        fn handle_system_power_event(&self, notification: &NSNotification) {
            let Some(event) = system_power_event_from_notification(notification) else {
                return;
            };
            let platform = delegate_platform(self);
            let mut lock = platform.0.lock();
            if let Some(mut callback) = lock.system_power_callback.take() {
                drop(lock);
                callback(event);
                platform
                    .0
                    .lock()
                    .system_power_callback
                    .get_or_insert(callback);
            }
        }
    }
);

impl GPUIApplication {
    fn shared(mtm: MainThreadMarker) -> Retained<Self> {
        let _ = mtm;
        unsafe { objc2::msg_send![Self::class(), sharedApplication] }
    }

    fn set_platform(&self, platform: *const MacPlatform) {
        self.ivars().platform.set(platform);
    }
}

impl GPUIApplicationDelegate {
    fn new(mtm: MainThreadMarker, platform: *const MacPlatform) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(DelegateIvars {
            platform: Cell::new(platform),
        });
        unsafe { objc2::msg_send![super(this), init] }
    }

    fn set_platform(&self, platform: *const MacPlatform) {
        self.ivars().platform.set(platform);
    }
}

fn shared_application() -> Retained<GPUIApplication> {
    GPUIApplication::shared(main_thread_marker())
}

fn gpui_delegate(app: &GPUIApplication) -> Option<Retained<GPUIApplicationDelegate>> {
    unsafe { objc2::msg_send![app, delegate] }
}

fn delegate_platform(this: &GPUIApplicationDelegate) -> &MacPlatform {
    let ptr = this.ivars().platform.get();
    assert!(
        !ptr.is_null(),
        "GPUIApplicationDelegate platform ivar is null"
    );
    unsafe { &*ptr }
}

fn represented_string(item: &Objc2NSMenuItem) -> Option<SharedString> {
    let represented = item.representedObject()?;
    let string = represented.downcast::<Objc2NSString>().ok()?;
    Some(string.to_string().into())
}

fn dispatch_menu_item(this: &GPUIApplicationDelegate, item: &Objc2NSMenuItem) {
    let platform = delegate_platform(this);
    let mut lock = platform.0.lock();
    if let Some(mut callback) = lock.menu_command.take() {
        let index = item.tag() as usize;
        if let Some(action) = lock.menu_actions.get(index) {
            let action = action.boxed_clone();
            drop(lock);
            callback(&*action);
        }
        platform.0.lock().menu_command.get_or_insert(callback);
    }
}

fn disable_autofill_heuristic_controller() {
    let user_defaults = NSUserDefaults::standardUserDefaults();
    let name = Objc2NSString::from_str("NSAutoFillHeuristicControllerEnabled");
    if user_defaults.objectForKey(&name).is_none() {
        user_defaults.setBool_forKey(false, &name);
    }
}

fn clear_menu_delegates(menu: &Objc2NSMenu) {
    menu.setDelegate(None);
    for item in menu.itemArray().iter() {
        if let Some(submenu) = item.submenu() {
            clear_menu_delegates(&submenu);
        }
    }
}

fn unhook_application_delegate(
    app: &GPUIApplication,
    delegate: &GPUIApplicationDelegate,
    state: &MacPlatformState,
) {
    let observer = AsRef::<AnyObject>::as_ref(delegate);
    unsafe {
        NSNotificationCenter::defaultCenter().removeObserver(observer);
        Objc2NSWorkspace::sharedWorkspace()
            .notificationCenter()
            .removeObserver(observer);
    }

    if let Some(menu) = app.mainMenu() {
        clear_menu_delegates(&menu);
    }
    if let Some(menu) = app.servicesMenu() {
        clear_menu_delegates(&menu);
    }
    if let Some(ptr) = state.dock_menu {
        if let Some(menu) = unsafe { Retained::retain(ptr as *mut Objc2NSMenu) } {
            clear_menu_delegates(&menu);
        }
    }
    if let Some(tray) = state.tray.as_ref() {
        tray.disconnect_app_delegate();
    }

    app.setDelegate(None);
}

fn register_launch_observers(this: &GPUIApplicationDelegate) {
    let notification_center = NSNotificationCenter::defaultCenter();
    let observer = AsRef::<AnyObject>::as_ref(this);
    let process_info = Objc2NSProcessInfo::processInfo();
    unsafe {
        notification_center.addObserver_selector_name_object(
            observer,
            objc2::sel!(onKeyboardLayoutChange:),
            Some(NSTextInputContextKeyboardSelectionDidChangeNotification),
            None,
        );
        notification_center.addObserver_selector_name_object(
            observer,
            objc2::sel!(onThermalStateChange:),
            Some(NSProcessInfoThermalStateDidChangeNotification),
            Some(AsRef::<AnyObject>::as_ref(&*process_info)),
        );

        let workspace_center = Objc2NSWorkspace::sharedWorkspace().notificationCenter();
        for (name, selector) in [
            (
                NSWorkspaceWillSleepNotification,
                objc2::sel!(handleSystemPowerEvent:),
            ),
            (
                NSWorkspaceDidWakeNotification,
                objc2::sel!(handleSystemPowerEvent:),
            ),
            (
                NSWorkspaceSessionDidResignActiveNotification,
                objc2::sel!(handleSystemPowerEvent:),
            ),
            (
                NSWorkspaceSessionDidBecomeActiveNotification,
                objc2::sel!(handleSystemPowerEvent:),
            ),
            (
                NSWorkspaceWillPowerOffNotification,
                objc2::sel!(handleSystemPowerEvent:),
            ),
        ] {
            workspace_center.addObserver_selector_name_object(observer, selector, Some(name), None);
        }
    }
}

fn system_power_event_from_notification(
    notification: &NSNotification,
) -> Option<crate::SystemPowerEvent> {
    match notification.name().to_string().as_str() {
        "NSWorkspaceWillSleepNotification" => Some(crate::SystemPowerEvent::Suspend),
        "NSWorkspaceDidWakeNotification" => Some(crate::SystemPowerEvent::Resume),
        "NSWorkspaceSessionDidResignActiveNotification" => {
            Some(crate::SystemPowerEvent::LockScreen)
        }
        "NSWorkspaceSessionDidBecomeActiveNotification" => {
            Some(crate::SystemPowerEvent::UnlockScreen)
        }
        "NSWorkspaceWillPowerOffNotification" => Some(crate::SystemPowerEvent::Shutdown),
        _ => None,
    }
}

pub(crate) struct MacPlatform(Mutex<MacPlatformState>, MainThreadMarker);

pub(crate) struct MacPlatformState {
    background_executor: BackgroundExecutor,
    foreground_executor: ForegroundExecutor,
    text_system: Arc<dyn PlatformTextSystem>,
    renderer_context: renderer::Context,
    atlas_initial_size: Size<DevicePixels>,
    headless: bool,
    pasteboard: Retained<Objc2NSPasteboard>,
    text_hash_pasteboard_type: Retained<Objc2NSString>,
    metadata_pasteboard_type: Retained<Objc2NSString>,
    reopen: Option<Box<dyn FnMut()>>,
    on_keyboard_layout_change: Option<Box<dyn FnMut()>>,
    on_thermal_state_change: Option<Box<dyn FnMut()>>,
    quit: Option<Box<dyn FnMut() -> bool>>,
    menu_command: Option<Box<dyn FnMut(&dyn Action)>>,
    validate_menu_command: Option<Box<dyn FnMut(&dyn Action) -> bool>>,
    will_open_menu: Option<Box<dyn FnMut()>>,
    menu_actions: Vec<Box<dyn Action>>,
    open_urls: Option<Box<dyn FnMut(Vec<String>)>>,
    finish_launching: Option<Box<dyn FnOnce()>>,
    dock_menu: Option<id>,
    menus: Option<Vec<OwnedMenu>>,
    keyboard_mapper: Rc<MacKeyboardMapper>,
    global_hotkey_mapper: NativeHotkeyMapper,
    tray: Option<MacTray>,
    tray_icon_rendering_mode: TrayIconRenderingMode,
    tray_icon_callback: Option<Box<dyn FnMut(TrayIconEvent)>>,
    tray_icon_click_callback: Option<Box<dyn FnMut(TrayIconClickEvent)>>,
    tray_menu_callback: Option<Box<dyn FnMut(SharedString)>>,
    global_hotkey_callback: Option<Box<dyn FnMut(u32)>>,
    global_hotkey_handler: Option<EventHandlerRef>,
    global_hotkey_registrations: std::collections::HashMap<u32, RegisteredGlobalHotkey>,
    system_power_callback: Option<Box<dyn FnMut(crate::SystemPowerEvent)>>,
    network_change_callback: Option<Box<dyn FnMut(crate::NetworkStatus)>>,
    media_key_callback: Option<Box<dyn FnMut(crate::MediaKeyEvent)>>,
    media_key_monitor: Option<id>,
    network_monitor: Option<*const c_void>,
    attention_request_id: isize,
    context_menu_callback: Option<Box<dyn FnMut(crate::SharedString)>>,
}

struct RegisteredGlobalHotkey {
    keystroke: crate::Keystroke,
    hotkey_ref: EventHotKeyRef,
}

impl Default for MacPlatform {
    fn default() -> Self {
        Self::new(false)
    }
}

impl MacPlatform {
    pub(crate) fn new(headless: bool) -> Self {
        let marker = MainThreadMarker::new().expect("Mac platform not created on main thread");
        Self::new_with_marker(headless, marker)
    }

    fn new_with_marker(headless: bool, marker: MainThreadMarker) -> Self {
        let dispatcher = Arc::new(MacDispatcher::new());

        #[cfg(feature = "font-kit")]
        let text_system = Arc::new(crate::MacTextSystem::new());

        #[cfg(not(feature = "font-kit"))]
        let text_system = Arc::new(crate::NoopTextSystem::new());

        let keyboard_layout = MacKeyboardLayout::new();
        let keyboard_mapper = Rc::new(MacKeyboardMapper::new(keyboard_layout.id()));
        let global_hotkey_mapper = NativeHotkeyMapper::new();
        #[allow(unused_unsafe)]
        let pasteboard = unsafe { Objc2NSPasteboard::generalPasteboard() };

        let state = Mutex::new(MacPlatformState {
            headless,
            text_system,
            background_executor: BackgroundExecutor::new(dispatcher.clone()),
            foreground_executor: ForegroundExecutor::new(dispatcher),
            renderer_context: renderer::Context::default(),
            atlas_initial_size: crate::AppResourceProfile::default().gpu.atlas_size(),
            pasteboard,
            text_hash_pasteboard_type: Objc2NSString::from_str("zed-text-hash"),
            metadata_pasteboard_type: Objc2NSString::from_str("zed-metadata"),
            reopen: None,
            quit: None,
            menu_command: None,
            validate_menu_command: None,
            will_open_menu: None,
            menu_actions: Default::default(),
            open_urls: None,
            finish_launching: None,
            dock_menu: None,
            on_keyboard_layout_change: None,
            on_thermal_state_change: None,
            menus: None,
            keyboard_mapper,
            global_hotkey_mapper,
            tray: None,
            tray_icon_rendering_mode: TrayIconRenderingMode::default(),
            tray_icon_callback: None,
            tray_icon_click_callback: None,
            tray_menu_callback: None,
            global_hotkey_callback: None,
            global_hotkey_handler: None,
            global_hotkey_registrations: std::collections::HashMap::new(),
            system_power_callback: None,
            network_change_callback: None,
            media_key_callback: None,
            media_key_monitor: None,
            network_monitor: None,
            attention_request_id: 0,
            context_menu_callback: None,
        });
        Self(state, marker)
    }

    fn ensure_tray(state: &mut MacPlatformState) -> &MacTray {
        state.tray.get_or_insert_with(MacTray::new)
    }

    #[allow(unused_unsafe)]
    unsafe fn read_from_pasteboard(
        &self,
        pasteboard: &Objc2NSPasteboard,
        kind: &NSPasteboardType,
    ) -> Option<Vec<u8>> {
        let data = unsafe { pasteboard.dataForType(kind) }?;
        Some(data.to_vec())
    }

    fn set_menu_delegate(menu: &Objc2NSMenu, delegate: &GPUIApplicationDelegate) {
        menu.setDelegate(Some(ProtocolObject::from_ref(delegate)));
    }

    #[allow(unused_unsafe)]
    unsafe fn create_menu_icon(icon_bytes: &[u8]) -> Option<Retained<Objc2NSImage>> {
        let ns_data = Objc2NSData::with_bytes(icon_bytes);
        let image = Objc2NSImage::initWithData(Objc2NSImage::alloc(), &ns_data)?;
        unsafe {
            image.setSize(Objc2NSSize {
                width: 16.0,
                height: 16.0,
            });
            image.setTemplate(true);
        }
        Some(image)
    }

    unsafe fn create_menu_bar(
        &self,
        menus: &Vec<Menu>,
        delegate: &GPUIApplicationDelegate,
        actions: &mut Vec<Box<dyn Action>>,
        keymap: &Keymap,
    ) -> Retained<Objc2NSMenu> {
        unsafe {
            let application_menu = Objc2NSMenu::new(main_thread_marker());
            Self::set_menu_delegate(&application_menu, delegate);

            for menu_config in menus {
                let menu = Objc2NSMenu::new(main_thread_marker());
                let menu_title = Objc2NSString::from_str(&menu_config.name);
                menu.setTitle(&menu_title);
                Self::set_menu_delegate(&menu, delegate);

                for item_config in &menu_config.items {
                    let item = Self::create_menu_item(item_config, delegate, actions, keymap);
                    menu.addItem(&item);
                }

                let menu_item = Objc2NSMenuItem::new(main_thread_marker());
                menu_item.setTitle(&menu_title);
                menu_item.setSubmenu(Some(&menu));

                if let Some(icon_bytes) = &menu_config.icon {
                    if let Some(image) = Self::create_menu_icon(icon_bytes) {
                        menu_item.setImage(Some(&image));
                    }
                }

                application_menu.addItem(&menu_item);

                if menu_config.name == "Window" {
                    shared_application().setWindowsMenu(Some(&menu));
                }
            }

            application_menu
        }
    }

    unsafe fn create_dock_menu(
        &self,
        menu_items: Vec<MenuItem>,
        delegate: &GPUIApplicationDelegate,
        actions: &mut Vec<Box<dyn Action>>,
        keymap: &Keymap,
    ) -> Retained<Objc2NSMenu> {
        unsafe {
            let dock_menu = Objc2NSMenu::new(main_thread_marker());
            Self::set_menu_delegate(&dock_menu, delegate);
            for item_config in menu_items {
                let item = Self::create_menu_item(&item_config, delegate, actions, keymap);
                dock_menu.addItem(&item);
            }

            dock_menu
        }
    }

    unsafe fn create_menu_item(
        item: &MenuItem,
        delegate: &GPUIApplicationDelegate,
        actions: &mut Vec<Box<dyn Action>>,
        keymap: &Keymap,
    ) -> Retained<Objc2NSMenuItem> {
        static DEFAULT_CONTEXT: OnceLock<Vec<KeyContext>> = OnceLock::new();

        unsafe {
            match item {
                MenuItem::Separator => Objc2NSMenuItem::separatorItem(main_thread_marker()),
                MenuItem::Action {
                    name,
                    action,
                    os_action,
                } => {
                    // Note that this is intentionally using earlier bindings, whereas typically
                    // later ones take display precedence. See the discussion on
                    // https://github.com/zed-industries/zed/issues/23621
                    let keystrokes = keymap
                        .bindings_for_action(action.as_ref())
                        .find_or_first(|binding| {
                            binding.predicate().is_none_or(|predicate| {
                                predicate.eval(DEFAULT_CONTEXT.get_or_init(|| {
                                    let mut workspace_context = KeyContext::new_with_defaults();
                                    workspace_context.add("Workspace");
                                    let mut pane_context = KeyContext::new_with_defaults();
                                    pane_context.add("Pane");
                                    let mut editor_context = KeyContext::new_with_defaults();
                                    editor_context.add("Editor");

                                    pane_context.extend(&editor_context);
                                    workspace_context.extend(&pane_context);
                                    vec![workspace_context]
                                }))
                            })
                        })
                        .map(|binding| binding.keystrokes());

                    let selector = match os_action {
                        Some(crate::OsAction::Cut) => objc2::sel!(cut:),
                        Some(crate::OsAction::Copy) => objc2::sel!(copy:),
                        Some(crate::OsAction::Paste) => objc2::sel!(paste:),
                        Some(crate::OsAction::SelectAll) => objc2::sel!(selectAll:),
                        // "undo:" and "redo:" are always disabled in our case, as
                        // we don't have a NSTextView/NSTextField to enable them on.
                        Some(crate::OsAction::Undo) => objc2::sel!(handleGPUIMenuItem:),
                        Some(crate::OsAction::Redo) => objc2::sel!(handleGPUIMenuItem:),
                        None => objc2::sel!(handleGPUIMenuItem:),
                    };

                    let title = Objc2NSString::from_str(name);
                    let empty = Objc2NSString::from_str("");
                    let item;
                    if let Some(keystrokes) = keystrokes {
                        if keystrokes.len() == 1 {
                            let keystroke = &keystrokes[0];
                            let mut mask = Objc2NSEventModifierFlags::empty();
                            for (modifier, flag) in &[
                                (
                                    keystroke.modifiers().platform,
                                    Objc2NSEventModifierFlags::Command,
                                ),
                                (
                                    keystroke.modifiers().control,
                                    Objc2NSEventModifierFlags::Control,
                                ),
                                (keystroke.modifiers().alt, Objc2NSEventModifierFlags::Option),
                                (
                                    keystroke.modifiers().shift,
                                    Objc2NSEventModifierFlags::Shift,
                                ),
                            ] {
                                if *modifier {
                                    mask |= *flag;
                                }
                            }

                            let key_equivalent =
                                Objc2NSString::from_str(key_to_native(keystroke.key()).as_ref());
                            item = Objc2NSMenuItem::initWithTitle_action_keyEquivalent(
                                Objc2NSMenuItem::alloc(main_thread_marker()),
                                &title,
                                Some(selector),
                                &key_equivalent,
                            );
                            if Self::os_version() >= SemanticVersion::new(12, 0, 0) {
                                item.setAllowsAutomaticKeyEquivalentLocalization(false);
                            }
                            item.setKeyEquivalentModifierMask(mask);
                        } else {
                            item = Objc2NSMenuItem::initWithTitle_action_keyEquivalent(
                                Objc2NSMenuItem::alloc(main_thread_marker()),
                                &title,
                                Some(selector),
                                &empty,
                            );
                        }
                    } else {
                        item = Objc2NSMenuItem::initWithTitle_action_keyEquivalent(
                            Objc2NSMenuItem::alloc(main_thread_marker()),
                            &title,
                            Some(selector),
                            &empty,
                        );
                    }

                    let tag: isize = actions
                        .len()
                        .try_into()
                        .expect("menu action count should fit in isize");
                    item.setTag(tag);
                    actions.push(action.boxed_clone());
                    item
                }
                MenuItem::Submenu(Menu { name, icon, items }) => {
                    let item = Objc2NSMenuItem::new(main_thread_marker());
                    let submenu = Objc2NSMenu::new(main_thread_marker());
                    Self::set_menu_delegate(&submenu, delegate);
                    for item in items {
                        let submenu_item = Self::create_menu_item(item, delegate, actions, keymap);
                        submenu.addItem(&submenu_item);
                    }
                    item.setSubmenu(Some(&submenu));
                    let title = Objc2NSString::from_str(name);
                    item.setTitle(&title);

                    if let Some(icon_bytes) = icon {
                        if let Some(image) = Self::create_menu_icon(icon_bytes) {
                            item.setImage(Some(&image));
                        }
                    }

                    item
                }
                MenuItem::SystemMenu(OsMenu { name, menu_type }) => {
                    let item = Objc2NSMenuItem::new(main_thread_marker());
                    let submenu = Objc2NSMenu::new(main_thread_marker());
                    Self::set_menu_delegate(&submenu, delegate);
                    item.setSubmenu(Some(&submenu));
                    let title = Objc2NSString::from_str(name);
                    item.setTitle(&title);

                    match menu_type {
                        SystemMenuType::Services => {
                            shared_application().setServicesMenu(Some(&submenu));
                        }
                    }

                    item
                }
            }
        }
    }

    fn os_version() -> SemanticVersion {
        let process_info = Objc2NSProcessInfo::processInfo();
        let version = process_info.operatingSystemVersion();
        SemanticVersion::new(
            version.majorVersion as usize,
            version.minorVersion as usize,
            version.patchVersion as usize,
        )
    }
}

impl Platform for MacPlatform {
    fn background_executor(&self) -> BackgroundExecutor {
        self.0.lock().background_executor.clone()
    }

    fn foreground_executor(&self) -> crate::ForegroundExecutor {
        self.0.lock().foreground_executor.clone()
    }

    fn text_system(&self) -> Arc<dyn PlatformTextSystem> {
        self.0.lock().text_system.clone()
    }

    fn run(&self, on_finish_launching: Box<dyn FnOnce()>) {
        let mut state = self.0.lock();
        if state.headless {
            drop(state);
            on_finish_launching();
            unsafe { CFRunLoopRun() };
        } else {
            state.finish_launching = Some(on_finish_launching);
            drop(state);
        }

        let app = shared_application();
        let platform_ptr = self as *const Self;
        app.set_platform(platform_ptr);
        let app_delegate = GPUIApplicationDelegate::new(self.1, platform_ptr);
        app.setDelegate(Some(ProtocolObject::from_ref(&*app_delegate)));

        unsafe {
            let pool = Objc2NSAutoreleasePool::new();
            app.run();
            pool.drain();
        }

        // `NSApplication.delegate`, menu delegates, and notification observers
        // are weak. Unhook them before dropping `app_delegate`.
        unhook_application_delegate(&app, &app_delegate, &self.0.lock());
        app.set_platform(ptr::null());
        app_delegate.set_platform(ptr::null());
    }

    fn quit(&self) {
        // Quitting the app causes us to close windows, which invokes `Window::on_close` callbacks
        // synchronously before this method terminates. If we call `Platform::quit` while holding a
        // borrow of the app state (which most of the time we will do), we will end up
        // double-borrowing the app state in the `on_close` callbacks for our open windows. To solve
        // this, we make quitting the application asynchronous so that we aren't holding borrows to
        // the app state on the stack when we actually terminate the app.

        use super::dispatcher::{dispatch_get_main_queue, dispatch_sys::dispatch_async_f};

        unsafe {
            dispatch_async_f(dispatch_get_main_queue(), ptr::null_mut(), Some(quit));
        }

        unsafe extern "C" fn quit(_: *mut c_void) {
            shared_application().terminate(None);
        }
    }

    fn restart(&self, _binary_path: Option<PathBuf>) {
        use std::os::unix::process::CommandExt as _;

        let app_pid = std::process::id().to_string();
        let app_path = self
            .app_path()
            .ok()
            // When the app is not bundled, `app_path` returns the
            // directory containing the executable. Disregard this
            // and get the path to the executable itself.
            .and_then(|path| (path.extension()?.to_str()? == "app").then_some(path))
            .unwrap_or_else(|| std::env::current_exe().unwrap());

        // Wait until this process has exited and then re-open this path.
        let script = r#"
            while kill -0 $0 2> /dev/null; do
                sleep 0.1
            done
            open "$1"
        "#;

        #[allow(
            clippy::disallowed_methods,
            reason = "We are restarting ourselves, using std command thus is fine"
        )]
        let restart_process = Command::new("/bin/bash")
            .arg("-c")
            .arg(script)
            .arg(app_pid)
            .arg(app_path)
            .process_group(0)
            .spawn();

        match restart_process {
            Ok(_) => self.quit(),
            Err(e) => log::error!("failed to spawn restart script: {:?}", e),
        }
    }

    fn activate(&self, ignoring_other_apps: bool) {
        #[allow(deprecated)]
        shared_application().activateIgnoringOtherApps(ignoring_other_apps);
    }

    fn hide(&self) {
        shared_application().hide(None);
    }

    fn hide_other_apps(&self) {
        shared_application().hideOtherApplications(None);
    }

    fn unhide_other_apps(&self) {
        shared_application().unhideAllApplications(None);
    }

    fn primary_display(&self) -> Option<Rc<dyn PlatformDisplay>> {
        Some(Rc::new(MacDisplay::primary()))
    }

    fn displays(&self) -> Vec<Rc<dyn PlatformDisplay>> {
        MacDisplay::all()
            .map(|screen| Rc::new(screen) as Rc<_>)
            .collect()
    }

    #[cfg(feature = "screen-capture")]
    fn is_screen_capture_supported(&self) -> bool {
        let min_version = super::NSOperatingSystemVersion::new(12, 3, 0);
        super::is_macos_version_at_least(min_version)
    }

    #[cfg(feature = "screen-capture")]
    fn screen_capture_sources(
        &self,
    ) -> oneshot::Receiver<Result<Vec<Rc<dyn crate::ScreenCaptureSource>>>> {
        super::screen_capture::get_sources(self.1)
    }

    fn active_window(&self) -> Option<AnyWindowHandle> {
        MacWindow::active_window()
    }

    // Returns the windows ordered front-to-back, meaning that the active
    // window is the first one in the returned vec.
    fn window_stack(&self) -> Option<Vec<AnyWindowHandle>> {
        Some(MacWindow::ordered_windows())
    }

    fn open_window(
        &self,
        handle: AnyWindowHandle,
        options: WindowParams,
    ) -> Result<Box<dyn PlatformWindow>> {
        let (renderer_context, atlas_initial_size) = {
            let state = self.0.lock();
            (state.renderer_context.clone(), state.atlas_initial_size)
        };
        Ok(Box::new(MacWindow::open(
            handle,
            options,
            self.foreground_executor(),
            renderer_context,
            atlas_initial_size,
            self.1,
        )))
    }

    fn trim_renderer_caches(&self) {
        let context = self.0.lock().renderer_context.clone();
        context.lock().trim();
    }

    fn configure_gpu_resources(&self, gpu: &GpuResourceBudget) {
        let mut state = self.0.lock();
        state.atlas_initial_size = gpu.atlas_size();
        let context = state.renderer_context.clone();
        context
            .lock()
            .configure_initial_buffer_size(gpu.instance_buffer_initial_size);
    }

    fn renderer_cache_stats(&self) -> RendererCacheStats {
        let context = self.0.lock().renderer_context.clone();
        let (idle_gpu_buffers, gpu_buffer_size_bytes) = context.lock().stats();
        RendererCacheStats::new(idle_gpu_buffers, gpu_buffer_size_bytes)
    }

    fn window_appearance(&self) -> WindowAppearance {
        let appearance = shared_application().effectiveAppearance();
        unsafe { super::window_appearance::from_native(Retained::as_ptr(&appearance) as ObjcId) }
    }

    fn set_window_appearance(&self, appearance: Option<WindowAppearance>) {
        // `None` clears the override by setting a nil appearance, so the app
        // falls back to tracking the system-wide light/dark setting.
        let ns_appearance = appearance.and_then(super::window_appearance::to_native);
        shared_application().setAppearance(ns_appearance.as_deref());
    }

    fn open_url(&self, url: &str) {
        let url_string = Objc2NSString::from_str(url);
        let Some(url) = Objc2NSURL::initWithString(Objc2NSURL::alloc(), &url_string) else {
            return;
        };
        Objc2NSWorkspace::sharedWorkspace().openURL(&url);
    }

    fn register_url_scheme(&self, scheme: &str) -> Task<anyhow::Result<()>> {
        use objc2_app_kit::NSWorkspace;
        use objc2_foundation::{NSError, NSString};

        // API only available post Monterey
        // https://developer.apple.com/documentation/appkit/nsworkspace/3753004-setdefaultapplicationaturl
        let (done_tx, done_rx) = oneshot::channel();
        if Self::os_version() < SemanticVersion::new(12, 0, 0) {
            return Task::ready(Err(anyhow!(
                "macOS 12.0 or later is required to register URL schemes"
            )));
        }

        let Some(bundle_id) = Objc2NSBundle::mainBundle().bundleIdentifier() else {
            return Task::ready(Err(anyhow!("Can only register URL scheme in bundled apps")));
        };

        let workspace = NSWorkspace::sharedWorkspace();
        let Some(app) = workspace.URLForApplicationWithBundleIdentifier(&bundle_id) else {
            return Task::ready(Err(anyhow!(
                "Cannot register URL scheme until app is installed"
            )));
        };

        let scheme = NSString::from_str(scheme);

        let done_tx = Cell::new(Some(done_tx));
        let handler = RcBlock::new(move |error: *mut NSError| {
            let result = if let Some(error) = unsafe { error.as_ref() } {
                Err(anyhow!(
                    "Failed to register: {}",
                    error.localizedDescription()
                ))
            } else {
                Ok(())
            };

            if let Some(done_tx) = done_tx.take() {
                _ = done_tx.send(result);
            }
        });

        workspace.setDefaultApplicationAtURL_toOpenURLsWithScheme_completionHandler(
            &app,
            &scheme,
            Some(&handler),
        );

        self.background_executor().spawn(async { done_rx.await? })
    }

    fn on_open_urls(&self, callback: Box<dyn FnMut(Vec<String>)>) {
        self.0.lock().open_urls = Some(callback);
    }

    fn prompt_for_paths(
        &self,
        options: PathPromptOptions,
    ) -> oneshot::Receiver<Result<Option<Vec<PathBuf>>>> {
        use objc2_foundation::NSString;

        let marker = self.1;
        let (done_tx, done_rx) = oneshot::channel();
        self.foreground_executor()
            .spawn(async move {
                let panel = Objc2NSOpenPanel::openPanel(marker);
                panel.setCanChooseDirectories(options.directories);
                panel.setCanChooseFiles(options.files);
                panel.setAllowsMultipleSelection(options.multiple);

                panel.setCanCreateDirectories(true);
                panel.setResolvesAliases(false);

                let done_tx = Cell::new(Some(done_tx));
                let handler = RcBlock::new({
                    let panel = panel.clone();
                    move |response: Objc2NSModalResponse| {
                        let Some(done_tx) = done_tx.take() else {
                            return;
                        };

                        let result = (response == Objc2NSModalResponseOK).then(|| {
                            panel
                                .URLs()
                                .iter()
                                .filter(|url| url.isFileURL())
                                .filter_map(|url| url.to_file_path())
                                .collect::<Vec<_>>()
                        });
                        _ = done_tx.send(Ok(result));
                    }
                });

                if let Some(prompt) = options.prompt {
                    panel.setPrompt(Some(&NSString::from_str(prompt.as_str())));
                }

                panel.beginWithCompletionHandler(&handler);
            })
            .detach();
        done_rx
    }

    fn prompt_for_new_path(
        &self,
        directory: &Path,
        suggested_name: Option<&str>,
    ) -> oneshot::Receiver<Result<Option<PathBuf>>> {
        use objc2_foundation::{NSString, NSURL};

        let url = NSURL::from_directory_path(directory);
        let suggested_name = suggested_name.map(NSString::from_str);
        let (done_tx, done_rx) = oneshot::channel();
        let marker = self.1;
        self.foreground_executor()
            .spawn(async move {
                let panel = Objc2NSSavePanel::savePanel(marker);
                panel.setDirectoryURL(url.as_deref());

                if let Some(suggested_name) = suggested_name {
                    panel.setNameFieldStringValue(&suggested_name);
                }

                let done_tx = Cell::new(Some(done_tx));
                let handler = RcBlock::new({
                    let panel = panel.clone();
                    move |response: Objc2NSModalResponse| {
                        let Some(done_tx) = done_tx.take() else {
                            return;
                        };

                        let result = if response == Objc2NSModalResponseOK {
                            panel
                                .URL()
                                .filter(|url| url.isFileURL())
                                .and_then(|url| url.to_file_path())
                                .map(|mut path| {
                                    let Some(filename) = path.file_name() else {
                                        return path;
                                    };
                                    let chunks = filename
                                        .as_bytes()
                                        .split(|&b| b == b'.')
                                        .collect::<Vec<_>>();

                                    // https://github.com/zed-industries/zed/issues/16969
                                    // Workaround a bug in macOS Sequoia that adds an extra file-extension
                                    // sometimes. e.g. `a.sql` becomes `a.sql.s` or `a.txtx` becomes `a.txtx.txt`
                                    //
                                    // This is conditional on OS version because I'd like to get rid of it, so that
                                    // you can manually create a file called `a.sql.s`. That said it seems better
                                    // to break that use-case than breaking `a.sql`.
                                    if let &[_, second, third] = chunks.as_slice()
                                        && second.starts_with(third)
                                        && Self::os_version() >= SemanticVersion::new(15, 0, 0)
                                    {
                                        path.set_extension("");
                                    }

                                    path
                                })
                        } else {
                            None
                        };

                        _ = done_tx.send(Ok(result));
                    }
                });

                panel.beginWithCompletionHandler(&handler);
            })
            .detach();

        done_rx
    }

    fn can_select_mixed_files_and_dirs(&self) -> bool {
        true
    }

    fn reveal_path(&self, path: &Path) {
        let path = path.to_path_buf();
        self.0
            .lock()
            .background_executor
            .spawn(async move {
                let full_path = Objc2NSString::from_str(path.to_str().unwrap_or(""));
                let root_full_path = Objc2NSString::from_str("");
                Objc2NSWorkspace::sharedWorkspace()
                    .selectFile_inFileViewerRootedAtPath(Some(&full_path), &root_full_path);
            })
            .detach();
    }

    fn open_with_system(&self, path: &Path) {
        let path = path.to_owned();
        self.0
            .lock()
            .background_executor
            .spawn(async move {
                if let Some(mut child) = smol::process::Command::new("open")
                    .arg(path)
                    .spawn()
                    .context("invoking open command")
                    .log_err()
                {
                    child.status().await.log_err();
                }
            })
            .detach();
    }

    fn on_quit(&self, callback: Box<dyn FnMut() -> bool>) {
        self.0.lock().quit = Some(callback);
    }

    fn on_reopen(&self, callback: Box<dyn FnMut()>) {
        self.0.lock().reopen = Some(callback);
    }

    fn on_keyboard_layout_change(&self, callback: Box<dyn FnMut()>) {
        self.0.lock().on_keyboard_layout_change = Some(callback);
    }

    fn on_thermal_state_change(&self, callback: Box<dyn FnMut()>) {
        self.0.lock().on_thermal_state_change = Some(callback);
    }

    fn thermal_state(&self) -> ThermalState {
        match Objc2NSProcessInfo::processInfo().thermalState() {
            Objc2NSProcessInfoThermalState::Nominal => ThermalState::Nominal,
            Objc2NSProcessInfoThermalState::Fair => ThermalState::Fair,
            Objc2NSProcessInfoThermalState::Serious => ThermalState::Serious,
            Objc2NSProcessInfoThermalState::Critical => ThermalState::Critical,
            _ => ThermalState::Nominal,
        }
    }

    fn on_app_menu_action(&self, callback: Box<dyn FnMut(&dyn Action)>) {
        self.0.lock().menu_command = Some(callback);
    }

    fn on_will_open_app_menu(&self, callback: Box<dyn FnMut()>) {
        self.0.lock().will_open_menu = Some(callback);
    }

    fn on_validate_app_menu_command(&self, callback: Box<dyn FnMut(&dyn Action) -> bool>) {
        self.0.lock().validate_menu_command = Some(callback);
    }

    fn keyboard_layout(&self) -> Box<dyn PlatformKeyboardLayout> {
        Box::new(MacKeyboardLayout::new())
    }

    fn keyboard_mapper(&self) -> Rc<dyn PlatformKeyboardMapper> {
        self.0.lock().keyboard_mapper.clone()
    }

    fn app_path(&self) -> Result<PathBuf> {
        let bundle = Objc2NSBundle::mainBundle();
        Ok(PathBuf::from(bundle.bundlePath().to_string()))
    }

    fn set_menus(&self, menus: Vec<Menu>, keymap: &Keymap) {
        let app = shared_application();
        let delegate = gpui_delegate(&app).expect("GPUI application delegate");
        let mut state = self.0.lock();
        let actions = &mut state.menu_actions;
        let menu = unsafe { self.create_menu_bar(&menus, &delegate, actions, keymap) };
        drop(state);
        app.setMainMenu(Some(&menu));
        self.0.lock().menus = Some(menus.into_iter().map(|menu| menu.owned()).collect());
    }

    fn get_menus(&self) -> Option<Vec<OwnedMenu>> {
        self.0.lock().menus.clone()
    }

    fn set_dock_menu(&self, menu: Vec<MenuItem>, keymap: &Keymap) {
        let app = shared_application();
        let delegate = gpui_delegate(&app).expect("GPUI application delegate");
        let mut state = self.0.lock();
        let actions = &mut state.menu_actions;
        let new =
            Retained::into_raw(unsafe { self.create_dock_menu(menu, &delegate, actions, keymap) })
                as ObjcId;
        if let Some(old) = state.dock_menu.replace(new) {
            unsafe { CFRelease(old as _) }
        }
    }

    fn add_recent_document(&self, path: &Path) {
        if let Some(path_str) = path.to_str() {
            let path = Objc2NSString::from_str(path_str);
            let url = Objc2NSURL::fileURLWithPath(&path);
            NSDocumentController::sharedDocumentController(main_thread_marker())
                .noteNewRecentDocumentURL(&url);
        }
    }

    fn path_for_auxiliary_executable(&self, name: &str) -> Result<PathBuf> {
        unsafe {
            let bundle = Objc2NSBundle::mainBundle();
            let name = Objc2NSString::from_str(name);
            let Some(url) = bundle.URLForAuxiliaryExecutable(&name) else {
                anyhow::bail!("resource not found");
            };
            ns_url_to_path(&url)
        }
    }

    fn set_cursor_style(&self, _style: CursorStyle) {
        // Cursor style is now managed per-window via PlatformWindow::set_cursor_style.
    }

    fn should_auto_hide_scrollbars(&self) -> bool {
        NSScroller::preferredScrollerStyle(main_thread_marker()) == NSScrollerStyle::Overlay
    }

    fn write_to_clipboard(&self, item: ClipboardItem) {
        use crate::ClipboardEntry;

        unsafe {
            // We only want to use NSAttributedString if there are multiple entries to write.
            if item.entries().len() <= 1 {
                match item.entries().first() {
                    Some(entry) => match entry {
                        ClipboardEntry::String(string) => {
                            self.write_plaintext_to_clipboard(string);
                        }
                        ClipboardEntry::Image(image) => {
                            self.write_image_to_clipboard(image);
                        }
                    },
                    None => {
                        // Writing an empty list of entries just clears the clipboard.
                        let state = self.0.lock();
                        state.pasteboard.clearContents();
                    }
                }
            } else {
                let any_images = false;
                let attributed_string = {
                    let buf = NSMutableAttributedString::initWithString(
                        NSMutableAttributedString::alloc(),
                        &Objc2NSString::from_str(""),
                    );

                    for entry in item.into_entries() {
                        if let ClipboardEntry::String(string) = entry {
                            let text = string.into_text();
                            let text = Objc2NSString::from_str(&text);
                            let to_append = Objc2NSAttributedString::initWithString(
                                Objc2NSAttributedString::alloc(),
                                &text,
                            );
                            buf.appendAttributedString(&to_append);
                        }
                    }

                    buf
                };

                let state = self.0.lock();
                state.pasteboard.clearContents();

                // Only set rich text clipboard types if we actually have 1+ images to include.
                if any_images {
                    let range = Objc2NSRange::from(0..attributed_string.length());
                    let attrs: Retained<NSDictionary<Objc2NSString, AnyObject>> =
                        NSDictionary::new();
                    if let Some(rtfd_data) = unsafe {
                        attributed_string
                            .as_super()
                            .RTFDFromRange_documentAttributes(range, &attrs)
                    } {
                        state
                            .pasteboard
                            .setData_forType(Some(&rtfd_data), Objc2NSPasteboardTypeRTFD);
                    }

                    if let Some(rtf_data) = unsafe {
                        attributed_string
                            .as_super()
                            .RTFFromRange_documentAttributes(range, &attrs)
                    } {
                        state
                            .pasteboard
                            .setData_forType(Some(&rtf_data), Objc2NSPasteboardTypeRTF);
                    }
                }

                let plain_text = attributed_string.string();
                state
                    .pasteboard
                    .setString_forType(&plain_text, Objc2NSPasteboardTypeString);
            }
        }
    }

    fn read_from_clipboard(&self) -> Option<ClipboardItem> {
        let state = self.0.lock();
        let pasteboard = &state.pasteboard;

        // First, see if it's a string.
        unsafe {
            if let Some(types) = pasteboard.types()
                && types.containsObject(Objc2NSPasteboardTypeString)
            {
                let data = pasteboard.dataForType(Objc2NSPasteboardTypeString)?;
                let bytes = data.to_vec();
                return Some(self.read_string_from_clipboard(&state, &bytes));
            }

            // If it wasn't a string, try the various supported image types.
            for format in ImageFormat::iter() {
                if let Some(item) = try_clipboard_image(&pasteboard, format) {
                    return Some(item);
                }
            }
        }

        // If it wasn't a string or a supported image type, give up.
        None
    }

    fn write_credentials(&self, url: &str, username: &str, password: &[u8]) -> Task<Result<()>> {
        let url = url.to_string();
        let username = username.to_string();
        let password = password.to_vec();
        self.background_executor().spawn(async move {
            unsafe {
                use security::*;

                let url = CFString::from(url.as_str());
                let username = CFString::from(username.as_str());
                let password = CFData::from_buffer(&password);

                // First, check if there are already credentials for the given server. If so, then
                // update the username and password.
                let mut verb = "updating";
                let mut query_attrs = CFMutableDictionary::with_capacity(2);
                query_attrs.set(kSecClass as *const _, kSecClassInternetPassword as *const _);
                query_attrs.set(kSecAttrServer as *const _, url.as_CFTypeRef());

                let mut attrs = CFMutableDictionary::with_capacity(4);
                attrs.set(kSecClass as *const _, kSecClassInternetPassword as *const _);
                attrs.set(kSecAttrServer as *const _, url.as_CFTypeRef());
                attrs.set(kSecAttrAccount as *const _, username.as_CFTypeRef());
                attrs.set(kSecValueData as *const _, password.as_CFTypeRef());

                let mut status = SecItemUpdate(
                    query_attrs.as_concrete_TypeRef(),
                    attrs.as_concrete_TypeRef(),
                );

                // If there were no existing credentials for the given server, then create them.
                if status == errSecItemNotFound {
                    verb = "creating";
                    status = SecItemAdd(attrs.as_concrete_TypeRef(), ptr::null_mut());
                }
                anyhow::ensure!(status == errSecSuccess, "{verb} password failed: {status}");
            }
            Ok(())
        })
    }

    fn read_credentials(&self, url: &str) -> Task<Result<Option<(String, Vec<u8>)>>> {
        let url = url.to_string();
        self.background_executor().spawn(async move {
            let url = CFString::from(url.as_str());
            let cf_true = CFBoolean::true_value().as_CFTypeRef();

            unsafe {
                use security::*;

                // Find any credentials for the given server URL.
                let mut attrs = CFMutableDictionary::with_capacity(5);
                attrs.set(kSecClass as *const _, kSecClassInternetPassword as *const _);
                attrs.set(kSecAttrServer as *const _, url.as_CFTypeRef());
                attrs.set(kSecReturnAttributes as *const _, cf_true);
                attrs.set(kSecReturnData as *const _, cf_true);

                let mut result = CFTypeRef::from(ptr::null());
                let status = SecItemCopyMatching(attrs.as_concrete_TypeRef(), &mut result);
                match status {
                    security::errSecSuccess => {}
                    security::errSecItemNotFound | security::errSecUserCanceled => return Ok(None),
                    _ => anyhow::bail!("reading password failed: {status}"),
                }

                let result = CFType::wrap_under_create_rule(result)
                    .downcast::<CFDictionary>()
                    .context("keychain item was not a dictionary")?;
                let username = result
                    .find(kSecAttrAccount as *const _)
                    .context("account was missing from keychain item")?;
                let username = CFType::wrap_under_get_rule(*username)
                    .downcast::<CFString>()
                    .context("account was not a string")?;
                let password = result
                    .find(kSecValueData as *const _)
                    .context("password was missing from keychain item")?;
                let password = CFType::wrap_under_get_rule(*password)
                    .downcast::<CFData>()
                    .context("password was not a string")?;

                Ok(Some((username.to_string(), password.bytes().to_vec())))
            }
        })
    }

    fn set_quit_mode(&self, mode: QuitMode) {
        // `QuitMode::Explicit` is the daemon-style mode: keep the app alive
        // without any windows and drop the Dock icon via the Accessory
        // activation policy. Other modes keep the regular foreground policy.
        // Lifecycle itself is owned by core; this hook only carries the
        // platform-visible side effect.
        let policy = match mode {
            QuitMode::Explicit => Objc2NSApplicationActivationPolicy::Accessory,
            QuitMode::LastWindowClosed | QuitMode::Default => {
                Objc2NSApplicationActivationPolicy::Regular
            }
        };
        let _ = shared_application().setActivationPolicy(policy);
    }

    fn set_tray_icon(&self, icon: Option<&[u8]>) {
        let mut state = self.0.lock();
        let rendering_mode = state.tray_icon_rendering_mode;
        Self::ensure_tray(&mut state).set_icon(icon, rendering_mode);
    }

    fn set_tray_icon_rendering_mode(&self, rendering_mode: TrayIconRenderingMode) {
        let mut state = self.0.lock();
        state.tray_icon_rendering_mode = rendering_mode;
        if let Some(tray) = &state.tray {
            tray.set_icon_rendering_mode(rendering_mode);
        }
    }

    fn set_tray_menu(&self, menu: Vec<TrayMenuItem>) {
        let mut state = self.0.lock();
        Self::ensure_tray(&mut state).set_menu(menu);
    }

    fn set_tray_tooltip(&self, tooltip: &str) {
        let mut state = self.0.lock();
        Self::ensure_tray(&mut state).set_tooltip(tooltip);
    }

    fn set_tray_panel_mode(&self, enabled: bool) {
        let mut state = self.0.lock();
        Self::ensure_tray(&mut state).set_panel_mode(enabled);
    }

    fn get_tray_icon_anchor(&self) -> Option<TrayAnchor> {
        let state = self.0.lock();
        state.tray.as_ref().and_then(|tray| tray.get_icon_anchor())
    }

    fn get_tray_icon_bounds(&self) -> Option<crate::Bounds<crate::Pixels>> {
        let state = self.0.lock();
        state.tray.as_ref().and_then(|tray| tray.get_icon_bounds())
    }

    fn on_tray_icon_event(&self, callback: Box<dyn FnMut(TrayIconEvent)>) {
        self.0.lock().tray_icon_callback = Some(callback);
    }

    fn on_tray_icon_click_event(&self, callback: Box<dyn FnMut(TrayIconClickEvent)>) {
        self.0.lock().tray_icon_click_callback = Some(callback);
    }

    fn on_tray_menu_action(&self, callback: Box<dyn FnMut(SharedString)>) {
        self.0.lock().tray_menu_callback = Some(callback);
    }

    fn register_global_hotkey(&self, id: u32, keystroke: &crate::Keystroke) -> Result<()> {
        let mut state = self.0.lock();

        if state.global_hotkey_handler.is_none() {
            unsafe {
                install_global_hotkey_handler(&self.0, &mut state)?;
            }
        }

        replace_global_hotkey_registration(&mut state, id, keystroke)
    }

    fn unregister_global_hotkey(&self, id: u32) {
        let mut state = self.0.lock();
        if let Some(registration) = state.global_hotkey_registrations.remove(&id) {
            unsafe { unregister_hotkey_ref(registration.hotkey_ref) };
        }
    }

    fn on_global_hotkey(&self, callback: Box<dyn FnMut(u32)>) {
        self.0.lock().global_hotkey_callback = Some(callback);
    }

    fn focused_window_info(&self) -> Option<crate::FocusedWindowInfo> {
        super::active_window::get_focused_window_info()
    }

    fn accessibility_status(&self) -> crate::PermissionStatus {
        super::permissions::accessibility_status()
    }

    fn request_accessibility_permission(&self) -> crate::PermissionRequestStatus {
        super::permissions::request_accessibility_permission();
        crate::PermissionRequestStatus::Requested
    }

    fn set_auto_launch(&self, app_id: &str, enabled: bool) -> Result<()> {
        super::auto_launch::set_auto_launch(app_id, enabled)
    }

    fn is_auto_launch_enabled(&self, app_id: &str) -> bool {
        super::auto_launch::is_auto_launch_enabled(app_id)
    }

    fn show_notification(&self, title: &str, body: &str) -> Result<()> {
        if Objc2NSBundle::mainBundle().bundleIdentifier().is_none() {
            return Err(anyhow!(
                "Notifications require an app bundle (bundleIdentifier is nil)"
            ));
        }

        let content = UNMutableNotificationContent::new();
        content.setTitle(&Objc2NSString::from_str(title));
        content.setBody(&Objc2NSString::from_str(body));

        let uuid_str = uuid::Uuid::new_v4().to_string();
        let request = UNNotificationRequest::requestWithIdentifier_content_trigger(
            &Objc2NSString::from_str(&uuid_str),
            content.as_super(),
            None,
        );
        UNUserNotificationCenter::currentNotificationCenter()
            .addNotificationRequest_withCompletionHandler(&request, None);
        Ok(())
    }

    fn delete_credentials(&self, url: &str) -> Task<Result<()>> {
        let url = url.to_string();

        self.background_executor().spawn(async move {
            unsafe {
                use security::*;

                let url = CFString::from(url.as_str());
                let mut query_attrs = CFMutableDictionary::with_capacity(2);
                query_attrs.set(kSecClass as *const _, kSecClassInternetPassword as *const _);
                query_attrs.set(kSecAttrServer as *const _, url.as_CFTypeRef());

                let status = SecItemDelete(query_attrs.as_concrete_TypeRef());
                anyhow::ensure!(status == errSecSuccess, "delete password failed: {status}");
            }
            Ok(())
        })
    }

    fn on_system_power_event(&self, callback: Box<dyn FnMut(crate::SystemPowerEvent)>) {
        self.0.lock().system_power_callback = Some(callback);
    }

    fn start_power_save_blocker(&self, kind: crate::PowerSaveBlockerKind) -> Option<u32> {
        super::power::start_power_save_blocker(kind)
    }

    fn stop_power_save_blocker(&self, id: u32) {
        super::power::stop_power_save_blocker(id);
    }

    fn system_idle_time(&self) -> Option<std::time::Duration> {
        super::power::system_idle_time()
    }

    fn network_status(&self) -> crate::NetworkStatus {
        super::network::network_status()
    }

    fn on_network_status_change(&self, callback: Box<dyn FnMut(crate::NetworkStatus)>) {
        let mut state = self.0.lock();

        if let Some(old_monitor) = state.network_monitor.take() {
            unsafe { super::network::cancel_path_monitor(old_monitor) };
        }

        state.network_change_callback = Some(callback);

        let platform_ptr = &self.0 as *const Mutex<MacPlatformState> as *const c_void;

        unsafe {
            let monitor = super::network::create_path_monitor();
            if monitor.is_null() {
                return;
            }

            let block = RcBlock::new(move |path: *const c_void| {
                let status = super::network::path_status_to_network_status(path);

                struct NetworkChangeCtx {
                    platform: *const c_void,
                    status: crate::NetworkStatus,
                }

                let ctx = Box::into_raw(Box::new(NetworkChangeCtx {
                    platform: platform_ptr,
                    status,
                }));

                use super::dispatcher::{dispatch_get_main_queue, dispatch_sys::dispatch_async_f};

                unsafe extern "C" fn invoke(ctx_ptr: *mut c_void) {
                    let ctx = unsafe { Box::from_raw(ctx_ptr as *mut NetworkChangeCtx) };
                    let platform_state =
                        unsafe { &*(ctx.platform as *const Mutex<MacPlatformState>) };
                    let mut lock = platform_state.lock();
                    if let Some(mut callback) = lock.network_change_callback.take() {
                        drop(lock);
                        callback(ctx.status);
                        platform_state
                            .lock()
                            .network_change_callback
                            .get_or_insert(callback);
                    }
                }

                dispatch_async_f(dispatch_get_main_queue(), ctx as *mut c_void, Some(invoke));
            });

            let queue = super::dispatcher::dispatch_get_main_queue();
            super::network::start_path_monitor(
                monitor,
                RcBlock::as_ptr(&block).cast(),
                queue as *const c_void,
            );
            // `nw_path_monitor` keeps the handler pointer; retain the heap block.
            std::mem::forget(block);

            state.network_monitor = Some(monitor);
        }
    }

    fn on_media_key_event(&self, callback: Box<dyn FnMut(crate::MediaKeyEvent)>) {
        let mut state = self.0.lock();
        state.media_key_callback = Some(callback);

        if state.media_key_monitor.is_some() {
            return;
        }

        let platform_ptr = &self.0 as *const Mutex<MacPlatformState> as *const c_void;

        let handler = RcBlock::new(move |event: ptr::NonNull<Objc2NSEvent>| {
            let event = unsafe { event.as_ref() };
            if event.subtype().0 != NX_SUBTYPE_AUX_CONTROL_BUTTONS {
                return;
            }

            let data1 = event.data1();
            let key_code = (data1 >> 16) & 0xFF;
            let flags = (data1 >> 8) & 0xFF;
            let is_down = (flags & 0x1) == 0;

            if !is_down {
                return;
            }

            let media_event = match key_code {
                16 => crate::MediaKeyEvent::PlayPause,
                17 => crate::MediaKeyEvent::NextTrack,
                18 => crate::MediaKeyEvent::PreviousTrack,
                19 => crate::MediaKeyEvent::Stop,
                20 => crate::MediaKeyEvent::Play,
                _ => return,
            };

            let platform_state = unsafe { &*(platform_ptr as *const Mutex<MacPlatformState>) };
            let mut lock = platform_state.lock();
            if let Some(mut callback) = lock.media_key_callback.take() {
                drop(lock);
                callback(media_event);
                platform_state
                    .lock()
                    .media_key_callback
                    .get_or_insert(callback);
            }
        });
        let monitor = Objc2NSEvent::addGlobalMonitorForEventsMatchingMask_handler(
            Objc2NSEventMask::SystemDefined,
            &handler,
        );
        std::mem::forget(handler);

        state.media_key_monitor = monitor.map(|monitor| Retained::into_raw(monitor) as id);
    }

    fn request_user_attention(&self, attention_type: crate::AttentionType) {
        let id = super::dock::request_user_attention(attention_type);
        self.0.lock().attention_request_id = id;
    }

    fn cancel_user_attention(&self) {
        let id = self.0.lock().attention_request_id;
        super::dock::cancel_user_attention(id);
    }

    fn set_dock_badge(&self, label: Option<&str>) {
        super::dock::set_dock_badge(label);
    }

    fn show_context_menu(
        &self,
        position: crate::Point<crate::Pixels>,
        items: Vec<crate::TrayMenuItem>,
        callback: Box<dyn FnMut(crate::SharedString)>,
    ) {
        self.0.lock().context_menu_callback = Some(callback);

        let menu = Objc2NSMenu::new(main_thread_marker());
        menu.setAutoenablesItems(false);
        unsafe {
            super::tray::build_menu_with_selector(
                &menu,
                &items,
                objc2::sel!(handleContextMenuItem:),
            );
            if let Some(point) = global_point_to_native_screen_point(position) {
                menu.popUpMenuPositioningItem_atLocation_inView(None, point, None);
            }
        }
    }

    fn show_dialog(
        &self,
        options: crate::DialogOptions,
    ) -> futures::channel::oneshot::Receiver<usize> {
        super::dialog::show_dialog(options)
    }

    fn os_info(&self) -> crate::OsInfo {
        super::os_info::get_os_info()
    }

    fn biometric_status(&self) -> crate::BiometricStatus {
        super::biometric::biometric_status()
    }

    fn authenticate_biometric(&self, reason: &str, callback: Box<dyn FnOnce(bool) + Send>) {
        super::biometric::authenticate_biometric(reason, callback);
    }
}

impl MacPlatform {
    unsafe fn read_string_from_clipboard(
        &self,
        state: &MacPlatformState,
        text_bytes: &[u8],
    ) -> ClipboardItem {
        unsafe {
            let text = String::from_utf8_lossy(text_bytes).to_string();
            let metadata = self
                .read_from_pasteboard(&state.pasteboard, &state.text_hash_pasteboard_type)
                .and_then(|hash_bytes| {
                    let hash_bytes = hash_bytes.try_into().ok()?;
                    let hash = u64::from_be_bytes(hash_bytes);
                    let metadata = self
                        .read_from_pasteboard(&state.pasteboard, &state.metadata_pasteboard_type)?;

                    if hash == ClipboardString::text_hash(&text) {
                        String::from_utf8(metadata).ok()
                    } else {
                        None
                    }
                });

            if let Some(metadata) = metadata {
                ClipboardItem::new_string_with_metadata(text, metadata)
            } else {
                ClipboardItem::new_string(text)
            }
        }
    }

    unsafe fn write_plaintext_to_clipboard(&self, string: &ClipboardString) {
        unsafe {
            let state = self.0.lock();
            state.pasteboard.clearContents();

            let text_bytes = Objc2NSData::with_bytes(string.text().as_bytes());
            state
                .pasteboard
                .setData_forType(Some(&text_bytes), Objc2NSPasteboardTypeString);

            if let Some(metadata) = string.metadata() {
                let hash_bytes = ClipboardString::text_hash(string.text()).to_be_bytes();
                let hash_bytes = Objc2NSData::with_bytes(&hash_bytes);
                state
                    .pasteboard
                    .setData_forType(Some(&hash_bytes), &state.text_hash_pasteboard_type);

                let metadata_bytes = Objc2NSData::with_bytes(metadata.as_bytes());
                state
                    .pasteboard
                    .setData_forType(Some(&metadata_bytes), &state.metadata_pasteboard_type);
            }
        }
    }

    #[allow(unused_unsafe)]
    unsafe fn write_image_to_clipboard(&self, image: &Image) {
        let state = self.0.lock();
        unsafe {
            state.pasteboard.clearContents();
        }

        let bytes = Objc2NSData::with_bytes(&image.bytes);

        unsafe {
            state
                .pasteboard
                .setData_forType(Some(&bytes), Into::<UTType>::into(image.format).inner());
        }
    }
}

#[allow(unused_unsafe)]
fn try_clipboard_image(
    pasteboard: &Objc2NSPasteboard,
    format: ImageFormat,
) -> Option<ClipboardItem> {
    let ut_type: UTType = format.into();

    unsafe {
        let types = pasteboard.types()?;
        if types.containsObject(ut_type.inner()) {
            let data = pasteboard.dataForType(ut_type.inner())?;
            let bytes = data.to_vec();
            Some(ClipboardItem::from(Image::new(format, bytes)))
        } else {
            None
        }
    }
}

unsafe fn install_global_hotkey_handler(
    platform_ptr: *const Mutex<MacPlatformState>,
    state: &mut MacPlatformState,
) -> Result<()> {
    let event_types = [EventTypeSpec {
        event_class: K_EVENT_CLASS_KEYBOARD,
        event_kind: K_EVENT_HOTKEY_PRESSED,
    }];
    let mut handler_ref = ptr::null_mut();
    let status = unsafe {
        InstallEventHandler(
            GetApplicationEventTarget(),
            hotkey_event_handler,
            event_types.len() as u32,
            event_types.as_ptr(),
            platform_ptr as *mut c_void,
            &mut handler_ref,
        )
    };
    if status != NO_ERR {
        return Err(anyhow!(
            "failed to install macOS global hotkey handler: OSStatus {}",
            status
        ));
    }

    state.global_hotkey_handler = Some(handler_ref);
    Ok(())
}

fn replace_global_hotkey_registration(
    state: &mut MacPlatformState,
    id: u32,
    keystroke: &crate::Keystroke,
) -> Result<()> {
    let previous_registration = state.global_hotkey_registrations.remove(&id);
    if let Some(previous_registration) = previous_registration.as_ref() {
        unsafe { unregister_hotkey_ref(previous_registration.hotkey_ref) };
    }

    match register_hotkey_ref(state, id, keystroke) {
        Ok(hotkey_ref) => {
            state.global_hotkey_registrations.insert(
                id,
                RegisteredGlobalHotkey {
                    keystroke: keystroke.clone(),
                    hotkey_ref,
                },
            );
            Ok(())
        }
        Err(err) => {
            if let Some(mut previous_registration) = previous_registration {
                match register_hotkey_ref(state, id, &previous_registration.keystroke) {
                    Ok(hotkey_ref) => {
                        previous_registration.hotkey_ref = hotkey_ref;
                        state
                            .global_hotkey_registrations
                            .insert(id, previous_registration);
                    }
                    Err(restore_err) => {
                        log::error!(
                            "failed to restore macOS global hotkey {} ({}) after registration failure: {}",
                            id,
                            previous_registration.keystroke,
                            restore_err
                        );
                    }
                }
            }

            Err(err)
        }
    }
}

fn reregister_global_hotkeys_for_current_layout(state: &mut MacPlatformState) {
    let registrations = std::mem::take(&mut state.global_hotkey_registrations);

    for registration in registrations.values() {
        unsafe { unregister_hotkey_ref(registration.hotkey_ref) };
    }

    for (id, mut registration) in registrations {
        match register_hotkey_ref(state, id, &registration.keystroke) {
            Ok(hotkey_ref) => {
                registration.hotkey_ref = hotkey_ref;
                state.global_hotkey_registrations.insert(id, registration);
            }
            Err(err) => {
                log::error!(
                    "failed to re-register macOS global hotkey {} ({}) after keyboard layout change: {}",
                    id,
                    registration.keystroke,
                    err
                );
            }
        }
    }
}

fn register_hotkey_ref(
    state: &MacPlatformState,
    id: u32,
    keystroke: &crate::Keystroke,
) -> Result<EventHotKeyRef> {
    let native_hotkey = state.global_hotkey_mapper.hotkey_to_native(keystroke)?;
    let hotkey_id = EventHotKeyID {
        signature: GPUI_HOTKEY_SIGNATURE,
        id,
    };
    let mut hotkey_ref = ptr::null_mut();
    let status = unsafe {
        RegisterEventHotKey(
            native_hotkey.key_code,
            native_hotkey.modifiers,
            hotkey_id,
            GetApplicationEventTarget(),
            K_EVENT_HOTKEY_EXCLUSIVE,
            &mut hotkey_ref,
        )
    };
    if status != NO_ERR {
        return Err(global_hotkey_registration_error(status));
    }

    Ok(hotkey_ref)
}

unsafe fn unregister_hotkey_ref(hotkey_ref: EventHotKeyRef) {
    unsafe {
        let _ = UnregisterEventHotKey(hotkey_ref);
    }
}

unsafe extern "C" fn hotkey_event_handler(
    _: EventHandlerCallRef,
    event: EventRef,
    user_data: *mut c_void,
) -> OSStatus {
    if user_data.is_null() {
        return EVENT_NOT_HANDLED_ERR;
    }

    let mut hotkey_id = EventHotKeyID {
        signature: 0,
        id: 0,
    };
    let status = unsafe {
        GetEventParameter(
            event,
            K_EVENT_PARAM_DIRECT_OBJECT,
            TYPE_EVENT_HOTKEY_ID,
            ptr::null_mut(),
            std::mem::size_of::<EventHotKeyID>(),
            ptr::null_mut(),
            &mut hotkey_id as *mut EventHotKeyID as *mut c_void,
        )
    };
    if status != NO_ERR {
        return status;
    }

    if hotkey_id.signature != GPUI_HOTKEY_SIGNATURE {
        return EVENT_NOT_HANDLED_ERR;
    }

    let platform_state = unsafe { &*(user_data as *const Mutex<MacPlatformState>) };
    let mut lock = platform_state.lock();
    if let Some(mut callback) = lock.global_hotkey_callback.take() {
        drop(lock);
        callback(hotkey_id.id);
        platform_state
            .lock()
            .global_hotkey_callback
            .get_or_insert(callback);
    }

    NO_ERR
}

fn global_hotkey_registration_error(status: OSStatus) -> anyhow::Error {
    if status == EVENT_HOTKEY_EXISTS_ERR {
        anyhow!("global hotkey is already in use")
    } else {
        anyhow!("RegisterEventHotKey failed with OSStatus {}", status)
    }
}

unsafe fn ns_url_to_path(url: &Objc2NSURL) -> Result<PathBuf> {
    anyhow::ensure!(
        url.isFileURL(),
        "url is not a file path: {}",
        url.absoluteString()
            .map(|s| s.to_string())
            .unwrap_or_default()
    );
    Ok(PathBuf::from(OsStr::from_bytes(unsafe {
        CStr::from_ptr(url.fileSystemRepresentation().as_ptr()).to_bytes()
    })))
}

type EventTargetRef = *mut c_void;
type EventHandlerRef = *mut c_void;
type EventHandlerCallRef = *mut c_void;
type EventHotKeyRef = *mut c_void;
type EventRef = *mut c_void;
type EventHandlerUPP = unsafe extern "C" fn(EventHandlerCallRef, EventRef, *mut c_void) -> OSStatus;

#[repr(C)]
#[derive(Clone, Copy)]
struct EventTypeSpec {
    event_class: u32,
    event_kind: u32,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct EventHotKeyID {
    signature: u32,
    id: u32,
}

const NO_ERR: OSStatus = 0;
const EVENT_NOT_HANDLED_ERR: OSStatus = -9874;
const EVENT_HOTKEY_EXISTS_ERR: OSStatus = -9878;
const K_EVENT_HOTKEY_PRESSED: u32 = 5;
const K_EVENT_HOTKEY_EXCLUSIVE: u32 = 1;
const K_EVENT_CLASS_KEYBOARD: u32 = four_char_code(*b"keyb");
const K_EVENT_PARAM_DIRECT_OBJECT: u32 = four_char_code(*b"----");
const TYPE_EVENT_HOTKEY_ID: u32 = four_char_code(*b"hkid");
const GPUI_HOTKEY_SIGNATURE: u32 = four_char_code(*b"GPUI");

const fn four_char_code(bytes: [u8; 4]) -> u32 {
    ((bytes[0] as u32) << 24)
        | ((bytes[1] as u32) << 16)
        | ((bytes[2] as u32) << 8)
        | (bytes[3] as u32)
}

#[link(name = "Carbon", kind = "framework")]
unsafe extern "C" {
    pub(super) fn TISCopyCurrentKeyboardLayoutInputSource() -> *mut AnyObject;
    pub(super) fn TISGetInputSourceProperty(
        inputSource: *mut AnyObject,
        propertyKey: *const c_void,
    ) -> *mut AnyObject;
    fn GetApplicationEventTarget() -> EventTargetRef;
    fn InstallEventHandler(
        target: EventTargetRef,
        handler: EventHandlerUPP,
        num_types: u32,
        event_list: *const EventTypeSpec,
        user_data: *mut c_void,
        handler_ref: *mut EventHandlerRef,
    ) -> OSStatus;
    fn GetEventParameter(
        event: EventRef,
        name: u32,
        desired_type: u32,
        actual_type: *mut u32,
        buffer_size: usize,
        actual_size: *mut usize,
        data: *mut c_void,
    ) -> OSStatus;
    fn RegisterEventHotKey(
        hot_key_code: u32,
        hot_key_modifiers: u32,
        hot_key_id: EventHotKeyID,
        target: EventTargetRef,
        options: u32,
        out_ref: *mut EventHotKeyRef,
    ) -> OSStatus;
    fn UnregisterEventHotKey(hot_key: EventHotKeyRef) -> OSStatus;

    pub(super) fn UCKeyTranslate(
        keyLayoutPtr: *const ::std::os::raw::c_void,
        virtualKeyCode: u16,
        keyAction: u16,
        modifierKeyState: u32,
        keyboardType: u32,
        keyTranslateOptions: u32,
        deadKeyState: *mut u32,
        maxStringLength: usize,
        actualStringLength: *mut usize,
        unicodeString: *mut u16,
    ) -> u32;
    pub(super) fn LMGetKbdType() -> u16;
    pub(super) static kTISPropertyUnicodeKeyLayoutData: CFStringRef;
    pub(super) static kTISPropertyInputSourceID: CFStringRef;
    pub(super) static kTISPropertyLocalizedName: CFStringRef;
}

mod security {
    #![allow(non_upper_case_globals)]
    use super::*;

    #[link(name = "Security", kind = "framework")]
    unsafe extern "C" {
        pub static kSecClass: CFStringRef;
        pub static kSecClassInternetPassword: CFStringRef;
        pub static kSecAttrServer: CFStringRef;
        pub static kSecAttrAccount: CFStringRef;
        pub static kSecValueData: CFStringRef;
        pub static kSecReturnAttributes: CFStringRef;
        pub static kSecReturnData: CFStringRef;

        pub fn SecItemAdd(attributes: CFDictionaryRef, result: *mut CFTypeRef) -> OSStatus;
        pub fn SecItemUpdate(query: CFDictionaryRef, attributes: CFDictionaryRef) -> OSStatus;
        pub fn SecItemDelete(query: CFDictionaryRef) -> OSStatus;
        pub fn SecItemCopyMatching(query: CFDictionaryRef, result: *mut CFTypeRef) -> OSStatus;
    }

    pub const errSecSuccess: OSStatus = 0;
    pub const errSecUserCanceled: OSStatus = -128;
    pub const errSecItemNotFound: OSStatus = -25300;
}

impl From<ImageFormat> for UTType {
    fn from(value: ImageFormat) -> Self {
        match value {
            ImageFormat::Png => Self::png(),
            ImageFormat::Jpeg => Self::jpeg(),
            ImageFormat::Tiff => Self::tiff(),
            ImageFormat::Webp => Self::webp(),
            ImageFormat::Gif => Self::gif(),
            ImageFormat::Bmp => Self::bmp(),
            ImageFormat::Svg => Self::svg(),
        }
    }
}

// See https://developer.apple.com/documentation/uniformtypeidentifiers/uttype-swift.struct/
enum UTType {
    Static(&'static NSPasteboardType),
    Owned(Retained<Objc2NSString>),
}

impl UTType {
    pub fn png() -> Self {
        // https://developer.apple.com/documentation/uniformtypeidentifiers/uttype-swift.struct/png
        Self::Static(unsafe { Objc2NSPasteboardTypePNG }) // This is a rare case where there's a built-in NSPasteboardType
    }

    pub fn jpeg() -> Self {
        // https://developer.apple.com/documentation/uniformtypeidentifiers/uttype-swift.struct/jpeg
        Self::Owned(Objc2NSString::from_str("public.jpeg"))
    }

    pub fn gif() -> Self {
        // https://developer.apple.com/documentation/uniformtypeidentifiers/uttype-swift.struct/gif
        Self::Owned(Objc2NSString::from_str("com.compuserve.gif"))
    }

    pub fn webp() -> Self {
        // https://developer.apple.com/documentation/uniformtypeidentifiers/uttype-swift.struct/webp
        Self::Owned(Objc2NSString::from_str("org.webmproject.webp"))
    }

    pub fn bmp() -> Self {
        // https://developer.apple.com/documentation/uniformtypeidentifiers/uttype-swift.struct/bmp
        Self::Owned(Objc2NSString::from_str("com.microsoft.bmp"))
    }

    pub fn svg() -> Self {
        // https://developer.apple.com/documentation/uniformtypeidentifiers/uttype-swift.struct/svg
        Self::Owned(Objc2NSString::from_str("public.svg-image"))
    }

    pub fn tiff() -> Self {
        // https://developer.apple.com/documentation/uniformtypeidentifiers/uttype-swift.struct/tiff
        Self::Static(unsafe { Objc2NSPasteboardTypeTIFF }) // This is a rare case where there's a built-in NSPasteboardType
    }

    fn inner(&self) -> &NSPasteboardType {
        match self {
            Self::Static(kind) => kind,
            Self::Owned(kind) => kind,
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::ClipboardItem;

    use super::*;

    #[test]
    fn mac_platform_new_requires_main_thread() {
        if MainThreadMarker::new().is_some() {
            return;
        }
        let panicked = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            MacPlatform::new(false);
        }));
        assert!(
            panicked.is_err(),
            "MacPlatform::new must panic off the AppKit main thread"
        );
    }

    #[test]
    fn test_clipboard() {
        let platform = build_platform();
        assert_eq!(platform.read_from_clipboard(), None);

        let item = ClipboardItem::new_string("1".to_string());
        platform.write_to_clipboard(item.clone());
        assert_eq!(platform.read_from_clipboard(), Some(item));

        let item = ClipboardItem::from(crate::ClipboardEntry::String(
            ClipboardString::new("2".to_string()).with_json_metadata(vec![3, 4]),
        ));
        platform.write_to_clipboard(item.clone());
        assert_eq!(platform.read_from_clipboard(), Some(item));

        let text_from_other_app = "text from other app";
        unsafe {
            let bytes = Objc2NSData::with_bytes(text_from_other_app.as_bytes());
            platform
                .0
                .lock()
                .pasteboard
                .setData_forType(Some(&bytes), Objc2NSPasteboardTypeString);
        }
        assert_eq!(
            platform.read_from_clipboard(),
            Some(ClipboardItem::new_string(text_from_other_app.to_string()))
        );
    }

    fn build_platform() -> MacPlatform {
        // libtest runs this on a worker thread, not the AppKit main thread.
        // `MacPlatform::new` must keep panicking off-main so production AppKit
        // calls stay honest. `--test-threads=1` still uses a worker, and
        // `dispatch_sync` to main would deadlock the harness. Clipboard I/O uses
        // a unique NSPasteboard, matching Zed's later pasteboard tests that do
        // not construct `MacPlatform` at all.
        let marker = MainThreadMarker::new().unwrap_or_else(|| {
            // SAFETY: this marker is stored on `MacPlatform` but unused by
            // clipboard I/O. Do not call AppKit UI APIs from this test path.
            unsafe { MainThreadMarker::new_unchecked() }
        });
        let platform = MacPlatform::new_with_marker(false, marker);
        platform.0.lock().pasteboard = Objc2NSPasteboard::pasteboardWithUniqueName();
        platform
    }
}

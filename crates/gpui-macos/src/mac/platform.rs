use super::tray::MacTray;
use super::{
    MacKeyboardLayout, MacKeyboardMapper, NSRange as PlatformNSRange,
    attributed_string::{NSAttributedString, NSMutableAttributedString},
    events::key_to_native,
    global_hotkey::NativeHotkeyMapper,
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
use block::ConcreteBlock;
use block2::RcBlock;
use core_foundation::{
    base::{CFRelease, CFType, CFTypeRef, OSStatus, TCFType},
    boolean::CFBoolean,
    data::CFData,
    dictionary::{CFDictionary, CFDictionaryRef, CFMutableDictionary},
    runloop::CFRunLoopRun,
    string::{CFString, CFStringRef},
};
use ctor::ctor;
use futures::channel::oneshot;
use itertools::Itertools;
use objc::{
    class,
    declare::ClassDecl,
    msg_send,
    runtime::{BOOL, Class, NO, Object, Sel},
    sel, sel_impl,
};
use objc2::{AnyThread, MainThreadMarker, MainThreadOnly, rc::Retained};
use objc2_app_kit::{
    NSApplication as Objc2NSApplication,
    NSApplicationActivationPolicy as Objc2NSApplicationActivationPolicy,
    NSEventModifierFlags as Objc2NSEventModifierFlags, NSImage as Objc2NSImage,
    NSMenu as Objc2NSMenu, NSMenuItem as Objc2NSMenuItem, NSModalResponse as Objc2NSModalResponse,
    NSModalResponseOK as Objc2NSModalResponseOK, NSOpenPanel as Objc2NSOpenPanel,
    NSPasteboard as Objc2NSPasteboard, NSPasteboardType,
    NSPasteboardTypePNG as Objc2NSPasteboardTypePNG,
    NSPasteboardTypeRTF as Objc2NSPasteboardTypeRTF,
    NSPasteboardTypeRTFD as Objc2NSPasteboardTypeRTFD,
    NSPasteboardTypeString as Objc2NSPasteboardTypeString,
    NSPasteboardTypeTIFF as Objc2NSPasteboardTypeTIFF, NSSavePanel as Objc2NSSavePanel,
};
use objc2_foundation::{
    NSAutoreleasePool as Objc2NSAutoreleasePool, NSBundle as Objc2NSBundle, NSData as Objc2NSData,
    NSProcessInfo as Objc2NSProcessInfo, NSSize as Objc2NSSize, NSString as Objc2NSString,
    NSURL as Objc2NSURL,
};
use parking_lot::Mutex;
use ptr::null_mut;
use std::{
    cell::Cell,
    convert::TryInto,
    ffi::{CStr, OsStr, c_void},
    os::{raw::c_char, unix::ffi::OsStrExt},
    path::{Path, PathBuf},
    process::Command,
    ptr,
    rc::Rc,
    slice, str,
    sync::{Arc, OnceLock},
};
use strum::IntoEnumIterator;
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

#[allow(non_upper_case_globals)]
const NSUTF8StringEncoding: NSUInteger = 4;

const MAC_PLATFORM_IVAR: &str = "platform";
static mut APP_CLASS: *const Class = ptr::null();
static mut APP_DELEGATE_CLASS: *const Class = ptr::null();

fn main_thread_marker() -> MainThreadMarker {
    unsafe { MainThreadMarker::new_unchecked() }
}

unsafe fn shared_application() -> ObjcId {
    unsafe { msg_send![APP_CLASS, sharedApplication] }
}

unsafe fn application_delegate(object: ObjcId) -> ObjcId {
    unsafe { msg_send![object, delegate] }
}

#[ctor]
unsafe fn build_classes() {
    unsafe {
        APP_CLASS = {
            let mut decl = ClassDecl::new("GPUIApplication", class!(NSApplication)).unwrap();
            decl.add_ivar::<*mut c_void>(MAC_PLATFORM_IVAR);
            decl.register()
        }
    };
    unsafe {
        APP_DELEGATE_CLASS = unsafe {
            let mut decl = ClassDecl::new("GPUIApplicationDelegate", class!(NSResponder)).unwrap();
            decl.add_ivar::<*mut c_void>(MAC_PLATFORM_IVAR);
            decl.add_method(
                sel!(applicationWillFinishLaunching:),
                will_finish_launching as extern "C" fn(&mut Object, Sel, id),
            );
            decl.add_method(
                sel!(applicationDidFinishLaunching:),
                did_finish_launching as extern "C" fn(&mut Object, Sel, id),
            );
            decl.add_method(
                sel!(applicationShouldHandleReopen:hasVisibleWindows:),
                should_handle_reopen as extern "C" fn(&mut Object, Sel, id, bool),
            );
            decl.add_method(
                sel!(applicationWillTerminate:),
                will_terminate as extern "C" fn(&mut Object, Sel, id),
            );
            decl.add_method(
                sel!(handleGPUIMenuItem:),
                handle_menu_item as extern "C" fn(&mut Object, Sel, id),
            );
            decl.add_method(
                sel!(handleTrayMenuItem:),
                handle_tray_menu_item as extern "C" fn(&mut Object, Sel, id),
            );
            decl.add_method(
                sel!(handleTrayPanelClick:),
                handle_tray_panel_click as extern "C" fn(&mut Object, Sel, id),
            );
            // Add menu item handlers so that OS save panels have the correct key commands
            decl.add_method(
                sel!(cut:),
                handle_menu_item as extern "C" fn(&mut Object, Sel, id),
            );
            decl.add_method(
                sel!(copy:),
                handle_menu_item as extern "C" fn(&mut Object, Sel, id),
            );
            decl.add_method(
                sel!(paste:),
                handle_menu_item as extern "C" fn(&mut Object, Sel, id),
            );
            decl.add_method(
                sel!(selectAll:),
                handle_menu_item as extern "C" fn(&mut Object, Sel, id),
            );
            decl.add_method(
                sel!(undo:),
                handle_menu_item as extern "C" fn(&mut Object, Sel, id),
            );
            decl.add_method(
                sel!(redo:),
                handle_menu_item as extern "C" fn(&mut Object, Sel, id),
            );
            decl.add_method(
                sel!(validateMenuItem:),
                validate_menu_item as extern "C" fn(&mut Object, Sel, id) -> bool,
            );
            decl.add_method(
                sel!(menuWillOpen:),
                menu_will_open as extern "C" fn(&mut Object, Sel, id),
            );
            decl.add_method(
                sel!(applicationDockMenu:),
                handle_dock_menu as extern "C" fn(&mut Object, Sel, id) -> id,
            );
            decl.add_method(
                sel!(application:openURLs:),
                open_urls as extern "C" fn(&mut Object, Sel, id, id),
            );

            decl.add_method(
                sel!(onKeyboardLayoutChange:),
                on_keyboard_layout_change as extern "C" fn(&mut Object, Sel, id),
            );
            decl.add_method(
                sel!(onThermalStateChange:),
                on_thermal_state_change as extern "C" fn(&mut Object, Sel, id),
            );

            decl.add_method(
                sel!(applicationShouldTerminateAfterLastWindowClosed:),
                should_terminate_after_last_window_closed
                    as extern "C" fn(&mut Object, Sel, id) -> BOOL,
            );

            decl.add_method(
                sel!(handleSystemPowerEvent:),
                handle_system_power_event as extern "C" fn(&mut Object, Sel, id),
            );

            decl.add_method(
                sel!(handleContextMenuItem:),
                handle_context_menu_item as extern "C" fn(&mut Object, Sel, id),
            );

            decl.register()
        }
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

    unsafe fn set_menu_delegate(menu: &Retained<Objc2NSMenu>, delegate: id) {
        unsafe {
            let _: () = msg_send![Retained::as_ptr(menu) as id, setDelegate: delegate];
        }
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
        delegate: id,
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
                    let app: id = msg_send![APP_CLASS, sharedApplication];
                    let _: () = msg_send![app, setWindowsMenu: Retained::as_ptr(&menu) as id];
                }
            }

            application_menu
        }
    }

    unsafe fn create_dock_menu(
        &self,
        menu_items: Vec<MenuItem>,
        delegate: id,
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
        delegate: id,
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
                            let app: id = msg_send![APP_CLASS, sharedApplication];
                            let _: () =
                                msg_send![app, setServicesMenu: Retained::as_ptr(&submenu) as id];
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

        unsafe {
            let app = shared_application();
            let app_delegate: id = msg_send![APP_DELEGATE_CLASS, new];
            let _: () = msg_send![app, setDelegate: app_delegate];

            let self_ptr = self as *const Self as *const c_void;
            (*app).set_ivar(MAC_PLATFORM_IVAR, self_ptr);
            (*app_delegate).set_ivar(MAC_PLATFORM_IVAR, self_ptr);

            let pool = Objc2NSAutoreleasePool::new();
            (&*app.cast::<Objc2NSApplication>()).run();
            pool.drain();

            (*app).set_ivar(MAC_PLATFORM_IVAR, null_mut::<c_void>());
            (*application_delegate(app)).set_ivar(MAC_PLATFORM_IVAR, null_mut::<c_void>());
        }
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
            unsafe {
                let app = shared_application();
                (&*app.cast::<Objc2NSApplication>()).terminate(None);
            }
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
        unsafe {
            let app = shared_application();
            let _: () = msg_send![app, activateIgnoringOtherApps: ignoring_other_apps];
        }
    }

    fn hide(&self) {
        unsafe {
            let app = shared_application();
            (&*app.cast::<Objc2NSApplication>()).hide(None);
        }
    }

    fn hide_other_apps(&self) {
        unsafe {
            let app = shared_application();
            (&*app.cast::<Objc2NSApplication>()).hideOtherApplications(None);
        }
    }

    fn unhide_other_apps(&self) {
        unsafe {
            let app = shared_application();
            (&*app.cast::<Objc2NSApplication>()).unhideAllApplications(None);
        }
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
        super::screen_capture::get_sources()
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
        unsafe {
            let app = shared_application();
            let appearance = (&*app.cast::<Objc2NSApplication>()).effectiveAppearance();
            super::window_appearance::from_native(Retained::as_ptr(&appearance) as ObjcId)
        }
    }

    fn set_window_appearance(&self, appearance: Option<WindowAppearance>) {
        unsafe {
            let app = shared_application();
            // `None` clears the override by setting a nil appearance, so the app
            // falls back to tracking the system-wide light/dark setting.
            let ns_appearance = appearance.and_then(super::window_appearance::to_native);
            (&*app.cast::<Objc2NSApplication>()).setAppearance(ns_appearance.as_deref());
        }
    }

    fn open_url(&self, url: &str) {
        unsafe {
            let url_string = Objc2NSString::from_str(url);
            let Some(url) = Objc2NSURL::initWithString(Objc2NSURL::alloc(), &url_string) else {
                return;
            };
            let workspace: id = msg_send![class!(NSWorkspace), sharedWorkspace];
            let _: () = msg_send![workspace, openURL: Retained::as_ptr(&url) as ObjcId];
        }
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
        unsafe {
            let path = path.to_path_buf();
            self.0
                .lock()
                .background_executor
                .spawn(async move {
                    let full_path = Objc2NSString::from_str(path.to_str().unwrap_or(""));
                    let root_full_path = Objc2NSString::from_str("");
                    let workspace: id = msg_send![class!(NSWorkspace), sharedWorkspace];
                    let _: BOOL = msg_send![
                        workspace,
                        selectFile: Retained::as_ptr(&full_path) as ObjcId
                        inFileViewerRootedAtPath: Retained::as_ptr(&root_full_path) as ObjcId
                    ];
                })
                .detach();
        }
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
        unsafe {
            let process_info: id = msg_send![class!(NSProcessInfo), processInfo];
            let state: NSInteger = msg_send![process_info, thermalState];
            match state {
                0 => ThermalState::Nominal,
                1 => ThermalState::Fair,
                2 => ThermalState::Serious,
                3 => ThermalState::Critical,
                _ => ThermalState::Nominal,
            }
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
        unsafe {
            let bundle = Objc2NSBundle::mainBundle();
            let bundle_path = bundle.bundlePath();
            Ok(path_from_objc(Retained::as_ptr(&bundle_path) as ObjcId))
        }
    }

    fn set_menus(&self, menus: Vec<Menu>, keymap: &Keymap) {
        unsafe {
            let app = shared_application();
            let delegate = application_delegate(app);
            let mut state = self.0.lock();
            let actions = &mut state.menu_actions;
            let menu = self.create_menu_bar(&menus, delegate, actions, keymap);
            drop(state);
            let _: () = msg_send![app, setMainMenu: Retained::as_ptr(&menu) as ObjcId];
        }
        self.0.lock().menus = Some(menus.into_iter().map(|menu| menu.owned()).collect());
    }

    fn get_menus(&self) -> Option<Vec<OwnedMenu>> {
        self.0.lock().menus.clone()
    }

    fn set_dock_menu(&self, menu: Vec<MenuItem>, keymap: &Keymap) {
        unsafe {
            let app = shared_application();
            let delegate = application_delegate(app);
            let mut state = self.0.lock();
            let actions = &mut state.menu_actions;
            let new = Retained::into_raw(self.create_dock_menu(menu, delegate, actions, keymap))
                as ObjcId;
            if let Some(old) = state.dock_menu.replace(new) {
                CFRelease(old as _)
            }
        }
    }

    fn add_recent_document(&self, path: &Path) {
        if let Some(path_str) = path.to_str() {
            unsafe {
                let document_controller: id =
                    msg_send![class!(NSDocumentController), sharedDocumentController];
                let path = Objc2NSString::from_str(path_str);
                let url = Objc2NSURL::fileURLWithPath(&path);
                let _: () = msg_send![
                    document_controller,
                    noteNewRecentDocumentURL: Retained::as_ptr(&url) as ObjcId
                ];
            }
        }
    }

    fn path_for_auxiliary_executable(&self, name: &str) -> Result<PathBuf> {
        unsafe {
            let bundle = Objc2NSBundle::mainBundle();
            let name = Objc2NSString::from_str(name);
            let Some(url) = bundle.URLForAuxiliaryExecutable(&name) else {
                anyhow::bail!("resource not found");
            };
            ns_url_to_path(Retained::as_ptr(&url) as ObjcId)
        }
    }

    fn set_cursor_style(&self, _style: CursorStyle) {
        // Cursor style is now managed per-window via PlatformWindow::set_cursor_style.
    }

    fn should_auto_hide_scrollbars(&self) -> bool {
        #[allow(non_upper_case_globals)]
        const NSScrollerStyleOverlay: NSInteger = 1;

        unsafe {
            let style: NSInteger = msg_send![class!(NSScroller), preferredScrollerStyle];
            style == NSScrollerStyleOverlay
        }
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
                let mut any_images = false;
                let attributed_string = {
                    let initial_text = Objc2NSString::from_str("");
                    let mut buf = NSMutableAttributedString::alloc(nil)
                        // TODO can we skip this? Or at least part of it?
                        .init_attributed_string(Retained::as_ptr(&initial_text) as ObjcId);

                    for entry in item.into_entries() {
                        if let ClipboardEntry::String(string) = entry {
                            let text = string.into_text();
                            let text = Objc2NSString::from_str(&text);
                            let to_append = NSAttributedString::alloc(nil)
                                .init_attributed_string(Retained::as_ptr(&text) as ObjcId);

                            buf.appendAttributedString_(to_append);
                        }
                    }

                    buf
                };

                let state = self.0.lock();
                state.pasteboard.clearContents();

                // Only set rich text clipboard types if we actually have 1+ images to include.
                if any_images {
                    let attributed_string_len: usize = msg_send![attributed_string, length];
                    let rtfd_data = attributed_string.RTFDFromRange_documentAttributes_(
                        PlatformNSRange::from(0..attributed_string_len),
                        nil,
                    );
                    if let Some(rtfd_data) = rtfd_data.cast::<Objc2NSData>().as_ref() {
                        state
                            .pasteboard
                            .setData_forType(Some(rtfd_data), Objc2NSPasteboardTypeRTFD);
                    }

                    let rtf_data = attributed_string.RTFFromRange_documentAttributes_(
                        PlatformNSRange::from(0..attributed_string_len),
                        nil,
                    );
                    if let Some(rtf_data) = rtf_data.cast::<Objc2NSData>().as_ref() {
                        state
                            .pasteboard
                            .setData_forType(Some(rtf_data), Objc2NSPasteboardTypeRTF);
                    }
                }

                let plain_text = attributed_string.string();
                let plain_text = plain_text
                    .cast::<Objc2NSString>()
                    .as_ref()
                    .expect("NSMutableAttributedString::string should return NSString");
                state
                    .pasteboard
                    .setString_forType(plain_text, Objc2NSPasteboardTypeString);
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
        unsafe {
            let app = shared_application();
            (&*app.cast::<Objc2NSApplication>()).setActivationPolicy(policy);
        }
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
        unsafe {
            let bundle: id = msg_send![class!(NSBundle), mainBundle];
            let bundle_id: id = msg_send![bundle, bundleIdentifier];
            if bundle_id == nil {
                return Err(anyhow!(
                    "Notifications require an app bundle (bundleIdentifier is nil)"
                ));
            }

            let center: id = msg_send![class!(UNUserNotificationCenter), currentNotificationCenter];
            if center == nil {
                return Err(anyhow!("UNUserNotificationCenter not available"));
            }
            let content: id = msg_send![class!(UNMutableNotificationContent), new];
            let ns_title = Objc2NSString::from_str(title);
            let _: () = msg_send![content, setTitle: Retained::as_ptr(&ns_title) as ObjcId];
            let ns_body = Objc2NSString::from_str(body);
            let _: () = msg_send![content, setBody: Retained::as_ptr(&ns_body) as ObjcId];

            let uuid_str = uuid::Uuid::new_v4().to_string();
            let ns_id = Objc2NSString::from_str(&uuid_str);
            let request: id = msg_send![
                class!(UNNotificationRequest),
                requestWithIdentifier: Retained::as_ptr(&ns_id) as ObjcId
                content: content
                trigger: nil
            ];
            let _: () =
                msg_send![center, addNotificationRequest: request withCompletionHandler: nil];
        }
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

            let block = ConcreteBlock::new(move |path: *const c_void| {
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
            let block = block.copy();

            let queue = super::dispatcher::dispatch_get_main_queue();
            super::network::start_path_monitor(
                monitor,
                &*block as *const _ as *const c_void,
                queue as *const c_void,
            );
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

        unsafe {
            let mask: u64 = 1 << 14; // NSSystemDefinedMask

            let block = ConcreteBlock::new(move |event: id| {
                let subtype: i16 = msg_send![event, subtype];
                if subtype != 8 {
                    return;
                }

                let data1: isize = msg_send![event, data1];
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

                let platform_state = &*(platform_ptr as *const Mutex<MacPlatformState>);
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
            let block = block.copy();
            let monitor: id = msg_send![
                class!(NSEvent),
                addGlobalMonitorForEventsMatchingMask: mask
                handler: &*block
            ];
            std::mem::forget(block);

            state.media_key_monitor = Some(monitor);
        }
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

        unsafe {
            let menu: id = msg_send![class!(NSMenu), new];
            let _: () = msg_send![menu, setAutoenablesItems: NO];
            super::tray::build_menu_with_selector(menu, &items, sel!(handleContextMenuItem:));

            if let Some(point) = global_point_to_native_screen_point(position) {
                let _: () =
                    msg_send![menu, popUpMenuPositioningItem: nil atLocation: point inView: nil];
            }
            let _: () = msg_send![menu, release];
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

unsafe fn path_from_objc(path: id) -> PathBuf {
    let len = msg_send![path, lengthOfBytesUsingEncoding: NSUTF8StringEncoding];
    let bytes: *const u8 = unsafe { msg_send![path, UTF8String] };
    let path = str::from_utf8(unsafe { slice::from_raw_parts(bytes, len) }).unwrap();
    PathBuf::from(path)
}

unsafe fn get_mac_platform(object: &mut Object) -> &MacPlatform {
    unsafe {
        let platform_ptr: *mut c_void = *object.get_ivar(MAC_PLATFORM_IVAR);
        assert!(!platform_ptr.is_null());
        &*(platform_ptr as *const MacPlatform)
    }
}

extern "C" fn will_finish_launching(_this: &mut Object, _: Sel, _: id) {
    unsafe {
        let user_defaults: id = msg_send![class!(NSUserDefaults), standardUserDefaults];

        // The autofill heuristic controller causes slowdown and high CPU usage.
        // We don't know exactly why. This disables the full heuristic controller.
        //
        // Adapted from: https://github.com/ghostty-org/ghostty/pull/8625
        let name = Objc2NSString::from_str("NSAutoFillHeuristicControllerEnabled");
        let existing_value: id =
            msg_send![user_defaults, objectForKey: Retained::as_ptr(&name) as ObjcId];
        if existing_value == nil {
            let false_value: id = msg_send![class!(NSNumber), numberWithBool:false];
            let _: () = msg_send![
                user_defaults,
                setObject: false_value
                forKey: Retained::as_ptr(&name) as ObjcId
            ];
        }
    }
}

extern "C" fn did_finish_launching(this: &mut Object, _: Sel, _: id) {
    unsafe {
        let notification_center: *mut Object =
            msg_send![class!(NSNotificationCenter), defaultCenter];
        let name =
            Objc2NSString::from_str("NSTextInputContextKeyboardSelectionDidChangeNotification");
        let _: () = msg_send![notification_center, addObserver: this as id
            selector: sel!(onKeyboardLayoutChange:)
            name: Retained::as_ptr(&name) as ObjcId
            object: nil
        ];
        let thermal_name =
            Objc2NSString::from_str("NSProcessInfoThermalStateDidChangeNotification");
        let process_info: id = msg_send![class!(NSProcessInfo), processInfo];
        let _: () = msg_send![notification_center, addObserver: this as id
            selector: sel!(onThermalStateChange:)
            name: Retained::as_ptr(&thermal_name) as ObjcId
            object: process_info
        ];

        let workspace: id = msg_send![class!(NSWorkspace), sharedWorkspace];
        let ws_notification_center: id = msg_send![workspace, notificationCenter];

        let power_notifications = [
            "NSWorkspaceWillSleepNotification",
            "NSWorkspaceDidWakeNotification",
            "NSWorkspaceSessionDidResignActiveNotification",
            "NSWorkspaceSessionDidBecomeActiveNotification",
            "NSWorkspaceWillPowerOffNotification",
        ];
        for name in &power_notifications {
            let ns_name = Objc2NSString::from_str(name);
            let _: () = msg_send![ws_notification_center, addObserver: this as id
                selector: sel!(handleSystemPowerEvent:)
                name: Retained::as_ptr(&ns_name) as ObjcId
                object: nil
            ];
        }

        let platform = get_mac_platform(this);
        let callback = platform.0.lock().finish_launching.take();
        if let Some(callback) = callback {
            callback();
        }
    }
}

extern "C" fn should_handle_reopen(this: &mut Object, _: Sel, _: id, has_open_windows: bool) {
    if !has_open_windows {
        let platform = unsafe { get_mac_platform(this) };
        let mut lock = platform.0.lock();
        if let Some(mut callback) = lock.reopen.take() {
            drop(lock);
            callback();
            platform.0.lock().reopen.get_or_insert(callback);
        }
    }
}

extern "C" fn will_terminate(this: &mut Object, _: Sel, _: id) {
    let platform = unsafe { get_mac_platform(this) };
    let mut lock = platform.0.lock();
    if let Some(mut callback) = lock.quit.take() {
        drop(lock);
        callback();
        platform.0.lock().quit.get_or_insert(callback);
    }
}

extern "C" fn on_keyboard_layout_change(this: &mut Object, _: Sel, _: id) {
    let platform = unsafe { get_mac_platform(this) };
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

extern "C" fn on_thermal_state_change(this: &mut Object, _: Sel, _: id) {
    let platform = unsafe { get_mac_platform(this) };
    let platform_ptr = platform as *const MacPlatform;

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

extern "C" fn should_terminate_after_last_window_closed(_this: &mut Object, _: Sel, _: id) -> BOOL {
    // Lifecycle decisions are owned by the core application via `QuitMode`;
    // the platform layer always defers to that and never auto-terminates on
    // window close. Core will call `Platform::quit` when the mode requires it.
    NO
}

extern "C" fn open_urls(this: &mut Object, _: Sel, _: id, urls: id) {
    let urls = unsafe {
        let count: usize = msg_send![urls, count];
        (0..count)
            .filter_map(|i| {
                let url: ObjcId = msg_send![urls, objectAtIndex: i];
                let absolute_string: ObjcId = msg_send![url, absoluteString];
                let utf8_string: *const c_char = msg_send![absolute_string, UTF8String];
                match CStr::from_ptr(utf8_string).to_str() {
                    Ok(string) => Some(string.to_string()),
                    Err(err) => {
                        log::error!("error converting path to string: {}", err);
                        None
                    }
                }
            })
            .collect::<Vec<_>>()
    };
    let platform = unsafe { get_mac_platform(this) };
    let mut lock = platform.0.lock();
    if let Some(mut callback) = lock.open_urls.take() {
        drop(lock);
        callback(urls);
        platform.0.lock().open_urls.get_or_insert(callback);
    }
}

extern "C" fn handle_menu_item(this: &mut Object, _: Sel, item: id) {
    unsafe {
        let platform = get_mac_platform(this);
        let mut lock = platform.0.lock();
        if let Some(mut callback) = lock.menu_command.take() {
            let tag: NSInteger = msg_send![item, tag];
            let index = tag as usize;
            if let Some(action) = lock.menu_actions.get(index) {
                let action = action.boxed_clone();
                drop(lock);
                callback(&*action);
            }
            platform.0.lock().menu_command.get_or_insert(callback);
        }
    }
}

extern "C" fn handle_tray_menu_item(this: &mut Object, _: Sel, item: id) {
    unsafe {
        let platform = get_mac_platform(this);
        let represented: id = msg_send![item, representedObject];
        if represented == nil {
            return;
        }
        let len: usize = msg_send![represented, lengthOfBytesUsingEncoding: NSUTF8StringEncoding];
        let bytes: *const u8 = msg_send![represented, UTF8String];
        let id_str = std::str::from_utf8(slice::from_raw_parts(bytes, len)).unwrap_or("");
        let shared_id: SharedString = id_str.to_string().into();

        let platform_ptr = platform as *const MacPlatform;

        use super::dispatcher::{dispatch_get_main_queue, dispatch_sys::dispatch_async_f};

        struct TrayActionCtx {
            platform: *const MacPlatform,
            id: SharedString,
        }

        let ctx = Box::into_raw(Box::new(TrayActionCtx {
            platform: platform_ptr,
            id: shared_id,
        }));

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

        dispatch_async_f(dispatch_get_main_queue(), ctx as *mut c_void, Some(invoke));
    }
}

extern "C" fn handle_tray_panel_click(this: &mut Object, _: Sel, _sender: id) {
    unsafe {
        let platform = get_mac_platform(this);
        let platform_ptr = platform as *const MacPlatform;

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

        dispatch_async_f(
            dispatch_get_main_queue(),
            platform_ptr as *mut c_void,
            Some(invoke),
        );
    }
}

extern "C" fn handle_system_power_event(this: &mut Object, _: Sel, notification: id) {
    unsafe {
        let name: id = msg_send![notification, name];
        let name_str: *const c_char = msg_send![name, UTF8String];
        let name_cstr = CStr::from_ptr(name_str);
        let name_bytes = name_cstr.to_bytes();

        let event = match name_bytes {
            b"NSWorkspaceWillSleepNotification" => crate::SystemPowerEvent::Suspend,
            b"NSWorkspaceDidWakeNotification" => crate::SystemPowerEvent::Resume,
            b"NSWorkspaceSessionDidResignActiveNotification" => crate::SystemPowerEvent::LockScreen,
            b"NSWorkspaceSessionDidBecomeActiveNotification" => {
                crate::SystemPowerEvent::UnlockScreen
            }
            b"NSWorkspaceWillPowerOffNotification" => crate::SystemPowerEvent::Shutdown,
            _ => return,
        };

        let platform = get_mac_platform(this);
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

extern "C" fn handle_context_menu_item(this: &mut Object, _: Sel, item: id) {
    unsafe {
        let platform = get_mac_platform(this);
        let represented: id = msg_send![item, representedObject];
        if represented == nil {
            return;
        }
        let len: usize = msg_send![represented, lengthOfBytesUsingEncoding: NSUTF8StringEncoding];
        let bytes: *const u8 = msg_send![represented, UTF8String];
        let id_str = std::str::from_utf8(slice::from_raw_parts(bytes, len)).unwrap_or("");
        let shared_id: SharedString = id_str.to_string().into();

        let mut lock = platform.0.lock();
        if let Some(mut callback) = lock.context_menu_callback.take() {
            drop(lock);
            callback(shared_id);
            platform
                .0
                .lock()
                .context_menu_callback
                .get_or_insert(callback);
        }
    }
}

extern "C" fn validate_menu_item(this: &mut Object, _: Sel, item: id) -> bool {
    unsafe {
        let mut result = false;
        let platform = get_mac_platform(this);
        let mut lock = platform.0.lock();
        if let Some(mut callback) = lock.validate_menu_command.take() {
            let tag: NSInteger = msg_send![item, tag];
            let index = tag as usize;
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

extern "C" fn menu_will_open(this: &mut Object, _: Sel, _: id) {
    unsafe {
        let platform = get_mac_platform(this);
        let mut lock = platform.0.lock();
        if let Some(mut callback) = lock.will_open_menu.take() {
            drop(lock);
            callback();
            platform.0.lock().will_open_menu.get_or_insert(callback);
        }
    }
}

extern "C" fn handle_dock_menu(this: &mut Object, _: Sel, _: id) -> id {
    unsafe {
        let platform = get_mac_platform(this);
        let mut state = platform.0.lock();
        if let Some(id) = state.dock_menu {
            id
        } else {
            nil
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

unsafe fn ns_url_to_path(url: id) -> Result<PathBuf> {
    let path: *mut c_char = msg_send![url, fileSystemRepresentation];
    anyhow::ensure!(!path.is_null(), "url is not a file path: {}", unsafe {
        let absolute_string: ObjcId = msg_send![url, absoluteString];
        let utf8_string: *const c_char = msg_send![absolute_string, UTF8String];
        CStr::from_ptr(utf8_string).to_string_lossy()
    });
    Ok(PathBuf::from(OsStr::from_bytes(unsafe {
        CStr::from_ptr(path).to_bytes()
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
    pub(super) fn TISCopyCurrentKeyboardLayoutInputSource() -> *mut Object;
    pub(super) fn TISGetInputSourceProperty(
        inputSource: *mut Object,
        propertyKey: *const c_void,
    ) -> *mut Object;
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
        let platform = MacPlatform::new(false);
        platform.0.lock().pasteboard = Objc2NSPasteboard::pasteboardWithUniqueName();
        platform
    }
}

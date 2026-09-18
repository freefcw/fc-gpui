use super::screen_frame_to_tray_anchor;
use crate::TrayMenuItem;
use crate::{Bounds, Pixels, TrayAnchor, TrayIconRenderingMode};
use objc2::runtime::AnyObject;
use objc2::{AnyThread, MainThreadMarker, MainThreadOnly, rc::Retained};
use objc2_app_kit::{
    NSApplication, NSApplicationDelegate, NSControlStateValueOff, NSControlStateValueOn, NSImage,
    NSMenu, NSMenuItem, NSStatusBar, NSStatusItem,
};
use objc2_foundation::{NSData, NSSize, NSString};
use std::cell::{Cell, RefCell};

pub(crate) struct MacTray {
    status_item: Retained<NSStatusItem>,
    panel_mode: Cell<bool>,
    stored_menu: RefCell<Option<Retained<NSMenu>>>,
}

impl MacTray {
    #[allow(unused_unsafe)]
    pub fn new() -> Self {
        unsafe {
            let status_bar = NSStatusBar::systemStatusBar();
            let length: f64 = -1.0;
            let status_item = status_bar.statusItemWithLength(length);
            status_item.setVisible(true);

            if let Some(button) = status_item.button(main_thread_marker()) {
                let default_title = NSString::from_str("App");
                button.setTitle(&default_title);
            }

            Self {
                status_item,
                panel_mode: Cell::new(false),
                stored_menu: RefCell::new(None),
            }
        }
    }

    pub fn set_icon_rendering_mode(&self, rendering_mode: TrayIconRenderingMode) {
        unsafe {
            if let Some(button) = self.status_item.button(main_thread_marker()) {
                if let Some(image) = button.image() {
                    Self::apply_icon_rendering_mode(&image, rendering_mode);
                }
            }
        }
    }

    pub fn set_icon(&self, icon_data: Option<&[u8]>, rendering_mode: TrayIconRenderingMode) {
        unsafe {
            let Some(button) = self.status_item.button(main_thread_marker()) else {
                return;
            };
            match icon_data {
                Some(data) => {
                    let ns_data = NSData::with_bytes(data);
                    if let Some(image) = NSImage::initWithData(NSImage::alloc(), &ns_data) {
                        image.setSize(NSSize {
                            width: 18.0,
                            height: 18.0,
                        });
                        Self::apply_icon_rendering_mode(&image, rendering_mode);
                        button.setImage(Some(&image));
                        let empty = NSString::from_str("");
                        button.setTitle(&empty);
                    }
                }
                None => {
                    button.setImage(None);
                }
            }
        }
    }

    #[allow(dead_code, unused_unsafe)]
    unsafe fn apply_icon_rendering_mode(image: &NSImage, rendering_mode: TrayIconRenderingMode) {
        let is_template = matches!(rendering_mode, TrayIconRenderingMode::Adaptive);
        unsafe { image.setTemplate(is_template) };
    }

    #[allow(dead_code, unused_unsafe)]
    pub fn set_title(&self, title: &str) {
        unsafe {
            if let Some(button) = self.status_item.button(main_thread_marker()) {
                let ns_title = NSString::from_str(title);
                button.setTitle(&ns_title);
            }
        }
    }

    #[allow(unused_unsafe)]
    pub fn set_tooltip(&self, tooltip: &str) {
        unsafe {
            if let Some(button) = self.status_item.button(main_thread_marker()) {
                let ns_tooltip = NSString::from_str(tooltip);
                button.setToolTip(Some(&ns_tooltip));
            }
        }
    }

    pub fn set_menu(&self, items: Vec<TrayMenuItem>) {
        unsafe {
            let menu = NSMenu::new(main_thread_marker());
            menu.setAutoenablesItems(false);
            build_menu_with_selector(&menu, &items, objc2::sel!(handleTrayMenuItem:));

            self.stored_menu.replace(Some(menu));

            if !self.panel_mode.get() {
                let stored_menu = self.stored_menu.borrow();
                self.status_item.setMenu(stored_menu.as_deref());
            }
        }
    }

    pub fn set_panel_mode(&self, enabled: bool) {
        self.panel_mode.set(enabled);
        unsafe {
            if enabled {
                self.status_item.setMenu(None);

                if let Some(button) = self.status_item.button(main_thread_marker()) {
                    if let Some(delegate) = get_app_delegate() {
                        button.setTarget(Some(delegate.as_ref()));
                        button.setAction(Some(objc2::sel!(handleTrayPanelClick:)));
                    }
                }
            } else {
                if let Some(button) = self.status_item.button(main_thread_marker()) {
                    button.setTarget(None);
                    button.setAction(None);
                }

                let stored = self.stored_menu.borrow();
                self.status_item.setMenu(stored.as_deref());
            }
        }
    }

    pub(crate) fn disconnect_app_delegate(&self) {
        unsafe {
            if let Some(button) = self.status_item.button(main_thread_marker()) {
                button.setTarget(None);
                button.setAction(None);
            }
            self.status_item.setMenu(None);
            if let Some(menu) = self.stored_menu.borrow().as_ref() {
                clear_menu_item_targets(menu);
            }
        }
    }

    pub fn get_icon_anchor(&self) -> Option<TrayAnchor> {
        unsafe {
            let button = self.status_item.button(main_thread_marker())?;
            let button_window = button.window()?;
            let frame = button_window.frame();
            let screen = button_window.screen()?;
            screen_frame_to_tray_anchor(Retained::as_ptr(&screen) as *mut AnyObject, frame)
        }
    }

    pub fn get_icon_bounds(&self) -> Option<Bounds<Pixels>> {
        self.get_icon_anchor().map(|anchor| anchor.bounds)
    }
}

impl Drop for MacTray {
    #[allow(unused_unsafe)]
    fn drop(&mut self) {
        unsafe {
            let status_bar = NSStatusBar::systemStatusBar();
            status_bar.removeStatusItem(&self.status_item);
        }
    }
}

fn clear_menu_item_targets(menu: &NSMenu) {
    unsafe {
        for item in menu.itemArray().iter() {
            item.setTarget(None);
            if let Some(submenu) = item.submenu() {
                clear_menu_item_targets(&submenu);
            }
        }
    }
}

fn get_app_delegate() -> Option<Retained<objc2::runtime::ProtocolObject<dyn NSApplicationDelegate>>>
{
    let app = NSApplication::sharedApplication(main_thread_marker());
    app.delegate()
}

pub(crate) unsafe fn configure_actionable_item_with_selector(
    menu_item: &NSMenuItem,
    item_id: &str,
    selector: objc2::runtime::Sel,
) {
    unsafe {
        if let Some(delegate) = get_app_delegate() {
            menu_item.setTarget(Some(delegate.as_ref()));
            menu_item.setAction(Some(selector));
            let represented = NSString::from_str(item_id);
            menu_item.setRepresentedObject(Some(represented.as_ref()));
            menu_item.setEnabled(true);
        }
    }
}

pub(crate) unsafe fn build_menu_with_selector(
    menu: &NSMenu,
    items: &[TrayMenuItem],
    selector: objc2::runtime::Sel,
) {
    unsafe {
        for item in items {
            match item {
                TrayMenuItem::Action { label, id } => {
                    let title = NSString::from_str(label.as_ref());
                    let empty = NSString::from_str("");
                    let menu_item = NSMenuItem::initWithTitle_action_keyEquivalent(
                        NSMenuItem::alloc(main_thread_marker()),
                        &title,
                        None,
                        &empty,
                    );
                    configure_actionable_item_with_selector(&menu_item, id.as_ref(), selector);
                    menu.addItem(&menu_item);
                }
                TrayMenuItem::Separator => {
                    let separator = NSMenuItem::separatorItem(main_thread_marker());
                    menu.addItem(&separator);
                }
                TrayMenuItem::Submenu {
                    label,
                    items: sub_items,
                } => {
                    let title = NSString::from_str(label.as_ref());
                    let empty = NSString::from_str("");
                    let menu_item = NSMenuItem::initWithTitle_action_keyEquivalent(
                        NSMenuItem::alloc(main_thread_marker()),
                        &title,
                        None,
                        &empty,
                    );
                    let submenu = NSMenu::new(main_thread_marker());
                    build_menu_with_selector(&submenu, sub_items, selector);
                    menu_item.setSubmenu(Some(&submenu));
                    menu.addItem(&menu_item);
                }
                TrayMenuItem::Toggle { label, checked, id } => {
                    let title = NSString::from_str(label.as_ref());
                    let empty = NSString::from_str("");
                    let menu_item = NSMenuItem::initWithTitle_action_keyEquivalent(
                        NSMenuItem::alloc(main_thread_marker()),
                        &title,
                        None,
                        &empty,
                    );
                    configure_actionable_item_with_selector(&menu_item, id.as_ref(), selector);
                    menu_item.setState(if *checked {
                        NSControlStateValueOn
                    } else {
                        NSControlStateValueOff
                    });
                    menu.addItem(&menu_item);
                }
            }
        }
    }
}

fn main_thread_marker() -> MainThreadMarker {
    unsafe { MainThreadMarker::new_unchecked() }
}

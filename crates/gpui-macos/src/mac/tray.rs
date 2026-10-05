use super::screen_frame_to_tray_anchor;
use crate::TrayIconEvent;
use crate::TrayMenuItem;
use crate::{Bounds, Pixels, TrayAnchor, TrayIconRenderingMode};
use objc2::runtime::AnyObject;
use objc2::{AnyThread, MainThreadMarker, MainThreadOnly, rc::Retained};
use objc2_app_kit::{
    NSApplication, NSApplicationDelegate, NSControlStateValueOff, NSControlStateValueOn,
    NSEventMask, NSEventType, NSImage, NSMenu, NSMenuItem, NSStatusBar, NSStatusItem,
};
use objc2_foundation::{NSData, NSSize, NSString};
use std::cell::{Cell, RefCell};

pub(crate) struct MacTray {
    status_item: Retained<NSStatusItem>,
    panel_mode: Cell<bool>,
    stored_menu: RefCell<Option<Retained<NSMenu>>>,
    click_menu_conflict_warned: Cell<bool>,
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

            let tray = Self {
                status_item,
                panel_mode: Cell::new(false),
                stored_menu: RefCell::new(None),
                click_menu_conflict_warned: Cell::new(false),
            };
            tray.wire_click_handler();
            tray
        }
    }

    /// Point the status item button's action at the app delegate's tray click
    /// handler. Idempotent, and re-run from every state transition because the
    /// app delegate may not exist yet when the tray is created before
    /// `Platform::run`. AppKit gives an attached menu precedence over the
    /// button action, so wiring never changes menu-mode behavior.
    pub(crate) fn wire_click_handler(&self) {
        unsafe {
            if let Some(button) = self.status_item.button(main_thread_marker()) {
                if let Some(delegate) = get_app_delegate() {
                    button.setTarget(Some(delegate.as_ref()));
                    button.setAction(Some(objc2::sel!(handleTrayPanelClick:)));
                    // NSControl defaults to the primary (left) activation
                    // event. Include secondary/other mouse-up events so the
                    // action is actually invoked before currentEvent is
                    // classified by the delegate.
                    button.sendActionOn(
                        NSEventMask::LeftMouseUp
                            | NSEventMask::RightMouseUp
                            | NSEventMask::OtherMouseUp,
                    );
                }
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
        self.wire_click_handler();
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
        self.wire_click_handler();
        if enabled {
            // Panel mode only detaches the menu so clicks reach the
            // always-wired button action instead of opening the menu.
            self.status_item.setMenu(None);
        } else {
            let stored = self.stored_menu.borrow();
            self.status_item.setMenu(stored.as_deref());
        }
    }

    pub fn panel_mode(&self) -> bool {
        self.panel_mode.get()
    }

    pub fn has_menu(&self) -> bool {
        self.stored_menu.borrow().is_some()
    }

    /// Warn once that an attached menu consumes tray clicks, so registered
    /// icon-click callbacks never fire until panel mode detaches the menu.
    pub fn warn_click_menu_conflict(&self) {
        if !self.click_menu_conflict_warned.replace(true) {
            log::warn!(
                "Tray icon click callbacks are registered, but a menu is attached and panel \
                 mode is off; clicks will open the menu instead of firing the callbacks. Call \
                 set_tray_panel_mode(true) to receive click events."
            );
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

/// Maps an AppKit mouse event to the cross-platform tray click kind.
///
/// `button` is `NSEvent.buttonNumber` (0 = left, 1 = right) and `click_count`
/// is `NSEvent.clickCount`. Non-mouse events (e.g. programmatic
/// `performClick:`) fall back to a plain left click.
fn classify_tray_click(is_mouse_event: bool, button: isize, click_count: isize) -> TrayIconEvent {
    if is_mouse_event && button == 0 && click_count >= 2 {
        return TrayIconEvent::DoubleClick;
    }
    match (is_mouse_event, button) {
        (true, 1) => TrayIconEvent::RightClick,
        _ => TrayIconEvent::LeftClick,
    }
}

/// Classifies the tray click that is currently being delivered, from
/// `NSApplication.currentEvent`. The status item action fires on mouse-up,
/// so the current event is the mouse event that triggered it.
pub(crate) fn current_tray_click_kind() -> TrayIconEvent {
    let Some(event) = NSApplication::sharedApplication(main_thread_marker()).currentEvent() else {
        return TrayIconEvent::LeftClick;
    };
    let is_mouse_event = matches!(
        event.r#type(),
        NSEventType::LeftMouseDown
            | NSEventType::LeftMouseUp
            | NSEventType::RightMouseDown
            | NSEventType::RightMouseUp
            | NSEventType::OtherMouseDown
            | NSEventType::OtherMouseUp
    );
    classify_tray_click(is_mouse_event, event.buttonNumber(), event.clickCount())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_left_right_and_double_clicks() {
        assert_eq!(classify_tray_click(true, 0, 1), TrayIconEvent::LeftClick);
        assert_eq!(classify_tray_click(true, 1, 1), TrayIconEvent::RightClick);
        assert_eq!(classify_tray_click(true, 0, 2), TrayIconEvent::DoubleClick);
        assert_eq!(classify_tray_click(true, 0, 3), TrayIconEvent::DoubleClick);
        // Double right-clicks stay right clicks; only the left button
        // reports DoubleClick.
        assert_eq!(classify_tray_click(true, 1, 2), TrayIconEvent::RightClick);
        // Middle and other buttons have no dedicated variant.
        assert_eq!(classify_tray_click(true, 2, 1), TrayIconEvent::LeftClick);
        // Non-mouse events (e.g. programmatic performClick:) default to left.
        assert_eq!(classify_tray_click(false, 1, 1), TrayIconEvent::LeftClick);
        assert_eq!(classify_tray_click(false, 0, 2), TrayIconEvent::LeftClick);
    }
}

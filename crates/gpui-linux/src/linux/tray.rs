#![cfg_attr(not(any(feature = "wayland", feature = "x11")), allow(dead_code))]

use std::{
    path::Path,
    sync::{Arc, Mutex},
};

use ksni::blocking::TrayMethods as _;

use crate::linux::platform::{LinuxTrayClickEvent, LinuxTrayEvent};
use gpui::{DevicePixels, SharedString, TrayIconEvent, TrayMenuItem, point};

type TrayActionCallback = Arc<Mutex<Option<Box<dyn Fn(SharedString) + Send>>>>;
type TrayClickCallback = Arc<Mutex<Option<Box<dyn Fn(LinuxTrayClickEvent) + Send>>>>;

const DEFAULT_STATUS_NOTIFIER_ID: &str = "gpui-tray";

struct GpuiTray {
    id: String,
    icon_data: Vec<u8>,
    tooltip: String,
    menu_items: Vec<TrayMenuItem>,
    action_callback: TrayActionCallback,
    click_callback: TrayClickCallback,
}

impl ksni::Tray for GpuiTray {
    /// Left-click calls `activate()` instead of opening the menu.
    const MENU_ON_ACTIVATE: bool = false;

    fn id(&self) -> String {
        self.id.clone()
    }

    fn icon_pixmap(&self) -> Vec<ksni::Icon> {
        if self.icon_data.is_empty() {
            return vec![];
        }
        if let Ok(img) = image::load_from_memory(&self.icon_data) {
            let rgba = img.to_rgba8();
            let width = rgba.width() as i32;
            let height = rgba.height() as i32;
            let raw = rgba.as_raw();
            let mut argb_data = Vec::with_capacity(raw.len());
            for pixel in raw.chunks_exact(4) {
                argb_data.push(pixel[3]);
                argb_data.push(pixel[0]);
                argb_data.push(pixel[1]);
                argb_data.push(pixel[2]);
            }
            vec![ksni::Icon {
                width,
                height,
                data: argb_data,
            }]
        } else {
            vec![]
        }
    }

    fn title(&self) -> String {
        self.tooltip.clone()
    }

    fn tool_tip(&self) -> ksni::ToolTip {
        ksni::ToolTip {
            title: self.tooltip.clone(),
            ..Default::default()
        }
    }

    fn activate(&mut self, x: i32, y: i32) {
        emit_click(
            &self.click_callback,
            click_event_with_position(TrayIconEvent::LeftClick, x, y),
        );
    }

    fn secondary_activate(&mut self, x: i32, y: i32) {
        // SNI "secondary activate" is an auxiliary activation, commonly
        // triggered by middle or alternate click depending on the host.
        // GPUI maps it to RightClick for compatibility with the existing API.
        emit_click(
            &self.click_callback,
            click_event_with_position(TrayIconEvent::RightClick, x, y),
        );
    }

    fn menu(&self) -> Vec<ksni::MenuItem<Self>> {
        self.menu_items
            .iter()
            .map(|item| convert_menu_item(item, &self.action_callback))
            .collect()
    }
}

fn derive_status_notifier_id_from_path(path: &Path) -> Option<String> {
    path.file_stem()
        .or_else(|| path.file_name())
        .map(|name| name.to_string_lossy().trim().to_string())
        .filter(|name| !name.is_empty())
}

fn default_status_notifier_id() -> String {
    // GNOME AppIndicator hosts keep the item hidden until Id is non-empty.
    std::env::current_exe()
        .ok()
        .and_then(|path| derive_status_notifier_id_from_path(&path))
        .unwrap_or_else(|| DEFAULT_STATUS_NOTIFIER_ID.to_string())
}

fn click_event_with_position(kind: TrayIconEvent, x: i32, y: i32) -> LinuxTrayClickEvent {
    LinuxTrayClickEvent {
        kind,
        position: point(DevicePixels(x), DevicePixels(y)),
    }
}

fn emit_click(callback: &TrayClickCallback, event: LinuxTrayClickEvent) {
    if let Ok(guard) = callback.lock()
        && let Some(ref callback) = *guard
    {
        callback(event);
    }
}

fn emit_action(callback: &TrayActionCallback, id: SharedString) {
    if let Ok(guard) = callback.lock()
        && let Some(ref callback) = *guard
    {
        callback(id);
    }
}

fn convert_menu_item(
    item: &TrayMenuItem,
    action_callback: &TrayActionCallback,
) -> ksni::MenuItem<GpuiTray> {
    match item {
        TrayMenuItem::Action { label, id } => {
            let id_clone = id.clone();
            let cb = action_callback.clone();
            ksni::MenuItem::Standard(ksni::menu::StandardItem {
                label: label.to_string(),
                activate: Box::new(move |_tray: &mut GpuiTray| {
                    emit_action(&cb, id_clone.clone());
                }),
                ..Default::default()
            })
        }
        TrayMenuItem::Separator => ksni::MenuItem::Separator,
        TrayMenuItem::Submenu { label, items } => ksni::MenuItem::SubMenu(ksni::menu::SubMenu {
            label: label.to_string(),
            submenu: items
                .iter()
                .map(|i| convert_menu_item(i, action_callback))
                .collect(),
            ..Default::default()
        }),
        TrayMenuItem::Toggle { label, checked, id } => {
            let id_clone = id.clone();
            let cb = action_callback.clone();
            ksni::MenuItem::Standard(ksni::menu::StandardItem {
                label: label.to_string(),
                icon_name: if *checked {
                    "checkbox-checked-symbolic".to_string()
                } else {
                    String::new()
                },
                activate: Box::new(move |_tray: &mut GpuiTray| {
                    emit_action(&cb, id_clone.clone());
                }),
                ..Default::default()
            })
        }
    }
}

pub struct LinuxTray {
    handle: Option<ksni::blocking::Handle<GpuiTray>>,
    action_callback: TrayActionCallback,
    click_callback: TrayClickCallback,
}

impl LinuxTray {
    pub fn with_event_sender(sender: calloop::channel::Sender<LinuxTrayEvent>) -> Self {
        let tray = Self::new();
        tray.set_event_sender(sender);
        tray
    }

    pub fn new() -> Self {
        Self {
            handle: None,
            action_callback: Arc::new(Mutex::new(None)),
            click_callback: Arc::new(Mutex::new(None)),
        }
    }

    /// No-op on ksni 0.3 — `MENU_ON_ACTIVATE = false` is the compile-time default.
    pub fn set_panel_mode(&mut self, _enabled: bool) {}

    fn ensure_started(&mut self) {
        if self.handle.is_some() {
            return;
        }
        let tray = GpuiTray {
            id: default_status_notifier_id(),
            icon_data: Vec::new(),
            tooltip: String::new(),
            menu_items: Vec::new(),
            action_callback: self.action_callback.clone(),
            click_callback: self.click_callback.clone(),
        };
        match tray.spawn() {
            Ok(handle) => {
                self.handle = Some(handle);
            }
            Err(e) => {
                log::warn!(target: "tray", "Failed to start tray service: {e}");
            }
        }
    }

    pub fn set_icon(&mut self, icon_data: Option<&[u8]>) {
        self.ensure_started();
        if let Some(handle) = &self.handle {
            let data = icon_data.unwrap_or(&[]).to_vec();
            if handle
                .update(move |tray: &mut GpuiTray| {
                    tray.icon_data = data;
                })
                .is_none()
            {
                log::warn!(target: "tray", "Tray service has shut down, cannot update icon");
            }
        }
    }

    pub fn set_tooltip(&mut self, tooltip: &str) {
        self.ensure_started();
        if let Some(handle) = &self.handle {
            let tooltip = tooltip.to_string();
            if handle
                .update(move |tray: &mut GpuiTray| {
                    tray.tooltip = tooltip;
                })
                .is_none()
            {
                log::warn!(target: "tray", "Tray service has shut down, cannot update tooltip");
            }
        }
    }

    pub fn set_menu(&mut self, items: Vec<TrayMenuItem>) {
        self.ensure_started();
        if let Some(handle) = &self.handle {
            if handle
                .update(move |tray: &mut GpuiTray| {
                    tray.menu_items = items;
                })
                .is_none()
            {
                log::warn!(target: "tray", "Tray service has shut down, cannot update menu");
            }
        }
    }

    fn set_on_menu_action(&self, callback: Box<dyn Fn(SharedString) + Send>) {
        if let Ok(mut guard) = self.action_callback.lock() {
            *guard = Some(callback);
        }
    }

    fn set_on_click(&self, callback: Box<dyn Fn(LinuxTrayClickEvent) + Send>) {
        if let Ok(mut guard) = self.click_callback.lock() {
            *guard = Some(callback);
        }
    }

    pub fn set_event_sender(&self, sender: calloop::channel::Sender<LinuxTrayEvent>) {
        let click_sender = sender.clone();
        self.set_on_click(Box::new(move |event| {
            let _ = click_sender.send(LinuxTrayEvent::Click(event));
        }));

        self.set_on_menu_action(Box::new(move |id| {
            let _ = sender.send(LinuxTrayEvent::MenuAction(id));
        }));
    }

    pub fn shutdown(&mut self) {
        if let Some(handle) = self.handle.take() {
            handle.shutdown().wait();
        }
    }
}

impl Drop for LinuxTray {
    fn drop(&mut self) {
        self.shutdown();
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::derive_status_notifier_id_from_path;

    #[test]
    fn derives_status_notifier_id_from_executable_stem() {
        assert_eq!(
            derive_status_notifier_id_from_path(Path::new("/opt/BananaTray/bin/bananatray")),
            Some("bananatray".to_string())
        );
    }

    #[test]
    fn drops_executable_suffix_when_deriving_status_notifier_id() {
        assert_eq!(
            derive_status_notifier_id_from_path(Path::new("/opt/BananaTray/bin/BananaTray.exe")),
            Some("BananaTray".to_string())
        );
    }

    #[test]
    fn returns_none_when_executable_name_is_missing() {
        assert_eq!(derive_status_notifier_id_from_path(Path::new("/")), None);
    }
}

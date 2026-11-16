//! Tray title demo.
//!
//! A counter replaces the title once a second, padded to two digits so the
//! item does not change width. "Toggle icon" keeps that title and removes or
//! restores the icon. The tooltip stays "Test Tray App".
//!
//! Linux stores the title and reports `TrayTitleSupport::Unsupported` until
//! ksni can publish an Ayatana label. Windows reports the same.

use std::cell::Cell;
use std::rc::Rc;
use std::time::Duration;

use gpui::{App, Application, QuitMode, Timer, TrayMenuItem};

const TRAY_ICON: &[u8] = include_bytes!("tray_icon.png");

fn main() {
    Application::new().run(|cx: &mut App| {
        cx.set_quit_mode(QuitMode::Explicit);

        eprintln!("tray title support: {:?}", cx.tray_title_support());
        cx.set_tray_tooltip("Test Tray App");
        cx.set_tray_icon(Some(TRAY_ICON));
        cx.set_tray_title(Some("00"));

        let show_icon = Rc::new(Cell::new(true));
        let ticks = Rc::new(Cell::new(0u32));

        cx.set_tray_menu(vec![
            TrayMenuItem::Action {
                label: "Toggle icon".into(),
                id: "toggle-icon".into(),
            },
            TrayMenuItem::Separator,
            TrayMenuItem::Action {
                label: "Quit".into(),
                id: "quit".into(),
            },
        ]);

        cx.on_tray_menu_action(move |id, cx| match id.as_ref() {
            "toggle-icon" => {
                let next = !show_icon.get();
                show_icon.set(next);
                cx.set_tray_icon(next.then_some(TRAY_ICON));
                eprintln!("tray icon {}", if next { "shown" } else { "hidden" });
            }
            "quit" => cx.quit(),
            _ => eprintln!("Menu action: {id}"),
        });

        cx.spawn(async move |cx| {
            loop {
                Timer::after(Duration::from_secs(1)).await;
                let next = ticks.get().wrapping_add(1) % 100;
                ticks.set(next);
                let title = format!("{next:02}");
                if cx
                    .update(|cx| {
                        cx.set_tray_title(Some(&title));
                    })
                    .is_err()
                {
                    break;
                }
            }
        })
        .detach();

        eprintln!("Tray should be visible. The title counts up once a second.");
    });
}

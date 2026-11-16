use gpui::*;

struct PlatformDemo;

impl Render for PlatformDemo {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .flex()
            .flex_col()
            .size_full()
            .justify_center()
            .items_center()
            .bg(rgb(0x1e1e2e))
            .child(
                div()
                    .text_color(rgb(0xcdd6f4))
                    .text_xl()
                    .child("Platform Features Demo"),
            )
    }
}

fn main() {
    Application::new().run(|cx: &mut App| {
        let info = cx.os_info();
        println!("OS: {} {} ({})", info.name, info.version, info.arch);
        println!("Locale: {}, Hostname: {}", info.locale, info.hostname);

        cx.set_crash_handler(None, |report| {
            eprintln!("CRASH: {}", report.message);
        });

        let status = cx.network_status();
        println!("Network: {:?}", status);

        if cx.supports_network_status_events() {
            cx.on_network_status_change(|status, _app| {
                println!("Network changed: {:?}", status);
            });
        } else {
            println!("Network change monitoring is not supported on this platform");
        }

        if cx.supports_system_power_events() {
            cx.on_system_power_event(|event, _app| {
                println!("Power event: {:?}", event);
            });
        } else {
            println!("System power events are not supported on this platform");
        }

        if let Some(idle) = cx.system_idle_time() {
            println!("System idle: {:?}", idle);
        }

        let bio = cx.biometric_status();
        println!("Biometrics: {:?}", bio);

        cx.set_dock_badge(Some("3"));

        cx.request_user_attention(AttentionType::Informational);

        let bounds = Bounds::centered(None, size(px(400.0), px(300.0)), cx);
        cx.open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                ..Default::default()
            },
            |_window, cx| cx.new(|_cx| PlatformDemo),
        )
        .unwrap();
    });
}

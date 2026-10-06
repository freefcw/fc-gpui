use gpui::{
    App, AppProfile, Application, Context, Empty, IntoElement, Render, Window, WindowKind,
    WindowOptions,
};

#[derive(gpui::Render)]
struct DerivedView;

struct ManualView;

impl Render for ManualView {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        Empty
    }
}

fn compile_startup_and_desktop_contracts() {
    let _headless = Application::headless();
    let _overlay = WindowOptions {
        kind: WindowKind::Overlay,
        mouse_passthrough: true,
        ..WindowOptions::default()
    };

    Application::new()
        .with_resource_profile(AppProfile::Minimal)
        .run(|cx: &mut App| {
            cx.set_quit_mode(gpui::QuitMode::Explicit);
            cx.set_tray_tooltip("Adabraka GPUI");
            let _ = cx.show_notification("Ready", "Compatibility fixture");
        });
}

// A plain (default-key) dependency imports under the library target name, so
// fc-gpui-collections must stay fc_gpui_collections; this pin fails to compile
// if a custom [lib] name ever returns.
fn compile_utility_default_key_import_contract() {
    let counts: fc_gpui_collections::HashMap<&str, u32> = fc_gpui_collections::HashMap::default();
    let _ = counts.len();
}

fn main() {
    let _ = DerivedView;
    let _ = ManualView;
    let _ = compile_startup_and_desktop_contracts;
    let _ = compile_utility_default_key_import_contract;
}

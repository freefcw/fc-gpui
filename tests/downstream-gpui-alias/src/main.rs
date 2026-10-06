//! Pins the Zed-style import path: `gpui = { package = "fc-gpui" }` keeps
//! `use gpui::…`, derives, `register_action!`, and `#[gpui::test]` working
//! with no extra configuration.

use gpui::Empty;

#[derive(gpui::Render)]
struct DerivedView;

#[derive(Clone, PartialEq, gpui::Action)]
struct AliasedAction;

#[derive(Clone, PartialEq, gpui::Action)]
#[action(no_register)]
struct ExplicitlyRegisteredAction;

gpui::register_action!(ExplicitlyRegisteredAction);

#[derive(gpui::IntoElement)]
struct DerivedElement;

impl gpui::RenderOnce for DerivedElement {
    fn render(self, _window: &mut gpui::Window, _cx: &mut gpui::App) -> impl gpui::IntoElement {
        Empty
    }
}

#[derive(gpui::AppContext, gpui::VisualContext)]
#[allow(dead_code)]
struct AliasedContext<'a, 'b> {
    #[app]
    app: &'a mut gpui::App,
    #[window]
    window: &'b mut gpui::Window,
}

#[gpui::test]
fn aliased_test_macro(_cx: &mut gpui::TestAppContext) {}

fn main() {
    let _ = DerivedView;
    let _ = AliasedAction;
    let _ = DerivedElement;
}

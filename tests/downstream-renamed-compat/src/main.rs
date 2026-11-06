#[derive(ui::Render)]
struct DerivedView;

#[derive(Clone, PartialEq, ui::Action)]
struct RenamedAction;

#[derive(Clone, PartialEq, ui::Action)]
#[action(no_register)]
struct ExplicitlyRegisteredAction;

ui::register_action!(ExplicitlyRegisteredAction);

#[derive(ui::IntoElement)]
struct DerivedElement;

impl ui::RenderOnce for DerivedElement {
    fn render(self, _window: &mut ui::Window, _cx: &mut ui::App) -> impl ui::IntoElement {
        ui::Empty
    }
}

#[derive(ui::AppContext, ui::VisualContext)]
#[allow(dead_code)]
struct RenamedContext<'a, 'b> {
    #[app]
    app: &'a mut ui::App,
    #[window]
    window: &'b mut ui::Window,
}

#[ui::test]
fn renamed_test_macro(_cx: &mut ui::TestAppContext) {}

#[cfg(test)]
use ui::private::rand::{RngCore, SeedableRng, rngs::StdRng};

#[ui::test(seed = 7)]
fn renamed_seeded_test_macro(mut rng: StdRng) {
    let mut expected = StdRng::seed_from_u64(7);
    assert_eq!(rng.next_u64(), expected.next_u64());
}

#[ui::test(seed = 11)]
async fn renamed_async_seeded_test_macro(mut rng: StdRng) {
    let mut expected = StdRng::seed_from_u64(11);
    assert_eq!(rng.next_u64(), expected.next_u64());
}

fn main() {
    let _ = DerivedView;
    let _ = RenamedAction;
    let _ = DerivedElement;
}

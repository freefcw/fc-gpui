#[test]
fn test_derive_context() {
    use fc_gpui_macros::{AppContext, VisualContext};
    use gpui::{App, Window};

    #[derive(AppContext, VisualContext)]
    struct _MyCustomContext<'a, 'b> {
        #[app]
        app: &'a mut App,
        #[window]
        window: &'b mut Window,
    }
}

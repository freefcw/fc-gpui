#[test]
fn test_derive_render() {
    use fc_gpui_macros::Render;

    #[derive(Render)]
    struct _Element;
}

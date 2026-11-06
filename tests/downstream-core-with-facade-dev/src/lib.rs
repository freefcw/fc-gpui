mod style_macros {
    use core_ui as style_ui;
    include!("../../style_macros.rs");
}

#[derive(core_ui::Render)]
pub struct CoreView;

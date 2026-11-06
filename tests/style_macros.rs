// Deliberately import neither `gpui`, `smallvec`, nor the generated method types.
// Both downstream fixtures provide their dependency through the `style_ui` alias.
#[derive(Default)]
struct StyledFixture(style_ui::StyleRefinement);

#[allow(dead_code)]
impl StyledFixture {
    fn style(&mut self) -> &mut style_ui::StyleRefinement {
        &mut self.0
    }

    style_ui::visibility_style_methods!({ visibility: pub });
    style_ui::margin_style_methods!({ visibility: pub });
    style_ui::padding_style_methods!({ visibility: pub });
    style_ui::position_style_methods!({ visibility: pub });
    style_ui::overflow_style_methods!({ visibility: pub });
    style_ui::cursor_style_methods!({ visibility: pub });
    style_ui::border_style_methods!({ visibility: pub });
    style_ui::box_shadow_style_methods!({ visibility: pub });
}

#[test]
fn renamed_dependency_style_macros_preserve_values() {
    let fixture = StyledFixture::default()
        .invisible()
        .visible()
        .m(style_ui::px(2.))
        .mt_neg_1()
        .p_2()
        .pr(style_ui::px(3.))
        .absolute()
        .top_1()
        .left(style_ui::px(4.))
        .overflow_hidden()
        .cursor(style_ui::CursorStyle::IBeam)
        .cursor_pointer()
        .border_2()
        .border_color(style_ui::hsla(0., 0., 0., 1.))
        .shadow_sm();

    let style = fixture.0;
    assert_eq!(style.visibility, Some(style_ui::Visibility::Visible));
    assert_eq!(style.margin.top, Some((-style_ui::rems(0.25)).into()));
    assert_eq!(style.margin.right, Some(style_ui::px(2.).into()));
    assert_eq!(style.padding.top, Some(style_ui::rems(0.5).into()));
    assert_eq!(style.padding.right, Some(style_ui::px(3.).into()));
    assert_eq!(style.position, Some(style_ui::Position::Absolute));
    assert_eq!(style.inset.top, Some(style_ui::rems(0.25).into()));
    assert_eq!(style.inset.left, Some(style_ui::px(4.).into()));
    assert_eq!(style.overflow.x, Some(style_ui::Overflow::Hidden));
    assert_eq!(style.overflow.y, Some(style_ui::Overflow::Hidden));
    assert_eq!(
        style.mouse_cursor,
        Some(style_ui::CursorStyle::PointingHand)
    );
    assert_eq!(style.border_widths.top, Some(style_ui::px(2.).into()));
    assert_eq!(style.border_color, Some(style_ui::hsla(0., 0., 0., 1.)));
    assert_eq!(style.box_shadow.unwrap().len(), 2);

    let inset = StyledFixture::default()
        .shadow(Default::default())
        .shadow_inset(Default::default())
        .inset_shadow(
            style_ui::hsla(0., 0., 0., 0.5),
            style_ui::px(2.),
            style_ui::px(1.),
        );
    let shadows = inset.0.box_shadow.unwrap();
    assert_eq!(shadows.len(), 1);
    assert!(shadows[0].inset);
}

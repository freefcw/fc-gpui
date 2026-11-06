//! A container query element, in the spirit of CSS container queries.
//! The element's own size is determined solely by its style and the space
//! offered by its parent.

use crate::{
    AnyElement, App, AvailableSpace, Bounds, Element, ElementId, GlobalElementId, Hitbox,
    InspectorElementId, Interactivity, IntoElement, LayoutId, Pixels, Size, StyleRefinement,
    Styled, Window, relative,
};

/// Construct a container query element with the given render callback.
/// The callback receives the size the element was assigned during layout and
/// returns the contents to display within it.
///
/// By default the element fills its parent (equivalent to `.size_full()`);
/// use the [`Styled`] methods to size it differently. Because the contents
/// don't exist until after layout, they cannot influence the element's size.
///
/// # Example
///
/// ```
/// # use gpui::{container_query, div, px, IntoElement, ParentElement};
/// container_query(|size, _window, _cx| {
///     if size.width < px(240.) {
///         div().child("Narrow layout")
///     } else {
///         div().child("Wide layout")
///     }
/// });
/// ```
pub fn container_query<E>(
    render: impl 'static + FnOnce(Size<Pixels>, &mut Window, &mut App) -> E,
) -> ContainerQuery
where
    E: IntoElement,
{
    let mut interactivity = Interactivity::new();
    interactivity.base_style.size.width = Some(relative(1.).into());
    interactivity.base_style.size.height = Some(relative(1.).into());

    ContainerQuery {
        render: Some(Box::new(|size, window, cx| {
            render(size, window, cx).into_any_element()
        })),
        interactivity,
    }
}

/// A container query element, created with [`container_query`].
pub struct ContainerQuery {
    render: Option<Box<dyn FnOnce(Size<Pixels>, &mut Window, &mut App) -> AnyElement>>,
    interactivity: Interactivity,
}

impl Element for ContainerQuery {
    type RequestLayoutState = ();
    type PrepaintState = (Option<Hitbox>, Option<AnyElement>);

    fn id(&self) -> Option<ElementId> {
        self.interactivity.element_id.clone()
    }

    fn source_location(&self) -> Option<&'static core::panic::Location<'static>> {
        self.interactivity.source_location()
    }

    fn request_layout(
        &mut self,
        id: Option<&GlobalElementId>,
        inspector_id: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, Self::RequestLayoutState) {
        let layout_id =
            self.interactivity
                .request_layout(id, inspector_id, window, cx, |style, window, cx| {
                    window.request_layout(style, [], cx)
                });
        (layout_id, ())
    }

    fn prepaint(
        &mut self,
        id: Option<&GlobalElementId>,
        inspector_id: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _request_layout: &mut Self::RequestLayoutState,
        window: &mut Window,
        cx: &mut App,
    ) -> Self::PrepaintState {
        self.interactivity.prepaint(
            id,
            inspector_id,
            bounds,
            bounds.size,
            window,
            cx,
            |_, _, hitbox, window, cx| {
                let child = self.render.take().map(|render| {
                    let mut child = render(bounds.size, window, cx);
                    child.layout_as_root(bounds.size.map(AvailableSpace::Definite), window, cx);
                    child.prepaint_at(bounds.origin, window, cx);
                    child
                });
                (hitbox, child)
            },
        )
    }

    fn paint(
        &mut self,
        id: Option<&GlobalElementId>,
        inspector_id: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _request_layout: &mut Self::RequestLayoutState,
        prepaint: &mut Self::PrepaintState,
        window: &mut Window,
        cx: &mut App,
    ) {
        let (hitbox, child) = prepaint;
        self.interactivity.paint(
            id,
            inspector_id,
            bounds,
            hitbox.as_ref(),
            window,
            cx,
            |_, window, cx| {
                if let Some(child) = child {
                    child.paint(window, cx);
                }
            },
        );
    }
}

impl IntoElement for ContainerQuery {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

impl Styled for ContainerQuery {
    fn style(&mut self) -> &mut StyleRefinement {
        &mut self.interactivity.base_style
    }
}

#[cfg(test)]
mod tests {
    use crate::{
        AppContext as _, Context, IntoElement, ParentElement, Pixels, Render, Size, Styled,
        TestAppContext, Window, container_query, div, px, size,
    };
    use std::cell::Cell;
    use std::rc::Rc;

    struct ContainerQueryView {
        last_size: Rc<Cell<Option<Size<Pixels>>>>,
    }

    impl Render for ContainerQueryView {
        fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
            let last_size = self.last_size.clone();
            container_query(move |container_size, _window, _cx| {
                last_size.set(Some(container_size));
                if container_size.width < px(400.) {
                    div().size_full().child("narrow")
                } else {
                    div().size_full().child("wide")
                }
            })
        }
    }

    #[gpui::test]
    fn container_query_builds_children_from_measured_size(cx: &mut TestAppContext) {
        let last_size = Rc::new(Cell::new(None));
        let (_, cx) = cx.add_window_view({
            let last_size = last_size.clone();
            move |_, _| ContainerQueryView { last_size }
        });

        cx.simulate_resize(size(px(640.), px(480.)));
        let wide = last_size
            .get()
            .expect("container_query should run after the wide resize");
        assert!(
            wide.width >= px(400.),
            "wide window should offer a container at least 400px wide, got {}",
            wide.width
        );

        cx.simulate_resize(size(px(320.), px(480.)));
        let narrow = last_size
            .get()
            .expect("container_query should run after the narrow resize");
        assert!(
            narrow.width < px(400.),
            "narrow window should offer a container under 400px wide, got {}",
            narrow.width
        );
        assert_ne!(
            wide, narrow,
            "resizing the window should rebuild children from a new measured size"
        );
    }

    #[gpui::test]
    fn container_query_honors_explicit_size(cx: &mut TestAppContext) {
        let last_size = Rc::new(Cell::new(None));
        struct SizedQuery {
            last_size: Rc<Cell<Option<Size<Pixels>>>>,
        }

        impl Render for SizedQuery {
            fn render(
                &mut self,
                _window: &mut Window,
                _cx: &mut Context<Self>,
            ) -> impl IntoElement {
                let last_size = self.last_size.clone();
                container_query(move |container_size, _window, _cx| {
                    last_size.set(Some(container_size));
                    div().size_full()
                })
                .w(px(240.))
                .h(px(80.))
            }
        }

        let (_, cx) = cx.add_window_view({
            let last_size = last_size.clone();
            move |_, _| SizedQuery { last_size }
        });
        cx.simulate_resize(size(px(800.), px(600.)));
        assert_eq!(
            last_size.get(),
            Some(size(px(240.), px(80.))),
            "container_query should measure from its own style, not only the parent offer"
        );
    }

    #[derive(Clone, Copy, Debug)]
    enum ContainerKind {
        Div,
        Query,
        ImageCache,
    }

    struct StyledContainer {
        kind: ContainerKind,
        hidden: bool,
        observations: Rc<std::cell::RefCell<Vec<(crate::Hsla, crate::ContentMask<Pixels>, f32)>>>,
        cache: crate::Entity<crate::RetainAllImageCache>,
    }

    impl Render for StyledContainer {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            let prepaint_observations = self.observations.clone();
            let paint_observations = self.observations.clone();
            let child = crate::canvas(
                move |_, window, _| {
                    prepaint_observations.borrow_mut().push((
                        window.text_style().color,
                        window.content_mask(),
                        window.element_opacity(),
                    ));
                },
                move |_, _, window, _| {
                    paint_observations.borrow_mut().push((
                        window.text_style().color,
                        window.content_mask(),
                        window.element_opacity(),
                    ));
                },
            )
            .min_w(px(100.))
            .min_h(px(100.))
            .bg(crate::green());
            let hidden = self.hidden;
            fn style<E: Styled>(element: E, hidden: bool) -> E {
                let element = element
                    .size(px(40.))
                    .bg(crate::red())
                    .text_color(crate::blue())
                    .opacity(0.5)
                    .overflow_hidden();
                if hidden { element.invisible() } else { element }
            }
            match self.kind {
                ContainerKind::Div => style(div().child(child), hidden).into_any_element(),
                ContainerKind::Query => {
                    style(container_query(move |_, _, _| child), hidden).into_any_element()
                }
                ContainerKind::ImageCache => {
                    style(crate::image_cache(self.cache.clone()).child(child), hidden)
                        .into_any_element()
                }
            }
        }
    }

    #[gpui::test]
    fn styled_containers_share_paint_and_inheritance_semantics(cx: &mut TestAppContext) {
        for kind in [
            ContainerKind::Div,
            ContainerKind::Query,
            ContainerKind::ImageCache,
        ] {
            for hidden in [false, true] {
                let observations = Rc::new(std::cell::RefCell::new(Vec::new()));
                let window = cx.add_window(|_, cx| StyledContainer {
                    kind,
                    hidden,
                    observations: observations.clone(),
                    cache: crate::RetainAllImageCache::new(cx),
                });
                observations.borrow_mut().clear();
                cx.update_window(window.into(), |_, window, cx| {
                    window.draw(cx).clear();
                    let quads = &window.rendered_frame.scene.quads;
                    assert_eq!(
                        quads.len(),
                        if hidden { 0 } else { 2 },
                        "{kind:?}, hidden={hidden}"
                    );
                    for quad in quads {
                        assert_eq!(quad.background.solid.a, 0.5, "{kind:?}");
                    }
                    assert_eq!(
                        observations.borrow().len(),
                        if hidden { 1 } else { 2 },
                        "{kind:?}, hidden={hidden}"
                    );
                    for (color, mask, opacity) in observations.borrow().iter() {
                        assert_eq!(*color, crate::blue(), "{kind:?}");
                        assert_eq!(mask.bounds.size, size(px(40.), px(40.)), "{kind:?}");
                        assert_eq!(*opacity, 0.5, "{kind:?}");
                    }
                })
                .unwrap();
            }
        }
    }
}

use std::cmp::Ordering;

use smallvec::SmallVec;

use crate::{
    AnyElement, App, Axis, Bounds, Corner, Display, Edges, Element, GlobalElementId,
    InspectorElementId, IntoElement, LayoutId, ParentElement, Pixels, Point, Position, Size, Style,
    Window, point, px,
};

/// The state that the anchored element element uses to track its children.
pub struct AnchoredState {
    child_layout_ids: SmallVec<[LayoutId; 4]>,
}

/// An anchored element that can be used to display UI that
/// will avoid overflowing the window bounds.
pub struct Anchored {
    children: SmallVec<[AnyElement; 2]>,
    anchor_corner: Corner,
    fit_mode: AnchoredFitMode,
    placement: Placement,
    position_mode: AnchoredPositionMode,
    offset: Option<Point<Pixels>>,
}

#[derive(Clone, Copy)]
enum Placement {
    InPlace,
    At(Point<Pixels>),
    Attached {
        target: Bounds<Pixels>,
        corner: Corner,
    },
}

/// anchored gives you an element that will avoid overflowing the window bounds.
/// Its children should have no margin to avoid measurement issues.
pub fn anchored() -> Anchored {
    Anchored {
        children: SmallVec::new(),
        anchor_corner: Corner::TopLeft,
        fit_mode: AnchoredFitMode::SwitchAnchor,
        placement: Placement::InPlace,
        position_mode: AnchoredPositionMode::Window,
        offset: None,
    }
}

impl Anchored {
    /// Sets which corner of the anchored element should be anchored to the current position.
    pub fn anchor(mut self, anchor: Corner) -> Self {
        self.anchor_corner = anchor;
        self
    }

    /// Sets the position in window coordinates
    /// (otherwise the location the anchored element is rendered is used)
    pub fn position(mut self, anchor: Point<Pixels>) -> Self {
        self.placement = Placement::At(anchor);
        self
    }

    /// Places the element next to `target`, with the element's [`Anchored::anchor`] corner
    /// on `corner` of `target`. For example, a submenu puts its top-left corner on its row's
    /// top-right corner. Use this instead of [`Anchored::position`].
    ///
    /// If the element doesn't fit in the window, it moves to the other side of `target`, as
    /// long as that side fits better. A submenu moves from the right of its row to the left,
    /// and a dropdown moves from below its button to above. The offset flips with it.
    /// [`AnchoredFitMode::SwitchAnchor`] has no effect here.
    pub fn attach(mut self, target: Bounds<Pixels>, corner: Corner) -> Self {
        self.placement = Placement::Attached { target, corner };
        self
    }

    /// Offset the final position by this amount.
    /// Useful when you want to anchor to an element but offset from it, such as in PopoverMenu.
    pub fn offset(mut self, offset: Point<Pixels>) -> Self {
        self.offset = Some(offset);
        self
    }

    /// Sets the position mode for this anchored element. Local will have this
    /// interpret its [`Anchored::position`] or [`Anchored::attach`] target as
    /// relative to the parent element. While Window will have it interpret the
    /// position as relative to the window.
    pub fn position_mode(mut self, mode: AnchoredPositionMode) -> Self {
        self.position_mode = mode;
        self
    }

    /// Snap to window edge instead of switching anchor corner when an overflow would occur.
    pub fn snap_to_window(mut self) -> Self {
        self.fit_mode = AnchoredFitMode::SnapToWindow;
        self
    }

    /// Snap to window edge and leave some margins.
    pub fn snap_to_window_with_margin(mut self, edges: impl Into<Edges<Pixels>>) -> Self {
        self.fit_mode = AnchoredFitMode::SnapToWindowWithMargin(edges.into());
        self
    }
}

impl ParentElement for Anchored {
    fn extend(&mut self, elements: impl IntoIterator<Item = AnyElement>) {
        self.children.extend(elements)
    }
}

impl Element for Anchored {
    type RequestLayoutState = AnchoredState;
    type PrepaintState = ();

    fn id(&self) -> Option<crate::ElementId> {
        None
    }

    fn source_location(&self) -> Option<&'static core::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (crate::LayoutId, Self::RequestLayoutState) {
        let child_layout_ids = self
            .children
            .iter_mut()
            .map(|child| child.request_layout(window, cx))
            .collect::<SmallVec<_>>();

        let anchored_style = Style {
            position: Position::Absolute,
            display: Display::Flex,
            ..Style::default()
        };

        let layout_id = window.request_layout(anchored_style, child_layout_ids.iter().copied(), cx);

        (layout_id, AnchoredState { child_layout_ids })
    }

    fn prepaint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        request_layout: &mut Self::RequestLayoutState,
        window: &mut Window,
        cx: &mut App,
    ) {
        if request_layout.child_layout_ids.is_empty() {
            return;
        }

        let children_bounds = request_layout
            .child_layout_ids
            .iter()
            .map(|id| window.layout_bounds(*id))
            .reduce(|acc, bounds| acc.union(&bounds))
            .unwrap();
        let size = children_bounds.size;

        let anchor_position = match self.placement {
            Placement::InPlace => None,
            Placement::At(position) => Some(position),
            Placement::Attached { target, corner } => Some(target.corner(corner)),
        };
        let (origin, mut desired) = self.position_mode.get_position_and_bounds(
            anchor_position,
            self.anchor_corner,
            size,
            bounds,
            self.offset,
        );

        let limits = Bounds {
            origin: Point::default(),
            size: window.viewport_size(),
        };

        if let Placement::Attached { target, mut corner } = self.placement {
            // Compare against the window edges, not the snap margin, so an element that
            // already fits doesn't move. The client inset doesn't count as room either,
            // since on Linux it's the window's transparent shadow.
            let visible = limits.inset(window.client_inset.unwrap_or(px(0.)));
            let overflow_along = |bounds: Bounds<Pixels>, axis: Axis| match axis {
                Axis::Horizontal => {
                    (visible.left() - bounds.left()).max(px(0.))
                        + (bounds.right() - visible.right()).max(px(0.))
                }
                Axis::Vertical => {
                    (visible.top() - bounds.top()).max(px(0.))
                        + (bounds.bottom() - visible.bottom()).max(px(0.))
                }
            };
            let mut anchor_corner = self.anchor_corner;
            let mut offset = self.offset.unwrap_or_default();
            for axis in [Axis::Horizontal, Axis::Vertical] {
                let corner_side = side_along(corner, axis);
                // `Corner` has no center variant, so `side_along` is never `Equal`.
                // The check matches upstream, where a centered attachment does not
                // flip along that axis.
                let is_outside_target = corner_side != Ordering::Equal
                    && side_along(anchor_corner, axis) == corner_side.reverse();
                let overflow = overflow_along(desired, axis);
                if !is_outside_target || overflow == px(0.) {
                    continue;
                }

                let flipped_corner = corner.other_side_corner_along(axis);
                let flipped_anchor = anchor_corner.other_side_corner_along(axis);
                let flipped_offset = match axis {
                    Axis::Horizontal => point(-offset.x, offset.y),
                    Axis::Vertical => point(offset.x, -offset.y),
                };
                let (_, flipped) = self.position_mode.get_position_and_bounds(
                    Some(target.corner(flipped_corner)),
                    flipped_anchor,
                    size,
                    bounds,
                    Some(flipped_offset),
                );
                if overflow_along(flipped, axis) < overflow {
                    corner = flipped_corner;
                    anchor_corner = flipped_anchor;
                    offset = flipped_offset;
                    desired = flipped;
                }
            }
        } else if self.fit_mode == AnchoredFitMode::SwitchAnchor {
            let mut anchor_corner = self.anchor_corner;

            if desired.left() < limits.left() || desired.right() > limits.right() {
                let switched = Bounds::from_corner_and_size(
                    anchor_corner.other_side_corner_along(Axis::Horizontal),
                    origin,
                    size,
                );
                if !(switched.left() < limits.left() || switched.right() > limits.right()) {
                    anchor_corner = anchor_corner.other_side_corner_along(Axis::Horizontal);
                    desired = switched
                }
            }

            if desired.top() < limits.top() || desired.bottom() > limits.bottom() {
                let switched = Bounds::from_corner_and_size(
                    anchor_corner.other_side_corner_along(Axis::Vertical),
                    origin,
                    size,
                );
                if !(switched.top() < limits.top() || switched.bottom() > limits.bottom()) {
                    desired = switched;
                }
            }
        }

        let client_inset = window.client_inset.unwrap_or(px(0.));
        let edges = match self.fit_mode {
            AnchoredFitMode::SnapToWindowWithMargin(edges) => edges,
            _ => Edges::default(),
        }
        .map(|edge| *edge + client_inset);

        // Snap the horizontal edges of the anchored element to the horizontal edges of the window if
        // its horizontal bounds overflow, aligning to the left if it is wider than the limits.
        if desired.right() > limits.right() {
            desired.origin.x -= desired.right() - limits.right() + edges.right;
        }
        if desired.left() < limits.left() {
            desired.origin.x = limits.origin.x + edges.left;
        }

        // Snap the vertical edges of the anchored element to the vertical edges of the window if
        // its vertical bounds overflow, aligning to the top if it is taller than the limits.
        if desired.bottom() > limits.bottom() {
            desired.origin.y -= desired.bottom() - limits.bottom() + edges.bottom;
        }
        if desired.top() < limits.top() {
            desired.origin.y = limits.origin.y + edges.top;
        }

        let offset = desired.origin - bounds.origin;
        let offset = point(offset.x.round(), offset.y.round());

        window.with_element_offset(offset, |window| {
            for child in &mut self.children {
                child.prepaint(window, cx);
            }
        })
    }

    fn paint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        _bounds: crate::Bounds<crate::Pixels>,
        _request_layout: &mut Self::RequestLayoutState,
        _prepaint: &mut Self::PrepaintState,
        window: &mut Window,
        cx: &mut App,
    ) {
        for child in &mut self.children {
            child.paint(window, cx);
        }
    }
}

impl IntoElement for Anchored {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

/// Which side of a box `corner` is on along `axis`. `Less` is left or top and
/// `Greater` is right or bottom.
///
/// Upstream's `Anchor` also has center variants, which return `Equal`. This
/// fork's [`Corner`] does not, so a centered attachment cannot be expressed.
fn side_along(corner: Corner, axis: Axis) -> Ordering {
    match axis {
        Axis::Horizontal => match corner {
            Corner::TopLeft | Corner::BottomLeft => Ordering::Less,
            Corner::TopRight | Corner::BottomRight => Ordering::Greater,
        },
        Axis::Vertical => match corner {
            Corner::TopLeft | Corner::TopRight => Ordering::Less,
            Corner::BottomLeft | Corner::BottomRight => Ordering::Greater,
        },
    }
}

/// Which algorithm to use when fitting the anchored element to be inside the window.
#[derive(Copy, Clone, PartialEq)]
pub enum AnchoredFitMode {
    /// Snap the anchored element to the window edge.
    SnapToWindow,
    /// Snap to window edge and leave some margins.
    SnapToWindowWithMargin(Edges<Pixels>),
    /// Switch which corner anchor this anchored element is attached to.
    SwitchAnchor,
}

/// Which algorithm to use when positioning the anchored element.
#[derive(Copy, Clone, PartialEq)]
pub enum AnchoredPositionMode {
    /// Position the anchored element relative to the window.
    Window,
    /// Position the anchored element relative to its parent.
    Local,
}

impl AnchoredPositionMode {
    fn get_position_and_bounds(
        &self,
        anchor_position: Option<Point<Pixels>>,
        anchor_corner: Corner,
        size: Size<Pixels>,
        bounds: Bounds<Pixels>,
        offset: Option<Point<Pixels>>,
    ) -> (Point<Pixels>, Bounds<Pixels>) {
        let offset = offset.unwrap_or_default();

        match self {
            AnchoredPositionMode::Window => {
                let anchor_position = anchor_position.unwrap_or(bounds.origin);
                let bounds =
                    Bounds::from_corner_and_size(anchor_corner, anchor_position + offset, size);
                (anchor_position, bounds)
            }
            AnchoredPositionMode::Local => {
                let anchor_position = anchor_position.unwrap_or_default();
                let bounds = Bounds::from_corner_and_size(
                    anchor_corner,
                    bounds.origin + anchor_position + offset,
                    size,
                );
                (anchor_position, bounds)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::{
        Bounds, Corner, Pixels, PlatformInput, Point, ScrollDelta, ScrollEvent, TestAppContext,
        Window, deferred, div, point, prelude::*, px, size,
    };

    struct AnchoredTestView {
        position: Point<Pixels>,
    }

    impl Render for AnchoredTestView {
        fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
            div().size_full().child(
                div()
                    .id("scroll-container")
                    .overflow_y_scroll()
                    .size_full()
                    .child(div().h(px(2000.)).w_full())
                    .child(
                        deferred(
                            super::anchored()
                                .snap_to_window()
                                .position(self.position)
                                .child(
                                    div()
                                        .id("menu")
                                        .debug_selector(|| "MENU".into())
                                        .w(px(200.))
                                        .h(px(300.)),
                                ),
                        )
                        .with_priority(1),
                    ),
            )
        }
    }

    fn draw_anchored_test_view(
        cx: &mut TestAppContext,
        position: Point<Pixels>,
    ) -> &mut crate::VisualTestContext {
        let (_, cx) = cx.add_window_view(|_, _| AnchoredTestView { position });
        cx.simulate_resize(size(px(800.), px(600.)));
        cx
    }

    fn menu_bounds(cx: &mut crate::VisualTestContext) -> crate::Bounds<Pixels> {
        cx.debug_bounds("MENU")
            .expect("MENU debug bounds not found")
    }

    #[gpui::test]
    fn test_anchored_position_without_scroll(cx: &mut TestAppContext) {
        let cx = draw_anchored_test_view(cx, point(px(100.), px(100.)));

        let menu_bounds = menu_bounds(cx);

        assert_eq!(menu_bounds.origin, point(px(100.), px(100.)));
        assert_eq!(menu_bounds.size, size(px(200.), px(300.)));
    }

    #[gpui::test]
    fn test_anchored_position_when_scrolled(cx: &mut TestAppContext) {
        let cx = draw_anchored_test_view(cx, point(px(100.), px(100.)));

        cx.update(|window, cx| {
            window.dispatch_event(
                PlatformInput::ScrollWheel(ScrollEvent {
                    position: point(px(400.), px(300.)),
                    delta: ScrollDelta::Pixels(point(px(0.), px(-1000.))),
                    ..Default::default()
                }),
                cx,
            );
        });
        cx.run_until_parked();

        let menu_bounds = menu_bounds(cx);

        assert_eq!(menu_bounds.origin, point(px(100.), px(100.)));
        assert_eq!(menu_bounds.size, size(px(200.), px(300.)));
    }

    #[gpui::test]
    fn test_anchored_snaps_to_window(cx: &mut TestAppContext) {
        let cx = draw_anchored_test_view(cx, point(px(100.), px(500.)));

        let menu_bounds = menu_bounds(cx);

        assert_eq!(menu_bounds.origin, point(px(100.), px(300.)));
        assert_eq!(menu_bounds.size, size(px(200.), px(300.)));
    }

    struct AttachedTestView {
        target: Bounds<Pixels>,
        corner: Corner,
        anchor: Corner,
    }

    impl Render for AttachedTestView {
        fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
            div().size_full().child(
                deferred(
                    super::anchored()
                        .attach(self.target, self.corner)
                        .anchor(self.anchor)
                        .offset(point(px(-2.), px(4.)))
                        .snap_to_window_with_margin(px(8.))
                        .child(
                            div()
                                .debug_selector(|| "MENU".into())
                                .w(px(200.))
                                .h(px(300.)),
                        ),
                )
                .with_priority(1),
            )
        }
    }

    #[gpui::test]
    fn test_anchored_attach_moves_to_the_side_with_room(cx: &mut TestAppContext) {
        let trigger = |x: f32, y: f32| Bounds::new(point(px(x), px(y)), size(px(100.), px(20.)));
        // A submenu sits to the right of its trigger, with their top edges lined up.
        let submenu = (Corner::TopRight, Corner::TopLeft);
        // A dropdown sits below its button, with their left edges lined up.
        let dropdown = (Corner::BottomLeft, Corner::TopLeft);
        // Upstream also covers a centered attachment (`Anchor::BottomCenter` /
        // `Anchor::TopCenter`), which can move above the button but never sideways.
        // `Corner` has no center variants, so that case is not representable here.
        for (target, (corner, anchor), expected_origin) in [
            (trigger(100., 100.), submenu, point(px(198.), px(104.))),
            (trigger(550., 100.), submenu, point(px(352.), px(104.))),
            (trigger(100., 400.), dropdown, point(px(98.), px(96.))),
            // Ends 4px from the window edge. That's inside the snap margin but still fits,
            // so it stays.
            (trigger(498., 100.), submenu, point(px(596.), px(104.))),
            // Doesn't fit on either side. The left sticks out less, so it goes left and
            // slides back into the window.
            (
                Bounds::new(point(px(180.), px(100.)), size(px(500.), px(20.))),
                submenu,
                point(px(8.), px(104.)),
            ),
            // Doesn't fit on either side, but the right sticks out less, so it stays right
            // and slides back into the window.
            (
                Bounds::new(point(px(50.), px(100.)), size(px(650.), px(20.))),
                submenu,
                point(px(592.), px(104.)),
            ),
            // A dropdown that runs off the right edge slides left. It never moves beside its
            // button.
            (trigger(700., 100.), dropdown, point(px(592.), px(124.))),
        ] {
            let (_, window_cx) = cx.add_window_view(|_, _| AttachedTestView {
                target,
                corner,
                anchor,
            });
            window_cx.simulate_resize(size(px(800.), px(600.)));

            let menu_bounds = window_cx
                .debug_bounds("MENU")
                .expect("MENU debug bounds not found");

            assert_eq!(
                menu_bounds.origin, expected_origin,
                "target: {target:?}, corner: {corner:?}, anchor: {anchor:?}"
            );
        }
    }
}

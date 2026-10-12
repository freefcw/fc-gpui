use crate::{Bounds, Pixels, PlatformDisplay, Point, Size, WindowPosition, point};
use std::rc::Rc;

/// Compute window bounds from a desired size and a semantic position.
pub fn compute_window_bounds(
    size: Size<Pixels>,
    position: &WindowPosition,
    displays: &[Rc<dyn PlatformDisplay>],
    primary_display: Option<&Rc<dyn PlatformDisplay>>,
) -> Bounds<Pixels> {
    match position {
        WindowPosition::Center => {
            if let Some(display) = primary_display {
                center_in(size, display.visible_bounds())
            } else {
                Bounds::new(Point::default(), size)
            }
        }
        WindowPosition::CenterOnDisplay(id) => {
            let display_bounds = displays
                .iter()
                .find(|d| d.id() == *id)
                .map(|d| d.visible_bounds());
            if let Some(bounds) = display_bounds {
                center_in(size, bounds)
            } else if let Some(display) = primary_display {
                center_in(size, display.visible_bounds())
            } else {
                Bounds::new(Point::default(), size)
            }
        }
        #[allow(deprecated)]
        WindowPosition::TrayCenter(tray_bounds) => {
            let x = tray_bounds.origin.x + (tray_bounds.size.width - size.width) * 0.5;
            let y = tray_bounds.origin.y + tray_bounds.size.height;
            Bounds::new(point(x, y), size)
        }
        WindowPosition::TrayAnchored(anchor) => {
            let tray_bounds = anchor.bounds;
            let x = tray_bounds.origin.x + (tray_bounds.size.width - size.width) * 0.5;
            let y = tray_bounds.origin.y + tray_bounds.size.height;
            Bounds::new(point(x, y), size)
        }
        WindowPosition::TopRight { margin } => {
            corner_position(size, primary_display, *margin, true, false)
        }
        WindowPosition::BottomRight { margin } => {
            corner_position(size, primary_display, *margin, true, true)
        }
        WindowPosition::TopLeft { margin } => {
            corner_position(size, primary_display, *margin, false, false)
        }
        WindowPosition::BottomLeft { margin } => {
            corner_position(size, primary_display, *margin, false, true)
        }
    }
}

fn center_in(size: Size<Pixels>, display: Bounds<Pixels>) -> Bounds<Pixels> {
    let x = display.origin.x + (display.size.width - size.width) * 0.5;
    let y = display.origin.y + (display.size.height - size.height) * 0.5;
    Bounds::new(point(x, y), size)
}

fn corner_position(
    size: Size<Pixels>,
    primary_display: Option<&Rc<dyn PlatformDisplay>>,
    margin: Pixels,
    right: bool,
    bottom: bool,
) -> Bounds<Pixels> {
    if let Some(display) = primary_display {
        let db = display.visible_bounds();
        let x = if right {
            db.origin.x + db.size.width - size.width - margin
        } else {
            db.origin.x + margin
        };
        let y = if bottom {
            db.origin.y + db.size.height - size.height - margin
        } else {
            db.origin.y + margin
        };
        Bounds::new(point(x, y), size)
    } else {
        Bounds::new(Point::default(), size)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{DisplayId, px, size};
    use anyhow::Result;
    use uuid::Uuid;

    #[derive(Debug)]
    struct FakeDisplay {
        id: DisplayId,
        bounds: Bounds<Pixels>,
        visible_bounds: Bounds<Pixels>,
    }

    impl PlatformDisplay for FakeDisplay {
        fn id(&self) -> DisplayId {
            self.id
        }

        fn uuid(&self) -> Result<Uuid> {
            Ok(Uuid::nil())
        }

        fn bounds(&self) -> Bounds<Pixels> {
            self.bounds
        }

        fn refresh_interval(&self) -> Option<std::time::Duration> {
            None
        }

        fn visible_bounds(&self) -> Bounds<Pixels> {
            self.visible_bounds
        }
    }

    fn display() -> Rc<dyn PlatformDisplay> {
        Rc::new(FakeDisplay {
            id: DisplayId::new(1),
            bounds: Bounds::new(point(px(0.), px(0.)), size(px(1000.), px(800.))),
            visible_bounds: Bounds::new(point(px(0.), px(40.)), size(px(1000.), px(700.))),
        })
    }

    #[test]
    fn centers_windows_in_visible_bounds() {
        let display = display();
        let bounds = compute_window_bounds(
            size(px(200.), px(100.)),
            &WindowPosition::Center,
            std::slice::from_ref(&display),
            Some(&display),
        );

        assert_eq!(
            bounds,
            Bounds::new(point(px(400.), px(340.)), size(px(200.), px(100.)))
        );
    }

    #[test]
    fn corners_windows_in_visible_bounds() {
        let display = display();
        let bounds = compute_window_bounds(
            size(px(200.), px(100.)),
            &WindowPosition::BottomRight { margin: px(10.) },
            std::slice::from_ref(&display),
            Some(&display),
        );

        assert_eq!(
            bounds,
            Bounds::new(point(px(790.), px(630.)), size(px(200.), px(100.)))
        );
    }
}

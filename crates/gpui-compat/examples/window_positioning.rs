use gpui::{
    App, Application, Bounds, Context, DisplayId, Hsla, Pixels, SharedString, Size, Window,
    WindowBackgroundAppearance, WindowBounds, WindowKind, WindowOptions, div, point, prelude::*,
    px, rgb,
};

#[cfg(all(target_os = "linux", feature = "wayland"))]
use gpui::layer_shell::{Anchor, KeyboardInteractivity, LayerShellOptions};

struct WindowContent {
    text: SharedString,
    bounds: Bounds<Pixels>,
    bg: Hsla,
}

impl Render for WindowContent {
    fn render(&mut self, window: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        let window_bounds = window.bounds();

        div()
            .flex()
            .flex_col()
            .bg(self.bg)
            .size_full()
            .items_center()
            .text_color(rgb(0xffffff))
            .child(self.text.clone())
            .child(
                div()
                    .flex()
                    .flex_col()
                    .text_sm()
                    .items_center()
                    .size_full()
                    .child(format!(
                        "origin: {}, {} size: {}, {}",
                        self.bounds.origin.x,
                        self.bounds.origin.y,
                        self.bounds.size.width,
                        self.bounds.size.height
                    ))
                    .child(format!(
                        "cx.bounds() origin: {}, {} size {}, {}",
                        window_bounds.origin.x,
                        window_bounds.origin.y,
                        window_bounds.size.width,
                        window_bounds.size.height
                    )),
            )
    }
}

fn build_window_options(
    display_id: DisplayId,
    display_bounds: Bounds<Pixels>,
    bounds: Bounds<Pixels>,
) -> WindowOptions {
    #[cfg(all(target_os = "linux", feature = "wayland"))]
    let kind = window_kind_for_compositor(gpui::guess_compositor(), display_bounds, bounds);
    #[cfg(not(all(target_os = "linux", feature = "wayland")))]
    let kind = {
        let _ = display_bounds;
        WindowKind::PopUp
    };

    WindowOptions {
        // Set the bounds of the window in screen coordinates
        window_bounds: Some(WindowBounds::Windowed(bounds)),
        // Specify the display_id to ensure the window is created on the correct screen
        display_id: Some(display_id),
        titlebar: None,
        window_background: WindowBackgroundAppearance::Transparent,
        focus: false,
        show: true,
        kind,
        is_movable: false,
        app_id: None,
        window_min_size: None,
        window_decorations: None,
        tabbing_identifier: None,
        ..Default::default()
    }
}

#[cfg(all(target_os = "linux", feature = "wayland"))]
fn window_kind_for_compositor(
    compositor: &str,
    display_bounds: Bounds<Pixels>,
    bounds: Bounds<Pixels>,
) -> WindowKind {
    if compositor == "Wayland" {
        WindowKind::LayerShell(layer_shell_options(display_bounds, bounds))
    } else {
        WindowKind::PopUp
    }
}

#[cfg(all(target_os = "linux", feature = "wayland"))]
fn layer_shell_options(display: Bounds<Pixels>, bounds: Bounds<Pixels>) -> LayerShellOptions {
    let center = bounds.center();
    let display_center = display.center();
    let top = center.y < display_center.y;
    let left = center.x < display_center.x;
    let anchor = (if top { Anchor::TOP } else { Anchor::BOTTOM })
        | if left { Anchor::LEFT } else { Anchor::RIGHT };

    let margin_top = if top {
        bounds.origin.y - display.origin.y
    } else {
        px(0.0)
    };
    let margin_right = if left {
        px(0.0)
    } else {
        display.right() - bounds.right()
    };
    let margin_bottom = if top {
        px(0.0)
    } else {
        display.bottom() - bounds.bottom()
    };
    let margin_left = if left {
        bounds.origin.x - display.origin.x
    } else {
        px(0.0)
    };

    LayerShellOptions {
        namespace: "gpui-window-positioning".to_string(),
        anchor,
        margin: Some((margin_top, margin_right, margin_bottom, margin_left)),
        keyboard_interactivity: KeyboardInteractivity::None,
        ..Default::default()
    }
}

#[cfg(all(test, target_os = "linux", feature = "wayland"))]
mod tests {
    use super::*;

    fn test_bounds() -> (Bounds<Pixels>, Bounds<Pixels>) {
        let display = Bounds::new(point(px(0.0), px(0.0)), Size::new(px(1920.0), px(1080.0)));
        let window = Bounds::new(
            point(px(1520.0), px(820.0)),
            Size::new(px(320.0), px(220.0)),
        );
        (display, window)
    }

    #[test]
    fn wayland_compositor_uses_layer_shell() {
        let (display, window) = test_bounds();

        assert!(matches!(
            window_kind_for_compositor("Wayland", display, window),
            WindowKind::LayerShell(_)
        ));
    }

    #[test]
    fn x11_compositor_uses_popup_window() {
        let (display, window) = test_bounds();

        assert_eq!(
            window_kind_for_compositor("X11", display, window),
            WindowKind::PopUp
        );
    }
}

fn main() {
    Application::new().run(|cx: &mut App| {
        // Create several new windows, positioned in the top right corner of each screen
        let size = Size {
            width: px(350.),
            height: px(75.),
        };
        let margin_offset = px(150.);

        for screen in cx.displays() {
            let display_bounds = screen.bounds();
            let bounds = Bounds {
                origin: display_bounds.origin + point(margin_offset, margin_offset),
                size,
            };

            cx.open_window(
                build_window_options(screen.id(), display_bounds, bounds),
                |_, cx| {
                    cx.new(|_| WindowContent {
                        text: format!("Top Left {:?}", screen.id()).into(),
                        bg: gpui::red(),
                        bounds,
                    })
                },
            )
            .unwrap();

            let bounds = Bounds {
                origin: display_bounds.top_right()
                    - point(size.width + margin_offset, -margin_offset),
                size,
            };

            cx.open_window(
                build_window_options(screen.id(), display_bounds, bounds),
                |_, cx| {
                    cx.new(|_| WindowContent {
                        text: format!("Top Right {:?}", screen.id()).into(),
                        bg: gpui::red(),
                        bounds,
                    })
                },
            )
            .unwrap();

            let bounds = Bounds {
                origin: display_bounds.bottom_left()
                    - point(-margin_offset, size.height + margin_offset),
                size,
            };

            cx.open_window(
                build_window_options(screen.id(), display_bounds, bounds),
                |_, cx| {
                    cx.new(|_| WindowContent {
                        text: format!("Bottom Left {:?}", screen.id()).into(),
                        bg: gpui::blue(),
                        bounds,
                    })
                },
            )
            .unwrap();

            let bounds = Bounds {
                origin: display_bounds.bottom_right()
                    - point(size.width + margin_offset, size.height + margin_offset),
                size,
            };

            cx.open_window(
                build_window_options(screen.id(), display_bounds, bounds),
                |_, cx| {
                    cx.new(|_| WindowContent {
                        text: format!("Bottom Right {:?}", screen.id()).into(),
                        bg: gpui::blue(),
                        bounds,
                    })
                },
            )
            .unwrap();

            let bounds = Bounds {
                origin: point(
                    display_bounds.center().x - size.center().x,
                    display_bounds.top() + margin_offset,
                ),
                size,
            };

            cx.open_window(
                build_window_options(screen.id(), display_bounds, bounds),
                |_, cx| {
                    cx.new(|_| WindowContent {
                        text: format!("Top Center {:?}", screen.id()).into(),
                        bg: gpui::black(),
                        bounds,
                    })
                },
            )
            .unwrap();

            let bounds = Bounds {
                origin: point(
                    display_bounds.left() + margin_offset,
                    display_bounds.center().y - size.center().y,
                ),
                size,
            };

            cx.open_window(
                build_window_options(screen.id(), display_bounds, bounds),
                |_, cx| {
                    cx.new(|_| WindowContent {
                        text: format!("Left Center {:?}", screen.id()).into(),
                        bg: gpui::black(),
                        bounds,
                    })
                },
            )
            .unwrap();

            let bounds = Bounds {
                origin: point(
                    display_bounds.center().x - size.center().x,
                    display_bounds.center().y - size.center().y,
                ),
                size,
            };

            cx.open_window(
                build_window_options(screen.id(), display_bounds, bounds),
                |_, cx| {
                    cx.new(|_| WindowContent {
                        text: format!("Center {:?}", screen.id()).into(),
                        bg: gpui::black(),
                        bounds,
                    })
                },
            )
            .unwrap();

            let bounds = Bounds {
                origin: point(
                    display_bounds.right() - size.width - margin_offset,
                    display_bounds.center().y - size.center().y,
                ),
                size,
            };

            cx.open_window(
                build_window_options(screen.id(), display_bounds, bounds),
                |_, cx| {
                    cx.new(|_| WindowContent {
                        text: format!("Right Center {:?}", screen.id()).into(),
                        bg: gpui::black(),
                        bounds,
                    })
                },
            )
            .unwrap();

            let bounds = Bounds {
                origin: point(
                    display_bounds.center().x - size.center().x,
                    display_bounds.bottom() - size.height - margin_offset,
                ),
                size,
            };

            cx.open_window(
                build_window_options(screen.id(), display_bounds, bounds),
                |_, cx| {
                    cx.new(|_| WindowContent {
                        text: format!("Bottom Center {:?}", screen.id()).into(),
                        bg: gpui::black(),
                        bounds,
                    })
                },
            )
            .unwrap();
        }
    });
}

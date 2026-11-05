#[cfg(not(any(
    target_os = "macos",
    target_os = "linux",
    target_os = "freebsd",
    target_os = "windows"
)))]
fn main() {
    if real_visual_required() {
        eprintln!("real_visual_smoke is required but unsupported on this platform");
        std::process::exit(1);
    }
    println!("real_visual_smoke is not supported on this platform; skipping");
}

fn real_visual_required() -> bool {
    std::env::var("GPUI_REQUIRE_REAL_VISUAL").is_ok_and(|value| value == "1")
}

#[cfg(any(
    target_os = "macos",
    target_os = "linux",
    target_os = "freebsd",
    target_os = "windows"
))]
fn main() {
    if !std::env::args().any(|arg| arg == "--ignored" || arg == "--include-ignored") {
        println!("real_visual_smoke is ignored by default; pass `-- --ignored` to run it");
        return;
    }

    if let Err(err) = run() {
        eprintln!("real_visual_smoke failed: {err:?}");
        std::process::exit(1);
    }
}

#[cfg(any(
    target_os = "macos",
    target_os = "linux",
    target_os = "freebsd",
    target_os = "windows"
))]
fn run() -> anyhow::Result<()> {
    use gpui::{
        AppContext as _, Context, IntoElement, RealVisualTestContext, Render, Styled as _,
        VisualTestCapabilities, Window, div, px, red, size,
    };
    use std::{cell::RefCell, rc::Rc};

    // xvfb + wgpu can hang in adapter/present, and Linux quit-during-launch used
    // to block forever in calloop. Fail required visual smoke in seconds, not 45m.
    // macOS/Windows keep the unbounded run so their assertions are unchanged.
    #[cfg(any(target_os = "linux", target_os = "freebsd"))]
    let _watchdog = LinuxRealVisualWatchdog::arm(std::time::Duration::from_secs(90));

    struct PaintedView;

    impl Render for PaintedView {
        fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
            div().w(px(20.0)).h(px(20.0)).bg(red())
        }
    }

    let capabilities = VisualTestCapabilities::detect();
    let Some(cx) = RealVisualTestContext::new_if_supported() else {
        anyhow::ensure!(
            !real_visual_required(),
            "real visual renderer is required but unavailable"
        );
        println!("real visual renderer is not available; skipping");
        return Ok(());
    };
    let outcome = Rc::new(RefCell::new(None));
    let outcome_in_run = outcome.clone();

    cx.run(move |cx| {
        let result = (|| -> anyhow::Result<()> {
            let requested_size = size(px(64.0), px(64.0));
            let window =
                cx.open_offscreen_window(requested_size, |_, app| app.new(|_| PaintedView))?;
            let window = window.into();
            cx.update_window(window, |_, window, _| -> anyhow::Result<()> {
                use raw_window_handle::{
                    HasDisplayHandle, HasWindowHandle, RawDisplayHandle, RawWindowHandle,
                };

                let window_handle = HasWindowHandle::window_handle(window)
                    .map_err(|error| anyhow::anyhow!("failed to get native window handle: {error}"))?
                    .as_raw();
                let display_handle = HasDisplayHandle::display_handle(window)
                    .map_err(|error| anyhow::anyhow!("failed to get native display handle: {error}"))?
                    .as_raw();

                let matches_platform = match (window_handle, display_handle) {
                    (RawWindowHandle::Win32(_), RawDisplayHandle::Windows(_)) => {
                        cfg!(target_os = "windows")
                    }
                    (RawWindowHandle::AppKit(_), RawDisplayHandle::AppKit(_)) => {
                        cfg!(target_os = "macos")
                    }
                    (RawWindowHandle::Xcb(_), RawDisplayHandle::Xcb(_))
                    | (RawWindowHandle::Wayland(_), RawDisplayHandle::Wayland(_)) => {
                        cfg!(any(target_os = "linux", target_os = "freebsd"))
                    }
                    _ => false,
                };
                anyhow::ensure!(
                    matches_platform,
                    "unexpected native handles: window={window_handle:?}, display={display_handle:?}"
                );
                Ok(())
            })??;
            let (bounds, scale_factor, artifact) = cx.update_window(window, |_, window, app| {
                let clear = window.draw(app);
                let artifact = window.visual_render_artifact();
                window.request_attention();
                window.present_for_visual_test();
                clear.clear();
                (window.bounds(), window.scale_factor(), artifact)
            })?;
            anyhow::ensure!(
                artifact.quads > 0,
                "expected rendered scene to contain a quad, got {:?}",
                artifact
            );
            let image = cx.capture_screenshot(window)?;
            let expected_origin = gpui::point(px(-10000.0), px(-10000.0));
            let expected_width = (f32::from(bounds.size.width) * scale_factor).round() as u32;
            let expected_height = (f32::from(bounds.size.height) * scale_factor).round() as u32;

            if capabilities.offscreen_positioned_window {
                anyhow::ensure!(
                    bounds.origin == expected_origin,
                    "expected origin {:?}, got {:?}",
                    expected_origin,
                    bounds.origin
                );
            }
            anyhow::ensure!(
                bounds.size.width >= requested_size.width
                    && bounds.size.height >= requested_size.height,
                "expected size at least {:?}, got {:?}",
                requested_size,
                bounds.size
            );
            anyhow::ensure!(
                image.width() == expected_width,
                "expected image width {}, got {}",
                expected_width,
                image.width()
            );
            anyhow::ensure!(
                image.height() == expected_height,
                "expected image height {}, got {}",
                expected_height,
                image.height()
            );
            let Some(first_pixel) = image.pixels().next().copied() else {
                anyhow::bail!("screenshot is empty");
            };
            let mut channel_min = [u8::MAX; 4];
            let mut channel_max = [u8::MIN; 4];
            let mut nonzero_rgb_pixels = 0usize;
            let mut nonzero_alpha_pixels = 0usize;
            let mut opaque_red_pixels = 0usize;
            let mut transparent_pixels = 0usize;
            let mut different_pixels = 0usize;
            for pixel in image.pixels() {
                for channel in 0..4 {
                    channel_min[channel] = channel_min[channel].min(pixel[channel]);
                    channel_max[channel] = channel_max[channel].max(pixel[channel]);
                }
                nonzero_rgb_pixels +=
                    (pixel[0] > 0 || pixel[1] > 0 || pixel[2] > 0) as usize;
                nonzero_alpha_pixels += (pixel[3] > 0) as usize;
                opaque_red_pixels += (pixel[3] == u8::MAX
                    && pixel[0] > pixel[1]
                    && pixel[0] > pixel[2]) as usize;
                transparent_pixels += (pixel[3] == 0) as usize;
                different_pixels += (*pixel != first_pixel) as usize;
            }
            anyhow::ensure!(
                nonzero_alpha_pixels > 0,
                "screenshot is fully transparent: channel_min={:?}, channel_max={:?}, nonzero_rgb_pixels={}",
                channel_min,
                channel_max,
                nonzero_rgb_pixels
            );
            anyhow::ensure!(
                opaque_red_pixels > 0,
                "screenshot does not contain the expected opaque red content: channel_min={:?}, channel_max={:?}, nonzero_alpha_pixels={}",
                channel_min,
                channel_max,
                nonzero_alpha_pixels
            );
            anyhow::ensure!(
                transparent_pixels > 0,
                "screenshot does not contain the expected transparent background: channel_min={:?}, channel_max={:?}, opaque_red_pixels={}",
                channel_min,
                channel_max,
                opaque_red_pixels
            );
            anyhow::ensure!(
                different_pixels > 0,
                "screenshot is a solid color: pixel={:?}, channel_min={:?}, channel_max={:?}, nonzero_alpha_pixels={}",
                first_pixel,
                channel_min,
                channel_max,
                nonzero_alpha_pixels
            );
            run_layer_shell_smoke(cx)?;
            Ok(())
        })();
        *outcome_in_run.borrow_mut() = Some(result);
        cx.quit();
    });

    outcome.borrow_mut().take().unwrap_or_else(|| {
        Err(anyhow::anyhow!(
            "real visual smoke did not report an outcome"
        ))
    })
}

#[cfg(all(any(target_os = "linux", target_os = "freebsd"), feature = "wayland"))]
fn run_layer_shell_smoke(cx: &mut gpui::RealVisualTestContext) -> anyhow::Result<()> {
    if std::env::var_os("GPUI_TEST_LAYER_SHELL").is_none() {
        return Ok(());
    }

    use gpui::{
        AppContext as _, Context, IntoElement, Render, Styled as _, Window, WindowBounds,
        WindowKind, WindowOptions, div,
        layer_shell::{Anchor, LayerShellOptions},
        px, red, size,
    };

    struct LayerShellView;

    impl Render for LayerShellView {
        fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
            div().size_full().bg(red())
        }
    }

    let bounds = gpui::Bounds::new(gpui::Point::default(), size(px(64.0), px(64.0)));
    let window = cx.app.borrow_mut().open_window(
        WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(bounds)),
            focus: false,
            show: true,
            kind: WindowKind::LayerShell(LayerShellOptions {
                namespace: "gpui-real-visual-smoke".to_string(),
                anchor: Anchor::TOP | Anchor::LEFT,
                ..Default::default()
            }),
            ..Default::default()
        },
        |_, app| app.new(|_| LayerShellView),
    )?;
    let window = window.into();
    cx.update_window(window, |_, window, app| {
        window.set_exclusive_zone(px(8.0));
        window.set_exclusive_edge(Anchor::TOP);
        let clear = window.draw(app);
        window.present_for_visual_test();
        clear.clear();
    })?;
    let image = cx.capture_screenshot(window)?;
    anyhow::ensure!(image.width() > 0 && image.height() > 0);
    anyhow::ensure!(image.pixels().any(|pixel| pixel[3] > 0));
    Ok(())
}

#[cfg(not(all(any(target_os = "linux", target_os = "freebsd"), feature = "wayland")))]
fn run_layer_shell_smoke(_cx: &mut gpui::RealVisualTestContext) -> anyhow::Result<()> {
    Ok(())
}

#[cfg(any(target_os = "linux", target_os = "freebsd"))]
struct LinuxRealVisualWatchdog {
    cancel: std::sync::Arc<std::sync::atomic::AtomicBool>,
}

#[cfg(any(target_os = "linux", target_os = "freebsd"))]
impl LinuxRealVisualWatchdog {
    fn arm(timeout: std::time::Duration) -> Self {
        use std::sync::Arc;
        use std::sync::atomic::{AtomicBool, Ordering};
        use std::time::Instant;

        let cancel = Arc::new(AtomicBool::new(false));
        let cancel_for_thread = cancel.clone();
        std::thread::Builder::new()
            .name("real-visual-watchdog".into())
            .spawn(move || {
                let deadline = Instant::now() + timeout;
                while Instant::now() < deadline {
                    if cancel_for_thread.load(Ordering::Relaxed) {
                        return;
                    }
                    std::thread::sleep(std::time::Duration::from_millis(100));
                }
                if !cancel_for_thread.load(Ordering::Relaxed) {
                    eprintln!(
                        "real_visual_smoke timed out after {}s waiting for the Linux renderer (GPUI_REQUIRE_REAL_VISUAL=1)",
                        timeout.as_secs()
                    );
                    std::process::exit(1);
                }
            })
            .expect("start real visual watchdog");
        Self { cancel }
    }
}

#[cfg(any(target_os = "linux", target_os = "freebsd"))]
impl Drop for LinuxRealVisualWatchdog {
    fn drop(&mut self) {
        self.cancel
            .store(true, std::sync::atomic::Ordering::Relaxed);
    }
}

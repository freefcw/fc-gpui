#![doc = include_str!("../README.md")]
#![cfg(target_os = "macos")]
#![allow(unused_mut)] // False positives in platform-specific state handling

mod mac;

pub(crate) use gpui::*;
pub(crate) use mac::*;

use std::rc::Rc;

/// Returns the default macOS platform implementation for this process.
pub fn current_platform(headless: bool) -> Rc<dyn gpui::Platform> {
    Rc::new(mac::MacPlatform::new(headless))
}

#[cfg(all(test, feature = "font-kit"))]
mod tests {
    use super::{MacTextSystem, current_platform};
    use gpui::{TestApp, TextRun, VisualTestCapabilities, WindowTextSystem, black, font, px};
    use std::sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    };
    use std::time::Duration;

    #[test]
    fn headless_mac_text_layout_produces_nonzero_metrics() {
        let app = TestApp::with_platform_text_system(Arc::new(MacTextSystem::new()));
        let text_system = app.text_system();
        let family = text_system
            .all_font_names()
            .into_iter()
            .find(|name| !name.starts_with('.'))
            .unwrap_or_else(|| ".SystemUIFont".to_string());
        let window_text_system = WindowTextSystem::new(text_system.clone());
        let text = "Headless text";
        let layout = window_text_system.layout_line(
            text,
            px(16.),
            &[TextRun {
                len: text.len(),
                font: font(family),
                color: black(),
                background_color: None,
                underline: None,
                strikethrough: None,
            }],
            None,
        );

        assert!(layout.width > px(0.));
        assert!(layout.ascent > px(0.));
        assert!(layout.runs.iter().any(|run| !run.glyphs.is_empty()));
    }

    #[test]
    #[ignore]
    fn real_visual_context_advance_clock_works() {
        let capabilities = VisualTestCapabilities::detect();
        if !capabilities.real_renderer || !capabilities.screenshot_capture {
            eprintln!("skipping: real visual renderer is not available");
            return;
        }
        let cx = gpui::RealVisualTestContext::with_platform(current_platform(false));
        let completed = Arc::new(AtomicBool::new(false));
        let timer = cx.executor().timer(Duration::from_secs(1));
        let completed_task = completed.clone();
        cx.spawn(async move {
            timer.await;
            completed_task.store(true, Ordering::SeqCst);
        })
        .detach();

        cx.run_until_parked();
        assert!(!completed.load(Ordering::SeqCst));

        cx.advance_clock(Duration::from_secs(1));
        cx.run_until_parked();
        assert!(completed.load(Ordering::SeqCst));
    }
}

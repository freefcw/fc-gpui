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
    fn background_color_boundaries_shape_consistently() {
        use gpui::{FontRun, LineLayout, PlatformTextSystem};

        let platform = Arc::new(MacTextSystem::new());
        // Separate TextSystems prevent one path from reusing the other's global
        // layout cache. Both calls must actually reach CoreText.
        let line_app = TestApp::with_platform_text_system(platform.clone());
        let text_app = TestApp::with_platform_text_system(platform.clone());
        let line_shaper = WindowTextSystem::new(line_app.text_system().clone());
        let text_shaper = WindowTextSystem::new(text_app.text_system().clone());
        let family = platform
            .all_font_names()
            .into_iter()
            .find(|name| !name.starts_with('.'))
            .unwrap_or_else(|| ".SystemUIFont".to_string());
        let font = font(family);
        let font_id = platform.font_id(&font).unwrap();

        let glyphs = |layout: &LineLayout| {
            layout
                .runs
                .iter()
                .flat_map(|run| {
                    run.glyphs.iter().map(move |glyph| {
                        (
                            run.font_id,
                            glyph.id,
                            glyph.index,
                            glyph.position,
                            glyph.is_emoji,
                        )
                    })
                })
                .collect::<Vec<_>>()
        };

        for (text, split) in [
            ("fi", 1),
            ("سلام", "س".len()),
            ("e\u{301}x", "e\u{301}".len()),
            ("e\u{301}x", 1),
            ("中文🙂", "中".len()),
        ] {
            let plain = TextRun {
                len: split,
                font: font.clone(),
                color: black(),
                background_color: None,
                underline: None,
                strikethrough: None,
            };
            let mut highlighted = plain.clone();
            highlighted.len = text.len() - split;
            highlighted.background_color = Some(black());
            let runs = [plain, highlighted];

            // Preserve shape_text's existing policy: decoration boundaries are
            // shaping boundaries, not a promise of identical unstyled glyphs.
            let expected = platform.layout_line(
                text,
                px(16.),
                &[
                    FontRun {
                        font_id,
                        len: split,
                    },
                    FontRun {
                        font_id,
                        len: text.len() - split,
                    },
                ],
            );
            let line = line_shaper.shape_line(text.into(), px(16.), &runs, None);
            let wrapped = text_shaper
                .shape_text(text.into(), px(16.), &runs, None, None)
                .unwrap();
            assert_eq!(wrapped.len(), 1);
            for layout in [&**line, wrapped[0].unwrapped_layout.as_ref()] {
                assert_eq!(layout.len, text.len());
                assert_eq!(layout.width, expected.width, "{text:?}");
                assert_eq!(layout.ascent, expected.ascent);
                assert_eq!(layout.descent, expected.descent);
                assert!(layout.width > px(0.));
                let actual = glyphs(layout);
                assert!(!actual.is_empty());
                assert_eq!(actual, glyphs(&expected), "{text:?} split at {split}");
                assert!(actual.iter().all(|(_, _, index, _, _)| {
                    *index < text.len() && text.is_char_boundary(*index)
                }));
            }
        }
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

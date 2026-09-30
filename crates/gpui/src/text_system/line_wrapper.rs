use crate::{FontId, FontRun, Pixels, PlatformTextSystem, SharedString, TextRun, px};
use collections::HashMap;
use std::{iter, sync::Arc};

/// Controls how soft-wrapped continuation lines are indented.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum IndentAdjustment {
    /// No indent - continuation lines start at column 0.
    NoIndent,
    /// Match the original line's leading whitespace.
    #[default]
    SameIndent,
    /// Add N extra columns of indent (in space-character widths).
    ExtraColumns(u32),
}

/// The GPUI line wrapper, used to wrap lines of text to a given width.
pub struct LineWrapper {
    platform_text_system: Arc<dyn PlatformTextSystem>,
    pub(crate) font_id: FontId,
    pub(crate) font_size: Pixels,
    cached_ascii_char_widths: [Option<Pixels>; 128],
    cached_other_char_widths: HashMap<char, Pixels>,
}

impl LineWrapper {
    /// The maximum indent that can be applied to a line.
    pub const MAX_INDENT: u32 = 256;

    pub(crate) fn new(
        font_id: FontId,
        font_size: Pixels,
        text_system: Arc<dyn PlatformTextSystem>,
    ) -> Self {
        Self {
            platform_text_system: text_system,
            font_id,
            font_size,
            cached_ascii_char_widths: [None; 128],
            cached_other_char_widths: HashMap::default(),
        }
    }

    /// Wrap a line of text to the given width with this wrapper's font and font size.
    ///
    /// `indent_adjustment` controls the indent applied to each continuation row.
    pub fn wrap_line<'a>(
        &'a mut self,
        fragments: &'a [LineFragment],
        wrap_width: Pixels,
        indent_adjustment: IndentAdjustment,
    ) -> impl Iterator<Item = Boundary> + 'a {
        let mut width = px(0.);
        let mut first_non_whitespace_ix = None;
        let mut base_indent = None;
        let mut last_candidate_ix = 0;
        let mut last_candidate_width = px(0.);
        let mut last_wrap_ix = 0;
        let mut prev_c = '\0';
        let mut index = 0;
        let mut candidates = fragments
            .iter()
            .flat_map(move |fragment| fragment.wrap_boundary_candidates())
            .peekable();
        iter::from_fn(move || {
            for candidate in candidates.by_ref() {
                let ix = index;
                index += candidate.len_utf8();
                let mut new_prev_c = prev_c;
                let item_width = match candidate {
                    WrapBoundaryCandidate::Char { character: c } => {
                        if c == '\n' {
                            continue;
                        }

                        if Self::is_word_char(c) {
                            if prev_c == ' ' && c != ' ' && first_non_whitespace_ix.is_some() {
                                last_candidate_ix = ix;
                                last_candidate_width = width;
                            }
                        } else {
                            // CJK may not be space separated, e.g.: `Hello world你好世界`
                            if c != ' ' && first_non_whitespace_ix.is_some() {
                                last_candidate_ix = ix;
                                last_candidate_width = width;
                            }
                        }

                        if c != ' ' && first_non_whitespace_ix.is_none() {
                            first_non_whitespace_ix = Some(ix);
                        }

                        new_prev_c = c;

                        self.width_for_char(c)
                    }
                    WrapBoundaryCandidate::Element {
                        width: element_width,
                        ..
                    } => {
                        if prev_c == ' ' && first_non_whitespace_ix.is_some() {
                            last_candidate_ix = ix;
                            last_candidate_width = width;
                        }

                        if first_non_whitespace_ix.is_none() {
                            first_non_whitespace_ix = Some(ix);
                        }

                        element_width
                    }
                };

                width += item_width;
                if width > wrap_width && ix > last_wrap_ix {
                    let wrap_at_candidate =
                        last_candidate_ix > 0 && width - last_candidate_width <= wrap_width;

                    let carried_width = if wrap_at_candidate {
                        width - last_candidate_width
                    } else {
                        item_width
                    };

                    // Compute base indentation from the first non-whitespace character on the
                    // line and retain it for all subsequent wrap rows. If the line begins with
                    // leading whitespace that wraps before any non-whitespace character (or is
                    // all whitespace), base_indent remains None so continuation rows within the
                    // leading whitespace do not receive ExtraColumns indentation.
                    if base_indent.is_none()
                        && let Some(first_non_whitespace_ix) = first_non_whitespace_ix
                    {
                        base_indent = Some(
                            Self::MAX_INDENT
                                .min(first_non_whitespace_ix.saturating_sub(last_wrap_ix) as u32),
                        );
                    }

                    let next_indent = match indent_adjustment {
                        IndentAdjustment::NoIndent => 0,
                        IndentAdjustment::SameIndent => base_indent.unwrap_or(0),
                        IndentAdjustment::ExtraColumns(extra) => {
                            if let Some(base_indent) = base_indent {
                                let candidate = base_indent.saturating_add(extra);
                                let candidate_indent_width =
                                    self.width_for_char(' ') * candidate as f32;
                                // Reserve headroom for any carried suffix from an earlier word
                                // boundary (and at least 2 columns for a full-width character)
                                // so the continuation line does not immediately exceed wrap width.
                                let min_headroom =
                                    carried_width.max(self.width_for_char(' ') * 2.0);
                                if candidate_indent_width + min_headroom > wrap_width {
                                    0
                                } else {
                                    Self::MAX_INDENT.min(candidate)
                                }
                            } else {
                                0
                            }
                        }
                    };

                    if wrap_at_candidate {
                        last_wrap_ix = last_candidate_ix;
                        width -= last_candidate_width;
                    } else {
                        last_wrap_ix = ix;
                        width = item_width;
                    }
                    last_candidate_ix = 0;

                    width += self.width_for_char(' ') * next_indent as f32;

                    return Some(Boundary::new(last_wrap_ix, next_indent));
                }

                prev_c = new_prev_c;
            }

            None
        })
    }

    /// Truncate a line of text to the given width with this wrapper's font and font size.
    pub fn truncate_line(
        &mut self,
        line: SharedString,
        truncate_width: Pixels,
        truncation_suffix: &str,
        runs: &mut Vec<TextRun>,
    ) -> SharedString {
        let mut width = px(0.);
        let suffix_width = truncation_suffix
            .chars()
            .map(|c| self.width_for_char(c))
            .fold(px(0.0), |a, x| a + x);
        let mut truncate_ix = 0;
        for (ix, c) in line.char_indices() {
            if width + suffix_width < truncate_width {
                truncate_ix = ix;
            }

            let char_width = self.width_for_char(c);
            width += char_width;

            if width.floor() > truncate_width {
                let truncated = line[..truncate_ix]
                    .trim_end_matches(|c: char| c.is_whitespace() || c.is_ascii_punctuation());
                let result = SharedString::from(format!("{}{}", truncated, truncation_suffix));
                update_runs_after_truncation(&result, truncation_suffix, runs);

                return result;
            }
        }

        line
    }

    pub(crate) fn is_word_char(c: char) -> bool {
        // ASCII alphanumeric characters, for English, numbers: `Hello123`, etc.
        c.is_ascii_alphanumeric() ||
        // Latin script in Unicode for French, German, Spanish, etc.
        // Latin-1 Supplement
        // https://en.wikipedia.org/wiki/Latin-1_Supplement
        matches!(c, '\u{00C0}'..='\u{00FF}') ||
        // Latin Extended-A
        // https://en.wikipedia.org/wiki/Latin_Extended-A
        matches!(c, '\u{0100}'..='\u{017F}') ||
        // Latin Extended-B
        // https://en.wikipedia.org/wiki/Latin_Extended-B
        matches!(c, '\u{0180}'..='\u{024F}') ||
        // Cyrillic for Russian, Ukrainian, etc.
        // https://en.wikipedia.org/wiki/Cyrillic_script_in_Unicode
        matches!(c, '\u{0400}'..='\u{04FF}') ||
        // Some other known special characters that should be treated as word characters,
        // e.g. `a-b`, `var_name`, `I'm`/`won't`, '@mention`, `#hashtag`, `100%`, `3.1415`, `2^3`, `a~b`, etc.
        matches!(c, '-' | '_' | '.' | '\'' | '’' | '‘' | '$' | '%' | '@' | '#' | '^' | '~' | ',' | '!' | ';' | '*') ||
        // Characters that used in URL, e.g. `https://github.com/zed-industries/zed?a=1&b=2` for better wrapping a long URL.
        matches!(c,  '/' | ':' | '?' | '&' | '=') ||
        // Closing punctuation never starts a line (UAX #14 LB13: no break
        // before `!`, `)`, `]`, `}`, closing quotes or an ellipsis) — `plz!`,
        // `see)`, `quoted”` wrap as one word instead of orphaning the mark on
        // the next line. `/` and `?` stay local URL-glue characters so long
        // paths and query strings (`a/b`, `foo?b=2`) still wrap as one token.
        matches!(c, ')' | ']' | '}' | '"' | '”' | '»' | '…') ||
        // `⋯` character is special used in Zed, to keep this at the end of the line.
        matches!(c, '⋯') ||
        // Non-breaking glue characters.
        matches!(c, '\u{202F}' | '\u{00A0}' | '\u{2011}')
    }

    #[inline(always)]
    fn width_for_char(&mut self, c: char) -> Pixels {
        if (c as u32) < 128 {
            if let Some(cached_width) = self.cached_ascii_char_widths[c as usize] {
                cached_width
            } else {
                let width = self.compute_width_for_char(c);
                self.cached_ascii_char_widths[c as usize] = Some(width);
                width
            }
        } else if let Some(cached_width) = self.cached_other_char_widths.get(&c) {
            *cached_width
        } else {
            let width = self.compute_width_for_char(c);
            self.cached_other_char_widths.insert(c, width);
            width
        }
    }

    fn compute_width_for_char(&self, c: char) -> Pixels {
        let mut buffer = [0; 4];
        let buffer = c.encode_utf8(&mut buffer);
        self.platform_text_system
            .layout_line(
                buffer,
                self.font_size,
                &[FontRun {
                    len: buffer.len(),
                    font_id: self.font_id,
                }],
            )
            .width
    }
}

fn update_runs_after_truncation(result: &str, ellipsis: &str, runs: &mut Vec<TextRun>) {
    let mut truncate_at = result.len() - ellipsis.len();
    let mut run_end = None;
    for (run_index, run) in runs.iter_mut().enumerate() {
        if run.len <= truncate_at {
            truncate_at -= run.len;
        } else {
            run.len = truncate_at + ellipsis.len();
            run_end = Some(run_index + 1);
            break;
        }
    }
    if let Some(run_end) = run_end {
        runs.truncate(run_end);
    }
}

/// A fragment of a line that can be wrapped.
pub enum LineFragment<'a> {
    /// A text fragment consisting of characters.
    Text {
        /// The text content of the fragment.
        text: &'a str,
    },
    /// A non-text element with a fixed width.
    Element {
        /// The width of the element in pixels.
        width: Pixels,
        /// The UTF-8 encoded length of the element.
        len_utf8: usize,
    },
}

impl<'a> LineFragment<'a> {
    /// Creates a new text fragment from the given text.
    pub fn text(text: &'a str) -> Self {
        LineFragment::Text { text }
    }

    /// Creates a new non-text element with the given width and UTF-8 encoded length.
    pub fn element(width: Pixels, len_utf8: usize) -> Self {
        LineFragment::Element { width, len_utf8 }
    }

    fn wrap_boundary_candidates(&self) -> impl Iterator<Item = WrapBoundaryCandidate> {
        let text = match self {
            LineFragment::Text { text } => text,
            LineFragment::Element { .. } => "\0",
        };
        text.chars().map(move |character| {
            if let LineFragment::Element { width, len_utf8 } = self {
                WrapBoundaryCandidate::Element {
                    width: *width,
                    len_utf8: *len_utf8,
                }
            } else {
                WrapBoundaryCandidate::Char { character }
            }
        })
    }
}

enum WrapBoundaryCandidate {
    Char { character: char },
    Element { width: Pixels, len_utf8: usize },
}

impl WrapBoundaryCandidate {
    pub fn len_utf8(&self) -> usize {
        match self {
            WrapBoundaryCandidate::Char { character } => character.len_utf8(),
            WrapBoundaryCandidate::Element { len_utf8: len, .. } => *len,
        }
    }
}

/// A boundary between two lines of text.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct Boundary {
    /// The index of the last character in a line
    pub ix: usize,
    /// The indent of the next line.
    pub next_indent: u32,
}

impl Boundary {
    fn new(ix: usize, next_indent: u32) -> Self {
        Self { ix, next_indent }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        Font, FontFeatures, FontStyle, FontWeight, Hsla, TestAppContext, TestDispatcher, font,
    };
    #[cfg(target_os = "macos")]
    use crate::{TextRun, WindowTextSystem, WrapBoundary};
    use rand::prelude::*;

    fn build_wrapper() -> LineWrapper {
        let dispatcher = TestDispatcher::new(StdRng::seed_from_u64(0));
        let cx = TestAppContext::build(dispatcher, None);
        let id = cx.text_system().resolve_font(&font(".ZedMono"));
        LineWrapper::new(id, px(16.), cx.text_system().platform_text_system.clone())
    }

    fn generate_test_runs(input_run_len: &[usize]) -> Vec<TextRun> {
        input_run_len
            .iter()
            .map(|run_len| TextRun {
                len: *run_len,
                font: Font {
                    family: "Dummy".into(),
                    features: FontFeatures::default(),
                    fallbacks: None,
                    weight: FontWeight::default(),
                    style: FontStyle::Normal,
                },
                color: Hsla::default(),
                background_color: None,
                underline: None,
                strikethrough: None,
            })
            .collect()
    }

    #[test]
    fn test_wrap_line() {
        let mut wrapper = build_wrapper();

        assert_eq!(
            wrapper
                .wrap_line(
                    &[LineFragment::text("aa bbb cccc ddddd eeee")],
                    px(72.),
                    IndentAdjustment::default()
                )
                .collect::<Vec<_>>(),
            &[
                Boundary::new(7, 0),
                Boundary::new(12, 0),
                Boundary::new(18, 0)
            ],
        );
        assert_eq!(
            wrapper
                .wrap_line(
                    &[LineFragment::text("aaa aaaaaaaaaaaaaaaaaa")],
                    px(72.0),
                    IndentAdjustment::default()
                )
                .collect::<Vec<_>>(),
            &[
                Boundary::new(4, 0),
                Boundary::new(11, 0),
                Boundary::new(18, 0)
            ],
        );
        assert_eq!(
            wrapper
                .wrap_line(
                    &[LineFragment::text("     aaaaaaa")],
                    px(72.),
                    IndentAdjustment::default()
                )
                .collect::<Vec<_>>(),
            &[
                Boundary::new(7, 5),
                Boundary::new(9, 5),
                Boundary::new(11, 5),
            ]
        );
        assert_eq!(
            wrapper
                .wrap_line(
                    &[LineFragment::text("                            ")],
                    px(72.),
                    IndentAdjustment::default(),
                )
                .collect::<Vec<_>>(),
            &[
                Boundary::new(7, 0),
                Boundary::new(14, 0),
                Boundary::new(21, 0)
            ]
        );
        assert_eq!(
            wrapper
                .wrap_line(
                    &[LineFragment::text("          aaaaaaaaaaaaaa")],
                    px(72.),
                    IndentAdjustment::default()
                )
                .collect::<Vec<_>>(),
            &[
                Boundary::new(7, 0),
                Boundary::new(14, 3),
                Boundary::new(18, 3),
                Boundary::new(22, 3),
            ]
        );

        // Test wrapping multiple text fragments
        assert_eq!(
            wrapper
                .wrap_line(
                    &[
                        LineFragment::text("aa bbb "),
                        LineFragment::text("cccc ddddd eeee")
                    ],
                    px(72.),
                    IndentAdjustment::default(),
                )
                .collect::<Vec<_>>(),
            &[
                Boundary::new(7, 0),
                Boundary::new(12, 0),
                Boundary::new(18, 0)
            ],
        );

        // Test wrapping with a mix of text and element fragments
        assert_eq!(
            wrapper
                .wrap_line(
                    &[
                        LineFragment::text("aa "),
                        LineFragment::element(px(20.), 1),
                        LineFragment::text(" bbb "),
                        LineFragment::element(px(30.), 1),
                        LineFragment::text(" cccc")
                    ],
                    px(72.),
                    IndentAdjustment::default(),
                )
                .collect::<Vec<_>>(),
            &[
                Boundary::new(5, 0),
                Boundary::new(9, 0),
                Boundary::new(11, 0)
            ],
        );

        // Test with element at the beginning and text afterward
        assert_eq!(
            wrapper
                .wrap_line(
                    &[
                        LineFragment::element(px(50.), 1),
                        LineFragment::text(" aaaa bbbb cccc dddd")
                    ],
                    px(72.),
                    IndentAdjustment::default(),
                )
                .collect::<Vec<_>>(),
            &[
                Boundary::new(2, 0),
                Boundary::new(7, 0),
                Boundary::new(12, 0),
                Boundary::new(17, 0)
            ],
        );

        // Test with a large element that forces wrapping by itself
        assert_eq!(
            wrapper
                .wrap_line(
                    &[
                        LineFragment::text("short text "),
                        LineFragment::element(px(100.), 1),
                        LineFragment::text(" more text")
                    ],
                    px(72.),
                    IndentAdjustment::default(),
                )
                .collect::<Vec<_>>(),
            &[
                Boundary::new(6, 0),
                Boundary::new(11, 0),
                Boundary::new(12, 0),
                Boundary::new(18, 0)
            ],
        );
    }

    #[test]
    fn test_truncate_line() {
        let mut wrapper = build_wrapper();

        fn perform_test(
            wrapper: &mut LineWrapper,
            text: &'static str,
            result: &'static str,
            ellipsis: &str,
        ) {
            perform_test_with_width(wrapper, text, result, ellipsis, px(220.));
        }

        fn perform_test_with_width(
            wrapper: &mut LineWrapper,
            text: &'static str,
            result: &'static str,
            ellipsis: &str,
            width: Pixels,
        ) {
            let dummy_run_lens = vec![text.len()];
            let mut dummy_runs = generate_test_runs(&dummy_run_lens);
            assert_eq!(
                wrapper.truncate_line(text.into(), width, ellipsis, &mut dummy_runs),
                result
            );
            assert_eq!(dummy_runs.first().unwrap().len, result.len());
        }

        perform_test(
            &mut wrapper,
            "aa bbb cccc ddddd eeee ffff gggg",
            "aa bbb cccc ddddd eeee",
            "",
        );
        perform_test(
            &mut wrapper,
            "aa bbb cccc ddddd eeee ffff gggg",
            "aa bbb cccc ddddd eee…",
            "…",
        );
        perform_test(
            &mut wrapper,
            "aa bbb cccc ddddd eeee ffff gggg",
            "aa bbb cccc dddd......",
            "......",
        );
        perform_test_with_width(
            &mut wrapper,
            "aa bbb cccc ddddd. eeee ffff gggg",
            "aa bbb cccc ddddd…",
            "…",
            px(195.),
        );
        perform_test_with_width(
            &mut wrapper,
            "aa bbb cccc ddddd  eeee ffff gggg",
            "aa bbb cccc ddddd…",
            "…",
            px(195.),
        );
    }

    #[test]
    fn test_truncate_multiple_runs() {
        let mut wrapper = build_wrapper();

        fn perform_test(
            wrapper: &mut LineWrapper,
            text: &'static str,
            result: &str,
            run_lens: &[usize],
            result_run_len: &[usize],
            line_width: Pixels,
        ) {
            let mut dummy_runs = generate_test_runs(run_lens);
            assert_eq!(
                wrapper.truncate_line(text.into(), line_width, "…", &mut dummy_runs),
                result
            );
            for (run, result_len) in dummy_runs.iter().zip(result_run_len) {
                assert_eq!(run.len, *result_len);
            }
        }
        // Case 0: Normal
        // Text: abcdefghijkl
        // Runs: Run0 { len: 12, ... }
        //
        // Truncate res: abcd… (truncate_at = 4)
        // Run res: Run0 { string: abcd…, len: 7, ... }
        perform_test(&mut wrapper, "abcdefghijkl", "abcd…", &[12], &[7], px(50.));
        // Case 1: Drop some runs
        // Text: abcdefghijkl
        // Runs: Run0 { len: 4, ... }, Run1 { len: 4, ... }, Run2 { len: 4, ... }
        //
        // Truncate res: abcdef… (truncate_at = 6)
        // Runs res: Run0 { string: abcd, len: 4, ... }, Run1 { string: ef…, len:
        // 5, ... }
        perform_test(
            &mut wrapper,
            "abcdefghijkl",
            "abcdef…",
            &[4, 4, 4],
            &[4, 5],
            px(70.),
        );
        // Case 2: Truncate at start of some run
        // Text: abcdefghijkl
        // Runs: Run0 { len: 4, ... }, Run1 { len: 4, ... }, Run2 { len: 4, ... }
        //
        // Truncate res: abcdefgh… (truncate_at = 8)
        // Runs res: Run0 { string: abcd, len: 4, ... }, Run1 { string: efgh, len:
        // 4, ... }, Run2 { string: …, len: 3, ... }
        perform_test(
            &mut wrapper,
            "abcdefghijkl",
            "abcdefgh…",
            &[4, 4, 4],
            &[4, 4, 3],
            px(90.),
        );
    }

    #[test]
    fn test_update_run_after_truncation() {
        fn perform_test(result: &str, run_lens: &[usize], result_run_lens: &[usize]) {
            let mut dummy_runs = generate_test_runs(run_lens);
            update_runs_after_truncation(result, "…", &mut dummy_runs);
            for (run, result_len) in dummy_runs.iter().zip(result_run_lens) {
                assert_eq!(run.len, *result_len);
            }
        }
        // Case 0: Normal
        // Text: abcdefghijkl
        // Runs: Run0 { len: 12, ... }
        //
        // Truncate res: abcd… (truncate_at = 4)
        // Run res: Run0 { string: abcd…, len: 7, ... }
        perform_test("abcd…", &[12], &[7]);
        // Case 1: Drop some runs
        // Text: abcdefghijkl
        // Runs: Run0 { len: 4, ... }, Run1 { len: 4, ... }, Run2 { len: 4, ... }
        //
        // Truncate res: abcdef… (truncate_at = 6)
        // Runs res: Run0 { string: abcd, len: 4, ... }, Run1 { string: ef…, len:
        // 5, ... }
        perform_test("abcdef…", &[4, 4, 4], &[4, 5]);
        // Case 2: Truncate at start of some run
        // Text: abcdefghijkl
        // Runs: Run0 { len: 4, ... }, Run1 { len: 4, ... }, Run2 { len: 4, ... }
        //
        // Truncate res: abcdefgh… (truncate_at = 8)
        // Runs res: Run0 { string: abcd, len: 4, ... }, Run1 { string: efgh, len:
        // 4, ... }, Run2 { string: …, len: 3, ... }
        perform_test("abcdefgh…", &[4, 4, 4], &[4, 4, 3]);
    }

    #[test]
    fn test_is_word_char() {
        #[track_caller]
        fn assert_word(word: &str) {
            for c in word.chars() {
                assert!(LineWrapper::is_word_char(c), "assertion failed for '{}'", c);
            }
        }

        #[track_caller]
        fn assert_not_word(word: &str) {
            let found = word.chars().any(|c| !LineWrapper::is_word_char(c));
            assert!(found, "assertion failed for '{}'", word);
        }

        assert_word("Hello123");
        assert_word("non-English");
        assert_word("var_name");
        assert_word("123456");
        assert_word("3.1415");
        assert_word("10^2");
        assert_word("1~2");
        assert_word("100%");
        assert_word("@mention");
        assert_word("#hashtag");
        assert_word("$variable");
        assert_word("more⋯");
        assert_word("won’t");
        assert_word("‘twas");
        assert_word("plz!");
        assert_word("see)");
        assert_word("quoted”");
        assert_word("well…");
        assert_word("foo\u{00A0}bar");
        assert_word("foo\u{202F}bar");
        assert_word("foo\u{2011}bar");

        // Space
        assert_not_word("foo bar");

        // URL case
        assert_word("https://github.com/zed-industries/zed/");
        assert_word("github.com");
        assert_word("a=1&b=2");

        // Latin-1 Supplement
        assert_word("ÀÁÂÃÄÅÆÇÈÉÊËÌÍÎÏ");
        // Latin Extended-A
        assert_word("ĀāĂăĄąĆćĈĉĊċČčĎď");
        // Latin Extended-B
        assert_word("ƀƁƂƃƄƅƆƇƈƉƊƋƌƍƎƏ");
        // Cyrillic
        assert_word("АБВГДЕЖЗИЙКЛМНОП");

        // non-word characters
        assert_not_word("你好");
        assert_not_word("안녕하세요");
        assert_not_word("こんにちは");
        assert_not_word("😀😁😂");
        assert_not_word("()[]{}<>");
    }

    // These seem to vary wildly based on the text system.
    #[cfg(target_os = "macos")]
    #[crate::test]
    fn test_wrap_shaped_line(cx: &mut TestAppContext) {
        cx.update(|cx| {
            let text_system = WindowTextSystem::new(cx.text_system().clone());

            let normal = TextRun {
                len: 0,
                font: font("Helvetica"),
                color: Default::default(),
                underline: Default::default(),
                strikethrough: None,
                background_color: None,
            };
            let bold = TextRun {
                len: 0,
                font: font("Helvetica").bold(),
                color: Default::default(),
                underline: Default::default(),
                strikethrough: None,
                background_color: None,
            };

            let text = "aa bbb cccc ddddd eeee".into();
            let lines = text_system
                .shape_text(
                    text,
                    px(16.),
                    &[
                        normal.with_len(4),
                        bold.with_len(5),
                        normal.with_len(6),
                        bold.with_len(1),
                        normal.with_len(7),
                    ],
                    Some(px(72.)),
                    None,
                )
                .unwrap();

            assert_eq!(
                lines[0].layout.wrap_boundaries(),
                &[
                    WrapBoundary {
                        run_ix: 0,
                        glyph_ix: 7
                    },
                    WrapBoundary {
                        run_ix: 0,
                        glyph_ix: 12
                    },
                    WrapBoundary {
                        run_ix: 0,
                        glyph_ix: 18
                    }
                ],
            );
        });
    }

    #[test]
    fn test_extra_columns_overflow_guard() {
        let mut wrapper = build_wrapper();
        let space_width = wrapper.width_for_char(' ');

        // 6 spaces indent, wrap width 10 columns.
        let text = "      ab cd ef gh";
        let wrap_width = space_width * 10.0;

        // When base_indent + extra overflows wrap width (6 + 8 + 2 > 10),
        // indent must fall back to 0 instead of degrading to one character per row.
        //
        // Expected wrapped lines (10 columns):
        //   |      ab  |  (row 0: 6 spaces + "ab ", len 9)
        //   |cd ef gh  |  (row 1: 0 indent + "cd ef gh", len 8)
        assert_eq!(
            wrapper
                .wrap_line(
                    &[LineFragment::text(text)],
                    wrap_width,
                    IndentAdjustment::ExtraColumns(8),
                )
                .collect::<Vec<_>>(),
            &[Boundary::new(9, 0)]
        );

        // When base_indent + extra fits within wrap width (6 + 2 + 2 <= 10),
        // the extra indent is applied and not clamped.
        //
        // Expected wrapped lines (10 columns):
        //   |      ab  |  (row 0: 6 spaces + "ab ", len 9)
        //   |        cd|  (row 1: 8 spaces + "cd", len 10)
        //   |        ef|  (row 2: 8 spaces + "ef", len 10)
        //   |        gh|  (row 3: 8 spaces + "gh", len 10)
        assert_eq!(
            wrapper
                .wrap_line(
                    &[LineFragment::text(text)],
                    wrap_width,
                    IndentAdjustment::ExtraColumns(2),
                )
                .collect::<Vec<_>>(),
            &[
                Boundary::new(9, 8),
                Boundary::new(11, 8),
                Boundary::new(13, 8),
                Boundary::new(15, 8),
            ]
        );

        // When text contains full-width (two-column) glyphs, reserving headroom for a
        // two-column character (candidate + 2 > wrap_width) ensures that candidate
        // indents leaving only 1 column cannot accept the indent and overflow.
        //
        // 1 space indent, wrap width 10 columns.
        // " 🦀🦀🦀🦀🦀🦀🦀🦀" with extra=8 (extra_two with tab_size=4):
        // candidate = 1 + 8 = 9.
        // With +2 headroom (9 + 2 > 10), indent falls back to 0.
        //
        // Expected wrapped lines (10 columns):
        //   | 🦀🦀🦀🦀 |  (row 0: 1 space + 4 two-column glyphs, 9 cols)
        //   |🦀🦀🦀🦀  |  (row 1: 0 indent + 4 two-column glyphs, 8 cols)
        let full_width_text = " 🦀🦀🦀🦀🦀🦀🦀🦀";
        assert_eq!(
            wrapper
                .wrap_line(
                    &[LineFragment::text(full_width_text)],
                    wrap_width,
                    IndentAdjustment::ExtraColumns(8),
                )
                .collect::<Vec<_>>(),
            &[Boundary::new(17, 0)]
        );

        // When the carried suffix fits within wrap width alongside extra indent
        // (1 indent + 9 carried suffix = 10 <= 10), the extra indent is applied.
        //
        // Expected wrapped lines (10 columns):
        //   |a         |  (row 0: "a ", len 2)
        //   | abcdefghi|  (row 1: 1 indent + "abcdefghi", len 10)
        assert_eq!(
            wrapper
                .wrap_line(
                    &[LineFragment::text("a abcdefghi")],
                    wrap_width,
                    IndentAdjustment::ExtraColumns(1),
                )
                .collect::<Vec<_>>(),
            &[Boundary::new(2, 1)]
        );

        // When extra indent exceeds wrap width by even 1 column
        // (2 indent + 9 carried suffix = 11 > 10), indent falls back to 0.
        //
        // Expected wrapped lines (10 columns):
        //   |a         |  (row 0: "a ", len 2)
        //   |abcdefghi |  (row 1: 0 indent + "abcdefghi", len 9)
        assert_eq!(
            wrapper
                .wrap_line(
                    &[LineFragment::text("a abcdefghi")],
                    wrap_width,
                    IndentAdjustment::ExtraColumns(2),
                )
                .collect::<Vec<_>>(),
            &[Boundary::new(2, 0)]
        );

        // When text wraps at an earlier word boundary, the carried suffix
        // must be accounted for so indent + carried_suffix <= wrap_width.
        //
        // "a abcdefghij" with wrap_width 10 columns, ExtraColumns(8):
        // candidate = 0 + 8 = 8.
        // carried suffix = "abcdefghi" (9 columns).
        // 8 indent + 9 carried suffix = 17 > 10, so indent falls back to 0.
        //
        // Expected wrapped lines (10 columns):
        //   |a         |  (row 0: "a ", len 2)
        //   |abcdefghij|  (row 1: 0 indent + "abcdefghij", len 10)
        let carried_text = "a abcdefghij";
        assert_eq!(
            wrapper
                .wrap_line(
                    &[LineFragment::text(carried_text)],
                    wrap_width,
                    IndentAdjustment::ExtraColumns(8),
                )
                .collect::<Vec<_>>(),
            &[Boundary::new(2, 0)]
        );

        // When a wider inline element is encountered after the first wrap,
        // the overflow guard must evaluate whether the element fits with the extra indent.
        // If text continues after the element row and wraps again, subsequent continuation
        // lines resume the extra indent if their carried content fits.
        //
        // "abcdefghijk " followed by an 8-column element and "z 12":
        // Row 0: "abcdefghij" (len 10)
        // Row 1: "k " (2 cols) with 8 indent (len 10)
        // Row 2: element (8 cols) cannot fit with 8 indent (8 + 8 = 16 > 10),
        //        so indent falls back to 0. Element (8 cols) + "z " (2 cols) = len 10.
        // Row 3: "12" (2 cols) fits with 8 indent (8 + 2 = 10 <= 10).
        //
        // Expected wrapped lines (10 columns):
        //   |abcdefghij|  (row 0: 10 cols)
        //   |        k |  (row 1: 8 indent + "k ", len 10)
        //   |[ELEMENT]z|  (row 2: 0 indent + [ELEMENT (8)] + "z ", len 10)
        //   |        12|  (row 3: 8 indent + "12", len 10)
        let element_fragments = [
            LineFragment::text("abcdefghijk "),
            LineFragment::element(space_width * 8.0, 1),
            LineFragment::text("z 12"),
        ];
        assert_eq!(
            wrapper
                .wrap_line(
                    &element_fragments,
                    wrap_width,
                    IndentAdjustment::ExtraColumns(8),
                )
                .collect::<Vec<_>>(),
            &[
                Boundary::new(10, 8),
                Boundary::new(12, 0),
                Boundary::new(15, 8),
            ]
        );

        // A line of only whitespace wrapping before any non-whitespace character
        // must not have extra columns added to subsequent rows.
        let spaces = "                    "; // 20 spaces
        assert_eq!(
            wrapper
                .wrap_line(
                    &[LineFragment::text(spaces)],
                    wrap_width,
                    IndentAdjustment::ExtraColumns(8),
                )
                .collect::<Vec<_>>(),
            &[Boundary::new(10, 0)]
        );

        // When leading whitespace wraps before the first non-whitespace character,
        // base_indent should reflect the leading whitespace on the row where non-whitespace begins.
        // 14 spaces followed by "ab cd ef gh", wrap width 10:
        // Row 0: 10 spaces (len 10) -> wraps at ix 10 with indent 0
        // Row 1: 4 spaces + "ab cd" (len 9) -> wraps at ix 20
        //        base_indent is 14 - 10 = 4.
        //        With ExtraColumns(2), candidate = 4 + 2 = 6. 6 + 2 (headroom) = 8 <= 10.
        //        Row 2 and subsequent rows get indent 6.
        let multiline_indent_text = "              ab cd ef gh";
        assert_eq!(
            wrapper
                .wrap_line(
                    &[LineFragment::text(multiline_indent_text)],
                    wrap_width,
                    IndentAdjustment::ExtraColumns(2),
                )
                .collect::<Vec<_>>(),
            &[
                Boundary::new(10, 0),
                Boundary::new(20, 6),
                Boundary::new(23, 6),
            ]
        );

        // When a word boundary precedes an oversized carried suffix that itself
        // exceeds wrap_width (e.g. text followed by an inline element), wrapping
        // at the earlier candidate would force the continuation row to immediately
        // overflow even with 0 indent. The wrapper must reject the candidate and
        // break before the overflowing item instead.
        //
        // "aaaaaaaaaaaaaaaaaaa a" (21 chars: 19 'a's, space, 'a') followed by 10-column element:
        // Row 0: "aaaaaaaaaa" (10 chars, len 10) -> wraps at ix 10
        // Row 1: "aaaaaaaa" (8 chars) with 2 indent (len 10) -> wraps at ix 18
        // Row 2: "a a" (3 chars) with 2 indent (len 5)
        //        candidate boundary at ix 20 (space)
        //        Then element (10 cols): width becomes 5 + 10 = 15 > wrap_width (10).
        //        Candidate would carry 'a' (1 col) + element (10 cols) = 11 cols > 10.
        //        Because carried suffix (11) > wrap_width (10), candidate is rejected.
        //        Wrapper breaks at ix 21 (before element) with indent 0.
        // Row 3: element (10 cols, len 10)
        let oversized_suffix_fragments = [
            LineFragment::text("aaaaaaaaaaaaaaaaaaa a"),
            LineFragment::element(space_width * 10.0, 1),
        ];
        assert_eq!(
            wrapper
                .wrap_line(
                    &oversized_suffix_fragments,
                    wrap_width,
                    IndentAdjustment::ExtraColumns(2),
                )
                .collect::<Vec<_>>(),
            &[
                Boundary::new(10, 2),
                Boundary::new(18, 2),
                Boundary::new(21, 0),
            ]
        );

        assert_eq!(
            wrapper
                .wrap_line(
                    &[LineFragment::text("  aaaaaaaa")],
                    space_width * 4.0,
                    IndentAdjustment::ExtraColumns(u32::MAX),
                )
                .collect::<Vec<_>>(),
            &[Boundary::new(4, 0), Boundary::new(8, 0)]
        );
    }
}

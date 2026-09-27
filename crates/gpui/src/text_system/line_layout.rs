use crate::{FontId, GlyphId, Pixels, PlatformTextSystem, Point, SharedString, Size, point, px};
use collections::FxHashMap;
use parking_lot::{Mutex, RwLock, RwLockUpgradableReadGuard};
use smallvec::SmallVec;
use std::{
    borrow::Borrow,
    hash::{Hash, Hasher},
    ops::Range,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
};

use super::LineWrapper;

/// A laid out and styled line of text
#[derive(Default, Debug)]
pub struct LineLayout {
    /// The font size for this line
    pub font_size: Pixels,
    /// The width of the line
    pub width: Pixels,
    /// The ascent of the line
    pub ascent: Pixels,
    /// The descent of the line
    pub descent: Pixels,
    /// The shaped runs that make up this line
    pub runs: Vec<ShapedRun>,
    /// The length of the line in utf-8 bytes
    pub len: usize,
}

/// A run of text that has been shaped .
#[derive(Debug, Clone)]
pub struct ShapedRun {
    /// The font id for this run
    pub font_id: FontId,
    /// The glyphs that make up this run
    pub glyphs: Vec<ShapedGlyph>,
}

/// A single glyph, ready to paint.
#[derive(Clone, Debug)]
pub struct ShapedGlyph {
    /// The ID for this glyph, as determined by the text system.
    pub id: GlyphId,

    /// The position of this glyph in its containing line.
    pub position: Point<Pixels>,

    /// The index of this glyph in the original text.
    pub index: usize,

    /// Whether this glyph is an emoji
    pub is_emoji: bool,
}

impl LineLayout {
    /// The index for the character at the given x coordinate
    pub fn index_for_x(&self, x: Pixels) -> Option<usize> {
        if x >= self.width {
            None
        } else {
            for run in self.runs.iter().rev() {
                for glyph in run.glyphs.iter().rev() {
                    if glyph.position.x <= x {
                        return Some(glyph.index);
                    }
                }
            }
            Some(0)
        }
    }

    /// closest_index_for_x returns the character boundary closest to the given x coordinate
    /// (e.g. to handle aligning up/down arrow keys)
    pub fn closest_index_for_x(&self, x: Pixels) -> usize {
        let mut prev_index = 0;
        let mut prev_x = px(0.);

        for run in self.runs.iter() {
            for glyph in run.glyphs.iter() {
                if glyph.position.x >= x {
                    if glyph.position.x - x < x - prev_x {
                        return glyph.index;
                    } else {
                        return prev_index;
                    }
                }
                prev_index = glyph.index;
                prev_x = glyph.position.x;
            }
        }

        if self.len == 1 {
            if x > self.width / 2. {
                return 1;
            } else {
                return 0;
            }
        }

        self.len
    }

    /// The x position of the character at the given index
    pub fn x_for_index(&self, index: usize) -> Pixels {
        for run in &self.runs {
            for glyph in &run.glyphs {
                if glyph.index >= index {
                    return glyph.position.x;
                }
            }
        }
        self.width
    }

    /// The corresponding Font at the given index
    pub fn font_id_for_index(&self, index: usize) -> Option<FontId> {
        for run in &self.runs {
            for glyph in &run.glyphs {
                if glyph.index >= index {
                    return Some(run.font_id);
                }
            }
        }
        None
    }

    /// Split this layout at a byte index, returning `(prefix, suffix)`.
    ///
    /// - `prefix` contains glyphs for bytes `[0, byte_index)` with original positions.
    ///   Its width equals the x-advance up to the split point.
    /// - `suffix` contains glyphs for bytes `[byte_index, len)` with positions
    ///   shifted left so the first glyph starts at x=0, and byte indices rebased to 0.
    /// - `font_size`, `ascent`, and `descent` are copied to both halves.
    pub fn split_at(&self, byte_index: usize) -> (LineLayout, LineLayout) {
        let x_offset = self.x_for_index(byte_index);

        // Partition glyph runs. A single run may contribute glyphs to both halves.
        let mut left_runs = Vec::new();
        let mut right_runs = Vec::new();

        for run in &self.runs {
            let split_pos = run.glyphs.partition_point(|g| g.index < byte_index);

            if split_pos > 0 {
                left_runs.push(ShapedRun {
                    font_id: run.font_id,
                    glyphs: run.glyphs[..split_pos].to_vec(),
                });
            }

            if split_pos < run.glyphs.len() {
                let right_glyphs = run.glyphs[split_pos..]
                    .iter()
                    .map(|g| ShapedGlyph {
                        id: g.id,
                        position: point(g.position.x - x_offset, g.position.y),
                        index: g.index - byte_index,
                        is_emoji: g.is_emoji,
                    })
                    .collect();
                right_runs.push(ShapedRun {
                    font_id: run.font_id,
                    glyphs: right_glyphs,
                });
            }
        }

        let left = LineLayout {
            font_size: self.font_size,
            width: x_offset,
            ascent: self.ascent,
            descent: self.descent,
            runs: left_runs,
            len: byte_index,
        };

        let right = LineLayout {
            font_size: self.font_size,
            width: self.width - x_offset,
            ascent: self.ascent,
            descent: self.descent,
            runs: right_runs,
            len: self.len - byte_index,
        };

        (left, right)
    }

    fn compute_wrap_boundaries(
        &self,
        text: &str,
        wrap_width: Pixels,
        max_lines: Option<usize>,
    ) -> SmallVec<[WrapBoundary; 1]> {
        let mut boundaries = SmallVec::new();
        let mut first_non_whitespace_ix = None;
        let mut last_candidate_ix = None;
        let mut last_candidate_x = px(0.);
        let mut last_boundary = WrapBoundary {
            run_ix: 0,
            glyph_ix: 0,
        };
        let mut last_boundary_x = px(0.);
        let mut prev_ch = '\0';
        let mut glyphs = self
            .runs
            .iter()
            .enumerate()
            .flat_map(move |(run_ix, run)| {
                run.glyphs.iter().enumerate().map(move |(glyph_ix, glyph)| {
                    let character = text[glyph.index..].chars().next().unwrap();
                    (
                        WrapBoundary { run_ix, glyph_ix },
                        character,
                        glyph.position.x,
                    )
                })
            })
            .peekable();

        while let Some((boundary, ch, x)) = glyphs.next() {
            if ch == '\n' {
                continue;
            }

            // Here is very similar to `LineWrapper::wrap_line` to determine text wrapping,
            // but there are some differences, so we have to duplicate the code here.
            if LineWrapper::is_word_char(ch) {
                if prev_ch == ' ' && ch != ' ' && first_non_whitespace_ix.is_some() {
                    last_candidate_ix = Some(boundary);
                    last_candidate_x = x;
                }
            } else {
                if ch != ' ' && first_non_whitespace_ix.is_some() {
                    last_candidate_ix = Some(boundary);
                    last_candidate_x = x;
                }
            }

            if ch != ' ' && first_non_whitespace_ix.is_none() {
                first_non_whitespace_ix = Some(boundary);
            }

            let next_x = glyphs.peek().map_or(self.width, |(_, _, x)| *x);
            let width = next_x - last_boundary_x;

            if width > wrap_width && boundary > last_boundary {
                // When used line_clamp, we should limit the number of lines.
                if let Some(max_lines) = max_lines
                    && boundaries.len() >= max_lines.saturating_sub(1)
                {
                    break;
                }

                if let Some(last_candidate_ix) = last_candidate_ix.take() {
                    last_boundary = last_candidate_ix;
                    last_boundary_x = last_candidate_x;
                } else {
                    last_boundary = boundary;
                    last_boundary_x = x;
                }
                boundaries.push(last_boundary);
            }
            prev_ch = ch;
        }

        boundaries
    }
}

/// A line of text that has been wrapped to fit a given width
#[derive(Default, Debug)]
pub struct WrappedLineLayout {
    /// The line layout, pre-wrapping.
    pub unwrapped_layout: Arc<LineLayout>,

    /// The boundaries at which the line was wrapped
    pub wrap_boundaries: SmallVec<[WrapBoundary; 1]>,

    /// The width of the line, if it was wrapped
    pub wrap_width: Option<Pixels>,
}

/// A boundary at which a line was wrapped
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct WrapBoundary {
    /// The index in the run just before the line was wrapped
    pub run_ix: usize,
    /// The index of the glyph just before the line was wrapped
    pub glyph_ix: usize,
}

impl WrappedLineLayout {
    /// The length of the underlying text, in utf8 bytes.
    #[allow(clippy::len_without_is_empty)]
    pub fn len(&self) -> usize {
        self.unwrapped_layout.len
    }

    /// The width of this line, in pixels, whether or not it was wrapped.
    pub fn width(&self) -> Pixels {
        self.wrap_width
            .unwrap_or(Pixels::MAX)
            .min(self.unwrapped_layout.width)
    }

    /// The size of the whole wrapped text, for the given line_height.
    /// can span multiple lines if there are multiple wrap boundaries.
    pub fn size(&self, line_height: Pixels) -> Size<Pixels> {
        Size {
            width: self.width(),
            height: line_height * (self.wrap_boundaries.len() + 1),
        }
    }

    /// The ascent of a line in this layout
    pub fn ascent(&self) -> Pixels {
        self.unwrapped_layout.ascent
    }

    /// The descent of a line in this layout
    pub fn descent(&self) -> Pixels {
        self.unwrapped_layout.descent
    }

    /// The wrap boundaries in this layout
    pub fn wrap_boundaries(&self) -> &[WrapBoundary] {
        &self.wrap_boundaries
    }

    /// The font size of this layout
    pub fn font_size(&self) -> Pixels {
        self.unwrapped_layout.font_size
    }

    /// The runs in this layout, sans wrapping
    pub fn runs(&self) -> &[ShapedRun] {
        &self.unwrapped_layout.runs
    }

    /// The index corresponding to a given position in this layout for the given line height.
    ///
    /// See also [`Self::closest_index_for_position`].
    pub fn index_for_position(
        &self,
        position: Point<Pixels>,
        line_height: Pixels,
    ) -> Result<usize, usize> {
        self._index_for_position(position, line_height, false)
    }

    /// The closest index to a given position in this layout for the given line height.
    ///
    /// Closest means the character boundary closest to the given position.
    ///
    /// See also [`LineLayout::closest_index_for_x`].
    pub fn closest_index_for_position(
        &self,
        position: Point<Pixels>,
        line_height: Pixels,
    ) -> Result<usize, usize> {
        self._index_for_position(position, line_height, true)
    }

    fn _index_for_position(
        &self,
        mut position: Point<Pixels>,
        line_height: Pixels,
        closest: bool,
    ) -> Result<usize, usize> {
        let wrapped_line_ix = (position.y / line_height) as usize;

        let wrapped_line_start_index;
        let wrapped_line_start_x;
        if wrapped_line_ix > 0 {
            let Some(line_start_boundary) = self.wrap_boundaries.get(wrapped_line_ix - 1) else {
                return Err(0);
            };
            let run = &self.unwrapped_layout.runs[line_start_boundary.run_ix];
            let glyph = &run.glyphs[line_start_boundary.glyph_ix];
            wrapped_line_start_index = glyph.index;
            wrapped_line_start_x = glyph.position.x;
        } else {
            wrapped_line_start_index = 0;
            wrapped_line_start_x = Pixels::ZERO;
        };

        let wrapped_line_end_index;
        let wrapped_line_end_x;
        if wrapped_line_ix < self.wrap_boundaries.len() {
            let next_wrap_boundary_ix = wrapped_line_ix;
            let next_wrap_boundary = self.wrap_boundaries[next_wrap_boundary_ix];
            let run = &self.unwrapped_layout.runs[next_wrap_boundary.run_ix];
            let glyph = &run.glyphs[next_wrap_boundary.glyph_ix];
            wrapped_line_end_index = glyph.index;
            wrapped_line_end_x = glyph.position.x;
        } else {
            wrapped_line_end_index = self.unwrapped_layout.len;
            wrapped_line_end_x = self.unwrapped_layout.width;
        };

        let mut position_in_unwrapped_line = position;
        position_in_unwrapped_line.x += wrapped_line_start_x;
        if position_in_unwrapped_line.x < wrapped_line_start_x {
            Err(wrapped_line_start_index)
        } else if position_in_unwrapped_line.x >= wrapped_line_end_x {
            Err(wrapped_line_end_index)
        } else {
            if closest {
                Ok(self
                    .unwrapped_layout
                    .closest_index_for_x(position_in_unwrapped_line.x))
            } else {
                // The shaper can place a trailing zero-width wrap boundary glyph slightly past
                // the line's width, so the row can extend past where `index_for_x` has glyphs.
                self.unwrapped_layout
                    .index_for_x(position_in_unwrapped_line.x)
                    .ok_or(wrapped_line_end_index)
            }
        }
    }

    /// Returns the pixel position for the given byte index.
    pub fn position_for_index(&self, index: usize, line_height: Pixels) -> Option<Point<Pixels>> {
        let mut line_start_ix = 0;
        let mut line_end_indices = self
            .wrap_boundaries
            .iter()
            .map(|wrap_boundary| {
                let run = &self.unwrapped_layout.runs[wrap_boundary.run_ix];
                let glyph = &run.glyphs[wrap_boundary.glyph_ix];
                glyph.index
            })
            .chain([self.len()])
            .enumerate();
        for (ix, line_end_ix) in line_end_indices {
            let line_y = ix as f32 * line_height;
            if index < line_start_ix {
                break;
            } else if index > line_end_ix {
                line_start_ix = line_end_ix;
                continue;
            } else {
                let line_start_x = self.unwrapped_layout.x_for_index(line_start_ix);
                let x = self.unwrapped_layout.x_for_index(index) - line_start_x;
                return Some(point(x, line_y));
            }
        }

        None
    }
}

struct GlobalCacheEntry<T> {
    value: T,
    last_access: AtomicU64,
}

pub(crate) struct GlobalLineLayoutCache {
    max_entries: usize,
    low_watermark: usize,
    lines: RwLock<FxHashMap<Arc<CacheKey>, GlobalCacheEntry<Arc<LineLayout>>>>,
    wrapped_lines: RwLock<FxHashMap<Arc<CacheKey>, GlobalCacheEntry<Arc<WrappedLineLayout>>>>,
    access_counter: AtomicU64,
}

impl GlobalLineLayoutCache {
    pub fn new(max_entries: usize, low_watermark: usize) -> Self {
        let max_entries = max_entries.max(1);
        // Ensure low_watermark is always less than max_entries to avoid
        // degenerate eviction behaviour.
        let low_watermark = low_watermark.min(max_entries.saturating_sub(1));
        Self {
            max_entries,
            low_watermark,
            lines: RwLock::new(FxHashMap::default()),
            wrapped_lines: RwLock::new(FxHashMap::default()),
            access_counter: AtomicU64::new(0),
        }
    }

    #[cfg(test)]
    pub(crate) fn budget_for_test(&self) -> (usize, usize) {
        (self.max_entries, self.low_watermark)
    }

    /// Drop every cached line and wrapped-line layout.
    ///
    /// Useful when an application enters a low-memory state (for example, the
    /// last visible window is hidden) and wants to release the heap held by
    /// shaped text. The cache will repopulate naturally as text is laid out
    /// again.
    pub fn clear(&self) {
        self.lines.write().clear();
        self.wrapped_lines.write().clear();
    }

    /// Returns `(line_entries, wrapped_line_entries)` for diagnostics.
    pub fn entry_counts(&self) -> (usize, usize) {
        (self.lines.read().len(), self.wrapped_lines.read().len())
    }

    fn get_line(&self, key: &dyn AsCacheKeyRef) -> Option<Arc<LineLayout>> {
        let lines = self.lines.read();
        if let Some(entry) = lines.get(key) {
            // Relaxed is fine — this is just for approximate LRU ordering
            entry.last_access.store(
                self.access_counter.fetch_add(1, Ordering::Relaxed),
                Ordering::Relaxed,
            );
            Some(entry.value.clone())
        } else {
            None
        }
    }

    fn insert_line(&self, key: Arc<CacheKey>, layout: Arc<LineLayout>) {
        let mut lines = self.lines.write();
        if lines.len() >= self.max_entries {
            Self::evict_to_watermark(&mut lines, self.low_watermark);
        }
        lines.insert(
            key,
            GlobalCacheEntry {
                value: layout,
                last_access: AtomicU64::new(self.access_counter.fetch_add(1, Ordering::Relaxed)),
            },
        );
    }

    fn get_wrapped_line(&self, key: &dyn AsCacheKeyRef) -> Option<Arc<WrappedLineLayout>> {
        let wrapped = self.wrapped_lines.read();
        if let Some(entry) = wrapped.get(key) {
            entry.last_access.store(
                self.access_counter.fetch_add(1, Ordering::Relaxed),
                Ordering::Relaxed,
            );
            Some(entry.value.clone())
        } else {
            None
        }
    }

    fn insert_wrapped_line(&self, key: Arc<CacheKey>, layout: Arc<WrappedLineLayout>) {
        let mut wrapped = self.wrapped_lines.write();
        if wrapped.len() >= self.max_entries {
            Self::evict_to_watermark(&mut wrapped, self.low_watermark);
        }
        wrapped.insert(
            key,
            GlobalCacheEntry {
                value: layout,
                last_access: AtomicU64::new(self.access_counter.fetch_add(1, Ordering::Relaxed)),
            },
        );
    }

    /// Evict oldest entries until the map size is at or below `target_size`.
    fn evict_to_watermark<V>(
        map: &mut FxHashMap<Arc<CacheKey>, GlobalCacheEntry<V>>,
        target_size: usize,
    ) {
        if map.len() <= target_size {
            return;
        }
        let to_remove = map.len() - target_size;
        let mut entries: Vec<_> = map
            .iter()
            .map(|(k, entry)| (k.clone(), entry.last_access.load(Ordering::Relaxed)))
            .collect();
        entries.sort_by_key(|(_, access)| *access);
        for (key, _) in entries.into_iter().take(to_remove) {
            map.remove(&key);
        }
    }
}

pub(crate) struct LineLayoutCache {
    previous_frame: Mutex<FrameCache>,
    current_frame: RwLock<FrameCache>,
    platform_text_system: Arc<dyn PlatformTextSystem>,
    global_cache: Arc<GlobalLineLayoutCache>,
}

#[derive(Default)]
struct FrameCache {
    lines: FxHashMap<Arc<CacheKey>, Arc<LineLayout>>,
    wrapped_lines: FxHashMap<Arc<CacheKey>, Arc<WrappedLineLayout>>,
    used_lines: Vec<Arc<CacheKey>>,
    used_wrapped_lines: Vec<Arc<CacheKey>>,
}

#[derive(Clone, Default)]
pub(crate) struct LineLayoutIndex {
    lines_index: usize,
    wrapped_lines_index: usize,
}

impl LineLayoutCache {
    pub fn new(
        platform_text_system: Arc<dyn PlatformTextSystem>,
        global_cache: Arc<GlobalLineLayoutCache>,
    ) -> Self {
        Self {
            previous_frame: Mutex::default(),
            current_frame: RwLock::default(),
            platform_text_system,
            global_cache,
        }
    }

    pub fn layout_index(&self) -> LineLayoutIndex {
        let frame = self.current_frame.read();
        LineLayoutIndex {
            lines_index: frame.used_lines.len(),
            wrapped_lines_index: frame.used_wrapped_lines.len(),
        }
    }

    pub fn reuse_layouts(&self, range: Range<LineLayoutIndex>) {
        let mut previous_frame = &mut *self.previous_frame.lock();
        let mut current_frame = &mut *self.current_frame.write();

        for key in &previous_frame.used_lines[range.start.lines_index..range.end.lines_index] {
            if let Some((key, line)) = previous_frame.lines.remove_entry(key) {
                current_frame.lines.insert(key, line);
            }
            current_frame.used_lines.push(key.clone());
        }

        for key in &previous_frame.used_wrapped_lines
            [range.start.wrapped_lines_index..range.end.wrapped_lines_index]
        {
            if let Some((key, line)) = previous_frame.wrapped_lines.remove_entry(key) {
                current_frame.wrapped_lines.insert(key, line);
            }
            current_frame.used_wrapped_lines.push(key.clone());
        }
    }

    pub fn truncate_layouts(&self, index: LineLayoutIndex) {
        let mut current_frame = &mut *self.current_frame.write();
        current_frame.used_lines.truncate(index.lines_index);
        current_frame
            .used_wrapped_lines
            .truncate(index.wrapped_lines_index);
    }

    pub fn finish_frame(&self) {
        let mut prev_frame = self.previous_frame.lock();
        let mut curr_frame = self.current_frame.write();
        std::mem::swap(&mut *prev_frame, &mut *curr_frame);
        curr_frame.lines.clear();
        curr_frame.wrapped_lines.clear();
        curr_frame.used_lines.clear();
        curr_frame.used_wrapped_lines.clear();
    }

    pub fn layout_wrapped_line<Text>(
        &self,
        text: Text,
        font_size: Pixels,
        runs: &[FontRun],
        wrap_width: Option<Pixels>,
        max_lines: Option<usize>,
    ) -> Arc<WrappedLineLayout>
    where
        Text: AsRef<str>,
        SharedString: From<Text>,
    {
        let key = &CacheKeyRef {
            text: text.as_ref(),
            font_size,
            runs,
            wrap_width,
            force_width: None,
            letter_spacing: None,
        } as &dyn AsCacheKeyRef;

        let current_frame = self.current_frame.upgradable_read();
        if let Some(layout) = current_frame.wrapped_lines.get(key) {
            return layout.clone();
        }

        let previous_frame_entry = self.previous_frame.lock().wrapped_lines.remove_entry(key);
        if let Some((key, layout)) = previous_frame_entry {
            let mut current_frame = RwLockUpgradableReadGuard::upgrade(current_frame);
            current_frame
                .wrapped_lines
                .insert(key.clone(), layout.clone());
            current_frame.used_wrapped_lines.push(key);
            layout
        } else {
            // Check global cross-window cache
            if let Some(layout) = self.global_cache.get_wrapped_line(key) {
                let mut current_frame = RwLockUpgradableReadGuard::upgrade(current_frame);
                let key = Arc::new(CacheKey {
                    text: SharedString::from(text),
                    font_size,
                    runs: SmallVec::from(runs),
                    wrap_width,
                    force_width: None,
                    letter_spacing: None,
                });
                current_frame
                    .wrapped_lines
                    .insert(key.clone(), layout.clone());
                current_frame.used_wrapped_lines.push(key);
                return layout;
            }

            drop(current_frame);
            let text = SharedString::from(text);
            let unwrapped_layout = self.layout_line::<&SharedString>(&text, font_size, runs, None);
            let wrap_boundaries = if let Some(wrap_width) = wrap_width {
                unwrapped_layout.compute_wrap_boundaries(text.as_ref(), wrap_width, max_lines)
            } else {
                SmallVec::new()
            };
            let layout = Arc::new(WrappedLineLayout {
                unwrapped_layout,
                wrap_boundaries,
                wrap_width,
            });
            let key = Arc::new(CacheKey {
                text,
                font_size,
                runs: SmallVec::from(runs),
                wrap_width,
                force_width: None,
                letter_spacing: None,
            });

            let mut current_frame = self.current_frame.write();
            current_frame
                .wrapped_lines
                .insert(key.clone(), layout.clone());
            current_frame.used_wrapped_lines.push(key.clone());
            self.global_cache.insert_wrapped_line(key, layout.clone());

            layout
        }
    }

    pub fn layout_line<Text>(
        &self,
        text: Text,
        font_size: Pixels,
        runs: &[FontRun],
        force_width: Option<Pixels>,
    ) -> Arc<LineLayout>
    where
        Text: AsRef<str>,
        SharedString: From<Text>,
    {
        self.layout_line_with_spacing(text, font_size, runs, force_width, None)
    }

    pub fn layout_line_with_spacing<Text>(
        &self,
        text: Text,
        font_size: Pixels,
        runs: &[FontRun],
        force_width: Option<Pixels>,
        letter_spacing: Option<Pixels>,
    ) -> Arc<LineLayout>
    where
        Text: AsRef<str>,
        SharedString: From<Text>,
    {
        let key = &CacheKeyRef {
            text: text.as_ref(),
            font_size,
            runs,
            wrap_width: None,
            force_width,
            letter_spacing,
        } as &dyn AsCacheKeyRef;

        let current_frame = self.current_frame.upgradable_read();
        if let Some(layout) = current_frame.lines.get(key) {
            return layout.clone();
        }

        let mut current_frame = RwLockUpgradableReadGuard::upgrade(current_frame);
        if let Some((key, layout)) = self.previous_frame.lock().lines.remove_entry(key) {
            current_frame.lines.insert(key.clone(), layout.clone());
            current_frame.used_lines.push(key);
            return layout;
        }

        // Check global cross-window cache
        if let Some(layout) = self.global_cache.get_line(key) {
            let key = Arc::new(CacheKey {
                text: SharedString::from(text),
                font_size,
                runs: SmallVec::from(runs),
                wrap_width: None,
                force_width,
                letter_spacing,
            });
            current_frame.lines.insert(key.clone(), layout.clone());
            current_frame.used_lines.push(key);
            return layout;
        }

        let text = SharedString::from(text);
        let mut layout = self
            .platform_text_system
            .layout_line(&text, font_size, runs);

        if let Some(force_width) = force_width {
            apply_force_width_to_layout(&mut layout, force_width);
        }

        if let Some(spacing) = letter_spacing {
            apply_letter_spacing_to_layout(&mut layout, spacing, force_width);
        }

        let key = Arc::new(CacheKey {
            text,
            font_size,
            runs: SmallVec::from(runs),
            wrap_width: None,
            force_width,
            letter_spacing,
        });
        let layout = Arc::new(layout);
        current_frame.lines.insert(key.clone(), layout.clone());
        current_frame.used_lines.push(key.clone());
        self.global_cache.insert_line(key, layout.clone());
        layout
    }
}

// Combining marks are shaped at the same x position as their base character.
// Forced monospace layout must keep them anchored instead of advancing a cell.
fn apply_force_width_to_layout(layout: &mut LineLayout, force_width: Pixels) {
    let mut glyph_pos: usize = 0;
    let mut last_base_shaped_x = px(f32::NEG_INFINITY);
    let mut last_base_actual_x = px(0.);

    for run in layout.runs.iter_mut() {
        for glyph in run.glyphs.iter_mut() {
            let shaped_x = glyph.position.x;

            if shaped_x > last_base_shaped_x + force_width * 0.5 {
                let forced_x = glyph_pos * force_width;
                if (shaped_x - forced_x).abs() > px(1.) {
                    glyph.position.x = forced_x;
                }
                last_base_shaped_x = shaped_x;
                last_base_actual_x = glyph.position.x;
                glyph_pos += 1;
            } else {
                glyph.position.x = last_base_actual_x + (shaped_x - last_base_shaped_x);
            }
        }
    }
}

fn apply_letter_spacing_to_layout(
    layout: &mut LineLayout,
    spacing: Pixels,
    force_width: Option<Pixels>,
) {
    let glyph_count = if let Some(force_width) = force_width {
        let mut base_glyph_count: usize = 0;
        let mut last_base_x = px(f32::NEG_INFINITY);
        let mut last_base_spacing = px(0.);

        for run in layout.runs.iter_mut() {
            for glyph in run.glyphs.iter_mut() {
                let x = glyph.position.x;

                if x > last_base_x + force_width * 0.5 {
                    last_base_spacing = spacing * base_glyph_count as f32;
                    glyph.position.x = x + last_base_spacing;
                    last_base_x = x;
                    base_glyph_count += 1;
                } else {
                    glyph.position.x = x + last_base_spacing;
                }
            }
        }

        base_glyph_count
    } else {
        let mut glyph_count: usize = 0;
        for run in layout.runs.iter_mut() {
            for glyph in run.glyphs.iter_mut() {
                glyph.position.x = glyph.position.x + spacing * glyph_count as f32;
                glyph_count += 1;
            }
        }

        glyph_count
    };

    if glyph_count > 1 {
        layout.width = layout.width + spacing * (glyph_count - 1) as f32;
    }
}

/// A run of text with a single font.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
#[allow(missing_docs)]
pub struct FontRun {
    pub len: usize,
    pub font_id: FontId,
}

trait AsCacheKeyRef {
    fn as_cache_key_ref(&self) -> CacheKeyRef<'_>;
}

#[derive(Clone, Debug, Eq)]
struct CacheKey {
    text: SharedString,
    font_size: Pixels,
    runs: SmallVec<[FontRun; 1]>,
    wrap_width: Option<Pixels>,
    force_width: Option<Pixels>,
    letter_spacing: Option<Pixels>,
}

#[derive(Copy, Clone, PartialEq, Eq, Hash)]
struct CacheKeyRef<'a> {
    text: &'a str,
    font_size: Pixels,
    runs: &'a [FontRun],
    wrap_width: Option<Pixels>,
    force_width: Option<Pixels>,
    letter_spacing: Option<Pixels>,
}

impl PartialEq for dyn AsCacheKeyRef + '_ {
    fn eq(&self, other: &dyn AsCacheKeyRef) -> bool {
        self.as_cache_key_ref() == other.as_cache_key_ref()
    }
}

impl Eq for dyn AsCacheKeyRef + '_ {}

impl Hash for dyn AsCacheKeyRef + '_ {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.as_cache_key_ref().hash(state)
    }
}

impl AsCacheKeyRef for CacheKey {
    fn as_cache_key_ref(&self) -> CacheKeyRef<'_> {
        CacheKeyRef {
            text: &self.text,
            font_size: self.font_size,
            runs: self.runs.as_slice(),
            wrap_width: self.wrap_width,
            force_width: self.force_width,
            letter_spacing: self.letter_spacing,
        }
    }
}

impl PartialEq for CacheKey {
    fn eq(&self, other: &Self) -> bool {
        self.as_cache_key_ref().eq(&other.as_cache_key_ref())
    }
}

impl Hash for CacheKey {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.as_cache_key_ref().hash(state);
    }
}

impl<'a> Borrow<dyn AsCacheKeyRef + 'a> for Arc<CacheKey> {
    fn borrow(&self) -> &(dyn AsCacheKeyRef + 'a) {
        self.as_ref() as &dyn AsCacheKeyRef
    }
}

impl AsCacheKeyRef for CacheKeyRef<'_> {
    fn as_cache_key_ref(&self) -> CacheKeyRef<'_> {
        *self
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::GlyphId;

    fn glyph_at(x: f32, index: usize) -> ShapedGlyph {
        ShapedGlyph {
            id: GlyphId(0),
            position: point(px(x), px(0.)),
            index,
            is_emoji: false,
        }
    }

    fn make_layout(glyphs: Vec<ShapedGlyph>) -> LineLayout {
        LineLayout {
            font_size: px(16.),
            width: px(100.),
            ascent: px(12.),
            descent: px(4.),
            runs: vec![ShapedRun {
                font_id: FontId(0),
                glyphs,
            }],
            len: 0,
        }
    }

    fn glyph_x_positions(layout: &LineLayout) -> Vec<f32> {
        layout.runs[0]
            .glyphs
            .iter()
            .map(|glyph| f32::from(glyph.position.x))
            .collect()
    }

    fn make_global_cache_key(text: &str) -> Arc<CacheKey> {
        Arc::new(CacheKey {
            text: SharedString::from(text.to_owned()),
            font_size: px(16.),
            runs: SmallVec::new(),
            wrap_width: None,
            force_width: None,
            letter_spacing: None,
        })
    }

    #[test]
    fn test_global_layout_cache_clear_drops_all_entries() {
        let cache = GlobalLineLayoutCache::new(64, 32);
        let layout = Arc::new(make_layout(vec![glyph_at(0., 0)]));
        cache.insert_line(make_global_cache_key("hello"), layout.clone());
        cache.insert_line(make_global_cache_key("world"), layout);

        assert_eq!(cache.entry_counts(), (2, 0));

        cache.clear();

        assert_eq!(cache.entry_counts(), (0, 0));
    }

    #[test]
    fn test_split_at_partitions_glyphs_and_rebases_suffix() {
        let layout = LineLayout {
            font_size: px(16.),
            width: px(30.),
            ascent: px(12.),
            descent: px(4.),
            runs: vec![ShapedRun {
                font_id: FontId(0),
                glyphs: vec![glyph_at(0., 0), glyph_at(10., 1), glyph_at(20., 2)],
            }],
            len: 3,
        };

        let (left, right) = layout.split_at(1);

        assert_eq!(left.len, 1);
        assert_eq!(left.width, px(10.));
        assert_eq!(left.font_size, layout.font_size);
        assert_eq!(left.ascent, layout.ascent);
        assert_eq!(left.descent, layout.descent);
        assert_eq!(glyph_x_positions(&left), vec![0.]);
        assert_eq!(left.runs[0].glyphs[0].index, 0);

        assert_eq!(right.len, 2);
        assert_eq!(right.width, px(20.));
        assert_eq!(glyph_x_positions(&right), vec![0., 10.]);
        assert_eq!(
            right.runs[0]
                .glyphs
                .iter()
                .map(|glyph| glyph.index)
                .collect::<Vec<_>>(),
            vec![0, 1]
        );
    }

    #[test]
    fn test_split_at_mid_run_and_empty_sides() {
        let layout = LineLayout {
            font_size: px(16.),
            width: px(20.),
            ascent: px(12.),
            descent: px(4.),
            runs: vec![
                ShapedRun {
                    font_id: FontId(0),
                    glyphs: vec![glyph_at(0., 0), glyph_at(8., 1)],
                },
                ShapedRun {
                    font_id: FontId(1),
                    glyphs: vec![glyph_at(16., 2)],
                },
            ],
            len: 3,
        };

        let (left, right) = layout.split_at(2);
        assert_eq!(left.runs.len(), 1);
        assert_eq!(left.runs[0].glyphs.len(), 2);
        assert_eq!(right.runs.len(), 1);
        assert_eq!(right.runs[0].font_id, FontId(1));
        assert_eq!(glyph_x_positions(&right), vec![0.]);

        let (empty_left, all_right) = layout.split_at(0);
        assert!(empty_left.runs.is_empty());
        assert_eq!(empty_left.width, px(0.));
        assert_eq!(all_right.len, 3);
        assert_eq!(all_right.runs.len(), 2);

        let (all_left, empty_right) = layout.split_at(3);
        assert_eq!(all_left.runs.len(), 2);
        assert!(empty_right.runs.is_empty());
        assert_eq!(empty_right.width, px(0.));
    }

    #[test]
    fn test_force_width_latin_unchanged() {
        let cell_width = px(8.);
        let mut layout = make_layout(vec![glyph_at(0., 0), glyph_at(8., 1), glyph_at(16., 2)]);

        apply_force_width_to_layout(&mut layout, cell_width);

        assert_eq!(glyph_x_positions(&layout), vec![0., 8., 16.]);
    }

    #[test]
    fn test_force_width_combining_marks_not_advanced() {
        let cell_width = px(8.);
        let mut layout = make_layout(vec![glyph_at(0., 0), glyph_at(0., 3)]);

        apply_force_width_to_layout(&mut layout, cell_width);

        assert_eq!(glyph_x_positions(&layout), vec![0., 0.]);
    }

    #[test]
    fn test_force_width_base_after_combining_mark() {
        let cell_width = px(8.);
        let mut layout = make_layout(vec![glyph_at(0., 0), glyph_at(0., 3), glyph_at(8., 6)]);

        apply_force_width_to_layout(&mut layout, cell_width);

        assert_eq!(glyph_x_positions(&layout), vec![0., 0., 8.]);
    }

    #[test]
    fn test_force_width_multiple_combining_marks() {
        let cell_width = px(8.);
        let mut layout = make_layout(vec![
            glyph_at(0., 0),
            glyph_at(0., 3),
            glyph_at(0., 6),
            glyph_at(8., 9),
        ]);

        apply_force_width_to_layout(&mut layout, cell_width);

        assert_eq!(glyph_x_positions(&layout), vec![0., 0., 0., 8.]);
    }

    #[test]
    fn test_force_width_corrects_drifted_base_positions() {
        let cell_width = px(8.);
        let mut layout = make_layout(vec![glyph_at(0.5, 0), glyph_at(10.2, 1), glyph_at(19.8, 2)]);

        apply_force_width_to_layout(&mut layout, cell_width);

        assert_eq!(glyph_x_positions(&layout), vec![0.5, 8., 16.]);
    }

    #[test]
    fn test_force_width_combining_mark_after_within_tolerance_base() {
        let cell_width = px(8.);
        let mut layout = make_layout(vec![glyph_at(0.5, 0), glyph_at(0.5, 3)]);

        apply_force_width_to_layout(&mut layout, cell_width);

        assert_eq!(glyph_x_positions(&layout), vec![0.5, 0.5]);
    }

    #[test]
    fn test_force_width_with_letter_spacing_keeps_combining_marks_anchored() {
        let cell_width = px(8.);
        let mut layout = make_layout(vec![glyph_at(0., 0), glyph_at(0., 3), glyph_at(8., 6)]);

        apply_force_width_to_layout(&mut layout, cell_width);
        apply_letter_spacing_to_layout(&mut layout, px(1.), Some(cell_width));

        assert_eq!(glyph_x_positions(&layout), vec![0., 0., 9.]);
        assert_eq!(layout.width, px(101.));
    }
}

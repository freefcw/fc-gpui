use anyhow::{Context as _, Ok, Result};
use collections::HashMap;
use cosmic_text::{
    Attrs, AttrsList, CacheKey, Family, Font as CosmicTextFont, FontFeatures as CosmicFontFeatures,
    FontSystem, ShapeBuffer, ShapeLine, Stretch, Style, SwashCache, SwashImage, Weight,
};
use gpui::{
    Bounds, DevicePixels, Font, FontFallbacks, FontFeatures, FontId, FontMetrics, FontRun,
    FontStyle, FontWeight, GlyphId, IsZero as _, LineLayout, Pixels, PlatformTextSystem, Point,
    RenderGlyphParams, SUBPIXEL_VARIANTS_X, SUBPIXEL_VARIANTS_Y, ShapedGlyph, ShapedRun,
    SharedString, Size, point, size,
};

use itertools::Itertools;
use parking_lot::RwLock;
use pathfinder_geometry::{
    rect::{RectF, RectI},
    vector::{Vector2F, Vector2I},
};
use smallvec::SmallVec;
use std::{borrow::Cow, ops::Range, sync::Arc};
use unicode_segmentation::UnicodeSegmentation;

pub(crate) struct CosmicTextSystem(RwLock<CosmicTextSystemState>);

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct FontKey {
    family: SharedString,
    features: FontFeatures,
    fallbacks: Option<FontFallbacks>,
}

impl FontKey {
    fn new(family: SharedString, features: FontFeatures, fallbacks: Option<FontFallbacks>) -> Self {
        Self {
            family,
            features,
            fallbacks,
        }
    }
}

struct CosmicTextSystemState {
    swash_cache: SwashCache,
    font_system: FontSystem,
    scratch: ShapeBuffer,
    pending_glyph_images: HashMap<RenderGlyphParams, SwashImage>,
    /// Contains all already loaded fonts, including all faces. Indexed by `FontId`.
    loaded_fonts: Vec<LoadedFont>,
    /// Caches the `FontId`s associated with a specific family to avoid iterating the font database
    /// for every font face in a family.
    font_ids_by_family_cache: HashMap<FontKey, SmallVec<[FontId; 4]>>,
}

struct LoadedFont {
    font: Arc<CosmicTextFont>,
    weight: cosmic_text::Weight,
    features: CosmicFontFeatures,
    is_known_emoji_font: bool,
    user_fallback_chain: Arc<[(FontId, SharedString)]>,
}

struct FontMatchProperties {
    primary_family_name: SharedString,
    stretch: Stretch,
    style: Style,
    weight: Weight,
    features: CosmicFontFeatures,
    fallback_chain: Arc<[(FontId, SharedString)]>,
}

impl FontMatchProperties {
    fn attributes<'a>(&'a self, font_id: FontId, family_name: &'a str) -> Attrs<'a> {
        Attrs::new()
            .metadata(font_id.0)
            .family(Family::Name(family_name))
            .stretch(self.stretch)
            .style(self.style)
            .weight(self.weight)
            .font_features(self.features.clone())
    }
}

impl CosmicTextSystem {
    pub(crate) fn new() -> Self {
        // todo(linux) make font loading non-blocking
        let mut font_system = FontSystem::new();

        Self(RwLock::new(CosmicTextSystemState {
            font_system,
            swash_cache: SwashCache::new(),
            scratch: ShapeBuffer::default(),
            pending_glyph_images: HashMap::default(),
            loaded_fonts: Vec::new(),
            font_ids_by_family_cache: HashMap::default(),
        }))
    }
}

impl Default for CosmicTextSystem {
    fn default() -> Self {
        Self::new()
    }
}

impl PlatformTextSystem for CosmicTextSystem {
    fn add_fonts(&self, fonts: Vec<Cow<'static, [u8]>>) -> Result<()> {
        self.0.write().add_fonts(fonts)
    }

    fn all_font_names(&self) -> Vec<String> {
        let mut result = self
            .0
            .read()
            .font_system
            .db()
            .faces()
            .filter_map(|face| face.families.first().map(|family| family.0.clone()))
            .collect_vec();
        result.sort();
        result.dedup();
        result
    }

    fn font_id(&self, font: &Font) -> Result<FontId> {
        // todo(linux): Do we need to use CosmicText's Font APIs? Can we consolidate this to use font_kit?
        let mut state = self.0.write();
        let key = FontKey::new(
            font.family.clone(),
            font.features.clone(),
            font.fallbacks.clone(),
        );
        let candidates = if let Some(font_ids) = state.font_ids_by_family_cache.get(&key) {
            font_ids.as_slice()
        } else {
            let font_ids =
                state.load_family(&font.family, &font.features, font.fallbacks.as_ref())?;
            state.font_ids_by_family_cache.insert(key.clone(), font_ids);
            state.font_ids_by_family_cache[&key].as_ref()
        };

        // todo(linux) ideally we would make fontdb's `find_best_match` pub instead of using font-kit here
        let candidate_properties = candidates
            .iter()
            .map(|font_id| {
                let database_id = state.loaded_font(*font_id).font.id();
                let face_info = state.font_system.db().face(database_id).expect("");
                face_info_into_properties(face_info)
            })
            .collect::<SmallVec<[_; 4]>>();

        let ix =
            font_kit::matching::find_best_match(&candidate_properties, &font_into_properties(font))
                .context("requested font family contains no font matching the other parameters")?;

        Ok(candidates[ix])
    }

    fn prewarm_fonts(&self, font_ids: &[FontId]) {
        self.0.write().prewarm_fonts(font_ids);
    }

    fn font_metrics(&self, font_id: FontId) -> FontMetrics {
        let metrics = self
            .0
            .read()
            .loaded_font(font_id)
            .font
            .as_swash()
            .metrics(&[]);

        FontMetrics {
            units_per_em: metrics.units_per_em as u32,
            ascent: metrics.ascent,
            descent: -metrics.descent, // todo(linux) confirm this is correct
            line_gap: metrics.leading,
            underline_position: metrics.underline_offset,
            underline_thickness: metrics.stroke_size,
            cap_height: metrics.cap_height,
            x_height: metrics.x_height,
            // todo(linux): Compute this correctly
            bounding_box: Bounds {
                origin: point(0.0, 0.0),
                size: size(metrics.max_width, metrics.ascent + metrics.descent),
            },
        }
    }

    fn typographic_bounds(&self, font_id: FontId, glyph_id: GlyphId) -> Result<Bounds<f32>> {
        let lock = self.0.read();
        let glyph_metrics = lock.loaded_font(font_id).font.as_swash().glyph_metrics(&[]);
        let glyph_id = glyph_id.0 as u16;
        // todo(linux): Compute this correctly
        // see https://github.com/servo/font-kit/blob/master/src/loaders/freetype.rs#L614-L620
        Ok(Bounds {
            origin: point(0.0, 0.0),
            size: size(
                glyph_metrics.advance_width(glyph_id),
                glyph_metrics.advance_height(glyph_id),
            ),
        })
    }

    fn advance(&self, font_id: FontId, glyph_id: GlyphId) -> Result<Size<f32>> {
        self.0.read().advance(font_id, glyph_id)
    }

    fn glyph_for_char(&self, font_id: FontId, ch: char) -> Option<GlyphId> {
        self.0.read().glyph_for_char(font_id, ch)
    }

    fn glyph_raster_bounds(&self, params: &RenderGlyphParams) -> Result<Bounds<DevicePixels>> {
        self.0.write().raster_bounds(params)
    }

    fn rasterize_glyph(
        &self,
        params: &RenderGlyphParams,
        raster_bounds: Bounds<DevicePixels>,
    ) -> Result<(Size<DevicePixels>, Vec<u8>)> {
        self.0.write().rasterize_glyph(params, raster_bounds)
    }

    fn layout_line(&self, text: &str, font_size: Pixels, runs: &[FontRun]) -> LineLayout {
        self.0.write().layout_line(text, font_size, runs)
    }
}

impl CosmicTextSystemState {
    fn loaded_font(&self, font_id: FontId) -> &LoadedFont {
        &self.loaded_fonts[font_id.0]
    }

    fn font_match_properties(&self, font_id: FontId) -> Option<FontMatchProperties> {
        let loaded_font = self.loaded_font(font_id);
        let Some(face) = self.font_system.db().face(loaded_font.font.id()) else {
            log::warn!("font face not found in database for font_id {:?}", font_id);
            return None;
        };
        let Some(first_family) = face.families.first() else {
            log::warn!("font face has no family names for font_id {:?}", font_id);
            return None;
        };

        Some(FontMatchProperties {
            primary_family_name: first_family.0.clone().into(),
            stretch: face.stretch,
            style: face.style,
            weight: face.weight,
            features: loaded_font.features.clone(),
            fallback_chain: Arc::clone(&loaded_font.user_fallback_chain),
        })
    }

    fn prewarm_fonts(&mut self, font_ids: &[FontId]) {
        for &font_id in font_ids {
            let Some(properties) = self.font_match_properties(font_id) else {
                continue;
            };
            let primary_attributes =
                properties.attributes(font_id, &properties.primary_family_name);
            self.font_system.get_font_matches(&primary_attributes);

            for (fallback_id, fallback_name) in &*properties.fallback_chain {
                let fallback_attributes = properties.attributes(*fallback_id, fallback_name);
                self.font_system.get_font_matches(&fallback_attributes);
            }
        }
    }

    fn font_weight(&self, font_id: cosmic_text::fontdb::ID) -> cosmic_text::Weight {
        self.font_system
            .db()
            .face(font_id)
            .map(|face| face.weight)
            .unwrap_or(cosmic_text::Weight::NORMAL)
    }

    #[profiling::function]
    fn add_fonts(&mut self, fonts: Vec<Cow<'static, [u8]>>) -> Result<()> {
        let db = self.font_system.db_mut();
        for bytes in fonts {
            db.load_font_source(cosmic_text::fontdb::Source::Binary(Arc::new(bytes)));
        }
        Ok(())
    }

    #[profiling::function]
    fn load_family(
        &mut self,
        name: &str,
        features: &FontFeatures,
        fallbacks: Option<&FontFallbacks>,
    ) -> Result<SmallVec<[FontId; 4]>> {
        // Resolve user-configured fallbacks once while loading the primary family.
        // Fallback families do not recursively inherit another fallback chain.
        let user_fallback_chain: Arc<[(FontId, SharedString)]> = match fallbacks {
            Some(fallbacks) if !fallbacks.fallback_list().is_empty() => {
                let mut chain = Vec::new();
                for fallback_name in fallbacks.fallback_list() {
                    let fallback_key = FontKey::new(
                        SharedString::from(fallback_name.clone()),
                        features.clone(),
                        None,
                    );
                    let fallback_ids =
                        if let Some(cached) = self.font_ids_by_family_cache.get(&fallback_key) {
                            cached.clone()
                        } else {
                            let loaded = self.load_family(fallback_name, features, None)?;
                            self.font_ids_by_family_cache
                                .insert(fallback_key.clone(), loaded.clone());
                            loaded
                        };
                    let Some(&fallback_id) = fallback_ids.first() else {
                        continue;
                    };
                    let database_id = self.loaded_fonts[fallback_id.0].font.id();
                    if let Some(face) = self.font_system.db().face(database_id)
                        && let Some(family) = face.families.first()
                    {
                        chain.push((fallback_id, SharedString::from(family.0.clone())));
                    }
                }
                Arc::from(chain)
            }
            _ => Arc::from(Vec::new()),
        };

        // TODO: Determine the proper system UI font.
        let name = gpui::font_name_with_fallbacks(name, "IBM Plex Sans");

        let families = self
            .font_system
            .db()
            .faces()
            .filter(|face| face.families.iter().any(|family| *name == family.0))
            .map(|face| (face.id, face.post_script_name.clone()))
            .collect::<SmallVec<[_; 4]>>();

        let mut loaded_font_ids = SmallVec::new();
        for (font_id, postscript_name) in families {
            let font_weight = self.font_weight(font_id);
            let font = self
                .font_system
                .get_font(font_id, font_weight)
                .context("Could not load font")?;

            // HACK: To let the storybook run and render Windows caption icons. We should actually do better font fallback.
            let allowed_bad_font_names = [
                "SegoeFluentIcons", // NOTE: Segoe fluent icons postscript name is inconsistent
                "Segoe Fluent Icons",
            ];

            if font.as_swash().charmap().map('m') == 0
                && !allowed_bad_font_names.contains(&postscript_name.as_str())
            {
                self.font_system.db_mut().remove_face(font.id());
                continue;
            };

            let font_id = FontId(self.loaded_fonts.len());
            loaded_font_ids.push(font_id);
            self.loaded_fonts.push(LoadedFont {
                font,
                weight: font_weight,
                features: cosmic_font_features(features)?,
                is_known_emoji_font: check_is_known_emoji_font(&postscript_name),
                user_fallback_chain: Arc::clone(&user_fallback_chain),
            });
        }

        Ok(loaded_font_ids)
    }

    fn advance(&self, font_id: FontId, glyph_id: GlyphId) -> Result<Size<f32>> {
        let glyph_metrics = self.loaded_font(font_id).font.as_swash().glyph_metrics(&[]);
        Ok(Size {
            width: glyph_metrics.advance_width(glyph_id.0 as u16),
            height: glyph_metrics.advance_height(glyph_id.0 as u16),
        })
    }

    fn glyph_for_char(&self, font_id: FontId, ch: char) -> Option<GlyphId> {
        let glyph_id = self.loaded_font(font_id).font.as_swash().charmap().map(ch);
        if glyph_id == 0 {
            None
        } else {
            Some(GlyphId(glyph_id.into()))
        }
    }

    fn glyph_image(&mut self, params: &RenderGlyphParams) -> Result<SwashImage> {
        let loaded_font = &self.loaded_fonts[params.font_id.0];
        let font = &loaded_font.font;
        let font_weight = loaded_font.weight;
        let subpixel_shift = point(
            params.subpixel_variant.x as f32 / SUBPIXEL_VARIANTS_X as f32 / params.scale_factor,
            params.subpixel_variant.y as f32 / SUBPIXEL_VARIANTS_Y as f32 / params.scale_factor,
        );
        self.swash_cache
            .get_image(
                &mut self.font_system,
                CacheKey::new(
                    font.id(),
                    params.glyph_id.0 as u16,
                    (params.font_size * params.scale_factor).into(),
                    (subpixel_shift.x, subpixel_shift.y.trunc()),
                    font_weight,
                    cosmic_text::CacheKeyFlags::empty(),
                )
                .0,
            )
            .clone()
            .with_context(|| format!("no image for {params:?} in font {font:?}"))
    }

    fn raster_bounds(&mut self, params: &RenderGlyphParams) -> Result<Bounds<DevicePixels>> {
        let image = self.glyph_image(params)?;
        let bounds = Bounds {
            origin: point(image.placement.left.into(), (-image.placement.top).into()),
            size: size(image.placement.width.into(), image.placement.height.into()),
        };
        if !bounds.is_zero() {
            self.pending_glyph_images.insert(params.clone(), image);
        }
        Ok(bounds)
    }

    #[profiling::function]
    fn rasterize_glyph(
        &mut self,
        params: &RenderGlyphParams,
        glyph_bounds: Bounds<DevicePixels>,
    ) -> Result<(Size<DevicePixels>, Vec<u8>)> {
        if glyph_bounds.size.width.0 == 0 || glyph_bounds.size.height.0 == 0 {
            anyhow::bail!("glyph bounds are empty");
        } else {
            let bitmap_size = glyph_bounds.size;
            let mut image = match self.pending_glyph_images.remove(params) {
                Some(image) => image,
                None => self.glyph_image(params)?,
            };

            if params.is_emoji {
                // Convert from RGBA to BGRA.
                for pixel in image.data.chunks_exact_mut(4) {
                    pixel.swap(0, 2);
                }
            }

            Ok((bitmap_size, image.data))
        }
    }

    /// This is used when cosmic_text has chosen a fallback font instead of using the requested
    /// font, typically to handle some unicode characters. When this happens, `loaded_fonts` may not
    /// yet have an entry for this fallback font, and so one is added.
    ///
    /// Note that callers shouldn't use this `FontId` somewhere that will retrieve the corresponding
    /// `LoadedFont.features`, as it will have an arbitrarily chosen or empty value. The only
    /// current use of this field is for the *input* of `layout_line`, and so it's fine to use
    /// `font_id_for_cosmic_id` when computing the *output* of `layout_line`.
    fn font_id_for_cosmic_id(
        &mut self,
        id: cosmic_text::fontdb::ID,
        weight: cosmic_text::Weight,
    ) -> Option<FontId> {
        if let Some(ix) = self
            .loaded_fonts
            .iter()
            .position(|loaded_font| loaded_font.font.id() == id && loaded_font.weight == weight)
        {
            Some(FontId(ix))
        } else {
            let font = self.font_system.get_font(id, weight)?;
            let is_known_emoji_font = self
                .font_system
                .db()
                .face(id)
                .is_some_and(|face| check_is_known_emoji_font(&face.post_script_name));

            let font_id = FontId(self.loaded_fonts.len());
            self.loaded_fonts.push(LoadedFont {
                font,
                weight,
                features: CosmicFontFeatures::new(),
                is_known_emoji_font,
                user_fallback_chain: Arc::from(Vec::new()),
            });

            Some(font_id)
        }
    }

    #[profiling::function]
    fn layout_line(&mut self, text: &str, font_size: Pixels, font_runs: &[FontRun]) -> LineLayout {
        if contains_paragraph_separator(text) {
            self.layout_line_with_separators(text, font_size, font_runs)
        } else {
            self.layout_line_no_separators(text, font_size, font_runs)
        }
    }

    fn layout_line_with_separators(
        &mut self,
        text: &str,
        font_size: Pixels,
        font_runs: &[FontRun],
    ) -> LineLayout {
        let mut layout = LineLayout {
            font_size,
            len: text.len(),
            ..Default::default()
        };
        let mut paragraph_start = 0;

        for (separator_start, separator) in text
            .char_indices()
            .filter(|(_, character)| is_paragraph_separator(*character))
        {
            let separator_end = separator_start + separator.len_utf8();
            self.shape_segment(
                text,
                paragraph_start..separator_start,
                font_size,
                font_runs,
                &mut layout,
            );
            self.shape_segment(
                text,
                separator_start..separator_end,
                font_size,
                font_runs,
                &mut layout,
            );
            paragraph_start = separator_end;
        }

        self.shape_segment(
            text,
            paragraph_start..text.len(),
            font_size,
            font_runs,
            &mut layout,
        );

        layout
    }

    fn shape_segment(
        &mut self,
        text: &str,
        range: Range<usize>,
        font_size: Pixels,
        font_runs: &[FontRun],
        layout: &mut LineLayout,
    ) {
        if range.is_empty() {
            return;
        }

        let segment_font_runs = clip_font_runs(font_runs, range.clone());
        let segment =
            self.layout_line_no_separators(&text[range.clone()], font_size, &segment_font_runs);

        let mut segment_runs = segment.runs;
        for run in &mut segment_runs {
            for glyph in &mut run.glyphs {
                glyph.index += range.start;
                glyph.position.x += layout.width;
            }
        }

        for mut run in segment_runs {
            if let Some(same_run) = layout
                .runs
                .last_mut()
                .filter(|last| last.font_id == run.font_id)
            {
                same_run.glyphs.append(&mut run.glyphs);
            } else {
                layout.runs.push(run);
            }
        }

        layout.width += segment.width;
        layout.ascent = layout.ascent.max(segment.ascent);
        layout.descent = layout.descent.max(segment.descent);
    }

    fn layout_line_no_separators(
        &mut self,
        text: &str,
        font_size: Pixels,
        font_runs: &[FontRun],
    ) -> LineLayout {
        let mut attrs_list = AttrsList::new(&Attrs::new());
        let mut offs = 0;
        for run in font_runs {
            let run_end = offs + run.len;
            let Some(properties) = self.font_match_properties(run.font_id) else {
                offs = run_end;
                continue;
            };

            let primary_attrs = properties.attributes(run.font_id, &properties.primary_family_name);
            let fallback_attrs: SmallVec<[Attrs<'_>; 4]> = properties
                .fallback_chain
                .iter()
                .map(|(font_id, family_name)| properties.attributes(*font_id, family_name))
                .collect();

            let spans = if properties.fallback_chain.is_empty() {
                smallvec::smallvec![RunSpan {
                    start: offs,
                    end: run_end,
                    slot: None,
                }]
            } else {
                let loaded_fonts = &self.loaded_fonts;
                let covers = |font_id: FontId, ch: char| charmap_covers(loaded_fonts, font_id, ch);
                compute_run_spans(
                    text,
                    offs,
                    run.len,
                    run.font_id,
                    &properties.fallback_chain,
                    &covers,
                )
            };

            for span in spans {
                let attrs = match span.slot {
                    None => &primary_attrs,
                    Some(ix) => &fallback_attrs[ix],
                };
                attrs_list.add_span(span.start..span.end, attrs);
            }

            offs = run_end;
        }

        let line = ShapeLine::new(
            &mut self.font_system,
            text,
            &attrs_list,
            cosmic_text::Shaping::Advanced,
            4,
        );
        let mut layout_lines = Vec::with_capacity(1);
        line.layout_to_buffer(
            &mut self.scratch,
            f32::from(font_size),
            None,
            cosmic_text::Wrap::None,
            cosmic_text::Ellipsize::None,
            None,
            &mut layout_lines,
            None,
            cosmic_text::Hinting::Disabled,
        );
        let layout = layout_lines.first().unwrap();

        let mut runs: Vec<ShapedRun> = Vec::new();
        for glyph in &layout.glyphs {
            let mut font_id = FontId(glyph.metadata);
            let mut loaded_font = self.loaded_font(font_id);
            if loaded_font.font.id() != glyph.font_id || loaded_font.weight != glyph.font_weight {
                let Some(fallback_font_id) =
                    self.font_id_for_cosmic_id(glyph.font_id, glyph.font_weight)
                else {
                    continue;
                };
                font_id = fallback_font_id;
                loaded_font = self.loaded_font(font_id);
            }
            let is_emoji = loaded_font.is_known_emoji_font;

            if glyph.glyph_id == 3 && is_emoji {
                continue;
            }

            let shaped_glyph = ShapedGlyph {
                id: GlyphId(glyph.glyph_id as u32),
                position: point(glyph.x.into(), glyph.y.into()),
                index: glyph.start,
                is_emoji,
            };

            if let Some(last_run) = runs
                .last_mut()
                .filter(|last_run| last_run.font_id == font_id)
            {
                last_run.glyphs.push(shaped_glyph);
            } else {
                runs.push(ShapedRun {
                    font_id,
                    glyphs: vec![shaped_glyph],
                });
            }
        }

        LineLayout {
            font_size,
            width: layout.w.into(),
            ascent: layout.max_ascent.into(),
            descent: layout.max_descent.into(),
            runs,
            len: text.len(),
        }
    }
}

#[inline(always)]
fn is_paragraph_separator(character: char) -> bool {
    unicode_bidi::bidi_class(character) == unicode_bidi::BidiClass::B
}

fn contains_paragraph_separator(text: &str) -> bool {
    if text
        .bytes()
        .any(|byte| matches!(byte, b'\n' | b'\r' | 0x1c | 0x1d | 0x1e))
    {
        return true;
    }

    !text.is_ascii() && text.chars().any(is_paragraph_separator)
}

fn clip_font_runs(font_runs: &[FontRun], range: Range<usize>) -> SmallVec<[FontRun; 4]> {
    let mut clipped = SmallVec::new();
    let mut offs = 0;
    for run in font_runs {
        let run_start = offs;
        offs += run.len;
        if offs <= range.start {
            continue;
        }
        if run_start >= range.end {
            break;
        }
        let start = run_start.max(range.start);
        let end = offs.min(range.end);
        if start < end {
            clipped.push(FontRun {
                len: end - start,
                font_id: run.font_id,
            });
        }
    }
    clipped
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct RunSpan {
    start: usize,
    end: usize,
    slot: Option<usize>,
}

fn compute_run_spans(
    text: &str,
    run_offset: usize,
    run_len: usize,
    primary: FontId,
    fallback_chain: &[(FontId, SharedString)],
    covers: &impl Fn(FontId, char) -> bool,
) -> SmallVec<[RunSpan; 4]> {
    let mut spans = SmallVec::new();
    let run_end = run_offset + run_len;
    if run_end <= run_offset {
        return spans;
    }
    if fallback_chain.is_empty() {
        spans.push(RunSpan {
            start: run_offset,
            end: run_end,
            slot: None,
        });
        return spans;
    }

    let run_text = &text[run_offset..run_end];
    let mut span_start = run_offset;
    let mut span_slot = None;

    for (grapheme_idx, grapheme) in run_text.grapheme_indices(true) {
        let abs = run_offset + grapheme_idx;
        let ch = grapheme.chars().next().unwrap_or('\0');
        let next_slot = pick_covering_slot(ch, span_slot, primary, fallback_chain, covers);
        if next_slot == span_slot {
            continue;
        }
        if abs > span_start {
            spans.push(RunSpan {
                start: span_start,
                end: abs,
                slot: span_slot,
            });
        }
        span_start = abs;
        span_slot = next_slot;
    }

    if span_start < run_end {
        spans.push(RunSpan {
            start: span_start,
            end: run_end,
            slot: span_slot,
        });
    }

    spans
}

fn slot_font_id(
    slot: Option<usize>,
    primary: FontId,
    fallback_chain: &[(FontId, SharedString)],
) -> FontId {
    match slot {
        None => primary,
        Some(ix) => fallback_chain[ix].0,
    }
}

fn pick_covering_slot(
    ch: char,
    current: Option<usize>,
    primary: FontId,
    fallback_chain: &[(FontId, SharedString)],
    covers: &impl Fn(FontId, char) -> bool,
) -> Option<usize> {
    if ch.is_ascii() || covers(primary, ch) {
        return None;
    }

    let current_id = slot_font_id(current, primary, fallback_chain);
    if covers(current_id, ch) {
        return current;
    }

    fallback_chain
        .iter()
        .enumerate()
        .find_map(|(ix, (font_id, _))| covers(*font_id, ch).then_some(ix))
}

fn charmap_covers(loaded_fonts: &[LoadedFont], id: FontId, ch: char) -> bool {
    loaded_fonts
        .get(id.0)
        .is_some_and(|loaded| loaded.font.as_swash().charmap().map(ch) != 0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::{font, px};
    use std::borrow::Cow;

    const IBM_PLEX_SANS: &[u8] =
        include_bytes!("../../test_data/fonts/ibm-plex-sans/IBMPlexSans-Regular.ttf");
    const LILEX: &[u8] = include_bytes!("../../test_data/fonts/lilex/Lilex-Regular.ttf");

    #[test]
    fn prewarm_fonts_is_safe_for_loaded_and_fallback_fonts() {
        let text_system = CosmicTextSystem::new();
        text_system
            .add_fonts(vec![Cow::Borrowed(IBM_PLEX_SANS), Cow::Borrowed(LILEX)])
            .unwrap();

        let primary_family = family_name(IBM_PLEX_SANS);
        let fallback_family = family_name(LILEX);
        let mut primary_font = font(primary_family);
        primary_font.fallbacks = Some(FontFallbacks::from_fonts(vec![fallback_family]));
        let primary_id = text_system.font_id(&primary_font).unwrap();

        text_system.prewarm_fonts(&[primary_id]);

        let layout = text_system.layout_line(
            "AB",
            px(16.),
            &[FontRun {
                len: 2,
                font_id: primary_id,
            }],
        );
        assert!(!layout.runs.is_empty());
    }

    #[test]
    fn rasterize_glyph_reuses_image_measured_for_bounds() {
        let text_system = CosmicTextSystem::new();
        text_system
            .add_fonts(vec![Cow::Borrowed(IBM_PLEX_SANS)])
            .unwrap();
        let font_id = text_system
            .font_id(&font(family_name(IBM_PLEX_SANS)))
            .unwrap();
        let glyph_id = text_system
            .glyph_for_char(font_id, 'A')
            .expect("IBM Plex Sans contains A");
        let params = RenderGlyphParams {
            font_id,
            glyph_id,
            font_size: px(16.),
            subpixel_variant: point(0, 0),
            scale_factor: 1.0,
            is_emoji: false,
            dilation: 0,
        };

        let bounds = text_system.glyph_raster_bounds(&params).unwrap();
        assert!(!bounds.is_zero());
        assert!(
            text_system
                .0
                .read()
                .pending_glyph_images
                .contains_key(&params)
        );

        let (size, data) = text_system.rasterize_glyph(&params, bounds).unwrap();
        assert_eq!(size, bounds.size);
        assert!(!data.is_empty());
        assert!(text_system.0.read().pending_glyph_images.is_empty());
    }

    #[test]
    fn layout_line_uses_configured_font_fallbacks_for_missing_glyphs() {
        let text_system = CosmicTextSystem::new();
        text_system
            .add_fonts(vec![Cow::Borrowed(IBM_PLEX_SANS), Cow::Borrowed(LILEX)])
            .unwrap();

        let primary_family = family_name(IBM_PLEX_SANS);
        let fallback_family = family_name(LILEX);
        let fallback_only = fallback_only_char(IBM_PLEX_SANS, LILEX);

        let fallback_id = text_system.font_id(&font(fallback_family.clone())).unwrap();
        let mut primary_font = font(primary_family);
        primary_font.fallbacks = Some(FontFallbacks::from_fonts(vec![fallback_family]));
        let primary_id = text_system.font_id(&primary_font).unwrap();

        assert_eq!(text_system.glyph_for_char(primary_id, fallback_only), None);
        assert!(
            text_system
                .glyph_for_char(fallback_id, fallback_only)
                .is_some()
        );

        let text = format!("A{fallback_only}B");
        let layout = text_system.layout_line(
            &text,
            px(16.),
            &[FontRun {
                len: text.len(),
                font_id: primary_id,
            }],
        );
        let run_font_ids = layout
            .runs
            .iter()
            .map(|run| run.font_id)
            .collect::<Vec<_>>();

        assert_eq!(run_font_ids, vec![primary_id, fallback_id, primary_id]);
    }

    #[test]
    fn layout_line_handles_mixed_direction_paragraphs() {
        let text_system = CosmicTextSystem::new();
        text_system
            .add_fonts(vec![Cow::Borrowed(IBM_PLEX_SANS)])
            .unwrap();
        let font_id = text_system
            .font_id(&font(family_name(IBM_PLEX_SANS)))
            .unwrap();

        for separator in [
            '\u{000a}', '\u{000d}', '\u{001c}', '\u{001d}', '\u{001e}', '\u{0085}', '\u{2029}',
        ] {
            let text = format!("A{separator}\u{05d0}");
            let layout = text_system.layout_line(
                &text,
                px(16.),
                &[FontRun {
                    len: text.len(),
                    font_id,
                }],
            );

            assert_eq!(layout.len, text.len(), "{text:?}");
            assert!(layout.width > Pixels::ZERO, "{text:?}");
            for glyph in layout.runs.iter().flat_map(|run| &run.glyphs) {
                assert!(glyph.index < text.len(), "{text:?}: {}", glyph.index);
                assert!(text.is_char_boundary(glyph.index));
            }
        }
    }

    #[test]
    fn paragraph_separator_detection_covers_fast_and_unicode_paths() {
        for separator in [
            '\u{000a}', '\u{000d}', '\u{001c}', '\u{001d}', '\u{001e}', '\u{0085}', '\u{2029}',
        ] {
            assert!(is_paragraph_separator(separator));
            assert!(contains_paragraph_separator(&format!("a{separator}b")));
        }

        for text in [
            "",
            "plain ascii",
            "\u{05d0}",
            "tab\there",
            "emoji \u{1f600}",
        ] {
            assert!(!contains_paragraph_separator(text), "{text:?}");
        }
    }

    #[test]
    fn font_runs_are_clipped_to_paragraph_segments() {
        let runs = [
            FontRun {
                len: 3,
                font_id: FontId(1),
            },
            FontRun {
                len: 4,
                font_id: FontId(2),
            },
        ];

        let expected: SmallVec<[FontRun; 4]> = smallvec::smallvec![
            FontRun {
                len: 1,
                font_id: FontId(1),
            },
            FontRun {
                len: 3,
                font_id: FontId(2),
            },
        ];
        assert_eq!(clip_font_runs(&runs, 2..6), expected);
    }

    #[test]
    fn run_spans_prefer_configured_fallbacks_for_missing_glyphs() {
        let primary = FontId(0);
        let fallback = FontId(1);
        let fallback_chain = [(fallback, SharedString::from("Emoji"))];
        let spans = compute_run_spans(
            "a😀b",
            0,
            "a😀b".len(),
            primary,
            &fallback_chain,
            &|font_id, ch| match font_id {
                FontId(0) => ch.is_ascii(),
                FontId(1) => ch == '😀',
                _ => false,
            },
        );

        assert_eq!(
            spans.as_slice(),
            &[
                RunSpan {
                    start: 0,
                    end: 1,
                    slot: None,
                },
                RunSpan {
                    start: 1,
                    end: 5,
                    slot: Some(0),
                },
                RunSpan {
                    start: 5,
                    end: 6,
                    slot: None,
                },
            ]
        );
    }

    #[test]
    fn run_spans_keep_grapheme_clusters_together() {
        let primary = FontId(0);
        let fallback = FontId(1);
        let fallback_chain = [(fallback, SharedString::from("Emoji"))];
        let text = "a👨‍👩‍👧‍👦b";
        let spans = compute_run_spans(
            text,
            0,
            text.len(),
            primary,
            &fallback_chain,
            &|font_id, ch| match font_id {
                FontId(0) => ch.is_ascii(),
                FontId(1) => ch == '👨',
                _ => false,
            },
        );

        assert_eq!(
            spans.as_slice(),
            &[
                RunSpan {
                    start: 0,
                    end: 1,
                    slot: None,
                },
                RunSpan {
                    start: 1,
                    end: text.len() - 1,
                    slot: Some(0),
                },
                RunSpan {
                    start: text.len() - 1,
                    end: text.len(),
                    slot: None,
                },
            ]
        );
    }

    fn family_name(font_bytes: &[u8]) -> String {
        let face = ttf_parser::Face::parse(font_bytes, 0).unwrap();
        let mut typographic_family = None;
        let mut family = None;

        for name in face.names() {
            let Some(value) = name.to_string() else {
                continue;
            };
            match name.name_id {
                ttf_parser::name_id::TYPOGRAPHIC_FAMILY => {
                    typographic_family.get_or_insert(value);
                }
                ttf_parser::name_id::FAMILY => {
                    family.get_or_insert(value);
                }
                _ => {}
            }
        }

        typographic_family.or(family).unwrap()
    }

    fn fallback_only_char(primary_bytes: &[u8], fallback_bytes: &[u8]) -> char {
        let primary = ttf_parser::Face::parse(primary_bytes, 0).unwrap();
        let fallback = ttf_parser::Face::parse(fallback_bytes, 0).unwrap();

        (0x80..=0xf8ff)
            .filter_map(char::from_u32)
            .find(|&ch| primary.glyph_index(ch).is_none() && fallback.glyph_index(ch).is_some())
            .unwrap()
    }
}

fn cosmic_font_features(features: &FontFeatures) -> Result<CosmicFontFeatures> {
    let mut result = CosmicFontFeatures::new();
    for feature in features.0.iter() {
        let name_bytes: [u8; 4] = feature
            .0
            .as_bytes()
            .try_into()
            .context("Incorrect feature flag format")?;

        let tag = cosmic_text::FeatureTag::new(&name_bytes);
        result.set(tag, feature.1);
    }
    Ok(result)
}

#[allow(dead_code)]
fn bounds_f32_from_rect_f(rect: RectF) -> Bounds<f32> {
    Bounds {
        origin: point(rect.origin_x(), rect.origin_y()),
        size: size(rect.width(), rect.height()),
    }
}

#[allow(dead_code)]
fn device_bounds_from_rect_i(rect: RectI) -> Bounds<DevicePixels> {
    Bounds {
        origin: point(DevicePixels(rect.origin_x()), DevicePixels(rect.origin_y())),
        size: size(DevicePixels(rect.width()), DevicePixels(rect.height())),
    }
}

#[allow(dead_code)]
fn device_size_from_vector_i(value: Vector2I) -> Size<DevicePixels> {
    size(value.x().into(), value.y().into())
}

#[allow(dead_code)]
fn bounds_i32_from_rect_i(rect: RectI) -> Bounds<i32> {
    Bounds {
        origin: point(rect.origin_x(), rect.origin_y()),
        size: size(rect.width(), rect.height()),
    }
}

#[allow(dead_code)]
fn vector_i_from_point_u32(point: Point<u32>) -> Vector2I {
    Vector2I::new(point.x as i32, point.y as i32)
}

#[allow(dead_code)]
fn size_f32_from_vector_f(vector: Vector2F) -> Size<f32> {
    size(vector.x(), vector.y())
}

#[allow(dead_code)]
fn cosmic_weight(weight: FontWeight) -> cosmic_text::Weight {
    cosmic_text::Weight(weight.0 as u16)
}

#[allow(dead_code)]
fn cosmic_style(style: FontStyle) -> cosmic_text::Style {
    match style {
        FontStyle::Normal => cosmic_text::Style::Normal,
        FontStyle::Italic => cosmic_text::Style::Italic,
        FontStyle::Oblique => cosmic_text::Style::Oblique,
    }
}

fn font_into_properties(font: &gpui::Font) -> font_kit::properties::Properties {
    font_kit::properties::Properties {
        style: match font.style {
            gpui::FontStyle::Normal => font_kit::properties::Style::Normal,
            gpui::FontStyle::Italic => font_kit::properties::Style::Italic,
            gpui::FontStyle::Oblique => font_kit::properties::Style::Oblique,
        },
        weight: font_kit::properties::Weight(font.weight.0),
        stretch: Default::default(),
    }
}

fn face_info_into_properties(
    face_info: &cosmic_text::fontdb::FaceInfo,
) -> font_kit::properties::Properties {
    font_kit::properties::Properties {
        style: match face_info.style {
            cosmic_text::Style::Normal => font_kit::properties::Style::Normal,
            cosmic_text::Style::Italic => font_kit::properties::Style::Italic,
            cosmic_text::Style::Oblique => font_kit::properties::Style::Oblique,
        },
        // both libs use the same values for weight
        weight: font_kit::properties::Weight(face_info.weight.0.into()),
        stretch: match face_info.stretch {
            cosmic_text::Stretch::Condensed => font_kit::properties::Stretch::CONDENSED,
            cosmic_text::Stretch::Expanded => font_kit::properties::Stretch::EXPANDED,
            cosmic_text::Stretch::ExtraCondensed => font_kit::properties::Stretch::EXTRA_CONDENSED,
            cosmic_text::Stretch::ExtraExpanded => font_kit::properties::Stretch::EXTRA_EXPANDED,
            cosmic_text::Stretch::Normal => font_kit::properties::Stretch::NORMAL,
            cosmic_text::Stretch::SemiCondensed => font_kit::properties::Stretch::SEMI_CONDENSED,
            cosmic_text::Stretch::SemiExpanded => font_kit::properties::Stretch::SEMI_EXPANDED,
            cosmic_text::Stretch::UltraCondensed => font_kit::properties::Stretch::ULTRA_CONDENSED,
            cosmic_text::Stretch::UltraExpanded => font_kit::properties::Stretch::ULTRA_EXPANDED,
        },
    }
}

fn check_is_known_emoji_font(postscript_name: &str) -> bool {
    // TODO: Include other common emoji fonts
    postscript_name == "NotoColorEmoji"
}

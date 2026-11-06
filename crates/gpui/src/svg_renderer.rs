use crate::{
    AssetSource, DevicePixels, IsZero, RenderImage, Result, SharedString, Size,
    swap_rgba_pa_to_bgra,
};
use image::Frame;
use parking_lot::RwLock;
use resvg::tiny_skia::Pixmap;
use smallvec::SmallVec;
use std::{
    borrow::Cow,
    hash::Hash,
    sync::{Arc, LazyLock},
};

/// Font data shared by an application's text and SVG renderers.
/// Database snapshots keep in-flight parses independent of later registrations.
#[derive(Default)]
pub(crate) struct SvgFontRegistry {
    database: RwLock<Option<Arc<usvg::fontdb::Database>>>,
}

impl SvgFontRegistry {
    fn snapshot(&self) -> Arc<usvg::fontdb::Database> {
        if let Some(database) = self.database.read().as_ref() {
            return database.clone();
        }
        self.database
            .write()
            .get_or_insert_with(default_font_database)
            .clone()
    }

    pub(crate) fn add_fonts(&self, fonts: Vec<Cow<'static, [u8]>>) {
        let mut database = self.database.write();
        let database = Arc::make_mut(database.get_or_insert_with(default_font_database));
        for font in fonts {
            database.load_font_source(usvg::fontdb::Source::Binary(Arc::new(font)));
        }
    }
}

fn default_font_database() -> Arc<usvg::fontdb::Database> {
    static SYSTEM_FONT_DB: LazyLock<Arc<usvg::fontdb::Database>> = LazyLock::new(|| {
        let mut database = usvg::fontdb::Database::new();
        database.load_system_fonts();
        load_bundled_fonts(&mut database);
        fix_generic_font_families(&mut database);
        Arc::new(database)
    });
    SYSTEM_FONT_DB.clone()
}

#[cfg(target_os = "macos")]
const EMOJI_FONT_FAMILIES: &[&str] = &["Apple Color Emoji", ".AppleColorEmojiUI"];

#[cfg(target_os = "windows")]
const EMOJI_FONT_FAMILIES: &[&str] = &["Segoe UI Emoji", "Segoe UI Symbol"];

#[cfg(any(target_os = "linux", target_os = "freebsd"))]
const EMOJI_FONT_FAMILIES: &[&str] = &[
    "Noto Color Emoji",
    "Emoji One",
    "Twitter Color Emoji",
    "JoyPixels",
];

#[cfg(not(any(
    target_os = "macos",
    target_os = "windows",
    target_os = "linux",
    target_os = "freebsd",
)))]
const EMOJI_FONT_FAMILIES: &[&str] = &[];

const IBM_PLEX_SANS_REGULAR: &[u8] =
    include_bytes!("../assets/fonts/ibm-plex-sans/IBMPlexSans-Regular.ttf");
const LILEX_REGULAR: &[u8] = include_bytes!("../assets/fonts/lilex/Lilex-Regular.ttf");

fn is_emoji_presentation(ch: char) -> bool {
    static EMOJI_PRESENTATION_REGEX: LazyLock<regex::Regex> =
        LazyLock::new(|| regex::Regex::new("\\p{Emoji_Presentation}").unwrap());
    let mut buf = [0u8; 4];
    EMOJI_PRESENTATION_REGEX.is_match(ch.encode_utf8(&mut buf))
}

fn font_has_char(db: &usvg::fontdb::Database, id: usvg::fontdb::ID, ch: char) -> bool {
    db.with_face_data(id, |font_data, face_index| {
        ttf_parser::Face::parse(font_data, face_index)
            .ok()
            .and_then(|face| face.glyph_index(ch))
            .is_some()
    })
    .unwrap_or(false)
}

fn select_emoji_font(
    ch: char,
    fonts: &[usvg::fontdb::ID],
    db: &usvg::fontdb::Database,
    families: &[&str],
) -> Option<usvg::fontdb::ID> {
    for family_name in families {
        let query = usvg::fontdb::Query {
            families: &[usvg::fontdb::Family::Name(family_name)],
            weight: usvg::fontdb::Weight(400),
            stretch: usvg::fontdb::Stretch::Normal,
            style: usvg::fontdb::Style::Normal,
        };

        let Some(id) = db.query(&query) else {
            continue;
        };

        if fonts.contains(&id) || !font_has_char(db, id, ch) {
            continue;
        }

        return Some(id);
    }

    None
}

/// When rendering SVGs, we render them at twice the size to get a higher-quality result.
pub const SMOOTH_SVG_SCALE_FACTOR: f32 = 2.;

#[derive(Clone, PartialEq, Hash, Eq)]
#[allow(missing_docs)]
pub struct RenderSvgParams {
    pub path: SharedString,
    pub size: Size<DevicePixels>,
}

#[derive(Clone)]
pub struct SvgRenderer {
    asset_source: Arc<dyn AssetSource>,
    usvg_options: Arc<usvg::Options<'static>>,
}

/// A parsed SVG document that can be rasterized at any scale.
///
/// Produced by [`SvgRenderer::parse_svg`] and rasterized by
/// [`SvgRenderer::render_parsed`]. Parsing resolves fonts and converts text
/// to paths, so callers that need to rasterize the same SVG at multiple
/// scales should retain this value to avoid re-paying the parse cost.
/// It is a snapshot: registering fonts later does not change its resolved text.
pub struct ParsedSvg(usvg::Tree);

/// The size in which to rasterize the SVG.
#[derive(Clone, Copy)]
pub enum SvgSize {
    /// A width in device pixels. The SVG retains its aspect ratio.
    Size(Size<DevicePixels>),
    /// An exact width and height in device pixels.
    ExactSize(Size<DevicePixels>),
    /// A logical scaling factor to apply to the size provided by the SVG.
    ScaleFactor(f32),
}

impl From<f32> for SvgSize {
    fn from(scale_factor: f32) -> Self {
        Self::ScaleFactor(scale_factor)
    }
}

impl SvgRenderer {
    pub fn new(asset_source: Arc<dyn AssetSource>) -> Self {
        Self::with_fonts(asset_source, Arc::default())
    }

    pub(crate) fn with_fonts(
        asset_source: Arc<dyn AssetSource>,
        fonts: Arc<SvgFontRegistry>,
    ) -> Self {
        let default_font_resolver = usvg::FontResolver::default_font_selector();
        let font_resolver = Box::new(
            move |font: &usvg::Font, db: &mut Arc<usvg::fontdb::Database>| {
                if db.is_empty() {
                    *db = fonts.snapshot();
                }
                if let Some(id) = default_font_resolver(font, db) {
                    return Some(id);
                }

                let sans_query = usvg::fontdb::Query {
                    families: &[usvg::fontdb::Family::SansSerif],
                    ..Default::default()
                };
                db.query(&sans_query)
                    .or_else(|| db.faces().next().map(|face| face.id))
            },
        );
        let default_fallback_selection = usvg::FontResolver::default_fallback_selector();
        let fallback_selection = Box::new(
            move |ch: char, fonts: &[usvg::fontdb::ID], db: &mut Arc<usvg::fontdb::Database>| {
                if is_emoji_presentation(ch) {
                    if let Some(id) = select_emoji_font(ch, fonts, db.as_ref(), EMOJI_FONT_FAMILIES)
                    {
                        return Some(id);
                    }
                }

                default_fallback_selection(ch, fonts, db)
            },
        );
        let options = usvg::Options {
            font_resolver: usvg::FontResolver {
                select_font: font_resolver,
                select_fallback: fallback_selection,
            },
            ..Default::default()
        };
        Self {
            asset_source,
            usvg_options: Arc::new(options),
        }
    }

    /// Parses SVG data into a [`ParsedSvg`] that can be rasterized at any scale.
    /// Application renderers use fonts installed through [`crate::TextSystem::add_fonts`].
    /// A renderer created with [`Self::new`] uses system and bundled fonts only.
    pub fn parse_svg(&self, bytes: &[u8]) -> std::result::Result<ParsedSvg, usvg::Error> {
        usvg::Tree::from_data(bytes, &self.usvg_options).map(ParsedSvg)
    }

    /// Rasterizes a previously parsed SVG into an image buffer.
    ///
    /// [`SvgSize::ScaleFactor`] retains the existing 2× [`SMOOTH_SVG_SCALE_FACTOR`]
    /// rasterization path used by [`SvgRenderer::render_single_frame`].
    pub fn render_parsed(
        &self,
        svg: &ParsedSvg,
        size: impl Into<SvgSize>,
    ) -> std::result::Result<Arc<RenderImage>, usvg::Error> {
        let (size, image_scale_factor) = match size.into() {
            SvgSize::Size(size) => (SvgSize::Size(size), 1.0),
            SvgSize::ExactSize(size) => (SvgSize::ExactSize(size), 1.0),
            SvgSize::ScaleFactor(scale_factor) => (
                SvgSize::ScaleFactor(scale_factor * SMOOTH_SVG_SCALE_FACTOR),
                SMOOTH_SVG_SCALE_FACTOR,
            ),
        };
        let pixmap = rasterize_tree(&svg.0, size)?;
        let mut buffer =
            image::ImageBuffer::from_raw(pixmap.width(), pixmap.height(), pixmap.take()).unwrap();

        for pixel in buffer.chunks_exact_mut(4) {
            swap_rgba_pa_to_bgra(pixel);
        }

        let mut image = RenderImage::new(SmallVec::from_elem(Frame::new(buffer), 1));
        image.scale_factor = image_scale_factor;
        Ok(Arc::new(image))
    }

    pub(crate) fn render_single_frame(
        &self,
        bytes: &[u8],
        scale_factor: f32,
    ) -> Result<Arc<RenderImage>, usvg::Error> {
        let svg = self.parse_svg(bytes)?;
        self.render_parsed(&svg, scale_factor)
    }

    pub(crate) fn render(
        &self,
        params: &RenderSvgParams,
        data: Option<&[u8]>,
    ) -> Result<Option<(Size<DevicePixels>, Vec<u8>)>> {
        anyhow::ensure!(!params.size.is_zero(), "can't render at a zero size");

        let owned;
        let bytes = if let Some(data) = data {
            data
        } else {
            match self.asset_source.load(&params.path)? {
                Some(bytes) => {
                    owned = bytes;
                    owned.as_ref()
                }
                None => return Ok(None),
            }
        };

        let pixmap = self.render_pixmap(bytes, SvgSize::Size(params.size))?;

        // Convert the pixmap's pixels into an alpha mask.
        let size = Size::new(
            DevicePixels(pixmap.width() as i32),
            DevicePixels(pixmap.height() as i32),
        );
        let alpha_mask = pixmap
            .pixels()
            .iter()
            .map(|p| p.alpha())
            .collect::<Vec<_>>();
        Ok(Some((size, alpha_mask)))
    }

    pub fn render_pixmap(&self, bytes: &[u8], size: SvgSize) -> Result<Pixmap, usvg::Error> {
        let tree = usvg::Tree::from_data(bytes, &self.usvg_options)?;
        rasterize_tree(&tree, size)
    }
}

fn rasterize_tree(tree: &usvg::Tree, size: SvgSize) -> Result<Pixmap, usvg::Error> {
    // Cap the size of the rendered pixmap to avoid texture allocation panics.
    // Related upstream issue: zed-industries/zed#56466.
    const MAX_SIZE: f32 = 8192.0;

    let svg_size = tree.size();
    let (mut width, mut height) = match size {
        SvgSize::Size(size) => {
            let scale = i32::from(size.width) as f32 / svg_size.width();
            (svg_size.width() * scale, svg_size.height() * scale)
        }
        SvgSize::ExactSize(size) => (i32::from(size.width) as f32, i32::from(size.height) as f32),
        SvgSize::ScaleFactor(scale) => (svg_size.width() * scale, svg_size.height() * scale),
    };

    if width > MAX_SIZE {
        log::warn!("Attempted to render pixmap where width ({width}) > MAX_SIZE ({MAX_SIZE})");
    }
    if height > MAX_SIZE {
        log::warn!("Attempted to render pixmap where height ({height}) > MAX_SIZE ({MAX_SIZE})");
    }
    let scale = (MAX_SIZE / width).min(MAX_SIZE / height).min(1.0);
    width *= scale;
    height *= scale;

    // Render the SVG to a pixmap with the specified width and height.
    let mut pixmap = resvg::tiny_skia::Pixmap::new(width as u32, height as u32)
        .ok_or(usvg::Error::InvalidSize)?;

    let transform = resvg::tiny_skia::Transform::from_scale(
        width / svg_size.width(),
        height / svg_size.height(),
    );

    resvg::render(tree, transform, &mut pixmap.as_mut());

    Ok(pixmap)
}

fn load_bundled_fonts(db: &mut usvg::fontdb::Database) {
    db.load_font_data(IBM_PLEX_SANS_REGULAR.to_vec());
    db.load_font_data(LILEX_REGULAR.to_vec());
}

fn fix_generic_font_families(db: &mut usvg::fontdb::Database) {
    use usvg::fontdb::{Family, Query};

    let families_and_fallbacks: &[(Family<'_>, &str)] = &[
        (Family::SansSerif, "IBM Plex Sans"),
        (Family::Serif, "IBM Plex Sans"),
        (Family::Monospace, "Lilex"),
        (Family::Cursive, "IBM Plex Sans"),
        (Family::Fantasy, "IBM Plex Sans"),
    ];

    for (family, fallback_name) in families_and_fallbacks {
        let query = Query {
            families: &[*family],
            ..Default::default()
        };
        if db.query(&query).is_none() {
            match family {
                Family::SansSerif => db.set_sans_serif_family(*fallback_name),
                Family::Serif => db.set_serif_family(*fallback_name),
                Family::Monospace => db.set_monospace_family(*fallback_name),
                Family::Cursive => db.set_cursive_family(*fallback_name),
                Family::Fantasy => db.set_fantasy_family(*fallback_name),
                _ => {}
            }
        }
    }
}

#[cfg(test)]
pub(crate) fn svg_test_font() -> Vec<u8> {
    // Give the bundled monospace font a family absent from system/bundled fonts.
    // Equal-length replacements preserve the sfnt table offsets.
    let mut bytes = LILEX_REGULAR.to_vec();
    for (old, new) in [
        (b"Lilex".as_slice(), b"GPUIX".as_slice()),
        (b"\0L\0i\0l\0e\0x".as_slice(), b"\0G\0P\0U\0I\0X".as_slice()),
    ] {
        for offset in 0..=bytes.len() - old.len() {
            if &bytes[offset..offset + old.len()] == old {
                bytes[offset..offset + old.len()].copy_from_slice(new);
            }
        }
    }
    bytes
}

#[cfg(test)]
pub(crate) const SVG_TEST_TEXT: &[u8] = br#"<svg xmlns="http://www.w3.org/2000/svg" width="200" height="40"><text x="0" y="30" font-size="24" font-family="GPUIX">iiiiWWWW</text></svg>"#;

#[cfg(test)]
mod tests {
    use super::*;
    use usvg::fontdb::{Database, Family, Query};

    #[test]
    fn registered_fonts_affect_new_parses_but_not_existing_snapshots() -> Result<()> {
        let text_system = crate::TextSystem::new(
            Arc::new(crate::NoopTextSystem::new()),
            &crate::AppResourceProfile::default().text,
        );
        let renderer = SvgRenderer::with_fonts(Arc::new(()), text_system.svg_fonts.clone());
        let renderer_clone = renderer.clone();
        let before = renderer.parse_svg(SVG_TEST_TEXT)?;
        let original_image = renderer.render_parsed(&before, 1.0)?;
        let query = Query {
            families: &[Family::Name("GPUIX")],
            ..Default::default()
        };
        assert!(before.0.fontdb().query(&query).is_none());

        text_system.add_fonts(vec![Cow::Owned(svg_test_font())])?;

        let after = renderer_clone.parse_svg(SVG_TEST_TEXT)?;
        assert!(after.0.fontdb().query(&query).is_some());
        let changed_image = renderer_clone.render_parsed(&after, 1.0)?;
        assert_ne!(original_image.as_bytes(0), changed_image.as_bytes(0));
        let old_snapshot_image = renderer.render_parsed(&before, 1.0)?;
        assert_eq!(original_image.as_bytes(0), old_snapshot_image.as_bytes(0));
        assert!(before.0.fontdb().query(&query).is_none());
        let independent = SvgRenderer::new(Arc::new(())).parse_svg(SVG_TEST_TEXT)?;
        assert!(independent.0.fontdb().query(&query).is_none());
        Ok(())
    }

    #[test]
    fn renders_parsed_svg_at_requested_size() -> Result<()> {
        let renderer = SvgRenderer::new(Arc::new(()));
        let svg = renderer.parse_svg(
            br#"<svg xmlns="http://www.w3.org/2000/svg" width="24pt" height="12pt"></svg>"#,
        )?;
        let requested_size = Size::new(DevicePixels(24), DevicePixels(12));
        let image = renderer.render_parsed(&svg, SvgSize::ExactSize(requested_size))?;

        assert_eq!(image.size(0), requested_size);
        Ok(())
    }

    #[test]
    fn preserves_aspect_ratio_for_width_constrained_size() -> Result<()> {
        let renderer = SvgRenderer::new(Arc::new(()));
        let svg = renderer.parse_svg(
            br#"<svg xmlns="http://www.w3.org/2000/svg" width="24pt" height="12pt"></svg>"#,
        )?;
        let image = renderer.render_parsed(
            &svg,
            SvgSize::Size(Size::new(DevicePixels(24), DevicePixels(24))),
        )?;

        assert_eq!(image.size(0), Size::new(DevicePixels(24), DevicePixels(12)));
        Ok(())
    }

    fn db_with_bundled_fonts() -> Database {
        let mut db = Database::new();
        load_bundled_fonts(&mut db);
        db
    }

    #[test]
    fn test_is_emoji_presentation() {
        let cases = [
            ("a", false),
            ("Z", false),
            ("1", false),
            ("#", false),
            ("*", false),
            ("漢", false),
            ("中", false),
            ("カ", false),
            ("©", false),
            ("♥", false),
            ("😀", true),
            ("✅", true),
            ("🇺🇸", true),
            ("©️", false),
            ("♥️", false),
            ("1️⃣", false),
        ];

        for (s, expected) in cases {
            assert_eq!(
                is_emoji_presentation(s.chars().next().unwrap()),
                expected,
                "for char {s:?}",
            );
        }
    }

    #[test]
    fn fix_generic_font_families_sets_all_families() {
        let mut db = db_with_bundled_fonts();
        fix_generic_font_families(&mut db);

        for family in [
            Family::SansSerif,
            Family::Serif,
            Family::Monospace,
            Family::Cursive,
            Family::Fantasy,
        ] {
            let query = Query {
                families: &[family],
                ..Default::default()
            };
            assert!(
                db.query(&query).is_some(),
                "expected generic family {family:?} to resolve"
            );
        }
    }

    #[test]
    fn test_select_emoji_font_skips_family_without_glyph() {
        let db = db_with_bundled_fonts();

        let ibm_plex_sans = db
            .query(&Query {
                families: &[Family::Name("IBM Plex Sans")],
                weight: usvg::fontdb::Weight(400),
                stretch: usvg::fontdb::Stretch::Normal,
                style: usvg::fontdb::Style::Normal,
            })
            .unwrap();
        let lilex = db
            .query(&Query {
                families: &[Family::Name("Lilex")],
                weight: usvg::fontdb::Weight(400),
                stretch: usvg::fontdb::Stretch::Normal,
                style: usvg::fontdb::Style::Normal,
            })
            .unwrap();
        let selected = select_emoji_font('│', &[], &db, &["IBM Plex Sans", "Lilex"]).unwrap();

        assert_eq!(selected, lilex);
        assert!(!font_has_char(&db, ibm_plex_sans, '│'));
        assert!(font_has_char(&db, selected, '│'));
    }

    #[test]
    fn fix_generic_font_families_monospace_resolves_to_lilex() {
        let mut db = db_with_bundled_fonts();
        fix_generic_font_families(&mut db);

        let query = Query {
            families: &[Family::Monospace],
            ..Default::default()
        };
        let id = db.query(&query).expect("monospace should resolve");
        let face = db.face(id).expect("face should exist");
        assert!(
            face.families.iter().any(|(name, _)| name.contains("Lilex")),
            "monospace should map to Lilex, got {:?}",
            face.families
        );
    }

    #[test]
    fn render_pixmap_caps_oversized_svg_dimensions() {
        let renderer = SvgRenderer::new(Arc::new(()));
        let svg = br#"<svg xmlns="http://www.w3.org/2000/svg" width="16000" height="32000">
            <rect width="16000" height="32000" fill="red"/>
        </svg>"#;

        let pixmap = renderer
            .render_pixmap(svg, SvgSize::ScaleFactor(1.0))
            .unwrap();

        assert!(pixmap.width() <= 8192);
        assert!(pixmap.height() <= 8192);
        assert_eq!(pixmap.width(), 4096);
        assert_eq!(pixmap.height(), 8192);
    }
}

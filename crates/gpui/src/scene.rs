// todo("windows"): remove
#![cfg_attr(windows, allow(dead_code))]

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::{
    AtlasTextureId, AtlasTile, Background, Bounds, ContentMask, Corners, Edges, Hsla, Pixels,
    Point, Radians, ScaledPixels, Size, TimedTransformation, bounds_tree::BoundsTree, point,
};
use std::{
    fmt::Debug,
    iter::Peekable,
    ops::{Add, Range, Sub},
    slice,
    time::Duration,
};

#[allow(non_camel_case_types, unused)]
#[allow(missing_docs)]
pub type PathVertex_ScaledPixels = PathVertex<ScaledPixels>;

#[allow(missing_docs)]
pub type DrawOrder = u32;

/// A boolean stored as a `u32` so GPU-facing structs contain no
/// compiler-inserted padding bytes when reinterpreted as instance data.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
#[repr(transparent)]
#[allow(missing_docs)]
pub struct PaddedBool32(u32);

impl From<bool> for PaddedBool32 {
    fn from(value: bool) -> Self {
        PaddedBool32(value as u32)
    }
}

#[derive(Default)]
#[allow(missing_docs)]
pub struct Scene {
    pub(crate) paint_operations: Vec<PaintOperation>,
    primitive_bounds: BoundsTree<ScaledPixels>,
    layer_stack: Vec<DrawOrder>,
    pub shadows: Vec<Shadow>,
    pub quads: Vec<Quad>,
    pub paths: Vec<Path<ScaledPixels>>,
    pub underlines: Vec<Underline>,
    pub monochrome_sprites: Vec<MonochromeSprite>,
    pub polychrome_sprites: Vec<PolychromeSprite>,
    pub surfaces: Vec<PaintSurface>,
    animations: Vec<SpriteAnimation>,
}

#[allow(missing_docs)]
impl Scene {
    pub(crate) fn clear(&mut self) {
        self.paint_operations.clear();
        self.primitive_bounds.clear();
        self.layer_stack.clear();
        self.paths.clear();
        self.shadows.clear();
        self.quads.clear();
        self.underlines.clear();
        self.monochrome_sprites.clear();
        self.polychrome_sprites.clear();
        self.surfaces.clear();
        self.animations.clear();
    }

    /// Inserts a sprite that [`Self::advance_animations`] keeps moving. The
    /// sprite's color is taken as its color before the animation's alpha.
    ///
    /// A replayed animation already recorded that color. The sprite's current
    /// color may include a later pose's alpha, so it must not replace the
    /// original or each redraw fades the SVG again.
    pub(crate) fn insert_animated_sprite(
        &mut self,
        mut sprite: MonochromeSprite,
        mut animation: SpriteAnimation,
    ) {
        sprite.animation = SpriteAnimationId(self.animations.len() as u32);
        if animation.sprite_index == usize::MAX {
            animation.base_color = sprite.color;
        }
        if self.try_insert_primitive(sprite.into()) {
            self.animations.push(animation);
        }
    }

    /// How long the window may wait between presents of this scene while only
    /// its animations need a frame: `None` when nothing in it animates, and
    /// zero when an animation wants every frame.
    pub(crate) fn animation_frame_interval(&self) -> Option<Duration> {
        self.animations
            .iter()
            .map(|animation| {
                animation
                    .transformation
                    .min_frame_interval()
                    .unwrap_or_default()
            })
            .min()
    }

    /// Moves every animated sprite to its pose at `elapsed` time on the app's
    /// synced animation clock, without the scene being rebuilt.
    pub(crate) fn advance_animations(&mut self, elapsed: Duration) {
        for animation in &self.animations {
            let Some(sprite) = self.monochrome_sprites.get_mut(animation.sprite_index) else {
                continue;
            };
            let pose = animation.transformation.at(elapsed);
            sprite.transformation = pose.into_matrix(animation.center, animation.scale_factor);
            sprite.color = animation.base_color.opacity(pose.alpha_factor());
        }
    }

    pub fn len(&self) -> usize {
        self.paint_operations.len()
    }

    pub(crate) fn push_layer(&mut self, bounds: Bounds<ScaledPixels>) {
        let order = self.primitive_bounds.insert(bounds);
        self.layer_stack.push(order);
        self.paint_operations
            .push(PaintOperation::StartLayer(bounds));
    }

    pub(crate) fn pop_layer(&mut self) {
        self.layer_stack.pop();
        self.paint_operations.push(PaintOperation::EndLayer);
    }

    pub(crate) fn insert_primitive(&mut self, primitive: impl Into<Primitive>) {
        self.try_insert_primitive(primitive.into());
    }

    /// Returns whether the primitive was kept, rather than clipped away.
    fn try_insert_primitive(&mut self, primitive: Primitive) -> bool {
        let clipped_bounds = primitive
            .bounds()
            .intersect(&primitive.content_mask().bounds);

        if clipped_bounds.is_empty() {
            return false;
        }

        let order = self
            .layer_stack
            .last()
            .copied()
            .unwrap_or_else(|| self.primitive_bounds.insert(clipped_bounds));
        let (kind, index) = match primitive {
            Primitive::Shadow(mut shadow) => {
                shadow.order = order;
                let idx = self.shadows.len();
                self.shadows.push(shadow);
                (PrimitiveKind::Shadow, idx)
            }
            Primitive::Quad(mut quad) => {
                quad.order = order;
                let idx = self.quads.len();
                self.quads.push(quad);
                (PrimitiveKind::Quad, idx)
            }
            Primitive::Path(mut path) => {
                path.order = order;
                path.id = PathId(self.paths.len());
                let idx = self.paths.len();
                self.paths.push(path);
                (PrimitiveKind::Path, idx)
            }
            Primitive::Underline(mut underline) => {
                underline.order = order;
                let idx = self.underlines.len();
                self.underlines.push(underline);
                (PrimitiveKind::Underline, idx)
            }
            Primitive::MonochromeSprite(mut sprite) => {
                sprite.order = order;
                let idx = self.monochrome_sprites.len();
                self.monochrome_sprites.push(sprite);
                (PrimitiveKind::MonochromeSprite, idx)
            }
            Primitive::PolychromeSprite(mut sprite) => {
                sprite.order = order;
                let idx = self.polychrome_sprites.len();
                self.polychrome_sprites.push(sprite);
                (PrimitiveKind::PolychromeSprite, idx)
            }
            Primitive::Surface(mut surface) => {
                surface.order = order;
                let idx = self.surfaces.len();
                self.surfaces.push(surface);
                (PrimitiveKind::Surface, idx)
            }
        };
        self.paint_operations
            .push(PaintOperation::Primitive(kind, index));
        true
    }

    pub(crate) fn replay(&mut self, range: Range<usize>, prev_scene: &Scene) {
        for operation in &prev_scene.paint_operations[range] {
            match operation {
                PaintOperation::Primitive(kind, index) => {
                    let primitive = match kind {
                        PrimitiveKind::Shadow => Primitive::Shadow(prev_scene.shadows[*index]),
                        PrimitiveKind::Quad => Primitive::Quad(prev_scene.quads[*index]),
                        PrimitiveKind::Path => Primitive::Path(prev_scene.paths[*index].clone()),
                        PrimitiveKind::Underline => {
                            Primitive::Underline(prev_scene.underlines[*index])
                        }
                        PrimitiveKind::MonochromeSprite => {
                            let sprite = prev_scene.monochrome_sprites[*index];
                            // Animation ids index the scene they were inserted into.
                            if let Some(animation) =
                                prev_scene.animations.get(sprite.animation.0 as usize)
                            {
                                self.insert_animated_sprite(sprite, *animation);
                                continue;
                            }
                            Primitive::MonochromeSprite(sprite)
                        }
                        PrimitiveKind::PolychromeSprite => {
                            Primitive::PolychromeSprite(prev_scene.polychrome_sprites[*index])
                        }
                        PrimitiveKind::Surface => {
                            Primitive::Surface(prev_scene.surfaces[*index].clone())
                        }
                    };
                    self.insert_primitive(primitive);
                }
                PaintOperation::StartLayer(bounds) => self.push_layer(*bounds),
                PaintOperation::EndLayer => self.pop_layer(),
            }
        }
    }

    pub(crate) fn finish(&mut self) {
        let shadow_indices = sort_primitives(&mut self.shadows, |shadow| shadow.order);
        let quad_indices = sort_primitives(&mut self.quads, |quad| quad.order);
        let path_indices = sort_primitives(&mut self.paths, |path| path.order);
        let underline_indices = sort_primitives(&mut self.underlines, |underline| underline.order);
        let monochrome_sprite_indices = sort_primitives(&mut self.monochrome_sprites, |sprite| {
            (sprite.order, sprite.tile.texture_id.index)
        });
        let polychrome_sprite_indices = sort_primitives(&mut self.polychrome_sprites, |sprite| {
            (sprite.order, sprite.tile.texture_id.index)
        });
        let surface_indices = sort_primitives(&mut self.surfaces, |surface| surface.order);
        if [
            &shadow_indices,
            &quad_indices,
            &path_indices,
            &underline_indices,
            &monochrome_sprite_indices,
            &polychrome_sprite_indices,
            &surface_indices,
        ]
        .iter()
        .all(|mapping| mapping.is_none())
        {
            self.rebind_sprite_animations();
            return;
        }

        // Paint ranges retain insertion order, while backend arrays use batch order.
        // Keep each log entry attached to its primitive when those arrays move.
        for operation in &mut self.paint_operations {
            if let PaintOperation::Primitive(kind, index) = operation {
                let mapping = match kind {
                    PrimitiveKind::Shadow => &shadow_indices,
                    PrimitiveKind::Quad => &quad_indices,
                    PrimitiveKind::Path => &path_indices,
                    PrimitiveKind::Underline => &underline_indices,
                    PrimitiveKind::MonochromeSprite => &monochrome_sprite_indices,
                    PrimitiveKind::PolychromeSprite => &polychrome_sprite_indices,
                    PrimitiveKind::Surface => &surface_indices,
                };
                if let Some(mapping) = mapping {
                    *index = mapping[*index];
                }
            }
        }
        self.rebind_sprite_animations();
    }

    /// Sorting moved the sprites, so animations learn where theirs ended up.
    ///
    /// Paint-operation indices are rewritten separately by [`sort_primitives`].
    /// The animation id travels with the sprite, and this records the index
    /// after that permutation, including when the sprites were already ordered.
    fn rebind_sprite_animations(&mut self) {
        if self.animations.is_empty() {
            return;
        }
        for (index, sprite) in self.monochrome_sprites.iter().enumerate() {
            if let Some(animation) = self.animations.get_mut(sprite.animation.0 as usize) {
                animation.sprite_index = index;
            }
        }
    }

    #[cfg_attr(
        all(
            any(target_os = "linux", target_os = "freebsd"),
            not(any(feature = "x11", feature = "wayland"))
        ),
        allow(dead_code)
    )]
    pub fn batches(&self) -> impl Iterator<Item = PrimitiveBatch<'_>> {
        self.batch_iter(None)
    }

    fn batch_iter<'a>(&'a self, atlas: Option<&'a dyn crate::PlatformAtlas>) -> BatchIterator<'a> {
        BatchIterator {
            atlas,
            shadows: &self.shadows,
            shadows_start: 0,
            shadows_iter: self.shadows.iter().peekable(),
            quads: &self.quads,
            quads_start: 0,
            quads_iter: self.quads.iter().peekable(),
            paths: &self.paths,
            paths_start: 0,
            paths_iter: self.paths.iter().peekable(),
            underlines: &self.underlines,
            underlines_start: 0,
            underlines_iter: self.underlines.iter().peekable(),
            monochrome_sprites: &self.monochrome_sprites,
            monochrome_sprites_start: 0,
            monochrome_sprites_iter: self.monochrome_sprites.iter().peekable(),
            polychrome_sprites: &self.polychrome_sprites,
            polychrome_sprites_start: 0,
            polychrome_sprites_iter: self.polychrome_sprites.iter().peekable(),
            surfaces: &self.surfaces,
            surfaces_start: 0,
            surfaces_iter: self.surfaces.iter().peekable(),
        }
    }

    /// Batches only sprites whose allocation is still live in `atlas`.
    /// Returned slices reference the original instance arrays, preserving GPU offsets.
    pub fn batches_for_atlas<'a>(
        &'a self,
        atlas: &'a dyn crate::PlatformAtlas,
    ) -> impl Iterator<Item = PrimitiveBatch<'a>> {
        self.batch_iter(Some(atlas))
    }
}

fn sort_primitives<T, K: Ord>(primitives: &mut [T], key: impl Fn(&T) -> K) -> Option<Vec<usize>> {
    // Painting commonly produces batch order already, so keep that path allocation-free.
    if primitives.is_sorted_by_key(&key) {
        return None;
    }

    let mut permutation = (0..primitives.len()).collect::<Vec<_>>();
    permutation.sort_unstable_by_key(|&index| key(&primitives[index]));
    let mut index_mapping = vec![0; primitives.len()];
    for (sorted_index, &original_index) in permutation.iter().enumerate() {
        index_mapping[original_index] = sorted_index;
    }

    permutation.copy_from_slice(&index_mapping);
    for index in 0..primitives.len() {
        while permutation[index] != index {
            let destination = permutation[index];
            primitives.swap(index, destination);
            permutation.swap(index, destination);
        }
    }
    Some(index_mapping)
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Default)]
#[cfg_attr(
    all(
        any(target_os = "linux", target_os = "freebsd"),
        not(any(feature = "x11", feature = "wayland"))
    ),
    allow(dead_code)
)]
pub(crate) enum PrimitiveKind {
    Shadow,
    #[default]
    Quad,
    Path,
    Underline,
    MonochromeSprite,
    PolychromeSprite,
    Surface,
}

pub(crate) enum PaintOperation {
    Primitive(PrimitiveKind, usize),
    StartLayer(Bounds<ScaledPixels>),
    EndLayer,
}

#[derive(Clone)]
pub(crate) enum Primitive {
    Shadow(Shadow),
    Quad(Quad),
    Path(Path<ScaledPixels>),
    Underline(Underline),
    MonochromeSprite(MonochromeSprite),
    PolychromeSprite(PolychromeSprite),
    Surface(PaintSurface),
}

#[allow(missing_docs)]
impl Primitive {
    pub fn bounds(&self) -> &Bounds<ScaledPixels> {
        match self {
            Primitive::Shadow(shadow) => &shadow.bounds,
            Primitive::Quad(quad) => &quad.bounds,
            Primitive::Path(path) => &path.bounds,
            Primitive::Underline(underline) => &underline.bounds,
            Primitive::MonochromeSprite(sprite) => &sprite.bounds,
            Primitive::PolychromeSprite(sprite) => &sprite.bounds,
            Primitive::Surface(surface) => &surface.bounds,
        }
    }

    pub fn content_mask(&self) -> &ContentMask<ScaledPixels> {
        match self {
            Primitive::Shadow(shadow) => &shadow.content_mask,
            Primitive::Quad(quad) => &quad.content_mask,
            Primitive::Path(path) => &path.content_mask,
            Primitive::Underline(underline) => &underline.content_mask,
            Primitive::MonochromeSprite(sprite) => &sprite.content_mask,
            Primitive::PolychromeSprite(sprite) => &sprite.content_mask,
            Primitive::Surface(surface) => &surface.content_mask,
        }
    }
}

#[cfg_attr(
    all(
        any(target_os = "linux", target_os = "freebsd"),
        not(any(feature = "x11", feature = "wayland"))
    ),
    allow(dead_code)
)]
struct BatchIterator<'a> {
    atlas: Option<&'a dyn crate::PlatformAtlas>,
    shadows: &'a [Shadow],
    shadows_start: usize,
    shadows_iter: Peekable<slice::Iter<'a, Shadow>>,
    quads: &'a [Quad],
    quads_start: usize,
    quads_iter: Peekable<slice::Iter<'a, Quad>>,
    paths: &'a [Path<ScaledPixels>],
    paths_start: usize,
    paths_iter: Peekable<slice::Iter<'a, Path<ScaledPixels>>>,
    underlines: &'a [Underline],
    underlines_start: usize,
    underlines_iter: Peekable<slice::Iter<'a, Underline>>,
    monochrome_sprites: &'a [MonochromeSprite],
    monochrome_sprites_start: usize,
    monochrome_sprites_iter: Peekable<slice::Iter<'a, MonochromeSprite>>,
    polychrome_sprites: &'a [PolychromeSprite],
    polychrome_sprites_start: usize,
    polychrome_sprites_iter: Peekable<slice::Iter<'a, PolychromeSprite>>,
    surfaces: &'a [PaintSurface],
    surfaces_start: usize,
    surfaces_iter: Peekable<slice::Iter<'a, PaintSurface>>,
}

impl BatchIterator<'_> {
    fn next_batch_kind(&mut self) -> Option<(PrimitiveKind, (DrawOrder, PrimitiveKind))> {
        let atlas = self.atlas;
        if let Some(atlas) = atlas {
            while self
                .monochrome_sprites_iter
                .next_if(|sprite| !atlas.is_tile_live(&sprite.tile))
                .is_some()
            {
                self.monochrome_sprites_start += 1;
            }
            while self
                .polychrome_sprites_iter
                .next_if(|sprite| !atlas.is_tile_live(&sprite.tile))
                .is_some()
            {
                self.polychrome_sprites_start += 1;
            }
        }
        let mut orders_and_kinds = [
            (
                self.shadows_iter.peek().map(|s| s.order),
                PrimitiveKind::Shadow,
            ),
            (self.quads_iter.peek().map(|q| q.order), PrimitiveKind::Quad),
            (self.paths_iter.peek().map(|q| q.order), PrimitiveKind::Path),
            (
                self.underlines_iter.peek().map(|u| u.order),
                PrimitiveKind::Underline,
            ),
            (
                self.monochrome_sprites_iter.peek().map(|s| s.order),
                PrimitiveKind::MonochromeSprite,
            ),
            (
                self.polychrome_sprites_iter.peek().map(|s| s.order),
                PrimitiveKind::PolychromeSprite,
            ),
            (
                self.surfaces_iter.peek().map(|s| s.order),
                PrimitiveKind::Surface,
            ),
        ];
        orders_and_kinds.sort_by_key(|(order, kind)| (order.unwrap_or(u32::MAX), *kind));

        let first = orders_and_kinds[0];
        let second = orders_and_kinds[1];
        first.0?;
        Some((first.1, (second.0.unwrap_or(u32::MAX), second.1)))
    }
}

// Advance indices along the original arrays, including any entries skipped before a batch.
fn consume_batch_range<'a, T>(
    iter: &mut Peekable<slice::Iter<'a, T>>,
    start: &mut usize,
    mut can_batch: impl FnMut(&T, &T) -> bool,
) -> Range<usize> {
    let range_start = *start;
    let first = iter.next().unwrap();
    *start += 1;
    while iter.next_if(|item| can_batch(first, item)).is_some() {
        *start += 1;
    }
    range_start..*start
}

fn consume_sprite_range<'a, T>(
    iter: &mut Peekable<slice::Iter<'a, T>>,
    start: &mut usize,
    batch_kind: PrimitiveKind,
    max_order_and_kind: (DrawOrder, PrimitiveKind),
    atlas: Option<&dyn crate::PlatformAtlas>,
    properties: impl Fn(&T) -> (DrawOrder, &AtlasTile),
) -> (AtlasTextureId, Range<usize>) {
    let texture_id = properties(iter.peek().unwrap()).1.texture_id;
    let range = consume_batch_range(iter, start, |_, sprite| {
        let (order, tile) = properties(sprite);
        (order, batch_kind) < max_order_and_kind
            && tile.texture_id == texture_id
            && atlas.is_none_or(|atlas| atlas.is_tile_live(tile))
    });
    (texture_id, range)
}

impl<'a> Iterator for BatchIterator<'a> {
    type Item = PrimitiveBatch<'a>;

    fn next(&mut self) -> Option<Self::Item> {
        let (batch_kind, max_order_and_kind) = self.next_batch_kind()?;
        let batch = match batch_kind {
            PrimitiveKind::Shadow => {
                let range = consume_batch_range(
                    &mut self.shadows_iter,
                    &mut self.shadows_start,
                    |_, item| (item.order, batch_kind) < max_order_and_kind,
                );
                PrimitiveBatch::Shadows(&self.shadows[range])
            }
            PrimitiveKind::Quad => {
                let range = consume_batch_range(
                    &mut self.quads_iter,
                    &mut self.quads_start,
                    |first, item| {
                        first.blend_mode == 0
                            && item.blend_mode == 0
                            && (item.order, batch_kind) < max_order_and_kind
                    },
                );
                PrimitiveBatch::Quads(&self.quads[range])
            }
            PrimitiveKind::Path => {
                let range =
                    consume_batch_range(&mut self.paths_iter, &mut self.paths_start, |_, item| {
                        (item.order, batch_kind) < max_order_and_kind
                    });
                PrimitiveBatch::Paths(&self.paths[range])
            }
            PrimitiveKind::Underline => {
                let range = consume_batch_range(
                    &mut self.underlines_iter,
                    &mut self.underlines_start,
                    |_, item| (item.order, batch_kind) < max_order_and_kind,
                );
                PrimitiveBatch::Underlines(&self.underlines[range])
            }
            PrimitiveKind::MonochromeSprite => {
                let (texture_id, range) = consume_sprite_range(
                    &mut self.monochrome_sprites_iter,
                    &mut self.monochrome_sprites_start,
                    batch_kind,
                    max_order_and_kind,
                    self.atlas,
                    |sprite| (sprite.order, &sprite.tile),
                );
                PrimitiveBatch::MonochromeSprites {
                    texture_id,
                    sprites: &self.monochrome_sprites[range],
                }
            }
            PrimitiveKind::PolychromeSprite => {
                let (texture_id, range) = consume_sprite_range(
                    &mut self.polychrome_sprites_iter,
                    &mut self.polychrome_sprites_start,
                    batch_kind,
                    max_order_and_kind,
                    self.atlas,
                    |sprite| (sprite.order, &sprite.tile),
                );
                PrimitiveBatch::PolychromeSprites {
                    texture_id,
                    sprites: &self.polychrome_sprites[range],
                }
            }
            PrimitiveKind::Surface => {
                let range = consume_batch_range(
                    &mut self.surfaces_iter,
                    &mut self.surfaces_start,
                    |_, item| (item.order, batch_kind) < max_order_and_kind,
                );
                PrimitiveBatch::Surfaces(&self.surfaces[range])
            }
        };
        Some(batch)
    }
}

#[derive(Debug)]
#[cfg_attr(
    all(
        any(target_os = "linux", target_os = "freebsd"),
        not(any(feature = "x11", feature = "wayland"))
    ),
    allow(dead_code)
)]
#[allow(missing_docs)]
pub enum PrimitiveBatch<'a> {
    Shadows(&'a [Shadow]),
    Quads(&'a [Quad]),
    Paths(&'a [Path<ScaledPixels>]),
    Underlines(&'a [Underline]),
    MonochromeSprites {
        texture_id: AtlasTextureId,
        sprites: &'a [MonochromeSprite],
    },
    PolychromeSprites {
        texture_id: AtlasTextureId,
        sprites: &'a [PolychromeSprite],
    },
    // Consumed by native renderers; wgpu currently keeps this as an ignored batch.
    #[allow(dead_code)]
    Surfaces(&'a [PaintSurface]),
}

#[derive(Default, Debug, Copy, Clone)]
#[repr(C)]
#[allow(missing_docs)]
pub struct Quad {
    pub order: DrawOrder,
    pub border_style: BorderStyle,
    pub bounds: Bounds<ScaledPixels>,
    pub content_mask: ContentMask<ScaledPixels>,
    pub background: Background,
    pub border_color: Hsla,
    pub corner_radii: Corners<ScaledPixels>,
    pub border_widths: Edges<ScaledPixels>,
    pub continuous_corners: u32,
    pub _pad_before_transform: u32, // padding before transform
    pub transform: TransformationMatrix,
    pub blend_mode: u32,
    pub _pad_end: u32, // padding after blend_mode
}

impl From<Quad> for Primitive {
    fn from(quad: Quad) -> Self {
        Primitive::Quad(quad)
    }
}

#[derive(Debug, Copy, Clone)]
#[repr(C)]
#[allow(missing_docs)]
pub struct Underline {
    pub order: DrawOrder,
    pub pad: u32, // align to 8 bytes
    pub bounds: Bounds<ScaledPixels>,
    pub content_mask: ContentMask<ScaledPixels>,
    pub color: Hsla,
    pub thickness: ScaledPixels,
    pub wavy: PaddedBool32,
}

impl From<Underline> for Primitive {
    fn from(underline: Underline) -> Self {
        Primitive::Underline(underline)
    }
}

#[derive(Debug, Copy, Clone)]
#[repr(C)]
#[allow(missing_docs)]
pub struct Shadow {
    pub order: DrawOrder,
    pub blur_radius: ScaledPixels,
    pub bounds: Bounds<ScaledPixels>,
    pub corner_radii: Corners<ScaledPixels>,
    pub content_mask: ContentMask<ScaledPixels>,
    pub color: Hsla,
    pub inset: u32,
    pub _pad_end: u32,
}

impl From<Shadow> for Primitive {
    fn from(shadow: Shadow) -> Self {
        Primitive::Shadow(shadow)
    }
}

/// The blend mode to apply when rendering a quad.
#[derive(Default, Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[repr(C)]
pub enum BlendMode {
    /// Standard alpha blending (source over destination).
    #[default]
    Normal = 0,
    /// Darkens by multiplying source and destination colors.
    Multiply = 1,
    /// Lightens by applying the screen formula to source and destination colors.
    Screen = 2,
    /// Combines multiply and screen based on destination luminance.
    Overlay = 3,
    /// A softer version of overlay that produces gentler contrast.
    SoftLight = 4,
    /// Subtracts the darker color from the lighter color.
    Difference = 5,
}

/// The style of a border.
#[derive(Default, Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[repr(C)]
pub enum BorderStyle {
    /// A solid border.
    #[default]
    Solid = 0,
    /// A dashed border.
    Dashed = 1,
}

/// A data type representing a 2 dimensional transformation that can be applied to an element.
#[derive(Debug, Clone, Copy, PartialEq)]
#[repr(C)]
pub struct TransformationMatrix {
    /// 2x2 matrix containing rotation and scale,
    /// stored row-major
    pub rotation_scale: [[f32; 2]; 2],
    /// translation vector
    pub translation: [f32; 2],
}

impl Eq for TransformationMatrix {}

impl TransformationMatrix {
    /// The unit matrix, has no effect.
    pub fn unit() -> Self {
        Self {
            rotation_scale: [[1.0, 0.0], [0.0, 1.0]],
            translation: [0.0, 0.0],
        }
    }

    /// Move the origin by a given point
    pub fn translate(mut self, point: Point<ScaledPixels>) -> Self {
        self.compose(Self {
            rotation_scale: [[1.0, 0.0], [0.0, 1.0]],
            translation: [point.x.0, point.y.0],
        })
    }

    /// Clockwise rotation in radians around the origin
    pub fn rotate(self, angle: Radians) -> Self {
        self.compose(Self {
            rotation_scale: [
                [angle.0.cos(), -angle.0.sin()],
                [angle.0.sin(), angle.0.cos()],
            ],
            translation: [0.0, 0.0],
        })
    }

    /// Scale around the origin
    pub fn scale(self, size: Size<f32>) -> Self {
        self.compose(Self {
            rotation_scale: [[size.width, 0.0], [0.0, size.height]],
            translation: [0.0, 0.0],
        })
    }

    /// Perform matrix multiplication with another transformation
    /// to produce a new transformation that is the result of
    /// applying both transformations: first, `other`, then `self`.
    #[inline]
    pub fn compose(self, other: TransformationMatrix) -> TransformationMatrix {
        if other == Self::unit() {
            return self;
        }
        // Perform matrix multiplication
        TransformationMatrix {
            rotation_scale: [
                [
                    self.rotation_scale[0][0] * other.rotation_scale[0][0]
                        + self.rotation_scale[0][1] * other.rotation_scale[1][0],
                    self.rotation_scale[0][0] * other.rotation_scale[0][1]
                        + self.rotation_scale[0][1] * other.rotation_scale[1][1],
                ],
                [
                    self.rotation_scale[1][0] * other.rotation_scale[0][0]
                        + self.rotation_scale[1][1] * other.rotation_scale[1][0],
                    self.rotation_scale[1][0] * other.rotation_scale[0][1]
                        + self.rotation_scale[1][1] * other.rotation_scale[1][1],
                ],
            ],
            translation: [
                self.translation[0]
                    + self.rotation_scale[0][0] * other.translation[0]
                    + self.rotation_scale[0][1] * other.translation[1],
                self.translation[1]
                    + self.rotation_scale[1][0] * other.translation[0]
                    + self.rotation_scale[1][1] * other.translation[1],
            ],
        }
    }

    /// Apply transformation to a point, mainly useful for debugging
    pub fn apply(&self, point: Point<Pixels>) -> Point<Pixels> {
        let input = [point.x.0, point.y.0];
        let mut output = self.translation;
        for (i, output_cell) in output.iter_mut().enumerate() {
            for (k, input_cell) in input.iter().enumerate() {
                *output_cell += self.rotation_scale[i][k] * *input_cell;
            }
        }
        Point::new(output[0].into(), output[1].into())
    }
}

impl Default for TransformationMatrix {
    fn default() -> Self {
        Self::unit()
    }
}

#[derive(Copy, Clone, Debug)]
#[repr(C)]
#[allow(missing_docs)]
pub struct MonochromeSprite {
    pub order: DrawOrder,
    /// Refers to an animation of the scene this sprite was inserted into.
    ///
    /// A single `u32` so it occupies the sprite's existing padding and leaves
    /// the instance layout unchanged on every backend; renderers ignore it.
    pub animation: SpriteAnimationId,
    pub tile: AtlasTile,
    pub bounds: Bounds<ScaledPixels>,
    pub content_mask: ContentMask<ScaledPixels>,
    pub color: Hsla,
    pub transformation: TransformationMatrix,
}

/// Refers to an animation of the scene a sprite is inserted into.
///
/// A single `u32` so it fits the sprite's existing padding and leaves the
/// instance layout unchanged on every backend; renderers ignore it.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
#[repr(transparent)]
pub struct SpriteAnimationId(u32);

impl SpriteAnimationId {
    /// No animation.
    pub const NONE: Self = Self(u32::MAX);
}

impl Default for SpriteAnimationId {
    fn default() -> Self {
        Self::NONE
    }
}

/// A [`TimedTransformation`] the window applies to a sprite right before
/// every present, so the scene can be presented again without being redrawn.
#[derive(Copy, Clone, Debug)]
pub(crate) struct SpriteAnimation {
    transformation: TimedTransformation,
    center: Point<Pixels>,
    scale_factor: f32,
    base_color: Hsla,
    sprite_index: usize,
}

impl SpriteAnimation {
    pub(crate) fn new(
        transformation: TimedTransformation,
        center: Point<Pixels>,
        scale_factor: f32,
    ) -> Self {
        Self {
            transformation,
            center,
            scale_factor,
            base_color: Hsla::default(),
            sprite_index: usize::MAX,
        }
    }
}

impl From<MonochromeSprite> for Primitive {
    fn from(sprite: MonochromeSprite) -> Self {
        Primitive::MonochromeSprite(sprite)
    }
}

#[derive(Copy, Clone, Debug)]
#[repr(C)]
#[allow(missing_docs)]
pub struct PolychromeSprite {
    pub order: DrawOrder,
    pub pad: u32, // align to 8 bytes
    pub tile: AtlasTile,
    pub grayscale: PaddedBool32,
    pub opacity: f32,
    pub bounds: Bounds<ScaledPixels>,
    pub content_mask: ContentMask<ScaledPixels>,
    pub corner_radii: Corners<ScaledPixels>,
}

impl From<PolychromeSprite> for Primitive {
    fn from(sprite: PolychromeSprite) -> Self {
        Primitive::PolychromeSprite(sprite)
    }
}

#[derive(Clone, Debug)]
#[allow(missing_docs)]
pub struct PaintSurface {
    pub order: DrawOrder,
    pub bounds: Bounds<ScaledPixels>,
    pub content_mask: ContentMask<ScaledPixels>,
    #[cfg(target_os = "macos")]
    pub image_buffer: core_video::pixel_buffer::CVPixelBuffer,
}

impl From<PaintSurface> for Primitive {
    fn from(surface: PaintSurface) -> Self {
        Primitive::Surface(surface)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{bounds, point, px, size, transparent_black};
    use std::{
        mem::{align_of, size_of},
        time::Duration,
    };

    fn test_quad(blend_mode: BlendMode) -> Quad {
        let bounds = bounds(
            point(0.0.into(), 0.0.into()),
            size(10.0.into(), 10.0.into()),
        );
        Quad {
            bounds,
            content_mask: ContentMask { bounds },
            background: transparent_black().into(),
            blend_mode: blend_mode as u32,
            ..Default::default()
        }
    }

    #[test]
    fn non_normal_blend_mode_quads_are_not_batched() {
        let mut scene = Scene::default();
        scene.insert_primitive(test_quad(BlendMode::Normal));
        scene.insert_primitive(test_quad(BlendMode::Normal));
        scene.insert_primitive(test_quad(BlendMode::Multiply));
        scene.insert_primitive(test_quad(BlendMode::Normal));
        scene.insert_primitive(test_quad(BlendMode::Screen));
        scene.finish();

        let batch_lengths = scene
            .batches()
            .filter_map(|batch| match batch {
                PrimitiveBatch::Quads(quads) => Some(quads.len()),
                _ => None,
            })
            .collect::<Vec<_>>();

        assert_eq!(batch_lengths, vec![2, 1, 1, 1]);
    }

    #[test]
    fn sprite_batches_preserve_texture_and_primitive_order_boundaries() {
        let tile = |kind, index| AtlasTile {
            texture_id: AtlasTextureId { kind, index },
            tile_id: crate::TileId(0),
            padding: 0,
            bounds: Bounds::default(),
        };
        let monochrome = |order, index| MonochromeSprite {
            order,
            animation: SpriteAnimationId::NONE,
            tile: tile(crate::AtlasTextureKind::Monochrome, index),
            bounds: Bounds::default(),
            content_mask: ContentMask::default(),
            color: Hsla::default(),
            transformation: TransformationMatrix::default(),
        };
        let polychrome = |order| PolychromeSprite {
            order,
            pad: 0,
            tile: tile(crate::AtlasTextureKind::Polychrome, 0),
            grayscale: false.into(),
            opacity: 1.,
            bounds: Bounds::default(),
            content_mask: ContentMask::default(),
            corner_radii: Corners::default(),
        };
        let mut quad = test_quad(BlendMode::Normal);
        quad.order = 1;
        let scene = Scene {
            quads: vec![quad],
            monochrome_sprites: vec![
                monochrome(0, 1),
                monochrome(0, 1),
                monochrome(0, 2),
                monochrome(1, 2),
                monochrome(2, 2),
            ],
            polychrome_sprites: vec![polychrome(0), polychrome(1)],
            ..Scene::default()
        };
        let batches = scene
            .batches()
            .map(|batch| match batch {
                PrimitiveBatch::Quads(quads) => (PrimitiveKind::Quad, 0, quads.len()),
                PrimitiveBatch::MonochromeSprites {
                    texture_id,
                    sprites,
                } => (
                    PrimitiveKind::MonochromeSprite,
                    texture_id.index,
                    sprites.len(),
                ),
                PrimitiveBatch::PolychromeSprites {
                    texture_id,
                    sprites,
                } => (
                    PrimitiveKind::PolychromeSprite,
                    texture_id.index,
                    sprites.len(),
                ),
                _ => panic!("unexpected primitive batch"),
            })
            .collect::<Vec<_>>();
        assert_eq!(
            batches,
            [
                (PrimitiveKind::MonochromeSprite, 1, 2),
                (PrimitiveKind::MonochromeSprite, 2, 1),
                (PrimitiveKind::PolychromeSprite, 0, 1),
                (PrimitiveKind::Quad, 0, 1),
                (PrimitiveKind::MonochromeSprite, 2, 1),
                (PrimitiveKind::PolychromeSprite, 0, 1),
                (PrimitiveKind::MonochromeSprite, 2, 1),
            ]
        );
    }

    #[test]
    fn replay_keeps_insertion_identity_after_primitive_sorting() {
        let mut scene = Scene::default();
        for (x, y) in [(0., 0.), (5., 5.), (100., 0.)] {
            let mut quad = test_quad(BlendMode::Normal);
            quad.bounds.origin = point(x.into(), y.into());
            quad.content_mask.bounds = quad.bounds;
            scene.insert_primitive(quad);
        }
        let expected_bounds = scene.quads[1].bounds;
        assert_eq!(
            scene
                .quads
                .iter()
                .map(|quad| quad.order)
                .collect::<Vec<_>>(),
            [1, 2, 1]
        );

        scene.finish();
        scene.finish();
        let mut replayed = Scene::default();
        replayed.replay(1..2, &scene);

        assert_eq!(replayed.quads.len(), 1);
        assert_eq!(replayed.quads[0].bounds, expected_bounds);
    }

    #[test]
    fn replay_keeps_sprite_identity_after_texture_sorting() {
        let mut scene = Scene::default();
        for (x, texture_index) in [(0., 2), (100., 1)] {
            let bounds = bounds(point(x.into(), 0.0.into()), size(10.0.into(), 10.0.into()));
            scene.insert_primitive(PolychromeSprite {
                order: 0,
                pad: 0,
                tile: AtlasTile {
                    texture_id: AtlasTextureId {
                        index: texture_index,
                        kind: crate::AtlasTextureKind::Polychrome,
                    },
                    tile_id: crate::TileId(0),
                    padding: 0,
                    bounds: Bounds::default(),
                },
                grayscale: false.into(),
                opacity: 1.,
                bounds,
                content_mask: ContentMask { bounds },
                corner_radii: Corners::default(),
            });
        }
        scene.finish();
        let mut replayed = Scene::default();
        replayed.replay(0..1, &scene);

        assert_eq!(replayed.polychrome_sprites[0].tile.texture_id.index, 2);
    }

    #[test]
    fn animated_sprite_follows_index_sort_and_replay() {
        let tile = |index| AtlasTile {
            texture_id: AtlasTextureId {
                kind: crate::AtlasTextureKind::Monochrome,
                index,
            },
            tile_id: crate::TileId(0),
            padding: 0,
            bounds: Bounds::default(),
        };
        let sprite = |x: f32, y: f32, texture: u32| {
            let bounds = bounds(point(x.into(), y.into()), size(10.0.into(), 10.0.into()));
            MonochromeSprite {
                order: 0,
                animation: SpriteAnimationId::NONE,
                tile: tile(texture),
                bounds,
                content_mask: ContentMask { bounds },
                color: Hsla {
                    a: 1.,
                    ..Hsla::default()
                },
                transformation: TransformationMatrix::default(),
            }
        };

        let mut scene = Scene::default();
        // (0, 0) and (100, 0) share a draw order; (5, 5) sorts between them.
        // The animated sprite is inserted second so a stable insertion order
        // would leave it at index 1, and the index sort has to move it.
        scene.insert_primitive(sprite(0., 0., 1));
        scene.insert_animated_sprite(
            sprite(5., 5., 2),
            SpriteAnimation::new(
                crate::TimedTransformation::rotation(
                    Duration::from_secs(1),
                    crate::Transformation::default(),
                ),
                point(px(8.), px(8.)),
                1.,
            ),
        );
        scene.insert_primitive(sprite(100., 0., 1));
        let orders = scene
            .monochrome_sprites
            .iter()
            .map(|sprite| sprite.order)
            .collect::<Vec<_>>();
        assert_eq!(orders, [1, 2, 1]);

        scene.finish();
        let animated = scene
            .monochrome_sprites
            .iter()
            .position(|sprite| sprite.animation != SpriteAnimationId::NONE)
            .expect("animated sprite");
        assert_ne!(animated, 1);
        assert_eq!(scene.animations[0].sprite_index, animated);
        let paint_index = scene
            .paint_operations
            .iter()
            .position(|operation| {
                matches!(
                    operation,
                    PaintOperation::Primitive(PrimitiveKind::MonochromeSprite, index)
                        if *index == animated
                )
            })
            .expect("paint operation");

        // Replay the unadvanced scene. Advancing only the copy fails if the
        // animation was stored but not attached to the sprite.
        let mut replayed = Scene::default();
        replayed.replay(paint_index..paint_index + 1, &scene);
        assert_eq!(replayed.animations.len(), 1);
        replayed.finish();
        assert!(
            (replayed.monochrome_sprites[0].transformation.rotation_scale[0][0] - 1.).abs() < 1e-5
        );
        replayed.advance_animations(Duration::from_millis(250));
        let replayed_rotation = replayed.monochrome_sprites[0].transformation.rotation_scale;
        assert!(replayed_rotation[0][0].abs() < 1e-5);
        assert!((replayed_rotation[0][1] + 1.).abs() < 1e-5);
        assert!((replayed_rotation[1][0] - 1.).abs() < 1e-5);
        assert!(replayed_rotation[1][1].abs() < 1e-5);
    }

    #[test]
    fn replay_does_not_fade_an_advanced_sprite_again() {
        let bounds = bounds(
            point(0.0.into(), 0.0.into()),
            size(10.0.into(), 10.0.into()),
        );
        let mut scene = Scene::default();
        scene.insert_animated_sprite(
            MonochromeSprite {
                order: 0,
                animation: SpriteAnimationId::NONE,
                tile: AtlasTile {
                    texture_id: AtlasTextureId {
                        kind: crate::AtlasTextureKind::Monochrome,
                        index: 0,
                    },
                    tile_id: crate::TileId(0),
                    padding: 0,
                    bounds: Bounds::default(),
                },
                bounds,
                content_mask: ContentMask { bounds },
                color: Hsla {
                    a: 1.,
                    ..Hsla::default()
                },
                transformation: TransformationMatrix::default(),
            },
            SpriteAnimation::new(
                crate::TimedTransformation::new(Duration::from_secs(1))
                    .from(crate::Transformation::alpha(1.))
                    .to(crate::Transformation::alpha(0.)),
                point(px(0.), px(0.)),
                1.,
            ),
        );
        scene.finish();
        scene.advance_animations(Duration::from_millis(500));
        assert!((scene.monochrome_sprites[0].color.a - 0.5).abs() < 1e-5);

        let mut replayed = Scene::default();
        replayed.replay(0..scene.len(), &scene);
        replayed.finish();
        replayed.advance_animations(Duration::from_millis(500));
        assert!((replayed.monochrome_sprites[0].color.a - 0.5).abs() < 1e-5);
    }

    #[test]
    fn padded_bool_matches_gpu_scalar_layout() {
        assert_eq!(size_of::<PaddedBool32>(), size_of::<u32>());
        assert_eq!(align_of::<PaddedBool32>(), align_of::<u32>());
        // SpriteAnimationId occupies MonochromeSprite's former padding slot.
        assert_eq!(size_of::<SpriteAnimationId>(), size_of::<u32>());
        assert_eq!(align_of::<SpriteAnimationId>(), align_of::<u32>());
    }
}

#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub(crate) struct PathId(pub(crate) usize);

/// A line made up of a series of vertices and control points.
#[derive(Clone, Debug)]
#[allow(missing_docs)]
pub struct Path<P: Clone + Debug + Default + PartialEq> {
    pub(crate) id: PathId,
    pub order: DrawOrder,
    pub bounds: Bounds<P>,
    pub content_mask: ContentMask<P>,
    pub vertices: Vec<PathVertex<P>>,
    pub color: Background,
    start: Point<P>,
    current: Point<P>,
    contour_count: usize,
}

impl Path<Pixels> {
    /// Create a new path with the given starting point.
    pub fn new(start: Point<Pixels>) -> Self {
        Self {
            id: PathId(0),
            order: DrawOrder::default(),
            vertices: Vec::new(),
            start,
            current: start,
            bounds: Bounds {
                origin: start,
                size: Default::default(),
            },
            content_mask: Default::default(),
            color: Default::default(),
            contour_count: 0,
        }
    }

    /// Scale this path by the given factor.
    pub fn scale(&self, factor: f32) -> Path<ScaledPixels> {
        Path {
            id: self.id,
            order: self.order,
            bounds: self.bounds.scale(factor),
            content_mask: self.content_mask.scale(factor),
            vertices: self
                .vertices
                .iter()
                .map(|vertex| vertex.scale(factor))
                .collect(),
            start: self.start.map(|start| start.scale(factor)),
            current: self.current.scale(factor),
            contour_count: self.contour_count,
            color: self.color,
        }
    }

    /// Move the start, current point to the given point.
    pub fn move_to(&mut self, to: Point<Pixels>) {
        self.contour_count += 1;
        self.start = to;
        self.current = to;
    }

    /// Draw a straight line from the current point to the given point.
    pub fn line_to(&mut self, to: Point<Pixels>) {
        self.contour_count += 1;
        if self.contour_count > 1 {
            self.push_triangle(
                (self.start, self.current, to),
                (point(0., 1.), point(0., 1.), point(0., 1.)),
            );
        }
        self.current = to;
    }

    /// Draw a curve from the current point to the given point, using the given control point.
    pub fn curve_to(&mut self, to: Point<Pixels>, ctrl: Point<Pixels>) {
        self.contour_count += 1;
        if self.contour_count > 1 {
            self.push_triangle(
                (self.start, self.current, to),
                (point(0., 1.), point(0., 1.), point(0., 1.)),
            );
        }

        self.push_triangle(
            (self.current, ctrl, to),
            (point(0., 0.), point(0.5, 0.), point(1., 1.)),
        );
        self.current = to;
    }

    /// Push a triangle to the Path.
    pub fn push_triangle(
        &mut self,
        xy: (Point<Pixels>, Point<Pixels>, Point<Pixels>),
        st: (Point<f32>, Point<f32>, Point<f32>),
    ) {
        self.bounds = self
            .bounds
            .union(&Bounds {
                origin: xy.0,
                size: Default::default(),
            })
            .union(&Bounds {
                origin: xy.1,
                size: Default::default(),
            })
            .union(&Bounds {
                origin: xy.2,
                size: Default::default(),
            });

        self.vertices.push(PathVertex {
            xy_position: xy.0,
            st_position: st.0,
            content_mask: Default::default(),
        });
        self.vertices.push(PathVertex {
            xy_position: xy.1,
            st_position: st.1,
            content_mask: Default::default(),
        });
        self.vertices.push(PathVertex {
            xy_position: xy.2,
            st_position: st.2,
            content_mask: Default::default(),
        });
    }
}

impl<T> Path<T>
where
    T: Clone + Debug + Default + PartialEq + PartialOrd + Add<T, Output = T> + Sub<Output = T>,
{
    #[allow(unused)]
    #[allow(missing_docs)]
    pub fn clipped_bounds(&self) -> Bounds<T> {
        self.bounds.intersect(&self.content_mask.bounds)
    }
}

impl From<Path<ScaledPixels>> for Primitive {
    fn from(path: Path<ScaledPixels>) -> Self {
        Primitive::Path(path)
    }
}

#[derive(Clone, Debug)]
#[repr(C)]
#[allow(missing_docs)]
pub struct PathVertex<P: Clone + Debug + Default + PartialEq> {
    pub xy_position: Point<P>,
    pub st_position: Point<f32>,
    pub content_mask: ContentMask<P>,
}

#[allow(missing_docs)]
impl PathVertex<Pixels> {
    pub fn scale(&self, factor: f32) -> PathVertex<ScaledPixels> {
        PathVertex {
            xy_position: self.xy_position.scale(factor),
            st_position: self.st_position,
            content_mask: self.content_mask.scale(factor),
        }
    }
}

#[cfg(test)]
mod atlas_liveness_tests {
    use super::*;
    use crate::{
        AtlasKey, AtlasTextureKind, AtlasTileRegistry, DevicePixels, PlatformAtlas, TileId, size,
    };
    use std::borrow::Cow;

    struct Atlas(AtlasTileRegistry);

    impl PlatformAtlas for Atlas {
        fn get_or_insert_with<'a>(
            &self,
            _: &AtlasKey,
            _: &mut dyn FnMut() -> anyhow::Result<Option<(Size<DevicePixels>, Cow<'a, [u8]>)>>,
        ) -> anyhow::Result<Option<AtlasTile>> {
            unreachable!()
        }

        fn remove(&self, _: &AtlasKey) {
            unreachable!()
        }

        fn is_tile_live(&self, tile: &AtlasTile) -> bool {
            self.0.is_live(tile)
        }
    }

    fn tile(kind: AtlasTextureKind, id: u32) -> AtlasTile {
        AtlasTile {
            texture_id: AtlasTextureId { index: 0, kind },
            tile_id: TileId(id),
            padding: 0,
            bounds: Bounds::new(
                crate::Point::default(),
                size(DevicePixels(64), DevicePixels(64)),
            ),
        }
    }

    #[test]
    fn atlas_batches_skip_recycled_tiles_and_preserve_instance_offsets() -> anyhow::Result<()> {
        let mut atlas = Atlas(AtlasTileRegistry::default());
        let mut keeper = tile(AtlasTextureKind::Polychrome, 1);
        atlas.0.register(&mut keeper)?;
        let mut stale = tile(AtlasTextureKind::Polychrome, 2);
        atlas.0.register(&mut stale)?;
        atlas.0.remove(&stale);
        let mut replacement = tile(AtlasTextureKind::Polychrome, 2);
        atlas.0.register(&mut replacement)?;
        let sprite = |tile| PolychromeSprite {
            order: 0,
            pad: 0,
            tile,
            grayscale: false.into(),
            opacity: 1.,
            bounds: Bounds::default(),
            content_mask: ContentMask::default(),
            corner_radii: Corners::default(),
        };
        let scene = Scene {
            polychrome_sprites: vec![
                sprite(keeper),
                sprite(stale),
                sprite(replacement),
                sprite(stale),
                sprite(replacement),
            ],
            ..Scene::default()
        };
        let batches = scene.batches_for_atlas(&atlas).collect::<Vec<_>>();
        assert_eq!(batches.len(), 3);
        for (batch, index) in batches.iter().zip([0, 2, 4]) {
            let PrimitiveBatch::PolychromeSprites { sprites, .. } = batch else {
                panic!("expected sprites")
            };
            assert_eq!(sprites.len(), 1);
            assert!(std::ptr::eq(
                sprites.as_ptr(),
                scene.polychrome_sprites[index..].as_ptr()
            ));
        }
        Ok(())
    }

    #[test]
    fn atlas_batches_skip_stale_monochrome_sprites_after_clear() -> anyhow::Result<()> {
        let mut atlas = Atlas(AtlasTileRegistry::default());
        let mut stale = tile(AtlasTextureKind::Monochrome, 1);
        atlas.0.register(&mut stale)?;
        atlas.0.clear();
        let mut replacement = tile(AtlasTextureKind::Monochrome, 1);
        atlas.0.register(&mut replacement)?;
        let sprite = |tile| MonochromeSprite {
            order: 0,
            animation: SpriteAnimationId::NONE,
            tile,
            bounds: Bounds::default(),
            content_mask: ContentMask::default(),
            color: Hsla::default(),
            transformation: TransformationMatrix::default(),
        };
        let scene = Scene {
            monochrome_sprites: vec![sprite(stale), sprite(replacement)],
            ..Scene::default()
        };
        let batches = scene.batches_for_atlas(&atlas).collect::<Vec<_>>();
        let [PrimitiveBatch::MonochromeSprites { sprites, .. }] = batches.as_slice() else {
            panic!("expected one batch")
        };
        assert_eq!(sprites.len(), 1);
        assert_eq!(sprites[0].tile, replacement);
        Ok(())
    }
}

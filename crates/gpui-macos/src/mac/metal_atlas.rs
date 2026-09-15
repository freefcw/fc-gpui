use crate::{
    AtlasKey, AtlasTextureId, AtlasTextureKind, AtlasTextureList, AtlasTile, Bounds, DevicePixels,
    PlatformAtlas, Point, Size,
};
use anyhow::{Context as _, Result};
use collections::FxHashMap;
use etagere::BucketedAtlasAllocator;
use objc2::rc::Retained;
use objc2::runtime::ProtocolObject;
use objc2_metal::{
    MTLDevice, MTLPixelFormat, MTLRegion, MTLTexture, MTLTextureDescriptor, MTLTextureUsage,
};
use parking_lot::Mutex;
use std::borrow::Cow;
use std::ptr::NonNull;

type MetalDevice = Retained<ProtocolObject<dyn MTLDevice>>;
type MetalTexture = Retained<ProtocolObject<dyn MTLTexture>>;

pub(crate) struct MetalAtlas(Mutex<MetalAtlasState>);

impl MetalAtlas {
    pub(crate) fn new(device: MetalDevice, atlas_initial_size: Size<DevicePixels>) -> Self {
        MetalAtlas(Mutex::new(MetalAtlasState {
            device,
            atlas_initial_size,
            monochrome_textures: Default::default(),
            polychrome_textures: Default::default(),
            tiles_by_key: Default::default(),
        }))
    }

    pub(crate) fn metal_texture(&self, id: AtlasTextureId) -> MetalTexture {
        self.0.lock().texture(id).metal_texture.clone()
    }
}

struct MetalAtlasState {
    device: MetalDevice,
    atlas_initial_size: Size<DevicePixels>,
    monochrome_textures: AtlasTextureList<MetalAtlasTexture>,
    polychrome_textures: AtlasTextureList<MetalAtlasTexture>,
    tiles_by_key: FxHashMap<AtlasKey, AtlasTile>,
}

impl PlatformAtlas for MetalAtlas {
    fn get_or_insert_with<'a>(
        &self,
        key: &AtlasKey,
        build: &mut dyn FnMut() -> Result<Option<(Size<DevicePixels>, Cow<'a, [u8]>)>>,
    ) -> Result<Option<AtlasTile>> {
        let mut lock = self.0.lock();
        if let Some(tile) = lock.tiles_by_key.get(key) {
            Ok(Some(*tile))
        } else {
            let Some((size, bytes)) = build()? else {
                return Ok(None);
            };
            let tile = lock
                .allocate(size, key.texture_kind())
                .context("failed to allocate")?;
            let texture = lock.texture(tile.texture_id);
            texture.upload(tile.bounds, &bytes);
            lock.tiles_by_key.insert(key.clone(), tile);
            Ok(Some(tile))
        }
    }

    fn remove(&self, key: &AtlasKey) {
        let mut lock = self.0.lock();
        let Some(tile) = lock.tiles_by_key.remove(key) else {
            return;
        };
        let id = tile.texture_id;

        let textures = match id.kind {
            AtlasTextureKind::Monochrome => &mut lock.monochrome_textures,
            AtlasTextureKind::Polychrome => &mut lock.polychrome_textures,
        };

        let Some(texture_slot) = textures
            .textures
            .iter_mut()
            .find(|texture| texture.as_ref().is_some_and(|v| v.id == id))
        else {
            return;
        };

        if let Some(mut texture) = texture_slot.take() {
            texture.allocator.deallocate(tile.tile_id.into());
            texture.decrement_ref_count();

            if texture.is_unreferenced() {
                textures.free_list.push(id.index as usize);
            } else {
                *texture_slot = Some(texture);
            }
        }
    }
}

impl MetalAtlasState {
    fn allocate(
        &mut self,
        size: Size<DevicePixels>,
        texture_kind: AtlasTextureKind,
    ) -> Option<AtlasTile> {
        {
            let textures = match texture_kind {
                AtlasTextureKind::Monochrome => &mut self.monochrome_textures,
                AtlasTextureKind::Polychrome => &mut self.polychrome_textures,
            };

            if let Some(tile) = textures
                .iter_mut()
                .rev()
                .find_map(|texture| texture.allocate(size))
            {
                return Some(tile);
            }
        }

        let texture = self.push_texture(size, texture_kind);
        texture.allocate(size)
    }

    fn push_texture(
        &mut self,
        min_size: Size<DevicePixels>,
        kind: AtlasTextureKind,
    ) -> &mut MetalAtlasTexture {
        // Max texture size on all modern Apple GPUs. Anything bigger than that crashes in validateWithDevice.
        const MAX_ATLAS_SIZE: Size<DevicePixels> = Size {
            width: DevicePixels(16384),
            height: DevicePixels(16384),
        };
        let size = min_size.max(&self.atlas_initial_size).min(&MAX_ATLAS_SIZE);
        let texture_descriptor = MTLTextureDescriptor::new();
        unsafe {
            texture_descriptor.setWidth(size.width.into());
            texture_descriptor.setHeight(size.height.into());
        }
        let (pixel_format, usage) = match kind {
            AtlasTextureKind::Monochrome => (MTLPixelFormat::A8Unorm, MTLTextureUsage::ShaderRead),
            AtlasTextureKind::Polychrome => {
                (MTLPixelFormat::BGRA8Unorm, MTLTextureUsage::ShaderRead)
            }
        };
        texture_descriptor.setPixelFormat(pixel_format);
        texture_descriptor.setUsage(usage);
        let metal_texture = self
            .device
            .newTextureWithDescriptor(&texture_descriptor)
            .expect("atlas texture");

        let texture_list = match kind {
            AtlasTextureKind::Monochrome => &mut self.monochrome_textures,
            AtlasTextureKind::Polychrome => &mut self.polychrome_textures,
        };

        let index = texture_list.free_list.pop();

        let atlas_texture = MetalAtlasTexture {
            id: AtlasTextureId {
                index: index.unwrap_or(texture_list.textures.len()) as u32,
                kind,
            },
            allocator: etagere::BucketedAtlasAllocator::new(device_size_to_etagere(size)),
            metal_texture,
            live_atlas_keys: 0,
        };

        if let Some(ix) = index {
            texture_list.textures[ix] = Some(atlas_texture);
            texture_list.textures.get_mut(ix)
        } else {
            texture_list.textures.push(Some(atlas_texture));
            texture_list.textures.last_mut()
        }
        .unwrap()
        .as_mut()
        .unwrap()
    }

    fn texture(&self, id: AtlasTextureId) -> &MetalAtlasTexture {
        let textures = match id.kind {
            crate::AtlasTextureKind::Monochrome => &self.monochrome_textures,
            crate::AtlasTextureKind::Polychrome => &self.polychrome_textures,
        };
        textures[id.index as usize].as_ref().unwrap()
    }
}

struct MetalAtlasTexture {
    id: AtlasTextureId,
    allocator: BucketedAtlasAllocator,
    metal_texture: MetalTexture,
    live_atlas_keys: u32,
}

// Metal textures are usable from any thread; objc2's `ProtocolObject<dyn MTLTexture>`
// is not marked `Send`, but `PlatformAtlas` requires `Send + Sync`.
unsafe impl Send for MetalAtlasTexture {}

impl MetalAtlasTexture {
    fn allocate(&mut self, size: Size<DevicePixels>) -> Option<AtlasTile> {
        let allocation = self.allocator.allocate(device_size_to_etagere(size))?;
        let tile = AtlasTile {
            texture_id: self.id,
            tile_id: allocation.id.into(),
            bounds: Bounds {
                origin: etagere_point_to_device(allocation.rectangle.min),
                size,
            },
            padding: 0,
        };
        self.live_atlas_keys += 1;
        Some(tile)
    }

    fn upload(&self, bounds: Bounds<DevicePixels>, bytes: &[u8]) {
        let region = MTLRegion {
            origin: objc2_metal::MTLOrigin {
                x: bounds.origin.x.into(),
                y: bounds.origin.y.into(),
                z: 0,
            },
            size: objc2_metal::MTLSize {
                width: bounds.size.width.into(),
                height: bounds.size.height.into(),
                depth: 1,
            },
        };
        unsafe {
            self.metal_texture
                .replaceRegion_mipmapLevel_withBytes_bytesPerRow(
                    region,
                    0,
                    NonNull::new(bytes.as_ptr() as *mut _).expect("atlas upload bytes"),
                    bounds.size.width.to_bytes(self.bytes_per_pixel()) as usize,
                );
        }
    }

    fn bytes_per_pixel(&self) -> u8 {
        match self.metal_texture.pixelFormat() {
            MTLPixelFormat::A8Unorm | MTLPixelFormat::R8Unorm => 1,
            MTLPixelFormat::RGBA8Unorm | MTLPixelFormat::BGRA8Unorm => 4,
            _ => unimplemented!(),
        }
    }

    fn decrement_ref_count(&mut self) {
        self.live_atlas_keys -= 1;
    }

    fn is_unreferenced(&mut self) -> bool {
        self.live_atlas_keys == 0
    }
}

fn device_size_to_etagere(size: Size<DevicePixels>) -> etagere::Size {
    etagere::Size::new(size.width.into(), size.height.into())
}

fn etagere_point_to_device(point: etagere::Point) -> Point<DevicePixels> {
    Point {
        x: DevicePixels::from(point.x),
        y: DevicePixels::from(point.y),
    }
}

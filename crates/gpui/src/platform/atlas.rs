use super::{AtlasTextureId, AtlasTile, TileId};
use anyhow::{Context as _, Result};
use collections::FxHashMap;

/// CPU-side allocation identities for an atlas whose texture slots and regions can be reused.
#[doc(hidden)]
#[derive(Default)]
pub struct AtlasTileRegistry {
    next_token: u32,
    live: FxHashMap<u32, (AtlasTextureId, TileId)>,
    epoch: u64,
}

impl AtlasTileRegistry {
    /// Assigns a fresh identity without changing the GPU instance layout.
    pub fn register(&mut self, tile: &mut AtlasTile) -> Result<()> {
        self.next_token = self
            .next_token
            .checked_add(1)
            .context("atlas allocation identity space exhausted")?;
        tile.set_allocation_identity(self.next_token);
        self.live
            .insert(tile.allocation_identity(), (tile.texture_id, tile.tile_id));
        Ok(())
    }

    /// Tests allocation identity, allowing a sprite to use a cropped tile's bounds.
    pub fn is_live(&self, tile: &AtlasTile) -> bool {
        self.live.get(&tile.allocation_identity()) == Some(&(tile.texture_id, tile.tile_id))
    }

    /// Invalidates one allocation and any cached paints that may reference it.
    pub fn remove(&mut self, tile: &AtlasTile) {
        if self.live.remove(&tile.allocation_identity()).is_some() {
            self.epoch = self.epoch.wrapping_add(1);
        }
    }

    /// Invalidates all allocations while preserving the identity sequence.
    pub fn clear(&mut self) {
        if !self.live.is_empty() {
            self.live.clear();
            self.epoch = self.epoch.wrapping_add(1);
        }
    }

    /// Changes whenever an allocation is removed or the atlas is cleared.
    pub fn epoch(&self) -> u64 {
        self.epoch
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{AtlasTextureKind, Bounds, DevicePixels, Point, size};

    fn tile() -> AtlasTile {
        AtlasTile {
            texture_id: AtlasTextureId {
                index: 0,
                kind: AtlasTextureKind::Polychrome,
            },
            tile_id: TileId(0),
            padding: 0,
            bounds: Bounds::new(Point::default(), size(DevicePixels(64), DevicePixels(64))),
        }
    }

    #[test]
    fn reused_texture_and_region_never_revive_an_old_allocation() -> Result<()> {
        let mut registry = AtlasTileRegistry::default();
        let mut first = tile();
        registry.register(&mut first)?;
        registry.remove(&first);
        let mut replacement = tile();
        registry.register(&mut replacement)?;

        assert!(!registry.is_live(&first));
        assert!(registry.is_live(&replacement));
        assert_ne!(
            first.allocation_identity(),
            replacement.allocation_identity()
        );
        assert_eq!(registry.epoch(), 1);
        Ok(())
    }

    #[test]
    fn clearing_an_atlas_does_not_restart_allocation_identities() -> Result<()> {
        let mut registry = AtlasTileRegistry::default();
        let mut first = tile();
        registry.register(&mut first)?;
        registry.clear();
        let mut replacement = tile();
        registry.register(&mut replacement)?;

        assert!(!registry.is_live(&first));
        assert!(registry.is_live(&replacement));
        assert_eq!(registry.epoch(), 1);
        Ok(())
    }

    #[test]
    fn cropped_tiles_share_the_original_allocation_identity() -> Result<()> {
        let mut registry = AtlasTileRegistry::default();
        let mut original = tile();
        registry.register(&mut original)?;
        let mut cropped = original;
        cropped.bounds.size = size(DevicePixels(8), DevicePixels(8));
        assert!(registry.is_live(&cropped));
        registry.remove(&original);
        assert!(!registry.is_live(&cropped));
        Ok(())
    }
}

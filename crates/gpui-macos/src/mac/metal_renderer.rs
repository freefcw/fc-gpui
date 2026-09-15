use super::metal_atlas::MetalAtlas;
use crate::{
    AtlasTextureId, Background, Bounds, ContentMask, DevicePixels, MonochromeSprite, PaintSurface,
    Path, Point, PolychromeSprite, PrimitiveBatch, Quad, ScaledPixels, Scene, Shadow, Size,
    Surface, Underline, point, size,
};
use anyhow::Result;
use block2::RcBlock;
use core_foundation::base::TCFType;
use core_video::pixel_buffer::kCVPixelFormatType_420YpCbCr8BiPlanarFullRange;
use dispatch2::DispatchData;
use media::core_video::CVMetalTextureCache;
use objc2::rc::Retained;
use objc2::runtime::ProtocolObject;
use objc2_core_foundation::CGSize;
use objc2_foundation::{NSRange, NSString};
#[cfg(feature = "runtime_shaders")]
use objc2_metal::MTLCompileOptions;
use objc2_metal::{
    MTLBlendFactor, MTLBlendOperation, MTLBlitCommandEncoder, MTLBuffer, MTLClearColor,
    MTLCommandBuffer, MTLCommandEncoder, MTLCommandQueue, MTLCopyAllDevices, MTLDevice,
    MTLDrawable, MTLFunction, MTLLibrary, MTLLoadAction, MTLOrigin, MTLPixelFormat,
    MTLPrimitiveType, MTLRegion, MTLRenderCommandEncoder, MTLRenderPassColorAttachmentDescriptor,
    MTLRenderPassDescriptor, MTLRenderPipelineDescriptor, MTLRenderPipelineState, MTLResource,
    MTLResourceOptions, MTLSize, MTLStorageMode, MTLStoreAction, MTLTexture, MTLTextureDescriptor,
    MTLTextureType, MTLTextureUsage, MTLViewport,
};
use objc2_quartz_core::{CAAutoresizingMask, CAMetalDrawable, CAMetalLayer};
use parking_lot::Mutex;
use std::{cell::Cell, ffi::c_void, mem, ptr, ptr::NonNull, sync::Arc};

// Exported to metal
pub(crate) type PointF = crate::Point<f32>;

type MetalDevice = Retained<ProtocolObject<dyn MTLDevice>>;
type MetalBuffer = Retained<ProtocolObject<dyn MTLBuffer>>;
type MetalTexture = Retained<ProtocolObject<dyn MTLTexture>>;
type MetalCommandQueue = Retained<ProtocolObject<dyn MTLCommandQueue>>;
type MetalCommandBuffer = Retained<ProtocolObject<dyn MTLCommandBuffer>>;
type MetalRenderEncoder = Retained<ProtocolObject<dyn MTLRenderCommandEncoder>>;
type MetalPipelineState = Retained<ProtocolObject<dyn MTLRenderPipelineState>>;

#[cfg(not(feature = "runtime_shaders"))]
const SHADERS_METALLIB: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/shaders.metallib"));
#[cfg(feature = "runtime_shaders")]
const SHADERS_SOURCE_FILE: &str = include_str!(concat!(env!("OUT_DIR"), "/stitched_shaders.metal"));
// Use 4x MSAA, all devices support it.
// https://developer.apple.com/documentation/metal/mtldevice/1433355-supportstexturesamplecount
const PATH_SAMPLE_COUNT: u32 = 4;
const MIN_INSTANCE_BUFFER_SIZE: usize = 256 * 1024;
const DEFAULT_INSTANCE_BUFFER_SIZE: usize = 2 * 1024 * 1024;
const MAX_INSTANCE_BUFFER_SIZE: usize = 256 * 1024 * 1024;

pub type Context = Arc<Mutex<InstanceBufferPool>>;
pub type Renderer = MetalRenderer;

pub unsafe fn new_renderer(
    context: self::Context,
    _native_window: *mut c_void,
    _native_view: *mut c_void,
    _bounds: crate::Size<f32>,
    _transparent: bool,
    atlas_initial_size: crate::Size<crate::DevicePixels>,
) -> Renderer {
    MetalRenderer::new(context, atlas_initial_size)
}

pub(crate) struct InstanceBufferPool {
    buffer_size: usize,
    generation: u64,
    buffers: Vec<MetalBuffer>,
}

impl Default for InstanceBufferPool {
    fn default() -> Self {
        Self {
            buffer_size: DEFAULT_INSTANCE_BUFFER_SIZE,
            generation: 0,
            buffers: Vec::new(),
        }
    }
}

pub(crate) struct InstanceBuffer {
    metal_buffer: MetalBuffer,
    size: usize,
    generation: u64,
}

// Instance buffers are handed to Metal completed-handlers on a worker queue.
// objc2 does not mark `MTLBuffer` as `Send`.
unsafe impl Send for InstanceBuffer {}
unsafe impl Send for InstanceBufferPool {}

impl InstanceBufferPool {
    pub(crate) fn configure_initial_buffer_size(&mut self, buffer_size: usize) {
        let buffer_size = buffer_size.clamp(MIN_INSTANCE_BUFFER_SIZE, MAX_INSTANCE_BUFFER_SIZE);
        if self.buffer_size != buffer_size {
            self.reset(buffer_size);
        }
    }

    pub(crate) fn reset(&mut self, buffer_size: usize) {
        self.buffer_size = buffer_size.clamp(MIN_INSTANCE_BUFFER_SIZE, MAX_INSTANCE_BUFFER_SIZE);
        self.generation = self.generation.wrapping_add(1);
        self.buffers.clear();
    }

    /// Drop every idle GPU buffer currently held by the pool while keeping the
    /// configured `buffer_size` intact. Future calls to [`Self::acquire`] will
    /// allocate fresh buffers on demand.
    ///
    /// Buffers that are still in use by in-flight command buffers are also
    /// prevented from re-entering the pool once those commands complete.
    pub(crate) fn trim(&mut self) {
        self.generation = self.generation.wrapping_add(1);
        self.buffers.clear();
    }

    /// Returns `(idle_buffer_count, configured_buffer_size_bytes)` for
    /// diagnostics.
    pub(crate) fn stats(&self) -> (usize, usize) {
        (self.buffers.len(), self.buffer_size)
    }

    pub(crate) fn acquire(&mut self, device: &ProtocolObject<dyn MTLDevice>) -> InstanceBuffer {
        let buffer = self.buffers.pop().unwrap_or_else(|| {
            device
                .newBufferWithLength_options(
                    self.buffer_size,
                    MTLResourceOptions::StorageModeManaged,
                )
                .expect("instance buffer")
        });
        InstanceBuffer {
            metal_buffer: buffer,
            size: self.buffer_size,
            generation: self.generation,
        }
    }

    pub(crate) fn release(&mut self, buffer: InstanceBuffer) {
        if buffer.size == self.buffer_size && buffer.generation == self.generation {
            self.buffers.push(buffer.metal_buffer)
        }
    }
}

struct AssertSend<T>(T);

unsafe impl<T> Send for AssertSend<T> {}

pub(crate) struct MetalRenderer {
    device: MetalDevice,
    layer: AssertSend<Retained<CAMetalLayer>>,
    presents_with_transaction: bool,
    command_queue: MetalCommandQueue,
    paths_rasterization_pipeline_state: MetalPipelineState,
    path_sprites_pipeline_state: MetalPipelineState,
    shadows_pipeline_state: MetalPipelineState,
    quads_pipeline_state: MetalPipelineState,
    underlines_pipeline_state: MetalPipelineState,
    monochrome_sprites_pipeline_state: MetalPipelineState,
    polychrome_sprites_pipeline_state: MetalPipelineState,
    surfaces_pipeline_state: MetalPipelineState,
    unit_vertices: MetalBuffer,
    instance_buffer_pool: Arc<Mutex<InstanceBufferPool>>,
    sprite_atlas: Arc<MetalAtlas>,
    core_video_texture_cache: CVMetalTextureCache,
    path_intermediate_texture: Option<MetalTexture>,
    path_intermediate_msaa_texture: Option<MetalTexture>,
    framebuffer_copy_texture: Option<MetalTexture>,
    path_sample_count: u32,
}

#[repr(C)]
pub struct PathRasterizationVertex {
    pub xy_position: Point<ScaledPixels>,
    pub st_position: Point<f32>,
    pub color: Background,
    pub bounds: Bounds<ScaledPixels>,
}

impl MetalRenderer {
    pub fn new(
        instance_buffer_pool: Arc<Mutex<InstanceBufferPool>>,
        atlas_initial_size: crate::Size<crate::DevicePixels>,
    ) -> Self {
        // Prefer low‐power integrated GPUs on Intel Mac. On Apple
        // Silicon, there is only ever one GPU, so this is equivalent to
        // `MTLCreateSystemDefaultDevice()`.
        let all_devices = MTLCopyAllDevices();
        let mut devices: Vec<MetalDevice> = (0..all_devices.count())
            .map(|index| all_devices.objectAtIndex(index))
            .collect();
        devices.sort_by_key(|device| (device.isRemovable(), device.isLowPower()));
        let Some(device) = devices.pop() else {
            log::error!("unable to access a compatible graphics device");
            std::process::exit(1);
        };

        let layer = CAMetalLayer::layer();
        layer.setDevice(Some(&device));
        layer.setPixelFormat(MTLPixelFormat::BGRA8Unorm);
        layer.setFramebufferOnly(false);
        layer.as_super().setOpaque(false);
        layer.setMaximumDrawableCount(3);
        layer.setAllowsNextDrawableTimeout(false);
        layer.as_super().setNeedsDisplayOnBoundsChange(true);
        layer.as_super().setAutoresizingMask(
            CAAutoresizingMask::LayerWidthSizable | CAAutoresizingMask::LayerHeightSizable,
        );
        #[cfg(feature = "runtime_shaders")]
        let library = device
            .newLibraryWithSource_options_error(
                &NSString::from_str(SHADERS_SOURCE_FILE),
                Some(&MTLCompileOptions::new()),
            )
            .expect("error building metal library");
        #[cfg(not(feature = "runtime_shaders"))]
        let library = device
            .newLibraryWithData_error(&DispatchData::from_static_bytes(SHADERS_METALLIB))
            .expect("error building metal library");

        fn to_float2_bits(point: PointF) -> u64 {
            let mut output = point.y.to_bits() as u64;
            output <<= 32;
            output |= point.x.to_bits() as u64;
            output
        }

        let unit_vertices = [
            to_float2_bits(point(0., 0.)),
            to_float2_bits(point(1., 0.)),
            to_float2_bits(point(0., 1.)),
            to_float2_bits(point(0., 1.)),
            to_float2_bits(point(1., 0.)),
            to_float2_bits(point(1., 1.)),
        ];
        let unit_vertices = unsafe {
            device
                .newBufferWithBytes_length_options(
                    NonNull::new(unit_vertices.as_ptr() as *mut c_void).unwrap(),
                    mem::size_of_val(&unit_vertices),
                    MTLResourceOptions::StorageModeManaged,
                )
                .expect("unit vertices buffer")
        };

        let paths_rasterization_pipeline_state = build_path_rasterization_pipeline_state(
            &device,
            &library,
            "paths_rasterization",
            "path_rasterization_vertex",
            "path_rasterization_fragment",
            MTLPixelFormat::BGRA8Unorm,
            PATH_SAMPLE_COUNT,
        );
        let path_sprites_pipeline_state = build_path_sprite_pipeline_state(
            &device,
            &library,
            "path_sprites",
            "path_sprite_vertex",
            "path_sprite_fragment",
            MTLPixelFormat::BGRA8Unorm,
        );
        let shadows_pipeline_state = build_pipeline_state(
            &device,
            &library,
            "shadows",
            "shadow_vertex",
            "shadow_fragment",
            MTLPixelFormat::BGRA8Unorm,
        );
        let quads_pipeline_state = build_pipeline_state(
            &device,
            &library,
            "quads",
            "quad_vertex",
            "quad_fragment",
            MTLPixelFormat::BGRA8Unorm,
        );
        let underlines_pipeline_state = build_pipeline_state(
            &device,
            &library,
            "underlines",
            "underline_vertex",
            "underline_fragment",
            MTLPixelFormat::BGRA8Unorm,
        );
        let monochrome_sprites_pipeline_state = build_pipeline_state(
            &device,
            &library,
            "monochrome_sprites",
            "monochrome_sprite_vertex",
            "monochrome_sprite_fragment",
            MTLPixelFormat::BGRA8Unorm,
        );
        let polychrome_sprites_pipeline_state = build_pipeline_state(
            &device,
            &library,
            "polychrome_sprites",
            "polychrome_sprite_vertex",
            "polychrome_sprite_fragment",
            MTLPixelFormat::BGRA8Unorm,
        );
        let surfaces_pipeline_state = build_pipeline_state(
            &device,
            &library,
            "surfaces",
            "surface_vertex",
            "surface_fragment",
            MTLPixelFormat::BGRA8Unorm,
        );

        let command_queue = device.newCommandQueue().expect("metal command queue");
        let sprite_atlas = Arc::new(MetalAtlas::new(device.clone(), atlas_initial_size));
        let core_video_texture_cache =
            unsafe { CVMetalTextureCache::new(Retained::as_ptr(&device).cast()).unwrap() };

        Self {
            device,
            layer: AssertSend(layer),
            presents_with_transaction: false,
            command_queue,
            paths_rasterization_pipeline_state,
            path_sprites_pipeline_state,
            shadows_pipeline_state,
            quads_pipeline_state,
            underlines_pipeline_state,
            monochrome_sprites_pipeline_state,
            polychrome_sprites_pipeline_state,
            surfaces_pipeline_state,
            unit_vertices,
            instance_buffer_pool,
            sprite_atlas,
            core_video_texture_cache,
            path_intermediate_texture: None,
            path_intermediate_msaa_texture: None,
            framebuffer_copy_texture: None,
            path_sample_count: PATH_SAMPLE_COUNT,
        }
    }

    pub fn layer(&self) -> &CAMetalLayer {
        &self.layer.0
    }

    pub fn sprite_atlas(&self) -> &Arc<MetalAtlas> {
        &self.sprite_atlas
    }

    pub fn set_presents_with_transaction(&mut self, presents_with_transaction: bool) {
        self.presents_with_transaction = presents_with_transaction;
        self.layer()
            .setPresentsWithTransaction(presents_with_transaction);
    }

    pub fn update_drawable_size(&mut self, size: Size<DevicePixels>) {
        let drawable_size = CGSize {
            width: size.width.0 as f64,
            height: size.height.0 as f64,
        };
        self.layer().setDrawableSize(drawable_size);
        let device_pixels_size = Size {
            width: DevicePixels(drawable_size.width as i32),
            height: DevicePixels(drawable_size.height as i32),
        };
        self.update_path_intermediate_textures(device_pixels_size);
    }

    fn update_path_intermediate_textures(&mut self, size: Size<DevicePixels>) {
        // We are uncertain when this happens, but sometimes size can be 0 here. Most likely before
        // the layout pass on window creation. Zero-sized texture creation causes SIGABRT.
        // https://github.com/zed-industries/zed/issues/36229
        if size.width.0 <= 0 || size.height.0 <= 0 {
            self.path_intermediate_texture = None;
            self.path_intermediate_msaa_texture = None;
            self.framebuffer_copy_texture = None;
            return;
        }

        let texture_descriptor = MTLTextureDescriptor::new();
        unsafe {
            texture_descriptor.setWidth(size.width.0 as usize);
            texture_descriptor.setHeight(size.height.0 as usize);
        }
        texture_descriptor.setPixelFormat(MTLPixelFormat::BGRA8Unorm);
        texture_descriptor.setUsage(MTLTextureUsage::RenderTarget | MTLTextureUsage::ShaderRead);
        self.path_intermediate_texture = Some(
            self.device
                .newTextureWithDescriptor(&texture_descriptor)
                .expect("path intermediate texture"),
        );

        if self.path_sample_count > 1 {
            texture_descriptor.setTextureType(MTLTextureType::Type2DMultisample);
            texture_descriptor.setStorageMode(MTLStorageMode::Private);
            unsafe {
                texture_descriptor.setSampleCount(self.path_sample_count as usize);
            }
            self.path_intermediate_msaa_texture = Some(
                self.device
                    .newTextureWithDescriptor(&texture_descriptor)
                    .expect("path intermediate MSAA texture"),
            );
        } else {
            self.path_intermediate_msaa_texture = None;
        }

        let framebuffer_descriptor = MTLTextureDescriptor::new();
        unsafe {
            framebuffer_descriptor.setWidth(size.width.0 as usize);
            framebuffer_descriptor.setHeight(size.height.0 as usize);
        }
        framebuffer_descriptor.setPixelFormat(MTLPixelFormat::BGRA8Unorm);
        framebuffer_descriptor.setUsage(MTLTextureUsage::ShaderRead);
        framebuffer_descriptor.setStorageMode(MTLStorageMode::Private);
        self.framebuffer_copy_texture = Some(
            self.device
                .newTextureWithDescriptor(&framebuffer_descriptor)
                .expect("framebuffer copy texture"),
        );
    }

    pub fn update_transparency(&self, _transparent: bool) {
        // todo(mac)?
    }

    pub fn destroy(&self) {
        // nothing to do
    }

    pub fn draw(&mut self, scene: &Scene) {
        let layer = self.layer().retain();
        let viewport_size = layer.drawableSize();
        let viewport_size: Size<DevicePixels> = size(
            (viewport_size.width.ceil() as i32).into(),
            (viewport_size.height.ceil() as i32).into(),
        );
        let drawable = if let Some(drawable) = layer.nextDrawable() {
            drawable
        } else {
            log::error!(
                "failed to retrieve next drawable, drawable size: {:?}",
                viewport_size
            );
            return;
        };

        self.ensure_buffer_size(scene);

        loop {
            let mut instance_buffer = self.instance_buffer_pool.lock().acquire(&self.device);

            let command_buffer =
                self.draw_primitives(scene, &mut instance_buffer, &drawable, viewport_size);

            match command_buffer {
                Ok(command_buffer) => {
                    release_instance_buffer_on_complete(
                        &command_buffer,
                        self.instance_buffer_pool.clone(),
                        instance_buffer,
                    );

                    if self.presents_with_transaction {
                        command_buffer.commit();
                        command_buffer.waitUntilScheduled();
                        drawable.present();
                    } else {
                        command_buffer.presentDrawable(ProtocolObject::from_ref(&**drawable));
                        command_buffer.commit();
                    }
                    return;
                }
                Err(err) => {
                    log::error!(
                        "failed to render: {}. retrying with larger instance buffer size",
                        err
                    );
                    let mut instance_buffer_pool = self.instance_buffer_pool.lock();
                    let buffer_size = instance_buffer_pool.buffer_size;
                    if buffer_size >= MAX_INSTANCE_BUFFER_SIZE {
                        log::error!("instance buffer size grew too large: {}", buffer_size);
                        break;
                    }
                    instance_buffer_pool.reset(buffer_size * 2);
                    log::info!(
                        "increased instance buffer size to {}",
                        instance_buffer_pool.buffer_size
                    );
                }
            }
        }
    }

    #[cfg(any(test, feature = "test-support"))]
    pub fn render_scene_to_image(
        &mut self,
        scene: &Scene,
        viewport_size: Size<DevicePixels>,
    ) -> Result<image::RgbaImage> {
        if viewport_size.width.0 <= 0 || viewport_size.height.0 <= 0 {
            anyhow::bail!("invalid render_to_image size: {:?}", viewport_size);
        }

        self.update_path_intermediate_textures(viewport_size);
        self.ensure_buffer_size(scene);

        let texture_descriptor = MTLTextureDescriptor::new();
        unsafe {
            texture_descriptor.setWidth(viewport_size.width.0 as usize);
            texture_descriptor.setHeight(viewport_size.height.0 as usize);
        }
        texture_descriptor.setPixelFormat(MTLPixelFormat::BGRA8Unorm);
        texture_descriptor.setUsage(MTLTextureUsage::RenderTarget | MTLTextureUsage::ShaderRead);
        texture_descriptor.setStorageMode(MTLStorageMode::Managed);
        let target_texture = self
            .device
            .newTextureWithDescriptor(&texture_descriptor)
            .expect("render-to-image texture");

        loop {
            let mut instance_buffer = self.instance_buffer_pool.lock().acquire(&self.device);
            let command_buffer = self.draw_primitives_to_texture(
                scene,
                &mut instance_buffer,
                &target_texture,
                viewport_size,
            );

            match command_buffer {
                Ok(command_buffer) => {
                    release_instance_buffer_on_complete(
                        &command_buffer,
                        self.instance_buffer_pool.clone(),
                        instance_buffer,
                    );

                    let blit_encoder = command_buffer
                        .blitCommandEncoder()
                        .expect("blit command encoder");
                    blit_encoder.synchronizeResource(ProtocolObject::from_ref(&**target_texture));
                    blit_encoder.endEncoding();

                    command_buffer.commit();
                    command_buffer.waitUntilCompleted();

                    let width = viewport_size.width.0 as u32;
                    let height = viewport_size.height.0 as u32;
                    let bytes_per_row = width as usize * 4;
                    let mut pixels = vec![0; height as usize * bytes_per_row];
                    let region = MTLRegion {
                        origin: MTLOrigin { x: 0, y: 0, z: 0 },
                        size: MTLSize {
                            width: width as usize,
                            height: height as usize,
                            depth: 1,
                        },
                    };
                    unsafe {
                        target_texture.getBytes_bytesPerRow_fromRegion_mipmapLevel(
                            NonNull::new(pixels.as_mut_ptr().cast()).unwrap(),
                            bytes_per_row,
                            region,
                            0,
                        );
                    }

                    for pixel in pixels.chunks_exact_mut(4) {
                        pixel.swap(0, 2);
                    }

                    return image::RgbaImage::from_raw(width, height, pixels).ok_or_else(|| {
                        anyhow::anyhow!("failed to create image from rendered Metal texture")
                    });
                }
                Err(err) => {
                    log::error!(
                        "failed to render image: {}. retrying with larger instance buffer size",
                        err
                    );
                    let mut instance_buffer_pool = self.instance_buffer_pool.lock();
                    let buffer_size = instance_buffer_pool.buffer_size;
                    if buffer_size >= MAX_INSTANCE_BUFFER_SIZE {
                        anyhow::bail!("instance buffer size grew too large: {}", buffer_size);
                    }
                    instance_buffer_pool.reset(buffer_size * 2);
                }
            }
        }
    }

    fn ensure_buffer_size(&self, scene: &Scene) {
        const ALIGN: usize = 256;
        let align_up = |size: usize| size.div_ceil(ALIGN) * ALIGN;

        let total_path_vertices: usize = scene.paths.iter().map(|p| p.vertices.len()).sum();

        let estimated_bytes = align_up(mem::size_of::<Shadow>() * scene.shadows.len())
            + align_up(mem::size_of::<Quad>() * scene.quads.len())
            + align_up(mem::size_of::<PathRasterizationVertex>() * total_path_vertices)
            + align_up(mem::size_of::<PathSprite>() * scene.paths.len())
            + align_up(mem::size_of::<Underline>() * scene.underlines.len())
            + align_up(mem::size_of::<MonochromeSprite>() * scene.monochrome_sprites.len())
            + align_up(mem::size_of::<PolychromeSprite>() * scene.polychrome_sprites.len())
            + align_up(mem::size_of::<SurfaceBounds>()) * scene.surfaces.len();

        let required = estimated_bytes + estimated_bytes / 5;

        let mut pool = self.instance_buffer_pool.lock();
        if pool.buffer_size < required {
            let mut new_size = pool.buffer_size;
            while new_size < required {
                new_size *= 2;
            }
            new_size = new_size.min(MAX_INSTANCE_BUFFER_SIZE);
            pool.reset(new_size);
        }
    }

    fn draw_primitives(
        &mut self,
        scene: &Scene,
        instance_buffer: &mut InstanceBuffer,
        drawable: &ProtocolObject<dyn CAMetalDrawable>,
        viewport_size: Size<DevicePixels>,
    ) -> Result<MetalCommandBuffer> {
        let texture = drawable.texture();
        self.draw_primitives_to_texture(scene, instance_buffer, &texture, viewport_size)
    }

    fn draw_primitives_to_texture(
        &mut self,
        scene: &Scene,
        instance_buffer: &mut InstanceBuffer,
        texture: &ProtocolObject<dyn MTLTexture>,
        viewport_size: Size<DevicePixels>,
    ) -> Result<MetalCommandBuffer> {
        let command_buffer = self
            .command_queue
            .commandBuffer()
            .expect("metal command buffer");
        let alpha = if self.layer().as_super().isOpaque() {
            1.
        } else {
            0.
        };
        let mut instance_offset = 0;

        let mut command_encoder = new_command_encoder_for_texture(
            &command_buffer,
            texture,
            viewport_size,
            |color_attachment| {
                color_attachment.setLoadAction(MTLLoadAction::Clear);
                color_attachment.setClearColor(MTLClearColor {
                    red: 0.,
                    green: 0.,
                    blue: 0.,
                    alpha,
                });
            },
        );

        for batch in scene.batches() {
            let ok = match batch {
                PrimitiveBatch::Shadows(shadows) => self.draw_shadows(
                    shadows,
                    instance_buffer,
                    &mut instance_offset,
                    viewport_size,
                    &command_encoder,
                ),
                PrimitiveBatch::Quads(quads) => {
                    if quads.len() == 1 && quads[0].blend_mode != 0 {
                        command_encoder.endEncoding();
                        self.copy_framebuffer_to_texture(texture, &command_buffer);
                        command_encoder = new_command_encoder_for_texture(
                            &command_buffer,
                            texture,
                            viewport_size,
                            |color_attachment| {
                                color_attachment.setLoadAction(MTLLoadAction::Load);
                            },
                        );
                    }
                    self.draw_quads(
                        quads,
                        instance_buffer,
                        &mut instance_offset,
                        viewport_size,
                        &command_encoder,
                    )
                }
                PrimitiveBatch::Paths(paths) => {
                    command_encoder.endEncoding();

                    let did_draw = self.draw_paths_to_intermediate(
                        paths,
                        instance_buffer,
                        &mut instance_offset,
                        viewport_size,
                        &command_buffer,
                    );

                    command_encoder = new_command_encoder_for_texture(
                        &command_buffer,
                        texture,
                        viewport_size,
                        |color_attachment| {
                            color_attachment.setLoadAction(MTLLoadAction::Load);
                        },
                    );

                    if did_draw {
                        self.draw_paths_from_intermediate(
                            paths,
                            instance_buffer,
                            &mut instance_offset,
                            viewport_size,
                            &command_encoder,
                        )
                    } else {
                        false
                    }
                }
                PrimitiveBatch::Underlines(underlines) => self.draw_underlines(
                    underlines,
                    instance_buffer,
                    &mut instance_offset,
                    viewport_size,
                    &command_encoder,
                ),
                PrimitiveBatch::MonochromeSprites {
                    texture_id,
                    sprites,
                } => self.draw_monochrome_sprites(
                    texture_id,
                    sprites,
                    instance_buffer,
                    &mut instance_offset,
                    viewport_size,
                    &command_encoder,
                ),
                PrimitiveBatch::PolychromeSprites {
                    texture_id,
                    sprites,
                } => self.draw_polychrome_sprites(
                    texture_id,
                    sprites,
                    instance_buffer,
                    &mut instance_offset,
                    viewport_size,
                    &command_encoder,
                ),
                PrimitiveBatch::Surfaces(surfaces) => self.draw_surfaces(
                    surfaces,
                    instance_buffer,
                    &mut instance_offset,
                    viewport_size,
                    &command_encoder,
                ),
            };
            if !ok {
                command_encoder.endEncoding();
                anyhow::bail!(
                    "scene too large: {} paths, {} shadows, {} quads, {} underlines, {} mono, {} poly, {} surfaces",
                    scene.paths.len(),
                    scene.shadows.len(),
                    scene.quads.len(),
                    scene.underlines.len(),
                    scene.monochrome_sprites.len(),
                    scene.polychrome_sprites.len(),
                    scene.surfaces.len(),
                );
            }
        }

        command_encoder.endEncoding();

        instance_buffer.metal_buffer.didModifyRange(NSRange {
            location: 0,
            length: instance_offset,
        });
        Ok(command_buffer)
    }

    fn draw_paths_to_intermediate(
        &self,
        paths: &[Path<ScaledPixels>],
        instance_buffer: &mut InstanceBuffer,
        instance_offset: &mut usize,
        viewport_size: Size<DevicePixels>,
        command_buffer: &ProtocolObject<dyn MTLCommandBuffer>,
    ) -> bool {
        if paths.is_empty() {
            return true;
        }
        let Some(intermediate_texture) = &self.path_intermediate_texture else {
            return false;
        };

        let render_pass_descriptor = MTLRenderPassDescriptor::renderPassDescriptor();
        let color_attachment = unsafe {
            render_pass_descriptor
                .colorAttachments()
                .objectAtIndexedSubscript(0)
        };
        color_attachment.setLoadAction(MTLLoadAction::Clear);
        color_attachment.setClearColor(MTLClearColor {
            red: 0.,
            green: 0.,
            blue: 0.,
            alpha: 0.,
        });

        if let Some(msaa_texture) = &self.path_intermediate_msaa_texture {
            color_attachment.setTexture(Some(msaa_texture));
            color_attachment.setResolveTexture(Some(intermediate_texture));
            color_attachment.setStoreAction(MTLStoreAction::MultisampleResolve);
        } else {
            color_attachment.setTexture(Some(intermediate_texture));
            color_attachment.setStoreAction(MTLStoreAction::Store);
        }

        let command_encoder = command_buffer
            .renderCommandEncoderWithDescriptor(&render_pass_descriptor)
            .expect("path rasterization encoder");
        command_encoder.setRenderPipelineState(&self.paths_rasterization_pipeline_state);

        align_offset(instance_offset);
        let mut vertices = Vec::new();
        for path in paths {
            vertices.extend(path.vertices.iter().map(|v| PathRasterizationVertex {
                xy_position: v.xy_position,
                st_position: v.st_position,
                color: path.color,
                bounds: path.bounds.intersect(&path.content_mask.bounds),
            }));
        }
        let vertices_bytes_len = mem::size_of_val(vertices.as_slice());
        let next_offset = *instance_offset + vertices_bytes_len;
        if next_offset > instance_buffer.size {
            command_encoder.endEncoding();
            return false;
        }
        unsafe {
            command_encoder.setVertexBuffer_offset_atIndex(
                Some(&instance_buffer.metal_buffer),
                *instance_offset,
                PathRasterizationInputIndex::Vertices as usize,
            );
            set_vertex_bytes(
                &command_encoder,
                PathRasterizationInputIndex::ViewportSize as usize,
                &viewport_size,
            );
            command_encoder.setFragmentBuffer_offset_atIndex(
                Some(&instance_buffer.metal_buffer),
                *instance_offset,
                PathRasterizationInputIndex::Vertices as usize,
            );
        }
        let buffer_contents = instance_contents(&instance_buffer.metal_buffer, *instance_offset);
        unsafe {
            ptr::copy_nonoverlapping(
                vertices.as_ptr() as *const u8,
                buffer_contents,
                vertices_bytes_len,
            );
            command_encoder.drawPrimitives_vertexStart_vertexCount(
                MTLPrimitiveType::Triangle,
                0,
                vertices.len(),
            );
        }
        *instance_offset = next_offset;

        command_encoder.endEncoding();
        true
    }

    fn draw_shadows(
        &self,
        shadows: &[Shadow],
        instance_buffer: &mut InstanceBuffer,
        instance_offset: &mut usize,
        viewport_size: Size<DevicePixels>,
        command_encoder: &ProtocolObject<dyn MTLRenderCommandEncoder>,
    ) -> bool {
        if shadows.is_empty() {
            return true;
        }
        align_offset(instance_offset);

        command_encoder.setRenderPipelineState(&self.shadows_pipeline_state);
        unsafe {
            command_encoder.setVertexBuffer_offset_atIndex(
                Some(&self.unit_vertices),
                0,
                ShadowInputIndex::Vertices as usize,
            );
            command_encoder.setVertexBuffer_offset_atIndex(
                Some(&instance_buffer.metal_buffer),
                *instance_offset,
                ShadowInputIndex::Shadows as usize,
            );
            command_encoder.setFragmentBuffer_offset_atIndex(
                Some(&instance_buffer.metal_buffer),
                *instance_offset,
                ShadowInputIndex::Shadows as usize,
            );
            set_vertex_bytes(
                command_encoder,
                ShadowInputIndex::ViewportSize as usize,
                &viewport_size,
            );
        }

        let shadow_bytes_len = mem::size_of_val(shadows);
        let buffer_contents = instance_contents(&instance_buffer.metal_buffer, *instance_offset);

        let next_offset = *instance_offset + shadow_bytes_len;
        if next_offset > instance_buffer.size {
            return false;
        }

        unsafe {
            ptr::copy_nonoverlapping(
                shadows.as_ptr() as *const u8,
                buffer_contents,
                shadow_bytes_len,
            );
            command_encoder.drawPrimitives_vertexStart_vertexCount_instanceCount(
                MTLPrimitiveType::Triangle,
                0,
                6,
                shadows.len(),
            );
        }
        *instance_offset = next_offset;
        true
    }

    fn draw_quads(
        &self,
        quads: &[Quad],
        instance_buffer: &mut InstanceBuffer,
        instance_offset: &mut usize,
        viewport_size: Size<DevicePixels>,
        command_encoder: &ProtocolObject<dyn MTLRenderCommandEncoder>,
    ) -> bool {
        if quads.is_empty() {
            return true;
        }
        align_offset(instance_offset);

        command_encoder.setRenderPipelineState(&self.quads_pipeline_state);
        if let Some(framebuffer_copy_texture) = &self.framebuffer_copy_texture {
            unsafe {
                command_encoder.setFragmentTexture_atIndex(
                    Some(framebuffer_copy_texture),
                    QuadInputIndex::FramebufferTexture as usize,
                );
            }
        }
        unsafe {
            command_encoder.setVertexBuffer_offset_atIndex(
                Some(&self.unit_vertices),
                0,
                QuadInputIndex::Vertices as usize,
            );
            command_encoder.setVertexBuffer_offset_atIndex(
                Some(&instance_buffer.metal_buffer),
                *instance_offset,
                QuadInputIndex::Quads as usize,
            );
            command_encoder.setFragmentBuffer_offset_atIndex(
                Some(&instance_buffer.metal_buffer),
                *instance_offset,
                QuadInputIndex::Quads as usize,
            );
            set_vertex_bytes(
                command_encoder,
                QuadInputIndex::ViewportSize as usize,
                &viewport_size,
            );
        }

        let quad_bytes_len = mem::size_of_val(quads);
        let buffer_contents = instance_contents(&instance_buffer.metal_buffer, *instance_offset);

        let next_offset = *instance_offset + quad_bytes_len;
        if next_offset > instance_buffer.size {
            return false;
        }

        unsafe {
            ptr::copy_nonoverlapping(quads.as_ptr() as *const u8, buffer_contents, quad_bytes_len);
            command_encoder.drawPrimitives_vertexStart_vertexCount_instanceCount(
                MTLPrimitiveType::Triangle,
                0,
                6,
                quads.len(),
            );
        }
        *instance_offset = next_offset;
        true
    }

    fn copy_framebuffer_to_texture(
        &self,
        texture: &ProtocolObject<dyn MTLTexture>,
        command_buffer: &ProtocolObject<dyn MTLCommandBuffer>,
    ) -> bool {
        let Some(framebuffer_copy_texture) = &self.framebuffer_copy_texture else {
            return false;
        };
        let width = texture.width().min(framebuffer_copy_texture.width());
        let height = texture.height().min(framebuffer_copy_texture.height());
        if width == 0 || height == 0 {
            return false;
        }

        let blit_encoder = command_buffer
            .blitCommandEncoder()
            .expect("framebuffer blit encoder");
        unsafe {
            blit_encoder.copyFromTexture_sourceSlice_sourceLevel_sourceOrigin_sourceSize_toTexture_destinationSlice_destinationLevel_destinationOrigin(
                texture,
                0,
                0,
                MTLOrigin { x: 0, y: 0, z: 0 },
                MTLSize {
                    width,
                    height,
                    depth: 1,
                },
                framebuffer_copy_texture,
                0,
                0,
                MTLOrigin { x: 0, y: 0, z: 0 },
            );
        }
        blit_encoder.endEncoding();
        true
    }

    fn draw_paths_from_intermediate(
        &self,
        paths: &[Path<ScaledPixels>],
        instance_buffer: &mut InstanceBuffer,
        instance_offset: &mut usize,
        viewport_size: Size<DevicePixels>,
        command_encoder: &ProtocolObject<dyn MTLRenderCommandEncoder>,
    ) -> bool {
        let Some(first_path) = paths.first() else {
            return true;
        };

        let Some(ref intermediate_texture) = self.path_intermediate_texture else {
            return false;
        };

        command_encoder.setRenderPipelineState(&self.path_sprites_pipeline_state);
        unsafe {
            command_encoder.setVertexBuffer_offset_atIndex(
                Some(&self.unit_vertices),
                0,
                SpriteInputIndex::Vertices as usize,
            );
            set_vertex_bytes(
                command_encoder,
                SpriteInputIndex::ViewportSize as usize,
                &viewport_size,
            );
            command_encoder.setFragmentTexture_atIndex(
                Some(intermediate_texture),
                SpriteInputIndex::AtlasTexture as usize,
            );
        }

        // When copying paths from the intermediate texture to the drawable,
        // each pixel must only be copied once, in case of transparent paths.
        //
        // If all paths have the same draw order, then their bounds are all
        // disjoint, so we can copy each path's bounds individually. If this
        // batch combines different draw orders, we perform a single copy
        // for a minimal spanning rect.
        let sprites;
        if paths.last().unwrap().order == first_path.order {
            sprites = paths
                .iter()
                .map(|path| PathSprite {
                    bounds: path.clipped_bounds(),
                })
                .collect();
        } else {
            let mut bounds = first_path.clipped_bounds();
            for path in paths.iter().skip(1) {
                bounds = bounds.union(&path.clipped_bounds());
            }
            sprites = vec![PathSprite { bounds }];
        }

        align_offset(instance_offset);
        let sprite_bytes_len = mem::size_of_val(sprites.as_slice());
        let next_offset = *instance_offset + sprite_bytes_len;
        if next_offset > instance_buffer.size {
            return false;
        }

        unsafe {
            command_encoder.setVertexBuffer_offset_atIndex(
                Some(&instance_buffer.metal_buffer),
                *instance_offset,
                SpriteInputIndex::Sprites as usize,
            );
        }

        let buffer_contents = instance_contents(&instance_buffer.metal_buffer, *instance_offset);
        unsafe {
            ptr::copy_nonoverlapping(
                sprites.as_ptr() as *const u8,
                buffer_contents,
                sprite_bytes_len,
            );
            command_encoder.drawPrimitives_vertexStart_vertexCount_instanceCount(
                MTLPrimitiveType::Triangle,
                0,
                6,
                sprites.len(),
            );
        }
        *instance_offset = next_offset;

        true
    }

    fn draw_underlines(
        &self,
        underlines: &[Underline],
        instance_buffer: &mut InstanceBuffer,
        instance_offset: &mut usize,
        viewport_size: Size<DevicePixels>,
        command_encoder: &ProtocolObject<dyn MTLRenderCommandEncoder>,
    ) -> bool {
        if underlines.is_empty() {
            return true;
        }
        align_offset(instance_offset);

        command_encoder.setRenderPipelineState(&self.underlines_pipeline_state);
        unsafe {
            command_encoder.setVertexBuffer_offset_atIndex(
                Some(&self.unit_vertices),
                0,
                UnderlineInputIndex::Vertices as usize,
            );
            command_encoder.setVertexBuffer_offset_atIndex(
                Some(&instance_buffer.metal_buffer),
                *instance_offset,
                UnderlineInputIndex::Underlines as usize,
            );
            command_encoder.setFragmentBuffer_offset_atIndex(
                Some(&instance_buffer.metal_buffer),
                *instance_offset,
                UnderlineInputIndex::Underlines as usize,
            );
            set_vertex_bytes(
                command_encoder,
                UnderlineInputIndex::ViewportSize as usize,
                &viewport_size,
            );
        }

        let underline_bytes_len = mem::size_of_val(underlines);
        let buffer_contents = instance_contents(&instance_buffer.metal_buffer, *instance_offset);

        let next_offset = *instance_offset + underline_bytes_len;
        if next_offset > instance_buffer.size {
            return false;
        }

        unsafe {
            ptr::copy_nonoverlapping(
                underlines.as_ptr() as *const u8,
                buffer_contents,
                underline_bytes_len,
            );
            command_encoder.drawPrimitives_vertexStart_vertexCount_instanceCount(
                MTLPrimitiveType::Triangle,
                0,
                6,
                underlines.len(),
            );
        }
        *instance_offset = next_offset;
        true
    }

    fn draw_monochrome_sprites(
        &self,
        texture_id: AtlasTextureId,
        sprites: &[MonochromeSprite],
        instance_buffer: &mut InstanceBuffer,
        instance_offset: &mut usize,
        viewport_size: Size<DevicePixels>,
        command_encoder: &ProtocolObject<dyn MTLRenderCommandEncoder>,
    ) -> bool {
        if sprites.is_empty() {
            return true;
        }
        align_offset(instance_offset);

        let sprite_bytes_len = mem::size_of_val(sprites);
        let buffer_contents = instance_contents(&instance_buffer.metal_buffer, *instance_offset);

        let next_offset = *instance_offset + sprite_bytes_len;
        if next_offset > instance_buffer.size {
            return false;
        }

        let texture = self.sprite_atlas.metal_texture(texture_id);
        let texture_size = size(
            DevicePixels(texture.width() as i32),
            DevicePixels(texture.height() as i32),
        );
        command_encoder.setRenderPipelineState(&self.monochrome_sprites_pipeline_state);
        unsafe {
            command_encoder.setVertexBuffer_offset_atIndex(
                Some(&self.unit_vertices),
                0,
                SpriteInputIndex::Vertices as usize,
            );
            command_encoder.setVertexBuffer_offset_atIndex(
                Some(&instance_buffer.metal_buffer),
                *instance_offset,
                SpriteInputIndex::Sprites as usize,
            );
            set_vertex_bytes(
                command_encoder,
                SpriteInputIndex::ViewportSize as usize,
                &viewport_size,
            );
            set_vertex_bytes(
                command_encoder,
                SpriteInputIndex::AtlasTextureSize as usize,
                &texture_size,
            );
            command_encoder.setFragmentBuffer_offset_atIndex(
                Some(&instance_buffer.metal_buffer),
                *instance_offset,
                SpriteInputIndex::Sprites as usize,
            );
            command_encoder.setFragmentTexture_atIndex(
                Some(&texture),
                SpriteInputIndex::AtlasTexture as usize,
            );
            ptr::copy_nonoverlapping(
                sprites.as_ptr() as *const u8,
                buffer_contents,
                sprite_bytes_len,
            );
            command_encoder.drawPrimitives_vertexStart_vertexCount_instanceCount(
                MTLPrimitiveType::Triangle,
                0,
                6,
                sprites.len(),
            );
        }
        *instance_offset = next_offset;
        true
    }

    fn draw_polychrome_sprites(
        &self,
        texture_id: AtlasTextureId,
        sprites: &[PolychromeSprite],
        instance_buffer: &mut InstanceBuffer,
        instance_offset: &mut usize,
        viewport_size: Size<DevicePixels>,
        command_encoder: &ProtocolObject<dyn MTLRenderCommandEncoder>,
    ) -> bool {
        if sprites.is_empty() {
            return true;
        }
        align_offset(instance_offset);

        let texture = self.sprite_atlas.metal_texture(texture_id);
        let texture_size = size(
            DevicePixels(texture.width() as i32),
            DevicePixels(texture.height() as i32),
        );
        command_encoder.setRenderPipelineState(&self.polychrome_sprites_pipeline_state);
        unsafe {
            command_encoder.setVertexBuffer_offset_atIndex(
                Some(&self.unit_vertices),
                0,
                SpriteInputIndex::Vertices as usize,
            );
            command_encoder.setVertexBuffer_offset_atIndex(
                Some(&instance_buffer.metal_buffer),
                *instance_offset,
                SpriteInputIndex::Sprites as usize,
            );
            set_vertex_bytes(
                command_encoder,
                SpriteInputIndex::ViewportSize as usize,
                &viewport_size,
            );
            set_vertex_bytes(
                command_encoder,
                SpriteInputIndex::AtlasTextureSize as usize,
                &texture_size,
            );
            command_encoder.setFragmentBuffer_offset_atIndex(
                Some(&instance_buffer.metal_buffer),
                *instance_offset,
                SpriteInputIndex::Sprites as usize,
            );
            command_encoder.setFragmentTexture_atIndex(
                Some(&texture),
                SpriteInputIndex::AtlasTexture as usize,
            );
        }

        let sprite_bytes_len = mem::size_of_val(sprites);
        let buffer_contents = instance_contents(&instance_buffer.metal_buffer, *instance_offset);

        let next_offset = *instance_offset + sprite_bytes_len;
        if next_offset > instance_buffer.size {
            return false;
        }

        unsafe {
            ptr::copy_nonoverlapping(
                sprites.as_ptr() as *const u8,
                buffer_contents,
                sprite_bytes_len,
            );
            command_encoder.drawPrimitives_vertexStart_vertexCount_instanceCount(
                MTLPrimitiveType::Triangle,
                0,
                6,
                sprites.len(),
            );
        }
        *instance_offset = next_offset;
        true
    }

    fn draw_surfaces(
        &mut self,
        surfaces: &[PaintSurface],
        instance_buffer: &mut InstanceBuffer,
        instance_offset: &mut usize,
        viewport_size: Size<DevicePixels>,
        command_encoder: &ProtocolObject<dyn MTLRenderCommandEncoder>,
    ) -> bool {
        command_encoder.setRenderPipelineState(&self.surfaces_pipeline_state);
        unsafe {
            command_encoder.setVertexBuffer_offset_atIndex(
                Some(&self.unit_vertices),
                0,
                SurfaceInputIndex::Vertices as usize,
            );
            set_vertex_bytes(
                command_encoder,
                SurfaceInputIndex::ViewportSize as usize,
                &viewport_size,
            );
        }

        for surface in surfaces {
            let texture_size = size(
                DevicePixels::from(surface.image_buffer.get_width() as i32),
                DevicePixels::from(surface.image_buffer.get_height() as i32),
            );

            assert_eq!(
                surface.image_buffer.get_pixel_format(),
                kCVPixelFormatType_420YpCbCr8BiPlanarFullRange
            );

            let y_texture = unsafe {
                self.core_video_texture_cache
                    .create_texture_from_image(
                        surface.image_buffer.as_concrete_TypeRef(),
                        ptr::null(),
                        MTLPixelFormat::R8Unorm.0,
                        surface.image_buffer.get_width_of_plane(0),
                        surface.image_buffer.get_height_of_plane(0),
                        0,
                    )
                    .unwrap()
            };
            let cb_cr_texture = unsafe {
                self.core_video_texture_cache
                    .create_texture_from_image(
                        surface.image_buffer.as_concrete_TypeRef(),
                        ptr::null(),
                        MTLPixelFormat::RG8Unorm.0,
                        surface.image_buffer.get_width_of_plane(1),
                        surface.image_buffer.get_height_of_plane(1),
                        1,
                    )
                    .unwrap()
            };

            align_offset(instance_offset);
            let next_offset = *instance_offset + mem::size_of::<Surface>();
            if next_offset > instance_buffer.size {
                return false;
            }

            let y_mtl = retain_mtl_texture(y_texture.as_texture_ptr());
            let cb_cr_mtl = retain_mtl_texture(cb_cr_texture.as_texture_ptr());
            unsafe {
                command_encoder.setVertexBuffer_offset_atIndex(
                    Some(&instance_buffer.metal_buffer),
                    *instance_offset,
                    SurfaceInputIndex::Surfaces as usize,
                );
                set_vertex_bytes(
                    command_encoder,
                    SurfaceInputIndex::TextureSize as usize,
                    &texture_size,
                );
                command_encoder
                    .setFragmentTexture_atIndex(Some(&y_mtl), SurfaceInputIndex::YTexture as usize);
                command_encoder.setFragmentTexture_atIndex(
                    Some(&cb_cr_mtl),
                    SurfaceInputIndex::CbCrTexture as usize,
                );

                let buffer_contents =
                    instance_contents(&instance_buffer.metal_buffer, *instance_offset)
                        as *mut SurfaceBounds;
                ptr::write(
                    buffer_contents,
                    SurfaceBounds {
                        bounds: surface.bounds,
                        content_mask: surface.content_mask,
                    },
                );

                command_encoder.drawPrimitives_vertexStart_vertexCount(
                    MTLPrimitiveType::Triangle,
                    0,
                    6,
                );
            }
            *instance_offset = next_offset;
        }
        true
    }
}

fn new_command_encoder_for_texture(
    command_buffer: &ProtocolObject<dyn MTLCommandBuffer>,
    texture: &ProtocolObject<dyn MTLTexture>,
    viewport_size: Size<DevicePixels>,
    configure_color_attachment: impl Fn(&MTLRenderPassColorAttachmentDescriptor),
) -> MetalRenderEncoder {
    let render_pass_descriptor = MTLRenderPassDescriptor::renderPassDescriptor();
    let color_attachment = unsafe {
        render_pass_descriptor
            .colorAttachments()
            .objectAtIndexedSubscript(0)
    };
    color_attachment.setTexture(Some(texture));
    color_attachment.setStoreAction(MTLStoreAction::Store);
    configure_color_attachment(&color_attachment);

    let command_encoder = command_buffer
        .renderCommandEncoderWithDescriptor(&render_pass_descriptor)
        .expect("render command encoder");
    command_encoder.setViewport(MTLViewport {
        originX: 0.0,
        originY: 0.0,
        width: i32::from(viewport_size.width) as f64,
        height: i32::from(viewport_size.height) as f64,
        znear: 0.0,
        zfar: 1.0,
    });
    command_encoder
}

fn build_pipeline_state(
    device: &ProtocolObject<dyn MTLDevice>,
    library: &ProtocolObject<dyn MTLLibrary>,
    label: &str,
    vertex_fn_name: &str,
    fragment_fn_name: &str,
    pixel_format: MTLPixelFormat,
) -> MetalPipelineState {
    let vertex_fn = library_function(library, vertex_fn_name);
    let fragment_fn = library_function(library, fragment_fn_name);

    let descriptor = MTLRenderPipelineDescriptor::new();
    descriptor.setLabel(Some(&NSString::from_str(label)));
    descriptor.setVertexFunction(Some(&vertex_fn));
    descriptor.setFragmentFunction(Some(&fragment_fn));
    let color_attachment = unsafe { descriptor.colorAttachments().objectAtIndexedSubscript(0) };
    color_attachment.setPixelFormat(pixel_format);
    color_attachment.setBlendingEnabled(true);
    color_attachment.setRgbBlendOperation(MTLBlendOperation::Add);
    color_attachment.setAlphaBlendOperation(MTLBlendOperation::Add);
    color_attachment.setSourceRGBBlendFactor(MTLBlendFactor::SourceAlpha);
    color_attachment.setSourceAlphaBlendFactor(MTLBlendFactor::One);
    color_attachment.setDestinationRGBBlendFactor(MTLBlendFactor::OneMinusSourceAlpha);
    color_attachment.setDestinationAlphaBlendFactor(MTLBlendFactor::One);

    device
        .newRenderPipelineStateWithDescriptor_error(&descriptor)
        .expect("could not create render pipeline state")
}

fn build_path_sprite_pipeline_state(
    device: &ProtocolObject<dyn MTLDevice>,
    library: &ProtocolObject<dyn MTLLibrary>,
    label: &str,
    vertex_fn_name: &str,
    fragment_fn_name: &str,
    pixel_format: MTLPixelFormat,
) -> MetalPipelineState {
    let vertex_fn = library_function(library, vertex_fn_name);
    let fragment_fn = library_function(library, fragment_fn_name);

    let descriptor = MTLRenderPipelineDescriptor::new();
    descriptor.setLabel(Some(&NSString::from_str(label)));
    descriptor.setVertexFunction(Some(&vertex_fn));
    descriptor.setFragmentFunction(Some(&fragment_fn));
    let color_attachment = unsafe { descriptor.colorAttachments().objectAtIndexedSubscript(0) };
    color_attachment.setPixelFormat(pixel_format);
    color_attachment.setBlendingEnabled(true);
    color_attachment.setRgbBlendOperation(MTLBlendOperation::Add);
    color_attachment.setAlphaBlendOperation(MTLBlendOperation::Add);
    color_attachment.setSourceRGBBlendFactor(MTLBlendFactor::One);
    color_attachment.setSourceAlphaBlendFactor(MTLBlendFactor::One);
    color_attachment.setDestinationRGBBlendFactor(MTLBlendFactor::OneMinusSourceAlpha);
    color_attachment.setDestinationAlphaBlendFactor(MTLBlendFactor::One);

    device
        .newRenderPipelineStateWithDescriptor_error(&descriptor)
        .expect("could not create render pipeline state")
}

fn build_path_rasterization_pipeline_state(
    device: &ProtocolObject<dyn MTLDevice>,
    library: &ProtocolObject<dyn MTLLibrary>,
    label: &str,
    vertex_fn_name: &str,
    fragment_fn_name: &str,
    pixel_format: MTLPixelFormat,
    path_sample_count: u32,
) -> MetalPipelineState {
    let vertex_fn = library_function(library, vertex_fn_name);
    let fragment_fn = library_function(library, fragment_fn_name);

    let descriptor = MTLRenderPipelineDescriptor::new();
    descriptor.setLabel(Some(&NSString::from_str(label)));
    descriptor.setVertexFunction(Some(&vertex_fn));
    descriptor.setFragmentFunction(Some(&fragment_fn));
    if path_sample_count > 1 {
        descriptor.setRasterSampleCount(path_sample_count as usize);
        descriptor.setAlphaToCoverageEnabled(false);
    }
    let color_attachment = unsafe { descriptor.colorAttachments().objectAtIndexedSubscript(0) };
    color_attachment.setPixelFormat(pixel_format);
    color_attachment.setBlendingEnabled(true);
    color_attachment.setRgbBlendOperation(MTLBlendOperation::Add);
    color_attachment.setAlphaBlendOperation(MTLBlendOperation::Add);
    color_attachment.setSourceRGBBlendFactor(MTLBlendFactor::One);
    color_attachment.setSourceAlphaBlendFactor(MTLBlendFactor::One);
    color_attachment.setDestinationRGBBlendFactor(MTLBlendFactor::OneMinusSourceAlpha);
    color_attachment.setDestinationAlphaBlendFactor(MTLBlendFactor::OneMinusSourceAlpha);

    device
        .newRenderPipelineStateWithDescriptor_error(&descriptor)
        .expect("could not create render pipeline state")
}

fn library_function(
    library: &ProtocolObject<dyn MTLLibrary>,
    name: &str,
) -> Retained<ProtocolObject<dyn MTLFunction>> {
    library
        .newFunctionWithName(&NSString::from_str(name))
        .unwrap_or_else(|| panic!("error locating function {name}"))
}

fn release_instance_buffer_on_complete(
    command_buffer: &ProtocolObject<dyn MTLCommandBuffer>,
    instance_buffer_pool: Arc<Mutex<InstanceBufferPool>>,
    instance_buffer: InstanceBuffer,
) {
    let instance_buffer = Cell::new(Some(instance_buffer));
    let handler = RcBlock::new(
        move |_buffer: NonNull<ProtocolObject<dyn MTLCommandBuffer>>| {
            if let Some(instance_buffer) = instance_buffer.take() {
                instance_buffer_pool.lock().release(instance_buffer);
            }
        },
    );
    unsafe {
        command_buffer.addCompletedHandler(RcBlock::as_ptr(&handler).cast_mut());
    }
}

fn retain_mtl_texture(ptr: *mut c_void) -> MetalTexture {
    unsafe { Retained::retain(ptr.cast::<ProtocolObject<dyn MTLTexture>>()) }
        .expect("CVMetalTextureGetTexture")
}

fn instance_contents(buffer: &ProtocolObject<dyn MTLBuffer>, offset: usize) -> *mut u8 {
    unsafe { buffer.contents().as_ptr().cast::<u8>().add(offset) }
}

unsafe fn set_vertex_bytes<T>(
    command_encoder: &ProtocolObject<dyn MTLRenderCommandEncoder>,
    index: usize,
    value: &T,
) {
    unsafe {
        command_encoder.setVertexBytes_length_atIndex(
            NonNull::from(value).cast(),
            mem::size_of_val(value),
            index,
        );
    }
}

// Align to multiples of 256 make Metal happy.
fn align_offset(offset: &mut usize) {
    *offset = (*offset).div_ceil(256) * 256;
}

#[repr(C)]
enum ShadowInputIndex {
    Vertices = 0,
    Shadows = 1,
    ViewportSize = 2,
}

#[repr(C)]
enum QuadInputIndex {
    Vertices = 0,
    Quads = 1,
    ViewportSize = 2,
    FramebufferTexture = 3,
}

#[repr(C)]
enum UnderlineInputIndex {
    Vertices = 0,
    Underlines = 1,
    ViewportSize = 2,
}

#[repr(C)]
enum SpriteInputIndex {
    Vertices = 0,
    Sprites = 1,
    ViewportSize = 2,
    AtlasTextureSize = 3,
    AtlasTexture = 4,
}

#[repr(C)]
enum SurfaceInputIndex {
    Vertices = 0,
    Surfaces = 1,
    ViewportSize = 2,
    TextureSize = 3,
    YTexture = 4,
    CbCrTexture = 5,
}

#[repr(C)]
enum PathRasterizationInputIndex {
    Vertices = 0,
    ViewportSize = 1,
}

#[derive(Clone, Debug, Eq, PartialEq)]
#[repr(C)]
pub struct PathSprite {
    pub bounds: Bounds<ScaledPixels>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
#[repr(C)]
pub struct SurfaceBounds {
    pub bounds: Bounds<ScaledPixels>,
    pub content_mask: ContentMask<ScaledPixels>,
}

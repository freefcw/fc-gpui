use crate::{
    AnyElement, AnyEntity, App, AppContext, Asset, AssetLogger, Bounds, Div, DivFrameState,
    Element, ElementId, Entity, GlobalElementId, Hitbox, ImageAssetLoader, ImageCacheError,
    InspectorElementId, IntoElement, LayoutId, ParentElement, Pixels, RenderImage, Resource,
    StyleRefinement, Styled, Window, div, hash,
};

use crate::asset_cache::CachedLoad;
use std::{collections::HashMap, fmt, sync::Arc};

/// An image cache element, all its child img elements will use the cache specified by this element.
/// Note that this could as simple as passing an `Entity<T: ImageCache>`
pub fn image_cache(image_cache_provider: impl ImageCacheProvider) -> ImageCacheElement {
    ImageCacheElement {
        div: div().image_cache(image_cache_provider),
    }
}

/// A dynamically typed image cache, which can be used to store any image cache
#[derive(Clone)]
pub struct AnyImageCache {
    image_cache: AnyEntity,
    load_fn: fn(
        image_cache: &AnyEntity,
        resource: &Resource,
        window: &mut Window,
        cx: &mut App,
    ) -> Option<Result<Arc<RenderImage>, ImageCacheError>>,
}

impl<I: ImageCache> From<Entity<I>> for AnyImageCache {
    fn from(image_cache: Entity<I>) -> Self {
        Self {
            image_cache: image_cache.into_any(),
            load_fn: any_image_cache::load::<I>,
        }
    }
}

impl AnyImageCache {
    pub(crate) fn entity_id(&self) -> crate::EntityId {
        self.image_cache.entity_id()
    }

    /// Load an image given a resource
    /// returns the result of loading the image if it has finished loading, or None if it is still loading
    pub fn load(
        &self,
        resource: &Resource,
        window: &mut Window,
        cx: &mut App,
    ) -> Option<Result<Arc<RenderImage>, ImageCacheError>> {
        (self.load_fn)(&self.image_cache, resource, window, cx)
    }
}

mod any_image_cache {
    use super::*;

    pub(crate) fn load<I: 'static + ImageCache>(
        image_cache: &AnyEntity,
        resource: &Resource,
        window: &mut Window,
        cx: &mut App,
    ) -> Option<Result<Arc<RenderImage>, ImageCacheError>> {
        let image_cache = image_cache.clone().downcast::<I>().unwrap();
        image_cache.update(cx, |image_cache, cx| image_cache.load(resource, window, cx))
    }
}

/// An image cache element.
pub struct ImageCacheElement {
    div: Div,
}

impl ParentElement for ImageCacheElement {
    fn extend(&mut self, elements: impl IntoIterator<Item = AnyElement>) {
        self.div.extend(elements)
    }
}

impl Styled for ImageCacheElement {
    fn style(&mut self) -> &mut StyleRefinement {
        self.div.style()
    }
}

impl IntoElement for ImageCacheElement {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

impl Element for ImageCacheElement {
    type RequestLayoutState = DivFrameState;
    type PrepaintState = Option<Hitbox>;

    fn id(&self) -> Option<ElementId> {
        None
    }

    fn source_location(&self) -> Option<&'static core::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        id: Option<&GlobalElementId>,
        inspector_id: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, Self::RequestLayoutState) {
        self.div.request_layout(id, inspector_id, window, cx)
    }

    fn prepaint(
        &mut self,
        id: Option<&GlobalElementId>,
        inspector_id: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        request_layout: &mut Self::RequestLayoutState,
        window: &mut Window,
        cx: &mut App,
    ) -> Self::PrepaintState {
        self.div
            .prepaint(id, inspector_id, bounds, request_layout, window, cx)
    }

    fn paint(
        &mut self,
        id: Option<&GlobalElementId>,
        inspector_id: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        request_layout: &mut Self::RequestLayoutState,
        prepaint: &mut Self::PrepaintState,
        window: &mut Window,
        cx: &mut App,
    ) {
        self.div.paint(
            id,
            inspector_id,
            bounds,
            request_layout,
            prepaint,
            window,
            cx,
        )
    }
}

/// Owns an image load and its cached result.
///
/// Dropping the entry cancels a pending load. Image caches must remove loaded
/// images from windows before discarding the entry.
pub struct ImageCacheItem(CachedLoad<Result<Arc<RenderImage>, ImageCacheError>>);

impl std::fmt::Debug for ImageCacheItem {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ImageCacheItem")
            .field("result", &self.get())
            .finish()
    }
}

impl ImageCacheItem {
    /// Starts loading an image into a new cache entry.
    pub fn new(source: &Resource, cx: &mut App) -> Self {
        let future = AssetLogger::<ImageAssetLoader>::load(source.clone(), cx);
        Self(CachedLoad::new(future, cx))
    }

    /// Returns the cached result without subscribing to completion notifications.
    pub fn get(&self) -> Option<Result<Arc<RenderImage>, ImageCacheError>> {
        self.0.get()
    }

    /// Returns the cached result or subscribes the current view to completion.
    pub fn use_image(&self, window: &Window) -> Option<Result<Arc<RenderImage>, ImageCacheError>> {
        self.0.use_by(window.current_view())
    }
}

/// An object that can handle the caching and unloading of images.
/// Implementations of this trait should ensure that images are removed from all windows when they are no longer needed.
pub trait ImageCache: 'static {
    /// Load an image given a resource
    /// returns the result of loading the image if it has finished loading, or None if it is still loading
    fn load(
        &mut self,
        resource: &Resource,
        window: &mut Window,
        cx: &mut App,
    ) -> Option<Result<Arc<RenderImage>, ImageCacheError>>;
}

/// An object that can create an ImageCache during the render phase.
/// See the ImageCache trait for more information.
pub trait ImageCacheProvider: 'static {
    /// Called once during request_layout to resolve the cache used by this element's
    /// children in layout, prepaint, and paint.
    fn provide(&mut self, _window: &mut Window, _cx: &mut App) -> AnyImageCache;
}

impl<T: ImageCache> ImageCacheProvider for Entity<T> {
    fn provide(&mut self, _window: &mut Window, _cx: &mut App) -> AnyImageCache {
        self.clone().into()
    }
}

/// An implementation of ImageCache, that uses an LRU caching strategy to unload images when the cache is full
pub struct RetainAllImageCache(HashMap<u64, ImageCacheItem>);

impl fmt::Debug for RetainAllImageCache {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("HashMapImageCache")
            .field("num_images", &self.0.len())
            .finish()
    }
}

impl RetainAllImageCache {
    /// Create a new image cache.
    #[inline]
    pub fn new(cx: &mut App) -> Entity<Self> {
        let e = cx.new(|_cx| RetainAllImageCache(HashMap::new()));
        cx.observe_release(&e, |image_cache, cx| {
            for (_, item) in std::mem::replace(&mut image_cache.0, HashMap::new()) {
                if let Some(Ok(image)) = item.get() {
                    cx.drop_image(image, None);
                }
            }
        })
        .detach();
        e
    }

    /// Load an image from the given source.
    ///
    /// Returns `None` if the image is loading.
    pub fn load(
        &mut self,
        source: &Resource,
        window: &mut Window,
        cx: &mut App,
    ) -> Option<Result<Arc<RenderImage>, ImageCacheError>> {
        let hash = hash(source);

        self.0
            .entry(hash)
            .or_insert_with(|| ImageCacheItem::new(source, cx))
            .use_image(window)
    }

    /// Clear the image cache.
    pub fn clear(&mut self, window: &mut Window, cx: &mut App) {
        for (_, item) in std::mem::replace(&mut self.0, HashMap::new()) {
            if let Some(Ok(image)) = item.get() {
                cx.drop_image(image, Some(window));
            }
        }
    }

    /// Remove the image from the cache by the given source.
    pub fn remove(&mut self, source: &Resource, window: &mut Window, cx: &mut App) {
        let hash = hash(source);
        if let Some(item) = self.0.remove(&hash)
            && let Some(Ok(image)) = item.get()
        {
            cx.drop_image(image, Some(window));
        }
    }

    /// Returns the number of images in the cache.
    pub fn len(&self) -> usize {
        self.0.len()
    }

    /// Returns true if the cache is empty.
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

impl ImageCache for RetainAllImageCache {
    fn load(
        &mut self,
        resource: &Resource,
        window: &mut Window,
        cx: &mut App,
    ) -> Option<Result<Arc<RenderImage>, ImageCacheError>> {
        RetainAllImageCache::load(self, resource, window, cx)
    }
}

/// Constructs a retain-all image cache that uses the element state associated with the given ID.
pub fn retain_all(id: impl Into<ElementId>) -> RetainAllImageCacheProvider {
    RetainAllImageCacheProvider { id: id.into() }
}

/// A provider struct for creating a retain-all image cache inline
pub struct RetainAllImageCacheProvider {
    id: ElementId,
}

impl ImageCacheProvider for RetainAllImageCacheProvider {
    fn provide(&mut self, window: &mut Window, cx: &mut App) -> AnyImageCache {
        window
            .with_global_id(self.id.clone(), |global_id, window| {
                window.with_element_state::<Entity<RetainAllImageCache>, _>(
                    global_id,
                    |cache, _window| {
                        let mut cache = cache.unwrap_or_else(|| RetainAllImageCache::new(cx));
                        (cache.clone(), cache)
                    },
                )
            })
            .into()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Context, Render, TestAppContext, container_query, div, img, px};
    use std::{cell::Cell, rc::Rc};

    struct TestImageCache {
        loads: Rc<Cell<usize>>,
        image: Arc<RenderImage>,
    }

    impl ImageCache for TestImageCache {
        fn load(
            &mut self,
            _: &Resource,
            _: &mut Window,
            _: &mut App,
        ) -> Option<Result<Arc<RenderImage>, ImageCacheError>> {
            self.loads.set(self.loads.get() + 1);
            Some(Ok(self.image.clone()))
        }
    }

    struct CountingProvider {
        calls: Rc<Cell<usize>>,
        cache: Entity<TestImageCache>,
    }

    impl ImageCacheProvider for CountingProvider {
        fn provide(&mut self, _: &mut Window, _: &mut App) -> AnyImageCache {
            self.calls.set(self.calls.get() + 1);
            self.cache.clone().into()
        }
    }

    struct CacheRoot {
        calls: Rc<Cell<usize>>,
        cache: Entity<TestImageCache>,
        use_div: bool,
    }

    impl Render for CacheRoot {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            let provider = CountingProvider {
                calls: self.calls.clone(),
                cache: self.cache.clone(),
            };
            let child = container_query(|_, _, _| img("test-image")).size(px(100.));
            if self.use_div {
                div().image_cache(provider).child(child).into_any_element()
            } else {
                image_cache(provider).child(child).into_any_element()
            }
        }
    }

    fn draw_cache_root(cx: &mut TestAppContext, use_div: bool) -> (usize, usize) {
        let calls = Rc::new(Cell::new(0));
        let loads = Rc::new(Cell::new(0));
        let cache = cx.new(|_| TestImageCache {
            loads: loads.clone(),
            image: Arc::new(RenderImage::new(vec![image::Frame::new(
                image::RgbaImage::new(8, 12),
            )])),
        });
        let window = cx.add_window(|_, _| CacheRoot {
            calls: calls.clone(),
            cache,
            use_div,
        });
        calls.set(0);
        loads.set(0);
        cx.update_window(window.into(), |_, window, cx| window.draw(cx).clear())
            .unwrap();
        (calls.get(), loads.get())
    }

    #[gpui::test]
    fn image_cache_provider_is_resolved_once_per_frame(cx: &mut TestAppContext) {
        for use_div in [false, true] {
            let (calls, _) = draw_cache_root(cx, use_div);
            assert_eq!(calls, 1, "use_div={use_div}");
        }
    }

    #[gpui::test]
    fn image_cache_scope_reaches_images_created_during_prepaint(cx: &mut TestAppContext) {
        for use_div in [false, true] {
            let (_, loads) = draw_cache_root(cx, use_div);
            assert_eq!(
                loads, 2,
                "layout and paint must use the same cache; use_div={use_div}"
            );
        }
    }

    #[gpui::test]
    fn cached_views_follow_the_nearest_image_cache_identity(cx: &mut TestAppContext) {
        struct CachedImage(Rc<Cell<usize>>);
        impl Render for CachedImage {
            fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
                self.0.set(self.0.get() + 1);
                img("same-resource").size(px(32.))
            }
        }

        struct Root {
            cache: Entity<TestImageCache>,
            inner_cache: Option<Entity<TestImageCache>>,
            child: Entity<CachedImage>,
            use_div: bool,
        }
        impl Render for Root {
            fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
                let child = crate::AnyView::from(self.child.clone())
                    .cached(StyleRefinement::default().size(px(32.)));
                let child = if let Some(cache) = self.inner_cache.as_ref() {
                    div()
                        .image_cache(cache.clone())
                        .child(child)
                        .into_any_element()
                } else {
                    child.into_any_element()
                };
                if self.use_div {
                    div()
                        .size(px(100.))
                        .image_cache(self.cache.clone())
                        .child(child)
                        .into_any_element()
                } else {
                    image_cache(self.cache.clone())
                        .size(px(100.))
                        .child(child)
                        .into_any_element()
                }
            }
        }

        for use_div in [false, true] {
            let renders = Rc::new(Cell::new(0));
            let loads_a = Rc::new(Cell::new(0));
            let loads_b = Rc::new(Cell::new(0));
            let cache_a = cx.new(|_| TestImageCache {
                loads: loads_a.clone(),
                image: Arc::new(RenderImage::new(vec![image::Frame::new(
                    image::RgbaImage::new(8, 12),
                )])),
            });
            let cache_b = cx.new(|_| TestImageCache {
                loads: loads_b.clone(),
                image: Arc::new(RenderImage::new(vec![image::Frame::new(
                    image::RgbaImage::new(16, 24),
                )])),
            });
            let child = cx.new(|_| CachedImage(renders.clone()));
            let window = cx.add_window(|_, _| Root {
                cache: cache_a.clone(),
                inner_cache: None,
                child,
                use_div,
            });
            let draw = |cx: &mut TestAppContext| {
                cx.update_window(window.into(), |_, window, cx| {
                    window.draw(cx).clear();
                    assert_eq!(window.rendered_frame.scene.polychrome_sprites.len(), 1);
                    window.rendered_frame.scene.polychrome_sprites[0].tile
                })
                .unwrap()
            };
            let first = draw(cx);
            let first_renders = renders.get();
            let first_loads = loads_a.get();
            assert_eq!(
                first.bounds.size,
                crate::size(crate::DevicePixels(8), crate::DevicePixels(12))
            );

            window.update(cx, |_, _, cx| cx.notify()).unwrap();
            assert_eq!(draw(cx), first);
            assert_eq!(
                renders.get(),
                first_renders,
                "unchanged provider should preserve cached output"
            );
            assert_eq!(loads_a.get(), first_loads);

            window
                .update(cx, |root, _, cx| {
                    root.cache = cache_b;
                    cx.notify();
                })
                .unwrap();
            let replacement = draw(cx);
            assert_eq!(
                replacement.bounds.size,
                crate::size(crate::DevicePixels(16), crate::DevicePixels(24))
            );
            assert_ne!(replacement, first);
            assert_eq!(renders.get(), first_renders + 1);
            assert!(
                loads_b.get() > 0,
                "replacement cache must supply the drawn image"
            );

            window.update(cx, |_, _, cx| cx.notify()).unwrap();
            assert_eq!(draw(cx), replacement);
            assert_eq!(renders.get(), first_renders + 1);

            let inner_loads = Rc::new(Cell::new(0));
            let inner_cache = cx.new(|_| TestImageCache {
                loads: inner_loads.clone(),
                image: Arc::new(RenderImage::new(vec![image::Frame::new(
                    image::RgbaImage::new(24, 32),
                )])),
            });
            window
                .update(cx, |root, _, cx| {
                    root.inner_cache = Some(inner_cache);
                    cx.notify();
                })
                .unwrap();
            let nested = draw(cx);
            assert_eq!(
                nested.bounds.size,
                crate::size(crate::DevicePixels(24), crate::DevicePixels(32))
            );
            let nested_renders = renders.get();
            let nested_loads = inner_loads.get();

            window
                .update(cx, |root, _, cx| {
                    root.cache = cache_a;
                    cx.notify();
                })
                .unwrap();
            assert_eq!(draw(cx), nested);
            assert_eq!(
                renders.get(),
                nested_renders,
                "a shadowed outer cache must not invalidate the subtree"
            );
            assert_eq!(inner_loads.get(), nested_loads);
        }
    }
}

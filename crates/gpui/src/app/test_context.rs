use crate::{
    Action, AnyView, AnyWindowHandle, App, AppCell, AppContext, AsyncApp, AvailableSpace,
    BackgroundExecutor, BorrowAppContext, Bounds, Capslock, ClipboardItem, DrawPhase, Drawable,
    Element, Empty, EventEmitter, ForegroundExecutor, Global, InputEvent, Keystroke, Modifiers,
    ModifiersChangedEvent, MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent, Pixels,
    Platform, PlatformTextSystem, Point, Render, Result, Size, Task, TestDispatcher, TestPlatform,
    TestScreenCaptureSource, TestWindow, TextSystem, VisualContext, Window, WindowBounds,
    WindowHandle, WindowOptions,
};
use anyhow::{anyhow, bail};
use futures::{Stream, StreamExt, channel::oneshot};
use rand::{SeedableRng, rngs::StdRng};
use std::{cell::RefCell, future::Future, ops::Deref, rc::Rc, sync::Arc, time::Duration};

/// A TestAppContext is provided to tests created with `#[gpui::test]`, it provides
/// an implementation of `Context` with additional methods that are useful in tests.
#[derive(Clone)]
pub struct TestAppContext {
    #[doc(hidden)]
    pub app: Rc<AppCell>,
    #[doc(hidden)]
    pub background_executor: BackgroundExecutor,
    #[doc(hidden)]
    pub foreground_executor: ForegroundExecutor,
    #[doc(hidden)]
    pub dispatcher: TestDispatcher,
    test_platform: Rc<TestPlatform>,
    text_system: Arc<TextSystem>,
    fn_name: Option<&'static str>,
    on_quit: Rc<RefCell<Vec<Box<dyn FnOnce() + 'static>>>>,
}

impl AppContext for TestAppContext {
    type Result<T> = T;

    fn new<T: 'static>(
        &mut self,
        build_entity: impl FnOnce(&mut Context<T>) -> T,
    ) -> Self::Result<Entity<T>> {
        let mut app = self.app.borrow_mut();
        app.new(build_entity)
    }

    fn reserve_entity<T: 'static>(&mut self) -> Self::Result<crate::Reservation<T>> {
        let mut app = self.app.borrow_mut();
        app.reserve_entity()
    }

    fn insert_entity<T: 'static>(
        &mut self,
        reservation: crate::Reservation<T>,
        build_entity: impl FnOnce(&mut Context<T>) -> T,
    ) -> Self::Result<Entity<T>> {
        let mut app = self.app.borrow_mut();
        app.insert_entity(reservation, build_entity)
    }

    fn update_entity<T: 'static, R>(
        &mut self,
        handle: &Entity<T>,
        update: impl FnOnce(&mut T, &mut Context<T>) -> R,
    ) -> Self::Result<R> {
        let mut app = self.app.borrow_mut();
        app.update_entity(handle, update)
    }

    fn as_mut<'a, T>(&'a mut self, _: &Entity<T>) -> Self::Result<super::GpuiBorrow<'a, T>>
    where
        T: 'static,
    {
        panic!("Cannot use as_mut with a test app context. Try calling update() first")
    }

    fn read_entity<T, R>(
        &self,
        handle: &Entity<T>,
        read: impl FnOnce(&T, &App) -> R,
    ) -> Self::Result<R>
    where
        T: 'static,
    {
        let app = self.app.borrow();
        app.read_entity(handle, read)
    }

    fn update_window<T, F>(&mut self, window: AnyWindowHandle, f: F) -> Result<T>
    where
        F: FnOnce(AnyView, &mut Window, &mut App) -> T,
    {
        let mut lock = self.app.borrow_mut();
        lock.update_window(window, f)
    }

    fn read_window<T, R>(
        &self,
        window: &WindowHandle<T>,
        read: impl FnOnce(Entity<T>, &App) -> R,
    ) -> Result<R>
    where
        T: 'static,
    {
        let app = self.app.borrow();
        app.read_window(window, read)
    }

    fn background_spawn<R>(&self, future: impl Future<Output = R> + Send + 'static) -> Task<R>
    where
        R: Send + 'static,
    {
        self.background_executor.spawn(future)
    }

    fn read_global<G, R>(&self, callback: impl FnOnce(&G, &App) -> R) -> Self::Result<R>
    where
        G: Global,
    {
        let app = self.app.borrow();
        app.read_global(callback)
    }
}

impl TestAppContext {
    /// Creates a new `TestAppContext`. Usually you can rely on `#[gpui::test]` to do this for you.
    pub fn build(dispatcher: TestDispatcher, fn_name: Option<&'static str>) -> Self {
        Self::build_with_text_system(dispatcher, fn_name, None)
    }

    pub(crate) fn build_with_text_system(
        dispatcher: TestDispatcher,
        fn_name: Option<&'static str>,
        platform_text_system: Option<Arc<dyn PlatformTextSystem>>,
    ) -> Self {
        let arc_dispatcher = Arc::new(dispatcher.clone());
        let background_executor = BackgroundExecutor::new(arc_dispatcher.clone());
        let foreground_executor = ForegroundExecutor::new(arc_dispatcher);
        let platform = if let Some(platform_text_system) = platform_text_system {
            TestPlatform::with_text_system(
                background_executor.clone(),
                foreground_executor.clone(),
                platform_text_system,
            )
        } else {
            TestPlatform::new(background_executor.clone(), foreground_executor.clone())
        };
        let asset_source = Arc::new(());
        let http_client = http_client::FakeHttpClient::with_404_response();
        let default_profile = crate::AppResourceProfile::default();
        let text_system = Arc::new(TextSystem::new(
            platform.text_system(),
            &default_profile.text,
        ));

        Self {
            app: App::new_app(platform.clone(), asset_source, http_client, default_profile),
            background_executor,
            foreground_executor,
            dispatcher,
            test_platform: platform,
            text_system,
            fn_name,
            on_quit: Rc::new(RefCell::new(Vec::default())),
        }
    }

    /// Create a single TestAppContext, for non-multi-client tests
    pub fn single() -> Self {
        let dispatcher = TestDispatcher::new(StdRng::seed_from_u64(0));
        Self::build(dispatcher, None)
    }

    /// The name of the test function that created this `TestAppContext`
    pub fn test_function_name(&self) -> Option<&'static str> {
        self.fn_name
    }

    /// Checks whether there have been any new path prompts received by the platform.
    pub fn did_prompt_for_new_path(&self) -> bool {
        self.test_platform.did_prompt_for_new_path()
    }

    /// Access the test platform for crate-internal assertions.
    #[cfg(test)]
    pub(crate) fn test_platform(&self) -> Rc<TestPlatform> {
        self.test_platform.clone()
    }

    /// returns a new `TestAppContext` re-using the same executors to interleave tasks.
    pub fn new_app(&self) -> TestAppContext {
        Self::build(self.dispatcher.clone(), self.fn_name)
    }

    /// Called by the test helper to end the test.
    /// public so the macro can call it.
    pub fn quit(&self) {
        self.on_quit.borrow_mut().drain(..).for_each(|f| f());
        self.app.borrow_mut().shutdown();
    }

    /// Register cleanup to run when the test ends.
    pub fn on_quit(&mut self, f: impl FnOnce() + 'static) {
        self.on_quit.borrow_mut().push(Box::new(f));
    }

    /// Schedules all windows to be redrawn on the next effect cycle.
    pub fn refresh(&mut self) -> Result<()> {
        let mut app = self.app.borrow_mut();
        app.refresh_windows();
        Ok(())
    }

    /// Returns an executor (for running tasks in the background)
    pub fn executor(&self) -> BackgroundExecutor {
        self.background_executor.clone()
    }

    /// Returns an executor (for running tasks on the main thread)
    pub fn foreground_executor(&self) -> &ForegroundExecutor {
        &self.foreground_executor
    }

    #[expect(clippy::wrong_self_convention)]
    fn new<T: 'static>(&mut self, build_entity: impl FnOnce(&mut Context<T>) -> T) -> Entity<T> {
        let mut cx = self.app.borrow_mut();
        cx.new(build_entity)
    }

    /// Gives you an `&mut App` for the duration of the closure
    pub fn update<R>(&self, f: impl FnOnce(&mut App) -> R) -> R {
        let mut cx = self.app.borrow_mut();
        cx.update(f)
    }

    /// Gives you an `&App` for the duration of the closure
    pub fn read<R>(&self, f: impl FnOnce(&App) -> R) -> R {
        let cx = self.app.borrow();
        f(&cx)
    }

    /// Adds a new window. The Window will always be backed by a `TestWindow` which
    /// can be retrieved with `self.test_window(handle)`
    pub fn add_window<F, V>(&mut self, build_window: F) -> WindowHandle<V>
    where
        F: FnOnce(&mut Window, &mut Context<V>) -> V,
        V: 'static + Render,
    {
        let mut cx = self.app.borrow_mut();

        // Some tests rely on the window size matching the bounds of the test display
        let bounds = Bounds::maximized(None, &cx);
        cx.open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                ..Default::default()
            },
            |window, cx| cx.new(|cx| build_window(window, cx)),
        )
        .unwrap()
    }

    /// Adds a new window with no content.
    pub fn add_empty_window(&mut self) -> &mut VisualTestContext {
        let mut cx = self.app.borrow_mut();
        let bounds = Bounds::maximized(None, &cx);
        let window = cx
            .open_window(
                WindowOptions {
                    window_bounds: Some(WindowBounds::Windowed(bounds)),
                    ..Default::default()
                },
                |_, cx| cx.new(|_| Empty),
            )
            .unwrap();
        drop(cx);
        let cx = VisualTestContext::from_window(*window.deref(), self).into_mut();
        cx.run_until_parked();
        cx
    }

    /// Adds a new window, and returns its root view and a `VisualTestContext` which can be used
    /// as a `Window` and `App` for the rest of the test. Typically you would shadow this context with
    /// the returned one. `let (view, cx) = cx.add_window_view(...);`
    pub fn add_window_view<F, V>(
        &mut self,
        build_root_view: F,
    ) -> (Entity<V>, &mut VisualTestContext)
    where
        F: FnOnce(&mut Window, &mut Context<V>) -> V,
        V: 'static + Render,
    {
        let mut cx = self.app.borrow_mut();
        let bounds = Bounds::maximized(None, &cx);
        let window = cx
            .open_window(
                WindowOptions {
                    window_bounds: Some(WindowBounds::Windowed(bounds)),
                    ..Default::default()
                },
                |window, cx| cx.new(|cx| build_root_view(window, cx)),
            )
            .unwrap();
        drop(cx);
        let view = window.root(self).unwrap();
        let cx = VisualTestContext::from_window(*window.deref(), self).into_mut();
        cx.run_until_parked();

        // it might be nice to try and cleanup these at the end of each test.
        (view, cx)
    }

    /// returns the TextSystem
    pub fn text_system(&self) -> &Arc<TextSystem> {
        &self.text_system
    }

    /// Simulates writing to the platform clipboard
    pub fn write_to_clipboard(&self, item: ClipboardItem) {
        self.test_platform.write_to_clipboard(item)
    }

    /// Simulates reading from the platform clipboard.
    /// This will return the most recent value from `write_to_clipboard`.
    pub fn read_from_clipboard(&self) -> Option<ClipboardItem> {
        self.test_platform.read_from_clipboard()
    }

    /// Simulates choosing a File in the platform's "Open" dialog.
    pub fn simulate_new_path_selection(
        &self,
        select_path: impl FnOnce(&std::path::Path) -> Option<std::path::PathBuf>,
    ) {
        self.test_platform.simulate_new_path_selection(select_path);
    }

    /// Simulates clicking a button in an platform-level alert dialog.
    #[track_caller]
    pub fn simulate_prompt_answer(&self, button: &str) {
        self.test_platform.simulate_prompt_answer(button);
    }

    /// Returns true if there's an alert dialog open.
    pub fn has_pending_prompt(&self) -> bool {
        self.test_platform.has_pending_prompt()
    }

    /// Returns true if there's an alert dialog open.
    pub fn pending_prompt(&self) -> Option<(String, String)> {
        self.test_platform.pending_prompt()
    }

    /// All the urls that have been opened with cx.open_url() during this test.
    pub fn opened_url(&self) -> Option<String> {
        self.test_platform.opened_url.borrow().clone()
    }

    /// Returns the latest tray icon bytes set through the platform.
    pub fn tray_icon(&self) -> Option<Vec<u8>> {
        self.test_platform.tray_icon()
    }

    /// Returns the latest tray icon rendering mode set through the platform.
    pub fn tray_icon_rendering_mode(&self) -> crate::TrayIconRenderingMode {
        self.test_platform.tray_icon_rendering_mode()
    }

    /// Simulates a system tray icon click event.
    pub fn simulate_tray_icon_click_event(&self, event: crate::TrayIconClickEvent) {
        self.test_platform.simulate_tray_icon_click_event(event);
    }

    /// Simulates the user resizing the window to the new size.
    pub fn simulate_window_resize(&self, window_handle: AnyWindowHandle, size: Size<Pixels>) {
        self.test_window(window_handle).simulate_resize(size);
    }

    /// Simulates the window moving to a display with a different scale factor.
    pub fn simulate_window_scale_factor_change(
        &self,
        window_handle: AnyWindowHandle,
        scale_factor: f32,
    ) {
        self.test_window(window_handle)
            .simulate_scale_factor_change(scale_factor);
    }

    /// Causes the given sources to be returned if the application queries for screen
    /// capture sources.
    pub fn set_screen_capture_sources(&self, sources: Vec<TestScreenCaptureSource>) {
        self.test_platform.set_screen_capture_sources(sources);
    }

    /// Returns all windows open in the test.
    pub fn windows(&self) -> Vec<AnyWindowHandle> {
        self.app.borrow().windows()
    }

    /// Run the given task on the main thread.
    #[track_caller]
    pub fn spawn<Fut, R>(&self, f: impl FnOnce(AsyncApp) -> Fut) -> Task<R>
    where
        Fut: Future<Output = R> + 'static,
        R: 'static,
    {
        self.foreground_executor.spawn(f(self.to_async()))
    }

    /// true if the given global is defined
    pub fn has_global<G: Global>(&self) -> bool {
        let app = self.app.borrow();
        app.has_global::<G>()
    }

    /// runs the given closure with a reference to the global
    /// panics if `has_global` would return false.
    pub fn read_global<G: Global, R>(&self, read: impl FnOnce(&G, &App) -> R) -> R {
        let app = self.app.borrow();
        read(app.global(), &app)
    }

    /// runs the given closure with a reference to the global (if set)
    pub fn try_read_global<G: Global, R>(&self, read: impl FnOnce(&G, &App) -> R) -> Option<R> {
        let lock = self.app.borrow();
        Some(read(lock.try_global()?, &lock))
    }

    /// sets the global in this context.
    pub fn set_global<G: Global>(&mut self, global: G) {
        let mut lock = self.app.borrow_mut();
        lock.update(|cx| cx.set_global(global))
    }

    /// updates the global in this context. (panics if `has_global` would return false)
    pub fn update_global<G: Global, R>(&mut self, update: impl FnOnce(&mut G, &mut App) -> R) -> R {
        let mut lock = self.app.borrow_mut();
        lock.update(|cx| cx.update_global(update))
    }

    /// Returns an `AsyncApp` which can be used to run tasks that expect to be on a background
    /// thread on the current thread in tests.
    pub fn to_async(&self) -> AsyncApp {
        AsyncApp {
            app: Rc::downgrade(&self.app),
            background_executor: self.background_executor.clone(),
            foreground_executor: self.foreground_executor.clone(),
        }
    }

    /// Wait until there are no more pending tasks.
    pub fn run_until_parked(&mut self) {
        self.background_executor.run_until_parked()
    }

    /// Simulate dispatching an action to the currently focused node in the window.
    pub fn dispatch_action<A>(&mut self, window: AnyWindowHandle, action: A)
    where
        A: Action,
    {
        window
            .update(self, |_, window, cx| {
                window.dispatch_action(action.boxed_clone(), cx)
            })
            .unwrap();

        self.background_executor.run_until_parked()
    }

    /// simulate_keystrokes takes a space-separated list of keys to type.
    /// cx.simulate_keystrokes("cmd-shift-p b k s p enter")
    /// in Zed, this will run backspace on the current editor through the command palette.
    /// This will also run the background executor until it's parked.
    pub fn simulate_keystrokes(&mut self, window: AnyWindowHandle, keystrokes: &str) {
        for keystroke in keystrokes
            .split(' ')
            .map(Keystroke::parse)
            .map(Result::unwrap)
        {
            self.dispatch_keystroke(window, keystroke);
        }

        self.background_executor.run_until_parked()
    }

    /// simulate_input takes a string of text to type.
    /// cx.simulate_input("abc")
    /// will type abc into your current editor
    /// This will also run the background executor until it's parked.
    pub fn simulate_input(&mut self, window: AnyWindowHandle, input: &str) {
        for keystroke in input.split("").map(Keystroke::parse).map(Result::unwrap) {
            self.dispatch_keystroke(window, keystroke);
        }

        self.background_executor.run_until_parked()
    }

    /// dispatches a single Keystroke (see also `simulate_keystrokes` and `simulate_input`)
    pub fn dispatch_keystroke(&mut self, window: AnyWindowHandle, keystroke: Keystroke) {
        self.update_window(window, |_, window, cx| {
            window.dispatch_keystroke(keystroke, cx)
        })
        .unwrap();
    }

    /// Returns the `TestWindow` backing the given handle.
    pub(crate) fn test_window(&self, window: AnyWindowHandle) -> TestWindow {
        self.app
            .borrow_mut()
            .windows
            .get_mut(window.id)
            .unwrap()
            .as_mut()
            .unwrap()
            .platform_window
            .as_test()
            .unwrap()
            .clone()
    }

    /// Returns a stream of notifications whenever the Entity is updated.
    pub fn notifications<T: 'static>(
        &mut self,
        entity: &Entity<T>,
    ) -> impl Stream<Item = ()> + use<T> {
        let (tx, rx) = futures::channel::mpsc::unbounded();
        self.update(|cx| {
            cx.observe(entity, {
                let tx = tx.clone();
                move |_, _| {
                    let _ = tx.unbounded_send(());
                }
            })
            .detach();
            cx.observe_release(entity, move |_, _| tx.close_channel())
                .detach()
        });
        rx
    }

    /// Returns a stream of events emitted by the given Entity.
    pub fn events<Evt, T: 'static + EventEmitter<Evt>>(
        &mut self,
        entity: &Entity<T>,
    ) -> futures::channel::mpsc::UnboundedReceiver<Evt>
    where
        Evt: 'static + Clone,
    {
        let (tx, rx) = futures::channel::mpsc::unbounded();
        entity
            .update(self, |_, cx: &mut Context<T>| {
                cx.subscribe(entity, move |_entity, _handle, event, _cx| {
                    let _ = tx.unbounded_send(event.clone());
                })
            })
            .detach();
        rx
    }

    /// Runs until the given condition becomes true. (Prefer `run_until_parked` if you
    /// don't need to jump in at a specific time).
    pub async fn condition<T: 'static>(
        &mut self,
        entity: &Entity<T>,
        mut predicate: impl FnMut(&mut T, &mut Context<T>) -> bool,
    ) {
        let timer = self.executor().timer(Duration::from_secs(3));
        let mut notifications = self.notifications(entity);

        use futures::FutureExt as _;
        use smol::future::FutureExt as _;

        async {
            loop {
                if entity.update(self, &mut predicate) {
                    return Ok(());
                }

                if notifications.next().await.is_none() {
                    bail!("entity dropped")
                }
            }
        }
        .race(timer.map(|_| Err(anyhow!("condition timed out"))))
        .await
        .unwrap();
    }

    /// Set a name for this App.
    #[cfg(any(test, feature = "test-support"))]
    pub fn set_name(&mut self, name: &'static str) {
        self.update(|cx| cx.name = Some(name))
    }
}

/// A higher-level test application wrapper that flushes pending effects after updates.
///
/// Use this for tests that do not need precise control over every dispatcher tick. Use
/// [`TestAppContext`] directly when a test needs to observe intermediate states before tasks run.
pub struct TestApp {
    cx: TestAppContext,
}

impl TestApp {
    /// Creates a new test application using a deterministic default seed.
    pub fn new() -> Self {
        Self {
            cx: TestAppContext::single(),
        }
    }

    /// Creates a new test application using the provided dispatcher seed.
    pub fn with_seed(seed: u64) -> Self {
        let dispatcher = TestDispatcher::new(StdRng::seed_from_u64(seed));
        Self {
            cx: TestAppContext::build(dispatcher, None),
        }
    }

    #[doc(hidden)]
    pub fn with_platform_text_system(platform_text_system: Arc<dyn PlatformTextSystem>) -> Self {
        let dispatcher = TestDispatcher::new(StdRng::seed_from_u64(0));
        Self {
            cx: TestAppContext::build_with_text_system(
                dispatcher,
                None,
                Some(platform_text_system),
            ),
        }
    }

    /// Wraps an existing [`TestAppContext`].
    pub fn from_context(cx: TestAppContext) -> Self {
        Self { cx }
    }

    /// Returns the underlying [`TestAppContext`].
    pub fn raw_context(&self) -> &TestAppContext {
        &self.cx
    }

    /// Returns the underlying mutable [`TestAppContext`].
    pub fn raw_context_mut(&mut self) -> &mut TestAppContext {
        &mut self.cx
    }

    /// Gives mutable access to [`App`] and then flushes pending effects and redraws.
    pub fn update<R>(&mut self, f: impl FnOnce(&mut App) -> R) -> R {
        let result = self.cx.update(f);
        self.flush();
        result
    }

    /// Gives mutable access to [`App`] without automatically flushing pending work.
    pub fn update_without_flush<R>(&mut self, f: impl FnOnce(&mut App) -> R) -> R {
        self.cx.update(f)
    }

    /// Gives read-only access to [`App`].
    pub fn read<R>(&self, f: impl FnOnce(&App) -> R) -> R {
        self.cx.read(f)
    }

    /// Runs pending tasks, refreshes windows, and runs tasks produced by redraw.
    pub fn flush(&mut self) {
        self.cx.run_until_parked();
        self.cx.refresh().unwrap();
        self.cx.run_until_parked();
    }

    /// Alias for [`Self::flush`].
    pub fn flush_effects(&mut self) {
        self.flush();
    }

    /// Runs pending tasks until the dispatcher parks.
    pub fn run_until_parked(&mut self) {
        self.cx.run_until_parked();
    }

    /// Opens a test window and returns a typed wrapper for its root view.
    pub fn open_window<F, V>(&mut self, build_window: F) -> TestAppWindow<V>
    where
        F: FnOnce(&mut Window, &mut Context<V>) -> V,
        V: 'static + Render,
    {
        let handle = self.cx.add_window(build_window);
        self.flush();
        TestAppWindow {
            handle,
            cx: self.cx.clone(),
        }
    }

    /// Returns the test text system.
    pub fn text_system(&self) -> &Arc<TextSystem> {
        self.cx.text_system()
    }

    /// Returns the latest tray icon bytes set through the test platform.
    pub fn tray_icon(&self) -> Option<Vec<u8>> {
        self.cx.tray_icon()
    }

    /// Returns the latest tray icon rendering mode set through the test platform.
    pub fn tray_icon_rendering_mode(&self) -> crate::TrayIconRenderingMode {
        self.cx.tray_icon_rendering_mode()
    }

    /// Simulates a system tray icon click event.
    pub fn simulate_tray_icon_click_event(&self, event: crate::TrayIconClickEvent) {
        self.cx.simulate_tray_icon_click_event(event);
    }
}

impl Default for TestApp {
    fn default() -> Self {
        Self::new()
    }
}

/// A typed handle for a root view opened by [`TestApp`].
pub struct TestAppWindow<V> {
    handle: WindowHandle<V>,
    cx: TestAppContext,
}

impl<V> Clone for TestAppWindow<V> {
    fn clone(&self) -> Self {
        Self {
            handle: self.handle,
            cx: self.cx.clone(),
        }
    }
}

impl<V: 'static + Render> TestAppWindow<V> {
    /// Returns the underlying window handle.
    pub fn handle(&self) -> WindowHandle<V> {
        self.handle
    }

    /// Returns the root entity for this window.
    pub fn root(&mut self) -> Entity<V> {
        self.handle.root(&mut self.cx).unwrap()
    }

    /// Reads the root view.
    pub fn read<R>(&self, f: impl FnOnce(&V, &App) -> R) -> R {
        self.handle.read_with(&self.cx, f).unwrap()
    }

    /// Updates the root view and then flushes pending effects.
    pub fn update<R>(&mut self, f: impl FnOnce(&mut V, &mut Window, &mut Context<V>) -> R) -> R {
        let result = self.handle.update(&mut self.cx, f).unwrap();
        self.flush();
        result
    }

    /// Simulate the window moving to a display with a different scale factor.
    pub fn simulate_scale_factor_change(&mut self, scale_factor: f32) {
        let window: AnyWindowHandle = self.handle.into();
        self.cx
            .simulate_window_scale_factor_change(window, scale_factor);
        self.cx.background_executor.run_until_parked();
    }

    /// Draws the window once.
    pub fn draw(&mut self) {
        let window: AnyWindowHandle = self.handle.into();
        self.cx
            .update_window(window, |_, window, cx| {
                window.draw(cx).clear();
            })
            .unwrap();
        self.flush();
    }

    /// Returns the structural render artifact from the most recent mock draw.
    pub fn visual_render_artifact(&mut self) -> Option<crate::VisualRenderArtifact> {
        let window: AnyWindowHandle = self.handle.into();
        self.cx.test_window(window).render_artifact().or_else(|| {
            self.cx
                .update_window(window, |_, window, _| window.visual_render_artifact())
                .ok()
        })
    }

    /// Flushes pending effects through the underlying test app context.
    pub fn flush(&mut self) {
        self.cx.run_until_parked();
        self.cx.refresh().unwrap();
        self.cx.run_until_parked();
    }
}

impl<T: 'static> Entity<T> {
    /// Block until the next event is emitted by the entity, then return it.
    pub fn next_event<Event>(&self, cx: &mut TestAppContext) -> impl Future<Output = Event>
    where
        Event: Send + Clone + 'static,
        T: EventEmitter<Event>,
    {
        let (tx, mut rx) = oneshot::channel();
        let mut tx = Some(tx);
        let subscription = self.update(cx, |_, cx| {
            cx.subscribe(self, move |_, _, event, _| {
                if let Some(tx) = tx.take() {
                    _ = tx.send(event.clone());
                }
            })
        });

        async move {
            let event = rx.await.expect("no event emitted");
            drop(subscription);
            event
        }
    }
}

impl<V: 'static> Entity<V> {
    /// Returns a future that resolves when the view is next updated.
    pub fn next_notification(
        &self,
        advance_clock_by: Duration,
        cx: &TestAppContext,
    ) -> impl Future<Output = ()> {
        use postage::prelude::{Sink as _, Stream as _};

        let (mut tx, mut rx) = postage::mpsc::channel(1);
        let subscription = cx.app.borrow_mut().observe(self, move |_, _| {
            tx.try_send(()).ok();
        });

        let duration = if std::env::var("CI").is_ok() {
            Duration::from_secs(5)
        } else {
            Duration::from_secs(1)
        };

        cx.executor().advance_clock(advance_clock_by);

        async move {
            let notification = crate::util::smol_timeout(duration, rx.recv())
                .await
                .expect("next notification timed out");
            drop(subscription);
            notification.expect("entity dropped while test was waiting for its next notification")
        }
    }
}

impl<V> Entity<V> {
    /// Returns a future that resolves when the condition becomes true.
    pub fn condition<Evt>(
        &self,
        cx: &TestAppContext,
        mut predicate: impl FnMut(&V, &App) -> bool,
    ) -> impl Future<Output = ()>
    where
        Evt: 'static,
        V: EventEmitter<Evt>,
    {
        use postage::prelude::{Sink as _, Stream as _};

        let (tx, mut rx) = postage::mpsc::channel(1024);

        let mut cx = cx.app.borrow_mut();
        let subscriptions = (
            cx.observe(self, {
                let mut tx = tx.clone();
                move |_, _| {
                    tx.blocking_send(()).ok();
                }
            }),
            cx.subscribe(self, {
                let mut tx = tx;
                move |_, _: &Evt, _| {
                    tx.blocking_send(()).ok();
                }
            }),
        );

        let cx = cx.this.upgrade().unwrap();
        let handle = self.downgrade();

        async move {
            crate::util::smol_timeout(Duration::from_secs(1), async move {
                loop {
                    {
                        let cx = cx.borrow();
                        let cx = &*cx;
                        if predicate(
                            handle
                                .upgrade()
                                .expect("view dropped with pending condition")
                                .read(cx),
                            cx,
                        ) {
                            break;
                        }
                    }

                    cx.borrow().background_executor().start_waiting();
                    rx.recv()
                        .await
                        .expect("view dropped with pending condition");
                    cx.borrow().background_executor().finish_waiting();
                }
            })
            .await
            .expect("condition timed out");
            drop(subscriptions);
        }
    }
}

use derive_more::{Deref, DerefMut};

use super::{Context, Entity};

/// Runtime capability flags for visual tests.
///
/// These flags describe what the current process can safely attempt without
/// assuming a GPU runner or display server is available.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct VisualTestCapabilities {
    /// True when the current platform is expected to support a real renderer.
    pub real_renderer: bool,
    /// True when screenshot capture is supported by the visual test layer.
    pub screenshot_capture: bool,
    /// True when real windows can be placed at screen-outside coordinates for smoke tests.
    pub offscreen_positioned_window: bool,
    /// True when tests can use deterministic dispatcher time.
    pub deterministic_clock: bool,
}

impl VisualTestCapabilities {
    /// Detects visual test capabilities for the current platform and environment.
    pub fn detect() -> Self {
        let real_renderer = detect_real_visual_renderer();
        Self {
            real_renderer,
            screenshot_capture: real_renderer
                && cfg!(any(
                    target_os = "macos",
                    target_os = "linux",
                    target_os = "freebsd",
                    target_os = "windows"
                )),
            offscreen_positioned_window: detect_offscreen_positioned_window(),
            deterministic_clock: true,
        }
    }
}

#[cfg(target_os = "macos")]
fn detect_real_visual_renderer() -> bool {
    true
}

#[cfg(any(target_os = "linux", target_os = "freebsd"))]
fn detect_real_visual_renderer() -> bool {
    std::env::var_os("DISPLAY").is_some() || std::env::var_os("WAYLAND_DISPLAY").is_some()
}

#[cfg(target_os = "windows")]
fn detect_real_visual_renderer() -> bool {
    true
}

#[cfg(not(any(
    target_os = "macos",
    target_os = "linux",
    target_os = "freebsd",
    target_os = "windows"
)))]
fn detect_real_visual_renderer() -> bool {
    false
}

#[cfg(target_os = "macos")]
fn detect_offscreen_positioned_window() -> bool {
    true
}

#[cfg(any(target_os = "linux", target_os = "freebsd"))]
fn detect_offscreen_positioned_window() -> bool {
    std::env::var_os("WAYLAND_DISPLAY").is_none() && std::env::var_os("DISPLAY").is_some()
}

#[cfg(target_os = "windows")]
fn detect_offscreen_positioned_window() -> bool {
    // The Windows backend (`retrieve_window_placement`) rejects bounds whose
    // center is not inside a monitor and falls back to a centered default, so
    // it does not guarantee the requested offscreen origin. macOS and X11 do.
    false
}

#[cfg(not(any(
    target_os = "macos",
    target_os = "linux",
    target_os = "freebsd",
    target_os = "windows"
)))]
fn detect_offscreen_positioned_window() -> bool {
    false
}

/// A real visual test context backed by the native platform renderer.
///
/// macOS and X11 windows use screen-outside coordinates so manual
/// renderer smoke tests do not occupy the user's normal workspace. Windows
/// places windows via the platform default (the backend rejects centers that
/// fall outside any monitor and falls back to a centered default), so tests
/// there do not assert an offscreen origin. Wayland uses the compositor-selected
/// position because clients cannot choose absolute coordinates.
#[cfg(all(
    any(
        target_os = "macos",
        target_os = "linux",
        target_os = "freebsd",
        target_os = "windows"
    ),
    any(test, feature = "test-support")
))]
pub struct RealVisualTestContext {
    /// The underlying app cell.
    pub app: Rc<AppCell>,
    background_executor: BackgroundExecutor,
    foreground_executor: ForegroundExecutor,
    dispatcher: TestDispatcher,
    platform: Rc<crate::VisualTestPlatform>,
    text_system: Arc<TextSystem>,
}

#[cfg(all(
    any(
        target_os = "macos",
        target_os = "linux",
        target_os = "freebsd",
        target_os = "windows"
    ),
    any(test, feature = "test-support")
))]
impl RealVisualTestContext {
    /// Creates a real visual test context with an injected platform and the default empty assets.
    pub fn with_platform(platform: Rc<dyn crate::Platform>) -> Self {
        Self::with_platform_and_asset_source(platform, Arc::new(()))
    }

    /// Creates a real visual test context with an injected platform and custom assets.
    pub fn with_platform_and_asset_source(
        platform: Rc<dyn crate::Platform>,
        asset_source: Arc<dyn crate::AssetSource>,
    ) -> Self {
        let seed = std::env::var("SEED")
            .ok()
            .and_then(|seed| seed.parse().ok())
            .unwrap_or(0);
        let platform = Rc::new(crate::VisualTestPlatform::new(platform, seed));
        let dispatcher = platform.dispatcher().clone();
        let background_executor = platform.background_executor();
        let foreground_executor = platform.foreground_executor();
        let default_profile = crate::AppResourceProfile::default();
        let text_system = Arc::new(TextSystem::new(
            platform.text_system(),
            &default_profile.text,
        ));
        let http_client = http_client::FakeHttpClient::with_404_response();
        let app = App::new_app(platform.clone(), asset_source, http_client, default_profile);

        Self {
            app,
            background_executor,
            foreground_executor,
            dispatcher,
            platform,
            text_system,
        }
    }

    /// Returns the wrapped visual platform for compatibility composition roots.
    #[doc(hidden)]
    pub fn visual_test_platform(&self) -> Rc<crate::VisualTestPlatform> {
        self.platform.clone()
    }

    /// Starts the real platform run loop and invokes the callback after launch.
    pub fn run<F>(self, on_finish_launching: F)
    where
        F: 'static + FnOnce(&mut Self),
    {
        let platform = self.platform.clone();
        let mut cx = Some(self);
        platform.run(Box::new(move || {
            if let Some(mut cx) = cx.take() {
                on_finish_launching(&mut cx);
            }
        }));
    }

    /// Opens a real platform window. Platforms that support client-controlled
    /// positioning place it outside the visible screen; Wayland uses the
    /// compositor-selected position.
    pub fn open_offscreen_window<V: 'static + Render>(
        &mut self,
        size: Size<Pixels>,
        build_root_view: impl FnOnce(&mut Window, &mut App) -> Entity<V>,
    ) -> Result<WindowHandle<V>> {
        let origin = if VisualTestCapabilities::detect().offscreen_positioned_window {
            crate::point(crate::px(-10000.0), crate::px(-10000.0))
        } else {
            Point::default()
        };
        let bounds = Bounds::new(origin, size);
        let mut app = self.app.borrow_mut();
        app.open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                focus: false,
                show: true,
                ..Default::default()
            },
            build_root_view,
        )
    }

    /// Runs pending deterministic app tasks until parked.
    pub fn run_until_parked(&self) {
        self.dispatcher.run_until_parked();
    }

    /// Advances deterministic app time and drains tasks that become ready.
    pub fn advance_clock(&self, duration: Duration) {
        self.dispatcher.advance_clock(duration);
    }

    /// Gives mutable access to a window.
    pub fn update_window<T, F>(&mut self, window: AnyWindowHandle, f: F) -> Result<T>
    where
        F: FnOnce(AnyView, &mut Window, &mut App) -> T,
    {
        self.app.borrow_mut().update_window(window, f)
    }

    /// Gracefully quits the underlying real platform app.
    pub fn quit(&self) {
        self.app.borrow().quit();
    }

    /// Spawns a task on the deterministic foreground executor.
    pub fn spawn<R>(&self, future: impl Future<Output = R> + 'static) -> Task<R>
    where
        R: 'static,
    {
        self.foreground_executor.spawn(future)
    }

    /// Returns the deterministic background executor.
    pub fn executor(&self) -> BackgroundExecutor {
        self.background_executor.clone()
    }

    /// Returns the text system used by this context.
    pub fn text_system(&self) -> &Arc<TextSystem> {
        &self.text_system
    }

    /// Returns whether the real platform supports screen capture APIs.
    pub fn is_screen_capture_supported(&self) -> bool {
        self.platform.is_screen_capture_supported()
    }

    /// Captures a screenshot through the platform render readback path.
    pub fn capture_screenshot(
        &mut self,
        window: AnyWindowHandle,
    ) -> anyhow::Result<image::RgbaImage> {
        self.update_window(window, |_, window, _| window.render_to_image())?
    }
}

#[derive(Deref, DerefMut, Clone)]
/// A VisualTestContext is the test-equivalent of a `Window` and `App`. It allows you to
/// run window-specific test code. It can be dereferenced to a `TextAppContext`.
pub struct VisualTestContext {
    #[deref]
    #[deref_mut]
    /// cx is the original TestAppContext (you can more easily access this using Deref)
    pub cx: TestAppContext,
    window: AnyWindowHandle,
}

impl VisualTestContext {
    /// Provides a `Window` and `App` for the duration of the closure.
    pub fn update<R>(&mut self, f: impl FnOnce(&mut Window, &mut App) -> R) -> R {
        self.cx
            .update_window(self.window, |_, window, cx| f(window, cx))
            .unwrap()
    }

    /// Creates a new VisualTestContext. You would typically shadow the passed in
    /// TestAppContext with this, as this is typically more useful.
    /// `let cx = VisualTestContext::from_window(window, cx);`
    pub fn from_window(window: AnyWindowHandle, cx: &TestAppContext) -> Self {
        Self {
            cx: cx.clone(),
            window,
        }
    }

    /// Wait until there are no more pending tasks.
    pub fn run_until_parked(&self) {
        self.cx.background_executor.run_until_parked();
    }

    /// Dispatch the action to the currently focused node.
    pub fn dispatch_action<A>(&mut self, action: A)
    where
        A: Action,
    {
        self.cx.dispatch_action(self.window, action)
    }

    /// Read the title off the window (set by `Window#set_window_title`)
    pub fn window_title(&mut self) -> Option<String> {
        self.cx.test_window(self.window).0.lock().title.clone()
    }

    /// Simulate a sequence of keystrokes `cx.simulate_keystrokes("cmd-p escape")`
    /// Automatically runs until parked.
    pub fn simulate_keystrokes(&mut self, keystrokes: &str) {
        self.cx.simulate_keystrokes(self.window, keystrokes)
    }

    /// Simulate typing text `cx.simulate_input("hello")`
    /// Automatically runs until parked.
    pub fn simulate_input(&mut self, input: &str) {
        self.cx.simulate_input(self.window, input)
    }

    /// Simulate a mouse move event to the given point
    pub fn simulate_mouse_move(
        &mut self,
        position: Point<Pixels>,
        button: impl Into<Option<MouseButton>>,
        modifiers: Modifiers,
    ) {
        self.simulate_event(MouseMoveEvent {
            position,
            modifiers,
            pressed_button: button.into(),
        })
    }

    /// Simulate a mouse down event to the given point
    pub fn simulate_mouse_down(
        &mut self,
        position: Point<Pixels>,
        button: MouseButton,
        modifiers: Modifiers,
    ) {
        self.simulate_event(MouseDownEvent {
            position,
            modifiers,
            button,
            click_count: 1,
            first_mouse: false,
        })
    }

    /// Simulate a mouse up event to the given point
    pub fn simulate_mouse_up(
        &mut self,
        position: Point<Pixels>,
        button: MouseButton,
        modifiers: Modifiers,
    ) {
        self.simulate_event(MouseUpEvent {
            position,
            modifiers,
            button,
            click_count: 1,
        })
    }

    /// Simulate a primary mouse click at the given point
    pub fn simulate_click(&mut self, position: Point<Pixels>, modifiers: Modifiers) {
        self.simulate_event(MouseDownEvent {
            position,
            modifiers,
            button: MouseButton::Left,
            click_count: 1,
            first_mouse: false,
        });
        self.simulate_event(MouseUpEvent {
            position,
            modifiers,
            button: MouseButton::Left,
            click_count: 1,
        });
    }

    /// Simulate a modifiers changed event
    pub fn simulate_modifiers_change(&mut self, modifiers: Modifiers) {
        self.simulate_event(ModifiersChangedEvent {
            modifiers,
            capslock: Capslock { on: false },
        })
    }

    /// Simulate a capslock changed event
    pub fn simulate_capslock_change(&mut self, on: bool) {
        self.simulate_event(ModifiersChangedEvent {
            modifiers: Modifiers::none(),
            capslock: Capslock { on },
        })
    }

    /// Simulates the system accessibility adapter activating for this window.
    ///
    /// Panics when GPUI was built without the `accessibility` feature.
    pub fn simulate_accessibility_activation(&mut self) {
        let activated = self
            .test_window(self.window)
            .simulate_accessibility_activation();
        assert!(
            activated,
            "accessibility activation requires the `accessibility` feature"
        );
        self.background_executor.run_until_parked();
    }

    /// Simulates an action requested by assistive technology for this window.
    ///
    /// Panics when GPUI was built without the `accessibility` feature.
    pub fn simulate_accessibility_action(&mut self, request: accesskit::ActionRequest) {
        let dispatched = self
            .test_window(self.window)
            .simulate_accessibility_action(request);
        assert!(
            dispatched,
            "accessibility actions require the `accessibility` feature"
        );
        self.background_executor.run_until_parked();
    }

    /// Simulates the user resizing the window to the new size.
    pub fn simulate_resize(&self, size: Size<Pixels>) {
        self.simulate_window_resize(self.window, size)
    }

    /// Simulates the window moving to a display with a different scale factor.
    pub fn simulate_scale_factor_change(&self, scale_factor: f32) {
        self.simulate_window_scale_factor_change(self.window, scale_factor)
    }

    /// debug_bounds returns the bounds of the element with the given selector.
    pub fn debug_bounds(&mut self, selector: &'static str) -> Option<Bounds<Pixels>> {
        self.update(|window, _| window.rendered_frame.debug_bounds.get(selector).copied())
    }

    /// Draw an element to the window. Useful for simulating events or actions
    pub fn draw<E>(
        &mut self,
        origin: Point<Pixels>,
        space: impl Into<Size<AvailableSpace>>,
        f: impl FnOnce(&mut Window, &mut App) -> E,
    ) -> (E::RequestLayoutState, E::PrepaintState)
    where
        E: Element,
    {
        self.update(|window, cx| {
            window.invalidator.set_phase(DrawPhase::Prepaint);
            let mut element = Drawable::new(f(window, cx));
            element.layout_as_root(space.into(), window, cx);
            window.with_absolute_element_offset(origin, |window| element.prepaint(window, cx));

            window.invalidator.set_phase(DrawPhase::Paint);
            let (request_layout_state, prepaint_state) = element.paint(window, cx);

            window.invalidator.set_phase(DrawPhase::None);
            window.refresh();

            (request_layout_state, prepaint_state)
        })
    }

    /// Simulate an event from the platform, e.g. a SrollWheelEvent
    /// Make sure you've called [VisualTestContext::draw] first!
    pub fn simulate_event<E: InputEvent>(&mut self, event: E) {
        self.test_window(self.window)
            .simulate_input(event.to_platform_input());
        self.background_executor.run_until_parked();
    }

    /// Simulates the user blurring the window.
    pub fn deactivate_window(&mut self) {
        if Some(self.window) == self.test_platform.active_window() {
            self.test_platform.set_active_window(None)
        }
        self.background_executor.run_until_parked();
    }

    /// Simulates the user closing the window.
    /// Returns true if the window was closed.
    pub fn simulate_close(&mut self) -> bool {
        let handler = self
            .cx
            .update_window(self.window, |_, window, _| {
                window
                    .platform_window
                    .as_test()
                    .unwrap()
                    .0
                    .lock()
                    .should_close_handler
                    .take()
            })
            .unwrap();
        if let Some(mut handler) = handler {
            let should_close = handler();
            self.cx
                .update_window(self.window, |_, window, _| {
                    window.platform_window.on_should_close(handler);
                })
                .unwrap();
            should_close
        } else {
            false
        }
    }

    /// Get an &mut VisualTestContext (which is mostly what you need to pass to other methods).
    /// This method internally retains the VisualTestContext until the end of the test.
    pub fn into_mut(self) -> &'static mut Self {
        let ptr = Box::into_raw(Box::new(self));
        // safety: on_quit will be called after the test has finished.
        // the executor will ensure that all tasks related to the test have stopped.
        // so there is no way for cx to be accessed after on_quit is called.
        let cx = Box::leak(unsafe { Box::from_raw(ptr) });
        cx.on_quit(move || unsafe {
            drop(Box::from_raw(ptr));
        });
        cx
    }
}

impl AppContext for VisualTestContext {
    type Result<T> = <TestAppContext as AppContext>::Result<T>;

    fn new<T: 'static>(
        &mut self,
        build_entity: impl FnOnce(&mut Context<T>) -> T,
    ) -> Self::Result<Entity<T>> {
        self.cx.new(build_entity)
    }

    fn reserve_entity<T: 'static>(&mut self) -> Self::Result<crate::Reservation<T>> {
        self.cx.reserve_entity()
    }

    fn insert_entity<T: 'static>(
        &mut self,
        reservation: crate::Reservation<T>,
        build_entity: impl FnOnce(&mut Context<T>) -> T,
    ) -> Self::Result<Entity<T>> {
        self.cx.insert_entity(reservation, build_entity)
    }

    fn update_entity<T, R>(
        &mut self,
        handle: &Entity<T>,
        update: impl FnOnce(&mut T, &mut Context<T>) -> R,
    ) -> Self::Result<R>
    where
        T: 'static,
    {
        self.cx.update_entity(handle, update)
    }

    fn as_mut<'a, T>(&'a mut self, handle: &Entity<T>) -> Self::Result<super::GpuiBorrow<'a, T>>
    where
        T: 'static,
    {
        self.cx.as_mut(handle)
    }

    fn read_entity<T, R>(
        &self,
        handle: &Entity<T>,
        read: impl FnOnce(&T, &App) -> R,
    ) -> Self::Result<R>
    where
        T: 'static,
    {
        self.cx.read_entity(handle, read)
    }

    fn update_window<T, F>(&mut self, window: AnyWindowHandle, f: F) -> Result<T>
    where
        F: FnOnce(AnyView, &mut Window, &mut App) -> T,
    {
        self.cx.update_window(window, f)
    }

    fn read_window<T, R>(
        &self,
        window: &WindowHandle<T>,
        read: impl FnOnce(Entity<T>, &App) -> R,
    ) -> Result<R>
    where
        T: 'static,
    {
        self.cx.read_window(window, read)
    }

    fn background_spawn<R>(&self, future: impl Future<Output = R> + Send + 'static) -> Task<R>
    where
        R: Send + 'static,
    {
        self.cx.background_spawn(future)
    }

    fn read_global<G, R>(&self, callback: impl FnOnce(&G, &App) -> R) -> Self::Result<R>
    where
        G: Global,
    {
        self.cx.read_global(callback)
    }
}

impl VisualContext for VisualTestContext {
    /// Get the underlying window handle underlying this context.
    fn window_handle(&self) -> AnyWindowHandle {
        self.window
    }

    fn new_window_entity<T: 'static>(
        &mut self,
        build_entity: impl FnOnce(&mut Window, &mut Context<T>) -> T,
    ) -> Self::Result<Entity<T>> {
        self.window
            .update(&mut self.cx, |_, window, cx| {
                cx.new(|cx| build_entity(window, cx))
            })
            .unwrap()
    }

    fn update_window_entity<V: 'static, R>(
        &mut self,
        view: &Entity<V>,
        update: impl FnOnce(&mut V, &mut Window, &mut Context<V>) -> R,
    ) -> Self::Result<R> {
        self.window
            .update(&mut self.cx, |_, window, cx| {
                view.update(cx, |v, cx| update(v, window, cx))
            })
            .unwrap()
    }

    fn replace_root_view<V>(
        &mut self,
        build_view: impl FnOnce(&mut Window, &mut Context<V>) -> V,
    ) -> Self::Result<Entity<V>>
    where
        V: 'static + Render,
    {
        self.window
            .update(&mut self.cx, |_, window, cx| {
                window.replace_root(cx, build_view)
            })
            .unwrap()
    }

    fn focus<V: crate::Focusable>(&mut self, view: &Entity<V>) -> Self::Result<()> {
        self.window
            .update(&mut self.cx, |_, window, cx| {
                view.read(cx).focus_handle(cx).focus(window)
            })
            .unwrap()
    }
}

impl AnyWindowHandle {
    /// Creates the given view in this window.
    pub fn build_entity<V: Render + 'static>(
        &self,
        cx: &mut TestAppContext,
        build_view: impl FnOnce(&mut Window, &mut Context<V>) -> V,
    ) -> Entity<V> {
        self.update(cx, |_, window, cx| cx.new(|cx| build_view(window, cx)))
            .unwrap()
    }
}

#[cfg(test)]
mod test_app_tests {
    use super::*;
    use crate::{InteractiveElement as _, Role, StatefulInteractiveElement as _, Styled as _, px};
    use std::cell::Cell;
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };

    struct TestView {
        value: usize,
    }

    struct PaintedView;

    struct AccessibleView;

    struct AccessibleActionView {
        invocations: Rc<Cell<usize>>,
    }

    impl Render for TestView {
        fn render(
            &mut self,
            _window: &mut Window,
            _cx: &mut Context<Self>,
        ) -> impl crate::IntoElement {
            Empty
        }
    }

    impl Render for PaintedView {
        fn render(
            &mut self,
            _window: &mut Window,
            _cx: &mut Context<Self>,
        ) -> impl crate::IntoElement {
            crate::div().w(px(20.)).h(px(20.)).bg(crate::black())
        }
    }

    impl Render for AccessibleView {
        fn render(
            &mut self,
            _window: &mut Window,
            _cx: &mut Context<Self>,
        ) -> impl crate::IntoElement {
            crate::div()
                .id("save-settings")
                .role(Role::Button)
                .aria_label("Save settings")
        }
    }

    impl Render for AccessibleActionView {
        fn render(
            &mut self,
            _window: &mut Window,
            _cx: &mut Context<Self>,
        ) -> impl crate::IntoElement {
            let invocations = self.invocations.clone();
            crate::div()
                .id("action-button")
                .role(Role::Button)
                .aria_label("Run action")
                .on_a11y_action(accesskit::Action::Click, move |_, _, _| {
                    invocations.set(invocations.get() + 1);
                })
        }
    }

    #[test]
    fn test_app_update_auto_flushes_effects() {
        let mut app = TestApp::new();
        let value = Arc::new(AtomicUsize::new(0));

        app.update({
            let value = value.clone();
            move |cx| {
                cx.background_executor()
                    .spawn(async move {
                        value.store(1, Ordering::SeqCst);
                    })
                    .detach();
            }
        });

        assert_eq!(value.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn test_app_update_without_flush_preserves_pending_state() {
        let mut app = TestApp::new();
        let value = Arc::new(AtomicUsize::new(0));

        app.update_without_flush({
            let value = value.clone();
            move |cx| {
                cx.background_executor()
                    .spawn(async move {
                        value.store(1, Ordering::SeqCst);
                    })
                    .detach();
            }
        });

        assert_eq!(value.load(Ordering::SeqCst), 0);
        app.flush();
        assert_eq!(value.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn test_app_open_window_returns_usable_handle() {
        let mut app = TestApp::new();
        let mut window = app.open_window(|_, _| TestView { value: 1 });

        assert_eq!(window.read(|view, _| view.value), 1);
        window.update(|view, _, _| view.value = 2);
        assert_eq!(window.read(|view, _| view.value), 2);
    }

    #[test]
    fn test_app_window_draw_succeeds() {
        let mut app = TestApp::new();
        let mut window = app.open_window(|_, _| TestView { value: 1 });

        window.draw();
    }

    #[test]
    fn test_simulate_scale_factor_change() {
        let mut app = TestApp::new();
        let mut window = app.open_window(|_, _| crate::EmptyView);
        let viewport_size = window.update(|_, window, _| {
            assert_eq!(window.scale_factor(), 2.0);
            window.viewport_size()
        });

        window.simulate_scale_factor_change(1.0);

        window.update(|_, window, _| {
            assert_eq!(window.scale_factor(), 1.0);
            assert_eq!(window.viewport_size(), viewport_size);
        });
    }

    #[test]
    fn visual_test_mock_render_artifact_reports_nonblank_scene() {
        let mut app = TestApp::new();
        let mut window = app.open_window(|_, _| PaintedView);

        window.draw();

        let artifact = window
            .visual_render_artifact()
            .expect("draw should produce a render artifact");
        assert!(artifact.is_nonblank());
        assert!(artifact.quads > 0);
    }

    #[test]
    fn test_platform_accessibility_activation_builds_a_debug_tree() {
        let mut cx = TestAppContext::single();
        let (_, cx) = cx.add_window_view(|_, _| AccessibleView);

        assert_eq!(cx.update(|window, _| window.debug_a11y_tree_json()), None);

        cx.simulate_accessibility_activation();
        cx.update(|window, cx| window.draw(cx).clear());

        let tree = cx
            .update(|window, _| window.debug_a11y_tree_json())
            .expect("activation should build an accessibility tree");
        let tree: serde_json::Value = serde_json::from_str(&tree).unwrap();
        let nodes = tree["nodes"].as_array().unwrap();
        assert!(nodes.iter().any(|node| {
            node["aria"]["role"] == "Button" && node["aria"]["label"] == "Save settings"
        }));
    }

    #[test]
    fn test_platform_dispatches_accessibility_actions_to_window_listeners() {
        let mut cx = TestAppContext::single();
        let invocations = Rc::new(Cell::new(0));
        let (_, cx) = cx.add_window_view({
            let invocations = invocations.clone();
            move |_, _| AccessibleActionView { invocations }
        });

        cx.simulate_accessibility_activation();
        cx.update(|window, cx| window.draw(cx).clear());
        let tree = cx
            .update(|window, _| window.debug_a11y_tree_json())
            .expect("activation should build an accessibility tree");
        let tree: serde_json::Value = serde_json::from_str(&tree).unwrap();
        let node_id = tree["nodes"]
            .as_array()
            .unwrap()
            .iter()
            .find(|node| node["aria"]["label"] == "Run action")
            .and_then(|node| node["accesskit_id"].as_str())
            .unwrap()
            .parse()
            .unwrap();

        cx.simulate_accessibility_action(accesskit::ActionRequest {
            action: accesskit::Action::Click,
            target_tree: accesskit::TreeId::ROOT,
            target_node: accesskit::NodeId(node_id),
            data: None,
        });

        assert_eq!(invocations.get(), 1);
    }

    #[test]
    fn visual_test_render_to_image_reports_unsupported_without_platform_impl() {
        let mut app = TestApp::new();
        let window = app.open_window(|_, _| PaintedView);
        let handle: AnyWindowHandle = window.handle().into();

        let result = app
            .raw_context_mut()
            .update_window(handle, |_, window, _| window.render_to_image())
            .unwrap();

        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("render_to_image not implemented")
        );
    }

    #[test]
    fn test_app_flush_with_no_windows_succeeds() {
        let mut app = TestApp::new();
        app.flush();
    }

    #[test]
    fn visual_test_capabilities_detect_does_not_panic() {
        let _ = VisualTestCapabilities::detect();
    }

    #[test]
    fn visual_test_capabilities_reports_deterministic_clock_true() {
        assert!(VisualTestCapabilities::detect().deterministic_clock);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn visual_test_capabilities_macos_has_real_renderer() {
        let capabilities = VisualTestCapabilities::detect();
        assert!(capabilities.real_renderer);
        assert!(capabilities.screenshot_capture);
        assert!(capabilities.offscreen_positioned_window);
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn windows_visual_harness_exposes_directx_screenshot_support() {
        let capabilities = VisualTestCapabilities::detect();
        assert!(capabilities.real_renderer);
        assert!(capabilities.screenshot_capture);
        assert!(!capabilities.offscreen_positioned_window);
        let _constructor: fn(Rc<dyn crate::Platform>) -> RealVisualTestContext =
            RealVisualTestContext::with_platform;
    }
}

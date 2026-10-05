use std::{
    any::{TypeId, type_name},
    cell::{BorrowMutError, Ref, RefCell, RefMut},
    ffi::OsString,
    marker::PhantomData,
    mem,
    ops::{Deref, DerefMut},
    path::{Path, PathBuf},
    rc::{Rc, Weak},
    sync::{Arc, atomic::Ordering::SeqCst},
    time::{Duration, Instant},
};

use anyhow::{Context as _, Result, anyhow};
use derive_more::{Deref, DerefMut};
use futures::{Future, FutureExt, channel::oneshot, future::LocalBoxFuture};
use itertools::Itertools;
use parking_lot::RwLock;
use slotmap::SlotMap;

pub use async_context::*;
use collections::{FxHashMap, FxHashSet, HashMap, VecDeque};
pub use context::*;
pub use entity_map::*;
use http_client::{HttpClient, Url};
use smallvec::SmallVec;
#[cfg(any(test, feature = "test-support"))]
pub use test_context::*;
use util::{ResultExt, debug_panic};

#[cfg(any(feature = "inspector", debug_assertions))]
use crate::InspectorElementRegistry;
use crate::asset_cache::CachedLoad;
use crate::{
    Action, ActionBuildError, ActionRegistry, Any, AnyView, AnyWindowHandle, AppContext,
    AppResourceProfile, Asset, AssetSource, AttentionType, BackgroundExecutor, BiometricStatus,
    Bounds, ClipboardItem, CrashReport, CursorStyle, DialogOptions, DispatchPhase, DisplayId,
    EventEmitter, FocusHandle, FocusMap, FocusedWindowInfo, ForegroundExecutor, Global, KeyBinding,
    KeyContext, Keymap, Keystroke, LayoutId, MediaKeyEvent, Menu, MenuItem, MissingGlyph,
    NetworkStatus, OsInfo, OwnedMenu, PathPromptOptions, PermissionRequestStatus, PermissionStatus,
    Pixels, Platform, PlatformDisplay, PlatformKeyboardLayout, PlatformKeyboardMapper, Point,
    PowerSaveBlockerKind, PromptBuilder, PromptButton, PromptHandle, PromptLevel, Render,
    RenderImage, RenderablePromptHandle, Reservation, ScreenCaptureSource, SharedString, Size,
    SubscriberSet, Subscription, SvgRenderer, SystemPowerEvent, Task, TextSystem, ThermalState,
    TrayAnchor, TrayIconClickEvent, TrayIconEvent, TrayIconRenderingMode, TrayMenuItem, Window,
    WindowAppearance, WindowHandle, WindowId, WindowInvalidator, WindowPosition,
    colors::{Colors, GlobalColors},
    hash, init_app_menus, point, px, size,
};

mod async_context;
mod context;
mod entity_map;
#[cfg(any(test, feature = "test-support"))]
mod test_context;

/// The duration for which futures returned from [Context::on_app_quit] can run before the application fully quits.
pub const SHUTDOWN_TIMEOUT: Duration = Duration::from_millis(100);

/// Temporary(?) wrapper around [`RefCell<App>`] to help us debug any double borrows.
/// Strongly consider removing after stabilization.
#[doc(hidden)]
pub struct AppCell {
    app: RefCell<App>,
}

impl AppCell {
    #[doc(hidden)]
    #[track_caller]
    pub fn borrow(&self) -> AppRef<'_> {
        if option_env!("TRACK_THREAD_BORROWS").is_some() {
            let thread_id = std::thread::current().id();
            eprintln!("borrowed {thread_id:?}");
        }
        AppRef(self.app.borrow())
    }

    #[doc(hidden)]
    #[track_caller]
    pub fn borrow_mut(&self) -> AppRefMut<'_> {
        if option_env!("TRACK_THREAD_BORROWS").is_some() {
            let thread_id = std::thread::current().id();
            eprintln!("borrowed {thread_id:?}");
        }
        AppRefMut(self.app.borrow_mut())
    }

    #[doc(hidden)]
    #[track_caller]
    pub fn try_borrow_mut(&self) -> Result<AppRefMut<'_>, BorrowMutError> {
        if option_env!("TRACK_THREAD_BORROWS").is_some() {
            let thread_id = std::thread::current().id();
            eprintln!("borrowed {thread_id:?}");
        }
        Ok(AppRefMut(self.app.try_borrow_mut()?))
    }
}

#[doc(hidden)]
#[derive(Deref, DerefMut)]
pub struct AppRef<'a>(Ref<'a, App>);

impl Drop for AppRef<'_> {
    fn drop(&mut self) {
        if option_env!("TRACK_THREAD_BORROWS").is_some() {
            let thread_id = std::thread::current().id();
            eprintln!("dropped borrow from {thread_id:?}");
        }
    }
}

#[doc(hidden)]
#[derive(Deref, DerefMut)]
pub struct AppRefMut<'a>(RefMut<'a, App>);

impl Drop for AppRefMut<'_> {
    fn drop(&mut self) {
        if option_env!("TRACK_THREAD_BORROWS").is_some() {
            let thread_id = std::thread::current().id();
            eprintln!("dropped {thread_id:?}");
        }
    }
}

/// A reference to a GPUI application, typically constructed in the `main` function of your app.
/// You won't interact with this type much outside of initial configuration and startup.
pub struct Application(Rc<AppCell>);

/// A strong handle to an [`Application`] started with [`Application::run_embedded`].
///
/// Dropping this handle releases the app, so an embedder must hold it for as long as the
/// app should run. While held, it is the embedder's entry point back into GPUI each time
/// the external run loop gives it control.
pub struct ApplicationHandle {
    app: Rc<AppCell>,
}

impl ApplicationHandle {
    /// Invoke `f` with the app context. Must not be called re-entrantly from code that
    /// is already inside an update; the app state is a `RefCell` and will panic on a
    /// double borrow.
    pub fn update<R>(&self, f: impl FnOnce(&mut App) -> R) -> R {
        let cx = &mut *self.app.borrow_mut();
        f(cx)
    }

    /// An [`AsyncApp`] for use across await points. It holds the app weakly; keeping the
    /// app alive remains this handle's job.
    pub fn to_async(&self) -> AsyncApp {
        self.update(|cx| cx.to_async())
    }
}

/// Represents an application before it is fully launched. Once your app is
/// configured, you'll start the app with `App::run`.
impl Application {
    /// Builds an application with a caller-provided platform implementation.
    ///
    /// This is a low-level construction seam for platform facade and backend crates.
    pub fn with_platform(platform: Rc<dyn Platform>) -> Self {
        Self(App::new_app(
            platform,
            Arc::new(()),
            Arc::new(NullHttpClient),
            AppResourceProfile::default(),
        ))
    }

    /// Builds this app with accessibility integration forcibly disabled.
    ///
    /// In this mode, accessibility APIs such as
    /// [`StatefulInteractiveElement::role`](crate::StatefulInteractiveElement::role)
    /// silently no-op.
    pub fn inaccessible(self) -> Self {
        self.0.borrow_mut().accessibility_force_disabled = true;
        self
    }

    /// Assign
    pub fn with_assets(self, asset_source: impl AssetSource) -> Self {
        let mut context_lock = self.0.borrow_mut();
        let asset_source = Arc::new(asset_source);
        context_lock.asset_source = asset_source.clone();
        context_lock.svg_renderer = SvgRenderer::new(asset_source);
        drop(context_lock);
        self
    }

    /// Configures arguments to pass when restarting the application.
    pub fn with_restart_arguments(self, arguments: Vec<OsString>) -> Self {
        self.0.borrow_mut().restart_arguments = arguments;
        self
    }

    /// Sets the HTTP client for the application.
    pub fn with_http_client(self, http_client: Arc<dyn HttpClient>) -> Self {
        let mut context_lock = self.0.borrow_mut();
        context_lock.http_client = http_client;
        drop(context_lock);
        self
    }

    /// Configures when the application should automatically quit.
    /// By default, [`QuitMode::Default`] is used.
    pub fn with_quit_mode(self, mode: QuitMode) -> Self {
        self.0.borrow_mut().set_quit_mode(mode);
        self
    }

    /// Sets the resource profile for the application.
    ///
    /// The resource profile controls internal cache sizes and GPU resource
    /// allocation. Use [`crate::AppProfile`] presets for common scenarios, or provide
    /// a custom [`AppResourceProfile`] for fine-grained control.
    ///
    /// **Must be called before [`Application::run`]** — this rebuilds text
    /// caches with the configured budget. Atlas configuration takes effect for
    /// subsequently opened windows.
    ///
    /// The core crate does not select a desktop backend. Applications using the
    /// published `gpui` facade should call `gpui::Application::new()` instead.
    ///
    /// ```rust,ignore
    /// gpui::Application::new()
    ///     .with_resource_profile(AppProfile::Minimal)
    ///     .run(|cx| { /* ... */ });
    /// ```
    pub fn with_resource_profile(self, profile: impl Into<AppResourceProfile>) -> Self {
        let profile = profile.into();
        // Set the element arena size before any windows (and thus thread-local
        // arenas) are created. This must happen on the main thread, which is
        // guaranteed by the `Application` API. Clamp to >= 1 because the
        // underlying `Arena::new` requires a non-zero chunk size and would
        // otherwise panic lazily on first use in another thread.
        crate::window::ELEMENT_ARENA_SIZE.store(
            profile.element_arena_size.max(1),
            std::sync::atomic::Ordering::Relaxed,
        );
        let mut context_lock = self.0.borrow_mut();
        context_lock.platform.configure_gpu_resources(&profile.gpu);
        context_lock.text_system = Arc::new(TextSystem::new(
            context_lock.platform.text_system(),
            &profile.text,
        ));
        context_lock.resource_profile = profile;
        drop(context_lock);
        self
    }

    /// Start the application. The provided callback will be called once the
    /// app is fully launched.
    pub fn run<F>(self, on_finish_launching: F)
    where
        F: 'static + FnOnce(&mut App),
    {
        let this = self.0.clone();
        let platform = self.0.borrow().platform.clone();
        platform.run(Box::new(move || {
            let cx = &mut *this.borrow_mut();
            on_finish_launching(cx);
        }));
    }

    /// Start the application for an embedder that drives the run loop itself.
    ///
    /// On ordinary platforms `Platform::run` blocks for the lifetime of the app, and the
    /// app state is kept alive by [`Application::run`]'s stack frame. Embedded platforms -
    /// where the run loop belongs to someone else, e.g. GPUI compiled into a Wasm guest,
    /// or a GPUI view hosted inside a foreign native application - implement
    /// `Platform::run` to invoke the launch callback and return immediately. This method
    /// supports that shape: it returns an [`ApplicationHandle`] that keeps the app alive
    /// and lets the embedder re-enter it whenever the external run loop yields control.
    pub fn run_embedded<F>(self, on_finish_launching: F) -> ApplicationHandle
    where
        F: 'static + FnOnce(&mut App),
    {
        let this = self.0.clone();
        let platform = self.0.borrow().platform.clone();
        platform.run(Box::new(move || {
            let cx = &mut *this.borrow_mut();
            on_finish_launching(cx);
        }));
        ApplicationHandle { app: self.0 }
    }

    /// Register a handler to be invoked when the platform instructs the application
    /// to open one or more URLs.
    pub fn on_open_urls<F>(&self, mut callback: F) -> &Self
    where
        F: 'static + FnMut(Vec<String>),
    {
        self.0.borrow().platform.on_open_urls(Box::new(callback));
        self
    }

    /// Invokes a handler when an already-running application is launched.
    /// On macOS, this can occur when the application icon is double-clicked or the app is launched via the dock.
    pub fn on_reopen<F>(&self, mut callback: F) -> &Self
    where
        F: 'static + FnMut(&mut App),
    {
        let this = Rc::downgrade(&self.0);
        self.0.borrow_mut().platform.on_reopen(Box::new(move || {
            if let Some(app) = this.upgrade() {
                callback(&mut app.borrow_mut());
            }
        }));
        self
    }

    /// Returns a handle to the [`BackgroundExecutor`] associated with this app, which can be used to spawn futures in the background.
    pub fn background_executor(&self) -> BackgroundExecutor {
        self.0.borrow().background_executor.clone()
    }

    /// Returns a handle to the [`ForegroundExecutor`] associated with this app, which can be used to spawn futures in the foreground.
    pub fn foreground_executor(&self) -> ForegroundExecutor {
        self.0.borrow().foreground_executor.clone()
    }

    /// Returns a reference to the [`TextSystem`] associated with this app.
    pub fn text_system(&self) -> Arc<TextSystem> {
        self.0.borrow().text_system.clone()
    }

    /// Returns the file URL of the executable with the specified name in the application bundle
    pub fn path_for_auxiliary_executable(&self, name: &str) -> Result<PathBuf> {
        self.0.borrow().path_for_auxiliary_executable(name)
    }
}

type Handler = Box<dyn FnMut(&mut App) -> bool + 'static>;
type Listener = Box<dyn FnMut(&dyn Any, &mut App) -> bool + 'static>;
pub(crate) type KeystrokeObserver =
    Box<dyn FnMut(&KeystrokeEvent, &mut Window, &mut App) -> bool + 'static>;
type QuitHandler = Box<dyn FnOnce(&mut App) -> LocalBoxFuture<'static, ()> + 'static>;
type WindowClosedHandler = Box<dyn FnMut(&mut App)>;
type ReleaseListener = Box<dyn FnOnce(&mut dyn Any, &mut App) + 'static>;
type NewEntityListener = Box<dyn FnMut(AnyEntity, &mut Option<&mut Window>, &mut App) + 'static>;

/// Defines when the application should automatically quit.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum QuitMode {
    /// Use [`QuitMode::Explicit`] on macOS and [`QuitMode::LastWindowClosed`] on other platforms.
    #[default]
    Default,
    /// Quit automatically when the last window is closed.
    LastWindowClosed,
    /// Quit only when requested via [`App::quit`].
    Explicit,
}

#[doc(hidden)]
#[derive(Clone, PartialEq, Eq)]
pub struct SystemWindowTab {
    pub id: WindowId,
    pub title: SharedString,
    pub handle: AnyWindowHandle,
    pub last_active_at: Instant,
}

impl SystemWindowTab {
    /// Create a new instance of the window tab.
    pub fn new(title: SharedString, handle: AnyWindowHandle) -> Self {
        Self {
            id: handle.id,
            title,
            handle,
            last_active_at: Instant::now(),
        }
    }
}

/// A controller for managing window tabs.
#[derive(Default)]
pub struct SystemWindowTabController {
    visible: Option<bool>,
    tab_groups: FxHashMap<usize, Vec<SystemWindowTab>>,
}

impl Global for SystemWindowTabController {}

impl SystemWindowTabController {
    /// Create a new instance of the window tab controller.
    pub fn new() -> Self {
        Self {
            visible: None,
            tab_groups: FxHashMap::default(),
        }
    }

    /// Initialize the global window tab controller.
    pub fn init(cx: &mut App) {
        cx.set_global(SystemWindowTabController::new());
    }

    /// Get all tab groups.
    pub fn tab_groups(&self) -> &FxHashMap<usize, Vec<SystemWindowTab>> {
        &self.tab_groups
    }

    /// Get the next tab group window handle.
    pub fn get_next_tab_group_window(cx: &mut App, id: WindowId) -> Option<&AnyWindowHandle> {
        let controller = cx.global::<SystemWindowTabController>();
        let current_group = controller
            .tab_groups
            .iter()
            .find_map(|(group, tabs)| tabs.iter().find(|tab| tab.id == id).map(|_| group));

        let current_group = current_group?;
        let mut group_ids: Vec<_> = controller.tab_groups.keys().collect();
        let idx = group_ids.iter().position(|g| *g == current_group)?;
        let next_idx = (idx + 1) % group_ids.len();

        controller
            .tab_groups
            .get(group_ids[next_idx])
            .and_then(|tabs| {
                tabs.iter()
                    .max_by_key(|tab| tab.last_active_at)
                    .or_else(|| tabs.first())
                    .map(|tab| &tab.handle)
            })
    }

    /// Get the previous tab group window handle.
    pub fn get_prev_tab_group_window(cx: &mut App, id: WindowId) -> Option<&AnyWindowHandle> {
        let controller = cx.global::<SystemWindowTabController>();
        let current_group = controller
            .tab_groups
            .iter()
            .find_map(|(group, tabs)| tabs.iter().find(|tab| tab.id == id).map(|_| group));

        let current_group = current_group?;
        let mut group_ids: Vec<_> = controller.tab_groups.keys().collect();
        let idx = group_ids.iter().position(|g| *g == current_group)?;
        let prev_idx = if idx == 0 {
            group_ids.len() - 1
        } else {
            idx - 1
        };

        controller
            .tab_groups
            .get(group_ids[prev_idx])
            .and_then(|tabs| {
                tabs.iter()
                    .max_by_key(|tab| tab.last_active_at)
                    .or_else(|| tabs.first())
                    .map(|tab| &tab.handle)
            })
    }

    /// Get all tabs in the same window.
    pub fn tabs(&self, id: WindowId) -> Option<&Vec<SystemWindowTab>> {
        let tab_group = self
            .tab_groups
            .iter()
            .find_map(|(group, tabs)| tabs.iter().find(|tab| tab.id == id).map(|_| *group));

        if let Some(tab_group) = tab_group {
            self.tab_groups.get(&tab_group)
        } else {
            None
        }
    }

    /// Initialize the visibility of the system window tab controller.
    pub fn init_visible(cx: &mut App, visible: bool) {
        let mut controller = cx.global_mut::<SystemWindowTabController>();
        if controller.visible.is_none() {
            controller.visible = Some(visible);
        }
    }

    /// Get the visibility of the system window tab controller.
    pub fn is_visible(&self) -> bool {
        self.visible.unwrap_or(false)
    }

    /// Set the visibility of the system window tab controller.
    pub fn set_visible(cx: &mut App, visible: bool) {
        let mut controller = cx.global_mut::<SystemWindowTabController>();
        controller.visible = Some(visible);
    }

    /// Update the last active of a window.
    pub fn update_last_active(cx: &mut App, id: WindowId) {
        let mut controller = cx.global_mut::<SystemWindowTabController>();
        for windows in controller.tab_groups.values_mut() {
            for tab in windows.iter_mut() {
                if tab.id == id {
                    tab.last_active_at = Instant::now();
                }
            }
        }
    }

    /// Update the position of a tab within its group.
    pub fn update_tab_position(cx: &mut App, id: WindowId, ix: usize) {
        let mut controller = cx.global_mut::<SystemWindowTabController>();
        for (_, windows) in controller.tab_groups.iter_mut() {
            if let Some(current_pos) = windows.iter().position(|tab| tab.id == id) {
                if ix < windows.len() && current_pos != ix {
                    let window_tab = windows.remove(current_pos);
                    windows.insert(ix, window_tab);
                }
                break;
            }
        }
    }

    /// Update the title of a tab.
    pub fn update_tab_title(cx: &mut App, id: WindowId, title: SharedString) {
        let controller = cx.global::<SystemWindowTabController>();
        let tab = controller
            .tab_groups
            .values()
            .flat_map(|windows| windows.iter())
            .find(|tab| tab.id == id);

        if tab.map_or(true, |t| t.title == title) {
            return;
        }

        let mut controller = cx.global_mut::<SystemWindowTabController>();
        for windows in controller.tab_groups.values_mut() {
            for tab in windows.iter_mut() {
                if tab.id == id {
                    tab.title = title.clone();
                }
            }
        }
    }

    /// Insert a tab into a tab group.
    pub fn add_tab(cx: &mut App, id: WindowId, tabs: Vec<SystemWindowTab>) {
        let mut controller = cx.global_mut::<SystemWindowTabController>();
        let Some(tab) = tabs.clone().into_iter().find(|tab| tab.id == id) else {
            return;
        };

        let mut expected_tab_ids: Vec<_> = tabs
            .iter()
            .filter(|tab| tab.id != id)
            .map(|tab| tab.id)
            .sorted()
            .collect();

        let mut tab_group_id = None;
        for (group_id, group_tabs) in &controller.tab_groups {
            let tab_ids: Vec<_> = group_tabs.iter().map(|tab| tab.id).sorted().collect();
            if tab_ids == expected_tab_ids {
                tab_group_id = Some(*group_id);
                break;
            }
        }

        if let Some(tab_group_id) = tab_group_id {
            if let Some(tabs) = controller.tab_groups.get_mut(&tab_group_id) {
                tabs.push(tab);
            }
        } else {
            let new_group_id = controller.tab_groups.len();
            controller.tab_groups.insert(new_group_id, tabs);
        }
    }

    /// Remove a tab from a tab group.
    pub fn remove_tab(cx: &mut App, id: WindowId) -> Option<SystemWindowTab> {
        let mut controller = cx.global_mut::<SystemWindowTabController>();
        let mut removed_tab = None;

        controller.tab_groups.retain(|_, tabs| {
            if let Some(pos) = tabs.iter().position(|tab| tab.id == id) {
                removed_tab = Some(tabs.remove(pos));
            }
            !tabs.is_empty()
        });

        removed_tab
    }

    /// Move a tab to a new tab group.
    pub fn move_tab_to_new_window(cx: &mut App, id: WindowId) {
        let mut removed_tab = Self::remove_tab(cx, id);
        let mut controller = cx.global_mut::<SystemWindowTabController>();

        if let Some(tab) = removed_tab {
            let new_group_id = controller.tab_groups.keys().max().map_or(0, |k| k + 1);
            controller.tab_groups.insert(new_group_id, vec![tab]);
        }
    }

    /// Merge all tab groups into a single group.
    pub fn merge_all_windows(cx: &mut App, id: WindowId) {
        let mut controller = cx.global_mut::<SystemWindowTabController>();
        let Some(initial_tabs) = controller.tabs(id) else {
            return;
        };

        let mut all_tabs = initial_tabs.clone();
        for tabs in controller.tab_groups.values() {
            all_tabs.extend(
                tabs.iter()
                    .filter(|tab| !initial_tabs.contains(tab))
                    .cloned(),
            );
        }

        controller.tab_groups.clear();
        controller.tab_groups.insert(0, all_tabs);
    }

    /// Selects the next tab in the tab group in the trailing direction.
    pub fn select_next_tab(cx: &mut App, id: WindowId) {
        let mut controller = cx.global_mut::<SystemWindowTabController>();
        let Some(tabs) = controller.tabs(id) else {
            return;
        };

        let current_index = tabs.iter().position(|tab| tab.id == id).unwrap();
        let next_index = (current_index + 1) % tabs.len();

        let _ = &tabs[next_index].handle.update(cx, |_, window, _| {
            window.activate_window();
        });
    }

    /// Selects the previous tab in the tab group in the leading direction.
    pub fn select_previous_tab(cx: &mut App, id: WindowId) {
        let mut controller = cx.global_mut::<SystemWindowTabController>();
        let Some(tabs) = controller.tabs(id) else {
            return;
        };

        let current_index = tabs.iter().position(|tab| tab.id == id).unwrap();
        let previous_index = if current_index == 0 {
            tabs.len() - 1
        } else {
            current_index - 1
        };

        let _ = &tabs[previous_index].handle.update(cx, |_, window, _| {
            window.activate_window();
        });
    }
}

/// Contains the state of the full application, and passed as a reference to a variety of callbacks.
/// Other [Context] derefs to this type.
/// You need a reference to an `App` to access the state of a [Entity].
pub struct App {
    pub(crate) this: Weak<AppCell>,
    pub(crate) platform: Rc<dyn Platform>,
    pub(crate) accessibility_force_disabled: bool,
    pub(crate) resource_profile: AppResourceProfile,
    text_system: Arc<TextSystem>,
    flushing_effects: bool,
    pending_updates: usize,
    pub(crate) actions: Rc<ActionRegistry>,
    pub(crate) active_drag: Option<AnyDrag>,
    pub(crate) background_executor: BackgroundExecutor,
    pub(crate) foreground_executor: ForegroundExecutor,
    pub(crate) loading_assets: FxHashMap<(TypeId, u64), Box<dyn Any>>,
    asset_source: Arc<dyn AssetSource>,
    pub(crate) svg_renderer: SvgRenderer,
    http_client: Arc<dyn HttpClient>,
    pub(crate) globals_by_type: FxHashMap<TypeId, Box<dyn Any>>,
    pub(crate) entities: EntityMap,
    pub(crate) window_update_stack: Vec<WindowId>,
    pub(crate) new_entity_observers: SubscriberSet<TypeId, NewEntityListener>,
    pub(crate) windows: SlotMap<WindowId, Option<Window>>,
    pub(crate) window_handles: FxHashMap<WindowId, AnyWindowHandle>,
    pub(crate) focus_handles: Arc<FocusMap>,
    pub(crate) keymap: Rc<RefCell<Keymap>>,
    pub(crate) keyboard_layout: Box<dyn PlatformKeyboardLayout>,
    pub(crate) keyboard_mapper: Rc<dyn PlatformKeyboardMapper>,
    pub(crate) global_action_listeners:
        FxHashMap<TypeId, Vec<Rc<dyn Fn(&dyn Any, DispatchPhase, &mut Self)>>>,
    pending_effects: VecDeque<Effect>,
    pub(crate) pending_notifications: FxHashSet<EntityId>,
    pub(crate) pending_global_notifications: FxHashSet<TypeId>,
    pub(crate) observers: SubscriberSet<EntityId, Handler>,
    // TypeId is the type of the event that the listener callback expects
    pub(crate) event_listeners: SubscriberSet<EntityId, (TypeId, Listener)>,
    pub(crate) keystroke_observers: SubscriberSet<(), KeystrokeObserver>,
    pub(crate) keystroke_interceptors: SubscriberSet<(), KeystrokeObserver>,
    pub(crate) keyboard_layout_observers: SubscriberSet<(), Handler>,
    pub(crate) thermal_state_observers: SubscriberSet<(), Handler>,
    pub(crate) release_listeners: SubscriberSet<EntityId, ReleaseListener>,
    pub(crate) global_observers: SubscriberSet<TypeId, Handler>,
    pub(crate) quit_observers: SubscriberSet<(), QuitHandler>,
    pub(crate) restart_observers: SubscriberSet<(), Handler>,
    pub(crate) restart_path: Option<PathBuf>,
    pub(crate) restart_arguments: Vec<OsString>,
    pub(crate) window_closed_observers: SubscriberSet<(), WindowClosedHandler>,
    pub(crate) layout_id_buffer: Vec<LayoutId>, // We recycle this memory across layout requests.
    pub(crate) propagate_event: bool,
    pub(crate) prompt_builder: Option<PromptBuilder>,
    pub(crate) window_invalidators_by_entity:
        FxHashMap<EntityId, FxHashMap<WindowId, WindowInvalidator>>,
    pub(crate) tracked_entities: FxHashMap<WindowId, FxHashSet<EntityId>>,
    #[cfg(any(feature = "inspector", debug_assertions))]
    pub(crate) inspector_renderer: Option<crate::InspectorRenderer>,
    #[cfg(any(feature = "inspector", debug_assertions))]
    pub(crate) inspector_element_registry: InspectorElementRegistry,
    #[cfg(any(test, feature = "test-support", debug_assertions))]
    pub(crate) name: Option<&'static str>,
    quitting: bool,
    pub(crate) quit_mode: QuitMode,
}

impl App {
    #[allow(clippy::new_ret_no_self)]
    pub(crate) fn new_app(
        platform: Rc<dyn Platform>,
        asset_source: Arc<dyn AssetSource>,
        http_client: Arc<dyn HttpClient>,
        resource_profile: AppResourceProfile,
    ) -> Rc<AppCell> {
        let executor = platform.background_executor();
        let foreground_executor = platform.foreground_executor();
        assert!(
            executor.is_main_thread(),
            "must construct App on main thread"
        );

        let text_system = Arc::new(TextSystem::new(
            platform.text_system(),
            &resource_profile.text,
        ));
        platform.configure_gpu_resources(&resource_profile.gpu);
        let entities = EntityMap::new();
        let keyboard_layout = platform.keyboard_layout();
        let keyboard_mapper = platform.keyboard_mapper();

        let app = Rc::new_cyclic(|this| AppCell {
            app: RefCell::new(App {
                this: this.clone(),
                platform: platform.clone(),
                accessibility_force_disabled: false,
                resource_profile,
                text_system,
                actions: Rc::new(ActionRegistry::default()),
                flushing_effects: false,
                pending_updates: 0,
                active_drag: None,
                background_executor: executor,
                foreground_executor,
                svg_renderer: SvgRenderer::new(asset_source.clone()),
                loading_assets: Default::default(),
                asset_source,
                http_client,
                globals_by_type: FxHashMap::default(),
                entities,
                new_entity_observers: SubscriberSet::new(),
                windows: SlotMap::with_key(),
                window_update_stack: Vec::new(),
                window_handles: FxHashMap::default(),
                focus_handles: Arc::new(RwLock::new(SlotMap::with_key())),
                keymap: Rc::new(RefCell::new(Keymap::default())),
                keyboard_layout,
                keyboard_mapper,
                global_action_listeners: FxHashMap::default(),
                pending_effects: VecDeque::new(),
                pending_notifications: FxHashSet::default(),
                pending_global_notifications: FxHashSet::default(),
                observers: SubscriberSet::new(),
                tracked_entities: FxHashMap::default(),
                window_invalidators_by_entity: FxHashMap::default(),
                event_listeners: SubscriberSet::new(),
                release_listeners: SubscriberSet::new(),
                keystroke_observers: SubscriberSet::new(),
                keystroke_interceptors: SubscriberSet::new(),
                keyboard_layout_observers: SubscriberSet::new(),
                thermal_state_observers: SubscriberSet::new(),
                global_observers: SubscriberSet::new(),
                quit_observers: SubscriberSet::new(),
                restart_observers: SubscriberSet::new(),
                restart_path: None,
                restart_arguments: Vec::new(),
                window_closed_observers: SubscriberSet::new(),
                layout_id_buffer: Default::default(),
                propagate_event: true,
                prompt_builder: Some(PromptBuilder::Default),
                #[cfg(any(feature = "inspector", debug_assertions))]
                inspector_renderer: None,
                #[cfg(any(feature = "inspector", debug_assertions))]
                inspector_element_registry: InspectorElementRegistry::default(),
                quitting: false,
                quit_mode: QuitMode::default(),

                #[cfg(any(test, feature = "test-support", debug_assertions))]
                name: None,
            }),
        });

        init_app_menus(platform.as_ref(), &app.borrow());
        SystemWindowTabController::init(&mut app.borrow_mut());

        platform.on_keyboard_layout_change(Box::new({
            let app = Rc::downgrade(&app);
            move || {
                if let Some(app) = app.upgrade() {
                    let cx = &mut app.borrow_mut();
                    cx.keyboard_layout = cx.platform.keyboard_layout();
                    cx.keyboard_mapper = cx.platform.keyboard_mapper();
                    cx.keyboard_layout_observers
                        .clone()
                        .retain(&(), move |callback| (callback)(cx));
                }
            }
        }));

        platform.on_thermal_state_change(Box::new({
            let app = Rc::downgrade(&app);
            move || {
                if let Some(app) = app.upgrade() {
                    let cx = &mut app.borrow_mut();
                    cx.thermal_state_observers
                        .clone()
                        .retain(&(), move |callback| (callback)(cx));
                }
            }
        }));

        platform.on_quit(Box::new({
            let cx = app.clone();
            move || match cx.try_borrow_mut() {
                Ok(mut cx) => {
                    cx.shutdown();
                    true
                }
                Err(_) => {
                    // Quit was requested while the AppCell was borrowed, so we
                    // can't shut down synchronously. The platform decides how
                    // to proceed (Windows Restart Manager posts WM_QUIT).
                    false
                }
            }
        }));

        app
    }

    /// Quit the application gracefully. Handlers registered with [`Context::on_app_quit`]
    /// will be given 100ms to complete before exiting.
    pub fn shutdown(&mut self) {
        let mut futures = Vec::new();

        for observer in self.quit_observers.remove(&()) {
            futures.push(observer(self));
        }

        self.windows.clear();
        self.window_handles.clear();
        self.flush_effects();
        self.quitting = true;

        let futures = futures::future::join_all(futures);
        if self
            .background_executor
            .block_with_timeout(SHUTDOWN_TIMEOUT, futures)
            .is_err()
        {
            log::error!("timed out waiting on app_will_quit");
        }

        self.quitting = false;
    }

    /// Get the id of the current keyboard layout
    pub fn keyboard_layout(&self) -> &dyn PlatformKeyboardLayout {
        self.keyboard_layout.as_ref()
    }

    /// Get the current keyboard mapper.
    pub fn keyboard_mapper(&self) -> &Rc<dyn PlatformKeyboardMapper> {
        &self.keyboard_mapper
    }

    /// Invokes a handler when the current keyboard layout changes
    pub fn on_keyboard_layout_change<F>(&self, mut callback: F) -> Subscription
    where
        F: 'static + FnMut(&mut App),
    {
        let (subscription, activate) = self.keyboard_layout_observers.insert(
            (),
            Box::new(move |cx| {
                callback(cx);
                true
            }),
        );
        activate();
        subscription
    }

    /// Returns the current thermal state of the system.
    pub fn thermal_state(&self) -> ThermalState {
        self.platform.thermal_state()
    }

    /// Prevents idle sleep while the returned guard is held.
    ///
    /// Dropping the guard restores the previous sleep policy.
    pub fn prevent_idle_sleep(&self, reason: &str) -> Task<Result<crate::ActivityGuard>> {
        self.platform.prevent_idle_sleep(reason)
    }

    /// Invokes a handler when the thermal state changes.
    pub fn on_thermal_state_change<F>(&self, mut callback: F) -> Subscription
    where
        F: 'static + FnMut(&mut App),
    {
        let (subscription, activate) = self.thermal_state_observers.insert(
            (),
            Box::new(move |cx| {
                callback(cx);
                true
            }),
        );
        activate();
        subscription
    }

    /// Gracefully quit the application via the platform's standard routine.
    pub fn quit(&self) {
        self.platform.quit();
    }

    /// Ask the platform renderer to drop idle pooled GPU resources where
    /// supported.
    ///
    /// Intended for long-running applications (tray icons, notification
    /// popups) that want to reclaim GPU memory during idle periods such as
    /// after the last visible window is hidden. The renderer will re-allocate
    /// resources on demand on the next frame.
    ///
    /// This is a best-effort operation. Every open window's renderer is
    /// trimmed first, then the platform-level shared caches; platforms that
    /// do not maintain trimmable GPU pools may no-op, and active renderer
    /// resources are kept alive until they can be safely released.
    pub fn trim_gpu_caches(&self) {
        for (_, window) in self.windows.iter() {
            if let Some(window) = window {
                window.platform_window.trim_renderer_caches();
            }
        }
        self.platform.trim_renderer_caches();
    }

    /// Snapshot of the renderer's pooled GPU resource usage. See
    /// [`crate::RendererCacheStats`].
    ///
    /// Intended primarily for diagnostics and benchmarking. Useful as a guard
    /// before calling [`App::trim_gpu_caches`] when callers want to skip the
    /// trim if no buffers are idle.
    pub fn renderer_cache_stats(&self) -> crate::RendererCacheStats {
        self.platform.renderer_cache_stats()
    }

    /// Schedules all windows in the application to be redrawn. This can be called
    /// multiple times in an update cycle and still result in a single redraw.
    pub fn refresh_windows(&mut self) {
        self.pending_effects.push_back(Effect::RefreshWindows);
    }

    pub(crate) fn update<R>(&mut self, update: impl FnOnce(&mut Self) -> R) -> R {
        self.start_update();
        let result = update(self);
        self.finish_update();
        result
    }

    pub(crate) fn start_update(&mut self) {
        self.pending_updates += 1;
    }

    pub(crate) fn finish_update(&mut self) {
        if !self.flushing_effects && self.pending_updates == 1 {
            self.flushing_effects = true;
            self.flush_effects();
            self.flushing_effects = false;
        }
        self.pending_updates -= 1;
    }

    /// Arrange a callback to be invoked when the given entity calls `notify` on its respective context.
    pub fn observe<W>(
        &mut self,
        entity: &Entity<W>,
        mut on_notify: impl FnMut(Entity<W>, &mut App) + 'static,
    ) -> Subscription
    where
        W: 'static,
    {
        self.observe_internal(entity, move |e, cx| {
            on_notify(e, cx);
            true
        })
    }

    pub(crate) fn detect_accessed_entities<R>(
        &mut self,
        callback: impl FnOnce(&mut App) -> R,
    ) -> (R, FxHashSet<EntityId>) {
        let accessed_entities_start = self.entities.accessed_entities.borrow().clone();
        let result = callback(self);
        let accessed_entities_end = self.entities.accessed_entities.borrow().clone();
        let entities_accessed_in_callback = accessed_entities_end
            .difference(&accessed_entities_start)
            .copied()
            .collect::<FxHashSet<EntityId>>();
        (result, entities_accessed_in_callback)
    }

    pub(crate) fn record_entities_accessed(
        &mut self,
        window_handle: AnyWindowHandle,
        invalidator: WindowInvalidator,
        entities: &FxHashSet<EntityId>,
    ) {
        let mut tracked_entities =
            std::mem::take(self.tracked_entities.entry(window_handle.id).or_default());
        for entity in tracked_entities.iter() {
            self.window_invalidators_by_entity
                .entry(*entity)
                .and_modify(|windows| {
                    windows.remove(&window_handle.id);
                });
        }
        for entity in entities.iter() {
            self.window_invalidators_by_entity
                .entry(*entity)
                .or_default()
                .insert(window_handle.id, invalidator.clone());
        }
        tracked_entities.clear();
        tracked_entities.extend(entities.iter().copied());
        self.tracked_entities
            .insert(window_handle.id, tracked_entities);
    }

    pub(crate) fn new_observer(&mut self, key: EntityId, value: Handler) -> Subscription {
        let (subscription, activate) = self.observers.insert(key, value);
        self.defer(move |_| activate());
        subscription
    }

    pub(crate) fn observe_internal<W>(
        &mut self,
        entity: &Entity<W>,
        mut on_notify: impl FnMut(Entity<W>, &mut App) -> bool + 'static,
    ) -> Subscription
    where
        W: 'static,
    {
        let entity_id = entity.entity_id();
        let handle = entity.downgrade();
        self.new_observer(
            entity_id,
            Box::new(move |cx| {
                if let Some(entity) = handle.upgrade() {
                    on_notify(entity, cx)
                } else {
                    false
                }
            }),
        )
    }

    /// Arrange for the given callback to be invoked whenever the given entity emits an event of a given type.
    /// The callback is provided a handle to the emitting entity and a reference to the emitted event.
    pub fn subscribe<T, Event>(
        &mut self,
        entity: &Entity<T>,
        mut on_event: impl FnMut(Entity<T>, &Event, &mut App) + 'static,
    ) -> Subscription
    where
        T: 'static + EventEmitter<Event>,
        Event: 'static,
    {
        self.subscribe_internal(entity, move |entity, event, cx| {
            on_event(entity, event, cx);
            true
        })
    }

    pub(crate) fn new_subscription(
        &mut self,
        key: EntityId,
        value: (TypeId, Listener),
    ) -> Subscription {
        let (subscription, activate) = self.event_listeners.insert(key, value);
        self.defer(move |_| activate());
        subscription
    }
    pub(crate) fn subscribe_internal<T, Evt>(
        &mut self,
        entity: &Entity<T>,
        mut on_event: impl FnMut(Entity<T>, &Evt, &mut App) -> bool + 'static,
    ) -> Subscription
    where
        T: 'static + EventEmitter<Evt>,
        Evt: 'static,
    {
        let entity_id = entity.entity_id();
        let handle = entity.downgrade();
        self.new_subscription(
            entity_id,
            (
                TypeId::of::<Evt>(),
                Box::new(move |event, cx| {
                    let event: &Evt = event.downcast_ref().expect("invalid event type");
                    if let Some(entity) = handle.upgrade() {
                        on_event(entity, event, cx)
                    } else {
                        false
                    }
                }),
            ),
        )
    }

    /// Returns handles to all open windows in the application.
    /// Each handle could be downcast to a handle typed for the root view of that window.
    /// To find all windows of a given type, you could filter on
    pub fn windows(&self) -> Vec<AnyWindowHandle> {
        self.windows
            .keys()
            .flat_map(|window_id| self.window_handles.get(&window_id).copied())
            .collect()
    }

    /// Returns the window handles ordered by their appearance on screen, front to back.
    ///
    /// The first window in the returned list is the active/topmost window of the application.
    ///
    /// This method returns None if the platform doesn't implement the method yet.
    pub fn window_stack(&self) -> Option<Vec<AnyWindowHandle>> {
        self.platform.window_stack()
    }

    /// Returns a handle to the window that is currently focused at the platform level, if one exists.
    pub fn active_window(&self) -> Option<AnyWindowHandle> {
        self.platform.active_window()
    }

    /// Opens a new window with the given option and the root view returned by the given function.
    /// The function is invoked with a `Window`, which can be used to interact with window-specific
    /// functionality.
    pub fn open_window<V: 'static + Render>(
        &mut self,
        options: crate::WindowOptions,
        build_root_view: impl FnOnce(&mut Window, &mut App) -> Entity<V>,
    ) -> anyhow::Result<WindowHandle<V>> {
        self.update(|cx| {
            let id = cx.windows.insert(None);
            let handle = WindowHandle::new(id);
            match Window::new(handle.into(), options, cx) {
                Ok(mut window) => {
                    cx.window_update_stack.push(id);
                    let root_view = build_root_view(&mut window, cx);
                    cx.window_update_stack.pop();
                    window.root.replace(root_view.into());
                    window.defer(cx, |window: &mut Window, cx| window.appearance_changed(cx));

                    // allow a window to draw at least once before returning
                    // this didn't cause any issues on non windows platforms as it seems we always won the race to on_request_frame
                    // on windows we quite frequently lose the race and return a window that has never rendered, which leads to a crash
                    // where DispatchTree::root_node_id asserts on empty nodes
                    let clear = window.draw(cx);
                    clear.clear();

                    cx.window_handles.insert(id, window.handle);
                    cx.windows.get_mut(id).unwrap().replace(window);
                    Ok(handle)
                }
                Err(e) => {
                    cx.windows.remove(id);
                    Err(e)
                }
            }
        })
    }

    /// Instructs the platform to activate the application by bringing it to the foreground.
    pub fn activate(&self, ignoring_other_apps: bool) {
        self.platform.activate(ignoring_other_apps);
    }

    /// Hide the application at the platform level.
    pub fn hide(&self) {
        self.platform.hide();
    }

    /// Hide other applications at the platform level.
    pub fn hide_other_apps(&self) {
        self.platform.hide_other_apps();
    }

    /// Unhide other applications at the platform level.
    pub fn unhide_other_apps(&self) {
        self.platform.unhide_other_apps();
    }

    /// Set the system tray icon.
    pub fn set_tray_icon(&self, icon: Option<&[u8]>) {
        self.platform.set_tray_icon(icon);
    }

    /// Set how the system tray icon should be rendered.
    pub fn set_tray_icon_rendering_mode(&self, rendering_mode: TrayIconRenderingMode) {
        self.platform.set_tray_icon_rendering_mode(rendering_mode);
    }

    /// Set the system tray menu items.
    pub fn set_tray_menu(&self, menu: Vec<TrayMenuItem>) {
        self.platform.set_tray_menu(menu);
    }

    /// Set the system tray tooltip.
    pub fn set_tray_tooltip(&self, tooltip: &str) {
        self.platform.set_tray_tooltip(tooltip);
    }

    /// Enable or disable tray panel mode.
    ///
    /// macOS only. When enabled, an attached menu is detached from the tray
    /// icon so clicks fire [`TrayIconEvent`]s instead of opening the menu.
    /// With no menu attached, clicks fire events regardless of this setting.
    pub fn set_tray_panel_mode(&self, enabled: bool) {
        self.platform.set_tray_panel_mode(enabled);
    }

    #[allow(missing_docs)]
    pub fn tray_icon_anchor(&self) -> Option<TrayAnchor> {
        self.platform.get_tray_icon_anchor()
    }

    /// Get the screen bounds of the tray icon, useful for positioning a panel below it.
    pub fn tray_icon_bounds(&self) -> Option<Bounds<Pixels>> {
        self.platform.get_tray_icon_bounds()
    }

    /// Build an approximate tray anchor from a screen position.
    ///
    /// This is useful on Linux StatusNotifierItem hosts, where click events
    /// provide a logical screen-coordinate hint but not the actual tray icon
    /// bounds. If the hint falls outside known displays, the primary display is
    /// used as a best-effort fallback so callers can still fall back from
    /// `WindowPosition::TrayAnchored` to a corner position if needed.
    pub fn tray_anchor_for_position(&self, position: Point<Pixels>) -> Option<TrayAnchor> {
        // This is a logical-size approximation of a tray icon, not a measured
        // host icon size. Linux SNI does not expose the actual icon bounds.
        const APPROXIMATE_TRAY_ICON_SIZE: Pixels = px(24.0);

        let displays = self.displays();
        let display = displays
            .iter()
            .find(|display| display.bounds().contains(&position))
            .cloned()
            .or_else(|| self.primary_display())?;
        let display_bounds = display.bounds();
        let half_size = APPROXIMATE_TRAY_ICON_SIZE * 0.5;
        let local_position = position - display_bounds.origin;

        Some(TrayAnchor {
            display_id: display.id(),
            bounds: Bounds::new(
                point(local_position.x - half_size, local_position.y - half_size),
                size(APPROXIMATE_TRAY_ICON_SIZE, APPROXIMATE_TRAY_ICON_SIZE),
            ),
        })
    }

    /// Register a callback for system tray icon events.
    ///
    /// Platform notes:
    /// - macOS: clicks are delivered whenever no menu is attached to the tray
    ///   icon; call [`Self::set_tray_panel_mode(true)`](Self::set_tray_panel_mode)
    ///   to detach a menu and receive clicks instead of it. Left, right, and
    ///   double clicks are distinguished. Callbacks run on the next main
    ///   queue turn after the click, not synchronously inside it.
    /// - Linux (StatusNotifierItem): left/secondary activation is mapped to
    ///   `LeftClick`/`RightClick` with a position hint; not every host (e.g.
    ///   some AppIndicator extensions) delivers activation events at all.
    pub fn on_tray_icon_event(&self, mut callback: impl FnMut(TrayIconEvent, &mut App) + 'static) {
        let this = self.this.clone();
        self.platform.on_tray_icon_event(Box::new(move |event| {
            if let Some(app) = this.upgrade() {
                callback(event, &mut app.borrow_mut());
            }
        }));
    }

    /// Register a callback for system tray icon click events with optional
    /// position information.
    ///
    /// See [`Self::on_tray_icon_event`] for delivery semantics. The
    /// `position` field carries a logical screen-coordinate hint where the
    /// platform provides one (Linux SNI); it is `None` on macOS, which has
    /// no click-position equivalent — use [`Self::tray_icon_bounds`] there.
    pub fn on_tray_icon_click_event(
        &self,
        mut callback: impl FnMut(TrayIconClickEvent, &mut App) + 'static,
    ) {
        let this = self.this.clone();
        self.platform
            .on_tray_icon_click_event(Box::new(move |event| {
                if let Some(app) = this.upgrade() {
                    callback(event, &mut app.borrow_mut());
                }
            }));
    }

    /// Register a callback for when a tray menu item is clicked.
    pub fn on_tray_menu_action(&self, mut callback: impl FnMut(SharedString, &mut App) + 'static) {
        let this = self.this.clone();
        self.platform.on_tray_menu_action(Box::new(move |id| {
            if let Some(app) = this.upgrade() {
                callback(id, &mut app.borrow_mut());
            }
        }));
    }

    /// Register a global hotkey with the given ID and keystroke.
    pub fn register_global_hotkey(&self, id: u32, keystroke: &Keystroke) -> Result<()> {
        self.platform.register_global_hotkey(id, keystroke)
    }

    /// Unregister a previously registered global hotkey.
    pub fn unregister_global_hotkey(&self, id: u32) {
        self.platform.unregister_global_hotkey(id);
    }

    /// Register a callback for global hotkey events.
    pub fn on_global_hotkey(&self, callback: impl FnMut(u32) + 'static) {
        self.platform.on_global_hotkey(Box::new(callback));
    }

    /// Get information about the currently focused window from any application.
    pub fn focused_window_info(&self) -> Option<FocusedWindowInfo> {
        self.platform.focused_window_info()
    }

    /// Check accessibility permission status.
    pub fn accessibility_status(&self) -> PermissionStatus {
        self.platform.accessibility_status()
    }

    /// Request accessibility permission from the user.
    pub fn request_accessibility_permission(&self) -> PermissionRequestStatus {
        self.platform.request_accessibility_permission()
    }

    /// Check microphone permission status.
    pub fn microphone_status(&self) -> PermissionStatus {
        self.platform.microphone_status()
    }

    /// Request microphone permission from the user.
    pub fn request_microphone_permission(
        &self,
        callback: impl FnOnce(bool) + 'static,
    ) -> PermissionRequestStatus {
        self.platform
            .request_microphone_permission(Box::new(callback))
    }

    /// Set whether the application should auto-launch at login.
    pub fn set_auto_launch(&self, app_id: &str, enabled: bool) -> Result<()> {
        self.platform.set_auto_launch(app_id, enabled)
    }

    /// Check whether the application is set to auto-launch at login.
    pub fn is_auto_launch_enabled(&self, app_id: &str) -> bool {
        self.platform.is_auto_launch_enabled(app_id)
    }

    /// Show an OS notification.
    pub fn show_notification(&self, title: &str, body: &str) -> Result<()> {
        self.platform.show_notification(title, body)
    }

    /// Configures when the application should automatically quit.
    /// By default, [`QuitMode::Default`] is used.
    pub fn set_quit_mode(&mut self, mode: QuitMode) {
        self.quit_mode = mode;
        self.platform.set_quit_mode(mode);
    }

    /// Register a callback for system power events (sleep, wake, shutdown).
    pub fn on_system_power_event(
        &self,
        mut callback: impl FnMut(SystemPowerEvent, &mut App) + 'static,
    ) {
        let this = self.this.clone();
        self.platform.on_system_power_event(Box::new(move |event| {
            if let Some(app) = this.upgrade() {
                callback(event, &mut app.borrow_mut());
            }
        }));
    }

    /// Start a power save blocker to prevent the system from sleeping or the display from dimming.
    pub fn start_power_save_blocker(&self, kind: PowerSaveBlockerKind) -> Option<u32> {
        self.platform.start_power_save_blocker(kind)
    }

    /// Stop a previously started power save blocker by its ID.
    pub fn stop_power_save_blocker(&self, id: u32) {
        self.platform.stop_power_save_blocker(id);
    }

    /// Get the duration since the last user input event.
    pub fn system_idle_time(&self) -> Option<Duration> {
        self.platform.system_idle_time()
    }

    /// Get the current network connectivity status.
    pub fn network_status(&self) -> NetworkStatus {
        self.platform.network_status()
    }

    /// Register a callback for network connectivity status changes.
    pub fn on_network_status_change(
        &self,
        mut callback: impl FnMut(NetworkStatus, &mut App) + 'static,
    ) {
        let this = self.this.clone();
        self.platform
            .on_network_status_change(Box::new(move |status| {
                if let Some(app) = this.upgrade() {
                    callback(status, &mut app.borrow_mut());
                }
            }));
    }

    /// Register a callback for media key events (play, pause, next, previous).
    pub fn on_media_key_event(&self, mut callback: impl FnMut(MediaKeyEvent, &mut App) + 'static) {
        let this = self.this.clone();
        self.platform.on_media_key_event(Box::new(move |event| {
            if let Some(app) = this.upgrade() {
                callback(event, &mut app.borrow_mut());
            }
        }));
    }

    /// Request the user's attention by bouncing the dock icon or flashing the taskbar.
    pub fn request_user_attention(&self, attention_type: AttentionType) {
        self.platform.request_user_attention(attention_type);
    }

    /// Cancel a previous user attention request.
    pub fn cancel_user_attention(&self) {
        self.platform.cancel_user_attention();
    }

    /// Set the dock badge label (macOS) or taskbar overlay text.
    pub fn set_dock_badge(&self, label: Option<&str>) {
        self.platform.set_dock_badge(label);
    }

    /// Show a context menu at the given screen position with the specified menu items.
    pub fn show_context_menu(
        &self,
        position: Point<Pixels>,
        items: Vec<TrayMenuItem>,
        mut callback: impl FnMut(SharedString, &mut App) + 'static,
    ) {
        let this = self.this.clone();
        self.platform.show_context_menu(
            position,
            items,
            Box::new(move |id| {
                if let Some(app) = this.upgrade() {
                    callback(id, &mut app.borrow_mut());
                }
            }),
        );
    }

    /// Show a native dialog with the given options, returning the index of the clicked button.
    pub fn show_dialog(&self, options: DialogOptions) -> oneshot::Receiver<usize> {
        self.platform.show_dialog(options)
    }

    /// Get operating system information (name, version, architecture).
    pub fn os_info(&self) -> OsInfo {
        self.platform.os_info()
    }

    /// Check whether biometric authentication (Touch ID, Windows Hello) is available.
    pub fn biometric_status(&self) -> BiometricStatus {
        self.platform.biometric_status()
    }

    /// Authenticate the user via biometrics with the given reason string.
    pub fn authenticate_biometric(
        &self,
        reason: &str,
        callback: impl FnOnce(bool) + Send + 'static,
    ) {
        self.platform
            .authenticate_biometric(reason, Box::new(callback));
    }

    /// Install a panic hook that captures crash reports with backtraces and OS info.
    pub fn set_crash_handler(
        &self,
        app_version: Option<String>,
        handler: impl Fn(CrashReport) + Send + Sync + 'static,
    ) {
        let os_info = self.platform.os_info();
        let handler = std::sync::Arc::new(handler);
        std::panic::set_hook(Box::new(move |panic_info| {
            let message = if let Some(msg) = panic_info.payload().downcast_ref::<&str>() {
                msg.to_string()
            } else if let Some(msg) = panic_info.payload().downcast_ref::<String>() {
                msg.clone()
            } else {
                "Unknown panic".to_string()
            };

            let location = panic_info
                .location()
                .map(|loc| format!("{}:{}:{}", loc.file(), loc.line(), loc.column()))
                .unwrap_or_default();

            let full_message = if location.is_empty() {
                message
            } else {
                format!("{message} at {location}")
            };

            let backtrace = std::backtrace::Backtrace::force_capture().to_string();

            let report = CrashReport {
                message: full_message,
                backtrace,
                os_info: os_info.clone(),
                app_version: app_version.clone(),
            };

            handler(report);
        }));
    }

    /// Returns the list of currently active displays.
    pub fn displays(&self) -> Vec<Rc<dyn PlatformDisplay>> {
        self.platform.displays()
    }

    /// Returns the primary display that will be used for new windows.
    pub fn primary_display(&self) -> Option<Rc<dyn PlatformDisplay>> {
        self.platform.primary_display()
    }

    /// Compute window bounds from a desired size and a semantic position.
    pub fn compute_window_bounds(
        &self,
        size: Size<Pixels>,
        position: &WindowPosition,
    ) -> Bounds<Pixels> {
        let displays = self.platform.displays();
        let primary = self.platform.primary_display();
        crate::platform::window_positioner::compute_window_bounds(
            size,
            position,
            &displays,
            primary.as_ref(),
        )
    }

    /// Returns whether `screen_capture_sources` may work.
    pub fn is_screen_capture_supported(&self) -> bool {
        self.platform.is_screen_capture_supported()
    }

    /// Returns a list of available screen capture sources.
    pub fn screen_capture_sources(
        &self,
    ) -> oneshot::Receiver<Result<Vec<Rc<dyn ScreenCaptureSource>>>> {
        self.platform.screen_capture_sources()
    }

    /// Returns the display with the given ID, if one exists.
    pub fn find_display(&self, id: DisplayId) -> Option<Rc<dyn PlatformDisplay>> {
        self.displays()
            .iter()
            .find(|display| display.id() == id)
            .cloned()
    }

    /// Returns the appearance of the application's windows.
    pub fn window_appearance(&self) -> WindowAppearance {
        self.platform.window_appearance()
    }

    /// Overrides the appearance (light/dark) applied to the app's windows, independent of
    /// the OS-wide setting. Pass `None` to clear the override and follow the system again.
    /// The current value is reported by [`App::window_appearance`].
    ///
    /// On macOS this sets the underlying `NSApplication.appearance`, which controls the
    /// native window chrome (the window border and titlebar) of every window. Use this
    /// when the app uses a dark theme while the system is in light mode (or vice versa)
    /// so the window edges render to match the theme. While an appearance is forced,
    /// windows stop tracking system light/dark changes; pass `None` to resume following
    /// the system. On other platforms the override is stored and reported by
    /// [`App::window_appearance`] without changing native chrome.
    pub fn set_window_appearance(&self, appearance: Option<WindowAppearance>) {
        self.platform.set_window_appearance(appearance);
    }

    /// Writes data to the primary selection buffer.
    /// Only available on Linux.
    #[cfg(any(target_os = "linux", target_os = "freebsd"))]
    pub fn write_to_primary(&self, item: ClipboardItem) {
        self.platform.write_to_primary(item)
    }

    /// Writes data to the platform clipboard.
    pub fn write_to_clipboard(&self, item: ClipboardItem) {
        self.platform.write_to_clipboard(item)
    }

    /// Reads data from the primary selection buffer.
    /// Only available on Linux.
    #[cfg(any(target_os = "linux", target_os = "freebsd"))]
    pub fn read_from_primary(&self) -> Option<ClipboardItem> {
        self.platform.read_from_primary()
    }

    /// Reads data from the platform clipboard.
    pub fn read_from_clipboard(&self) -> Option<ClipboardItem> {
        self.platform.read_from_clipboard()
    }

    /// Writes credentials to the platform keychain.
    pub fn write_credentials(
        &self,
        url: &str,
        username: &str,
        password: &[u8],
    ) -> Task<Result<()>> {
        self.platform.write_credentials(url, username, password)
    }

    /// Reads credentials from the platform keychain.
    pub fn read_credentials(&self, url: &str) -> Task<Result<Option<(String, Vec<u8>)>>> {
        self.platform.read_credentials(url)
    }

    /// Deletes credentials from the platform keychain.
    pub fn delete_credentials(&self, url: &str) -> Task<Result<()>> {
        self.platform.delete_credentials(url)
    }

    /// Directs the platform's default browser to open the given URL.
    pub fn open_url(&self, url: &str) {
        self.platform.open_url(url);
    }

    /// Registers the given URL scheme (e.g. `zed` for `zed://` urls) to be
    /// opened by the current app.
    ///
    /// On some platforms (e.g. macOS) you may be able to register URL schemes
    /// as part of app distribution, but this method exists to let you register
    /// schemes at runtime.
    pub fn register_url_scheme(&self, scheme: &str) -> Task<Result<()>> {
        self.platform.register_url_scheme(scheme)
    }

    /// Returns the full pathname of the current app bundle.
    ///
    /// Returns an error if the app is not being run from a bundle.
    pub fn app_path(&self) -> Result<PathBuf> {
        self.platform.app_path()
    }

    /// On Linux, returns the name of the compositor in use.
    ///
    /// Returns an empty string on other platforms.
    pub fn compositor_name(&self) -> &'static str {
        self.platform.compositor_name()
    }

    /// Returns the file URL of the executable with the specified name in the application bundle
    pub fn path_for_auxiliary_executable(&self, name: &str) -> Result<PathBuf> {
        self.platform.path_for_auxiliary_executable(name)
    }

    /// Displays a platform modal for selecting paths.
    ///
    /// When one or more paths are selected, they'll be relayed asynchronously via the returned oneshot channel.
    /// If cancelled, a `None` will be relayed instead.
    /// May return an error on Linux if the file picker couldn't be opened.
    pub fn prompt_for_paths(
        &self,
        options: PathPromptOptions,
    ) -> oneshot::Receiver<Result<Option<Vec<PathBuf>>>> {
        self.platform.prompt_for_paths(options)
    }

    /// Displays a platform modal for selecting a new path where a file can be saved.
    ///
    /// The provided directory will be used to set the initial location.
    /// When a path is selected, it is relayed asynchronously via the returned oneshot channel.
    /// If cancelled, a `None` will be relayed instead.
    /// May return an error on Linux if the file picker couldn't be opened.
    pub fn prompt_for_new_path(
        &self,
        directory: &Path,
        suggested_name: Option<&str>,
    ) -> oneshot::Receiver<Result<Option<PathBuf>>> {
        self.platform.prompt_for_new_path(directory, suggested_name)
    }

    /// Reveals the specified path at the platform level, such as in Finder on macOS.
    pub fn reveal_path(&self, path: &Path) {
        self.platform.reveal_path(path)
    }

    /// Opens the specified path with the system's default application.
    pub fn open_with_system(&self, path: &Path) {
        self.platform.open_with_system(path)
    }

    /// Returns whether the user has configured scrollbars to auto-hide at the platform level.
    pub fn should_auto_hide_scrollbars(&self) -> bool {
        self.platform.should_auto_hide_scrollbars()
    }

    /// Restarts the application.
    pub fn restart(&mut self) {
        self.restart_observers
            .clone()
            .retain(&(), |observer| observer(self));
        self.platform.restart(
            self.restart_path.take(),
            mem::take(&mut self.restart_arguments),
        )
    }

    /// Sets the path to use when restarting the application.
    pub fn set_restart_path(&mut self, path: PathBuf) {
        self.restart_path = Some(path);
    }

    /// Returns the HTTP client for the application.
    pub fn http_client(&self) -> Arc<dyn HttpClient> {
        self.http_client.clone()
    }

    /// Sets the HTTP client for the application.
    pub fn set_http_client(&mut self, new_client: Arc<dyn HttpClient>) {
        self.http_client = new_client;
    }

    /// Returns the SVG renderer used by the application.
    pub fn svg_renderer(&self) -> SvgRenderer {
        self.svg_renderer.clone()
    }

    pub(crate) fn push_effect(&mut self, effect: Effect) {
        match &effect {
            Effect::Notify { emitter } => {
                if !self.pending_notifications.insert(*emitter) {
                    return;
                }
            }
            Effect::NotifyGlobalObservers { global_type } => {
                if !self.pending_global_notifications.insert(*global_type) {
                    return;
                }
            }
            _ => {}
        };

        self.pending_effects.push_back(effect);
    }

    /// Called at the end of [`App::update`] to complete any side effects
    /// such as notifying observers, emitting events, etc. Effects can themselves
    /// cause effects, so we continue looping until all effects are processed.
    fn flush_effects(&mut self) {
        loop {
            self.release_dropped_entities();
            self.release_dropped_focus_handles();
            if let Some(effect) = self.pending_effects.pop_front() {
                match effect {
                    Effect::Notify { emitter } => {
                        self.apply_notify_effect(emitter);
                    }

                    Effect::Emit {
                        emitter,
                        event_type,
                        event,
                    } => self.apply_emit_effect(emitter, event_type, event),

                    Effect::RefreshWindows => {
                        self.apply_refresh_effect();
                    }

                    Effect::NotifyGlobalObservers { global_type } => {
                        self.apply_notify_global_observers_effect(global_type);
                    }

                    Effect::Defer { callback } => {
                        self.apply_defer_effect(callback);
                    }
                    Effect::EntityCreated {
                        entity,
                        tid,
                        window,
                    } => {
                        self.apply_entity_created_effect(entity, tid, window);
                    }
                }
            } else {
                #[cfg(any(test, feature = "test-support"))]
                for window in self
                    .windows
                    .values()
                    .filter_map(|window| {
                        let window = window.as_ref()?;
                        window.invalidator.is_dirty().then_some(window.handle)
                    })
                    .collect::<Vec<_>>()
                {
                    self.update_window(window, |_, window, cx| window.draw(cx).clear())
                        .unwrap();
                }

                if self.pending_effects.is_empty() {
                    for window in self.windows.values().filter_map(|window| window.as_ref()) {
                        if window.invalidator.is_dirty()
                            || window.needs_present.get()
                            || !window.next_frame_callbacks.borrow().is_empty()
                        {
                            window.platform_window.schedule_frame();
                        }
                    }

                    break;
                }
            }
        }
    }

    /// Repeatedly called during `flush_effects` to release any entities whose
    /// reference count has become zero. We invoke any release observers before dropping
    /// each entity.
    fn release_dropped_entities(&mut self) {
        loop {
            let dropped = self.entities.take_dropped();
            if dropped.is_empty() {
                break;
            }

            for (entity_id, mut entity) in dropped {
                self.observers.remove(&entity_id);
                self.event_listeners.remove(&entity_id);
                for release_callback in self.release_listeners.remove(&entity_id) {
                    release_callback(entity.as_mut(), self);
                }
            }
        }
    }

    /// Repeatedly called during `flush_effects` to handle a focused handle being dropped.
    fn release_dropped_focus_handles(&mut self) {
        self.focus_handles
            .clone()
            .write()
            .retain(|handle_id, focus| {
                if focus.ref_count.load(SeqCst) == 0 {
                    for window_handle in self.windows() {
                        window_handle
                            .update(self, |_, window, cx| {
                                if window.focus == Some(handle_id) {
                                    window.blur(cx);
                                }
                            })
                            .unwrap();
                    }
                    false
                } else {
                    true
                }
            });
    }

    fn apply_notify_effect(&mut self, emitter: EntityId) {
        self.pending_notifications.remove(&emitter);

        self.observers
            .clone()
            .retain(&emitter, |handler| handler(self));
    }

    fn apply_emit_effect(&mut self, emitter: EntityId, event_type: TypeId, event: Box<dyn Any>) {
        self.event_listeners
            .clone()
            .retain(&emitter, |(stored_type, handler)| {
                if *stored_type == event_type {
                    handler(event.as_ref(), self)
                } else {
                    true
                }
            });
    }

    fn apply_refresh_effect(&mut self) {
        for window in self.windows.values_mut() {
            if let Some(window) = window.as_mut() {
                window.refreshing = true;
                window.invalidator.set_dirty(true);
            }
        }
    }

    fn apply_notify_global_observers_effect(&mut self, type_id: TypeId) {
        self.pending_global_notifications.remove(&type_id);
        self.global_observers
            .clone()
            .retain(&type_id, |observer| observer(self));
    }

    fn apply_defer_effect(&mut self, callback: Box<dyn FnOnce(&mut Self) + 'static>) {
        callback(self);
    }

    fn apply_entity_created_effect(
        &mut self,
        entity: AnyEntity,
        tid: TypeId,
        window: Option<WindowId>,
    ) {
        self.new_entity_observers.clone().retain(&tid, |observer| {
            if let Some(id) = window {
                self.update_window_id(id, {
                    let entity = entity.clone();
                    |_, window, cx| (observer)(entity, &mut Some(window), cx)
                })
                .expect("All windows should be off the stack when flushing effects");
            } else {
                (observer)(entity.clone(), &mut None, self)
            }
            true
        });
    }

    fn update_window_id<T, F>(&mut self, id: WindowId, update: F) -> Result<T>
    where
        F: FnOnce(AnyView, &mut Window, &mut App) -> T,
    {
        self.update(|cx| {
            let mut window = cx.windows.get_mut(id)?.take()?;

            let root_view = window.root.clone().unwrap();

            cx.window_update_stack.push(window.handle.id);
            let result = update(root_view, &mut window, cx);
            cx.window_update_stack.pop();

            if window.removed {
                cx.window_handles.remove(&id);
                cx.windows.remove(id);

                cx.window_closed_observers.clone().retain(&(), |callback| {
                    callback(cx);
                    true
                });

                let quit_on_empty = match cx.quit_mode {
                    QuitMode::Explicit => false,
                    QuitMode::LastWindowClosed => true,
                    QuitMode::Default => cfg!(not(target_os = "macos")),
                };

                if quit_on_empty && cx.windows.is_empty() {
                    cx.quit();
                }
            } else {
                cx.windows.get_mut(id)?.replace(window);
            }

            Some(result)
        })
        .context("window not found")
    }

    /// Creates an `AsyncApp`, which can be cloned and has a static lifetime
    /// so it can be held across `await` points.
    pub fn to_async(&self) -> AsyncApp {
        AsyncApp {
            app: self.this.clone(),
            background_executor: self.background_executor.clone(),
            foreground_executor: self.foreground_executor.clone(),
        }
    }

    /// Obtains a reference to the executor, which can be used to spawn futures.
    pub fn background_executor(&self) -> &BackgroundExecutor {
        &self.background_executor
    }

    /// Obtains a reference to the executor, which can be used to spawn futures.
    pub fn foreground_executor(&self) -> &ForegroundExecutor {
        if self.quitting {
            panic!("Can't spawn on main thread after on_app_quit")
        };
        &self.foreground_executor
    }

    /// Spawns the future returned by the given function on the main thread. The closure will be invoked
    /// with [AsyncApp], which allows the application state to be accessed across await points.
    #[track_caller]
    pub fn spawn<AsyncFn, R>(&self, f: AsyncFn) -> Task<R>
    where
        AsyncFn: AsyncFnOnce(&mut AsyncApp) -> R + 'static,
        R: 'static,
    {
        if self.quitting {
            debug_panic!("Can't spawn on main thread after on_app_quit")
        };

        let mut cx = self.to_async();

        self.foreground_executor
            .spawn(async move { f(&mut cx).await })
    }

    /// Schedules the given function to be run at the end of the current effect cycle, allowing entities
    /// that are currently on the stack to be returned to the app.
    pub fn defer(&mut self, f: impl FnOnce(&mut App) + 'static) {
        self.push_effect(Effect::Defer {
            callback: Box::new(f),
        });
    }

    /// Accessor for the application's asset source, which is provided when constructing the `App`.
    pub fn asset_source(&self) -> &Arc<dyn AssetSource> {
        &self.asset_source
    }

    /// Accessor for the text system.
    pub fn text_system(&self) -> &Arc<TextSystem> {
        &self.text_system
    }

    /// Check whether a global of the given type has been assigned.
    pub fn has_global<G: Global>(&self) -> bool {
        self.globals_by_type.contains_key(&TypeId::of::<G>())
    }

    /// Access the global of the given type. Panics if a global for that type has not been assigned.
    #[track_caller]
    pub fn global<G: Global>(&self) -> &G {
        self.globals_by_type
            .get(&TypeId::of::<G>())
            .map(|any_state| any_state.downcast_ref::<G>().unwrap())
            .with_context(|| format!("no state of type {} exists", type_name::<G>()))
            .unwrap()
    }

    /// Access the global of the given type if a value has been assigned.
    pub fn try_global<G: Global>(&self) -> Option<&G> {
        self.globals_by_type
            .get(&TypeId::of::<G>())
            .map(|any_state| any_state.downcast_ref::<G>().unwrap())
    }

    /// Access the global of the given type mutably. Panics if a global for that type has not been assigned.
    #[track_caller]
    pub fn global_mut<G: Global>(&mut self) -> &mut G {
        let global_type = TypeId::of::<G>();
        self.push_effect(Effect::NotifyGlobalObservers { global_type });
        self.globals_by_type
            .get_mut(&global_type)
            .and_then(|any_state| any_state.downcast_mut::<G>())
            .with_context(|| format!("no state of type {} exists", type_name::<G>()))
            .unwrap()
    }

    /// Access the global of the given type mutably. A default value is assigned if a global of this type has not
    /// yet been assigned.
    pub fn default_global<G: Global + Default>(&mut self) -> &mut G {
        let global_type = TypeId::of::<G>();
        self.push_effect(Effect::NotifyGlobalObservers { global_type });
        self.globals_by_type
            .entry(global_type)
            .or_insert_with(|| Box::<G>::default())
            .downcast_mut::<G>()
            .unwrap()
    }

    /// Sets the value of the global of the given type.
    pub fn set_global<G: Global>(&mut self, global: G) {
        let global_type = TypeId::of::<G>();
        self.push_effect(Effect::NotifyGlobalObservers { global_type });
        self.globals_by_type.insert(global_type, Box::new(global));
    }

    /// Clear all stored globals. Does not notify global observers.
    #[cfg(any(test, feature = "test-support"))]
    pub fn clear_globals(&mut self) {
        self.globals_by_type.drain();
    }

    /// Remove the global of the given type from the app context. Does not notify global observers.
    pub fn remove_global<G: Global>(&mut self) -> G {
        let global_type = TypeId::of::<G>();
        self.push_effect(Effect::NotifyGlobalObservers { global_type });
        *self
            .globals_by_type
            .remove(&global_type)
            .unwrap_or_else(|| panic!("no global added for {}", std::any::type_name::<G>()))
            .downcast()
            .unwrap()
    }

    /// Register a callback to be invoked when a global of the given type is updated.
    pub fn observe_global<G: Global>(
        &mut self,
        mut f: impl FnMut(&mut Self) + 'static,
    ) -> Subscription {
        let (subscription, activate) = self.global_observers.insert(
            TypeId::of::<G>(),
            Box::new(move |cx| {
                f(cx);
                true
            }),
        );
        self.defer(move |_| activate());
        subscription
    }

    /// Move the global of the given type to the stack.
    #[track_caller]
    pub(crate) fn lease_global<G: Global>(&mut self) -> GlobalLease<G> {
        GlobalLease::new(
            self.globals_by_type
                .remove(&TypeId::of::<G>())
                .with_context(|| format!("no global registered of type {}", type_name::<G>()))
                .unwrap(),
        )
    }

    /// Restore the global of the given type after it is moved to the stack.
    pub(crate) fn end_global_lease<G: Global>(&mut self, lease: GlobalLease<G>) {
        let global_type = TypeId::of::<G>();

        self.push_effect(Effect::NotifyGlobalObservers { global_type });
        self.globals_by_type.insert(global_type, lease.global);
    }

    pub(crate) fn new_entity_observer(
        &self,
        key: TypeId,
        value: NewEntityListener,
    ) -> Subscription {
        let (subscription, activate) = self.new_entity_observers.insert(key, value);
        activate();
        subscription
    }

    /// Arrange for the given function to be invoked whenever a view of the specified type is created.
    /// The function will be passed a mutable reference to the view along with an appropriate context.
    pub fn observe_new<T: 'static>(
        &self,
        on_new: impl 'static + Fn(&mut T, Option<&mut Window>, &mut Context<T>),
    ) -> Subscription {
        self.new_entity_observer(
            TypeId::of::<T>(),
            Box::new(
                move |any_entity: AnyEntity, window: &mut Option<&mut Window>, cx: &mut App| {
                    any_entity
                        .downcast::<T>()
                        .unwrap()
                        .update(cx, |entity_state, cx| {
                            on_new(entity_state, window.as_deref_mut(), cx)
                        })
                },
            ),
        )
    }

    /// Observe the release of a entity. The callback is invoked after the entity
    /// has no more strong references but before it has been dropped.
    pub fn observe_release<T>(
        &self,
        handle: &Entity<T>,
        on_release: impl FnOnce(&mut T, &mut App) + 'static,
    ) -> Subscription
    where
        T: 'static,
    {
        let (subscription, activate) = self.release_listeners.insert(
            handle.entity_id(),
            Box::new(move |entity, cx| {
                let entity = entity.downcast_mut().expect("invalid entity type");
                on_release(entity, cx)
            }),
        );
        activate();
        subscription
    }

    /// Observe the release of a entity. The callback is invoked after the entity
    /// has no more strong references but before it has been dropped.
    pub fn observe_release_in<T>(
        &self,
        handle: &Entity<T>,
        window: &Window,
        on_release: impl FnOnce(&mut T, &mut Window, &mut App) + 'static,
    ) -> Subscription
    where
        T: 'static,
    {
        let window_handle = window.handle;
        self.observe_release(handle, move |entity, cx| {
            let _ = window_handle.update(cx, |_, window, cx| on_release(entity, window, cx));
        })
    }

    /// Register a callback to be invoked after a keystroke is resolved in any window,
    /// including the action that handled it, if any. Keystrokes consumed by an
    /// interceptor or raw keyboard event handler are not observed.
    /// Standalone modifiers are observed on release.
    pub fn observe_keystrokes(
        &mut self,
        mut f: impl FnMut(&KeystrokeEvent, &mut Window, &mut App) + 'static,
    ) -> Subscription {
        fn inner(
            keystroke_observers: &SubscriberSet<(), KeystrokeObserver>,
            handler: KeystrokeObserver,
        ) -> Subscription {
            let (subscription, activate) = keystroke_observers.insert((), handler);
            activate();
            subscription
        }

        inner(
            &self.keystroke_observers,
            Box::new(move |event, window, cx| {
                f(event, window, cx);
                true
            }),
        )
    }

    /// Registers a callback to be invoked with grapheme clusters that exhausted
    /// font fallback, as observed by the platform text system. Intended for
    /// dynamic font installation: the callback can register a font covering the
    /// reported graphemes with [`TextSystem::add_fonts`], which invalidates
    /// cached layouts so the text is shaped again.
    ///
    /// Reports are deduplicated and bounded (see `MAX_REPORTED_MISSING_GLYPHS`).
    /// Only one registration is available per application; later calls log a
    /// warning and are ignored. The callback runs on the main thread until the
    /// application quits.
    pub fn on_missing_glyphs(&self, mut callback: impl FnMut(&[MissingGlyph], &mut App) + 'static) {
        let Some(mut receiver) = self.text_system.take_missing_glyph_receiver() else {
            log::warn!("App::on_missing_glyphs called more than once; ignoring registration");
            return;
        };
        self.spawn(async move |cx| {
            while let Ok(missing_glyphs) = receiver.recv().await {
                if cx.update(|cx| callback(&missing_glyphs, cx)).is_err() {
                    // The application is shutting down.
                    break;
                }
            }
        })
        .detach();
    }

    /// Register a callback to be invoked when a keystroke is received by the application
    /// in any window. Note that this fires _before_ all other action and event mechanisms have resolved
    /// unlike [`App::observe_keystrokes`] which fires after. This means that `cx.stop_propagation` calls
    /// within interceptors will prevent action dispatch
    pub fn intercept_keystrokes(
        &mut self,
        mut f: impl FnMut(&KeystrokeEvent, &mut Window, &mut App) + 'static,
    ) -> Subscription {
        fn inner(
            keystroke_interceptors: &SubscriberSet<(), KeystrokeObserver>,
            handler: KeystrokeObserver,
        ) -> Subscription {
            let (subscription, activate) = keystroke_interceptors.insert((), handler);
            activate();
            subscription
        }

        inner(
            &self.keystroke_interceptors,
            Box::new(move |event, window, cx| {
                f(event, window, cx);
                true
            }),
        )
    }

    /// Register key bindings.
    pub fn bind_keys(&mut self, bindings: impl IntoIterator<Item = KeyBinding>) {
        self.keymap.borrow_mut().add_bindings(bindings);
        self.pending_effects.push_back(Effect::RefreshWindows);
    }

    /// Clear all key bindings in the app.
    pub fn clear_key_bindings(&mut self) {
        self.keymap.borrow_mut().clear();
        self.pending_effects.push_back(Effect::RefreshWindows);
    }

    /// Get all key bindings in the app.
    pub fn key_bindings(&self) -> Rc<RefCell<Keymap>> {
        self.keymap.clone()
    }

    /// Register a global handler for actions invoked via the keyboard. These handlers are run at
    /// the end of the bubble phase for actions, and so will only be invoked if there are no other
    /// handlers or if they called `cx.propagate()`.
    pub fn on_action<A: Action>(&mut self, listener: impl Fn(&A, &mut Self) + 'static) {
        self.global_action_listeners
            .entry(TypeId::of::<A>())
            .or_default()
            .push(Rc::new(move |action, phase, cx| {
                if phase == DispatchPhase::Bubble {
                    let action = action.downcast_ref().unwrap();
                    listener(action, cx)
                }
            }));
    }

    /// Event handlers propagate events by default. Call this method to stop dispatching to
    /// event handlers with a lower z-index (mouse) or higher in the tree (keyboard). This is
    /// the opposite of [`Self::propagate`]. It's also possible to cancel a call to [`Self::propagate`] by
    /// calling this method before effects are flushed.
    pub fn stop_propagation(&mut self) {
        self.propagate_event = false;
    }

    /// Action handlers stop propagation by default during the bubble phase of action dispatch
    /// dispatching to action handlers higher in the element tree. This is the opposite of
    /// [`Self::stop_propagation`]. It's also possible to cancel a call to [`Self::stop_propagation`] by calling
    /// this method before effects are flushed.
    pub fn propagate(&mut self) {
        self.propagate_event = true;
    }

    /// Build an action from some arbitrary data, typically a keymap entry.
    pub fn build_action(
        &self,
        name: &str,
        data: Option<serde_json::Value>,
    ) -> std::result::Result<Box<dyn Action>, ActionBuildError> {
        self.actions.build_action(name, data)
    }

    /// Get all action names that have been registered. Note that registration only allows for
    /// actions to be built dynamically, and is unrelated to binding actions in the element tree.
    pub fn all_action_names(&self) -> &[&'static str] {
        self.actions.all_action_names()
    }

    /// Returns key bindings that invoke the given action on the currently focused element, without
    /// checking context. Bindings are returned in the order they were added. For display, the last
    /// binding should take precedence.
    pub fn all_bindings_for_input(&self, input: &[Keystroke]) -> Vec<KeyBinding> {
        RefCell::borrow(&self.keymap).all_bindings_for_input(input)
    }

    /// Get all non-internal actions that have been registered, along with their schemas.
    pub fn action_schemas(
        &self,
        generator: &mut schemars::SchemaGenerator,
    ) -> Vec<(&'static str, Option<schemars::Schema>)> {
        self.actions.action_schemas(generator)
    }

    /// Get a map from a deprecated action name to the canonical name.
    pub fn deprecated_actions_to_preferred_actions(&self) -> &HashMap<&'static str, &'static str> {
        self.actions.deprecated_aliases()
    }

    /// Get a map from an action name to the deprecation messages.
    pub fn action_deprecation_messages(&self) -> &HashMap<&'static str, &'static str> {
        self.actions.deprecation_messages()
    }

    /// Get a map from an action name to the documentation.
    pub fn action_documentation(&self) -> &HashMap<&'static str, &'static str> {
        self.actions.documentation()
    }

    /// Register a callback to be invoked when the application is about to quit.
    /// It is not possible to cancel the quit event at this point.
    pub fn on_app_quit<Fut>(
        &self,
        mut on_quit: impl FnMut(&mut App) -> Fut + 'static,
    ) -> Subscription
    where
        Fut: 'static + Future<Output = ()>,
    {
        let (subscription, activate) = self.quit_observers.insert(
            (),
            Box::new(move |cx| {
                let future = on_quit(cx);
                future.boxed_local()
            }),
        );
        activate();
        subscription
    }

    /// Register a callback to be invoked when the application is about to restart.
    ///
    /// These callbacks are called before any `on_app_quit` callbacks.
    pub fn on_app_restart(&self, mut on_restart: impl 'static + FnMut(&mut App)) -> Subscription {
        let (subscription, activate) = self.restart_observers.insert(
            (),
            Box::new(move |cx| {
                on_restart(cx);
                true
            }),
        );
        activate();
        subscription
    }

    /// Register a callback to be invoked when a window is closed
    /// The window is no longer accessible at the point this callback is invoked.
    pub fn on_window_closed(&self, mut on_closed: impl FnMut(&mut App) + 'static) -> Subscription {
        let (subscription, activate) = self.window_closed_observers.insert((), Box::new(on_closed));
        activate();
        subscription
    }

    pub(crate) fn clear_pending_keystrokes(&mut self) {
        for window in self.windows() {
            window
                .update(self, |_, window, cx| {
                    window.clear_pending_keystrokes(cx);
                })
                .ok();
        }
    }

    /// Checks if the given action is bound in the current context, as defined by the app's current focus,
    /// the bindings in the element tree, and any global action listeners.
    pub fn is_action_available(&mut self, action: &dyn Action) -> bool {
        let mut action_available = false;
        if let Some(window) = self.active_window()
            && let Ok(window_action_available) =
                window.update(self, |_, window, cx| window.is_action_available(action, cx))
        {
            action_available = window_action_available;
        }

        action_available
            || self
                .global_action_listeners
                .contains_key(&action.as_any().type_id())
    }

    /// Sets the menu bar for this application. This will replace any existing menu bar.
    pub fn set_menus(&self, menus: Vec<Menu>) {
        self.platform.set_menus(menus, &self.keymap.borrow());
    }

    /// Gets the menu bar for this application.
    pub fn get_menus(&self) -> Option<Vec<OwnedMenu>> {
        self.platform.get_menus()
    }

    /// Sets the right click menu for the app icon in the dock
    pub fn set_dock_menu(&self, menus: Vec<MenuItem>) {
        self.platform.set_dock_menu(menus, &self.keymap.borrow())
    }

    /// Performs the action associated with the given dock menu item, only used on Windows for now.
    pub fn perform_dock_menu_action(&self, action: usize) {
        self.platform.perform_dock_menu_action(action);
    }

    /// Adds given path to the bottom of the list of recent paths for the application.
    /// The list is usually shown on the application icon's context menu in the dock,
    /// and allows to open the recent files via that context menu.
    /// If the path is already in the list, it will be moved to the bottom of the list.
    pub fn add_recent_document(&self, path: &Path) {
        self.platform.add_recent_document(path);
    }

    /// Updates the jump list with the updated list of recent paths for the application, only used on Windows for now.
    /// Note that this also sets the dock menu on Windows.
    pub fn update_jump_list(
        &self,
        menus: Vec<MenuItem>,
        entries: Vec<SmallVec<[PathBuf; 2]>>,
    ) -> Vec<SmallVec<[PathBuf; 2]>> {
        self.platform.update_jump_list(menus, entries)
    }

    /// Dispatch an action to the currently active window or global action handler
    /// See [`crate::Action`] for more information on how actions work
    pub fn dispatch_action(&mut self, action: &dyn Action) {
        if let Some(active_window) = self.active_window() {
            active_window
                .update(self, |_, window, cx| {
                    window.dispatch_action(action.boxed_clone(), cx)
                })
                .log_err();
        } else {
            self.dispatch_global_action(action);
        }
    }

    fn dispatch_global_action(&mut self, action: &dyn Action) {
        self.propagate_event = true;

        if let Some(mut global_listeners) = self
            .global_action_listeners
            .remove(&action.as_any().type_id())
        {
            for listener in &global_listeners {
                listener(action.as_any(), DispatchPhase::Capture, self);
                if !self.propagate_event {
                    break;
                }
            }

            global_listeners.extend(
                self.global_action_listeners
                    .remove(&action.as_any().type_id())
                    .unwrap_or_default(),
            );

            self.global_action_listeners
                .insert(action.as_any().type_id(), global_listeners);
        }

        if self.propagate_event
            && let Some(mut global_listeners) = self
                .global_action_listeners
                .remove(&action.as_any().type_id())
        {
            for listener in global_listeners.iter().rev() {
                listener(action.as_any(), DispatchPhase::Bubble, self);
                if !self.propagate_event {
                    break;
                }
            }

            global_listeners.extend(
                self.global_action_listeners
                    .remove(&action.as_any().type_id())
                    .unwrap_or_default(),
            );

            self.global_action_listeners
                .insert(action.as_any().type_id(), global_listeners);
        }
    }

    /// Is there currently something being dragged?
    pub fn has_active_drag(&self) -> bool {
        self.active_drag.is_some()
    }

    /// Gets the cursor style of the currently active drag operation.
    pub fn active_drag_cursor_style(&self) -> Option<CursorStyle> {
        self.active_drag.as_ref().and_then(|drag| drag.cursor_style)
    }

    /// Stops active drag and clears any related effects.
    pub fn stop_active_drag(&mut self, window: &mut Window) -> bool {
        if self.active_drag.is_some() {
            self.active_drag = None;
            window.refresh();
            true
        } else {
            false
        }
    }

    /// Sets the cursor style for the currently active drag operation.
    pub fn set_active_drag_cursor_style(
        &mut self,
        cursor_style: CursorStyle,
        window: &mut Window,
    ) -> bool {
        if let Some(ref mut drag) = self.active_drag {
            drag.cursor_style = Some(cursor_style);
            window.refresh();
            true
        } else {
            false
        }
    }

    /// Set the prompt renderer for GPUI. This will replace the default or platform specific
    /// prompts with this custom implementation.
    pub fn set_prompt_builder(
        &mut self,
        renderer: impl Fn(
            PromptLevel,
            &str,
            Option<&str>,
            &[PromptButton],
            PromptHandle,
            &mut Window,
            &mut App,
        ) -> RenderablePromptHandle
        + 'static,
    ) {
        self.prompt_builder = Some(PromptBuilder::Custom(Box::new(renderer)));
    }

    /// Reset the prompt builder to the default implementation.
    pub fn reset_prompt_builder(&mut self) {
        self.prompt_builder = Some(PromptBuilder::Default);
    }

    /// Remove an asset from GPUI's cache
    pub fn remove_asset<A: Asset>(&mut self, source: &A::Source) {
        let asset_id = (TypeId::of::<A>(), hash(source));
        self.loading_assets.remove(&asset_id);
    }

    /// Check whether an asset is present in GPUI's cache (loading or loaded),
    /// without fetching it.
    #[cfg(any(test, feature = "test-support"))]
    pub fn has_asset<A: Asset>(&self, source: &A::Source) -> bool {
        let asset_id = (TypeId::of::<A>(), hash(source));
        self.loading_assets.contains_key(&asset_id)
    }

    /// Starts loading an uncached asset and returns its result once available.
    ///
    /// Pending loads and completed results are cached until [`Self::remove_asset`].
    /// This method does not subscribe a view to completion notifications.
    pub fn fetch_asset<A: Asset>(&mut self, source: &A::Source) -> Option<A::Output> {
        self.asset_entry::<A>(source).get()
    }

    pub(crate) fn asset_entry<A: Asset>(&mut self, source: &A::Source) -> &CachedLoad<A::Output> {
        let asset_id = (TypeId::of::<A>(), hash(source));
        if !self.loading_assets.contains_key(&asset_id) {
            let future = A::load(source.clone(), self);
            let entry = CachedLoad::new(future, self);
            self.loading_assets.insert(asset_id, Box::new(entry));
        }
        self.loading_assets
            .get(&asset_id)
            .and_then(|entry| entry.downcast_ref())
            .expect("asset cache entries are keyed by their asset type")
    }

    /// Obtain a new [`FocusHandle`], which allows you to track and manipulate the keyboard focus
    /// for elements rendered within this window.
    #[track_caller]
    pub fn focus_handle(&self) -> FocusHandle {
        FocusHandle::new(&self.focus_handles)
    }

    /// Tell GPUI that an entity has changed and observers of it should be notified.
    pub fn notify(&mut self, entity_id: EntityId) {
        let window_invalidators = mem::take(
            self.window_invalidators_by_entity
                .entry(entity_id)
                .or_default(),
        );

        if window_invalidators.is_empty() {
            if self.pending_notifications.insert(entity_id) {
                self.pending_effects
                    .push_back(Effect::Notify { emitter: entity_id });
            }
        } else {
            for invalidator in window_invalidators.values() {
                invalidator.invalidate_view(entity_id, self);
            }
        }

        self.window_invalidators_by_entity
            .insert(entity_id, window_invalidators);
    }

    /// Returns the name for this [`App`].
    #[cfg(any(test, feature = "test-support", debug_assertions))]
    pub fn get_name(&self) -> Option<&'static str> {
        self.name
    }

    /// Returns `true` if the platform file picker supports selecting a mix of files and directories.
    pub fn can_select_mixed_files_and_dirs(&self) -> bool {
        self.platform.can_select_mixed_files_and_dirs()
    }

    /// Removes an image from the sprite atlas on all windows.
    ///
    /// If the current window is being updated, it will be removed from `App.windows`, you can use `current_window` to specify the current window.
    /// This is a no-op if the image is not in the sprite atlas.
    pub fn drop_image(&mut self, image: Arc<RenderImage>, current_window: Option<&mut Window>) {
        // remove the texture from all other windows
        for window in self.windows.values_mut().flatten() {
            _ = window.drop_image(image.clone());
        }

        // remove the texture from the current window
        if let Some(window) = current_window {
            _ = window.drop_image(image);
        }
    }

    /// Sets the renderer for the inspector.
    #[cfg(any(feature = "inspector", debug_assertions))]
    pub fn set_inspector_renderer(&mut self, f: crate::InspectorRenderer) {
        self.inspector_renderer = Some(f);
    }

    /// Registers a renderer specific to an inspector state.
    #[cfg(any(feature = "inspector", debug_assertions))]
    pub fn register_inspector_element<T: 'static, R: crate::IntoElement>(
        &mut self,
        f: impl 'static + Fn(crate::InspectorElementId, &T, &mut Window, &mut App) -> R,
    ) {
        self.inspector_element_registry.register(f);
    }

    /// Initializes gpui's default colors for the application.
    ///
    /// These colors can be accessed through `cx.default_colors()`.
    pub fn init_colors(&mut self) {
        self.set_global(GlobalColors(Arc::new(Colors::default())));
    }
}

impl AppContext for App {
    type Result<T> = T;

    /// Builds an entity that is owned by the application.
    ///
    /// The given function will be invoked with a [`Context`] and must return an object representing the entity. An
    /// [`Entity`] handle will be returned, which can be used to access the entity in a context.
    fn new<T: 'static>(&mut self, build_entity: impl FnOnce(&mut Context<T>) -> T) -> Entity<T> {
        self.update(|cx| {
            let slot = cx.entities.reserve();
            let handle = slot.clone();
            let entity = build_entity(&mut Context::new_context(cx, slot.downgrade()));

            cx.push_effect(Effect::EntityCreated {
                entity: handle.clone().into_any(),
                tid: TypeId::of::<T>(),
                window: cx.window_update_stack.last().cloned(),
            });

            cx.entities.insert(slot, entity);
            handle
        })
    }

    fn reserve_entity<T: 'static>(&mut self) -> Self::Result<Reservation<T>> {
        Reservation(self.entities.reserve())
    }

    fn insert_entity<T: 'static>(
        &mut self,
        reservation: Reservation<T>,
        build_entity: impl FnOnce(&mut Context<T>) -> T,
    ) -> Self::Result<Entity<T>> {
        self.update(|cx| {
            let slot = reservation.0;
            let entity = build_entity(&mut Context::new_context(cx, slot.downgrade()));
            cx.entities.insert(slot, entity)
        })
    }

    /// Updates the entity referenced by the given handle. The function is passed a mutable reference to the
    /// entity along with a `Context` for the entity.
    fn update_entity<T: 'static, R>(
        &mut self,
        handle: &Entity<T>,
        update: impl FnOnce(&mut T, &mut Context<T>) -> R,
    ) -> R {
        self.update(|cx| {
            let mut entity = cx.entities.lease(handle);
            let result = update(
                &mut entity,
                &mut Context::new_context(cx, handle.downgrade()),
            );
            cx.entities.end_lease(entity);
            result
        })
    }

    fn as_mut<'a, T>(&'a mut self, handle: &Entity<T>) -> GpuiBorrow<'a, T>
    where
        T: 'static,
    {
        GpuiBorrow::new(handle.clone(), self)
    }

    fn read_entity<T, R>(
        &self,
        handle: &Entity<T>,
        read: impl FnOnce(&T, &App) -> R,
    ) -> Self::Result<R>
    where
        T: 'static,
    {
        let entity = self.entities.read(handle);
        read(entity, self)
    }

    fn update_window<T, F>(&mut self, handle: AnyWindowHandle, update: F) -> Result<T>
    where
        F: FnOnce(AnyView, &mut Window, &mut App) -> T,
    {
        self.update_window_id(handle.id, update)
    }

    fn read_window<T, R>(
        &self,
        window: &WindowHandle<T>,
        read: impl FnOnce(Entity<T>, &App) -> R,
    ) -> Result<R>
    where
        T: 'static,
    {
        let window = self
            .windows
            .get(window.id)
            .context("window not found")?
            .as_ref()
            .expect("attempted to read a window that is already on the stack");

        let root_view = window.root.clone().unwrap();
        let view = root_view
            .downcast::<T>()
            .map_err(|_| anyhow!("root view's type has changed"))?;

        Ok(read(view, self))
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
        let mut g = self.global::<G>();
        callback(g, self)
    }
}

/// These effects are processed at the end of each application update cycle.
pub(crate) enum Effect {
    Notify {
        emitter: EntityId,
    },
    Emit {
        emitter: EntityId,
        event_type: TypeId,
        event: Box<dyn Any>,
    },
    RefreshWindows,
    NotifyGlobalObservers {
        global_type: TypeId,
    },
    Defer {
        callback: Box<dyn FnOnce(&mut App) + 'static>,
    },
    EntityCreated {
        entity: AnyEntity,
        tid: TypeId,
        window: Option<WindowId>,
    },
}

impl std::fmt::Debug for Effect {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Effect::Notify { emitter } => write!(f, "Notify({})", emitter),
            Effect::Emit { emitter, .. } => write!(f, "Emit({:?})", emitter),
            Effect::RefreshWindows => write!(f, "RefreshWindows"),
            Effect::NotifyGlobalObservers { global_type } => {
                write!(f, "NotifyGlobalObservers({:?})", global_type)
            }
            Effect::Defer { .. } => write!(f, "Defer(..)"),
            Effect::EntityCreated { entity, .. } => write!(f, "EntityCreated({:?})", entity),
        }
    }
}

/// Wraps a global variable value during `update_global` while the value has been moved to the stack.
pub(crate) struct GlobalLease<G: Global> {
    global: Box<dyn Any>,
    global_type: PhantomData<G>,
}

impl<G: Global> GlobalLease<G> {
    fn new(global: Box<dyn Any>) -> Self {
        GlobalLease {
            global,
            global_type: PhantomData,
        }
    }
}

impl<G: Global> Deref for GlobalLease<G> {
    type Target = G;

    fn deref(&self) -> &Self::Target {
        self.global.downcast_ref().unwrap()
    }
}

impl<G: Global> DerefMut for GlobalLease<G> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        self.global.downcast_mut().unwrap()
    }
}

/// Contains state associated with an active drag operation, started by dragging an element
/// within the window or by dragging into the app from the underlying platform.
pub struct AnyDrag {
    /// The view used to render this drag
    pub view: AnyView,

    /// The value of the dragged item, to be dropped
    pub value: Arc<dyn Any>,

    /// This is used to render the dragged item in the same place
    /// on the original element that the drag was initiated
    pub cursor_offset: Point<Pixels>,

    /// The cursor style to use while dragging
    pub cursor_style: Option<CursorStyle>,
}

/// Contains state associated with a tooltip. You'll only need this struct if you're implementing
/// tooltip behavior on a custom element. Otherwise, use [Div::tooltip](crate::Interactivity::tooltip).
#[derive(Clone)]
pub struct AnyTooltip {
    /// The view used to display the tooltip
    pub view: AnyView,

    /// The absolute position of the mouse when the tooltip was deployed.
    pub mouse_position: Point<Pixels>,

    /// Given the bounds of the tooltip, checks whether the tooltip should still be visible and
    /// updates its state accordingly. This is needed atop the hovered element's mouse move handler
    /// to handle the case where the element is not painted (e.g. via use of `visible_on_hover`).
    pub check_visible_and_update: Rc<dyn Fn(Bounds<Pixels>, &mut Window, &mut App) -> bool>,
}

/// Whether a keystroke should prefer character input or key bindings.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum InputPreference {
    /// Prefer typing text over triggering key bindings.
    CharacterInput,
    /// Dispatch key bindings normally, if any match.
    KeyBindings,
}

/// A keystroke event, and potentially the associated action
#[derive(Debug)]
pub struct KeystrokeEvent {
    /// The keystroke that occurred
    pub keystroke: Keystroke,

    /// Whether this keystroke should prefer character input or key bindings.
    /// This is [`InputPreference::CharacterInput`] when the platform prefers text for the key
    /// (e.g. AltGr on Windows) and the focused input accepts text. Interceptors still receive
    /// these keystrokes and can consume them.
    ///
    /// If the keystroke is part of a multi-stroke binding, it still waits as pending input
    /// even when this is [`InputPreference::CharacterInput`].
    pub input_preference: InputPreference,

    /// The action that was resolved for the keystroke, if any
    pub action: Option<Box<dyn Action>>,

    /// The context stack at the time
    pub context_stack: Vec<KeyContext>,
}

struct NullHttpClient;

impl HttpClient for NullHttpClient {
    fn send(
        &self,
        _req: http_client::Request<http_client::AsyncBody>,
    ) -> futures::future::BoxFuture<
        'static,
        anyhow::Result<http_client::Response<http_client::AsyncBody>>,
    > {
        async move {
            anyhow::bail!("No HttpClient available");
        }
        .boxed()
    }

    fn user_agent(&self) -> Option<&http_client::http::HeaderValue> {
        None
    }

    fn proxy(&self) -> Option<&Url> {
        None
    }

    fn type_name(&self) -> &'static str {
        type_name::<Self>()
    }
}

/// A mutable reference to an entity owned by GPUI
pub struct GpuiBorrow<'a, T> {
    inner: Option<Lease<T>>,
    app: &'a mut App,
}

impl<'a, T: 'static> GpuiBorrow<'a, T> {
    fn new(inner: Entity<T>, app: &'a mut App) -> Self {
        app.start_update();
        let lease = app.entities.lease(&inner);
        Self {
            inner: Some(lease),
            app,
        }
    }
}

impl<'a, T: 'static> std::borrow::Borrow<T> for GpuiBorrow<'a, T> {
    fn borrow(&self) -> &T {
        self.inner.as_ref().unwrap().borrow()
    }
}

impl<'a, T: 'static> std::borrow::BorrowMut<T> for GpuiBorrow<'a, T> {
    fn borrow_mut(&mut self) -> &mut T {
        self.inner.as_mut().unwrap().borrow_mut()
    }
}

impl<'a, T: 'static> std::ops::Deref for GpuiBorrow<'a, T> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        self.inner.as_ref().unwrap()
    }
}

impl<'a, T: 'static> std::ops::DerefMut for GpuiBorrow<'a, T> {
    fn deref_mut(&mut self) -> &mut T {
        self.inner.as_mut().unwrap()
    }
}

impl<'a, T> Drop for GpuiBorrow<'a, T> {
    fn drop(&mut self) {
        let lease = self.inner.take().unwrap();
        self.app.notify(lease.id);
        self.app.entities.end_lease(lease);
        self.app.finish_update();
    }
}

#[cfg(test)]
mod test {
    use std::{
        cell::{Cell, RefCell},
        ffi::OsString,
        path::PathBuf,
        rc::Rc,
        sync::Arc,
    };

    #[cfg(unix)]
    use std::os::unix::ffi::OsStringExt;

    use rand::{SeedableRng, rngs::StdRng};

    use super::{Application, ApplicationHandle, NullHttpClient};
    use crate::{
        AppContext, AppResourceProfile, BackgroundExecutor, Context, Empty, ForegroundExecutor,
        IntoElement, Platform, QuitMode, Render, TestAppContext, TestDispatcher, TestPlatform,
        TrayIconClickEvent, TrayIconEvent, TrayIconRenderingMode, Window, WindowAppearance, point,
        px,
    };

    struct RenderCounter(Rc<Cell<usize>>);

    impl Render for RenderCounter {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            self.0.set(self.0.get() + 1);
            Empty
        }
    }

    #[crate::test]
    fn async_app_refresh_flushes_refresh_effect(cx: &mut TestAppContext) {
        let render_count = Rc::new(Cell::new(0));

        let _window = cx.add_window({
            let render_count = render_count.clone();
            move |_, _| RenderCounter(render_count)
        });

        cx.run_until_parked();
        let render_count_before_refresh = render_count.get();

        cx.to_async().refresh().unwrap();

        assert_eq!(render_count.get(), render_count_before_refresh + 1);
    }

    #[test]
    fn test_with_platform_uses_injected_platform() {
        let dispatcher = Arc::new(TestDispatcher::new(StdRng::seed_from_u64(0)));
        let platform: Rc<dyn Platform> = TestPlatform::new(
            BackgroundExecutor::new(dispatcher.clone()),
            ForegroundExecutor::new(dispatcher),
        );

        let application = Application::with_platform(platform.clone());

        assert!(Rc::ptr_eq(&application.0.borrow().platform, &platform));
    }

    #[test]
    fn test_with_quit_mode_updates_core_and_platform_state() {
        let dispatcher = Arc::new(TestDispatcher::new(StdRng::seed_from_u64(0)));
        let platform = TestPlatform::new(
            BackgroundExecutor::new(dispatcher.clone()),
            ForegroundExecutor::new(dispatcher),
        );

        let application =
            Application::with_platform(platform.clone()).with_quit_mode(QuitMode::Explicit);

        assert_eq!(application.0.borrow().quit_mode, QuitMode::Explicit);
        assert_eq!(*platform.last_quit_mode.lock(), Some(QuitMode::Explicit));
    }

    #[test]
    fn test_inaccessible_sets_force_disabled() {
        let dispatcher = Arc::new(TestDispatcher::new(StdRng::seed_from_u64(0)));
        let platform = TestPlatform::new(
            BackgroundExecutor::new(dispatcher.clone()),
            ForegroundExecutor::new(dispatcher),
        );
        let application = Application(super::App::new_app(
            platform,
            Arc::new(()),
            Arc::new(NullHttpClient),
            AppResourceProfile::default(),
        ))
        .inaccessible();

        assert!(application.0.borrow().accessibility_force_disabled);
    }

    #[test]
    fn test_with_resource_profile_rebuilds_text_system_budget() {
        let dispatcher = Arc::new(TestDispatcher::new(StdRng::seed_from_u64(0)));
        let platform = TestPlatform::new(
            BackgroundExecutor::new(dispatcher.clone()),
            ForegroundExecutor::new(dispatcher),
        );
        let mut profile = AppResourceProfile::default();
        profile.text.line_layout_cache_max_entries = 7;
        profile.text.line_layout_cache_low_watermark = 3;
        profile.text.raster_bounds_cache_max_entries = Some(11);
        profile.element_arena_size =
            crate::window::ELEMENT_ARENA_SIZE.load(std::sync::atomic::Ordering::Relaxed);

        let application = Application(super::App::new_app(
            platform,
            Arc::new(()),
            Arc::new(NullHttpClient),
            AppResourceProfile::default(),
        ))
        .with_resource_profile(profile);

        let app = application.0.borrow();
        assert_eq!(app.text_system.resource_budget_for_test(), (7, 3, Some(11)));
        assert_eq!(app.resource_profile.text.line_layout_cache_max_entries, 7);
        assert_eq!(
            app.resource_profile.gpu.instance_buffer_initial_size,
            2 * 1024 * 1024
        );
    }

    #[test]
    fn test_with_resource_profile_updates_gpu_budget() {
        let dispatcher = Arc::new(TestDispatcher::new(StdRng::seed_from_u64(0)));
        let platform = TestPlatform::new(
            BackgroundExecutor::new(dispatcher.clone()),
            ForegroundExecutor::new(dispatcher),
        );
        let platform_handle = platform.clone();
        let mut profile = AppResourceProfile::default();
        profile.gpu.instance_buffer_initial_size = 768 * 1024;
        profile.element_arena_size =
            crate::window::ELEMENT_ARENA_SIZE.load(std::sync::atomic::Ordering::Relaxed);

        let application = Application(super::App::new_app(
            platform,
            Arc::new(()),
            Arc::new(NullHttpClient),
            AppResourceProfile::default(),
        ))
        .with_resource_profile(profile);

        let app = application.0.borrow();
        assert_eq!(
            app.resource_profile.gpu.instance_buffer_initial_size,
            768 * 1024
        );
        assert_eq!(
            platform_handle
                .gpu_resource_budget
                .lock()
                .instance_buffer_initial_size,
            768 * 1024
        );
    }

    #[test]
    fn test_application_handle_keeps_app_alive_for_embedders() {
        let dispatcher = Arc::new(TestDispatcher::new(StdRng::seed_from_u64(0)));
        let platform = TestPlatform::new(
            BackgroundExecutor::new(dispatcher.clone()),
            ForegroundExecutor::new(dispatcher),
        );
        let application = Application(super::App::new_app(
            platform,
            Arc::new(()),
            Arc::new(NullHttpClient),
            AppResourceProfile::default(),
        ));
        let weak_app = Rc::downgrade(&application.0);
        let handle = ApplicationHandle {
            app: application.0.clone(),
        };
        drop(application);

        assert!(weak_app.upgrade().is_some());

        let did_update = Rc::new(RefCell::new(false));
        let did_update_clone = did_update.clone();
        handle.update(move |_| {
            *did_update_clone.borrow_mut() = true;
        });

        assert!(*did_update.borrow());

        drop(handle);
        assert!(weak_app.upgrade().is_none());
    }

    #[test]
    fn test_with_resource_profile_clamps_line_layout_cache_to_one_entry() {
        let dispatcher = Arc::new(TestDispatcher::new(StdRng::seed_from_u64(0)));
        let platform = TestPlatform::new(
            BackgroundExecutor::new(dispatcher.clone()),
            ForegroundExecutor::new(dispatcher),
        );
        let mut profile = AppResourceProfile::default();
        profile.text.line_layout_cache_max_entries = 0;
        profile.text.line_layout_cache_low_watermark = 5;
        profile.element_arena_size =
            crate::window::ELEMENT_ARENA_SIZE.load(std::sync::atomic::Ordering::Relaxed);

        let application = Application(super::App::new_app(
            platform,
            Arc::new(()),
            Arc::new(NullHttpClient),
            AppResourceProfile::default(),
        ))
        .with_resource_profile(profile);

        let app = application.0.borrow();
        assert_eq!(
            app.text_system.resource_budget_for_test(),
            (1, 0, None),
            "line layout caches should retain a valid minimum capacity"
        );
    }

    #[test]
    fn test_gpui_borrow() {
        let cx = TestAppContext::single();
        let observation_count = Rc::new(RefCell::new(0));

        let state = cx.update(|cx| {
            let state = cx.new(|_| false);
            cx.observe(&state, {
                let observation_count = observation_count.clone();
                move |_, _| {
                    let mut count = observation_count.borrow_mut();
                    *count += 1;
                }
            })
            .detach();

            state
        });

        cx.update(|cx| {
            // Calling this like this so that we don't clobber the borrow_mut above
            *std::borrow::BorrowMut::borrow_mut(&mut state.as_mut(cx)) = true;
        });

        cx.update(|cx| {
            state.write(cx, false);
        });

        assert_eq!(*observation_count.borrow(), 2);
    }

    #[test]
    fn test_tray_icon_rendering_mode_default() {
        assert_eq!(
            TrayIconRenderingMode::default(),
            TrayIconRenderingMode::Adaptive
        );
    }

    #[test]
    fn test_set_tray_icon_forwards_to_platform() {
        let cx = TestAppContext::single();
        let icon = [1_u8, 2, 3, 4];

        cx.update(|cx| {
            cx.set_tray_icon(Some(&icon));
        });

        assert_eq!(cx.tray_icon(), Some(icon.to_vec()));
    }

    #[test]
    fn test_set_tray_icon_rendering_mode_forwards_to_platform() {
        let cx = TestAppContext::single();

        cx.update(|cx| {
            cx.set_tray_icon_rendering_mode(TrayIconRenderingMode::Original);
        });

        assert_eq!(
            cx.tray_icon_rendering_mode(),
            TrayIconRenderingMode::Original
        );
    }

    #[test]
    fn test_tray_icon_rendering_mode_and_icon_updates_are_order_independent() {
        let first = TestAppContext::single();
        let second = TestAppContext::single();
        let icon = [9_u8, 8, 7, 6];

        first.update(|cx| {
            cx.set_tray_icon_rendering_mode(TrayIconRenderingMode::Original);
            cx.set_tray_icon(Some(&icon));
        });

        second.update(|cx| {
            cx.set_tray_icon(Some(&icon));
            cx.set_tray_icon_rendering_mode(TrayIconRenderingMode::Original);
        });

        assert_eq!(first.tray_icon(), Some(icon.to_vec()));
        assert_eq!(second.tray_icon(), Some(icon.to_vec()));
        assert_eq!(
            first.tray_icon_rendering_mode(),
            TrayIconRenderingMode::Original
        );
        assert_eq!(
            second.tray_icon_rendering_mode(),
            TrayIconRenderingMode::Original
        );
    }

    #[test]
    fn test_tray_icon_click_event_keeps_legacy_callback_compatible() {
        let cx = TestAppContext::single();
        let legacy_event = Rc::new(RefCell::new(None));
        let click_event = Rc::new(RefCell::new(None));

        cx.update({
            let legacy_event = legacy_event.clone();
            let click_event = click_event.clone();
            |cx| {
                cx.on_tray_icon_event(move |event, _| {
                    *legacy_event.borrow_mut() = Some(event);
                });
                cx.on_tray_icon_click_event(move |event, _| {
                    *click_event.borrow_mut() = Some(event);
                });
            }
        });

        let event =
            TrayIconClickEvent::with_position(TrayIconEvent::LeftClick, point(px(120.0), px(24.0)));
        cx.simulate_tray_icon_click_event(event.clone());

        assert_eq!(*legacy_event.borrow(), Some(TrayIconEvent::LeftClick));
        assert_eq!(*click_event.borrow(), Some(event));
    }

    #[test]
    fn test_tray_icon_callbacks_keep_re_registration_during_dispatch() {
        let cx = TestAppContext::single();
        let legacy_events = Rc::new(RefCell::new(Vec::new()));
        let click_events = Rc::new(RefCell::new(Vec::new()));

        cx.update({
            let legacy_events = legacy_events.clone();
            let click_events = click_events.clone();
            move |cx| {
                cx.on_tray_icon_event({
                    let legacy_events = legacy_events.clone();
                    move |event, cx| {
                        legacy_events.borrow_mut().push(("old", event));
                        cx.on_tray_icon_event({
                            let legacy_events = legacy_events.clone();
                            move |event, _| {
                                legacy_events.borrow_mut().push(("new", event));
                            }
                        });
                    }
                });

                cx.on_tray_icon_click_event({
                    let click_events = click_events.clone();
                    move |event, cx| {
                        click_events.borrow_mut().push(("old", event.kind));
                        cx.on_tray_icon_click_event({
                            let click_events = click_events.clone();
                            move |event, _| {
                                click_events.borrow_mut().push(("new", event.kind));
                            }
                        });
                    }
                });
            }
        });

        cx.simulate_tray_icon_click_event(TrayIconClickEvent::new(TrayIconEvent::LeftClick));
        cx.simulate_tray_icon_click_event(TrayIconClickEvent::new(TrayIconEvent::RightClick));

        assert_eq!(
            *legacy_events.borrow(),
            vec![
                ("old", TrayIconEvent::LeftClick),
                ("new", TrayIconEvent::RightClick)
            ]
        );
        assert_eq!(
            *click_events.borrow(),
            vec![
                ("old", TrayIconEvent::LeftClick),
                ("new", TrayIconEvent::RightClick)
            ]
        );
    }

    #[test]
    fn test_set_window_appearance_override_is_reported_by_getter() {
        let cx = TestAppContext::single();
        assert_eq!(
            cx.read(|cx| cx.window_appearance()),
            WindowAppearance::Light
        );

        cx.update(|cx| cx.set_window_appearance(Some(WindowAppearance::Dark)));
        assert_eq!(cx.read(|cx| cx.window_appearance()), WindowAppearance::Dark);

        cx.update(|cx| cx.set_window_appearance(Some(WindowAppearance::VibrantLight)));
        assert_eq!(
            cx.read(|cx| cx.window_appearance()),
            WindowAppearance::VibrantLight
        );

        cx.update(|cx| cx.set_window_appearance(None));
        assert_eq!(
            cx.read(|cx| cx.window_appearance()),
            WindowAppearance::Light
        );
    }

    #[test]
    fn test_tray_anchor_for_position_uses_display_local_bounds() {
        let cx = TestAppContext::single();
        let anchor = cx.update(|cx| cx.tray_anchor_for_position(point(px(120.0), px(24.0))));
        let anchor = anchor.expect("expected tray anchor");

        assert_eq!(anchor.display_id.0, 1);
        assert_eq!(anchor.bounds.origin, point(px(108.0), px(12.0)));
        assert_eq!(anchor.bounds.size.width, px(24.0));
        assert_eq!(anchor.bounds.size.height, px(24.0));
    }

    #[test]
    fn test_tray_anchor_for_position_keeps_out_of_display_hint() {
        let cx = TestAppContext::single();
        let anchor = cx.update(|cx| cx.tray_anchor_for_position(point(px(-20.0), px(10.0))));
        let anchor = anchor.expect("expected tray anchor");

        assert_eq!(anchor.display_id.0, 1);
        assert_eq!(anchor.bounds.origin, point(px(-32.0), px(-2.0)));
    }

    #[crate::test]
    async fn test_on_missing_glyphs_delivers_reports(cx: &mut TestAppContext) {
        use crate::{FallbackFontClass, MissingGlyph};

        let reported = Rc::new(RefCell::new(Vec::new()));
        cx.update(|cx| {
            cx.on_missing_glyphs({
                let reported = reported.clone();
                move |glyphs, _| reported.borrow_mut().extend(glyphs.iter().cloned())
            });
            cx.text_system().report_missing_glyphs_in_test(vec![
                MissingGlyph::new("\u{1F980}".into(), FallbackFontClass::Monospace),
                MissingGlyph::new("\u{1F980}".into(), FallbackFontClass::Monospace),
            ]);
        });

        cx.run_until_parked();

        // Duplicate reports within one batch are delivered once.
        assert_eq!(
            reported.borrow().as_slice(),
            &[MissingGlyph::new(
                "\u{1F980}".into(),
                FallbackFontClass::Monospace
            )]
        );
    }

    #[crate::test]
    async fn test_restart_preserves_path_and_arguments(cx: &mut TestAppContext) {
        #[cfg(unix)]
        let user_data_dir = OsString::from_vec(b"/tmp/zed data/\xff".to_vec());
        #[cfg(not(unix))]
        let user_data_dir = OsString::from("C:\\zed data");
        let arguments = vec![OsString::from("--user-data-dir"), user_data_dir];
        let restart_path = PathBuf::from("updated-zed");
        let _application =
            super::Application(cx.app.clone()).with_restart_arguments(arguments.clone());
        let restart = cx.expect_restart();

        cx.update(|cx| {
            cx.set_restart_path(restart_path.clone());
            cx.restart();
        });

        let (path, restart_arguments) = restart.await.expect("restart was not requested");
        assert_eq!(path, Some(restart_path));
        assert_eq!(restart_arguments, arguments);
    }
}

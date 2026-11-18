#[cfg(any(feature = "inspector", debug_assertions))]
use crate::Inspector;
use crate::{
    Action, AnyDrag, AnyElement, AnyImageCache, AnyTooltip, AnyView, App, AppContext, Arena, Asset,
    AsyncWindowContext, AtlasTile, AvailableSpace, Background, BlendMode, BorderStyle, Bounds,
    BoxShadow, Capslock, Context, Corners, CursorStyle, Decorations, DevicePixels,
    DispatchActionListener, DispatchNodeId, DispatchTree, DisplayId, Edges, Effect, Entity,
    EntityId, EventEmitter, FileDropEvent, FontId, Global, GlobalElementId, GlyphId, GpuSpecs,
    Hsla, InputHandler, InputPreference, IsZero, KeyBinding, KeyContext, KeyDownEvent, KeyEvent,
    Keystroke, KeystrokeEvent, LayoutId, LineLayoutIndex, Modifiers, ModifiersChangedEvent,
    MonochromeSprite, MouseButton, MouseEvent, MouseMoveEvent, MouseUpEvent, Path, Pixels,
    PlatformAtlas, PlatformDisplay, PlatformInput, PlatformInputHandler, PlatformWindow, Point,
    PolychromeSprite, ProgressBarState, PromptButton, PromptLevel, Quad, Render, RenderGlyphParams,
    RenderImage, RenderImageParams, RenderSvgParams, Replay, ResizeAnchor, ResizeEdge,
    SMOOTH_SVG_SCALE_FACTOR, SUBPIXEL_VARIANTS_X, SUBPIXEL_VARIANTS_Y, ScaledPixels, Scene, Shadow,
    SharedString, Size, StrikethroughStyle, Style, SubscriberSet, Subscription, SystemWindowTab,
    SystemWindowTabController, TabStopMap, TaffyLayoutEngine, Task, TextStyle, TextStyleRefinement,
    ThermalState, TransformationMatrix, Underline, UnderlineStyle, WindowAppearance,
    WindowBackgroundAppearance, WindowBounds, WindowControls, WindowDecorations, WindowOptions,
    WindowParams, WindowState, WindowTextSystem, WindowVisibility, point, prelude::*, px, rems,
    size, transparent_black,
};
use anyhow::{Context as _, Result, anyhow};
use collections::{FxHashMap, FxHashSet};
#[cfg(target_os = "macos")]
use core_video::pixel_buffer::CVPixelBuffer;
use derive_more::{Deref, DerefMut};
use futures::channel::oneshot;
#[cfg(feature = "input-latency-histogram")]
use hdrhistogram::Histogram;
use itertools::FoldWhile::{Continue, Done};
use itertools::Itertools;
use parking_lot::RwLock;
use raw_window_handle::{HandleError, HasDisplayHandle, HasWindowHandle};
use refineable::Refineable;
use slotmap::SlotMap;
use smallvec::SmallVec;
use std::{
    any::{Any, TypeId},
    borrow::Cow,
    cell::{Cell, RefCell},
    cmp,
    fmt::{Debug, Display},
    hash::{Hash, Hasher},
    marker::PhantomData,
    mem,
    ops::{DerefMut, Range},
    rc::Rc,
    sync::{
        Arc, Weak,
        atomic::{AtomicBool, AtomicUsize, Ordering::SeqCst},
    },
    time::{Duration, Instant},
};
use util::post_inc;
use util::{ResultExt, measure};
use uuid::Uuid;

pub(crate) mod a11y;
mod prompts;

mod input;
mod paint;

use crate::util::{
    atomic_incr_if_not_zero, ceil_to_device_pixel, floor_to_device_pixel, round_half_toward_zero,
    round_half_toward_zero_f64, round_stroke_to_device_pixel, round_to_device_pixel,
};
use a11y::{A11y, ROOT_NODE_ID};
pub use prompts::*;

#[doc(hidden)]
pub const DEFAULT_WINDOW_SIZE: Size<Pixels> = size(px(1536.), px(864.));

/// Represents the two different phases when dispatching events.
#[derive(Default, Copy, Clone, Debug, Eq, PartialEq)]
pub enum DispatchPhase {
    /// After the capture phase comes the bubble phase, in which mouse event listeners are
    /// invoked front to back and keyboard event listeners are invoked from the focused element
    /// to the root of the element tree. This is the phase you'll most commonly want to use when
    /// registering event listeners.
    #[default]
    Bubble,
    /// During the initial capture phase, mouse event listeners are invoked back to front, and keyboard
    /// listeners are invoked from the root of the tree downward toward the focused element. This phase
    /// is used for special purposes such as clearing the "pressed" state for click events. If
    /// you stop event propagation during this phase, you need to know what you're doing. Handlers
    /// outside of the immediate region may rely on detecting non-local events during this phase.
    Capture,
}

impl DispatchPhase {
    /// Returns true if this represents the "bubble" phase.
    #[inline]
    pub fn bubble(self) -> bool {
        self == DispatchPhase::Bubble
    }

    /// Returns true if this represents the "capture" phase.
    #[inline]
    pub fn capture(self) -> bool {
        self == DispatchPhase::Capture
    }
}

struct WindowInvalidatorInner {
    pub dirty: bool,
    pub draw_phase: DrawPhase,
    pub dirty_views: FxHashSet<EntityId>,
    pub update_count: usize,
}

#[derive(Clone)]
pub(crate) struct WindowInvalidator {
    inner: Rc<RefCell<WindowInvalidatorInner>>,
}

impl WindowInvalidator {
    pub fn new() -> Self {
        WindowInvalidator {
            inner: Rc::new(RefCell::new(WindowInvalidatorInner {
                dirty: true,
                draw_phase: DrawPhase::None,
                dirty_views: FxHashSet::default(),
                update_count: 0,
            })),
        }
    }

    pub fn invalidate_view(&self, entity: EntityId, cx: &mut App) -> bool {
        let mut inner = self.inner.borrow_mut();
        inner.update_count += 1;
        inner.dirty_views.insert(entity);
        if inner.draw_phase == DrawPhase::None {
            inner.dirty = true;
            cx.push_effect(Effect::Notify { emitter: entity });
            true
        } else {
            false
        }
    }

    pub fn is_dirty(&self) -> bool {
        self.inner.borrow().dirty
    }

    pub fn set_dirty(&self, dirty: bool) {
        let mut inner = self.inner.borrow_mut();
        inner.dirty = dirty;
        if dirty {
            inner.update_count += 1;
        }
    }

    pub fn set_phase(&self, phase: DrawPhase) {
        self.inner.borrow_mut().draw_phase = phase
    }

    pub fn update_count(&self) -> usize {
        self.inner.borrow().update_count
    }

    pub fn take_views(&self) -> FxHashSet<EntityId> {
        mem::take(&mut self.inner.borrow_mut().dirty_views)
    }

    pub fn replace_views(&self, views: FxHashSet<EntityId>) {
        self.inner.borrow_mut().dirty_views = views;
    }

    pub fn not_drawing(&self) -> bool {
        self.inner.borrow().draw_phase == DrawPhase::None
    }

    #[track_caller]
    pub fn debug_assert_paint(&self) {
        debug_assert!(
            matches!(self.inner.borrow().draw_phase, DrawPhase::Paint),
            "this method can only be called during paint"
        );
    }

    #[track_caller]
    pub fn debug_assert_prepaint(&self) {
        debug_assert!(
            matches!(self.inner.borrow().draw_phase, DrawPhase::Prepaint),
            "this method can only be called during request_layout, or prepaint"
        );
    }

    #[track_caller]
    pub fn debug_assert_paint_or_prepaint(&self) {
        debug_assert!(
            matches!(
                self.inner.borrow().draw_phase,
                DrawPhase::Paint | DrawPhase::Prepaint
            ),
            "this method can only be called during request_layout, prepaint, or paint"
        );
    }
}

type AnyObserver = Box<dyn FnMut(&mut Window, &mut App) -> bool + 'static>;

pub(crate) type AnyWindowFocusListener =
    Box<dyn FnMut(&WindowFocusEvent, &mut Window, &mut App) -> bool + 'static>;

pub(crate) struct WindowFocusEvent {
    pub(crate) previous_focus_path: SmallVec<[FocusId; 8]>,
    pub(crate) current_focus_path: SmallVec<[FocusId; 8]>,
}

impl WindowFocusEvent {
    pub fn is_focus_in(&self, focus_id: FocusId) -> bool {
        !self.previous_focus_path.contains(&focus_id) && self.current_focus_path.contains(&focus_id)
    }

    pub fn is_focus_out(&self, focus_id: FocusId) -> bool {
        self.previous_focus_path.contains(&focus_id) && !self.current_focus_path.contains(&focus_id)
    }
}

/// This is provided when subscribing for `Context::on_focus_out` events.
pub struct FocusOutEvent {
    /// A weak focus handle representing what was blurred.
    pub blurred: WeakFocusHandle,
}

slotmap::new_key_type! {
    /// A globally unique identifier for a focusable element.
    pub struct FocusId;
}

/// Global element arena initial size, set by [`Application::with_resource_profile`]
/// before any windows are opened. Defaults to 1 MiB.
pub(crate) static ELEMENT_ARENA_SIZE: std::sync::atomic::AtomicUsize =
    std::sync::atomic::AtomicUsize::new(1024 * 1024);

thread_local! {
    pub(crate) static ELEMENT_ARENA: RefCell<Arena> = RefCell::new(
        Arena::new(ELEMENT_ARENA_SIZE.load(std::sync::atomic::Ordering::Relaxed))
    );
}

/// Balances an arena draw scope even when a draw unwinds with a panic.
struct ElementArenaScope;

impl ElementArenaScope {
    fn enter() -> Self {
        ELEMENT_ARENA.with_borrow_mut(Arena::begin_scope);
        Self
    }
}

impl Drop for ElementArenaScope {
    fn drop(&mut self) {
        ELEMENT_ARENA.with_borrow_mut(Arena::end_scope);
    }
}

/// Whether a window draw is currently in progress on this thread.
///
/// This holds exactly while an [`ElementArenaScope`] is active. The
/// `on_request_frame` callback uses it to defer draw requests that arrive
/// re-entrantly while a draw is already on the stack (e.g. via nested message
/// pumping in the Windows window procedure), instead of running a nested draw
/// or panicking on the already-borrowed `App`. If the arena cannot be borrowed
/// the draw must be in progress, so this conservatively returns `true`.
fn draw_in_progress() -> bool {
    ELEMENT_ARENA.with(|arena| {
        arena
            .try_borrow()
            .map(|arena| arena.has_active_scope())
            .unwrap_or(true)
    })
}

/// Returned when the element arena has been used and so must be cleared before the next draw.
#[must_use]
pub struct ArenaClearNeeded;

impl ArenaClearNeeded {
    /// Clear the element arena.
    pub fn clear(self) {
        ELEMENT_ARENA.with_borrow_mut(|element_arena| {
            element_arena.clear();
        });
    }
}

pub(crate) type FocusMap = RwLock<SlotMap<FocusId, FocusRef>>;
pub(crate) struct FocusRef {
    pub(crate) ref_count: AtomicUsize,
    pub(crate) tab_index: isize,
    pub(crate) tab_stop: bool,
}

impl FocusId {
    /// Obtains whether the element associated with this handle is currently focused.
    pub fn is_focused(&self, window: &Window) -> bool {
        window.focus == Some(*self)
    }

    /// Obtains whether the element associated with this handle contains the focused
    /// element or is itself focused.
    pub fn contains_focused(&self, window: &Window, cx: &App) -> bool {
        window
            .focused(cx)
            .is_some_and(|focused| self.contains(focused.id, window))
    }

    /// Obtains whether the element associated with this handle is contained within the
    /// focused element or is itself focused.
    pub fn within_focused(&self, window: &Window, cx: &App) -> bool {
        let focused = window.focused(cx);
        focused.is_some_and(|focused| focused.id.contains(*self, window))
    }

    /// Obtains whether this handle contains the given handle in the most recently rendered frame.
    pub(crate) fn contains(&self, other: Self, window: &Window) -> bool {
        window
            .rendered_frame
            .dispatch_tree
            .focus_contains(*self, other)
    }
}

/// A handle which can be used to track and manipulate the focused element in a window.
pub struct FocusHandle {
    pub(crate) id: FocusId,
    handles: Arc<FocusMap>,
    /// The index of this element in the tab order.
    pub tab_index: isize,
    /// Whether this element can be focused by tab navigation.
    pub tab_stop: bool,
}

impl std::fmt::Debug for FocusHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_fmt(format_args!("FocusHandle({:?})", self.id))
    }
}

impl FocusHandle {
    pub(crate) fn new(handles: &Arc<FocusMap>) -> Self {
        let id = handles.write().insert(FocusRef {
            ref_count: AtomicUsize::new(1),
            tab_index: 0,
            tab_stop: false,
        });

        Self {
            id,
            tab_index: 0,
            tab_stop: false,
            handles: handles.clone(),
        }
    }

    pub(crate) fn for_id(id: FocusId, handles: &Arc<FocusMap>) -> Option<Self> {
        let lock = handles.read();
        let focus = lock.get(id)?;
        if atomic_incr_if_not_zero(&focus.ref_count) == 0 {
            return None;
        }
        Some(Self {
            id,
            tab_index: focus.tab_index,
            tab_stop: focus.tab_stop,
            handles: handles.clone(),
        })
    }

    /// Sets the tab index of the element associated with this handle.
    pub fn tab_index(mut self, index: isize) -> Self {
        self.tab_index = index;
        if let Some(focus) = self.handles.write().get_mut(self.id) {
            focus.tab_index = index;
        }
        self
    }

    /// Sets whether the element associated with this handle is a tab stop.
    ///
    /// When `false`, the element will not be included in the tab order.
    pub fn tab_stop(mut self, tab_stop: bool) -> Self {
        self.tab_stop = tab_stop;
        if let Some(focus) = self.handles.write().get_mut(self.id) {
            focus.tab_stop = tab_stop;
        }
        self
    }

    /// Converts this focus handle into a weak variant, which does not prevent it from being released.
    pub fn downgrade(&self) -> WeakFocusHandle {
        WeakFocusHandle {
            id: self.id,
            handles: Arc::downgrade(&self.handles),
        }
    }

    /// Moves the focus to the element associated with this handle.
    pub fn focus(&self, window: &mut Window) {
        window.focus(self)
    }

    /// Obtains whether the element associated with this handle is currently focused.
    pub fn is_focused(&self, window: &Window) -> bool {
        self.id.is_focused(window)
    }

    /// Obtains whether the element associated with this handle contains the focused
    /// element or is itself focused.
    pub fn contains_focused(&self, window: &Window, cx: &App) -> bool {
        self.id.contains_focused(window, cx)
    }

    /// Obtains whether the element associated with this handle is contained within the
    /// focused element or is itself focused.
    pub fn within_focused(&self, window: &Window, cx: &mut App) -> bool {
        self.id.within_focused(window, cx)
    }

    /// Obtains whether this handle contains the given handle in the most recently rendered frame.
    pub fn contains(&self, other: &Self, window: &Window) -> bool {
        self.id.contains(other.id, window)
    }

    /// Dispatch an action on the element that rendered this focus handle
    pub fn dispatch_action(&self, action: &dyn Action, window: &mut Window, cx: &mut App) {
        if let Some(node_id) = window
            .rendered_frame
            .dispatch_tree
            .focusable_node_id(self.id)
        {
            window.dispatch_action_on_node(node_id, action, cx)
        }
    }
}

impl Clone for FocusHandle {
    fn clone(&self) -> Self {
        Self::for_id(self.id, &self.handles).unwrap()
    }
}

impl PartialEq for FocusHandle {
    fn eq(&self, other: &Self) -> bool {
        self.id == other.id
    }
}

impl Eq for FocusHandle {}

impl Drop for FocusHandle {
    fn drop(&mut self) {
        self.handles
            .read()
            .get(self.id)
            .unwrap()
            .ref_count
            .fetch_sub(1, SeqCst);
    }
}

/// A weak reference to a focus handle.
#[derive(Clone, Debug)]
pub struct WeakFocusHandle {
    pub(crate) id: FocusId,
    pub(crate) handles: Weak<FocusMap>,
}

impl WeakFocusHandle {
    /// Attempts to upgrade the [WeakFocusHandle] to a [FocusHandle].
    pub fn upgrade(&self) -> Option<FocusHandle> {
        let handles = self.handles.upgrade()?;
        FocusHandle::for_id(self.id, &handles)
    }
}

impl PartialEq for WeakFocusHandle {
    fn eq(&self, other: &WeakFocusHandle) -> bool {
        self.id == other.id
    }
}

impl Eq for WeakFocusHandle {}

impl PartialEq<FocusHandle> for WeakFocusHandle {
    fn eq(&self, other: &FocusHandle) -> bool {
        self.id == other.id
    }
}

impl PartialEq<WeakFocusHandle> for FocusHandle {
    fn eq(&self, other: &WeakFocusHandle) -> bool {
        self.id == other.id
    }
}

/// Focusable allows users of your view to easily
/// focus it (using window.focus_view(cx, view))
pub trait Focusable: 'static {
    /// Returns the focus handle associated with this view.
    fn focus_handle(&self, cx: &App) -> FocusHandle;
}

impl<V: Focusable> Focusable for Entity<V> {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.read(cx).focus_handle(cx)
    }
}

/// ManagedView is a view (like a Modal, Popover, Menu, etc.)
/// where the lifecycle of the view is handled by another view.
pub trait ManagedView: Focusable + EventEmitter<DismissEvent> + Render {}

impl<M: Focusable + EventEmitter<DismissEvent> + Render> ManagedView for M {}

/// Emitted by implementers of [`ManagedView`] to indicate the view should be dismissed, such as when a view is presented as a modal.
pub struct DismissEvent;

type FrameCallback = Box<dyn FnOnce(&mut Window, &mut App)>;

pub(crate) type AnyMouseListener =
    Box<dyn FnMut(&dyn Any, DispatchPhase, &mut Window, &mut App) + 'static>;

#[derive(Clone)]
pub(crate) struct CursorStyleRequest {
    pub(crate) hitbox_id: Option<HitboxId>,
    pub(crate) style: CursorStyle,
}

#[derive(Default, Eq, PartialEq)]
pub(crate) struct HitTest {
    pub(crate) ids: SmallVec<[HitboxId; 8]>,
    pub(crate) hover_hitbox_count: usize,
}

/// A type of window control area that corresponds to the platform window.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WindowControlArea {
    /// An area that allows dragging of the platform window.
    Drag,
    /// An area that allows closing of the platform window.
    Close,
    /// An area that allows maximizing of the platform window.
    Max,
    /// An area that allows minimizing of the platform window.
    Min,
}

/// An identifier for a [Hitbox] which also includes [HitboxBehavior].
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
pub struct HitboxId(u64);

impl HitboxId {
    /// Checks if the hitbox with this ID is currently hovered. Except when handling
    /// `ScrollWheelEvent`, this is typically what you want when determining whether to handle mouse
    /// events or paint hover styles.
    ///
    /// See [`Hitbox::is_hovered`] for details.
    pub fn is_hovered(self, window: &Window) -> bool {
        // If this hitbox has captured the pointer, it's always considered hovered
        if window.captured_hitbox == Some(self) {
            return true;
        }
        if window.last_input_was_keyboard() {
            return false;
        }
        self.hit_test(window)
    }

    /// Checks if the hitbox with this ID is currently hovered, regardless of the last
    /// input modality used.
    ///
    /// See [`HitboxId::is_hovered`] for more details.
    pub(crate) fn is_hovered_ignoring_last_input(self, window: &Window) -> bool {
        // If this hitbox has captured the pointer, it's always considered hovered
        if window.captured_hitbox == Some(self) {
            return true;
        }
        self.hit_test(window)
    }

    fn hit_test(self, window: &Window) -> bool {
        let hit_test = &window.mouse_hit_test;
        for id in hit_test.ids.iter().take(hit_test.hover_hitbox_count) {
            if self == *id {
                return true;
            }
        }
        false
    }

    /// Checks if the hitbox with this ID contains the mouse and should handle scroll events.
    /// Typically this should only be used when handling `ScrollWheelEvent`, and otherwise
    /// `is_hovered` should be used. See the documentation of `Hitbox::is_hovered` for details about
    /// this distinction.
    pub fn should_handle_scroll(self, window: &Window) -> bool {
        window.mouse_hit_test.ids.contains(&self)
    }

    fn next(mut self) -> HitboxId {
        HitboxId(self.0.wrapping_add(1))
    }
}

/// A rectangular region that potentially blocks hitboxes inserted prior.
/// See [Window::insert_hitbox] for more details.
#[derive(Clone, Debug, Deref)]
pub struct Hitbox {
    /// A unique identifier for the hitbox.
    pub id: HitboxId,
    /// The bounds of the hitbox.
    #[deref]
    pub bounds: Bounds<Pixels>,
    /// The content mask when the hitbox was inserted.
    pub content_mask: ContentMask<Pixels>,
    /// Flags that specify hitbox behavior.
    pub behavior: HitboxBehavior,
}

impl Hitbox {
    /// Checks if the hitbox is currently hovered. Except when handling `ScrollWheelEvent`, this is
    /// typically what you want when determining whether to handle mouse events or paint hover
    /// styles.
    ///
    /// This can return `false` even when the hitbox contains the mouse, if a hitbox in front of
    /// this sets `HitboxBehavior::BlockMouse` (`InteractiveElement::occlude`) or
    /// `HitboxBehavior::BlockMouseExceptScroll` (`InteractiveElement::block_mouse_except_scroll`).
    ///
    /// Handling of `ScrollWheelEvent` should typically use `should_handle_scroll` instead.
    /// Concretely, this is due to use-cases like overlays that cause the elements under to be
    /// non-interactive while still allowing scrolling. More abstractly, this is because
    /// `is_hovered` is about element interactions directly under the mouse - mouse moves, clicks,
    /// hover styling, etc. In contrast, scrolling is about finding the current outer scrollable
    /// container.
    pub fn is_hovered(&self, window: &Window) -> bool {
        self.id.is_hovered(window)
    }

    /// Checks whether this hitbox would be hovered at `position`, regardless of the current input
    /// modality or mouse position.
    ///
    /// Unlike [`Hitbox::is_hovered`], this is a position-based hit test against the rendered
    /// frame. It does not consider pointer capture or the current input modality, but it still
    /// respects content masks and hitbox occlusion. During a drag that captured this hitbox,
    /// `is_hovered` reports the hitbox as hovered even outside its bounds, while `is_hovered_at`
    /// only tests the supplied `position`.
    pub fn is_hovered_at(&self, position: Point<Pixels>, window: &Window) -> bool {
        let hit_test = window.rendered_frame.hit_test(position);
        hit_test
            .ids
            .iter()
            .take(hit_test.hover_hitbox_count)
            .any(|id| self.id == *id)
    }

    /// Checks if the hitbox contains the mouse and should handle scroll events. Typically this
    /// should only be used when handling `ScrollWheelEvent`, and otherwise `is_hovered` should be
    /// used. See the documentation of `Hitbox::is_hovered` for details about this distinction.
    ///
    /// This can return `false` even when the hitbox contains the mouse, if a hitbox in front of
    /// this sets `HitboxBehavior::BlockMouse` (`InteractiveElement::occlude`).
    pub fn should_handle_scroll(&self, window: &Window) -> bool {
        self.id.should_handle_scroll(window)
    }
}

/// How the hitbox affects mouse behavior.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub enum HitboxBehavior {
    /// Normal hitbox mouse behavior, doesn't affect mouse handling for other hitboxes.
    #[default]
    Normal,

    /// All hitboxes behind this hitbox will be ignored and so will have `hitbox.is_hovered() ==
    /// false` and `hitbox.should_handle_scroll() == false`. Typically for elements this causes
    /// skipping of all mouse events, hover styles, and tooltips. This flag is set by
    /// [`InteractiveElement::occlude`].
    ///
    /// For mouse handlers that check those hitboxes, this behaves the same as registering a
    /// bubble-phase handler for every mouse event type:
    ///
    /// ```ignore
    /// window.on_mouse_event(move |_: &EveryMouseEventTypeHere, phase, window, cx| {
    ///     if phase == DispatchPhase::Capture && hitbox.is_hovered(window) {
    ///         cx.stop_propagation();
    ///     }
    /// })
    /// ```
    ///
    /// This has effects beyond event handling - any use of hitbox checking, such as hover
    /// styles and tooltops. These other behaviors are the main point of this mechanism. An
    /// alternative might be to not affect mouse event handling - but this would allow
    /// inconsistent UI where clicks and moves interact with elements that are not considered to
    /// be hovered.
    BlockMouse,

    /// All hitboxes behind this hitbox will have `hitbox.is_hovered() == false`, even when
    /// `hitbox.should_handle_scroll() == true`. Typically for elements this causes all mouse
    /// interaction except scroll events to be ignored - see the documentation of
    /// [`Hitbox::is_hovered`] for details. This flag is set by
    /// [`InteractiveElement::block_mouse_except_scroll`].
    ///
    /// For mouse handlers that check those hitboxes, this behaves the same as registering a
    /// bubble-phase handler for every mouse event type **except** `ScrollWheelEvent`:
    ///
    /// ```ignore
    /// window.on_mouse_event(move |_: &EveryMouseEventTypeExceptScroll, phase, window, cx| {
    ///     if phase == DispatchPhase::Bubble && hitbox.should_handle_scroll(window) {
    ///         cx.stop_propagation();
    ///     }
    /// })
    /// ```
    ///
    /// See the documentation of [`Hitbox::is_hovered`] for details of why `ScrollWheelEvent` is
    /// handled differently than other mouse events. If also blocking these scroll events is
    /// desired, then a `cx.stop_propagation()` handler like the one above can be used.
    ///
    /// This has effects beyond event handling - this affects any use of `is_hovered`, such as
    /// hover styles and tooltops. These other behaviors are the main point of this mechanism.
    /// An alternative might be to not affect mouse event handling - but this would allow
    /// inconsistent UI where clicks and moves interact with elements that are not considered to
    /// be hovered.
    BlockMouseExceptScroll,
}

/// An identifier for a tooltip.
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
pub struct TooltipId(usize);

impl TooltipId {
    /// Checks if the tooltip is currently hovered.
    pub fn is_hovered(&self, window: &Window) -> bool {
        window
            .tooltip_bounds
            .as_ref()
            .is_some_and(|tooltip_bounds| {
                tooltip_bounds.id == *self
                    && tooltip_bounds.bounds.contains(&window.mouse_position())
            })
    }
}

pub(crate) struct TooltipBounds {
    id: TooltipId,
    bounds: Bounds<Pixels>,
}

#[derive(Clone)]
pub(crate) struct TooltipRequest {
    id: TooltipId,
    tooltip: AnyTooltip,
}

pub(crate) struct DeferredDraw {
    current_view: EntityId,
    priority: usize,
    parent_node: DispatchNodeId,
    element_id_stack: SmallVec<[ElementId; 32]>,
    text_style_stack: SmallVec<[TextStyleRefinement; 4]>,
    element: Option<AnyElement>,
    absolute_offset: Point<Pixels>,
    prepaint_range: Range<PrepaintStateIndex>,
    paint_range: Range<PaintIndex>,
}

pub(crate) struct Frame {
    pub(crate) focus: Option<FocusId>,
    pub(crate) window_active: bool,
    pub(crate) element_states: FxHashMap<(GlobalElementId, TypeId), ElementStateBox>,
    accessed_element_states: Vec<(GlobalElementId, TypeId)>,
    pub(crate) mouse_listeners: Vec<Option<AnyMouseListener>>,
    pub(crate) dispatch_tree: DispatchTree,
    pub(crate) scene: Scene,
    pub(crate) hitboxes: Vec<Hitbox>,
    pub(crate) window_control_hitboxes: Vec<(WindowControlArea, Hitbox)>,
    pub(crate) deferred_draws: Vec<DeferredDraw>,
    pub(crate) input_handlers: Vec<Option<PlatformInputHandler>>,
    pub(crate) tooltip_requests: Vec<Option<TooltipRequest>>,
    pub(crate) cursor_styles: Vec<CursorStyleRequest>,
    #[cfg(any(test, feature = "test-support"))]
    pub(crate) debug_bounds: FxHashMap<String, Bounds<Pixels>>,
    #[cfg(any(test, feature = "test-support"))]
    debug_bounds_records: Vec<(String, Bounds<Pixels>)>,
    #[cfg(any(feature = "inspector", debug_assertions))]
    pub(crate) next_inspector_instance_ids: FxHashMap<Rc<crate::InspectorElementPath>, usize>,
    #[cfg(any(feature = "inspector", debug_assertions))]
    pub(crate) inspector_hitboxes: FxHashMap<HitboxId, crate::InspectorElementId>,
    pub(crate) tab_stops: TabStopMap,
}

#[derive(Clone, Default)]
pub(crate) struct PrepaintStateIndex {
    hitboxes_index: usize,
    tooltips_index: usize,
    deferred_draws_index: usize,
    dispatch_tree_index: usize,
    accessed_element_states_index: usize,
    line_layout_index: LineLayoutIndex,
}

#[derive(Clone, Default)]
pub(crate) struct PaintIndex {
    scene_index: usize,
    #[cfg(any(test, feature = "test-support"))]
    debug_bounds_index: usize,
    mouse_listeners_index: usize,
    input_handlers_index: usize,
    cursor_styles_index: usize,
    accessed_element_states_index: usize,
    tab_handle_index: usize,
    line_layout_index: LineLayoutIndex,
}

impl Frame {
    #[cfg(any(test, feature = "test-support"))]
    pub(crate) fn record_debug_bounds(&mut self, selector: String, bounds: Bounds<Pixels>) {
        self.debug_bounds.insert(selector.clone(), bounds);
        self.debug_bounds_records.push((selector, bounds));
    }

    pub(crate) fn new(dispatch_tree: DispatchTree) -> Self {
        Frame {
            focus: None,
            window_active: false,
            element_states: FxHashMap::default(),
            accessed_element_states: Vec::new(),
            mouse_listeners: Vec::new(),
            dispatch_tree,
            scene: Scene::default(),
            hitboxes: Vec::new(),
            window_control_hitboxes: Vec::new(),
            deferred_draws: Vec::new(),
            input_handlers: Vec::new(),
            tooltip_requests: Vec::new(),
            cursor_styles: Vec::new(),

            #[cfg(any(test, feature = "test-support"))]
            debug_bounds: FxHashMap::default(),
            #[cfg(any(test, feature = "test-support"))]
            debug_bounds_records: Vec::new(),

            #[cfg(any(feature = "inspector", debug_assertions))]
            next_inspector_instance_ids: FxHashMap::default(),

            #[cfg(any(feature = "inspector", debug_assertions))]
            inspector_hitboxes: FxHashMap::default(),
            tab_stops: TabStopMap::default(),
        }
    }

    pub(crate) fn clear(&mut self) {
        self.element_states.clear();
        self.accessed_element_states.clear();
        self.mouse_listeners.clear();
        self.dispatch_tree.clear();
        self.scene.clear();
        self.input_handlers.clear();
        self.tooltip_requests.clear();
        self.cursor_styles.clear();
        self.hitboxes.clear();
        self.window_control_hitboxes.clear();
        self.deferred_draws.clear();
        self.tab_stops.clear();
        self.focus = None;

        #[cfg(any(test, feature = "test-support"))]
        {
            self.debug_bounds.clear();
            self.debug_bounds_records.clear();
        }

        #[cfg(any(feature = "inspector", debug_assertions))]
        {
            self.next_inspector_instance_ids.clear();
            self.inspector_hitboxes.clear();
        }
    }

    pub(crate) fn cursor_style(&self, window: &Window) -> Option<CursorStyle> {
        self.cursor_styles
            .iter()
            .rev()
            .fold_while(None, |style, request| match request.hitbox_id {
                None => Done(Some(request.style)),
                Some(hitbox_id) => Continue(style.or_else(|| {
                    hitbox_id
                        .is_hovered_ignoring_last_input(window)
                        .then_some(request.style)
                })),
            })
            .into_inner()
    }

    pub(crate) fn hit_test(&self, position: Point<Pixels>) -> HitTest {
        let mut set_hover_hitbox_count = false;
        let mut hit_test = HitTest::default();
        for hitbox in self.hitboxes.iter().rev() {
            let bounds = hitbox.bounds.intersect(&hitbox.content_mask.bounds);
            if bounds.contains(&position) {
                hit_test.ids.push(hitbox.id);
                if !set_hover_hitbox_count
                    && hitbox.behavior == HitboxBehavior::BlockMouseExceptScroll
                {
                    hit_test.hover_hitbox_count = hit_test.ids.len();
                    set_hover_hitbox_count = true;
                }
                if hitbox.behavior == HitboxBehavior::BlockMouse {
                    break;
                }
            }
        }
        if !set_hover_hitbox_count {
            hit_test.hover_hitbox_count = hit_test.ids.len();
        }
        hit_test
    }

    pub(crate) fn focus_path(&self) -> SmallVec<[FocusId; 8]> {
        self.focus
            .map(|focus_id| self.dispatch_tree.focus_path(focus_id))
            .unwrap_or_default()
    }

    pub(crate) fn finish(&mut self, prev_frame: &mut Self) {
        for element_state_key in &self.accessed_element_states {
            if let Some((element_state_key, element_state)) =
                prev_frame.element_states.remove_entry(element_state_key)
            {
                self.element_states.insert(element_state_key, element_state);
            }
        }

        self.scene.finish();
    }
}

/// Tracks input event timestamps to determine if input is arriving at a high rate.
/// Used to sustain VRR (Variable Refresh Rate) presentation during active input.
#[derive(Clone, Debug)]
pub(crate) struct InputRateTracker {
    timestamps: Vec<Instant>,
    window: Duration,
    inputs_per_second: u32,
    sustain_until: Instant,
    sustain_duration: Duration,
}

impl Default for InputRateTracker {
    fn default() -> Self {
        Self {
            timestamps: Vec::new(),
            window: Duration::from_millis(100),
            inputs_per_second: 60,
            sustain_until: Instant::now(),
            sustain_duration: Duration::from_secs(1),
        }
    }
}

impl InputRateTracker {
    pub fn record_input(&mut self) {
        let now = Instant::now();
        self.timestamps.push(now);
        self.timestamps
            .retain(|&t| now.duration_since(t) <= self.window);
        let min_events = self.inputs_per_second as u128 * self.window.as_millis() / 1000;
        if self.timestamps.len() as u128 >= min_events {
            self.sustain_until = now + self.sustain_duration;
        }
    }

    pub fn is_high_rate(&self) -> bool {
        Instant::now() < self.sustain_until
    }
}

fn frame_throttle_interval(
    force_render: bool,
    require_presentation: bool,
    has_next_frame_callbacks: bool,
    is_active: bool,
    has_high_rate_input: bool,
    is_thermally_constrained: bool,
    inactive_frame_interval: Option<Duration>,
) -> Option<Duration> {
    if require_presentation || (!force_render && !has_next_frame_callbacks) {
        None
    } else if !is_active && !has_high_rate_input {
        inactive_frame_interval
    } else if is_thermally_constrained {
        Some(Duration::from_micros(16667))
    } else {
        None
    }
}

/// A point-in-time snapshot of the input latency histograms for a window.
#[cfg(feature = "input-latency-histogram")]
pub struct InputLatencySnapshot {
    /// Histogram of input-to-frame latency samples, in nanoseconds.
    pub latency_histogram: Histogram<u64>,
    /// Histogram of input events coalesced per rendered frame.
    pub events_per_frame_histogram: Histogram<u64>,
    /// Count of input events that arrived mid-draw and were excluded from latency recording.
    pub mid_draw_events_dropped: u64,
}

#[cfg(feature = "input-latency-histogram")]
struct InputLatencyTracker {
    first_input_at: Option<Instant>,
    pending_input_count: u64,
    latency_histogram: Histogram<u64>,
    events_per_frame_histogram: Histogram<u64>,
    mid_draw_events_dropped: u64,
}

#[cfg(feature = "input-latency-histogram")]
impl InputLatencyTracker {
    fn new() -> Result<Self> {
        Ok(Self {
            first_input_at: None,
            pending_input_count: 0,
            latency_histogram: Histogram::new(3)
                .map_err(|error| anyhow!("failed to create input latency histogram: {error}"))?,
            events_per_frame_histogram: Histogram::new(3)
                .map_err(|error| anyhow!("failed to create events per frame histogram: {error}"))?,
            mid_draw_events_dropped: 0,
        })
    }

    fn record_input(&mut self, dispatch_time: Instant) {
        self.first_input_at.get_or_insert(dispatch_time);
        self.pending_input_count += 1;
    }

    fn record_mid_draw_input(&mut self) {
        self.mid_draw_events_dropped += 1;
    }

    fn record_frame_presented(&mut self) {
        if let Some(first_input_at) = self.first_input_at.take() {
            self.latency_histogram
                .record(first_input_at.elapsed().as_nanos() as u64)
                .ok();
        }

        if self.pending_input_count > 0 {
            self.events_per_frame_histogram
                .record(self.pending_input_count)
                .ok();
            self.pending_input_count = 0;
        }
    }

    fn snapshot(&self) -> InputLatencySnapshot {
        InputLatencySnapshot {
            latency_histogram: self.latency_histogram.clone(),
            events_per_frame_histogram: self.events_per_frame_histogram.clone(),
            mid_draw_events_dropped: self.mid_draw_events_dropped,
        }
    }
}

/// Holds the state for a specific window.
pub struct Window {
    pub(crate) handle: AnyWindowHandle,
    pub(crate) invalidator: WindowInvalidator,
    pub(crate) removed: bool,
    pub(crate) platform_window: Box<dyn PlatformWindow>,
    pub(crate) a11y: A11y,
    display_id: Option<DisplayId>,
    sprite_atlas: Arc<dyn PlatformAtlas>,
    text_system: Arc<WindowTextSystem>,
    rem_size: Pixels,
    /// The stack of override values for the window's rem size.
    ///
    /// This is used by `with_rem_size` to allow rendering an element tree with
    /// a given rem size.
    rem_size_override_stack: SmallVec<[Pixels; 8]>,
    pub(crate) viewport_size: Size<Pixels>,
    layout_engine: Option<TaffyLayoutEngine>,
    pub(crate) root: Option<AnyView>,
    pub(crate) element_id_stack: SmallVec<[ElementId; 32]>,
    pub(crate) text_style_stack: SmallVec<[TextStyleRefinement; 4]>,
    pub(crate) rendered_entity_stack: SmallVec<[EntityId; 16]>,
    pub(crate) element_offset_stack: SmallVec<[Point<Pixels>; 16]>,
    pub(crate) element_opacity: f32,
    pub(crate) content_mask_stack: SmallVec<[ContentMask<Pixels>; 16]>,
    pub(crate) requested_autoscroll: Option<Bounds<Pixels>>,
    pub(crate) image_cache_stack: SmallVec<[AnyImageCache; 4]>,
    pub(crate) rendered_frame: Frame,
    pub(crate) next_frame: Frame,
    next_hitbox_id: HitboxId,
    pub(crate) next_tooltip_id: TooltipId,
    pub(crate) tooltip_bounds: Option<TooltipBounds>,
    pub(crate) next_frame_callbacks: Rc<RefCell<Vec<FrameCallback>>>,
    pub(crate) dirty_views: FxHashSet<EntityId>,
    focus_listeners: SubscriberSet<(), AnyWindowFocusListener>,
    pub(crate) focus_lost_listeners: SubscriberSet<(), AnyObserver>,
    focus_lost_path: SmallVec<[FocusId; 8]>,
    default_prevented: bool,
    mouse_position: Point<Pixels>,
    mouse_hit_test: HitTest,
    modifiers: Modifiers,
    capslock: Capslock,
    scale_factor: f32,
    pub(crate) bounds_observers: SubscriberSet<(), AnyObserver>,
    appearance: WindowAppearance,
    pub(crate) appearance_observers: SubscriberSet<(), AnyObserver>,
    active: Rc<Cell<bool>>,
    visibility: WindowVisibility,
    pub(crate) visibility_observers:
        SubscriberSet<(), Box<dyn FnMut(WindowVisibility, &mut Window, &mut App) -> bool>>,
    hovered: Rc<Cell<bool>>,
    pub(crate) needs_present: Rc<Cell<bool>>,
    pub(crate) input_rate_tracker: Rc<RefCell<InputRateTracker>>,
    #[cfg(feature = "input-latency-histogram")]
    input_latency_tracker: InputLatencyTracker,
    #[allow(dead_code)]
    pub(crate) last_frame_time: Rc<Cell<Option<Instant>>>,
    pub(crate) last_input_was_keyboard: Cell<bool>,
    pub(crate) refreshing: bool,
    pub(crate) activation_observers: SubscriberSet<(), AnyObserver>,
    pub(crate) focus: Option<FocusId>,
    focus_enabled: bool,
    pending_input: Option<PendingInput>,
    pending_modifier: ModifierState,
    pub(crate) pending_input_observers: SubscriberSet<(), AnyObserver>,
    prompt: Option<RenderablePromptHandle>,
    pub(crate) client_inset: Option<Pixels>,
    /// The hitbox that has captured the pointer, if any.
    /// While captured, mouse events route to this hitbox regardless of hit testing.
    captured_hitbox: Option<HitboxId>,
    #[cfg(any(feature = "inspector", debug_assertions))]
    inspector: Option<Entity<Inspector>>,
}

#[derive(Clone, Debug, Default)]
struct ModifierState {
    modifiers: Modifiers,
    saw_other_input: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum DrawPhase {
    None,
    Prepaint,
    Paint,
    Focus,
}

pub(crate) const PENDING_INPUT_TIMEOUT: Duration = Duration::from_secs(1);

/// Pending input for a potential multi-stroke key binding.
pub struct PendingInputStatus<'a> {
    keystrokes: &'a [Keystroke],
    timeout: Option<PendingInputTimeoutStatus>,
}

impl<'a> PendingInputStatus<'a> {
    /// Returns the keystrokes entered so far.
    pub fn keystrokes(&self) -> &'a [Keystroke] {
        self.keystrokes
    }

    /// Returns the timeout state for flushing this input, if it needs a timeout.
    pub fn timeout(&self) -> Option<PendingInputTimeoutStatus> {
        self.timeout
    }
}

/// The timeout state for pending input.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PendingInputTimeoutStatus {
    duration: Duration,
    remaining: Duration,
    started_at: Option<Instant>,
    paused: bool,
}

impl PendingInputTimeoutStatus {
    /// Returns the full timeout duration.
    pub fn duration(&self) -> Duration {
        self.duration
    }

    /// Returns the duration remaining before pending input is flushed.
    pub fn remaining(&self, cx: &App) -> Duration {
        self.started_at
            .map(|started_at| {
                self.remaining
                    .saturating_sub(cx.background_executor().now() - started_at)
            })
            .unwrap_or(self.remaining)
    }

    /// Returns whether the timeout is paused.
    pub fn is_paused(&self) -> bool {
        self.paused
    }
}

#[derive(Debug)]
struct PendingInputTimeout {
    duration: Duration,
    remaining: Duration,
    state: PendingInputTimeoutState,
}

#[derive(Debug)]
enum PendingInputTimeoutState {
    Running { started_at: Instant, task: Task<()> },
    Paused { pause: PendingInputTimeoutPause },
}

struct PendingInputTimeoutPause {
    owner_id: EntityId,
    _release_subscription: Subscription,
}

impl std::fmt::Debug for PendingInputTimeoutPause {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PendingInputTimeoutPause")
            .field("owner_id", &self.owner_id)
            .finish_non_exhaustive()
    }
}

impl PendingInputTimeout {
    fn is_paused(&self) -> bool {
        matches!(&self.state, PendingInputTimeoutState::Paused { .. })
    }

    fn pause(&mut self, pause: PendingInputTimeoutPause, now: Instant) -> bool {
        match mem::replace(&mut self.state, PendingInputTimeoutState::Paused { pause }) {
            PendingInputTimeoutState::Running { started_at, task } => {
                self.remaining = self.remaining.saturating_sub(now - started_at);
                drop(task);
                true
            }
            previous_state @ PendingInputTimeoutState::Paused { .. } => {
                self.state = previous_state;
                false
            }
        }
    }

    fn pause_owner_id(&self) -> Option<EntityId> {
        match &self.state {
            PendingInputTimeoutState::Running { .. } => None,
            PendingInputTimeoutState::Paused { pause } => Some(pause.owner_id),
        }
    }

    fn resume(&mut self, owner_id: EntityId, started_at: Instant, task: Task<()>) -> bool {
        match mem::replace(
            &mut self.state,
            PendingInputTimeoutState::Running { started_at, task },
        ) {
            PendingInputTimeoutState::Paused { pause } if pause.owner_id == owner_id => true,
            previous_state => {
                self.state = previous_state;
                false
            }
        }
    }

    fn reset_duration(&mut self, duration: Duration) {
        self.duration = duration;
        self.remaining = duration;
    }

    fn status(&self) -> PendingInputTimeoutStatus {
        let (started_at, paused) = match &self.state {
            PendingInputTimeoutState::Running { started_at, .. } => (Some(*started_at), false),
            PendingInputTimeoutState::Paused { .. } => (None, true),
        };
        PendingInputTimeoutStatus {
            duration: self.duration,
            remaining: self.remaining,
            started_at,
            paused,
        }
    }
}

#[derive(Default, Debug)]
struct PendingInput {
    keystrokes: SmallVec<[Keystroke; 1]>,
    focus: Option<FocusId>,
    timeout: Option<PendingInputTimeout>,
}

pub(crate) struct ElementStateBox {
    pub(crate) inner: Box<dyn Any>,
    #[cfg(debug_assertions)]
    pub(crate) type_name: &'static str,
}

fn default_bounds(display_id: Option<DisplayId>, cx: &mut App) -> Bounds<Pixels> {
    const DEFAULT_WINDOW_OFFSET: Point<Pixels> = point(px(0.), px(35.));

    // TODO, BUG: if you open a window with the currently active window
    // on the stack, this will erroneously select the 'unwrap_or_else'
    // code path
    cx.active_window()
        .and_then(|w| w.update(cx, |_, window, _| window.bounds()).ok())
        .map(|mut bounds| {
            bounds.origin += DEFAULT_WINDOW_OFFSET;
            bounds
        })
        .unwrap_or_else(|| {
            let display = display_id
                .map(|id| cx.find_display(id))
                .unwrap_or_else(|| cx.primary_display());

            display
                .map(|display| display.default_bounds())
                .unwrap_or_else(|| Bounds::new(point(px(0.), px(0.)), DEFAULT_WINDOW_SIZE))
        })
}

fn border_only_quad_strips(
    outer_bounds: Bounds<ScaledPixels>,
    widths: Edges<ScaledPixels>,
    radii: Corners<ScaledPixels>,
) -> Option<[Bounds<ScaledPixels>; 4]> {
    let antialias_slack = point(ScaledPixels(1.0), ScaledPixels(1.0));
    let top_left_inset = point(
        widths.left,
        widths.top.max(radii.top_left).max(radii.top_right),
    ) + antialias_slack;
    let bottom_right_inset = point(
        widths.right,
        widths.bottom.max(radii.bottom_left).max(radii.bottom_right),
    ) + antialias_slack;
    let inner_bounds = Bounds::from_corners(
        outer_bounds.origin + top_left_inset,
        outer_bounds.bottom_right() - bottom_right_inset,
    );
    if inner_bounds.is_empty() {
        return None;
    }

    Some([
        Bounds::from_corners(
            outer_bounds.origin,
            point(outer_bounds.right(), inner_bounds.top()),
        ),
        Bounds::from_corners(
            point(outer_bounds.left(), inner_bounds.bottom()),
            outer_bounds.bottom_right(),
        ),
        Bounds::from_corners(
            point(outer_bounds.left(), inner_bounds.top()),
            inner_bounds.bottom_left(),
        ),
        Bounds::from_corners(
            inner_bounds.top_right(),
            point(outer_bounds.right(), inner_bounds.bottom()),
        ),
    ])
}

impl Window {
    pub(crate) fn new(
        handle: AnyWindowHandle,
        options: WindowOptions,
        cx: &mut App,
    ) -> Result<Self> {
        let WindowOptions {
            window_bounds,
            titlebar,
            focus,
            show,
            kind,
            is_movable,
            app_owns_titlebar_drag,
            inactive_frame_interval,
            is_resizable,
            is_minimizable,
            display_id,
            window_background,
            app_id,
            window_min_size,
            window_decorations,
            #[cfg_attr(not(target_os = "macos"), allow(unused_variables))]
            tabbing_identifier,
            mouse_passthrough,
            icon,
        } = options;

        let bounds = window_bounds
            .map(|bounds| bounds.get_bounds())
            .unwrap_or_else(|| default_bounds(display_id, cx));
        let mut platform_window = cx.platform.open_window(
            handle,
            WindowParams {
                bounds,
                titlebar,
                kind,
                is_movable,
                app_owns_titlebar_drag,
                is_resizable,
                is_minimizable,
                focus,
                app_id: app_id.clone(),
                show,
                display_id,
                window_min_size,
                #[cfg(target_os = "macos")]
                tabbing_identifier,
                mouse_passthrough,
                icon,
            },
        )?;

        let tab_bar_visible = platform_window.tab_bar_visible();
        SystemWindowTabController::init_visible(cx, tab_bar_visible);
        if let Some(tabs) = platform_window.tabbed_windows() {
            SystemWindowTabController::add_tab(cx, handle.window_id(), tabs);
        }

        let display_id = platform_window.display().map(|display| display.id());
        let sprite_atlas = platform_window.sprite_atlas();
        let mouse_position = platform_window.mouse_position();
        let modifiers = platform_window.modifiers();
        let capslock = platform_window.capslock();
        let content_size = platform_window.content_size();
        let scale_factor = platform_window.scale_factor();
        let appearance = platform_window.appearance();
        let text_system = Arc::new(WindowTextSystem::new(cx.text_system().clone()));
        let invalidator = WindowInvalidator::new();
        let active = Rc::new(Cell::new(platform_window.is_active()));
        let visibility = platform_window.visibility();
        let hovered = Rc::new(Cell::new(platform_window.is_hovered()));
        let needs_present = Rc::new(Cell::new(false));
        let next_frame_callbacks: Rc<RefCell<Vec<FrameCallback>>> = Default::default();
        let input_rate_tracker = Rc::new(RefCell::new(InputRateTracker::default()));
        let last_frame_time = Rc::new(Cell::new(None::<Instant>));

        platform_window
            .request_decorations(window_decorations.unwrap_or(WindowDecorations::Server));
        platform_window.set_background_appearance(window_background);

        if let Some(ref window_open_state) = window_bounds {
            match window_open_state {
                WindowBounds::Fullscreen(_) => platform_window.toggle_fullscreen(),
                WindowBounds::Maximized(_) => platform_window.zoom(),
                WindowBounds::Windowed(_) => {}
            }
        }

        let accessibility_force_disabled =
            cx.accessibility_force_disabled || !cfg!(feature = "accessibility");
        let a11y_active_flag = Arc::new(AtomicBool::new(false));

        #[cfg(not(target_family = "wasm"))]
        if !accessibility_force_disabled {
            let initial_tree = accesskit::TreeUpdate {
                nodes: vec![(ROOT_NODE_ID, accesskit::Node::new(accesskit::Role::Window))],
                tree: Some(accesskit::TreeInfo::new(ROOT_NODE_ID)),
                tree_id: accesskit::TreeId::ROOT,
                focus: ROOT_NODE_ID,
            };
            let (activation_sender, activation_receiver) = async_channel::unbounded::<()>();
            let (deactivation_sender, deactivation_receiver) = async_channel::unbounded::<()>();
            let (action_sender, action_receiver) =
                async_channel::unbounded::<accesskit::ActionRequest>();

            platform_window.a11y_init(crate::A11yCallbacks {
                activation: {
                    let active_flag = a11y_active_flag.clone();
                    Box::new(move || {
                        log::info!("Accessibility activated");
                        active_flag.store(true, SeqCst);
                        activation_sender.send_blocking(()).log_err();
                        Some(initial_tree.clone())
                    })
                },
                action: Box::new(move |request| {
                    action_sender.send_blocking(request).log_err();
                }),
                deactivation: {
                    let active_flag = a11y_active_flag.clone();
                    Box::new(move || {
                        log::info!("Accessibility deactivated");
                        active_flag.store(false, SeqCst);
                        deactivation_sender.send_blocking(()).log_err();
                    })
                },
            });

            let mut async_cx = cx.to_async();
            cx.foreground_executor()
                .spawn(async move {
                    while activation_receiver.recv().await.is_ok() {
                        handle
                            .update(&mut async_cx, |_, window, _| window.refresh())
                            .log_err();
                    }
                })
                .detach();

            let mut async_cx = cx.to_async();
            cx.foreground_executor()
                .spawn(async move {
                    while deactivation_receiver.recv().await.is_ok() {
                        handle
                            .update(&mut async_cx, |_, window, _| window.refresh())
                            .log_err();
                    }
                })
                .detach();

            let mut async_cx = cx.to_async();
            cx.foreground_executor()
                .spawn(async move {
                    while let Ok(request) = action_receiver.recv().await {
                        handle
                            .update(&mut async_cx, |_, window, cx| {
                                window.handle_a11y_action(request, cx);
                            })
                            .log_err();
                    }
                })
                .detach();
        }

        platform_window.on_close(Box::new({
            let window_id = handle.window_id();
            let mut cx = cx.to_async();
            move || {
                let _ = handle.update(&mut cx, |_, window, _| window.remove_window());
                let _ = cx.update(|cx| {
                    SystemWindowTabController::remove_tab(cx, window_id);
                });
            }
        }));
        platform_window.on_request_frame(Box::new({
            let mut cx = cx.to_async();
            let invalidator = invalidator.clone();
            let active = active.clone();
            let needs_present = needs_present.clone();
            let next_frame_callbacks = next_frame_callbacks.clone();
            let input_rate_tracker = input_rate_tracker.clone();
            let last_frame_time = last_frame_time.clone();
            let mut deferred_force_render = false;
            move |request_frame_options| {
                // This must be checked before anything else: if this request
                // arrived re-entrantly while a draw is on this thread's stack
                // (e.g. via a nested message pump in the Windows window
                // procedure), drawing would nest draws, and even touching the
                // App would panic on its already-mutable borrow. Skip instead;
                // the platform leaves the window invalidated (or re-invalidates
                // it), so a fresh request arrives once the in-progress draw
                // unwinds. Remember force_render so the deferred frame still
                // bypasses the view cache.
                //
                // Returning here skips a demand-driven `schedule_frame` retry.
                // Calling into App would panic on its already-mutable borrow,
                // and this branch is unreachable on Wayland in practice: only
                // Windows pumps platform events (and thus requests frames)
                // mid-draw.
                if draw_in_progress() {
                    log::debug!("deferring re-entrant window draw request");
                    deferred_force_render |= request_frame_options.force_render;
                    return;
                }
                // Take the deferred flag first: `||` short-circuits, and leaving
                // the flag set when this request already forces a render would
                // force a second, redundant render on the next frame.
                let force_render =
                    mem::take(&mut deferred_force_render) || request_frame_options.force_render;

                let thermal_state = handle
                    .update(&mut cx, |_, _, cx| cx.thermal_state())
                    .log_err();

                // Throttle frame rate based on conditions:
                // - Thermal pressure (Serious/Critical): cap to ~60fps
                // - Inactive window (not focused): cap to ~30fps to save energy,
                //   unless high-rate input is arriving under the pointer.
                let has_high_rate_input = input_rate_tracker.borrow().is_high_rate();
                let min_frame_interval = frame_throttle_interval(
                    force_render,
                    request_frame_options.require_presentation,
                    !next_frame_callbacks.borrow().is_empty(),
                    active.get(),
                    has_high_rate_input,
                    matches!(
                        thermal_state,
                        Some(ThermalState::Critical | ThermalState::Serious)
                    ),
                    inactive_frame_interval,
                );

                let now = Instant::now();
                if let Some(min_interval) = min_frame_interval {
                    if let Some(last_frame) = last_frame_time.get() {
                        if now.duration_since(last_frame) < min_interval {
                            // Don't lose a pending forced render to throttling.
                            deferred_force_render |= force_render;
                            // Deferred by throttling: ask demand-driven platforms to retry.
                            handle
                                .update(&mut cx, |_, window, _| {
                                    window.platform_window.schedule_frame();
                                })
                                .log_err();
                            return;
                        }
                    }
                }
                last_frame_time.set(Some(now));

                let next_frame_callbacks = next_frame_callbacks.take();
                if !next_frame_callbacks.is_empty() {
                    handle
                        .update(&mut cx, |_, window, cx| {
                            for callback in next_frame_callbacks {
                                callback(window, cx);
                            }
                        })
                        .log_err();
                }

                // Keep presenting if input was recently arriving at a high rate (>= 60fps),
                // including while an unfocused window receives pointer input.
                let needs_present = request_frame_options.require_presentation
                    || needs_present.get()
                    || has_high_rate_input;

                if invalidator.is_dirty() || force_render {
                    measure("frame duration", || {
                        handle
                            .update(&mut cx, |_, window, cx| {
                                if force_render {
                                    window.refresh();
                                }
                                let arena_clear_needed = window.draw(cx);
                                window.present();
                                // drop the arena elements after present to reduce latency
                                arena_clear_needed.clear();
                            })
                            .log_err();
                    })
                } else if needs_present {
                    handle
                        .update(&mut cx, |_, window, _| window.present())
                        .log_err();
                }

                handle
                    .update(&mut cx, |_, window, _| {
                        if window.invalidator.is_dirty()
                            || !window.next_frame_callbacks.borrow().is_empty()
                        {
                            window.platform_window.schedule_frame();
                        }
                    })
                    .log_err();
            }
        }));
        platform_window.on_resize(Box::new({
            let mut cx = cx.to_async();
            let foreground_executor = cx.foreground_executor().clone();
            move |_, _| {
                // Frame changes issued inside a window update (e.g.
                // `Window::resize_anchored`) re-enter this callback
                // synchronously while the app is still borrowed. When that
                // happens, retry once the current update completes instead
                // of dropping the resize notification.
                if handle
                    .update(&mut cx, |_, window, cx| window.bounds_changed(cx))
                    .is_err()
                {
                    let mut cx = cx.clone();
                    foreground_executor
                        .spawn(async move {
                            handle
                                .update(&mut cx, |_, window, cx| window.bounds_changed(cx))
                                .log_err();
                        })
                        .detach();
                }
            }
        }));
        platform_window.on_moved(Box::new({
            let mut cx = cx.to_async();
            let foreground_executor = cx.foreground_executor().clone();
            move || {
                // See the `on_resize` registration above: moved notifications
                // can also fire synchronously while the app is borrowed.
                if handle
                    .update(&mut cx, |_, window, cx| window.bounds_changed(cx))
                    .is_err()
                {
                    let mut cx = cx.clone();
                    foreground_executor
                        .spawn(async move {
                            handle
                                .update(&mut cx, |_, window, cx| window.bounds_changed(cx))
                                .log_err();
                        })
                        .detach();
                }
            }
        }));
        platform_window.on_appearance_changed(Box::new({
            let cx = cx.to_async();
            let foreground_executor = cx.foreground_executor().clone();
            move || {
                let mut cx = cx.clone();
                // Defer the update because changing the AppKit appearance may
                // synchronously invoke this callback while App is already borrowed.
                foreground_executor
                    .spawn(async move {
                        handle
                            .update(&mut cx, |_, window, cx| window.appearance_changed(cx))
                            .log_err();
                    })
                    .detach();
            }
        }));
        platform_window.on_active_status_change(Box::new({
            let mut cx = cx.to_async();
            move |active| {
                handle
                    .update(&mut cx, |_, window, cx| {
                        if !active {
                            // macOS: per-window to immediately unhide cursor if hidden.
                            // Linux/Windows: platform-level handles it.
                            window.platform_window.set_cursor_style(CursorStyle::Arrow);
                            cx.platform.set_cursor_style(CursorStyle::Arrow);
                        }

                        window.active.set(active);
                        window.modifiers = window.platform_window.modifiers();
                        window.capslock = window.platform_window.capslock();
                        window
                            .activation_observers
                            .clone()
                            .retain(&(), |callback| callback(window, cx));

                        window.bounds_changed(cx);
                        window.refresh();

                        SystemWindowTabController::update_last_active(cx, window.handle.id);
                    })
                    .log_err();
            }
        }));
        platform_window.on_visibility_change(Box::new({
            let mut cx = cx.to_async();
            move |visibility| {
                handle
                    .update(&mut cx, |_, window, cx| {
                        if window.visibility == visibility {
                            return;
                        }
                        window.visibility = visibility;
                        window
                            .visibility_observers
                            .clone()
                            .retain(&(), |callback| callback(visibility, window, cx));
                    })
                    .log_err();
            }
        }));
        platform_window.on_hover_status_change(Box::new({
            let mut cx = cx.to_async();
            move |active| {
                handle
                    .update(&mut cx, |_, window, _| {
                        window.hovered.set(active);
                        window.refresh();
                    })
                    .log_err();
            }
        }));
        platform_window.on_input({
            let mut cx = cx.to_async();
            Box::new(move |event| {
                handle
                    .update(&mut cx, |_, window, cx| window.dispatch_event(event, cx))
                    .log_err()
                    .unwrap_or(DispatchEventResult::default())
            })
        });
        platform_window.on_hit_test_window_control({
            let mut cx = cx.to_async();
            Box::new(move || {
                handle
                    .update(&mut cx, |_, window, _cx| {
                        for (area, hitbox) in &window.rendered_frame.window_control_hitboxes {
                            if window.mouse_hit_test.ids.contains(&hitbox.id) {
                                return Some(*area);
                            }
                        }
                        None
                    })
                    .log_err()
                    .unwrap_or(None)
            })
        });
        platform_window.on_move_tab_to_new_window({
            let mut cx = cx.to_async();
            Box::new(move || {
                handle
                    .update(&mut cx, |_, _window, cx| {
                        SystemWindowTabController::move_tab_to_new_window(cx, handle.window_id());
                    })
                    .log_err();
            })
        });
        platform_window.on_merge_all_windows({
            let mut cx = cx.to_async();
            Box::new(move || {
                handle
                    .update(&mut cx, |_, _window, cx| {
                        SystemWindowTabController::merge_all_windows(cx, handle.window_id());
                    })
                    .log_err();
            })
        });
        platform_window.on_select_next_tab({
            let mut cx = cx.to_async();
            Box::new(move || {
                handle
                    .update(&mut cx, |_, _window, cx| {
                        SystemWindowTabController::select_next_tab(cx, handle.window_id());
                    })
                    .log_err();
            })
        });
        platform_window.on_select_previous_tab({
            let mut cx = cx.to_async();
            Box::new(move || {
                handle
                    .update(&mut cx, |_, _window, cx| {
                        SystemWindowTabController::select_previous_tab(cx, handle.window_id())
                    })
                    .log_err();
            })
        });
        platform_window.on_toggle_tab_bar({
            let mut cx = cx.to_async();
            Box::new(move || {
                handle
                    .update(&mut cx, |_, window, cx| {
                        let tab_bar_visible = window.platform_window.tab_bar_visible();
                        SystemWindowTabController::set_visible(cx, tab_bar_visible);
                    })
                    .log_err();
            })
        });

        if let Some(app_id) = app_id {
            platform_window.set_app_id(&app_id);
        }

        // Map only after all callbacks above are registered, and only when
        // the window is meant to be visible: backends that gate visibility on
        // mapping (X11) honor `WindowOptions::show` this way, matching macOS
        // where `show`/`focus` are consumed at window creation.
        if show {
            platform_window.map_window()?;
        }

        Ok(Window {
            handle,
            invalidator,
            removed: false,
            platform_window,
            a11y: A11y::new(a11y_active_flag, accessibility_force_disabled),
            display_id,
            sprite_atlas,
            text_system,
            rem_size: px(16.),
            rem_size_override_stack: SmallVec::new(),
            viewport_size: content_size,
            layout_engine: Some(TaffyLayoutEngine::new()),
            root: None,
            element_id_stack: SmallVec::default(),
            text_style_stack: SmallVec::new(),
            rendered_entity_stack: SmallVec::new(),
            element_offset_stack: SmallVec::new(),
            content_mask_stack: SmallVec::new(),
            element_opacity: 1.0,
            requested_autoscroll: None,
            rendered_frame: Frame::new(DispatchTree::new(cx.keymap.clone(), cx.actions.clone())),
            next_frame: Frame::new(DispatchTree::new(cx.keymap.clone(), cx.actions.clone())),
            next_frame_callbacks,
            next_hitbox_id: HitboxId(0),
            next_tooltip_id: TooltipId::default(),
            tooltip_bounds: None,
            dirty_views: FxHashSet::default(),
            focus_listeners: SubscriberSet::new(),
            focus_lost_listeners: SubscriberSet::new(),
            focus_lost_path: SmallVec::new(),
            default_prevented: true,
            mouse_position,
            mouse_hit_test: HitTest::default(),
            modifiers,
            capslock,
            scale_factor,
            bounds_observers: SubscriberSet::new(),
            appearance,
            appearance_observers: SubscriberSet::new(),
            active,
            visibility,
            visibility_observers: SubscriberSet::new(),
            hovered,
            needs_present,
            input_rate_tracker,
            #[cfg(feature = "input-latency-histogram")]
            input_latency_tracker: InputLatencyTracker::new()?,
            last_frame_time,
            last_input_was_keyboard: Cell::new(false),
            refreshing: false,
            activation_observers: SubscriberSet::new(),
            focus: None,
            focus_enabled: true,
            pending_input: None,
            pending_modifier: ModifierState::default(),
            pending_input_observers: SubscriberSet::new(),
            prompt: None,
            client_inset: None,
            image_cache_stack: SmallVec::new(),
            captured_hitbox: None,
            #[cfg(any(feature = "inspector", debug_assertions))]
            inspector: None,
        })
    }

    pub(crate) fn new_focus_listener(
        &self,
        value: AnyWindowFocusListener,
    ) -> (Subscription, impl FnOnce() + use<>) {
        self.focus_listeners.insert((), value)
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
#[allow(missing_docs)]
pub struct DispatchEventResult {
    pub propagate: bool,
    pub default_prevented: bool,
}

/// Indicates which region of the window is visible. Content falling outside of this mask will not be
/// rendered. Currently, only rectangular content masks are supported, but we give the mask its own type
/// to leave room to support more complex shapes in the future.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
#[repr(C)]
pub struct ContentMask<P: Clone + Debug + Default + PartialEq> {
    /// The bounds
    pub bounds: Bounds<P>,
}

impl ContentMask<Pixels> {
    /// Scale the content mask's pixel units by the given scaling factor.
    pub fn scale(&self, factor: f32) -> ContentMask<ScaledPixels> {
        ContentMask {
            bounds: self.bounds.scale(factor),
        }
    }

    /// Intersect the content mask with the given content mask.
    pub fn intersect(&self, other: &Self) -> Self {
        let bounds = self.bounds.intersect(&other.bounds);
        ContentMask { bounds }
    }
}

impl Window {
    fn mark_view_dirty(&mut self, view_id: EntityId) {
        // Mark ancestor views as dirty. If already in the `dirty_views` set, then all its ancestors
        // should already be dirty.
        for view_id in self
            .rendered_frame
            .dispatch_tree
            .view_path(view_id)
            .into_iter()
            .rev()
        {
            if !self.dirty_views.insert(view_id) {
                break;
            }
        }
    }

    /// Whether the platform is presenting this window's frames (see
    /// [`WindowVisibility`]).
    pub fn visibility(&self) -> WindowVisibility {
        self.visibility
    }

    /// Whether frames drawn for this window will be shown.
    ///
    /// This is not the window's shown/hidden state: a shown window that is
    /// fully behind another window, minimized, or on a sleeping display is not
    /// visible here. [`PlatformWindow::is_visible`] reports that shown/hidden
    /// state instead.
    pub fn is_visible(&self) -> bool {
        self.visibility.is_visible()
    }

    /// Registers a callback to be invoked when the window's visibility changes.
    pub fn observe_window_visibility(
        &self,
        mut callback: impl FnMut(WindowVisibility, &mut Window, &mut App) + 'static,
    ) -> Subscription {
        let (subscription, activate) = self.visibility_observers.insert(
            (),
            Box::new(move |visibility, window, cx| {
                callback(visibility, window, cx);
                true
            }),
        );
        activate();
        subscription
    }

    /// Registers a callback to be invoked when the window appearance changes.
    pub fn observe_window_appearance(
        &self,
        mut callback: impl FnMut(&mut Window, &mut App) + 'static,
    ) -> Subscription {
        let (subscription, activate) = self.appearance_observers.insert(
            (),
            Box::new(move |window, cx| {
                callback(window, cx);
                true
            }),
        );
        activate();
        subscription
    }

    /// Replaces the root entity of the window with a new one.
    pub fn replace_root<E>(
        &mut self,
        cx: &mut App,
        build_view: impl FnOnce(&mut Window, &mut Context<E>) -> E,
    ) -> Entity<E>
    where
        E: 'static + Render,
    {
        let view = cx.new(|cx| build_view(self, cx));
        self.root = Some(view.clone().into());
        self.refresh();
        view
    }

    /// Returns the root entity of the window, if it has one.
    pub fn root<E>(&self) -> Option<Option<Entity<E>>>
    where
        E: 'static + Render,
    {
        self.root
            .as_ref()
            .map(|view| view.clone().downcast::<E>().ok())
    }

    /// Obtain a handle to the window that belongs to this context.
    pub fn window_handle(&self) -> AnyWindowHandle {
        self.handle
    }

    /// Mark the window as dirty, scheduling it to be redrawn on the next frame.
    pub fn refresh(&mut self) {
        if self.invalidator.not_drawing() {
            self.refreshing = true;
            self.invalidator.set_dirty(true);
        }
    }

    /// Close this window.
    pub fn remove_window(&mut self) {
        self.removed = true;
    }

    /// Obtain the currently focused [`FocusHandle`]. If no elements are focused, returns `None`.
    pub fn focused(&self, cx: &App) -> Option<FocusHandle> {
        self.focus
            .and_then(|id| FocusHandle::for_id(id, &cx.focus_handles))
    }

    /// While focus-lost listeners are being dispatched, returns the closest ancestor of the
    /// previously focused element that can still receive focus, making it a suitable target
    /// for focus restoration. Returns `None` at all other times, or when no such ancestor exists.
    pub fn focus_lost_restore_target(&self, cx: &App) -> Option<FocusHandle> {
        let (_leaf, ancestors) = self.focus_lost_path.split_last()?;
        ancestors.iter().rev().find_map(|id| {
            self.rendered_frame.dispatch_tree.focusable_node_id(*id)?;
            FocusHandle::for_id(*id, &cx.focus_handles)
        })
    }

    /// Move focus to the element associated with the given [`FocusHandle`].
    pub fn focus(&mut self, handle: &FocusHandle) {
        if !self.focus_enabled || self.focus == Some(handle.id) {
            return;
        }

        self.focus = Some(handle.id);
        self.pending_input.take();
        self.refresh();
    }

    /// Remove focus from all elements within this context's window.
    pub fn blur(&mut self, cx: &mut App) {
        self.clear_pending_keystrokes(cx);

        if !self.focus_enabled {
            return;
        }

        self.focus = None;
        self.refresh();
    }

    /// Blur the window and don't allow anything in it to be focused again.
    pub fn disable_focus(&mut self, cx: &mut App) {
        self.blur(cx);
        self.focus_enabled = false;
    }

    /// Move focus to next tab stop.
    pub fn focus_next(&mut self) {
        if !self.focus_enabled {
            return;
        }

        if let Some(handle) = self.rendered_frame.tab_stops.next(self.focus.as_ref()) {
            self.focus(&handle)
        }
    }

    /// Move focus to previous tab stop.
    pub fn focus_prev(&mut self) {
        if !self.focus_enabled {
            return;
        }

        if let Some(handle) = self.rendered_frame.tab_stops.prev(self.focus.as_ref()) {
            self.focus(&handle)
        }
    }

    /// Accessor for the text system.
    pub fn text_system(&self) -> &Arc<WindowTextSystem> {
        &self.text_system
    }

    /// The current text style. Which is composed of all the style refinements provided to `with_text_style`.
    pub fn text_style(&self) -> TextStyle {
        let mut style = TextStyle::default();
        for refinement in &self.text_style_stack {
            style.refine(refinement);
        }
        style
    }

    /// Check if the platform window is maximized
    /// On some platforms (namely Windows) this is different than the bounds being the size of the display
    pub fn is_maximized(&self) -> bool {
        self.platform_window.is_maximized()
    }

    /// request a certain window decoration (Wayland)
    pub fn request_decorations(&self, decorations: WindowDecorations) {
        self.platform_window.request_decorations(decorations);
    }

    /// Sets how much space a Wayland layer-shell surface reserves from its anchored edge.
    pub fn set_exclusive_zone(&self, zone: Pixels) {
        self.platform_window.set_exclusive_zone(zone);
    }

    /// Selects the anchored edge used by a Wayland layer-shell surface's exclusive zone.
    /// Invalid or unsupported edges are ignored by the platform implementation.
    #[cfg(all(any(target_os = "linux", target_os = "freebsd"), feature = "wayland"))]
    pub fn set_exclusive_edge(&self, edge: crate::layer_shell::Anchor) {
        self.platform_window.set_exclusive_edge(edge);
    }

    /// Start a window resize operation (Wayland)
    pub fn start_window_resize(&self, edge: ResizeEdge) {
        self.platform_window.start_window_resize(edge);
    }

    /// Return the `WindowBounds` to indicate that how a window should be opened
    /// after it has been closed
    pub fn window_bounds(&self) -> WindowBounds {
        self.platform_window.window_bounds()
    }

    /// Return the `WindowBounds` excluding insets (Wayland and X11)
    pub fn inner_window_bounds(&self) -> WindowBounds {
        self.platform_window.inner_window_bounds()
    }

    /// Encode the window's native restorable state into an opaque blob.
    /// Returns `None` on platforms without native state restoration or if encoding fails.
    pub fn native_window_state(&self) -> Option<Vec<u8>> {
        self.platform_window.native_window_state()
    }

    /// Restore the window's native state from a blob previously produced
    /// by [`Window::native_window_state`]. A no-op on platforms without native state restoration.
    pub fn restore_native_window_state(&self, state: &[u8]) {
        self.platform_window.restore_native_window_state(state);
    }

    /// Dispatch the given action on the currently focused element.
    pub fn dispatch_action(&mut self, action: Box<dyn Action>, cx: &mut App) {
        let focus_id = self.focused(cx).map(|handle| handle.id);

        let window = self.handle;
        cx.defer(move |cx| {
            window
                .update(cx, |_, window, cx| {
                    let node_id = window.focus_node_id_in_rendered_frame(focus_id);
                    window.dispatch_action_on_node(node_id, action.as_ref(), cx);
                })
                .log_err();
        })
    }

    pub(crate) fn dispatch_keystroke_observers(
        &mut self,
        keystroke: &Keystroke,
        input_preference: InputPreference,
        action: Option<&dyn Action>,
        context_stack: Vec<KeyContext>,
        cx: &mut App,
    ) {
        cx.keystroke_observers.clone().retain(&(), move |callback| {
            (callback)(
                &KeystrokeEvent {
                    keystroke: keystroke.clone(),
                    input_preference,
                    action: action.map(|action| action.boxed_clone()),
                    context_stack: context_stack.clone(),
                },
                self,
                cx,
            )
        });
    }

    pub(crate) fn dispatch_keystroke_interceptors(
        &mut self,
        keystroke: &Keystroke,
        input_preference: InputPreference,
        context_stack: Vec<KeyContext>,
        cx: &mut App,
    ) {
        cx.keystroke_interceptors
            .clone()
            .retain(&(), move |callback| {
                (callback)(
                    &KeystrokeEvent {
                        keystroke: keystroke.clone(),
                        input_preference,
                        action: None,
                        context_stack: context_stack.clone(),
                    },
                    self,
                    cx,
                )
            });
    }

    /// Schedules the given function to be run at the end of the current effect cycle, allowing entities
    /// that are currently on the stack to be returned to the app.
    pub fn defer(&self, cx: &mut App, f: impl FnOnce(&mut Window, &mut App) + 'static) {
        let handle = self.handle;
        cx.defer(move |cx| {
            handle.update(cx, |_, window, cx| f(window, cx)).ok();
        });
    }

    /// Subscribe to events emitted by a entity.
    /// The entity to which you're subscribing must implement the [`EventEmitter`] trait.
    /// The callback will be invoked a handle to the emitting entity, the event, and a window context for the current window.
    pub fn observe<T: 'static>(
        &mut self,
        observed: &Entity<T>,
        cx: &mut App,
        mut on_notify: impl FnMut(Entity<T>, &mut Window, &mut App) + 'static,
    ) -> Subscription {
        let entity_id = observed.entity_id();
        let observed = observed.downgrade();
        let window_handle = self.handle;
        cx.new_observer(
            entity_id,
            Box::new(move |cx| {
                window_handle
                    .update(cx, |_, window, cx| {
                        if let Some(handle) = observed.upgrade() {
                            on_notify(handle, window, cx);
                            true
                        } else {
                            false
                        }
                    })
                    .unwrap_or(false)
            }),
        )
    }

    /// Subscribe to events emitted by a entity.
    /// The entity to which you're subscribing must implement the [`EventEmitter`] trait.
    /// The callback will be invoked a handle to the emitting entity, the event, and a window context for the current window.
    pub fn subscribe<Emitter, Evt>(
        &mut self,
        entity: &Entity<Emitter>,
        cx: &mut App,
        mut on_event: impl FnMut(Entity<Emitter>, &Evt, &mut Window, &mut App) + 'static,
    ) -> Subscription
    where
        Emitter: EventEmitter<Evt>,
        Evt: 'static,
    {
        let entity_id = entity.entity_id();
        let handle = entity.downgrade();
        let window_handle = self.handle;
        cx.new_subscription(
            entity_id,
            (
                TypeId::of::<Evt>(),
                Box::new(move |event, cx| {
                    window_handle
                        .update(cx, |_, window, cx| {
                            if let Some(entity) = handle.upgrade() {
                                let event = event.downcast_ref().expect("invalid event type");
                                on_event(entity, event, window, cx);
                                true
                            } else {
                                false
                            }
                        })
                        .unwrap_or(false)
                }),
            ),
        )
    }

    /// Register a callback to be invoked when the given `Entity` is released.
    pub fn observe_release<T>(
        &self,
        entity: &Entity<T>,
        cx: &mut App,
        mut on_release: impl FnOnce(&mut T, &mut Window, &mut App) + 'static,
    ) -> Subscription
    where
        T: 'static,
    {
        let entity_id = entity.entity_id();
        let window_handle = self.handle;
        let (subscription, activate) = cx.release_listeners.insert(
            entity_id,
            Box::new(move |entity, cx| {
                let entity = entity.downcast_mut().expect("invalid entity type");
                let _ = window_handle.update(cx, |_, window, cx| on_release(entity, window, cx));
            }),
        );
        activate();
        subscription
    }

    /// Creates an [`AsyncWindowContext`], which has a static lifetime and can be held across
    /// await points in async code.
    pub fn to_async(&self, cx: &App) -> AsyncWindowContext {
        AsyncWindowContext::new_context(cx.to_async(), self.handle)
    }

    /// Schedule the given closure to be run directly after the current frame is rendered.
    pub fn on_next_frame(&self, callback: impl FnOnce(&mut Window, &mut App) + 'static) {
        RefCell::borrow_mut(&self.next_frame_callbacks).push(Box::new(callback));
        self.platform_window.schedule_frame();
    }

    /// Schedule a frame to be drawn on the next animation frame.
    ///
    /// This is useful for elements that need to animate continuously, such as a video player or an animated GIF.
    /// It will cause the window to redraw on the next frame, even if no other changes have occurred.
    ///
    /// If called from within a view, it will notify that view on the next frame. Otherwise, it will refresh the entire window.
    pub fn request_animation_frame(&self) {
        let entity = self.current_view();
        self.on_next_frame(move |_, cx| cx.notify(entity));
    }

    /// Spawn the future returned by the given closure on the application thread pool.
    /// The closure is provided a handle to the current window and an `AsyncWindowContext` for
    /// use within your future.
    #[track_caller]
    pub fn spawn<AsyncFn, R>(&self, cx: &App, f: AsyncFn) -> Task<R>
    where
        R: 'static,
        AsyncFn: AsyncFnOnce(&mut AsyncWindowContext) -> R + 'static,
    {
        let handle = self.handle;
        cx.spawn(async move |app| {
            let mut async_window_cx = AsyncWindowContext::new_context(app.clone(), handle);
            f(&mut async_window_cx).await
        })
    }

    fn bounds_changed(&mut self, cx: &mut App) {
        self.scale_factor = self.platform_window.scale_factor();
        self.viewport_size = self.platform_window.content_size();
        self.display_id = self.platform_window.display().map(|display| display.id());
        self.mouse_position = self.platform_window.mouse_position();
        self.platform_window.a11y_update_window_bounds();

        self.refresh();

        self.bounds_observers
            .clone()
            .retain(&(), |callback| callback(self, cx));
    }

    /// Returns the bounds of the current window in the global coordinate space, which could span across multiple displays.
    pub fn bounds(&self) -> Bounds<Pixels> {
        self.platform_window.bounds()
    }

    /// Set the content size of the window.
    pub fn resize(&mut self, size: Size<Pixels>) {
        self.platform_window.resize(size);
    }

    /// Set the content size of the window without animating, keeping
    /// `anchor` fixed on screen (e.g. `ResizeAnchor::TopLeft` grows the
    /// window down and to the right, leaving the top-left corner in place).
    ///
    /// On platforms without an anchored implementation this falls back to
    /// [`Self::resize`].
    pub fn resize_anchored(&mut self, size: Size<Pixels>, anchor: ResizeAnchor) {
        self.platform_window.resize_anchored(size, anchor);
    }

    /// Returns whether or not the window is currently fullscreen
    pub fn is_fullscreen(&self) -> bool {
        self.platform_window.is_fullscreen()
    }

    /// Returns whether the window is currently in simple (borderless) fullscreen,
    /// where it covers the entire screen including the menu bar and notch area.
    /// Always `false` on platforms other than macOS.
    pub fn is_simple_fullscreen(&self) -> bool {
        self.platform_window.is_simple_fullscreen()
    }

    pub(crate) fn appearance_changed(&mut self, cx: &mut App) {
        self.appearance = self.platform_window.appearance();

        self.appearance_observers
            .clone()
            .retain(&(), |callback| callback(self, cx));
    }

    /// Returns the appearance of the current window.
    pub fn appearance(&self) -> WindowAppearance {
        self.appearance
    }

    /// Returns the size of the drawable area within the window.
    pub fn viewport_size(&self) -> Size<Pixels> {
        self.viewport_size
    }

    /// Returns whether this window is focused by the operating system (receiving key events).
    pub fn is_window_active(&self) -> bool {
        self.active.get()
    }

    /// Returns whether this window is considered to be the window
    /// that currently owns the mouse cursor.
    /// On mac, this is equivalent to `is_window_active`.
    pub fn is_window_hovered(&self) -> bool {
        if cfg!(any(
            target_os = "windows",
            target_os = "linux",
            target_os = "freebsd"
        )) {
            self.hovered.get()
        } else {
            self.is_window_active()
        }
    }

    /// Returns whether the last input was from the keyboard.
    /// This is useful for determining cursor visibility behavior.
    pub fn last_input_was_keyboard(&self) -> bool {
        self.last_input_was_keyboard.get()
    }

    /// Toggle zoom on the window.
    pub fn zoom_window(&self) {
        self.platform_window.zoom();
    }

    /// Opens the native title bar context menu, useful when implementing client side decorations (Wayland and X11)
    pub fn show_window_menu(&self, position: Point<Pixels>) {
        self.platform_window.show_window_menu(position)
    }

    /// Tells the compositor to take control of window movement (Wayland and X11)
    ///
    /// Events may not be received during a move operation.
    pub fn start_window_move(&self) {
        self.platform_window.start_window_move()
    }

    /// When using client side decorations, set this to the width of the invisible decorations (Wayland and X11)
    pub fn set_client_inset(&mut self, inset: Pixels) {
        self.client_inset = Some(inset);
        self.platform_window.set_client_inset(inset);
    }

    /// Returns the client_inset value by [`Self::set_client_inset`].
    pub fn client_inset(&self) -> Option<Pixels> {
        self.client_inset
    }

    /// Returns whether the title bar window controls need to be rendered by the application (Wayland and X11)
    pub fn window_decorations(&self) -> Decorations {
        self.platform_window.window_decorations()
    }

    /// Returns which window controls are currently visible (Wayland)
    pub fn window_controls(&self) -> WindowControls {
        self.platform_window.window_controls()
    }

    /// Updates the window's title at the platform level.
    pub fn set_window_title(&mut self, title: &str) {
        self.platform_window.set_title(title);
    }

    /// Sets the application identifier.
    pub fn set_app_id(&mut self, app_id: &str) {
        self.platform_window.set_app_id(app_id);
    }

    /// Sets the window background appearance.
    pub fn set_background_appearance(&self, background_appearance: WindowBackgroundAppearance) {
        self.platform_window
            .set_background_appearance(background_appearance);
    }

    /// Mark the window as dirty at the platform level.
    pub fn set_window_edited(&mut self, edited: bool) {
        self.platform_window.set_edited(edited);
    }

    /// Determine the display on which the window is visible.
    pub fn display(&self, cx: &App) -> Option<Rc<dyn PlatformDisplay>> {
        cx.platform
            .displays()
            .into_iter()
            .find(|display| Some(display.id()) == self.display_id)
    }

    /// Show the platform character palette.
    pub fn show_character_palette(&self) {
        self.platform_window.show_character_palette();
    }

    /// The scale factor of the display associated with the window. For example, it could
    /// return 2.0 for a "retina" display, indicating that each logical pixel should actually
    /// be rendered as two pixels on screen.
    pub fn scale_factor(&self) -> f32 {
        self.scale_factor
    }

    /// The size of an em for the base font of the application. Adjusting this value allows the
    /// UI to scale, just like zooming a web page.
    pub fn rem_size(&self) -> Pixels {
        self.rem_size_override_stack
            .last()
            .copied()
            .unwrap_or(self.rem_size)
    }

    /// Sets the size of an em for the base font of the application. Adjusting this value allows the
    /// UI to scale, just like zooming a web page.
    pub fn set_rem_size(&mut self, rem_size: impl Into<Pixels>) {
        self.rem_size = rem_size.into();
    }

    /// Acquire a globally unique identifier for the given ElementId.
    /// Only valid for the duration of the provided closure.
    pub fn with_global_id<R>(
        &mut self,
        element_id: ElementId,
        f: impl FnOnce(&GlobalElementId, &mut Self) -> R,
    ) -> R {
        self.element_id_stack.push(element_id);
        let global_id = GlobalElementId(self.element_id_stack.clone());
        let result = f(&global_id, self);
        self.element_id_stack.pop();
        result
    }

    /// Executes the provided function with the specified rem size.
    ///
    /// This method must only be called as part of element drawing.
    pub fn with_rem_size<F, R>(&mut self, rem_size: Option<impl Into<Pixels>>, f: F) -> R
    where
        F: FnOnce(&mut Self) -> R,
    {
        self.invalidator.debug_assert_paint_or_prepaint();

        if let Some(rem_size) = rem_size {
            self.rem_size_override_stack.push(rem_size.into());
            let result = f(self);
            self.rem_size_override_stack.pop();
            result
        } else {
            f(self)
        }
    }

    /// The line height associated with the current text style.
    pub fn line_height(&self) -> Pixels {
        self.text_style().line_height_in_pixels(self.rem_size())
    }

    /// Rounds a logical value to the nearest device pixel.
    #[inline]
    pub fn pixel_snap(&self, value: Pixels) -> Pixels {
        px(round_to_device_pixel(value.0, self.scale_factor()) / self.scale_factor())
    }

    /// f64 variant of [`Self::pixel_snap`].
    #[inline]
    pub fn pixel_snap_f64(&self, value: f64) -> f64 {
        let scale_factor = f64::from(self.scale_factor());
        round_half_toward_zero_f64(value * scale_factor) / scale_factor
    }

    /// Snaps a bounds' origin and size to the nearest device pixel.
    #[inline]
    pub fn pixel_snap_bounds(&self, bounds: Bounds<Pixels>) -> Bounds<Pixels> {
        bounds.map(|c| self.pixel_snap(c))
    }

    /// Snaps a point's coordinates to the nearest device pixel.
    #[inline]
    pub fn pixel_snap_point(&self, position: Point<Pixels>) -> Point<Pixels> {
        position.map(|c| self.pixel_snap(c))
    }

    #[inline]
    fn snap_bounds(&self, bounds: Bounds<Pixels>) -> Bounds<ScaledPixels> {
        let scale_factor = self.scale_factor();
        let left = round_to_device_pixel(bounds.left().0, scale_factor);
        let top = round_to_device_pixel(bounds.top().0, scale_factor);
        let right = round_to_device_pixel(bounds.right().0, scale_factor).max(left);
        let bottom = round_to_device_pixel(bounds.bottom().0, scale_factor).max(top);
        Bounds::from_corners(
            point(ScaledPixels(left), ScaledPixels(top)),
            point(ScaledPixels(right), ScaledPixels(bottom)),
        )
    }

    #[inline]
    fn snap_stroke(&self, value: Pixels) -> ScaledPixels {
        ScaledPixels(round_stroke_to_device_pixel(value.0, self.scale_factor()))
    }

    #[inline]
    fn snap_border_widths(&self, edges: Edges<Pixels>) -> Edges<ScaledPixels> {
        edges.map(|e| self.snap_stroke(*e))
    }

    #[inline]
    fn cover_bounds(&self, bounds: Bounds<Pixels>) -> Bounds<ScaledPixels> {
        let scale_factor = self.scale_factor();
        let left = floor_to_device_pixel(bounds.left().0, scale_factor);
        let top = floor_to_device_pixel(bounds.top().0, scale_factor);
        let right = ceil_to_device_pixel(bounds.right().0, scale_factor).max(left);
        let bottom = ceil_to_device_pixel(bounds.bottom().0, scale_factor).max(top);
        Bounds::from_corners(
            point(ScaledPixels(left), ScaledPixels(top)),
            point(ScaledPixels(right), ScaledPixels(bottom)),
        )
    }

    #[inline]
    fn snapped_content_mask(&self) -> ContentMask<ScaledPixels> {
        ContentMask {
            bounds: self.cover_bounds(self.content_mask().bounds),
        }
    }

    /// Call to prevent the default action of an event. Currently only used to prevent
    /// parent elements from becoming focused on mouse down.
    pub fn prevent_default(&mut self) {
        self.default_prevented = true;
    }

    /// Obtain whether default has been prevented for the event currently being dispatched.
    pub fn default_prevented(&self) -> bool {
        self.default_prevented
    }

    /// Determine whether the given action is available along the dispatch path to the currently focused element.
    pub fn is_action_available(&self, action: &dyn Action, cx: &mut App) -> bool {
        let node_id =
            self.focus_node_id_in_rendered_frame(self.focused(cx).map(|handle| handle.id));
        self.rendered_frame
            .dispatch_tree
            .is_action_available(action, node_id)
    }

    /// The position of the mouse relative to the window.
    pub fn mouse_position(&self) -> Point<Pixels> {
        self.mouse_position
    }

    /// Captures the pointer for the given hitbox. While captured, all mouse move and mouse up
    /// events will be routed to listeners that check this hitbox's `is_hovered` status,
    /// regardless of actual hit testing. This enables drag operations that continue
    /// even when the pointer moves outside the element's bounds.
    ///
    /// The capture is automatically released on mouse up.
    pub fn capture_pointer(&mut self, hitbox_id: HitboxId) {
        self.captured_hitbox = Some(hitbox_id);
    }

    /// Releases any active pointer capture.
    pub fn release_pointer(&mut self) {
        self.captured_hitbox = None;
    }

    /// Returns the hitbox that has captured the pointer, if any.
    pub fn captured_hitbox(&self) -> Option<HitboxId> {
        self.captured_hitbox
    }

    /// The current state of the keyboard's modifiers
    pub fn modifiers(&self) -> Modifiers {
        self.modifiers
    }

    /// The current state of the keyboard's capslock
    pub fn capslock(&self) -> Capslock {
        self.capslock
    }

    /// Register the given handler to be invoked whenever the global of the given type
    /// is updated.
    pub fn observe_global<G: Global>(
        &mut self,
        cx: &mut App,
        f: impl Fn(&mut Window, &mut App) + 'static,
    ) -> Subscription {
        let window_handle = self.handle;
        let (subscription, activate) = cx.global_observers.insert(
            TypeId::of::<G>(),
            Box::new(move |cx| {
                window_handle
                    .update(cx, |_, window, cx| f(window, cx))
                    .is_ok()
            }),
        );
        cx.defer(move |_| activate());
        subscription
    }

    /// Focus the current window and bring it to the foreground at the platform level.
    pub fn activate_window(&self) {
        self.platform_window.activate();
    }

    /// Requests that the operating system draw attention to this window.
    pub fn request_attention(&self) {
        self.platform_window.request_attention();
    }

    /// Minimize the current window at the platform level.
    pub fn minimize_window(&self) {
        self.platform_window.minimize();
    }

    /// Show the current window at the platform level.
    pub fn show_window(&self) {
        self.platform_window.show();
    }

    /// Hide the current window at the platform level.
    pub fn hide_window(&self) {
        self.platform_window.hide();
    }

    /// Returns whether the current window is visible at the platform level.
    pub fn is_window_visible(&self) -> bool {
        self.platform_window.is_visible()
    }

    /// Set whether mouse events should pass through the current window.
    pub fn set_mouse_passthrough(&self, passthrough: bool) {
        self.platform_window.set_mouse_passthrough(passthrough);
    }

    /// Linux (wayland) only: Set the window's input region, the area that receives pointer
    /// and touch input. Events outside it pass through to whatever is below the window.
    ///
    /// - `Some(rects)` restricts input to the union of `rects`, in window coordinates.
    /// - `Some(&[])` is an empty region, so the window receives no pointer or touch input.
    /// - `None` resets the region to the default, so the whole window receives input again.
    pub fn set_input_region(&self, region: Option<&[Bounds<Pixels>]>) {
        self.platform_window.set_input_region(region);
    }

    /// Toggle full screen status on the current window at the platform level.
    pub fn toggle_fullscreen(&self) {
        self.platform_window.toggle_fullscreen();
    }

    /// Toggle simple (borderless) fullscreen, where the window covers the entire
    /// screen including the menu bar and, on notched displays, the area around the
    /// notch. Unlike [`Window::toggle_fullscreen`], this does not move the window
    /// into its own Mission Control space. Only has an effect on macOS.
    pub fn toggle_simple_fullscreen(&self) {
        self.platform_window.toggle_simple_fullscreen();
    }

    /// Set the progress bar state for this window's taskbar/dock representation.
    pub fn set_progress_bar(&self, state: ProgressBarState) {
        self.platform_window.set_progress_bar(state);
    }

    /// Capture the current window state for save/restore.
    pub fn window_state(&self) -> WindowState {
        let bounds = self.platform_window.window_bounds();
        let display_id = self.platform_window.display().map(|d| d.id());
        let fullscreen = self.platform_window.is_fullscreen();
        WindowState {
            bounds,
            display_id,
            fullscreen,
        }
    }

    /// Restore a previously captured window state.
    ///
    /// This restores the fullscreen state. Window bounds are set at creation
    /// time via `WindowOptions`/`WindowParams`, so this primarily handles
    /// toggling fullscreen to match the saved state.
    pub fn restore_window_state(&self, state: &WindowState) {
        let is_fullscreen = self.platform_window.is_fullscreen();
        if state.fullscreen != is_fullscreen {
            self.platform_window.toggle_fullscreen();
        }
    }

    /// Updates the IME panel position suggestions for languages like japanese, chinese.
    pub fn invalidate_character_coordinates(&self) {
        self.on_next_frame(|window, cx| {
            if let Some(mut input_handler) = window.platform_window.take_input_handler() {
                if let Some(bounds) = input_handler.selected_bounds(window, cx) {
                    window.platform_window.update_ime_position(bounds);
                }
                window.platform_window.set_input_handler(input_handler);
            }
        });
    }

    /// Present a platform dialog.
    /// The provided message will be presented, along with buttons for each answer.
    /// When a button is clicked, the returned Receiver will receive the index of the clicked button.
    pub fn prompt<T>(
        &mut self,
        level: PromptLevel,
        message: &str,
        detail: Option<&str>,
        answers: &[T],
        cx: &mut App,
    ) -> oneshot::Receiver<usize>
    where
        T: Clone + Into<PromptButton>,
    {
        let prompt_builder = cx.prompt_builder.take();
        let Some(prompt_builder) = prompt_builder else {
            unreachable!("Re-entrant window prompting is not supported by GPUI");
        };

        let answers = answers
            .iter()
            .map(|answer| answer.clone().into())
            .collect::<Vec<_>>();

        let receiver = match &prompt_builder {
            PromptBuilder::Default => self
                .platform_window
                .prompt(level, message, detail, &answers)
                .unwrap_or_else(|| {
                    self.build_custom_prompt(&prompt_builder, level, message, detail, &answers, cx)
                }),
            PromptBuilder::Custom(_) => {
                self.build_custom_prompt(&prompt_builder, level, message, detail, &answers, cx)
            }
        };

        cx.prompt_builder = Some(prompt_builder);

        receiver
    }

    fn build_custom_prompt(
        &mut self,
        prompt_builder: &PromptBuilder,
        level: PromptLevel,
        message: &str,
        detail: Option<&str>,
        answers: &[PromptButton],
        cx: &mut App,
    ) -> oneshot::Receiver<usize> {
        let (sender, receiver) = oneshot::channel();
        let handle = PromptHandle::new(sender);
        let handle = (prompt_builder)(level, message, detail, answers, handle, self, cx);
        self.prompt = Some(handle);
        receiver
    }

    /// Returns whether a GPUI-rendered prompt is currently active in this window.
    ///
    /// Platform-native prompt dialogs are not represented by this state.
    pub fn has_active_prompt(&self) -> bool {
        self.prompt.is_some()
    }

    /// Returns the current context stack.
    pub fn context_stack(&self) -> Vec<KeyContext> {
        let node_id = self.focus_node_id_in_rendered_frame(self.focus);
        let dispatch_tree = &self.rendered_frame.dispatch_tree;
        dispatch_tree
            .dispatch_path(node_id)
            .iter()
            .filter_map(move |&node_id| dispatch_tree.node(node_id).context.clone())
            .collect()
    }

    /// Returns all available actions for the focused element.
    pub fn available_actions(&self, cx: &App) -> Vec<Box<dyn Action>> {
        let node_id = self.focus_node_id_in_rendered_frame(self.focus);
        let mut actions = self.rendered_frame.dispatch_tree.available_actions(node_id);
        for action_type in cx.global_action_listeners.keys() {
            if let Err(ix) = actions.binary_search_by_key(action_type, |a| a.as_any().type_id()) {
                let action = cx.actions.build_action_type(action_type).ok();
                if let Some(action) = action {
                    actions.insert(ix, action);
                }
            }
        }
        actions
    }

    /// Returns key bindings that invoke an action on the currently focused element. Bindings are
    /// returned in the order they were added. For display, the last binding should take precedence.
    pub fn bindings_for_action(&self, action: &dyn Action) -> Vec<KeyBinding> {
        self.rendered_frame
            .dispatch_tree
            .bindings_for_action(action, &self.rendered_frame.dispatch_tree.context_stack)
    }

    /// Returns the highest precedence key binding that invokes an action on the currently focused
    /// element. This is more efficient than getting the last result of `bindings_for_action`.
    pub fn highest_precedence_binding_for_action(&self, action: &dyn Action) -> Option<KeyBinding> {
        self.rendered_frame
            .dispatch_tree
            .highest_precedence_binding_for_action(
                action,
                &self.rendered_frame.dispatch_tree.context_stack,
            )
    }

    /// Returns the key bindings for an action in a context.
    pub fn bindings_for_action_in_context(
        &self,
        action: &dyn Action,
        context: KeyContext,
    ) -> Vec<KeyBinding> {
        let dispatch_tree = &self.rendered_frame.dispatch_tree;
        dispatch_tree.bindings_for_action(action, &[context])
    }

    /// Returns the highest precedence key binding for an action in a context. This is more
    /// efficient than getting the last result of `bindings_for_action_in_context`.
    pub fn highest_precedence_binding_for_action_in_context(
        &self,
        action: &dyn Action,
        context: KeyContext,
    ) -> Option<KeyBinding> {
        let dispatch_tree = &self.rendered_frame.dispatch_tree;
        dispatch_tree.highest_precedence_binding_for_action(action, &[context])
    }

    /// Returns any bindings that would invoke an action on the given focus handle if it were
    /// focused. Bindings are returned in the order they were added. For display, the last binding
    /// should take precedence.
    pub fn bindings_for_action_in(
        &self,
        action: &dyn Action,
        focus_handle: &FocusHandle,
    ) -> Vec<KeyBinding> {
        let dispatch_tree = &self.rendered_frame.dispatch_tree;
        let Some(context_stack) = self.context_stack_for_focus_handle(focus_handle) else {
            return vec![];
        };
        dispatch_tree.bindings_for_action(action, &context_stack)
    }

    /// Returns the highest precedence key binding that would invoke an action on the given focus
    /// handle if it were focused. This is more efficient than getting the last result of
    /// `bindings_for_action_in`.
    pub fn highest_precedence_binding_for_action_in(
        &self,
        action: &dyn Action,
        focus_handle: &FocusHandle,
    ) -> Option<KeyBinding> {
        let dispatch_tree = &self.rendered_frame.dispatch_tree;
        let context_stack = self.context_stack_for_focus_handle(focus_handle)?;
        dispatch_tree.highest_precedence_binding_for_action(action, &context_stack)
    }

    fn context_stack_for_focus_handle(
        &self,
        focus_handle: &FocusHandle,
    ) -> Option<Vec<KeyContext>> {
        let dispatch_tree = &self.rendered_frame.dispatch_tree;
        let node_id = dispatch_tree.focusable_node_id(focus_handle.id)?;
        let context_stack: Vec<_> = dispatch_tree
            .dispatch_path(node_id)
            .into_iter()
            .filter_map(|node_id| dispatch_tree.node(node_id).context.clone())
            .collect();
        Some(context_stack)
    }

    /// Returns a generic event listener that invokes the given listener with the view and context associated with the given view handle.
    pub fn listener_for<V: Render, E>(
        &self,
        view: &Entity<V>,
        f: impl Fn(&mut V, &E, &mut Window, &mut Context<V>) + 'static,
    ) -> impl Fn(&E, &mut Window, &mut App) + 'static {
        let view = view.downgrade();
        move |e: &E, window: &mut Window, cx: &mut App| {
            view.update(cx, |view, cx| f(view, e, window, cx)).ok();
        }
    }

    /// Returns a generic handler that invokes the given handler with the view and context associated with the given view handle.
    pub fn handler_for<V: Render, Callback: Fn(&mut V, &mut Window, &mut Context<V>) + 'static>(
        &self,
        view: &Entity<V>,
        f: Callback,
    ) -> impl Fn(&mut Window, &mut App) + use<V, Callback> {
        let view = view.downgrade();
        move |window: &mut Window, cx: &mut App| {
            view.update(cx, |view, cx| f(view, window, cx)).ok();
        }
    }

    /// Register a callback that can interrupt the closing of the current window based the returned boolean.
    /// If the callback returns false, the window won't be closed.
    pub fn on_window_should_close(
        &self,
        cx: &App,
        f: impl Fn(&mut Window, &mut App) -> bool + 'static,
    ) {
        let mut cx = self.to_async(cx);
        self.platform_window.on_should_close(Box::new(move || {
            cx.update(|window, cx| f(window, cx)).unwrap_or(true)
        }))
    }

    /// Register an action listener on the window for the next frame. The type of action
    /// is determined by the first parameter of the given listener. When the next frame is rendered
    /// the listener will be cleared.
    ///
    /// This is a fairly low-level method, so prefer using action handlers on elements unless you have
    /// a specific need to register a global listener.
    pub fn on_action(
        &mut self,
        action_type: TypeId,
        listener: impl Fn(&dyn Any, DispatchPhase, &mut Window, &mut App) + 'static,
    ) {
        self.next_frame
            .dispatch_tree
            .on_action(action_type, Rc::new(listener));
    }

    /// Register an action listener on the window for the next frame if the condition is true.
    /// The type of action is determined by the first parameter of the given listener.
    /// When the next frame is rendered the listener will be cleared.
    ///
    /// This is a fairly low-level method, so prefer using action handlers on elements unless you have
    /// a specific need to register a global listener.
    pub fn on_action_when(
        &mut self,
        condition: bool,
        action_type: TypeId,
        listener: impl Fn(&dyn Any, DispatchPhase, &mut Window, &mut App) + 'static,
    ) {
        if condition {
            self.next_frame
                .dispatch_tree
                .on_action(action_type, Rc::new(listener));
        }
    }

    /// Read information about the GPU backing this window.
    /// Currently returns None on Mac and Windows.
    pub fn gpu_specs(&self) -> Option<GpuSpecs> {
        self.platform_window.gpu_specs()
    }

    /// Perform titlebar double-click action.
    /// This is macOS specific.
    pub fn titlebar_double_click(&self) {
        self.platform_window.titlebar_double_click();
    }

    /// Gets the window's title at the platform level.
    /// This is macOS specific.
    pub fn window_title(&self) -> String {
        self.platform_window.get_title()
    }

    /// Returns a list of all tabbed windows and their titles.
    /// This is macOS specific.
    pub fn tabbed_windows(&self) -> Option<Vec<SystemWindowTab>> {
        self.platform_window.tabbed_windows()
    }

    /// Returns the tab bar visibility.
    /// This is macOS specific.
    pub fn tab_bar_visible(&self) -> bool {
        self.platform_window.tab_bar_visible()
    }

    /// Merges all open windows into a single tabbed window.
    /// This is macOS specific.
    pub fn merge_all_windows(&self) {
        self.platform_window.merge_all_windows()
    }

    /// Moves the tab to a new containing window.
    /// This is macOS specific.
    pub fn move_tab_to_new_window(&self) {
        self.platform_window.move_tab_to_new_window()
    }

    /// Shows or hides the window tab overview.
    /// This is macOS specific.
    pub fn toggle_window_tab_overview(&self) {
        self.platform_window.toggle_window_tab_overview()
    }

    /// Sets the tabbing identifier for the window.
    /// This is macOS specific.
    pub fn set_tabbing_identifier(&self, tabbing_identifier: Option<String>) {
        self.platform_window
            .set_tabbing_identifier(tabbing_identifier)
    }

    /// Request the OS to play an alert sound.
    pub fn play_system_bell(&self) {
        self.platform_window.play_system_bell()
    }

    /// Returns whether accessibility support is enabled for this window.
    ///
    /// This is false when the app was created with [`crate::Application::inaccessible`].
    /// The result does not depend on whether assistive technology is currently
    /// connected, so it can be used to gate subscriptions that are only needed
    /// for accessibility.
    pub fn is_a11y_enabled(&self) -> bool {
        self.a11y.is_enabled()
    }

    /// Returns a JSON representation of the latest completed accessibility frame.
    ///
    /// Returns `None` until accessibility has produced a tree for this window.
    pub fn debug_a11y_tree_json(&self) -> Option<String> {
        self.a11y.debug_tree_json()
    }

    /// Register a listener for an accessibility action on a specific node.
    /// The listener is called when assistive technology requests the given
    /// action on the node identified by `node_id`.
    pub fn on_a11y_action(
        &mut self,
        node_id: accesskit::NodeId,
        action: accesskit::Action,
        listener: impl FnMut(Option<&accesskit::ActionData>, &mut Window, &mut App) + 'static,
    ) {
        self.a11y
            .action_listeners
            .entry(node_id)
            .or_default()
            .push((action, Box::new(listener)));
    }

    #[cfg(not(target_family = "wasm"))]
    pub(crate) fn handle_a11y_action(&mut self, request: accesskit::ActionRequest, cx: &mut App) {
        if let Some(mut listeners) = self.a11y.action_listeners.remove(&request.target_node) {
            let action_data = request.data.as_ref();
            let mut matched = false;
            for (action, listener) in &mut listeners {
                if *action == request.action {
                    listener(action_data, self, cx);
                    matched = true;
                }
            }
            self.a11y
                .action_listeners
                .insert(request.target_node, listeners);
            if matched {
                return;
            }
        }

        match request.action {
            accesskit::Action::Click => {
                if let Some(bounds) = self.a11y.node_bounds.get(&request.target_node).copied() {
                    let center = bounds.center();
                    self.dispatch_event(
                        PlatformInput::MouseDown(crate::MouseDownEvent {
                            button: MouseButton::Left,
                            position: center,
                            modifiers: Modifiers::default(),
                            click_count: 1,
                            first_mouse: false,
                        }),
                        cx,
                    );
                    self.dispatch_event(
                        PlatformInput::MouseUp(MouseUpEvent {
                            button: MouseButton::Left,
                            position: center,
                            modifiers: Modifiers::default(),
                            click_count: 1,
                        }),
                        cx,
                    );
                }
            }
            accesskit::Action::Focus => {
                if let Some(focus_id) = self.a11y.focus_ids.get(&request.target_node).copied()
                    && let Some(handle) = FocusHandle::for_id(focus_id, &cx.focus_handles)
                {
                    self.focus(&handle);
                }
            }
            accesskit::Action::Blur => self.blur(cx),
            _ => {
                log::debug!(
                    "Unhandled a11y action: {:?} on {:?}",
                    request.action,
                    request.target_node
                );
            }
        }
    }

    /// Toggles the inspector mode on this window.
    #[cfg(any(feature = "inspector", debug_assertions))]
    pub fn toggle_inspector(&mut self, cx: &mut App) {
        self.inspector = match self.inspector {
            None => Some(cx.new(|_| Inspector::new())),
            Some(_) => None,
        };
        self.refresh();
    }

    /// Returns true if the window is in inspector mode.
    pub fn is_inspector_picking(&self, _cx: &App) -> bool {
        #[cfg(any(feature = "inspector", debug_assertions))]
        {
            if let Some(inspector) = &self.inspector {
                return inspector.read(_cx).is_picking();
            }
        }
        false
    }

    /// Executes the provided function with mutable access to an inspector state.
    #[cfg(any(feature = "inspector", debug_assertions))]
    pub fn with_inspector_state<T: 'static, R>(
        &mut self,
        _inspector_id: Option<&crate::InspectorElementId>,
        cx: &mut App,
        f: impl FnOnce(&mut Option<T>, &mut Self) -> R,
    ) -> R {
        if let Some(inspector_id) = _inspector_id
            && let Some(inspector) = &self.inspector
        {
            let inspector = inspector.clone();
            let active_element_id = inspector.read(cx).active_element_id();
            if Some(inspector_id) == active_element_id {
                return inspector.update(cx, |inspector, _cx| {
                    inspector.with_active_element_state(self, f)
                });
            }
        }
        f(&mut None, self)
    }

    #[cfg(any(feature = "inspector", debug_assertions))]
    pub(crate) fn build_inspector_element_id(
        &mut self,
        path: crate::InspectorElementPath,
    ) -> crate::InspectorElementId {
        self.invalidator.debug_assert_paint_or_prepaint();
        let path = Rc::new(path);
        let next_instance_id = self
            .next_frame
            .next_inspector_instance_ids
            .entry(path.clone())
            .or_insert(0);
        let instance_id = *next_instance_id;
        *next_instance_id += 1;
        crate::InspectorElementId { path, instance_id }
    }

    #[cfg(any(feature = "inspector", debug_assertions))]
    fn prepaint_inspector(&mut self, inspector_width: Pixels, cx: &mut App) -> Option<AnyElement> {
        if let Some(inspector) = self.inspector.take() {
            let mut inspector_element = AnyView::from(inspector.clone()).into_any_element();
            inspector_element.prepaint_as_root(
                point(self.viewport_size.width - inspector_width, px(0.0)),
                size(inspector_width, self.viewport_size.height).into(),
                self,
                cx,
            );
            self.inspector = Some(inspector);
            Some(inspector_element)
        } else {
            None
        }
    }

    #[cfg(any(feature = "inspector", debug_assertions))]
    fn paint_inspector(&mut self, mut inspector_element: Option<AnyElement>, cx: &mut App) {
        if let Some(mut inspector_element) = inspector_element {
            inspector_element.paint(self, cx);
        };
    }

    /// Registers a hitbox that can be used for inspector picking mode, allowing users to select and
    /// inspect UI elements by clicking on them.
    #[cfg(any(feature = "inspector", debug_assertions))]
    pub fn insert_inspector_hitbox(
        &mut self,
        hitbox_id: HitboxId,
        inspector_id: Option<&crate::InspectorElementId>,
        cx: &App,
    ) {
        self.invalidator.debug_assert_paint_or_prepaint();
        if !self.is_inspector_picking(cx) {
            return;
        }
        if let Some(inspector_id) = inspector_id {
            self.next_frame
                .inspector_hitboxes
                .insert(hitbox_id, inspector_id.clone());
        }
    }

    #[cfg(any(feature = "inspector", debug_assertions))]
    fn paint_inspector_hitbox(&mut self, cx: &App) {
        if let Some(inspector) = self.inspector.as_ref() {
            let inspector = inspector.read(cx);
            if let Some((hitbox_id, _)) = self.hovered_inspector_hitbox(inspector, &self.next_frame)
                && let Some(hitbox) = self
                    .next_frame
                    .hitboxes
                    .iter()
                    .find(|hitbox| hitbox.id == hitbox_id)
            {
                self.paint_quad(crate::fill(hitbox.bounds, crate::rgba(0x61afef4d)));
            }
        }
    }

    #[cfg(any(feature = "inspector", debug_assertions))]
    fn handle_inspector_mouse_event(&mut self, event: &dyn Any, cx: &mut App) {
        let Some(inspector) = self.inspector.clone() else {
            return;
        };
        if event.downcast_ref::<MouseMoveEvent>().is_some() {
            inspector.update(cx, |inspector, _cx| {
                if let Some((_, inspector_id)) =
                    self.hovered_inspector_hitbox(inspector, &self.rendered_frame)
                {
                    inspector.hover(inspector_id, self);
                }
            });
        } else if event.downcast_ref::<crate::MouseDownEvent>().is_some() {
            inspector.update(cx, |inspector, _cx| {
                if let Some((_, inspector_id)) =
                    self.hovered_inspector_hitbox(inspector, &self.rendered_frame)
                {
                    inspector.select(inspector_id, self);
                }
            });
        } else if let Some(event) = event.downcast_ref::<crate::ScrollWheelEvent>() {
            // This should be kept in sync with SCROLL_LINES in x11 platform.
            const SCROLL_LINES: f32 = 3.0;
            const SCROLL_PIXELS_PER_LAYER: f32 = 36.0;
            let delta_y = event
                .delta
                .pixel_delta(px(SCROLL_PIXELS_PER_LAYER / SCROLL_LINES))
                .y;
            if let Some(inspector) = self.inspector.clone() {
                inspector.update(cx, |inspector, _cx| {
                    if let Some(depth) = inspector.pick_depth.as_mut() {
                        *depth += f32::from(delta_y) / SCROLL_PIXELS_PER_LAYER;
                        let max_depth = self.mouse_hit_test.ids.len() as f32 - 0.5;
                        if *depth < 0.0 {
                            *depth = 0.0;
                        } else if *depth > max_depth {
                            *depth = max_depth;
                        }
                        if let Some((_, inspector_id)) =
                            self.hovered_inspector_hitbox(inspector, &self.rendered_frame)
                        {
                            inspector.set_active_element_id(inspector_id, self);
                        }
                    }
                });
            }
        }
    }

    #[cfg(any(feature = "inspector", debug_assertions))]
    fn hovered_inspector_hitbox(
        &self,
        inspector: &Inspector,
        frame: &Frame,
    ) -> Option<(HitboxId, crate::InspectorElementId)> {
        if let Some(pick_depth) = inspector.pick_depth {
            let depth = (pick_depth as i64).try_into().unwrap_or(0);
            let max_skipped = self.mouse_hit_test.ids.len().saturating_sub(1);
            let skip_count = (depth as usize).min(max_skipped);
            for hitbox_id in self.mouse_hit_test.ids.iter().skip(skip_count) {
                if let Some(inspector_id) = frame.inspector_hitboxes.get(hitbox_id) {
                    return Some((*hitbox_id, inspector_id.clone()));
                }
            }
        }
        None
    }

    /// For testing: set the current modifier keys state.
    /// This does not generate any events.
    #[cfg(any(test, feature = "test-support"))]
    pub fn set_modifiers(&mut self, modifiers: Modifiers) {
        self.modifiers = modifiers;
    }
}

// #[derive(Clone, Copy, Eq, PartialEq, Hash)]
slotmap::new_key_type! {
    /// A unique identifier for a window.
    pub struct WindowId;
}

impl WindowId {
    /// Converts this window ID to a `u64`.
    pub fn as_u64(&self) -> u64 {
        self.0.as_ffi()
    }
}

impl From<u64> for WindowId {
    fn from(value: u64) -> Self {
        WindowId(slotmap::KeyData::from_ffi(value))
    }
}

/// A handle to a window with a specific root view type.
/// Note that this does not keep the window alive on its own.
#[derive(Deref, DerefMut)]
pub struct WindowHandle<V> {
    #[deref]
    #[deref_mut]
    pub(crate) any_handle: AnyWindowHandle,
    state_type: PhantomData<V>,
}

impl<V> Debug for WindowHandle<V> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WindowHandle")
            .field("any_handle", &self.any_handle.id.as_u64())
            .finish()
    }
}

impl<V: 'static + Render> WindowHandle<V> {
    /// Creates a new handle from a window ID.
    /// This does not check if the root type of the window is `V`.
    pub fn new(id: WindowId) -> Self {
        WindowHandle {
            any_handle: AnyWindowHandle {
                id,
                state_type: TypeId::of::<V>(),
            },
            state_type: PhantomData,
        }
    }

    /// Get the root view out of this window.
    ///
    /// This will fail if the window is closed or if the root view's type does not match `V`.
    #[cfg(any(test, feature = "test-support"))]
    pub fn root<C>(&self, cx: &mut C) -> Result<Entity<V>>
    where
        C: AppContext,
    {
        crate::Flatten::flatten(cx.update_window(self.any_handle, |root_view, _, _| {
            root_view
                .downcast::<V>()
                .map_err(|_| anyhow!("the type of the window's root view has changed"))
        }))
    }

    /// Updates the root view of this window.
    ///
    /// This will fail if the window has been closed or if the root view's type does not match
    pub fn update<C, R>(
        &self,
        cx: &mut C,
        update: impl FnOnce(&mut V, &mut Window, &mut Context<V>) -> R,
    ) -> Result<R>
    where
        C: AppContext,
    {
        cx.update_window(self.any_handle, |root_view, window, cx| {
            let view = root_view
                .downcast::<V>()
                .map_err(|_| anyhow!("the type of the window's root view has changed"))?;

            Ok(view.update(cx, |view, cx| update(view, window, cx)))
        })?
    }

    /// Read the root view out of this window.
    ///
    /// This will fail if the window is closed or if the root view's type does not match `V`.
    pub fn read<'a>(&self, cx: &'a App) -> Result<&'a V> {
        let x = cx
            .windows
            .get(self.id)
            .and_then(|window| {
                window
                    .as_ref()
                    .and_then(|window| window.root.clone())
                    .map(|root_view| root_view.downcast::<V>())
            })
            .context("window not found")?
            .map_err(|_| anyhow!("the type of the window's root view has changed"))?;

        Ok(x.read(cx))
    }

    /// Read the root view out of this window, with a callback
    ///
    /// This will fail if the window is closed or if the root view's type does not match `V`.
    pub fn read_with<C, R>(&self, cx: &C, read_with: impl FnOnce(&V, &App) -> R) -> Result<R>
    where
        C: AppContext,
    {
        cx.read_window(self, |root_view, cx| read_with(root_view.read(cx), cx))
    }

    /// Read the root view pointer off of this window.
    ///
    /// This will fail if the window is closed or if the root view's type does not match `V`.
    pub fn entity<C>(&self, cx: &C) -> Result<Entity<V>>
    where
        C: AppContext,
    {
        cx.read_window(self, |root_view, _cx| root_view)
    }

    /// Check if this window is 'active'.
    ///
    /// Will return `None` if the window is closed or currently
    /// borrowed.
    pub fn is_active(&self, cx: &mut App) -> Option<bool> {
        cx.update_window(self.any_handle, |_, window, _| window.is_window_active())
            .ok()
    }
}

impl<V> Copy for WindowHandle<V> {}

impl<V> Clone for WindowHandle<V> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<V> PartialEq for WindowHandle<V> {
    fn eq(&self, other: &Self) -> bool {
        self.any_handle == other.any_handle
    }
}

impl<V> Eq for WindowHandle<V> {}

impl<V> Hash for WindowHandle<V> {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.any_handle.hash(state);
    }
}

impl<V: 'static> From<WindowHandle<V>> for AnyWindowHandle {
    fn from(val: WindowHandle<V>) -> Self {
        val.any_handle
    }
}

unsafe impl<V> Send for WindowHandle<V> {}
unsafe impl<V> Sync for WindowHandle<V> {}

/// A handle to a window with any root view type, which can be downcast to a window with a specific root view type.
#[derive(Copy, Clone, PartialEq, Eq, Hash)]
pub struct AnyWindowHandle {
    pub(crate) id: WindowId,
    state_type: TypeId,
}

impl AnyWindowHandle {
    /// Get the ID of this window.
    pub fn window_id(&self) -> WindowId {
        self.id
    }

    /// Attempt to convert this handle to a window handle with a specific root view type.
    /// If the types do not match, this will return `None`.
    pub fn downcast<T: 'static>(&self) -> Option<WindowHandle<T>> {
        if TypeId::of::<T>() == self.state_type {
            Some(WindowHandle {
                any_handle: *self,
                state_type: PhantomData,
            })
        } else {
            None
        }
    }

    /// Updates the state of the root view of this window.
    ///
    /// This will fail if the window has been closed.
    pub fn update<C, R>(
        self,
        cx: &mut C,
        update: impl FnOnce(AnyView, &mut Window, &mut App) -> R,
    ) -> Result<R>
    where
        C: AppContext,
    {
        cx.update_window(self, update)
    }

    /// Read the state of the root view of this window.
    ///
    /// This will fail if the window has been closed.
    pub fn read<T, C, R>(self, cx: &C, read: impl FnOnce(Entity<T>, &App) -> R) -> Result<R>
    where
        C: AppContext,
        T: 'static,
    {
        let view = self
            .downcast::<T>()
            .context("the type of the window's root view has changed")?;

        cx.read_window(&view, read)
    }
}

impl HasWindowHandle for Window {
    fn window_handle(&self) -> Result<raw_window_handle::WindowHandle<'_>, HandleError> {
        self.platform_window.window_handle()
    }
}

impl HasDisplayHandle for Window {
    fn display_handle(
        &self,
    ) -> std::result::Result<raw_window_handle::DisplayHandle<'_>, HandleError> {
        self.platform_window.display_handle()
    }
}

/// An identifier for an [`Element`].
///
/// Can be constructed with a string, a number, or both, as well
/// as other internal representations.
#[derive(Clone, Debug, Eq, PartialEq, Hash)]
pub enum ElementId {
    /// The ID of a View element
    View(EntityId),
    /// An integer ID.
    Integer(u64),
    /// A string based ID.
    Name(SharedString),
    /// A UUID.
    Uuid(Uuid),
    /// An ID that's equated with a focus handle.
    FocusHandle(FocusId),
    /// A combination of a name and an integer.
    NamedInteger(SharedString, u64),
    /// A path.
    Path(Arc<std::path::Path>),
    /// A code location.
    CodeLocation(core::panic::Location<'static>),
    /// A labeled child of an element.
    NamedChild(Box<ElementId>, SharedString),
    /// A byte array ID.
    OpaqueId([u8; 20]),
}

impl ElementId {
    /// Constructs an `ElementId::NamedInteger` from a name and `usize`.
    pub fn named_usize(name: impl Into<SharedString>, integer: usize) -> ElementId {
        Self::NamedInteger(name.into(), integer as u64)
    }
}

impl Display for ElementId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ElementId::View(entity_id) => write!(f, "view-{}", entity_id)?,
            ElementId::Integer(ix) => write!(f, "{}", ix)?,
            ElementId::Name(name) => write!(f, "{}", name)?,
            ElementId::FocusHandle(_) => write!(f, "FocusHandle")?,
            ElementId::NamedInteger(s, i) => write!(f, "{}-{}", s, i)?,
            ElementId::Uuid(uuid) => write!(f, "{}", uuid)?,
            ElementId::Path(path) => write!(f, "{}", path.display())?,
            ElementId::CodeLocation(location) => write!(f, "{}", location)?,
            ElementId::NamedChild(id, name) => write!(f, "{}-{}", id, name)?,
            ElementId::OpaqueId(opaque_id) => write!(f, "{:x?}", opaque_id)?,
        }

        Ok(())
    }
}

impl TryInto<SharedString> for ElementId {
    type Error = anyhow::Error;

    fn try_into(self) -> anyhow::Result<SharedString> {
        if let ElementId::Name(name) = self {
            Ok(name)
        } else {
            anyhow::bail!("element id is not string")
        }
    }
}

impl From<u64> for ElementId {
    fn from(id: u64) -> Self {
        ElementId::Integer(id)
    }
}

impl From<usize> for ElementId {
    fn from(id: usize) -> Self {
        ElementId::Integer(id as u64)
    }
}

impl From<i32> for ElementId {
    fn from(id: i32) -> Self {
        Self::Integer(id as u64)
    }
}

impl From<SharedString> for ElementId {
    fn from(name: SharedString) -> Self {
        ElementId::Name(name)
    }
}

impl From<String> for ElementId {
    fn from(name: String) -> Self {
        ElementId::Name(name.into())
    }
}

impl From<Arc<str>> for ElementId {
    fn from(name: Arc<str>) -> Self {
        ElementId::Name(name.into())
    }
}

impl From<Arc<std::path::Path>> for ElementId {
    fn from(path: Arc<std::path::Path>) -> Self {
        ElementId::Path(path)
    }
}

impl From<&'static str> for ElementId {
    fn from(name: &'static str) -> Self {
        ElementId::Name(SharedString::new_static(name))
    }
}

impl<'a> From<&'a FocusHandle> for ElementId {
    fn from(handle: &'a FocusHandle) -> Self {
        ElementId::FocusHandle(handle.id)
    }
}

impl From<(&'static str, EntityId)> for ElementId {
    fn from((name, id): (&'static str, EntityId)) -> Self {
        ElementId::NamedInteger(SharedString::new_static(name), id.as_u64())
    }
}

impl From<(&'static str, usize)> for ElementId {
    fn from((name, id): (&'static str, usize)) -> Self {
        ElementId::NamedInteger(SharedString::new_static(name), id as u64)
    }
}

impl From<(SharedString, usize)> for ElementId {
    fn from((name, id): (SharedString, usize)) -> Self {
        ElementId::NamedInteger(name, id as u64)
    }
}

impl From<(&'static str, u64)> for ElementId {
    fn from((name, id): (&'static str, u64)) -> Self {
        ElementId::NamedInteger(SharedString::new_static(name), id)
    }
}

impl From<Uuid> for ElementId {
    fn from(value: Uuid) -> Self {
        Self::Uuid(value)
    }
}

impl From<(&'static str, u32)> for ElementId {
    fn from((name, id): (&'static str, u32)) -> Self {
        ElementId::NamedInteger(SharedString::new_static(name), id.into())
    }
}

impl<T: Into<SharedString>> From<(ElementId, T)> for ElementId {
    fn from((id, name): (ElementId, T)) -> Self {
        ElementId::NamedChild(Box::new(id), name.into())
    }
}

impl From<&'static core::panic::Location<'static>> for ElementId {
    fn from(location: &'static core::panic::Location<'static>) -> Self {
        ElementId::CodeLocation(*location)
    }
}

impl From<[u8; 20]> for ElementId {
    fn from(opaque_id: [u8; 20]) -> Self {
        ElementId::OpaqueId(opaque_id)
    }
}

/// A rectangle to be rendered in the window at the given position and size.
/// Passed as an argument [`Window::paint_quad`].
#[derive(Clone)]
pub struct PaintQuad {
    /// The bounds of the quad within the window.
    pub bounds: Bounds<Pixels>,
    /// The radii of the quad's corners.
    pub corner_radii: Corners<Pixels>,
    /// The background color of the quad.
    pub background: Background,
    /// The widths of the quad's borders.
    pub border_widths: Edges<Pixels>,
    /// The color of the quad's borders.
    pub border_color: Hsla,
    /// The style of the quad's borders.
    pub border_style: BorderStyle,
    /// Whether to use continuous (squircle) corner rounding.
    pub continuous_corners: bool,
    /// The 2D affine transform applied to this quad.
    pub transform: TransformationMatrix,
    /// The blend mode to apply when rendering this quad.
    pub blend_mode: BlendMode,
}

impl PaintQuad {
    /// Sets the corner radii of the quad.
    pub fn corner_radii(self, corner_radii: impl Into<Corners<Pixels>>) -> Self {
        PaintQuad {
            corner_radii: corner_radii.into(),
            ..self
        }
    }

    /// Sets the border widths of the quad.
    pub fn border_widths(self, border_widths: impl Into<Edges<Pixels>>) -> Self {
        PaintQuad {
            border_widths: border_widths.into(),
            ..self
        }
    }

    /// Sets the border color of the quad.
    pub fn border_color(self, border_color: impl Into<Hsla>) -> Self {
        PaintQuad {
            border_color: border_color.into(),
            ..self
        }
    }

    /// Sets the background color of the quad.
    pub fn background(self, background: impl Into<Background>) -> Self {
        PaintQuad {
            background: background.into(),
            ..self
        }
    }
}

/// Creates a quad with the given parameters.
pub fn quad(
    bounds: Bounds<Pixels>,
    corner_radii: impl Into<Corners<Pixels>>,
    background: impl Into<Background>,
    border_widths: impl Into<Edges<Pixels>>,
    border_color: impl Into<Hsla>,
    border_style: BorderStyle,
) -> PaintQuad {
    PaintQuad {
        bounds,
        corner_radii: corner_radii.into(),
        background: background.into(),
        border_widths: border_widths.into(),
        border_color: border_color.into(),
        border_style,
        continuous_corners: false,
        transform: TransformationMatrix::unit(),
        blend_mode: BlendMode::Normal,
    }
}

/// Creates a filled quad with the given bounds and background color.
pub fn fill(bounds: impl Into<Bounds<Pixels>>, background: impl Into<Background>) -> PaintQuad {
    PaintQuad {
        bounds: bounds.into(),
        corner_radii: (0.).into(),
        background: background.into(),
        border_widths: (0.).into(),
        border_color: transparent_black(),
        border_style: BorderStyle::default(),
        continuous_corners: false,
        transform: TransformationMatrix::unit(),
        blend_mode: BlendMode::Normal,
    }
}

/// Creates a rectangle outline with the given bounds, border color, and a 1px border width
pub fn outline(
    bounds: impl Into<Bounds<Pixels>>,
    border_color: impl Into<Hsla>,
    border_style: BorderStyle,
) -> PaintQuad {
    PaintQuad {
        bounds: bounds.into(),
        corner_radii: (0.).into(),
        background: transparent_black().into(),
        border_widths: (1.).into(),
        border_color: border_color.into(),
        border_style,
        continuous_corners: false,
        transform: TransformationMatrix::unit(),
        blend_mode: BlendMode::Normal,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        ContentMask, Context, ExternalPaths, HitboxBehavior, InputEvent as _, QuitMode, Render,
        TestAppContext, WindowOptions, bounds, canvas, div, hsla, point, px, size,
    };
    use std::{
        cell::{Cell, RefCell},
        path::PathBuf,
        rc::Rc,
    };

    #[test]
    fn high_rate_input_avoids_inactive_window_throttling() {
        let default_inactive = Some(Duration::from_micros(33333));
        assert_eq!(
            frame_throttle_interval(true, false, false, false, false, false, default_inactive),
            default_inactive
        );
        assert_eq!(
            frame_throttle_interval(true, false, false, false, true, false, default_inactive),
            None
        );
        assert_eq!(
            frame_throttle_interval(true, false, false, false, true, true, default_inactive),
            Some(Duration::from_micros(16667)),
            "thermal pressure still caps high-rate input"
        );
    }

    #[test]
    fn inactive_frame_interval_is_configurable() {
        let custom = Some(Duration::from_millis(16));
        assert_eq!(
            frame_throttle_interval(true, false, false, false, false, false, custom),
            custom
        );
        assert_eq!(
            frame_throttle_interval(true, false, false, false, false, false, None),
            None,
            "None disables inactive-window throttling"
        );
    }

    #[test]
    fn hovered_at_ignores_pointer_capture() {
        let mut cx = TestAppContext::single();
        let window = cx.add_window(|_, _| EmptyView);
        let window: AnyWindowHandle = window.into();

        cx.update_window(window, |_, window, _| {
            let bounds = bounds(point(px(0.), px(0.)), size(px(10.), px(10.)));
            let hitbox = Hitbox {
                id: HitboxId(1),
                bounds,
                content_mask: ContentMask { bounds },
                behavior: HitboxBehavior::Normal,
            };
            window.rendered_frame.hitboxes.push(hitbox.clone());
            window.capture_pointer(hitbox.id);

            assert!(hitbox.is_hovered(window));
            assert!(hitbox.is_hovered_at(point(px(5.), px(5.)), window));
            assert!(!hitbox.is_hovered_at(point(px(20.), px(20.)), window));
        })
        .unwrap();
    }

    #[test]
    fn hovered_at_respects_content_masks_and_occlusion() {
        let mut cx = TestAppContext::single();
        let window = cx.add_window(|_, _| EmptyView);
        let window: AnyWindowHandle = window.into();

        cx.update_window(window, |_, window, _| {
            let target_bounds = bounds(point(px(0.), px(0.)), size(px(20.), px(20.)));
            let target = Hitbox {
                id: HitboxId(1),
                bounds: target_bounds,
                content_mask: ContentMask {
                    bounds: bounds(point(px(0.), px(0.)), size(px(10.), px(20.))),
                },
                behavior: HitboxBehavior::Normal,
            };
            let blocker = Hitbox {
                id: HitboxId(2),
                bounds: target_bounds,
                content_mask: ContentMask {
                    bounds: target_bounds,
                },
                behavior: HitboxBehavior::BlockMouse,
            };
            window.rendered_frame.hitboxes.push(target.clone());
            assert!(target.is_hovered_at(point(px(5.), px(5.)), window));
            assert!(!target.is_hovered_at(point(px(15.), px(5.)), window));

            window.rendered_frame.hitboxes.push(blocker.clone());

            assert!(!target.is_hovered_at(point(px(5.), px(5.)), window));
            assert!(blocker.is_hovered_at(point(px(15.), px(5.)), window));
        })
        .unwrap();
    }

    #[test]
    fn hovered_at_ignores_keyboard_input_modality() {
        let mut cx = TestAppContext::single();
        let window = cx.add_window(|_, _| EmptyView);
        let window: AnyWindowHandle = window.into();

        cx.update_window(window, |_, window, _| {
            let bounds = bounds(point(px(0.), px(0.)), size(px(10.), px(10.)));
            let hitbox = Hitbox {
                id: HitboxId(1),
                bounds,
                content_mask: ContentMask { bounds },
                behavior: HitboxBehavior::Normal,
            };
            window.rendered_frame.hitboxes.push(hitbox.clone());
            window.last_input_was_keyboard.set(true);

            assert!(!hitbox.is_hovered(window));
            assert!(hitbox.is_hovered_at(point(px(5.), px(5.)), window));
        })
        .unwrap();
    }

    /// A re-entrant frame request must be deferred while a draw is on the
    /// stack. `draw_in_progress` is the guard the request-frame callback uses
    /// before touching the already-borrowed App.
    #[test]
    fn draw_in_progress_tracks_element_arena_scope() {
        assert!(!draw_in_progress());

        let scope = ElementArenaScope::enter();
        assert!(draw_in_progress());

        let nested = ElementArenaScope::enter();
        assert!(draw_in_progress());
        drop(nested);
        assert!(draw_in_progress(), "outer draw is still in progress");

        drop(scope);
        assert!(!draw_in_progress());
    }

    struct EmptyView;

    impl Render for EmptyView {
        fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
            div()
        }
    }

    struct FileDropExitView(Rc<Cell<usize>>);

    impl Render for FileDropExitView {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            div().size_full().on_file_drop_exit({
                let observed_file_drop_exit = self.0.clone();
                move |_, _, _| observed_file_drop_exit.set(observed_file_drop_exit.get() + 1)
            })
        }
    }

    /// Inbound platform file-drag leave is window-local. Exiting one window must not
    /// notify another window's `on_file_drop_exit` listener, and keyboard input in the
    /// destination window must not suppress the leave callback.
    #[test]
    fn file_drop_exit_is_window_local() {
        let mut cx = TestAppContext::single();
        let first_destination_exit_count = Rc::new(Cell::new(0));
        let first_destination: AnyWindowHandle = cx
            .add_window({
                let first_destination_exit_count = first_destination_exit_count.clone();
                move |_, _| FileDropExitView(first_destination_exit_count)
            })
            .into();
        let second_destination_exit_count = Rc::new(Cell::new(0));
        let second_destination: AnyWindowHandle = cx
            .add_window({
                let second_destination_exit_count = second_destination_exit_count.clone();
                move |_, _| FileDropExitView(second_destination_exit_count)
            })
            .into();
        let reentry_position = point(px(30.), px(30.));
        let external_paths =
            || ExternalPaths([PathBuf::from("/tmp/dropped.txt")].into_iter().collect());

        let update_result = cx.update_window(first_destination, |_, window, cx| {
            window.draw(cx).clear();
            window.dispatch_event(
                FileDropEvent::Entered {
                    position: reentry_position,
                    paths: external_paths(),
                }
                .to_platform_input(),
                cx,
            );
            window.dispatch_event(FileDropEvent::Exited.to_platform_input(), cx);
            assert!(cx.active_drag.is_none());
            assert_eq!(first_destination_exit_count.get(), 1);
            assert_eq!(second_destination_exit_count.get(), 0);
        });
        assert!(
            update_result.is_ok(),
            "failed to handle drag in first destination window: {update_result:?}"
        );

        let update_result = cx.update_window(second_destination, |_, window, cx| {
            window.draw(cx).clear();
            window.dispatch_event(
                PlatformInput::KeyDown(KeyDownEvent {
                    keystroke: Keystroke::parse("down").expect("valid keystroke"),
                    is_held: false,
                    prefer_character_input: false,
                }),
                cx,
            );
            window.dispatch_event(
                FileDropEvent::Entered {
                    position: reentry_position,
                    paths: external_paths(),
                }
                .to_platform_input(),
                cx,
            );
            assert!(
                cx.active_drag
                    .as_ref()
                    .is_some_and(|drag| drag.value.downcast_ref::<ExternalPaths>().is_some())
            );
            assert_eq!(first_destination_exit_count.get(), 1);
            assert_eq!(second_destination_exit_count.get(), 0);

            window.dispatch_event(FileDropEvent::Exited.to_platform_input(), cx);
            assert!(cx.active_drag.is_none());
            assert_eq!(first_destination_exit_count.get(), 1);
            assert_eq!(second_destination_exit_count.get(), 1);
        });
        assert!(
            update_result.is_ok(),
            "failed to handle drag in second destination window: {update_result:?}"
        );
    }

    #[test]
    fn open_window_propagates_platform_map_error() {
        let cx = TestAppContext::single();
        cx.test_platform()
            .fail_next_window_map("test window mapping failed");

        let result =
            cx.update(|cx| cx.open_window(WindowOptions::default(), |_, cx| cx.new(|_| EmptyView)));

        let error = match result {
            Ok(_) => panic!("window mapping unexpectedly succeeded"),
            Err(error) => error,
        };
        assert_eq!(error.to_string(), "test window mapping failed");
        assert!(cx.read(|cx| cx.windows().is_empty()));
    }

    struct OpensWindowOnPaint {
        opened: Rc<Cell<bool>>,
    }

    impl Render for OpensWindowOnPaint {
        fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
            let opened = self.opened.clone();
            div()
                .size_full()
                .child(canvas(
                    |_, _, _| {},
                    move |_, _, _window, cx| {
                        if !opened.replace(true) {
                            cx.open_window(WindowOptions::default(), |_, cx| cx.new(|_| EmptyView))
                                .unwrap();
                        }
                    },
                ))
                .child(div().child("after"))
        }
    }

    /// Opening a window synchronously draws it and requests an element-arena
    /// clear. That request must not invalidate outer elements that remain to be
    /// painted after the nested draw returns.
    #[test]
    fn opening_a_window_during_draw_keeps_outer_elements_alive() {
        let mut cx = TestAppContext::single();
        let opened = Rc::new(Cell::new(false));
        let window = cx.add_window({
            let opened = opened.clone();
            move |_, _| OpensWindowOnPaint { opened }
        });

        assert!(opened.get());
        assert_eq!(cx.windows().len(), 2);

        cx.update_window(window.into(), |_, window, cx| window.draw(cx).clear())
            .unwrap();
    }

    struct FocusLostRestoreView {
        parent: FocusHandle,
        child: FocusHandle,
        show_child: bool,
        observed_restore: Option<FocusHandle>,
        _subscription: Subscription,
    }

    impl FocusLostRestoreView {
        fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
            Self {
                parent: cx.focus_handle(),
                child: cx.focus_handle(),
                show_child: true,
                observed_restore: None,
                _subscription: cx.on_focus_lost(window, |this, window, cx| {
                    this.observed_restore = window.focus_lost_restore_target(cx);
                }),
            }
        }
    }

    impl Render for FocusLostRestoreView {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            div()
                .id("parent")
                .track_focus(&self.parent)
                .when(self.show_child, |parent| {
                    parent.child(div().id("child").track_focus(&self.child))
                })
        }
    }

    /// Removing a focused descendant fires `on_focus_lost`. During that callback,
    /// `focus_lost_restore_target` is the closest ancestor still in the tree.
    #[test]
    fn focus_lost_restore_target_is_nearest_surviving_ancestor() {
        let mut cx = TestAppContext::single();
        let window = cx.add_window(FocusLostRestoreView::new);

        window
            .update(&mut cx, |view, window, _| {
                window.focus(&view.child);
            })
            .unwrap();

        cx.update_window(window.into(), |_, window, cx| {
            window.draw(cx).clear();
            assert!(
                window.focus_lost_restore_target(cx).is_none(),
                "restore target is only set while focus-lost listeners run"
            );
        })
        .unwrap();

        window
            .update(&mut cx, |view, window, _| {
                assert!(view.child.is_focused(window));
                view.show_child = false;
            })
            .unwrap();

        cx.update_window(window.into(), |_, window, cx| {
            window.draw(cx).clear();
            assert!(
                window.focus_lost_restore_target(cx).is_none(),
                "restore target must be cleared after focus-lost listeners return"
            );
        })
        .unwrap();

        window
            .update(&mut cx, |view, _, _| {
                assert_eq!(
                    view.observed_restore.as_ref(),
                    Some(&view.parent),
                    "restore target should be the parent that remained in the tree"
                );
            })
            .unwrap();
    }

    #[gpui::test]
    fn test_window_reports_no_raw_handle_instead_of_panicking(cx: &mut TestAppContext) {
        use raw_window_handle::{HandleError, HasDisplayHandle as _, HasWindowHandle as _};

        let window = cx.add_window(|_, _| EmptyView);
        window
            .update(cx, |_, window, _| {
                assert!(matches!(
                    window.window_handle(),
                    Err(HandleError::NotSupported)
                ));
                assert!(matches!(
                    window.display_handle(),
                    Err(HandleError::NotSupported)
                ));
            })
            .unwrap();
    }

    /// Visibility transitions reach observers exactly once each, with the new
    /// state already stored on the window, and never wake the platform for a
    /// frame: the platform requests one itself when it resumes presenting.
    #[gpui::test]
    fn test_window_visibility(cx: &mut TestAppContext) {
        let window = cx.add_window(|_, _| EmptyView);
        let observed = Rc::new(RefCell::new(Vec::new()));
        let _subscription = window
            .update(cx, {
                let observed = observed.clone();
                move |_, window, _| {
                    assert_eq!(window.visibility(), WindowVisibility::Visible);
                    assert!(window.is_visible());
                    window.observe_window_visibility(move |visibility, window, _| {
                        assert_eq!(window.visibility(), visibility);
                        observed.borrow_mut().push(visibility);
                    })
                }
            })
            .unwrap();
        let test_window = cx.test_window(window.into());
        // Effect flush schedules a frame for a window that is already dirty.
        // Clear that so the count below only moves if visibility itself asks
        // for a frame.
        window
            .update(cx, |_, window, _| {
                window.invalidator.set_dirty(false);
                window.needs_present.set(false);
            })
            .unwrap();
        let frame_wake_count = test_window.frame_wake_count();

        test_window.simulate_visibility_change(WindowVisibility::Hidden);
        assert_eq!(*observed.borrow(), [WindowVisibility::Hidden]);
        window
            .update(cx, |_, window, _| assert!(!window.is_visible()))
            .unwrap();

        // Platforms may report the same state again; observers only see changes.
        test_window.simulate_visibility_change(WindowVisibility::Hidden);
        assert_eq!(observed.borrow().len(), 1);

        test_window.simulate_visibility_change(WindowVisibility::Visible);
        assert_eq!(
            *observed.borrow(),
            [WindowVisibility::Hidden, WindowVisibility::Visible]
        );
        window
            .update(cx, |_, window, _| assert!(window.is_visible()))
            .unwrap();
        assert_eq!(test_window.frame_wake_count(), frame_wake_count);
    }

    #[gpui::test]
    fn test_appearance_change_runs_after_app_update(cx: &mut TestAppContext) {
        let window = cx.add_window(|_, _| EmptyView);
        let observed_appearance = Rc::new(Cell::new(None));
        let _subscription = window
            .update(cx, {
                let observed_appearance = observed_appearance.clone();
                move |_, window, _| {
                    window.observe_window_appearance(move |window, _| {
                        observed_appearance.set(Some(window.appearance()));
                    })
                }
            })
            .unwrap();
        let test_window = cx.test_window(window.into());

        cx.update(|_| {
            test_window.simulate_appearance_change(WindowAppearance::Dark);
            assert_eq!(observed_appearance.get(), None);
        });
        cx.run_until_parked();

        assert_eq!(observed_appearance.get(), Some(WindowAppearance::Dark));
    }

    #[test]
    fn test_scale_factor_change_preserves_bounds_and_survives_resize() {
        let mut cx = TestAppContext::single();
        let window = cx.add_window(|_, _| EmptyView);
        let handle: AnyWindowHandle = window.into();
        let window_state = |cx: &mut TestAppContext| {
            cx.update_window(handle, |_, window, _| {
                (
                    window.scale_factor(),
                    window.bounds(),
                    window.viewport_size(),
                )
            })
            .unwrap()
        };

        let (scale_factor, mut expected_bounds, _) = window_state(&mut cx);
        assert_eq!(scale_factor, 2.0);

        for (scale_factor, resized_size) in [
            (1.0, size(px(800.), px(600.))),
            (1.25, size(px(640.), px(480.))),
            (2.0, size(px(1024.), px(768.))),
        ] {
            cx.simulate_window_scale_factor_change(handle, scale_factor);
            assert_eq!(
                window_state(&mut cx),
                (scale_factor, expected_bounds, expected_bounds.size)
            );

            cx.simulate_window_resize(handle, resized_size);
            expected_bounds.size = resized_size;
            assert_eq!(
                window_state(&mut cx),
                (scale_factor, expected_bounds, resized_size)
            );
        }
    }

    struct RootView {
        explicit_size: bool,
        child_bounds: Rc<Cell<Bounds<Pixels>>>,
    }

    impl Render for RootView {
        fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
            let child_bounds = self.child_bounds.clone();
            let mut root = div().flex().flex_col().child(
                canvas(
                    move |bounds, _, _| child_bounds.set(bounds),
                    |_, _, _, _| {},
                )
                .size_full(),
            );
            if self.explicit_size {
                root = root.w(px(300.)).h(px(200.));
            }
            root
        }
    }

    #[test]
    fn auto_sized_window_root_fills_the_window() {
        let mut cx = TestAppContext::single();
        let child_bounds = Rc::new(Cell::new(Bounds::default()));
        let window = cx.add_window({
            let child_bounds = child_bounds.clone();
            move |_, _| RootView {
                explicit_size: false,
                child_bounds,
            }
        });

        let viewport_size = cx
            .update_window(window.into(), |_, window, cx| {
                window.draw(cx).clear();
                window.viewport_size()
            })
            .unwrap();

        assert_eq!(child_bounds.get().size, viewport_size);
    }

    #[test]
    fn explicitly_sized_window_root_keeps_its_size() {
        let mut cx = TestAppContext::single();
        let child_bounds = Rc::new(Cell::new(Bounds::default()));
        let window = cx.add_window({
            let child_bounds = child_bounds.clone();
            move |_, _| RootView {
                explicit_size: true,
                child_bounds,
            }
        });

        cx.update_window(window.into(), |_, window, cx| {
            window.draw(cx).clear();
        })
        .unwrap();

        assert_eq!(child_bounds.get().size, size(px(300.), px(200.)));
    }

    fn scaled(value: f32) -> ScaledPixels {
        ScaledPixels(value)
    }

    #[test]
    fn border_only_quad_strips_cover_asymmetric_borders_and_corners() {
        let bounds = Bounds::new(
            point(scaled(0.), scaled(0.)),
            size(scaled(100.), scaled(80.)),
        );
        let widths = Edges {
            top: scaled(6.),
            right: scaled(4.),
            bottom: scaled(8.),
            left: scaled(2.),
        };
        let radii = Corners {
            top_left: scaled(10.),
            top_right: scaled(12.),
            bottom_right: scaled(16.),
            bottom_left: scaled(14.),
        };

        let strips = border_only_quad_strips(bounds, widths, radii).unwrap();
        assert_eq!(
            strips,
            [
                Bounds::from_corners(
                    point(scaled(0.), scaled(0.)),
                    point(scaled(100.), scaled(13.)),
                ),
                Bounds::from_corners(
                    point(scaled(0.), scaled(63.)),
                    point(scaled(100.), scaled(80.)),
                ),
                Bounds::from_corners(
                    point(scaled(0.), scaled(13.)),
                    point(scaled(3.), scaled(63.)),
                ),
                Bounds::from_corners(
                    point(scaled(95.), scaled(13.)),
                    point(scaled(100.), scaled(63.)),
                ),
            ]
        );
    }

    #[test]
    fn border_only_quad_keeps_one_primitive_when_interior_is_too_small() {
        let bounds = Bounds::new(
            point(scaled(0.), scaled(0.)),
            size(scaled(10.), scaled(10.)),
        );
        let widths = Edges {
            top: scaled(5.),
            right: scaled(5.),
            bottom: scaled(5.),
            left: scaled(5.),
        };

        assert!(border_only_quad_strips(bounds, widths, Corners::default()).is_none());
    }

    struct BorderOnlyQuadView;

    impl Render for BorderOnlyQuadView {
        fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
            canvas(
                |_, _, _| {},
                |bounds, _, window, _| {
                    window.paint_quad(outline(bounds, hsla(0., 0., 1., 1.), BorderStyle::Solid));
                },
            )
            .size(px(100.))
        }
    }

    #[test]
    fn border_only_quad_emits_four_clipped_primitives() {
        let mut cx = TestAppContext::single();
        let window = cx.add_window(|_, _| BorderOnlyQuadView);

        let primitive_count = cx
            .update_window(window.into(), |_, window, cx| {
                window.draw(cx).clear();
                window.rendered_frame.scene.quads.len()
            })
            .unwrap();

        assert_eq!(primitive_count, 4);
    }

    #[test]
    fn resource_profile_gpu_budget_reaches_the_platform() {
        let cx = TestAppContext::single();

        let budget = cx.test_platform().gpu_resource_budget.lock().clone();
        assert_eq!(budget.instance_buffer_initial_size, 2 * 1024 * 1024);
        assert_eq!(budget.instance_buffer_max_size, 256 * 1024 * 1024);
    }

    #[test]
    fn window_attention_is_forwarded_to_the_platform_window() {
        let mut cx = TestAppContext::single();
        let window = cx.add_window(|_, _| BorderOnlyQuadView);
        let window = AnyWindowHandle::from(window);

        cx.update_window(window, |_, window, _| window.request_attention())
            .unwrap();

        assert_eq!(cx.test_window(window).0.lock().attention_requests, 1);
    }

    #[test]
    fn quit_mode_explicit_keeps_app_alive_after_last_window_closed() {
        let mut cx = TestAppContext::single();
        cx.update(|cx| cx.set_quit_mode(QuitMode::Explicit));
        assert_eq!(
            *cx.test_platform().last_quit_mode.lock(),
            Some(QuitMode::Explicit)
        );
        let window = cx.add_window(|_, _| BorderOnlyQuadView);
        let window = AnyWindowHandle::from(window);

        cx.update_window(window, |_, window, _| window.remove_window())
            .unwrap();

        assert!(!*cx.test_platform().did_quit.lock());
    }

    #[test]
    fn quit_mode_last_window_closed_quits_after_last_window_closed() {
        let mut cx = TestAppContext::single();
        cx.update(|cx| cx.set_quit_mode(QuitMode::LastWindowClosed));
        let window = cx.add_window(|_, _| BorderOnlyQuadView);
        let window = AnyWindowHandle::from(window);

        cx.update_window(window, |_, window, _| window.remove_window())
            .unwrap();

        assert!(*cx.test_platform().did_quit.lock());
    }

    #[test]
    fn quit_mode_runtime_change_applies_to_the_next_window_close() {
        let mut cx = TestAppContext::single();
        cx.update(|cx| cx.set_quit_mode(QuitMode::Explicit));
        let window = AnyWindowHandle::from(cx.add_window(|_, _| BorderOnlyQuadView));

        cx.update(|cx| cx.set_quit_mode(QuitMode::LastWindowClosed));
        assert_eq!(
            *cx.test_platform().last_quit_mode.lock(),
            Some(QuitMode::LastWindowClosed)
        );
        cx.update_window(window, |_, window, _| window.remove_window())
            .unwrap();

        assert!(*cx.test_platform().did_quit.lock());
    }

    #[test]
    fn quit_mode_last_window_closed_waits_for_every_window() {
        let mut cx = TestAppContext::single();
        cx.update(|cx| cx.set_quit_mode(QuitMode::LastWindowClosed));
        let first = AnyWindowHandle::from(cx.add_window(|_, _| BorderOnlyQuadView));
        let second = AnyWindowHandle::from(cx.add_window(|_, _| BorderOnlyQuadView));

        cx.update_window(first, |_, window, _| window.remove_window())
            .unwrap();
        assert!(!*cx.test_platform().did_quit.lock());

        cx.update_window(second, |_, window, _| window.remove_window())
            .unwrap();
        assert!(*cx.test_platform().did_quit.lock());
    }

    #[test]
    fn quit_mode_default_follows_platform_convention() {
        let mut cx = TestAppContext::single();
        // QuitMode::Default is the initial value, no need to set it.
        let window = cx.add_window(|_, _| BorderOnlyQuadView);
        let window = AnyWindowHandle::from(window);

        cx.update_window(window, |_, window, _| window.remove_window())
            .unwrap();

        // On macOS the default is to stay alive (Explicit-like); on other
        // platforms the default is to quit when the last window closes.
        let expected = cfg!(not(target_os = "macos"));
        assert_eq!(*cx.test_platform().did_quit.lock(), expected);
    }
}

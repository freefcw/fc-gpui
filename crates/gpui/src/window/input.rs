#[allow(unused_imports)]
use super::a11y::{A11y, ROOT_NODE_ID};
#[allow(unused_imports)]
use super::*;
#[cfg(any(feature = "inspector", debug_assertions))]
use crate::Inspector;
#[allow(unused_imports)]
use crate::util::{
    atomic_incr_if_not_zero, ceil_to_device_pixel, floor_to_device_pixel, round_half_toward_zero,
    round_half_toward_zero_f64, round_stroke_to_device_pixel, round_to_device_pixel,
};
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

impl Window {
    /// Sets an input handler, such as [`ElementInputHandler`][element_input_handler], which interfaces with the
    /// platform to receive textual input with proper integration with concerns such
    /// as IME interactions. This handler will be active for the upcoming frame until the following frame is
    /// rendered.
    ///
    /// This method should only be called as part of the paint phase of element drawing.
    ///
    /// [element_input_handler]: crate::ElementInputHandler
    pub fn handle_input(
        &mut self,
        focus_handle: &FocusHandle,
        input_handler: impl InputHandler,
        cx: &App,
    ) {
        self.invalidator.debug_assert_paint();

        if focus_handle.is_focused(self) {
            let cx = self.to_async(cx);
            self.next_frame
                .input_handlers
                .push(Some(PlatformInputHandler::new(cx, Box::new(input_handler))));
        }
    }

    /// Register a mouse event listener on the window for the next frame. The type of event
    /// is determined by the first parameter of the given listener. When the next frame is rendered
    /// the listener will be cleared.
    ///
    /// This method should only be called as part of the paint phase of element drawing.
    pub fn on_mouse_event<Event: MouseEvent>(
        &mut self,
        mut handler: impl FnMut(&Event, DispatchPhase, &mut Window, &mut App) + 'static,
    ) {
        self.invalidator.debug_assert_paint();

        self.next_frame.mouse_listeners.push(Some(Box::new(
            move |event: &dyn Any, phase: DispatchPhase, window: &mut Window, cx: &mut App| {
                if let Some(event) = event.downcast_ref() {
                    handler(event, phase, window, cx)
                }
            },
        )));
    }

    /// Register a key event listener on the window for the next frame. The type of event
    /// is determined by the first parameter of the given listener. When the next frame is rendered
    /// the listener will be cleared.
    ///
    /// This is a fairly low-level method, so prefer using event handlers on elements unless you have
    /// a specific need to register a global listener.
    ///
    /// This method should only be called as part of the paint phase of element drawing.
    pub fn on_key_event<Event: KeyEvent>(
        &mut self,
        listener: impl Fn(&Event, DispatchPhase, &mut Window, &mut App) + 'static,
    ) {
        self.invalidator.debug_assert_paint();

        self.next_frame.dispatch_tree.on_key_event(Rc::new(
            move |event: &dyn Any, phase, window: &mut Window, cx: &mut App| {
                if let Some(event) = event.downcast_ref::<Event>() {
                    listener(event, phase, window, cx)
                }
            },
        ));
    }

    /// Register a modifiers changed event listener on the window for the next frame.
    ///
    /// This is a fairly low-level method, so prefer using event handlers on elements unless you have
    /// a specific need to register a global listener.
    ///
    /// This method should only be called as part of the paint phase of element drawing.
    pub fn on_modifiers_changed(
        &mut self,
        listener: impl Fn(&ModifiersChangedEvent, &mut Window, &mut App) + 'static,
    ) {
        self.invalidator.debug_assert_paint();

        self.next_frame.dispatch_tree.on_modifiers_changed(Rc::new(
            move |event: &ModifiersChangedEvent, window: &mut Window, cx: &mut App| {
                listener(event, window, cx)
            },
        ));
    }

    /// Register a listener to be called when the given focus handle or one of its descendants receives focus.
    /// This does not fire if the given focus handle - or one of its descendants - was previously focused.
    /// Returns a subscription and persists until the subscription is dropped.
    pub fn on_focus_in(
        &mut self,
        handle: &FocusHandle,
        cx: &mut App,
        mut listener: impl FnMut(&mut Window, &mut App) + 'static,
    ) -> Subscription {
        let focus_id = handle.id;
        let (subscription, activate) =
            self.new_focus_listener(Box::new(move |event, window, cx| {
                if event.is_focus_in(focus_id) {
                    listener(window, cx);
                }
                true
            }));
        cx.defer(move |_| activate());
        subscription
    }

    /// Register a listener to be called when the given focus handle or one of its descendants loses focus.
    /// Returns a subscription and persists until the subscription is dropped.
    pub fn on_focus_out(
        &mut self,
        handle: &FocusHandle,
        cx: &mut App,
        mut listener: impl FnMut(FocusOutEvent, &mut Window, &mut App) + 'static,
    ) -> Subscription {
        let focus_id = handle.id;
        let (subscription, activate) =
            self.new_focus_listener(Box::new(move |event, window, cx| {
                if let Some(blurred_id) = event.previous_focus_path.last().copied()
                    && event.is_focus_out(focus_id)
                {
                    let event = FocusOutEvent {
                        blurred: WeakFocusHandle {
                            id: blurred_id,
                            handles: Arc::downgrade(&cx.focus_handles),
                        },
                    };
                    listener(event, window, cx)
                }
                true
            }));
        cx.defer(move |_| activate());
        subscription
    }

    pub(crate) fn reset_cursor_style(&self, cx: &mut App) {
        // Set the cursor only if we're the active window.
        if self.is_window_hovered() {
            let style = self
                .rendered_frame
                .cursor_style(self)
                .unwrap_or(CursorStyle::Arrow);
            // macOS: per-window call routes to the correct window (including panels/overlays)
            // without relying on NSApp.mainWindow guessing.
            // Linux/Windows: platform-level call handles cursor via their own focused-window logic.
            // TODO: implement PlatformWindow::set_cursor_style on Linux/Windows and remove the
            // platform-level call.
            self.platform_window.set_cursor_style(style);
            cx.platform.set_cursor_style(style);
        }
    }

    /// Dispatch a given keystroke as though the user had typed it.
    /// You can create a keystroke with Keystroke::parse("").
    pub fn dispatch_keystroke(&mut self, keystroke: Keystroke, cx: &mut App) -> bool {
        let keystroke = keystroke.with_simulated_ime();
        let result = self.dispatch_event(
            PlatformInput::KeyDown(KeyDownEvent {
                keystroke: keystroke.clone(),
                is_held: false,
                prefer_character_input: false,
            }),
            cx,
        );
        if !result.propagate {
            return true;
        }

        if let Some(input) = keystroke.key_char
            && let Some(mut input_handler) = self.platform_window.take_input_handler()
        {
            input_handler.dispatch_input(&input, self, cx);
            self.platform_window.set_input_handler(input_handler);
            return true;
        }

        false
    }

    /// Return a key binding string for an action, to display in the UI. Uses the highest precedence
    /// binding for the action (last binding added to the keymap).
    pub fn keystroke_text_for(&self, action: &dyn Action) -> String {
        self.highest_precedence_binding_for_action(action)
            .map(|binding| {
                binding
                    .keystrokes()
                    .iter()
                    .map(ToString::to_string)
                    .collect::<Vec<_>>()
                    .join(" ")
            })
            .unwrap_or_else(|| action.name().to_string())
    }

    /// Dispatch a mouse or keyboard event on the window.
    #[profiling::function]
    pub fn dispatch_event(&mut self, event: PlatformInput, cx: &mut App) -> DispatchEventResult {
        #[cfg(feature = "input-latency-histogram")]
        let dispatch_time = Instant::now();
        let update_count_before = self.invalidator.update_count();

        // Handlers may set this to false by calling `stop_propagation`.
        cx.propagate_event = true;
        // Handlers may set this to true by calling `prevent_default`.
        self.default_prevented = false;

        let event = match event {
            // Track the mouse position with our own state, since accessing the platform
            // API for the mouse position can only occur on the main thread.
            PlatformInput::MouseMove(mouse_move) => {
                self.mouse_position = mouse_move.position;
                self.modifiers = mouse_move.modifiers;
                PlatformInput::MouseMove(mouse_move)
            }
            PlatformInput::MouseDown(mouse_down) => {
                self.mouse_position = mouse_down.position;
                self.modifiers = mouse_down.modifiers;
                PlatformInput::MouseDown(mouse_down)
            }
            PlatformInput::MouseUp(mouse_up) => {
                self.mouse_position = mouse_up.position;
                self.modifiers = mouse_up.modifiers;
                PlatformInput::MouseUp(mouse_up)
            }
            PlatformInput::MouseExited(mouse_exited) => {
                self.modifiers = mouse_exited.modifiers;
                PlatformInput::MouseExited(mouse_exited)
            }
            PlatformInput::ModifiersChanged(modifiers_changed) => {
                self.modifiers = modifiers_changed.modifiers;
                self.capslock = modifiers_changed.capslock;
                PlatformInput::ModifiersChanged(modifiers_changed)
            }
            PlatformInput::ScrollWheel(scroll_wheel) => {
                self.mouse_position = scroll_wheel.position;
                self.modifiers = scroll_wheel.modifiers;
                PlatformInput::ScrollWheel(scroll_wheel)
            }
            PlatformInput::Pinch(pinch) => {
                self.mouse_position = pinch.position;
                self.modifiers = pinch.modifiers;
                PlatformInput::Pinch(pinch)
            }
            // Translate dragging and dropping of external files from the operating system
            // to internal drag and drop events.
            PlatformInput::FileDrop(file_drop) => match file_drop {
                FileDropEvent::Entered { position, paths } => {
                    self.mouse_position = position;
                    if cx.active_drag.is_none() {
                        cx.active_drag = Some(AnyDrag {
                            value: Arc::new(paths.clone()),
                            view: cx.new(|_| paths).into(),
                            cursor_offset: position,
                            cursor_style: None,
                        });
                    }
                    PlatformInput::MouseMove(MouseMoveEvent {
                        position,
                        pressed_button: Some(MouseButton::Left),
                        modifiers: Modifiers::default(),
                    })
                }
                FileDropEvent::Pending { position } => {
                    self.mouse_position = position;
                    PlatformInput::MouseMove(MouseMoveEvent {
                        position,
                        pressed_button: Some(MouseButton::Left),
                        modifiers: Modifiers::default(),
                    })
                }
                FileDropEvent::Submit { position } => {
                    cx.activate(true);
                    self.mouse_position = position;
                    PlatformInput::MouseUp(MouseUpEvent {
                        button: MouseButton::Left,
                        position,
                        modifiers: Modifiers::default(),
                        click_count: 1,
                    })
                }
                FileDropEvent::Exited => {
                    cx.active_drag.take();
                    PlatformInput::FileDrop(FileDropEvent::Exited)
                }
            },
            PlatformInput::KeyDown(_) | PlatformInput::KeyUp(_) => event,
        };

        if let Some(any_mouse_event) = event.mouse_event() {
            self.dispatch_mouse_event(any_mouse_event, cx);
        } else if let Some(any_key_event) = event.keyboard_event() {
            self.dispatch_key_event(any_key_event, cx);
        }

        if self.invalidator.update_count() > update_count_before {
            self.input_rate_tracker.borrow_mut().record_input();
            #[cfg(feature = "input-latency-histogram")]
            if self.invalidator.not_drawing() {
                self.input_latency_tracker.record_input(dispatch_time);
            } else {
                self.input_latency_tracker.record_mid_draw_input();
            }
        }

        DispatchEventResult {
            propagate: cx.propagate_event,
            default_prevented: self.default_prevented,
        }
    }

    fn dispatch_mouse_event(&mut self, event: &dyn Any, cx: &mut App) {
        self.last_input_was_keyboard.set(false);

        let hit_test = self.rendered_frame.hit_test(self.mouse_position());
        if hit_test != self.mouse_hit_test {
            self.mouse_hit_test = hit_test;
            self.reset_cursor_style(cx);
        }

        #[cfg(any(feature = "inspector", debug_assertions))]
        if self.is_inspector_picking(cx) {
            self.handle_inspector_mouse_event(event, cx);
            // When inspector is picking, all other mouse handling is skipped.
            return;
        }

        let mut mouse_listeners = mem::take(&mut self.rendered_frame.mouse_listeners);

        // Capture phase, events bubble from back to front. Handlers for this phase are used for
        // special purposes, such as detecting events outside of a given Bounds.
        for listener in &mut mouse_listeners {
            let listener = listener.as_mut().unwrap();
            listener(event, DispatchPhase::Capture, self, cx);
            if !cx.propagate_event {
                break;
            }
        }

        // Bubble phase, where most normal handlers do their work.
        if cx.propagate_event {
            for listener in mouse_listeners.iter_mut().rev() {
                let listener = listener.as_mut().unwrap();
                listener(event, DispatchPhase::Bubble, self, cx);
                if !cx.propagate_event {
                    break;
                }
            }
        }

        self.rendered_frame.mouse_listeners = mouse_listeners;

        if cx.has_active_drag() {
            if event.is::<MouseMoveEvent>() {
                // If this was a mouse move event, redraw the window so that the
                // active drag can follow the mouse cursor.
                self.refresh();
            } else if event.is::<MouseUpEvent>() {
                // If this was a mouse up event, cancel the active drag and redraw
                // the window.
                cx.active_drag = None;
                self.refresh();
            }
        }

        if event.is::<MouseUpEvent>() && self.captured_hitbox.is_some() {
            self.captured_hitbox = None;
        }
    }

    fn dispatch_key_event(&mut self, event: &dyn Any, cx: &mut App) {
        self.last_input_was_keyboard.set(true);

        if self.invalidator.is_dirty() {
            self.draw(cx).clear();
        }

        let node_id = self.focus_node_id_in_rendered_frame(self.focus);
        let dispatch_path = self.rendered_frame.dispatch_tree.dispatch_path(node_id);

        let mut keystroke: Option<Keystroke> = None;

        if let Some(event) = event.downcast_ref::<ModifiersChangedEvent>() {
            if event.modifiers.number_of_modifiers() == 0
                && self.pending_modifier.modifiers.number_of_modifiers() == 1
                && !self.pending_modifier.saw_other_input
            {
                let key = match self.pending_modifier.modifiers {
                    modifiers if modifiers.shift => Some("shift"),
                    modifiers if modifiers.control => Some("control"),
                    modifiers if modifiers.alt => Some("alt"),
                    modifiers if modifiers.platform => Some("platform"),
                    modifiers if modifiers.function => Some("function"),
                    _ => None,
                };
                if let Some(key) = key {
                    keystroke = Some(Keystroke {
                        key: key.to_string(),
                        key_char: None,
                        modifiers: Modifiers::default(),
                    });
                }
            }

            if self.pending_modifier.modifiers.number_of_modifiers() == 0
                && event.modifiers.number_of_modifiers() == 1
            {
                self.pending_modifier.saw_other_input = false
            } else if event.modifiers.number_of_modifiers() > 1 {
                self.pending_modifier.saw_other_input = true
            }
            self.pending_modifier.modifiers = event.modifiers
        } else if let Some(key_down_event) = event.downcast_ref::<KeyDownEvent>() {
            self.pending_modifier.saw_other_input = true;
            keystroke = Some(key_down_event.keystroke.clone());
        }

        let Some(keystroke) = keystroke else {
            self.finish_dispatch_key_event(
                event,
                None,
                InputPreference::KeyBindings,
                dispatch_path,
                self.context_stack(),
                cx,
            );
            return;
        };

        let input_preference = self.input_preference(event, cx);

        cx.propagate_event = true;
        self.dispatch_keystroke_interceptors(
            &keystroke,
            input_preference,
            self.context_stack(),
            cx,
        );
        if !cx.propagate_event {
            self.finish_dispatch_key_event(
                event,
                Some(&keystroke),
                input_preference,
                dispatch_path,
                self.context_stack(),
                cx,
            );
            return;
        }

        let mut currently_pending = self.pending_input.take().unwrap_or_default();
        if currently_pending.focus.is_some() && currently_pending.focus != self.focus {
            currently_pending = PendingInput::default();
        }

        let match_result = self.rendered_frame.dispatch_tree.dispatch_key(
            currently_pending.keystrokes,
            keystroke.clone(),
            &dispatch_path,
        );

        if !match_result.to_replay.is_empty() {
            self.replay_pending_input(match_result.to_replay, cx);
            cx.propagate_event = true;
        }

        if !match_result.pending.is_empty() {
            let previous_timeout = currently_pending.timeout.take();
            currently_pending.keystrokes = match_result.pending;
            currently_pending.focus = self.focus;

            // This tree always flushes pending input. It does not record whether the
            // prefix is already a binding, so a non-binding prefix still times out
            // instead of waiting until a shorter binding makes the sequence ambiguous.
            currently_pending.timeout = match previous_timeout {
                Some(mut timeout) if timeout.is_paused() => {
                    timeout.reset_duration(PENDING_INPUT_TIMEOUT);
                    Some(timeout)
                }
                previous_timeout => {
                    drop(previous_timeout);
                    Some(self.new_pending_input_timeout(PENDING_INPUT_TIMEOUT, cx))
                }
            };
            self.pending_input = Some(currently_pending);
            self.pending_input_changed(cx);
            cx.propagate_event = false;
            return;
        }

        // Interceptors or replayed actions may have changed whether the input handler accepts text.
        let input_preference = self.input_preference(event, cx);
        if input_preference == InputPreference::KeyBindings {
            for binding in match_result.bindings {
                self.dispatch_action_on_node(node_id, binding.action.as_ref(), cx);
                if !cx.propagate_event {
                    self.dispatch_keystroke_observers(
                        &keystroke,
                        input_preference,
                        Some(binding.action.as_ref()),
                        match_result.context_stack,
                        cx,
                    );
                    self.pending_input_changed(cx);
                    return;
                }
            }
        }

        self.finish_dispatch_key_event(
            event,
            Some(&keystroke),
            input_preference,
            dispatch_path,
            match_result.context_stack,
            cx,
        );
        self.pending_input_changed(cx);
    }

    // If modifiers are not excessive (e.g. AltGr), and the input handler is accepting text input,
    // we prefer the text input over bindings.
    fn input_preference(&mut self, event: &dyn Any, cx: &mut App) -> InputPreference {
        let prefer_character_input = event
            .downcast_ref::<KeyDownEvent>()
            .is_some_and(|key_down_event| key_down_event.prefer_character_input);
        if !prefer_character_input {
            return InputPreference::KeyBindings;
        }
        let Some(mut input_handler) = self.platform_window.take_input_handler() else {
            return InputPreference::KeyBindings;
        };
        let accepts_text_input = input_handler.accepts_text_input(self, cx);
        self.platform_window.set_input_handler(input_handler);
        if accepts_text_input {
            InputPreference::CharacterInput
        } else {
            InputPreference::KeyBindings
        }
    }

    fn new_pending_input_timeout(&self, duration: Duration, cx: &App) -> PendingInputTimeout {
        let (started_at, task) = self.start_pending_input_timeout(duration, cx);
        PendingInputTimeout {
            duration,
            remaining: duration,
            state: PendingInputTimeoutState::Running { started_at, task },
        }
    }

    fn start_pending_input_timeout(&self, remaining: Duration, cx: &App) -> (Instant, Task<()>) {
        let started_at = cx.background_executor().now();
        let task = self.spawn(cx, async move |cx| {
            cx.background_executor.timer(remaining).await;
            cx.update(move |window, cx| {
                let Some(currently_pending) = window
                    .pending_input
                    .take()
                    .filter(|pending| pending.focus == window.focus)
                else {
                    return;
                };

                let node_id = window.focus_node_id_in_rendered_frame(window.focus);
                let dispatch_path = window.rendered_frame.dispatch_tree.dispatch_path(node_id);

                let to_replay = window
                    .rendered_frame
                    .dispatch_tree
                    .flush_dispatch(currently_pending.keystrokes, &dispatch_path);

                window.pending_input_changed(cx);
                window.replay_pending_input(to_replay, cx)
            })
            .log_err();
        });
        (started_at, task)
    }

    fn finish_dispatch_key_event(
        &mut self,
        event: &dyn Any,
        recognized_keystroke: Option<&Keystroke>,
        input_preference: InputPreference,
        dispatch_path: SmallVec<[DispatchNodeId; 32]>,
        context_stack: Vec<KeyContext>,
        cx: &mut App,
    ) {
        self.dispatch_key_down_up_event(event, &dispatch_path, cx);
        if !cx.propagate_event {
            return;
        }

        self.dispatch_modifiers_changed_event(event, &dispatch_path, cx);
        if !cx.propagate_event {
            return;
        }

        if let Some(keystroke) = recognized_keystroke {
            self.dispatch_keystroke_observers(keystroke, input_preference, None, context_stack, cx);
        }
    }

    fn pending_input_changed(&mut self, cx: &mut App) {
        self.pending_input_observers
            .clone()
            .retain(&(), |callback| callback(self, cx));
    }

    fn dispatch_key_down_up_event(
        &mut self,
        event: &dyn Any,
        dispatch_path: &SmallVec<[DispatchNodeId; 32]>,
        cx: &mut App,
    ) {
        // Capture phase
        for node_id in dispatch_path {
            let node = self.rendered_frame.dispatch_tree.node(*node_id);

            for key_listener in node.key_listeners.clone() {
                key_listener(event, DispatchPhase::Capture, self, cx);
                if !cx.propagate_event {
                    return;
                }
            }
        }

        // Bubble phase
        for node_id in dispatch_path.iter().rev() {
            // Handle low level key events
            let node = self.rendered_frame.dispatch_tree.node(*node_id);
            for key_listener in node.key_listeners.clone() {
                key_listener(event, DispatchPhase::Bubble, self, cx);
                if !cx.propagate_event {
                    return;
                }
            }
        }
    }

    fn dispatch_modifiers_changed_event(
        &mut self,
        event: &dyn Any,
        dispatch_path: &SmallVec<[DispatchNodeId; 32]>,
        cx: &mut App,
    ) {
        let Some(event) = event.downcast_ref::<ModifiersChangedEvent>() else {
            return;
        };
        for node_id in dispatch_path.iter().rev() {
            let node = self.rendered_frame.dispatch_tree.node(*node_id);
            for listener in node.modifiers_changed_listeners.clone() {
                listener(event, self, cx);
                if !cx.propagate_event {
                    return;
                }
            }
        }
    }

    /// Determine whether a potential multi-stroke key binding is in progress on this window.
    pub fn has_pending_keystrokes(&self) -> bool {
        self.pending_input().is_some()
    }

    #[cfg(test)]
    pub(crate) fn pending_input_is_none(&self) -> bool {
        self.pending_input.is_none()
    }

    fn defer_pending_input_changed(&self, cx: &mut App) {
        // Avoid re-entrant entity updates by deferring observer notifications to the end of the
        // current effect cycle, and only for this window.
        self.defer(cx, |window, cx| {
            window.pending_input_changed(cx);
        });
    }

    pub(crate) fn clear_pending_keystrokes(&mut self, cx: &mut App) {
        if self.pending_input.take().is_some() {
            self.defer_pending_input_changed(cx);
        }
    }

    /// Returns pending input that can still complete a multi-stroke key binding. Input left over
    /// from a previous focus can never complete one.
    pub fn pending_input(&self) -> Option<PendingInputStatus<'_>> {
        self.pending_input
            .as_ref()
            .filter(|pending_input| pending_input.focus == self.focus)
            .map(|pending_input| PendingInputStatus {
                keystrokes: pending_input.keystrokes.as_slice(),
                timeout: pending_input
                    .timeout
                    .as_ref()
                    .map(PendingInputTimeout::status),
            })
    }

    /// Pauses or resumes the current pending input timeout on behalf of `owner`.
    ///
    /// A paused timeout resumes automatically if `owner` is released. Returns whether the timeout
    /// state changed. A timeout paused by one owner cannot be resumed by another.
    pub fn set_pending_input_timeout_paused<T: 'static>(
        &mut self,
        owner: &Entity<T>,
        paused: bool,
        cx: &mut App,
    ) -> bool {
        let owner_id = owner.entity_id();
        if !paused {
            return self.resume_pending_input_timeout(owner_id, cx);
        }

        let timeout = self
            .pending_input
            .as_ref()
            .filter(|pending_input| pending_input.focus == self.focus)
            .and_then(|pending_input| pending_input.timeout.as_ref());
        let Some(timeout) = timeout else {
            return false;
        };
        if timeout.is_paused() {
            return false;
        }

        let release_subscription = self.observe_release(owner, cx, move |_, window, cx| {
            window.resume_pending_input_timeout(owner_id, cx);
        });
        let now = cx.background_executor().now();
        let changed = self
            .pending_input
            .as_mut()
            .filter(|pending_input| pending_input.focus == self.focus)
            .and_then(|pending_input| pending_input.timeout.as_mut())
            .is_some_and(|timeout| {
                timeout.pause(
                    PendingInputTimeoutPause {
                        owner_id,
                        _release_subscription: release_subscription,
                    },
                    now,
                )
            });

        if changed {
            self.defer_pending_input_changed(cx);
        }
        changed
    }

    fn resume_pending_input_timeout(&mut self, owner_id: EntityId, cx: &mut App) -> bool {
        let Some(remaining) = self
            .pending_input
            .as_ref()
            .filter(|pending_input| pending_input.focus == self.focus)
            .and_then(|pending_input| pending_input.timeout.as_ref())
            .filter(|timeout| timeout.pause_owner_id() == Some(owner_id))
            .map(|timeout| timeout.remaining)
        else {
            return false;
        };

        let (started_at, task) = self.start_pending_input_timeout(remaining, cx);
        let changed = self
            .pending_input
            .as_mut()
            .filter(|pending_input| pending_input.focus == self.focus)
            .and_then(|pending_input| pending_input.timeout.as_mut())
            .is_some_and(|timeout| timeout.resume(owner_id, started_at, task));

        if changed {
            self.defer_pending_input_changed(cx);
        }
        changed
    }

    /// Returns the currently pending input keystrokes that might result in a multi-stroke key binding.
    pub fn pending_input_keystrokes(&self) -> Option<&[Keystroke]> {
        self.pending_input()
            .map(|pending_input| pending_input.keystrokes())
    }

    fn replay_pending_input(&mut self, replays: SmallVec<[Replay; 1]>, cx: &mut App) {
        let node_id = self.focus_node_id_in_rendered_frame(self.focus);
        let dispatch_path = self.rendered_frame.dispatch_tree.dispatch_path(node_id);

        'replay: for replay in replays {
            let event = KeyDownEvent {
                keystroke: replay.keystroke.clone(),
                is_held: false,
                prefer_character_input: true,
            };

            cx.propagate_event = true;
            for binding in replay.bindings {
                self.dispatch_action_on_node(node_id, binding.action.as_ref(), cx);
                if !cx.propagate_event {
                    self.dispatch_keystroke_observers(
                        &replay.keystroke,
                        InputPreference::KeyBindings,
                        Some(binding.action.as_ref()),
                        Vec::default(),
                        cx,
                    );
                    continue 'replay;
                }
            }

            self.dispatch_key_down_up_event(&event, &dispatch_path, cx);
            if !cx.propagate_event {
                continue 'replay;
            }
            if let Some(input) = replay.keystroke.key_char.as_ref().cloned()
                && let Some(mut input_handler) = self.platform_window.take_input_handler()
            {
                input_handler.dispatch_input(&input, self, cx);
                self.platform_window.set_input_handler(input_handler)
            }
        }
    }

    pub(crate) fn focus_node_id_in_rendered_frame(
        &self,
        focus_id: Option<FocusId>,
    ) -> DispatchNodeId {
        focus_id
            .and_then(|focus_id| {
                self.rendered_frame
                    .dispatch_tree
                    .focusable_node_id(focus_id)
            })
            .unwrap_or_else(|| self.rendered_frame.dispatch_tree.root_node_id())
    }

    pub(crate) fn dispatch_action_on_node(
        &mut self,
        node_id: DispatchNodeId,
        action: &dyn Action,
        cx: &mut App,
    ) {
        let dispatch_path = self.rendered_frame.dispatch_tree.dispatch_path(node_id);

        // Capture phase for global actions.
        cx.propagate_event = true;
        cx.dispatch_global_action_listeners(action, DispatchPhase::Capture);

        if !cx.propagate_event {
            return;
        }

        // Capture phase for window actions.
        for node_id in &dispatch_path {
            let node = self.rendered_frame.dispatch_tree.node(*node_id);
            for DispatchActionListener {
                action_type,
                listener,
            } in node.action_listeners.clone()
            {
                let any_action = action.as_any();
                if action_type == any_action.type_id() {
                    listener(any_action, DispatchPhase::Capture, self, cx);

                    if !cx.propagate_event {
                        return;
                    }
                }
            }
        }

        // Bubble phase for window actions.
        for node_id in dispatch_path.iter().rev() {
            let node = self.rendered_frame.dispatch_tree.node(*node_id);
            for DispatchActionListener {
                action_type,
                listener,
            } in node.action_listeners.clone()
            {
                let any_action = action.as_any();
                if action_type == any_action.type_id() {
                    cx.propagate_event = false; // Actions stop propagation by default during the bubble phase
                    listener(any_action, DispatchPhase::Bubble, self, cx);

                    if !cx.propagate_event {
                        return;
                    }
                }
            }
        }

        // Bubble phase for global actions.
        cx.dispatch_global_action_listeners(action, DispatchPhase::Bubble);
    }
}

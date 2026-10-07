use std::{
    cell::Cell,
    rc::Rc,
    time::{Duration, Instant},
};

use crate::{
    AnyElement, App, Element, ElementId, GlobalElementId, InspectorElementId, IntoElement,
    ParentElement, SpringAnimation, SpringConfig, SpringPlayback, SpringState, SpringTarget,
    Window,
};

pub use easing::*;
use smallvec::SmallVec;

/// A handle that can be used to cancel an in-flight animation.
#[derive(Clone)]
pub struct AnimationHandle {
    cancelled: Rc<Cell<bool>>,
}

impl AnimationHandle {
    fn new() -> Self {
        Self {
            cancelled: Rc::new(Cell::new(false)),
        }
    }

    /// Cancel the animation, causing it to jump to its final state.
    pub fn cancel(&self) {
        self.cancelled.set(true);
    }

    /// Whether the animation has been cancelled.
    pub fn is_cancelled(&self) -> bool {
        self.cancelled.get()
    }
}

/// How an [`Animation`] behaves once one pass finishes.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum AnimationRepeat {
    /// Play once and hold the final value (the default).
    #[default]
    Once,
    /// Restart forever, wrapping progress back to the start.
    Forever,
    /// Play a fixed number of passes in total, then hold the final value.
    /// Zero is clamped to one pass.
    Times(u32),
}

/// An animation that can be applied to an element.
#[derive(Clone)]
pub struct Animation {
    /// The amount of time for which this animation should run. A zero
    /// duration completes immediately after the delay, including for
    /// [`AnimationRepeat::Forever`].
    pub duration: Duration,
    /// The amount of time to wait before the animation starts. The delay
    /// applies once, when the animation begins; it is not repeated between
    /// passes of a repeating animation.
    pub delay: Duration,
    /// Whether and how this animation repeats when a pass finishes
    pub repeat: AnimationRepeat,
    /// A function that maps normalized time to an animated value.
    /// The result may exceed 0..1 for easing functions that overshoot.
    pub easing: Rc<dyn Fn(f32) -> f32>,
    /// The maximum number of times per second this animation re-renders.
    /// When `None`, the animation re-renders on every frame.
    pub max_fps: Option<f32>,
}

impl Animation {
    /// Create a new animation with the given duration.
    /// By default the animation will only run once and will use a linear easing function.
    pub fn new(duration: Duration) -> Self {
        Self {
            duration,
            delay: Duration::ZERO,
            repeat: AnimationRepeat::Once,
            easing: Rc::new(linear),
            max_fps: None,
        }
    }

    /// Set the animation to loop forever when it finishes.
    pub fn repeat(mut self) -> Self {
        self.repeat = AnimationRepeat::Forever;
        self
    }

    /// Set the animation to run the given number of passes in total, then
    /// hold its final value. Zero is treated as one pass.
    pub fn repeat_n(mut self, times: u32) -> Self {
        self.repeat = AnimationRepeat::Times(times);
        self
    }

    /// Set how the animation repeats when a pass finishes.
    pub fn with_repeat(mut self, repeat: AnimationRepeat) -> Self {
        self.repeat = repeat;
        self
    }

    /// Wait for the given duration before the animation starts. While the
    /// delay elapses the animation holds its initial value (`easing(0)`) and
    /// no animation frames are requested. In an animation chain (see
    /// [`AnimationExt::with_animations`]) the delay of each animation counts
    /// from the moment that animation begins.
    pub fn with_delay(mut self, delay: Duration) -> Self {
        self.delay = delay;
        self
    }

    /// Sets the easing function used to map normalized time to an animated value.
    ///
    /// The output is not clamped, allowing physical easing functions such as
    /// springs to overshoot.
    pub fn with_easing(mut self, easing: impl Fn(f32) -> f32 + 'static) -> Self {
        self.easing = Rc::new(easing);
        self
    }

    /// Limit how often this animation re-renders. Instead of re-rendering on
    /// every frame, the animation schedules its next render `1 / max_fps`
    /// seconds after the current one. Values that are not finite and positive
    /// are ignored.
    pub fn with_max_fps(mut self, max_fps: f32) -> Self {
        self.max_fps = Some(max_fps);
        self
    }
}

/// An extension trait for adding the animation wrapper to both Elements and Components
pub trait AnimationExt {
    /// Render this component or element with an animation
    fn with_animation(
        self,
        id: impl Into<ElementId>,
        animation: Animation,
        animator: impl Fn(Self, f32) -> Self + 'static,
    ) -> AnimationElement<Self>
    where
        Self: Sized,
    {
        AnimationElement {
            id: id.into(),
            element: Some(self),
            animator: Box::new(move |this, _, value| animator(this, value)),
            animations: smallvec::smallvec![animation],
            cancel_handle: None,
        }
    }

    /// Render this component or element with a chain of animations
    fn with_animations(
        self,
        id: impl Into<ElementId>,
        animations: Vec<Animation>,
        animator: impl Fn(Self, usize, f32) -> Self + 'static,
    ) -> AnimationElement<Self>
    where
        Self: Sized,
    {
        AnimationElement {
            id: id.into(),
            element: Some(self),
            animator: Box::new(animator),
            animations: animations.into(),
            cancel_handle: None,
        }
    }

    /// Render this component or element with a cancellable animation.
    /// Returns the animated element and a handle that can be used to cancel the animation.
    fn with_cancellable_animation(
        self,
        id: impl Into<ElementId>,
        animation: Animation,
        animator: impl Fn(Self, f32) -> Self + 'static,
    ) -> (AnimationElement<Self>, AnimationHandle)
    where
        Self: Sized,
    {
        let handle = AnimationHandle::new();
        let element = AnimationElement {
            id: id.into(),
            element: Some(self),
            animator: Box::new(move |this, _, value| animator(this, value)),
            animations: smallvec::smallvec![animation],
            cancel_handle: Some(handle.cancelled.clone()),
        };
        (element, handle)
    }

    /// Renders this component or element at the value produced by a spring.
    ///
    /// The element ID preserves position and velocity across target changes.
    /// A newly mounted spring starts at its target unless configured with
    /// [`SpringAnimation::from`].
    fn with_spring<T>(
        self,
        id: impl Into<ElementId>,
        animation: SpringAnimation<T>,
        animator: impl FnOnce(Self, T::Output) -> Self + 'static,
    ) -> SpringAnimationElement<Self>
    where
        Self: Sized,
        T: SpringTarget,
        T::Output: 'static,
    {
        let SpringAnimation {
            config,
            target,
            epsilon,
            initial,
            playback,
        } = animation;
        let scalar_target = target.target();
        SpringAnimationElement {
            id: id.into(),
            element: Some(self),
            config,
            target: scalar_target,
            epsilon,
            initial,
            playback,
            animator: Some(Box::new(move |this, value| {
                animator(this, target.resolve(value))
            })),
        }
    }
}

impl<E: IntoElement + 'static> AnimationExt for E {}

/// A GPUI element that applies an animation to another element
pub struct AnimationElement<E> {
    id: ElementId,
    element: Option<E>,
    animations: SmallVec<[Animation; 1]>,
    animator: Box<dyn Fn(E, usize, f32) -> E + 'static>,
    cancel_handle: Option<Rc<Cell<bool>>>,
}

/// A GPUI element driven by a stateful spring.
pub struct SpringAnimationElement<E> {
    id: ElementId,
    element: Option<E>,
    config: SpringConfig,
    target: f32,
    epsilon: f32,
    initial: Option<f32>,
    playback: SpringPlayback,
    animator: Option<Box<dyn FnOnce(E, f32) -> E + 'static>>,
}

impl<E: ParentElement> ParentElement for SpringAnimationElement<E> {
    fn extend(&mut self, elements: impl IntoIterator<Item = AnyElement>) {
        let Some(element) = &mut self.element else {
            return;
        };

        element.extend(elements);
    }
}

impl<E> SpringAnimationElement<E> {
    /// Returns a new [`SpringAnimationElement<E>`] after applying the given function
    /// to the element being animated.
    pub fn map_element(mut self, f: impl FnOnce(E) -> E) -> SpringAnimationElement<E> {
        self.element = self.element.map(f);
        self
    }
}

impl<E: IntoElement + 'static> IntoElement for SpringAnimationElement<E> {
    type Element = SpringAnimationElement<E>;

    fn into_element(self) -> Self::Element {
        self
    }
}

impl<E: ParentElement> ParentElement for AnimationElement<E> {
    fn extend(&mut self, elements: impl IntoIterator<Item = AnyElement>) {
        let Some(element) = &mut self.element else {
            return;
        };

        element.extend(elements);
    }
}

impl<E> AnimationElement<E> {
    /// Returns a new [`AnimationElement<E>`] after applying the given function
    /// to the element being animated.
    pub fn map_element(mut self, f: impl FnOnce(E) -> E) -> AnimationElement<E> {
        self.element = self.element.map(f);
        self
    }
}

impl<E: IntoElement + 'static> IntoElement for AnimationElement<E> {
    type Element = AnimationElement<E>;

    fn into_element(self) -> Self::Element {
        self
    }
}

struct AnimationState {
    start: Instant,
    animation_ix: usize,
    /// Whether a repaint timer is already scheduled, so overlapping renders
    /// don't stack extra timers.
    repaint_pending: Rc<Cell<bool>>,
    /// Identifies the currently scheduled repaint timer. A newer animation
    /// phase invalidates older timers without needing to cancel their tasks.
    repaint_generation: Rc<Cell<u64>>,
}

/// Where a single animation is in its timeline for a given elapsed time,
/// ignoring easing.
#[derive(Clone, Copy, Debug, PartialEq)]
enum AnimationPass {
    /// The start delay has not elapsed yet; progress is zero.
    Delayed,
    /// The animation is running. `cycle_delta` is the normalized progress
    /// within the current pass, in `0.0..=1.0`.
    Running { cycle_delta: f32 },
    /// All passes are finished; progress is one.
    Finished,
}

fn animation_pass(animation: &Animation, elapsed: Duration) -> AnimationPass {
    if elapsed < animation.delay {
        return AnimationPass::Delayed;
    }

    let elapsed = elapsed - animation.delay;
    if animation.duration.is_zero() {
        return AnimationPass::Finished;
    }

    match animation.repeat {
        AnimationRepeat::Once => {
            if elapsed >= animation.duration {
                AnimationPass::Finished
            } else {
                AnimationPass::Running {
                    cycle_delta: elapsed.as_secs_f32() / animation.duration.as_secs_f32(),
                }
            }
        }
        AnimationRepeat::Forever => {
            let delta = elapsed.as_secs_f32() / animation.duration.as_secs_f32();
            AnimationPass::Running {
                cycle_delta: delta % 1.0,
            }
        }
        AnimationRepeat::Times(times) => {
            let times = times.max(1);
            let total_duration = animation
                .duration
                .checked_mul(times)
                .unwrap_or(Duration::MAX);
            if elapsed >= total_duration {
                AnimationPass::Finished
            } else {
                let delta = elapsed.as_secs_f32() / animation.duration.as_secs_f32();
                AnimationPass::Running {
                    cycle_delta: delta % 1.0,
                }
            }
        }
    }
}

fn schedule_repaint(
    window: &mut Window,
    cx: &mut App,
    delay: Duration,
    repaint_pending: &Rc<Cell<bool>>,
    repaint_generation: &Rc<Cell<u64>>,
) {
    if repaint_pending.get() {
        return;
    }

    repaint_pending.set(true);
    let generation = repaint_generation.get().wrapping_add(1);
    repaint_generation.set(generation);
    let repaint_pending = repaint_pending.clone();
    let repaint_generation = repaint_generation.clone();
    let view = window.current_view();
    window
        .spawn(cx, async move |cx| {
            cx.background_executor().timer(delay).await;
            if repaint_generation.get() != generation {
                return;
            }
            repaint_pending.set(false);
            cx.update(move |_, cx| cx.notify(view)).ok();
        })
        .detach();
}

fn invalidate_scheduled_repaint(state: &AnimationState) {
    state
        .repaint_generation
        .set(state.repaint_generation.get().wrapping_add(1));
    state.repaint_pending.set(false);
}

struct SpringElementState {
    spring: SpringState,
    target: f32,
    config: SpringConfig,
    initial: f32,
    playback: SpringPlayback,
    updated_at: Instant,
}

impl<E: IntoElement + 'static> Element for SpringAnimationElement<E> {
    type RequestLayoutState = AnyElement;
    type PrepaintState = ();

    fn id(&self) -> Option<ElementId> {
        Some(self.id.clone())
    }

    fn source_location(&self) -> Option<&'static core::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        global_id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (crate::LayoutId, Self::RequestLayoutState) {
        window.with_element_state(global_id.unwrap(), |state, window| {
            let now = Instant::now();
            let initial = self.initial.unwrap_or(self.target);
            let mut state = state.unwrap_or_else(|| SpringElementState {
                spring: SpringState {
                    position: initial,
                    velocity: 0.0,
                },
                target: self.target,
                config: self.config,
                initial,
                playback: self.playback,
                updated_at: now,
            });

            let elapsed = now.duration_since(state.updated_at).as_secs_f32();
            match state.playback {
                SpringPlayback::Running => {
                    state.spring = state.config.step(state.spring, state.target, elapsed);
                }
                SpringPlayback::Paused
                | SpringPlayback::Stopped
                | SpringPlayback::Completed
                | SpringPlayback::Cancelled => {}
            }

            state.config = self.config;
            state.target = self.target;

            let done = match self.playback {
                SpringPlayback::Running => {
                    let done = state
                        .config
                        .is_settled(state.spring, state.target, self.epsilon);
                    if done {
                        state.spring = SpringState {
                            position: state.target,
                            velocity: 0.0,
                        };
                    }
                    done
                }
                SpringPlayback::Paused => true,
                SpringPlayback::Stopped => {
                    state.spring.velocity = 0.0;
                    true
                }
                SpringPlayback::Completed => {
                    state.spring = SpringState {
                        position: state.target,
                        velocity: 0.0,
                    };
                    true
                }
                SpringPlayback::Cancelled => {
                    state.spring = SpringState {
                        position: state.initial,
                        velocity: 0.0,
                    };
                    true
                }
            };
            state.playback = self.playback;
            state.updated_at = now;

            let element = self.element.take().expect("should only be called once");
            let animator = self.animator.take().expect("should only be called once");
            let mut element = animator(element, state.spring.position).into_any_element();

            if !done {
                window.request_animation_frame();
            }

            ((element.request_layout(window, cx), element), state)
        })
    }

    fn prepaint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        _bounds: crate::Bounds<crate::Pixels>,
        element: &mut Self::RequestLayoutState,
        window: &mut Window,
        cx: &mut App,
    ) -> Self::PrepaintState {
        element.prepaint(window, cx);
    }

    fn paint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        _bounds: crate::Bounds<crate::Pixels>,
        element: &mut Self::RequestLayoutState,
        _: &mut Self::PrepaintState,
        window: &mut Window,
        cx: &mut App,
    ) {
        element.paint(window, cx);
    }
}

impl<E: IntoElement + 'static> Element for AnimationElement<E> {
    type RequestLayoutState = AnyElement;
    type PrepaintState = ();

    fn id(&self) -> Option<ElementId> {
        Some(self.id.clone())
    }

    fn source_location(&self) -> Option<&'static core::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        global_id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (crate::LayoutId, Self::RequestLayoutState) {
        window.with_element_state(global_id.unwrap(), |state, window| {
            let mut state = state.unwrap_or_else(|| AnimationState {
                start: Instant::now(),
                animation_ix: 0,
                repaint_pending: Rc::new(Cell::new(false)),
                repaint_generation: Rc::new(Cell::new(0)),
            });

            let cancelled = self.cancel_handle.as_ref().map_or(false, |h| h.get());

            let animation_ix = state.animation_ix;
            let animation = self.animations[animation_ix].clone();

            let (delta, done) = if cancelled {
                (1.0_f32, true)
            } else {
                let elapsed = state.start.elapsed();
                match animation_pass(&animation, elapsed) {
                    AnimationPass::Delayed => ((animation.easing)(0.0), false),
                    AnimationPass::Running { cycle_delta } => {
                        ((animation.easing)(cycle_delta), false)
                    }
                    AnimationPass::Finished => {
                        if animation_ix >= self.animations.len() - 1 {
                            ((animation.easing)(1.0), true)
                        } else {
                            state.start = Instant::now();
                            state.animation_ix += 1;
                            ((animation.easing)(1.0), false)
                        }
                    }
                }
            };

            debug_assert!(delta.is_finite(), "animated value should be finite");

            let element = self.element.take().expect("should only be called once");
            let mut element = (self.animator)(element, animation_ix, delta).into_any_element();

            let animation_changed = state.animation_ix != animation_ix;
            if animation_changed || done {
                // A timer for the previous animation must not delay the next
                // animation's delay or frame-rate schedule, or repaint after
                // the animation has already completed.
                invalidate_scheduled_repaint(&state);
            }

            if !done {
                let animation = &self.animations[state.animation_ix];
                let elapsed = state.start.elapsed();
                if elapsed < animation.delay {
                    let remaining = animation.delay - elapsed;
                    schedule_repaint(
                        window,
                        cx,
                        remaining,
                        &state.repaint_pending,
                        &state.repaint_generation,
                    );
                } else {
                    match animation.max_fps {
                        Some(max_fps) if max_fps.is_finite() && max_fps > 0.0 => {
                            let interval = Duration::from_secs_f32(1.0 / max_fps);
                            schedule_repaint(
                                window,
                                cx,
                                interval,
                                &state.repaint_pending,
                                &state.repaint_generation,
                            );
                        }
                        _ => window.request_animation_frame(),
                    }
                }
            }

            ((element.request_layout(window, cx), element), state)
        })
    }

    fn prepaint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        _bounds: crate::Bounds<crate::Pixels>,
        element: &mut Self::RequestLayoutState,
        window: &mut Window,
        cx: &mut App,
    ) -> Self::PrepaintState {
        element.prepaint(window, cx);
    }

    fn paint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        _bounds: crate::Bounds<crate::Pixels>,
        element: &mut Self::RequestLayoutState,
        _: &mut Self::PrepaintState,
        window: &mut Window,
        cx: &mut App,
    ) {
        element.paint(window, cx);
    }
}

mod easing {
    use std::f32::consts::PI;

    /// The linear easing function, or delta itself
    pub fn linear(delta: f32) -> f32 {
        delta
    }

    /// The quadratic easing function, delta * delta
    pub fn quadratic(delta: f32) -> f32 {
        delta * delta
    }

    /// The quadratic ease-in-out function, which starts and ends slowly but speeds up in the middle
    pub fn ease_in_out(delta: f32) -> f32 {
        if delta < 0.5 {
            2.0 * delta * delta
        } else {
            let x = -2.0 * delta + 2.0;
            1.0 - x * x / 2.0
        }
    }

    /// The Quint ease-out function, which starts quickly and decelerates to a stop
    pub fn ease_out_quint() -> impl Fn(f32) -> f32 {
        move |delta| 1.0 - (1.0 - delta).powi(5)
    }

    /// Apply the given easing function, first in the forward direction and then in the reverse direction
    pub fn bounce(easing: impl Fn(f32) -> f32) -> impl Fn(f32) -> f32 {
        move |delta| {
            if delta < 0.5 {
                easing(delta * 2.0)
            } else {
                easing((1.0 - delta) * 2.0)
            }
        }
    }

    /// A custom easing function for pulsating alpha that slows down as it approaches 0.1
    pub fn pulsating_between(min: f32, max: f32) -> impl Fn(f32) -> f32 {
        let range = max - min;

        move |delta| {
            // Use a combination of sine and cubic functions for a more natural breathing rhythm
            let t = (delta * 2.0 * PI).sin();
            let breath = (t * t * t + t) / 2.0;

            // Map the breath to our desired alpha range
            let normalized_alpha = (breath + 1.0) / 2.0;

            min + (normalized_alpha * range)
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::{InteractiveElement, Styled, div, px};

    use super::*;

    #[test]
    fn test_animation_parent() {
        div()
            .id("id")
            .with_animation(
                "animation",
                Animation::new(Duration::from_secs(1)),
                |el, _t| el,
            )
            .child(div());
    }

    #[test]
    fn test_spring_animation_parent() {
        div()
            .id("id")
            .with_spring(
                "spring-animation",
                SpringAnimation::new(SpringConfig::new(100.0, 10.0, 1.0))
                    .to(px(10.0))
                    .from(px(0.0)),
                |element, value| element.left(value),
            )
            .child(div());
    }

    #[test]
    fn test_with_max_fps_stores_positive_and_invalid_rates() {
        let capped = Animation::new(Duration::from_secs(1)).with_max_fps(12.0);
        assert_eq!(capped.max_fps, Some(12.0));

        let invalid = Animation::new(Duration::from_secs(1)).with_max_fps(0.0);
        assert_eq!(invalid.max_fps, Some(0.0));
    }

    #[test]
    fn test_with_delay_stores_delay() {
        let delayed = Animation::new(Duration::from_secs(1)).with_delay(Duration::from_millis(250));
        assert_eq!(delayed.delay, Duration::from_millis(250));

        let immediate = Animation::new(Duration::from_secs(1));
        assert_eq!(immediate.delay, Duration::ZERO);
    }

    #[test]
    fn test_repeat_builders() {
        assert_eq!(
            Animation::new(Duration::from_secs(1)).repeat,
            AnimationRepeat::Once
        );
        assert_eq!(
            Animation::new(Duration::from_secs(1)).repeat().repeat,
            AnimationRepeat::Forever
        );
        assert_eq!(
            Animation::new(Duration::from_secs(1)).repeat_n(3).repeat,
            AnimationRepeat::Times(3)
        );
        assert_eq!(
            Animation::new(Duration::from_secs(1))
                .with_repeat(AnimationRepeat::Times(2))
                .repeat,
            AnimationRepeat::Times(2)
        );
    }

    #[test]
    fn test_animation_pass_once() {
        let animation = Animation::new(Duration::from_secs(2));
        assert_eq!(
            animation_pass(&animation, Duration::ZERO),
            AnimationPass::Running { cycle_delta: 0.0 }
        );
        assert_eq!(
            animation_pass(&animation, Duration::from_secs(1)),
            AnimationPass::Running { cycle_delta: 0.5 }
        );
        assert_eq!(
            animation_pass(&animation, Duration::from_secs(2)),
            AnimationPass::Finished
        );
        assert_eq!(
            animation_pass(&animation, Duration::from_secs(3)),
            AnimationPass::Finished
        );
    }

    #[test]
    fn test_animation_pass_delay_holds_zero_then_runs() {
        let animation =
            Animation::new(Duration::from_secs(2)).with_delay(Duration::from_millis(500));
        assert_eq!(
            animation_pass(&animation, Duration::from_millis(499)),
            AnimationPass::Delayed
        );
        assert_eq!(
            animation_pass(&animation, Duration::from_millis(500)),
            AnimationPass::Running { cycle_delta: 0.0 }
        );
        assert_eq!(
            animation_pass(&animation, Duration::from_millis(1500)),
            AnimationPass::Running { cycle_delta: 0.5 }
        );
        assert_eq!(
            animation_pass(&animation, Duration::from_millis(2600)),
            AnimationPass::Finished
        );
    }

    #[test]
    fn test_animation_pass_forever_wraps() {
        let animation = Animation::new(Duration::from_secs(1)).repeat();
        assert_eq!(
            animation_pass(&animation, Duration::from_millis(1500)),
            AnimationPass::Running { cycle_delta: 0.5 }
        );
        assert_eq!(
            animation_pass(&animation, Duration::from_secs(3)),
            AnimationPass::Running { cycle_delta: 0.0 }
        );
    }

    #[test]
    fn test_animation_pass_finite_times() {
        let animation = Animation::new(Duration::from_secs(1)).repeat_n(3);
        assert_eq!(
            animation_pass(&animation, Duration::from_millis(1500)),
            AnimationPass::Running { cycle_delta: 0.5 }
        );
        // Third and final pass.
        assert_eq!(
            animation_pass(&animation, Duration::from_millis(2500)),
            AnimationPass::Running { cycle_delta: 0.5 }
        );
        // Finish exactly when the final pass completes.
        assert_eq!(
            animation_pass(&animation, Duration::from_secs(3)),
            AnimationPass::Finished
        );
        assert_eq!(
            animation_pass(&animation, Duration::from_millis(3100)),
            AnimationPass::Finished
        );
    }

    #[test]
    fn test_animation_pass_times_zero_clamps_to_once() {
        let animation = Animation::new(Duration::from_secs(1)).repeat_n(0);
        assert_eq!(
            animation_pass(&animation, Duration::from_millis(500)),
            AnimationPass::Running { cycle_delta: 0.5 }
        );
        assert_eq!(
            animation_pass(&animation, Duration::from_millis(1500)),
            AnimationPass::Finished
        );
    }

    #[test]
    fn test_animation_pass_zero_duration_finishes_immediately() {
        let animation = Animation::new(Duration::ZERO);

        assert_eq!(
            animation_pass(&animation, Duration::ZERO),
            AnimationPass::Finished
        );
        assert_eq!(
            animation_pass(&animation.repeat(), Duration::from_secs(1)),
            AnimationPass::Finished
        );
    }
}

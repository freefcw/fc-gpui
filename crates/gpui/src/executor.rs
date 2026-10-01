use crate::{ActivityGuard, App, PlatformDispatcher, profiler};
use async_task::Runnable;
use futures::channel::mpsc;
use smol::prelude::*;
use std::mem::ManuallyDrop;
use std::panic::Location;
use std::thread::{self, ThreadId};
use std::{
    fmt::Debug,
    marker::PhantomData,
    mem,
    num::NonZeroUsize,
    pin::Pin,
    rc::Rc,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering::SeqCst},
    },
    task::{Context, Poll},
    time::{Duration, Instant},
};
use util::TryFutureExt;
use waker_fn::waker_fn;

#[cfg(any(test, feature = "test-support"))]
use rand::rngs::StdRng;

/// A pointer to the executor that is currently running,
/// for spawning background tasks.
#[derive(Clone)]
pub struct BackgroundExecutor {
    #[doc(hidden)]
    pub dispatcher: Arc<dyn PlatformDispatcher>,
}

/// A pointer to the executor that is currently running,
/// for spawning tasks on the main thread.
///
/// This is intentionally `!Send` via the `not_send` marker field. This is because
/// `ForegroundExecutor::spawn` does not require `Send` but checks at runtime that the future is
/// only polled from the same thread it was spawned from. These checks would fail when spawning
/// foreground tasks from from background threads.
#[derive(Clone)]
pub struct ForegroundExecutor {
    #[doc(hidden)]
    pub dispatcher: Arc<dyn PlatformDispatcher>,
    not_send: PhantomData<Rc<()>>,
}

/// Task is a primitive that allows work to happen in the background.
///
/// It implements [`Future`] so you can `.await` on it.
///
/// If you drop a task it will be cancelled immediately. Calling [`Task::detach`] allows
/// the task to continue running, but with no way to return a value.
#[must_use]
#[derive(Debug)]
pub struct Task<T>(TaskState<T>);

#[derive(Debug)]
enum TaskState<T> {
    /// A task that is ready to return a value
    Ready(Option<T>),

    /// A task that is currently running.
    Spawned(async_task::Task<T>),
}

impl<T> Task<T> {
    /// Creates a new task that will resolve with the value
    pub fn ready(val: T) -> Self {
        Task(TaskState::Ready(Some(val)))
    }

    /// Detaching a task runs it to completion in the background
    pub fn detach(self) {
        match self {
            Task(TaskState::Ready(_)) => {}
            Task(TaskState::Spawned(task)) => task.detach(),
        }
    }
}

impl<E, T> Task<Result<T, E>>
where
    T: 'static,
    E: 'static + Debug,
{
    /// Run the task to completion in the background and log any
    /// errors that occur.
    #[track_caller]
    pub fn detach_and_log_err(self, cx: &App) {
        let location = core::panic::Location::caller();
        cx.foreground_executor()
            .spawn(self.log_tracked_err(*location))
            .detach();
    }
}

impl<T> Future for Task<T> {
    type Output = T;

    fn poll(self: Pin<&mut Self>, cx: &mut Context) -> Poll<Self::Output> {
        match unsafe { self.get_unchecked_mut() } {
            Task(TaskState::Ready(val)) => Poll::Ready(val.take().unwrap()),
            Task(TaskState::Spawned(task)) => task.poll(cx),
        }
    }
}

/// The scheduling priority for a task.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum TaskPriority {
    /// Work that should be preferred over ordinary background work.
    High,
    /// Ordinary background work. This is the default and matches existing behavior.
    #[default]
    Medium,
    /// Work that can run after ordinary background work.
    Low,
}

/// A task label is an opaque identifier that you can use to refer to a task in tests.
#[derive(Clone, Copy, Debug, Eq)]
pub struct TaskLabel {
    id: NonZeroUsize,
    priority: TaskPriority,
}

impl PartialEq for TaskLabel {
    fn eq(&self, other: &Self) -> bool {
        self.id == other.id
    }
}

impl std::hash::Hash for TaskLabel {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        self.id.hash(state);
    }
}

impl Default for TaskLabel {
    fn default() -> Self {
        Self::new()
    }
}

impl TaskLabel {
    /// Construct a new task label.
    pub fn new() -> Self {
        Self::new_with_priority(TaskPriority::Medium)
    }

    fn new_with_priority(priority: TaskPriority) -> Self {
        static NEXT_TASK_LABEL: AtomicUsize = AtomicUsize::new(1);
        Self {
            id: NEXT_TASK_LABEL.fetch_add(1, SeqCst).try_into().unwrap(),
            priority,
        }
    }

    #[doc(hidden)]
    pub fn priority(self) -> TaskPriority {
        self.priority
    }
}

type AnyLocalFuture<R> = Pin<Box<dyn 'static + Future<Output = R>>>;

type AnyFuture<R> = Pin<Box<dyn 'static + Send + Future<Output = R>>>;

struct TimedFuture<F> {
    inner: F,
    location: &'static Location<'static>,
    started: bool,
    timing: Option<profiler::TaskTimingHandle>,
}

impl<F> TimedFuture<F> {
    fn new(inner: F, location: &'static Location<'static>) -> Self {
        Self {
            inner,
            location,
            started: false,
            timing: None,
        }
    }
}

impl<F: Future> Future for TimedFuture<F> {
    type Output = F::Output;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let this = unsafe { self.get_unchecked_mut() };
        if !this.started {
            this.started = true;
            this.timing = profiler::record_start(this.location);
        }

        let result = unsafe { Pin::new_unchecked(&mut this.inner) }.poll(cx);
        if result.is_ready()
            && let Some(timing) = this.timing.take()
        {
            profiler::record_end(timing, profiler::TaskTimingStatus::Completed);
        }
        result
    }
}

impl<F> Drop for TimedFuture<F> {
    fn drop(&mut self) {
        if let Some(timing) = self.timing.take() {
            profiler::record_end(timing, profiler::TaskTimingStatus::Cancelled);
        }
    }
}

/// BackgroundExecutor lets you run things on background threads.
/// In production this is a thread pool with no ordering guarantees.
/// In tests this is simulated by running tasks one by one in a deterministic
/// (but arbitrary) order controlled by the `SEED` environment variable.
impl BackgroundExecutor {
    #[doc(hidden)]
    pub fn new(dispatcher: Arc<dyn PlatformDispatcher>) -> Self {
        Self { dispatcher }
    }

    /// Prevents App Nap-style throttling while the returned guard is held.
    ///
    /// This does not prevent the system from entering idle sleep.
    pub fn prevent_app_nap(&self, reason: &str) -> ActivityGuard {
        self.dispatcher.prevent_app_nap(reason)
    }

    /// Enqueues the given future to be run to completion on a background thread.
    #[track_caller]
    pub fn spawn<R>(&self, future: impl Future<Output = R> + Send + 'static) -> Task<R>
    where
        R: Send + 'static,
    {
        self.spawn_internal::<R>(Box::pin(future), None)
    }

    /// Enqueues the given future to be run to completion on a background thread.
    /// The given label can be used to control the priority of the task in tests.
    #[track_caller]
    pub fn spawn_labeled<R>(
        &self,
        label: TaskLabel,
        future: impl Future<Output = R> + Send + 'static,
    ) -> Task<R>
    where
        R: Send + 'static,
    {
        self.spawn_internal::<R>(Box::pin(future), Some(label))
    }

    #[track_caller]
    #[doc(hidden)]
    pub fn spawn_with_priority<R>(
        &self,
        priority: TaskPriority,
        future: impl Future<Output = R> + Send + 'static,
    ) -> Task<R>
    where
        R: Send + 'static,
    {
        self.spawn_internal::<R>(
            Box::pin(future),
            Some(TaskLabel::new_with_priority(priority)),
        )
    }

    #[track_caller]
    fn spawn_internal<R: Send + 'static>(
        &self,
        future: AnyFuture<R>,
        label: Option<TaskLabel>,
    ) -> Task<R> {
        let dispatcher = self.dispatcher.clone();
        let future = TimedFuture::new(future, Location::caller());
        let (runnable, task) =
            async_task::spawn(future, move |runnable| dispatcher.dispatch(runnable, label));
        runnable.schedule();
        Task(TaskState::Spawned(task))
    }

    /// Used by the test harness to run an async test in a synchronous fashion.
    #[cfg(any(test, feature = "test-support"))]
    #[track_caller]
    pub fn block_test<R>(&self, future: impl Future<Output = R>) -> R {
        if let Ok(value) = self.block_internal(false, future, None) {
            value
        } else {
            unreachable!()
        }
    }

    /// Block the current thread until the given future resolves.
    /// Consider using `block_with_timeout` instead.
    pub fn block<R>(&self, future: impl Future<Output = R>) -> R {
        if let Ok(value) = self.block_internal(true, future, None) {
            value
        } else {
            unreachable!()
        }
    }

    #[cfg(not(any(test, feature = "test-support")))]
    pub(crate) fn block_internal<Fut: Future>(
        &self,
        _background_only: bool,
        future: Fut,
        timeout: Option<Duration>,
    ) -> Result<Fut::Output, impl Future<Output = Fut::Output> + use<Fut>> {
        use std::time::Instant;

        let mut future = Box::pin(future);
        if timeout == Some(Duration::ZERO) {
            return Err(future);
        }
        let deadline = timeout.map(|timeout| Instant::now() + timeout);

        let unparker = self.dispatcher.unparker();
        let waker = waker_fn(move || {
            unparker.unpark();
        });
        let mut cx = std::task::Context::from_waker(&waker);

        loop {
            match future.as_mut().poll(&mut cx) {
                Poll::Ready(result) => return Ok(result),
                Poll::Pending => {
                    let timeout =
                        deadline.map(|deadline| deadline.saturating_duration_since(Instant::now()));
                    if !self.dispatcher.park(timeout)
                        && deadline.is_some_and(|deadline| deadline < Instant::now())
                    {
                        return Err(future);
                    }
                }
            }
        }
    }

    #[cfg(any(test, feature = "test-support"))]
    #[track_caller]
    pub(crate) fn block_internal<Fut: Future>(
        &self,
        background_only: bool,
        future: Fut,
        timeout: Option<Duration>,
    ) -> Result<Fut::Output, impl Future<Output = Fut::Output> + use<Fut>> {
        use std::sync::atomic::AtomicBool;

        let mut future = Box::pin(future);
        if timeout == Some(Duration::ZERO) {
            return Err(future);
        }
        let Some(dispatcher) = self.dispatcher.as_test() else {
            return Err(future);
        };

        let mut max_ticks = if timeout.is_some() {
            dispatcher.gen_block_on_ticks()
        } else {
            usize::MAX
        };
        let unparker = self.dispatcher.unparker();
        let awoken = Arc::new(AtomicBool::new(false));
        let waker = waker_fn({
            let awoken = awoken.clone();
            move || {
                awoken.store(true, SeqCst);
                unparker.unpark();
            }
        });
        let mut cx = std::task::Context::from_waker(&waker);

        loop {
            match future.as_mut().poll(&mut cx) {
                Poll::Ready(result) => return Ok(result),
                Poll::Pending => {
                    if max_ticks == 0 {
                        return Err(future);
                    }
                    max_ticks -= 1;

                    if !dispatcher.tick(background_only) {
                        if awoken.swap(false, SeqCst) {
                            continue;
                        }

                        if !dispatcher.parking_allowed() {
                            if dispatcher.advance_clock_to_next_delayed() {
                                continue;
                            }
                            let mut backtrace_message = String::new();
                            let mut waiting_message = String::new();
                            if let Some(backtrace) = dispatcher.waiting_backtrace() {
                                backtrace_message =
                                    format!("\nbacktrace of waiting future:\n{:?}", backtrace);
                            }
                            if let Some(waiting_hint) = dispatcher.waiting_hint() {
                                waiting_message = format!("\n  waiting on: {}\n", waiting_hint);
                            }
                            panic!(
                                "parked with nothing left to run{waiting_message}{backtrace_message}",
                            )
                        }
                        self.dispatcher.park(None);
                    }
                }
            }
        }
    }

    /// Block the current thread until the given future resolves
    /// or `duration` has elapsed.
    pub fn block_with_timeout<Fut: Future>(
        &self,
        duration: Duration,
        future: Fut,
    ) -> Result<Fut::Output, impl Future<Output = Fut::Output> + use<Fut>> {
        self.block_internal(true, future, Some(duration))
    }

    /// Scoped lets you start a number of tasks and waits
    /// for all of them to complete before returning.
    pub async fn scoped<'scope, F>(&self, scheduler: F)
    where
        F: FnOnce(&mut Scope<'scope>),
    {
        let mut scope = Scope::new(self.clone());
        (scheduler)(&mut scope);
        let spawned = mem::take(&mut scope.futures)
            .into_iter()
            .map(|f| self.spawn(f))
            .collect::<Vec<_>>();
        for task in spawned {
            task.await;
        }
    }

    /// Get the current time.
    ///
    /// Calling this instead of `std::time::Instant::now` allows the use
    /// of fake timers in tests.
    pub fn now(&self) -> Instant {
        self.dispatcher.now()
    }

    /// Returns a task that will complete after the given duration.
    /// Depending on other concurrent tasks the elapsed duration may be longer
    /// than requested.
    #[track_caller]
    pub fn timer(&self, duration: Duration) -> Task<()> {
        if duration.is_zero() {
            return Task::ready(());
        }
        let future = TimedFuture::new(Box::pin(async move {}), Location::caller());
        let (runnable, task) = async_task::spawn(future, {
            let dispatcher = self.dispatcher.clone();
            move |runnable| dispatcher.dispatch_after(duration, runnable)
        });
        runnable.schedule();
        Task(TaskState::Spawned(task))
    }

    /// in tests, start_waiting lets you indicate which task is waiting (for debugging only)
    #[cfg(any(test, feature = "test-support"))]
    pub fn start_waiting(&self) {
        self.dispatcher.as_test().unwrap().start_waiting();
    }

    /// in tests, removes the debugging data added by start_waiting
    #[cfg(any(test, feature = "test-support"))]
    pub fn finish_waiting(&self) {
        self.dispatcher.as_test().unwrap().finish_waiting();
    }

    /// in tests, run an arbitrary number of tasks (determined by the SEED environment variable)
    #[cfg(any(test, feature = "test-support"))]
    pub fn simulate_random_delay(&self) -> impl Future<Output = ()> + use<> {
        self.dispatcher.as_test().unwrap().simulate_random_delay()
    }

    /// in tests, indicate that a given task from `spawn_labeled` should run after everything else
    #[cfg(any(test, feature = "test-support"))]
    pub fn deprioritize(&self, task_label: TaskLabel) {
        self.dispatcher.as_test().unwrap().deprioritize(task_label)
    }

    /// in tests, move time forward. This does not run any tasks, but does make `timer`s ready.
    #[cfg(any(test, feature = "test-support"))]
    pub fn advance_clock(&self, duration: Duration) {
        self.dispatcher.as_test().unwrap().advance_clock(duration)
    }

    /// in tests, run one task.
    #[cfg(any(test, feature = "test-support"))]
    pub fn tick(&self) -> bool {
        self.dispatcher.as_test().unwrap().tick(false)
    }

    /// in tests, run all tasks that are ready to run. If after doing so
    /// the test still has outstanding tasks, this will panic. (See also [`Self::allow_parking`])
    #[cfg(any(test, feature = "test-support"))]
    pub fn run_until_parked(&self) {
        self.dispatcher.as_test().unwrap().run_until_parked()
    }

    /// in tests, prevents `run_until_parked` from panicking if there are outstanding tasks.
    /// This is useful when you are integrating other (non-GPUI) futures, like disk access, that
    /// do take real async time to run.
    #[cfg(any(test, feature = "test-support"))]
    pub fn allow_parking(&self) {
        self.dispatcher.as_test().unwrap().allow_parking();
    }

    /// undoes the effect of [`Self::allow_parking`].
    #[cfg(any(test, feature = "test-support"))]
    pub fn forbid_parking(&self) {
        self.dispatcher.as_test().unwrap().forbid_parking();
    }

    /// adds detail to the "parked with nothing let to run" message.
    #[cfg(any(test, feature = "test-support"))]
    pub fn set_waiting_hint(&self, msg: Option<String>) {
        self.dispatcher.as_test().unwrap().set_waiting_hint(msg);
    }

    /// in tests, returns the rng used by the dispatcher and seeded by the `SEED` environment variable
    #[cfg(any(test, feature = "test-support"))]
    pub fn rng(&self) -> StdRng {
        self.dispatcher.as_test().unwrap().rng()
    }

    /// How many CPUs are available to the dispatcher.
    pub fn num_cpus(&self) -> usize {
        #[cfg(any(test, feature = "test-support"))]
        return 4;

        #[cfg(not(any(test, feature = "test-support")))]
        return num_cpus::get();
    }

    /// Whether we're on the main thread.
    pub fn is_main_thread(&self) -> bool {
        self.dispatcher.is_main_thread()
    }

    #[cfg(any(test, feature = "test-support"))]
    /// in tests, control the number of ticks that `block_with_timeout` will run before timing out.
    pub fn set_block_on_ticks(&self, range: std::ops::RangeInclusive<usize>) {
        self.dispatcher.as_test().unwrap().set_block_on_ticks(range);
    }
}

/// ForegroundExecutor runs things on the main thread.
impl ForegroundExecutor {
    /// Creates a new ForegroundExecutor from the given PlatformDispatcher.
    pub fn new(dispatcher: Arc<dyn PlatformDispatcher>) -> Self {
        Self {
            dispatcher,
            not_send: PhantomData,
        }
    }

    /// Enqueues the given Task to run on the main thread at some point in the future.
    #[track_caller]
    pub fn spawn<R>(&self, future: impl Future<Output = R> + 'static) -> Task<R>
    where
        R: 'static,
    {
        let dispatcher = self.dispatcher.clone();

        #[track_caller]
        fn inner<R: 'static>(
            dispatcher: Arc<dyn PlatformDispatcher>,
            future: AnyLocalFuture<R>,
        ) -> Task<R> {
            let future = TimedFuture::new(future, Location::caller());
            let (runnable, task) = spawn_local_with_source_location(future, move |runnable| {
                dispatcher.dispatch_on_main_thread(runnable)
            });
            runnable.schedule();
            Task(TaskState::Spawned(task))
        }
        inner::<R>(dispatcher, Box::pin(future))
    }
}

/// Variant of `async_task::spawn_local` that includes the source location of the spawn in panics.
///
/// Copy-modified from:
/// <https://github.com/smol-rs/async-task/blob/ca9dbe1db9c422fd765847fa91306e30a6bb58a9/src/runnable.rs#L405>
#[track_caller]
fn spawn_local_with_source_location<Fut, S>(
    future: Fut,
    schedule: S,
) -> (Runnable<()>, async_task::Task<Fut::Output, ()>)
where
    Fut: Future + 'static,
    Fut::Output: 'static,
    S: async_task::Schedule<()> + Send + Sync + 'static,
{
    #[inline]
    fn thread_id() -> ThreadId {
        std::thread_local! {
            static ID: ThreadId = thread::current().id();
        }
        ID.try_with(|id| *id)
            .unwrap_or_else(|_| thread::current().id())
    }

    struct Checked<F> {
        id: ThreadId,
        inner: ManuallyDrop<F>,
        location: &'static Location<'static>,
    }

    impl<F> Drop for Checked<F> {
        fn drop(&mut self) {
            assert!(
                self.id == thread_id(),
                "local task dropped by a thread that didn't spawn it. Task spawned at {}",
                self.location
            );
            unsafe {
                ManuallyDrop::drop(&mut self.inner);
            }
        }
    }

    impl<F: Future> Future for Checked<F> {
        type Output = F::Output;

        fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
            assert!(
                self.id == thread_id(),
                "local task polled by a thread that didn't spawn it. Task spawned at {}",
                self.location
            );
            unsafe { self.map_unchecked_mut(|c| &mut *c.inner).poll(cx) }
        }
    }

    // Wrap the future into one that checks which thread it's on.
    let future = Checked {
        id: thread_id(),
        inner: ManuallyDrop::new(future),
        location: Location::caller(),
    };

    unsafe { async_task::spawn_unchecked(future, schedule) }
}

/// Scope manages a set of tasks that are enqueued and waited on together. See [`BackgroundExecutor::scoped`].
pub struct Scope<'a> {
    executor: BackgroundExecutor,
    futures: Vec<Pin<Box<dyn Future<Output = ()> + Send + 'static>>>,
    tx: Option<mpsc::Sender<()>>,
    rx: mpsc::Receiver<()>,
    lifetime: PhantomData<&'a ()>,
}

impl<'a> Scope<'a> {
    fn new(executor: BackgroundExecutor) -> Self {
        let (tx, rx) = mpsc::channel(1);
        Self {
            executor,
            tx: Some(tx),
            rx,
            futures: Default::default(),
            lifetime: PhantomData,
        }
    }

    /// How many CPUs are available to the dispatcher.
    pub fn num_cpus(&self) -> usize {
        self.executor.num_cpus()
    }

    /// Spawn a future into this scope.
    pub fn spawn<F>(&mut self, f: F)
    where
        F: Future<Output = ()> + Send + 'a,
    {
        let tx = self.tx.clone().unwrap();

        // SAFETY: The 'a lifetime is guaranteed to outlive any of these futures because
        // dropping this `Scope` blocks until all of the futures have resolved.
        let f = unsafe {
            mem::transmute::<
                Pin<Box<dyn Future<Output = ()> + Send + 'a>>,
                Pin<Box<dyn Future<Output = ()> + Send + 'static>>,
            >(Box::pin(async move {
                f.await;
                drop(tx);
            }))
        };
        self.futures.push(f);
    }
}

impl Drop for Scope<'_> {
    fn drop(&mut self) {
        self.tx.take().unwrap();

        // Wait until the channel is closed, which means that all of the spawned
        // futures have resolved.
        self.executor.block(self.rx.next());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{TestAppContext, profiler};
    use std::sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    };

    fn reset_profiler() {
        profiler::set_enabled(false);
    }

    fn collect_timings() -> Vec<profiler::ThreadTimingsDelta> {
        let mut collector = profiler::ProfilingCollector::new(Instant::now());
        profiler::profiler_collect_timings(&mut collector)
    }

    #[test]
    fn profiler_disabled_records_nothing() {
        let _guard = profiler::test_guard();
        reset_profiler();

        let mut cx = TestAppContext::single();
        cx.executor().spawn(async {}).detach();
        cx.run_until_parked();

        assert!(collect_timings().is_empty());
        reset_profiler();
    }

    #[test]
    fn profiler_enabled_records_background_task_timing() {
        let _guard = profiler::test_guard();
        reset_profiler();
        profiler::set_enabled(true);

        let mut cx = TestAppContext::single();
        let mut collector = profiler::ProfilingCollector::new(Instant::now());
        cx.executor().spawn(async {}).detach();
        cx.run_until_parked();

        let deltas = profiler::profiler_collect_timings(&mut collector);
        let timing = deltas
            .iter()
            .flat_map(|delta| &delta.new_timings)
            .next()
            .expect("expected a recorded background task timing");
        assert_eq!(timing.status, profiler::TaskTimingStatus::Completed);

        reset_profiler();
    }

    #[test]
    fn profiler_enabled_records_foreground_task_location() {
        let _guard = profiler::test_guard();
        reset_profiler();
        profiler::set_enabled(true);

        let mut cx = TestAppContext::single();
        let mut collector = profiler::ProfilingCollector::new(Instant::now());
        cx.foreground_executor().spawn(async {}).detach();
        cx.run_until_parked();

        let deltas = profiler::profiler_collect_timings(&mut collector);
        let timing = deltas
            .iter()
            .flat_map(|delta| &delta.new_timings)
            .find(|timing| timing.location.file.as_ref().contains("executor.rs"))
            .expect("expected foreground task timing to include its source location");
        assert!(timing.location.line > 0);
        assert!(timing.location.column > 0);

        reset_profiler();
    }

    #[test]
    fn profiler_timing_start_is_first_poll_not_spawn() {
        let _guard = profiler::test_guard();
        reset_profiler();
        profiler::set_enabled(true);

        let mut cx = TestAppContext::single();
        let mut collector = profiler::ProfilingCollector::new(Instant::now());
        cx.executor().spawn(async {}).detach();
        assert!(profiler::profiler_collect_timings(&mut collector).is_empty());

        cx.run_until_parked();
        assert!(!profiler::profiler_collect_timings(&mut collector).is_empty());

        reset_profiler();
    }

    #[test]
    fn baseline_foreground_task_runs_on_main_thread() {
        let mut cx = TestAppContext::single();
        let executor = cx.executor();
        let ran_on_main_thread = Arc::new(AtomicBool::new(false));

        cx.foreground_executor()
            .spawn({
                let ran_on_main_thread = ran_on_main_thread.clone();
                async move {
                    ran_on_main_thread.store(executor.is_main_thread(), Ordering::SeqCst);
                }
            })
            .detach();
        cx.run_until_parked();

        assert!(ran_on_main_thread.load(Ordering::SeqCst));
    }

    #[test]
    fn baseline_background_task_completes_after_run_until_parked() {
        let mut cx = TestAppContext::single();
        let completed = Arc::new(AtomicBool::new(false));

        cx.executor()
            .spawn({
                let completed = completed.clone();
                async move {
                    completed.store(true, Ordering::SeqCst);
                }
            })
            .detach();
        cx.run_until_parked();

        assert!(completed.load(Ordering::SeqCst));
    }

    #[test]
    fn baseline_timer_fires_after_advance_clock() {
        let cx = TestAppContext::single();
        let executor = cx.executor();
        let fired = Arc::new(AtomicBool::new(false));

        executor
            .spawn({
                let executor = executor.clone();
                let fired = fired.clone();
                async move {
                    executor.timer(Duration::from_millis(10)).await;
                    fired.store(true, Ordering::SeqCst);
                }
            })
            .detach();

        executor.advance_clock(Duration::from_millis(10));
        assert!(fired.load(Ordering::SeqCst));
    }

    #[test]
    fn baseline_deprioritize_delays_labeled_task() {
        let mut cx = TestAppContext::single();
        let executor = cx.executor();
        let label = TaskLabel::new();
        let order = Arc::new(Mutex::new(Vec::new()));

        executor.deprioritize(label);
        executor
            .spawn_labeled(label, {
                let order = order.clone();
                async move {
                    order.lock().unwrap().push("labeled");
                }
            })
            .detach();
        executor
            .spawn({
                let order = order.clone();
                async move {
                    order.lock().unwrap().push("normal");
                }
            })
            .detach();
        cx.run_until_parked();

        assert_eq!(&*order.lock().unwrap(), &["normal", "labeled"]);
    }

    #[test]
    fn baseline_block_with_timeout_returns_bounded_pending_future() {
        let cx = TestAppContext::single();
        let executor = cx.executor();
        executor.set_block_on_ticks(0..=0);

        let result =
            executor.block_with_timeout(Duration::from_millis(1), futures::future::pending::<()>());

        assert!(result.is_err());
    }

    #[test]
    fn baseline_headless_app_tasks_complete_without_window() {
        let mut cx = TestAppContext::single();
        let completed = Arc::new(AtomicBool::new(false));

        cx.executor()
            .spawn({
                let completed = completed.clone();
                async move {
                    completed.store(true, Ordering::SeqCst);
                }
            })
            .detach();
        cx.run_until_parked();

        assert!(completed.load(Ordering::SeqCst));
        assert!(cx.windows().is_empty());
    }

    #[test]
    fn baseline_scoped_tasks_complete_before_scope_returns() {
        let cx = TestAppContext::single();
        let executor = cx.executor();
        let completed = Arc::new(AtomicBool::new(false));

        executor.block_test({
            let executor = executor.clone();
            let completed = completed.clone();
            async move {
                executor
                    .scoped(|scope| {
                        scope.spawn(async move {
                            completed.store(true, Ordering::SeqCst);
                        });
                    })
                    .await;
            }
        });

        assert!(completed.load(Ordering::SeqCst));
    }

    #[test]
    fn spawn_with_priority_defaults_to_medium_behavior() {
        let mut cx = TestAppContext::single();
        let completed = Arc::new(AtomicBool::new(false));

        cx.executor()
            .spawn_with_priority(TaskPriority::Medium, {
                let completed = completed.clone();
                async move {
                    completed.store(true, Ordering::SeqCst);
                }
            })
            .detach();
        cx.run_until_parked();

        assert!(completed.load(Ordering::SeqCst));
    }

    #[test]
    fn spawn_with_priority_high_is_observable_in_test_dispatcher() {
        let mut cx = TestAppContext::single();
        let executor = cx.executor();
        let order = Arc::new(Mutex::new(Vec::new()));

        executor
            .spawn_with_priority(TaskPriority::Low, {
                let order = order.clone();
                async move {
                    order.lock().unwrap().push("low");
                }
            })
            .detach();
        executor
            .spawn_with_priority(TaskPriority::High, {
                let order = order.clone();
                async move {
                    order.lock().unwrap().push("high");
                }
            })
            .detach();
        cx.run_until_parked();

        assert_eq!(&*order.lock().unwrap(), &["high", "low"]);
    }

    #[test]
    fn spawn_labeled_deprioritize_still_works_after_priority_addition() {
        baseline_deprioritize_delays_labeled_task();
    }

    #[test]
    fn low_priority_task_eventually_runs() {
        let mut cx = TestAppContext::single();
        let completed = Arc::new(AtomicBool::new(false));

        cx.executor()
            .spawn_with_priority(TaskPriority::Low, {
                let completed = completed.clone();
                async move {
                    completed.store(true, Ordering::SeqCst);
                }
            })
            .detach();
        cx.run_until_parked();

        assert!(completed.load(Ordering::SeqCst));
    }
}

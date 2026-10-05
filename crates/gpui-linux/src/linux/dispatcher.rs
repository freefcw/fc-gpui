use async_task::Runnable;
use calloop::{
    EventLoop,
    channel::{self, Sender},
    timer::TimeoutAction,
};
use gpui::{PlatformDispatcher, TaskLabel};
use parking::{Parker, Unparker};
use parking_lot::Mutex;
use std::{
    thread,
    time::{Duration, Instant},
};
use util::ResultExt;

struct TimerAfter {
    duration: Duration,
    runnable: Runnable,
}

pub(crate) struct LinuxDispatcher {
    parker: Mutex<Parker>,
    main_sender: Sender<Runnable>,
    timer_sender: Sender<TimerAfter>,
    background_sender: flume::Sender<Runnable>,
    _background_threads: Vec<thread::JoinHandle<()>>,
    main_thread_id: thread::ThreadId,
}

impl LinuxDispatcher {
    pub fn new(main_sender: Sender<Runnable>) -> Self {
        let (background_sender, background_receiver) = flume::unbounded::<Runnable>();
        let thread_count = std::thread::available_parallelism()
            .map(|i| i.get())
            .unwrap_or(1);

        let mut background_threads = (0..thread_count)
            .map(|i| {
                let receiver = background_receiver.clone();
                std::thread::Builder::new()
                    .name(format!("Worker-{i}"))
                    .spawn(move || {
                        for runnable in receiver {
                            let start = Instant::now();

                            runnable.run();

                            log::trace!(
                                "background thread {}: ran runnable. took: {:?}",
                                i,
                                start.elapsed()
                            );
                        }
                    })
                    .unwrap()
            })
            .collect::<Vec<_>>();

        let (timer_sender, timer_channel) = calloop::channel::channel::<TimerAfter>();
        let timer_thread = std::thread::Builder::new()
            .name("Timer".to_owned())
            .spawn(|| {
                let mut event_loop: EventLoop<()> =
                    EventLoop::try_new().expect("Failed to initialize timer loop!");

                let handle = event_loop.handle();
                let timer_handle = event_loop.handle();
                let signal = event_loop.get_signal();
                handle
                    .insert_source(timer_channel, move |e, _, _| {
                        // The dispatcher owning the sender is gone; timers already
                        // scheduled would run tasks nothing can observe.
                        if let channel::Event::Closed = e {
                            signal.stop();
                        }
                        if let channel::Event::Msg(timer) = e {
                            // This has to be in an option to satisfy the borrow checker. The callback below should only be scheduled once.
                            let mut runnable = Some(timer.runnable);
                            timer_handle
                                .insert_source(
                                    calloop::timer::Timer::from_duration(timer.duration),
                                    move |_, _, _| {
                                        if let Some(runnable) = runnable.take() {
                                            runnable.run();
                                        }
                                        TimeoutAction::Drop
                                    },
                                )
                                .expect("Failed to start timer");
                        }
                    })
                    .expect("Failed to start timer thread");

                event_loop.run(None, &mut (), |_| {}).log_err();
            })
            .unwrap();

        background_threads.push(timer_thread);

        Self {
            parker: Mutex::new(Parker::new()),
            main_sender,
            timer_sender,
            background_sender,
            _background_threads: background_threads,
            main_thread_id: thread::current().id(),
        }
    }
}

impl PlatformDispatcher for LinuxDispatcher {
    fn is_main_thread(&self) -> bool {
        thread::current().id() == self.main_thread_id
    }

    fn dispatch(&self, runnable: Runnable, _: Option<TaskLabel>) {
        self.background_sender.send(runnable).unwrap();
    }

    fn dispatch_on_main_thread(&self, runnable: Runnable) {
        self.main_sender.send(runnable).unwrap_or_else(|runnable| {
            // NOTE: Runnable may wrap a Future that is !Send.
            //
            // This is usually safe because we only poll it on the main thread.
            // However if the send fails, we know that:
            // 1. main_receiver has been dropped (which implies the app is shutting down)
            // 2. we are on a background thread.
            // It is not safe to drop something !Send on the wrong thread, and
            // the app will exit soon anyway, so we must forget the runnable.
            std::mem::forget(runnable);
        });
    }

    fn dispatch_after(&self, duration: Duration, runnable: Runnable) {
        if let Err(error) = self.timer_sender.send(TimerAfter { duration, runnable }) {
            // Dropping a scheduled runnable cancels its task and can make an
            // awaiting task panic. During shutdown, leave it pending instead.
            std::mem::forget(error);
        }
    }

    fn park(&self, timeout: Option<Duration>) -> bool {
        if let Some(timeout) = timeout {
            self.parker.lock().park_timeout(timeout)
        } else {
            self.parker.lock().park();
            true
        }
    }

    fn unparker(&self) -> Unparker {
        self.parker.lock().unparker()
    }
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use std::{
        collections::BTreeSet,
        thread,
        time::{Duration, Instant},
    };

    use super::*;

    fn thread_ids_with_comm_prefix(prefix: &str) -> BTreeSet<i32> {
        let mut ids = BTreeSet::new();
        let Ok(entries) = std::fs::read_dir("/proc/self/task") else {
            return ids;
        };
        for entry in entries.flatten() {
            let Ok(tid) = entry.file_name().to_string_lossy().parse::<i32>() else {
                continue;
            };
            let Ok(comm) = std::fs::read_to_string(entry.path().join("comm")) else {
                continue;
            };
            if comm.trim_end().starts_with(prefix) {
                ids.insert(tid);
            }
        }
        ids
    }

    fn wait_for_new_threads(prefix: &str, before: &BTreeSet<i32>) -> BTreeSet<i32> {
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            let appeared = thread_ids_with_comm_prefix(prefix)
                .difference(before)
                .copied()
                .collect::<BTreeSet<_>>();
            if !appeared.is_empty() {
                return appeared;
            }
            assert!(
                Instant::now() < deadline,
                "dispatcher did not start a thread named {prefix}"
            );
            thread::sleep(Duration::from_millis(10));
        }
    }

    #[test]
    fn dispatcher_threads_exit_when_dropped() {
        let timers_before = thread_ids_with_comm_prefix("Timer");
        let workers_before = thread_ids_with_comm_prefix("Worker-");
        let (main_sender, _main_receiver) = calloop::channel::channel::<Runnable>();
        let dispatcher = LinuxDispatcher::new(main_sender);
        let timers = wait_for_new_threads("Timer", &timers_before);
        let workers = wait_for_new_threads("Worker-", &workers_before);
        assert_eq!(timers.len(), 1, "dispatcher starts one timer thread");

        drop(dispatcher);

        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let live_timers = thread_ids_with_comm_prefix("Timer");
            let live_workers = thread_ids_with_comm_prefix("Worker-");
            let timer_gone = timers.iter().all(|tid| !live_timers.contains(tid));
            let workers_gone = workers.iter().all(|tid| !live_workers.contains(tid));
            if timer_gone && workers_gone {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "dispatcher threads stayed alive after drop; timer still alive: {}, workers still alive: {}",
                !timer_gone,
                !workers_gone,
            );
            thread::sleep(Duration::from_millis(20));
        }
    }
}

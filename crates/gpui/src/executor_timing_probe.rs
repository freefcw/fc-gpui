//! Spawn-site probe for the profiler timing test.
//!
//! The profiler records task timings into global state, so unrelated tests
//! running in parallel can contribute timings while a profiler test holds the
//! profiler enabled. Spawning from this dedicated file lets the test identify
//! timings from its own task by file name, independent of concurrent tests.
use crate::BackgroundExecutor;

pub fn spawn_noop(executor: &BackgroundExecutor) {
    executor.spawn(async {}).detach();
}

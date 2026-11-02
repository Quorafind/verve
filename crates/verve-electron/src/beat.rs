//! The beat both kinds of guest keep: what a runtime does between jobs, and how long until it has
//! something to do. One definition, so the browser process and a page cannot drift apart on it.

use std::time::Duration;

use verve::Runtime;

/// Runs what is due: timers, workers' messages, and the microtasks they leave.
pub(crate) fn run(runtime: &mut Runtime) {
    runtime.run_due();
    runtime.run_workers();
    runtime.run_microtasks();
}

/// How long until the runtime's earliest timer is due; `None` when it has none.
pub(crate) fn next_due(runtime: &Runtime) -> Option<Duration> {
    let due = runtime.next_due()?;
    Some(Duration::from_secs_f64(
        ((due - runtime.now()) / 1000.).max(0.),
    ))
}

//! The JIT in `electron`'s isolates (`--features jit`): on by default in the browser process's
//! runtime and in a page's, off with `VERVE_JIT=0`, and the object layout its property caches read
//! checks out on each isolate's own thread — the check is per thread, run the first time that
//! thread compiles or the first time it is asked.
//!
//! Neither guest (`MainGuest`, `RendererGuest`) lends out its runtime, and `jit_stats` is per
//! runtime, so each test builds the same thing on an isolate thread of the same shape: a
//! `Runtime::new()` with the Hub's isolate options, and for a page the document installed on it.
//! What the `electron` binary does with the app's own script end to end is measured against the
//! benchmarks instead (`docs/electron/packages/P63-electron-jit.md`).

#![cfg(feature = "jit")]

use std::process::Command;
use std::time::Duration;

use verve::isolate::{Guest, Isolate, Options, Outbox};
use verve::{Runtime, print};
use verve_dom::FrameClock;

/// A hot loop that reads and writes a property every iteration: the shape a compiled function
/// keeps property caches for (`docs/jit.md`, "Property caches and elements").
const HOT: &str = "
    function run(o, n) {
        let s = 0;
        for (let i = 0; i < n; i++) {
            s = (s + o.a) | 0;
            o.a = (s & 7) + 1;
        }
        return s;
    }
    const o = { a: 1 };
    let t = 0;
    for (let k = 0; k < 20; k++) t = (t + run(o, 2000)) | 0;
    t
";

/// What `HOT` comes to: what Node 22.21.1 gives for the same loop as `verve` does.
const HOT_RESULT: &str = "319660";

/// The stack `hub::isolate_options` gives a JS thread; that constant is private to the crate, so
/// this is the same value written out. `idle` is `Options::default()`'s 500 ms, which is what the
/// Hub uses too.
const STACK: usize = 16 * 1024 * 1024;

/// What an isolate's runtime says of the JIT after the loop, and what the loop came to.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Measured {
    enabled: bool,
    compiled: u64,
    entries: u64,
    live_functions: usize,
    /// Whether property and element accesses compile inline on this thread.
    layout_checked: bool,
    /// What `HOT` came to in this isolate.
    result: String,
}

/// An isolate running a runtime, as the browser process or a page has one.
struct Hot {
    runtime: Runtime,
    page: bool,
    measured: Option<Measured>,
}

impl Guest for Hot {
    type View = Measured;
    type Event = ();

    fn tick(&mut self) {
        self.runtime.run_due();
        self.runtime.run_microtasks();
    }

    fn next_due(&self) -> Option<Duration> {
        None
    }

    fn drain(&mut self, _: &mut Outbox<()>) {}

    fn publish(&mut self, _: u64, _: bool) -> Option<Measured> {
        self.measured.take()
    }
}

/// Starts an isolate of the browser process's shape, or of a page's, and runs `HOT` in it.
fn measure(page: bool, name: &str) -> Measured {
    let isolate = Isolate::spawn(
        Options {
            name: name.into(),
            stack_size: STACK,
            ..Options::default()
        },
        move || {
            let runtime = Runtime::new();
            Hot {
                runtime,
                page,
                measured: None,
            }
        },
        || {},
    )
    .expect("an isolate thread");
    let job = isolate.send(move |guest, _| {
        if guest.page {
            verve_dom::install(
                &mut guest.runtime,
                verve_dom::Options {
                    // The shell says when a frame is drawn; nothing here draws any.
                    frames: FrameClock::Host,
                    ..verve_dom::Options::default()
                },
            )
            .expect("a page's document installs");
        }
        let result = guest.runtime.eval(HOT).expect("the loop runs");
        let stats = guest.runtime.jit_stats();
        guest.measured = Some(Measured {
            enabled: guest.runtime.jit_enabled(),
            compiled: stats.compiled,
            entries: stats.entries,
            live_functions: stats.live_functions,
            layout_checked: boa_engine::vm::object_layout_checked(),
            result: print(&result),
        });
    });
    isolate
        .wait_for(job, Duration::from_secs(60))
        .expect("the isolate measured itself")
}

/// A JIT that ran the loop compiled, with property accesses inline as they are when the layout
/// checked out on the isolate's thread — and the loop came to what it does in Node.
fn assert_compiled(measured: Measured, what: &str) {
    assert_eq!(
        measured.result, HOT_RESULT,
        "{what}: the loop came to something else"
    );
    assert!(measured.enabled, "{what}: the JIT is off by default");
    assert!(
        measured.compiled > 0 && measured.entries > 0,
        "{what}: nothing was compiled: {measured:?}"
    );
    assert!(
        measured.live_functions > 0,
        "{what}: no compiled function is mapped: {measured:?}"
    );
    assert!(
        measured.layout_checked,
        "{what}: the object layout did not check out, so property accesses are helpers: {measured:?}"
    );
}

#[test]
fn the_browser_process_isolate_compiles_its_hot_loop() {
    assert_compiled(measure(false, "electron-main"), "the browser process");
}

#[test]
fn a_pages_isolate_compiles_its_hot_loop() {
    assert_compiled(measure(true, "renderer-1"), "a page");
}

/// `VERVE_JIT=0` switches the JIT off in every isolate, as it does for `verve run`. The variable
/// is read when a runtime is made, so this runs the test again in a process of its own.
#[test]
fn verve_jit_0_keeps_every_isolate_interpreted() {
    if std::env::var("VERVE_JIT").is_ok_and(|value| value.trim() == "0") {
        for (page, name) in [(false, "electron-main"), (true, "renderer-1")] {
            let measured = measure(page, name);
            assert_eq!(
                measured.result, HOT_RESULT,
                "{name}: the loop came to something else: {measured:?}"
            );
            assert!(!measured.enabled, "{name}: the JIT is on: {measured:?}");
            assert_eq!(
                (measured.compiled, measured.entries, measured.live_functions),
                (0, 0, 0),
                "{name}: compiled code ran"
            );
        }
        return;
    }
    let exe = std::env::current_exe().expect("the test binary's path");
    let output = Command::new(exe)
        .args([
            "--exact",
            "verve_jit_0_keeps_every_isolate_interpreted",
            "--test-threads=1",
        ])
        .env("VERVE_JIT", "0")
        .output()
        .expect("the test binary runs");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(output.status.success(), "with VERVE_JIT=0: {stdout}");
    // A filter that matches nothing also exits 0, with "0 passed": without this the test would
    // pass having run nothing at all.
    assert!(
        stdout.contains("test result: ok. 1 passed"),
        "the child process did not run the test: {stdout}"
    );
}

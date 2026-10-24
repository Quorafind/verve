//! Isolates: a runtime on a thread of its own, sent jobs, answering with events and views — and
//! failing alone: a script that will not return is stopped from outside, and a panic ends only
//! the isolate it happened in.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use runtime_x::RuntimeX;
use runtime_x::isolate::{Guest, Health, Isolate, Options, Outbox};

/// A runtime and nothing else: its jobs evaluate scripts, and its view is how many have run.
struct Scripts {
    runtime: RuntimeX,
    ran: u64,
}

impl Guest for Scripts {
    type View = u64;
    type Event = String;

    fn tick(&mut self) {
        self.runtime.run_due();
        self.runtime.run_microtasks();
    }

    fn next_due(&self) -> Option<Duration> {
        let now = self.runtime.now();
        self.runtime
            .next_due()
            .map(|at| Duration::from_secs_f64((at - now).max(0.) / 1000.))
    }

    fn drain(&mut self, _: &mut Outbox<String>) {}

    fn publish(&mut self, _: u64, ran: bool) -> Option<u64> {
        ran.then_some(self.ran)
    }

    fn interrupt_handle(&mut self) -> Option<runtime_x::isolate::InterruptHandle> {
        Some(self.runtime.interrupt_handle())
    }
}

fn spawn(name: &str, wakes: Arc<AtomicUsize>) -> Isolate<Scripts> {
    Isolate::spawn(
        Options {
            name: name.into(),
            ..Options::default()
        },
        || Scripts {
            runtime: RuntimeX::new(),
            ran: 0,
        },
        move || {
            wakes.fetch_add(1, Ordering::SeqCst);
        },
    )
    .unwrap()
}

fn eval(isolate: &Isolate<Scripts>, source: &'static str) -> u64 {
    isolate.send(move |guest, out| {
        guest.ran += 1;
        let said = match guest.runtime.eval(source) {
            Ok(value) => runtime_x::print(&value),
            Err(error) => format!("error: {error}"),
        };
        out.push(said);
    })
}

fn events_until(isolate: &Isolate<Scripts>, count: usize) -> Vec<String> {
    let started = Instant::now();
    let mut events = Vec::new();
    while events.len() < count && started.elapsed() < Duration::from_secs(10) {
        events.extend(isolate.take_events());
        std::thread::sleep(Duration::from_millis(2));
    }
    events
}

#[test]
fn an_isolate_answers_without_its_embedder_waiting() {
    let wakes = Arc::new(AtomicUsize::new(0));
    let isolate = spawn("answers", Arc::clone(&wakes));
    let job = eval(&isolate, "6 * 7");
    assert_eq!(isolate.wait_for(job, Duration::from_secs(10)), Some(1));
    assert_eq!(events_until(&isolate, 1), ["42"]);
    assert!(wakes.load(Ordering::SeqCst) > 0);
    assert_eq!(isolate.health(), Health::Alive);
}

#[test]
fn a_script_that_will_not_return_is_stopped_from_outside() {
    let isolate = spawn("endless", Arc::new(AtomicUsize::new(0)));
    let started = Instant::now();
    eval(&isolate, "try { while (true) {} } catch (e) { 'caught' }");
    // Sending to it does not wait for it.
    let after = eval(&isolate, "'after'");
    assert!(started.elapsed() < Duration::from_millis(50));
    // A watchdog sees it busy, and stops it.
    while isolate
        .busy_for()
        .is_none_or(|busy| busy < Duration::from_millis(100))
    {
        assert!(
            started.elapsed() < Duration::from_secs(10),
            "it never looked busy"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
    assert!(isolate.interrupt());
    let events = events_until(&isolate, 2);
    assert!(events[0].contains("interrupted"), "{events:?}");
    // The isolate goes on: the job after it ran.
    assert_eq!(events[1], "after");
    assert!(isolate.applied() >= after);
    assert_eq!(isolate.health(), Health::Alive);
}

#[test]
fn a_panic_ends_its_own_isolate_and_no_other() {
    let doomed = spawn("doomed", Arc::new(AtomicUsize::new(0)));
    let other = spawn("other", Arc::new(AtomicUsize::new(0)));
    doomed.send(|_, _| panic!("a plugin's host went wrong"));
    let started = Instant::now();
    while doomed.health() == Health::Alive {
        assert!(started.elapsed() < Duration::from_secs(10), "it never died");
        std::thread::sleep(Duration::from_millis(5));
    }
    assert_eq!(
        doomed.health(),
        Health::Dead("a plugin's host went wrong".into())
    );
    // Sent to the dead one: dropped, not waited for.
    eval(&doomed, "1");
    // The other one is untouched.
    eval(&other, "'still here'");
    assert_eq!(events_until(&other, 1), ["still here"]);
    assert_eq!(other.health(), Health::Alive);
}

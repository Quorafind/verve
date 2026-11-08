//! Isolates: a runtime on a thread of its own, sent jobs, answering with events and views — and
//! failing alone: a script that will not return is stopped from outside, and a panic ends only
//! the isolate it happened in.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use verve::Runtime;
use verve::isolate::{Guest, Health, Isolate, Options, Outbox};

/// A runtime and nothing else: its jobs evaluate scripts, and its view is how many have run.
struct Scripts {
    runtime: Runtime,
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

    fn interrupt_handle(&mut self) -> Option<verve::isolate::InterruptHandle> {
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
            runtime: Runtime::new(),
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
            Ok(value) => verve::print(&value),
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

/// A guest that says something as it is made, and whose beat takes a long while once asked to.
struct Slow {
    said: Vec<String>,
    slow_beats: usize,
}

impl Guest for Slow {
    type View = ();
    type Event = String;

    fn tick(&mut self) {
        if self.slow_beats > 0 {
            self.slow_beats -= 1;
            std::thread::sleep(Duration::from_secs(2));
        }
    }

    fn next_due(&self) -> Option<Duration> {
        (self.slow_beats > 0).then_some(Duration::ZERO)
    }

    fn drain(&mut self, out: &mut Outbox<String>) {
        for event in self.said.drain(..) {
            out.push(event);
        }
    }

    fn publish(&mut self, _: u64, _: bool) -> Option<()> {
        None
    }
}

/// What it said, waited for up to `within`.
fn heard_within(isolate: &Isolate<Slow>, within: Duration) -> Vec<String> {
    let started = Instant::now();
    loop {
        let events = isolate.take_events();
        if !events.is_empty() || started.elapsed() >= within {
            return events;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
}

#[test]
fn what_a_guest_says_reaches_the_embedder_before_a_long_beat() {
    let isolate = Isolate::spawn(
        Options::default(),
        || Slow {
            said: vec!["made".into()],
            slow_beats: 1,
        },
        || {},
    )
    .unwrap();
    // Said while it was made, and its first beat is the slow one.
    assert_eq!(
        heard_within(&isolate, Duration::from_millis(1500)),
        ["made"]
    );
    // Said by a job, with another slow beat after it.
    std::thread::sleep(Duration::from_millis(2500));
    isolate.send(|guest, _| {
        guest.said.push("from a job".into());
        guest.slow_beats = 1;
    });
    assert_eq!(
        heard_within(&isolate, Duration::from_millis(1500)),
        ["from a job"]
    );
}

/// A guest that counts its beats, and says when it was dropped.
struct Counting {
    beats: Arc<AtomicUsize>,
    ended: Arc<AtomicUsize>,
}

impl Guest for Counting {
    type View = ();
    type Event = ();

    fn tick(&mut self) {
        self.beats.fetch_add(1, Ordering::SeqCst);
    }

    fn next_due(&self) -> Option<Duration> {
        None
    }

    fn drain(&mut self, _: &mut Outbox<()>) {}

    fn publish(&mut self, _: u64, _: bool) -> Option<()> {
        None
    }

    fn shutdown(&mut self) {
        self.ended.fetch_add(1, Ordering::SeqCst);
    }
}

#[test]
fn a_poke_runs_the_beat_at_once_and_does_not_keep_the_isolate_alive() {
    let (beats, ended) = (Arc::new(AtomicUsize::new(0)), Arc::new(AtomicUsize::new(0)));
    let guest = Counting {
        beats: Arc::clone(&beats),
        ended: Arc::clone(&ended),
    };
    let isolate = Isolate::spawn(
        Options {
            idle: Duration::from_secs(30),
            ..Options::default()
        },
        move || guest,
        || {},
    )
    .unwrap();
    let poke = isolate.poker();
    let before = beats.load(Ordering::SeqCst);
    poke();
    let started = Instant::now();
    while beats.load(Ordering::SeqCst) == before && started.elapsed() < Duration::from_secs(5) {
        std::thread::sleep(Duration::from_millis(2));
    }
    assert!(beats.load(Ordering::SeqCst) > before, "the poke ran a beat");
    assert_eq!(isolate.applied(), 0, "a poke is no numbered job");

    // The poker is still held, and the isolate still ends when it is let go.
    drop(isolate);
    let started = Instant::now();
    while ended.load(Ordering::SeqCst) == 0 && started.elapsed() < Duration::from_secs(5) {
        std::thread::sleep(Duration::from_millis(2));
    }
    assert_eq!(ended.load(Ordering::SeqCst), 1);
    poke();
}

/// A guest that keeps a record of what ran in it, and a flag its thread sets as it ends.
struct Probe {
    record: Arc<std::sync::Mutex<Vec<&'static str>>>,
    ended: Arc<std::sync::atomic::AtomicBool>,
    /// How long after it is made its beat is due; it is due again each time.
    due: Option<Duration>,
}

impl Guest for Probe {
    type View = ();
    type Event = &'static str;

    fn tick(&mut self) {
        self.record.lock().unwrap().push("tick");
    }

    fn next_due(&self) -> Option<Duration> {
        self.due
    }

    fn drain(&mut self, _: &mut Outbox<&'static str>) {}

    fn publish(&mut self, _: u64, _: bool) -> Option<()> {
        None
    }

    fn shutdown(&mut self) {
        self.ended.store(true, Ordering::SeqCst);
    }
}

type Record = Arc<std::sync::Mutex<Vec<&'static str>>>;
type Ended = Arc<std::sync::atomic::AtomicBool>;

fn probe_isolate(
    due: Option<Duration>,
    gate: Option<std::sync::mpsc::Receiver<()>>,
) -> (Isolate<Probe>, Record, Ended) {
    let record = Record::default();
    let ended = Ended::default();
    let (made_record, made_ended) = (Arc::clone(&record), Arc::clone(&ended));
    let isolate = Isolate::spawn(
        Options::default(),
        move || {
            if let Some(gate) = gate {
                let _ = gate.recv();
            }
            Probe {
                record: made_record,
                ended: made_ended,
                due,
            }
        },
        || {},
    )
    .unwrap();
    (isolate, record, ended)
}

fn ends_within_ten_seconds(ended: &Ended) -> bool {
    let started = Instant::now();
    while !ended.load(Ordering::SeqCst) && started.elapsed() < Duration::from_secs(10) {
        std::thread::sleep(Duration::from_millis(2));
    }
    ended.load(Ordering::SeqCst)
}

#[test]
fn a_retired_isolate_runs_no_job_already_queued_and_says_nothing_more() {
    let (isolate, record, ended) = probe_isolate(None, None);
    let (started, hold) = (std::sync::mpsc::channel(), std::sync::mpsc::channel::<()>());
    let (started_tx, started_rx) = started;
    let (hold_tx, hold_rx) = hold;
    isolate.send(move |_, _| {
        started_tx.send(()).unwrap();
        let _ = hold_rx.recv();
    });
    started_rx.recv_timeout(Duration::from_secs(10)).unwrap();
    // Queued behind the job that runs now.
    let rec = Arc::clone(&record);
    isolate.send(move |_, out| {
        rec.lock().unwrap().push("queued");
        out.push("queued");
    });
    isolate.retire();
    hold_tx.send(()).unwrap();
    assert!(ends_within_ten_seconds(&ended), "the thread ends");
    assert_eq!(record.lock().unwrap().as_slice(), [] as [&str; 0]);
    assert!(isolate.take_events().is_empty());
}

#[test]
fn a_retired_isolate_fires_no_timer_that_falls_due() {
    let (isolate, record, ended) = probe_isolate(Some(Duration::from_millis(400)), None);
    // Long enough for the guest to be made and waiting; the timer falls due at 400 ms.
    std::thread::sleep(Duration::from_millis(100));
    isolate.retire();
    assert!(ends_within_ten_seconds(&ended), "the thread ends");
    std::thread::sleep(Duration::from_millis(500));
    assert!(record.lock().unwrap().is_empty(), "no tick ran");
    assert!(isolate.take_events().is_empty());
}

#[test]
fn an_isolate_retired_while_its_guest_is_made_runs_nothing_after() {
    let (gate_tx, gate_rx) = std::sync::mpsc::channel();
    let (isolate, record, ended) = probe_isolate(Some(Duration::from_millis(1)), Some(gate_rx));
    let rec = Arc::clone(&record);
    isolate.send(move |_, _| rec.lock().unwrap().push("job"));
    isolate.retire();
    gate_tx.send(()).unwrap();
    assert!(ends_within_ten_seconds(&ended), "the thread ends");
    std::thread::sleep(Duration::from_millis(50));
    assert!(record.lock().unwrap().is_empty(), "nothing ran");
    assert!(isolate.take_events().is_empty());
}

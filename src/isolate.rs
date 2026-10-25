//! Isolates: a script runtime on a thread of its own, which its embedder talks to without ever
//! waiting for it.
//!
//! An isolate is what V8 calls one — a heap of its own (Boa's collector keeps one per thread),
//! one thread running it — together with the loop that keeps it: the embedder sends it **jobs**,
//! closures run on the isolate's thread in the order they were sent, with the guest — whatever
//! the embedder put there: a [`Runtime`](crate::Runtime) and the API it serves — in hand; the
//! guest's own beat runs between them (timers, workers, whatever else it has due); and after each
//! turn the guest hands back **events** — what its scripts asked of the embedder, the answers to
//! jobs — and, when something changed, a **view**: a snapshot of whatever the embedder reads of
//! it, theirs to keep and read on their own thread. The embedder is woken when there is something
//! new, and never blocks on the isolate: a script busy for seconds, or one that never returns,
//! holds its own thread and nothing else.
//!
//! And an isolate that fails fails alone. A panic in a job or in the beat ends that isolate —
//! [`Isolate::health`] says so, with the panic's message — and no other; a script that will not
//! return can be stopped from outside with [`Isolate::interrupt`] (it throws, uncatchably, at its
//! next loop iteration or call), and [`Isolate::busy_for`] is what a watchdog asks to know when to.
//!
//! What to run in an isolate is the embedder's: implement [`Guest`] for it.

use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

pub use boa_engine::context::InterruptHandle;

/// What runs in an isolate: made on the isolate's thread, and kept there.
pub trait Guest: 'static {
    /// What the embedder reads of the guest, taken after a turn that changed something.
    type View: Send + 'static;
    /// What the guest tells the embedder.
    type Event: Send + 'static;

    /// Runs what is due: timers, workers, whatever the guest keeps time for.
    fn tick(&mut self);

    /// How long until [`Guest::tick`] has something to do; `None` when nothing is waiting.
    fn next_due(&self) -> Option<Duration>;

    /// Moves what the guest has for the embedder into `out`. Called after every job and every
    /// beat.
    fn drain(&mut self, out: &mut Outbox<Self::Event>);

    /// A view of the guest as it is now, if anything the embedder reads changed since the last
    /// one — or `ran` says jobs ran, which an embedder waiting on one wants to see. `applied` is
    /// the number of the last job done, for the view to carry.
    fn publish(&mut self, applied: u64, ran: bool) -> Option<Self::View>;

    /// The handle that stops what the guest's scripts are running; see [`Isolate::interrupt`].
    /// Asked once, as the isolate starts.
    fn interrupt_handle(&mut self) -> Option<InterruptHandle> {
        None
    }

    /// The isolate is ending because its embedder let it go: the last thing run on its thread.
    fn shutdown(&mut self) {}
}

/// What a job, or the guest, sends the embedder.
pub struct Outbox<E> {
    events: Vec<E>,
}

impl<E> Default for Outbox<E> {
    fn default() -> Self {
        Self { events: Vec::new() }
    }
}

impl<E> Outbox<E> {
    pub fn push(&mut self, event: E) {
        self.events.push(event);
    }

    pub fn is_empty(&self) -> bool {
        self.events.is_empty()
    }
}

/// A job for an isolate.
pub type Job<G> = Box<dyn FnOnce(&mut G, &mut Outbox<<G as Guest>::Event>) + Send>;

/// How an isolate is doing.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Health {
    /// Running, or waiting for something to do.
    Alive,
    /// Ended: a panic took it — the panic's message — or its guest could not be made.
    Dead(String),
}

/// How an isolate is started.
#[derive(Clone, Debug)]
pub struct Options {
    /// Its thread's name.
    pub name: String,
    /// Its thread's stack: what a deeply nested script, or a large one being parsed, needs.
    pub stack_size: usize,
    /// The longest it sleeps with nothing due, so a guest's slow work — reading a folder again
    /// now and then — still runs.
    pub idle: Duration,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            name: "isolate".into(),
            stack_size: 32 * 1024 * 1024,
            idle: Duration::from_millis(500),
        }
    }
}

struct Shared<G: Guest> {
    view: Mutex<Option<G::View>>,
    events: Mutex<Vec<G::Event>>,
    health: Mutex<Health>,
    /// When what is running now began, in milliseconds since `start`; 0 when nothing runs.
    busy_since: AtomicU64,
    /// The last job done.
    applied: AtomicU64,
    start: Instant,
    interrupt: Mutex<Option<InterruptHandle>>,
    wake: Box<dyn Fn() + Send + Sync>,
}

impl<G: Guest> Shared<G> {
    fn now(&self) -> u64 {
        // Never 0, which means nothing runs.
        self.start.elapsed().as_millis() as u64 + 1
    }
}

/// The embedder's handle on an isolate; see the module's comment. Dropping it lets the isolate
/// finish: once it has done what it was sent, the guest's [`Guest::shutdown`] runs and the thread
/// ends.
pub struct Isolate<G: Guest> {
    inbox: Sender<(u64, Job<G>)>,
    shared: Arc<Shared<G>>,
    next: std::cell::Cell<u64>,
}

impl<G: Guest> Isolate<G> {
    /// Starts an isolate: its thread, and on it the guest `make` makes. `wake` is called, on the
    /// isolate's thread, whenever there is something new for the embedder — a view, events, an
    /// isolate that died.
    pub fn spawn(
        options: Options,
        make: impl FnOnce() -> G + Send + 'static,
        wake: impl Fn() + Send + Sync + 'static,
    ) -> std::io::Result<Self> {
        let (inbox, jobs) = mpsc::channel();
        let shared = Arc::new(Shared {
            view: Mutex::new(None),
            events: Mutex::new(Vec::new()),
            health: Mutex::new(Health::Alive),
            busy_since: AtomicU64::new(0),
            applied: AtomicU64::new(0),
            start: Instant::now(),
            interrupt: Mutex::new(None),
            wake: Box::new(wake),
        });
        let thread_shared = Arc::clone(&shared);
        let idle = options.idle;
        let stack = options.stack_size;
        std::thread::Builder::new()
            .name(options.name)
            .stack_size(stack)
            .spawn(move || {
                crate::set_thread_stack(stack);
                run(make, jobs, &thread_shared, idle)
            })?;
        Ok(Self {
            inbox,
            shared,
            next: std::cell::Cell::new(0),
        })
    }

    /// Sends `job`, to run after every job sent before it. Answers its number; see
    /// [`Isolate::applied`]. A job sent to an isolate that has died is dropped.
    pub fn send(&self, job: impl FnOnce(&mut G, &mut Outbox<G::Event>) + Send + 'static) -> u64 {
        let n = self.next.get() + 1;
        self.next.set(n);
        let _ = self.inbox.send((n, Box::new(job)));
        n
    }

    /// The newest view published since this was last asked.
    pub fn take_view(&self) -> Option<G::View> {
        self.shared.view.lock().ok()?.take()
    }

    pub fn take_events(&self) -> Vec<G::Event> {
        self.shared
            .events
            .lock()
            .map(|mut events| std::mem::take(&mut *events))
            .unwrap_or_default()
    }

    /// The number of the last job done.
    pub fn applied(&self) -> u64 {
        self.shared.applied.load(Ordering::SeqCst)
    }

    pub fn health(&self) -> Health {
        self.shared
            .health
            .lock()
            .map(|health| health.clone())
            .unwrap_or_else(|_| Health::Dead("its state was poisoned".into()))
    }

    /// How long what the isolate is running now — a job, or its beat — has been running;
    /// `None` when it is waiting. What a watchdog asks.
    pub fn busy_for(&self) -> Option<Duration> {
        match self.shared.busy_since.load(Ordering::SeqCst) {
            0 => None,
            since => Some(Duration::from_millis(
                self.shared.now().saturating_sub(since),
            )),
        }
    }

    /// Stops the script the isolate is running: it throws at its next loop iteration or call,
    /// and JavaScript cannot catch it. The isolate goes on to its next job. Answers whether the
    /// guest gave a handle to stop it with.
    pub fn interrupt(&self) -> bool {
        match self.shared.interrupt.lock().ok().and_then(|h| h.clone()) {
            Some(handle) => {
                handle.interrupt();
                true
            }
            None => false,
        }
    }

    /// Waits, up to `timeout`, for job `job` to be done and a view after it to be published,
    /// and answers that view. For tests, and nothing that must not block.
    pub fn wait_for(&self, job: u64, timeout: Duration) -> Option<G::View> {
        let started = Instant::now();
        loop {
            if self.applied() >= job
                && let Some(view) = self.take_view()
            {
                return Some(view);
            }
            if started.elapsed() >= timeout || matches!(self.health(), Health::Dead(_)) {
                return None;
            }
            std::thread::sleep(Duration::from_millis(2));
        }
    }
}

/// What a panic said, as text.
fn panic_message(panic: &(dyn std::any::Any + Send)) -> String {
    if let Some(text) = panic.downcast_ref::<&str>() {
        (*text).to_owned()
    } else if let Some(text) = panic.downcast_ref::<String>() {
        text.clone()
    } else {
        "a panic".to_owned()
    }
}

/// Moves `out`'s events to where the embedder takes them; `true` when there were any.
fn deliver<G: Guest>(shared: &Shared<G>, out: &mut Outbox<G::Event>) -> bool {
    if out.is_empty() {
        return false;
    }
    if let Ok(mut events) = shared.events.lock() {
        events.append(&mut out.events);
    }
    true
}

/// The isolate's life on its thread.
fn run<G: Guest>(
    make: impl FnOnce() -> G,
    jobs: Receiver<(u64, Job<G>)>,
    shared: &Shared<G>,
    idle: Duration,
) {
    let die = |message: String| {
        if let Ok(mut health) = shared.health.lock() {
            *health = Health::Dead(message);
        }
        shared.busy_since.store(0, Ordering::SeqCst);
        (shared.wake)();
    };
    shared.busy_since.store(shared.now(), Ordering::SeqCst);
    let mut guest = match catch_unwind(AssertUnwindSafe(make)) {
        Ok(guest) => guest,
        Err(panic) => return die(panic_message(&*panic)),
    };
    if let Ok(mut slot) = shared.interrupt.lock() {
        *slot = guest.interrupt_handle();
    }
    shared.busy_since.store(0, Ordering::SeqCst);
    let mut out = Outbox::default();
    // What the guest said while it was being made — a script's first output, before its first
    // timer — reaches the embedder now, not after a beat that may run long.
    guest.drain(&mut out);
    if deliver(shared, &mut out) {
        (shared.wake)();
    }
    let mut applied = 0;
    loop {
        let wait = guest.next_due().map_or(idle, |due| due.min(idle));
        let first = match jobs.recv_timeout(wait) {
            Ok(job) => Some(job),
            Err(RecvTimeoutError::Timeout) => None,
            Err(RecvTimeoutError::Disconnected) => break,
        };
        let mut batch: Vec<(u64, Job<G>)> = first.into_iter().collect();
        while let Ok(job) = jobs.try_recv() {
            batch.push(job);
        }
        let ran = !batch.is_empty();
        // The turn: the jobs, then the beat — each inside the isolate's walls.
        shared.busy_since.store(shared.now(), Ordering::SeqCst);
        let turn = catch_unwind(AssertUnwindSafe(|| {
            for (n, job) in batch {
                job(&mut guest, &mut out);
                guest.drain(&mut out);
                applied = n;
                shared.applied.store(n, Ordering::SeqCst);
            }
            // What the jobs made happen is told before the beat, which may run long.
            if deliver(shared, &mut out) {
                (shared.wake)();
            }
            guest.tick();
            guest.drain(&mut out);
            guest.publish(applied, ran)
        }));
        shared.busy_since.store(0, Ordering::SeqCst);
        let view = match turn {
            Ok(view) => view,
            Err(panic) => {
                // What it had already said still reaches the embedder.
                if let Ok(mut events) = shared.events.lock() {
                    events.append(&mut out.events);
                }
                // Its state is whatever the panic left: not touched again — not even dropped,
                // which would run its scripts' cleanup in it.
                std::mem::forget(guest);
                return die(panic_message(&*panic));
            }
        };
        let mut news = false;
        if let Some(view) = view {
            if let Ok(mut slot) = shared.view.lock() {
                *slot = Some(view);
            }
            news = true;
        }
        if !out.is_empty() {
            if let Ok(mut events) = shared.events.lock() {
                events.append(&mut out.events);
            }
            news = true;
        }
        if news {
            (shared.wake)();
        }
    }
    if catch_unwind(AssertUnwindSafe(|| guest.shutdown())).is_err() {
        std::mem::forget(guest);
    }
}

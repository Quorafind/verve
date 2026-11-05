//! Message ports whose other end is in another isolate: a `MessagePort` in one realm driven by a
//! [`PortEnd`] that another thread holds, as the main process holds the other half of a page's
//! channel.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use boa_engine::js_string;
use verve::Runtime;
use verve::clone::Cloned;
use verve::messaging::{PortEnd, PortEvent, adopt_port, release_port, remote_port_pair};

const WITHIN: Duration = Duration::from_secs(5);

type Job = Box<dyn FnOnce(&mut Runtime) + Send>;

/// A realm on a thread of its own, run on the host's beat the way an isolate does: a job, then
/// whatever is due, the workers and the ports, then the microtasks. The realm's waker is a job
/// that does nothing, so a port end calling it makes the thread take another beat.
struct Page {
    jobs: Sender<Job>,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl Page {
    fn new() -> Self {
        let (jobs, inbox): (Sender<Job>, Receiver<Job>) = mpsc::channel();
        let poke = jobs.clone();
        let stop = Arc::new(AtomicBool::new(false));
        let stopping = Arc::clone(&stop);
        let thread = std::thread::spawn(move || {
            let mut runtime = Runtime::new();
            let waker: verve::Waker = Arc::new(Mutexed::new(poke).waker());
            runtime.set_waker(Some(waker));
            while let Ok(job) = inbox.recv() {
                if stopping.load(Ordering::SeqCst) {
                    break;
                }
                job(&mut runtime);
                runtime.run_due();
                runtime.run_workers();
                runtime.run_microtasks();
            }
        });
        Self {
            jobs,
            stop,
            thread: Some(thread),
        }
    }

    /// Runs `f` on the page's thread and answers what it returned.
    fn run<T: Send + 'static>(&self, f: impl FnOnce(&mut Runtime) -> T + Send + 'static) -> T {
        let (answer, wait) = mpsc::channel();
        let job: Job = Box::new(move |runtime| {
            let _ = answer.send(f(runtime));
        });
        self.jobs.send(job).expect("the page is running");
        wait.recv_timeout(WITHIN).expect("the page answered")
    }

    /// Evaluates `script` in the page and answers its value written as JSON.
    fn json(&self, script: &'static str) -> String {
        self.run(move |runtime| {
            let value = runtime.eval(&format!("JSON.stringify({script})"));
            verve::print(&value.unwrap_or_else(|e| panic!("{script}: {e}")))
        })
    }

    /// Waits until `script` is true in the page.
    fn until(&self, script: &'static str) {
        let deadline = Instant::now() + WITHIN;
        while self.json(script) != "true" {
            assert!(Instant::now() < deadline, "{script} never held");
            std::thread::sleep(Duration::from_millis(2));
        }
    }

    /// Makes `port` — a global of the page — the port that `end` drives.
    fn adopt(&self, end: PortEnd, name: &'static str) {
        self.run(move |runtime| {
            let port = adopt_port(end, runtime.context()).expect("the port is made");
            let global = runtime.context().global_object();
            global
                .set(js_string!(name), port, false, runtime.context())
                .expect("the global is set");
        });
    }
}

impl Drop for Page {
    fn drop(&mut self) {
        // The realm's waker holds a sender of its own, so the thread ends by being told to.
        self.stop.store(true, Ordering::SeqCst);
        let _ = self.jobs.send(Box::new(|_| {}));
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// The waker of a page: a job that does nothing.
struct Mutexed(std::sync::Mutex<Sender<Job>>);

impl Mutexed {
    fn new(jobs: Sender<Job>) -> Self {
        Self(std::sync::Mutex::new(jobs))
    }

    fn waker(self) -> impl Fn() + Send + Sync {
        move || {
            let _ = self.0.lock().expect("not poisoned").send(Box::new(|_| {}));
        }
    }
}

/// A clone of what `script` evaluates to.
fn cloned(script: &str) -> Cloned {
    let mut runtime = Runtime::new();
    let value = runtime.eval(script).expect("the script runs");
    Cloned::from_value(&value, runtime.context()).expect("the value clones")
}

/// What a clone looks like as JSON.
fn json_of(clone: &Cloned) -> String {
    let mut runtime = Runtime::new();
    let value = clone
        .to_value(runtime.context())
        .expect("the value is made");
    let global = runtime.context().global_object();
    global
        .set(js_string!("v"), value, false, runtime.context())
        .expect("the global is set");
    verve::print(&runtime.eval("JSON.stringify(v)").expect("it writes out"))
}

/// The next thing `end` hears, waiting for it.
fn heard(end: &PortEnd) -> PortEvent {
    let deadline = Instant::now() + WITHIN;
    loop {
        if let Some(event) = end.receive() {
            return event;
        }
        assert!(Instant::now() < deadline, "nothing arrived");
        std::thread::sleep(Duration::from_millis(1));
    }
}

#[test]
fn a_message_from_another_isolate_is_an_event_and_the_reply_goes_back() {
    let (here, there) = remote_port_pair();
    let page = Page::new();
    page.adopt(there, "port");
    page.json("(globalThis.log = [], port.onmessage = e => { log.push(e.data); port.postMessage({ n: e.data.n * 2 }); }, 1)");
    here.send(cloned("({ n: 21 })"), vec![]);
    let PortEvent::Message(reply, ports) = heard(&here) else {
        panic!("a reply");
    };
    assert_eq!(json_of(&reply), r#"{"n":42}"#);
    assert!(ports.is_empty());
    assert_eq!(page.json("log"), r#"[{"n":21}]"#);
}

#[test]
fn messages_are_heard_in_the_order_sent_and_before_the_port_is_started_they_wait() {
    let (here, there) = remote_port_pair();
    let page = Page::new();
    for n in 0..3 {
        here.send(cloned(&format!("({n})")), vec![]);
    }
    page.adopt(there, "port");
    // `addEventListener` leaves the port unstarted: the three wait for `start()`.
    page.json("(globalThis.log = [], port.addEventListener('message', e => log.push(e.data)), 1)");
    std::thread::sleep(Duration::from_millis(50));
    assert_eq!(page.json("log"), "[]");
    page.json("(port.start(), 1)");
    page.until("log.length === 3");
    assert_eq!(page.json("log"), "[0,1,2]");
}

#[test]
fn a_port_sent_along_a_port_arrives_as_a_port_that_works() {
    let (here, there) = remote_port_pair();
    let (inner_here, inner_there) = remote_port_pair();
    let page = Page::new();
    page.adopt(there, "port");
    page.json("(port.onmessage = e => e.ports[0].postMessage('through ' + e.data + ' ' + e.ports.length), 1)");
    here.send(cloned("'the port'"), vec![inner_there]);
    let PortEvent::Message(text, _) = heard(&inner_here) else {
        panic!("a message on the sent port");
    };
    assert_eq!(json_of(&text), r#""through the port 1""#);
    // And back: a port the page posts is an end here.
    page.json(
        "(globalThis.channel = new MessageChannel(), port.postMessage('here', [channel.port2]), 1)",
    );
    let PortEvent::Message(_, mut ends) = heard(&here) else {
        panic!("the page's port");
    };
    let end = ends.pop().expect("one port came");
    page.json("(channel.port1.postMessage('over the new port'), 1)");
    let PortEvent::Message(text, _) = heard(&end) else {
        panic!("a message on the page's own channel");
    };
    assert_eq!(json_of(&text), r#""over the new port""#);
}

#[test]
fn a_port_of_a_channel_in_the_realm_can_be_given_away_and_its_partner_keeps_talking() {
    let page = Page::new();
    page.run(|runtime| {
        runtime
            .eval("globalThis.log = []; globalThis.channel = new MessageChannel(); channel.port1.onmessage = e => log.push(e.data)")
            .expect("a channel");
    });
    let end = page.run(|runtime| {
        let port = runtime.eval("channel.port2").expect("the port");
        release_port(&port, runtime.context()).expect("it can be given away")
    });
    end.send(cloned("'from outside'"), vec![]);
    page.until("log.length === 1");
    assert_eq!(page.json("log"), r#"["from outside"]"#);
    page.json("(channel.port1.postMessage({ up: true }), 1)");
    let PortEvent::Message(up, _) = heard(&end) else {
        panic!("a message back");
    };
    assert_eq!(json_of(&up), r#"{"up":true}"#);
    // The one given away cannot be used, or given away again.
    let again = page.run(|runtime| {
        let port = runtime.eval("channel.port2").expect("the port");
        release_port(&port, runtime.context()).is_err()
    });
    assert!(again);
}

#[test]
fn closing_a_port_is_heard_at_the_other_end_after_what_was_sent() {
    let (here, there) = remote_port_pair();
    let page = Page::new();
    page.adopt(there, "port");
    page.json("(port.postMessage('last'), port.close(), port.postMessage('ignored'), 1)");
    assert!(matches!(heard(&here), PortEvent::Message(..)));
    assert!(matches!(heard(&here), PortEvent::Closed));
    assert!(here.receive().is_none());

    let (here, there) = remote_port_pair();
    page.adopt(there, "second");
    drop(here);
    page.until("(second.postMessage('nobody is there'), true)");
}

#[test]
fn a_burst_wakes_the_receiver_once_and_a_later_message_wakes_it_again() {
    let (here, there) = remote_port_pair();
    let wakes = Arc::new(AtomicUsize::new(0));
    let counted = Arc::clone(&wakes);
    there.set_waker(Arc::new(move || {
        counted.fetch_add(1, Ordering::SeqCst);
    }));
    for n in 0..1000 {
        here.send(cloned(&n.to_string()), vec![]);
    }
    assert_eq!(wakes.load(Ordering::SeqCst), 1);
    let mut seen = 0;
    while there.receive().is_some() {
        seen += 1;
    }
    assert_eq!(seen, 1000);
    here.send(cloned("1"), vec![]);
    assert_eq!(wakes.load(Ordering::SeqCst), 2);
}

#[test]
fn a_realm_with_a_waker_set_sleeps_while_its_port_is_quiet() {
    let (_here, there) = remote_port_pair();
    let page = Page::new();
    page.adopt(there, "port");
    let due = page.run(|runtime| runtime.next_due());
    assert_eq!(due, None);
}

#[test]
fn a_refused_transfer_list_leaves_every_port_as_it_was() {
    let (_here, there) = remote_port_pair();
    let page = Page::new();
    page.adopt(there, "port");
    page.json(
        "(globalThis.log = [], globalThis.threw = [], globalThis.channel = new MessageChannel(), \
         channel.port2.onmessage = e => log.push(e.data), 1)",
    );
    // Each list is refused whole: the port in it is not taken, though it comes first.
    page.json(
        "(threw.push((() => { try { port.postMessage('x', [channel.port1, {}]); } catch (e) { return e.name; } })()), \
         threw.push((() => { try { port.postMessage('x', [channel.port1, channel.port1]); } catch (e) { return e.name; } })()), \
         channel.port1.postMessage('alive'), 1)",
    );
    page.until("log.length === 1");
    assert_eq!(page.json("log"), r#"["alive"]"#);
    assert_eq!(page.json("threw"), r#"["DataCloneError","DataCloneError"]"#);
}

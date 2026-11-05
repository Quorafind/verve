//! The two ends of a message port whose halves live in different isolates.
//!
//! A [`PortEnd`] is `Send`: one end goes to the thread that will own it while the other stays,
//! and each side takes messages out of its end whenever its host looks (the realm does when it
//! is woken; see [`super::adopt_port`]). The channel between them is a pair of queues with a
//! wake-up for whoever is receiving — pushing wakes the receiver only if it has not been woken
//! since it last emptied the queue, so a burst of messages is one wake-up.
//!
//! A message carries its payload as a [`Cloned`] and the ports it transfers as ends of their
//! own, which is how a port is passed along a port, and over `ipcRenderer.postMessage`.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};

use crate::Waker;
use crate::clone::Cloned;

/// What one side sent the other.
#[derive(Debug)]
enum Wire {
    Message(Cloned, Vec<PortEnd>),
    Close,
}

/// What a [`PortEnd`] hears.
#[derive(Debug)]
pub enum PortEvent {
    /// A message, with the ports that came along with it.
    Message(Cloned, Vec<PortEnd>),
    /// The other end was closed or dropped. The last thing an end hears.
    Closed,
}

/// Messages on their way to one end.
#[derive(Default)]
struct Pipe {
    queue: Mutex<VecDeque<Wire>>,
    /// Whether a wake-up was sent and the queue has not been emptied since.
    woken: AtomicBool,
    waker: Mutex<Option<Waker>>,
}

impl std::fmt::Debug for Pipe {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Pipe")
    }
}

fn locked<T>(slot: &Mutex<T>) -> MutexGuard<'_, T> {
    slot.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

impl Pipe {
    fn push(&self, wire: Wire) {
        locked(&self.queue).push_back(wire);
        if !self.woken.swap(true, Ordering::AcqRel) {
            self.wake();
        }
    }

    fn pop(&self) -> Option<Wire> {
        let mut queue = locked(&self.queue);
        let wire = queue.pop_front();
        if queue.is_empty() {
            self.woken.store(false, Ordering::Release);
        }
        wire
    }

    fn wake(&self) {
        let waker = locked(&self.waker).clone();
        if let Some(waker) = waker {
            waker();
        }
    }
}

/// One end of a port pair made by [`remote_port_pair`]: send to the other end, hear from it.
/// Dropping it closes it.
#[derive(Debug)]
pub struct PortEnd {
    inbox: Arc<Pipe>,
    outbox: Arc<Pipe>,
    closed: AtomicBool,
}

/// Two ends of one port, to give to two isolates.
pub fn remote_port_pair() -> (PortEnd, PortEnd) {
    let (one, two) = (Arc::new(Pipe::default()), Arc::new(Pipe::default()));
    let end = |inbox: &Arc<Pipe>, outbox: &Arc<Pipe>| PortEnd {
        inbox: Arc::clone(inbox),
        outbox: Arc::clone(outbox),
        closed: AtomicBool::new(false),
    };
    (end(&one, &two), end(&two, &one))
}

impl PortEnd {
    /// Sends `message` and the ports that go with it. Nothing happens once this end is closed.
    pub fn send(&self, message: Cloned, ports: Vec<PortEnd>) {
        if !self.closed.load(Ordering::Acquire) {
            self.outbox.push(Wire::Message(message, ports));
        }
    }

    /// The next thing heard, if anything has come: `None` when the queue is empty or this end
    /// is closed.
    pub fn receive(&self) -> Option<PortEvent> {
        if self.closed.load(Ordering::Acquire) {
            return None;
        }
        Some(match self.inbox.pop()? {
            Wire::Message(message, ports) => PortEvent::Message(message, ports),
            Wire::Close => PortEvent::Closed,
        })
    }

    /// Closes this end: the other end hears [`PortEvent::Closed`] after what was already sent.
    pub fn close(&self) {
        if !self.closed.swap(true, Ordering::AcqRel) {
            self.outbox.push(Wire::Close);
        }
    }

    /// Whether [`PortEnd::close`] was called.
    pub fn is_closed(&self) -> bool {
        self.closed.load(Ordering::Acquire)
    }

    /// What to call, from any thread, when something arrives for this end after it was last
    /// emptied — the owner's way of being woken. Called at once if something is waiting already.
    pub fn set_waker(&self, waker: Waker) {
        *locked(&self.inbox.waker) = Some(waker);
        if !locked(&self.inbox.queue).is_empty() {
            self.inbox.woken.store(true, Ordering::Release);
            self.inbox.wake();
        }
    }
}

impl Drop for PortEnd {
    fn drop(&mut self) {
        self.close();
    }
}

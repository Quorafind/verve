//! The pipes and processes of child processes, inside the network reactor's loop.
//!
//! The reactor owns the parent's end of each pipe and each running program, as it owns its
//! sockets, and speaks of them in the same events: a pipe is a stream that takes `Write`, `End`
//! and `Close` and answers `Data`, `Written`, `Finished`, `End` and `Closed`; a program ends
//! with `Exited`. Reads are one chunk per `Read` credit, so a child that writes faster than
//! JavaScript reads is held back by the full pipe, not buffered here; a write is reported when
//! the system took all of it, so a `Writable` has one chunk in flight at a time.
//!
//! Readiness is edge-triggered, so each pipe remembers whether it last saw "would block" and
//! tries again only when an event or a new credit says it may.

use std::collections::{HashMap, VecDeque};
use std::io::{self, Read, Write};

use mio::{Interest, Poll, Token, event::Event as Ready};

use super::launch::{Flow, Io, Process};
use crate::network::reactor::{Command, Event, Id, Output};

/// How much one `Data` event carries at most.
const CHUNK: usize = 64 * 1024;

/// The parent's end of one pipe.
struct Channel {
    /// `None` once the sending side has been closed on a pipe that only sends.
    io: Option<Io>,
    flow: Flow,
    credit: bool,
    readable: bool,
    writable: bool,
    /// A write was accepted and Windows has not yet said it was done with it: closing the
    /// pipe now could cut the last chunk short.
    inflight: bool,
    ended: bool,
    finished: bool,
    writes: VecDeque<(Vec<u8>, usize)>,
    end: bool,
}

/// Every pipe and running program of one reactor.
#[derive(Default)]
pub(crate) struct Pipes {
    channels: HashMap<Id, Channel>,
    children: HashMap<Id, Process>,
}

impl Pipes {
    /// Takes `command` if it is about a pipe or a program; hands it back otherwise.
    pub(crate) fn command(
        &mut self,
        command: Command,
        poll: &Poll,
        out: &Output,
    ) -> Option<Command> {
        match command {
            Command::Pipe(id, io, flow) => self.open(id, io, flow, poll, out),
            Command::Track(id, process) => {
                self.children.insert(id, process);
            }
            Command::Kill(id, signal) => {
                if let Some(process) = self.children.get_mut(&id) {
                    let _ = process.kill(signal);
                }
            }
            #[cfg(windows)]
            Command::Reap => self.reap(out),
            Command::Read(id, on) if self.channels.contains_key(&id) => {
                if let Some(c) = self.channels.get_mut(&id) {
                    c.credit = on;
                }
            }
            Command::Write(id, data) if self.channels.contains_key(&id) => {
                if let Some(c) = self.channels.get_mut(&id) {
                    c.writes.push_back((data, 0));
                }
            }
            Command::End(id) if self.channels.contains_key(&id) => {
                if let Some(c) = self.channels.get_mut(&id) {
                    c.end = true;
                }
            }
            Command::Close(id)
                if self.channels.contains_key(&id) || self.children.contains_key(&id) =>
            {
                if let Some(mut c) = self.channels.remove(&id)
                    && let Some(io) = c.io.as_mut()
                {
                    let _ = poll.registry().deregister(io);
                }
                self.children.remove(&id);
                let _ = out.send((id, Event::Closed));
            }
            other => return Some(other),
        }
        None
    }

    fn open(&mut self, id: Id, mut io: Io, flow: Flow, poll: &Poll, out: &Output) {
        let interest = Interest::READABLE | Interest::WRITABLE;
        if let Err(e) = poll
            .registry()
            .register(&mut io, Token(id as usize), interest)
        {
            let _ = out.send((id, Event::Error("EIO".into(), format!("pipe: {e}"))));
            return;
        }
        self.channels.insert(
            id,
            Channel {
                io: Some(io),
                flow,
                credit: false,
                readable: true,
                writable: true,
                inflight: false,
                ended: false,
                finished: false,
                writes: VecDeque::new(),
                end: false,
            },
        );
    }

    /// Notes what the system said about a pipe; answers whether `ready` was one of ours.
    pub(crate) fn ready(&mut self, id: Id, ready: &Ready) -> bool {
        let Some(channel) = self.channels.get_mut(&id) else {
            return false;
        };
        if ready.is_readable() || ready.is_read_closed() || ready.is_error() {
            channel.readable = true;
        }
        if ready.is_writable() || ready.is_write_closed() || ready.is_error() {
            channel.writable = true;
            channel.inflight = false;
        }
        true
    }

    /// One turn: writes flushed, ends delivered, reads made, programs that have ended reported.
    pub(crate) fn pump(&mut self, out: &Output) {
        for (&id, channel) in self.channels.iter_mut() {
            channel.write(id, out);
            channel.finish(id, out);
            channel.read(id, out);
        }
        self.reap(out);
    }

    fn reap(&mut self, out: &Output) {
        let ended: Vec<(Id, super::launch::Exit)> = self
            .children
            .iter_mut()
            .filter_map(|(&id, process)| process.try_exit().map(|exit| (id, exit)))
            .collect();
        for (id, exit) in ended {
            self.children.remove(&id);
            let _ = out.send((id, Event::Exited(exit.code, exit.signal)));
        }
    }
}

impl Channel {
    fn write(&mut self, id: Id, out: &Output) {
        let Some(io) = self.io.as_mut() else {
            return;
        };
        while self.writable {
            let Some((data, at)) = self.writes.front_mut() else {
                break;
            };
            if *at < data.len() {
                match io.write(&data[*at..]) {
                    Ok(n) => {
                        *at += n;
                        self.inflight = cfg!(windows);
                    }
                    Err(e) if e.kind() == io::ErrorKind::WouldBlock => {
                        self.writable = false;
                        break;
                    }
                    Err(e) => {
                        self.writes.clear();
                        self.inflight = false;
                        let _ =
                            out.send((id, Event::Error(code_of(&e).into(), format!("write {e}"))));
                        break;
                    }
                }
            }
            if *at >= data.len() {
                let len = data.len();
                self.writes.pop_front();
                let _ = out.send((id, Event::Written(len)));
            }
        }
    }

    /// The end of what the parent sends, once everything it sent has been taken.
    fn finish(&mut self, id: Id, out: &Output) {
        if !self.end || self.finished || self.inflight || !self.writes.is_empty() {
            return;
        }
        self.finished = true;
        match (self.flow, self.io.as_mut()) {
            // A pipe that only sends is closed outright: that is the end the child reads.
            (Flow::ToChild, _) => self.io = None,
            (Flow::Both, Some(io)) => {
                io.shutdown_write();
            }
            _ => {}
        }
        let _ = out.send((id, Event::Finished));
    }

    fn read(&mut self, id: Id, out: &Output) {
        if !(self.credit && self.readable && !self.ended && self.flow != Flow::ToChild) {
            return;
        }
        let Some(io) = self.io.as_mut() else {
            return;
        };
        let mut chunk = vec![0; CHUNK];
        match io.read(&mut chunk) {
            Ok(0) => self.ended_with(id, out, Event::End),
            Ok(n) => {
                chunk.truncate(n);
                self.credit = false;
                let _ = out.send((id, Event::Data(chunk)));
            }
            Err(e) if e.kind() == io::ErrorKind::WouldBlock => self.readable = false,
            // A peer that went away reads as the end of the stream, whichever way the system says so.
            Err(e) if is_closed(&e) => self.ended_with(id, out, Event::End),
            Err(e) => {
                let event = Event::Error(code_of(&e).into(), format!("read {e}"));
                self.ended_with(id, out, event);
            }
        }
    }

    fn ended_with(&mut self, id: Id, out: &Output, event: Event) {
        self.ended = true;
        let _ = out.send((id, event));
    }
}

fn is_closed(error: &io::Error) -> bool {
    matches!(
        error.kind(),
        io::ErrorKind::BrokenPipe | io::ErrorKind::ConnectionReset | io::ErrorKind::UnexpectedEof
    )
}

fn code_of(error: &io::Error) -> &'static str {
    if is_closed(error) { "EPIPE" } else { "EIO" }
}

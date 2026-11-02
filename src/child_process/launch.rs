//! Starting a program, the same way on every system: [`spawn`] takes a [`SpawnSpec`] and gives
//! back the running [`Process`] and the parent's end of each pipe it was started with.

use super::spec::{SpawnSpec, Stdio};

#[cfg(unix)]
use super::unix as sys;
#[cfg(windows)]
use super::windows as sys;

pub(crate) use sys::{Blocking, Io, Process, classify, kill_pid};

/// Who reads the parent's ends of the pipes.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Mode {
    /// The network reactor, which polls them without blocking.
    Reactor,
    /// The caller, which blocks on them: `spawnSync`.
    Blocking,
}

/// Which way data goes on a pipe, from the parent's side.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Flow {
    /// Standard input: the parent only writes.
    ToChild,
    /// Standard output and error: the parent only reads.
    FromChild,
    /// The IPC channel and any pipe past the standard three.
    Both,
}

impl Flow {
    /// The flow of the pipe at descriptor `index`.
    pub(crate) fn of(entry: Stdio, index: usize) -> Self {
        match (entry, index) {
            (Stdio::Pipe, 0) => Self::ToChild,
            (Stdio::Pipe, 1 | 2) => Self::FromChild,
            _ => Self::Both,
        }
    }
}

/// How a program ended: its exit code, or the signal that ended it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Exit {
    pub code: Option<i64>,
    pub signal: Option<i32>,
}

/// The parent's end of one pipe.
pub(crate) enum Parent {
    Reactor(Io),
    Blocking(Blocking),
}

/// The parent's end of each pipe, with the descriptor index it is the other end of.
pub(crate) type Ends = Vec<(usize, Parent)>;

/// A program that has started.
pub(crate) struct Started {
    pub process: Process,
    pub pipes: Ends,
}

/// Starts the program `spec` describes.
pub(crate) fn spawn(spec: &SpawnSpec, mode: Mode) -> std::io::Result<Started> {
    sys::spawn(spec, mode)
}

/// The IPC channel or pipe this process was started with at descriptor `fd`.
pub(crate) fn inherited(fd: i32) -> Option<Io> {
    Io::inherited(fd)
}

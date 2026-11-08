//! Starting a program on Unix with `std::process::Command`, whose `fork`/`exec` is as libuv's.
//! The parent's end of every pipe is one half of a socket pair, as libuv makes them, so the same
//! descriptor serves a standard stream and the two-way IPC channel. Descriptors past 2 are
//! put in place in `pre_exec`, between `fork` and `exec`, where only async-signal-safe calls
//! may be made.

use std::io;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::os::unix::net::UnixStream;
use std::os::unix::process::{CommandExt, ExitStatusExt};
use std::process::{Child, Command};
use std::time::{Duration, Instant};

use super::launch::{Exit, Mode, Parent, Started};
use super::spec::{SpawnSpec, Stdio};
use crate::network::reactor::Sender;

/// The blocking end of a pipe, for the synchronous calls.
pub(crate) type Blocking = UnixStream;

/// The parent's end of a pipe, for the reactor to poll.
pub(crate) struct Io(mio::net::UnixStream);

impl Io {
    /// A pipe the child was started with, found at descriptor `fd`.
    pub(super) fn inherited(fd: i32) -> Option<Self> {
        let mut status = std::mem::MaybeUninit::<libc::stat>::uninit();
        // SAFETY: `status` is a place for the answer; the call only reads the descriptor.
        let known = unsafe { libc::fstat(fd, status.as_mut_ptr()) } == 0;
        // SAFETY: `fstat` succeeded, so `status` is initialised.
        let is_socket =
            known && unsafe { status.assume_init() }.st_mode & libc::S_IFMT == libc::S_IFSOCK;
        if !is_socket {
            return None;
        }
        // SAFETY: the descriptor was handed down for this use and nothing else owns it.
        let stream = unsafe { UnixStream::from_raw_fd(fd) };
        // Not for the programs this one starts in its turn.
        // SAFETY: `fcntl` takes numbers only.
        unsafe { libc::fcntl(fd, libc::F_SETFD, libc::FD_CLOEXEC) };
        stream.set_nonblocking(true).ok()?;
        Some(Self(mio::net::UnixStream::from_std(stream)))
    }

    /// Closes the sending half, so the peer reads the end of the stream.
    pub(super) fn shutdown_write(&mut self) -> bool {
        self.0.shutdown(std::net::Shutdown::Write).is_ok()
    }
}

impl io::Read for Io {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        io::Read::read(&mut self.0, buffer)
    }
}

impl io::Write for Io {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        io::Write::write(&mut self.0, buffer)
    }

    fn flush(&mut self) -> io::Result<()> {
        io::Write::flush(&mut self.0)
    }
}

impl mio::event::Source for Io {
    fn register(
        &mut self,
        registry: &mio::Registry,
        token: mio::Token,
        interests: mio::Interest,
    ) -> io::Result<()> {
        self.0.register(registry, token, interests)
    }

    fn reregister(
        &mut self,
        registry: &mio::Registry,
        token: mio::Token,
        interests: mio::Interest,
    ) -> io::Result<()> {
        self.0.reregister(registry, token, interests)
    }

    fn deregister(&mut self, registry: &mio::Registry) -> io::Result<()> {
        self.0.deregister(registry)
    }
}

/// A started program.
pub(crate) struct Process(Child);

impl Process {
    pub(super) fn pid(&self) -> u32 {
        self.0.id()
    }

    /// Nothing to arrange: the reactor looks at a program that may have ended on every turn.
    pub(super) fn watch(&mut self, _: Sender) -> io::Result<()> {
        Ok(())
    }

    /// How the program ended, if it has.
    pub(super) fn try_exit(&mut self) -> Option<Exit> {
        self.0.try_wait().ok().flatten().map(exit_of)
    }

    /// Waits for the program to end, at most `timeout`.
    pub(super) fn wait(&mut self, timeout: Option<Duration>) -> Option<Exit> {
        let Some(timeout) = timeout else {
            return self.0.wait().ok().map(exit_of);
        };
        let deadline = Instant::now() + timeout;
        loop {
            if let Some(exit) = self.try_exit() {
                return Some(exit);
            }
            if Instant::now() >= deadline {
                return None;
            }
            std::thread::sleep(Duration::from_millis(1));
        }
    }

    pub(super) fn kill(&mut self, signal: i32) -> io::Result<()> {
        kill_pid(i64::from(self.pid()), signal)
    }
}

/// `process.kill(pid, signal)`.
/// Every signal means something here; a negative pid is a process group.
pub(crate) fn check_signal(_: i32) -> io::Result<()> {
    Ok(())
}

pub(crate) fn kill_pid(pid: i64, signal: i32) -> io::Result<()> {
    let pid = libc::pid_t::try_from(pid).map_err(|_| io::Error::from_raw_os_error(libc::ESRCH))?;
    // SAFETY: `kill` takes numbers only.
    if unsafe { libc::kill(pid, signal) } == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

/// Starts the program `spec` describes.
pub(super) fn spawn(spec: &SpawnSpec, mode: Mode) -> io::Result<Started> {
    let mut command = Command::new(&spec.file);
    command
        .arg0(&spec.args[0])
        .args(&spec.args[1..])
        .current_dir(&spec.cwd)
        .env_clear()
        .envs(spec.env.iter().map(|(k, v)| (k, v)));
    if let Some(uid) = spec.uid {
        command.uid(uid);
    }
    if let Some(gid) = spec.gid {
        command.gid(gid);
    }
    let mut parents = Vec::new();
    let mut extra: Vec<(OwnedFd, RawFd)> = Vec::new();
    for (index, entry) in spec.stdio.iter().enumerate() {
        let child: Option<OwnedFd> = match entry {
            Stdio::Ignore => Some(open_null()?),
            Stdio::Inherit(fd) if *fd as usize == index => None,
            Stdio::Inherit(fd) => Some(duplicate(*fd)?),
            Stdio::Pipe | Stdio::Ipc => {
                let (parent, child) = UnixStream::pair()?;
                parents.push((index, parent_end(parent, mode)?));
                Some(child.into())
            }
        };
        match (index, child) {
            (0, Some(fd)) => command.stdin(fd),
            (1, Some(fd)) => command.stdout(fd),
            (2, Some(fd)) => command.stderr(fd),
            (_, Some(fd)) => {
                extra.push((fd, index as RawFd));
                &mut command
            }
            (_, None) => &mut command,
        };
    }
    let detached = spec.detached;
    let places: Vec<(RawFd, RawFd)> = extra.iter().map(|(fd, to)| (fd.as_raw_fd(), *to)).collect();
    // SAFETY: the closure runs between `fork` and `exec` and calls only `setsid`, `fcntl`,
    // `dup2` and `close`, which are async-signal-safe, on numbers decided before the fork.
    unsafe {
        command.pre_exec(move || {
            if detached {
                libc::setsid();
            }
            place_descriptors(&places)
        });
    }
    let child = command.spawn()?;
    Ok(Started {
        process: Process(child),
        pipes: parents,
    })
}

/// The parent's end of a pipe: nonblocking for the reactor, as it is for the synchronous calls.
fn parent_end(stream: UnixStream, mode: Mode) -> io::Result<Parent> {
    match mode {
        Mode::Reactor => {
            stream.set_nonblocking(true)?;
            Ok(Parent::Reactor(Io(mio::net::UnixStream::from_std(stream))))
        }
        Mode::Blocking => Ok(Parent::Blocking(stream)),
    }
}

fn open_null() -> io::Result<OwnedFd> {
    Ok(std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open("/dev/null")?
        .into())
}

fn duplicate(fd: RawFd) -> io::Result<OwnedFd> {
    // SAFETY: `fcntl` with F_DUPFD_CLOEXEC takes numbers only; the result is a new descriptor.
    let copy = unsafe { libc::fcntl(fd, libc::F_DUPFD_CLOEXEC, 3) };
    if copy < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: `copy` is a descriptor that was just made and nothing else owns.
    Ok(unsafe { OwnedFd::from_raw_fd(copy) })
}

/// Puts each `(from, to)` in place, in the child. A source is first moved out of the way of
/// every target, so placing one cannot overwrite another still to be placed.
fn place_descriptors(places: &[(RawFd, RawFd)]) -> io::Result<()> {
    let floor = places.iter().map(|(_, to)| *to).max().unwrap_or(0) + 1;
    let mut moved = [(0, 0); 16];
    if places.len() > moved.len() {
        return Err(io::Error::from_raw_os_error(libc::EMFILE));
    }
    for (slot, (from, to)) in moved.iter_mut().zip(places) {
        // SAFETY: `fcntl` takes numbers only.
        let safe = unsafe { libc::fcntl(*from, libc::F_DUPFD, floor) };
        if safe < 0 {
            return Err(io::Error::last_os_error());
        }
        *slot = (safe, *to);
    }
    for (safe, to) in &moved[..places.len()] {
        // SAFETY: `dup2` and `close` take numbers only. `dup2` clears close-on-exec on `to`.
        unsafe {
            if libc::dup2(*safe, *to) < 0 {
                return Err(io::Error::last_os_error());
            }
            libc::close(*safe);
        }
    }
    Ok(())
}

/// The name Node reports for a failed start and the negative errno libuv gives it.
pub(crate) fn classify(error: &io::Error) -> (&'static str, i64) {
    let number = error.raw_os_error().unwrap_or(libc::EIO);
    let name = crate::os::ERRNO
        .iter()
        .find(|(_, known)| *known == i64::from(number))
        .map_or("UNKNOWN", |(name, _)| *name);
    (name, -i64::from(number))
}

fn exit_of(status: std::process::ExitStatus) -> Exit {
    Exit {
        code: status.code().map(i64::from),
        signal: status.signal(),
    }
}

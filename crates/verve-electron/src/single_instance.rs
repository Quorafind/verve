//! `app.requestSingleInstanceLock()`: one running instance of an app per profile directory.
//!
//! The lock belongs to the profile directory (`userData`), not the executable, as Electron's
//! does: the key is the first sixteen hex digits of the SHA-256 of its path. The first process
//! to ask holds the lock and listens; a later one sends its command line, working directory and
//! `additionalData` as one JSON object and waits for a byte that says the first has it, then is
//! told `false`. On Windows the lock is a named mutex (per session) and the channel a named pipe,
//! made first by this process and open to this user only, and checked by the second instance to
//! be served by this user before it sends a byte; elsewhere
//! an `flock`ed `SingletonLock` file in the profile directory, and a Unix socket that
//! the profile's `SingletonSocket` link points to. The socket is in a folder only its user can
//! enter (`$XDG_RUNTIME_DIR`, or a `0700` folder of an unguessable name under `/tmp`, a profile
//! path being too long for a socket's), which a second instance checks before it trusts it.
//! `additionalData` crosses as JSON, so a `Date` arrives as its string, as it does in Electron.
//!
//! A lock that cannot be taken for a reason other than another instance holding it — no file to
//! lock, no mutex — is refused, as Electron's `LOCK_ERROR` is: `requestSingleInstanceLock()`
//! answers `false`. Once the lock is held, a socket or thread that cannot be made leaves the app
//! holding it with nobody listening; a later instance then waits out its five seconds and ends.

use std::io::{BufRead, BufReader, Read, Write};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, mpsc};
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use crate::hub::Hub;
use crate::launcher::Start;
use crate::msg::{AppEvent, FromShell};

/// How long a second instance waits for the first to answer, and for its channel to exist.
const PATIENCE: Duration = Duration::from_secs(5);

/// The lock this process holds. Dropping it releases the lock and ends the listener.
pub(crate) struct SingleInstance {
    stop: Arc<AtomicBool>,
    /// Set by the listener's thread as it ends.
    done: Arc<AtomicBool>,
    /// Wakes the listener so it sees `stop`.
    wake: Box<dyn Fn() + Send + Sync>,
    _held: imp::Held,
}

impl Drop for SingleInstance {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        // The listener may be about to make the channel it waits on, where a wake finds nobody:
        // wake it until it says it is gone.
        for _ in 0..200 {
            if self.done.load(Ordering::SeqCst) {
                return;
            }
            (self.wake)();
            std::thread::sleep(Duration::from_millis(10));
        }
    }
}

impl SingleInstance {
    /// Takes the lock for the profile directory `user_data` and answers it, or — when another
    /// process holds it — hands that process `argv`, the working directory and `data` (JSON
    /// text) and answers `None`.
    pub(crate) fn request(
        user_data: &Path,
        hub: &Arc<Hub>,
        argv: Vec<String>,
        data: String,
    ) -> Option<Self> {
        // A directory that cannot be made fails the lock's file next, which is reported there.
        let _ = std::fs::create_dir_all(user_data);
        let key = key_of(user_data);
        let message = encode(&argv, &data);
        imp::request(user_data, &key, &message, hub)
    }

    /// An instance that holds the lock and listens to nobody.
    fn unguarded(why: &str, held: imp::Held) -> Self {
        eprintln!("electron: the single instance lock is held but cannot be listened on ({why})");
        Self {
            stop: Arc::new(AtomicBool::new(true)),
            done: Arc::new(AtomicBool::new(true)),
            wake: Box::new(|| {}),
            _held: held,
        }
    }
}

/// The key a profile directory is locked under.
fn key_of(user_data: &Path) -> String {
    let path = std::fs::canonicalize(user_data).unwrap_or_else(|_| user_data.to_path_buf());
    let mut text = path.to_string_lossy().into_owned();
    if cfg!(windows) {
        text.make_ascii_lowercase();
    }
    let digest = Sha256::digest(text.as_bytes());
    verve::encoding::hex(&digest)[..16].to_owned()
}

/// The command line a second instance reports: the executable, the switches, then the rest —
/// the order Chromium's `CommandLine` gives `second-instance`.
pub(crate) fn second_instance_argv(start: &Start) -> Vec<String> {
    let (switches, rest): (Vec<_>, Vec<_>) = start
        .argv
        .iter()
        .partition(|arg| arg.starts_with('-') && *arg != "-");
    std::iter::once(crate::launcher::exe_path().to_string_lossy().into_owned())
        .chain(switches.into_iter().chain(rest).cloned())
        .collect()
}

/// One line of JSON: what a second instance sends.
fn encode(argv: &[String], data: &str) -> String {
    let cwd = std::env::current_dir().unwrap_or_default();
    let data: Value = serde_json::from_str(data).unwrap_or(Value::Null);
    let message = json!({ "argv": argv, "cwd": cwd.to_string_lossy(), "additionalData": data });
    format!("{message}\n")
}

/// Reads one second instance's message from `stream`, tells the app, and answers the byte that
/// says it was heard.
fn receive(stream: impl Read + Write, hub: &Hub) {
    let mut stream = BufReader::new(stream);
    let mut line = String::new();
    if stream.read_line(&mut line).is_err() {
        return;
    }
    let Ok(message) = serde_json::from_str::<Value>(&line) else {
        return;
    };
    let argv = message["argv"]
        .as_array()
        .map(|list| {
            list.iter()
                .filter_map(|a| a.as_str().map(str::to_owned))
                .collect()
        })
        .unwrap_or_default();
    let cwd = message["cwd"].as_str().unwrap_or_default().into();
    let data = message["additionalData"].to_string();
    hub.from_shell(FromShell::App(AppEvent::SecondInstance { argv, cwd, data }));
    let stream = stream.get_mut();
    // The second instance that went away before the answer has nothing to be told.
    if stream.write_all(&[1]).is_ok() {
        let _ = stream.flush();
    }
}

/// Sends `message` to the first instance and waits for its byte, but not longer than
/// [`PATIENCE`]: a first instance that is stuck does not stop the second from ending.
fn send(mut stream: impl Read + Write + Send + 'static, message: &str) {
    let message = message.to_owned();
    let (done, wait) = mpsc::channel();
    std::thread::spawn(move || {
        if stream.write_all(message.as_bytes()).is_ok() && stream.flush().is_ok() {
            // Whatever comes back, or none, the wait is over.
            let _ = stream.read(&mut [0u8; 1]);
        }
        // The waiter that gave up is gone, and has no use for the news.
        let _ = done.send(());
    });
    let _ = wait.recv_timeout(PATIENCE);
}

/// Ends a listener's `done` flag when its thread ends, however it does.
struct Finished(Arc<AtomicBool>);

impl Drop for Finished {
    fn drop(&mut self) {
        self.0.store(true, Ordering::SeqCst);
    }
}

#[cfg(windows)]
mod imp {
    use std::ffi::c_void;
    use std::fs::{File, OpenOptions};
    use std::os::windows::io::{AsRawHandle, FromRawHandle, RawHandle};

    use windows_sys::Win32::Foundation::{
        CloseHandle, ERROR_ALREADY_EXISTS, ERROR_PIPE_CONNECTED, GetLastError, HANDLE,
        INVALID_HANDLE_VALUE, LocalFree,
    };
    use windows_sys::Win32::Security::Authorization::{
        ConvertSidToStringSidW, ConvertStringSecurityDescriptorToSecurityDescriptorW,
        SDDL_REVISION_1,
    };
    use windows_sys::Win32::Security::{
        EqualSid, GetTokenInformation, PSID, SECURITY_ATTRIBUTES, TOKEN_QUERY, TOKEN_USER,
        TokenUser,
    };
    use windows_sys::Win32::Storage::FileSystem::{
        FILE_FLAG_FIRST_PIPE_INSTANCE, PIPE_ACCESS_DUPLEX,
    };
    use windows_sys::Win32::System::Pipes::{
        ConnectNamedPipe, CreateNamedPipeW, GetNamedPipeServerProcessId, PIPE_TYPE_BYTE,
        PIPE_UNLIMITED_INSTANCES, PIPE_WAIT,
    };
    use windows_sys::Win32::System::Threading::{
        CreateMutexW, GetCurrentProcess, OpenProcess, OpenProcessToken,
        PROCESS_QUERY_LIMITED_INFORMATION,
    };

    use super::*;
    use crate::api::app::platform::wide;

    /// A named mutex, held until dropped.
    pub(super) struct Held(HANDLE);

    // SAFETY: a mutex handle is a kernel object that any thread may close.
    unsafe impl Send for Held {}

    impl Drop for Held {
        fn drop(&mut self) {
            // SAFETY: the handle was returned by `CreateMutexW` and is closed once.
            unsafe { CloseHandle(self.0) };
        }
    }

    /// The user a process runs as: the `TOKEN_USER` the system filled in, kept whole because the
    /// SID it holds points into it (as `u64`s, so that it is aligned).
    struct UserSid(Vec<u64>);

    impl UserSid {
        fn of(process: HANDLE) -> Option<Self> {
            let mut token: HANDLE = std::ptr::null_mut();
            // SAFETY: `token` is an out-pointer; it is closed below. The first query only
            // measures, the second fills a buffer of that size.
            unsafe {
                if OpenProcessToken(process, TOKEN_QUERY, &mut token) == 0 {
                    return None;
                }
                let mut bytes = 0u32;
                GetTokenInformation(token, TokenUser, std::ptr::null_mut(), 0, &mut bytes);
                let mut buffer = vec![0u64; (bytes as usize).div_ceil(8).max(1)];
                let read = GetTokenInformation(
                    token,
                    TokenUser,
                    buffer.as_mut_ptr().cast(),
                    bytes,
                    &mut bytes,
                );
                CloseHandle(token);
                (read != 0).then_some(Self(buffer))
            }
        }

        fn sid(&self) -> PSID {
            // SAFETY: the buffer holds the `TOKEN_USER` `GetTokenInformation` wrote.
            unsafe { (*self.0.as_ptr().cast::<TOKEN_USER>()).User.Sid }
        }
    }

    /// A security descriptor that lets the current user, and nobody else, use the pipe.
    struct OwnerOnly(*mut c_void);

    // SAFETY: a descriptor is plain memory the system allocated, read by whoever holds it.
    unsafe impl Send for OwnerOnly {}

    impl Drop for OwnerOnly {
        fn drop(&mut self) {
            // SAFETY: allocated by the conversion below, freed once.
            unsafe { LocalFree(self.0) };
        }
    }

    impl OwnerOnly {
        fn new() -> Option<Self> {
            let user = UserSid::of(unsafe_current_process())?;
            let mut text: *mut u16 = std::ptr::null_mut();
            let mut descriptor: *mut c_void = std::ptr::null_mut();
            // SAFETY: the SID string and the descriptor are allocated by the system; the string
            // is copied and freed, the descriptor is freed by `Drop`.
            unsafe {
                if ConvertSidToStringSidW(user.sid(), &mut text) == 0 {
                    return None;
                }
                let length = (0..).take_while(|&i| *text.add(i) != 0).count();
                let sid = String::from_utf16_lossy(std::slice::from_raw_parts(text, length));
                LocalFree(text.cast());
                let sddl = wide(&format!("D:P(A;;GA;;;{sid})"));
                let made = ConvertStringSecurityDescriptorToSecurityDescriptorW(
                    sddl.as_ptr(),
                    SDDL_REVISION_1,
                    &mut descriptor,
                    std::ptr::null_mut(),
                );
                (made != 0).then_some(Self(descriptor))
            }
        }

        fn attributes(&self) -> SECURITY_ATTRIBUTES {
            SECURITY_ATTRIBUTES {
                nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
                lpSecurityDescriptor: self.0,
                bInheritHandle: 0,
            }
        }
    }

    fn unsafe_current_process() -> HANDLE {
        // SAFETY: a pseudo-handle that needs no closing.
        unsafe { GetCurrentProcess() }
    }

    /// A pipe handle that may go to the listener's thread.
    struct Pipe(HANDLE);

    // SAFETY: a pipe handle is a kernel object that any thread may use.
    unsafe impl Send for Pipe {}

    /// A new instance of the pipe `name`, open to the current user only. The first one is made with
    /// `FILE_FLAG_FIRST_PIPE_INSTANCE`, which fails if anyone made the name before: a pipe is
    /// visible to every session of the machine, a mutex of `Local\` is not.
    fn make_pipe(name: &[u16], access: &OwnerOnly, first: bool) -> Option<Pipe> {
        let attributes = access.attributes();
        let open = if first {
            PIPE_ACCESS_DUPLEX | FILE_FLAG_FIRST_PIPE_INSTANCE
        } else {
            PIPE_ACCESS_DUPLEX
        };
        // SAFETY: a NUL-terminated name and attributes that outlive the call.
        let handle = unsafe {
            CreateNamedPipeW(
                name.as_ptr(),
                open,
                PIPE_TYPE_BYTE | PIPE_WAIT,
                PIPE_UNLIMITED_INSTANCES,
                65536,
                65536,
                0,
                &attributes,
            )
        };
        (handle != INVALID_HANDLE_VALUE).then_some(Pipe(handle))
    }

    pub(super) fn request(
        _: &Path,
        key: &str,
        message: &str,
        hub: &Arc<Hub>,
    ) -> Option<SingleInstance> {
        let name = wide(&format!("Local\\verve-electron-{key}"));
        // SAFETY: a NUL-terminated name; the last error is read before anything else runs.
        let (handle, existed) = unsafe {
            let handle = CreateMutexW(std::ptr::null(), 0, name.as_ptr());
            (handle, GetLastError() == ERROR_ALREADY_EXISTS)
        };
        if handle.is_null() {
            eprintln!("electron: the single instance lock could not be taken (no mutex)");
            return None;
        }
        let held = Held(handle);
        let pipe = format!(r"\\.\pipe\verve-electron-{key}");
        if existed {
            drop(held);
            tell_first(&pipe, message);
            return None;
        }
        // The lock is held. What fails from here leaves the app with the lock and nobody to hear
        // a second instance: a later one waits out its patience and ends.
        let Some(access) = OwnerOnly::new() else {
            return Some(SingleInstance::unguarded("no security descriptor", held));
        };
        let pipe_name = wide(&pipe);
        let Some(first) = make_pipe(&pipe_name, &access, true) else {
            let why = "the pipe's name is taken by another process";
            return Some(SingleInstance::unguarded(why, held));
        };
        let (stop, done) = (
            Arc::new(AtomicBool::new(false)),
            Arc::new(AtomicBool::new(false)),
        );
        let spawned = listen(
            (pipe_name, access, first),
            Arc::clone(&stop),
            Arc::clone(&done),
            Arc::downgrade(hub),
        );
        if let Err(error) = spawned {
            return Some(SingleInstance::unguarded(&error.to_string(), held));
        }
        Some(SingleInstance {
            stop,
            done,
            wake: Box::new(move || {
                // A connection that fails means nobody is waiting yet; the caller tries again.
                let _ = OpenOptions::new().read(true).write(true).open(&pipe);
            }),
            _held: held,
        })
    }

    /// Whether the process serving `pipe` runs as the same user as this one. A pipe is visible to
    /// every session, so a second instance sends its command line and data to no one else.
    fn served_by_this_user(pipe: &File) -> bool {
        let mut pid = 0u32;
        // SAFETY: `pid` is an out-pointer; the process handle opened is closed.
        let theirs = unsafe {
            if GetNamedPipeServerProcessId(pipe.as_raw_handle() as HANDLE, &mut pid) == 0 {
                return false;
            }
            let process = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
            if process.is_null() {
                return false;
            }
            let user = UserSid::of(process);
            CloseHandle(process);
            user
        };
        let ours = UserSid::of(unsafe_current_process());
        match (ours, theirs) {
            // SAFETY: both SIDs are valid, kept alive by their buffers.
            (Some(ours), Some(theirs)) => unsafe { EqualSid(ours.sid(), theirs.sid()) != 0 },
            _ => false,
        }
    }

    /// Connects to the first instance's pipe — which it may still be making — and sends, unless
    /// `trusted` says the pipe is not the first instance's.
    fn tell_first(pipe: &str, message: &str) {
        tell_first_if(pipe, message, served_by_this_user);
    }

    fn tell_first_if(pipe: &str, message: &str, trusted: fn(&File) -> bool) {
        let deadline = Instant::now() + PATIENCE;
        while Instant::now() < deadline {
            if let Ok(file) = OpenOptions::new().read(true).write(true).open(pipe) {
                if trusted(&file) {
                    super::send(file, message);
                } else {
                    eprintln!("electron: the single instance pipe is not served by this user");
                }
                return;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    /// Serves the pipe on a thread of its own until `stop`, one second instance at a time.
    fn listen(
        (name, access, first): (Vec<u16>, OwnerOnly, Pipe),
        stop: Arc<AtomicBool>,
        done: Arc<AtomicBool>,
        hub: std::sync::Weak<Hub>,
    ) -> std::io::Result<()> {
        std::thread::Builder::new()
            .name("single-instance".into())
            .spawn(move || {
                let _finished = Finished(done);
                let mut next = Some(first);
                while !stop.load(Ordering::SeqCst) {
                    let Some(Pipe(handle)) =
                        next.take().or_else(|| make_pipe(&name, &access, false))
                    else {
                        return;
                    };
                    // SAFETY: `handle` is a pipe server end; the call blocks until a client comes,
                    // and the last error is read before anything else can change it.
                    let (connected, error) = unsafe {
                        (
                            ConnectNamedPipe(handle, std::ptr::null_mut()),
                            GetLastError(),
                        )
                    };
                    // SAFETY: the handle is a valid file handle that nothing else owns.
                    let file = unsafe { File::from_raw_handle(handle as RawHandle) };
                    // A client that connected between the pipe's making and this call is an error
                    // that is not one.
                    let client = connected != 0 || error == ERROR_PIPE_CONNECTED;
                    if client
                        && !stop.load(Ordering::SeqCst)
                        && let Some(hub) = hub.upgrade()
                    {
                        serve(file, &hub);
                    }
                }
            })
            .map(drop)
    }

    /// Hears one second instance, then waits until it has read the answer.
    fn serve(file: File, hub: &Hub) {
        let sync = file.try_clone();
        super::receive(file, hub);
        // The answer is read by the client before the pipe is closed on it.
        if let Ok(file) = sync {
            // A pipe whose client left has nothing more to wait for.
            let _ = file.sync_all();
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use crate::ElectronOptions;

        fn hub() -> Arc<Hub> {
            Hub::new(ElectronOptions {
                app_path: std::env::temp_dir(),
                dark: false,
            })
        }

        /// A pipe made by this user is served by this user.
        #[test]
        fn a_pipe_this_user_serves_is_trusted() {
            let name = format!(r"\\.\pipe\verve-test-trusted-{}", std::process::id());
            let wide_name = wide(&name);
            let access = OwnerOnly::new().expect("a descriptor");
            let server = make_pipe(&wide_name, &access, true).expect("a pipe");
            let file = OpenOptions::new()
                .read(true)
                .write(true)
                .open(&name)
                .unwrap();
            assert!(served_by_this_user(&file));
            drop(file);
            // SAFETY: the handle is the pipe made above, closed once.
            unsafe { CloseHandle(server.0) };
        }

        /// A second instance that does not trust the pipe sends nothing to it: a server that
        /// would take the message hears nothing and is closed on.
        #[test]
        fn nothing_is_sent_to_a_pipe_that_is_not_trusted() {
            let name = format!(r"\\.\pipe\verve-test-untrusted-{}", std::process::id());
            let wide_name = wide(&name);
            let access = OwnerOnly::new().expect("a descriptor");
            let server = make_pipe(&wide_name, &access, true).expect("a pipe");
            let handle = server.0 as usize;
            let heard = std::thread::spawn(move || {
                let handle = handle as HANDLE;
                // SAFETY: the pipe made above; this thread owns it from here.
                unsafe { ConnectNamedPipe(handle, std::ptr::null_mut()) };
                // SAFETY: the pipe's handle, owned by this thread now and closed with the file.
                let mut file = unsafe { File::from_raw_handle(handle as RawHandle) };
                let mut bytes = Vec::new();
                let _ = file.read_to_end(&mut bytes);
                bytes
            });
            tell_first_if(&name, "secret\n", |_| false);
            assert!(heard.join().unwrap().is_empty());
        }

        /// A name somebody else made first is not taken over: the first instance of a pipe is
        /// made with `FILE_FLAG_FIRST_PIPE_INSTANCE`, and the lock is then held with no listener.
        #[test]
        fn a_pipe_name_that_is_taken_leaves_the_lock_without_a_listener() {
            let profile =
                std::env::temp_dir().join(format!("verve-pipe-squat-{}", std::process::id()));
            let _ = std::fs::create_dir_all(&profile);
            let key = key_of(&profile);
            let name = format!(r"\\.\pipe\verve-electron-{key}");
            let squatter =
                make_pipe(&wide(&name), &OwnerOnly::new().unwrap(), true).expect("a pipe");
            let instance = SingleInstance::request(&profile, &hub(), Vec::new(), "null".into());
            assert!(instance.is_some(), "the lock is held");
            assert!(
                instance
                    .as_ref()
                    .is_some_and(|i| i.done.load(Ordering::SeqCst)),
                "and nobody listens"
            );
            drop(instance);
            // SAFETY: the squatter's handle, closed once.
            unsafe { CloseHandle(squatter.0) };
            let _ = std::fs::remove_dir_all(&profile);
        }
    }
}

#[cfg(unix)]
mod imp {
    use std::fs::{DirBuilder, File, OpenOptions, TryLockError};
    use std::os::unix::fs::{DirBuilderExt, MetadataExt};
    use std::os::unix::net::{UnixListener, UnixStream};
    use std::path::PathBuf;

    use super::*;

    /// Where sockets are made when `$XDG_RUNTIME_DIR` is not there to hold them: short, because a
    /// socket's path may not pass 104 bytes on macOS, and shared, which is why the socket goes in a
    /// directory of its own.
    const SHARED: &str = "/tmp";

    /// What the first instance holds until it lets go: the lock file, and the socket and the
    /// folder it made for it, which are taken away with it.
    pub(super) struct Held {
        _lock: File,
        link: PathBuf,
        socket: Option<PathBuf>,
        folder: Option<PathBuf>,
    }

    impl Drop for Held {
        fn drop(&mut self) {
            // Each of these may be gone already (a failed bind makes no socket); nothing is
            // left to do about one that cannot be removed.
            let _ = std::fs::remove_file(&self.link);
            if let Some(socket) = &self.socket {
                let _ = std::fs::remove_file(socket);
            }
            if let Some(folder) = &self.folder {
                let _ = std::fs::remove_dir(folder);
            }
        }
    }

    /// Whether `dir` is a folder of `owner`'s that nobody else can enter or write in.
    fn is_private(dir: &Path, owner: u32) -> bool {
        std::fs::metadata(dir)
            .is_ok_and(|meta| meta.is_dir() && meta.uid() == owner && meta.mode() & 0o077 == 0)
    }

    /// A number nobody can guess, from the hasher's per-process random keys.
    fn unguessable() -> u64 {
        use std::hash::{BuildHasher, Hasher};
        std::collections::hash_map::RandomState::new()
            .build_hasher()
            .finish()
    }

    /// The folder for the socket: `$XDG_RUNTIME_DIR` when it is the user's own and private, else a
    /// folder of an unguessable name made here with mode 0700 — as Chromium does, so another user
    /// cannot take the name first and be handed a second instance's command line.
    fn socket_folder(key: &str, owner: u32) -> std::io::Result<(PathBuf, bool)> {
        if let Some(run) = std::env::var_os("XDG_RUNTIME_DIR").map(PathBuf::from)
            && is_private(&run, owner)
        {
            return Ok((run, false));
        }
        for _ in 0..16 {
            let folder = PathBuf::from(SHARED).join(format!("verve-{key}-{:016x}", unguessable()));
            match DirBuilder::new().mode(0o700).create(&folder) {
                Ok(()) => return Ok((folder, true)),
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
                Err(error) => return Err(error),
            }
        }
        Err(std::io::ErrorKind::AlreadyExists.into())
    }

    pub(super) fn request(
        user_data: &Path,
        key: &str,
        message: &str,
        hub: &Arc<Hub>,
    ) -> Option<SingleInstance> {
        let owner = std::fs::metadata(user_data).ok()?.uid();
        let link = user_data.join("SingletonSocket");
        let opened = OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(user_data.join("SingletonLock"));
        let lock = match opened {
            Ok(lock) => lock,
            Err(error) => return refused(&error.to_string()),
        };
        match lock.try_lock() {
            Ok(()) => {}
            Err(TryLockError::WouldBlock) => {
                tell_first(&link, owner, message);
                return None;
            }
            Err(TryLockError::Error(error)) => return refused(&error.to_string()),
        }
        let mut held = Held {
            _lock: lock,
            link,
            socket: None,
            folder: None,
        };
        // The lock is held. What fails from here leaves the app with the lock and nobody to hear
        // a second instance: a later one waits out its patience and ends.
        let (folder, made) = match socket_folder(key, owner) {
            Ok(folder) => folder,
            Err(error) => return Some(SingleInstance::unguarded(&error.to_string(), held)),
        };
        let socket = folder.join(if made {
            "s".into()
        } else {
            format!("verve-{key}.sock")
        });
        held.folder = made.then_some(folder);
        held.socket = Some(socket.clone());
        // A link a dead first instance left; this process holds the lock, so it is stale.
        let _ = std::fs::remove_file(&held.link);
        let listener = UnixListener::bind(&socket).and_then(|listener| {
            std::os::unix::fs::symlink(&socket, &held.link).map(|()| listener)
        });
        let listener = match listener {
            Ok(listener) => listener,
            Err(error) => return Some(SingleInstance::unguarded(&error.to_string(), held)),
        };
        let (stop, done) = (
            Arc::new(AtomicBool::new(false)),
            Arc::new(AtomicBool::new(false)),
        );
        let (flag, finished, weak) = (Arc::clone(&stop), Arc::clone(&done), Arc::downgrade(hub));
        let spawned = std::thread::Builder::new()
            .name("single-instance".into())
            .spawn(move || {
                let _finished = Finished(finished);
                for stream in listener.incoming() {
                    if flag.load(Ordering::SeqCst) {
                        return;
                    }
                    if let (Ok(stream), Some(hub)) = (stream, weak.upgrade()) {
                        super::receive(stream, &hub);
                    }
                }
            });
        if let Err(error) = spawned {
            return Some(SingleInstance::unguarded(&error.to_string(), held));
        }
        Some(SingleInstance {
            stop,
            done,
            wake: Box::new(move || {
                // A connection that fails means the listener is already gone.
                let _ = UnixStream::connect(&socket);
            }),
            _held: held,
        })
    }

    /// Electron answers `false` when the lock cannot be taken (its `LOCK_ERROR`); so does this,
    /// since an app that runs twice on a profile it cannot lock does harm.
    fn refused(why: &str) -> Option<SingleInstance> {
        eprintln!("electron: the single instance lock could not be taken ({why})");
        None
    }

    /// Connects to the first instance's socket — which it may still be making — and sends. The
    /// socket is where the profile's `SingletonSocket` link points, and only if the folder it is
    /// in is the user's own and private: a link into someone else's folder is not the first
    /// instance.
    fn tell_first(link: &Path, owner: u32, message: &str) {
        let deadline = Instant::now() + PATIENCE;
        while Instant::now() < deadline {
            if let Ok(socket) = std::fs::read_link(link) {
                if !socket.parent().is_some_and(|dir| is_private(dir, owner)) {
                    eprintln!("electron: the single instance socket is not in a private folder");
                    return;
                }
                if let Ok(stream) = UnixStream::connect(&socket) {
                    super::send(stream, message);
                    return;
                }
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_key_is_sixteen_hex_digits_of_the_profile_path() {
        let dir = std::env::temp_dir();
        let key = key_of(&dir);
        assert_eq!(key.len(), 16);
        assert!(key.bytes().all(|b| b.is_ascii_hexdigit()));
        assert_ne!(key, key_of(&dir.join("elsewhere")));
    }

    #[test]
    fn a_second_instance_reports_the_switches_before_the_rest() {
        let mut start = Start::development("app".into());
        start.argv = ["app", "--flag", "x", "--user-data-dir=u"]
            .map(String::from)
            .to_vec();
        let argv = second_instance_argv(&start);
        assert_eq!(argv[1..], ["--flag", "--user-data-dir=u", "app", "x"]);
    }

    #[test]
    fn the_message_is_one_line_of_json_with_the_data_inside() {
        let message = encode(&["a".into()], r#"{"k":[1,null]}"#);
        assert!(message.ends_with('\n') && message.matches('\n').count() == 1);
        let parsed: Value = serde_json::from_str(&message).unwrap();
        assert_eq!(parsed["argv"], json!(["a"]));
        assert_eq!(parsed["additionalData"], json!({"k": [1, null]}));
    }
}

//! Starting a program on Windows with `CreateProcessW`, the way libuv's `uv_spawn` does, which
//! is what Node's `child_process` is on top of. `std::process` is not used for two reasons: a
//! child's extra descriptors (the IPC channel of `fork`) travel in `lpReserved2`, the table the
//! C runtime reads them from, which a real Node child finds `NODE_CHANNEL_FD` in; and the
//! command line is built by libuv's quoting rules, not `std`'s.
//!
//! The parent's end of a pipe is an overlapped named pipe the reactor polls, and the child's a
//! plain handle to the same pipe, as libuv makes them (`uv__create_stdio_pipe_pair`).

use std::ffi::{OsStr, OsString, c_void};
use std::fs::File;
use std::io::{self, Read, Write};
use std::os::windows::ffi::OsStrExt;
use std::os::windows::io::{FromRawHandle, RawHandle};
use std::path::{Path, PathBuf};
use std::ptr::{null, null_mut};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

use mio::windows::NamedPipe;
use mio::{Interest, Registry, Token, event::Source};
use windows_sys::Win32::Foundation::{
    CloseHandle, DUPLICATE_SAME_ACCESS, DuplicateHandle, GENERIC_READ, GENERIC_WRITE, HANDLE,
    HANDLE_FLAG_INHERIT, INVALID_HANDLE_VALUE, SetHandleInformation, WAIT_OBJECT_0,
};
use windows_sys::Win32::Security::SECURITY_ATTRIBUTES;
use windows_sys::Win32::Storage::FileSystem::{
    CreateFileW, FILE_ATTRIBUTE_NORMAL, FILE_FLAG_FIRST_PIPE_INSTANCE, FILE_FLAG_OVERLAPPED,
    FILE_SHARE_READ, FILE_SHARE_WRITE, FILE_TYPE_CHAR, FILE_TYPE_PIPE, GetFileType, OPEN_EXISTING,
    PIPE_ACCESS_DUPLEX, PIPE_ACCESS_INBOUND, PIPE_ACCESS_OUTBOUND,
};
use windows_sys::Win32::System::Console::{
    GetStdHandle, STD_ERROR_HANDLE, STD_INPUT_HANDLE, STD_OUTPUT_HANDLE,
};
use windows_sys::Win32::System::JobObjects::{
    AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
    JOB_OBJECT_LIMIT_SILENT_BREAKAWAY_OK, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
    JobObjectExtendedLimitInformation, SetInformationJobObject,
};
use windows_sys::Win32::System::Pipes::{
    CreateNamedPipeW, CreatePipe, PIPE_READMODE_BYTE, PIPE_REJECT_REMOTE_CLIENTS, PIPE_TYPE_BYTE,
    PIPE_WAIT,
};
use windows_sys::Win32::System::Threading::{
    CREATE_NEW_PROCESS_GROUP, CREATE_NO_WINDOW, CREATE_SUSPENDED, CREATE_UNICODE_ENVIRONMENT,
    CreateProcessW, DETACHED_PROCESS, DeleteProcThreadAttributeList, EXTENDED_STARTUPINFO_PRESENT,
    GetCurrentProcess, GetExitCodeProcess, GetStartupInfoW, InitializeProcThreadAttributeList,
    LPPROC_THREAD_ATTRIBUTE_LIST, OpenProcess, PROC_THREAD_ATTRIBUTE_HANDLE_LIST,
    PROCESS_INFORMATION, PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_SYNCHRONIZE, PROCESS_TERMINATE,
    RegisterWaitForSingleObject, ResumeThread, STARTF_USESHOWWINDOW, STARTF_USESTDHANDLES,
    STARTUPINFOEXW, STARTUPINFOW, TerminateProcess, UnregisterWaitEx, UpdateProcThreadAttribute,
    WT_EXECUTEONLYONCE, WaitForSingleObject,
};

use super::launch::{Ends, Exit, Flow, Mode, Parent, Started};
use super::spec::{SpawnSpec, Stdio};
use crate::network::reactor::{Command, Sender};

/// The blocking end of a pipe, for the synchronous calls.
pub(crate) type Blocking = File;

/// `FOPEN | FPIPE`, `FOPEN | FDEV`, `FOPEN`: the flags the C runtime keeps for a descriptor.
const FOPEN: u8 = 0x01;
const FPIPE: u8 = 0x08;
const FDEV: u8 = 0x40;

/// Handles are inherited by every process started while they are inheritable, so one starts at
/// a time.
static SPAWNING: Mutex<()> = Mutex::new(());

/// A handle that is closed when it goes.
struct Owned(HANDLE);

// SAFETY: a handle is a number the kernel looks up; any thread may use or close it.
unsafe impl Send for Owned {}

impl Drop for Owned {
    fn drop(&mut self) {
        if !self.0.is_null() && self.0 != INVALID_HANDLE_VALUE {
            // SAFETY: the handle is owned here and closed once.
            unsafe { CloseHandle(self.0) };
        }
    }
}

/// The parent's end of a pipe, for the reactor to poll.
pub(crate) struct Io(NamedPipe);

impl Io {
    /// A pipe the child was started with, found at descriptor `fd` of the table it inherited.
    pub(super) fn inherited(fd: i32) -> Option<Self> {
        let handle = inherited_handle(fd)?;
        // SAFETY: the handle was handed down for this use and nothing else owns it; the parent
        // opened it overlapped, as a named pipe the reactor can poll must be.
        Some(Self(unsafe {
            NamedPipe::from_raw_handle(handle as RawHandle)
        }))
    }

    /// Windows cannot close one direction of a pipe: the caller drops it instead.
    pub(super) fn shutdown_write(&mut self) -> bool {
        false
    }
}

impl Read for Io {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        self.0.read(buffer)
    }
}

impl Write for Io {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        self.0.write(buffer)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.0.flush()
    }
}

impl Source for Io {
    fn register(
        &mut self,
        registry: &Registry,
        token: Token,
        interests: Interest,
    ) -> io::Result<()> {
        self.0.register(registry, token, interests)
    }

    fn reregister(
        &mut self,
        registry: &Registry,
        token: Token,
        interests: Interest,
    ) -> io::Result<()> {
        self.0.reregister(registry, token, interests)
    }

    fn deregister(&mut self, registry: &Registry) -> io::Result<()> {
        self.0.deregister(registry)
    }
}

/// A started program.
pub(crate) struct Process {
    handle: Owned,
    pid: u32,
    /// Declared after `handle`, but dropped first: see `Drop for Process`.
    wait: Option<Wait>,
    /// The signal `kill` sent: Windows has none, so this is what `exit` reports.
    killed: Option<i32>,
}

impl Process {
    pub(super) fn pid(&self) -> u32 {
        self.pid
    }

    /// Tells the reactor, through `sender`, when the program ends.
    pub(super) fn watch(&mut self, sender: Sender) -> io::Result<()> {
        let context = Box::into_raw(Box::new(sender));
        let mut wait: HANDLE = null_mut();
        // SAFETY: the callback only reads through the pointer, and `Wait` frees the box after
        // cancelling the registration and waiting for a running callback; the process handle
        // outlives the registration (see `Drop for Process`).
        let registered = unsafe {
            RegisterWaitForSingleObject(
                &mut wait,
                self.handle.0,
                Some(exited),
                context.cast::<c_void>(),
                u32::MAX,
                WT_EXECUTEONLYONCE,
            )
        };
        if registered == 0 {
            // SAFETY: the callback will never run, so the box is still ours alone.
            drop(unsafe { Box::from_raw(context) });
            return Err(io::Error::last_os_error());
        }
        self.wait = Some(Wait {
            handle: wait,
            context,
        });
        Ok(())
    }

    /// How the program ended, if it has.
    pub(super) fn try_exit(&mut self) -> Option<Exit> {
        // SAFETY: the process handle is valid for as long as `self` is.
        if unsafe { WaitForSingleObject(self.handle.0, 0) } != WAIT_OBJECT_0 {
            return None;
        }
        let mut code = 0_u32;
        // SAFETY: as above; `code` is a place for the answer.
        unsafe { GetExitCodeProcess(self.handle.0, &mut code) };
        Some(match self.killed {
            Some(signal) => Exit {
                code: None,
                signal: Some(signal),
            },
            None => Exit {
                code: Some(i64::from(code)),
                signal: None,
            },
        })
    }

    /// Waits for the program to end, at most `timeout`.
    pub(super) fn wait(&mut self, timeout: Option<Duration>) -> Option<Exit> {
        let millis = timeout.map_or(u32::MAX, |t| {
            t.as_millis().min(u128::from(u32::MAX - 1)) as u32
        });
        // SAFETY: the process handle is valid for as long as `self` is.
        unsafe { WaitForSingleObject(self.handle.0, millis) };
        self.try_exit()
    }

    /// Ends the program, as libuv's `uv_process_kill` does: every signal but 0 terminates it.
    pub(super) fn kill(&mut self, signal: i32) -> io::Result<()> {
        terminate(self.handle.0, signal)?;
        if signal != 0 {
            self.killed = Some(signal);
        }
        Ok(())
    }
}

/// The wait callback: wakes the reactor that owns the process.
unsafe extern "system" fn exited(context: *mut c_void, _: bool) {
    // SAFETY: `watch` passed a live `Sender`, which `Wait` frees only after this has returned.
    let sender = unsafe { &*context.cast::<Sender>() };
    sender.send(Command::Reap);
}

/// A registered wait on a process, and the sender its callback uses.
struct Wait {
    handle: HANDLE,
    context: *mut Sender,
}

// SAFETY: a wait handle is a number the kernel looks up, and the sender is `Send`; the pointer
// is owned by this value alone.
unsafe impl Send for Wait {}

impl Drop for Wait {
    fn drop(&mut self) {
        // SAFETY: the handle came from `RegisterWaitForSingleObject`. `INVALID_HANDLE_VALUE`
        // makes the call wait for a callback that is running, so none uses the box afterwards,
        // which is then ours to free whether or not the callback ran.
        unsafe {
            UnregisterWaitEx(self.handle, INVALID_HANDLE_VALUE);
            drop(Box::from_raw(self.context));
        }
    }
}

impl Drop for Process {
    fn drop(&mut self) {
        // The wait must be gone before the handle it waits on is closed; fields would drop the
        // other way round.
        self.wait.take();
    }
}

/// The signals Windows has an answer to, as libuv's `uv_kill` has: 0 asks whether the process is
/// there; these four end it.
pub(crate) fn check_signal(signal: i32) -> io::Result<()> {
    match signal {
        0 | 2 | 3 | 9 | 15 => Ok(()),
        _ => Err(io::Error::from_raw_os_error(ERROR_CALL_NOT_IMPLEMENTED)),
    }
}

/// `signal` 0 asks whether the process is there; any other ends it.
fn terminate(process: HANDLE, signal: i32) -> io::Result<()> {
    check_signal(signal)?;
    // SAFETY: the caller's handle is valid.
    if unsafe { WaitForSingleObject(process, 0) } == WAIT_OBJECT_0 {
        return Err(io::Error::from_raw_os_error(ERROR_INVALID_PARAMETER));
    }
    if signal == 0 {
        return Ok(());
    }
    // SAFETY: as above.
    if unsafe { TerminateProcess(process, 1) } == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

/// `ERROR_INVALID_PARAMETER`: what opening a process that is gone says, and what a batch file
/// started without a shell is refused with.
const ERROR_INVALID_PARAMETER: i32 = 87;
/// `ERROR_CALL_NOT_IMPLEMENTED`: a signal Windows has no meaning for.
const ERROR_CALL_NOT_IMPLEMENTED: i32 = 120;

/// `process.kill(pid, signal)`.
pub(crate) fn kill_pid(pid: i64, signal: i32) -> io::Result<()> {
    let pid =
        u32::try_from(pid).map_err(|_| io::Error::from_raw_os_error(ERROR_INVALID_PARAMETER))?;
    // SAFETY: plain call; the handle is closed below.
    let handle = unsafe {
        OpenProcess(
            PROCESS_TERMINATE | PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_SYNCHRONIZE,
            0,
            pid,
        )
    };
    if handle.is_null() {
        return Err(io::Error::last_os_error());
    }
    let handle = Owned(handle);
    terminate(handle.0, signal)
}

/// Starts the program `spec` describes.
pub(super) fn spawn(spec: &SpawnSpec, mode: Mode) -> io::Result<Started> {
    if spec.uid.is_some() || spec.gid.is_some() {
        return Err(io::Error::from(io::ErrorKind::Unsupported));
    }
    let _one_at_a_time = SPAWNING.lock().unwrap_or_else(|e| e.into_inner());
    let program = find_program(spec)?;
    let mut command_line = command_line(spec);
    let environment = environment_block(spec);
    let cwd = wide(spec.cwd.as_os_str());
    let (children, parents) = stdiohandles(spec, mode)?;
    let table = reserved_table(&children);
    let mut handles: Vec<HANDLE> = children.iter().map(ChildEnd::handle).collect();
    handles.sort();
    handles.dedup();
    let inherited = HandleList::new(handles)?;
    // SAFETY: STARTUPINFOEXW is plain data; all-zero is a valid empty one.
    let mut extended: STARTUPINFOEXW = unsafe { std::mem::zeroed() };
    extended.lpAttributeList = inherited.as_ptr();
    let startup = &mut extended.StartupInfo;
    startup.cb = std::mem::size_of::<STARTUPINFOEXW>() as u32;
    startup.dwFlags = STARTF_USESTDHANDLES;
    startup.cbReserved2 = table.len() as u16;
    startup.lpReserved2 = table.as_ptr().cast_mut();
    for (slot, child) in [
        &mut startup.hStdInput,
        &mut startup.hStdOutput,
        &mut startup.hStdError,
    ]
    .into_iter()
    .zip(&children)
    {
        *slot = child.handle();
    }
    let mut flags = CREATE_UNICODE_ENVIRONMENT | EXTENDED_STARTUPINFO_PRESENT;
    if spec.detached {
        flags |= DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP;
    }
    if spec.windows_hide {
        // A console program that shares the parent's console must keep it, as in libuv.
        if !spec.stdio.iter().any(|s| matches!(s, Stdio::Inherit(_))) {
            flags |= CREATE_NO_WINDOW;
        }
        startup.dwFlags |= STARTF_USESHOWWINDOW;
        startup.wShowWindow = 0; // SW_HIDE
    }
    // A child that is not detached dies with us, as libuv's does: it is put in a job that kills
    // its processes when the last handle to it closes, which is when this process ends.
    let job = if spec.detached { None } else { global_job() };
    if job.is_some() {
        flags |= CREATE_SUSPENDED;
    }
    // SAFETY: PROCESS_INFORMATION is plain data.
    let mut info: PROCESS_INFORMATION = unsafe { std::mem::zeroed() };
    // SAFETY: every pointer is to a buffer alive for this call and nul-terminated where the
    // function expects it; the command line is a mutable copy, as the function may write to it.
    let created = unsafe {
        CreateProcessW(
            program.as_ptr(),
            command_line.as_mut_ptr(),
            null(),
            null(),
            1,
            flags,
            environment.as_ptr().cast(),
            cwd.as_ptr(),
            &extended.StartupInfo,
            &mut info,
        )
    };
    if created == 0 {
        return Err(io::Error::last_os_error());
    }
    if let Some(job) = job {
        // A process already in a job that forbids more is left as it is: it still runs.
        // SAFETY: both handles are valid; the thread is suspended and is resumed once.
        unsafe {
            AssignProcessToJobObject(job, info.hProcess);
            ResumeThread(info.hThread);
        }
    }
    drop(Owned(info.hThread));
    drop(children);
    Ok(Started {
        process: Process {
            handle: Owned(info.hProcess),
            pid: info.dwProcessId,
            wait: None,
            killed: None,
        },
        pipes: parents,
    })
}

/// The handles a child may inherit, and no others: without this the child gets every
/// inheritable handle of ours, including the pipes other children are being given, and holds
/// them open after we are gone.
struct HandleList {
    list: Vec<u64>,
    /// Kept alive: the attribute list points at it.
    handles: Vec<HANDLE>,
}

impl HandleList {
    fn new(handles: Vec<HANDLE>) -> io::Result<Self> {
        let mut size = 0;
        // SAFETY: asking for the size needed returns an error and fills `size`.
        unsafe { InitializeProcThreadAttributeList(null_mut(), 1, 0, &mut size) };
        let mut list = vec![0_u64; size.div_ceil(8)];
        let pointer = list.as_mut_ptr().cast::<c_void>();
        // SAFETY: `list` is `size` bytes, aligned for the structure.
        if unsafe { InitializeProcThreadAttributeList(pointer, 1, 0, &mut size) } == 0 {
            return Err(io::Error::last_os_error());
        }
        let this = Self { list, handles };
        // SAFETY: the list is initialised; the handle array lives as long as `this`.
        let set = unsafe {
            UpdateProcThreadAttribute(
                this.list.as_ptr().cast_mut().cast(),
                0,
                PROC_THREAD_ATTRIBUTE_HANDLE_LIST as usize,
                this.handles.as_ptr().cast(),
                this.handles.len() * std::mem::size_of::<HANDLE>(),
                null_mut(),
                null(),
            )
        };
        if set == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(this)
    }

    fn as_ptr(&self) -> LPPROC_THREAD_ATTRIBUTE_LIST {
        self.list.as_ptr().cast_mut().cast()
    }
}

impl Drop for HandleList {
    fn drop(&mut self) {
        // SAFETY: the list was initialised in `new`.
        unsafe { DeleteProcThreadAttributeList(self.as_ptr()) };
    }
}

/// The job every child that is not detached belongs to, made the first time one starts.
fn global_job() -> Option<HANDLE> {
    static JOB: OnceLock<usize> = OnceLock::new();
    let raw = *JOB.get_or_init(|| {
        // SAFETY: plain calls; the structure is plain data and the handle is kept for good.
        unsafe {
            let job = CreateJobObjectW(null(), null());
            if job.is_null() {
                return 0;
            }
            let mut limits: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = std::mem::zeroed();
            limits.BasicLimitInformation.LimitFlags =
                JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE | JOB_OBJECT_LIMIT_SILENT_BREAKAWAY_OK;
            let set = SetInformationJobObject(
                job,
                JobObjectExtendedLimitInformation,
                std::ptr::addr_of!(limits).cast(),
                std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
            );
            if set == 0 {
                CloseHandle(job);
                return 0;
            }
            job as usize
        }
    });
    (raw != 0).then_some(raw as HANDLE)
}

// ─── The program and its command line ────────────────────────────────────────────────────────

fn wide(text: &OsStr) -> Vec<u16> {
    text.encode_wide().chain(Some(0)).collect()
}

/// The program's path, nul-terminated, searched for as libuv does; `.bat` and `.cmd` files
/// are not started without a shell, which is Node's answer to CVE-2024-27980.
fn find_program(spec: &SpawnSpec) -> io::Result<Vec<u16>> {
    // The name as asked for is checked as well as the file found: Windows ignores trailing dots
    // and spaces, so `x.bat ` is `x.bat` to it and not to a check of the found path alone.
    if is_batch_file(&spec.file) {
        return Err(io::Error::from_raw_os_error(ERROR_INVALID_PARAMETER));
    }
    let search_path = spec
        .env
        .iter()
        .find(|(name, _)| name.to_string_lossy().eq_ignore_ascii_case("PATH"))
        .map(|(_, value)| value.to_string_lossy().into_owned())
        .or_else(|| std::env::var("PATH").ok())
        .unwrap_or_default();
    let found = search(&spec.file, &spec.cwd, &search_path, &|p| p.is_file())
        .ok_or_else(|| io::Error::from_raw_os_error(2))?;
    if is_batch_file(&found.to_string_lossy()) {
        return Err(io::Error::from_raw_os_error(ERROR_INVALID_PARAMETER));
    }
    Ok(wide(found.as_os_str()))
}

/// Whether `name` ends in `.bat` or `.cmd`, ignoring the trailing dots and spaces Windows
/// ignores: Node's `IsWindowsBatchFile`, which closed CVE-2024-36138.
pub(super) fn is_batch_file(name: &str) -> bool {
    let trimmed = name.trim_end_matches(['.', ' ', '\t']);
    trimmed.rsplit_once('.').is_some_and(|(_, extension)| {
        extension.eq_ignore_ascii_case("bat") || extension.eq_ignore_ascii_case("cmd")
    })
}

/// Where `file` is: against `cwd` when it names a folder, else in `cwd` and then each folder of
/// `path` (`;`-separated), trying the name as it is when it has an extension, and with `.com`
/// then `.exe` added. `PATHEXT` is not consulted, as libuv does not.
pub(super) fn search(
    file: &str,
    cwd: &Path,
    path: &str,
    is_file: &dyn Fn(&Path) -> bool,
) -> Option<PathBuf> {
    let names_a_folder = file.contains(['\\', '/']) || file.as_bytes().get(1) == Some(&b':');
    let in_folder = |folder: &Path| -> Option<PathBuf> {
        if let Some(exact) = file.strip_suffix('.') {
            let candidate = folder.join(exact);
            return is_file(&candidate).then_some(candidate);
        }
        let base = folder.join(file);
        if Path::new(file).extension().is_some() && is_file(&base) {
            return Some(base);
        }
        ["com", "exe"].iter().find_map(|extension| {
            let mut name = base.clone().into_os_string();
            name.push(".");
            name.push(extension);
            let candidate = PathBuf::from(name);
            is_file(&candidate).then_some(candidate)
        })
    };
    if names_a_folder {
        return in_folder(cwd);
    }
    in_folder(cwd).or_else(|| {
        path.split(';')
            .map(|folder| folder.trim_matches('"'))
            .filter(|folder| !folder.is_empty())
            .find_map(|folder| in_folder(&cwd.join(folder)))
    })
}

/// `argv` as one command line, each argument quoted by libuv's rules unless the arguments are
/// to go verbatim. Nul-terminated.
fn command_line(spec: &SpawnSpec) -> Vec<u16> {
    let mut line: Vec<u16> = Vec::new();
    for (at, arg) in spec.args.iter().enumerate() {
        if at > 0 {
            line.push(u16::from(b' '));
        }
        let arg: Vec<u16> = arg.encode_utf16().collect();
        if spec.windows_verbatim_arguments {
            line.extend(arg);
        } else {
            line.extend(quote(&arg));
        }
    }
    line.push(0);
    line
}

/// libuv's `quote_cmd_arg`: an argument as `CommandLineToArgvW` reads back to itself.
pub(super) fn quote(arg: &[u16]) -> Vec<u16> {
    let is = |c: u16, ch: char| c == ch as u16;
    if arg.is_empty() {
        return vec![b'"'.into(), b'"'.into()];
    }
    if !arg.iter().any(|&c| is(c, ' ') || is(c, '\t') || is(c, '"')) {
        return arg.to_vec();
    }
    if !arg.iter().any(|&c| is(c, '"') || is(c, '\\')) {
        let mut quoted = vec![u16::from(b'"')];
        quoted.extend_from_slice(arg);
        quoted.push(u16::from(b'"'));
        return quoted;
    }
    // Written backwards, so a run of backslashes before a quote can be doubled as it is met.
    let mut reversed = vec![u16::from(b'"')];
    let mut quote_hit = true;
    for &c in arg.iter().rev() {
        reversed.push(c);
        if quote_hit && is(c, '\\') {
            reversed.push(u16::from(b'\\'));
        } else if is(c, '"') {
            quote_hit = true;
            reversed.push(u16::from(b'\\'));
        } else {
            quote_hit = false;
        }
    }
    reversed.push(u16::from(b'"'));
    reversed.reverse();
    reversed
}

/// The names libuv fills in from the parent's environment when the child's has not got them.
const REQUIRED: [&str; 11] = [
    "HOMEDRIVE",
    "HOMEPATH",
    "LOGONSERVER",
    "PATH",
    "SYSTEMDRIVE",
    "SYSTEMROOT",
    "TEMP",
    "USERDOMAIN",
    "USERNAME",
    "USERPROFILE",
    "WINDIR",
];

/// The environment block: `NAME=value` entries, sorted case-insensitively, then a second nul.
fn environment_block(spec: &SpawnSpec) -> Vec<u16> {
    let mut pairs: Vec<(OsString, OsString)> = spec.env.clone();
    for name in REQUIRED {
        let present = pairs
            .iter()
            .any(|(k, _)| k.to_string_lossy().eq_ignore_ascii_case(name));
        if let (false, Some(value)) = (present, std::env::var_os(name)) {
            pairs.push((name.into(), value));
        }
    }
    pairs.sort_by_key(|(name, _)| name.to_string_lossy().to_uppercase());
    let mut block: Vec<u16> = Vec::new();
    for (name, value) in pairs {
        block.extend(name.encode_wide());
        block.push(u16::from(b'='));
        block.extend(value.encode_wide());
        block.push(0);
    }
    if block.is_empty() {
        block.push(0);
    }
    block.push(0);
    block
}

// ─── Descriptors ─────────────────────────────────────────────────────────────────────────────

/// What the child holds on one descriptor: a handle and the C runtime's flags for it.
struct ChildEnd {
    handle: Owned,
    flags: u8,
}

impl ChildEnd {
    fn handle(&self) -> HANDLE {
        self.handle.0
    }
}

fn inheritable() -> SECURITY_ATTRIBUTES {
    SECURITY_ATTRIBUTES {
        nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
        lpSecurityDescriptor: null_mut(),
        bInheritHandle: 1,
    }
}

/// The child's descriptors and the parent's ends of the pipes among them.
fn stdiohandles(spec: &SpawnSpec, mode: Mode) -> io::Result<(Vec<ChildEnd>, Ends)> {
    let mut children = Vec::new();
    let mut parents = Vec::new();
    for (index, entry) in spec.stdio.iter().enumerate() {
        let child = match entry {
            Stdio::Ignore => null_device()?,
            Stdio::Inherit(fd) => match inherited_std(*fd) {
                Some(end) => end?,
                None => null_device()?,
            },
            Stdio::Pipe | Stdio::Ipc => {
                let flow = Flow::of(*entry, index);
                let (parent, child) = match mode {
                    Mode::Reactor => named_pipe(flow, index > 2)?,
                    Mode::Blocking => anonymous_pipe(flow)?,
                };
                parents.push((index, parent));
                child
            }
        };
        children.push(child);
    }
    while children.len() < 3 {
        children.push(null_device()?);
    }
    Ok((children, parents))
}

fn null_device() -> io::Result<ChildEnd> {
    let name = wide(OsStr::new("NUL"));
    let attributes = inheritable();
    // SAFETY: the name is nul-terminated and the attributes outlive the call.
    let handle = unsafe {
        CreateFileW(
            name.as_ptr(),
            GENERIC_READ | GENERIC_WRITE,
            FILE_SHARE_READ | FILE_SHARE_WRITE,
            &attributes,
            OPEN_EXISTING,
            FILE_ATTRIBUTE_NORMAL,
            null_mut(),
        )
    };
    if handle == INVALID_HANDLE_VALUE {
        return Err(io::Error::last_os_error());
    }
    Ok(ChildEnd {
        handle: Owned(handle),
        flags: FOPEN | FDEV,
    })
}

/// The parent's own standard handle `fd`, duplicated so the child may inherit it; `None` when
/// the parent has none.
fn inherited_std(fd: i32) -> Option<io::Result<ChildEnd>> {
    let which = [STD_INPUT_HANDLE, STD_OUTPUT_HANDLE, STD_ERROR_HANDLE][fd as usize];
    // SAFETY: plain call.
    let own = unsafe { GetStdHandle(which) };
    if own.is_null() || own == INVALID_HANDLE_VALUE {
        return None;
    }
    let mut copy: HANDLE = null_mut();
    // SAFETY: both process handles are the current process's pseudo-handle; `copy` receives
    // a new handle, which is inheritable.
    let duplicated = unsafe {
        DuplicateHandle(
            GetCurrentProcess(),
            own,
            GetCurrentProcess(),
            &mut copy,
            0,
            1,
            DUPLICATE_SAME_ACCESS,
        )
    };
    if duplicated == 0 {
        return Some(Err(io::Error::last_os_error()));
    }
    // SAFETY: plain call on a valid handle.
    let flags = match unsafe { GetFileType(own) } {
        FILE_TYPE_CHAR => FOPEN | FDEV,
        FILE_TYPE_PIPE => FOPEN | FPIPE,
        _ => FOPEN,
    };
    Some(Ok(ChildEnd {
        handle: Owned(copy),
        flags,
    }))
}

static PIPES: AtomicU64 = AtomicU64::new(0);

/// An overlapped named pipe: the server end for the parent to poll, the client end for the
/// child. The child's end is overlapped too when the child will poll it itself — the IPC
/// channel — and plain, as a program expects its standard handles, when it will not.
fn named_pipe(flow: Flow, child_polls: bool) -> io::Result<(Parent, ChildEnd)> {
    let unique = PIPES.fetch_add(1, Ordering::Relaxed);
    let clock = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos());
    let name = wide(OsStr::new(&format!(
        r"\\.\pipe\verve-{}-{unique}-{clock:x}",
        std::process::id()
    )));
    let (open, access) = match flow {
        Flow::ToChild => (PIPE_ACCESS_OUTBOUND, GENERIC_READ),
        Flow::FromChild => (PIPE_ACCESS_INBOUND, GENERIC_WRITE),
        Flow::Both => (PIPE_ACCESS_DUPLEX, GENERIC_READ | GENERIC_WRITE),
    };
    // SAFETY: the name is nul-terminated; no attributes, so the server end is not inheritable.
    let server = unsafe {
        CreateNamedPipeW(
            name.as_ptr(),
            open | FILE_FLAG_OVERLAPPED | FILE_FLAG_FIRST_PIPE_INSTANCE,
            PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT | PIPE_REJECT_REMOTE_CLIENTS,
            1,
            65536,
            65536,
            0,
            null(),
        )
    };
    if server == INVALID_HANDLE_VALUE {
        return Err(io::Error::last_os_error());
    }
    let server = Owned(server);
    let attributes = inheritable();
    let overlapped = if child_polls { FILE_FLAG_OVERLAPPED } else { 0 };
    // SAFETY: as above; the attributes make this end inheritable.
    let client = unsafe {
        CreateFileW(
            name.as_ptr(),
            access,
            0,
            &attributes,
            OPEN_EXISTING,
            overlapped,
            null_mut(),
        )
    };
    if client == INVALID_HANDLE_VALUE {
        return Err(io::Error::last_os_error());
    }
    let handle = server.0;
    std::mem::forget(server);
    // SAFETY: the handle is an overlapped pipe, which nothing else owns now.
    let parent = unsafe { NamedPipe::from_raw_handle(handle as RawHandle) };
    Ok((
        Parent::Reactor(Io(parent)),
        ChildEnd {
            handle: Owned(client),
            flags: FOPEN | FPIPE,
        },
    ))
}

/// A pair from `CreatePipe`, for the synchronous calls, which read with plain blocking calls.
fn anonymous_pipe(flow: Flow) -> io::Result<(Parent, ChildEnd)> {
    if flow == Flow::Both {
        return Err(io::Error::from(io::ErrorKind::Unsupported));
    }
    let attributes = inheritable();
    let (mut read, mut write): (HANDLE, HANDLE) = (null_mut(), null_mut());
    // SAFETY: both places are valid for the handles; the attributes outlive the call.
    if unsafe { CreatePipe(&mut read, &mut write, &attributes, 0) } == 0 {
        return Err(io::Error::last_os_error());
    }
    let (read, write) = (Owned(read), Owned(write));
    let (parent, child) = if flow == Flow::ToChild {
        (write, read)
    } else {
        (read, write)
    };
    // SAFETY: the parent's end must not be inherited by the child.
    if unsafe { SetHandleInformation(parent.0, HANDLE_FLAG_INHERIT, 0) } == 0 {
        return Err(io::Error::last_os_error());
    }
    let raw = parent.0;
    std::mem::forget(parent);
    // SAFETY: the handle is a pipe end that nothing else owns now.
    let file = unsafe { File::from_raw_handle(raw as RawHandle) };
    Ok((
        Parent::Blocking(file),
        ChildEnd {
            handle: child,
            flags: FOPEN | FPIPE,
        },
    ))
}

/// The C runtime's descriptor table for `lpReserved2`: a count, a flag byte for each descriptor,
/// and a handle for each.
fn reserved_table(children: &[ChildEnd]) -> Vec<u8> {
    let count = children.len();
    let mut table = Vec::with_capacity(4 + count * 9);
    table.extend_from_slice(&(count as u32).to_le_bytes());
    table.extend(children.iter().map(|c| c.flags));
    for child in children {
        table.extend_from_slice(&(child.handle() as usize).to_le_bytes());
    }
    table
}

/// The handle at descriptor `fd` of the table this process was started with.
fn inherited_handle(fd: i32) -> Option<HANDLE> {
    // SAFETY: STARTUPINFOW is plain data; the call fills it.
    let mut startup: STARTUPINFOW = unsafe { std::mem::zeroed() };
    startup.cb = std::mem::size_of::<STARTUPINFOW>() as u32;
    // SAFETY: as above.
    unsafe { GetStartupInfoW(&mut startup) };
    if startup.lpReserved2.is_null() || usize::from(startup.cbReserved2) < 4 {
        return None;
    }
    // SAFETY: the system gives `cbReserved2` bytes at `lpReserved2` for the life of the process.
    let table = unsafe {
        std::slice::from_raw_parts(startup.lpReserved2, usize::from(startup.cbReserved2))
    };
    let count = u32::from_le_bytes(table[..4].try_into().ok()?) as usize;
    let at = usize::try_from(fd).ok().filter(|&at| at < count)?;
    let flags = *table.get(4 + at)?;
    let start = 4 + count + at * 8;
    let raw = u64::from_le_bytes(table.get(start..start + 8)?.try_into().ok()?);
    (flags & FOPEN != 0 && raw != u64::MAX).then_some(raw as usize as HANDLE)
}

/// Windows' error for a failed start, as the name Node reports and libuv's negative number.
pub(crate) fn classify(error: &io::Error) -> (&'static str, i64) {
    if error.kind() == io::ErrorKind::Unsupported {
        return ("ENOTSUP", -4049);
    }
    match error.raw_os_error() {
        Some(2 | 3 | 267) => ("ENOENT", -4058),
        Some(5) => ("EPERM", -4048),
        Some(87) => ("EINVAL", -4071),
        Some(120) => ("ENOSYS", -4054),
        _ => ("UNKNOWN", -4094),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn quoted(text: &str) -> String {
        let units: Vec<u16> = text.encode_utf16().collect();
        String::from_utf16(&quote(&units)).unwrap()
    }

    #[test]
    fn arguments_are_quoted_as_libuv_quotes_them() {
        assert_eq!(quoted(""), "\"\"");
        assert_eq!(quoted("plain"), "plain");
        assert_eq!(quoted("two words"), "\"two words\"");
        assert_eq!(quoted("say \"hi\""), "\"say \\\"hi\\\"\"");
        assert_eq!(quoted("back\\slash"), "back\\slash");
        assert_eq!(quoted("trail \\"), "\"trail \\\\\"");
        assert_eq!(quoted("a^&|b"), "a^&|b");
    }

    #[test]
    fn a_program_is_looked_for_in_the_folder_then_the_path_with_com_and_exe() {
        let files = [
            r"C:\w\tool.exe",
            r"C:\bin\other.com",
            r"C:\bin\both.exe",
            r"C:\bin\both.com",
        ];
        let has = |p: &Path| files.iter().any(|f| Path::new(f) == p);
        let find = |file: &str| search(file, Path::new(r"C:\w"), r"C:\bin;C:\other", &has);
        assert_eq!(find("tool"), Some(PathBuf::from(r"C:\w\tool.exe")));
        assert_eq!(find("other"), Some(PathBuf::from(r"C:\bin\other.com")));
        assert_eq!(find("both"), Some(PathBuf::from(r"C:\bin\both.com")));
        assert_eq!(find("tool.exe"), Some(PathBuf::from(r"C:\w\tool.exe")));
        assert_eq!(find("missing"), None);
        assert_eq!(
            find(r"C:\bin\other"),
            Some(PathBuf::from(r"C:\bin\other.com"))
        );
    }

    #[test]
    fn batch_files_are_known_by_the_name_asked_for_trailing_dots_and_spaces_too() {
        for name in [
            "a.bat",
            "A.CMD",
            "x.bat ",
            "x.bat. .",
            "dir\\y.cmd.",
            "z.bat\t",
        ] {
            assert!(is_batch_file(name), "{name:?}");
        }
        for name in ["a.exe", "bat", "x.bat.exe", "cmd", "x.batx", ""] {
            assert!(!is_batch_file(name), "{name:?}");
        }
    }

    #[test]
    fn the_environment_block_is_sorted_and_ends_with_two_nuls() {
        let spec = SpawnSpec {
            file: "x".into(),
            args: vec!["x".into()],
            cwd: PathBuf::from("."),
            env: vec![("b".into(), "2".into()), ("A".into(), "1".into())],
            stdio: vec![],
            detached: false,
            uid: None,
            gid: None,
            windows_hide: false,
            windows_verbatim_arguments: false,
        };
        let block = String::from_utf16_lossy(&environment_block(&spec));
        assert!(block.starts_with("A=1\0b=2\0"));
        assert!(block.ends_with("\0\0"));
    }
}

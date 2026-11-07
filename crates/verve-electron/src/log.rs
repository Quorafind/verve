//! What Verve says on the terminal: its own diagnostics (`VERVE_ELECTRON_LOG`), the console of a
//! page when it is asked for, and the text `openDevTools()` dumps.
//!
//! Electron writes a page's console to the terminal only when logging is on
//! (`ELECTRON_ENABLE_LOGGING`, `--enable-logging`; Electron 39.8.10 writes nothing otherwise), so
//! that is the rule here, and `VERVE_ELECTRON_LOG=console` and an open DevTools are the other two
//! ways to ask. The diagnostics are `RUST_LOG`-like: `VERVE_ELECTRON_LOG=ipc,load=debug` names
//! categories and, after `=`, the level of each (`error`, `warn`, `info` by default, `debug`,
//! `trace`); `all` names every category. A category that is off costs one atomic read, which is
//! why this is a table and a macro and not a logging crate.

use std::fmt;
use std::fs::File;
use std::io::{IsTerminal, Write};
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

use verve::Level;

use crate::ids::ContentsId;

/// What a diagnostic line is about.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Category {
    /// The console of a page, written to the terminal.
    Console,
    /// Every message of `ipcMain` and `ipcRenderer`.
    Ipc,
    /// Windows and pages: what they report of their life.
    Lifecycle,
    /// Addresses a page loads.
    Load,
    /// What the main process asks of the shell's windows.
    Shell,
    /// What a person did to a window.
    Input,
    /// Timings.
    Perf,
    /// The packed app.
    Asar,
    /// Custom schemes and `protocol.handle`.
    Protocol,
}

impl Category {
    const ALL: [Category; 9] = [
        Category::Console,
        Category::Ipc,
        Category::Lifecycle,
        Category::Load,
        Category::Shell,
        Category::Input,
        Category::Perf,
        Category::Asar,
        Category::Protocol,
    ];

    /// The name `VERVE_ELECTRON_LOG` and the lines use.
    pub fn name(self) -> &'static str {
        match self {
            Category::Console => "console",
            Category::Ipc => "ipc",
            Category::Lifecycle => "lifecycle",
            Category::Load => "load",
            Category::Shell => "shell",
            Category::Input => "input",
            Category::Perf => "perf",
            Category::Asar => "asar",
            Category::Protocol => "protocol",
        }
    }
}

/// How much a diagnostic line matters; a category set to a level shows that level and the ones
/// above it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Severity {
    Error = 1,
    Warn,
    Info,
    Debug,
    Trace,
}

impl Severity {
    fn named(name: &str) -> Option<Self> {
        Some(match name {
            "error" => Self::Error,
            "warn" => Self::Warn,
            "info" => Self::Info,
            "debug" => Self::Debug,
            "trace" => Self::Trace,
            _ => return None,
        })
    }
}

/// The levels `VERVE_ELECTRON_LOG` set, a category at a time; `0` is off.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Filter([u8; Category::ALL.len()]);

impl Filter {
    /// Reads `ipc,load=debug`: names without a level are `info`; later entries win; a name or a
    /// level that means nothing is ignored.
    pub fn parse(text: &str) -> Self {
        let mut filter = Self::default();
        for entry in text.split(',').map(str::trim).filter(|e| !e.is_empty()) {
            let (name, level) = entry.split_once('=').unwrap_or((entry, "info"));
            let Some(level) = Severity::named(level.trim()) else {
                continue;
            };
            match name.trim() {
                "all" => filter.0 = [level as u8; Category::ALL.len()],
                name => {
                    if let Some(at) = Category::ALL.iter().position(|c| c.name() == name) {
                        filter.0[at] = level as u8;
                    }
                }
            }
        }
        filter
    }
}

/// Where the lines go.
enum Sink {
    Stderr,
    /// `VERVE_ELECTRON_LOG_FILE`: the diagnostics are appended here; the console stays on stderr.
    File(Mutex<File>),
    /// A test reads what was written.
    Memory(Mutex<Vec<u8>>),
}

/// The terminal's output of one app.
pub struct Log {
    levels: [AtomicU8; Category::ALL.len()],
    sink: Sink,
    colors: bool,
    /// Whether every page's console goes to the terminal.
    console: AtomicBool,
}

impl Log {
    fn with(filter: Filter, sink: Sink, colors: bool) -> Self {
        Self {
            levels: filter.0.map(AtomicU8::new),
            console: AtomicBool::new(filter.0[Category::Console as usize] != 0),
            sink,
            colors,
        }
    }

    /// A log that keeps what it writes, for [`Log::written`]: how a test reads the terminal.
    pub fn to_memory(filter: Filter) -> Self {
        Self::with(filter, Sink::Memory(Mutex::default()), false)
    }

    /// The log this process's environment asks for: `VERVE_ELECTRON_LOG`, `VERVE_ELECTRON_LOG_FILE`,
    /// `ELECTRON_ENABLE_LOGGING`, and for the colours `NO_COLOR`, `FORCE_COLOR` and whether
    /// standard error is a terminal.
    pub fn from_environment() -> Self {
        let env = |name: &str| std::env::var(name).ok();
        let filter = Filter::parse(&env("VERVE_ELECTRON_LOG").unwrap_or_default());
        let sink = env("VERVE_ELECTRON_LOG_FILE")
            .and_then(|path| {
                let open = File::options().create(true).append(true).open(path);
                open.ok().map(Mutex::new)
            })
            .map_or(Sink::Stderr, Sink::File);
        let colors = wants_color(
            env("FORCE_COLOR").as_deref(),
            env("NO_COLOR").as_deref(),
            std::io::stderr().is_terminal(),
        );
        let log = Self::with(filter, sink, colors);
        if enabled_by(env("ELECTRON_ENABLE_LOGGING").as_deref()) {
            log.set_console(true);
        }
        log
    }

    /// Whether a line of `category` at `severity` is written: one atomic read.
    pub fn enabled(&self, category: Category, severity: Severity) -> bool {
        severity as u8 <= self.levels[category as usize].load(Ordering::Relaxed)
    }

    /// Writes a diagnostic line; callers ask [`Log::enabled`] first (see [`diag!`]).
    pub fn write(&self, category: Category, severity: Severity, line: fmt::Arguments) {
        let level = match severity {
            Severity::Error => ":error",
            Severity::Warn => ":warn",
            _ => "",
        };
        let text = format!("[verve:{}{level}] {line}", category.name());
        match &self.sink {
            Sink::File(file) => {
                let mut file = file.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
                // A diagnostic that cannot be written is not worth stopping the app for.
                let _ = writeln!(file, "{text}");
            }
            _ => self.terminal(&text),
        }
    }

    /// Writes `text` and a newline on the terminal: standard error, or what a test reads.
    pub fn terminal(&self, text: &str) {
        match &self.sink {
            Sink::Memory(kept) => {
                let mut kept = kept.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
                kept.extend_from_slice(text.as_bytes());
                kept.push(b'\n');
            }
            _ => {
                // Standard error that is closed has nobody to tell.
                let _ = writeln!(std::io::stderr().lock(), "{text}");
            }
        }
    }

    /// What a [`Log::to_memory`] has written so far.
    pub fn written(&self) -> String {
        match &self.sink {
            Sink::Memory(kept) => {
                let kept = kept.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
                String::from_utf8_lossy(&kept).into_owned()
            }
            _ => String::new(),
        }
    }

    /// Whether anything is written to the terminal on its own: a category is on, or logging is.
    pub fn writes_anything(&self) -> bool {
        self.console() || self.levels.iter().any(|l| l.load(Ordering::Relaxed) != 0)
    }

    /// Whether every page's console is written to the terminal (logging is on).
    pub fn console(&self) -> bool {
        self.console.load(Ordering::Relaxed)
    }

    /// Turns the terminal output of every page's console on, as `--enable-logging` does.
    pub fn set_console(&self, on: bool) {
        self.console.store(on, Ordering::Relaxed);
    }

    /// One line of the console of page `contents`:
    /// `[renderer:<id> <file>:<line>] <message>`, coloured by level on a terminal.
    pub fn console_line(&self, contents: ContentsId, level: Level, at: (&str, u32), message: &str) {
        let (file, line) = at;
        let file = file.rsplit(['/', '\\']).next().unwrap_or(file);
        let place = if file.is_empty() {
            format!("renderer:{contents}")
        } else {
            format!("renderer:{contents} {file}:{line}")
        };
        let text = format!("[{place}] {message}");
        let color = match level {
            Level::Warn => "\x1b[33m",
            Level::Error => "\x1b[31m",
            Level::Debug => "\x1b[2m",
            Level::Log | Level::Info => "",
        };
        if self.colors && !color.is_empty() {
            self.terminal(&format!("{color}{text}\x1b[0m"));
        } else {
            self.terminal(&text);
        }
    }
}

/// The log of this process, made from its environment on first use.
pub(crate) fn process_log() -> &'static Arc<Log> {
    static GLOBAL: OnceLock<Arc<Log>> = OnceLock::new();
    GLOBAL.get_or_init(|| Arc::new(Log::from_environment()))
}

/// Writes a diagnostic line when its category is on: `diag!(log, Ipc, Info, "send {channel}")`.
macro_rules! diag {
    ($log:expr, $category:ident, $severity:ident, $($line:tt)+) => {
        if $log.enabled($crate::log::Category::$category, $crate::log::Severity::$severity) {
            $log.write(
                $crate::log::Category::$category,
                $crate::log::Severity::$severity,
                format_args!($($line)+),
            );
        }
    };
}
pub(crate) use diag;

/// `ELECTRON_ENABLE_LOGGING` as Electron 39.8.10 reads it on Windows: any value that is set
/// turns logging on, `0` and `false` too (tests/electron/devtools/expected/logging-env.txt).
fn enabled_by(value: Option<&str>) -> bool {
    value.is_some_and(|v| !v.is_empty())
}

/// Whether colours are written, in the order of Node 22's `lib/internal/tty.js`: a `FORCE_COLOR`
/// that is empty, `1`, `true`, `2` or `3` turns them on and any other value off, whatever else is
/// set; without it, a `NO_COLOR` with any value, even an empty one, turns them off; otherwise
/// they follow whether the output is a terminal.
fn wants_color(force: Option<&str>, no_color: Option<&str>, is_terminal: bool) -> bool {
    match (force, no_color) {
        (Some(force), _) => matches!(force, "" | "1" | "true" | "2" | "3"),
        (None, Some(_)) => false,
        (None, None) => is_terminal,
    }
}

/// Gives a process that has no console one, so that what it writes on the terminal (logging
/// switches, the diagnostics, the warnings of the switches it cannot honour) reaches the terminal
/// that started it. The executable is a console program, which has one and is left as it is,
/// until it becomes a Windows GUI program, which starts with none: this then attaches to the
/// parent's and points standard output and error at it unless they were redirected.
#[cfg(windows)]
pub fn attach_parent_console() {
    use windows_sys::Win32::Foundation::INVALID_HANDLE_VALUE;
    use windows_sys::Win32::Storage::FileSystem::{
        CreateFileW, FILE_GENERIC_WRITE, FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING,
    };
    use windows_sys::Win32::System::Console::{
        ATTACH_PARENT_PROCESS, AttachConsole, GetStdHandle, STD_ERROR_HANDLE, STD_OUTPUT_HANDLE,
        SetStdHandle,
    };
    let conout: Vec<u16> = "CONOUT$\0".encode_utf16().collect();
    // SAFETY: the calls take no pointer but `conout`, which ends with a NUL and outlives them, and
    // a null security descriptor and template, which they allow.
    unsafe {
        if AttachConsole(ATTACH_PARENT_PROCESS) == 0 {
            return;
        }
        for which in [STD_OUTPUT_HANDLE, STD_ERROR_HANDLE] {
            let current = GetStdHandle(which);
            if !current.is_null() && current != INVALID_HANDLE_VALUE {
                continue;
            }
            let console = CreateFileW(
                conout.as_ptr(),
                FILE_GENERIC_WRITE,
                FILE_SHARE_READ | FILE_SHARE_WRITE,
                std::ptr::null(),
                OPEN_EXISTING,
                0,
                std::ptr::null_mut(),
            );
            if console != INVALID_HANDLE_VALUE {
                SetStdHandle(which, console);
            }
        }
    }
}

/// Elsewhere a program started from a terminal already writes to it.
#[cfg(not(windows))]
pub fn attach_parent_console() {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_filter_reads_names_levels_and_all() {
        let filter = Filter::parse("ipc, load=debug,bogus,asar=loud");
        let level = |c: Category| filter.0[c as usize];
        assert_eq!(level(Category::Ipc), Severity::Info as u8);
        assert_eq!(level(Category::Load), Severity::Debug as u8);
        assert_eq!(level(Category::Asar), 0);
        assert_eq!(level(Category::Console), 0);
        let all = Filter::parse("all=warn,shell=trace");
        assert_eq!(all.0[Category::Perf as usize], Severity::Warn as u8);
        assert_eq!(all.0[Category::Shell as usize], Severity::Trace as u8);
    }

    #[test]
    fn a_category_shows_its_level_and_those_above() {
        let log = Log::to_memory(Filter::parse("ipc=warn,load=debug"));
        assert!(log.enabled(Category::Ipc, Severity::Error));
        assert!(log.enabled(Category::Ipc, Severity::Warn));
        assert!(!log.enabled(Category::Ipc, Severity::Info));
        assert!(log.enabled(Category::Load, Severity::Info));
        assert!(!log.enabled(Category::Load, Severity::Trace));
        assert!(!log.enabled(Category::Shell, Severity::Error));
    }

    #[test]
    fn a_diagnostic_line_names_its_category_and_a_closed_one_is_not_formatted() {
        let log = Log::to_memory(Filter::parse("ipc"));
        let mut formatted = 0;
        let mut count = || {
            formatted += 1;
            formatted
        };
        diag!(log, Ipc, Info, "send {}", count());
        diag!(log, Load, Info, "load {}", count());
        diag!(log, Ipc, Warn, "late {}", count());
        assert_eq!(
            log.written(),
            "[verve:ipc] send 1\n[verve:ipc:warn] late 2\n"
        );
    }

    #[test]
    fn colour_follows_force_color_then_no_color_then_the_terminal() {
        assert!(wants_color(None, None, true));
        assert!(!wants_color(None, None, false));
        assert!(!wants_color(None, Some("1"), true));
        assert!(!wants_color(None, Some(""), true));
        assert!(wants_color(Some("1"), Some("1"), false));
        assert!(!wants_color(Some("0"), None, true));
        assert!(!wants_color(Some("false"), None, true));
        assert!(wants_color(Some(""), None, false));
    }

    #[test]
    fn logging_is_on_for_any_value_the_variable_is_set_to() {
        assert!(enabled_by(Some("1")));
        assert!(enabled_by(Some("file")));
        assert!(enabled_by(Some("0")));
        assert!(enabled_by(Some("false")));
        assert!(!enabled_by(Some("")));
        assert!(!enabled_by(None));
    }

    #[test]
    fn a_console_line_names_the_page_and_the_place_and_colours_by_level() {
        let log = Log::with(Filter::default(), Sink::Memory(Mutex::default()), true);
        log.console_line(2, Level::Warn, ("file:///C:/app/page.html", 4), "careful");
        log.console_line(2, Level::Log, ("file:///C:/app/page.html", 5), "plain");
        log.console_line(3, Level::Error, ("", 0), "no place");
        assert_eq!(
            log.written(),
            "\x1b[33m[renderer:2 page.html:4] careful\x1b[0m\n\
             [renderer:2 page.html:5] plain\n\
             \x1b[31m[renderer:3] no place\x1b[0m\n"
        );
    }
}

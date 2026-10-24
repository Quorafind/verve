//! `os`, and the constants Node keeps for the system: `os.constants` (signals, error numbers,
//! priorities), `fs.constants`, and the old `constants` module that merges them.
//!
//! What the standard library can say is real — the platform, the architecture, the folders, the
//! host name, how many processors. What it cannot (memory, the kernel's version, load) is real
//! on Linux, read from `/proc`, and elsewhere a plausible figure: enough for code that logs it or
//! sizes a cache by it, without a dependency that asks the system.

use boa_engine::{
    Context, JsResult, JsString, JsValue, js_string, native_function::NativeFunctionPointer,
};

use crate::native::{arg, array, exports, js_text, object};

/// `fs.constants`, with Linux's numbers (what Node's own constants are on most systems): code
/// passes them back to `fs`, which reads them with the same numbers.
pub(crate) const FS_CONSTANTS: &[(&str, i64)] = &[
    ("UV_FS_SYMLINK_DIR", 1),
    ("UV_FS_SYMLINK_JUNCTION", 2),
    ("O_RDONLY", 0),
    ("O_WRONLY", 1),
    ("O_RDWR", 2),
    ("UV_DIRENT_UNKNOWN", 0),
    ("UV_DIRENT_FILE", 1),
    ("UV_DIRENT_DIR", 2),
    ("UV_DIRENT_LINK", 3),
    ("UV_DIRENT_FIFO", 4),
    ("UV_DIRENT_SOCKET", 5),
    ("UV_DIRENT_CHAR", 6),
    ("UV_DIRENT_BLOCK", 7),
    ("S_IFMT", 0o170000),
    ("S_IFREG", 0o100000),
    ("S_IFDIR", 0o040000),
    ("S_IFCHR", 0o020000),
    ("S_IFBLK", 0o060000),
    ("S_IFIFO", 0o010000),
    ("S_IFLNK", 0o120000),
    ("S_IFSOCK", 0o140000),
    ("O_CREAT", 0o100),
    ("O_EXCL", 0o200),
    ("UV_FS_O_FILEMAP", 0),
    ("O_NOCTTY", 0o400),
    ("O_TRUNC", 0o1000),
    ("O_APPEND", 0o2000),
    ("O_DIRECTORY", 0o200000),
    ("O_NOATIME", 0o1000000),
    ("O_NOFOLLOW", 0o400000),
    ("O_SYNC", 0o4010000),
    ("O_DSYNC", 0o10000),
    ("O_DIRECT", 0o40000),
    ("O_NONBLOCK", 0o4000),
    ("S_IRWXU", 0o700),
    ("S_IRUSR", 0o400),
    ("S_IWUSR", 0o200),
    ("S_IXUSR", 0o100),
    ("S_IRWXG", 0o70),
    ("S_IRGRP", 0o40),
    ("S_IWGRP", 0o20),
    ("S_IXGRP", 0o10),
    ("S_IRWXO", 0o7),
    ("S_IROTH", 0o4),
    ("S_IWOTH", 0o2),
    ("S_IXOTH", 0o1),
    ("F_OK", 0),
    ("R_OK", 4),
    ("W_OK", 2),
    ("X_OK", 1),
    ("UV_FS_COPYFILE_EXCL", 1),
    ("COPYFILE_EXCL", 1),
    ("UV_FS_COPYFILE_FICLONE", 2),
    ("COPYFILE_FICLONE", 2),
    ("UV_FS_COPYFILE_FICLONE_FORCE", 4),
    ("COPYFILE_FICLONE_FORCE", 4),
];

pub(crate) const SIGNALS: &[(&str, i64)] = &[
    ("SIGHUP", 1),
    ("SIGINT", 2),
    ("SIGQUIT", 3),
    ("SIGILL", 4),
    ("SIGTRAP", 5),
    ("SIGABRT", 6),
    ("SIGIOT", 6),
    ("SIGBUS", 7),
    ("SIGFPE", 8),
    ("SIGKILL", 9),
    ("SIGUSR1", 10),
    ("SIGSEGV", 11),
    ("SIGUSR2", 12),
    ("SIGPIPE", 13),
    ("SIGALRM", 14),
    ("SIGTERM", 15),
    ("SIGCHLD", 17),
    ("SIGSTKFLT", 16),
    ("SIGCONT", 18),
    ("SIGSTOP", 19),
    ("SIGTSTP", 20),
    ("SIGTTIN", 21),
    ("SIGTTOU", 22),
    ("SIGURG", 23),
    ("SIGXCPU", 24),
    ("SIGXFSZ", 25),
    ("SIGVTALRM", 26),
    ("SIGPROF", 27),
    ("SIGWINCH", 28),
    ("SIGIO", 29),
    ("SIGPOLL", 29),
    ("SIGPWR", 30),
    ("SIGSYS", 31),
];

/// Error numbers, Linux's; `fs` errors carry them negated as `errno`, as libuv's do.
pub(crate) const ERRNO: &[(&str, i64)] = &[
    ("E2BIG", 7),
    ("EACCES", 13),
    ("EADDRINUSE", 98),
    ("EADDRNOTAVAIL", 99),
    ("EAFNOSUPPORT", 97),
    ("EAGAIN", 11),
    ("EALREADY", 114),
    ("EBADF", 9),
    ("EBUSY", 16),
    ("ECANCELED", 125),
    ("ECHILD", 10),
    ("ECONNABORTED", 103),
    ("ECONNREFUSED", 111),
    ("ECONNRESET", 104),
    ("EDEADLK", 35),
    ("EDESTADDRREQ", 89),
    ("EDOM", 33),
    ("EEXIST", 17),
    ("EFAULT", 14),
    ("EFBIG", 27),
    ("EHOSTUNREACH", 113),
    ("EINPROGRESS", 115),
    ("EINTR", 4),
    ("EINVAL", 22),
    ("EIO", 5),
    ("EISCONN", 106),
    ("EISDIR", 21),
    ("ELOOP", 40),
    ("EMFILE", 24),
    ("EMLINK", 31),
    ("EMSGSIZE", 90),
    ("ENAMETOOLONG", 36),
    ("ENETDOWN", 100),
    ("ENETRESET", 102),
    ("ENETUNREACH", 101),
    ("ENFILE", 23),
    ("ENOBUFS", 105),
    ("ENODEV", 19),
    ("ENOENT", 2),
    ("ENOEXEC", 8),
    ("ENOMEM", 12),
    ("ENOSPC", 28),
    ("ENOSYS", 38),
    ("ENOTCONN", 107),
    ("ENOTDIR", 20),
    ("ENOTEMPTY", 39),
    ("ENOTSOCK", 88),
    ("ENOTSUP", 95),
    ("ENXIO", 6),
    ("EOPNOTSUPP", 95),
    ("EOVERFLOW", 75),
    ("EPERM", 1),
    ("EPIPE", 32),
    ("EPROTO", 71),
    ("ERANGE", 34),
    ("EROFS", 30),
    ("ESPIPE", 29),
    ("ESRCH", 3),
    ("ETIMEDOUT", 110),
    ("ETXTBSY", 26),
    ("EWOULDBLOCK", 11),
    ("EXDEV", 18),
];

const PRIORITY: &[(&str, i64)] = &[
    ("PRIORITY_LOW", 19),
    ("PRIORITY_BELOW_NORMAL", 10),
    ("PRIORITY_NORMAL", 0),
    ("PRIORITY_ABOVE_NORMAL", -7),
    ("PRIORITY_HIGH", -14),
    ("PRIORITY_HIGHEST", -20),
];

/// The error number of `code`, as `os.constants.errno` has it.
pub(crate) fn errno(code: &str) -> i64 {
    ERRNO
        .iter()
        .find(|(name, _)| *name == code)
        .map_or(0, |(_, n)| *n)
}

pub(crate) fn constants_object(
    context: &mut Context,
    list: &[(&str, i64)],
) -> JsResult<boa_engine::JsObject> {
    let out = object(context);
    for (name, value) in list {
        out.set(JsString::from(*name), *value as f64, false, context)?;
    }
    Ok(out)
}

fn home() -> String {
    std::env::var("HOME")
        .or_else(|_| std::env::var("USERPROFILE"))
        .unwrap_or_else(|_| "/".into())
}

fn user() -> String {
    std::env::var("USERNAME")
        .or_else(|_| std::env::var("USER"))
        .unwrap_or_default()
}

fn cores() -> usize {
    std::thread::available_parallelism().map_or(1, |n| n.get())
}

/// A line of `/proc/meminfo`, in bytes.
fn meminfo(key: &str) -> Option<f64> {
    let text = std::fs::read_to_string("/proc/meminfo").ok()?;
    let line = text.lines().find(|l| l.starts_with(key))?;
    let kb: f64 = line.split_whitespace().nth(1)?.parse().ok()?;
    Some(kb * 1024.)
}

fn release() -> String {
    if let Ok(text) = std::fs::read_to_string("/proc/sys/kernel/osrelease") {
        return text.trim().to_owned();
    }
    match std::env::consts::OS {
        "windows" => "10.0.19045".into(),
        "macos" => "24.0.0".into(),
        _ => "6.0.0".into(),
    }
}

fn os_type() -> &'static str {
    match std::env::consts::OS {
        "windows" => "Windows_NT",
        "macos" | "ios" => "Darwin",
        "freebsd" => "FreeBSD",
        "openbsd" => "OpenBSD",
        _ => "Linux",
    }
}

fn machine() -> &'static str {
    match std::env::consts::ARCH {
        "x86_64" => "x86_64",
        "aarch64" => {
            if cfg!(target_os = "linux") {
                "aarch64"
            } else {
                "arm64"
            }
        }
        "x86" => "i686",
        other => other,
    }
}

const OS_FUNCTIONS: &[(&str, usize, NativeFunctionPointer)] = &[
    ("platform", 0, |_, _, _| {
        Ok(js_text(crate::node::platform()))
    }),
    ("type", 0, |_, _, _| Ok(js_text(os_type()))),
    ("arch", 0, |_, _, _| {
        Ok(js_text(match std::env::consts::ARCH {
            "x86_64" => "x64",
            "aarch64" => "arm64",
            "x86" => "ia32",
            other => other,
        }))
    }),
    ("machine", 0, |_, _, _| Ok(js_text(machine()))),
    ("release", 0, |_, _, _| Ok(js_text(&release()))),
    ("version", 0, |_, _, _| {
        Ok(js_text(match std::env::consts::OS {
            "windows" => "Windows 10 Pro",
            "macos" => "Darwin Kernel Version 24.0.0",
            _ => "#1 SMP",
        }))
    }),
    ("homedir", 0, |_, _, _| Ok(js_text(&home()))),
    ("tmpdir", 0, |_, _, _| {
        let dir = std::env::temp_dir().to_string_lossy().into_owned();
        // Node takes off a trailing separator (but not a root's).
        let trimmed = dir.trim_end_matches(['/', '\\']);
        Ok(js_text(if trimmed.is_empty() || trimmed.ends_with(':') {
            &dir
        } else {
            trimmed
        }))
    }),
    ("hostname", 0, |_, _, _| {
        let name = std::env::var("COMPUTERNAME")
            .or_else(|_| std::env::var("HOSTNAME"))
            .ok()
            .or_else(|| {
                std::fs::read_to_string("/etc/hostname")
                    .ok()
                    .map(|t| t.trim().to_owned())
            })
            .unwrap_or_else(|| "localhost".into());
        Ok(js_text(&name))
    }),
    ("cpus", 0, |_, _, cx| {
        let mut list = Vec::new();
        for _ in 0..cores() {
            let cpu = object(cx);
            cpu.set(js_string!("model"), js_text("CPU"), false, cx)?;
            cpu.set(js_string!("speed"), 2400, false, cx)?;
            let times = object(cx);
            for key in ["user", "nice", "sys", "idle", "irq"] {
                times.set(JsString::from(key), 0, false, cx)?;
            }
            cpu.set(js_string!("times"), times, false, cx)?;
            list.push(cpu.into());
        }
        Ok(array(cx, list))
    }),
    ("availableParallelism", 0, |_, _, _| Ok(cores().into())),
    ("totalmem", 0, |_, _, _| {
        Ok(meminfo("MemTotal:")
            .unwrap_or(16. * 1024. * 1024. * 1024.)
            .into())
    }),
    ("freemem", 0, |_, _, _| {
        Ok(meminfo("MemAvailable:")
            .unwrap_or(8. * 1024. * 1024. * 1024.)
            .into())
    }),
    ("uptime", 0, |_, _, cx| {
        let from_proc = std::fs::read_to_string("/proc/uptime").ok().and_then(|t| {
            t.split_whitespace()
                .next()
                .and_then(|n| n.parse::<f64>().ok())
        });
        Ok(from_proc
            .unwrap_or_else(|| 3600. + crate::now(cx) / 1000.)
            .floor()
            .into())
    }),
    ("loadavg", 0, |_, _, cx| {
        Ok(array(cx, vec![0.into(), 0.into(), 0.into()]))
    }),
    ("networkInterfaces", 0, |_, _, cx| Ok(object(cx).into())),
    ("endianness", 0, |_, _, _| {
        Ok(js_text(if cfg!(target_endian = "little") {
            "LE"
        } else {
            "BE"
        }))
    }),
    ("getPriority", 1, |_, _, _| Ok(0.into())),
    ("setPriority", 2, |_, _, _| Ok(JsValue::undefined())),
    ("userInfo", 1, |_, args, cx| {
        let info = object(cx);
        let as_buffer = crate::native::get(&arg(args, 0), "encoding", cx)?
            .as_string()
            .is_some_and(|e| e.to_std_string_escaped() == "buffer");
        let text = |value: &str, cx: &mut Context| -> JsResult<JsValue> {
            if as_buffer {
                crate::encoding::make_buffer(cx, value.as_bytes().to_vec())
            } else {
                Ok(js_text(value))
            }
        };
        let id = if cfg!(windows) { -1 } else { 1000 };
        info.set(js_string!("uid"), id, false, cx)?;
        info.set(js_string!("gid"), id, false, cx)?;
        let username = text(&user(), cx)?;
        info.set(js_string!("username"), username, false, cx)?;
        let homedir = text(&home(), cx)?;
        info.set(js_string!("homedir"), homedir, false, cx)?;
        let shell = if cfg!(windows) {
            JsValue::null()
        } else {
            text(
                &std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".into()),
                cx,
            )?
        };
        info.set(js_string!("shell"), shell, false, cx)?;
        Ok(info.into())
    }),
];

pub(crate) fn module(context: &mut Context, _: &str) -> JsResult<JsValue> {
    let os = object(context);
    exports(context, &os, OS_FUNCTIONS)?;
    os.set(
        js_string!("EOL"),
        js_text(if cfg!(windows) { "\r\n" } else { "\n" }),
        false,
        context,
    )?;
    os.set(
        js_string!("devNull"),
        js_text(if cfg!(windows) {
            "\\\\.\\nul"
        } else {
            "/dev/null"
        }),
        false,
        context,
    )?;
    let constants = object(context);
    let signals = constants_object(context, SIGNALS)?;
    constants.set(js_string!("signals"), signals, false, context)?;
    let errno = constants_object(context, ERRNO)?;
    constants.set(js_string!("errno"), errno, false, context)?;
    let priority = constants_object(context, PRIORITY)?;
    constants.set(js_string!("priority"), priority, false, context)?;
    let dlopen = constants_object(
        context,
        &[
            ("RTLD_LAZY", 1),
            ("RTLD_NOW", 2),
            ("RTLD_GLOBAL", 256),
            ("RTLD_LOCAL", 0),
        ],
    )?;
    constants.set(js_string!("dlopen"), dlopen, false, context)?;
    constants.set(js_string!("UV_UDP_REUSEADDR"), 4, false, context)?;
    os.set(js_string!("constants"), constants, false, context)?;
    Ok(os.into())
}

/// The deprecated `constants` module: `os.constants`' error numbers, signals and priorities and
/// `fs.constants`, all at the top level.
pub(crate) fn constants_module(context: &mut Context, _: &str) -> JsResult<JsValue> {
    let out = object(context);
    for list in [ERRNO, SIGNALS, PRIORITY, FS_CONSTANTS] {
        for (name, value) in list {
            out.set(JsString::from(*name), *value as f64, false, context)?;
        }
    }
    for (name, value) in [
        ("RTLD_LAZY", 1),
        ("RTLD_NOW", 2),
        ("RTLD_GLOBAL", 256),
        ("RTLD_LOCAL", 0),
    ] {
        out.set(JsString::from(name), value, false, context)?;
    }
    Ok(out.into())
}

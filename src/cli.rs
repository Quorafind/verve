//! `verve run`: a script as a process of its own, the way `node script.js` runs one.
//!
//! The library's realm is a guest in someone else's window — the host owns the clock, the
//! console and the end of the program. Here the realm is the program: `process.argv` is this
//! command line, `process.env` this environment, `console` and `process.stdout` write to the
//! process's own descriptors, `process.stdin` reads its input, and the process ends when Node's
//! would. An embedder that ships a binary of its own calls [`main`] from it.
//!
//! **The exit rule** is Node's. The process runs until nothing can make progress any more: no
//! timer that was not `unref()`ed, no input being read (`process.stdin` flowing, or listened to
//! for `readable`, and not at its end), no worker, no socket, server or lookup that was not
//! `unref()`ed. Then `beforeExit` is emitted — if its listeners start something, the process
//! goes on — and it exits with `process.exitCode`, or 0. An ES module entry whose top-level
//! `await` never settled exits 13 with Node's warning. An uncaught exception, or a rejection with
//! no `unhandledRejection` listener (Node's `--unhandled-rejections=throw`), is printed and exits
//! 1, after `uncaughtException` listeners if there are any (a listener that throws exits 7).
//! `process.exit(code)` tells the `exit` listeners and ends the process there and then.
//!
//! **Waiting** is a block on one signal, with a timeout of the next timer: the input's reader
//! thread, the network reactor and every worker wake it when they have something, through
//! [`crate::Runtime::set_waker`]. Nothing polls.
//!
//! **Permissions** are on, as with Node's `--permission`, unless `-A` grants everything: no
//! file may be read or written, no worker started and no host reached unless a flag says so.
//! The entry file itself may always be read — the one path a script needs to start — but not
//! the folder it is in, so what it imports must be granted too. A `-e` script needs no grant.
//! Child processes, native addons and WASI do not exist here, granted or not.
//!
//! Flags come Deno's way with Node's spellings beside them, so a command line built for
//! `node --permission` maps one to one:
//!
//! | flag | means |
//! |---|---|
//! | `-A`, `--allow-all` | everything granted |
//! | `--allow-read[=a,b]`, `--allow-fs-read=<path>` (repeatable, `*` for all) | read |
//! | `--allow-write[=a,b]`, `--allow-fs-write=<path>` (repeatable, `*` for all) | write |
//! | `--allow-net[=host[:port],…]` | connect to those (alone: anything; and listen, and DNS) |
//! | `--allow-worker` | start workers |
//! | `--use-env-proxy` | `fetch`, `WebSocket` and the `http`/`https` global agents go through `HTTP_PROXY`/`HTTPS_PROXY`, minus `NO_PROXY` — also when `NODE_USE_ENV_PROXY=1` |
//! | `--permission`, `--experimental-permission` | accepted: permissions are always on |
//! | `-e`, `--eval <source>` | run `source` as `node -e` does (also `verve eval <source>`) |
//! | `--disable-proto=throw\|delete` | `Object.prototype.__proto__` throws, or is gone |
//! | `--stack-size=<KiB>`, `--stack_size=<KiB>` | the script thread's stack, in KiB as V8's |
//! | `--no-warnings` | `process.emitWarning` writes nothing |
//! | `--jitless` | hot functions stay interpreted, whatever `VERVE_JIT` says (Node's flag) |
//! | `--code-cache=<dir>` | keep the compiled code of ES modules in `dir` between runs (also `VERVE_CODE_CACHE=<dir>`); see `Runtime::set_code_cache_dir` |
//! | `--experimental-vm-modules`, `--no-addons` | accepted; nothing to change here |
//! | `--max-old-space-size=<MiB>` | accepted and ignored: Boa has no heap cap; bound memory with the OS (a Job Object, `RLIMIT_AS`) |
//!
//! `--allow-read` and `--allow-write` split their value at commas; Node's `--allow-fs-read` and
//! `--allow-fs-write` take one path each, as Node 22 does. Paths are relative to the working
//! directory. An unknown flag is an error (exit 9, as Node's).
//!
//! **A proxy** is Node 24's: with `--use-env-proxy` or `NODE_USE_ENV_PROXY=1` the proxy
//! variables are read once, at startup, and `--allow-net` is checked against each request's
//! target, never the proxy — a host that confines the process to its proxy grants the hosts it
//! means, and the proxy it set up needs no grant.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

use boa_engine::{
    Context, JsError, JsResult, JsValue, Source, js_string,
    object::builtins::{JsFunction, JsPromise},
};

use crate::native::{function, js_text, object};
use crate::{FsAccess, NetworkRule, Permissions, Runtime};

/// The script thread's stack when no `--stack-size` is given: Boa's interpreter recurses
/// deeply, and a bundle parsed in a debug build needs room.
const DEFAULT_STACK: usize = 64 * 1024 * 1024;

/// The least stack a script thread is given, whatever `--stack-size` asks: below it, Boa
/// overflows the thread parsing an ordinary module before any script could hit its own limit.
const MIN_STACK: usize = 4 * 1024 * 1024;

const USAGE: &str = "usage: verve run [flags] <entry.js|.mjs|.cjs> [args...]
       verve run [flags] -e <source> [args...]
       verve --version

flags: -A/--allow-all, --allow-read[=paths], --allow-fs-read=<path>, --allow-write[=paths],
       --allow-fs-write=<path>, --allow-net[=hosts], --allow-worker, --permission,
       --use-env-proxy,
       --disable-proto=throw|delete, --stack-size=<KiB>, --no-warnings, --jitless,
       --code-cache=<dir>,
       --experimental-vm-modules, --no-addons, --max-old-space-size=<MiB>";

/// What the command line asked for.
#[derive(Debug, Default)]
struct Options {
    all: bool,
    read: Option<Vec<PathBuf>>,
    write: Option<Vec<PathBuf>>,
    net: Option<Vec<NetworkRule>>,
    worker: bool,
    use_env_proxy: bool,
    stack_kib: Option<usize>,
    disable_proto: Option<Proto>,
    no_warnings: bool,
    jitless: bool,
    code_cache: Option<PathBuf>,
    eval: Option<String>,
    entry: Option<PathBuf>,
    args: Vec<String>,
    exec_argv: Vec<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Proto {
    Throw,
    Delete,
}

/// What the command line comes to.
enum Command {
    Run(Box<Options>),
    Print(String),
}

/// `verve`'s entry: `args` as `std::env::args_os()` gives them, the program's name first.
/// Answers the code the process should exit with. `process.exit()` ends the process itself,
/// as Node's does, without returning here.
pub fn main(args: impl IntoIterator<Item = OsString>) -> ExitCode {
    main_with(args, |_| Ok(()))
}

/// What an embedder adds to the runtime before the script runs: builtin modules, globals.
/// Called on the script's thread, after the process globals exist and before any user code;
/// an error ends the process with code 70, as a failed process setup does.
pub type Setup = fn(&mut Runtime) -> Result<(), String>;

/// [`main`] for an embedder that ships its own runner: `setup` runs on the runtime before
/// the script does (see [`Setup`]).
pub fn main_with(args: impl IntoIterator<Item = OsString>, setup: Setup) -> ExitCode {
    let args: Vec<String> = args
        .into_iter()
        .map(|a| a.to_string_lossy().into_owned())
        .collect();
    let argv0 = args.first().cloned().unwrap_or_else(|| "verve".into());
    match parse(args.into_iter().skip(1).collect()) {
        Ok(Command::Print(text)) => {
            println!("{text}");
            ExitCode::SUCCESS
        }
        Ok(Command::Run(options)) => {
            let code = run(*options, argv0, setup);
            ExitCode::from((code & 0xff) as u8)
        }
        Err(message) => {
            eprintln!("verve: {message}\n{USAGE}");
            ExitCode::from(9)
        }
    }
}

fn parse(args: Vec<String>) -> Result<Command, String> {
    let mut args = args.into_iter().peekable();
    let mut options = Options::default();
    match args.peek().map(String::as_str) {
        None => return Err("no script given".into()),
        Some("--version" | "-v" | "-V") => {
            return Ok(Command::Print(format!(
                "verve {} (node {})",
                env!("CARGO_PKG_VERSION"),
                crate::process::NODE_VERSION
            )));
        }
        Some("--help" | "-h" | "help") => return Ok(Command::Print(USAGE.into())),
        Some("run") => {
            args.next();
        }
        Some("eval") => {
            args.next();
            let source = args.next().ok_or("eval needs a source")?;
            options.eval = Some(source);
        }
        _ => {}
    }
    while let Some(arg) = args.next() {
        if arg == "--" {
            break;
        }
        if !arg.starts_with('-') || arg == "-" {
            if options.eval.is_none() {
                options.entry = Some(PathBuf::from(arg));
            } else {
                options.args.push(arg);
            }
            break;
        }
        let (flag, value) = match arg.split_once('=') {
            Some((flag, value)) => (flag.to_owned(), Some(value.to_owned())),
            None => (arg.clone(), None),
        };
        let mut taken = None;
        let mut needs_value = |name: &str| -> Result<String, String> {
            match &value {
                Some(value) => Ok(value.clone()),
                None => {
                    let next = args.next().ok_or(format!("{name} needs a value"))?;
                    taken = Some(next.clone());
                    Ok(next)
                }
            }
        };
        match flag.as_str() {
            "-A" | "--allow-all" => options.all = true,
            "--allow-read" => grant_paths(&mut options.read, value.as_deref(), true),
            "--allow-fs-read" => {
                let path = needs_value("--allow-fs-read")?;
                grant_paths(&mut options.read, Some(&path), false);
            }
            "--allow-write" => grant_paths(&mut options.write, value.as_deref(), true),
            "--allow-fs-write" => {
                let path = needs_value("--allow-fs-write")?;
                grant_paths(&mut options.write, Some(&path), false);
            }
            "--allow-net" => {
                let rules = options.net.get_or_insert_with(Vec::new);
                match &value {
                    None => rules.push(NetworkRule {
                        host: None,
                        port: None,
                    }),
                    Some(list) => {
                        for entry in list.split(',').filter(|e| !e.is_empty()) {
                            rules.push(net_rule(entry)?);
                        }
                    }
                }
            }
            "--allow-worker" => options.worker = true,
            "--use-env-proxy" => options.use_env_proxy = true,
            "--permission" | "--experimental-permission" => {}
            "--no-warnings" => options.no_warnings = true,
            "--jitless" => options.jitless = true,
            "--code-cache" => {
                options.code_cache = Some(PathBuf::from(needs_value("--code-cache")?))
            }
            "--experimental-vm-modules" | "--no-addons" => {}
            "--max-old-space-size" => {
                let size = needs_value("--max-old-space-size")?;
                size.parse::<u64>()
                    .map_err(|_| format!("--max-old-space-size: not a number: {size}"))?;
            }
            "--stack-size" | "--stack_size" => {
                let size = needs_value("--stack-size")?;
                let kib = size
                    .parse::<usize>()
                    .map_err(|_| format!("--stack-size: not a number of KiB: {size}"))?;
                options.stack_kib = Some(kib);
            }
            "--disable-proto" => {
                options.disable_proto = Some(match needs_value("--disable-proto")?.as_str() {
                    "throw" => Proto::Throw,
                    "delete" => Proto::Delete,
                    other => {
                        return Err(format!(
                            "--disable-proto must be throw or delete, not {other}"
                        ));
                    }
                });
            }
            "-e" | "--eval" => options.eval = Some(needs_value("--eval")?),
            "--allow-child-process" | "--allow-addons" | "--allow-wasi" => {
                return Err(format!(
                    "{flag}: Verve has no child processes, native addons or WASI to allow"
                ));
            }
            _ => return Err(format!("unknown flag {arg}")),
        }
        // `execArgv` is the runtime's own part of the command line, values and all.
        options.exec_argv.push(arg.clone());
        options.exec_argv.extend(taken);
    }
    options.args.extend(args);
    if options.eval.is_none() && options.entry.is_none() {
        return Err("no script given".into());
    }
    Ok(Command::Run(Box::new(options)))
}

/// A path grant: `*` (or a bare `--allow-read`) for everything, else the paths, relative to
/// the working directory.
fn grant_paths(grant: &mut Option<Vec<PathBuf>>, value: Option<&str>, commas: bool) {
    let list = grant.get_or_insert_with(Vec::new);
    let Some(value) = value else {
        list.push(PathBuf::from("*"));
        return;
    };
    let parts: Vec<&str> = if commas {
        value.split(',').filter(|p| !p.is_empty()).collect()
    } else {
        vec![value]
    };
    for part in parts {
        list.push(PathBuf::from(part));
    }
}

/// `host`, `host:port` or `[v6]:port` as an outbound rule.
fn net_rule(entry: &str) -> Result<NetworkRule, String> {
    let (host, port) = if let Some(rest) = entry.strip_prefix('[') {
        let (host, rest) = rest
            .split_once(']')
            .ok_or_else(|| format!("--allow-net: bad address {entry}"))?;
        (host.to_owned(), rest.strip_prefix(':'))
    } else {
        match entry.rsplit_once(':') {
            Some((host, port)) if !host.contains(':') => (host.to_owned(), Some(port)),
            _ => (entry.to_owned(), None),
        }
    };
    let port = match port {
        Some(port) => Some(
            port.parse::<u16>()
                .map_err(|_| format!("--allow-net: bad port in {entry}"))?,
        ),
        None => None,
    };
    let host = if host.is_empty() || host == "*" {
        None
    } else {
        Some(host)
    };
    Ok(NetworkRule { host, port })
}

/// The grants the options come to; `entry` may always be read.
fn permissions(options: &Options, entry: Option<&Path>) -> Permissions {
    if options.all {
        return Permissions::all();
    }
    let mut permissions = Permissions::none();
    let fs = |grant: &Option<Vec<PathBuf>>, extra: Option<&Path>| match grant {
        Some(paths) if paths.iter().any(|p| p.as_os_str() == "*") => FsAccess::All,
        _ => {
            let mut paths: Vec<PathBuf> = grant.clone().unwrap_or_default();
            paths.extend(extra.map(Path::to_path_buf));
            FsAccess::Only(paths)
        }
    };
    permissions.fs_read = fs(&options.read, entry);
    permissions.fs_write = fs(&options.write, None);
    permissions.worker = options.worker;
    if let Some(rules) = &options.net {
        let any = rules.iter().any(|r| r.host.is_none() && r.port.is_none());
        permissions.outbound = rules.clone();
        if any {
            permissions.listen = crate::ListenPermission::Any;
            permissions.dns = true;
        }
    }
    permissions
}

/// The signal the script thread sleeps on, and everything that has work for it raises.
#[derive(Default)]
struct Signal {
    raised: Mutex<bool>,
    ready: Condvar,
}

impl Signal {
    fn raise(&self) {
        if let Ok(mut raised) = self.raised.lock() {
            *raised = true;
            self.ready.notify_one();
        }
    }

    fn wait(&self, timeout: Option<Duration>) {
        let Ok(mut raised) = self.raised.lock() else {
            return;
        };
        if !*raised {
            raised = match timeout {
                Some(timeout) => match self.ready.wait_timeout(raised, timeout) {
                    Ok((raised, _)) => raised,
                    Err(_) => return,
                },
                None => match self.ready.wait(raised) {
                    Ok(raised) => raised,
                    Err(_) => return,
                },
            };
        }
        *raised = false;
    }
}

/// Runs the script on a thread with the stack it asked for, and answers its exit code.
fn run(options: Options, argv0: String, embedder: Setup) -> i32 {
    let stack = options
        .stack_kib
        .map_or(DEFAULT_STACK, |kib| kib.saturating_mul(1024))
        .max(MIN_STACK);
    let thread = std::thread::Builder::new()
        .name("verve-main".into())
        .stack_size(stack)
        .spawn(move || {
            crate::set_thread_stack(stack);
            script(options, argv0, embedder)
        });
    match thread.map(|t| t.join()) {
        Ok(Ok(code)) => code,
        Ok(Err(_)) => 70,
        Err(error) => {
            eprintln!("verve: no thread to run the script on: {error}");
            70
        }
    }
}

/// `path` made absolute against the working directory, `.` and `..` taken out: Node's
/// `path.resolve`, which is what `process.argv[1]` is.
fn resolved(path: &Path) -> PathBuf {
    let absolute = crate::permission::absolute(path);
    crate::modules::clean(&absolute)
}

fn script(options: Options, argv0: String, embedder: Setup) -> i32 {
    let entry = options.entry.as_deref().map(resolved);
    let permissions = permissions(&options, entry.as_deref());
    let signal = Arc::new(Signal::default());
    let waker: crate::Waker = {
        let signal = signal.clone();
        Arc::new(move || signal.raise())
    };
    let mut runtime = Runtime::new();
    if options.jitless {
        runtime.set_jit_enabled(false);
    }
    let code_cache = options.code_cache.clone().or_else(|| {
        std::env::var_os("VERVE_CODE_CACHE")
            .filter(|dir| !dir.is_empty())
            .map(PathBuf::from)
    });
    if let Some(dir) = code_cache
        && let Err(error) = runtime.set_code_cache_dir(Some(&dir))
    {
        eprintln!("verve: code cache {}: {error}", dir.display());
    }
    runtime.set_permissions(permissions.clone());
    runtime.set_worker_permissions(permissions);
    runtime.set_waker(Some(waker.clone()));
    if options.use_env_proxy {
        runtime.set_proxy_env(Some(crate::ProxyEnv::from_process_vars()));
    } else if let Some(proxy) = crate::ProxyEnv::from_process_env() {
        runtime.set_proxy_env(Some(proxy));
    }
    let exec_path = std::env::current_exe()
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_else(|_| argv0.clone());
    let mut argv = vec![exec_path.clone()];
    argv.extend(entry.iter().map(|e| e.to_string_lossy().into_owned()));
    argv.extend(options.args.iter().cloned());
    let setup = crate::process::Setup {
        argv,
        argv0,
        exec_path,
        exec_argv: options.exec_argv.clone(),
        env: std::env::vars_os()
            .map(|(k, v)| {
                (
                    k.to_string_lossy().into_owned(),
                    v.to_string_lossy().into_owned(),
                )
            })
            .collect(),
        no_warnings: options.no_warnings,
        waker,
    };
    let context = runtime.context();
    if let Err(error) = crate::process::become_process(context, setup) {
        eprintln!("verve: {}", crate::describe(context, &error));
        return 70;
    }
    if let Err(error) = embedder(&mut runtime) {
        eprintln!("verve: {error}");
        return 70;
    }
    let context = runtime.context();
    if let Some(proto) = options.disable_proto
        && let Err(error) = disable_proto(context, proto)
    {
        eprintln!("verve: {}", crate::describe(context, &error));
        return 70;
    }
    let started = match (&options.eval, &entry) {
        (Some(source), _) => eval(context, source).map(|()| None),
        (None, Some(entry)) => start(context, entry),
        (None, None) => Ok(None),
    };
    let main = match started {
        Ok(main) => main,
        Err(error) => {
            crate::process::uncaught(context, &error);
            None
        }
    };
    runtime.run_microtasks();
    let code = event_loop(&mut runtime, &signal, main.as_ref());
    let code = crate::process::exiting(runtime.context(), code);
    use std::io::Write;
    let _ = std::io::stdout().flush();
    let _ = std::io::stderr().flush();
    code
}

/// Runs the entry: an ES module answers the promise of its evaluation, which a top-level
/// `await` may leave pending; CommonJS runs to its end here.
fn start(context: &mut Context, entry: &Path) -> JsResult<Option<(JsPromise, String)>> {
    let source = std::fs::read_to_string(entry).map_err(|error| {
        let error = crate::native::coded_error(
            context,
            &format!("Cannot find module '{}': {error}", entry.display()),
            "MODULE_NOT_FOUND",
        );
        if let Ok(value) = error.clone().into_opaque(context)
            && let Some(object) = value.as_object()
        {
            let stack = js_text(&format!("Error: Cannot find module '{}'", entry.display()));
            let _ = object.set(js_string!("stack"), stack, false, context);
        }
        error
    })?;
    if crate::esm::is_esm(entry, &source, context)? {
        let loader = crate::shared(context).esm.clone();
        let url = loader.insert_source(entry, &source, context)?;
        let promise = loader.start(&url, context)?;
        // The entry's own failure is an uncaught exception, not an unhandled rejection.
        let failed = function(context, "", 1, |_, args, cx| {
            let error = JsError::from_opaque(crate::native::arg(args, 0));
            crate::process::uncaught(cx, &error);
            Ok(JsValue::undefined())
        });
        let watched = promise.then(None, JsFunction::from_object(failed), context)?;
        let _ = watched;
        Ok(Some((promise, url.to_string())))
    } else {
        crate::process::set_main(context, entry.to_path_buf());
        crate::modules::run_module(context, entry, &source, None)?;
        Ok(None)
    }
}

/// `node -e`: the source as a script in the global scope, with `require` (relative to the
/// working directory), `module`, `exports`, `__filename` (`[eval]`) and `__dirname` (`.`).
fn eval(context: &mut Context, source: &str) -> JsResult<()> {
    let cwd = PathBuf::from(crate::shared(context).cwd.borrow().clone());
    let module = object(context);
    let exports = object(context);
    module.set(js_string!("exports"), exports.clone(), false, context)?;
    module.set(js_string!("id"), js_text("[eval]"), false, context)?;
    module.set(js_string!("filename"), js_text("[eval]"), false, context)?;
    module.set(js_string!("loaded"), false, false, context)?;
    let require = crate::modules::require_function(context, &cwd, None)?;
    let global = context.global_object();
    for (name, value) in [
        ("module", JsValue::from(module)),
        ("exports", exports.into()),
        ("require", require),
        ("__filename", js_text("[eval]")),
        ("__dirname", js_text(".")),
    ] {
        global.set(js_string!(name), value, false, context)?;
    }
    context.eval(Source::from_bytes(source).with_path(Path::new("[eval]")))?;
    Ok(())
}

/// `--disable-proto`: `Object.prototype.__proto__` throws Node's `ERR_PROTO_ACCESS` when
/// touched, or is not there at all. `{ __proto__: x }` in a literal, which is syntax rather
/// than the accessor, still works, as in Node — and in every `vm` context made later, as Node
/// does it for each V8 context.
fn disable_proto(context: &mut Context, proto: Proto) -> JsResult<()> {
    let script = match proto {
        Proto::Delete => "delete Object.prototype.__proto__;",
        Proto::Throw => {
            "(() => {
                const denied = () => {
                    const error = new Error('Accessing Object.prototype.__proto__ has been ' +
                        'disallowed with --disable-proto=throw');
                    error.code = 'ERR_PROTO_ACCESS';
                    throw error;
                };
                Object.defineProperty(Object.prototype, '__proto__', {
                    get: denied, set: denied, enumerable: false, configurable: true,
                });
            })();"
        }
    };
    context.eval(Source::from_bytes(script))?;
    context.insert_data(crate::vm::DisabledProto(script));
    Ok(())
}

/// Whether anything can still make progress; see the module's exit rule.
fn alive(runtime: &mut Runtime) -> bool {
    let context = runtime.context();
    crate::has_ref_timers(context)
        || crate::process::stdin_keeps_alive(context)
        || crate::worker::alive(context)
        || crate::network::keeps_alive(context)
}

/// The loop: input, I/O, timers and workers heard, then a sleep until the next timer or a
/// wake. Answers the exit code when nothing is left — `None` for `process.exitCode`'s.
fn event_loop(
    runtime: &mut Runtime,
    signal: &Signal,
    main: Option<&(JsPromise, String)>,
) -> Option<i32> {
    loop {
        crate::process::deliver_stdin(runtime.context());
        runtime.run_due();
        runtime.run_workers();
        if alive(runtime) {
            let wait = runtime
                .next_due()
                .map(|due| Duration::from_secs_f64(((due - runtime.now()) / 1000.).max(0.)));
            signal.wait(wait);
            continue;
        }
        // Nothing left: `beforeExit`, whose listeners may start something more.
        let context = runtime.context();
        let code = crate::process::exit_code(context);
        let process = crate::native::global(context, "process").unwrap_or_default();
        if let Err(error) = crate::native::invoke(
            &process,
            "emit",
            &[js_text("beforeExit"), code.into()],
            context,
        ) {
            crate::process::uncaught(context, &error);
        }
        runtime.run_microtasks();
        if alive(runtime) || runtime.next_due().is_some_and(|due| due <= runtime.now()) {
            continue;
        }
        if let Some((promise, url)) = main
            && matches!(
                promise.state(),
                boa_engine::builtins::promise::PromiseState::Pending
            )
        {
            let _ = crate::process::write_fd(
                2,
                format!("Warning: Detected unsettled top-level await at {url}\n").as_bytes(),
            );
            return Some(13);
        }
        return None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn options(args: &[&str]) -> Options {
        match parse(args.iter().map(|a| a.to_string()).collect()) {
            Ok(Command::Run(options)) => *options,
            Ok(Command::Print(text)) => panic!("printed {text}"),
            Err(error) => panic!("{error}"),
        }
    }

    #[test]
    fn an_embedder_s_setup_runs_before_the_script_and_can_end_it() {
        fn embed(runtime: &mut Runtime) -> Result<(), String> {
            let exports = boa_engine::object::ObjectInitializer::new(runtime.context())
                .property(
                    boa_engine::js_string!("answer"),
                    42,
                    boa_engine::property::Attribute::all(),
                )
                .build();
            runtime.define_builtin("embedded", exports.into());
            Ok(())
        }
        let run = |setup: Setup| {
            script(
                options(&["-e", "process.exitCode = require('embedded').answer"]),
                "verve".into(),
                setup,
            )
        };
        assert_eq!(run(embed), 42, "the script sees what setup defined");
        assert_eq!(run(|_| Err("no".into())), 70, "a failed setup ends the run");
    }

    #[test]
    fn jitless_keeps_the_jit_off_whatever_the_environment_says() {
        fn jit_off(runtime: &mut Runtime) -> Result<(), String> {
            if runtime.jit_enabled() {
                Err("the JIT is on".into())
            } else {
                Ok(())
            }
        }
        let run = |args: &[&str]| script(options(args), "verve".into(), jit_off);
        assert_eq!(run(&["--jitless", "-e", "0"]), 0);
        #[cfg(feature = "jit")]
        if std::env::var_os("VERVE_JIT").is_none() {
            assert_eq!(
                run(&["-e", "0"]),
                70,
                "without the flag a jit build compiles"
            );
        }
    }

    #[test]
    fn node_flags_map_one_to_one() {
        let o = options(&[
            "run",
            "--permission",
            "--allow-fs-read=/a,b",
            "--allow-fs-read",
            "/c",
            "--allow-fs-write=/d",
            "x.mjs",
            "--loader",
            "y",
        ]);
        assert_eq!(
            o.read,
            Some(vec![PathBuf::from("/a,b"), PathBuf::from("/c")])
        );
        assert_eq!(o.write, Some(vec![PathBuf::from("/d")]));
        assert_eq!(o.entry, Some(PathBuf::from("x.mjs")));
        assert_eq!(o.args, vec!["--loader", "y"]);
        let o = options(&[
            "--allow-read=/a,/b",
            "--allow-net=example.com:443,[::1]:80",
            "x",
        ]);
        assert_eq!(o.read, Some(vec![PathBuf::from("/a"), PathBuf::from("/b")]));
        let net = o.net.unwrap();
        assert_eq!(net[0].host.as_deref(), Some("example.com"));
        assert_eq!(net[0].port, Some(443));
        assert_eq!(net[1].host.as_deref(), Some("::1"));
    }

    #[test]
    fn code_modes_command_line_parses() {
        let o = options(&[
            "--no-warnings",
            "--permission",
            "--experimental-vm-modules",
            "--no-addons",
            "--disable-proto=throw",
            "--max-old-space-size=192",
            "--stack_size=3072",
            "-e",
            "0",
        ]);
        assert_eq!(o.eval.as_deref(), Some("0"));
        assert_eq!(o.stack_kib, Some(3072));
        assert_eq!(o.disable_proto, Some(Proto::Throw));
        assert!(o.no_warnings);
        let p = permissions(&o, None);
        assert_eq!(p.fs_read, FsAccess::Only(Vec::new()));
        assert!(!p.worker);
    }

    #[test]
    fn unknown_flags_and_missing_scripts_are_errors() {
        assert!(parse(vec!["--frobnicate".into(), "x.js".into()]).is_err());
        assert!(parse(vec!["run".into()]).is_err());
        assert!(parse(vec!["--allow-child-process".into(), "x.js".into()]).is_err());
    }
}

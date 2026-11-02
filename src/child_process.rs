//! Node's `child_process`: programs started from a realm that was granted that, under the
//! permission model's `child` bit; a realm without it sees the classes and nothing that starts
//! (see `denied`).
//!
//! `spawn` starts the program on the calling thread, as Node does, so `pid` is there at once and
//! a program that cannot be found is told of synchronously and reported a turn later. What
//! happens after — the pipes and the program's end — is the network reactor's: the parent's end
//! of each pipe becomes a `net.Socket` (`child.stdout instanceof net.Socket`) that the reactor
//! feeds as it feeds a TCP one, and the program is tracked there until it ends (`pipes`).
//!
//! Events come in Node's order: `spawn`, the streams' data, `exit`, and `close` once the program
//! has ended and every stream has closed.

mod denied;
mod exec;
mod fork;
mod ipc;
mod launch;
mod pipes;
mod signals;
mod spec;
mod sync;
#[cfg(unix)]
mod unix;
#[cfg(windows)]
mod windows;

use std::io;

use boa_engine::native_function::NativeFunctionPointer;
use boa_engine::{Context, JsObject, JsResult, JsString, JsValue, js_string};

use crate::native::{
    ClassSpec, arg, array, class, closure, coded_error_value, exports, function, get, global,
    hidden, invoke, js_text, object,
};
use crate::network::reactor::{Command, Event, Id};
use crate::network::{self, tcp};
use crate::sockets::{emitter_class, emitter_init, make};
use launch::{Mode, Parent};
use spec::{Behavior, Parsed, Serialization, SpawnSpec, Stdio};

pub(crate) use ipc::join_parent;
pub(crate) use launch::{Flow, Io, Process};
pub(crate) use pipes::Pipes;

/// `require('child_process')`.
pub(crate) fn module(context: &mut Context, _: &str) -> JsResult<JsValue> {
    if !allowed(context) {
        return denied::module(context);
    }
    let module = object(context);
    let emitter = emitter_class(context)?;
    let (child, _) = class(
        context,
        ClassSpec {
            name: "ChildProcess",
            parent: Some(&emitter),
            init: init_child,
            methods: CHILD_METHODS,
            ..ClassSpec::default()
        },
    )?;
    module.set(js_string!("ChildProcess"), child, false, context)?;
    for (name, f) in [
        ("spawn", spawn as NativeFunctionPointer),
        ("exec", exec::exec),
        ("execFile", exec::exec_file),
        ("fork", fork::fork),
    ] {
        let f = function(context, name, 3, f);
        module.set(JsString::from(name), f, false, context)?;
    }
    let (exec, exec_file) = (
        get(&module.clone().into(), "exec", context)?,
        get(&module.clone().into(), "execFile", context)?,
    );
    if let (Some(exec), Some(exec_file)) = (exec.as_object(), exec_file.as_object()) {
        exec::install_promisified(&exec, &exec_file, context)?;
    }
    exports(
        context,
        &module,
        &[
            ("execSync", 2, sync::exec_sync),
            ("execFileSync", 3, sync::exec_file_sync),
            ("spawnSync", 3, sync::spawn_sync),
        ],
    )?;
    Ok(module.into())
}

/// Whether the realm was granted programs: the `child` bit of the permission model.
fn allowed(context: &Context) -> bool {
    crate::shared(context)
        .network
        .borrow()
        .permissions
        .child_process
}

fn init_child(this: &JsObject, _: &[JsValue], context: &mut Context) -> JsResult<()> {
    emitter_init(this, &[], context)?;
    for (key, value) in [
        ("connected", JsValue::from(false)),
        ("signalCode", JsValue::null()),
        ("exitCode", JsValue::null()),
        ("killed", JsValue::from(false)),
    ] {
        this.set(JsString::from(key), value, false, context)?;
    }
    Ok(())
}

const CHILD_METHODS: &[(&str, usize, NativeFunctionPointer)] = &[
    ("kill", 1, kill),
    ("ref", 0, |this, _, cx| network::set_ref(this, true, cx)),
    ("unref", 0, |this, _, cx| network::set_ref(this, false, cx)),
    ("send", 4, ipc::send),
    ("disconnect", 0, ipc::disconnect),
];

// ─── spawn ───────────────────────────────────────────────────────────────────────────────────

/// `spawn(file, args?, options?)`.
fn spawn(this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    if !allowed(context) {
        return denied::refused(this, args, context);
    }
    let parsed = spec::parse(&arg(args, 0), &arg(args, 1), &arg(args, 2), context)?;
    Ok(start(parsed, context)?.into())
}

/// Starts the program and answers the `ChildProcess` that stands for it, started or not.
fn start(parsed: Parsed, context: &mut Context) -> JsResult<JsObject> {
    let Parsed { spec, behavior } = parsed;
    let child = make(context, "child_process", "ChildProcess", &[])?;
    describe(&child, &spec, context)?;
    let id = tcp::attach(&child, context)?;
    hidden(&child, js_string!("__child"), true, context)?;
    match launch::spawn(&spec, Mode::Reactor) {
        Ok(started) => {
            track(&child, id, started, &spec, behavior.serialization, context)?;
            watch_limits(&child, &behavior, context)?;
            let spawned = closure(
                context,
                "",
                0,
                |_, _, child: &JsObject, cx| {
                    invoke(&child.clone().into(), "emit", &[js_text("spawn")], cx)
                },
                child.clone(),
            );
            crate::microtask(context, spawned.into(), Vec::new());
        }
        Err(error) => {
            let (code, errno) = launch::classify(&error);
            // Only these are reported a turn later; any other failure is thrown, as in Node.
            if matches!(code, "ENOENT" | "EACCES" | "EAGAIN" | "EMFILE" | "ENFILE") {
                fail(&child, id, &error, &spec, context)?;
            } else {
                network::send(context, Command::Close(id));
                let syscall = format!("spawn {}", spec.file);
                let failure = system_error(context, &syscall, code, errno, &spec)?;
                return Err(boa_engine::JsError::from_opaque(failure));
            }
        }
    }
    Ok(child)
}

/// `spawnfile` and `spawnargs`, which are there whether or not the program started.
fn describe(child: &JsObject, spec: &SpawnSpec, context: &mut Context) -> JsResult<()> {
    child.set(js_string!("spawnfile"), js_text(&spec.file), false, context)?;
    let args = spec.args.iter().map(|a| js_text(a)).collect();
    let args = array(context, args);
    child.set(js_string!("spawnargs"), args, false, context)?;
    Ok(())
}

/// The program is running: its pipes become sockets, and the reactor tracks it.
fn track(
    child: &JsObject,
    id: Id,
    started: launch::Started,
    spec: &SpawnSpec,
    serialization: Serialization,
    context: &mut Context,
) -> JsResult<()> {
    let launch::Started { mut process, pipes } = started;
    if let Some(sender) = network::sender(context)
        && let Err(error) = process.watch(sender)
    {
        // A program nobody will hear the end of is not left running.
        let _ = process.kill(signals::SIGKILL);
        return Err(crate::native::type_error(format!("spawn: {error}")));
    }
    child.set(js_string!("pid"), process.pid(), false, context)?;
    network::send(context, Command::Track(id, process));
    hidden(child, js_string!("__open"), 0, context)?;
    let mut streams = vec![JsValue::null(); spec.stdio.len()];
    for (index, parent) in pipes {
        let Parent::Reactor(io) = parent else {
            continue;
        };
        let flow = Flow::of(spec.stdio[index], index);
        streams[index] = pipe_socket(child, io, flow, context)?.into();
    }
    // The channel is not a stream the caller reads; it is `child.send` and `'message'`.
    if let Some(index) = spec.stdio.iter().position(|s| *s == Stdio::Ipc)
        && let Some(socket) = streams[index].as_object()
    {
        ipc::attach(child, &socket, serialization, context)?;
        streams[index] = JsValue::null();
    }
    for (name, index) in [("stdin", 0), ("stdout", 1), ("stderr", 2)] {
        child.set(JsString::from(name), streams[index].clone(), false, context)?;
    }
    let stdio = array(context, streams);
    child.set(js_string!("stdio"), stdio, false, context)?;
    Ok(())
}

/// The parent's end of a pipe as a `net.Socket` that reads, writes or both, and that the child
/// waits for before it closes.
fn pipe_socket(child: &JsObject, io: Io, flow: Flow, context: &mut Context) -> JsResult<JsObject> {
    let options = object(context);
    // A two-way pipe ends when either side does; the one-way ones finish the idle half themselves.
    options.set(
        js_string!("allowHalfOpen"),
        flow != Flow::Both,
        false,
        context,
    )?;
    let socket = make(context, "net", "Socket", &[options.into()])?;
    let id = tcp::attach(&socket, context)?;
    network::send(context, Command::Pipe(id, io, flow));
    socket.set(js_string!("pending"), false, false, context)?;
    let this: JsValue = socket.clone().into();
    // The half that carries nothing is finished at once, so the socket closes with the pipe.
    match flow {
        Flow::ToChild => {
            invoke(&this, "push", &[JsValue::null()], context)?;
            invoke(&this, "resume", &[], context)?;
        }
        Flow::FromChild => {
            invoke(&this, "end", &[], context)?;
        }
        Flow::Both => {}
    }
    count_open(child, 1, context)?;
    let closed = closure(
        context,
        "",
        0,
        |_, _, child: &JsObject, cx| {
            count_open(child, -1, cx)?;
            maybe_close(child, cx)?;
            Ok(JsValue::undefined())
        },
        child.clone(),
    );
    invoke(&this, "once", &[js_text("close"), closed.into()], context)?;
    Ok(socket)
}

fn count_open(child: &JsObject, by: i32, context: &mut Context) -> JsResult<()> {
    let open = get(&child.clone().into(), "__open", context)?
        .as_number()
        .unwrap_or(0.);
    hidden(child, js_string!("__open"), open + f64::from(by), context)
}

/// The program could not be started: `error` then `close`, a turn from now, as Node reports it.
fn fail(
    child: &JsObject,
    id: Id,
    error: &io::Error,
    spec: &SpawnSpec,
    context: &mut Context,
) -> JsResult<()> {
    let (code, errno) = launch::classify(error);
    let failure = system_error(context, &format!("spawn {}", spec.file), code, errno, spec)?;
    network::send(context, Command::Close(id));
    // The streams exist, as Node's do, and are closed.
    let mut streams = Vec::new();
    for name in ["stdin", "stdout", "stderr"] {
        let socket = make(context, "net", "Socket", &[])?;
        invoke(&socket.clone().into(), "destroy", &[], context)?;
        child.set(JsString::from(name), socket.clone(), false, context)?;
        streams.push(socket.into());
    }
    let stdio = array(context, streams);
    child.set(js_string!("stdio"), stdio, false, context)?;
    let report = closure(
        context,
        "",
        0,
        |_, _, (child, failure, errno): &(JsObject, JsValue, f64), cx| {
            let this: JsValue = child.clone().into();
            child.set(js_string!("exitCode"), *errno, false, cx)?;
            invoke(&this, "emit", &[js_text("error"), failure.clone()], cx)?;
            invoke(
                &this,
                "emit",
                &[js_text("close"), (*errno).into(), JsValue::null()],
                cx,
            )
        },
        (child.clone(), failure, errno as f64),
    );
    crate::microtask(context, report.into(), Vec::new());
    Ok(())
}

// ─── Limits ──────────────────────────────────────────────────────────────────────────────────

/// `timeout` and `signal`: the program is killed when the time is up or the signal aborts.
fn watch_limits(child: &JsObject, behavior: &Behavior, context: &mut Context) -> JsResult<()> {
    if behavior.timeout > 0. {
        let kill_it = closure(
            context,
            "",
            0,
            |_, _, (child, signal): &(JsObject, i32), cx| {
                invoke(&child.clone().into(), "kill", &[(*signal).into()], cx)
            },
            (child.clone(), behavior.kill_signal),
        );
        let set_timeout = global(context, "setTimeout")?;
        let timer = crate::call(
            context,
            &set_timeout,
            &JsValue::undefined(),
            &[kill_it.into(), behavior.timeout.into()],
        )?;
        hidden(child, js_string!("__timer"), timer, context)?;
    }
    if let Some(signal) = &behavior.signal {
        let abort = closure(
            context,
            "",
            0,
            |_, _, (child, signal, kill): &(JsObject, JsObject, i32), cx| {
                abort(child, signal, *kill, cx)
            },
            (child.clone(), signal.clone(), behavior.kill_signal),
        );
        let this: JsValue = signal.clone().into();
        if get(&this, "aborted", context)?.to_boolean() {
            crate::microtask(context, abort.into(), Vec::new());
        } else {
            let once = object(context);
            once.set(js_string!("once"), true, false, context)?;
            invoke(
                &this,
                "addEventListener",
                &[js_text("abort"), abort.into(), once.into()],
                context,
            )?;
        }
    }
    Ok(())
}

/// The signal aborted: the program is killed, and told of with `AbortError`.
fn abort(
    child: &JsObject,
    signal: &JsObject,
    kill_signal: i32,
    context: &mut Context,
) -> JsResult<JsValue> {
    let this: JsValue = child.clone().into();
    let sent = invoke(&this, "kill", &[kill_signal.into()], context)?;
    if sent.to_boolean() {
        let reason = get(&signal.clone().into(), "reason", context)?;
        let error = crate::events::abort_error(context, reason);
        invoke(&this, "emit", &[js_text("error"), error], context)?;
    }
    Ok(JsValue::undefined())
}

// ─── Kill, exit, close ───────────────────────────────────────────────────────────────────────

/// `child.kill(signal = 'SIGTERM')`: whether the signal was sent.
fn kill(this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let given = arg(args, 0);
    let signal = if given.is_undefined() {
        signals::SIGTERM
    } else {
        signals::from_js(&given, context)?
    };
    let Some(child) = this.as_object() else {
        return Ok(false.into());
    };
    let running =
        get(this, "exitCode", context)?.is_null() && get(this, "signalCode", context)?.is_null();
    if !running || !get(this, "pid", context)?.is_number() {
        return Ok(false.into());
    }
    if let Err(error) = launch::check_signal(signal) {
        return Err(kill_error(&error, context)?);
    }
    let id = network::id(this, context)?;
    network::send(context, Command::Kill(id, signal));
    child.set(js_string!("killed"), true, false, context)?;
    Ok(true.into())
}

/// What the reactor reports about a child process.
pub(crate) fn deliver(
    id: Id,
    target: &JsObject,
    event: Event,
    context: &mut Context,
) -> JsResult<()> {
    match event {
        Event::Exited(code, signal) => exited(target, code, signal, context),
        Event::Closed => {
            network::forget(id, context);
            Ok(())
        }
        _ => Ok(()),
    }
}

fn exited(
    child: &JsObject,
    code: Option<i64>,
    signal: Option<i32>,
    context: &mut Context,
) -> JsResult<()> {
    let this: JsValue = child.clone().into();
    let name = signal
        .and_then(signals::name)
        .map_or(JsValue::null(), js_text);
    let code = code.map_or(JsValue::null(), |c| JsValue::from(c as f64));
    child.set(js_string!("exitCode"), code.clone(), false, context)?;
    child.set(js_string!("signalCode"), name.clone(), false, context)?;
    hidden(child, js_string!("__exited"), true, context)?;
    let timer = get(&this, "__timer", context)?;
    if !timer.is_undefined() {
        let clear = global(context, "clearTimeout")?;
        crate::call(context, &clear, &JsValue::undefined(), &[timer])?;
    }
    // As Node's `onexit`: nobody can write to a program that is gone.
    let stdin = get(&this, "stdin", context)?;
    if stdin.is_object() {
        invoke(&stdin, "destroy", &[], context)?;
    }
    invoke(&this, "emit", &[js_text("exit"), code, name], context)?;
    flush_stdio(&this, context)?;
    maybe_close(child, context)
}

/// Streams nobody reads are drained, so they reach their end and the child can close: what
/// Node's `flushStdio` does.
fn flush_stdio(child: &JsValue, context: &mut Context) -> JsResult<()> {
    let stdio = get(child, "stdio", context)?;
    for stream in crate::native::list_of(&stdio, context)? {
        if !stream.is_object() {
            continue;
        }
        let mut listeners = 0.;
        for event in ["data", "readable"] {
            let count = invoke(&stream, "listenerCount", &[js_text(event)], context)?;
            listeners += count.as_number().unwrap_or(0.);
        }
        if listeners == 0. {
            invoke(&stream, "resume", &[], context)?;
        }
    }
    Ok(())
}

/// `close`, once the program has ended and every stream has closed.
fn maybe_close(child: &JsObject, context: &mut Context) -> JsResult<()> {
    let this: JsValue = child.clone().into();
    let ended = get(&this, "__exited", context)?.to_boolean();
    let open = get(&this, "__open", context)?.as_number().unwrap_or(0.);
    if !ended || open > 0. || get(&this, "__closed", context)?.to_boolean() {
        return Ok(());
    }
    hidden(child, js_string!("__closed"), true, context)?;
    let code = get(&this, "exitCode", context)?;
    let signal = get(&this, "signalCode", context)?;
    let id = network::id(&this, context)?;
    invoke(&this, "emit", &[js_text("close"), code, signal], context)?;
    network::send(context, Command::Close(id));
    Ok(())
}

/// Node's error for a system call that failed: `spawn x ENOENT`, with the `errno`, `code`,
/// `syscall`, `path` and `spawnargs` it carries.
fn system_error(
    context: &mut Context,
    syscall: &str,
    code: &str,
    errno: i64,
    spec: &SpawnSpec,
) -> JsResult<JsValue> {
    let failure = coded_error_value(context, &format!("{syscall} {code}"), code);
    if let Some(e) = failure.as_object() {
        e.set(js_string!("errno"), errno as f64, false, context)?;
        e.set(js_string!("syscall"), js_text(syscall), false, context)?;
        e.set(js_string!("path"), js_text(&spec.file), false, context)?;
        let rest = spec.args.iter().skip(1).map(|a| js_text(a)).collect();
        let rest = array(context, rest);
        e.set(js_string!("spawnargs"), rest, false, context)?;
    }
    Ok(failure)
}

/// `process.kill(pid, signal = 'SIGTERM')` for a realm that may start programs, which may also
/// signal them; `None` for one that may not, which keeps its refusal.
pub(crate) fn process_kill(args: &[JsValue], context: &mut Context) -> JsResult<Option<JsValue>> {
    if !allowed(context) {
        return Ok(None);
    }
    let pid = arg(args, 0);
    // Negative: a process group, on systems that have them.
    let number = match pid.as_number() {
        Some(n) if n.fract() == 0. && (f64::from(i32::MIN)..=f64::from(i32::MAX)).contains(&n) => {
            n as i64
        }
        _ => {
            return Err(crate::native::invalid_arg_type(
                context,
                "pid",
                "of type number",
                &pid,
            ));
        }
    };
    let given = arg(args, 1);
    let signal = if given.is_undefined() {
        signals::SIGTERM
    } else {
        signals::from_js(&given, context)?
    };
    match launch::kill_pid(number, signal) {
        Ok(()) => Ok(Some(true.into())),
        Err(error) => Err(kill_error(&error, context)?),
    }
}

/// Node's error for a `kill` that failed: `kill ESRCH`, with its `errno` and `syscall`.
fn kill_error(error: &io::Error, context: &mut Context) -> JsResult<boa_engine::JsError> {
    let (code, errno) = launch::kill_failure(error);
    let failure = coded_error_value(context, &format!("kill {code}"), code);
    if let Some(e) = failure.as_object() {
        e.set(js_string!("errno"), errno as f64, false, context)?;
        e.set(js_string!("syscall"), js_text("kill"), false, context)?;
    }
    Ok(boa_engine::JsError::from_opaque(failure))
}

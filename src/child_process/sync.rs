//! `spawnSync`, `execFileSync` and `execSync`: a program run to its end with the caller
//! blocked, as Node's are. Nothing here goes through the reactor — the caller's thread is the
//! one waiting, and each pipe the program writes to is drained by a thread of its own for the
//! length of the call, so neither pipe can fill while the other is read.

use std::io::{Read, Write};
use std::sync::mpsc::{self, Receiver};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use boa_engine::{Context, JsNativeError, JsResult, JsValue, js_string};

use super::launch::{self, Exit, Mode, Parent};
use super::spec::{self, Parsed, SpawnSpec, Stdio};
use super::{denied, signals};
use crate::native::{arg, array, get, global, invalid_arg_type, invoke, js_text, object, text_of};

/// What is read from one pipe: the bytes kept, and a way to hear that the pipe has ended.
struct Drain {
    kept: Arc<Mutex<Vec<u8>>>,
    ended: Receiver<()>,
}

/// What the thread that waits for a run hears: the program's end, or output past `maxBuffer`.
enum Heard {
    Exited(Option<Exit>),
    TooMuch,
}

/// How a run ended other than by the program's own exit.
#[derive(Clone, Copy, PartialEq)]
enum Cut {
    TimedOut,
    TooMuch,
}

/// What a run came to.
struct Outcome {
    pid: u32,
    exit: Option<Exit>,
    cut: Option<Cut>,
    streams: Vec<Option<Vec<u8>>>,
}

/// `spawnSync(file, args?, options?)`.
pub(super) fn spawn_sync(
    this: &JsValue,
    args: &[JsValue],
    context: &mut Context,
) -> JsResult<JsValue> {
    if !super::allowed(context) {
        return denied::spawn_sync(this, args, context);
    }
    let parsed = spec::parse(&arg(args, 0), &arg(args, 1), &arg(args, 2), context)?;
    let options = options_of(&arg(args, 2), &arg(args, 1), context)?;
    run(parsed, &options, context)
}

/// `execFileSync(file, args?, options?)`.
pub(super) fn exec_file_sync(
    this: &JsValue,
    args: &[JsValue],
    context: &mut Context,
) -> JsResult<JsValue> {
    if !super::allowed(context) {
        return denied::run_sync(this, args, context);
    }
    let parsed = spec::parse(&arg(args, 0), &arg(args, 1), &arg(args, 2), context)?;
    let options = options_of(&arg(args, 2), &arg(args, 1), context)?;
    let command = command_of(&arg(args, 0), &arg(args, 1), context)?;
    let own_stdio = !get(&arg(args, 2), "stdio", context)?.is_null_or_undefined();
    let result = run(parsed, &options, context)?;
    show_stderr(&result, own_stdio, context)?;
    checked(result, &command, context)
}

/// `execSync(command, options?)`: through a shell.
pub(super) fn exec_sync(
    this: &JsValue,
    args: &[JsValue],
    context: &mut Context,
) -> JsResult<JsValue> {
    if !super::allowed(context) {
        return denied::run_sync(this, args, context);
    }
    let options = arg(args, 1);
    let copy = spec::options_copy(&options, context)?;
    if !get(&copy.clone().into(), "shell", context)?.is_string() {
        copy.set(js_string!("shell"), true, false, context)?;
    }
    let own_stdio = !get(&options, "stdio", context)?.is_null_or_undefined();
    let copy: JsValue = copy.into();
    let parsed = spec::parse(&arg(args, 0), &JsValue::undefined(), &copy, context)?;
    let settings = options_of(&copy, &JsValue::undefined(), context)?;
    let command = text_of(&arg(args, 0), context)?;
    let result = run(parsed, &settings, context)?;
    show_stderr(&result, own_stdio, context)?;
    checked(result, &command, context)
}

/// Unless `stdio` says otherwise, what the program wrote to standard error is also ours to show.
fn show_stderr(result: &JsValue, own_stdio: bool, context: &mut Context) -> JsResult<()> {
    let stderr = get(result, "stderr", context)?;
    if !own_stdio && stderr.is_object() {
        let process = get(&global(context, "process")?, "stderr", context)?;
        invoke(&process, "write", &[stderr], context)?;
    }
    Ok(())
}

/// The settings of a synchronous call besides what a spawn takes.
struct Settings {
    input: Option<Vec<u8>>,
    max_buffer: usize,
    encoding: Option<String>,
}

fn options_of(options: &JsValue, second: &JsValue, context: &mut Context) -> JsResult<Settings> {
    // `spawnSync(file, options)` has no argument list.
    let options = if second.is_object() && !second.as_object().is_some_and(|o| o.is_array()) {
        second
    } else {
        options
    };
    let max = get(options, "maxBuffer", context)?;
    let max_buffer = match max.as_number() {
        _ if max.is_null_or_undefined() => 1024 * 1024,
        Some(n) if n >= 0. => n.min(1e15) as usize,
        _ => {
            return Err(invalid_arg_type(
                context,
                "options.maxBuffer",
                "of type number",
                &max,
            ));
        }
    };
    let input = get(options, "input", context)?;
    let input = if input.is_null_or_undefined() {
        None
    } else if let Some(bytes) = crate::encoding::bytes_of(&input, context)? {
        Some(bytes)
    } else if input.is_string() {
        Some(text_of(&input, context)?.into_bytes())
    } else {
        return Err(invalid_arg_type(
            context,
            "options.stdio[0]",
            "of type string or an instance of Buffer, TypedArray, or DataView",
            &input,
        ));
    };
    let encoding = get(options, "encoding", context)?;
    let encoding = encoding
        .is_string()
        .then(|| text_of(&encoding, context))
        .transpose()?;
    Ok(Settings {
        input,
        max_buffer,
        encoding: encoding.filter(|e| e != "buffer"),
    })
}

/// `command` and its arguments, as an error message shows them.
fn command_of(file: &JsValue, list: &JsValue, context: &mut Context) -> JsResult<String> {
    let mut command = text_of(file, context)?;
    for word in crate::native::list_of(list, context)? {
        command.push(' ');
        command.push_str(&text_of(&word, context)?);
    }
    Ok(command)
}

/// A run whose failure is thrown: what `execSync` and `execFileSync` do with the result.
fn checked(result: JsValue, command: &str, context: &mut Context) -> JsResult<JsValue> {
    let stderr = get(&result, "stderr", context)?;
    let failure = get(&result, "error", context)?;
    let status = get(&result, "status", context)?;
    let error = if failure.is_object() {
        failure
    } else if status.as_number() != Some(0.) {
        let mut message = format!("Command failed: {command}");
        if stderr.is_object() && get(&stderr, "length", context)?.as_number() > Some(0.) {
            message.push('\n');
            message.push_str(&text_of(
                &invoke(&stderr, "toString", &[], context)?,
                context,
            )?);
        }
        JsNativeError::error()
            .with_message(message)
            .into_opaque(context)
            .into()
    } else {
        return get(&result, "stdout", context);
    };
    if let (Some(target), Some(source)) = (error.as_object(), result.as_object()) {
        for key in ["status", "signal", "output", "pid", "stdout", "stderr"] {
            let value = source.get(boa_engine::JsString::from(key), context)?;
            target.set(boa_engine::JsString::from(key), value, false, context)?;
        }
    }
    Err(boa_engine::JsError::from_opaque(error))
}

fn run(parsed: Parsed, settings: &Settings, context: &mut Context) -> JsResult<JsValue> {
    let Parsed { spec, behavior } = parsed;
    let file = spec.file.clone();
    let outcome = match start(&spec, settings, &behavior) {
        Ok(outcome) => outcome,
        Err(error) => return failed_to_start(&spec, &error, context),
    };
    let result = object(context);
    let error = match outcome.cut {
        Some(Cut::TimedOut) => Some(("ETIMEDOUT", launch::errno_of("ETIMEDOUT"))),
        Some(Cut::TooMuch) => Some(("ENOBUFS", launch::errno_of("ENOBUFS"))),
        None => None,
    };
    if let Some((code, errno)) = error {
        let failure =
            super::system_error(context, &format!("spawnSync {file}"), code, errno, &spec)?;
        result.set(js_string!("error"), failure, false, context)?;
    }
    let (status, signal) = match outcome.exit {
        Some(Exit { code, signal }) => (
            code.map_or(JsValue::null(), |c| JsValue::from(c as f64)),
            signal
                .and_then(signals::name)
                .map_or(JsValue::null(), js_text),
        ),
        None => (JsValue::null(), JsValue::null()),
    };
    result.set(js_string!("status"), status, false, context)?;
    result.set(js_string!("signal"), signal, false, context)?;
    let mut values = Vec::new();
    for bytes in outcome.streams {
        values.push(match bytes {
            Some(bytes) => decode(bytes, settings.encoding.as_deref(), context)?,
            None => JsValue::null(),
        });
    }
    let output = array(context, values.clone());
    result.set(js_string!("output"), output, false, context)?;
    result.set(js_string!("pid"), outcome.pid, false, context)?;
    for (name, at) in [("stdout", 1), ("stderr", 2)] {
        let value = values.get(at).cloned().unwrap_or_else(JsValue::null);
        result.set(boa_engine::JsString::from(name), value, false, context)?;
    }
    Ok(result.into())
}

/// The result of a program that could not start.
fn failed_to_start(
    spec: &SpawnSpec,
    error: &std::io::Error,
    context: &mut Context,
) -> JsResult<JsValue> {
    let (code, errno) = launch::classify(error);
    let failure = super::system_error(
        context,
        &format!("spawnSync {}", spec.file),
        code,
        errno,
        spec,
    )?;
    let result = object(context);
    result.set(js_string!("error"), failure, false, context)?;
    for key in ["status", "signal", "output", "stdout", "stderr"] {
        result.set(
            boa_engine::JsString::from(key),
            JsValue::null(),
            false,
            context,
        )?;
    }
    result.set(js_string!("pid"), 0, false, context)?;
    Ok(result.into())
}

fn decode(bytes: Vec<u8>, encoding: Option<&str>, context: &mut Context) -> JsResult<JsValue> {
    let buffer = crate::encoding::make_buffer(context, bytes)?;
    match encoding {
        Some(encoding) => invoke(&buffer, "toString", &[js_text(encoding)], context),
        None => Ok(buffer),
    }
}

/// Starts the program, feeds it its input, drains its output, and waits.
fn start(
    spec: &SpawnSpec,
    settings: &Settings,
    behavior: &spec::Behavior,
) -> std::io::Result<Outcome> {
    // Past the standard three there is nothing to read, and no IPC.
    let mut spec = spec.clone();
    for entry in spec.stdio.iter_mut().skip(3) {
        *entry = Stdio::Ignore;
    }
    let launch::Started { mut process, pipes } = launch::spawn(&spec, Mode::Blocking)?;
    let pid = process.pid();
    let (tell, hear) = mpsc::channel();
    let mut drains: Vec<(usize, Drain)> = Vec::new();
    for (index, parent) in pipes {
        let Parent::Blocking(pipe) = parent else {
            continue;
        };
        if index == 0 {
            write_input(pipe, settings.input.clone().unwrap_or_default());
        } else {
            drains.push((index, drain(pipe, settings.max_buffer, &tell)));
        }
    }
    // The program is waited for on a thread of its own, so this one can sleep until the first
    // of its end, too much output, and the timeout, without looking at the clock.
    let waiter = tell.clone();
    std::thread::spawn(move || {
        let _ = waiter.send(Heard::Exited(process.wait(None)));
    });
    let started = Instant::now();
    let timeout = (behavior.timeout > 0.).then(|| Duration::from_millis(behavior.timeout as u64));
    let mut cut = None;
    let mut exit = loop {
        let heard = match timeout {
            Some(t) if cut.is_none() => hear.recv_timeout(t.saturating_sub(started.elapsed())),
            _ => hear
                .recv()
                .map_err(|_| mpsc::RecvTimeoutError::Disconnected),
        };
        match heard {
            Ok(Heard::Exited(exit)) => break exit,
            Ok(Heard::TooMuch) if cut.is_none() => cut = Some(Cut::TooMuch),
            Ok(Heard::TooMuch) => continue,
            Err(mpsc::RecvTimeoutError::Timeout) => cut = Some(Cut::TimedOut),
            Err(mpsc::RecvTimeoutError::Disconnected) => break None,
        }
        // Killed once; then only the end is waited for.
        let _ = launch::kill_pid(i64::from(pid), behavior.kill_signal);
    };
    if cut.is_some() {
        // Windows has no signals to report: the one that was sent is the answer.
        exit = Some(Exit {
            code: None,
            signal: Some(behavior.kill_signal),
        });
    }
    // The pipes are read to their end — which a grandchild holding one open can put off — for as
    // long as the timeout leaves, or a moment after a kill.
    let limit = match (cut, timeout) {
        (Some(_), _) => Some(Duration::from_millis(200)),
        (None, Some(t)) => Some(t.saturating_sub(started.elapsed())),
        (None, None) => None,
    };
    let mut streams: Vec<Option<Vec<u8>>> = vec![None; spec.stdio.len().max(3)];
    for (index, drain) in drains {
        match limit {
            Some(limit) => drop(drain.ended.recv_timeout(limit)),
            None => drop(drain.ended.recv()),
        }
        let kept = drain.kept.lock().map(|k| k.clone()).unwrap_or_default();
        streams[index] = Some(kept);
    }
    if cut.is_none() && hear.try_iter().any(|h| matches!(h, Heard::TooMuch)) {
        cut = Some(Cut::TooMuch);
    }
    if spec.stdio[0] == Stdio::Pipe {
        streams[0] = None;
    }
    Ok(Outcome {
        pid,
        exit,
        cut,
        streams,
    })
}

/// Writes `input` to the program's standard input, then closes it, on a thread of its own: the
/// program may not read it until it has written something.
fn write_input(mut pipe: launch::Blocking, input: Vec<u8>) {
    std::thread::spawn(move || {
        // A program that stops reading early is not an error of ours.
        let _ = pipe.write_all(&input);
    });
}

/// Reads a pipe to its end on a thread of its own, keeping `max` bytes; more sets `too_much`.
fn drain(mut pipe: launch::Blocking, max: usize, tell: &mpsc::Sender<Heard>) -> Drain {
    let kept = Arc::new(Mutex::new(Vec::new()));
    let (done, ended) = mpsc::channel();
    let (sink, tell) = (Arc::clone(&kept), tell.clone());
    std::thread::spawn(move || {
        let mut chunk = vec![0; 64 * 1024];
        while let Ok(n) = pipe.read(&mut chunk) {
            if n == 0 {
                break;
            }
            if let Ok(mut kept) = sink.lock() {
                let room = max.saturating_sub(kept.len());
                kept.extend_from_slice(&chunk[..n.min(room)]);
                if n > room {
                    let _ = tell.send(Heard::TooMuch);
                }
            }
        }
        let _ = done.send(());
    });
    Drain { kept, ended }
}

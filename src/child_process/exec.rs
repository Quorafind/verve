//! `exec` and `execFile`: a program run to its end, its output collected and handed to a callback
//! — Node's `lib/child_process.js`. `exec` is `execFile` through a shell. Both answer the
//! `ChildProcess`, and both have a `util.promisify.custom` that resolves `{ stdout, stderr }`
//! and, on failure, rejects with an error that carries them.

use std::cell::RefCell;
use std::rc::Rc;

use boa_engine::builtins::promise::ResolvingFunctions;
use boa_engine::object::builtins::JsPromise;
use boa_engine::{Context, JsResult, JsValue, js_string};
use boa_gc::{Finalize, Trace};

use super::spec;
use crate::native::{
    arg, closure, function, get, invalid_arg_type, invoke, js_text, list_of, object, text_of,
};

/// What the output of the program has come to so far.
struct Capture {
    out: Vec<u8>,
    err: Vec<u8>,
    max_buffer: usize,
    kill_signal: i32,
    /// The stream whose output went past `max_buffer`.
    exceeded: Option<&'static str>,
    done: bool,
    encoding: String,
    command: String,
}

/// What the handlers of one run share.
#[derive(Clone, Trace, Finalize)]
struct Run {
    child: boa_engine::JsObject,
    callback: JsValue,
    #[unsafe_ignore_trace]
    capture: Rc<RefCell<Capture>>,
}

/// `exec(command, options?, callback?)`.
pub(super) fn exec(this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    if !super::allowed(context) {
        return super::denied::refused(this, args, context);
    }
    let (callback, args) = split_callback(args);
    run(
        &arg(&args, 0),
        &JsValue::undefined(),
        &arg(&args, 1),
        callback,
        true,
        context,
    )
}

/// `execFile(file, args?, options?, callback?)`.
pub(super) fn exec_file(
    this: &JsValue,
    args: &[JsValue],
    context: &mut Context,
) -> JsResult<JsValue> {
    if !super::allowed(context) {
        return super::denied::refused(this, args, context);
    }
    let (callback, args) = split_callback(args);
    let second_is_args = arg(&args, 1).as_object().is_some_and(|o| o.is_array());
    let (list, options) = if second_is_args {
        (arg(&args, 1), arg(&args, 2))
    } else {
        (JsValue::undefined(), arg(&args, 1))
    };
    run(&arg(&args, 0), &list, &options, callback, false, context)
}

fn split_callback(args: &[JsValue]) -> (JsValue, Vec<JsValue>) {
    match args.last() {
        Some(last) if last.is_callable() => (last.clone(), args[..args.len() - 1].to_vec()),
        _ => (JsValue::undefined(), args.to_vec()),
    }
}

fn run(
    file: &JsValue,
    list: &JsValue,
    options: &JsValue,
    callback: JsValue,
    through_shell: bool,
    context: &mut Context,
) -> JsResult<JsValue> {
    let options = with_defaults(options, through_shell, context)?;
    let capture = capture_of(file, list, &options, context)?;
    let parsed = spec::parse(file, list, &options, context)?;
    let child = super::start(parsed, context)?;
    let run = Run {
        child: child.clone(),
        callback,
        capture: Rc::new(RefCell::new(capture)),
    };
    let this: JsValue = child.clone().into();
    for (name, stderr) in [("stdout", false), ("stderr", true)] {
        let stream = get(&this, name, context)?;
        if stream.is_object() {
            let on_data = closure(context, "", 1, collect, (run.clone(), stderr));
            invoke(&stream, "on", &[js_text("data"), on_data.into()], context)?;
        }
    }
    let on_error = closure(context, "", 1, failed, run.clone());
    invoke(&this, "on", &[js_text("error"), on_error.into()], context)?;
    let on_close = closure(context, "", 2, closed, run);
    invoke(&this, "on", &[js_text("close"), on_close.into()], context)?;
    Ok(this)
}

/// `options` as a copy with what `exec` adds: a shell, unless `execFile`.
fn with_defaults(
    options: &JsValue,
    through_shell: bool,
    context: &mut Context,
) -> JsResult<JsValue> {
    if !options.is_null_or_undefined() && !options.is_object() {
        return Err(invalid_arg_type(
            context,
            "options",
            "of type object",
            options,
        ));
    }
    let copy = spec::options_copy(options, context)?;
    if through_shell && !get(&copy.clone().into(), "shell", context)?.is_string() {
        copy.set(js_string!("shell"), true, false, context)?;
    }
    Ok(copy.into())
}

fn capture_of(
    file: &JsValue,
    list: &JsValue,
    options: &JsValue,
    context: &mut Context,
) -> JsResult<Capture> {
    let max = get(options, "maxBuffer", context)?;
    let max_buffer = match max.as_number() {
        _ if max.is_undefined() => 1024 * 1024,
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
    let signal = get(options, "killSignal", context)?;
    let kill_signal = if signal.is_null_or_undefined() {
        super::signals::SIGTERM
    } else {
        super::signals::from_js(&signal, context)?
    };
    let encoding = get(options, "encoding", context)?;
    let encoding = if encoding.is_string() {
        text_of(&encoding, context)?
    } else {
        "utf8".into()
    };
    let mut command = text_of(file, context)?;
    for word in list_of(list, context)? {
        command.push(' ');
        command.push_str(&text_of(&word, context)?);
    }
    Ok(Capture {
        out: Vec::new(),
        err: Vec::new(),
        max_buffer,
        kill_signal,
        exceeded: None,
        done: false,
        encoding,
        command,
    })
}

/// A chunk of output: kept, up to `maxBuffer`; past it the program is killed.
fn collect(
    _: &JsValue,
    args: &[JsValue],
    (run, stderr): &(Run, bool),
    context: &mut Context,
) -> JsResult<JsValue> {
    let Some(chunk) = crate::encoding::bytes_of(&arg(args, 0), context)? else {
        return Ok(JsValue::undefined());
    };
    let exceeded = {
        let mut capture = run.capture.borrow_mut();
        let room = capture.max_buffer;
        let kept = if *stderr {
            &mut capture.err
        } else {
            &mut capture.out
        };
        let fits = room.saturating_sub(kept.len());
        kept.extend_from_slice(&chunk[..chunk.len().min(fits)]);
        let over = chunk.len() > fits;
        if over && capture.exceeded.is_none() {
            capture.exceeded = Some(if *stderr { "stderr" } else { "stdout" });
        }
        (over, capture.kill_signal)
    };
    if let (true, signal) = exceeded {
        let child: JsValue = run.child.clone().into();
        invoke(&child, "kill", &[signal.into()], context)?;
        for name in ["stdout", "stderr"] {
            let stream = get(&child, name, context)?;
            if stream.is_object() {
                invoke(&stream, "destroy", &[], context)?;
            }
        }
    }
    Ok(JsValue::undefined())
}

/// The program could not start (or the pipe failed): the callback hears it at once.
fn failed(_: &JsValue, args: &[JsValue], run: &Run, context: &mut Context) -> JsResult<JsValue> {
    finish(run, Err(arg(args, 0)), context)?;
    Ok(JsValue::undefined())
}

fn closed(_: &JsValue, args: &[JsValue], run: &Run, context: &mut Context) -> JsResult<JsValue> {
    finish(run, Ok((arg(args, 0), arg(args, 1))), context)?;
    Ok(JsValue::undefined())
}

/// Calls the callback, once: `(null, stdout, stderr)`, or `(error, stdout, stderr)` for a
/// program that failed, was killed or said it failed with a non-zero exit code.
fn finish(
    run: &Run,
    outcome: Result<(JsValue, JsValue), JsValue>,
    context: &mut Context,
) -> JsResult<()> {
    let (exceeded, command, out, err) = {
        let mut capture = run.capture.borrow_mut();
        if std::mem::replace(&mut capture.done, true) {
            return Ok(());
        }
        (
            capture.exceeded,
            capture.command.clone(),
            capture.out.clone(),
            capture.err.clone(),
        )
    };
    let encoding = run.capture.borrow().encoding.clone();
    let stderr_text = String::from_utf8_lossy(&err).into_owned();
    let stdout = decode(out, &encoding, context)?;
    let stderr = decode(err, &encoding, context)?;
    let failure = match (exceeded, outcome) {
        (Some(stream), _) => {
            let message = format!("{stream} maxBuffer length exceeded");
            let error = boa_engine::JsNativeError::range()
                .with_message(message)
                .into_opaque(context);
            let code = js_text("ERR_CHILD_PROCESS_STDIO_MAXBUFFER");
            error.set(js_string!("code"), code, false, context)?;
            Some(error.into())
        }
        (None, Err(error)) => Some(error),
        (None, Ok((code, signal))) if code.as_number() == Some(0.) && signal.is_null() => None,
        (None, Ok((code, signal))) => Some(exit_error(
            run,
            &command,
            &stderr_text,
            code,
            signal,
            context,
        )?),
    };
    if let Some(error) = failure.as_ref().and_then(JsValue::as_object) {
        error.set(js_string!("cmd"), js_text(&command), false, context)?;
    }
    if run.callback.is_callable() {
        let error = failure.unwrap_or_else(JsValue::null);
        crate::call(
            context,
            &run.callback,
            &JsValue::undefined(),
            &[error, stdout, stderr],
        )?;
    }
    Ok(())
}

/// `Command failed: <command>\n<stderr>`, with the code, whether the program was killed, and
/// the signal.
fn exit_error(
    run: &Run,
    command: &str,
    stderr: &str,
    code: JsValue,
    signal: JsValue,
    context: &mut Context,
) -> JsResult<JsValue> {
    let message = format!("Command failed: {command}\n{stderr}");
    let object = boa_engine::JsNativeError::error()
        .with_message(message)
        .into_opaque(context);
    let killed = get(&run.child.clone().into(), "killed", context)?;
    object.set(js_string!("code"), code, false, context)?;
    object.set(js_string!("killed"), killed, false, context)?;
    object.set(js_string!("signal"), signal, false, context)?;
    Ok(object.into())
}

/// Output as the callback gets it: a `Buffer` for `'buffer'`, else a string in `encoding`.
fn decode(bytes: Vec<u8>, encoding: &str, context: &mut Context) -> JsResult<JsValue> {
    let buffer = crate::encoding::make_buffer(context, bytes)?;
    if encoding == "buffer" {
        return Ok(buffer);
    }
    invoke(&buffer, "toString", &[js_text(encoding)], context)
}

// ─── util.promisify.custom ───────────────────────────────────────────────────────────────────

/// The functions `util.promisify` finds on `exec` and `execFile`.
pub(super) fn install_promisified(
    exec: &boa_engine::JsObject,
    exec_file: &boa_engine::JsObject,
    context: &mut Context,
) -> JsResult<()> {
    let key = crate::util::symbol_for(context, "nodejs.util.promisify.custom")?
        .to_property_key(context)?;
    let promised_exec = function(context, "exec", 2, |this, args, cx| {
        promised(this, args, true, cx)
    });
    let promised_file = function(context, "execFile", 3, |this, args, cx| {
        promised(this, args, false, cx)
    });
    exec.set(key.clone(), promised_exec, false, context)?;
    exec_file.set(key, promised_file, false, context)?;
    Ok(())
}

/// A promise of `{ stdout, stderr }`, with the `ChildProcess` on its `child`.
fn promised(
    this: &JsValue,
    args: &[JsValue],
    through_shell: bool,
    context: &mut Context,
) -> JsResult<JsValue> {
    let (promise, resolvers) = JsPromise::new_pending(context);
    let settle = closure(context, "", 3, settled, resolvers);
    let mut all = args.to_vec();
    all.push(settle.into());
    let child = if through_shell {
        exec(this, &all, context)?
    } else {
        exec_file(this, &all, context)?
    };
    let promise: JsValue = promise.into();
    if let Some(p) = promise.as_object() {
        p.set(js_string!("child"), child, false, context)?;
    }
    Ok(promise)
}

fn settled(
    _: &JsValue,
    args: &[JsValue],
    resolvers: &ResolvingFunctions,
    context: &mut Context,
) -> JsResult<JsValue> {
    let (error, stdout, stderr) = (arg(args, 0), arg(args, 1), arg(args, 2));
    if error.is_null_or_undefined() {
        let result = object(context);
        result.set(js_string!("stdout"), stdout, false, context)?;
        result.set(js_string!("stderr"), stderr, false, context)?;
        resolvers
            .resolve
            .call(&JsValue::undefined(), &[result.into()], context)?;
    } else {
        if let Some(e) = error.as_object() {
            e.set(js_string!("stdout"), stdout, false, context)?;
            e.set(js_string!("stderr"), stderr, false, context)?;
        }
        resolvers
            .reject
            .call(&JsValue::undefined(), &[error], context)?;
    }
    Ok(JsValue::undefined())
}

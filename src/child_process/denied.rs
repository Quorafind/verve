//! `child_process` as a realm without the grant sees it: the classes and constants are there,
//! and nothing starts. Under the permission model the refusal is its own, thrown at once
//! (`ERR_ACCESS_DENIED` for `ChildProcess`); outside it a call is told that programs are not
//! available, as it always has been.

use boa_engine::{Context, JsObject, JsResult, JsString, JsValue, js_string};

use crate::native::{arg, array, coded_error_value, exports, function, js_text, object};
use crate::sockets::{
    call_later, define, emitter_class, emitter_init, fail_later_with, last_callback, make,
    returns_this,
};

const NO_PROGRAMS: &str = "child_process is not available in Verve: plugins cannot start programs";

pub(super) fn spawn_error(context: &mut Context, syscall: &str, file: &JsValue) -> JsValue {
    let file = if file.is_string() {
        crate::print(file)
    } else {
        String::new()
    };
    let error = coded_error_value(
        context,
        &format!("{syscall} {file} ENOSYS: {NO_PROGRAMS}"),
        "ENOSYS",
    );
    if let Some(e) = error.as_object() {
        let _ = e.set(
            js_string!("syscall"),
            js_text(format!("{syscall} {file}").trim_end()),
            false,
            context,
        );
        let _ = e.set(js_string!("path"), js_text(&file), false, context);
        let _ = e.set(js_string!("errno"), -38, false, context);
    }
    error
}

fn child_init(this: &JsObject, _: &[JsValue], context: &mut Context) -> JsResult<()> {
    emitter_init(this, &[], context)?;
    let emitter = emitter_class(context)?;
    for stream in ["stdin", "stdout", "stderr"] {
        let made = emitter.construct(&[], None, context)?;
        exports(
            context,
            &made,
            &[
                ("setEncoding", 1, returns_this),
                ("write", 3, |_, _, _| Ok(false.into())),
                ("end", 3, returns_this),
                ("pipe", 1, |_, args, _| Ok(arg(args, 0))),
                ("destroy", 1, returns_this),
                ("resume", 0, returns_this),
                ("pause", 0, returns_this),
            ],
        )?;
        this.set(JsString::from(stream), made, false, context)?;
    }
    let mut streams = Vec::new();
    for stream in ["stdin", "stdout", "stderr"] {
        streams.push(this.get(JsString::from(stream), context)?);
    }
    let stdio = array(context, streams);
    this.set(js_string!("stdio"), stdio, false, context)?;
    this.set(js_string!("pid"), JsValue::undefined(), false, context)?;
    this.set(js_string!("exitCode"), JsValue::null(), false, context)?;
    this.set(js_string!("signalCode"), JsValue::null(), false, context)?;
    this.set(js_string!("killed"), false, false, context)?;
    this.set(js_string!("connected"), false, false, context)?;
    Ok(())
}

/// `spawn`, `exec`, `execFile`, `fork`: a `ChildProcess` that never started — its callback hears
/// why, and it emits `error` and `close`.
pub(super) fn refused(_: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    crate::permission::child_process(context)?;
    let error = spawn_error(context, "spawn", &arg(args, 0));
    // `exec`'s callback is `(error, stdout, stderr)`.
    call_later(
        context,
        last_callback(args),
        vec![error.clone(), js_text(""), js_text("")],
    );
    let child = make(context, "child_process", "ChildProcess", &[])?;
    // As a program that could not be found closes: Node's negative errno as the code, no signal.
    fail_later_with(
        context,
        &child,
        error,
        Some(vec![JsValue::from(-38), JsValue::null()]),
    );
    Ok(child.into())
}

pub(super) fn module(context: &mut Context) -> JsResult<JsValue> {
    let module = object(context);
    let emitter = emitter_class(context)?;
    define(
        context,
        &module,
        "ChildProcess",
        &emitter,
        child_init,
        &[
            ("kill", 1, |_, _, _| Ok(false.into())),
            ("send", 4, |_, args, cx| {
                let error = coded_error_value(cx, "Channel closed", "ERR_IPC_CHANNEL_CLOSED");
                call_later(cx, last_callback(args), vec![error]);
                Ok(false.into())
            }),
            ("disconnect", 0, |_, _, _| Ok(JsValue::undefined())),
            ("ref", 0, returns_this),
            ("unref", 0, returns_this),
        ],
    )?;
    for name in ["exec", "execFile", "spawn", "fork"] {
        let f = function(context, name, 3, refused);
        module.set(JsString::from(name), f, false, context)?;
    }
    exports(
        context,
        &module,
        &[
            ("execSync", 2, |_, args, cx| {
                crate::permission::child_process(cx)?;
                Err(boa_engine::JsError::from_opaque(spawn_error(
                    cx,
                    "spawnSync",
                    &arg(args, 0),
                )))
            }),
            ("execFileSync", 3, |_, args, cx| {
                crate::permission::child_process(cx)?;
                Err(boa_engine::JsError::from_opaque(spawn_error(
                    cx,
                    "spawnSync",
                    &arg(args, 0),
                )))
            }),
            ("spawnSync", 3, |_, args, cx| {
                crate::permission::child_process(cx)?;
                // As Node answers a program that could not start: no output, no status, the error.
                let result = object(cx);
                let error = spawn_error(cx, "spawnSync", &arg(args, 0));
                for key in ["output", "stdout", "stderr", "status", "signal"] {
                    result.set(JsString::from(key), JsValue::null(), false, cx)?;
                }
                result.set(js_string!("pid"), 0, false, cx)?;
                result.set(js_string!("error"), error, false, cx)?;
                Ok(result.into())
            }),
        ],
    )?;
    Ok(module.into())
}

//! `fork(modulePath, args?, options?)`: another copy of the runtime, running a module, with an IPC
//! channel (`ipc`) to it. The child is `execPath execArgv modulePath args`, and finds the
//! channel at the descriptor `NODE_CHANNEL_FD` names. Where `process.versions.electron` is set
//! and the child is this very executable, `ELECTRON_RUN_AS_NODE=1` makes it a Node rather than an
//! Electron app, as Electron's `fork` does.

use boa_engine::{Context, JsResult, JsValue, js_string};

use super::spec;
use crate::native::{
    arg, array, coded_error, get, global, invalid_arg_type, js_text, list_of, text_of,
};

pub(super) fn fork(this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    if !super::allowed(context) {
        return super::denied::refused(this, args, context);
    }
    let module_path = arg(args, 0);
    if !module_path.is_string() {
        return Err(invalid_arg_type(
            context,
            "modulePath",
            "of type string",
            &module_path,
        ));
    }
    let (list, options) = match arg(args, 1) {
        given if given.as_object().is_some_and(|o| o.is_array()) => (given, arg(args, 2)),
        given if given.is_object() => (JsValue::undefined(), given),
        _ => (JsValue::undefined(), arg(args, 2)),
    };
    let options = spec::options_copy(&options, context)?;
    let given: JsValue = options.clone().into();
    let process = global(context, "process")?;
    let exec_path = match get(&given, "execPath", context)? {
        own if own.is_string() => own,
        _ => get(&process, "execPath", context)?,
    };
    let mut argv = match get(&given, "execArgv", context)? {
        own if own.is_object() => list_of(&own, context)?,
        _ => inherited_exec_argv(&process, context)?,
    };
    argv.push(module_path);
    argv.extend(list_of(&list, context)?);
    let channel = stdio_with_channel(&given, context)?;
    options.set(js_string!("stdio"), channel, false, context)?;
    options.set(js_string!("shell"), false, false, context)?;
    let base_env = match get(&given, "env", context)? {
        own if own.is_object() => own,
        _ => get(&process, "env", context)?,
    };
    let env = spec::options_copy(&base_env, context)?;
    let versions = get(&process, "versions", context)?;
    let own_path = get(&process, "execPath", context)?;
    let is_electron = get(&versions, "electron", context)?.is_string();
    if is_electron && text_of(&exec_path, context)? == text_of(&own_path, context)? {
        env.set(
            js_string!("ELECTRON_RUN_AS_NODE"),
            js_text("1"),
            false,
            context,
        )?;
    }
    options.set(js_string!("env"), env, false, context)?;
    let argv = array(context, argv);
    let parsed = spec::parse(&exec_path, &argv, &given, context)?;
    Ok(super::start(parsed, context)?.into())
}

/// This process's own `execArgv`, without the script it was run with: a child that ran `-e <source>`
/// again would fork again, without end. Node takes the pair out for the same reason.
fn inherited_exec_argv(process: &JsValue, context: &mut Context) -> JsResult<Vec<JsValue>> {
    let all = list_of(&get(process, "execArgv", context)?, context)?;
    let eval = get(process, "_eval", context)?;
    if !eval.is_string() {
        return Ok(all);
    }
    let source = crate::print(&eval);
    let mut kept = Vec::new();
    let mut words = all.into_iter().peekable();
    while let Some(word) = words.next() {
        let takes_source = matches!(
            crate::print(&word).as_str(),
            "-e" | "--eval" | "-p" | "--print"
        );
        if takes_source
            && words
                .peek()
                .is_some_and(|next| crate::print(next) == source)
        {
            words.next();
        } else {
            kept.push(word);
        }
    }
    Ok(kept)
}

/// `silent` makes the first three pipes, else they are the parent's own; a `stdio` of the
/// caller's must name the channel as `'ipc'`, or gets one after the three.
fn stdio_with_channel(options: &JsValue, context: &mut Context) -> JsResult<JsValue> {
    let given = get(options, "stdio", context)?;
    let words: Vec<JsValue> = if given.is_string() {
        vec![given.clone(), given.clone(), given, js_text("ipc")]
    } else if given.is_object() {
        let words = list_of(&given, context)?;
        if !words.iter().any(is_ipc) {
            return Err(coded_error(
                context,
                "Forked processes must have an IPC channel, missing value 'ipc' in options.stdio",
                "ERR_CHILD_PROCESS_IPC_REQUIRED",
            ));
        }
        words
    } else if get(options, "silent", context)?.to_boolean() {
        vec![
            js_text("pipe"),
            js_text("pipe"),
            js_text("pipe"),
            js_text("ipc"),
        ]
    } else {
        vec![0.into(), 1.into(), 2.into(), js_text("ipc")]
    };
    Ok(array(context, words))
}

fn is_ipc(word: &JsValue) -> bool {
    word.is_string() && crate::print(word) == "ipc"
}

//! What a program is started with: the arguments of `spawn`, `fork` and the rest settled the way
//! Node's `normalizeSpawnArguments` and `getValidStdio` settle them — the shell expanded, the
//! environment merged, `stdio` made a list — so the platform backends see one plain
//! description and know nothing of JavaScript.

use std::ffi::OsString;
use std::path::PathBuf;

use boa_engine::{Context, JsObject, JsResult, JsValue, js_string};

use crate::native::{
    get, global, invalid_arg_type, invalid_arg_value, invoke, js_text, list_of, text_of,
};

use super::signals;

/// One entry of `stdio`: what the child finds on the descriptor of that index.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Stdio {
    /// A pipe to the parent: `'pipe'` and `'overlapped'`.
    Pipe,
    /// The null device.
    Ignore,
    /// The parent's own descriptor, `fd` (0 to 2).
    Inherit(i32),
    /// Node's message channel; at most one.
    Ipc,
}

/// A program to start, as every backend takes it.
#[derive(Clone, Debug)]
pub(crate) struct SpawnSpec {
    /// The program, before `PATH` is searched; the shell when there is one.
    pub file: String,
    /// `argv`, the first entry included (`argv0`, or the file).
    pub args: Vec<String>,
    pub cwd: PathBuf,
    pub env: Vec<(OsString, OsString)>,
    /// The child's descriptors, by index.
    pub stdio: Vec<Stdio>,
    pub detached: bool,
    pub uid: Option<u32>,
    pub gid: Option<u32>,
    #[cfg(windows)]
    pub windows_hide: bool,
    #[cfg(windows)]
    pub windows_verbatim_arguments: bool,
}

/// How `fork` and `send` encode a message.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Serialization {
    Json,
    Advanced,
}

/// What `spawn` does with a child besides starting it.
pub(crate) struct Behavior {
    /// Milliseconds after which the child is killed; 0 for never.
    pub timeout: f64,
    pub kill_signal: i32,
    pub signal: Option<JsObject>,
    pub serialization: Serialization,
}

/// The settled arguments of one call.
pub(crate) struct Parsed {
    pub spec: SpawnSpec,
    pub behavior: Behavior,
}

/// A copy of the options object, to add to without touching the caller's.
pub(crate) fn options_copy(options: &JsValue, context: &mut Context) -> JsResult<JsObject> {
    let copy = crate::native::object(context);
    let assign = get(&global(context, "Object")?, "assign", context)?;
    crate::call(
        context,
        &assign,
        &JsValue::undefined(),
        &[copy.clone().into(), options.clone()],
    )?;
    Ok(copy)
}

/// `(file, args?, options?)` as `spawn` takes them.
pub(crate) fn parse(
    file: &JsValue,
    args: &JsValue,
    options: &JsValue,
    context: &mut Context,
) -> JsResult<Parsed> {
    if !file.is_string() {
        return Err(invalid_arg_type(context, "file", "of type string", file));
    }
    let file = text_of(file, context)?;
    no_nul(context, "file", &file)?;
    if file.is_empty() {
        return Err(invalid_arg_value(
            context,
            "file",
            &js_text(""),
            "cannot be empty",
        ));
    }
    let (mut args, options) = split_args(args, options, context)?;
    for value in &args {
        no_nul(context, "args", value)?;
    }
    let cwd = cwd_of(&options, context)?;
    let mut file = file;
    let mut verbatim = flag(&options, "windowsVerbatimArguments", context)?;
    if let Some(shell) = shell_of(&options, context)? {
        let command = std::iter::once(file.as_str())
            .chain(args.iter().map(String::as_str))
            .collect::<Vec<_>>()
            .join(" ");
        (file, args, verbatim) = shell_command(&shell, &command, cfg!(windows), verbatim);
    }
    let argv0 = get(&options, "argv0", context)?;
    args.insert(
        0,
        if argv0.is_string() {
            let argv0 = text_of(&argv0, context)?;
            no_nul(context, "options.argv0", &argv0)?;
            argv0
        } else {
            file.clone()
        },
    );
    // Checked everywhere; only Windows has a window to hide and a command line to quote.
    let windows_hide = flag(&options, "windowsHide", context)?;
    let mut env = environment(&options, context)?;
    let stdio = stdio_of(&get(&options, "stdio", context)?, context)?;
    let behavior = behavior_of(&options, context)?;
    channel_variables(&mut env, &stdio, behavior.serialization);
    let spec = SpawnSpec {
        file,
        args,
        cwd,
        env,
        stdio,
        detached: flag(&options, "detached", context)?,
        uid: id_of(&options, "uid", context)?,
        gid: id_of(&options, "gid", context)?,
        #[cfg(windows)]
        windows_hide,
        #[cfg(windows)]
        windows_verbatim_arguments: verbatim,
    };
    let _ = (windows_hide, verbatim);
    Ok(Parsed { spec, behavior })
}

/// A string that goes to the system must have no NUL in it, or the system would read it short.
fn no_nul(context: &mut Context, name: &str, text: &str) -> JsResult<()> {
    if text.contains('\0') {
        return Err(invalid_arg_value(
            context,
            name,
            &js_text(text),
            "must be a string without null bytes",
        ));
    }
    Ok(())
}

/// What tells a child where its IPC channel is: set whenever `stdio` has one, as Node's
/// `spawn` does, not only for `fork`.
fn channel_variables(
    env: &mut Vec<(OsString, OsString)>,
    stdio: &[Stdio],
    serialization: Serialization,
) {
    let Some(index) = stdio.iter().position(|s| *s == Stdio::Ipc) else {
        return;
    };
    let mode = match serialization {
        Serialization::Json => "json",
        Serialization::Advanced => "advanced",
    };
    for (name, value) in [
        ("NODE_CHANNEL_FD", index.to_string()),
        ("NODE_CHANNEL_SERIALIZATION_MODE", mode.to_owned()),
    ] {
        env.retain(|(known, _)| known != name);
        env.push((name.into(), value.into()));
    }
}

/// The argument list and the options object: `spawn(file, options)` is `spawn(file, [], options)`.
fn split_args(
    args: &JsValue,
    options: &JsValue,
    context: &mut Context,
) -> JsResult<(Vec<String>, JsValue)> {
    let (list, options) = if args.is_null_or_undefined() {
        (Vec::new(), options.clone())
    } else if args.as_object().is_some_and(|o| o.is_array()) {
        (list_of(args, context)?, options.clone())
    } else if args.is_object() {
        (Vec::new(), args.clone())
    } else {
        return Err(invalid_arg_type(context, "args", "of type object", args));
    };
    if !options.is_null_or_undefined() && !options.is_object() {
        return Err(invalid_arg_type(
            context,
            "options",
            "of type object",
            &options,
        ));
    }
    let strings = list
        .iter()
        .map(|value| text_of(value, context))
        .collect::<JsResult<_>>()?;
    Ok((strings, options))
}

/// A boolean option; anything else given is Node's `ERR_INVALID_ARG_TYPE`.
fn flag(options: &JsValue, name: &str, context: &mut Context) -> JsResult<bool> {
    let value = get(options, name, context)?;
    if value.is_null_or_undefined() {
        return Ok(false);
    }
    value.as_boolean().ok_or_else(|| {
        invalid_arg_type(
            context,
            &format!("options.{name}"),
            "of type boolean",
            &value,
        )
    })
}

/// `uid` or `gid`: a 32-bit integer.
fn id_of(options: &JsValue, name: &str, context: &mut Context) -> JsResult<Option<u32>> {
    let value = get(options, name, context)?;
    if value.is_null_or_undefined() {
        return Ok(None);
    }
    match value.as_number() {
        Some(n) if n.fract() == 0. && (0. ..=f64::from(u32::MAX)).contains(&n) => {
            Ok(Some(n as u32))
        }
        _ => Err(invalid_arg_type(
            context,
            &format!("options.{name}"),
            "of type number",
            &value,
        )),
    }
}

/// The working folder of the child: `cwd`, a path or a `file:` URL, against the realm's own
/// folder — which is not the process's, so a program started without one gets the realm's.
fn cwd_of(options: &JsValue, context: &mut Context) -> JsResult<PathBuf> {
    let base = PathBuf::from(crate::shared(context).cwd.borrow().clone());
    let value = get(options, "cwd", context)?;
    if value.is_null_or_undefined() {
        return Ok(base);
    }
    let href = get(&value, "href", context)?;
    let text = if value.is_string() {
        text_of(&value, context)?
    } else if href.is_string() {
        let href = text_of(&href, context)?;
        crate::node_url::file_url_to_path(&href, cfg!(windows))
            .map_err(|_| invalid_arg_value(context, "options.cwd", &value, "must be a file: URL"))?
    } else {
        return Err(invalid_arg_type(
            context,
            "options.cwd",
            "of type string or an instance of URL",
            &value,
        ));
    };
    if text.contains('\0') {
        return Err(invalid_arg_value(
            context,
            "options.cwd",
            &value,
            "must be a string without null bytes",
        ));
    }
    Ok(base.join(text))
}

/// `shell`: `true` for the system's, a path for another, `None` for no shell.
fn shell_of(options: &JsValue, context: &mut Context) -> JsResult<Option<String>> {
    let value = get(options, "shell", context)?;
    if value.is_string() {
        let shell = text_of(&value, context)?;
        no_nul(context, "options.shell", &shell)?;
        // Node tests `options.shell` for truth: an empty string is no shell.
        return Ok((!shell.is_empty()).then_some(shell));
    }
    match value.as_boolean() {
        Some(true) => Ok(Some(default_shell(context)?)),
        Some(false) => Ok(None),
        None if value.is_null_or_undefined() => Ok(None),
        None => Err(invalid_arg_type(
            context,
            "options.shell",
            "one of type boolean or string",
            &value,
        )),
    }
}

/// `process.env.ComSpec || 'cmd.exe'` on Windows, `/bin/sh` elsewhere.
fn default_shell(context: &mut Context) -> JsResult<String> {
    if !cfg!(windows) {
        return Ok("/bin/sh".into());
    }
    let process = global(context, "process")?;
    let env = get(&process, "env", context)?;
    let comspec = get(&env, "ComSpec", context)?;
    if comspec.is_string() {
        let comspec = text_of(&comspec, context)?;
        if !comspec.is_empty() {
            return Ok(comspec);
        }
    }
    Ok("cmd.exe".into())
}

/// The program and arguments that run `command` in `shell`, and whether the arguments go to
/// the command line as they are: for `cmd.exe`, `/d /s /c "command"` and verbatim — Node's
/// `normalizeSpawnArguments`.
pub(crate) fn shell_command(
    shell: &str,
    command: &str,
    windows: bool,
    verbatim: bool,
) -> (String, Vec<String>, bool) {
    // Node's `/^(?:.*\\)?cmd(?:\.exe)?$/i`: only a backslash makes a folder.
    let name = shell
        .rsplit('\\')
        .next()
        .unwrap_or(shell)
        .to_ascii_lowercase();
    if windows && (name == "cmd" || name == "cmd.exe") {
        let args = ["/d", "/s", "/c"]
            .iter()
            .map(|a| (*a).to_owned())
            .chain([format!("\"{command}\"")])
            .collect();
        return (shell.to_owned(), args, true);
    }
    (
        shell.to_owned(),
        vec!["-c".into(), command.into()],
        verbatim,
    )
}

/// The environment of the child: `options.env`, or the realm's `process.env`, as pairs. A
/// value of `undefined` leaves the name out; on Windows names are case-insensitive, so of two
/// that differ only in case the first in order is kept.
fn environment(options: &JsValue, context: &mut Context) -> JsResult<Vec<(OsString, OsString)>> {
    let mut source = get(options, "env", context)?;
    if source.is_null_or_undefined() {
        source = get(&global(context, "process")?, "env", context)?;
    } else if !source.is_object() {
        return Err(invalid_arg_type(
            context,
            "options.env",
            "of type object",
            &source,
        ));
    }
    let keys = invoke(
        &global(context, "Object")?,
        "keys",
        std::slice::from_ref(&source),
        context,
    )?;
    let mut pairs: Vec<(String, String)> = Vec::new();
    for key in list_of(&keys, context)? {
        let name = text_of(&key, context)?;
        let value = get(&source, &name, context)?;
        if !value.is_undefined() {
            let value = text_of(&value, context)?;
            no_nul(context, &format!("options.env['{name}']"), &name)?;
            no_nul(context, &format!("options.env['{name}']"), &value)?;
            pairs.push((name, value));
        }
    }
    if cfg!(windows) {
        pairs.sort_by_key(|(name, _)| name.to_uppercase());
        pairs.dedup_by_key(|(name, _)| name.to_uppercase());
    }
    Ok(pairs
        .into_iter()
        .map(|(name, value)| (name.into(), value.into()))
        .collect())
}

/// `stdio`: one word for the first three, or a list; Node's `getValidStdio`.
fn stdio_of(value: &JsValue, context: &mut Context) -> JsResult<Vec<Stdio>> {
    let items = if value.is_null_or_undefined() {
        vec![js_text("pipe"); 3]
    } else if value.is_string() {
        vec![value.clone(); 3]
    } else if value.as_object().is_some_and(|o| o.is_array()) {
        list_of(value, context)?
    } else {
        return Err(invalid_arg_value(
            context,
            "stdio",
            value,
            "must be an array or a string",
        ));
    };
    let mut stdio = Vec::new();
    for (index, item) in items.iter().enumerate() {
        let entry = stdio_entry(item, index, context)?;
        if entry == Stdio::Ipc && stdio.contains(&Stdio::Ipc) {
            return Err(crate::native::coded_error(
                context,
                "Child process can have only one IPC pipe",
                "ERR_IPC_ONE_PIPE",
            ));
        }
        stdio.push(entry);
    }
    while stdio.len() < 3 {
        stdio.push(Stdio::Pipe);
    }
    Ok(stdio)
}

fn stdio_entry(item: &JsValue, index: usize, context: &mut Context) -> JsResult<Stdio> {
    if item.is_null_or_undefined() {
        return Ok(if index < 3 {
            Stdio::Pipe
        } else {
            Stdio::Ignore
        });
    }
    let descriptor = if item.is_object() {
        get(item, "fd", context)?
    } else {
        item.clone()
    };
    if let Some(fd) = descriptor.as_number() {
        return if (0. ..=2.).contains(&fd) && fd.fract() == 0. {
            Ok(Stdio::Inherit(fd as i32))
        } else {
            Err(invalid_arg_value(
                context,
                "stdio",
                item,
                "can only inherit the standard streams",
            ))
        };
    }
    match text_of(item, context)?.as_str() {
        "pipe" | "overlapped" if item.is_string() => Ok(Stdio::Pipe),
        "ignore" if item.is_string() => Ok(Stdio::Ignore),
        "ipc" if item.is_string() => Ok(Stdio::Ipc),
        "inherit" if item.is_string() && index < 3 => Ok(Stdio::Inherit(index as i32)),
        _ => Err(invalid_arg_value(context, "stdio", item, "is invalid")),
    }
}

/// `timeout`, `killSignal`, `signal` and `serialization`.
fn behavior_of(options: &JsValue, context: &mut Context) -> JsResult<Behavior> {
    let timeout = get(options, "timeout", context)?;
    let timeout = match timeout.as_number() {
        _ if timeout.is_null_or_undefined() => 0.,
        Some(n) if n.is_finite() && n >= 0. && n.fract() == 0. => n,
        Some(_) => {
            let message = format!(
                "The value of \"options.timeout\" is out of range. It must be an integer >= 0. Received {}",
                crate::print(&timeout)
            );
            let error = boa_engine::JsNativeError::range()
                .with_message(message)
                .into_opaque(context);
            error.set(
                js_string!("code"),
                js_text("ERR_OUT_OF_RANGE"),
                false,
                context,
            )?;
            return Err(boa_engine::JsError::from_opaque(error.into()));
        }
        None => {
            return Err(invalid_arg_type(
                context,
                "options.timeout",
                "of type number",
                &timeout,
            ));
        }
    };
    let kill = get(options, "killSignal", context)?;
    let kill_signal = if kill.is_null_or_undefined() {
        signals::SIGTERM
    } else {
        signals::from_js(&kill, context)?
    };
    let signal = get(options, "signal", context)?;
    let signal = match signal.as_object() {
        Some(object) => Some(object),
        None if signal.is_null_or_undefined() => None,
        None => {
            return Err(invalid_arg_type(
                context,
                "options.signal",
                "an instance of AbortSignal",
                &signal,
            ));
        }
    };
    let mode = get(options, "serialization", context)?;
    let serialization = if mode.is_null_or_undefined() {
        Serialization::Json
    } else {
        match text_of(&mode, context)?.as_str() {
            "json" => Serialization::Json,
            "advanced" => Serialization::Advanced,
            _ => {
                let reason = "must be one of: 'json', 'advanced'";
                return Err(invalid_arg_value(
                    context,
                    "options.serialization",
                    &mode,
                    reason,
                ));
            }
        }
    };
    Ok(Behavior {
        timeout,
        kill_signal,
        signal,
        serialization,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cmd_gets_node_style_switches_and_verbatim_arguments() {
        let (file, args, verbatim) =
            shell_command("C:\\Windows\\System32\\CMD.EXE", "echo hi", true, false);
        assert_eq!(file, "C:\\Windows\\System32\\CMD.EXE");
        assert_eq!(args, ["/d", "/s", "/c", "\"echo hi\""]);
        assert!(verbatim);
    }

    #[test]
    fn other_shells_and_other_systems_take_dash_c() {
        let (_, args, verbatim) = shell_command("/bin/sh", "ls -l", false, false);
        assert_eq!(args, ["-c", "ls -l"]);
        assert!(!verbatim);
        let (_, args, _) = shell_command("pwsh.exe", "ls", true, false);
        assert_eq!(args, ["-c", "ls"]);
    }
}

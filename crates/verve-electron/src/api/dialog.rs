//! `dialog`: the boxes a person answers. Electron checks the options in JavaScript (its
//! `lib/browser/api/dialog.ts`, whose messages are repeated here) and asks its native side, which
//! is the shell: a dialog is a [`ToShell::Dialog`] and its answer comes back as
//! [`FromShell::DialogResult`] for the promise versions, or on a channel the main process waits
//! on for the `Sync` ones — the one place the main process waits for the shell, and the shell
//! never waits for it, so the pages and the shell go on while the script is stopped
//! (`docs/electron/packages/P02-skeleton-and-contracts.md`, the exceptions to rule 1).

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::sync::mpsc;

use boa_engine::builtins::promise::ResolvingFunctions;
use boa_engine::object::builtins::JsPromise;
use boa_engine::{Context, JsObject, JsResult, JsValue};
use verve::native::{self, arg};

use super::browser_window::window_id_of;
use super::define::{self, Args};
use super::shared;
use crate::ids::WindowId;
use crate::msg::{
    DialogReply, DialogResult, DialogSpec, FileFilter, MessageKind, OpenProps, SaveProps, ToShell,
};

/// Which dialog an answer is for, to be made into the object the script expects.
#[derive(Clone, Copy)]
enum Asked {
    Open,
    Save,
    /// With the button that closing the box answers.
    MessageBox(usize),
}

/// The promise dialogs that have not been answered.
#[derive(Default)]
pub(crate) struct Pending {
    next: RefCell<u64>,
    waiting: RefCell<BTreeMap<u64, (Asked, ResolvingFunctions)>>,
}

impl Pending {
    fn call(&self) -> u64 {
        let mut next = self.next.borrow_mut();
        *next += 1;
        *next
    }
}

/// The `dialog` module.
pub(crate) fn make(context: &mut Context) -> JsResult<JsValue> {
    let module = native::object(context);
    native::exports(
        context,
        &module,
        &[
            ("showOpenDialog", 2, |_, a, cx| open(a, false, cx)),
            ("showOpenDialogSync", 2, |_, a, cx| open(a, true, cx)),
            ("showSaveDialog", 2, |_, a, cx| save(a, false, cx)),
            ("showSaveDialogSync", 2, |_, a, cx| save(a, true, cx)),
            ("showMessageBox", 2, |_, a, cx| message_box(a, false, cx)),
            ("showMessageBoxSync", 2, |_, a, cx| message_box(a, true, cx)),
            ("showErrorBox", 2, show_error_box),
            ("showCertificateTrustDialog", 2, certificate_trust),
        ],
    )?;
    Ok(module.into())
}

/// `showCertificateTrustDialog`: Windows and macOS show the system's certificate box; Verve has
/// no certificate store to show it for.
fn certificate_trust(_: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let (_, options) = split(args, context);
    let options = options
        .as_object()
        .ok_or_else(|| native::type_error("options must be an object"))?;
    let certificate = options.get(boa_engine::js_string!("certificate"), context)?;
    if !certificate.is_object() {
        return Err(native::type_error("certificate must be an object"));
    }
    let message = options.get(boa_engine::js_string!("message"), context)?;
    if !message.is_undefined() && !message.is_string() {
        return Err(native::type_error("message must be a string"));
    }
    define::rejected(context, "dialog.showCertificateTrustDialog")
}

/// `showErrorBox(title, content)`: no answer, and allowed before `ready`.
fn show_error_box(_: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let args = Args::new(args);
    let title = args.string(0, "title", context)?;
    let content = args.string(1, "content", context)?;
    eprintln!(
        "{title}
{content}"
    );
    shared(context).hub.error_box(title, content);
    Ok(JsValue::undefined())
}

fn open(args: &[JsValue], sync: bool, context: &mut Context) -> JsResult<JsValue> {
    ready(context)?;
    let (window, options) = split(args, context);
    let options = options.as_object();
    let text = |name: &str, label: &str, cx: &mut Context| text_field(&options, name, label, cx);
    let properties = names_field(&options, "properties", Some("openFile"), context)?;
    let spec = DialogSpec::Open {
        title: text("title", "Title", context)?,
        button_label: text("buttonLabel", "Button label", context)?,
        default_path: text("defaultPath", "Default path", context)?,
        message: text("message", "Message", context)?,
        filters: filters(&options, context)?,
        properties: open_props(&properties),
    };
    ask(context, window, spec, Asked::Open, sync)
}

fn save(args: &[JsValue], sync: bool, context: &mut Context) -> JsResult<JsValue> {
    ready(context)?;
    let (window, options) = split(args, context);
    let options = options.as_object();
    let text = |name: &str, label: &str, cx: &mut Context| text_field(&options, name, label, cx);
    let properties = names_field(&options, "properties", None, context)?;
    let shows_tag_field = match &options {
        Some(object) => {
            let value = object.get(boa_engine::js_string!("showsTagField"), context)?;
            value.is_undefined() || value.to_boolean()
        }
        None => true,
    };
    let spec = DialogSpec::Save {
        button_label: text("buttonLabel", "Button label", context)?,
        default_path: text("defaultPath", "Default path", context)?,
        title: text("title", "Title", context)?,
        message: text("message", "Message", context)?,
        name_field_label: text("nameFieldLabel", "Name field label", context)?,
        filters: filters(&options, context)?,
        shows_tag_field,
        properties: save_props(&properties),
    };
    ask(context, window, spec, Asked::Save, sync)
}

fn message_box(args: &[JsValue], sync: bool, context: &mut Context) -> JsResult<JsValue> {
    ready(context)?;
    let (window, options) = split(args, context);
    let options = options.as_object();
    let kind = message_kind(&options, context)?;
    let mut buttons = match field(&options, "buttons", context)? {
        value if value.is_undefined() => Vec::new(),
        value if value.as_object().is_some_and(|o| o.is_array()) => {
            let items = native::list_of(&value, context)?;
            items
                .iter()
                .map(|item| native::text_of(item, context))
                .collect::<JsResult<_>>()?
        }
        _ => return Err(native::type_error("Buttons must be an array")),
    };
    if field(&options, "normalizeAccessKeys", context)?.to_boolean() {
        buttons
            .iter_mut()
            .for_each(|button| *button = normalize_access_key(button));
    }
    let title = text_field(&options, "title", "Title", context)?;
    let no_link = match field(&options, "noLink", context)? {
        value if value.is_undefined() => false,
        value => value
            .as_boolean()
            .ok_or_else(|| native::type_error("noLink must be a boolean"))?,
    };
    let message = text_field(&options, "message", "Message", context)?;
    let detail = text_field(&options, "detail", "Detail", context)?;
    let checkbox_label = text_field(&options, "checkboxLabel", "checkboxLabel", context)?;
    let checkbox_checked = field(&options, "checkboxChecked", context)?.to_boolean();
    if checkbox_checked && checkbox_label.is_empty() {
        return Err(define::error(
            "checkboxChecked requires that checkboxLabel also be passed",
        ));
    }
    let default_id = match field(&options, "defaultId", context)? {
        value if value.is_undefined() => -1,
        value => value.to_i32(context)?,
    };
    let cancel_id = cancel_id(&options, default_id, &buttons, context)?;
    let icon = match field(&options, "icon", context)? {
        value if value.is_string() => Some(native::text_of(&value, context)?.into()),
        _ => None,
    };
    let spec = DialogSpec::MessageBox {
        kind,
        buttons,
        default_id,
        cancel_id,
        title,
        message,
        detail,
        checkbox_label,
        checkbox_checked,
        icon,
        no_link,
    };
    ask(context, window, spec, Asked::MessageBox(cancel_id), sync)
}

/// `dialog module can only be used after app is ready`.
fn ready(context: &mut Context) -> JsResult<()> {
    if shared(context).app.is_ready() {
        return Ok(());
    }
    Err(define::error(
        "dialog module can only be used after app is ready",
    ))
}

/// `(window, options)` or `(options)`: the first argument is the window when it is one.
fn split(args: &[JsValue], context: &mut Context) -> (Option<WindowId>, JsValue) {
    let first = arg(args, 0);
    if first.to_boolean() && window_id_of(&first, context).is_none() {
        return (None, first);
    }
    (window_id_of(&first, context), arg(args, 1))
}

fn field(options: &Option<JsObject>, name: &str, context: &mut Context) -> JsResult<JsValue> {
    match options {
        Some(object) => object.get(boa_engine::JsString::from(name), context),
        None => Ok(JsValue::undefined()),
    }
}

/// A string option, `""` when left out; anything else is `TypeError: <label> must be a string`.
fn text_field(
    options: &Option<JsObject>,
    name: &str,
    label: &str,
    context: &mut Context,
) -> JsResult<String> {
    let value = field(options, name, context)?;
    if value.is_undefined() {
        return Ok(String::new());
    }
    match value.as_string() {
        Some(text) => Ok(text.to_std_string_escaped()),
        None => Err(native::type_error(format!("{label} must be a string"))),
    }
}

/// The strings of the `properties` array; `default` when it is left out.
fn names_field(
    options: &Option<JsObject>,
    name: &str,
    default: Option<&str>,
    context: &mut Context,
) -> JsResult<Vec<String>> {
    let value = field(options, name, context)?;
    if value.is_undefined() {
        return Ok(default.map(str::to_owned).into_iter().collect());
    }
    if !value.as_object().is_some_and(|o| o.is_array()) {
        return Err(native::type_error("Properties must be an array"));
    }
    let items = native::list_of(&value, context)?;
    Ok(items
        .iter()
        .filter_map(|item| item.as_string().map(|s| s.to_std_string_escaped()))
        .collect())
}

fn filters(options: &Option<JsObject>, context: &mut Context) -> JsResult<Vec<FileFilter>> {
    let value = field(options, "filters", context)?;
    let mut filters = Vec::new();
    for item in native::list_of(&value, context)? {
        let name = native::get(&item, "name", context)?;
        let extensions = native::get(&item, "extensions", context)?;
        let names = native::list_of(&extensions, context)?;
        filters.push(FileFilter {
            name: name
                .as_string()
                .map_or_else(String::new, |s| s.to_std_string_escaped()),
            extensions: names
                .iter()
                .filter_map(|e| e.as_string().map(|s| s.to_std_string_escaped()))
                .collect(),
        });
    }
    Ok(filters)
}

fn open_props(names: &[String]) -> OpenProps {
    let has = |name: &str| names.iter().any(|n| n == name);
    OpenProps {
        open_file: has("openFile"),
        open_directory: has("openDirectory"),
        multi_selections: has("multiSelections"),
        show_hidden_files: has("showHiddenFiles"),
        create_directory: has("createDirectory"),
        prompt_to_create: has("promptToCreate"),
        no_resolve_aliases: has("noResolveAliases"),
        treat_package_as_directory: has("treatPackageAsDirectory"),
        dont_add_to_recent: has("dontAddToRecent"),
    }
}

fn save_props(names: &[String]) -> SaveProps {
    let has = |name: &str| names.iter().any(|n| n == name);
    SaveProps {
        show_hidden_files: has("showHiddenFiles"),
        create_directory: has("createDirectory"),
        treat_package_as_directory: has("treatPackageAsDirectory"),
        show_overwrite_confirmation: has("showOverwriteConfirmation"),
        dont_add_to_recent: has("dontAddToRecent"),
    }
}

fn message_kind(options: &Option<JsObject>, context: &mut Context) -> JsResult<MessageKind> {
    let value = field(options, "type", context)?;
    if value.is_undefined() {
        return Ok(MessageKind::None);
    }
    match value
        .as_string()
        .map(|s| s.to_std_string_escaped())
        .as_deref()
    {
        Some("none") => Ok(MessageKind::None),
        Some("info") => Ok(MessageKind::Info),
        Some("warning") => Ok(MessageKind::Warning),
        Some("error") => Ok(MessageKind::Error),
        Some("question") => Ok(MessageKind::Question),
        _ => Err(native::type_error("Invalid message box type")),
    }
}

/// The button Escape and closing the box answer: `cancelId`, else the one named Cancel or No, else
/// the first (the second when the first is the default).
fn cancel_id(
    options: &Option<JsObject>,
    default_id: i32,
    buttons: &[String],
    context: &mut Context,
) -> JsResult<usize> {
    let given = field(options, "cancelId", context)?;
    if !given.is_null_or_undefined() {
        return Ok(given.to_i32(context)?.max(0) as usize);
    }
    let named = buttons.iter().position(|button| {
        let lower = button.to_lowercase();
        lower == "cancel" || lower == "no"
    });
    Ok(named.unwrap_or(usize::from(default_id == 0 && buttons.len() > 1)))
}

/// Electron's `normalizeAccessKeys`: macOS has no mnemonics, so `&` goes (`&&` is a literal one);
/// Linux (GTK) writes them with `_`, so a literal `_` is doubled and `&x` becomes `_x`; Windows
/// keeps `&`.
fn normalize_access_key(button: &str) -> String {
    if cfg!(target_os = "macos") {
        button
            .replace("&&", "\u{1}")
            .replace('&', "")
            .replace('\u{1}', "&")
    } else if cfg!(target_os = "linux") {
        button
            .replace('_', "__")
            .replace("&&", "\u{1}")
            .replace('&', "_")
            .replace('\u{1}', "&")
    } else {
        button.to_owned()
    }
}

/// Sends the dialog to the shell and answers as the script's call wants it.
fn ask(
    context: &mut Context,
    parent: Option<WindowId>,
    spec: DialogSpec,
    asked: Asked,
    sync: bool,
) -> JsResult<JsValue> {
    let state = shared(context);
    let call = state.dialogs.call();
    if sync {
        let (answer, wait) = mpsc::sync_channel(1);
        let reply = DialogReply::Sync(answer);
        state.hub.to_shell(ToShell::Dialog {
            call,
            parent,
            spec,
            reply,
        });
        // The script waits as Electron's does; the shell and the pages do not.
        let result = wait.recv().unwrap_or(DialogResult::Closed);
        return sync_value(context, asked, &result);
    }
    let (promise, resolvers) = JsPromise::new_pending(context);
    (state.dialogs.waiting.borrow_mut()).insert(call, (asked, resolvers));
    let reply = DialogReply::Async;
    state.hub.to_shell(ToShell::Dialog {
        call,
        parent,
        spec,
        reply,
    });
    Ok(promise.into())
}

/// What `show*Sync` answers: the paths (`undefined` when cancelled), the path (`''`), or the
/// button's index.
fn sync_value(context: &mut Context, asked: Asked, result: &DialogResult) -> JsResult<JsValue> {
    Ok(match (asked, result) {
        (
            Asked::Open,
            DialogResult::Open {
                canceled: false,
                file_paths,
            },
        ) => paths(context, file_paths),
        (
            Asked::Save,
            DialogResult::Save {
                canceled: false,
                file_path: Some(path),
            },
        ) => native::js_text(&path.to_string_lossy()),
        (Asked::Save, _) => native::js_text(""),
        (Asked::MessageBox(_), DialogResult::MessageBox { response, .. }) => {
            (*response as f64).into()
        }
        (Asked::MessageBox(cancel), _) => (cancel as f64).into(),
        _ => JsValue::undefined(),
    })
}

/// What the promise versions resolve with.
fn async_value(context: &mut Context, asked: Asked, result: &DialogResult) -> JsResult<JsValue> {
    let record = |context: &mut Context, fields: &[(&str, JsValue)]| {
        define::record(context, fields).map(JsValue::from)
    };
    match (asked, result) {
        (
            Asked::Open,
            DialogResult::Open {
                canceled,
                file_paths,
            },
        ) => {
            let list = paths(context, file_paths);
            record(
                context,
                &[("canceled", (*canceled).into()), ("filePaths", list)],
            )
        }
        (
            Asked::Save,
            DialogResult::Save {
                canceled,
                file_path,
            },
        ) => {
            let path = file_path
                .as_ref()
                .map_or_else(String::new, |p| p.to_string_lossy().into_owned());
            record(
                context,
                &[
                    ("canceled", (*canceled).into()),
                    ("filePath", native::js_text(&path)),
                ],
            )
        }
        (
            Asked::MessageBox(_),
            DialogResult::MessageBox {
                response,
                checkbox_checked,
            },
        ) => record(
            context,
            &[
                ("response", (*response as f64).into()),
                ("checkboxChecked", (*checkbox_checked).into()),
            ],
        ),
        (Asked::MessageBox(cancel), _) => record(
            context,
            &[
                ("response", (cancel as f64).into()),
                ("checkboxChecked", false.into()),
            ],
        ),
        (Asked::Open, _) => {
            let none = native::array(context, Vec::new());
            record(context, &[("canceled", true.into()), ("filePaths", none)])
        }
        (Asked::Save, _) => record(
            context,
            &[("canceled", true.into()), ("filePath", native::js_text(""))],
        ),
    }
}

fn paths(context: &mut Context, paths: &[std::path::PathBuf]) -> JsValue {
    let items = paths
        .iter()
        .map(|p| native::js_text(&p.to_string_lossy()))
        .collect();
    native::array(context, items)
}

/// The shell answered dialog `call`: its promise settles. A call nobody waits for — it was
/// answered twice, or by a shell that is not this app's — is ignored.
pub(crate) fn answered(context: &mut Context, call: u64, result: &DialogResult) {
    let state = shared(context);
    let waiting = state.dialogs.waiting.borrow_mut().remove(&call);
    let Some((asked, resolvers)) = waiting else {
        return;
    };
    let outcome = async_value(context, asked, result).and_then(|value| {
        resolvers
            .resolve
            .call(&JsValue::undefined(), &[value], context)
    });
    if let Err(error) = outcome {
        verve::report(context, &error);
    }
}

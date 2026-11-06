//! `globalShortcut`: keys the system hands to the app even while another window has the focus.
//!
//! Electron 39.8.10 (observed on Windows): `register` throws `Error: globalShortcut cannot be used
//! before the app is ready` until `ready`, but only after the arguments were converted: a string
//! that is no accelerator, or a missing function, is a gin `TypeError` (`Error processing argument
//! at index 0, conversion failure from X`, `Insufficient number of arguments.`) even then;
//! `isRegistered` answers `false` before `ready`, `unregister` and `unregisterAll` throw like
//! `register`, and `unregister` of a key never registered is nothing. `register` answers whether
//! the system took the key; `registerAll` answers `true` for no keys at all, and `false` and holds
//! none of its keys when it was refused one. The shell is the one to ask, and `register` waits for its answer for at most 500 ms
//! (`docs/electron/packages/P02-skeleton-and-contracts.md`, the exceptions to rule 1): the one
//! system call is all the shell does before it answers.

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::mpsc;
use std::time::Duration;

use boa_engine::{Context, JsObject, JsResult, JsValue};
use verve::native::{self, arg};

use super::define::{self, Args};
use super::shared;
use crate::accelerator::Accelerator;
use crate::msg::MenuOp;

/// How long `register` waits for the shell.
const PATIENCE: Duration = Duration::from_millis(500);

#[derive(Default)]
struct Registry {
    next: u32,
    keys: Vec<(u32, Accelerator, JsObject)>,
}

fn registry(context: &mut Context) -> Rc<RefCell<Registry>> {
    if let Some(found) = context.get_data::<Rc<RefCell<Registry>>>() {
        return Rc::clone(found);
    }
    let made = Rc::<RefCell<Registry>>::default();
    context.insert_data(Rc::clone(&made));
    made
}

/// The `globalShortcut` module.
///
/// # Errors
///
/// A property could not be defined.
pub(crate) fn make(context: &mut Context) -> JsResult<JsValue> {
    let module = native::object(context);
    native::exports(
        context,
        &module,
        &[
            ("register", 2, register),
            ("registerAll", 2, register_all),
            ("isRegistered", 1, is_registered),
            ("unregister", 1, unregister),
            ("unregisterAll", 0, unregister_all),
        ],
    )?;
    Ok(module.into())
}

/// The accelerator the string argument `at` is: a gin conversion failure for anything else.
fn accelerator(args: &Args<'_>, values: &[JsValue], at: usize) -> JsResult<Accelerator> {
    arg(values, at)
        .as_string()
        .and_then(|text| Accelerator::parse(&text.to_std_string_escaped()).ok())
        .ok_or_else(|| args.conversion_failure(at))
}

/// The array of accelerators argument `at` is: gin fails the whole vector, naming nothing, when
/// one element is no accelerator, and a lone string is named.
fn accelerators(
    args: &Args<'_>,
    values: &[JsValue],
    at: usize,
    context: &mut Context,
) -> JsResult<Vec<Accelerator>> {
    let value = arg(values, at);
    if !value.as_object().is_some_and(|o| o.is_array()) {
        return Err(args.conversion_failure(at));
    }
    let mut keys = Vec::new();
    for text in native::list_of(&value, context)? {
        let parsed = text
            .as_string()
            .and_then(|text| Accelerator::parse(&text.to_std_string_escaped()).ok());
        keys.push(parsed.ok_or_else(|| args.conversion_failure(at))?);
    }
    Ok(keys)
}

/// Electron 39.8.10 converts every argument first and only then checks that the app is ready.
fn ready(context: &mut Context) -> JsResult<()> {
    if shared(context).app.is_ready() {
        return Ok(());
    }
    Err(define::error(
        "globalShortcut cannot be used before the app is ready",
    ))
}

fn register(_: &JsValue, values: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let args = Args::new(values);
    args.require(2)?;
    let key = accelerator(&args, values, 0)?;
    let callback = args.function(1, "callback", context)?;
    ready(context)?;
    Ok(register_key(context, key, &callback).into())
}

/// All or none, as Electron's `RegisterAll`: the first key the system refuses gives back the ones
/// taken before it.
fn register_all(_: &JsValue, values: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let args = Args::new(values);
    args.require(2)?;
    let keys = accelerators(&args, values, 0, context)?;
    let callback = args.function(1, "callback", context)?;
    ready(context)?;
    for (taken, key) in keys.iter().enumerate() {
        if !register_key(context, *key, &callback) {
            for key in &keys[..taken] {
                forget_key(context, *key);
            }
            return Ok(false.into());
        }
    }
    Ok(true.into())
}

/// Asks the shell to take `key`; whether it did. A key this app has registered already is not
/// taken again.
fn register_key(context: &mut Context, key: Accelerator, callback: &JsObject) -> bool {
    let registry = registry(context);
    if registry
        .borrow()
        .keys
        .iter()
        .any(|(_, have, _)| *have == key)
    {
        return false;
    }
    let id = {
        let mut registry = registry.borrow_mut();
        registry.next += 1;
        registry.next
    };
    let (reply, answer) = mpsc::sync_channel(1);
    super::menu_state::send(
        context,
        MenuOp::ShortcutRegister {
            id,
            accelerator: key,
            reply,
        },
    );
    match answer.recv_timeout(PATIENCE) {
        Ok(true) => {
            registry.borrow_mut().keys.push((id, key, callback.clone()));
            true
        }
        Ok(false) => false,
        Err(_) => {
            eprintln!("electron: the shell did not answer globalShortcut.register({key})");
            super::menu_state::send(context, MenuOp::ShortcutUnregister { id });
            false
        }
    }
}

fn is_registered(_: &JsValue, values: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let args = Args::new(values);
    args.require(1)?;
    let key = accelerator(&args, values, 0)?;
    let found = registry(context)
        .borrow()
        .keys
        .iter()
        .any(|(_, have, _)| *have == key);
    Ok(found.into())
}

fn unregister(_: &JsValue, values: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let args = Args::new(values);
    args.require(1)?;
    let key = accelerator(&args, values, 0)?;
    ready(context)?;
    forget_key(context, key);
    Ok(JsValue::undefined())
}

/// Gives `key` back to the system, if this app holds it.
fn forget_key(context: &mut Context, key: Accelerator) {
    let registry = registry(context);
    let found = (registry.borrow().keys.iter()).position(|(_, have, _)| *have == key);
    if let Some(at) = found {
        let (id, _, _) = registry.borrow_mut().keys.remove(at);
        super::menu_state::send(context, MenuOp::ShortcutUnregister { id });
    }
}

fn unregister_all(_: &JsValue, _: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    ready(context)?;
    registry(context).borrow_mut().keys.clear();
    super::menu_state::send(context, MenuOp::ShortcutUnregisterAll);
    Ok(JsValue::undefined())
}

/// The system pressed shortcut `id`: its callback runs.
pub(crate) fn pressed(context: &mut Context, id: u32) {
    let callback = registry(context)
        .borrow()
        .keys
        .iter()
        .find(|(have, _, _)| *have == id)
        .map(|(_, _, callback)| callback.clone());
    if let Some(callback) = callback {
        let called = callback.call(&JsValue::undefined(), &[], context);
        define::report(context, called);
    }
}

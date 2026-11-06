//! `ipcRenderer`, the page's half of Electron's IPC: Electron 39's `lib/renderer/api/ipc-renderer.ts`
//! and the native calls under it.
//!
//! What a page sends is not sent at once: it waits in the page's outbox until the job that sent
//! it has finished, and then the whole outbox goes to the main process as one job, so a page that
//! sends a hundred thousand messages wakes the main process's thread a handful of times, not that
//! many. Order is kept: a `sendSync`, which cannot wait, takes the outbox with it.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

use boa_engine::builtins::promise::ResolvingFunctions;
use boa_engine::object::builtins::JsPromise;
use boa_engine::{Context, JsObject, JsResult, JsValue};
use verve::clone::Cloned;
use verve::events::emitter_init;
use verve::messaging::{adopt_port, release_port};
use verve::native::{self, ClassSpec, NewOnly, arg, js_text};
use verve::{WorldId, current_world};

use super::define::{self, Args};
use crate::hub::Hub;
use crate::ids::ContentsId;
use crate::ipc;
use crate::msg::{FromRenderer, SyncReply, ToRenderer};

/// How often a page blocked in `sendSync` looks up from waiting to see whether it is being stopped.
const LOOK_UP: Duration = Duration::from_millis(50);

/// What the page has to tell the main process, in order: its IPC, its console lines, its
/// lifecycle. One list, so nothing it says overtakes anything it said before.
pub(crate) type Outbox = Rc<RefCell<Vec<FromRenderer>>>;

/// The page's outbox.
pub(crate) fn outbox(context: &Context) -> Outbox {
    Rc::clone(&state(context).outbox)
}

/// What the page's IPC keeps between calls.
pub(crate) struct IpcRenderer {
    hub: Arc<Hub>,
    contents: ContentsId,
    /// The document the page is in: the Hub hears nothing from one that has been replaced.
    generation: u64,
    /// The `ipcRenderer` each world has, made when first asked for: a world's own, so that what
    /// it calls and the promises it gets are its own.
    objects: RefCell<HashMap<WorldId, JsObject>>,
    /// An `invoke` waits for its answer here: how to settle its promise, the channel it asked and
    /// the world it asked from.
    calls: RefCell<HashMap<u64, (ResolvingFunctions, String, WorldId)>>,
    outbox: Outbox,
    /// A function that does nothing: calling it is how a page waiting in `sendSync` finds out it
    /// is being stopped, since the engine asks at every call.
    nothing: JsObject,
}

/// Gives the realm its `ipcRenderer`.
pub(crate) fn install(
    context: &mut Context,
    hub: Arc<Hub>,
    (contents, generation): (ContentsId, u64),
) -> JsResult<()> {
    let state = IpcRenderer {
        hub,
        contents,
        generation,
        objects: RefCell::new(HashMap::new()),
        calls: RefCell::new(HashMap::new()),
        outbox: Rc::new(RefCell::new(Vec::new())),
        nothing: native::function(context, "", 0, |_, _, _| Ok(JsValue::undefined())),
    };
    context.insert_data(Rc::new(state));
    Ok(())
}

fn state(context: &Context) -> Rc<IpcRenderer> {
    context
        .get_data::<Rc<IpcRenderer>>()
        .cloned()
        .expect("a page's electron module is installed before ipcRenderer is used")
}

/// `ipcRenderer`, the object `require('electron')` answers in the world that is running.
pub(crate) fn ipc_renderer(context: &mut Context) -> JsResult<JsObject> {
    let world = current_world(context);
    if let Some(object) = state(context).objects.borrow().get(&world) {
        return Ok(object.clone());
    }
    let emitter = define::emitter(context)?;
    let (class, _) = native::class(
        context,
        ClassSpec {
            name: "IpcRenderer",
            init: emitter_init,
            parent: Some(&emitter),
            methods: &[
                ("send", 1, send),
                ("invoke", 1, call_remote),
                ("sendSync", 1, send_sync),
                ("postMessage", 2, post_message),
                ("sendToHost", 1, |_, _, _| Ok(JsValue::undefined())),
            ],
            new_only: Some(NewOnly::Class),
            ..ClassSpec::default()
        },
    )?;
    let object = class.construct(&[], None, context)?;
    state(context)
        .objects
        .borrow_mut()
        .insert(world, object.clone());
    Ok(object)
}

/// The channel argument of a call, read as Electron's native binding reads it: the channel is
/// the second of the arguments the binding takes (after the "internal" flag) for `send`,
/// `invoke` and `sendSync`, and the first for `postMessage`.
fn channel_of(args: &[JsValue], at: usize, context: &mut Context) -> JsResult<String> {
    let seen = [JsValue::from(false), arg(args, 0)];
    let seen = if at == 0 { &seen[1..] } else { &seen[..] };
    Args::new(seen).string(at, "channel", context)
}

/// The arguments after the channel, packed to travel.
fn rest_of(args: &[JsValue], context: &mut Context) -> JsResult<Vec<Cloned>> {
    ipc::pack(args.get(1..).unwrap_or_default(), context)
}

// ---------- sending ----------

fn send(_: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let channel = channel_of(args, 1, context)?;
    let args = rest_of(args, context)?;
    state(context)
        .outbox
        .borrow_mut()
        .push(FromRenderer::Ipc { channel, args });
    Ok(JsValue::undefined())
}

fn call_remote(_: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let asked =
        channel_of(args, 1, context).and_then(|channel| Ok((channel, rest_of(args, context)?)));
    let (channel, packed) = match asked {
        Ok(asked) => asked,
        Err(error) => return native::settled(Err(error), context),
    };
    let ipc = state(context);
    let call = ipc::next_call();
    let (promise, resolving) = JsPromise::new_pending(context);
    ipc.calls
        .borrow_mut()
        .insert(call, (resolving, channel.clone(), current_world(context)));
    ipc.outbox.borrow_mut().push(FromRenderer::Invoke {
        call,
        channel,
        args: packed,
    });
    Ok(promise.into())
}

/// `sendSync`: sends, and then waits for the main process's answer with the page's thread held,
/// as Electron's page is. The wait is looked up from every so often so a page that is being closed
/// is not kept; an answer that never comes because the event was let go reads as
/// `{ error: 'reply was never sent' }`, as Electron's does.
fn send_sync(_: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let channel = channel_of(args, 1, context)?;
    let packed = rest_of(args, context)?;
    let ipc = state(context);
    let (reply, wait) = SyncReply::channel();
    let mut batch = std::mem::take(&mut *ipc.outbox.borrow_mut());
    batch.push(FromRenderer::SendSync {
        channel,
        args: packed,
        reply,
    });
    ipc.hub
        .from_renderer_batch(ipc.contents, ipc.generation, batch);
    loop {
        match wait.recv_timeout(LOOK_UP) {
            Ok(value) => return value.to_value(context),
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                verve::call(
                    context,
                    &ipc.nothing.clone().into(),
                    &JsValue::undefined(),
                    &[],
                )?;
            }
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                if ipc.hub.exit_code().is_some() {
                    return Err(define::error("Main process exited"));
                }
                return Ok(define::record(context, &[("error", js_text(ipc::NEVER_SENT))])?.into());
            }
        }
    }
}

fn post_message(_: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let channel = channel_of(args, 0, context)?;
    // The message is cloned before any port is taken: a message that cannot be leaves them usable.
    let message = ipc::clone(&arg(args, 1), context)?;
    let transfer = arg(args, 2);
    let mut ends = Vec::new();
    if !transfer.is_undefined() {
        let invalid = || native::type_error("Invalid value for transfer");
        let list = transfer
            .as_object()
            .filter(|o| o.is_array())
            .ok_or_else(invalid)?;
        let ports = native::list_of(&list.into(), context)?;
        // All of them or none: a list with something wrong in it takes no port.
        if !verve::messaging::can_release(&ports) {
            return Err(invalid());
        }
        for port in &ports {
            ends.push(release_port(port, context).map_err(|_| invalid())?);
        }
    }
    state(context).outbox.borrow_mut().push(FromRenderer::Post {
        channel,
        message,
        ports: ends,
    });
    Ok(JsValue::undefined())
}

/// Hands the main process what the page has sent since the last time, as one job. The page's
/// thread does it after every job it runs, and `sendSync` does it with its own message, so
/// what reaches the main process is always in the order it was sent.
pub(crate) fn flush(context: &Context) {
    let ipc = state(context);
    let batch = std::mem::take(&mut *ipc.outbox.borrow_mut());
    if !batch.is_empty() {
        ipc.hub
            .from_renderer_batch(ipc.contents, ipc.generation, batch);
    }
}

// ---------- hearing ----------

/// The page hears what the main process sent.
pub(crate) fn receive(context: &mut Context, message: ToRenderer) {
    let heard = match message {
        ToRenderer::Ipc { channel, args, .. } => emit_everywhere(context, &channel, |cx| {
            Ok((ipc::unpack(&args, cx)?, Vec::new()))
        }),
        ToRenderer::Post {
            channel,
            message,
            ports,
        } => {
            let mut ends = Some(ports);
            // The ports go to the first world that hears the message: a port has one owner.
            emit_everywhere(context, &channel, |cx| {
                let mut ports = Vec::new();
                for end in ends.take().unwrap_or_default() {
                    ports.push(adopt_port(end, cx)?);
                }
                Ok((vec![message.to_value(cx)?], ports))
            })
        }
        ToRenderer::InvokeReply { call, result } => settle(context, call, result),
        _ => Ok(()),
    };
    define::report(context, heard);
}

/// Emits `channel` on the `ipcRenderer` of every world that has one, each in its own world with
/// the arguments and ports `make` makes there: a preload hears what the main process sends, and
/// so does a page with Node.
fn emit_everywhere(
    context: &mut Context,
    channel: &str,
    mut make: impl FnMut(&mut Context) -> JsResult<(Vec<JsValue>, Vec<JsValue>)>,
) -> JsResult<()> {
    let worlds: Vec<(WorldId, JsObject)> = {
        let ipc = state(context);
        let objects = ipc.objects.borrow();
        let mut worlds: Vec<_> = objects.iter().map(|(w, o)| (*w, o.clone())).collect();
        worlds.sort_by_key(|(world, _)| *world);
        worlds
    };
    for (world, object) in worlds {
        if verve::world_realm(context, world).is_none() {
            continue;
        }
        verve::in_world(context, world, |cx| {
            make(cx).and_then(|(args, ports)| emit(cx, &object, channel, args, ports))
        })?;
    }
    Ok(())
}

/// Emits `channel` on `object`, an `ipcRenderer`, with an `IpcRendererEvent` and `args`.
fn emit(
    context: &mut Context,
    object: &JsObject,
    channel: &str,
    args: Vec<JsValue>,
    ports: Vec<JsValue>,
) -> JsResult<()> {
    let ports = native::array(context, ports);
    let event = define::record(
        context,
        &[("sender", object.clone().into()), ("ports", ports)],
    )?;
    let mut all = vec![event.into()];
    all.extend(args);
    define::emit(context, object, channel, &all)
}

/// The answer to an `invoke`: its value, or an `Error` that says the remote method failed and
/// how.
fn settle(context: &mut Context, call: u64, result: Result<Cloned, Cloned>) -> JsResult<()> {
    let waiting = state(context).calls.borrow_mut().remove(&call);
    let Some((resolving, channel, world)) = waiting else {
        return Ok(());
    };
    if verve::world_realm(context, world).is_none() {
        return Ok(());
    }
    verve::in_world(context, world, |cx| {
        settle_in_world(cx, &resolving, &channel, result)
    })
}

/// [`settle`], with the world the call was made in the running one.
fn settle_in_world(
    context: &mut Context,
    resolving: &ResolvingFunctions,
    channel: &str,
    result: Result<Cloned, Cloned>,
) -> JsResult<()> {
    match result {
        Ok(value) => {
            let value = value.to_value(context)?;
            resolving
                .resolve
                .call(&JsValue::undefined(), &[value], context)?;
        }
        Err(failure) => {
            let failure = failure.to_value(context)?;
            let text = native::text_of(&failure, context)?;
            let message = format!("Error invoking remote method '{channel}': {text}");
            let error = define::error(&message).into_opaque(context)?;
            resolving
                .reject
                .call(&JsValue::undefined(), &[error], context)?;
        }
    }
    Ok(())
}

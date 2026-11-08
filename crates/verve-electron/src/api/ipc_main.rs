//! `ipcMain`, and what the main process does with the messages a page sends it: Electron 39's
//! `lib/browser/api/web-contents.ts` and `lib/browser/ipc-main-impl.ts`.
//!
//! `ipcMain`, `webContents.ipc` and `webFrameMain.ipc` are three instances of one `IpcMainImpl`
//! class: an `EventEmitter` that also keeps `invoke` handlers. A message from a page is heard as
//! the events [`emit_chain`] lists, in the order Electron emits them, and an `invoke` is
//! answered by the first handler of the frame, the contents and `ipcMain`, in that order. The
//! handler tables live here, in the realm's state, not on objects a script can see.

use std::cell::{Cell, RefCell};
use std::collections::{BTreeMap, HashMap};
use std::rc::Rc;
use std::sync::Arc;
use std::time::{Duration, Instant};

use boa_engine::gc::{Finalize, Gc, Trace, WeakGc};
use boa_engine::object::builtins::{JsFunction, JsPromise};
use boa_engine::property::PropertyDescriptor;
use boa_engine::{Context, JsObject, JsResult, JsString, JsValue, js_string};
use verve::clone::Cloned;
use verve::events::emitter_init;
use verve::messaging::PortEnd;
use verve::native::{self, ClassSpec, NewOnly, arg, js_text};

use super::contents_ipc::{self, Contents};
use super::message_port_main;
use super::{define, shared, web_frame_main};
use crate::hub::Hub;
use crate::ids::ContentsId;
use crate::ipc::{self, MAIN_FRAME};
use crate::msg::{FromRenderer, SyncReply, ToRenderer};

/// What the main process keeps for IPC between calls.
pub(crate) struct IpcState {
    /// The `ipcMain` singleton.
    ipc_main: JsObject,
    /// The class `ipcMain`, `webContents.ipc` and `webFrameMain.ipc` are instances of.
    class: JsObject,
    /// `WebFrameMain`.
    pub(crate) frame_class: JsObject,
    /// For each `IpcMainImpl` that has handlers, by channel.
    handlers: RefCell<Vec<(JsObject, HashMap<String, Handler>)>>,
    /// The objects of the pages that are open.
    pub(crate) pages: RefCell<BTreeMap<ContentsId, Contents>>,
    /// The reply of the last `sendSync` heard, to see after its listeners ran whether it was
    /// answered.
    last_sync: RefCell<Option<WeakGc<ReplySlot>>>,
    /// The `sendSync` events still without an answer, with how many times each was looked for, and
    /// when the collector next looks for the ones let go.
    unanswered: RefCell<Vec<(WeakGc<ReplySlot>, u8)>>,
    sweep_due: Cell<Option<Instant>>,
}

/// A handler of `invoke`, and whether `handleOnce` made it.
#[derive(Clone)]
struct Handler {
    function: JsObject,
    once: bool,
}

/// The state of the main process's IPC, made the first time something needs it.
pub(crate) fn state(context: &mut Context) -> JsResult<Rc<IpcState>> {
    if let Some(state) = context.get_data::<Rc<IpcState>>() {
        return Ok(Rc::clone(state));
    }
    let emitter = define::emitter(context)?;
    let (class, _) = native::class(
        context,
        ClassSpec {
            name: "IpcMainImpl",
            init: emitter_init,
            parent: Some(&emitter),
            methods: &[
                ("handle", 2, |this, args, cx| {
                    register(this, args, false, cx)
                }),
                ("handleOnce", 2, |this, args, cx| {
                    register(this, args, true, cx)
                }),
                ("removeHandler", 1, remove_handler),
            ],
            new_only: Some(NewOnly::Class),
            ..ClassSpec::default()
        },
    )?;
    let ipc_main = class.construct(&[], None, context)?;
    let made = Rc::new(IpcState {
        ipc_main,
        class,
        frame_class: web_frame_main::make_class(context)?,
        handlers: RefCell::new(Vec::new()),
        pages: RefCell::new(BTreeMap::new()),
        last_sync: RefCell::new(None),
        unanswered: RefCell::new(Vec::new()),
        sweep_due: Cell::new(None),
    });
    context.insert_data(Rc::clone(&made));
    Ok(made)
}

/// `ipcMain`, the singleton `require('electron')` answers.
pub(crate) fn ipc_main(context: &mut Context) -> JsResult<JsObject> {
    Ok(state(context)?.ipc_main.clone())
}

/// A new `IpcMainImpl`, for a page or a frame.
pub(crate) fn new_ipc(context: &mut Context) -> JsResult<JsObject> {
    state(context)?.class.construct(&[], None, context)
}

/// Lets go of the handlers of an `IpcMainImpl` that is gone with its page.
pub(crate) fn forget_handlers(context: &mut Context, owners: &[&JsObject]) {
    if let Ok(state) = state(context) {
        state
            .handlers
            .borrow_mut()
            .retain(|(owner, _)| !owners.iter().any(|gone| JsObject::equals(owner, gone)));
    }
}

/// `require('electron')`'s `ipcMain` and `MessageChannelMain`.
pub(crate) fn add_exports(context: &mut Context, module: &JsObject) -> JsResult<()> {
    module.set(js_string!("ipcMain"), ipc_main(context)?, false, context)?;
    let channel = message_port_main::make_channel_class(context)?;
    module.set(js_string!("MessageChannelMain"), channel, false, context)?;
    Ok(())
}

// ---------- handle, handleOnce, removeHandler ----------

/// Runs `f` on the handler table of the `IpcMainImpl` `this`.
fn with_handlers<T>(
    this: &JsValue,
    context: &mut Context,
    f: impl FnOnce(&mut HashMap<String, Handler>) -> T,
) -> JsResult<T> {
    let object = this.as_object().ok_or_else(define::destroyed)?;
    let state = state(context)?;
    let mut tables = state.handlers.borrow_mut();
    let owned = |(owner, _): &(JsObject, _)| JsObject::equals(owner, &object);
    let at = tables.iter().position(owned).unwrap_or_else(|| {
        tables.push((object.clone(), HashMap::new()));
        tables.len() - 1
    });
    Ok(f(&mut tables[at].1))
}

fn register(
    this: &JsValue,
    args: &[JsValue],
    once: bool,
    context: &mut Context,
) -> JsResult<JsValue> {
    let channel = native::text_of(&arg(args, 0), context)?;
    let Some(function) = arg(args, 1).as_callable() else {
        let given = arg(args, 1).type_of();
        return Err(native::type_error(format!(
            "Expected handler to be a function, but found type '{given}'"
        )));
    };
    let free = with_handlers(this, context, |table| {
        let free = !table.contains_key(&channel);
        if free {
            table.insert(channel.clone(), Handler { function, once });
        }
        free
    })?;
    if !free {
        let message = format!("Attempted to register a second handler for '{channel}'");
        return Err(define::error(&message));
    }
    Ok(JsValue::undefined())
}

fn remove_handler(this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let channel = native::text_of(&arg(args, 0), context)?;
    with_handlers(this, context, |table| table.remove(&channel))?;
    Ok(JsValue::undefined())
}

/// The handler `channel` has, looking in `owners` in order; a `handleOnce` one is taken off as
/// it is found.
fn handler_for(channel: &str, owners: &[&JsObject], context: &mut Context) -> Option<JsObject> {
    owners.iter().find_map(|owner| {
        let took = |table: &mut HashMap<String, Handler>| {
            let handler = table.get(channel)?.clone();
            if handler.once {
                table.remove(channel);
            }
            Some(handler.function)
        };
        with_handlers(&(*owner).clone().into(), context, took)
            .ok()
            .flatten()
    })
}

// ---------- the events ----------

/// The kinds of message the main process hears, which differ in the fields of their event.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Heard {
    Send,
    Invoke,
    Sync,
    Post,
}

/// A fresh event for a message from page `id`: an `IpcMainEvent`, or an `IpcMainInvokeEvent`.
fn make_event(
    kind: Heard,
    page: &Contents,
    id: ContentsId,
    context: &mut Context,
) -> JsResult<JsObject> {
    let event = define::event(context)?;
    let fields = [
        ("type", js_text("frame")),
        ("sender", page.object.clone().into()),
    ];
    let rest: [(&str, JsValue); 3] = [
        ("frameId", MAIN_FRAME.into()),
        ("processId", ipc::process_id(id).into()),
        ("frameTreeNodeId", MAIN_FRAME.into()),
    ];
    for (name, value) in fields {
        event.set(JsString::from(name), value, false, context)?;
    }
    // The frame is `null` once the sender is gone, whenever it is asked.
    let frame = native::closure(
        context,
        "get senderFrame",
        0,
        sender_frame,
        (id, page.frame.clone()),
    );
    let accessor = PropertyDescriptor::builder()
        .get(frame)
        .enumerable(true)
        .configurable(true);
    event.define_property_or_throw(js_string!("senderFrame"), accessor, context)?;
    for (name, value) in rest {
        event.set(JsString::from(name), value, false, context)?;
    }
    if matches!(kind, Heard::Send | Heard::Sync) {
        let reply = native::closure(context, "reply", 1, reply, id);
        event.set(js_string!("reply"), reply, false, context)?;
    }
    Ok(event)
}

fn sender_frame(
    _: &JsValue,
    _: &[JsValue],
    (id, frame): &(ContentsId, JsObject),
    context: &mut Context,
) -> JsResult<JsValue> {
    if contents_ipc::is_open(context, *id) {
        Ok(frame.clone().into())
    } else {
        Ok(JsValue::null())
    }
}

/// `event.reply(channel, ...args)`: a message back to the page the event came from, if it is
/// still there.
fn reply(
    _: &JsValue,
    args: &[JsValue],
    id: &ContentsId,
    context: &mut Context,
) -> JsResult<JsValue> {
    if contents_ipc::is_open(context, *id) {
        contents_ipc::send(context, *id, args)?;
    }
    Ok(JsValue::undefined())
}

// ---------- messages from a page ----------

/// The main process hears `message` from page `id`. A listener that throws has thrown in the
/// main process.
pub(crate) fn receive(context: &mut Context, id: ContentsId, message: FromRenderer) {
    let Ok(Some(page)) = contents_ipc::page(context, id) else {
        return;
    };
    let heard = match message {
        FromRenderer::Ipc { channel, args } => ipc::unpack(&args, context)
            .and_then(|args| send_heard(context, &page, id, Heard::Send, &channel, args, None)),
        FromRenderer::Post {
            channel,
            message,
            ports,
        } => message.to_value(context).and_then(|message| {
            let heard = Heard::Post;
            send_heard(
                context,
                &page,
                id,
                heard,
                &channel,
                vec![message],
                Some(ports),
            )
        }),
        FromRenderer::SendSync {
            channel,
            args,
            reply,
        } => sync_heard(context, &page, id, &channel, &args, reply),
        FromRenderer::Invoke {
            call,
            channel,
            args,
        } => answer(context, &page, id, call, &channel, &args),
        _ => Ok(()),
    };
    define::report(context, heard);
}

/// A message that is only heard: its event, and the channel on the frame's, the contents' and
/// the app's `ipc`.
fn send_heard(
    context: &mut Context,
    page: &Contents,
    id: ContentsId,
    kind: Heard,
    channel: &str,
    args: Vec<JsValue>,
    ports: Option<Vec<PortEnd>>,
) -> JsResult<()> {
    let event = make_event(kind, page, id, context)?;
    if let Some(ports) = ports {
        let list = message_port_main::adopt_all(ports, context)?;
        event.set(js_string!("ports"), list, false, context)?;
    }
    let mut all = vec![event.into()];
    all.extend(args);
    // Electron's `-ipc-ports` handler tells the frame's `ipc`, the contents' and `ipcMain`, but
    // not the contents itself.
    let event_name = (kind != Heard::Post).then_some("ipc-message");
    emit_chain(context, page, event_name, channel, &all)
}

/// Emits a message the way Electron's `-ipc-message` handler does: the contents' own `event_name`
/// event with the channel, when there is one, then the channel itself on the frame's `ipc`, the
/// contents' `ipc` and `ipcMain`. `all` is the event, then the arguments.
fn emit_chain(
    context: &mut Context,
    page: &Contents,
    event_name: Option<&str>,
    channel: &str,
    all: &[JsValue],
) -> JsResult<()> {
    if let Some(event_name) = event_name {
        let mut named = vec![all[0].clone(), js_text(channel)];
        named.extend_from_slice(&all[1..]);
        define::emit(context, &page.object, event_name, &named)?;
    }
    let ipc_main = state(context)?.ipc_main.clone();
    for target in [&page.frame_ipc, &page.ipc, &ipc_main] {
        define::emit(context, target, channel, all)?;
    }
    Ok(())
}

// ---------- sendSync ----------

/// What a `sendSync` is answered with, held by the event's `returnValue` setter: when the
/// event is collected with the reply unsent, the page is told no answer is coming.
#[derive(Trace, Finalize)]
struct ReplySlot {
    #[unsafe_ignore_trace]
    reply: RefCell<Option<SyncReply>>,
}

fn sync_heard(
    context: &mut Context,
    page: &Contents,
    id: ContentsId,
    channel: &str,
    args: &[Cloned],
    reply: SyncReply,
) -> JsResult<()> {
    if !is_heard_sync(context, page, channel)? {
        // Nobody can answer: the page is told so at once, with no event to wait for.
        let text = format!(
            "WebContents #{id} called ipcRenderer.sendSync() with '{channel}' channel without listeners."
        );
        console(context, "warn", &[js_text(&text)])?;
        let none = define::record(context, &[("error", js_text(ipc::NEVER_SENT))])?;
        reply.send(ipc::clone(&none.into(), context)?);
        return Ok(());
    }
    let event = make_event(Heard::Sync, page, id, context)?;
    let slot = Gc::new(ReplySlot {
        reply: RefCell::new(Some(reply)),
    });
    *state(context)?.last_sync.borrow_mut() = Some(WeakGc::new(&slot));
    let setter = native::closure(context, "set returnValue", 1, set_return_value, slot);
    let getter = native::function(context, "get returnValue", 0, |_, _, _| {
        Ok(JsValue::undefined())
    });
    let accessor = PropertyDescriptor::builder()
        .get(getter)
        .set(setter)
        .enumerable(false)
        .configurable(true);
    event.define_property_or_throw(js_string!("returnValue"), accessor, context)?;
    let mut all = vec![event.into()];
    all.extend(ipc::unpack(args, context)?);
    emit_chain(context, page, Some("ipc-message-sync"), channel, &all)
}

fn set_return_value(
    _: &JsValue,
    args: &[JsValue],
    slot: &Gc<ReplySlot>,
    context: &mut Context,
) -> JsResult<JsValue> {
    if slot.reply.borrow().is_none() {
        return Ok(JsValue::undefined());
    }
    let value = ipc::clone(&arg(args, 0), context)?;
    if let Some(reply) = slot.reply.borrow_mut().take() {
        reply.send(value);
    }
    Ok(JsValue::undefined())
}

/// Whether anything listens for a `sendSync` on `channel`: the contents, for its
/// `ipc-message-sync` event, or the channel on the frame, the contents' `ipc` or `ipcMain`.
fn is_heard_sync(context: &mut Context, page: &Contents, channel: &str) -> JsResult<bool> {
    let ipc_main = state(context)?.ipc_main.clone();
    let mut count = listener_count(&page.object, "ipc-message-sync", context)?;
    for target in [&page.frame_ipc, &page.ipc, &ipc_main] {
        count += listener_count(target, channel, context)?;
    }
    Ok(count > 0.)
}

fn listener_count(target: &JsObject, name: &str, context: &mut Context) -> JsResult<f64> {
    let count = native::invoke(
        &target.clone().into(),
        "listenerCount",
        &[js_text(name)],
        context,
    )?;
    Ok(count.as_number().unwrap_or_default())
}

/// How long a `sendSync` event that was not answered waits before the collector looks for it, and how
/// many times it is looked for: an event still there after that is held by a listener, whose job it
/// is to answer.
const SWEEP_EVERY: Duration = Duration::from_millis(100);
const SWEEPS: u8 = 3;

/// After a `sendSync`'s listeners and the microtasks they left have run: if its event is still
/// without an answer it is watched, and the page is told when it is let go — which takes a
/// collection, so one runs at most every 100 ms for all the events watched (a full mark-sweep of
/// the main heap is not something to do per call), and an event survives three of them at most.
/// An event something still holds — a timer that will set it — keeps the page waiting. Answers when
/// the collector next looks, if any event is watched.
pub(crate) fn watch_unanswered(context: &mut Context) -> Option<Instant> {
    let state = state(context).ok()?;
    let heard = state.last_sync.borrow_mut().take();
    if let Some(weak) = heard
        && weak
            .upgrade()
            .is_some_and(|slot| slot.reply.borrow().is_some())
    {
        state.unanswered.borrow_mut().push((weak, 0));
        let soon = Instant::now() + SWEEP_EVERY;
        let due = state.sweep_due.get().map_or(soon, |due| due.min(soon));
        state.sweep_due.set(Some(due));
    }
    state.sweep_due.get()
}

/// Collects if a look is due, and answers when the next one is, if any event is still watched.
pub(crate) fn sweep(context: &mut Context) -> Option<Instant> {
    let state = state(context).ok()?;
    let due = state.sweep_due.get()?;
    if Instant::now() < due {
        return Some(due);
    }
    boa_engine::gc::force_collect();
    let mut watched = state.unanswered.borrow_mut();
    watched.retain_mut(|(weak, tries)| {
        *tries += 1;
        *tries < SWEEPS
            && weak
                .upgrade()
                .is_some_and(|slot| slot.reply.borrow().is_some())
    });
    let next = (!watched.is_empty()).then(|| Instant::now() + SWEEP_EVERY);
    state.sweep_due.set(next);
    next
}

// ---------- invoke ----------

/// The page and call an `invoke` answers, and the channel for the log.
#[derive(Clone, Trace, Finalize)]
struct Pending {
    #[unsafe_ignore_trace]
    hub: Arc<Hub>,
    contents: ContentsId,
    call: u64,
    channel: String,
}

fn answer(
    context: &mut Context,
    page: &Contents,
    id: ContentsId,
    call: u64,
    channel: &str,
    args: &[Cloned],
) -> JsResult<()> {
    let pending = Pending {
        hub: Arc::clone(&shared(context).hub),
        contents: id,
        call,
        channel: channel.to_owned(),
    };
    let ipc_main = state(context)?.ipc_main.clone();
    let owners = [&page.frame_ipc, &page.ipc, &ipc_main];
    let Some(handler) = handler_for(channel, &owners, context) else {
        let error = ipc::error_value(&format!("No handler registered for '{channel}'"), context)?;
        return settle(Err(error), &pending, context);
    };
    let mut all = vec![make_event(Heard::Invoke, page, id, context)?.into()];
    all.extend(ipc::unpack(args, context)?);
    match verve::call(context, &handler.into(), &JsValue::undefined(), &all) {
        Ok(value) => settle_later(value, pending, context),
        Err(error) => {
            let error = error.into_opaque(context)?;
            settle(Err(error), &pending, context)
        }
    }
}

/// Answers the page when the handler's result — a value, or a promise — settles.
fn settle_later(value: JsValue, pending: Pending, context: &mut Context) -> JsResult<()> {
    let promise = JsPromise::resolve(value, context)?;
    let on_ok = native::closure(
        context,
        "",
        1,
        |_, args, pending: &Pending, cx| {
            settle(Ok(arg(args, 0)), pending, cx)?;
            Ok(JsValue::undefined())
        },
        pending.clone(),
    );
    let on_err = native::closure(
        context,
        "",
        1,
        |_, args, pending: &Pending, cx| {
            settle(Err(arg(args, 0)), pending, cx)?;
            Ok(JsValue::undefined())
        },
        pending,
    );
    // The promise `then` makes has no reader: the closures are all that matters.
    let _ = promise.then(
        JsFunction::from_object(on_ok),
        JsFunction::from_object(on_err),
        context,
    );
    Ok(())
}

/// Sends the page the result of its `invoke`: the value cloned, or — for a handler that threw,
/// rejected, or answered what cannot be cloned — the text of the failure, which the main process
/// also logs as Electron does.
fn settle(
    outcome: Result<JsValue, JsValue>,
    pending: &Pending,
    context: &mut Context,
) -> JsResult<()> {
    let outcome = match outcome {
        Ok(value) => match ipc::clone(&value, context) {
            Ok(cloned) => Ok(cloned),
            Err(error) => Err(error.into_opaque(context)?),
        },
        Err(error) => Err(error),
    };
    let result = match outcome {
        Ok(cloned) => Ok(cloned),
        Err(error) => {
            let said = format!("Error occurred in handler for '{}':", pending.channel);
            console(context, "error", &[js_text(&said), error.clone()])?;
            Err(ipc::failure(&ipc::error_text(&error, context), context))
        }
    };
    let call = pending.call;
    let reply = ToRenderer::InvokeReply { call, result };
    pending.hub.to_renderer(pending.contents, reply);
    Ok(())
}

/// `console[level](...args)` in the main process.
pub(crate) fn console(context: &mut Context, level: &str, args: &[JsValue]) -> JsResult<()> {
    let console = native::global(context, "console")?;
    native::invoke(&console, level, args, context)?;
    Ok(())
}

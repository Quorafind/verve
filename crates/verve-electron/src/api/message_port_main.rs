//! `MessageChannelMain` and `MessagePortMain`: Electron's message ports in the main process,
//! which are `EventEmitter`s rather than event targets.
//!
//! A port is one [`PortEnd`] of a pair the kernel makes, so the other end can be given to a page
//! (`webContents.postMessage`) and drives a `MessagePort` there. Messages wait in the pair until
//! the port is started; the main process's beat takes them out ([`pump`]) and emits them, up to a
//! bounded number at a time so one chatty port cannot hold the process up.

use std::cell::RefCell;
use std::rc::Rc;

use boa_engine::gc::{Finalize, Trace};
use boa_engine::{Context, JsData, JsObject, JsResult, JsValue};
use verve::clone::Cloned;
use verve::events::emitter_init;
use verve::messaging::{PortEnd, PortEvent, remote_port_pair};
use verve::native::{self, ClassSpec, NewOnly, arg};

use super::define;
use crate::ipc;

/// The most events one port is heard for in one beat.
const PER_BEAT: usize = 1000;

/// The ports of the main process that are open.
#[derive(Default)]
struct Ports {
    open: RefCell<Vec<JsObject>>,
    prototype: RefCell<Option<JsObject>>,
    waker: RefCell<Option<verve::Waker>>,
}

/// What a `MessagePortMain` is: the kernel's end, and where the port is in its life.
#[derive(Trace, Finalize, JsData)]
struct PortData {
    #[unsafe_ignore_trace]
    state: RefCell<PortState>,
}

#[derive(Default)]
struct PortState {
    /// `None` once the port was transferred: it is "neutered", and every call on it is ignored.
    end: Option<PortEnd>,
    started: bool,
    closed: bool,
    /// Whether the port's own `'close'` is still to be emitted.
    close_pending: bool,
}

/// How a bad entry of a transfer list is reported: `port.postMessage` and
/// `webContents.postMessage` word it differently.
#[derive(Clone, Copy)]
pub(crate) enum Invalid {
    Port,
    Contents,
}

/// Has the main process's beat wake the isolate for ports, with `waker`.
pub(crate) fn set_waker(context: &mut Context, waker: verve::Waker) {
    *ports(context).waker.borrow_mut() = Some(waker);
}

fn wake(context: &mut Context) {
    let waker = ports(context).waker.borrow().clone();
    if let Some(waker) = waker {
        waker();
    }
}

/// The ports of the main process's realm.
fn ports(context: &mut Context) -> Rc<Ports> {
    if let Some(ports) = context.get_data::<Rc<Ports>>() {
        return Rc::clone(ports);
    }
    let made = Rc::new(Ports::default());
    context.insert_data(Rc::clone(&made));
    made
}

/// `MessageChannelMain`: `new MessageChannelMain()` makes `{ port1, port2 }`.
pub(crate) fn make_channel_class(context: &mut Context) -> JsResult<JsObject> {
    let emitter = define::emitter(context)?;
    let (class, _) = native::class(
        context,
        ClassSpec {
            name: "MessageChannelMain",
            init: |this, _, cx| {
                emitter_init(this, &[], cx)?;
                let (one, two) = remote_port_pair();
                for (name, end) in [("port1", one), ("port2", two)] {
                    let port = new_port(cx, end)?;
                    this.set(boa_engine::JsString::from(name), port, false, cx)?;
                }
                Ok(())
            },
            parent: Some(&emitter),
            new_only: Some(NewOnly::Class),
            ..ClassSpec::default()
        },
    )?;
    Ok(class)
}

/// The prototype of `MessagePortMain`, made the first time a port is.
fn prototype(context: &mut Context) -> JsResult<JsObject> {
    let ports = ports(context);
    if let Some(prototype) = ports.prototype.borrow().clone() {
        return Ok(prototype);
    }
    let emitter = define::emitter(context)?;
    let (_, prototype) = native::class(
        context,
        ClassSpec {
            name: "MessagePortMain",
            parent: Some(&emitter),
            methods: &[
                ("postMessage", 1, post_message),
                ("start", 0, start),
                ("close", 0, close),
            ],
            ..ClassSpec::default()
        },
    )?;
    *ports.prototype.borrow_mut() = Some(prototype.clone());
    Ok(prototype)
}

/// A `MessagePortMain` for `end`, heard from the main process's next beat on once it is started.
pub(crate) fn new_port(context: &mut Context, end: PortEnd) -> JsResult<JsObject> {
    let waker = ports(context).waker.borrow().clone();
    if let Some(waker) = waker {
        end.set_waker(waker);
    }
    let data = PortData {
        state: RefCell::new(PortState {
            end: Some(end),
            ..PortState::default()
        }),
    };
    let port = JsObject::from_proto_and_data(prototype(context)?, data);
    emitter_init(&port, &[], context)?;
    ports(context).open.borrow_mut().push(port.clone());
    Ok(port)
}

/// An array of `MessagePortMain`s for the ends that came along with a message.
pub(crate) fn adopt_all(ends: Vec<PortEnd>, context: &mut Context) -> JsResult<JsValue> {
    let mut ports = Vec::new();
    for end in ends {
        ports.push(new_port(context, end)?.into());
    }
    Ok(native::array(context, ports))
}

/// Runs `f` on the state of the port `this`: `TypeError: Illegal invocation` for anything else.
fn with_port<T>(this: &JsValue, f: impl FnOnce(&mut PortState) -> T) -> JsResult<T> {
    let object = this.as_object();
    let data = object
        .as_ref()
        .and_then(|o| o.downcast_ref::<PortData>())
        .ok_or_else(|| native::type_error("Illegal invocation"))?;
    let mut state = data.state.borrow_mut();
    Ok(f(&mut state))
}

/// The ends of the ports in `transfer`, taken from them: they are the page's from now on and the
/// ports they were are neutered.
///
/// # Errors
///
/// What Electron says of a transfer list that is not an array of live `MessagePortMain`s.
pub(crate) fn take_ports(
    transfer: &JsValue,
    how: Invalid,
    source: Option<&JsValue>,
    context: &mut Context,
) -> JsResult<Vec<PortEnd>> {
    if transfer.is_undefined() {
        return Ok(Vec::new());
    }
    let items = match transfer.as_object().filter(|o| o.is_array()) {
        Some(list) => native::list_of(&list.into(), context)?,
        None => return Err(invalid_transfer(how, None)),
    };
    let ports: Vec<_> = items
        .iter()
        .map(|item| item.as_object().filter(|o| o.is::<PortData>()))
        .collect();
    if let Some(at) = ports.iter().position(Option::is_none) {
        return Err(invalid_transfer(how, Some(at)));
    }
    let ports: Vec<JsValue> = ports.into_iter().flatten().map(JsValue::from).collect();
    for (at, port) in ports.iter().enumerate() {
        let same = |other: &JsValue| match (port.as_object(), other.as_object()) {
            (Some(a), Some(b)) => JsObject::equals(&a, &b),
            _ => false,
        };
        if ports[..at].iter().any(same) {
            return Err(define::error(&format!(
                "Port at index {at} is a duplicate."
            )));
        }
        if source.is_some_and(same) {
            let message = format!("Port at index {at} contains the source port.");
            return Err(define::error(&message));
        }
    }
    // All of them must be usable before any is taken: a refused list leaves the ports as they were.
    for (at, port) in ports.iter().enumerate() {
        if !with_port(port, |state| state.end.is_some() && !state.closed)? {
            let message = format!("Port at index {at} is already neutered.");
            return Err(define::error(&message));
        }
    }
    let mut ends = Vec::new();
    for port in &ports {
        ends.extend(with_port(port, |state| state.end.take())?);
    }
    Ok(ends)
}

fn invalid_transfer(how: Invalid, at: Option<usize>) -> boa_engine::JsError {
    match (how, at) {
        (Invalid::Contents, _) => define::error("Invalid value for transfer"),
        (Invalid::Port, Some(at)) => {
            native::type_error(format!("Port at index {at} is not a valid port"))
        }
        (Invalid::Port, None) => {
            native::type_error("transferables must be an array of MessagePorts")
        }
    }
}

// ---------- the methods ----------

fn post_message(this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    // A port that is closed or was given away takes the call and does nothing, whatever it is
    // given: nothing is cloned and no port is taken.
    if with_port(this, |state| state.end.is_none() || state.closed)? {
        return Ok(JsValue::undefined());
    }
    let message = ipc::clone(&arg(args, 0), context)?;
    let ports = take_ports(&arg(args, 1), Invalid::Port, Some(this), context)?;
    with_port(this, |state| {
        if let Some(end) = state.end.as_ref().filter(|_| !state.closed) {
            end.send(message, ports);
        }
    })?;
    Ok(JsValue::undefined())
}

fn start(this: &JsValue, _: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    with_port(this, |state| state.started = state.end.is_some())?;
    wake(context);
    Ok(JsValue::undefined())
}

fn close(this: &JsValue, _: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    with_port(this, |state| {
        if let Some(end) = state.end.as_ref().filter(|_| !state.closed) {
            end.close();
            state.closed = true;
            state.close_pending = true;
        }
    })?;
    wake(context);
    Ok(JsValue::undefined())
}

// ---------- hearing ----------

/// Hears what has arrived for the started ports, and the ports that were closed since the last
/// beat. Answers whether anything was heard.
pub(crate) fn pump(context: &mut Context) -> bool {
    let all = ports(context);
    let open = all.open.borrow().clone();
    let mut heard = false;
    for port in &open {
        heard |= pump_port(port, context);
    }
    all.open.borrow_mut().retain(|port| {
        // A port that was given away has no end and nothing more to hear.
        with_port(&port.clone().into(), |state| {
            state.end.is_some() && (!state.closed || state.close_pending)
        })
        .unwrap_or(false)
    });
    heard
}

/// What a port heard: a message with its ports, or its close.
enum Heard {
    Message(Cloned, Vec<PortEnd>),
    Closed,
}

fn pump_port(port: &JsObject, context: &mut Context) -> bool {
    let mut heard = false;
    for _ in 0..PER_BEAT {
        let next = with_port(&port.clone().into(), next_heard).ok().flatten();
        let Some(next) = next else {
            return heard;
        };
        heard = true;
        let emitted = match next {
            Heard::Message(data, ends) => emit_message(port, &data, ends, context),
            Heard::Closed => define::emit(context, port, "close", &[]),
        };
        define::report(context, emitted);
    }
    wake(context);
    heard
}

fn next_heard(state: &mut PortState) -> Option<Heard> {
    if state.close_pending {
        state.close_pending = false;
        return Some(Heard::Closed);
    }
    if !state.started || state.closed {
        return None;
    }
    match state.end.as_ref()?.receive()? {
        PortEvent::Message(data, ends) => Some(Heard::Message(data, ends)),
        PortEvent::Closed => {
            state.closed = true;
            Some(Heard::Closed)
        }
    }
}

fn emit_message(
    port: &JsObject,
    data: &Cloned,
    ends: Vec<PortEnd>,
    context: &mut Context,
) -> JsResult<()> {
    let ports = adopt_all(ends, context)?;
    let data = data.to_value(context)?;
    let event = define::record(context, &[("data", data), ("ports", ports)])?;
    define::emit(context, port, "message", &[event.into()])?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_port_given_away_is_no_longer_kept_and_a_refused_list_takes_nothing() {
        let mut runtime = verve::Runtime::new();
        let context = runtime.context();
        let (one, two) = remote_port_pair();
        let _kept = new_port(context, one).expect("a port");
        let gone = new_port(context, two).expect("a port");
        assert_eq!(ports(context).open.borrow().len(), 2);

        // One of two is no port: nothing is taken from the first.
        let refused = native::array(context, vec![gone.clone().into(), JsValue::from(1)]);
        assert!(take_ports(&refused, Invalid::Contents, None, context).is_err());
        assert!(with_port(&gone.clone().into(), |state| state.end.is_some()).expect("a port"));

        let list = native::array(context, vec![gone.into()]);
        let ends = take_ports(&list, Invalid::Contents, None, context).expect("a list of ports");
        assert_eq!(ends.len(), 1);
        pump(context);
        assert_eq!(ports(context).open.borrow().len(), 1);
    }
}

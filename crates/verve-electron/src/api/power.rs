//! `powerSaveBlocker` and `powerMonitor`: keeping the machine awake, and asking how it is doing.
//! The monitor's events (`suspend`, `resume`, `lock-screen`, …) need the shell to watch the
//! system's power messages and are not sent yet; the queries answer from the system directly.

use std::cell::RefCell;
use std::collections::BTreeMap;

use boa_engine::{Context, JsResult, JsValue};
use verve::native::{self, arg};

use super::define::{self, Args};
use crate::os::Block;

/// The blockers that are in force, by id; the system is kept awake while there is any.
#[derive(Default)]
struct Blockers {
    last: RefCell<u32>,
    active: RefCell<BTreeMap<u32, Block>>,
}

impl Blockers {
    fn start(&self, block: Block) -> u32 {
        let id = {
            let mut last = self.last.borrow_mut();
            *last += 1;
            *last
        };
        self.active.borrow_mut().insert(id, block);
        self.apply();
        id
    }

    fn stop(&self, id: u32) -> bool {
        let removed = self.active.borrow_mut().remove(&id).is_some();
        self.apply();
        removed
    }

    fn apply(&self) {
        let blocks: Vec<Block> = self.active.borrow().values().copied().collect();
        crate::os::keep_awake(&blocks);
    }
}

fn blockers(context: &mut Context) -> &Blockers {
    if context.get_data::<Blockers>().is_none() {
        context.insert_data(Blockers::default());
    }
    context.get_data::<Blockers>().expect("inserted above")
}

/// The `powerSaveBlocker` module.
///
/// # Errors
///
/// A property that could not be defined.
pub(crate) fn make_blocker(context: &mut Context) -> JsResult<JsValue> {
    let module = native::object(context);
    native::exports(
        context,
        &module,
        &[
            ("start", 1, start),
            ("stop", 1, stop),
            ("isStarted", 1, is_started),
        ],
    )?;
    Ok(module.into())
}

fn start(_: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let kind = Args::new(args).string(0, "type", context)?;
    let block = match kind.as_str() {
        "prevent-app-suspension" => Block::AppSuspension,
        "prevent-display-sleep" => Block::DisplaySleep,
        _ => {
            return Err(Args::new(args).conversion_failure(0));
        }
    };
    Ok(f64::from(blockers(context).start(block)).into())
}

fn id_arg(args: &[JsValue], context: &mut Context) -> JsResult<u32> {
    let id = Args::new(args).f64(0, "id", context)?;
    Ok(id as u32)
}

fn stop(_: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let id = id_arg(args, context)?;
    Ok(blockers(context).stop(id).into())
}

fn is_started(_: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let id = id_arg(args, context)?;
    Ok(blockers(context).active.borrow().contains_key(&id).into())
}

/// The `powerMonitor` module: an `EventEmitter` with the queries.
///
/// # Errors
///
/// A property that could not be defined.
pub(crate) fn make_monitor(context: &mut Context) -> JsResult<JsValue> {
    let emitter = define::emitter(context)?;
    let monitor = emitter.construct(&[], None, context)?;
    native::exports(
        context,
        &monitor,
        &[
            ("getSystemIdleState", 1, idle_state),
            ("getSystemIdleTime", 0, |_, _, _| {
                Ok((crate::os::idle_seconds() as f64).into())
            }),
            ("getCurrentThermalState", 0, |_, _, _| {
                Ok(native::js_text("unknown"))
            }),
            ("isOnBatteryPower", 0, on_battery),
        ],
    )?;
    native::accessors(context, &monitor, &[("onBatteryPower", on_battery, None)])?;
    Ok(monitor.into())
}

fn on_battery(_: &JsValue, _: &[JsValue], _: &mut Context) -> JsResult<JsValue> {
    Ok(crate::os::on_battery().into())
}

/// `getSystemIdleState(idleThreshold)`: `'idle'` when the user has been away that many seconds.
fn idle_state(_: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    // Electron names the value even when none was given: `conversion failure from undefined`.
    let given = [arg(args, 0)];
    let threshold = Args::new(&given).f64(0, "idleThreshold", context)?;
    let idle = crate::os::idle_seconds() as f64 >= threshold;
    Ok(native::js_text(if idle { "idle" } else { "active" }))
}

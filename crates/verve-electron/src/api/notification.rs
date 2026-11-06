//! `Notification`: the class exists, so an app that feature-detects it with `isSupported()` takes
//! its other branch, and one that does not gets `'failed'` from `show()` instead of a crash. The
//! system's toast (WinRT `ToastNotification`, `UNUserNotificationCenter`, D-Bus) is not bound yet.

use boa_engine::{Context, JsObject, JsResult, JsValue};
use verve::events::emitter_init;
use verve::native::{self, ClassSpec, NewOnly};

use super::define;

/// The `Notification` class, extending `EventEmitter`.
///
/// # Errors
///
/// The class could not be made.
pub(crate) fn make(context: &mut Context) -> JsResult<JsValue> {
    let emitter = define::emitter(context)?;
    let (class, _) = native::class(
        context,
        ClassSpec {
            name: "Notification",
            length: 1,
            init,
            parent: Some(&emitter),
            methods: &[
                ("show", 0, show),
                ("close", 0, |_, _, _| Ok(JsValue::undefined())),
            ],
            statics: &[("isSupported", 0, |_, _, _| Ok(false.into()))],
            new_only: Some(NewOnly::Native),
            ..ClassSpec::default()
        },
    )?;
    Ok(class.into())
}

fn init(this: &JsObject, _: &[JsValue], context: &mut Context) -> JsResult<()> {
    emitter_init(this, &[], context)
}

/// `show()`: the toast cannot be shown, which `'failed'` says with the event and the reason.
fn show(this: &JsValue, _: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    if let Some(object) = this.as_object() {
        let event = define::event(context)?;
        let reason = native::js_text("Notifications are not supported by Verve yet");
        define::emit(context, &object, "failed", &[event.into(), reason])?;
    }
    Ok(JsValue::undefined())
}

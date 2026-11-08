//! The globals of a fresh realm: timers, `queueMicrotask`, the console, and what each web and
//! Node module installs.

#[cfg(feature = "wasm")]
use crate::wasm;
use crate::{
    Level, clear_timer_native, console_group, console_label, console_line, console_time, crypto,
    dom_events, encoding, fetch, indexeddb, intl, log, messaging, microtask, navigator, process,
    set_timer, shared, streams, url, url_pattern, web, worker, zlib,
};
use boa_engine::{
    Context, JsNativeError, JsResult, JsString, JsValue, NativeFunction, js_string,
    object::ObjectInitializer, property::Attribute,
};

pub(super) fn install(context: &mut Context) -> JsResult<()> {
    let global = context.global_object();
    context.register_global_property(js_string!("global"), global.clone(), Attribute::all())?;

    // Timers.
    let functions: [(
        &str,
        usize,
        boa_engine::native_function::NativeFunctionPointer,
    ); 6] = [
        ("setTimeout", 2, |_, args, context| {
            set_timer(args, false, context)
        }),
        ("setInterval", 2, |_, args, context| {
            set_timer(args, true, context)
        }),
        ("setImmediate", 1, |_, args, context| {
            let mut with_zero = vec![args.first().cloned().unwrap_or_default(), 0.into()];
            with_zero.extend(args.iter().skip(1).cloned());
            set_timer(&with_zero, false, context)
        }),
        ("clearTimeout", 1, clear_timer_native),
        ("clearInterval", 1, clear_timer_native),
        ("clearImmediate", 1, clear_timer_native),
    ];
    for (name, length, function) in functions {
        context.register_global_callable(
            JsString::from(name),
            length,
            NativeFunction::from_fn_ptr(function),
        )?;
    }
    context.register_global_callable(
        js_string!("queueMicrotask"),
        1,
        NativeFunction::from_fn_ptr(|_, args, context| {
            let callback = args.first().cloned().unwrap_or_default();
            if !callback.is_callable() {
                return Err(JsNativeError::typ()
                    .with_message("queueMicrotask: callback is not a function")
                    .into());
            }
            microtask(context, callback, Vec::new());
            Ok(JsValue::undefined())
        }),
    )?;

    // The console.
    let mut console = ObjectInitializer::new(context);
    let levels: [(&str, boa_engine::native_function::NativeFunctionPointer); 6] = [
        ("log", |_, args, context| log(Level::Log, args, context)),
        ("info", |_, args, context| log(Level::Info, args, context)),
        ("warn", |_, args, context| log(Level::Warn, args, context)),
        ("error", |_, args, context| log(Level::Error, args, context)),
        ("debug", |_, args, context| log(Level::Debug, args, context)),
        ("trace", |_, args, context| log(Level::Debug, args, context)),
    ];
    for (name, function) in levels {
        console.function(
            NativeFunction::from_fn_ptr(function),
            JsString::from(name),
            0,
        );
    }
    let more: [(&str, boa_engine::native_function::NativeFunctionPointer); 12] = [
        ("dir", |_, args, context| {
            log(Level::Log, &args[..args.len().min(1)], context)
        }),
        ("table", |_, args, context| log(Level::Log, args, context)),
        ("assert", |_, args, context| {
            if args.first().is_some_and(JsValue::to_boolean) {
                return Ok(JsValue::undefined());
            }
            let mut rest = vec![JsValue::from(js_string!("Assertion failed:"))];
            rest.extend(args.iter().skip(1).cloned());
            log(Level::Error, &rest, context)
        }),
        ("time", |_, args, context| {
            let label = console_label(args, context)?;
            let state = shared(context);
            let now = (state.clock)();
            state.console_timers.borrow_mut().insert(label, now);
            Ok(JsValue::undefined())
        }),
        ("timeLog", |_, args, context| {
            console_time(args, context, false)
        }),
        ("timeEnd", |_, args, context| {
            console_time(args, context, true)
        }),
        ("count", |_, args, context| {
            let label = console_label(args, context)?;
            let count = {
                let state = shared(context);
                let mut counts = state.console_counts.borrow_mut();
                let count = counts.entry(label.clone()).or_default();
                *count += 1;
                *count
            };
            console_line(context, Level::Log, format!("{label}: {count}"));
            Ok(JsValue::undefined())
        }),
        ("countReset", |_, args, context| {
            let label = console_label(args, context)?;
            shared(context).console_counts.borrow_mut().remove(&label);
            Ok(JsValue::undefined())
        }),
        ("group", |_, args, context| console_group(args, context)),
        ("groupCollapsed", |_, args, context| {
            console_group(args, context)
        }),
        ("groupEnd", |_, _, context| {
            let state = shared(context);
            state
                .console_depth
                .set(state.console_depth.get().saturating_sub(1));
            Ok(JsValue::undefined())
        }),
        ("clear", |_, _, _| Ok(JsValue::undefined())),
    ];
    for (name, function) in more {
        console.function(
            NativeFunction::from_fn_ptr(function),
            JsString::from(name),
            0,
        );
    }
    let console = console.build();
    context.register_global_property(js_string!("console"), console, Attribute::all())?;

    intl::install(context)?;
    encoding::install(context)?;
    url::install(context)?;
    url_pattern::install(context)?;
    web::install(context)?;
    crypto::install(context)?;
    streams::install(context)?;
    fetch::install(context)?;
    zlib::install(context)?;
    dom_events::install(context)?;
    messaging::install(context)?;
    navigator::install(context)?;
    worker::install(context)?;
    indexeddb::install(context)?;
    #[cfg(feature = "wasm")]
    wasm::install(context)?;
    // Node's own modules, which Electron gives a desktop plugin, are made when first required;
    // `process` is a global too, and an `EventEmitter`.
    process::install(context)?;
    Ok(())
}

//! V8's stack trace API, which code bundled from npm relies on: the format of `error.stack`,
//! `Error.captureStackTrace`, `Error.stackTraceLimit` and `Error.prepareStackTrace` with its
//! `CallSite` objects.
//!
//! More information:
//!  - [V8 documentation][v8]
//!
//! [v8]: https://v8.dev/docs/stack-trace-api

use std::{cell::Cell, fmt::Write};

use crate::{
    Context, JsArgs, JsData, JsNativeError, JsResult, JsString, JsValue,
    builtins::function::OrdinaryFunction,
    js_string,
    native_function::NativeFunction,
    object::{FunctionObjectBuilder, JsObject, ObjectInitializer, builtins::JsArray},
    property::PropertyDescriptor,
    vm::{
        shadow_stack::{Backtrace, ShadowEntry},
        source_info::SourcePath,
    },
};
use boa_ast::Position;
use boa_gc::{Finalize, Gc, GcRefCell, Trace};

use super::Error;

thread_local! {
    /// Set while `Error.prepareStackTrace` runs, so a stack read inside it is formatted plainly
    /// instead of calling it again, as V8 does.
    static PREPARING: Cell<bool> = const { Cell::new(false) };
}

/// The default of `Error.stackTraceLimit`, as in V8.
pub(super) const DEFAULT_STACK_TRACE_LIMIT: i32 = 10;

/// How many frames a stack captured now keeps: `Error.stackTraceLimit`, at most the runtime's
/// backtrace limit, and none when it is not a number (as V8).
///
/// The property is read without running code, so only a data property counts.
pub(super) fn stack_trace_limit(context: &Context) -> usize {
    let ceiling = context.runtime_limits().backtrace_limit();
    let constructor = context.intrinsics().constructors().error().constructor();
    let value = constructor
        .borrow()
        .properties()
        .get(&js_string!("stackTraceLimit").into())
        .and_then(|desc| desc.value().cloned());
    let Some(limit) = value.and_then(|value| value.as_number()) else {
        return 0;
    };
    if limit.is_nan() || limit <= 0.0 {
        0
    } else if limit >= ceiling as f64 {
        ceiling
    } else {
        limit as usize
    }
}

/// The shadow-stack identity of a function, to find its frames with: the source info of its
/// code. `None` for anything that has no bytecode.
pub(super) fn function_identity(function: &JsValue) -> Option<crate::vm::source_info::SourceInfo> {
    let function = function.as_object()?;
    let function = function.downcast_ref::<OrdinaryFunction>()?;
    // A function compiled when first called runs its compiled code, so its frames are those.
    Some(
        crate::vm::CodeBlock::resolved(&function.code)
            .source_info
            .clone(),
    )
}

/// One frame of a stack, as V8 shows and describes it.
#[derive(Debug, Clone, Trace, Finalize, JsData)]
// SAFETY: Nothing in `CallSite` needs tracing, so this is safe.
#[boa_gc(unsafe_empty_trace)]
pub(super) struct CallSite {
    function_name: Option<JsString>,
    file_name: Option<JsString>,
    line: Option<u32>,
    column: Option<u32>,
    native: bool,
    eval: bool,
}

impl CallSite {
    /// The call site of a shadow-stack entry; `None` for an entry that is not a JavaScript call
    /// (the Rust location a native error was made at).
    fn from_entry(entry: &ShadowEntry) -> Option<Self> {
        match entry {
            ShadowEntry::Native { function_name, .. } => {
                let function_name = function_name.clone()?;
                Some(Self {
                    function_name: (!function_name.is_empty()).then_some(function_name),
                    file_name: None,
                    line: None,
                    column: None,
                    native: true,
                    eval: false,
                })
            }
            ShadowEntry::Bytecode { pc, source_info } => {
                let name = source_info.function_name();
                let function_name = if name.is_empty() || name == &js_string!("<main>") {
                    None
                } else if name == &js_string!("<eval>") {
                    Some(js_string!("eval"))
                } else {
                    Some(name.clone())
                };
                let path = source_info.map().path();
                let file_name = match path {
                    SourcePath::Path(path) => Some(JsString::from(path.display().to_string())),
                    SourcePath::None | SourcePath::Eval | SourcePath::Json => None,
                };
                let position = source_info.map().find(*pc);
                Some(Self {
                    function_name,
                    file_name,
                    line: position.map(Position::line_number),
                    column: position.map(Position::column_number),
                    native: false,
                    eval: matches!(path, SourcePath::Eval),
                })
            }
        }
    }

    /// The frame as a line of V8's `stack` shows it, without the leading `at`: `f (file:1:2)`,
    /// or `file:1:2` for top-level code.
    fn text(&self) -> String {
        let location = if self.native {
            "native".to_owned()
        } else {
            let mut location = self
                .file_name
                .as_ref()
                .map_or_else(|| "<anonymous>".to_owned(), JsString::to_std_string_escaped);
            if let (Some(line), Some(column)) = (self.line, self.column) {
                let _ = write!(location, ":{line}:{column}");
            }
            location
        };
        match &self.function_name {
            Some(name) => format!("{} ({location})", name.to_std_string_escaped()),
            None if self.native => format!("<anonymous> ({location})"),
            None => location,
        }
    }
}

/// The call sites of `backtrace`, newest first.
fn call_sites(backtrace: &Backtrace) -> impl Iterator<Item = CallSite> + '_ {
    backtrace.iter().rev().filter_map(CallSite::from_entry)
}

/// Formats the `stack` of `error` from `backtrace` as V8 does: `Error.prepareStackTrace(error,
/// callSites)` if it is set, else `${error}` followed by a `    at …` line per frame.
pub(super) fn format_stack(
    error: &JsObject,
    backtrace: &Backtrace,
    context: &mut Context,
) -> JsResult<JsValue> {
    let constructor = context.intrinsics().constructors().error().constructor();
    let prepare = constructor.get(js_string!("prepareStackTrace"), context)?;
    if let Some(prepare) = prepare.as_callable()
        && !PREPARING.get()
    {
        let prototype = call_site_prototype(context);
        let sites = call_sites(backtrace)
            .map(|site| {
                JsObject::from_proto_and_data_with_shared_shape(
                    context.root_shape(),
                    prototype.clone(),
                    site,
                )
                .upcast()
                .into()
            })
            .collect::<Vec<JsValue>>();
        let sites = JsArray::from_iter(sites, context);
        PREPARING.set(true);
        let result = prepare.call(
            &constructor.into(),
            &[error.clone().into(), sites.into()],
            context,
        );
        PREPARING.set(false);
        return result;
    }

    let header = Error::to_string(&error.clone().into(), &[], context)?.to_string(context)?;
    let mut stack = header.to_std_string_escaped();
    for site in call_sites(backtrace) {
        stack.push_str("\n    at ");
        stack.push_str(&site.text());
    }
    Ok(JsString::from(stack).into())
}

/// `Error.captureStackTrace ( targetObject [ , constructorOpt ] )`
///
/// Gives `targetObject` a `stack` with the current stack, leaving out `constructorOpt`'s newest
/// call and everything above it (V8).
pub(super) fn capture_stack_trace(
    _: &JsValue,
    args: &[JsValue],
    context: &mut Context,
) -> JsResult<JsValue> {
    #[derive(Trace, Finalize)]
    struct Captured {
        backtrace: Backtrace,
        formatted: GcRefCell<Option<JsValue>>,
    }

    let Some(target) = args.get_or_undefined(0).as_object() else {
        return Err(JsNativeError::typ()
            .with_message("Error.captureStackTrace: invalid argument")
            .into());
    };

    let limit = stack_trace_limit(context);
    let skip_until = args.get(1).and_then(function_identity);
    let backtrace = context.vm.shadow_stack.capture(limit, skip_until.as_ref());

    // The stack is formatted when first read, as in V8, so a `message` or `name` set after the
    // capture is in its first line.
    let captured = Gc::new(Captured {
        backtrace,
        formatted: GcRefCell::new(None),
    });
    let get = FunctionObjectBuilder::new(
        context.realm(),
        NativeFunction::from_copy_closure_with_captures(
            |this, _, captured: &Gc<Captured>, context| {
                if let Some(value) = captured.formatted.borrow().as_ref() {
                    return Ok(value.clone());
                }
                let Some(this) = this.as_object() else {
                    return Ok(JsValue::undefined());
                };
                let value = format_stack(&this, &captured.backtrace, context)?;
                *captured.formatted.borrow_mut() = Some(value.clone());
                Ok(value)
            },
            captured,
        ),
    )
    .name(js_string!("stack"))
    .build();
    let set = FunctionObjectBuilder::new(
        context.realm(),
        NativeFunction::from_fn_ptr(|this, args, context| {
            if let Some(this) = this.as_object() {
                this.define_property_or_throw(
                    js_string!("stack"),
                    PropertyDescriptor::builder()
                        .value(args.get_or_undefined(0).clone())
                        .writable(true)
                        .enumerable(false)
                        .configurable(true),
                    context,
                )?;
            }
            Ok(JsValue::undefined())
        }),
    )
    .name(js_string!("stack"))
    .length(1)
    .build();

    target.define_property_or_throw(
        js_string!("stack"),
        PropertyDescriptor::builder()
            .get(get)
            .set(set)
            .enumerable(false)
            .configurable(true),
        context,
    )?;

    Ok(JsValue::undefined())
}

/// The prototype of the `CallSite`s handed to `Error.prepareStackTrace`.
fn call_site_prototype(context: &mut Context) -> JsObject {
    type Method = fn(&JsValue, &[JsValue], &mut Context) -> JsResult<JsValue>;

    fn site(this: &JsValue) -> JsResult<CallSite> {
        this.as_object()
            .and_then(|o| o.downcast_ref::<CallSite>().map(|site| site.clone()))
            .ok_or_else(|| {
                JsNativeError::typ()
                    .with_message("CallSite method called on a non-CallSite object")
                    .into()
            })
    }
    fn string_or(value: Option<JsString>, otherwise: JsValue) -> JsValue {
        value.map_or(otherwise, Into::into)
    }
    fn number_or_null(value: Option<u32>) -> JsValue {
        value.map_or_else(JsValue::null, Into::into)
    }

    let mut builder = ObjectInitializer::new(context);
    let methods: [(&str, Method); 22] = [
        ("getThis", |_, _, _| Ok(JsValue::undefined())),
        ("getTypeName", |_, _, _| Ok(JsValue::null())),
        ("getFunction", |_, _, _| Ok(JsValue::undefined())),
        ("getFunctionName", |this, _, _| {
            Ok(string_or(site(this)?.function_name, JsValue::null()))
        }),
        ("getMethodName", |_, _, _| Ok(JsValue::null())),
        ("getFileName", |this, _, _| {
            Ok(string_or(site(this)?.file_name, JsValue::undefined()))
        }),
        ("getScriptNameOrSourceURL", |this, _, _| {
            Ok(string_or(site(this)?.file_name, JsValue::undefined()))
        }),
        ("getScriptHash", |_, _, _| Ok(js_string!().into())),
        ("getLineNumber", |this, _, _| {
            Ok(number_or_null(site(this)?.line))
        }),
        ("getColumnNumber", |this, _, _| {
            Ok(number_or_null(site(this)?.column))
        }),
        ("getEnclosingLineNumber", |_, _, _| Ok(JsValue::null())),
        ("getEnclosingColumnNumber", |_, _, _| Ok(JsValue::null())),
        ("getPosition", |_, _, _| Ok(0.into())),
        ("getEvalOrigin", |_, _, _| Ok(JsValue::undefined())),
        ("getPromiseIndex", |_, _, _| Ok(JsValue::null())),
        ("isToplevel", |_, _, _| Ok(true.into())),
        ("isEval", |this, _, _| Ok(site(this)?.eval.into())),
        ("isNative", |this, _, _| Ok(site(this)?.native.into())),
        ("isConstructor", |_, _, _| Ok(false.into())),
        ("isAsync", |_, _, _| Ok(false.into())),
        ("isPromiseAll", |_, _, _| Ok(false.into())),
        ("toString", |this, _, _| {
            Ok(JsString::from(site(this)?.text()).into())
        }),
    ];
    for (name, method) in methods {
        builder.function(NativeFunction::from_fn_ptr(method), JsString::from(name), 0);
    }
    builder.build()
}

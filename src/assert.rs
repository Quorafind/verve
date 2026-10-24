//! Node's `assert` (and `assert/strict`): the checks libraries make of their own arguments and
//! state, failing with an `AssertionError` whose message reads as Node's does —
//! `Expected values to be strictly equal:\n\n1 !== 2\n`.
//!
//! `assert` is `assert.ok`, with the rest on it. `assert.strict` (= `assert/strict`) has the
//! strict checks under the plain names: its `equal` is `strictEqual`, its `deepEqual`
//! `deepStrictEqual`. Equality is `util.isDeepStrictEqual`'s, and its loose sibling's.

use boa_engine::{
    Context, JsError, JsNativeError, JsObject, JsResult, JsString, JsValue, NativeFunction,
    js_string,
    native_function::NativeFunctionPointer,
    object::{
        FunctionObjectBuilder,
        builtins::{JsFunction, JsPromise, JsRegExp},
    },
};

use crate::native::{
    arg, closure, coded_error, function, get, hidden, invoke, js_text, object, text_of,
};
use crate::util::{Options, deep_equal, inspect};

fn shown(value: &JsValue, context: &mut Context) -> JsResult<String> {
    let options = Options {
        depth: 1000.,
        custom_inspect: false,
        sorted: true,
        ..Options::default()
    };
    inspect(value, &options, context)
}

/// The `AssertionError` class of this realm.
fn assertion_error_class(context: &mut Context) -> JsResult<JsObject> {
    let assert = crate::modules::node_module(context, "assert")?;
    get(&assert, "AssertionError", context)?
        .as_object()
        .ok_or_else(|| {
            JsNativeError::typ()
                .with_message("AssertionError is missing")
                .into()
        })
}

/// What a failed check throws: the `message` given, when it is an `Error`; otherwise an
/// `AssertionError` with `message` (or the generated one) and what was compared.
fn failure(
    context: &mut Context,
    message: &JsValue,
    generated: String,
    actual: JsValue,
    expected: JsValue,
    operator: &str,
) -> JsError {
    if let Some(given) = message.as_object()
        && is_error(&given, context)
    {
        return JsError::from_opaque(message.clone());
    }
    let made = (|| -> JsResult<JsValue> {
        let class = assertion_error_class(context)?;
        let options = object(context);
        options.set(js_string!("message"), message.clone(), false, context)?;
        options.set(js_string!("actual"), actual, false, context)?;
        options.set(js_string!("expected"), expected, false, context)?;
        options.set(js_string!("operator"), js_text(operator), false, context)?;
        let error = class.construct(&[options.into()], None, context)?;
        if message.is_undefined() {
            error.set(js_string!("message"), js_text(&generated), false, context)?;
            error.set(js_string!("generatedMessage"), true, false, context)?;
        }
        Ok(error.into())
    })();
    match made {
        Ok(error) => JsError::from_opaque(error),
        Err(error) => error,
    }
}

fn is_error(target: &JsObject, context: &mut Context) -> bool {
    let prototype = context.intrinsics().constructors().error().prototype();
    target.is::<boa_engine::builtins::error::Error>()
        || target
            .prototype()
            .is_some_and(|p| crate::native::inherits(&p, &prototype))
}

/// `a OP b` on one line when both are short, as `+ actual - expected` lines when not.
fn comparison(heading: &str, a: &str, b: &str, operator: &str) -> String {
    if !a.contains('\n') && !b.contains('\n') && a.len() + b.len() < 100 {
        return format!("{heading}\n\n{a} {operator} {b}\n");
    }
    let plus: Vec<String> = a.lines().map(|l| format!("+ {l}")).collect();
    let minus: Vec<String> = b.lines().map(|l| format!("- {l}")).collect();
    format!(
        "{heading}\n+ actual - expected\n\n{}\n{}",
        plus.join("\n"),
        minus.join("\n")
    )
}

// ─── The checks ──────────────────────────────────────────────────────────────────────────────

fn ok(_: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    if args.is_empty() {
        return Err(failure(
            context,
            &JsValue::undefined(),
            "No value argument passed to `assert.ok()`".into(),
            JsValue::undefined(),
            true.into(),
            "==",
        ));
    }
    let value = arg(args, 0);
    if value.to_boolean() {
        return Ok(JsValue::undefined());
    }
    let generated = format!(
        "The expression evaluated to a falsy value:\n\n  assert.ok({})\n",
        shown(&value, context)?
    );
    Err(failure(
        context,
        &arg(args, 1),
        generated,
        value,
        true.into(),
        "==",
    ))
}

fn strict_equal(_: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let (actual, expected) = (arg(args, 0), arg(args, 1));
    if JsValue::same_value(&actual, &expected) {
        return Ok(JsValue::undefined());
    }
    let (a, b) = (shown(&actual, context)?, shown(&expected, context)?);
    let generated = if a == b {
        format!("Values have same structure but are not reference-equal:\n\n{a}\n")
    } else {
        comparison("Expected values to be strictly equal:", &a, &b, "!==")
    };
    Err(failure(
        context,
        &arg(args, 2),
        generated,
        actual,
        expected,
        "strictEqual",
    ))
}

fn not_strict_equal(_: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let (actual, expected) = (arg(args, 0), arg(args, 1));
    if !JsValue::same_value(&actual, &expected) {
        return Ok(JsValue::undefined());
    }
    let generated = format!(
        "Expected \"actual\" to be strictly unequal to: {}",
        shown(&expected, context)?
    );
    Err(failure(
        context,
        &arg(args, 2),
        generated,
        actual,
        expected,
        "notStrictEqual",
    ))
}

fn loose_equal(a: &JsValue, b: &JsValue, context: &mut Context) -> JsResult<bool> {
    let nan = a.as_number().is_some_and(f64::is_nan) && b.as_number().is_some_and(f64::is_nan);
    Ok(nan || a.equals(b, context)?)
}

fn equal(_: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let (actual, expected) = (arg(args, 0), arg(args, 1));
    if loose_equal(&actual, &expected, context)? {
        return Ok(JsValue::undefined());
    }
    let generated = format!(
        "{} == {}",
        shown(&actual, context)?,
        shown(&expected, context)?
    );
    Err(failure(
        context,
        &arg(args, 2),
        generated,
        actual,
        expected,
        "==",
    ))
}

fn not_equal(_: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let (actual, expected) = (arg(args, 0), arg(args, 1));
    if !loose_equal(&actual, &expected, context)? {
        return Ok(JsValue::undefined());
    }
    let generated = format!(
        "{} != {}",
        shown(&actual, context)?,
        shown(&expected, context)?
    );
    Err(failure(
        context,
        &arg(args, 2),
        generated,
        actual,
        expected,
        "!=",
    ))
}

fn deep_strict_equal(_: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let (actual, expected) = (arg(args, 0), arg(args, 1));
    if deep_equal(&actual, &expected, true, context)? {
        return Ok(JsValue::undefined());
    }
    let (a, b) = (shown(&actual, context)?, shown(&expected, context)?);
    let generated = comparison("Expected values to be strictly deep-equal:", &a, &b, "!==");
    Err(failure(
        context,
        &arg(args, 2),
        generated,
        actual,
        expected,
        "deepStrictEqual",
    ))
}

fn not_deep_strict_equal(
    _: &JsValue,
    args: &[JsValue],
    context: &mut Context,
) -> JsResult<JsValue> {
    let (actual, expected) = (arg(args, 0), arg(args, 1));
    if !deep_equal(&actual, &expected, true, context)? {
        return Ok(JsValue::undefined());
    }
    let generated = format!(
        "Expected \"actual\" not to be strictly deep-equal to: {}",
        shown(&expected, context)?
    );
    Err(failure(
        context,
        &arg(args, 2),
        generated,
        actual,
        expected,
        "notDeepStrictEqual",
    ))
}

fn deep_loose_equal(_: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let (actual, expected) = (arg(args, 0), arg(args, 1));
    if deep_equal(&actual, &expected, false, context)? {
        return Ok(JsValue::undefined());
    }
    let (a, b) = (shown(&actual, context)?, shown(&expected, context)?);
    let generated = format!(
        "Expected values to be loosely deep-equal:\n\n{a}\n\nshould loosely deep-equal\n\n{b}"
    );
    Err(failure(
        context,
        &arg(args, 2),
        generated,
        actual,
        expected,
        "deepEqual",
    ))
}

fn not_deep_loose_equal(_: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let (actual, expected) = (arg(args, 0), arg(args, 1));
    if !deep_equal(&actual, &expected, false, context)? {
        return Ok(JsValue::undefined());
    }
    let generated = format!(
        "Expected \"actual\" not to be loosely deep-equal to:\n\n{}",
        shown(&expected, context)?
    );
    Err(failure(
        context,
        &arg(args, 2),
        generated,
        actual,
        expected,
        "notDeepEqual",
    ))
}

fn fail(_: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let message = arg(args, 0);
    if args.len() >= 2 {
        // The old `fail(actual, expected, message, operator)`.
        let (actual, expected) = (arg(args, 0), arg(args, 1));
        let operator = match arg(args, 3) {
            o if o.is_undefined() => "!=".to_owned(),
            o => text_of(&o, context)?,
        };
        let generated = format!(
            "{} {operator} {}",
            shown(&actual, context)?,
            shown(&expected, context)?
        );
        return Err(failure(
            context,
            &arg(args, 2),
            generated,
            actual,
            expected,
            &operator,
        ));
    }
    Err(failure(
        context,
        &message,
        "Failed".into(),
        JsValue::undefined(),
        JsValue::undefined(),
        "fail",
    ))
}

fn if_error(_: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let value = arg(args, 0);
    if value.is_null_or_undefined() {
        return Ok(JsValue::undefined());
    }
    let described = match value.as_object() {
        Some(error) if is_error(&error, context) => {
            text_of(&error.get(js_string!("message"), context)?, context)?
        }
        _ => shown(&value, context)?,
    };
    let error = failure(
        context,
        &JsValue::undefined(),
        format!("ifError got unwanted exception: {described}"),
        value,
        JsValue::null(),
        "ifError",
    );
    Err(error)
}

fn regexp_test(regexp: &JsValue, text: &str, context: &mut Context) -> JsResult<bool> {
    // A global or sticky expression starts from where it left off; Node resets it first.
    if let Some(r) = regexp.as_object() {
        r.set(js_string!("lastIndex"), 0, false, context)?;
    }
    Ok(invoke(regexp, "test", &[js_text(text)], context)?.to_boolean())
}

fn is_regexp(value: &JsValue) -> bool {
    value
        .as_object()
        .is_some_and(|o| JsRegExp::from_object(o).is_ok())
}

fn matching(args: &[JsValue], context: &mut Context, want: bool) -> JsResult<JsValue> {
    let (input, regexp) = (arg(args, 0), arg(args, 1));
    if !is_regexp(&regexp) {
        return Err(coded_error(
            context,
            "The \"regexp\" argument must be an instance of RegExp.",
            "ERR_INVALID_ARG_TYPE",
        ));
    }
    let operator = if want { "match" } else { "doesNotMatch" };
    if !input.is_string() {
        let generated = format!(
            "The \"string\" argument must be of type string. Received type {} ({})",
            input.type_of(),
            shown(&input, context)?
        );
        return Err(failure(
            context,
            &arg(args, 2),
            generated,
            input,
            regexp,
            operator,
        ));
    }
    let text = crate::print(&input);
    if regexp_test(&regexp, &text, context)? == want {
        return Ok(JsValue::undefined());
    }
    let pattern = text_of(&regexp, context)?;
    let generated = if want {
        format!(
            "The input did not match the regular expression {pattern}. Input:\n\n{}\n",
            shown(&input, context)?
        )
    } else {
        format!(
            "The input was expected to not match the regular expression {pattern}. Input:\n\n{}\n",
            shown(&input, context)?
        )
    };
    Err(failure(
        context,
        &arg(args, 2),
        generated,
        input,
        regexp,
        operator,
    ))
}

/// Whether what was thrown is what `expected` describes — a class, a `RegExp`, a validation
/// function, or an object of properties — answering the `AssertionError` when not.
fn check_thrown(
    actual: &JsValue,
    expected: &JsValue,
    message: &JsValue,
    operator: &str,
    context: &mut Context,
) -> JsResult<()> {
    if expected.is_undefined() || expected.is_string() {
        return Ok(());
    }
    if is_regexp(expected) {
        let text = text_of(actual, context)?;
        if regexp_test(expected, &text, context)? {
            return Ok(());
        }
        let pattern = text_of(expected, context)?;
        let generated = format!(
            "The input did not match the regular expression {pattern}. Input:\n\n{}\n",
            shown(&js_text(&text), context)?
        );
        return Err(failure(
            context,
            message,
            generated,
            actual.clone(),
            expected.clone(),
            operator,
        ));
    }
    if let Some(expected_fn) = expected.as_object().filter(|e| e.is_callable()) {
        let prototype = expected_fn.get(js_string!("prototype"), context)?;
        if prototype.is_object() && actual.is_object() && actual.instance_of(expected, context)? {
            return Ok(());
        }
        let error_class = context.intrinsics().constructors().error().constructor();
        let is_class = prototype.as_object().is_some_and(|p| {
            let error_prototype = context.intrinsics().constructors().error().prototype();
            crate::native::inherits(&p, &error_prototype)
        }) || JsObject::equals(&expected_fn, &error_class);
        if is_class {
            let wanted = text_of(&expected_fn.get(js_string!("name"), context)?, context)?;
            let received = match actual.as_object() {
                Some(o) => {
                    let constructor = o.get(js_string!("constructor"), context)?;
                    text_of(&get(&constructor, "name", context)?, context)?
                }
                None => shown(actual, context)?,
            };
            let generated = format!(
                "The error is expected to be an instance of \"{wanted}\". Received \"{received}\""
            );
            return Err(failure(
                context,
                message,
                generated,
                actual.clone(),
                expected.clone(),
                operator,
            ));
        }
        let returned = crate::call(
            context,
            expected,
            &JsValue::undefined(),
            std::slice::from_ref(actual),
        )?;
        if returned.as_boolean() == Some(true) {
            return Ok(());
        }
        let name = text_of(&expected_fn.get(js_string!("name"), context)?, context)?;
        let generated = format!(
            "The {}validation function is expected to return \"true\". Received {}",
            if name.is_empty() {
                String::new()
            } else {
                format!("\"{name}\" ")
            },
            shown(&returned, context)?
        );
        return Err(failure(
            context,
            message,
            generated,
            actual.clone(),
            expected.clone(),
            operator,
        ));
    }
    if let Some(wanted) = expected.as_object() {
        // Every own key of the object must match: `RegExp`s against strings, the rest deeply.
        for key in wanted.own_property_keys(context)? {
            let want = wanted.get(key.clone(), context)?;
            let got = match actual.as_object() {
                Some(a) => a.get(key.clone(), context)?,
                None => JsValue::undefined(),
            };
            let matched = if is_regexp(&want) && got.is_string() {
                regexp_test(&want, &crate::print(&got), context)?
            } else {
                deep_equal(&got, &want, true, context)?
            };
            if !matched {
                let generated = comparison(
                    "Expected values to be strictly deep-equal:",
                    &format!("{key}: {}", shown(&got, context)?),
                    &format!("{key}: {}", shown(&want, context)?),
                    "!==",
                );
                return Err(failure(
                    context,
                    message,
                    generated,
                    actual.clone(),
                    expected.clone(),
                    operator,
                ));
            }
        }
    }
    Ok(())
}

/// `throws(fn, expected?, message?)`: `expected` may be left out, its place taken by the message.
fn expected_and_message(args: &[JsValue]) -> (JsValue, JsValue) {
    let (second, third) = (arg(args, 1), arg(args, 2));
    if second.is_string() && args.len() == 2 {
        (JsValue::undefined(), second)
    } else {
        (second, third)
    }
}

fn throws(_: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let f = arg(args, 0);
    if !f.is_callable() {
        return Err(coded_error(
            context,
            "The \"fn\" argument must be of type function.",
            "ERR_INVALID_ARG_TYPE",
        ));
    }
    let (expected, message) = expected_and_message(args);
    match crate::call(context, &f, &JsValue::undefined(), &[]) {
        Ok(_) => {
            let mut generated = "Missing expected exception".to_owned();
            if let Some(class) = expected.as_object().filter(|e| e.is_callable()) {
                let name = text_of(&class.get(js_string!("name"), context)?, context)?;
                if !name.is_empty() {
                    generated.push_str(&format!(" ({name})"));
                }
            }
            if message.is_string() {
                generated.push_str(&format!(": {}", crate::print(&message)));
            } else {
                generated.push('.');
            }
            Err(failure(
                context,
                &JsValue::undefined(),
                generated,
                JsValue::undefined(),
                expected,
                "throws",
            ))
        }
        Err(error) => {
            let thrown = error.into_opaque(context)?;
            check_thrown(&thrown, &expected, &message, "throws", context)?;
            Ok(JsValue::undefined())
        }
    }
}

fn does_not_throw(_: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let f = arg(args, 0);
    if !f.is_callable() {
        return Err(coded_error(
            context,
            "The \"fn\" argument must be of type function.",
            "ERR_INVALID_ARG_TYPE",
        ));
    }
    let (expected, message) = expected_and_message(args);
    match crate::call(context, &f, &JsValue::undefined(), &[]) {
        Ok(_) => Ok(JsValue::undefined()),
        Err(error) => {
            let thrown = error.into_opaque(context)?;
            // Thrown, but not what `expected` names: thrown on, as Node does.
            if expected.is_object()
                && check_thrown(
                    &thrown,
                    &expected,
                    &JsValue::undefined(),
                    "doesNotThrow",
                    context,
                )
                .is_err()
            {
                return Err(JsError::from_opaque(thrown));
            }
            let described = described(&thrown, context)?;
            let mut generated = "Got unwanted exception".to_owned();
            if message.is_string() {
                generated.push_str(&format!(": {}", crate::print(&message)));
            } else {
                generated.push('.');
            }
            generated.push_str(&format!("\nActual message: \"{described}\""));
            Err(failure(
                context,
                &JsValue::undefined(),
                generated,
                thrown,
                expected,
                "doesNotThrow",
            ))
        }
    }
}

fn described(thrown: &JsValue, context: &mut Context) -> JsResult<String> {
    Ok(match thrown.as_object() {
        Some(error) if is_error(&error, context) => {
            text_of(&error.get(js_string!("message"), context)?, context)?
        }
        _ => shown(thrown, context)?,
    })
}

/// The promise `rejects` and `doesNotReject` wait on: the one given, or the one the function
/// given answers.
fn awaited(value: &JsValue, context: &mut Context) -> JsResult<JsPromise> {
    let value = if value.is_callable() {
        match crate::call(context, value, &JsValue::undefined(), &[]) {
            Ok(returned) => {
                if !get(&returned, "then", context)?.is_callable() {
                    return Err(coded_error(
                        context,
                        "Expected instance of Promise to be returned from the \"promiseFn\" function",
                        "ERR_INVALID_RETURN_VALUE",
                    ));
                }
                returned
            }
            Err(error) => return JsPromise::reject(error, context),
        }
    } else if get(value, "then", context)?.is_callable() {
        value.clone()
    } else {
        return Err(coded_error(
            context,
            "The \"promiseFn\" argument must be of type function or an instance of Promise.",
            "ERR_INVALID_ARG_TYPE",
        ));
    };
    JsPromise::resolve(value, context)
}

fn as_function(object: JsObject) -> JsFunction {
    JsFunction::from_object(object).expect("a native function")
}

fn rejects(_: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let promise = match awaited(&arg(args, 0), context) {
        Ok(promise) => promise,
        Err(error) => return Ok(JsPromise::reject(error, context)?.into()),
    };
    let (expected, message) = expected_and_message(args);
    let fulfilled = closure(
        context,
        "",
        1,
        |_, _, (expected, message): &(JsValue, JsValue), cx| {
            let mut generated = "Missing expected rejection".to_owned();
            if message.is_string() {
                generated.push_str(&format!(": {}", crate::print(message)));
            } else {
                generated.push('.');
            }
            Err(failure(
                cx,
                &JsValue::undefined(),
                generated,
                JsValue::undefined(),
                expected.clone(),
                "rejects",
            ))
        },
        (expected.clone(), message.clone()),
    );
    let rejected = closure(
        context,
        "",
        1,
        |_, args, (expected, message): &(JsValue, JsValue), cx| {
            check_thrown(&arg(args, 0), expected, message, "rejects", cx)?;
            Ok(JsValue::undefined())
        },
        (expected, message),
    );
    Ok(promise
        .then(
            Some(as_function(fulfilled)),
            Some(as_function(rejected)),
            context,
        )?
        .into())
}

fn does_not_reject(_: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let promise = match awaited(&arg(args, 0), context) {
        Ok(promise) => promise,
        Err(error) => return Ok(JsPromise::reject(error, context)?.into()),
    };
    let (expected, message) = expected_and_message(args);
    let fulfilled = function(context, "", 1, |_, _, _| Ok(JsValue::undefined()));
    let rejected = closure(
        context,
        "",
        1,
        |_, args, (expected, message): &(JsValue, JsValue), cx| {
            let thrown = arg(args, 0);
            let described = described(&thrown, cx)?;
            let mut generated = "Got unwanted rejection".to_owned();
            if message.is_string() {
                generated.push_str(&format!(": {}", crate::print(message)));
            } else {
                generated.push('.');
            }
            generated.push_str(&format!("\nActual message: \"{described}\""));
            Err(failure(
                cx,
                &JsValue::undefined(),
                generated,
                thrown,
                expected.clone(),
                "doesNotReject",
            ))
        },
        (expected, message),
    );
    Ok(promise
        .then(
            Some(as_function(fulfilled)),
            Some(as_function(rejected)),
            context,
        )?
        .into())
}

// ─── AssertionError ──────────────────────────────────────────────────────────────────────────

/// `new AssertionError({ message, actual, expected, operator })`: a real `Error` — it has a
/// stack, and `util.types.isNativeError` says so — made on `new.target`'s prototype.
fn construct_assertion_error(
    new_target: &JsValue,
    args: &[JsValue],
    context: &mut Context,
) -> JsResult<JsValue> {
    let Some(new_target) = new_target.as_object() else {
        return Err(JsNativeError::typ()
            .with_message("Class constructor AssertionError cannot be invoked without 'new'")
            .into());
    };
    let options = arg(args, 0);
    if !options.is_object() {
        return Err(coded_error(
            context,
            "The \"options\" argument must be of type object.",
            "ERR_INVALID_ARG_TYPE",
        ));
    }
    let message = get(&options, "message", context)?;
    let actual = get(&options, "actual", context)?;
    let expected = get(&options, "expected", context)?;
    let operator = get(&options, "operator", context)?;
    let generated = message.is_undefined();
    let text = if generated {
        format!(
            "{} {} {}",
            shown(&actual, context)?,
            crate::print(&operator),
            shown(&expected, context)?
        )
    } else {
        text_of(&message, context)?
    };
    let error_class = context.intrinsics().constructors().error().constructor();
    let error = error_class.construct(&[js_text(&text)], Some(&new_target), context)?;
    error.set(js_string!("generatedMessage"), generated, false, context)?;
    error.set(
        js_string!("code"),
        js_string!("ERR_ASSERTION"),
        false,
        context,
    )?;
    error.set(js_string!("actual"), actual, false, context)?;
    error.set(js_string!("expected"), expected, false, context)?;
    error.set(js_string!("operator"), operator, false, context)?;
    Ok(error.into())
}

fn assertion_error(context: &mut Context) -> JsResult<JsObject> {
    let class: JsObject = FunctionObjectBuilder::new(
        context.realm(),
        NativeFunction::from_fn_ptr(construct_assertion_error),
    )
    .name(js_string!("AssertionError"))
    .length(1)
    .constructor(true)
    .build()
    .into();
    let (error_prototype, error_constructor) = {
        let error = context.intrinsics().constructors().error();
        (error.prototype(), error.constructor())
    };
    let prototype = object(context);
    prototype.set_prototype(Some(error_prototype));
    class.set_prototype(Some(error_constructor));
    hidden(
        &prototype,
        js_string!("constructor"),
        class.clone(),
        context,
    )?;
    hidden(
        &prototype,
        js_string!("name"),
        js_text("AssertionError"),
        context,
    )?;
    let to_string = function(context, "toString", 0, |this, _, cx| {
        let name = text_of(&get(this, "name", cx)?, cx)?;
        let code = get(this, "code", cx)?;
        let message = text_of(&get(this, "message", cx)?, cx)?;
        let code = if code.is_string() {
            format!(" [{}]", crate::print(&code))
        } else {
            String::new()
        };
        Ok(js_text(&format!("{name}{code}: {message}")))
    });
    hidden(&prototype, js_string!("toString"), to_string, context)?;
    class.define_property_or_throw(
        js_string!("prototype"),
        boa_engine::property::PropertyDescriptor::builder()
            .value(prototype)
            .writable(false)
            .enumerable(false)
            .configurable(false),
        context,
    )?;
    Ok(class)
}

// ─── The module ──────────────────────────────────────────────────────────────────────────────

/// The checks with their loose names, and what `assert.strict` puts under those names.
const CHECKS: &[(&str, usize, NativeFunctionPointer, NativeFunctionPointer)] = &[
    ("ok", 2, ok, ok),
    ("equal", 3, equal, strict_equal),
    ("notEqual", 3, not_equal, not_strict_equal),
    ("deepEqual", 3, deep_loose_equal, deep_strict_equal),
    (
        "notDeepEqual",
        3,
        not_deep_loose_equal,
        not_deep_strict_equal,
    ),
    ("strictEqual", 3, strict_equal, strict_equal),
    ("notStrictEqual", 3, not_strict_equal, not_strict_equal),
    ("deepStrictEqual", 3, deep_strict_equal, deep_strict_equal),
    (
        "notDeepStrictEqual",
        3,
        not_deep_strict_equal,
        not_deep_strict_equal,
    ),
    ("throws", 3, throws, throws),
    ("doesNotThrow", 3, does_not_throw, does_not_throw),
    ("rejects", 3, rejects, rejects),
    ("doesNotReject", 3, does_not_reject, does_not_reject),
    (
        "match",
        3,
        |_, args, cx| matching(args, cx, true),
        |_, args, cx| matching(args, cx, true),
    ),
    (
        "doesNotMatch",
        3,
        |_, args, cx| matching(args, cx, false),
        |_, args, cx| matching(args, cx, false),
    ),
    ("fail", 1, fail, fail),
    ("ifError", 1, if_error, if_error),
];

/// `require('assert')`.
pub(crate) fn assert_module(context: &mut Context, _: &str) -> JsResult<JsValue> {
    let assert = function(context, "ok", 2, ok);
    let strict = function(context, "strict", 2, ok);
    for (name, length, loose, strict_fn) in CHECKS {
        let f = function(context, name, *length, *loose);
        assert.set(JsString::from(*name), f, false, context)?;
        let f = function(context, name, *length, *strict_fn);
        strict.set(JsString::from(*name), f, false, context)?;
    }
    let class = assertion_error(context)?;
    for target in [&assert, &strict] {
        target.set(js_string!("AssertionError"), class.clone(), false, context)?;
        target.set(js_string!("strict"), strict.clone(), false, context)?;
    }
    Ok(assert.into())
}

/// `require('assert/strict')`: `assert.strict`.
pub(crate) fn strict_module(context: &mut Context, _: &str) -> JsResult<JsValue> {
    let assert = crate::modules::node_module(context, "assert")?;
    get(&assert, "strict", context)
}

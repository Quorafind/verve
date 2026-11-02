//! What `api::define` reads arguments and options with, and how it tells one class's objects
//! from another's. The messages are Electron 39.8.10's (gin) and Node 22's.

use verve::Runtime;
use verve::boa_engine::{JsObject, JsResult, JsValue};
use verve_electron::api::define::{self, Args, Fields, Kind};

fn object(runtime: &mut Runtime, source: &str) -> JsObject {
    runtime
        .eval(&format!("({source})"))
        .unwrap_or_else(|error| panic!("{error}"))
        .as_object()
        .expect("an object")
}

/// The error a result failed with, as `Name: message (code)`.
fn failure<T>(result: JsResult<T>, runtime: &mut Runtime) -> String {
    let error = result.map(|_| ()).expect_err("it fails");
    let value = error
        .into_opaque(runtime.context())
        .expect("an error value");
    let to_string = verve::native::invoke(&value, "toString", &[], runtime.context());
    let text = verve::print(&to_string.unwrap_or_default());
    let code = verve::native::get(&value, "code", runtime.context()).unwrap_or_default();
    if code.is_undefined() {
        text
    } else {
        format!("{text} ({})", verve::print(&code))
    }
}

#[test]
fn a_constructors_options_leave_a_wrong_typed_field_at_its_default() {
    let mut runtime = Runtime::new();
    // Electron 39.8.10: width 'abc', 300.7 and '300' make an 800 pixel window; `show: 'yes'` a
    // shown one, `resizable: 0` a fixed one, `title: 5` the default title.
    let given = object(
        &mut runtime,
        "{ width: 'abc', height: 300.7, show: 'yes', resizable: 0, title: 5, depth: 300 }",
    );
    let fields = Fields::new(Some(&given));
    let context = runtime.context();
    assert_eq!(fields.int("width", 800., context).unwrap(), 800.);
    assert_eq!(fields.int("height", 600., context).unwrap(), 600.);
    assert_eq!(fields.int("depth", 1., context).unwrap(), 300.);
    assert!(fields.bool("show", false, context).unwrap());
    assert!(!fields.bool("resizable", true, context).unwrap());
    assert_eq!(fields.string("title", context).unwrap(), None);
    assert_eq!(fields.string("missing", context).unwrap(), None);
    assert_eq!(Fields::new(None).f64("width", 7., context).unwrap(), 7.);
}

#[test]
fn a_getter_that_throws_is_the_scripts_error_not_a_default() {
    let mut runtime = Runtime::new();
    let given = object(&mut runtime, "{ get width() { throw new Error('boom'); } }");
    let result = Fields::new(Some(&given)).int("width", 800., runtime.context());
    assert_eq!(failure(result, &mut runtime), "Error: boom");
}

#[test]
fn strict_fields_name_the_property_as_node_does() {
    let mut runtime = Runtime::new();
    let given = object(
        &mut runtime,
        "{ width: 'wide', run: 1, mode: 'sideways', ok: 'a' }",
    );
    let fields = Fields::strict(Some(&given), "options");
    let context = runtime.context();
    let result = fields.f64("width", 0., context);
    assert_eq!(
        failure(result, &mut runtime),
        "TypeError: The \"options.width\" property must be of type number. \
         Received type string ('wide') (ERR_INVALID_ARG_TYPE)"
    );
    let result = fields.function("run", runtime.context());
    assert_eq!(
        failure(result, &mut runtime),
        "TypeError: The \"options.run\" property must be of type function. \
         Received type number (1) (ERR_INVALID_ARG_TYPE)"
    );
    let result = fields.one_of("mode", &["left", "right"], runtime.context());
    assert_eq!(
        failure(result, &mut runtime),
        "TypeError: The argument 'options.mode' must be one of: left, right. \
         Received 'sideways' (ERR_INVALID_ARG_VALUE)"
    );
    let context = runtime.context();
    assert_eq!(
        fields.one_of("ok", &["a", "b"], context).unwrap(),
        Some("a")
    );
    // The lenient reading of the same fields takes the default instead.
    let lenient = Fields::new(Some(&given));
    assert_eq!(lenient.one_of("mode", &["left"], context).unwrap(), None);
    assert!(lenient.function("run", context).unwrap().is_none());
}

#[test]
fn a_native_methods_arguments_fail_as_gin_does() {
    let mut runtime = Runtime::new();
    let context = runtime.context();
    let given = [JsValue::new(5)];
    let result = Args::new(&given).string(0, "url", context);
    assert_eq!(
        failure(result, &mut runtime),
        "TypeError: Error processing argument at index 0, conversion failure from "
    );
    // Electron 39.8.10: `win.setTitle()` has no argument to convert.
    let result = Args::new(&[]).string(0, "title", runtime.context());
    assert_eq!(
        failure(result, &mut runtime),
        "TypeError: Insufficient number of arguments."
    );
    let given = [JsValue::from(verve::boa_engine::JsString::from("a"))];
    let result = Args::new(&given).f64(0, "width", runtime.context());
    assert_eq!(
        failure(result, &mut runtime),
        "TypeError: Error processing argument at index 0, conversion failure from a"
    );
    // A method Electron writes in JavaScript fails as Node's do.
    let result = Args::node(&[]).string(0, "channel", runtime.context());
    assert_eq!(
        failure(result, &mut runtime),
        "TypeError: The \"channel\" argument must be of type string. Received undefined \
         (ERR_INVALID_ARG_TYPE)"
    );
    let context = runtime.context();
    assert!(Args::new(&[]).bool_or(0, "x", true, context).unwrap());
    assert!(
        !Args::new(&[JsValue::new(0)])
            .bool_or(0, "x", true, context)
            .unwrap()
    );
}

#[test]
fn an_object_is_known_by_the_class_it_was_bound_as() {
    const WINDOW: Kind = Kind("window");
    const CONTENTS: Kind = Kind("contents");
    let mut runtime = Runtime::new();
    let window = object(&mut runtime, "{}");
    let other = object(&mut runtime, "{}");
    let context = runtime.context();
    define::bind(&window, WINDOW, 4, context);
    let this = JsValue::from(window);
    assert_eq!(define::id_of(&this, WINDOW, context), Some(4));
    assert_eq!(define::id_of(&this, CONTENTS, context), None);
    assert_eq!(define::live(&this, WINDOW, context, Some).unwrap(), (4, 4));
    // Gone: what the id stood for no longer is.
    let gone = define::live(&this, WINDOW, context, |_| None::<()>).map(|_| ());
    assert_eq!(
        failure(gone, &mut runtime),
        "TypeError: Object has been destroyed"
    );
    // An instance of another class, and an object that is no instance at all.
    let wrong = define::live(&this, CONTENTS, runtime.context(), Some).map(|_| ());
    assert_eq!(
        failure(wrong, &mut runtime),
        "TypeError: Illegal invocation"
    );
    let stranger = define::live(&JsValue::from(other), WINDOW, runtime.context(), Some).map(|_| ());
    assert_eq!(
        failure(stranger, &mut runtime),
        "TypeError: Object has been destroyed"
    );
}

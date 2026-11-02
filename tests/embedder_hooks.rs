//! The kernel's hooks for an embedder building Electron on it, a section for each: the structured
//! clone that crosses threads.

use boa_engine::JsValue;
use verve::Runtime;
use verve::clone::{CloneBytesError, Cloned};

// ─── A value between realms ───────────────────────────────────────────────────────────────

/// What a value made of `script` looks like when written out again in `other`, and again after
/// the trip through bytes.
fn round_trip(script: &str) -> (String, String) {
    let mut from = Runtime::new();
    let value = from.eval(script).unwrap();
    let cloned = Cloned::from_value(&value, from.context()).unwrap();
    let bytes = cloned.to_bytes().unwrap();
    let reread = Cloned::from_bytes(&bytes).unwrap();

    let describe = r#"
        (function (v) {
            return [
                v.text, v.number, v.big.toString(), v.date.getTime(),
                v.re.source + '/' + v.re.flags,
                v.map.get('k'), [...v.set].join(','), v.list.join(','), v.wrapped.deep,
                v.error.name + ': ' + v.error.message,
                v.cycle === v,
                v.u8.constructor.name + ':' + v.u8.join(','),
                v.i16.constructor.name + ':' + v.i16.join(','),
                v.view.constructor.name + ':' + v.view.byteLength,
                v.buffer.byteLength,
                v.blob instanceof Blob, v.blob.size, v.blob.type,
                v.file instanceof File, v.file.name, v.file.size,
                v.nested[0].b === v.nested[1].b,
            ].join('|');
        })
    "#;
    let mut to = Runtime::new();
    let describe = to.eval(describe).unwrap();
    let ask = |value: JsValue, to: &mut Runtime| {
        let text = to
            .call(&describe, &JsValue::undefined(), &[value])
            .unwrap_or_else(|e| panic!("{e}"));
        verve::print(&text)
    };
    let direct = ask(cloned.to_value(to.context()).unwrap(), &mut to);
    let written = ask(reread.to_value(to.context()).unwrap(), &mut to);
    (direct, written)
}

/// A value holding one of everything the clone carries, cycles and shared parts included.
const EVERYTHING: &str = r#"
    (() => {
        const shared = { b: 'shared' };
        const v = {
            text: 'héllo', number: 42.5, big: 9007199254740993n,
            date: new Date(1700000000000), re: /ab+c/gi,
            map: new Map([['k', 'v']]), set: new Set([1, 2]), list: [1, 2, 3],
            wrapped: { deep: 'd' }, error: new TypeError('bad'),
            u8: new Uint8Array([1, 2, 3]), i16: new Int16Array([-1, 2]),
            view: new DataView(new ArrayBuffer(4)), buffer: new ArrayBuffer(2),
            blob: new Blob(['abc'], { type: 'text/plain' }),
            file: new File(['xy'], 'n.txt', { lastModified: 5 }),
            nested: [{ b: shared }, { b: shared }],
        };
        v.cycle = v;
        return v;
    })()
"#;

const EVERYTHING_SEEN: &str = "héllo|42.5|9007199254740993|1700000000000|ab+c/gi|v|1,2|1,2,3|d|\
TypeError: bad|true|Uint8Array:1,2,3|Int16Array:-1,2|DataView:4|2|true|3|text/plain|true|n.txt|2|true";

fn assert_send<T: Send>() {}

// ─── The structured clone ───────────────────────────────────────────────────────────────────

#[test]
fn a_clone_carries_every_kind_of_value_between_realms() {
    assert_send::<Cloned>();
    let (direct, written) = round_trip(EVERYTHING);
    assert_eq!(direct, EVERYTHING_SEEN);
    assert_eq!(written, EVERYTHING_SEEN);
}

#[test]
fn a_clone_of_an_error_can_be_made_without_a_realm() {
    let mut rt = Runtime::new();
    let cloned = Cloned::from_error_message("Error", "it failed");
    let value = cloned.to_value(rt.context()).unwrap();
    let error = value.as_object().expect("an object");
    let name = error
        .get(boa_engine::js_string!("name"), rt.context())
        .unwrap();
    let message = error
        .get(boa_engine::js_string!("message"), rt.context())
        .unwrap();
    assert_eq!(verve::print(&name), "Error");
    assert_eq!(verve::print(&message), "it failed");
    // Bytes that are not a clone say what is wrong with them.
    assert_eq!(
        Cloned::from_bytes(&[9, 9, 9]).unwrap_err(),
        CloneBytesError::Truncated { at: 1 }
    );
    assert_eq!(
        Cloned::from_bytes(&[200]).unwrap_err(),
        CloneBytesError::UnknownTag { tag: 200, at: 0 }
    );
    assert_eq!(
        Cloned::from_bytes(&[0, 0]).unwrap_err(),
        CloneBytesError::Trailing { count: 1 }
    );
    // Bytes from another process do not get to recurse without end: arrays inside arrays.
    let nested: Vec<u8> = (0..100_000)
        .flat_map(|_| [12, 1, 0, 0, 0])
        .chain([0])
        .collect();
    assert!(matches!(
        Cloned::from_bytes(&nested).unwrap_err(),
        CloneBytesError::TooDeep { .. }
    ));
}

#[test]
fn what_cannot_be_cloned_is_a_data_clone_error() {
    let mut rt = Runtime::new();
    let function = rt.eval("(function f() {})").unwrap();
    let denied = Cloned::from_value(&function, rt.context()).unwrap_err();
    assert!(verve::describe(rt.context(), &denied).contains("DataCloneError"));
    let symbol = rt.eval("Symbol('s')").unwrap();
    assert!(Cloned::from_value(&symbol, rt.context()).is_err());
}

// ─── The rest of the hooks ─────────────────────────────────────────────────────────────────

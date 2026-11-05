//! What each kind of value becomes when it crosses between the main process and a page: the
//! HTML structured clone, as Electron 39.8.10 applies it (`tests/electron/ipc/observe`
//! sent every sample of its `samples.js` each way and described what arrived).

use std::collections::BTreeMap;

use serde_json::{Value, json};

use crate::Ipc;

/// What arrived, by sample, Electron 39.8.10 — the same in both directions, through `send`, an
/// `invoke`'s answer and a `sendSync`'s `returnValue`.
const ELECTRON: &str = r##"{
  "undefined": "#undefined",
  "null": null,
  "true": true,
  "negativeZero": "#-0",
  "nan": "#NaN",
  "infinity": "#Infinity",
  "string": "héllo",
  "loneSurrogate": "#lone:d800",
  "bigint": "#12n",
  "date": {"ctor":"Date","time":86400000},
  "invalidDate": {"ctor":"Date","time":null},
  "regexp": {"ctor":"RegExp","source":"a+b","flags":"gi","lastIndex":0},
  "error": {"ctor":"Error","name":"Error","message":"plain","own":["stack","message"],"stackHead":"Error: plain","props":{"message":"plain"},"enumerable":[]},
  "typeError": {"ctor":"TypeError","name":"TypeError","message":"typed","own":["stack","message"],"stackHead":"TypeError: typed","props":{"message":"typed"},"enumerable":[]},
  "errorWithProps": {"ctor":"RangeError","name":"RangeError","message":"ranged","own":["stack","message"],"stackHead":"RangeError: ranged","props":{"message":"ranged"},"enumerable":[]},
  "errorWithCause": {"ctor":"Error","name":"Error","message":"outer","own":["stack","message","cause"],"stackHead":"Error: outer","props":{"message":"outer","cause":{"ctor":"Error","name":"Error","message":"inner","own":["stack","message"],"stackHead":"Error: inner","props":{"message":"inner"},"enumerable":[]}},"enumerable":[]},
  "customError": {"ctor":"Error","name":"Error","message":"mine","own":["stack","message"],"stackHead":"MyError: mine","props":{"message":"mine"},"enumerable":[]},
  "errorWithoutStack": {"ctor":"Error","name":"Error","message":"nostack","own":["stack","message"],"stackHead":"undefined","props":{"message":"nostack"},"enumerable":[]},
  "arrayBuffer": {"ctor":"ArrayBuffer","bytes":[9,8,7]},
  "uint8": {"ctor":"Uint8Array","isBuffer":false,"byteOffset":0,"byteLength":3,"bufferByteLength":3,"bytes":[1,2,3]},
  "float32Offset": {"ctor":"Float32Array","isBuffer":false,"byteOffset":4,"byteLength":8,"bufferByteLength":16,"bytes":[0,0,32,64,0,0,96,64]},
  "dataView": {"ctor":"DataView","isBuffer":false,"byteOffset":1,"byteLength":2,"bufferByteLength":4,"bytes":[2,3]},
  "uint8Window": {"ctor":"Uint8Array","isBuffer":false,"byteOffset":2,"byteLength":3,"bufferByteLength":8,"bytes":[3,4,5]},
  "bigInt64": {"ctor":"BigInt64Array","isBuffer":false,"byteOffset":0,"byteLength":8,"bufferByteLength":8,"bytes":[5,0,0,0,0,0,0,0]},
  "buffer": {"ctor":"Uint8Array","isBuffer":false,"byteOffset":8,"byteLength":3,"bufferByteLength":8192,"bytes":[97,98,99]},
  "bufferAlloc": {"ctor":"Uint8Array","isBuffer":false,"byteOffset":0,"byteLength":3,"bufferByteLength":3,"bytes":[7,7,7]},
  "map": {"ctor":"Map","entries":[["a",1],[{"ctor":"Object","tag":"Object","keys":["k"],"props":{"k":1},"symbols":0},{"ctor":"Set","items":[1]}]]},
  "set": {"ctor":"Set","items":[1,"two",{"ctor":"Array","length":1,"items":[3],"extra":[]}]},
  "nested": {"ctor":"Object","tag":"Object","keys":["a"],"props":{"a":{"ctor":"Object","tag":"Object","keys":["b"],"props":{"b":{"ctor":"Array","length":2,"items":[1,{"ctor":"Object","tag":"Object","keys":["c"],"props":{"c":2},"symbols":0}],"extra":[]}},"symbols":0}},"symbols":0},
  "circular": {"ctor":"Object","tag":"Object","keys":["name","self"],"props":{"name":"loop","self":"#seen:0"},"symbols":0},
  "sharedTwice": {"ctor":"Array","length":2,"items":[{"ctor":"Object","tag":"Object","keys":["n"],"props":{"n":1},"symbols":0},"#seen:1"],"extra":[]},
  "sparse": {"ctor":"Array","length":3,"items":[1,"#hole",3],"extra":[]},
  "arrayWithProps": {"ctor":"Array","length":2,"items":[1,2],"extra":[["label","x"]]},
  "getter": {"ctor":"Object","tag":"Object","keys":["computed","plain"],"props":{"computed":5,"plain":1},"symbols":0},
  "nullProto": {"ctor":"Object","tag":"Object","keys":["k"],"props":{"k":"v"},"symbols":0},
  "symbolKey": {"ctor":"Object","tag":"Object","keys":["visible"],"props":{"visible":2},"symbols":0},
  "instance": {"ctor":"Object","tag":"Object","keys":["x"],"props":{"x":1},"symbols":0},
  "undefinedProperty": {"ctor":"Object","tag":"Object","keys":["a","b"],"props":{"a":"#undefined","b":1},"symbols":0},
  "boxedNumber": {"ctor":"Number","tag":"Number","boxed":4},
  "boxedString": {"ctor":"String","tag":"String","boxed":"s"},
  "url": {"ctor":"Object","tag":"Object","keys":[],"props":{},"symbols":0}
}"##;

/// Samples Electron cannot clone: sending them throws `An object could not be cloned.`
const REFUSED: [&str; 7] = [
    "function",
    "symbol",
    "promise",
    "weakMap",
    "objectWithFunction",
    "proxy",
    "generator",
];

/// What Verve makes of the sample `name`, given what Electron made of it. They agree on all but
/// these, each a difference of the clone and not of IPC:
///
/// - a view crosses with its own bytes, not the whole `ArrayBuffer` behind it at its offset;
/// - a hole in an array comes out `undefined`, and an array's own non-index properties are lost;
/// - `delete error.stack` leaves the stack the engine computes; and
/// - a boxed number is an empty object and a boxed string an object of its characters.
fn as_verve(name: &str, mut shape: Value) -> Value {
    match name {
        "buffer" | "float32Offset" | "uint8Window" | "dataView" => {
            shape["bufferByteLength"] = shape["byteLength"].clone();
            shape["byteOffset"] = json!(0);
        }
        "sparse" => shape["items"][1] = json!("#undefined"),
        "arrayWithProps" => shape["extra"] = json!([]),
        "errorWithoutStack" => shape["stackHead"] = json!("Error: nostack"),
        "boxedNumber" => {
            shape =
                json!({ "ctor": "Object", "tag": "Object", "keys": [], "props": {}, "symbols": 0 })
        }
        "boxedString" => {
            shape = json!({ "ctor": "Object", "tag": "Object", "keys": ["0"], "props": { "0": "s" }, "symbols": 0 });
        }
        _ => {}
    }
    shape
}

/// An error's own property names in order of name: V8 lists `stack` before `message`, and Verve
/// the other way round, which nothing depends on.
fn sorted_own(shape: &mut Value) {
    match shape {
        Value::Object(fields) => {
            if let Some(Value::Array(own)) = fields.get_mut("own") {
                own.sort_by_key(|name| name.as_str().map(str::to_owned));
            }
            fields.values_mut().for_each(sorted_own);
        }
        Value::Array(items) => items.iter_mut().for_each(sorted_own),
        _ => {}
    }
}

fn expected() -> BTreeMap<String, Value> {
    let table: BTreeMap<String, Value> = serde_json::from_str(ELECTRON).expect("the table is JSON");
    table
        .into_iter()
        .map(|(name, shape)| (name.clone(), as_verve(&name, shape)))
        .collect()
}

fn arrived(ipc: &Ipc, side: &str) -> BTreeMap<String, Value> {
    let shapes = if side == "main" {
        ipc.main("got")
    } else {
        ipc.page("got")
    };
    serde_json::from_value(shapes).expect("a map of shapes")
}

fn assert_arrived_as_electron_delivers(actual: &BTreeMap<String, Value>) {
    let mut wrong = Vec::new();
    for (name, want) in &expected() {
        let (mut want, mut got) = (
            want.clone(),
            actual.get(name).cloned().unwrap_or(Value::Null),
        );
        sorted_own(&mut want);
        sorted_own(&mut got);
        if got != want {
            wrong.push(format!("{name}:\n  wanted {want}\n  got    {got}"));
        }
    }
    assert!(wrong.is_empty(), "{}", wrong.join("\n"));
    assert_eq!(actual.len(), expected().len());
}

#[test]
fn every_kind_of_value_a_page_sends_arrives_as_electron_delivers_it() {
    let ipc = Ipc::new(
        "globalThis.got = {}; electron.ipcMain.on('clone', (event, name, value) => { got[name] = shape(value); }); \
         electron.ipcMain.handle('flush', () => 1);",
    );
    let refused = ipc.run(
        r"
const refused = {};
for (const [name, value] of Object.entries(samples())) {
  try { ipc.send('clone', name, value); } catch (e) { refused[name] = [e.constructor.name, e.message]; }
}
await ipc.invoke('flush');
return refused;",
    );
    // `flush` is answered after everything the page sent before it.
    let expected_refused: BTreeMap<String, Value> = REFUSED
        .iter()
        .map(|name| {
            (
                (*name).to_owned(),
                json!(["Error", "An object could not be cloned."]),
            )
        })
        .collect();
    assert_eq!(
        serde_json::from_value::<BTreeMap<String, Value>>(refused).expect("a map"),
        expected_refused
    );
    ipc.until(&format!("Object.keys(got).length === {}", expected().len()));
    assert_arrived_as_electron_delivers(&arrived(&ipc, "main"));
}

#[test]
fn every_kind_of_value_the_main_process_sends_arrives_as_electron_delivers_it() {
    let ipc = Ipc::new("");
    ipc.page("globalThis.got = {}; ipc.on('clone', (event, name, value) => { got[name] = shape(value); }); 1");
    // Electron 39.8.10: a value that cannot be cloned is logged, not thrown, one line each.
    ipc.main(
        "for (const [name, value] of Object.entries(samples())) { win.webContents.send('clone', name, value); } 1",
    );
    assert!(ipc.app.run_until(
        |_| ipc.page("Object.keys(got).length") == json!(expected().len()),
        crate::TIMEOUT
    ));
    assert_arrived_as_electron_delivers(&arrived(&ipc, "page"));
    let failures = ipc
        .app
        .console()
        .iter()
        .filter(|l| l.starts_with("Error sending from webFrameMain:"))
        .count();
    assert_eq!(failures, REFUSED.len());
}

/// An `invoke`'s answer and a `sendSync`'s `returnValue` are cloned the same way.
#[test]
fn an_answer_is_cloned_as_a_message_is() {
    let ipc = Ipc::new(
        r"
electron.ipcMain.handle('echo', (event, name) => samples()[name]);
electron.ipcMain.on('echo-sync', (event, name) => { event.returnValue = samples()[name]; });
",
    );
    let names = [
        "error",
        "errorWithProps",
        "customError",
        "errorWithCause",
        "uint8Window",
        "arrayBuffer",
        "map",
        "regexp",
        "date",
        "nullProto",
        "instance",
        "circular",
    ];
    let both = ipc.run(&format!(
        "const out = {{}}; for (const name of {names:?}) {{ out[name] = [shape(await ipc.invoke('echo', name)), shape(ipc.sendSync('echo-sync', name))]; }} return out"
    ));
    let expected = expected();
    for name in names {
        let (mut got, mut want) = (both[name].clone(), json!([expected[name], expected[name]]));
        sorted_own(&mut got);
        sorted_own(&mut want);
        assert_eq!(got, want, "{name}");
    }
}

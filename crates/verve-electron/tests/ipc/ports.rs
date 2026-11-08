//! `MessageChannelMain` and `MessagePortMain`, and ports crossing between the main process and
//! a page, held to what Electron 39.8.10 did (`tests/electron/ipc/observe`).

use std::time::Duration;

use serde_json::json;

use crate::Ipc;

/// Electron 39.8.10: an `EventEmitter` with `port1` and `port2`; a port has `postMessage`,
/// `start` and `close`, is not started by listening, and hears `{ data, ports }`.
#[test]
fn a_port_hears_nothing_until_it_is_started() {
    let ipc = Ipc::new(
        r"
const one = new electron.MessageChannelMain();
globalThis.shapeOfChannel = {
  ctor: one.constructor.name, keys: Object.keys(one), port1: one.port1.constructor.name,
  emitter: one.port1 instanceof require('node:events'),
  methods: Object.getOwnPropertyNames(Object.getPrototypeOf(one.port1)).sort(),
  without: (() => { try { electron.MessageChannelMain(); } catch (e) { return [e.constructor.name, e.message]; } })(),
};
globalThis.heard = [];
one.port2.on('message', (e) => heard.push({ keys: Object.keys(e), data: shape(e.data), ports: shape(e.ports) }));
one.port1.postMessage({ a: 1 });
globalThis.one = one;
",
    );
    assert_eq!(
        ipc.main("shapeOfChannel"),
        json!({
            "ctor": "MessageChannelMain", "keys": ["_events", "_eventsCount", "_maxListeners", "port1", "port2"],
            "port1": "MessagePortMain", "emitter": true, "methods": ["close", "constructor", "postMessage", "start"],
            "without": ["TypeError", "Class constructor MessageChannelMain cannot be invoked without 'new'"]
        })
    );
    std::thread::sleep(Duration::from_millis(200));
    assert_eq!(ipc.main("heard.length"), json!(0));
    ipc.main("one.port2.start(); 1");
    ipc.until("heard.length === 1");
    assert_eq!(
        ipc.main("heard[0]"),
        json!({
            "keys": ["data", "ports"],
            "data": { "ctor": "Object", "tag": "Object", "keys": ["a"], "props": { "a": 1 }, "symbols": 0 },
            "ports": { "ctor": "Array", "length": 0, "items": [], "extra": [] }
        })
    );
}

/// Electron 39.8.10: a port can travel along a port; what cannot be in a transfer list says so;
/// a closed or transferred port ignores every call.
#[test]
fn ports_travel_along_ports_and_bad_transfers_are_refused_in_electrons_words() {
    let ipc = Ipc::new(
        r"
const one = new electron.MessageChannelMain();
const inner = new electron.MessageChannelMain();
globalThis.carried = [];
one.port2.on('message', (e) => carried.push(e.ports.map((p) => p.constructor.name)));
one.port2.start();
one.port1.postMessage('with-port', [inner.port1]);
globalThis.misc = {
  object: attempt(() => one.port1.postMessage('x', [{}])),
  number: attempt(() => one.port1.postMessage('x', [1])),
  string: attempt(() => one.port1.postMessage('x', 'str')),
  arrayBuffer: attempt(() => one.port1.postMessage('x', [new ArrayBuffer(1)])),
  noArguments: attempt(() => one.port1.postMessage()),
  function: attempt(() => one.port1.postMessage(() => 1)),
  afterTransfer: attempt(() => inner.port1.postMessage('after')),
};
",
    );
    // The second message is the `postMessage()` of the list below, which sends `undefined`.
    ipc.until("carried.length === 2");
    assert_eq!(ipc.main("carried"), json!([["MessagePortMain"], []]));
    let bad = |message: &str| json!({ "threw": ["TypeError", message] });
    assert_eq!(
        ipc.main("misc"),
        json!({
            "object": bad("Port at index 0 is not a valid port"),
            "number": bad("Port at index 0 is not a valid port"),
            "string": bad("transferables must be an array of MessagePorts"),
            "arrayBuffer": bad("Port at index 0 is not a valid port"),
            "noArguments": { "returned": "#undefined" },
            "function": { "threw": ["Error", "An object could not be cloned."] },
            "afterTransfer": { "returned": "#undefined" }
        })
    );
}

/// Electron 39.8.10: closing a port emits `'close'` on it; the other end hears it too, once it
/// is started; everything is ignored on a closed port.
#[test]
fn closing_a_port_is_heard_by_it_and_by_a_started_peer() {
    let ipc = Ipc::new(
        r"
const two = new electron.MessageChannelMain();
globalThis.closes = [];
two.port1.on('close', () => closes.push('port1'));
two.port2.on('close', () => closes.push('port2'));
two.port1.close();
const started = new electron.MessageChannelMain();
globalThis.startedCloses = [];
started.port1.on('close', () => startedCloses.push('port1'));
started.port2.on('close', () => startedCloses.push('port2'));
started.port2.start();
started.port1.close();
globalThis.afterClose = [attempt(() => two.port1.postMessage('x')), attempt(() => two.port2.postMessage('x')),
  attempt(() => two.port1.close()), attempt(() => two.port1.start())];
",
    );
    ipc.until("startedCloses.length === 2");
    std::thread::sleep(Duration::from_millis(200));
    // Electron 39.8.10: the closing end hears its own close; the unstarted peer hears nothing.
    assert_eq!(ipc.main("closes"), json!(["port1"]));
    let mut started = ipc
        .main("startedCloses")
        .as_array()
        .cloned()
        .unwrap_or_default();
    started.sort_by_key(|v| v.as_str().map(str::to_owned));
    assert_eq!(started, vec![json!("port1"), json!("port2")]);
    let nothing = json!({ "returned": "#undefined" });
    assert_eq!(
        ipc.main("afterClose"),
        json!([nothing, nothing, nothing, nothing])
    );
}

/// Electron 39.8.10: a port given to a page arrives as a `MessagePort` in `event.ports`, talks
/// both ways, the page closing it is heard, and the main process's copy is neutered.
#[test]
fn a_port_given_to_the_page_talks_both_ways() {
    let ipc = Ipc::new(
        r"
const down = new electron.MessageChannelMain();
globalThis.fromPage = []; globalThis.closed = false; globalThis.afterTransfer = null;
down.port2.on('message', (e) => fromPage.push({ data: shape(e.data), ports: shape(e.ports) }));
down.port2.on('close', () => { globalThis.closed = true; });
down.port2.start();
globalThis.down = down;
",
    );
    ipc.page(
        r"
globalThis.pageGot = [];
ipc.on('port-down', (event, message) => {
  const port = event.ports[0];
  globalThis.portInfo = { message: shape(message), isMessagePort: port instanceof MessagePort, ctor: port.constructor.name,
    ports: event.ports.length };
  port.onmessage = (e) => { pageGot.push({ data: shape(e.data), keys: Object.keys(e) }); port.close(); };
  port.postMessage({ from: 'page' });
});
1",
    );
    ipc.main("win.webContents.postMessage('port-down', { greeting: 'hello' }, [down.port1]); 1");
    ipc.until("fromPage.length === 1");
    ipc.main("down.port2.postMessage({ from: 'main' }); 1");
    assert!(
        ipc.app
            .run_until(|_| ipc.page("pageGot.length") == json!(1), crate::TIMEOUT)
    );
    ipc.until("closed === true");
    let object = |key: &str, value: serde_json::Value| json!({ "ctor": "Object", "tag": "Object", "keys": [key], "props": { key: value }, "symbols": 0 });
    assert_eq!(
        ipc.page("portInfo"),
        json!({ "message": object("greeting", json!("hello")), "isMessagePort": true, "ctor": "MessagePort", "ports": 1 })
    );
    assert_eq!(ipc.main("fromPage[0].data"), object("from", json!("page")));
    assert_eq!(ipc.page("pageGot[0].data"), object("from", json!("main")));
    // The main process's copy was given away: Electron ignores calls on it, and refuses a second gift.
    let again = ipc.main(
        r"
[attempt(() => down.port1.postMessage('x')), attempt(() => down.port1.start()), attempt(() => down.port1.close()),
 attempt(() => win.webContents.postMessage('port-down', 1, [down.port1]))]",
    );
    assert_eq!(
        again,
        json!([
            { "returned": "#undefined" },
            { "returned": "#undefined" },
            { "returned": "#undefined" },
            { "threw": ["Error", "Port at index 0 is already neutered."] }
        ])
    );
}

/// Electron 39.8.10: the page gives the main process one end of a `MessageChannel` with
/// `ipcRenderer.postMessage`; it is a `MessagePortMain` in `event.ports`, and the one the page
/// kept talks to it.
#[test]
fn a_port_given_to_the_main_process_talks_both_ways() {
    let ipc = Ipc::new(
        r"
globalThis.got = {};
electron.ipcMain.on('post-plain', (event, message) => {
  globalThis.got.plain = { keys: Object.keys(event), ports: shape(event.ports), message: shape(message) };
});
electron.ipcMain.on('port-up', (event, message) => {
  const port = event.ports[0];
  globalThis.got.up = { keys: Object.keys(event), message: shape(message), ports: event.ports.length, ctor: port.constructor.name };
  port.on('message', (e) => { globalThis.got.fromPage = shape(e.data); });
  port.start();
  port.postMessage('hello-from-main');
});
",
    );
    let outcome = ipc.run(
        r"
ipc.postMessage('post-plain', 'x');
const { port1, port2 } = new MessageChannel();
globalThis.fromMain = undefined;
port1.onmessage = (e) => { globalThis.fromMain = e.data; };
ipc.postMessage('port-up', { with: 'port' }, [port2]);
const after = await both(() => port2.postMessage('x'));
const again = await both(() => ipc.postMessage('port-up', 1, [port2]));
port1.postMessage('page-to-main');
return { after, again };",
    );
    assert_eq!(
        outcome,
        json!({
            "after": {},
            "again": { "threw": ["TypeError", "Invalid value for transfer"] }
        })
    );
    ipc.until("got.fromPage !== undefined");
    assert!(ipc.app.run_until(
        |_| ipc.page("fromMain") == json!("hello-from-main"),
        crate::TIMEOUT
    ));
    assert_eq!(ipc.main("got.fromPage"), json!("page-to-main"));
    let keys = json!([
        "type",
        "sender",
        "senderFrame",
        "frameId",
        "processId",
        "frameTreeNodeId",
        "ports"
    ]);
    assert_eq!(
        ipc.main("got.plain"),
        json!({ "keys": keys, "ports": { "ctor": "Array", "length": 0, "items": [], "extra": [] }, "message": "x" })
    );
    let up = ipc.main("got.up");
    assert_eq!(up["keys"], keys);
    assert_eq!(
        (up["ports"].clone(), up["ctor"].clone()),
        (json!(1), json!("MessagePortMain"))
    );
}

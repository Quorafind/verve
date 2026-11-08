//! `send`, `invoke`/`handle`, `sendSync`, `webContents.send` and the errors around them, held to
//! what Electron 39.8.10 did (`tests/electron/ipc/observe`).

use serde_json::json;

use crate::Ipc;

/// Electron 39.8.10: `ipc-message`, then the frame's `ipc`, then the contents' `ipc`, then
/// `ipcMain`; the event has these fields in this order, `ports` is not one of them, and
/// `senderFrame` is the contents' `mainFrame`.
#[test]
fn a_message_is_heard_in_electrons_order_by_an_event_with_electrons_fields() {
    let ipc = Ipc::new(
        r"
const order = (globalThis.order = []);
const contents = win.webContents;
contents.on('ipc-message', (e, channel) => channel === 'order' && order.push('webContents ipc-message'));
contents.on('ipc-message-sync', (e, channel) => channel === 'order-sync' && order.push('ipc-message-sync'));
contents.mainFrame.ipc.on('order', () => order.push('frame.ipc'));
contents.ipc.on('order', () => order.push('webContents.ipc'));
contents.mainFrame.ipc.on('order-sync', () => order.push('frame.ipc sync'));
contents.ipc.on('order-sync', () => order.push('webContents.ipc sync'));
electron.ipcMain.on('order', (event, ...args) => {
  order.push('ipcMain');
  globalThis.heard = {
    keys: Object.keys(event), type: event.type, frameId: event.frameId,
    processId: typeof event.processId, sender: event.sender === contents,
    frame: event.senderFrame === contents.mainFrame, ports: typeof event.ports,
    reply: typeof event.reply, returnValue: 'returnValue' in event, args: shape(args),
  };
});
electron.ipcMain.on('order-sync', (event) => {
  order.push('ipcMain sync');
  globalThis.syncKeys = Object.keys(event);
  globalThis.syncNames = Object.getOwnPropertyNames(event).includes('returnValue');
  event.returnValue = 'ok';
});
",
    );
    ipc.page("ipc.send('order', 1, 'two'); 1");
    ipc.until("order.length === 4");
    assert_eq!(
        ipc.main("order"),
        json!([
            "webContents ipc-message",
            "frame.ipc",
            "webContents.ipc",
            "ipcMain"
        ])
    );
    let keys = json!([
        "type",
        "sender",
        "senderFrame",
        "frameId",
        "processId",
        "frameTreeNodeId",
        "reply"
    ]);
    assert_eq!(
        ipc.main("heard"),
        json!({
            "keys": keys, "type": "frame", "frameId": 1, "processId": "number", "sender": true,
            "frame": true, "ports": "undefined", "reply": "function", "returnValue": false,
            "args": { "ctor": "Array", "length": 2, "items": [1, "two"], "extra": [] }
        })
    );

    ipc.main("order.length = 0; 1");
    assert_eq!(ipc.page("ipc.sendSync('order-sync')"), json!("ok"));
    assert_eq!(
        ipc.main("order"),
        json!([
            "ipc-message-sync",
            "frame.ipc sync",
            "webContents.ipc sync",
            "ipcMain sync"
        ])
    );
    // A `sendSync` event also has `returnValue`, which is not listed among its keys.
    assert_eq!(ipc.main("syncKeys"), keys);
    assert_eq!(ipc.main("syncNames"), json!(true));
}

/// Electron 39.8.10: a page's listener is called with an event whose keys are `sender` and
/// `ports`, an empty array; `event.reply` is a message back to the sender.
#[test]
fn a_reply_reaches_the_page_as_a_message_with_electrons_event() {
    let ipc = Ipc::new(
        "electron.ipcMain.on('ping', (event, a, b) => { globalThis.returned = event.reply('pong', a + b, 2); });",
    );
    ipc.page(
        "globalThis.got = null; ipc.on('pong', (event, ...args) => { globalThis.got = { keys: Object.keys(event), \
         sender: event.sender === ipc, ports: shape(event.ports), senderId: typeof event.senderId, args }; }); \
         ipc.send('ping', 1, 2); 1",
    );
    assert!(
        ipc.app
            .run_until(|_| ipc.page("got") != json!(null), crate::TIMEOUT)
    );
    assert_eq!(
        ipc.page("got"),
        json!({
            "keys": ["sender", "ports"], "sender": true, "senderId": "undefined", "args": [3, 2],
            "ports": { "ctor": "Array", "length": 0, "items": [], "extra": [] }
        })
    );
    assert_eq!(ipc.main("returned"), json!(null));
}

#[test]
fn invoke_resolves_with_what_a_handler_returns_or_its_promise_settles_with() {
    let ipc = Ipc::new(
        r"
electron.ipcMain.handle('add', (event, a, b) => a + b);
electron.ipcMain.handle('later', async (event, ms) => { await new Promise((r) => setTimeout(r, ms)); return 'late'; });
electron.ipcMain.handle('keys', (event) => ({ keys: Object.keys(event), reply: typeof event.reply, type: event.type,
  sender: event.sender === win.webContents, named: ['ports', 'returnValue'].map((k) => k in event) }));
",
    );
    let page = ipc.run(
        "return [await ipc.invoke('add', 20, 22), await ipc.invoke('later', 20), await ipc.invoke('keys')]",
    );
    // Electron 39.8.10: an invoke's event has no `reply`, `ports` or `returnValue`.
    let keys = json!({ "keys": ["type", "sender", "senderFrame", "frameId", "processId", "frameTreeNodeId"],
        "reply": "undefined", "type": "frame", "sender": true, "named": [false, false] });
    assert_eq!(page, json!([42, "late", keys]));
}

/// Electron 39.8.10, every line: what an `invoke` is rejected with is `Error invoking remote
/// method '<channel>': ` and `String(error)` of what the handler threw, and the main process
/// logs `Error occurred in handler for '<channel>':` with the error.
#[test]
fn a_handler_that_fails_rejects_the_invoke_with_the_text_of_its_error() {
    let ipc = Ipc::new(
        r"
class MyError extends Error { constructor(m) { super(m); this.name = 'MyError'; } }
const ranged = new RangeError('ranged'); ranged.code = 'E_RANGE';
const fails = {
  'throws-error': () => { throw ranged; },
  'throws-string': () => { throw 'just text'; },
  'throws-object': () => { throw { a: 1 }; },
  'throws-null': () => { throw null; },
  'throws-number': () => { throw 42; },
  'throws-custom': () => { throw new MyError('mine'); },
  'throws-empty': () => { throw new Error(); },
  rejects: async () => { throw new TypeError('async boom'); },
  'returns-function': () => () => 1,
  'returns-promise-function': async () => () => 1,
  'returns-symbol': () => Symbol('s'),
};
for (const [channel, fn] of Object.entries(fails)) electron.ipcMain.handle(channel, fn);
",
    );
    let channels = [
        ("throws-error", "RangeError: ranged"),
        ("throws-string", "just text"),
        ("throws-object", "[object Object]"),
        ("throws-null", "reply was never sent"),
        ("throws-number", "42"),
        ("throws-custom", "MyError: mine"),
        ("throws-empty", "Error"),
        ("rejects", "TypeError: async boom"),
        ("returns-function", "Error: An object could not be cloned."),
        (
            "returns-promise-function",
            "Error: An object could not be cloned.",
        ),
        ("returns-symbol", "Error: An object could not be cloned."),
        (
            "no-such-channel",
            "Error: No handler registered for 'no-such-channel'",
        ),
    ];
    let names: Vec<_> = channels.iter().map(|(name, _)| *name).collect();
    let outcome = ipc.run(&format!(
        "const out = {{}}; for (const channel of {names:?}) out[channel] = await both(() => ipc.invoke(channel)); return out"
    ));
    for (channel, text) in channels {
        let message = format!("Error invoking remote method '{channel}': {text}");
        assert_eq!(
            outcome[channel],
            json!({ "rejected": ["Error", message] }),
            "{channel}"
        );
    }
    let logged = ipc.app.console();
    let said = |line: &str| logged.iter().any(|l| l.starts_with(line));
    assert!(
        said("Error occurred in handler for 'throws-error': RangeError: ranged"),
        "{logged:?}"
    );
    assert!(said(
        "Error occurred in handler for 'throws-string': just text"
    ));
    assert!(said(
        "Error occurred in handler for 'throws-object': { a: 1 }"
    ));
    assert!(said("Error occurred in handler for 'throws-null': null"));
    assert!(said("Error occurred in handler for 'throws-number': 42"));
    assert!(said(
        "Error occurred in handler for 'throws-custom': MyError: mine"
    ));
    assert!(said(
        "Error occurred in handler for 'rejects': TypeError: async boom"
    ));
    assert!(said(
        "Error occurred in handler for 'returns-function': Error: An object could not be cloned."
    ));
    assert!(said(
        "Error occurred in handler for 'no-such-channel': Error: No handler registered for 'no-such-channel'"
    ));
}

/// Electron 39.8.10: the frame's `ipc` first, then the contents', then `ipcMain`; a listener
/// (`on`) is not a handler; `handleOnce` answers once; the first handler registered stays.
#[test]
fn the_first_handler_of_the_frame_the_contents_and_ipc_main_answers_an_invoke() {
    let ipc = Ipc::new(
        r"
const contents = win.webContents;
electron.ipcMain.on('prio', () => { globalThis.listened = true; });
contents.ipc.on('only-listener', () => {});
const frame = () => contents.mainFrame.ipc.removeHandler('prio');
contents.mainFrame.ipc.handle('prio', () => 'frame.ipc');
contents.ipc.handle('prio', () => 'webContents.ipc');
electron.ipcMain.handle('prio', () => 'ipcMain');
const steps = [frame, () => contents.ipc.removeHandler('prio'), () => electron.ipcMain.removeHandler('prio')];
electron.ipcMain.handle('step', () => steps.shift()());
electron.ipcMain.handleOnce('once', () => 'first');
electron.ipcMain.handle('dup', () => 1);
",
    );
    let outcome = ipc.run(
        r"
const prio = [];
for (let at = 0; at < 4; at++) { prio.push(await both(() => ipc.invoke('prio'))); if (at < 3) await ipc.invoke('step'); }
return { prio, only: await both(() => ipc.invoke('only-listener')),
  once: [await both(() => ipc.invoke('once')), await both(() => ipc.invoke('once'))] };",
    );
    let nobody = |channel: &str| json!({ "rejected": ["Error", format!("Error invoking remote method '{channel}': Error: No handler registered for '{channel}'")] });
    assert_eq!(
        outcome,
        json!({
            "prio": [{ "resolved": "frame.ipc" }, { "resolved": "webContents.ipc" }, { "resolved": "ipcMain" }, nobody("prio")],
            "only": nobody("only-listener"),
            "once": [{ "resolved": "first" }, nobody("once")]
        })
    );
    assert_eq!(ipc.main("globalThis.listened"), json!(null));
}

/// Electron 39.8.10.
#[test]
fn registering_handlers_fails_as_electrons_does() {
    let ipc = Ipc::new("");
    let outcome = ipc.main(
        r"
const { ipcMain } = electron;
({
  twice: attempt(() => { ipcMain.handle('dup', () => 1); ipcMain.handle('dup', () => 2); }),
  nonString: attempt(() => ipcMain.handle(5, () => 1)),
  nonFunction: attempt(() => ipcMain.handle('bad', 5)),
  noArguments: attempt(() => ipcMain.handle()),
  removeMissing: attempt(() => ipcMain.removeHandler('never-registered')),
  onceTwice: attempt(() => { ipcMain.handleOnce('dup-once', () => 1); ipcMain.handleOnce('dup-once', () => 2); }),
  returnsSelf: ipcMain.on('x', () => {}) === ipcMain,
  classes: [ipcMain.constructor.name, win.webContents.ipc.constructor.name, win.webContents.mainFrame.ipc.constructor.name,
    win.webContents.mainFrame.constructor.name, new Set([ipcMain, win.webContents.ipc, win.webContents.mainFrame.ipc]).size],
})",
    );
    assert_eq!(
        outcome,
        json!({
            "twice": { "threw": ["Error", "Attempted to register a second handler for 'dup'"] },
            "nonString": { "returned": "#undefined" },
            "nonFunction": { "threw": ["TypeError", "Expected handler to be a function, but found type 'number'"] },
            "noArguments": { "threw": ["TypeError", "Expected handler to be a function, but found type 'undefined'"] },
            "removeMissing": { "returned": "#undefined" },
            "onceTwice": { "threw": ["Error", "Attempted to register a second handler for 'dup-once'"] },
            "returnsSelf": true,
            "classes": ["IpcMainImpl", "IpcMainImpl", "IpcMainImpl", "WebFrameMain", 3]
        })
    );
}

/// Electron 39.8.10: native-binding errors, with the channel at argument index 1, for what the
/// page gets wrong; a value that cannot be cloned is `Error: An object could not be cloned.`
#[test]
fn the_page_is_told_what_it_got_wrong_the_way_electrons_bindings_tell_it() {
    let ipc = Ipc::new("");
    let outcome = ipc.run(
        r"
const bad = (index, from) => ({ threw: ['TypeError', `Error processing argument at index ${index}, conversion failure from ${from}`] });
const rejectedBad = (index, from) => ({ rejected: ['TypeError', `Error processing argument at index ${index}, conversion failure from ${from}`] });
const clone = ['Error', 'An object could not be cloned.'];
return {
  sendNone: await both(() => ipc.send()), sendNumber: await both(() => ipc.send(5)),
  sendFunction: await both(() => ipc.send('x', () => 1)),
  invokeNone: await both(() => ipc.invoke()), invokeNumber: await both(() => ipc.invoke(5)),
  invokeFunction: await both(() => ipc.invoke('x', () => 1)),
  sendSyncNone: await both(() => ipc.sendSync()), sendSyncFunction: await both(() => ipc.sendSync('x', () => 1)),
  postMessageNone: await both(() => ipc.postMessage()),
  transfer: [await both(() => ipc.postMessage('x', 1, [1])), await both(() => ipc.postMessage('x', 1, 'str')),
    await both(() => ipc.postMessage('x', 1, [new ArrayBuffer(1)]))],
  sendToHost: await both(() => ipc.sendToHost('x', 1)),
  members: [typeof ipc.sendTo, ipc.constructor.name, Object.keys(ipc)],
  expected: { bad: bad(1, 'undefined'), rejectedBad: rejectedBad(1, 'undefined') },
};",
    );
    let threw = |index: u32, from: &str| json!({ "threw": ["TypeError", format!("Error processing argument at index {index}, conversion failure from {from}")] });
    let rejected = |index: u32, from: &str| json!({ "rejected": ["TypeError", format!("Error processing argument at index {index}, conversion failure from {from}")] });
    let cloned = json!({ "threw": ["Error", "An object could not be cloned."] });
    let invalid = json!({ "threw": ["TypeError", "Invalid value for transfer"] });
    assert_eq!(outcome["sendNone"], threw(1, "undefined"));
    assert_eq!(outcome["sendNumber"], threw(1, ""));
    assert_eq!(outcome["sendFunction"], cloned);
    assert_eq!(outcome["invokeNone"], rejected(1, "undefined"));
    assert_eq!(outcome["invokeNumber"], rejected(1, ""));
    assert_eq!(
        outcome["invokeFunction"],
        json!({ "rejected": ["Error", "An object could not be cloned."] })
    );
    assert_eq!(outcome["sendSyncNone"], threw(1, "undefined"));
    assert_eq!(outcome["sendSyncFunction"], cloned);
    assert_eq!(outcome["postMessageNone"], threw(0, "undefined"));
    assert_eq!(outcome["transfer"], json!([invalid, invalid, invalid]));
    // `sendToHost` does nothing without a `<webview>` host, and `sendTo` is gone since Electron 28.
    assert_eq!(outcome["sendToHost"], json!({}));
    assert_eq!(
        outcome["members"],
        json!([
            "undefined",
            "IpcRenderer",
            ["_events", "_eventsCount", "_maxListeners"]
        ])
    );
}

/// Electron 39.8.10: `sendSync` blocks the page until `returnValue` is set — also from a later
/// callback — and the first assignment wins.
#[test]
fn send_sync_waits_for_the_return_value_whenever_it_is_set() {
    let ipc = Ipc::new(
        r"
electron.ipcMain.on('x2', (event, n) => { event.returnValue = n * 2; });
electron.ipcMain.on('late', (event) => { setTimeout(() => { event.returnValue = 'set later'; }, 100); });
electron.ipcMain.on('twice', (event) => { event.returnValue = 'first'; event.returnValue = 'second'; });
electron.ipcMain.on('bad', (event) => {
  try { event.returnValue = () => 1; } catch (e) { globalThis.assigned = [e.constructor.name, e.message]; }
});
",
    );
    let started = std::time::Instant::now();
    let outcome = ipc.page("[ipc.sendSync('x2', 21), ipc.sendSync('late'), ipc.sendSync('twice')]");
    assert_eq!(outcome, json!([42, "set later", "first"]));
    assert!(started.elapsed() >= std::time::Duration::from_millis(100));
    // Electron 39.8.10: a value that cannot be cloned throws in the listener and leaves the page
    // waiting until the event is collected; then it is told, as for any reply that never comes,
    // `{ error: 'reply was never sent' }` — a value, not an error. Verve looks for the event at
    // its next sweep, within about 100 ms of the listeners and their microtasks being done
    // (Electron's page hangs until the collector happens to run).
    assert_eq!(
        ipc.page("shape(ipc.sendSync('bad'))")["props"],
        json!({ "error": "reply was never sent" })
    );
    assert_eq!(
        ipc.main("assigned"),
        json!(["Error", "An object could not be cloned."])
    );
}

/// Electron 39.8.10: without a listener the main process warns, and the page is told
/// `{ error: 'reply was never sent' }`; an event nobody answers but something still holds keeps
/// the page waiting.
#[test]
fn send_sync_without_a_listener_warns_and_answers_that_no_reply_came() {
    let ipc = Ipc::new(
        r"
electron.ipcMain.on('unset', () => {});
electron.ipcMain.on('held', (event) => { globalThis.held = event; });
globalThis.arrived = [];
electron.ipcMain.on('before', () => arrived.push('before'));
electron.ipcMain.on('after', (event, value) => arrived.push('after:' + value));
",
    );
    let outcome = ipc.page("[ipc.sendSync('nobody-listens', 1), ipc.sendSync('unset')]");
    assert_eq!(
        outcome,
        json!([{ "error": "reply was never sent" }, { "error": "reply was never sent" }])
    );
    let id = ipc.page;
    let warning = format!(
        "WebContents #{id} called ipcRenderer.sendSync() with 'nobody-listens' channel without listeners."
    );
    assert!(
        ipc.app.console().contains(&warning),
        "{:?}",
        ipc.app.console()
    );
    assert!(
        !ipc.app
            .console()
            .iter()
            .any(|l| l.contains("'unset' channel without"))
    );

    // A held event is not let go: the page waits for it, and the reply can still be sent.
    ipc.page(
        "setTimeout(() => { ipc.send('before'); ipc.send('after', ipc.sendSync('held')); }, 0); 1",
    );
    ipc.until("globalThis.held !== undefined && arrived.length === 1");
    std::thread::sleep(std::time::Duration::from_millis(300));
    assert_eq!(ipc.main("arrived"), json!(["before"]));
    ipc.main("held.returnValue = 'now'; 1");
    ipc.until("arrived.length === 2");
    assert_eq!(ipc.main("arrived"), json!(["before", "after:now"]));
}

/// Electron 39.8.10.
#[test]
fn web_contents_send_and_send_to_frame_say_what_electron_says() {
    let ipc = Ipc::new("");
    ipc.page(
        "globalThis.heard = []; ipc.on('rcv', (event, ...args) => heard.push(shape(args))); 1",
    );
    let outcome = ipc.main(
        r"
const wc = win.webContents;
({
  send: attempt(() => wc.send('rcv', 1, 'two')),
  noChannel: attempt(() => wc.send()), numberChannel: attempt(() => wc.send(5)),
  toFrame: attempt(() => wc.sendToFrame(1, 'rcv', 'frame-1')),
  toFramePair: attempt(() => wc.sendToFrame([wc.mainFrame.processId, wc.mainFrame.frameTreeNodeId], 'rcv', 'frame-pair')),
  toOtherFrame: attempt(() => wc.sendToFrame(99999, 'rcv', 'other-frame')),
  mainFrameSend: attempt(() => wc.mainFrame.send('rcv', 'via-main-frame')),
  ids: [wc.mainFrame.frameTreeNodeId, wc.mainFrame.routingId, typeof wc.mainFrame.processId],
  badTransfer: [attempt(() => wc.postMessage('rcv', 1, [{}])), attempt(() => wc.postMessage('rcv', 1, [1])),
    attempt(() => wc.postMessage('rcv', 1, 'str')), attempt(() => wc.postMessage('rcv', 1, [new ArrayBuffer(1)])),
    attempt(() => wc.mainFrame.postMessage('rcv', 1, [{}]))],
  plainPost: attempt(() => wc.postMessage('rcv', { posted: 1 })),
})",
    );
    let invalid = json!({ "threw": ["Error", "Invalid value for transfer"] });
    assert_eq!(
        outcome,
        json!({
            "send": { "returned": "#undefined" },
            "noChannel": { "threw": ["TypeError", "Missing required channel argument"] },
            "numberChannel": { "threw": ["TypeError", "Missing required channel argument"] },
            "toFrame": { "returned": true }, "toFramePair": { "returned": true },
            "toOtherFrame": { "returned": false },
            "mainFrameSend": { "returned": "#undefined" },
            "ids": [1, 1, "number"],
            "badTransfer": [invalid, invalid, invalid, invalid, invalid],
            "plainPost": { "returned": "#undefined" }
        })
    );
    assert!(
        ipc.app
            .run_until(|_| ipc.page("heard.length") == json!(5), crate::TIMEOUT)
    );
    let plain = |value: serde_json::Value| json!({ "ctor": "Array", "length": 1, "items": [value], "extra": [] });
    let posted = json!({ "ctor": "Object", "tag": "Object", "keys": ["posted"], "props": { "posted": 1 }, "symbols": 0 });
    assert_eq!(
        ipc.page("heard"),
        json!([
            { "ctor": "Array", "length": 2, "items": [1, "two"], "extra": [] },
            plain(json!("frame-1")), plain(json!("frame-pair")), plain(json!("via-main-frame")), plain(posted)
        ])
    );
}

/// Electron 39.8.10: a message that cannot be cloned does not throw, it is logged; a destroyed
/// `webContents` throws on `send`, `sendToFrame`, `postMessage` and `mainFrame`, still has its
/// `id` and `ipc`, and `event.reply` to it is silent.
#[test]
fn web_contents_send_is_logged_when_it_cannot_clone_and_throws_when_destroyed() {
    let ipc = Ipc::new(
        "electron.ipcMain.on('hello', (event) => { globalThis.hello = event; }); \
         globalThis.keep = new electron.BrowserWindow({ show: false });",
    );
    ipc.main("win.webContents.send('rcv', () => 1); 1");
    let failure = "Error sending from webFrameMain:  Error: Failed to serialize arguments";
    let logged = ipc.app.console();
    assert!(
        logged.iter().any(|line| line.starts_with(failure)),
        "{logged:?}"
    );

    ipc.page("ipc.send('hello'); 1");
    ipc.until("globalThis.hello !== undefined");
    ipc.main(
        "globalThis.wc = win.webContents; globalThis.frame = hello.senderFrame; win.close(); 1",
    );
    ipc.until("(() => { try { wc.send('x'); return false; } catch (e) { return true; } })()");
    let outcome = ipc.main(
        r"
const event = hello;
({
  send: attempt(() => wc.send('x')), sendToFrame: attempt(() => wc.sendToFrame(1, 'x')),
  postMessage: attempt(() => wc.postMessage('x', 1)), mainFrame: attempt(() => typeof wc.mainFrame),
  id: attempt(() => wc.id), ipc: attempt(() => typeof wc.ipc),
  reply: attempt(() => event.reply('x', 1)), frameSend: attempt(() => frame.send('x')),
  senderFrame: attempt(() => event.senderFrame === null), eventSender: attempt(() => event.sender === wc),
})",
    );
    let destroyed = json!({ "threw": ["TypeError", "Object has been destroyed"] });
    assert_eq!(outcome["send"], destroyed);
    assert_eq!(outcome["sendToFrame"], destroyed);
    assert_eq!(outcome["postMessage"], destroyed);
    assert_eq!(outcome["mainFrame"], destroyed);
    assert_eq!(outcome["id"], json!({ "returned": ipc.page }));
    assert_eq!(outcome["ipc"], json!({ "returned": "object" }));
    assert_eq!(outcome["reply"], json!({ "returned": "#undefined" }));
    assert_eq!(outcome["frameSend"], json!({ "returned": "#undefined" }));
    // Electron 39.8.10: the sender's frame is `null` once it is gone; its contents is still the object.
    assert_eq!(outcome["senderFrame"], json!({ "returned": true }));
    assert_eq!(outcome["eventSender"], json!({ "returned": true }));
}

/// Sends, invokes and sendSyncs made in one turn arrive in the order they were made, and all of a
/// turn's messages are one wake-up of the main process.
#[test]
fn a_pages_messages_arrive_in_the_order_they_were_made() {
    let ipc = Ipc::new(
        r"
globalThis.arrived = [];
electron.ipcMain.on('seq', (event, i) => arrived.push('s' + i));
electron.ipcMain.on('seq-sync', (event, i) => { arrived.push('y' + i); event.returnValue = i; });
electron.ipcMain.handle('seq-invoke', (event, i) => { arrived.push('i' + i); return i; });
",
    );
    let sums = ipc.run(
        "const pending = []; for (let i = 0; i < 100; i++) { ipc.send('seq', i); pending.push(ipc.invoke('seq-invoke', i)); \
         if (i % 25 === 0) ipc.sendSync('seq-sync', i); } return (await Promise.all(pending)).length",
    );
    assert_eq!(sums, json!(100));
    let arrived = ipc.main("arrived");
    let arrived: Vec<&str> = arrived
        .as_array()
        .expect("a list")
        .iter()
        .filter_map(|v| v.as_str())
        .collect();
    let kinds = |kind: char| -> Vec<u32> {
        arrived
            .iter()
            .filter(|a| a.starts_with(kind))
            .map(|a| a[1..].parse().expect("a number"))
            .collect()
    };
    assert_eq!(kinds('s'), (0..100).collect::<Vec<_>>());
    assert_eq!(kinds('i'), (0..100).collect::<Vec<_>>());
    assert_eq!(kinds('y'), vec![0, 25, 50, 75]);
    // A sendSync takes everything sent before it along: `s0 i0 y0` and so on.
    assert_eq!(&arrived[..3], ["s0", "i0", "y0"]);
}

/// A page held in `sendSync` that nobody answers is let go when its window closes: the thread
/// that was blocked ends, which is what lets go of the `Hub` it held.
#[test]
fn closing_the_window_frees_a_page_that_is_waiting_for_a_return_value() {
    let ipc = Ipc::new(
        "electron.ipcMain.on('hang', (event) => { globalThis.hang = event; }); electron.ipcMain.on('ready', () => { globalThis.ready = true; }); globalThis.keep = new electron.BrowserWindow({ show: false });",
    );
    ipc.page("setTimeout(() => { ipc.send('ready'); ipc.sendSync('hang'); }, 0); 1");
    ipc.until("globalThis.ready === true && globalThis.hang !== undefined");
    let held = std::sync::Arc::strong_count(&ipc.app.hub);
    ipc.main("win.close(); 1");
    assert!(ipc.app.run_until(
        |app| std::sync::Arc::strong_count(&app.hub) < held,
        crate::TIMEOUT
    ));
    assert_eq!(ipc.main("1 + 1"), json!(2));
}

/// Electron's main process going away takes its pages with it: a page blocked in `sendSync`
/// when the app exits is woken (it is told `Main process exited`) and its thread ends.
#[test]
fn a_page_waiting_for_a_return_value_is_released_when_the_app_exits() {
    let ipc = Ipc::new(
        "electron.ipcMain.on('hang', (event) => { globalThis.hang = event; }); electron.ipcMain.on('ready', () => { setTimeout(() => electron.app.exit(0), 50); });",
    );
    ipc.page("setTimeout(() => { ipc.send('ready'); ipc.sendSync('hang'); }, 0); 1");
    assert_eq!(ipc.app.exit_code(crate::TIMEOUT), Some(0));
    assert!(ipc.app.run_until(
        |app| std::sync::Arc::strong_count(&app.hub) == 1,
        crate::TIMEOUT
    ));
}

/// What a page sends while it hears two messages in one go reaches the main process in the order
/// it was sent, `sendSync` included: the second listener's `sendSync` does not pass the first
/// one's `send`.
#[test]
fn a_send_sync_does_not_pass_the_sends_made_while_hearing_the_message_before() {
    let ipc = Ipc::new(
        "globalThis.arrived = []; \
         electron.ipcMain.on('first', () => arrived.push('first')); \
         electron.ipcMain.on('second', (event) => { arrived.push('second'); event.returnValue = 1; });",
    );
    // The page is busy for a while, so both messages wait in its inbox and are heard together.
    ipc.page(
        "ipc.on('a', () => ipc.send('first')); ipc.on('b', () => ipc.sendSync('second')); \
         setTimeout(() => { const until = Date.now() + 300; while (Date.now() < until); }, 0); 1",
    );
    ipc.main("setTimeout(() => { win.webContents.send('a'); win.webContents.send('b'); }, 100); 1");
    ipc.until("arrived.length === 2");
    assert_eq!(ipc.main("arrived"), json!(["first", "second"]));
}

/// Electron 39.8.10: a message with ports (`ipcRenderer.postMessage`) is not an `ipc-message` of
/// the contents; the frame's `ipc`, the contents' and `ipcMain` hear it.
#[test]
fn a_message_with_ports_is_not_an_ipc_message_of_the_contents() {
    let ipc = Ipc::new(
        r"
const heard = (globalThis.heard = []);
win.webContents.on('ipc-message', (e, channel) => channel === 'post-order' && heard.push('webContents ipc-message'));
win.webContents.mainFrame.ipc.on('post-order', () => heard.push('frame.ipc'));
win.webContents.ipc.on('post-order', () => heard.push('webContents.ipc'));
electron.ipcMain.on('post-order', () => heard.push('ipcMain'));
",
    );
    ipc.page("ipc.postMessage('post-order', 'x'); 1");
    ipc.until("heard.length === 3");
    assert_eq!(
        ipc.main("heard"),
        json!(["frame.ipc", "webContents.ipc", "ipcMain"])
    );
}

/// A page has a `require` only with `nodeIntegration`, without context isolation and without the
/// sandbox (Electron 39.8.10: the sandbox is off by default only with `nodeIntegration`, and a
/// sandboxed page has no Node whatever else it asks for).
#[test]
fn a_page_has_a_require_only_without_isolation_and_sandbox() {
    let kinds = [
        ("{}", "undefined"),
        ("{ nodeIntegration: true }", "undefined"),
        (
            "{ nodeIntegration: true, contextIsolation: false }",
            "function",
        ),
        (
            "{ nodeIntegration: true, contextIsolation: false, sandbox: true }",
            "undefined",
        ),
        (
            "{ nodeIntegration: true, contextIsolation: false, sandbox: false }",
            "function",
        ),
        ("{ contextIsolation: false, sandbox: false }", "undefined"),
    ];
    let windows: String = kinds
        .iter()
        .map(|(prefs, _)| {
            format!(
                "new BrowserWindow({{ show: false, webPreferences: {prefs} }});
"
            )
        })
        .collect();
    let app = crate::support::App::launch_script(
        &format!(
            "const {{ BrowserWindow }} = require('electron');
{windows}"
        ),
        &[],
    );
    assert!(app.run_until(
        |app| app.shell.windows().len() == kinds.len(),
        crate::TIMEOUT
    ));
    let seen: Vec<_> = app
        .shell
        .windows()
        .iter()
        .map(|w| app.eval_renderer(w.contents, "typeof require"))
        .collect();
    let expected: Vec<_> = kinds.iter().map(|(_, typ)| format!("\"{typ}\"")).collect();
    assert_eq!(seen, expected);
}

/// A second window with a page of its own, and the listeners both pages' `sendSync`s go to.
fn two_windows() -> (Ipc, u32) {
    let ipc = Ipc::new(
        r"
globalThis.arrived = [];
globalThis.other = new electron.BrowserWindow({ show: false, webPreferences: { nodeIntegration: true, contextIsolation: false } });
other.loadURL('data:text/html,<p>other</p>');
electron.ipcMain.on('unset', () => arrived.push('unset'));
electron.ipcMain.on('held', (event) => { globalThis.held = event; arrived.push('held'); });
electron.ipcMain.on('ping', () => arrived.push('ping'));
electron.ipcMain.on('done', (event, who) => arrived.push('done ' + who));
",
    );
    assert!(
        ipc.app
            .run_until(|app| app.shell.windows().len() == 2, crate::TIMEOUT)
    );
    let other = ipc.app.shell.windows()[1].contents;
    (ipc, other)
}

/// Another page's messages do not make the main process forget an event it is waiting to see let
/// go: the first page is answered whatever the second says in the meantime.
#[test]
fn a_page_waiting_on_an_unanswered_event_is_answered_whatever_another_page_says() {
    let (ipc, other) = two_windows();
    ipc.page("setTimeout(() => { ipc.sendSync('unset'); ipc.send('done', 'A'); }, 0); 1");
    ipc.until("arrived.includes('unset')");
    ipc.app
        .eval_renderer(other, "require('electron').ipcRenderer.send('ping'); 1");
    ipc.until("arrived.includes('done A')");
    assert_eq!(ipc.main("arrived"), json!(["unset", "ping", "done A"]));
}

/// An event a listener holds is the listener's to answer: it does not slow the pages whose events
/// were let go.
#[test]
fn an_event_that_is_held_does_not_hold_up_the_pages_whose_events_are_not() {
    let (ipc, other) = two_windows();
    ipc.page("setTimeout(() => { ipc.sendSync('held'); }, 0); 1");
    ipc.until("arrived.includes('held')");
    std::thread::sleep(std::time::Duration::from_millis(600));
    ipc.app.eval_renderer(
        other,
        "setTimeout(() => { const r = require('electron').ipcRenderer; r.sendSync('unset'); r.send('done', 'B'); }, 0); 1",
    );
    let began = std::time::Instant::now();
    ipc.until("arrived.includes('done B')");
    assert!(
        began.elapsed() < std::time::Duration::from_secs(2),
        "{:?}",
        began.elapsed()
    );
}

/// Electron 39.8.10: a port listed twice, or the port posted on, is refused in these words, and
/// nothing is taken; a closed or given-away port takes the call and does nothing at all.
#[test]
fn a_transfer_list_is_checked_whole_and_a_dead_port_does_nothing() {
    let ipc = Ipc::new(
        r"
const [a, b, closed, away, fresh1, fresh2] = Array.from({ length: 6 }, () => new electron.MessageChannelMain());
closed.port1.close();
win.webContents.postMessage('rcv', 1, [away.port1]);
globalThis.lists = {
  twice: attempt(() => a.port1.postMessage('x', [b.port1, b.port1])),
  itself: attempt(() => a.port1.postMessage('x', [a.port1])),
  twiceToPage: attempt(() => win.webContents.postMessage('rcv', 1, [b.port2, b.port2])),
  portAfterRefused: attempt(() => b.port2.postMessage('still usable')),
  closedCloneFails: attempt(() => closed.port1.postMessage(() => 1, [fresh1.port1])),
  freshAfterClosed: attempt(() => fresh1.port1.postMessage('usable')),
  neuteredCloneFails: attempt(() => away.port1.postMessage(() => 1, [fresh2.port1])),
  freshAfterNeutered: attempt(() => fresh2.port1.postMessage('usable')),
  closedBadList: attempt(() => closed.port1.postMessage('x', 'not a list')),
};
",
    );
    let lists = ipc.main("lists");
    let duplicate = json!({ "threw": ["Error", "Port at index 1 is a duplicate."] });
    let nothing = json!({ "returned": "#undefined" });
    assert_eq!(lists["twice"], duplicate);
    assert_eq!(lists["twiceToPage"], duplicate);
    assert_eq!(
        lists["itself"],
        json!({ "threw": ["Error", "Port at index 0 contains the source port."] })
    );
    for ok in [
        "portAfterRefused",
        "closedCloneFails",
        "freshAfterClosed",
        "neuteredCloneFails",
        "freshAfterNeutered",
        "closedBadList",
    ] {
        assert_eq!(lists[ok], nothing, "{ok}");
    }
}

/// What a page says to the main process leaves in the order it happened: a console line written
/// before a `send` is heard before it, so a main process that ends on the message still has it.
#[test]
fn a_console_line_is_heard_before_the_message_sent_after_it() {
    let ipc = Ipc::new("electron.ipcMain.on('done', () => electron.app.exit(0));");
    ipc.page("console.log('before done'); ipc.send('done'); 1");
    assert_eq!(ipc.app.exit_code(crate::TIMEOUT), Some(0));
    assert!(
        ipc.app.console().iter().any(|l| l == "before done"),
        "{:?}",
        ipc.app.console()
    );
}

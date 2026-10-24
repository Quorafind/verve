//! The web's events without a document — `EventTarget`, `Event` and its kin, `DOMException`,
//! `AbortController` — messages within the realm, `WebSocket` failing as an unreachable one
//! does, `structuredClone`, and `navigator`.

use std::cell::Cell;
use std::rc::Rc;

use verve::Runtime;

/// A runtime on a clock that moves only when told to.
fn runtime() -> (Rc<Cell<f64>>, Runtime) {
    let time = Rc::new(Cell::new(0.));
    let read = Rc::clone(&time);
    (time, Runtime::with_clock(move || read.get()))
}

/// Runs `script` (which fills the global array `log`), then the timers due within `ms`, and
/// answers `log`.
fn run(script: &str, ms: f64) -> Vec<String> {
    let (time, mut rt) = runtime();
    rt.eval(&format!("var log = [];\n{script}"))
        .unwrap_or_else(|e| panic!("{e}"));
    let mut now = 0.;
    while now <= ms {
        time.set(now);
        rt.run_due();
        now += 1.;
    }
    let text = rt.eval("JSON.stringify(log.map(String))").unwrap();
    serde_json::from_str(&verve::print(&text)).unwrap()
}

#[test]
fn event_targets_hear_their_listeners_once_each_in_order() {
    let log = run(
        r#"
        class Bus extends EventTarget { constructor() { super(); this.mine = 1; } }
        const bus = new Bus();
        const f = e => log.push('f ' + e.type + ' ' + (e.target === bus) + ' ' + (e.currentTarget === bus) + ' ' + e.eventPhase);
        bus.addEventListener('x', f);
        bus.addEventListener('x', f);
        bus.addEventListener('x', { handleEvent(e) { log.push('object ' + (this !== bus)); } });
        bus.addEventListener('x', () => log.push('once'), { once: true });
        bus.addEventListener('x', () => { throw new Error('boom'); });
        bus.addEventListener('x', () => log.push('capture first'), true);
        const first = bus.dispatchEvent(new Event('x'));
        bus.dispatchEvent(new Event('x'));
        bus.removeEventListener('x', f);
        bus.addEventListener('y', e => { log.push('stop'); e.stopImmediatePropagation(); });
        bus.addEventListener('y', () => log.push('never'));
        bus.dispatchEvent(new Event('y'));
        log.push(first, bus.mine, bus instanceof EventTarget, Object.prototype.toString.call(bus));
        "#,
        0.,
    );
    assert_eq!(
        log,
        [
            "capture first",
            "f x true true 2",
            "object true",
            "once",
            "capture first",
            "f x true true 2",
            "object true",
            "stop",
            "true",
            "1",
            "true",
            "[object EventTarget]",
        ]
    );
}

#[test]
fn events_cancel_carry_details_and_handlers_take_their_place() {
    let log = run(
        r#"
        const t = new EventTarget();
        t.addEventListener('c', e => e.preventDefault());
        const cancelable = new CustomEvent('c', { cancelable: true, detail: { n: 5 } });
        const plain = new Event('c');
        const passive = new Event('p', { cancelable: true });
        t.addEventListener('p', e => e.preventDefault(), { passive: true });
        log.push(t.dispatchEvent(cancelable), cancelable.defaultPrevented, cancelable.detail.n, t.dispatchEvent(plain),
                 t.dispatchEvent(passive), cancelable instanceof Event, cancelable.target === t, cancelable.eventPhase,
                 Event.AT_TARGET, plain.isTrusted, typeof plain.timeStamp);
        const m = new MessageEvent('message', { data: [1] });
        const c = new CloseEvent('close', { code: 1000, wasClean: true });
        const er = new ErrorEvent('error', { message: 'm', error: 4 });
        log.push(m.data[0], m.origin === '', c.code, c.wasClean, er.message, er.error);
        try { Event('x'); } catch (e) { log.push(e instanceof TypeError); }
        try { new Event(); } catch (e) { log.push('needs a type'); }
        try { t.dispatchEvent({ type: 'x' }); log.push('foreign heard'); } catch (e) { log.push(e.message); }
        "#,
        0.,
    );
    assert_eq!(
        log,
        [
            "false",
            "true",
            "5",
            "true",
            "true",
            "true",
            "true",
            "0",
            "2",
            "false",
            "number",
            "1",
            "true",
            "1000",
            "true",
            "m",
            "4",
            "true",
            "needs a type",
            "foreign heard",
        ]
    );
}

#[test]
fn dom_exceptions_are_errors_with_names_and_codes() {
    let log = run(
        r#"
        const d = new DOMException('gone', 'NotFoundError');
        const plain = new DOMException();
        const caused = new DOMException('m', { name: 'DataCloneError', cause: 1 });
        class Mine extends DOMException {}
        log.push(d.name, d.message, d.code, d instanceof Error, d instanceof DOMException, String(d),
                 Object.prototype.toString.call(d), plain.name, plain.code, caused.code, caused.cause,
                 DOMException.ABORT_ERR, d.TIMEOUT_ERR, typeof d.stack, new Mine('x', 'AbortError') instanceof Mine);
        "#,
        0.,
    );
    assert_eq!(
        log,
        [
            "NotFoundError",
            "gone",
            "8",
            "true",
            "true",
            "NotFoundError: gone",
            "[object DOMException]",
            "Error",
            "0",
            "25",
            "1",
            "20",
            "23",
            "string",
            "true",
        ]
    );
}

#[test]
fn abort_signals_are_event_targets() {
    let log = run(
        r#"
        const ac = new AbortController();
        const t = new EventTarget();
        t.addEventListener('x', () => log.push('until aborted'), { signal: ac.signal });
        t.dispatchEvent(new Event('x'));
        ac.signal.onabort = e => log.push('onabort ' + e.type);
        ac.signal.addEventListener('abort', () => log.push('listener'));
        const any = AbortSignal.any([ac.signal, new AbortController().signal]);
        any.addEventListener('abort', () => log.push('any ' + (any.reason === ac.signal.reason)));
        ac.abort();
        ac.abort();
        t.dispatchEvent(new Event('x'));
        log.push(ac.signal.aborted, ac.signal.reason.name, ac.signal.reason instanceof DOMException, ac.signal instanceof EventTarget);
        try { ac.signal.throwIfAborted(); } catch (e) { log.push('threw ' + e.name); }
        log.push(AbortSignal.abort('why').reason, AbortSignal.any([AbortSignal.abort(3)]).reason);
        try { new AbortSignal(); } catch (e) { log.push(e.message); }
        const late = AbortSignal.timeout(10);
        late.addEventListener('abort', () => log.push('timed out ' + late.reason.name));
        "#,
        20.,
    );
    assert_eq!(
        log,
        [
            "until aborted",
            "onabort abort",
            "listener",
            "any true",
            "true",
            "AbortError",
            "true",
            "true",
            "threw AbortError",
            "why",
            "3",
            "Illegal constructor",
            "timed out TimeoutError",
        ]
    );
}

#[test]
fn messages_cross_ports_and_channels_as_tasks() {
    let log = run(
        r#"
        const { port1, port2 } = new MessageChannel();
        port1.onmessage = e => log.push('port ' + JSON.stringify(e.data) + ' ' + (e instanceof MessageEvent));
        const sent = { a: [1, 2] };
        port2.postMessage(sent);
        sent.a.push(3);
        log.push('sent');
        // A port heard through addEventListener waits for start().
        const other = new MessageChannel();
        other.port2.addEventListener('message', e => log.push('started ' + e.data));
        other.port1.postMessage('queued');
        setTimeout(() => other.port2.start(), 5);
        const b1 = new BroadcastChannel('notes'), b2 = new BroadcastChannel('notes'), b3 = new BroadcastChannel('else');
        b1.onmessage = () => log.push('not to itself');
        b2.onmessage = e => log.push('broadcast ' + e.data + ' ' + b2.name);
        b3.onmessage = () => log.push('not to another name');
        b1.postMessage('hi');
        b2.close();
        b1.postMessage('after close');
        try { b2.postMessage('x'); } catch (e) { log.push(e.name); }
        "#,
        10.,
    );
    assert_eq!(
        log,
        [
            "sent",
            "InvalidStateError",
            "port {\"a\":[1,2]} true",
            "started queued"
        ]
    );
}

#[test]
fn a_broadcast_is_heard_before_the_channel_closes() {
    let log = run(
        r#"
        const b1 = new BroadcastChannel('n'), b2 = new BroadcastChannel('n');
        b2.onmessage = e => log.push('heard ' + e.data);
        b1.postMessage('hi');
        "#,
        1.,
    );
    assert_eq!(log, ["heard hi"]);
}

#[test]
fn a_websocket_fails_as_an_unreachable_server_does() {
    let (time, mut rt) = runtime();
    rt.eval(
        r#"
        var log = [];
        const ws = new WebSocket('wss://example.com/live');
        log.push(ws.readyState, ws.url, WebSocket.OPEN, ws.binaryType);
        try { ws.send('x'); } catch (e) { log.push(e.name); }
        try { new WebSocket('ftp://x'); } catch (e) { log.push(e.name); }
        try { ws.close(1234); } catch (e) { log.push(e.name); }
        ws.onopen = () => log.push('never opens');
        ws.onerror = e => log.push('error ' + e.type + ' ' + ws.readyState);
        ws.addEventListener('close', e => log.push('close ' + e.code + ' ' + e.wasClean));
        "#,
    )
    .unwrap();
    time.set(1.);
    rt.run_due();
    let log = verve::print(&rt.eval("log.join('|')").unwrap());
    assert_eq!(
        log,
        "0|wss://example.com/live|1|blob|InvalidStateError|SyntaxError|InvalidAccessError|error error 3|close 1006 false"
    );
    assert!(rt.take_console().iter().any(|l| {
        l.text
            .contains("WebSocket connection to 'wss://example.com/live' failed")
    }));
}

#[test]
fn structured_clone_keeps_kinds_shapes_and_cycles() {
    let log = run(
        r#"
        const buf = new ArrayBuffer(8);
        const a = new Uint16Array(buf, 2, 2); a[0] = 7;
        const src = {
            a, b: new Uint8Array(buf), dv: new DataView(buf, 1, 3), d: new Date(5), r: /x+/gi,
            m: new Map([[1, { k: 2 }]]), s: new Set(['q']), n: new Number(3), str: new String('s'), big: Object(10n),
            e: new RangeError('bad', { cause: 'c' }), sparse: [1, , 3], blob: new Blob(['hi'], { type: 'text/plain' }),
            file: new File(['x'], 'f.txt'), de: new DOMException('m', 'AbortError'), f64: new Float64Array([1.5]),
        };
        src.self = src;
        const c = structuredClone(src);
        log.push(c.a instanceof Uint16Array, c.a[0], c.a.buffer === c.b.buffer, c.a.buffer !== buf, c.dv.byteOffset,
                 c.d.getTime(), String(c.r), c.m.get(1).k, c.s.has('q'), typeof c.n, c.n + 0, c.str + '', typeof c.big,
                 c.e instanceof RangeError, c.e.message, c.e.cause, c.sparse.length, 1 in c.sparse, c.blob.type,
                 c.blob.size, c.file.name, c.de instanceof DOMException, c.de.name, c.self === c, c.f64[0]);
        for (const bad of [() => {}, Symbol('s'), Promise.resolve(), new WeakMap(), new EventTarget()]) {
            try { structuredClone(bad); log.push('cloned'); } catch (e) { log.push(e.name + ' ' + e.code); }
        }
        const moved = new ArrayBuffer(4);
        const copy = structuredClone(moved, { transfer: [moved] });
        log.push(copy.byteLength, moved.byteLength);
        "#,
        0.,
    );
    let mut expected: Vec<&str> = vec![
        "true",
        "7",
        "true",
        "true",
        "1",
        "5",
        "/x+/gi",
        "2",
        "true",
        "object",
        "3",
        "s",
        "object",
        "true",
        "bad",
        "c",
        "3",
        "false",
        "text/plain",
        "2",
        "f.txt",
        "true",
        "AbortError",
        "true",
        "1.5",
    ];
    expected.extend(["DataCloneError 25"; 5]);
    expected.extend(["4", "0"]);
    assert_eq!(log, expected);
}

#[test]
fn navigator_says_it_is_electrons_chromium() {
    let log = run(
        r#"
        log.push(/Chrome\/142\./.test(navigator.userAgent), /Electron\/39\./.test(navigator.userAgent),
                 ['Win32', 'MacIntel', 'Linux x86_64'].includes(navigator.platform), navigator.language,
                 navigator.hardwareConcurrency > 0, navigator.maxTouchPoints, navigator.onLine,
                 navigator.userAgentData.brands.some(b => b.brand === 'Chromium'), typeof navigator.clipboard);
        globalThis.navigator = { userAgent: 'the host' };
        log.push(navigator.userAgent);
        "#,
        0.,
    );
    assert_eq!(
        log,
        [
            "true",
            "true",
            "true",
            "en-US",
            "true",
            "0",
            "true",
            "true",
            "undefined",
            "the host"
        ]
    );
}

#[test]
fn the_host_may_put_its_own_event_classes_in_place() {
    let log = run(
        r#"
        const ours = AbortController;
        globalThis.EventTarget = function HostTarget() {};
        globalThis.Event = function HostEvent(type) { this.type = type; };
        const ac = new ours();
        ac.signal.addEventListener('abort', e => log.push('still heard ' + e.type));
        ac.abort();
        "#,
        0.,
    );
    assert_eq!(log, ["still heard abort"]);
}

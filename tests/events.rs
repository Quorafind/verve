//! `events` as Node has it: where listeners are kept, `newListener` and `removeListener`,
//! `once` wrappers, the leak warning, `errorMonitor`, and the statics `once` and `on`.

use verve::Runtime;

/// Runs `body` as a CommonJS module; it sets `exports.result` to an array. Promises it starts
/// settle before this returns; `exports.later`, when it is a function, is called after that
/// and its array answered instead.
fn run(body: &str) -> (Vec<String>, Vec<verve::ConsoleLine>) {
    let mut rt = Runtime::new();
    let dir = std::env::temp_dir().join(format!("verve-events-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let exports = rt
        .load_module(&dir.join("main.js"), body)
        .unwrap_or_else(|e| panic!("{e}"));
    rt.run_microtasks();
    let json = rt
        .eval("(e) => JSON.stringify(typeof e.later === 'function' ? e.later() : e.result)")
        .unwrap();
    let text = rt
        .call(&json, &boa_engine::JsValue::undefined(), &[exports])
        .unwrap_or_else(|e| panic!("{e}"));
    let parsed: serde_json::Value = serde_json::from_str(&verve::print(&text)).unwrap();
    let result = parsed
        .as_array()
        .unwrap()
        .iter()
        .map(|v| {
            v.as_str()
                .map(str::to_owned)
                .unwrap_or_else(|| v.to_string())
        })
        .collect();
    (result, rt.take_console())
}

#[test]
fn listeners_are_kept_where_node_keeps_them() {
    let (result, _) = run(r#"
        const EventEmitter = require('events');
        const e = new EventEmitter();
        const announced = [];
        e.on('newListener', (name) => announced.push('+' + String(name)));
        e.on('removeListener', (name) => announced.push('-' + String(name)));
        const a = () => {}, b = () => {};
        e.on('x', a);
        const one = typeof e._events.x;
        e.on('x', b);
        const two = Array.isArray(e._events.x);
        e.prependListener('x', b);
        const order = e.listeners('x').map(f => f === a ? 'a' : 'b').join('');
        e.off('x', b);
        const after = e.listeners('x').map(f => f === a ? 'a' : 'b').join('');
        const sym = Symbol('s');
        e.on(sym, a);
        const names = e.eventNames().map(String).join(',');
        e.removeAllListeners('x');
        exports.result = [
            one, two, order, after, names, String(e.listenerCount('x')),
            announced.join(' '), String(e._eventsCount), String(Object.getPrototypeOf(e._events)),
            String(e.listenerCount(sym, a)),
        ];
    "#);
    assert_eq!(
        result,
        [
            "function",
            "true",
            "bab",
            "ba",
            "newListener,removeListener,x,Symbol(s)",
            "0",
            "+removeListener +x +x +x -x +Symbol(s) -x -x",
            "3",
            "null",
            "1",
        ]
    );
}

#[test]
fn once_wrappers_errors_and_the_leak_warning() {
    let (result, console) = run(r#"
        const EventEmitter = require('events');
        const e = new EventEmitter();
        const heard = [];
        const f = (v) => heard.push(v);
        e.once('x', f);
        const raw = e.rawListeners('x')[0];
        const unwrapped = e.listeners('x')[0] === f;
        e.emit('x', 1); e.emit('x', 2);
        e.once('y', f); e.off('y', f);
        const monitored = [];
        e.on(EventEmitter.errorMonitor, (err) => monitored.push(err.message || err));
        let thrown;
        try { e.emit('error', new Error('boom')) } catch (err) { thrown = err.message }
        let code;
        try { e.emit('error', 'just text') } catch (err) { code = err.code + ' ' + err.context }
        const leaky = new EventEmitter();
        leaky.setMaxListeners(2);
        for (let i = 0; i < 4; i++) leaky.on('data', () => {});
        exports.result = [
            heard.join(','), String(raw.listener === f), String(unwrapped), String(e.listenerCount('y')),
            thrown, monitored.join(','), code, String(leaky.getMaxListeners()),
            String(EventEmitter.defaultMaxListeners), String(new EventEmitter().getMaxListeners()),
        ];
    "#);
    assert_eq!(
        result,
        [
            "1",
            "true",
            "true",
            "0",
            "boom",
            "boom,just text",
            "ERR_UNHANDLED_ERROR just text",
            "2",
            "10",
            "10"
        ]
    );
    let warnings: Vec<_> = console
        .iter()
        .filter(|l| l.text.contains("MaxListenersExceededWarning"))
        .collect();
    assert_eq!(warnings.len(), 1, "{console:?}");
}

#[test]
fn the_statics_once_and_on_are_promises_and_iterators() {
    let (result, _) = run(r#"
        const { EventEmitter, once, on } = require('events');
        const e = new EventEmitter();
        const out = [];
        once(e, 'ready').then(args => out.push('ready ' + args.join('+')));
        e.emit('ready', 1, 2);
        once(e, 'never').catch(err => out.push('rejected ' + err.message));
        e.emit('error', new Error('bad'));
        const it = on(e, 'tick');
        e.emit('tick', 'a');
        e.emit('tick', 'b');
        (async () => {
            for await (const [v] of it) {
                out.push('tick ' + v);
                if (v === 'c') break;
            }
            out.push('loop done ' + e.listenerCount('tick'));
        })();
        Promise.resolve().then(() => e.emit('tick', 'c'));
        exports.later = () => out;
    "#);
    assert_eq!(
        result,
        [
            "ready 1+2",
            "rejected bad",
            "tick a",
            "tick b",
            "tick c",
            "loop done 0"
        ]
    );
}

#[test]
fn an_emitter_extends_and_mixes_in_as_old_code_does() {
    let (result, _) = run(r#"
        const EventEmitter = require('events');
        const util = require('util');
        function Legacy() { EventEmitter.call(this) }
        util.inherits(Legacy, EventEmitter);
        Legacy.prototype.fire = function () { return this.emit('go', 7) };
        const l = new Legacy();
        let got = 0;
        l.on('go', v => got = v);
        // A prototype that copied the methods, and an object whose constructor never ran.
        function Mixed() {}
        Object.assign(Mixed.prototype, EventEmitter.prototype);
        const m = new Mixed();
        let mixedGot = 0;
        m.on('z', v => mixedGot = v);
        m.emit('z', 3);
        exports.result = [
            String(l.fire()), String(got), String(mixedGot), String(typeof EventEmitter.init),
            String(new EventEmitter.EventEmitterAsyncResource() instanceof EventEmitter),
        ];
    "#);
    assert_eq!(result, ["true", "7", "3", "function", "true"]);
}

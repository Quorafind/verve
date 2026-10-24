//! The WHATWG streams as bundles use them: sources pulled and pushed, readers, `for await`,
//! `tee`, `pipeTo` and `pipeThrough` with backpressure and abort signals, byte streams read
//! into a buffer of one's own, subclasses of `TransformStream`, the text streams, and
//! `Blob.prototype.stream()`.

use std::cell::Cell;
use std::rc::Rc;

use runtime_x::RuntimeX;

/// Runs `body` — an async function's body, which sets `result` to an array — on a clock the
/// test moves, until it has set it; answers the array's items as strings.
pub fn run_async(body: &str) -> Vec<String> {
    let time = Rc::new(Cell::new(0.));
    let read = Rc::clone(&time);
    let mut rt = RuntimeX::with_clock(move || read.get());
    let source = format!(
        "globalThis.result = undefined; (async () => {{ {body}\n}})().catch(e => {{ globalThis.result = ['threw ' + (e && e.stack || e)]; }});"
    );
    let dir = std::env::temp_dir();
    rt.load_module(&dir.join("runtime-x-streams-test.js"), &source)
        .unwrap_or_else(|e| panic!("{e}"));
    for _ in 0..200 {
        if rt
            .eval("globalThis.result !== undefined")
            .map(|v| v.to_boolean())
            .unwrap_or(false)
        {
            break;
        }
        time.set(time.get() + 10.);
        rt.run_due();
    }
    let text = rt
        .eval("JSON.stringify(globalThis.result)")
        .unwrap_or_else(|e| panic!("{e}"));
    let parsed: serde_json::Value = serde_json::from_str(&runtime_x::print(&text))
        .unwrap_or_else(|_| panic!("no result: {}", runtime_x::print(&text)));
    parsed
        .as_array()
        .unwrap_or_else(|| panic!("result is not an array: {parsed}"))
        .iter()
        .map(|v| {
            v.as_str()
                .map(str::to_owned)
                .unwrap_or_else(|| v.to_string())
        })
        .collect()
}

#[test]
fn a_source_pushed_and_pulled_is_read_to_its_end() {
    let result = run_async(
        r#"
        const pushed = new ReadableStream({ start(c) { c.enqueue('a'); c.enqueue('b'); c.close(); } });
        const reader = pushed.getReader();
        const locked = pushed.locked;
        const got = [];
        for (let step; !(step = await reader.read()).done;) got.push(step.value);
        await reader.closed;
        reader.releaseLock();
        let n = 0;
        const pulled = new ReadableStream({ pull(c) { n++; n > 3 ? c.close() : c.enqueue(n); } }, { highWaterMark: 0 });
        const counted = [];
        for await (const v of pulled) counted.push(v);
        let cancelled;
        const endless = new ReadableStream({ pull(c) { c.enqueue('x'); }, cancel(reason) { cancelled = reason; } });
        for await (const v of endless) break;
        result = [locked, pushed.locked, got.join(','), counted.join(','), cancelled === undefined ? 'cancelled' : 'no', typeof ReadableStreamDefaultReader];
    "#,
    );
    assert_eq!(
        result,
        ["true", "false", "a,b", "1,2,3", "cancelled", "function"]
    );
}

#[test]
fn failures_reach_the_reader() {
    let result = run_async(
        r#"
        const out = [];
        try { await new ReadableStream({ start(c) { c.error(new Error('boom')); } }).getReader().read(); } catch (e) { out.push(e.message); }
        try { await new ReadableStream({ pull() { throw new Error('pull failed'); } }).getReader().read(); } catch (e) { out.push(e.message); }
        try { new ReadableStream({ start() { throw new Error('start threw'); } }); } catch (e) { out.push(e.message); }
        const s = new ReadableStream();
        s.getReader();
        try { s.getReader(); } catch (e) { out.push(e.constructor.name); }
        try { ReadableStream(); } catch (e) { out.push(e.constructor.name); }
        const closedEarly = new ReadableStream({ start(c) { c.close(); } });
        try { new ReadableStream({ start(c) { c.close(); c.enqueue(1); } }); } catch (e) { out.push('enqueue after close: ' + e.constructor.name); }
        out.push(String((await closedEarly.getReader().read()).done));
        result = out;
    "#,
    );
    assert_eq!(
        result,
        [
            "boom",
            "pull failed",
            "start threw",
            "TypeError",
            "TypeError",
            "enqueue after close: TypeError",
            "true"
        ]
    );
}

#[test]
fn tee_from_and_strategies() {
    let result = run_async(
        r#"
        const [a, b] = ReadableStream.from([1, 2, 3]).tee();
        const ra = [], rb = [];
        for await (const v of a) ra.push(v);
        for await (const v of b) rb.push(v);
        async function* gen() { yield 'p'; yield 'q'; }
        const fromAsync = [];
        for await (const v of ReadableStream.from(gen())) fromAsync.push(v);
        const fromPromises = [];
        for await (const v of ReadableStream.from([Promise.resolve(7), 8])) fromPromises.push(v);
        const count = new CountQueuingStrategy({ highWaterMark: 4 });
        const bytes = new ByteLengthQueuingStrategy({ highWaterMark: 16 });
        let controller;
        new ReadableStream({ start(c) { controller = c; } }, bytes);
        const before = controller.desiredSize;
        controller.enqueue(new Uint8Array(10));
        result = [ra.join(), rb.join(), fromAsync.join(), fromPromises.join(), count.highWaterMark, count.size(), bytes.size(new Uint8Array(5)), before, controller.desiredSize];
    "#,
    );
    assert_eq!(
        result,
        ["1,2,3", "1,2,3", "p,q", "7,8", "4", "1", "5", "16", "6"]
    );
}

#[test]
fn writable_streams_queue_writes_and_push_back() {
    let result = run_async(
        r#"
        const written = [];
        const ws = new WritableStream({
            write(chunk) { return new Promise(r => setTimeout(() => { written.push(chunk); r(); }, 5)); },
            close() { written.push('closed'); },
        }, new CountQueuingStrategy({ highWaterMark: 2 }));
        const w = ws.getWriter();
        const sizes = [w.desiredSize];
        w.write('a'); sizes.push(w.desiredSize);
        w.write('b'); sizes.push(w.desiredSize);
        const readyBefore = await Promise.race([w.ready.then(() => 'ready'), Promise.resolve('waiting')]);
        await w.ready;
        await w.close();
        await w.closed;
        let aborted;
        const ws2 = new WritableStream({ abort(reason) { aborted = reason; } });
        await ws2.abort('why');
        const failing = new WritableStream({ write() { throw new Error('sink failed'); } }).getWriter();
        let failed;
        try { await failing.write(1); } catch (e) { failed = e.message; }
        let signalled;
        const ws3 = new WritableStream({ start(c) { c.signal.addEventListener('abort', () => { signalled = c.signal.reason; }); } });
        await ws3.abort('stop');
        result = [sizes.join(','), readyBefore, written.join(','), aborted, failed, signalled, ws.locked];
    "#,
    );
    assert_eq!(
        result,
        [
            "2,1,0",
            "waiting",
            "a,b,closed",
            "why",
            "sink failed",
            "stop",
            "true"
        ]
    );
}

#[test]
fn pipes_carry_chunks_through_transforms_and_stop_on_abort() {
    let result = run_async(
        r#"
        class Upper extends TransformStream {
            constructor() { super({ transform(chunk, c) { c.enqueue(chunk.toUpperCase()); }, flush(c) { c.enqueue('!'); } }); }
        }
        const upper = new Upper();
        const out = [];
        await ReadableStream.from(['x', 'y']).pipeThrough(upper).pipeTo(new WritableStream({ write(c) { out.push(c); } }));
        // A transformer that enqueues several chunks for one, and the identity transform.
        const split = new TransformStream({ transform(line, c) { for (const part of line.split(' ')) c.enqueue(part); } });
        const words = [];
        await ReadableStream.from(['a b', 'c']).pipeThrough(split).pipeThrough(new TransformStream()).pipeTo(new WritableStream({ write(w) { words.push(w); } }));
        // An abort signal stops a pipe that would never end.
        const controller = new AbortController();
        let cancelledWith;
        const never = new ReadableStream({ pull() { return new Promise(() => {}); }, cancel(r) { cancelledWith = r.message; } });
        const piping = never.pipeTo(new WritableStream(), { signal: controller.signal });
        controller.abort(new Error('stop'));
        let abortedWith;
        try { await piping; } catch (e) { abortedWith = e.message; }
        // An error in the transform fails both ends.
        const bad = new TransformStream({ transform() { throw new Error('bad chunk'); } });
        let sourceCancelled;
        let pipeFailed;
        try {
            await new ReadableStream({ start(c) { c.enqueue(1); }, cancel(r) { sourceCancelled = r.message; } }).pipeThrough(bad).pipeTo(new WritableStream());
        } catch (e) { pipeFailed = e.message; }
        result = [out.join(''), upper instanceof TransformStream, upper instanceof Upper, words.join(','), abortedWith, cancelledWith, pipeFailed, sourceCancelled];
    "#,
    );
    assert_eq!(
        result,
        [
            "XY!",
            "true",
            "true",
            "a,b,c",
            "stop",
            "stop",
            "bad chunk",
            "bad chunk"
        ]
    );
}

#[test]
fn byte_streams_fill_a_readers_own_buffer() {
    let result = run_async(
        r#"
        const stream = new ReadableStream({ type: 'bytes', start(c) { c.enqueue(new Uint8Array([1, 2, 3, 4, 5])); c.close(); } });
        const reader = stream.getReader({ mode: 'byob' });
        const first = await reader.read(new Uint8Array(3));
        const second = await reader.read(new Uint8Array(3));
        const third = await reader.read(new Uint8Array(3));
        let notBytes;
        try { new ReadableStream().getReader({ mode: 'byob' }); } catch (e) { notBytes = e.constructor.name; }
        let controller;
        const pulled = new ReadableStream({ type: 'bytes', start(c) { controller = c; } });
        result = [Array.from(first.value).join(), Array.from(second.value).join(), third.done, notBytes, controller.byobRequest, controller.constructor.name];
    "#,
    );
    assert_eq!(
        result,
        [
            "1,2,3",
            "4,5",
            "true",
            "TypeError",
            "null",
            "ReadableByteStreamController"
        ]
    );
}

#[test]
fn text_streams_handle_characters_split_across_chunks() {
    let result = run_async(
        r#"
        const encoded = new TextEncoder().encode('héllo wörld 😀');
        const parts = [encoded.slice(0, 2), encoded.slice(2, 9), encoded.slice(9, 15), encoded.slice(15)];
        const text = [];
        await ReadableStream.from(parts).pipeThrough(new TextDecoderStream()).pipeTo(new WritableStream({ write(s) { text.push(s); } }));
        const bytes = [];
        // A surrogate pair split across two string chunks.
        await ReadableStream.from(['h', '\ud83d', '\ude00']).pipeThrough(new TextEncoderStream()).pipeTo(new WritableStream({ write(b) { bytes.push(...b); } }));
        const decoder = new TextDecoderStream('utf-8', { fatal: true });
        let fatal;
        const writer = decoder.writable.getWriter();
        writer.write(new Uint8Array([0xff])).catch(() => {});
        try { await decoder.readable.getReader().read(); } catch (e) { fatal = e.constructor.name; }
        result = [text.join(''), new TextDecoderStream().encoding, new TextEncoderStream().encoding, bytes.join(','), fatal];
    "#,
    );
    assert_eq!(
        result,
        [
            "héllo wörld 😀",
            "utf-8",
            "utf-8",
            "104,240,159,152,128",
            "TypeError"
        ]
    );
}

#[test]
fn blobs_stream_and_stream_web_is_requirable() {
    let result = run_async(
        r#"
        const chunks = [];
        for await (const chunk of new Blob(['streamed ', 'blob']).stream()) chunks.push(new TextDecoder().decode(chunk));
        const web = require('stream/web');
        const nodeWeb = require('node:stream/web');
        result = [chunks.join(''), web.ReadableStream === ReadableStream, nodeWeb.TransformStream === TransformStream, typeof web.TextDecoderStream];
    "#,
    );
    assert_eq!(result, ["streamed blob", "true", "true", "function"]);
}

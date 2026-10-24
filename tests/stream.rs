//! `stream` and `string_decoder` as Node has them: reading, writing, transforming, piping,
//! `pipeline` and `finished`, classes extended the ES5 way and the class way.

use runtime_x::RuntimeX;

/// Runs `body` as a CommonJS module; once the microtasks it queued have run (and any timers
/// due), answers `exports.result` — or what `exports.later()` answers, when it set that.
fn run(body: &str) -> Vec<String> {
    let mut rt = RuntimeX::new();
    let dir = std::env::temp_dir().join(format!("runtime-x-stream-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let exports = rt
        .load_module(&dir.join("main.js"), body)
        .unwrap_or_else(|e| panic!("{e}"));
    for _ in 0..10 {
        rt.run_microtasks();
        rt.run_due();
    }
    let json = rt
        .eval("(e) => JSON.stringify(typeof e.later === 'function' ? e.later() : e.result)")
        .unwrap();
    let text = rt
        .call(&json, &boa_engine::JsValue::undefined(), &[exports])
        .unwrap_or_else(|e| panic!("{e}"));
    let console = rt.take_console();
    let parsed: serde_json::Value = serde_json::from_str(&runtime_x::print(&text))
        .unwrap_or_else(|e| panic!("{e}: {} {console:?}", runtime_x::print(&text)));
    parsed
        .as_array()
        .unwrap_or_else(|| panic!("no result: {console:?}"))
        .iter()
        .map(|v| {
            v.as_str()
                .map(str::to_owned)
                .unwrap_or_else(|| v.to_string())
        })
        .collect()
}

#[test]
fn the_module_is_the_stream_class_with_the_rest_on_it() {
    let result = run(r#"
        const stream = require('stream');
        const { Readable, Writable, Duplex, Transform, PassThrough, Stream } = stream;
        const d = new Duplex();
        const t = new PassThrough();
        exports.result = [
            stream === Stream, stream === require('node:stream'),
            require('_stream_readable') === Readable, require('stream/promises') === stream.promises,
            d instanceof Readable, d instanceof Writable, d instanceof Stream,
            t instanceof Transform, t instanceof Duplex, t instanceof Writable,
            new Readable() instanceof Writable, new Writable() instanceof Readable,
            typeof stream.pipeline, typeof stream.finished, typeof Readable.from,
            stream.getDefaultHighWaterMark(false), stream.getDefaultHighWaterMark(true),
            new Readable({ objectMode: true }).readableHighWaterMark,
        ];
    "#);
    assert_eq!(
        result,
        [
            "true", "true", "true", "true", "true", "true", "true", "true", "true", "true",
            "false", "false", "function", "function", "function", "65536", "16", "16",
        ]
    );
}

#[test]
fn a_readable_pushes_flows_pauses_and_ends() {
    let result = run(r#"
        const { Readable } = require('stream');
        const log = [], objectLog = [], textLog = [];
        let n = 0;
        const r = new Readable({
            read() { n++; this.push(n <= 3 ? 'chunk' + n : null) },
        });
        r.on('data', d => log.push('data ' + d));
        r.on('end', () => log.push('end'));
        r.on('close', () => log.push('close'));
        // Paused mode: `readable`, then `read()`.
        const objects = new Readable({ objectMode: true, read() {} });
        objects.push({ a: 1 }); objects.push({ b: 2 }); objects.push(null);
        const got = [];
        objects.on('readable', () => { let o; while ((o = objects.read()) !== null) got.push(JSON.stringify(o)) });
        objects.on('end', () => objectLog.push('objects ' + got.join(' ')));
        // Text decoded across a character cut in two.
        const euro = Buffer.from('€');
        const text = new Readable({ read() {} });
        text.setEncoding('utf8');
        text.push(euro.subarray(0, 1)); text.push(euro.subarray(1)); text.push(null);
        let decoded = '';
        text.on('data', d => decoded += d).on('end', () => textLog.push('decoded ' + decoded + ' ' + typeof decoded));
        exports.later = () => log.concat(objectLog, textLog);
    "#);
    assert_eq!(
        result,
        [
            "data chunk1",
            "data chunk2",
            "data chunk3",
            "end",
            "close",
            "objects {\"a\":1} {\"b\":2}",
            "decoded € string",
        ]
    );
}

#[test]
fn readable_from_async_iteration_and_to_array() {
    let result = run(r#"
        const { Readable } = require('stream');
        const log = [];
        (async () => {
            const items = [];
            for await (const x of Readable.from([1, 2, 3])) items.push(x);
            log.push('sync ' + items.join(','));
            async function* gen() { yield 'a'; await null; yield 'b'; }
            log.push('async ' + (await Readable.from(gen()).toArray()).join(','));
            const r = new Readable({ read() {} });
            r.push('x'); r.push('y'); r.push(null);
            let text = '';
            for await (const chunk of r) text += chunk;
            log.push('bytes ' + text);
            const early = Readable.from([1, 2, 3, 4]);
            for await (const x of early) { if (x === 2) break; }
            log.push('broke, destroyed ' + early.destroyed);
            const failing = new Readable({ read() { this.destroy(new Error('nope')) } });
            try { for await (const x of failing) {} } catch (e) { log.push('caught ' + e.message) }
        })();
        exports.later = () => log;
    "#);
    assert_eq!(
        result,
        [
            "sync 1,2,3",
            "async a,b",
            "bytes xy",
            "broke, destroyed true",
            "caught nope"
        ]
    );
}

#[test]
fn a_writable_buffers_drains_and_finishes() {
    let result = run(r#"
        const { Writable } = require('stream');
        const log = [];
        const written = [];
        const w = new Writable({
            highWaterMark: 3,
            write(chunk, encoding, callback) { written.push(chunk.toString()); setTimeout(callback, 0) },
        });
        log.push('write a ' + w.write('a'));
        log.push('write bcd ' + w.write('bcd'));
        log.push('length ' + w.writableLength + ' needDrain ' + w.writableNeedDrain);
        w.on('drain', () => log.push('drain'));
        w.on('finish', () => log.push('finish ' + written.join('|')));
        w.on('close', () => log.push('close'));
        w.end('e', () => log.push('end callback'));
        w.write('late', (err) => log.push('late: ' + err.code));
        w.on('error', (err) => log.push('error ' + err.code));
        // `_writev` takes what was corked, all at once.
        const batches = [];
        const v = new Writable({
            objectMode: true,
            writev(chunks, callback) { batches.push(chunks.map(c => c.chunk).join('+')); callback() },
            write(chunk, enc, callback) { batches.push('one ' + chunk); callback() },
        });
        v.cork(); v.write(1); v.write(2); v.write(3); v.uncork();
        v.end(4);
        v.on('finish', () => log.push('batches ' + batches.join(' ')));
        exports.later = () => log;
    "#);
    assert_eq!(
        result,
        [
            "write a true",
            "write bcd false",
            "length 4 needDrain true",
            "late: ERR_STREAM_WRITE_AFTER_END",
            "error ERR_STREAM_WRITE_AFTER_END",
            "close",
            "batches 1+2+3 one 4",
            "end callback",
        ]
    );
}

#[test]
fn a_writable_that_writes_at_once_finishes_in_node_order() {
    let result = run(r#"
        const { Writable } = require('stream');
        const log = [];
        const w = new Writable({ write(chunk, enc, cb) { log.push('write ' + chunk); cb() } });
        w.on('drain', () => log.push('drain'));
        w.on('prefinish', () => log.push('prefinish'));
        w.on('finish', () => log.push('finish'));
        w.on('close', () => log.push('close'));
        w.write('a', () => log.push('cb a'));
        w.end('b', () => log.push('end cb'));
        log.push('sync done');
        exports.later = () => log;
    "#);
    assert_eq!(
        result,
        [
            "write a",
            "write b",
            "prefinish",
            "sync done",
            "cb a",
            "end cb",
            "finish",
            "close"
        ]
    );
}

#[test]
fn transforms_and_pipes_and_old_style_subclasses() {
    let result = run(r#"
        const stream = require('stream');
        const util = require('util');
        const log = [];
        // ES5, as readable-stream-era packages write it.
        function Upper(options) {
            if (!(this instanceof Upper)) return new Upper(options);
            stream.Transform.call(this, options);
            this.count = 0;
        }
        util.inherits(Upper, stream.Transform);
        Upper.prototype._transform = function (chunk, enc, cb) { this.count++; cb(null, chunk.toString().toUpperCase()) };
        Upper.prototype._flush = function (cb) { this.push('!'); cb() };
        // A class.
        class Lines extends stream.Transform {
            constructor() { super({ readableObjectMode: true }); this.rest = '' }
            _transform(chunk, enc, cb) {
                const parts = (this.rest + chunk).split('\n');
                this.rest = parts.pop();
                for (const p of parts) this.push(p);
                cb();
            }
            _flush(cb) { if (this.rest) this.push(this.rest); cb() }
        }
        const lines = [];
        const sink = new stream.Writable({ objectMode: true, write(line, enc, cb) { lines.push(line); cb() } });
        const source = stream.Readable.from(['ab\ncd', '\nef'], { objectMode: false });
        source.pipe(Upper()).pipe(new Lines()).pipe(sink);
        sink.on('finish', () => log.push('lines ' + lines.join(',')));
        // An old-style stream: Stream.call, emitting data and end by hand, piped the legacy way.
        function Old() { stream.Stream.call(this); this.readable = true }
        util.inherits(Old, stream.Stream);
        const old = new Old();
        const pass = new stream.PassThrough();
        let passed = '';
        pass.on('data', d => passed += d).on('end', () => log.push('passed ' + passed));
        old.pipe(pass);
        old.emit('data', 'x'); old.emit('data', 'y'); old.emit('end');
        exports.later = () => log;
    "#);
    assert_eq!(result, ["passed xy", "lines AB,CD,EF!"]);
}

#[test]
fn pipeline_and_finished_report_the_end_or_the_first_error() {
    let result = run(r#"
        const { pipeline, finished, Readable, Writable, Transform, PassThrough } = require('stream');
        const { pipeline: pipelinePromise } = require('stream/promises');
        const log = [];
        const out = [];
        pipeline(
            Readable.from(['a', 'b']),
            new Transform({ transform(c, e, cb) { cb(null, c + c) } }),
            new Writable({ write(c, e, cb) { out.push(String(c)); cb() } }),
            (err) => log.push('done ' + (err ? err.message : 'ok') + ' ' + out.join('')),
        );
        const broken = new Transform({ transform(c, e, cb) { cb(new Error('bad chunk')) } });
        const tail = new PassThrough();
        pipeline(Readable.from(['x']), broken, tail, (err) => {
            log.push('failed ' + err.message + ' destroyed ' + tail.destroyed);
        });
        const r = new Readable({ read() {} });
        finished(r, (err) => log.push('finished ' + (err ? err.code : 'ok')));
        r.destroy();
        (async () => {
            const collected = [];
            await pipelinePromise(
                Readable.from(['p', 'q']),
                async function* (source) { for await (const c of source) yield c.toUpperCase() },
                new Writable({ objectMode: true, write(c, e, cb) { collected.push(c); cb() } }),
            );
            log.push('promise ' + collected.join(''));
        })();
        exports.later = () => log;
    "#);
    let mut sorted = result.clone();
    sorted.sort();
    assert_eq!(
        sorted,
        [
            "done ok aabb",
            "failed bad chunk destroyed true",
            "finished ERR_STREAM_PREMATURE_CLOSE",
            "promise PQ",
        ],
        "{result:?}"
    );
}

#[test]
fn destroy_says_error_then_close_once() {
    let result = run(r#"
        const { Readable, Writable, Duplex } = require('stream');
        const log = [];
        const r = new Readable({ read() {} });
        r.on('error', e => log.push('error ' + e.message));
        r.on('close', () => log.push('close ' + r.destroyed + ' ' + r.errored.message));
        r.destroy(new Error('gone'));
        r.destroy(new Error('again'));
        const w = new Writable({
            write(c, e, cb) { cb() },
            destroy(err, cb) { log.push('_destroy ' + err); cb(err) },
        });
        w.on('close', () => log.push('w close'));
        w.destroy();
        const d = new Duplex({ read() {}, write(c, e, cb) { cb() }, allowHalfOpen: false });
        d.on('finish', () => log.push('d finish'));
        d.on('end', () => log.push('d end'));
        d.push(null);
        d.resume();
        exports.later = () => log;
    "#);
    assert_eq!(
        result,
        [
            "_destroy null",
            "error gone",
            "close true gone",
            "w close",
            "d end",
            "d finish"
        ]
    );
}

#[test]
fn the_string_decoder_keeps_what_a_piece_cut_in_two() {
    let result = run(r#"
        const { StringDecoder } = require('string_decoder');
        const utf8 = new StringDecoder('utf8');
        const bytes = Buffer.from('a€b');
        const parts = [utf8.write(bytes.subarray(0, 2)), utf8.write(bytes.subarray(2, 4)), utf8.write(bytes.subarray(4))];
        const cut = new StringDecoder();
        const unfinished = cut.write(Buffer.from([0xe2, 0x82])) + '|' + cut.end();
        const b64 = new StringDecoder('base64');
        const encoded = b64.write(Buffer.from('he')) + '|' + b64.write(Buffer.from('llo')) + '|' + b64.end();
        const utf16 = new StringDecoder('ucs2');
        const smile = Buffer.from('😀', 'utf16le');
        const halves = utf16.write(smile.subarray(0, 3)) + '|' + utf16.write(smile.subarray(3));
        let unknown;
        try { new StringDecoder('klingon') } catch (e) { unknown = e.code }
        exports.result = [
            parts.join('|'), unfinished, encoded, halves, utf16.encoding, new StringDecoder('hex').write(Buffer.from([255])),
            unknown, new StringDecoder('latin1').end(Buffer.from([0xe9])),
        ];
    "#);
    assert_eq!(
        result,
        [
            "a|€|b",
            "|\u{fffd}",
            "|aGVs|bG8=",
            "|😀",
            "utf16le",
            "ff",
            "ERR_UNKNOWN_ENCODING",
            "é"
        ]
    );
}

#[test]
fn hash_like_writables_as_browserify_packages_make_them() {
    // `create-hash`/`browserify-sign` shape: a Writable subclass made the ES5 way, its chunks
    // collected and read on `finish`.
    let result = run(r#"
        var stream = require('stream');
        var inherits = require('util').inherits;
        function Sign(algorithm) {
            stream.Writable.call(this);
            this._chunks = [];
            this._algorithm = algorithm;
        }
        inherits(Sign, stream.Writable);
        Sign.prototype._write = function (data, _, done) { this._chunks.push(data); done() };
        Sign.prototype.update = function (data) { this.write(data); return this };
        var s = new Sign('sha256');
        var heard = '';
        s.on('finish', function () { heard = Buffer.concat(s._chunks).toString() });
        s.update('ab').update('cd');
        s.end();
        exports.later = () => [heard, String(s instanceof stream.Writable), String(s.writableFinished)];
    "#);
    assert_eq!(result, ["abcd", "true", "true"]);
}

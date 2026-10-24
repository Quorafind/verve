//! `zlib` as plugins and the libraries they bundle use it: the synchronous and callback forms
//! of every format, data Node made read back, Node's errors, and the streams on `Transform`.

use runtime_x::RuntimeX;

fn result_of(rt: &mut RuntimeX, body: &str) -> Vec<String> {
    let dir = std::env::temp_dir().join(format!("runtime-x-zlib-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let exports = rt
        .load_module(&dir.join("main.js"), body)
        .unwrap_or_else(|e| panic!("{e}"));
    let json = rt.eval("JSON.stringify").unwrap();
    let text = rt
        .call(&json, &boa_engine::JsValue::undefined(), &[exports])
        .unwrap_or_else(|e| panic!("{e}"));
    let parsed: serde_json::Value = serde_json::from_str(&runtime_x::print(&text)).unwrap();
    let Some(result) = parsed["result"].as_array() else {
        panic!("no result: {parsed} {:?}", rt.take_console());
    };
    result
        .iter()
        .map(|v| {
            v.as_str()
                .map(str::to_owned)
                .unwrap_or_else(|| v.to_string())
        })
        .collect()
}

fn run(body: &str) -> Vec<String> {
    result_of(&mut RuntimeX::new(), body)
}

#[test]
fn every_format_synchronously_and_what_node_made() {
    let result = run(r#"
        const zlib = require('node:zlib');
        const text = 'Lorem ipsum dolor sit amet. '.repeat(200);
        const out = [];
        for (const [pack, unpack] of [['deflate', 'inflate'], ['deflateRaw', 'inflateRaw'], ['gzip', 'gunzip'],
                                      ['gzip', 'unzip'], ['deflate', 'unzip'], ['brotliCompress', 'brotliDecompress']]) {
            const packed = zlib[pack + 'Sync'](text, { level: 9 });
            out.push(pack + ':' + (packed.length < 200) + ':' + (zlib[unpack + 'Sync'](packed).toString() === text));
        }
        // What Node's zlib made, read back.
        out.push(zlib.inflateSync(Buffer.from('eJzLSM3JyQcABiwCFQ==', 'base64')).toString());
        out.push(zlib.gunzipSync(Buffer.from('H4sIAAAAAAAAA8tIzcnJBwCGphA2BQAAAA==', 'base64')).toString());
        out.push(zlib.unzipSync(new Uint8Array(Buffer.from('eJzLSM3JyQcABiwCFQ==', 'base64'))).toString());
        out.push(String(Buffer.isBuffer(zlib.gzipSync('x'))));
        out.push(zlib.gzipSync('x').subarray(0, 3).toString('hex'));
        out.push(String(zlib.crc32('hello')));
        out.push(String(zlib.crc32(Buffer.from(' world'), zlib.crc32('hello')) === zlib.crc32('hello world')));
        out.push(zlib.constants.Z_BEST_COMPRESSION + ',' + zlib.Z_SYNC_FLUSH + ',' + zlib.constants.BROTLI_PARAM_QUALITY + ',' + zlib.codes.Z_DATA_ERROR + ',' + zlib.codes['-3']);
        const quick = zlib.brotliCompressSync(text, { params: { [zlib.constants.BROTLI_PARAM_QUALITY]: 4 } });
        out.push(String(zlib.brotliDecompressSync(quick).toString() === text));
        exports.result = out;
    "#);
    assert_eq!(
        result,
        [
            "deflate:true:true",
            "deflateRaw:true:true",
            "gzip:true:true",
            "gzip:true:true",
            "deflate:true:true",
            "brotliCompress:true:true",
            "hello",
            "hello",
            "hello",
            "true",
            "1f8b08",
            "907060870",
            "true",
            "9,2,1,-3,Z_DATA_ERROR",
            "true",
        ]
    );
}

#[test]
fn errors_carry_nodes_codes_and_callbacks_answer_later() {
    let result = run(r#"
        const zlib = require('zlib');
        const { promisify } = require('util');
        const out = [];
        const fail = f => { try { f(); return 'no error' } catch (e) { return e.code + ' ' + e.errno + ' ' + e.message } };
        out.push(fail(() => zlib.inflateSync(Buffer.from('not zlib data'))));
        out.push(fail(() => zlib.gunzipSync(Buffer.from('not gzip data'))));
        const whole = zlib.deflateSync('some text to cut short, some text to cut short');
        out.push(fail(() => zlib.inflateSync(whole.subarray(0, whole.length - 4))));
        out.push(fail(() => zlib.inflateSync(42)));
        out.push(fail(() => zlib.deflateSync('x', { level: 12 })));
        out.push(fail(() => zlib.inflateSync(zlib.deflateSync('a'.repeat(1000)), { maxOutputLength: 10 })));
        let sync = true;
        zlib.gzip('called back', (err, packed) => {
            out.push('async ' + !sync + ' ' + err);
            zlib.gunzip(packed, (err, text) => {
                out.push(text.toString());
                zlib.inflate(Buffer.from('nope'), async err => {
                    out.push(err.code);
                    const packed = await promisify(zlib.deflate)('promised');
                    out.push((await promisify(zlib.inflate)(packed)).toString());
                    exports.result = out;
                });
            });
        });
        sync = false;
    "#);
    assert_eq!(
        result,
        [
            "Z_DATA_ERROR -3 incorrect header check",
            "Z_DATA_ERROR -3 incorrect header check",
            "Z_BUF_ERROR -5 unexpected end of file",
            "ERR_INVALID_ARG_TYPE undefined The \"buffer\" argument must be of type string or an instance of Buffer, TypedArray, DataView, or ArrayBuffer. Received number",
            "undefined undefined The value of \"options.level\" is out of range. It must be >= -1 and <= 9. Received 12",
            "ERR_BUFFER_TOO_LARGE undefined Cannot create a Buffer larger than 10 bytes",
            "async true null",
            "called back",
            "Z_DATA_ERROR",
            "promised",
        ]
    );
}

#[test]
fn streams_are_transforms_feeding_the_codec() {
    let mut rt = RuntimeX::new();
    // The stream module's `Transform`, as its public API has it, kept to what zlib's streams
    // reach: `transform(chunk, encoding, callback)`, `flush(callback)`, `push`.
    let stream = rt
        .eval(
            r#"({ Transform: class Transform {
                constructor(options) { this.options = options; this.out = []; this.ended = false; }
                push(chunk) { if (chunk !== null) this.out.push(chunk); return true; }
                write(chunk) { this.options.transform.call(this, chunk, 'buffer', (err, data) => {
                    if (err) throw err; if (data) this.push(data); }); }
                end() { this.options.flush.call(this, (err, data) => { if (err) throw err; if (data) this.push(data);
                    this.ended = true; }); }
                read() { return Buffer.concat(this.out); }
            } })"#,
        )
        .unwrap();
    rt.define_builtin("stream", stream);
    let result = result_of(
        &mut rt,
        r#"
        const zlib = require('zlib');
        const gzip = zlib.createGzip({ level: 1 });
        gzip.write(Buffer.from('streamed '));
        gzip.write('in pieces');
        gzip.end();
        const packed = gzip.read();
        const gunzip = new zlib.Gunzip();
        for (let i = 0; i < packed.length; i += 3) gunzip.write(packed.subarray(i, i + 3));
        gunzip.end();
        let failed = 'no error';
        const inflate = zlib.createInflate();
        try { inflate.write(Buffer.from('garbage!')); } catch (e) { failed = e.code; }
        exports.result = [
            String(gzip.ended), String(gzip.bytesWritten), packed.subarray(0, 2).toString('hex'),
            gunzip.read().toString(), failed, typeof gzip.flush, typeof gzip.close,
        ];
    "#,
    );
    assert_eq!(
        result,
        [
            "true",
            "18",
            "1f8b",
            "streamed in pieces",
            "Z_DATA_ERROR",
            "function",
            "function"
        ]
    );
}

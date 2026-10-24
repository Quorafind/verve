//! Node's `Buffer` and `require('buffer')`: numbers read and written at an offset, the
//! encodings as Node reads and writes them, `copy`, `fill`, `indexOf`, `swap`, views sharing
//! memory, and Node's errors.

use std::path::Path;

use runtime_x::RuntimeX;

fn run(body: &str) -> Vec<String> {
    let mut rt = RuntimeX::new();
    let exports = rt
        .load_module(Path::new("/plugins/buffer-test/main.js"), body)
        .unwrap_or_else(|e| panic!("{e}"));
    let json = rt.eval("JSON.stringify").unwrap();
    let text = rt
        .call(&json, &boa_engine::JsValue::undefined(), &[exports])
        .unwrap_or_else(|e| panic!("{e}"));
    let parsed: serde_json::Value = serde_json::from_str(&runtime_x::print(&text)).unwrap();
    parsed["result"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| {
            v.as_str()
                .map(str::to_owned)
                .unwrap_or_else(|| v.to_string())
        })
        .collect()
}

#[test]
fn numbers_at_an_offset() {
    let result = run(r#"
        const b = Buffer.alloc(16);
        const caught = (f) => { try { return String(f()) } catch (e) { return e.code + ': ' + e.message } };
        b.writeUInt32BE(0xdeadbeef, 0); b.writeInt16LE(-2, 4); b.writeDoubleLE(1.5, 8);
        exports.result = [
            b.toString('hex'), b.readUInt32BE(0).toString(16), b.readInt16LE(4), b.readDoubleLE(8), b.readUInt8(0),
            b.readUint16BE(0), b.writeFloatBE(0.5, 12), b.readFloatBE(12),
            caught(() => b.readUInt32LE(14)), caught(() => b.writeUInt8(256, 0)), caught(() => b.readUInt8(1.5)),
            caught(() => b.writeUInt32LE(2 ** 33, 0)),
            b.writeUIntBE(0x123456, 0, 3), b.readUIntBE(0, 3).toString(16), b.readIntLE(0, 3), b.readIntBE(0, 1),
            (() => { const x = Buffer.alloc(8); x.writeBigUInt64LE(2n ** 63n); return String(x.readBigUInt64LE()) })(),
            (() => { const x = Buffer.alloc(8); x.writeBigInt64BE(-5n); return String(x.readBigInt64BE()) })(),
            caught(() => Buffer.alloc(8).writeBigInt64BE(2n ** 63n)),
            caught(() => Buffer.alloc(2).readUInt32LE()),
        ].map(String);
    "#);
    assert_eq!(
        result,
        [
            "deadbeeffeff0000000000000000f83f",
            "deadbeef",
            "-2",
            "1.5",
            "222",
            "57005",
            "16",
            "0.5",
            "ERR_OUT_OF_RANGE: The value of \"offset\" is out of range. It must be >= 0 and <= 12. Received 14",
            "ERR_OUT_OF_RANGE: The value of \"value\" is out of range. It must be >= 0 and <= 255. Received 256",
            "ERR_OUT_OF_RANGE: The value of \"offset\" is out of range. It must be an integer. Received 1.5",
            "ERR_OUT_OF_RANGE: The value of \"value\" is out of range. It must be >= 0 and <= 4294967295. Received 8_589_934_592",
            "3",
            "123456",
            "5649426",
            "18",
            "9223372036854775808",
            "-5",
            "ERR_OUT_OF_RANGE: The value of \"value\" is out of range. It must be >= -(2n ** 63n) and < 2n ** 63n. Received 9_223_372_036_854_775_808n",
            "ERR_BUFFER_OUT_OF_BOUNDS: Attempt to access memory outside buffer bounds",
        ]
    );
}

#[test]
fn encodings_as_node_reads_and_writes_them() {
    let result = run(r#"
        exports.result = [
            Buffer.from('abc', 'hex').length, Buffer.from('aGVsbG8', 'base64').toString(),
            Buffer.from('aGk=\n', 'base64').toString(), Buffer.from('-_8', 'base64url').toString('hex'),
            Buffer.from('héllo').toString('latin1'), Buffer.from('héllo', 'latin1').toString('hex'),
            Buffer.from('\ud800').toString('hex'), Buffer.from('hi', 'utf16le').toString('hex'),
            Buffer.from('hi').toString('base64url'), Buffer.from('ÿ', 'ascii').toString('hex'),
            Buffer.from([0xe9]).toString('ascii'), Buffer.byteLength('héllo'), Buffer.byteLength('aGVsbG8=', 'base64'),
            Buffer.isEncoding('UTF-8'), Buffer.isEncoding(''), Buffer.from('hello').toString('utf8', 1, 3),
            (() => { try { Buffer.from('x').toString('nope') } catch (e) { return e.code } })(),
            new TextEncoder().encode('\ud800').join(),
        ].map(String);
    "#);
    assert_eq!(
        result,
        [
            "1",
            "hello",
            "hi",
            "fbff",
            "hÃ©llo",
            "68e96c6c6f",
            "efbfbd",
            "68006900",
            "aGk",
            "ff",
            "i",
            "6",
            "5",
            "true",
            "false",
            "el",
            "ERR_UNKNOWN_ENCODING",
            "239,191,189",
        ]
    );
}

#[test]
fn making_copying_searching_and_sharing() {
    let result = run(r#"
        const caught = (f) => { try { return String(f()) } catch (e) { return e.code } };
        exports.result = [
            Buffer.from([1, 256, -1]).toString('hex'), Buffer.from(new Uint16Array([1, 256])).toString('hex'),
            Buffer.from({ type: 'Buffer', data: [1, 2] }).toString('hex'), Buffer.from(new String('str')).toString(),
            Buffer.from(new ArrayBuffer(8), 2, 4).length, Buffer.of(1, 2).toString('hex'),
            Buffer.concat([Buffer.from('a'), new Uint8Array([98])]).toString(), Buffer.concat([Buffer.from('abc')], 2).toString(),
            Buffer.compare(Buffer.from('a'), Buffer.from('b')), Buffer.isBuffer(Buffer.alloc(1)), Buffer.isBuffer(new Uint8Array(1)),
            Buffer.alloc(5, 'ab').toString(), Buffer.alloc(4, 0x41).toString(), Buffer.alloc(8).fill('xy', 2, 6).toString('hex'),
            Buffer.from('hello world').indexOf('o'), Buffer.from('hello world').lastIndexOf('o'),
            Buffer.from('hello').includes('ell'), Buffer.from('hello').indexOf(Buffer.from('lo')), Buffer.from('hello').indexOf(108, 3),
            Buffer.from([1, 2, 3, 4]).swap16().toString('hex'), Buffer.from([1, 2, 3, 4]).swap32().toString('hex'),
            caught(() => Buffer.from([1, 2, 3]).swap16()),
            (() => { const t = Buffer.alloc(4); const n = Buffer.from('xyz').copy(t, 1); return n + ':' + t.toString('hex') })(),
            Buffer.from('abcdef').slice(1, 3).toString(), Buffer.isBuffer(Buffer.from('abc').subarray(1)),
            Buffer.from('abcdef').subarray(-2).toString(),
            (() => { const p = Buffer.from('abc'); p.slice(1)[0] = 0x7a; return p.toString() })(),
            Buffer.from('abc').equals(Buffer.from('abc')), Buffer.from('abc').compare(Buffer.from('abd')),
            (() => { const x = Buffer.from('hello'); return x.write('XY', 1) + ':' + x.toString() })(),
            Buffer.alloc(3).write('€€'), Buffer('abc').toString(), new Buffer(3).length,
            Buffer.prototype instanceof Uint8Array, Buffer.from('ab').map(x => x + 1) instanceof Buffer,
            Buffer.from('abc').parent instanceof ArrayBuffer, require('util').inspect(Buffer.from('hi')),
            caught(() => Buffer.from(5)), caught(() => Buffer.alloc(-1)), caught(() => Buffer.concat([1])),
        ].map(String);
    "#);
    assert_eq!(
        result,
        [
            "0100ff",
            "0100",
            "0102",
            "str",
            "4",
            "0102",
            "ab",
            "ab",
            "-1",
            "true",
            "false",
            "ababa",
            "AAAA",
            "0000787978790000",
            "4",
            "7",
            "true",
            "3",
            "3",
            "02010403",
            "04030201",
            "ERR_INVALID_BUFFER_SIZE",
            "3:0078797a",
            "bc",
            "true",
            "ef",
            "azc",
            "true",
            "-1",
            "2:hXYlo",
            "3",
            "abc",
            "3",
            "true",
            "true",
            "true",
            "<Buffer 68 69>",
            "ERR_INVALID_ARG_TYPE",
            "ERR_OUT_OF_RANGE",
            "ERR_INVALID_ARG_TYPE",
        ]
    );
}

#[test]
fn the_buffer_module() {
    let result = run(r#"
        const buffer = require('node:buffer');
        const { Buffer: B, kMaxLength, constants, SlowBuffer, isUtf8, isAscii, transcode, INSPECT_MAX_BYTES } = buffer;
        exports.result = [
            B === Buffer, kMaxLength === constants.MAX_LENGTH, typeof constants.MAX_STRING_LENGTH, typeof SlowBuffer,
            SlowBuffer(3).length, isUtf8(Buffer.from([0xff])), isUtf8(Buffer.from('é')), isAscii(Buffer.from('a')),
            transcode(Buffer.from('€'), 'utf8', 'ucs2').toString('hex'), transcode(Buffer.from('€a'), 'utf8', 'latin1').toString(),
            INSPECT_MAX_BYTES, buffer.Blob === Blob, buffer.atob === atob,
        ].map(String);
    "#);
    assert_eq!(
        result,
        [
            "true", "true", "number", "function", "3", "false", "true", "true", "ac20", "?a", "50",
            "true", "true"
        ]
    );
}

//! `atob`/`btoa`, `TextEncoder`/`TextDecoder` and Node's `Buffer`, as plugin bundles use them.

use verve::Runtime;

fn js(runtime: &mut Runtime, source: &str) -> String {
    let value = runtime
        .eval(source)
        .unwrap_or_else(|e| panic!("{source}: {e}"));
    verve::print(&value)
}

#[test]
fn bytes_and_text_answer_as_a_browser_and_node_do() {
    let mut rt = Runtime::new();
    let cases = [
        // atob and btoa: binary strings.
        ("btoa('hello')", "aGVsbG8="),
        ("atob('aGVsbG8=')", "hello"),
        ("atob(btoa('\\xff\\x00a'))", "\u{ff}\u{0}a"),
        (
            "(() => { try { btoa('中') } catch (e) { return 'threw' } })()",
            "threw",
        ),
        // TextEncoder and TextDecoder: UTF-8 both ways, a view of a larger buffer included.
        // Their methods are on their prototypes, as bundles reach them.
        (
            "TextDecoder.prototype.decode.bind(new TextDecoder)(new Uint8Array([104, 105]))",
            "hi",
        ),
        (
            "new TextEncoder() instanceof TextEncoder && typeof TextEncoder.prototype.encode",
            "function",
        ),
        (
            "class Mine extends TextDecoder {}; new Mine().decode(new Uint8Array([97]))",
            "a",
        ),
        ("new TextEncoder().encode('é中').length", "5"),
        (
            "Array.from(new TextEncoder().encode('é')).join()",
            "195,169",
        ),
        (
            "new TextDecoder().decode(new TextEncoder().encode('Su 素'))",
            "Su 素",
        ),
        (
            "new TextDecoder().decode(new Uint8Array([0, 104, 105, 0]).subarray(1, 3))",
            "hi",
        ),
        (
            "new TextDecoder('utf-8').decode(new Uint8Array([0xef, 0xbb, 0xbf, 0x61]).buffer)",
            "a",
        ),
        (
            "new TextDecoder('utf-16le').decode(new Uint8Array([0x61, 0, 0x62, 0]))",
            "ab",
        ),
        (
            "(() => { try { new TextDecoder('utf-8', { fatal: true }).decode(new Uint8Array([0xff])) } catch (e) { return 'threw' } })()",
            "threw",
        ),
        ("new TextDecoder().encoding", "utf-8"),
        // Buffer: a Uint8Array, with Node's encodings.
        ("Buffer.from('hello').toString('base64')", "aGVsbG8="),
        ("Buffer.from('aGVsbG8=', 'base64').toString()", "hello"),
        ("Buffer.from('cafe', 'hex')[0]", "202"),
        ("Buffer.from([104, 105]).toString('hex')", "6869"),
        ("Buffer.from('素').length", "3"),
        ("Buffer.from('a') instanceof Uint8Array", "true"),
        (
            "Buffer.isBuffer(Buffer.alloc(2)) && !Buffer.isBuffer(new Uint8Array(2))",
            "true",
        ),
        (
            "Buffer.concat([Buffer.from('ab'), Buffer.from('cd')]).toString()",
            "abcd",
        ),
        ("Buffer.byteLength('é中')", "5"),
        ("Buffer.alloc(3, 'ab').toString()", "aba"),
        ("Buffer.from('hello').slice(1, 3).toString()", "el"),
        (
            "(() => { const b = Buffer.from('hello'); b.slice(0, 1)[0] = 72; return b.toString() })()",
            "Hello",
        ),
        (
            "Buffer.from(new Uint8Array([1, 2, 3]).buffer, 1, 2).length",
            "2",
        ),
        (
            "JSON.stringify(Buffer.from('hi'))",
            "{\"type\":\"Buffer\",\"data\":[104,105]}",
        ),
        ("Buffer.from('hi').equals(Buffer.from([104, 105]))", "true"),
        ("Buffer.from('hello world').toString('utf8', 6)", "world"),
        ("Buffer.from('-_8', 'base64url').toString('base64')", "+/8="),
    ];
    for (source, expected) in cases {
        assert_eq!(js(&mut rt, source), expected, "{source}");
    }
}

/// Boa 0.22 read a class method's computed name from the wrong environment when it named a
/// binding of an enclosing function (see `crates/boa_ast/VENDORED.md`); Effect's bundles, in
/// Day Planner among others, define their errors that way.
#[test]
fn a_computed_method_name_reads_the_enclosing_functions_bindings() {
    let mut rt = Runtime::new();
    for source in [
        "(function(){ var k = 'x'; function f(){ return class { [k](){ return 7 } } } return new (f())().x() })()",
        "(function(){ const k = Symbol.for('x'); const C = function(){ class E extends globalThis.Error { [k](){ return 7 } } return E }(); return new C()[k]() })()",
        "(function(){ let k = 'g'; function f(){ class C { static [k](){ return 7 } } return C } return f().g() })()",
        "(function(){ const k = 'v'; function f(){ class C { get [k](){ return 7 } } return C } return new (f())().v })()",
    ] {
        assert_eq!(js(&mut rt, source), "7", "{source}");
    }
}

/// Boa 0.22 lost `this` in an arrow function within another that did not itself mention
/// `this` (see `crates/boa_ast/VENDORED.md`); settings tabs written with declarative settings
/// — Admonition's, Day Planner's — read their plugin so.
#[test]
fn an_arrow_within_an_arrow_keeps_the_methods_this() {
    let mut rt = Runtime::new();
    for (source, expected) in [
        (
            "(function(){ return () => (() => this.p)() }).call({ p: 1 })()",
            "1",
        ),
        (
            "(function(){ const a = () => { const b = () => this.p; return b() }; return a() }).call({ p: 2 })",
            "2",
        ),
        (
            "(function(){ return () => () => () => this.p }).call({ p: 3 })()()()",
            "3",
        ),
        (
            "(() => { const c = new (class { p = 5; items(){ return [{ render: (e) => { let got; e(o => got = this.p); return got } }] } })(); return c.items()[0].render(f => f()) })()",
            "5",
        ),
        ("new (class { x = 6; f = () => () => this.x })().f()()", "6"),
        (
            "String((function(){ return () => function(){ return (() => this)() }.call('own') }).call({})())",
            "own",
        ),
    ] {
        assert_eq!(js(&mut rt, source), expected, "{source}");
    }
}

#[test]
fn urls_parse_and_their_params_follow_them() {
    let mut rt = Runtime::new();
    let cases = [
        (
            "new URL('https://user:pw@example.com:8080/a/b?x=1&y=two#top').host",
            "example.com:8080",
        ),
        ("new URL('https://example.com:443/').port", ""),
        (
            "new URL('../c?q', 'https://example.com/a/b/').href",
            "https://example.com/a/c?q",
        ),
        (
            "new URL('https://example.com/p?x=1').searchParams.get('x')",
            "1",
        ),
        (
            "(() => { const u = new URL('https://e.com/?a=1'); u.searchParams.append('b', 'x y'); return u.href })()",
            "https://e.com/?a=1&b=x+y",
        ),
        (
            "(() => { const u = new URL('https://e.com/'); u.search = '?k=v'; return u.searchParams.get('k') })()",
            "v",
        ),
        (
            "(() => { const u = new URL('https://e.com/a'); u.hash = 'h'; u.pathname = '/b'; return u.toString() })()",
            "https://e.com/b#h",
        ),
        (
            "new URL('obsidian://open?vault=V&file=F').protocol",
            "obsidian:",
        ),
        ("new URL('https://e.com/').origin", "https://e.com"),
        (
            "(() => { try { new URL('not a url') } catch (e) { return e instanceof TypeError } })()",
            "true",
        ),
        (
            "URL.canParse('https://e.com') && !URL.canParse('nope')",
            "true",
        ),
        (
            "new URLSearchParams('?a=1&a=2&b=3').getAll('a').join()",
            "1,2",
        ),
        (
            "new URLSearchParams({ q: 'a b', n: 1 }).toString()",
            "q=a+b&n=1",
        ),
        (
            "[...new URLSearchParams([['x', '1'], ['y', '2']])].map(([k, v]) => k + v).join()",
            "x1,y2",
        ),
        ("new URLSearchParams('a=1&b=2').size", "2"),
        (
            "(() => { const p = new URLSearchParams('b=2&a=1&a=3'); p.set('a', '9'); p.sort(); return p.toString() })()",
            "a=9&b=2",
        ),
        (
            "JSON.stringify(new URL('https://e.com/x'))",
            "\"https://e.com/x\"",
        ),
        ("URL.createObjectURL({}).startsWith('blob:')", "true"),
    ];
    for (source, expected) in cases {
        assert_eq!(js(&mut rt, source), expected, "{source}");
    }
}

#[test]
fn web_globals_answer_as_a_browser_does() {
    let mut rt = Runtime::new();
    let cases = [
        (
            "/^[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/.test(crypto.randomUUID())",
            "true",
        ),
        ("crypto.randomUUID() !== crypto.randomUUID()", "true"),
        ("crypto.getRandomValues(new Uint8Array(16)).length", "16"),
        (
            "(() => { const a = new Uint32Array(4); crypto.getRandomValues(a); return a.some(x => x !== 0) })()",
            "true",
        ),
        (
            "(() => { const o = { d: new Date(5), m: new Map([[1, { x: 2 }]]), s: new Set([3]), a: [1, [2]] }; o.self = o; const c = structuredClone(o); return [c !== o, c.d.getTime(), c.m.get(1).x, c.s.has(3), c.a[1][0], c.self === c, c.m.get(1) !== o.m.get(1)].join() })()",
            "true,5,2,true,2,true,true",
        ),
        (
            "(() => { try { structuredClone(() => 1) } catch (e) { return 'refused' } })()",
            "refused",
        ),
        ("typeof performance.now()", "number"),
        (
            "(() => { const c = new AbortController(); const heard = []; c.signal.addEventListener('abort', () => heard.push('heard')); c.abort(); return [c.signal.aborted, c.signal.reason.name, heard.join()].join() })()",
            "true,AbortError,heard",
        ),
        ("AbortSignal.abort().aborted", "true"),
        (
            "(() => { try { AbortSignal.abort('why').throwIfAborted() } catch (e) { return e } })()",
            "why",
        ),
        (
            "new Blob(['ab', new Uint8Array([99])], { type: 'Text/Plain' }).size",
            "3",
        ),
        ("new Blob([], { type: 'Text/Plain' }).type", "text/plain"),
        ("new File(['x'], 'n.md').name", "n.md"),
        ("new File(['x'], 'n.md') instanceof Blob", "true"),
    ];
    for (source, expected) in cases {
        assert_eq!(js(&mut rt, source), expected, "{source}");
    }
    // What answers a promise: settled by the time `eval` returns, its microtasks run.
    js(
        &mut rt,
        "globalThis.out = []; new Blob(['héllo']).text().then(t => out.push(t)); new Blob(['abcdef']).slice(1, -1).text().then(t => out.push(t)); crypto.subtle.digest('SHA-256', new TextEncoder().encode('abc')).then(b => out.push(Array.from(new Uint8Array(b)).slice(0, 4).map(x => x.toString(16).padStart(2, '0')).join('')))",
    );
    assert_eq!(js(&mut rt, "out.join(' ')"), "héllo bcde ba7816bf");
}

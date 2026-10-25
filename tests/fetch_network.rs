//! `fetch` on the network: against servers in the same realm (Node's `http` and `https` on the
//! loopback, which `fetch` shares a reactor with) and, where the client is elsewhere — an
//! isolate, a worker — a plain one on a Rust thread. Every server is local; nothing here needs the
//! internet.

use std::io::{Read, Write};
use std::time::{Duration, Instant};

use verve::isolate::{Guest, Isolate, Options, Outbox};
use verve::{ListenPermission, NetworkRule, Permissions, Runtime};

#[path = "support/tls.rs"]
mod tls;

/// Connecting to 127.0.0.1 and serving on the loopback; nothing else.
fn loopback() -> Permissions {
    Permissions {
        outbound: vec![NetworkRule {
            host: Some("127.0.0.1".into()),
            port: None,
        }],
        listen: ListenPermission::Loopback,
        dns: false,
        child_process: false,
    }
}

/// Runs `body`, an async function's body that sets `result`, on a runtime of its own thread;
/// answers `result` as JSON. `setup` prepares the runtime first.
fn run_with(body: &str, setup: impl FnOnce(&mut Runtime) + Send + 'static) -> String {
    let script = format!(
        "(async () => {{ {body}\n}})().catch(e => {{ globalThis.result = ['threw', String(e), e && e.cause ? String(e.cause.code || e.cause) : null]; }});"
    );
    std::thread::Builder::new()
        .stack_size(16 * 1024 * 1024)
        .spawn(move || {
            verve::set_thread_stack(16 * 1024 * 1024);
            let mut rt = Runtime::new();
            setup(&mut rt);
            rt.eval(&tls::prelude()).unwrap();
            let entry =
                std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fetch-entry.cjs");
            rt.load_module(&entry, &script).unwrap();
            let deadline = Instant::now() + Duration::from_secs(30);
            loop {
                rt.run_network_until(Some(Instant::now() + Duration::from_millis(5)));
                rt.run_due();
                rt.run_workers();
                let transport = rt.network_diagnostics();
                assert!(
                    transport.queued_read_bytes <= (transport.open_sockets + 1) * 64 * 1024,
                    "unbounded network read queue: {transport:?}"
                );
                if rt
                    .eval("globalThis.result !== undefined")
                    .unwrap()
                    .to_boolean()
                {
                    break;
                }
                assert!(
                    Instant::now() < deadline,
                    "fetch test timed out: {:?}",
                    rt.take_console()
                );
                std::thread::sleep(Duration::from_millis(1));
            }
            verve::print(&rt.eval("JSON.stringify(globalThis.result)").unwrap())
        })
        .unwrap()
        .join()
        .unwrap()
}

fn run(body: &str) -> String {
    run_with(body, |rt| rt.set_permissions(loopback()))
}

/// A one-request-per-connection HTTP server on a Rust thread: each connection is answered with
/// `hello` and closed. Answers its port.
fn plain_server(connections: usize) -> u16 {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    std::thread::spawn(move || {
        for stream in listener.incoming().take(connections) {
            let mut stream = stream.unwrap();
            let mut seen = Vec::new();
            let mut buffer = [0; 1024];
            while !seen.windows(4).any(|w| w == b"\r\n\r\n") {
                match stream.read(&mut buffer) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => seen.extend_from_slice(&buffer[..n]),
                }
            }
            let _ = stream.write_all(
                b"HTTP/1.1 200 OK\r\nContent-Length: 5\r\nConnection: close\r\n\r\nhello",
            );
        }
    });
    port
}

/// A server in the realm: `handler(req, res)` for every request; `globalThis.port` once it
/// listens.
const SERVE: &str = r#"
    const http = require('http');
    const serve = (handler) => new Promise(done => {
        const server = http.createServer(handler);
        server.listen(0, '127.0.0.1', () => done(server));
    });
    const base = (server) => 'http://127.0.0.1:' + server.address().port;
    const collect = (req) => new Promise(done => {
        const parts = []; req.on('data', b => parts.push(b)); req.on('end', () => done(Buffer.concat(parts)));
    });
"#;

#[test]
fn sse_events_arrive_as_they_are_sent() {
    // The server sends the next event only after the client has read the last one: a fetch
    // that waited for the whole body would never see the first.
    let body = format!(
        r#"{SERVE}
        let next, received;
        const server = await serve(async (req, res) => {{
            received = [req.method, req.headers['content-type'], req.headers.authorization, (await collect(req)).toString()];
            res.writeHead(200, {{ 'content-type': 'text/event-stream' }});
            res.write('data: {{"n":1}}\n\n');
            next = () => {{ res.write('data: {{"n":2}}\n\n'); next = () => res.end('data: {{"n":3}}\n\ndata: [DONE]\n\n'); }};
        }});
        class EventSourceParserStream extends TransformStream {{
            constructor() {{
                let buffer = '';
                super({{ transform(text, controller) {{
                    buffer += text;
                    let at;
                    while ((at = buffer.indexOf('\n\n')) >= 0) {{
                        const block = buffer.slice(0, at); buffer = buffer.slice(at + 2);
                        controller.enqueue({{ data: block.replace(/^data: /, '') }});
                    }}
                }} }});
            }}
        }}
        const controller = new AbortController();
        const response = await fetch(base(server) + '/chat/completions', {{
            method: 'POST',
            headers: {{ 'Content-Type': 'application/json', Authorization: 'Bearer key' }},
            body: JSON.stringify({{ stream: true }}),
            signal: controller.signal,
        }});
        const events = [];
        const reader = response.body
            .pipeThrough(new TextDecoderStream())
            .pipeThrough(new EventSourceParserStream())
            .getReader();
        while (true) {{
            const {{ done, value }} = await reader.read();
            if (done) break;
            events.push(value.data);
            if (next) next();
        }}
        server.close();
        globalThis.result = [response.ok, response.status, response.headers.get('content-type'), received, events];
    "#
    );
    assert_eq!(
        run(&body),
        r#"[true,200,"text/event-stream",["POST","application/json","Bearer key","{\"stream\":true}"],["{\"n\":1}","{\"n\":2}","{\"n\":3}","[DONE]"]]"#
    );
}

#[test]
fn abort_mid_stream_errors_the_body_and_closes_the_connection() {
    let body = format!(
        r#"{SERVE}
        let closed = new Promise(done => globalThis.serverClosed = done);
        const server = await serve((req, res) => {{
            res.writeHead(200);
            res.write('first');
            req.socket.on('close', () => globalThis.serverClosed(true));
        }});
        const controller = new AbortController();
        const response = await fetch(base(server), {{ signal: controller.signal }});
        const reader = response.body.getReader();
        const first = new TextDecoder().decode((await reader.read()).value);
        controller.abort();
        let error;
        try {{ await reader.read(); }} catch (e) {{ error = e; }}
        const sawClose = await closed;
        server.close();
        globalThis.result = [first, error.name, error instanceof DOMException, sawClose];
    "#
    );
    assert_eq!(run(&body), r#"["first","AbortError",true,true]"#);
}

#[test]
fn abort_before_and_while_waiting_for_the_response() {
    let body = format!(
        r#"{SERVE}
        let requests = 0;
        const server = await serve((req, res) => {{ requests++; /* never answers */ }});
        const early = new AbortController();
        early.abort();
        const before = await fetch(base(server), {{ signal: early.signal }}).catch(e => e.name);
        const late = new AbortController();
        const pending = fetch(base(server), {{ signal: late.signal }}).catch(e => e);
        await new Promise(r => setTimeout(r, 50));
        late.abort(new Error('changed my mind'));
        const during = await pending;
        const timed = await fetch(base(server), {{ signal: AbortSignal.timeout(30) }}).catch(e => e.name);
        server.closeAllConnections();
        server.close();
        globalThis.result = [before, during.message, timed, requests];
    "#
    );
    assert_eq!(
        run(&body),
        r#"["AbortError","changed my mind","TimeoutError",2]"#
    );
}

/// `text` with every `localhost:<port>` written `localhost:PORT`.
fn mask_ports(text: &str) -> String {
    let mut out = String::new();
    let mut rest = text;
    while let Some(at) = rest.find("localhost:") {
        out.push_str(&rest[..at + 10]);
        rest = rest[at + 10..].trim_start_matches(|c: char| c.is_ascii_digit());
        out.push_str("PORT");
    }
    out + rest
}

#[test]
fn hosts_the_permissions_refuse_fail_as_undici_fails() {
    let body = format!(
        r#"{SERVE}
        const server = await serve((req, res) => {{
            if (req.url === '/away') {{ res.writeHead(302, {{ location: 'http://localhost:' + server.address().port + '/' }}); res.end(); }}
            else res.end('ok');
        }});
        const port = server.address().port;
        const describe = (e) => [e.constructor.name, e.message, e.cause.code, e.cause.message, e.cause.resource];
        const refused = await fetch('http://localhost:' + port + '/').catch(describe);
        const redirected = await fetch(base(server) + '/away').catch(e => [e.message, e.cause.code]);
        // Node's `http` is refused by the same rule, with its own error.
        const viaHttp = await new Promise(done => http.get('http://localhost:' + port + '/', () => done('connected')).on('error', e => done(e.code)));
        const allowed = await (await fetch(base(server))).text();
        const closed = await serve(() => {{}});
        const closedPort = closed.address().port;
        await new Promise(done => closed.close(done));
        const unreachable = await fetch('http://127.0.0.1:' + closedPort + '/').catch(e => [e.message, e.cause.code]);
        server.close();
        globalThis.result = [refused, redirected, viaHttp, allowed, unreachable];
    "#
    );
    assert_eq!(
        mask_ports(&run(&body)),
        r#"[["TypeError","fetch failed","ERR_ACCESS_DENIED","Verve's permissions do not allow connecting to localhost:PORT","localhost:PORT"],["fetch failed","ERR_ACCESS_DENIED"],"ENOSYS","ok",["fetch failed","ECONNREFUSED"]]"#
    );
}

#[test]
fn redirects_follow_return_or_refuse() {
    let body = format!(
        r#"{SERVE}
        const server = await serve(async (req, res) => {{
            const [, kind, n] = req.url.split('/');
            const body = (await collect(req)).toString();
            if (kind === 'chain' && +n > 0) {{ res.writeHead(302, {{ location: '/chain/' + (n - 1) }}); return res.end(); }}
            if (kind === 'see-other') {{ res.writeHead(303, {{ location: '/echo' }}); return res.end(); }}
            if (kind === 'temporary') {{ res.writeHead(307, {{ location: '/echo' }}); return res.end(); }}
            res.end(JSON.stringify([req.method, body, req.url]));
        }});
        const followed = await fetch(base(server) + '/chain/3');
        const followedBody = await followed.json();
        const manual = await fetch(base(server) + '/chain/1', {{ redirect: 'manual' }});
        const refused = await fetch(base(server) + '/chain/1', {{ redirect: 'error' }}).catch(e => e.message);
        const tooMany = await fetch(base(server) + '/chain/21').catch(e => [e.message, e.cause.message]);
        const twenty = (await fetch(base(server) + '/chain/20')).status;
        const seeOther = await (await fetch(base(server) + '/see-other', {{ method: 'POST', body: 'gone' }})).json();
        const temporary = await (await fetch(base(server) + '/temporary', {{ method: 'POST', body: 'kept' }})).json();
        server.close();
        globalThis.result = [
            [followed.status, followed.redirected, followed.url.endsWith('/chain/0'), followedBody],
            [manual.status, manual.redirected, manual.headers.get('location')],
            refused, tooMany, twenty, seeOther, temporary,
        ];
    "#
    );
    assert_eq!(
        run(&body),
        r#"[[200,true,true,["GET","","/chain/0"]],[302,false,"/chain/0"],"fetch failed",["fetch failed","redirect count exceeded"],200,["GET","","/echo"],["POST","kept","/echo"]]"#
    );
}

#[test]
fn compressed_bodies_are_decoded() {
    let body = format!(
        r#"{SERVE}
        const zlib = require('zlib');
        const text = 'data: hello\n\n'.repeat(500);
        let asked;
        const server = await serve((req, res) => {{
            asked = req.headers['accept-encoding'];
            const coding = req.url.slice(1);
            const encoded = {{
                gzip: zlib.gzipSync(text), deflate: zlib.deflateSync(text),
                'deflate-raw': zlib.deflateRawSync(text), br: zlib.brotliCompressSync(text),
                identity: Buffer.from(text),
            }}[coding];
            res.writeHead(200, {{ 'content-encoding': coding === 'deflate-raw' ? 'deflate' : coding }});
            // In small pieces, so the decoders see the stream as it arrives.
            for (let at = 0; at < encoded.length; at += 100) res.write(encoded.subarray(at, at + 100));
            res.end();
        }});
        const out = [];
        for (const coding of ['gzip', 'deflate', 'deflate-raw', 'br', 'identity']) {{
            const response = await fetch(base(server) + '/' + coding);
            out.push([coding, (await response.text()) === text, response.headers.get('content-encoding')]);
        }}
        server.close();
        globalThis.result = [asked, out];
    "#
    );
    assert_eq!(
        run(&body),
        r#"["gzip, deflate, br",[["gzip",true,"gzip"],["deflate",true,"deflate"],["deflate-raw",true,"deflate"],["br",true,"br"],["identity",true,"identity"]]]"#
    );
}

#[test]
fn request_bodies_of_every_kind() {
    let body = format!(
        r#"{SERVE}
        const server = await serve(async (req, res) => {{
            const bytes = await collect(req);
            res.end(JSON.stringify([req.headers['content-type'] || null, req.headers['content-length'] || null, bytes.toString()]));
        }});
        const send = async (body, extra) => (await fetch(base(server), {{ method: 'POST', body, ...extra }})).json();
        const form = new FormData();
        form.append('field', 'value');
        form.append('file', new Blob(['contents'], {{ type: 'text/plain' }}), 'a.txt');
        const formEcho = await send(form);
        const stream = new ReadableStream({{ start(c) {{ c.enqueue(new TextEncoder().encode('str')); c.enqueue(new TextEncoder().encode('eam')); c.close(); }} }});
        globalThis.result = [
            await send('text'),
            await send(new TextEncoder().encode('typed')),
            await send(new TextEncoder().encode('buffer').buffer),
            await send(new Blob(['blob'], {{ type: 'application/x-blob' }})),
            await send(new URLSearchParams({{ a: '1', b: 'two' }})),
            [formEcho[0].startsWith('multipart/form-data; boundary='), formEcho[2].includes('name="field"\r\n\r\nvalue'), formEcho[2].includes('filename="a.txt"')],
            await send(stream, {{ duplex: 'half' }}),
            await send(undefined),
        ];
        server.close();
    "#
    );
    assert_eq!(
        run(&body),
        r#"[["text/plain;charset=UTF-8","4","text"],[null,"5","typed"],[null,"6","buffer"],["application/x-blob","4","blob"],["application/x-www-form-urlencoded;charset=UTF-8","9","a=1&b=two"],[true,true,true],[null,"6","stream"],[null,"0",""]]"#
    );
}

#[test]
fn a_slow_reader_holds_back_the_sender() {
    // 32 MiB offered; the reader takes one chunk and then waits. What the server managed to
    // send meanwhile is what the socket buffers hold, not the whole body.
    let body = format!(
        r#"{SERVE}
        let sent = 0;
        const total = 32 * 1024 * 1024, piece = Buffer.alloc(64 * 1024, 120);
        const server = await serve((req, res) => {{
            const pump = () => {{
                while (sent < total) {{
                    sent += piece.length;
                    if (!res.write(piece)) return res.once('drain', pump);
                }}
                res.end();
            }};
            pump();
        }});
        const response = await fetch(base(server));
        const reader = response.body.getReader();
        await reader.read();
        await new Promise(r => setTimeout(r, 300));
        const whileWaiting = sent;
        let read = 0;
        for (;;) {{ const {{ done, value }} = await reader.read(); if (done) break; read += value.length; }}
        server.close();
        globalThis.result = [whileWaiting < total / 2, sent === total, read > total - 2 * piece.length];
    "#
    );
    assert_eq!(run(&body), r#"[true,true,true]"#);
}

#[test]
fn https_with_a_trusted_test_certificate() {
    let body = r#"
        const https = require('https');
        const server = await new Promise(done => {
            const s = https.createServer({ cert: TLS_CERT, key: TLS_KEY }, (req, res) => res.end('secure ' + req.headers.host));
            s.listen(0, '127.0.0.1', () => done(s));
        });
        const url = 'https://127.0.0.1:' + server.address().port + '/';
        const response = await fetch(url);
        const text = await response.text();
        server.close();
        globalThis.result = [response.status, text.startsWith('secure 127.0.0.1:'), response.url === url];
    "#;
    assert_eq!(
        run_with(body, |rt| {
            rt.set_permissions(loopback());
            assert_eq!(
                rt.add_trusted_certificates(tls::localhost().cert.as_bytes())
                    .unwrap(),
                1
            );
        }),
        r#"[200,true,true]"#
    );
    // Not trusted, the same server is a network error.
    let untrusted = r#"
        const https = require('https');
        const server = await new Promise(done => {
            const s = https.createServer({ cert: TLS_CERT, key: TLS_KEY }, (req, res) => res.end('secure'));
            s.on('tlsClientError', () => {});
            s.listen(0, '127.0.0.1', () => done(s));
        });
        const failed = await fetch('https://127.0.0.1:' + server.address().port + '/').catch(e => [e.message, typeof e.cause.code]);
        server.close();
        globalThis.result = failed;
    "#;
    assert_eq!(run(untrusted), r#"["fetch failed","string"]"#);
}

#[test]
fn data_and_blob_addresses_need_no_network() {
    let body = r#"
        const data = await fetch('data:text/plain;base64,aGVsbG8=');
        const plain = await fetch('data:,a%20b');
        const url = URL.createObjectURL(new Blob(['from a blob']));
        const blob = await fetch(url);
        const missing = await fetch('blob:nodedata:missing').catch(e => e.message);
        const scheme = await fetch('ftp://example.com/').catch(e => [e.message, e.cause.message]);
        const denied = await fetch('http://127.0.0.1:9/').catch(e => e.cause.code);
        globalThis.result = [
            [data.status, data.headers.get('content-type'), await data.text()],
            [plain.headers.get('content-type'), await plain.text()],
            [blob.status, await blob.text()],
            missing, scheme, denied,
        ];
    "#;
    // No permissions at all.
    assert_eq!(
        run_with(body, |_| {}),
        r#"[[200,"text/plain","hello"],["text/plain;charset=US-ASCII","a b"],[200,"from a blob"],"fetch failed",["fetch failed","unknown scheme 'ftp:'"],"ERR_ACCESS_DENIED"]"#
    );
}

#[test]
fn workers_fetch_with_the_grant_their_page_gives_them() {
    let port = plain_server(1);
    let body = format!(
        r#"
        const source = `fetch('http://127.0.0.1:{port}/').then(r => r.text()).then(t => postMessage(t), e => postMessage(e.message + ' ' + e.cause.code));`;
        const ask = () => new Promise(done => {{
            const worker = new Worker('data:text/javascript,' + encodeURIComponent(source));
            worker.onmessage = (e) => {{ done(e.data); worker.terminate(); }};
        }});
        globalThis.result = [await ask()];
    "#
    );
    assert_eq!(
        run_with(&body, |rt| rt.set_worker_permissions(loopback())),
        r#"["hello"]"#
    );
    // Without a grant, a worker is refused even when its page is not.
    assert_eq!(
        run_with(&body, |rt| rt.set_permissions(loopback())),
        r#"["fetch failed ERR_ACCESS_DENIED"]"#
    );
}

/// A runtime in an isolate, its embedder granting the loopback and reading `result` back.
struct Fetcher {
    runtime: Runtime,
}

impl Guest for Fetcher {
    type View = ();
    type Event = String;

    fn tick(&mut self) {
        self.runtime.run_due();
        self.runtime.run_microtasks();
    }

    fn next_due(&self) -> Option<Duration> {
        let now = self.runtime.now();
        self.runtime
            .next_due()
            .map(|at| Duration::from_secs_f64((at - now).max(0.) / 1000.))
    }

    fn drain(&mut self, out: &mut Outbox<String>) {
        if let Ok(value) = self.runtime.eval("globalThis.result")
            && !value.is_undefined()
        {
            out.push(verve::print(&value));
            let _ = self.runtime.eval("globalThis.result = undefined");
        }
    }

    fn publish(&mut self, _: u64, _: bool) -> Option<()> {
        None
    }
}

#[test]
fn isolates_fetch_on_their_own_thread() {
    let port = plain_server(1);
    let isolate = Isolate::spawn(
        Options {
            name: "fetcher".into(),
            ..Options::default()
        },
        || Fetcher {
            runtime: Runtime::new(),
        },
        || {},
    )
    .unwrap();
    isolate.send(move |guest, _| {
        guest.runtime.set_permissions(loopback());
        guest
            .runtime
            .eval(&format!(
                "fetch('http://127.0.0.1:{port}/').then(r => r.text()).then(t => globalThis.result = t)"
            ))
            .unwrap();
    });
    let started = Instant::now();
    let mut events = Vec::new();
    while events.is_empty() && started.elapsed() < Duration::from_secs(20) {
        events.extend(isolate.take_events());
        std::thread::sleep(Duration::from_millis(2));
    }
    assert_eq!(events, ["hello"]);
}

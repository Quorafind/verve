//! Node 24's environment proxy: `fetch`, `WebSocket` and the `http`/`https` global agents
//! through an HTTP proxy, as a sandbox that leaves a process nothing but its proxy needs. The
//! proxy here is a small one on Rust threads that records what it is asked and sends every
//! name to 127.0.0.1 — the targets' names never resolve anywhere else, so a request that
//! arrives proves it was not resolved by the realm.

use std::io::{Read, Write};
use std::net::{Shutdown, TcpListener, TcpStream};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use verve::{ListenPermission, NetworkRule, Permissions, ProxyEnv, Runtime};

#[path = "support/tls.rs"]
mod tls;

const VERVE: &str = env!("CARGO_BIN_EXE_verve");

// ─── The proxy ──────────────────────────────────────────────────────────────────────────────

struct Proxy {
    port: u16,
    heads: Arc<Mutex<Vec<String>>>,
    connections: Arc<AtomicUsize>,
}

impl Proxy {
    /// A proxy that tunnels and forwards, or one that refuses every `CONNECT` with a 403.
    fn start(refuse: bool) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let heads = Arc::new(Mutex::new(Vec::new()));
        let connections = Arc::new(AtomicUsize::new(0));
        let (seen, count) = (heads.clone(), connections.clone());
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(stream) = stream else { break };
                count.fetch_add(1, Ordering::SeqCst);
                let seen = seen.clone();
                std::thread::spawn(move || serve(stream, refuse, seen));
            }
        });
        Self {
            port,
            heads,
            connections,
        }
    }

    fn url(&self) -> String {
        format!("http://127.0.0.1:{}", self.port)
    }

    fn heads(&self) -> Vec<String> {
        self.heads.lock().unwrap().clone()
    }

    fn connections(&self) -> usize {
        self.connections.load(Ordering::SeqCst)
    }
}

fn read_head(stream: &mut TcpStream) -> Option<String> {
    let mut head = Vec::new();
    let mut byte = [0; 1];
    while !head.ends_with(b"\r\n\r\n") {
        match stream.read(&mut byte) {
            Ok(1) => head.push(byte[0]),
            _ => return None,
        }
    }
    Some(String::from_utf8_lossy(&head).into_owned())
}

fn port_of(authority: &str, default: u16) -> u16 {
    match authority.rsplit_once(':') {
        Some((host, port)) if !host.ends_with(':') => port.parse().unwrap_or(default),
        _ => default,
    }
}

fn serve(mut client: TcpStream, refuse: bool, heads: Arc<Mutex<Vec<String>>>) {
    let Some(head) = read_head(&mut client) else {
        return;
    };
    heads.lock().unwrap().push(head.clone());
    let line = head.lines().next().unwrap_or_default().to_owned();
    let mut parts = line.split(' ');
    let (method, target) = (
        parts.next().unwrap_or_default(),
        parts.next().unwrap_or_default(),
    );
    if method == "CONNECT" {
        if refuse {
            let _ = client.write_all(
                b"HTTP/1.1 403 Forbidden\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
            );
            return;
        }
        let Ok(upstream) = TcpStream::connect(("127.0.0.1", port_of(target, 443))) else {
            let _ = client.write_all(b"HTTP/1.1 502 Bad Gateway\r\n\r\n");
            return;
        };
        let _ = client.write_all(b"HTTP/1.1 200 Connection established\r\n\r\n");
        pipe(client, upstream);
    } else {
        let rest = target.strip_prefix("http://").unwrap_or(target);
        let (authority, path) = rest.split_at(rest.find('/').unwrap_or(rest.len()));
        let Ok(mut upstream) = TcpStream::connect(("127.0.0.1", port_of(authority, 80))) else {
            return;
        };
        let mut forwarded = format!(
            "{method} {} HTTP/1.1\r\n",
            if path.is_empty() { "/" } else { path }
        );
        for header in head.split("\r\n").skip(1).filter(|h| !h.is_empty()) {
            if !header
                .to_ascii_lowercase()
                .starts_with("proxy-authorization:")
            {
                forwarded.push_str(header);
                forwarded.push_str("\r\n");
            }
        }
        forwarded.push_str("\r\n");
        let _ = upstream.write_all(forwarded.as_bytes());
        pipe(client, upstream);
    }
}

fn pipe(client: TcpStream, upstream: TcpStream) {
    let (mut from_client, mut to_upstream) =
        (client.try_clone().unwrap(), upstream.try_clone().unwrap());
    std::thread::spawn(move || {
        let _ = std::io::copy(&mut from_client, &mut to_upstream);
        let _ = to_upstream.shutdown(Shutdown::Write);
    });
    let (mut client, mut upstream) = (client, upstream);
    let _ = std::io::copy(&mut upstream, &mut client);
    let _ = client.shutdown(Shutdown::Write);
}

/// A plain HTTP server on a Rust thread: each connection is answered with `hello` and closed.
fn origin(connections: usize) -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    std::thread::spawn(move || {
        for stream in listener.incoming().take(connections) {
            let mut stream = stream.unwrap();
            let _ = read_head(&mut stream);
            let _ = stream.write_all(
                b"HTTP/1.1 200 OK\r\nContent-Length: 5\r\nConnection: close\r\n\r\nhello",
            );
        }
    });
    port
}

/// A WebSocket server on a Rust thread that echoes what it is sent.
fn echo_websocket() -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    std::thread::spawn(move || {
        let (stream, _) = listener.accept().unwrap();
        let mut socket = tungstenite::accept(stream).unwrap();
        while let Ok(message) = socket.read() {
            if message.is_text() || message.is_binary() {
                let _ = socket.send(message);
            } else if message.is_close() {
                break;
            }
        }
    });
    port
}

// ─── The realm ──────────────────────────────────────────────────────────────────────────────

fn rule(host: &str, port: Option<u16>) -> NetworkRule {
    NetworkRule {
        host: Some(host.into()),
        port,
    }
}

/// These hosts, and serving on the loopback: never the proxy's own address unless listed.
fn granting(rules: Vec<NetworkRule>) -> Permissions {
    Permissions {
        outbound: rules,
        listen: ListenPermission::Loopback,
        ..Permissions::default()
    }
}

fn through(proxy: &str, no_proxy: Option<&str>) -> ProxyEnv {
    ProxyEnv {
        http_proxy: Some(proxy.into()),
        https_proxy: Some(proxy.into()),
        no_proxy: no_proxy.map(Into::into),
    }
}

/// Runs `body`, an async function's body that sets `result`, on a runtime of its own thread;
/// answers `result` as JSON.
fn run(body: &str, permissions: Permissions, proxy: Option<ProxyEnv>) -> String {
    let script = format!(
        "(async () => {{ {body}\n}})().catch(e => {{ globalThis.result = ['threw', String(e), e && e.cause ? String(e.cause.code || e.cause) : null]; }});"
    );
    std::thread::Builder::new()
        .stack_size(16 * 1024 * 1024)
        .spawn(move || {
            verve::set_thread_stack(16 * 1024 * 1024);
            let mut rt = Runtime::new();
            rt.set_permissions(permissions);
            rt.set_proxy_env(proxy);
            rt.add_trusted_certificates(tls::localhost().cert.as_bytes())
                .unwrap();
            rt.eval(&tls::prelude()).unwrap();
            let entry =
                std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/proxy-entry.cjs");
            rt.load_module(&entry, &script).unwrap();
            let deadline = Instant::now() + Duration::from_secs(30);
            loop {
                rt.run_network_until(Some(Instant::now() + Duration::from_millis(5)));
                rt.run_due();
                if rt
                    .eval("globalThis.result !== undefined")
                    .unwrap()
                    .to_boolean()
                {
                    break;
                }
                assert!(
                    Instant::now() < deadline,
                    "proxy test timed out: {:?}",
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

/// `http.get(url, options)`'s answer: `[status, body]`, or `['error', code, statusCode]`.
const GET: &str = r#"
    const answer = (module, url, options = {}) => new Promise(done => {
        const req = module.get(url, options, res => {
            const parts = []; res.on('data', b => parts.push(b));
            res.on('end', () => done([res.statusCode, Buffer.concat(parts).toString()]));
        });
        req.on('error', e => done(['error', e.code, e.statusCode]));
    });
"#;

// ─── Tests ──────────────────────────────────────────────────────────────────────────────────

#[test]
fn fetch_sends_http_to_the_proxy_with_the_whole_url() {
    let target = origin(1);
    let proxy = Proxy::start(false);
    // Only the target is granted: the configured proxy needs no rule, and `target.invalid`
    // resolves nowhere — the proxy alone knows where it is.
    let body = format!(
        "const r = await fetch('http://target.invalid:{target}/path?q=1');
         globalThis.result = [r.status, await r.text()];"
    );
    let permissions = granting(vec![rule("target.invalid", Some(target))]);
    assert_eq!(
        run(&body, permissions, Some(through(&proxy.url(), None))),
        r#"[200,"hello"]"#
    );
    let heads = proxy.heads();
    assert_eq!(heads.len(), 1);
    assert!(
        heads[0].starts_with(&format!(
            "GET http://target.invalid:{target}/path?q=1 HTTP/1.1\r\n"
        )),
        "{heads:?}"
    );
    assert!(
        heads[0].contains(&format!("Host: target.invalid:{target}\r\n")),
        "{heads:?}"
    );
    assert!(
        !heads[0]
            .to_ascii_lowercase()
            .contains("proxy-authorization")
    );
}

#[test]
fn credentials_in_the_proxy_url_become_proxy_authorization() {
    let target = origin(1);
    let proxy = Proxy::start(false);
    let url = format!("http://user:p%40ss@127.0.0.1:{}", proxy.port);
    let body = format!(
        "const r = await fetch('http://target.invalid:{target}/');
         let tunnel;
         try {{ await fetch('https://localhost:1/'); }} catch (e) {{ tunnel = e.message; }}
         globalThis.result = [await r.text(), tunnel];"
    );
    let permissions = granting(vec![rule("target.invalid", None), rule("localhost", None)]);
    assert_eq!(
        run(&body, permissions, Some(through(&url, None))),
        r#"["hello","fetch failed"]"#
    );
    // `user:p@ss`, decoded from the URL, for the forwarded request and the CONNECT alike.
    let heads = proxy.heads();
    assert_eq!(heads.len(), 2, "{heads:?}");
    for head in &heads {
        assert!(
            head.contains("Proxy-Authorization: Basic dXNlcjpwQHNz\r\n"),
            "{head}"
        );
    }
    assert!(heads[1].starts_with("CONNECT localhost:1 HTTP/1.1\r\nHost: localhost:1\r\n"));
}

#[test]
fn https_goes_through_a_connect_tunnel_verified_against_the_target() {
    let proxy = Proxy::start(false);
    let body = r#"
        const https = require('https');
        const server = await new Promise(done => {
            const s = https.createServer({ cert: TLS_CERT, key: TLS_KEY }, (req, res) => res.end('secure ' + req.headers.host));
            s.on('tlsClientError', () => {});
            s.listen(0, '127.0.0.1', () => done(s));
        });
        const port = server.address().port;
        const r = await fetch(`https://localhost:${port}/path`);
        const text = await r.text();
        // The same server under a name its certificate does not carry: refused.
        const wrong = await fetch(`https://other.invalid:${port}/`).then(() => 'answered', e => [e.message, e.cause && e.cause.code]);
        globalThis.result = [r.status, text === 'secure localhost:' + port, wrong, port];
        server.close();
    "#;
    let permissions = granting(vec![rule("localhost", None), rule("other.invalid", None)]);
    let result = run(body, permissions, Some(through(&proxy.url(), None)));
    assert!(
        result.starts_with(r#"[200,true,["fetch failed","ERR_TLS_CERT_ALTNAME_INVALID"],"#),
        "{result}"
    );
    let port = result
        .trim_end_matches(']')
        .rsplit(',')
        .next()
        .unwrap()
        .to_owned();
    let heads = proxy.heads();
    assert!(
        heads[0].starts_with(&format!("CONNECT localhost:{port} HTTP/1.1\r\n")),
        "{heads:?}"
    );
    assert!(
        heads[1].starts_with(&format!("CONNECT other.invalid:{port} HTTP/1.1\r\n")),
        "{heads:?}"
    );
}

#[test]
fn a_refused_tunnel_fails_fetch_and_https_with_the_proxys_answer() {
    let proxy = Proxy::start(true);
    let body = format!(
        "{GET}
         const failed = await fetch('https://blocked.invalid/').catch(e => [e.message, e.cause.code, e.cause.message]);
         const https = await answer(require('https'), 'https://blocked.invalid/');
         globalThis.result = [failed, https];"
    );
    let permissions = granting(vec![rule("blocked.invalid", None)]);
    assert_eq!(
        run(&body, permissions, Some(through(&proxy.url(), None))),
        r#"[["fetch failed","UND_ERR_ABORTED","Proxy response (403) !== 200 when HTTP Tunneling"],["error","ERR_PROXY_TUNNEL",403]]"#
    );
    assert_eq!(proxy.connections(), 2);
}

#[test]
fn no_proxy_hosts_are_reached_directly() {
    let proxy = Proxy::start(false);
    let body = format!(
        "{GET}
         const http = require('http');
         const server = await new Promise(done => {{
             const s = http.createServer((req, res) => res.end('direct ' + req.url));
             s.listen(0, '127.0.0.1', () => done(s));
         }});
         const port = server.address().port;
         const viaFetch = await (await fetch(`http://127.0.0.1:${{port}}/a`)).text();
         const viaGet = await answer(http, `http://127.0.0.1:${{port}}/b`);
         server.close();
         globalThis.result = [viaFetch, viaGet];"
    );
    for no_proxy in ["127.0.0.1", "localhost,127.0.0.0/8", "*", "[::1],127.0.0.1"] {
        let permissions = granting(vec![rule("127.0.0.1", None)]);
        assert_eq!(
            run(
                &body,
                permissions,
                Some(through(&proxy.url(), Some(no_proxy)))
            ),
            r#"["direct /a",[200,"direct /b"]]"#,
            "{no_proxy}"
        );
    }
    assert_eq!(proxy.connections(), 0);
}

#[test]
fn an_ungranted_target_is_refused_before_the_proxy_is_reached() {
    let proxy = Proxy::start(false);
    let body = format!(
        "{GET}
         const failed = await fetch('https://elsewhere.invalid/').catch(e => [e.message, e.cause.code]);
         const get = await answer(require('http'), 'http://elsewhere.invalid/');
         const ws = await new Promise(done => {{
             const socket = new WebSocket('wss://elsewhere.invalid/');
             socket.onerror = () => done('error'); socket.onopen = () => done('open');
         }});
         globalThis.result = [failed, get, ws];"
    );
    let permissions = granting(vec![rule("localhost", None)]);
    let result = run(&body, permissions, Some(through(&proxy.url(), None)));
    assert_eq!(
        result,
        // `http` refuses an ungranted host as it always has, as the network Verve lacks.
        r#"[["fetch failed","ERR_ACCESS_DENIED"],["error","ENOSYS",null],"error"]"#
    );
    assert_eq!(proxy.connections(), 0);
}

#[test]
fn websockets_go_through_a_connect_tunnel() {
    let proxy = Proxy::start(false);
    let port = echo_websocket();
    let body = format!(
        "globalThis.result = await new Promise(done => {{
             const ws = new WebSocket('ws://localhost:{port}/live');
             ws.onopen = () => ws.send('ping');
             ws.onmessage = e => {{ done(['echo', e.data]); ws.close(); }};
             ws.onerror = () => done(['error']);
         }});"
    );
    let permissions = granting(vec![rule("localhost", Some(port))]);
    assert_eq!(
        run(&body, permissions, Some(through(&proxy.url(), None))),
        r#"["echo","ping"]"#
    );
    let heads = proxy.heads();
    assert_eq!(heads.len(), 1);
    assert!(
        heads[0].starts_with(&format!("CONNECT localhost:{port} HTTP/1.1\r\n")),
        "{heads:?}"
    );
}

#[test]
fn the_global_agent_is_proxied_and_an_agent_of_ones_own_is_not() {
    let proxy = Proxy::start(false);
    let other = Proxy::start(false);
    let body = format!(
        "{GET}
         const http = require('http');
         const server = await new Promise(done => {{
             const s = http.createServer((req, res) => res.end(req.url));
             s.listen(0, '127.0.0.1', () => done(s));
         }});
         const port = server.address().port;
         const url = (path) => `http://localhost:${{port}}${{path}}`;
         const global = await answer(http, url('/global'));
         const own = await answer(http, url('/own'), {{ agent: new http.Agent() }});
         const none = await answer(http, url('/none'), {{ agent: false }});
         // An agent's own `proxyEnv` naming the configured proxy goes through it; one naming a
         // proxy the realm was not given needs a grant for that proxy, which it lacks.
         const configured = await answer(http, url('/configured'), {{ agent: new http.Agent({{ proxyEnv: {{ HTTP_PROXY: '{configured}' }} }}) }});
         const ungranted = await answer(http, url('/ungranted'), {{ agent: new http.Agent({{ proxyEnv: {{ http_proxy: '{ungranted}' }} }}) }});
         server.close();
         globalThis.result = [global, own, none, configured, ungranted];",
        configured = proxy.url(),
        ungranted = other.url(),
    );
    let permissions = granting(vec![rule("localhost", None)]);
    assert_eq!(
        run(&body, permissions, Some(through(&proxy.url(), None))),
        r#"[[200,"/global"],[200,"/own"],[200,"/none"],[200,"/configured"],["error","EACCES",null]]"#
    );
    let heads = proxy.heads();
    assert_eq!(heads.len(), 2, "{heads:?}");
    assert!(heads[0].starts_with("GET http://localhost:"), "{heads:?}");
    assert!(heads[0].contains("/global HTTP/1.1\r\n"), "{heads:?}");
    assert!(heads[1].contains("/configured HTTP/1.1\r\n"), "{heads:?}");
    assert_eq!(other.connections(), 0);
}

#[test]
fn without_a_proxy_nothing_is_proxied() {
    let proxy = Proxy::start(false);
    let body = r#"
        const http = require('http');
        const server = await new Promise(done => {
            const s = http.createServer((req, res) => res.end('direct'));
            s.listen(0, '127.0.0.1', () => done(s));
        });
        const text = await (await fetch(`http://127.0.0.1:${server.address().port}/`)).text();
        server.close();
        globalThis.result = text;
    "#;
    // The variables are in the realm's environment only as data: nothing reads them.
    let permissions = granting(vec![rule("127.0.0.1", None)]);
    assert_eq!(run(body, permissions, None), r#""direct""#);
    assert_eq!(proxy.connections(), 0);
}

#[test]
fn an_unsupported_proxy_fails_rather_than_going_direct() {
    let body = format!(
        "{GET}
         const failed = await fetch('http://target.invalid/').catch(e => [e.message, e.cause.code]);
         let thrown;
         try {{ require('http').get('http://target.invalid/'); }} catch (e) {{ thrown = e.code; }}
         globalThis.result = [failed, thrown];"
    );
    let permissions = granting(vec![rule("target.invalid", None)]);
    assert_eq!(
        run(
            &body,
            permissions,
            Some(through("socks5://127.0.0.1:1080", None))
        ),
        r#"[["fetch failed","ERR_PROXY_INVALID_CONFIG"],"ERR_PROXY_INVALID_CONFIG"]"#
    );
}

// ─── verve run ──────────────────────────────────────────────────────────────────────────────

fn verve_run(args: &[&str], env: &[(&str, String)]) -> (i32, String, String) {
    let mut command = std::process::Command::new(VERVE);
    command.arg("run").args(args);
    for name in [
        "NODE_USE_ENV_PROXY",
        "HTTP_PROXY",
        "http_proxy",
        "HTTPS_PROXY",
        "https_proxy",
        "NO_PROXY",
        "no_proxy",
        "ALL_PROXY",
        "all_proxy",
    ] {
        command.env_remove(name);
    }
    for (name, value) in env {
        command.env(name, value);
    }
    let output = command.output().unwrap();
    (
        output.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&output.stdout).into_owned(),
        String::from_utf8_lossy(&output.stderr).into_owned(),
    )
}

#[test]
fn verve_run_takes_the_proxy_from_its_environment() {
    let proxy = Proxy::start(false);
    let target = origin(2);
    let script = format!(
        "fetch('http://target.invalid:{target}/run').then(r => r.text()).then(t => console.log(t))"
    );
    let grant = format!("--allow-net=target.invalid:{target}");
    // Rebon's container sandbox sets these: both cases of HTTP(S)_PROXY, a SOCKS ALL_PROXY
    // that Node (and Verve) leaves alone, and NODE_USE_ENV_PROXY.
    let sandbox = vec![
        ("NODE_USE_ENV_PROXY", "1".to_owned()),
        ("http_proxy", proxy.url()),
        ("HTTP_PROXY", proxy.url()),
        ("https_proxy", proxy.url()),
        ("HTTPS_PROXY", proxy.url()),
        ("all_proxy", "socks5://127.0.0.1:1".to_owned()),
        ("ALL_PROXY", "socks5://127.0.0.1:1".to_owned()),
    ];
    let (code, stdout, stderr) = verve_run(&[&grant, "-e", &script], &sandbox);
    assert_eq!((code, stdout.trim()), (0, "hello"), "{stderr}");
    // `--use-env-proxy` says the same without NODE_USE_ENV_PROXY.
    let flagged = vec![("HTTP_PROXY", proxy.url())];
    let (code, stdout, stderr) = verve_run(&["--use-env-proxy", &grant, "-e", &script], &flagged);
    assert_eq!((code, stdout.trim()), (0, "hello"), "{stderr}");
    let heads = proxy.heads();
    assert_eq!(heads.len(), 2, "{heads:?}");
    for head in &heads {
        assert!(head.starts_with(&format!(
            "GET http://target.invalid:{target}/run HTTP/1.1\r\n"
        )));
    }
    // The variables alone are not enough, as with Node: the name is looked up, and fails.
    let (code, _, _) = verve_run(&[&grant, "-e", &script], &flagged);
    assert_ne!(code, 0);
    assert_eq!(proxy.connections(), 2);
}

//! `session`: the cookies of a session (the API, a page's requests and `document.cookie`, the
//! main process's `net`, the file a persistent session keeps) and `webRequest` (what a listener
//! sees and may change, and what costs nothing when nobody listens). The expected values are
//! frozen from Electron 39.8.10 (`probe5` in the P16 scratch folder; the facts are listed in
//! P16's contract). Every server is a thread of this test on `127.0.0.1`.

#![allow(clippy::unwrap_used)]

mod support;

use std::io::{Read, Write};
use std::path::Path;
use std::sync::{Arc, Mutex};

use serde_json::{Value, json};
use support::{App, TIMEOUT};
use verve_electron::launcher::{Start, Target};
use verve_electron::msg::DialogResult;

/// What a server heard: the path of each request, and its headers (names lowercase).
type Seen = Arc<Mutex<Vec<(String, Vec<(String, String)>)>>>;

/// A server on a port of its own. `/page` is a page titled `P`, `/set` sets two cookies (one
/// `HttpOnly`), `/redir` sets one and redirects to `/echo`, and any other path answers the
/// headers it got, as JSON, with `x-resp: y`.
fn server() -> (u16, Seen) {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let seen: Seen = Arc::default();
    let heard = Arc::clone(&seen);
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { continue };
            let heard = Arc::clone(&heard);
            std::thread::spawn(move || answer(&mut stream, &heard));
        }
    });
    (port, seen)
}

fn answer(stream: &mut std::net::TcpStream, heard: &Seen) {
    let mut raw = Vec::new();
    let mut buffer = [0; 4096];
    while !raw.windows(4).any(|w| w == b"\r\n\r\n") {
        let n = stream.read(&mut buffer).unwrap_or(0);
        if n == 0 {
            return;
        }
        raw.extend_from_slice(&buffer[..n]);
    }
    let text = String::from_utf8_lossy(&raw).into_owned();
    let mut lines = text.split("\r\n");
    let path = lines
        .next()
        .and_then(|line| line.split(' ').nth(1))
        .unwrap_or("/")
        .to_owned();
    let headers: Vec<(String, String)> = lines
        .take_while(|line| !line.is_empty())
        .filter_map(|line| line.split_once(": "))
        .map(|(name, value)| (name.to_lowercase(), value.to_owned()))
        .collect();
    let body = json!(
        headers
            .iter()
            .cloned()
            .collect::<std::collections::BTreeMap<_, _>>()
    );
    heard.lock().unwrap().push((path.clone(), headers.clone()));
    if path.starts_with("/slow") {
        return slow(stream, &headers, !path.contains("norange"));
    }
    let (status, extra, text) = match path.split('?').next().unwrap_or("") {
        "/page" => (
            "200 OK",
            "content-type: text/html\r\n".to_owned(),
            "<title>P</title>hello".to_owned(),
        ),
        "/set" => (
            "200 OK",
            "set-cookie: s=1; Path=/\r\nset-cookie: h=2; HttpOnly; Path=/\r\n".to_owned(),
            "set".to_owned(),
        ),
        "/dl" => (
            "200 OK",
            "content-type: application/octet-stream\r\n".to_owned(),
            "a".repeat(300_000),
        ),
        "/att" => (
            "200 OK",
            "content-type: text/plain\r\ncontent-disposition: attachment; filename=\"att.txt\"\r\n"
                .to_owned(),
            "attached".to_owned(),
        ),
        "/redir" => (
            "302 Found",
            "location: /echo\r\nset-cookie: r=1; Path=/\r\n".to_owned(),
            String::new(),
        ),
        _ => (
            "200 OK",
            "content-type: application/json\r\nx-resp: y\r\n".to_owned(),
            body.to_string(),
        ),
    };
    let reply = format!(
        "HTTP/1.1 {status}\r\n{extra}content-length: {}\r\nconnection: close\r\n\r\n{text}",
        text.len()
    );
    let _ = stream.write_all(reply.as_bytes());
}

/// How long `/slow` is: it answers `SLOW_LENGTH` bytes, the byte at `i` being `a + i % 26`, in
/// pieces of 100 000 with a pause between them, and honours `Range: bytes=N-` unless the path
/// says `norange`, when it answers the whole body with 200.
const SLOW_LENGTH: usize = 600_000;

fn slow_byte(at: usize) -> u8 {
    b'a' + (at % 26) as u8
}

fn slow(stream: &mut std::net::TcpStream, headers: &[(String, String)], ranges: bool) {
    let from = headers
        .iter()
        .filter(|_| ranges)
        .find(|(name, _)| name == "range")
        .and_then(|(_, value)| value.strip_prefix("bytes="))
        .and_then(|value| value.trim_end_matches('-').parse::<usize>().ok());
    let start = from.unwrap_or(0);
    let head = match from {
        Some(start) => format!(
            "HTTP/1.1 206 Partial Content\r\ncontent-range: bytes {start}-{}/{SLOW_LENGTH}\r\n",
            SLOW_LENGTH - 1
        ),
        None => "HTTP/1.1 200 OK\r\n".to_owned(),
    };
    let head = format!(
        "{head}content-type: application/octet-stream\r\naccept-ranges: bytes\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
        SLOW_LENGTH - start
    );
    if stream.write_all(head.as_bytes()).is_err() {
        return;
    }
    for piece in (start..SLOW_LENGTH).collect::<Vec<_>>().chunks(100_000) {
        let bytes: Vec<u8> = piece.iter().map(|at| slow_byte(*at)).collect();
        if stream.write_all(&bytes).is_err() {
            return;
        }
        std::thread::sleep(std::time::Duration::from_millis(150));
    }
}

/// The headers of the last request for `path`, by name.
fn headers_of(seen: &Seen, path: &str) -> std::collections::BTreeMap<String, String> {
    let all = seen.lock().unwrap();
    let found = all.iter().rev().find(|(p, _)| p == path);
    found
        .map(|(_, h)| h.iter().cloned().collect())
        .unwrap_or_default()
}

fn paths(seen: &Seen) -> Vec<String> {
    seen.lock()
        .unwrap()
        .iter()
        .map(|(p, _)| p.clone())
        .collect()
}

/// A local port nothing listens on, so a request that goes there directly fails at once. The
/// proxy tests use it instead of a name under `.invalid`: some resolvers answer every name, and
/// the connection then hangs.
fn closed_port() -> u16 {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    listener.local_addr().unwrap().port()
}

/// Runs the app `main_js` with `profile` as its `userData`; the script fills `globalThis.out`
/// and sets `out.done`.
fn launch(main_js: &str, profile: &Path) -> App {
    let dir = App::scratch_dir("session");
    std::fs::write(dir.join("main.js"), main_js).unwrap();
    let mut start = Start::development(dir.clone());
    start.target = Target::App {
        app_path: dir.clone(),
        entry: dir.join("main.js"),
    };
    let switch = format!("--user-data-dir={}", profile.display());
    start.switches = vec![("user-data-dir".into(), profile.display().to_string())];
    start.argv = vec![switch];
    App::launch_with(start, Some(dir))
}

/// `globalThis.out` of the main process once it says it is done.
fn done(app: &App) -> Value {
    let finished = |app: &App| app.eval_main("!!globalThis.out && !!globalThis.out.done") == "true";
    assert!(
        app.run_until(finished, TIMEOUT),
        "console: {:?}",
        app.console()
    );
    let out: Value = serde_json::from_str(&app.eval_main("globalThis.out")).unwrap();
    assert!(
        out.get("threw").is_none(),
        "{out}; console: {:?}",
        app.console()
    );
    out
}

/// An app in the default profile of a test: `setup` runs when the app is ready, in a main
/// process that has the server's `base`, then a hidden window shows `/page`.
fn page_app(setup: &str, base: &str) -> App {
    let main = format!(
        "const {{ app, BrowserWindow, session, net }} = require('electron');
         globalThis.out = {{}}; globalThis.session = session; globalThis.require = require; globalThis.net = net;
         const base = '{base}'; globalThis.base = base;
         app.whenReady().then(async () => {{
           {setup}
           const win = new BrowserWindow({{ show: false }});
           win.loadURL(base + '/page');
           globalThis.win = win;
           out.done = true;
         }}).catch((e) => {{ out.threw = String(e && e.stack); }});"
    );
    launch(&main, &App::scratch_dir("profile"))
}

fn contents_of(app: &App) -> u32 {
    app.window("P")
        .unwrap_or_else(|| panic!("no window; console: {:?}", app.console()))
        .contents
}

/// What each bad call of `cookies` fails with, by the label the script gave it (Electron 39.8.10,
/// probe5).
fn what_the_api_refuses_with() -> Value {
    json!({
        "nourl": "Error: Missing required option 'url'",
        "badurl": "Error: Failed to set cookie - The cookie was set with an invalid Domain attribute.EXCLUDE_INVALID_DOMAIN, DO_NOT_WARN, NO_EXEMPTION",
        "badname": "Error: Failed to set cookie - The cookie contains ASCII control charactersEXCLUDE_DISALLOWED_CHARACTER, DO_NOT_WARN, NO_EXEMPTION",
        "badvalue": "Error: Failed to set cookie - The cookie contains ASCII control charactersEXCLUDE_DISALLOWED_CHARACTER, DO_NOT_WARN, NO_EXEMPTION",
        "wrongdomain": "Error: Failed to set cookie - The cookie was set with an invalid Domain attribute.EXCLUDE_INVALID_DOMAIN, DO_NOT_WARN, NO_EXEMPTION",
        "badsamesite": "Error: Failed to convert 'bogus' to an appropriate cookie same site value",
        "notobject": "TypeError: Error processing argument at index 0, conversion failure from x",
        "getnone": "TypeError: Insufficient number of arguments.",
        "removenone": "TypeError: Insufficient number of arguments.",
    })
}

#[test]
fn a_cookie_a_server_sets_comes_back_with_the_next_request_and_document_cookie_hides_httponly() {
    let (port, seen) = server();
    let base = format!("http://127.0.0.1:{port}");
    let setup = "globalThis.changes = [];
        session.defaultSession.cookies.on('changed', (e, c, cause, removed) => changes.push([c.name, cause, removed]));";
    let app = page_app(setup, &base);
    done(&app);
    let contents = contents_of(&app);
    let page = app.run_page(
        contents,
        "await fetch('/set'); const mine = document.cookie; await fetch('/echo'); return mine;",
        TIMEOUT,
    );
    // `document.cookie` sees the one a script may see; the request carried both, in the order
    // they were made (probe5: the `Cookie` header joins by path length, then age).
    assert_eq!(page["value"], "s=1");
    assert_eq!(headers_of(&seen, "/echo")["cookie"], "s=1; h=2");
    let listed = app.run_page(
        contents,
        "document.cookie = 'w=3; path=/'; await fetch('/echo2'); return document.cookie;",
        TIMEOUT,
    );
    assert_eq!(listed["value"], "s=1; w=3");
    assert_eq!(headers_of(&seen, "/echo2")["cookie"], "s=1; h=2; w=3");
    // The main process sees them all, `HttpOnly` too, in `get` and in `changed`.
    app.eval_main(
        "globalThis.got = null; session.defaultSession.cookies.get({}).then((c) => { globalThis.got = c.map((x) => [x.name, x.httpOnly, x.hostOnly, x.domain, x.session, x.sameSite]); }); 1",
    );
    assert!(app.run_until(|app| app.eval_main("globalThis.got") != "null", TIMEOUT));
    assert_eq!(
        serde_json::from_str::<Value>(&app.eval_main("globalThis.got")).unwrap(),
        json!([
            ["s", false, true, "127.0.0.1", true, "unspecified"],
            ["h", true, true, "127.0.0.1", true, "unspecified"],
            ["w", false, true, "127.0.0.1", true, "unspecified"],
        ])
    );
    let told = |app: &App| app.eval_main("changes.length") == "3";
    assert!(
        app.run_until(told, TIMEOUT),
        "{}",
        app.eval_main("globalThis.changes")
    );
    assert_eq!(
        serde_json::from_str::<Value>(&app.eval_main("globalThis.changes")).unwrap(),
        json!([
            ["s", "explicit", false],
            ["h", "explicit", false],
            ["w", "explicit", false]
        ])
    );
}

#[test]
fn the_main_process_uses_the_jar_when_it_asks_to_and_net_fetch_asks_by_default() {
    let (port, seen) = server();
    let base = format!("http://127.0.0.1:{port}");
    let setup = "const jar = session.defaultSession.cookies;
        await jar.set({ url: base + '/', name: 'k', value: 'v' });
        const request = (options) => new Promise((resolve) => {
          const r = net.request(options);
          r.on('response', (m) => { m.on('data', () => {}); m.on('end', resolve); });
          r.end();
        });
        await request(base + '/plain');
        await request({ url: base + '/use', useSessionCookies: true });
        await request({ url: base + '/omit', credentials: 'omit' });
        await request({ url: base + '/include', credentials: 'include' });
        await net.fetch(base + '/fetch');
        await net.fetch(base + '/fetch-omit', { credentials: 'omit' });
        await request({ url: base + '/redir', useSessionCookies: true });
        out.after = (await jar.get({ name: 'r' })).map((c) => c.value);";
    let app = page_app(setup, &base);
    let out = done(&app);
    let sent = |path: &str| headers_of(&seen, path).get("cookie").cloned();
    // Observed in Electron 39.8.10: a plain `net.request` sends no cookie, `useSessionCookies`
    // and `credentials: 'include'` do, `omit` does not, and `net.fetch` does unless it omits.
    assert_eq!(sent("/plain"), None);
    assert_eq!(sent("/use").as_deref(), Some("k=v"));
    assert_eq!(sent("/omit"), None);
    assert_eq!(sent("/include").as_deref(), Some("k=v"));
    assert_eq!(sent("/fetch").as_deref(), Some("k=v"));
    assert_eq!(sent("/fetch-omit"), None);
    // The cookie a redirect sets is kept and goes with the next hop.
    assert_eq!(sent("/echo").as_deref(), Some("k=v; r=1"));
    assert_eq!(out["after"], json!(["1"]));
}

#[test]
fn the_cookies_api_sets_gets_and_removes_and_says_what_changed() {
    let setup = "const jar = session.defaultSession.cookies;
        const changes = [];
        jar.on('changed', (e, c, cause, removed) => changes.push([c.name, c.value, cause, removed]));
        const url = 'http://example.test/a/b';
        await jar.set({ url, name: 'a', value: '1' });
        await jar.set({ url, name: 'a', value: '2' });
        await jar.set({ url, name: 'b', value: '1', domain: 'example.test', path: '/x', httpOnly: true, expirationDate: 4102444800, sameSite: 'strict' });
        await jar.set({ url, name: 'n', value: '1', sameSite: 'no_restriction' });
        await jar.set({ url, name: 'gone', value: '1', expirationDate: 1 });
        out.all = await jar.get({});
        out.byUrl = (await jar.get({ url: 'http://example.test/x/y' })).map((c) => c.name);
        out.byName = (await jar.get({ name: 'a' })).map((c) => c.value);
        out.byDomain = (await jar.get({ domain: 'example.test' })).map((c) => c.name);
        out.sessionOnly = (await jar.get({ session: true })).map((c) => c.name);
        await jar.remove(url, 'a');
        await jar.remove('nonsense', 'a');
        out.afterRemove = (await jar.get({})).map((c) => c.name);
        const failures = {};
        const tryit = async (label, f) => { try { await f(); failures[label] = 'ok'; } catch (e) { failures[label] = e.name + ': ' + e.message; } };
        await tryit('nourl', () => jar.set({ name: 'x', value: 'y' }));
        await tryit('badurl', () => jar.set({ url: 'nonsense', name: 'x', value: 'y' }));
        await tryit('badname', () => jar.set({ url, name: 'a b;', value: 'y' }));
        await tryit('badvalue', () => jar.set({ url, name: 'x', value: 'v;x' }));
        await tryit('wrongdomain', () => jar.set({ url, name: 'x', value: 'y', domain: 'other.test' }));
        await tryit('badsamesite', () => jar.set({ url, name: 'x', value: 'y', sameSite: 'bogus' }));
        await tryit('notobject', async () => jar.set('x'));
        await tryit('getnone', async () => jar.get());
        await tryit('removenone', async () => jar.remove());
        out.failures = failures;
        await jar.flushStore();
        out.changes = changes;";
    let (port, _) = server();
    let app = page_app(setup, &format!("http://127.0.0.1:{port}"));
    let out = done(&app);
    let names = |key: &str| -> Vec<String> { serde_json::from_value(out[key].clone()).unwrap() };
    // The paths are all two characters long, so the order is the order they were made in, and
    // the overwritten `a` was made again when it was overwritten (probe5).
    let cookie_names: Vec<&str> = out["all"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["name"].as_str().unwrap())
        .collect();
    assert_eq!(cookie_names, ["a", "b", "n"]);
    let b = &out["all"][1];
    assert_eq!(
        b,
        &json!({ "name": "b", "value": "1", "domain": ".example.test", "hostOnly": false,
                 "path": "/x", "secure": false, "httpOnly": true, "session": false,
                 "expirationDate": 4_102_444_800_u64, "sameSite": "strict" })
    );
    // `no_restriction` is stored secure (probe5), and the API's default same-site is `lax`.
    assert_eq!(out["all"][2]["secure"], true);
    assert_eq!(out["all"][2]["sameSite"], "no_restriction");
    assert_eq!(out["all"][0]["sameSite"], "lax");
    assert_eq!(out["all"][0]["domain"], "example.test");
    assert_eq!(names("byUrl"), ["b"]);
    assert_eq!(names("byName"), ["2"]);
    assert_eq!(names("byDomain"), ["a", "b", "n"]);
    assert_eq!(names("sessionOnly"), ["a", "n"]);
    assert_eq!(names("afterRemove"), ["b", "n"]);
    assert_eq!(out["failures"], what_the_api_refuses_with());
    // Setting is `explicit`; setting again says `overwrite` for the old one first; remove is
    // `explicit` and removed.
    let said: Vec<String> = out["changes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| format!("{}={} {} {}", c[0], c[1], c[2], c[3]))
        .collect();
    assert_eq!(
        said,
        [
            "\"a\"=\"1\" \"explicit\" false",
            "\"a\"=\"1\" \"overwrite\" true",
            "\"a\"=\"2\" \"explicit\" false",
            "\"b\"=\"1\" \"explicit\" false",
            "\"n\"=\"1\" \"explicit\" false",
            "\"a\"=\"2\" \"explicit\" true",
        ]
    );
}

#[test]
fn a_persistent_partitions_cookies_survive_a_new_hub_and_a_memory_partitions_do_not() {
    let profile = App::scratch_dir("profile");
    let first = "const keep = session.fromPartition('persist:a'), mem = session.fromPartition('mem');
        const until = Math.floor(Date.now() / 1000) + 3600;
        const url = 'http://example.test/';
        await keep.cookies.set({ url, name: 'keep', value: '1', expirationDate: until });
        await keep.cookies.set({ url, name: 'sess', value: '1' });
        await mem.cookies.set({ url, name: 'mem', value: '1', expirationDate: until });
        await session.defaultSession.cookies.set({ url, name: 'def', value: '1', expirationDate: until });";
    let script = |setup: &str| {
        format!(
            "const {{ app, session }} = require('electron'); globalThis.out = {{}};
             app.whenReady().then(async () => {{ {setup} out.done = true; }})
               .catch((e) => {{ out.threw = String(e && e.stack); }});"
        )
    };
    let one = launch(&script(first), &profile);
    done(&one);
    one.hub.shutdown(0);
    assert_eq!(one.exit_code(TIMEOUT), Some(0));
    // The file of the default session is the profile's own, a partition's is under `Partitions`,
    // and a memory partition has none.
    assert!(profile.join("Cookies.json").exists());
    assert!(profile.join("Partitions/a/Cookies.json").exists());
    assert!(!profile.join("Partitions/mem").exists());
    let second = "const names = async (s) => (await s.cookies.get({})).map((c) => c.name);
        out.keep = await names(session.fromPartition('persist:a'));
        out.mem = await names(session.fromPartition('mem'));
        out.def = await names(session.defaultSession);";
    let two = launch(&script(second), &profile);
    let out = done(&two);
    assert_eq!(out["keep"], json!(["keep"]));
    assert_eq!(out["mem"], json!([]));
    assert_eq!(out["def"], json!(["def"]));
}

#[test]
fn a_cancelling_listener_blocks_only_the_requests_it_matches() {
    let (port, seen) = server();
    let base = format!("http://127.0.0.1:{port}");
    let setup = "globalThis.heard = [];
        session.defaultSession.webRequest.onBeforeRequest({ urls: ['*://*/blocked*'] }, (d, cb) => {
          heard.push(d.url.split('/').pop()); cb({ cancel: true });
        });";
    let app = page_app(setup, &base);
    done(&app);
    let contents = contents_of(&app);
    let page = app.run_page(
        contents,
        "const status = (path) => fetch(path).then((r) => r.status, (e) => String(e));
         return [await status('/blocked1'), await status('/fine')];",
        TIMEOUT,
    );
    assert_eq!(page["value"], json!(["TypeError: Failed to fetch", 200]));
    assert_eq!(app.eval_main("globalThis.heard"), r#"["blocked1"]"#);
    let reached = paths(&seen);
    assert!(reached.contains(&"/fine".to_owned()));
    assert!(!reached.contains(&"/blocked1".to_owned()), "{reached:?}");
}

#[test]
fn a_listener_changes_the_headers_the_server_gets_and_the_ones_the_page_reads() {
    let (port, seen) = server();
    let base = format!("http://127.0.0.1:{port}");
    let setup = "const wr = session.defaultSession.webRequest;
        wr.onBeforeSendHeaders((d, cb) => cb({ requestHeaders: Object.assign({}, d.requestHeaders, { 'X-Added': 'yes' }) }));
        wr.onHeadersReceived((d, cb) => cb({ responseHeaders: Object.assign({}, d.responseHeaders, { 'x-changed': ['1'] }) }));
        wr.onBeforeRequest({ urls: ['*://*/zzz*'] }, (d, cb) => cb({ redirectURL: base + '/api?redirected' }));";
    let app = page_app(setup, &base);
    done(&app);
    let contents = contents_of(&app);
    let page = app.run_page(
        contents,
        "const a = await fetch('/api'); const r = await fetch('/zzz');
         return [a.headers.get('x-changed'), r.status, r.url, r.redirected];",
        TIMEOUT,
    );
    assert_eq!(headers_of(&seen, "/api")["x-added"], "yes");
    assert_eq!(
        page["value"],
        json!(["1", 200, format!("{base}/api?redirected"), true])
    );
    // The redirect went where the listener said, and `/zzz` itself was never asked.
    assert!(!paths(&seen).contains(&"/zzz".to_owned()));
}

#[test]
fn what_a_listener_is_told_has_the_fields_electron_gives_in_its_order() {
    let (port, _) = server();
    let base = format!("http://127.0.0.1:{port}");
    let setup = "globalThis.keys = {}; globalThis.facts = {};
        const wr = session.defaultSession.webRequest;
        for (const name of ['onBeforeRequest', 'onBeforeSendHeaders', 'onSendHeaders', 'onHeadersReceived',
                            'onResponseStarted', 'onBeforeRedirect', 'onCompleted', 'onErrorOccurred']) {
          wr[name]({ urls: ['*://*/seen*', '*://*/redir*', '*://127.0.0.1:1/*'] }, (d, cb) => {
            if (!keys[name]) { keys[name] = Object.keys(d); facts[name] = [typeof cb, d.resourceType, d.method, d.webContentsId === win.webContents.id, d.referrer, d.statusLine, d.statusCode, d.error]; }
            if (cb) cb({});
          });
        }";
    let app = page_app(setup, &base);
    done(&app);
    let contents = contents_of(&app);
    app.run_page(
        contents,
        "await fetch('/seen-a', { method: 'POST', body: 'abc' }); await fetch('/redir');
         await fetch('http://127.0.0.1:1/refused').catch(() => 0); return 1;",
        TIMEOUT,
    );
    let wait = |app: &App| app.eval_main("Object.keys(keys).length") == "8";
    assert!(
        app.run_until(wait, TIMEOUT),
        "{}",
        app.eval_main("JSON.stringify(keys)")
    );
    let keys: Value = serde_json::from_str(&app.eval_main("globalThis.keys")).unwrap();
    let list = |names: &[&str]| json!(names);
    let head = ["id", "url", "method", "timestamp", "resourceType"];
    let page_tail = ["frame", "webContents", "webContentsId", "referrer"];
    let with = |a: &[&str], b: &[&str], c: &[&str]| -> Value { list(&[a, b, c].concat()) };
    let response = [
        "ip",
        "fromCache",
        "statusLine",
        "statusCode",
        "responseHeaders",
    ];
    // Observed with a page's `fetch` in Electron 39.8.10 (probe5, `wr_log`).
    assert_eq!(
        keys["onBeforeRequest"],
        with(&head, &page_tail, &["uploadData", "redirectURL"])
    );
    assert_eq!(
        keys["onBeforeSendHeaders"],
        with(&head, &page_tail, &["uploadData", "requestHeaders"])
    );
    assert_eq!(
        keys["onSendHeaders"],
        with(&head, &page_tail, &["uploadData", "requestHeaders"])
    );
    let after_response = |tail: &[&str]| -> Value {
        list(&[&head[..], &response[..], &page_tail[..], tail].concat())
    };
    assert_eq!(keys["onHeadersReceived"], after_response(&["uploadData"]));
    assert_eq!(keys["onResponseStarted"], after_response(&["uploadData"]));
    assert_eq!(
        keys["onCompleted"],
        after_response(&["uploadData", "error"])
    );
    // The redirect is of a `GET`, which has no body, and tells where it goes.
    assert_eq!(keys["onBeforeRedirect"], after_response(&["redirectURL"]));
    assert_eq!(keys["onErrorOccurred"], with(&head, &page_tail, &["error"]));
    let facts: Value = serde_json::from_str(&app.eval_main("globalThis.facts")).unwrap();
    assert_eq!(
        facts["onBeforeRequest"],
        json!([
            "function",
            "xhr",
            "POST",
            true,
            format!("{base}/page"),
            null,
            null,
            null
        ])
    );
    assert_eq!(facts["onSendHeaders"][0], "undefined");
    assert_eq!(facts["onCompleted"][5], "HTTP/1.1 200 OK");
    assert_eq!(facts["onCompleted"][7], "net::OK");
}

#[test]
fn nobody_listening_costs_no_message_and_a_filter_that_does_not_match_costs_none_either() {
    let (port, _) = server();
    let base = format!("http://127.0.0.1:{port}");
    let app = page_app("globalThis.wr = session.defaultSession.webRequest;", &base);
    done(&app);
    let contents = contents_of(&app);
    let session = app.hub.session_for(None);
    let get = "await fetch('/a'); await fetch('/b'); return 1;";
    app.run_page(contents, get, TIMEOUT);
    assert_eq!(session.web_request_messages(), 0);
    // A listener for other addresses is nobody for these.
    app.eval_main("wr.onBeforeRequest({ urls: ['*://*/other*'] }, (d, cb) => cb({})); 1");
    app.run_page(contents, get, TIMEOUT);
    assert_eq!(session.web_request_messages(), 0);
    // One for these costs a message each.
    app.eval_main("wr.onBeforeRequest({ urls: ['*://*/a*'] }, (d, cb) => cb({})); 1");
    app.run_page(contents, get, TIMEOUT);
    assert_eq!(session.web_request_messages(), 1);
    // And taking it away costs nothing again.
    app.eval_main("wr.onBeforeRequest(null); 1");
    app.run_page(contents, get, TIMEOUT);
    assert_eq!(session.web_request_messages(), 1);
}

#[test]
fn the_main_processs_own_requests_pass_the_listeners_without_a_page() {
    let (port, seen) = server();
    let base = format!("http://127.0.0.1:{port}");
    let setup = "globalThis.heard = [];
        const wr = session.defaultSession.webRequest;
        wr.onBeforeSendHeaders((d, cb) => { heard.push(['send', 'webContentsId' in d, d.resourceType]); cb({ requestHeaders: { 'X-Main': '1' } }); });
        wr.onCompleted((d) => { heard.push(['completed', d.statusCode, d.error]); });
        await (await net.fetch(base + '/viamain')).text();";
    let app = page_app(setup, &base);
    done(&app);
    // `onCompleted` is told when the body has been read, which the `text()` above waited for.
    let heard = |app: &App| app.eval_main("globalThis.heard").contains("completed");
    assert!(app.run_until(heard, TIMEOUT));
    assert_eq!(
        serde_json::from_str::<Value>(&app.eval_main("globalThis.heard")).unwrap(),
        json!([["send", false, "other"], ["completed", 200, "net::OK"]])
    );
    assert_eq!(headers_of(&seen, "/viamain")["x-main"], "1");
}

/// What the page sees of notifications: its permission, what asking for it answers, and what
/// `permissions.query` says.
const NOTIFICATIONS: &str =
    "const q = await navigator.permissions.query({ name: 'notifications' });
    return [Notification.permission, await Notification.requestPermission(), q.state];";

#[test]
fn denying_notifications_makes_the_pages_request_answer_denied() {
    let (port, _) = server();
    let base = format!("http://127.0.0.1:{port}");
    let app = page_app("globalThis.asked = []; globalThis.checked = [];", &base);
    done(&app);
    let contents = contents_of(&app);
    let ask = || app.run_page(contents, NOTIFICATIONS, TIMEOUT)["value"].clone();
    // No handler allows everything, as Electron does (`probe7`).
    assert_eq!(ask(), json!(["granted", "granted", "granted"]));
    let session = app.hub.session_for(None);
    assert!(!session.has_permission_handler(false));
    // The request handler is asked by `requestPermission` alone, and its callback decides.
    app.eval_main(
        "session.defaultSession.setPermissionRequestHandler((wc, permission, callback, details) => {
           asked.push([permission, wc.id === win.webContents.id, details]); callback(false); }); 1",
    );
    assert!(session.has_permission_handler(false));
    assert_eq!(ask(), json!(["granted", "denied", "granted"]));
    let details = json!({ "isMainFrame": true, "requestingUrl": format!("{base}/page") });
    assert_eq!(
        serde_json::from_str::<Value>(&app.eval_main("globalThis.asked")).unwrap(),
        json!([["notifications", true, details]])
    );
    // The check handler is asked by `permissions.query`, which answers what it returns, and what
    // it answered is what `Notification.permission` reads afterwards (Verve does not ask the
    // handler for that property itself); a request has no handler of its own to ask.
    app.eval_main(
        "session.defaultSession.setPermissionRequestHandler(null);
         session.defaultSession.setPermissionCheckHandler((wc, permission, origin, details) => {
           checked.push([permission, origin, Object.keys(details)]); return false; }); 1",
    );
    assert!(!session.has_permission_handler(false) && session.has_permission_handler(true));
    let answers = ask();
    assert_eq!(answers[1], json!("granted"));
    assert_eq!(answers[2], json!("denied"));
    let checked: Value = serde_json::from_str(&app.eval_main("globalThis.checked")).unwrap();
    let keys = json!(["embeddingOrigin", "isMainFrame", "requestingUrl"]);
    assert_eq!(
        checked[0],
        json!(["notifications", format!("{base}/"), keys])
    );
    // A check handler that throws denies, and the page's query is answered all the same.
    app.eval_main(
        "session.defaultSession.setPermissionCheckHandler(() => { throw new Error('boom'); }); 1",
    );
    assert_eq!(ask()[2], json!("denied"));
    // Taking the handlers away allows everything again.
    app.eval_main("session.defaultSession.setPermissionCheckHandler(null); 1");
    assert_eq!(ask(), json!(["granted", "granted", "granted"]));
    let refused = app.eval_main(
        "try { session.defaultSession.setPermissionRequestHandler(5); 'ok' } catch (e) { e.message }",
    );
    assert!(refused.contains("Must pass null or function"), "{refused}");
}

#[test]
fn a_proxy_the_test_started_sees_the_requests_of_the_page_and_of_the_main_process() {
    let (port, direct) = server();
    let (proxy_port, proxied) = server();
    let base = format!("http://127.0.0.1:{port}");
    let setup = format!(
        "await session.defaultSession.setProxy({{ proxyRules: 'http://127.0.0.1:{proxy_port}' }});
         globalThis.resolved = [await session.defaultSession.resolveProxy('http://proxied.test/x'),
           await session.defaultSession.resolveProxy(base + '/x'), await session.defaultSession.resolveProxy()];"
    );
    let app = page_app(&setup, &base);
    done(&app);
    let contents = contents_of(&app);
    // The proxy's own address is reached by the page's load of `/page`: the loopback is direct.
    assert_eq!(paths(&direct), ["/page"]);
    let page = app.run_page(
        contents,
        "const a = await fetch('http://proxied.test/from-page'); const b = await fetch('/direct');
         return [a.status, b.status];",
        TIMEOUT,
    );
    assert_eq!(page["value"], json!([200, 200]));
    app.eval_main("net.fetch('http://proxied.test/from-main').then((r) => r.text()).then((t) => { globalThis.main_body = t; }); 1");
    assert!(app.run_until(
        |app| app.eval_main("typeof main_body") == r#""string""#,
        TIMEOUT
    ));
    assert_eq!(
        paths(&proxied),
        [
            "http://proxied.test/from-page",
            "http://proxied.test/from-main"
        ]
    );
    assert_eq!(paths(&direct), ["/page", "/direct"]);
    let resolved = app.eval_main("JSON.stringify(resolved)");
    let want = format!(r#""[\"PROXY 127.0.0.1:{proxy_port}\",\"DIRECT\",\"DIRECT\"]""#);
    assert_eq!(resolved, want);
    // Back to direct, and a mode Electron does not have is refused with its words.
    let after = app.eval_main(
        "session.defaultSession.setProxy({ mode: 'direct' })
           .then(() => session.defaultSession.resolveProxy('http://proxied.test/x'))
           .then((r) => { globalThis.direct = r; });
         session.defaultSession.setProxy({ mode: 'bogus' }).catch((e) => { globalThis.refused = e.message; }); 1",
    );
    assert_eq!(after, "1");
    assert!(app.run_until(
        |app| app.eval_main("typeof refused") == r#""string""#,
        TIMEOUT
    ));
    assert_eq!(app.eval_main("direct"), r#""DIRECT""#);
    assert_eq!(
        app.eval_main("refused"),
        r#""Invalid mode, must be one of direct, auto_detect, pac_script, fixed_servers or system""#
    );
}

/// Runs `code` in the main process, waits until it has set `globalThis.<name>` and answers that
/// value as JSON text.
fn main_result(app: &App, code: &str, name: &str) -> String {
    app.eval_main(&format!("{code}; 1"));
    assert!(
        app.run_until(
            |app| app.eval_main(&format!("typeof globalThis.{name}")) != r#""undefined""#,
            TIMEOUT
        ),
        "console: {:?}",
        app.console()
    );
    // `eval_main` answers its result as JSON, and the result here is already JSON text.
    let text = app.eval_main(&format!("JSON.stringify(globalThis.{name})"));
    serde_json::from_str(&text).unwrap()
}

#[test]
fn a_sessions_proxy_is_the_route_of_its_requests_only_and_node_and_global_fetch_stay_direct() {
    let (port, _) = server();
    let (proxy_port, proxied) = server();
    let base = format!("http://127.0.0.1:{port}");
    let setup = format!(
        "await session.defaultSession.setProxy({{ proxyRules: 'http://127.0.0.1:{proxy_port}' }});"
    );
    let app = page_app(&setup, &base);
    done(&app);
    let through = main_result(
        &app,
        "net.fetch('http://proxied.test/net').then((r) => { globalThis.through = r.status; })",
        "through",
    );
    assert_eq!(through, "200");
    // Node's `http` and the global `fetch` of the main process are not Electron's network
    // stack: after the session's proxy was used they still connect directly, and the proxy hears
    // nothing of them. They aim at a closed local port, so going direct fails at once, where the
    // proxy would have answered. The kernel's own proxy settings don't skip the loopback, so a
    // proxy wrongly set on the main process would still be heard.
    let closed = closed_port();
    let node = main_result(
        &app,
        &format!(
            "globalThis.node = []; const note = (what) => () => {{ node.push(what); if (node.length === 2) globalThis.node_done = true; }};
             require('http').get('http://127.0.0.1:{closed}/node-http', () => note('reached')()).on('error', note('failed'));
             fetch('http://127.0.0.1:{closed}/node-fetch').then(note('reached'), note('failed'))"
        ),
        "node_done",
    );
    assert_eq!(node, "true");
    assert_eq!(
        app.eval_main("JSON.stringify(node)"),
        r#""[\"failed\",\"failed\"]""#
    );
    assert_eq!(paths(&proxied), ["http://proxied.test/net"]);
}

#[test]
fn two_sessions_with_two_proxies_and_slow_listeners_each_reach_only_their_own() {
    let (port, _) = server();
    let (proxy_a, seen_a) = server();
    let (proxy_b, seen_b) = server();
    let base = format!("http://127.0.0.1:{port}");
    let setup = format!(
        "const a = session.fromPartition('a'); const b = session.fromPartition('b');
         await a.setProxy({{ proxyRules: 'http://127.0.0.1:{proxy_a}' }});
         await b.setProxy({{ proxyRules: 'http://127.0.0.1:{proxy_b}' }});
         for (const [s, delay] of [[a, 150], [b, 20]]) {{
           s.webRequest.onBeforeRequest((details, callback) => setTimeout(() => callback({{}}), delay));
         }}
         globalThis.a = a; globalThis.b = b;"
    );
    let app = page_app(&setup, &base);
    done(&app);
    // `a`'s listener answers after `b`'s, so each decision is still pending while the other
    // session's requests are decided and sent.
    let statuses = main_result(
        &app,
        "Promise.all([a.fetch('http://one.test/1'), b.fetch('http://two.test/1'), a.fetch('http://one.test/2'),
           b.fetch('http://two.test/2')]).then((r) => { globalThis.statuses = r.map((x) => x.status); })",
        "statuses",
    );
    assert_eq!(statuses, "[200,200,200,200]");
    let mut a = paths(&seen_a);
    a.sort();
    let mut b = paths(&seen_b);
    b.sort();
    assert_eq!(a, ["http://one.test/1", "http://one.test/2"]);
    assert_eq!(b, ["http://two.test/1", "http://two.test/2"]);
}

#[test]
fn rules_for_http_only_send_https_requests_nowhere_near_the_proxy() {
    let (port, direct) = server();
    let (proxy_port, proxied) = server();
    let base = format!("http://127.0.0.1:{port}");
    let setup = format!(
        "await session.defaultSession.setProxy({{ proxyRules: 'http=127.0.0.1:{proxy_port}' }});
         globalThis.resolved = await session.defaultSession.resolveProxy('https://secure.invalid/');"
    );
    let app = page_app(&setup, &base);
    done(&app);
    assert_eq!(app.eval_main("resolved"), r#""DIRECT""#);
    // The window's load of `/page` is direct, since the loopback is never proxied unless
    // `<-loopback>` says so; `out.done` doesn't wait for it, so the test does.
    assert!(app.run_until(|_| paths(&direct) == ["/page"], TIMEOUT));
    // With `<-loopback>`, an `http:` request to a closed local port would go to the proxy, while
    // an `https:` one goes direct: it fails at once, and no `CONNECT` reaches the proxy.
    let closed = closed_port();
    let outcome = main_result(
        &app,
        &format!(
            "const s = session.defaultSession;
             s.setProxy({{ proxyRules: 'http=127.0.0.1:{proxy_port}', proxyBypassRules: '<-loopback>' }})
               .then(() => Promise.all(['http', 'https'].map((scheme) => s.resolveProxy(`${{scheme}}://127.0.0.1:{closed}/`))))
               .then((routes) => {{ globalThis.routes = routes; return net.fetch('https://127.0.0.1:{closed}/x'); }})
               .then(() => {{ globalThis.outcome = 'reached'; }}, () => {{ globalThis.outcome = 'failed'; }})"
        ),
        "outcome",
    );
    assert_eq!(
        app.eval_main("JSON.stringify(routes)"),
        format!(r#""[\"PROXY 127.0.0.1:{proxy_port}\",\"DIRECT\"]""#)
    );
    assert_eq!(outcome, r#""failed""#);
    assert_eq!(paths(&proxied), Vec::<String>::new());
}

#[test]
fn the_session_methods_that_do_nothing_exist_resolve_and_store_what_they_are_given() {
    let (port, _) = server();
    let base = format!("http://127.0.0.1:{port}");
    let app = page_app("", &base);
    done(&app);
    let result = main_result(
        &app,
        "(async () => { const s = session.defaultSession; const out = {};
           for (const name of ['clearCache', 'clearStorageData', 'clearAuthCache', 'clearHostResolverCache',
             'closeAllConnections', 'forceReloadProxyConfig']) out[name] = await s[name]() === undefined;
           out.size = await s.getCacheSize();
           s.setSpellCheckerEnabled(false); s.setSpellCheckerLanguages(['en-US']);
           out.spell = [s.isSpellCheckerEnabled(), s.getSpellCheckerLanguages(), s.addWordToSpellCheckerDictionary('x')];
           out.flush = s.flushStorageData() === undefined;
           s.setDownloadPath('/tmp'); s.setSSLConfig({}); s.setCertificateVerifyProc(null);
           out.net = [typeof s.netLog.startLogging, s.netLog.currentlyLogging, await s.netLog.stopLogging(), s.netLog === s.netLog];
           globalThis.stubs = out; })()",
        "stubs",
    );
    let got: Value = serde_json::from_str(&result).unwrap();
    assert_eq!(got["size"], 0);
    assert_eq!(got["clearCache"], true);
    assert_eq!(got["closeAllConnections"], true);
    assert_eq!(got["flush"], true);
    assert_eq!(got["spell"], json!([false, ["en-US"], true]));
    assert_eq!(got["net"], json!(["function", false, "", true]));
}

/// What the downloads of the tests do, in the main process: every `will-download` is logged
/// with what the item says at that moment, saved under `dir` (except `/nosave`, which a listener
/// refuses), and every event of the item is logged too. `/slow-cancel` is cancelled and
/// `/slow-pause` paused for 400 ms, at the first `updated`.
fn downloads_setup(dir: &Path) -> String {
    let dir = serde_json::to_string(&dir.to_string_lossy()).unwrap();
    format!(
        "const path = require('path'); const fs = require('fs'); const dir = {dir};
         globalThis.log = [];
         session.defaultSession.on('will-download', (event, item, wc) => {{
           const e = {{ url: item.getURL(), name: item.getFilename(), mime: item.getMimeType(), total: item.getTotalBytes(),
             state: item.getState(), savePath: item.getSavePath(), wc: wc && wc.id, events: [] }};
           log.push(e);
           if (e.url.includes('/nosave')) {{ event.preventDefault(); return; }}
           const target = path.join(dir, 'f' + log.length + '-' + item.getFilename());
           item.setSavePath(target);
           e.after = item.getSavePath() === target;
           let first = true;
           item.on('updated', (ev, state) => {{
             e.events.push(['updated', state, item.isPaused(), fs.existsSync(target + '.crdownload')]);
             if (e.url.includes('slow-pause') && item.getReceivedBytes() === 0) return;
             if (!first) return; first = false;
             if (e.url.includes('slow-cancel')) item.cancel();
             if (e.url.includes('slow-pause')) {{ item.pause(); e.paused = [item.isPaused(), item.canResume(), item.getState()]; e.pausedAt = item.getReceivedBytes();
               setTimeout(() => item.resume(), 400); }}
           }});
           item.on('done', (ev, state) => {{
             e.events.push(['done', state, fs.existsSync(target), fs.existsSync(target + '.crdownload')]);
             e.size = fs.existsSync(target) ? fs.statSync(target).size : null; e.received = item.getReceivedBytes(); e.target = target; e.done = true;
           }});
         }});"
    )
}

/// The entry of `log` for the download of `url_end`, once it is done (or at once, for one that
/// is refused: `refused`).
fn download_of(app: &App, url_end: &str, refused: bool) -> Value {
    let find = format!("globalThis.log.find((e) => e.url.endsWith('{url_end}'))");
    let read = |app: &App| -> Value {
        let text = app.eval_main(&format!("{find} || null"));
        serde_json::from_str(&text).unwrap_or(Value::Null)
    };
    let ready = |app: &App| {
        let found = read(app);
        found.get("done").is_some() || (refused && !found.is_null())
    };
    assert!(
        app.run_until(ready, TIMEOUT),
        "console: {:?}",
        app.console()
    );
    read(app)
}

#[test]
fn a_download_with_a_save_path_completes_and_cancel_leaves_nothing() {
    let (port, _) = server();
    let base = format!("http://127.0.0.1:{port}");
    let dir = App::scratch_dir("downloads");
    let app = page_app(&downloads_setup(&dir), &base);
    done(&app);
    let contents = contents_of(&app);
    app.eval_main("session.defaultSession.downloadURL(base + '/dl'); 1");
    let item = download_of(&app, "/dl", false);
    // `will-download` is heard with the head's facts, and a session's own call names no page
    // (Electron 39.8.10, `probe7`).
    assert_eq!(item["name"], "dl");
    assert_eq!(item["mime"], "application/octet-stream");
    assert_eq!(item["total"], 300_000);
    assert_eq!(item["state"], "progressing");
    assert_eq!(item["savePath"], "");
    assert_eq!(item["after"], true);
    assert_eq!(item["wc"], Value::Null);
    let events = item["events"].as_array().unwrap();
    // Verve opens its partial file (`<target>.crdownload`) before the first `updated`; Chromium's
    // temporary name differs and is not observable by an app, so only the final state is pinned.
    assert_eq!(
        events.first().unwrap(),
        &json!(["updated", "progressing", false, true])
    );
    assert_eq!(
        events.last().unwrap(),
        &json!(["done", "completed", true, false])
    );
    assert_eq!(item["size"], 300_000);
    assert_eq!(item["received"], 300_000);
    // A `webContents` names itself, and the attachment's own file name is the item's.
    app.eval_main("win.webContents.downloadURL(base + '/att'); 1");
    let attached = download_of(&app, "/att", false);
    assert_eq!(attached["name"], "att.txt");
    assert_eq!(attached["mime"], "text/plain");
    assert_eq!(attached["wc"], contents);
    let target = attached["target"].as_str().unwrap();
    assert_eq!(std::fs::read_to_string(target).unwrap(), "attached");
    // A listener that prevents the default has no download: nothing is heard of it.
    app.eval_main("session.defaultSession.downloadURL(base + '/nosave'); 1");
    let refused = download_of(&app, "/nosave", true);
    assert_eq!(refused["events"], json!([]));
    // `cancel()` ends it as `cancelled`, with neither the file nor the partial one left.
    app.eval_main("session.defaultSession.downloadURL(base + '/slow-cancel'); 1");
    let cancelled = download_of(&app, "/slow-cancel", false);
    let last = cancelled["events"]
        .as_array()
        .unwrap()
        .last()
        .unwrap()
        .clone();
    assert_eq!(last, json!(["done", "cancelled", false, false]));
}

#[test]
fn a_paused_download_goes_on_with_a_range_request_and_ends_whole() {
    let (port, seen) = server();
    let base = format!("http://127.0.0.1:{port}");
    let dir = App::scratch_dir("downloads");
    let app = page_app(&downloads_setup(&dir), &base);
    done(&app);
    app.eval_main("session.defaultSession.downloadURL(base + '/slow-pause'); 1");
    let item = download_of(&app, "/slow-pause", false);
    // Paused, it can be resumed and is still `progressing` (Electron 39.8.10, `probe7`).
    assert_eq!(item["paused"], json!([true, true, "progressing"]));
    assert_eq!(
        item["events"].as_array().unwrap().last().unwrap(),
        &json!(["done", "completed", true, false])
    );
    let bytes = std::fs::read(item["target"].as_str().unwrap()).unwrap();
    let expected: Vec<u8> = (0..SLOW_LENGTH).map(slow_byte).collect();
    assert!(bytes == expected, "the file is {} bytes", bytes.len());
    // The resumed request asks for the rest: `bytes=N-`, from where the pause left off.
    let ranges = range_values(&seen, "/slow-pause");
    assert_eq!(
        ranges.len(),
        1,
        "the second request asked for the rest: {ranges:?}"
    );
    let from = ranges[0].trim_start_matches("bytes=").trim_end_matches('-');
    let from: u64 = from.parse().unwrap();
    assert!(
        from > 0 && from >= item["pausedAt"].as_u64().unwrap(),
        "{from}"
    );
}

#[test]
fn a_server_that_ignores_range_makes_the_resumed_download_start_over_and_end_whole() {
    let (port, seen) = server();
    let base = format!("http://127.0.0.1:{port}");
    let dir = App::scratch_dir("downloads");
    let app = page_app(&downloads_setup(&dir), &base);
    done(&app);
    app.eval_main("session.defaultSession.downloadURL(base + '/slow-pause-norange'); 1");
    let item = download_of(&app, "/slow-pause-norange", false);
    assert_eq!(
        item["events"].as_array().unwrap().last().unwrap(),
        &json!(["done", "completed", true, false])
    );
    // The 200 to the range request restarts the file at its start: no byte twice, none missing.
    let bytes = std::fs::read(item["target"].as_str().unwrap()).unwrap();
    let expected: Vec<u8> = (0..SLOW_LENGTH).map(slow_byte).collect();
    assert!(bytes == expected, "the file is {} bytes", bytes.len());
    assert_eq!(range_values(&seen, "/slow-pause-norange").len(), 1);
}

/// The `Range` headers the requests for `path` carried.
fn range_values(seen: &Seen, path: &str) -> Vec<String> {
    let all = seen.lock().unwrap();
    all.iter()
        .filter(|(p, _)| p == path)
        .flat_map(|(_, headers)| headers.iter().filter(|(name, _)| name == "range"))
        .map(|(_, value)| value.clone())
        .collect()
}

/// A `will-download` listener that does not name a path, so the save dialog is asked.
const ASKING_SETUP: &str = "globalThis.log = [];
    session.defaultSession.on('will-download', (event, item) => {
      const e = { url: item.getURL() }; log.push(e);
      item.on('done', (ev, state) => { e.state = state; e.target = item.getSavePath(); e.done = true; });
    });";

#[test]
fn without_a_save_path_the_save_dialog_chooses_and_cancelling_it_cancels_the_download() {
    let (port, _) = server();
    let base = format!("http://127.0.0.1:{port}");
    let dir = App::scratch_dir("downloads");
    let app = page_app(ASKING_SETUP, &base);
    done(&app);
    // The person picks a file in the dialog (a preset answer: no real dialog opens).
    let chosen = dir.join("chosen.bin");
    app.shell.queue_dialog_answer(DialogResult::Save {
        canceled: false,
        file_path: Some(chosen.clone()),
    });
    app.eval_main("session.defaultSession.downloadURL(base + '/dl'); 1");
    let item = download_of(&app, "/dl", false);
    assert_eq!(item["state"], "completed");
    assert_eq!(std::fs::metadata(&chosen).unwrap().len(), 300_000);
    // Without an answer the headless shell cancels the dialog, and the download ends cancelled.
    app.eval_main("session.defaultSession.downloadURL(base + '/att'); 1");
    let item = download_of(&app, "/att", false);
    assert_eq!(item["state"], "cancelled");
    assert_eq!(app.shell.dialogs().len(), 2);
}

#[test]
fn a_download_link_and_an_attachment_page_are_downloads() {
    let (port, _) = server();
    let base = format!("http://127.0.0.1:{port}");
    let dir = App::scratch_dir("downloads");
    let app = page_app(&downloads_setup(&dir), &base);
    done(&app);
    let contents = contents_of(&app);
    app.run_page(
        contents,
        "document.body.insertAdjacentHTML('beforeend', '<a id=l href=\"/dl\" download=\"x.bin\">d</a>');
         document.getElementById('l').click(); return 1;",
        TIMEOUT,
    );
    let link = download_of(&app, "/dl", false);
    assert_eq!(link["name"], "x.bin");
    assert_eq!(link["wc"], contents);
    assert_eq!(link["size"], 300_000);
    // A page that is an attachment fails to load (`ERR_FAILED`, -2) and is saved instead.
    app.eval_main(
        "win.loadURL(base + '/att').catch((e) => { globalThis.load_error = String(e.message); }); 1",
    );
    let attached = download_of(&app, "/att", false);
    assert_eq!(attached["name"], "att.txt");
    assert!(app.run_until(
        |app| app.eval_main("typeof load_error") == r#""string""#,
        TIMEOUT
    ));
    assert!(app.eval_main("load_error").contains("ERR_FAILED (-2)"));
}

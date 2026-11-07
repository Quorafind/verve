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
    heard.lock().unwrap().push((path.clone(), headers));
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
         globalThis.out = {{}}; globalThis.session = session; const base = '{base}';
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

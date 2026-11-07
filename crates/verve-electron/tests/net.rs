//! `net`: `net.request` (its events, redirects followed, handed over or refused, an upload
//! written in pieces), `net.fetch` of files and of a local server, `isOnline` and `resolveHost`.
//! Every server is a thread of this test on `127.0.0.1`. The expected values are frozen from
//! Electron 39.8.10 (probe2 in the P16 scratch folder, and the source of
//! `lib/common/api/net-client-request.ts` and `lib/browser/api/net-fetch.ts`).

#![allow(clippy::unwrap_used)]

mod support;

use std::io::{Read, Write};

use serde_json::{Value, json};
use support::{App, TIMEOUT};

/// A server answering `/redir` with a redirect to `/target`, `/target` with `target`, and any
/// other path with the request it got, as JSON. Answers its port.
fn server() -> u16 {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { continue };
            std::thread::spawn(move || answer(&mut stream));
        }
    });
    port
}

/// The body of a `Transfer-Encoding: chunked` message: the chunks' data, in order.
fn dechunked(mut raw: &[u8]) -> String {
    let mut body = Vec::new();
    while let Some(at) = raw.windows(2).position(|w| w == b"\r\n") {
        let size =
            usize::from_str_radix(String::from_utf8_lossy(&raw[..at]).trim(), 16).unwrap_or(0);
        if size == 0 || raw.len() < at + 2 + size {
            break;
        }
        body.extend_from_slice(&raw[at + 2..at + 2 + size]);
        raw = &raw[(at + 2 + size + 2).min(raw.len())..];
    }
    String::from_utf8_lossy(&body).into_owned()
}

fn answer(stream: &mut std::net::TcpStream) {
    let mut seen = Vec::new();
    let mut buffer = [0; 4096];
    let (head_end, length) = loop {
        let n = stream.read(&mut buffer).unwrap_or(0);
        if n == 0 {
            return;
        }
        seen.extend_from_slice(&buffer[..n]);
        if let Some(at) = seen.windows(4).position(|w| w == b"\r\n\r\n") {
            let head = String::from_utf8_lossy(&seen[..at]).to_lowercase();
            let length = head
                .lines()
                .find_map(|line| line.strip_prefix("content-length: "))
                .and_then(|n| n.trim().parse::<usize>().ok())
                .unwrap_or(0);
            break (at + 4, length);
        }
    };
    while seen.len() < head_end + length {
        let n = stream.read(&mut buffer).unwrap_or(0);
        if n == 0 {
            break;
        }
        seen.extend_from_slice(&buffer[..n]);
    }
    let head = String::from_utf8_lossy(&seen[..head_end]).into_owned();
    let chunked = head.to_lowercase().contains("transfer-encoding: chunked");
    while chunked && !seen.ends_with(b"0\r\n\r\n") {
        let n = stream.read(&mut buffer).unwrap_or(0);
        if n == 0 {
            break;
        }
        seen.extend_from_slice(&buffer[..n]);
    }
    let body = if chunked {
        dechunked(&seen[head_end..])
    } else {
        String::from_utf8_lossy(&seen[head_end..]).into_owned()
    };
    let (method, path) = {
        let mut words = head.split(' ');
        (words.next().unwrap_or(""), words.next().unwrap_or(""))
    };
    let (status, extra, text) = match path {
        "/redir" => ("302 Found", "location: /target\r\n", String::new()),
        "/target" => ("200 OK", "x-t: 1\r\n", "target".to_owned()),
        _ => {
            let reply = json!({ "method": method, "path": path, "body": body,
                                "chunked": head.to_lowercase().contains("transfer-encoding: chunked") });
            (
                "200 OK",
                "content-type: application/json\r\n",
                reply.to_string(),
            )
        }
    };
    let reply = format!(
        "HTTP/1.1 {status}\r\n{extra}content-length: {}\r\nconnection: close\r\n\r\n{text}",
        text.len()
    );
    let _ = stream.write_all(reply.as_bytes());
}

/// Runs `script` in the main process once the app is ready, with the server's `port` and the
/// helpers `note(key, value)` and `fail(e)`; answers what it noted.
fn main_says(script: &str, files: &[(&str, &str)]) -> Value {
    let port = server();
    let main = format!(
        "const {{ app, net }} = require('electron');
         const {{ pathToFileURL }} = require('url'); const path = require('path');
         globalThis.out = {{}}; const port = {port};
         const note = (key, value) => {{ out[key] = value; }};
         const fail = (e) => String(e && e.message);
         app.whenReady().then(async () => {{ {script}
           note('done', true); }}).catch((e) => note('threw', String(e && e.stack)));"
    );
    support::report_of(&main, files)
}

/// A script's helper that makes a request, ends it, records the events it gets and waits for the
/// response's body to end or for the request to fail.
const REQUEST: &str = "const request = (options, listen = () => {}) => new Promise((resolve) => {
    const events = [], req = net.request(options);
    for (const name of ['response', 'finish', 'abort', 'error', 'close', 'redirect']) {
      req.on(name, (...args) => {
        events.push(name === 'redirect' ? 'redirect:' + JSON.stringify(args.slice(0, 3)) : name === 'error' ? 'error:' + args[0].message : name);
        if (name === 'response') {
          const res = args[0]; let body = '';
          events.push('status:' + res.statusCode + ' ' + res.statusMessage);
          res.on('data', (chunk) => body += chunk);
          res.on('end', () => { events.push('end:' + body); setTimeout(() => resolve(events), 50); });
        }
        if (name === 'error' || name === 'abort') setTimeout(() => resolve(events), 200);
        listen(name, req, args);
      });
    }
    req.end();
  });";

#[test]
fn net_request_follows_a_redirect_by_default_and_says_so() {
    let out = main_says(
        &format!(
            "{REQUEST}
             let extra = '';
             const events = await request('http://127.0.0.1:' + port + '/redir', (name, req, args) => {{
               if (name === 'response') extra = args[0].headers['x-t'] + ' ' + args[0].httpVersion;
             }});
             note('extra', extra);
             note('events', events);"
        ),
        &[],
    );
    let events: Vec<String> = serde_json::from_value(out["events"].clone()).unwrap();
    // `finish` and `close` come before the response, as `Writable` ends the request when its
    // body has been handed over (probe2, `request-follow`).
    assert_eq!(events[0], "finish");
    assert_eq!(events[1], "close");
    assert!(
        events[2].starts_with("redirect:[302,\"GET\",\"http://127.0.0.1:"),
        "{events:?}"
    );
    assert!(events[2].ends_with("/target\"]"), "{events:?}");
    assert_eq!(&events[3..], ["response", "status:200 OK", "end:target"]);
    assert_eq!(out["extra"], "1 1.1");
}

#[test]
fn a_manual_redirect_waits_for_followredirect_and_is_cancelled_without_it() {
    let out = main_says(
        &format!(
            "{REQUEST}
             const followed = await request({{ url: 'http://127.0.0.1:' + port + '/redir', redirect: 'manual' }},
               (name, req) => {{ if (name === 'redirect') req.followRedirect(); }});
             note('followed', followed.filter((e) => !e.startsWith('redirect')));
             const refused = await request({{ url: 'http://127.0.0.1:' + port + '/redir', redirect: 'manual' }});
             note('refused', refused);
             const errored = await request({{ url: 'http://127.0.0.1:' + port + '/redir', redirect: 'error' }});
             note('errored', errored);"
        ),
        &[],
    );
    let list = |key: &str| -> Vec<String> { serde_json::from_value(out[key].clone()).unwrap() };
    let followed = list("followed");
    assert!(
        followed.contains(&"response".to_owned()) && followed.contains(&"end:target".to_owned()),
        "{followed:?}"
    );
    let refused = list("refused");
    assert!(
        refused.contains(&"error:Redirect was cancelled".to_owned()),
        "{refused:?}"
    );
    assert!(!refused.contains(&"response".to_owned()), "{refused:?}");
    let errored = list("errored");
    assert!(
        errored
            .contains(&"error:Attempted to redirect, but redirect policy was 'error'".to_owned()),
        "{errored:?}"
    );
}

#[test]
fn a_post_written_in_pieces_arrives_whole() {
    let out = main_says(
        &format!(
            "{REQUEST}
             const events = await request({{ method: 'POST', url: 'http://127.0.0.1:' + port + '/echo', headers: {{ 'X-One': '1' }} }},
               (name, req) => {{}});
             note('events', events);
             const sent = await new Promise((resolve) => {{
               const req = net.request({{ method: 'POST', url: 'http://127.0.0.1:' + port + '/echo' }});
               req.chunkedEncoding = true;
               req.on('response', (res) => {{ let b = ''; res.on('data', (c) => b += c); res.on('end', () => resolve(JSON.parse(b))); }});
               req.write('part1,'); req.write('part2,'); req.end('end');
             }});
             note('sent', sent);
             const req2 = net.request('http://127.0.0.1:' + port + '/x');
             req2.setHeader('X-Two', '2');
             note('headers', [req2.getHeader('x-two'), req2.getHeader('X-Absent') === undefined]);
             try {{ req2.setHeader('bad name', 'x'); }} catch (e) {{ note('bad', fail(e)); }}
             req2.abort();"
        ),
        &[],
    );
    let events: Vec<String> = serde_json::from_value(out["events"].clone()).unwrap();
    assert!(
        events.contains(&"response".to_owned()) && events.contains(&"status:200 OK".to_owned()),
        "{events:?}"
    );
    assert_eq!(out["sent"]["method"], "POST");
    assert_eq!(out["sent"]["body"], "part1,part2,end");
    assert_eq!(out["headers"], json!(["2", true]));
    assert_eq!(out["bad"], "Invalid header name: 'bad name'");
}

#[test]
fn net_fetch_reads_files_and_servers_and_builds_a_plain_response() {
    let out = main_says(
        "const file = pathToFileURL(path.join(__dirname, 'data.json')).toString();
         const r = await net.fetch(file);
         note('file', [r.status, r.statusText, r.headers.get('content-type'), r.url, r.type, r.redirected, await r.text()]);
         const h = await net.fetch('http://127.0.0.1:' + port + '/redir');
         note('redirect', [h.status, h.url, h.redirected, await h.text()]);
         try { await net.fetch('http://127.0.0.1:' + port + '/redir', { redirect: 'manual' }); } catch (e) { note('manual', fail(e)); }
         const p = await net.fetch('http://127.0.0.1:' + port + '/echo', { method: 'POST', body: 'hi' });
         note('post', (await p.json()).body);
         try { await net.fetch(file + 'nope'); } catch (e) { note('missing', [e.name, e.message]); }
         try { await net.fetch('not a url'); } catch (e) { note('bad', e.name); }",
        &[("data.json", r#"{"a":1}"#)],
    );
    // Probe2 `netfetch-file`: 200 OK, application/json, no url, `default`; `netfetch-redir`:
    // followed, `redirected` false, no url. `statusText` of a file is `OK` (probe4, Electron 39.8.10).
    assert_eq!(
        out["file"],
        json!([
            200,
            "OK",
            "application/json",
            "",
            "default",
            false,
            r#"{"a":1}"#
        ])
    );
    assert_eq!(out["redirect"], json!([200, "", false, "target"]));
    assert_eq!(out["manual"], "Redirect was cancelled");
    assert_eq!(out["post"], "hi");
    assert_eq!(out["missing"], json!(["Error", "net::ERR_FILE_NOT_FOUND"]));
    assert_eq!(out["bad"], "TypeError");
}

/// Electron 39.8.10 answers a `file:` fetch with `content-type` and `last-modified` and nothing
/// else (probe2, `netfetch-file`): the date is the file's own, in an HTTP date's form.
#[test]
fn net_fetch_of_a_file_carries_the_files_modified_time() {
    let out = main_says(
        "const file = path.join(__dirname, 'data.json');
         const r = await net.fetch(pathToFileURL(file).toString());
         note('names', [...r.headers.keys()].sort());
         note('date', r.headers.get('last-modified'));
         note('mtime', require('fs').statSync(file).mtime.toUTCString());",
        &[("data.json", r#"{"a":1}"#)],
    );
    assert_eq!(out["names"], json!(["content-type", "last-modified"]));
    assert_eq!(out["date"], out["mtime"]);
}

#[test]
fn net_answers_whether_the_machine_is_online_and_resolves_hosts() {
    let out = main_says(
        "note('online', [typeof net.isOnline(), net.isOnline() === net.online]);
         const found = await net.resolveHost('localhost');
         note('endpoints', found.endpoints.map((e) => e.family).sort());
         const ip = await net.resolveHost('127.0.0.1');
         note('ip', ip.endpoints);
         try { net.request({ url: 'ftp://example.com/' }); } catch (e) { note('ftp', fail(e)); }
         try { net.request({ url: 'nonsense' }); } catch (e) { note('nonsense', [e.name, e.code]); }",
        &[],
    );
    assert_eq!(out["online"], json!(["boolean", true]));
    assert!(!out["endpoints"].as_array().unwrap().is_empty());
    assert_eq!(
        out["ip"],
        json!([{ "address": "127.0.0.1", "family": "ipv4" }])
    );
    assert_eq!(
        out["ftp"],
        "ClientRequest only supports http: and https: protocols"
    );
    assert_eq!(out["nonsense"], json!(["TypeError", "ERR_INVALID_URL"]));
}

#[test]
fn net_request_before_ready_throws_electrons_error() {
    let main = "const { net } = require('electron'); globalThis.out = '';
        try { net.request('http://127.0.0.1:1/'); } catch (e) { globalThis.out = e.message; }";
    let app = App::launch_script(main, &[]);
    assert!(app.run_until(|app| app.eval_main("globalThis.out") != r#""""#, TIMEOUT));
    assert_eq!(
        app.eval_main("globalThis.out"),
        r#""net module can only be used after app is ready""#
    );
}

#[test]
fn an_aborted_request_finishes_then_aborts_then_closes() {
    let out = main_says(
        "const events = [];
         const req = net.request('http://127.0.0.1:' + port + '/x');
         for (const name of ['finish', 'abort', 'close', 'error', 'response']) req.on(name, () => events.push(name));
         req.end(); req.abort();
         await new Promise((resolve) => setTimeout(resolve, 300));
         note('events', events);",
        &[],
    );
    // Probe2 `request-abort`.
    assert_eq!(out["events"], json!(["finish", "abort", "close"]));
}

//! `net`: `net.fetch` of files and of a local server, `isOnline` and `resolveHost`. Every server
//! is a thread of this test on `127.0.0.1`. The expected values are frozen from Electron 39.8.10
//! (probe2 in the P16 scratch folder, and the source of `lib/browser/api/net-fetch.ts`).

#![allow(clippy::unwrap_used)]

mod support;

use std::io::{Read, Write};

use serde_json::{Value, json};

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

#[test]
fn net_answers_whether_the_machine_is_online_and_resolves_hosts() {
    let out = main_says(
        "note('online', [typeof net.isOnline(), net.isOnline() === net.online]);
         const found = await net.resolveHost('localhost');
         note('endpoints', found.endpoints.map((e) => e.family).sort());
         const ip = await net.resolveHost('127.0.0.1');
         note('ip', ip.endpoints);",
        &[],
    );
    assert_eq!(out["online"], json!(["boolean", true]));
    assert!(!out["endpoints"].as_array().unwrap().is_empty());
    assert_eq!(
        out["ip"],
        json!([{ "address": "127.0.0.1", "family": "ipv4" }])
    );
}

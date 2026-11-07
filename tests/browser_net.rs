//! The browser's network hook (`verve::browser_net`): a host that answers an address itself,
//! pulled a chunk at a time, and one that watches the requests that go to the network. A realm
//! with no hook fetches as `tests/fetch_network.rs` shows; these tests are the realm with one.

use std::cell::RefCell;
use std::collections::{HashMap, VecDeque};
use std::io::{Read, Write};
use std::rc::Rc;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use boa_engine::object::builtins::JsPromise;
use boa_engine::{Context, JsValue, js_string};
use verve::browser_net::{
    BrowserNet, Decider, Decision, FailureStyle, InternalFetch, NetError, NetEvent, RequestHead,
    Responder, ResponseDecision, ResponseHead, fetch_internal,
};
use verve::{ListenPermission, NetworkRule, Permissions, Runtime};

/// What the fake host does with the requests it is asked about.
#[derive(Clone, Copy, Default, PartialEq)]
enum Watch {
    /// Everything passes unchanged.
    #[default]
    Pass,
    /// A header is added to the request and one to the response.
    Edit,
    /// The request is cancelled.
    Cancel,
    /// The request is sent to `/second` instead.
    Redirect,
    /// The response is cancelled.
    CancelResponse,
}

/// A request the host was asked to handle, with its body.
type Seen = (RequestHead, Option<Vec<u8>>);
/// An unfinished answer: its responder and the chunks it still owes.
type Owing = (Responder, VecDeque<Vec<u8>>);

/// A host answering `app:` itself, and watching the rest.
#[derive(Default)]
struct Fake {
    watch: Watch,
    style: FailureStyle,
    /// What happened, in order: `handle`, `pull`, `cancel`, `notify` lines.
    log: Rc<RefCell<Vec<String>>>,
    /// The requests that were asked to be handled, with their bodies.
    seen: Rc<RefCell<Vec<Seen>>>,
    /// The responders of answers not yet finished, with the chunks they still owe.
    open: RefCell<HashMap<u64, Owing>>,
}

impl Fake {
    fn note(&self, line: String) {
        self.log.borrow_mut().push(line);
    }
}

impl BrowserNet for Fake {
    fn custom(&self, url: &url::Url) -> bool {
        url.scheme() == "app"
    }

    fn handle(
        &self,
        request: RequestHead,
        body: Option<Vec<u8>>,
        respond: Responder,
        _: &mut Context,
    ) {
        self.note(format!("handle {} {}", request.method, request.url));
        let path = url::Url::parse(&request.url).unwrap().path().to_owned();
        let id = request.id;
        self.seen.borrow_mut().push((request, body.clone()));
        match path.as_str() {
            "/chunks" | "/many" => {
                let count = if path == "/many" { 50 } else { 3 };
                let chunks = (0..count).map(|n| format!("[{n}]").into_bytes()).collect();
                respond.head(
                    200,
                    "OK".into(),
                    vec![("content-type".into(), "text/x".into())],
                );
                self.open.borrow_mut().insert(id, (respond, chunks));
            }
            "/fail" => respond.fail(NetError::from_code(-6)),
            "/echo" => {
                respond.head(201, "Made".into(), vec![("x-seen".into(), "yes".into())]);
                respond.chunk(body.unwrap_or_default());
                respond.end();
            }
            "/nobody" => {
                respond.head(204, String::new(), Vec::new());
                respond.end();
            }
            _ => drop(respond),
        }
    }

    fn pull(&self, id: u64, _: &mut Context) {
        self.note("pull".into());
        let mut open = self.open.borrow_mut();
        let Some((respond, chunks)) = open.get_mut(&id) else {
            return;
        };
        match chunks.pop_front() {
            Some(chunk) => respond.chunk(chunk),
            None => {
                if let Some((respond, _)) = open.remove(&id) {
                    respond.end();
                }
            }
        }
    }

    fn cancel(&self, id: u64, _: &mut Context) {
        self.note("cancel".into());
        self.open.borrow_mut().remove(&id);
    }

    fn before_send(
        &self,
        mut head: RequestHead,
        _: Option<&[u8]>,
        decide: Decider<Decision>,
        _: &mut Context,
    ) {
        self.note(format!("before_send {}", head.url));
        decide.decide(match self.watch {
            Watch::Cancel => Decision::Cancel,
            Watch::Redirect if !head.url.ends_with("/second") => {
                Decision::Redirect(head.url.replace("/first", "/second"))
            }
            Watch::Edit => {
                head.headers.push(("x-added".into(), "by-host".into()));
                Decision::Continue(head)
            }
            _ => Decision::Continue(head),
        });
    }

    fn headers_received(
        &self,
        request: &RequestHead,
        mut response: ResponseHead,
        decide: Decider<ResponseDecision>,
        _: &mut Context,
    ) {
        self.note(format!(
            "headers_received {} {}",
            request.url, response.status
        ));
        decide.decide(match self.watch {
            Watch::CancelResponse => ResponseDecision::Cancel,
            Watch::Edit => {
                response.headers.push(("x-edited".into(), "yes".into()));
                ResponseDecision::Continue(response)
            }
            _ => ResponseDecision::Continue(response),
        });
    }

    fn notify(&self, event: NetEvent, _: &mut Context) {
        let line = match event {
            NetEvent::Sent(_) => "sent".to_owned(),
            NetEvent::Started { response, .. } => format!("started {}", response.status),
            NetEvent::Redirected { to, .. } => format!("redirected {to}"),
            NetEvent::Completed { .. } => "completed".to_owned(),
            NetEvent::Failed { error, .. } => format!("failed {}", error.name),
        };
        self.note(line);
    }

    fn failure_style(&self) -> FailureStyle {
        self.style
    }

    fn user_agent(&self) -> Option<String> {
        Some("Hosted/1.0".into())
    }
}

fn loopback() -> Permissions {
    Permissions {
        outbound: vec![NetworkRule {
            host: Some("127.0.0.1".into()),
            port: None,
        }],
        listen: ListenPermission::Loopback,
        dns: false,
        child_process: false,
        ..Permissions::default()
    }
}

/// Runs `body`, an async function's body that sets `result`, in a realm whose `fetch` asks `net`;
/// answers `result` as JSON.
fn run(net: &Rc<Fake>, body: &str) -> String {
    let mut rt = Runtime::new();
    rt.set_permissions(loopback());
    rt.set_browser_net(Some(Rc::clone(net) as Rc<dyn BrowserNet>));
    let script = format!(
        "(async () => {{ {body}\n}})().catch(e => {{ globalThis.result = ['threw', String(e), e && e.cause ? String(e.cause.code || e.cause) : null]; }});"
    );
    let entry = std::env::temp_dir().join("verve-browser-net-test.js");
    rt.load_module(&entry, &script).unwrap();
    let deadline = Instant::now() + Duration::from_secs(20);
    while !rt
        .eval("globalThis.result !== undefined")
        .unwrap()
        .to_boolean()
    {
        rt.run_due();
        assert!(
            Instant::now() < deadline,
            "timed out: {:?} {:?}",
            net.log.borrow(),
            rt.take_console()
        );
        std::thread::sleep(Duration::from_millis(1));
    }
    verve::print(&rt.eval("JSON.stringify(globalThis.result)").unwrap())
}

fn fake(watch: Watch) -> Rc<Fake> {
    Rc::new(Fake {
        watch,
        ..Fake::default()
    })
}

#[test]
fn an_address_the_host_claims_is_answered_by_the_host_a_chunk_per_pull() {
    let net = fake(Watch::Pass);
    let result = run(
        &net,
        "const r = await fetch('app://bundle/chunks');
         result = [r.status, r.statusText, r.headers.get('content-type'), r.url, await r.text()];",
    );
    assert_eq!(
        result,
        r#"[200,"OK","text/x","app://bundle/chunks","[0][1][2]"]"#
    );
    let log = net.log.borrow();
    assert_eq!(log[0], "handle GET app://bundle/chunks");
    // One pull for each chunk, one that finds the end.
    assert_eq!(
        log.iter().filter(|line| *line == "pull").count(),
        4,
        "{log:?}"
    );
}

#[test]
fn a_reader_that_reads_everything_gets_everything_in_order() {
    let net = fake(Watch::Pass);
    let result = run(
        &net,
        "const r = await fetch('app://bundle/many');
         const text = await r.text();
         result = [text.length, text.startsWith('[0][1][2]'), text.endsWith('[49]')];",
    );
    assert_eq!(result, "[190,true,true]");
}

#[test]
fn a_reader_that_stops_reading_stops_the_hosts_pulls() {
    let net = fake(Watch::Pass);
    let result = run(
        &net,
        "const r = await fetch('app://bundle/many');
         const reader = r.body.getReader();
         await reader.read();
         await new Promise(done => setTimeout(done, 150));
         result = [r.status];",
    );
    assert_eq!(result, "[200]");
    let pulls = net
        .log
        .borrow()
        .iter()
        .filter(|line| *line == "pull")
        .count();
    // The stream pulls once when it is made (high-water mark 1) and once more when the one
    // read empties its queue; then it is full again, and nothing pulls until another read.
    assert_eq!(pulls, 2, "pulls for a reader that read one chunk");
}

#[test]
fn a_host_that_fails_fails_the_fetch_with_the_net_error_as_cause() {
    let net = fake(Watch::Pass);
    let result = run(&net, "await fetch('app://bundle/fail');");
    assert_eq!(
        result,
        r#"["threw","TypeError: fetch failed","ERR_FILE_NOT_FOUND"]"#
    );
    let result = run(&net, "await fetch('app://bundle/unanswered');");
    assert_eq!(
        result,
        r#"["threw","TypeError: fetch failed","ERR_FAILED"]"#
    );
}

#[test]
fn a_failure_rejects_in_the_style_the_host_asks_for() {
    for (style, expected) in [
        (
            FailureStyle::Chromium,
            r#"["threw","TypeError: Failed to fetch",null]"#,
        ),
        (
            FailureStyle::Net,
            r#"["threw","Error: net::ERR_FILE_NOT_FOUND",null]"#,
        ),
    ] {
        let net = Rc::new(Fake {
            style,
            ..Fake::default()
        });
        assert_eq!(run(&net, "await fetch('app://bundle/fail');"), expected);
    }
}

#[test]
fn the_request_reaches_the_host_whole_and_a_null_body_status_has_no_body() {
    let net = fake(Watch::Pass);
    let result = run(
        &net,
        "const r = await fetch('app://bundle/echo', { method: 'POST', headers: { 'X-A': 'b' }, body: 'payload' });
         const none = await fetch('app://bundle/nobody');
         result = [r.status, r.headers.get('x-seen'), await r.text(), none.status, none.body];",
    );
    assert_eq!(result, r#"[201,"yes","payload",204,null]"#);
    let seen = net.seen.borrow();
    let (head, body) = &seen[0];
    assert_eq!(
        (head.method.as_str(), head.resource_type.name()),
        ("POST", "xhr")
    );
    assert!(
        head.headers.contains(&("x-a".into(), "b".into())),
        "{head:?}"
    );
    assert_eq!(body.as_deref(), Some(&b"payload"[..]));
}

#[test]
fn cancelling_the_body_or_aborting_tells_the_host_to_stop() {
    let net = fake(Watch::Pass);
    let result = run(
        &net,
        "const r = await fetch('app://bundle/many');
         await r.body.cancel();
         const c = new AbortController();
         const p = fetch('app://bundle/many', { signal: c.signal });
         const second = await p;
         c.abort();
         let after;
         try { await second.arrayBuffer(); } catch (e) { after = e.name; }
         result = [after];",
    );
    assert_eq!(result, r#"["AbortError"]"#);
    let cancels = net
        .log
        .borrow()
        .iter()
        .filter(|line| *line == "cancel")
        .count();
    assert_eq!(cancels, 2);
}

// ─── The requests that go to the network ────────────────────────────────────────────────────

/// A server on a Rust thread that answers every request with its own request head as the body
/// (`/first` and `/second` too), and `x-server` in the head; answers its port and the heads it saw.
fn echo_server(connections: usize) -> (u16, Arc<Mutex<Vec<String>>>) {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let heads = Arc::new(Mutex::new(Vec::new()));
    let kept = Arc::clone(&heads);
    std::thread::spawn(move || {
        for stream in listener.incoming().take(connections) {
            let mut stream = stream.unwrap();
            let mut seen = Vec::new();
            let mut buffer = [0; 1024];
            while !seen.windows(4).any(|w| w == b"\r\n\r\n") {
                let n = stream.read(&mut buffer).unwrap();
                if n == 0 {
                    break;
                }
                seen.extend_from_slice(&buffer[..n]);
            }
            let head = String::from_utf8_lossy(&seen).into_owned();
            kept.lock().unwrap().push(head.clone());
            let reply = format!(
                "HTTP/1.1 200 OK\r\nx-server: 1\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{head}",
                head.len()
            );
            stream.write_all(reply.as_bytes()).unwrap();
        }
    });
    (port, heads)
}

#[test]
fn the_host_edits_the_request_and_the_response_of_a_network_fetch() {
    let (port, heads) = echo_server(1);
    let net = fake(Watch::Edit);
    let result = run(
        &net,
        &format!(
            "const r = await fetch('http://127.0.0.1:{port}/first', {{ headers: {{ 'X-Own': '1' }} }});
             const text = await r.text();
             result = [r.status, r.headers.get('x-edited'), r.headers.get('x-server'), text.includes('x-added: by-host'), text.includes('user-agent: Hosted/1.0'), text.includes('x-own: 1')];"
        ),
    );
    assert_eq!(result, r#"[200,"yes","1",true,true,true]"#);
    assert!(
        heads.lock().unwrap()[0]
            .to_lowercase()
            .contains("x-added: by-host")
    );
    let log = net.log.borrow();
    let stages: Vec<_> = log
        .iter()
        .map(|line| line.split(' ').next().unwrap())
        .collect();
    assert_eq!(
        stages,
        [
            "before_send",
            "sent",
            "headers_received",
            "started",
            "completed"
        ],
        "{log:?}"
    );
    assert_eq!(log[0], format!("before_send http://127.0.0.1:{port}/first"));
}

#[test]
fn the_host_cancels_a_request_before_it_is_sent_or_a_response_after() {
    let (port, heads) = echo_server(1);
    let net = fake(Watch::Cancel);
    let result = run(
        &net,
        &format!("await fetch('http://127.0.0.1:{port}/first');"),
    );
    assert_eq!(
        result,
        r#"["threw","TypeError: fetch failed","ERR_BLOCKED_BY_CLIENT"]"#
    );
    assert!(
        heads.lock().unwrap().is_empty(),
        "a cancelled request is never sent"
    );
    assert!(
        net.log
            .borrow()
            .contains(&"failed net::ERR_BLOCKED_BY_CLIENT".to_owned())
    );
    let net = fake(Watch::CancelResponse);
    let result = run(
        &net,
        &format!("await fetch('http://127.0.0.1:{port}/first');"),
    );
    assert_eq!(
        result,
        r#"["threw","TypeError: fetch failed","ERR_BLOCKED_BY_CLIENT"]"#
    );
}

#[test]
fn the_host_sends_a_request_to_another_address() {
    let (port, heads) = echo_server(1);
    let net = fake(Watch::Redirect);
    let result = run(
        &net,
        &format!(
            "const r = await fetch('http://127.0.0.1:{port}/first'); result = [r.status, r.redirected, r.url];"
        ),
    );
    assert_eq!(
        result,
        format!(r#"[200,true,"http://127.0.0.1:{port}/second"]"#)
    );
    assert!(heads.lock().unwrap()[0].starts_with("GET /second "));
    let log = net.log.borrow();
    assert!(
        log.iter()
            .any(|line| line.starts_with("before_send") && line.ends_with("/second"))
    );
}

// ─── An explicit hook ───────────────────────────────────────────────────────────────────────

/// The state of the promise `fetch_internal` answered, after the realm has run.
fn settled(rt: &mut Runtime, promise: boa_engine::JsObject) -> String {
    let promise = JsPromise::from_object(promise).unwrap();
    for _ in 0..2000 {
        rt.run_due();
        if !matches!(
            promise.state(),
            boa_engine::builtins::promise::PromiseState::Pending
        ) {
            break;
        }
        std::thread::sleep(Duration::from_millis(1));
    }
    match promise.state() {
        boa_engine::builtins::promise::PromiseState::Fulfilled(value) => {
            let status = value
                .as_object()
                .unwrap()
                .get(js_string!("status"), rt.context())
                .unwrap();
            format!("fulfilled {}", verve::print(&status))
        }
        boa_engine::builtins::promise::PromiseState::Rejected(reason) => {
            format!("rejected {}", verve::print(&reason))
        }
        boa_engine::builtins::promise::PromiseState::Pending => "pending".into(),
    }
}

#[test]
fn a_fetch_the_host_makes_takes_its_own_hook_and_none_means_the_realms_own_ways() {
    let mut rt = Runtime::new();
    let net = fake(Watch::Pass);
    let how = |net: Option<Rc<dyn BrowserNet>>| InternalFetch {
        net,
        resource_type: verve::browser_net::ResourceType::Other,
        initiator: Some("app://bundle".into()),
    };
    let url = JsValue::from(js_string!("app://bundle/echo"));
    let answered = fetch_internal(
        rt.context(),
        &url,
        &JsValue::undefined(),
        how(Some(Rc::clone(&net) as Rc<dyn BrowserNet>)),
    );
    assert_eq!(settled(&mut rt, answered), "fulfilled 201");
    let head = net.seen.borrow()[0].0.clone();
    assert_eq!(head.resource_type.name(), "other");
    assert_eq!(head.initiator.as_deref(), Some("app://bundle"));
    // Given no hook, the fetch does not know the scheme.
    let refused = fetch_internal(rt.context(), &url, &JsValue::undefined(), how(None));
    assert!(settled(&mut rt, refused).starts_with("rejected TypeError: fetch failed"));
}

//! Web Workers: a script from a blob address, run in a realm of its own on a thread of its own,
//! spoken to by messages that are copied across — and a page that never waits for one.

use runtime_x::RuntimeX;

fn js(rt: &mut RuntimeX, source: &str) -> String {
    let value = rt.eval(source).unwrap_or_else(|e| panic!("{source}: {e}"));
    runtime_x::print(&value)
}

#[test]
fn a_worker_answers_its_page() {
    let mut rt = RuntimeX::new();
    js(
        &mut rt,
        r#"
        globalThis.heard = [];
        const code = `
            console.log('worker up');
            self.onmessage = (e) => {
                const { n, when, tags, bytes } = e.data;
                setTimeout(() => postMessage({
                    doubled: n * 2,
                    year: when.getUTCFullYear(),
                    tags: tags.get('a'),
                    sum: bytes.reduce((a, b) => a + b, 0),
                    isDate: when instanceof Date,
                }), 0);
            };
            addEventListener('message', (e) => { if (e.data === 'bye') close(); });
        `;
        const url = URL.createObjectURL(new Blob([code], { type: 'application/javascript' }));
        globalThis.worker = new Worker(url, { name: 'doubler' });
        worker.onmessage = (e) => heard.push(JSON.stringify(e.data));
        worker.postMessage({ n: 21, when: new Date(Date.UTC(2024, 0, 1)), tags: new Map([['a', 'x']]), bytes: new Uint8Array([1, 2, 3]) });
        'sent'
    "#,
    );
    // Not yet: a message waits for the beat.
    assert_eq!(js(&mut rt, "heard.length"), "0");
    assert!(rt.next_due().is_some(), "the worker has work waiting");
    for _ in 0..50 {
        rt.wait_for_workers(std::time::Duration::from_millis(100));
        rt.run_workers();
        if js(&mut rt, "heard.length") != "0" {
            break;
        }
    }
    assert_eq!(
        js(&mut rt, "heard[0]"),
        r#"{"doubled":42,"year":2024,"tags":"x","sum":6,"isDate":true}"#
    );
    let console: Vec<String> = rt.take_console().into_iter().map(|l| l.text).collect();
    assert!(
        console.iter().any(|l| l == "[worker] worker up"),
        "{console:?}"
    );
    // A worker that closes itself is gone.
    js(&mut rt, "worker.postMessage('bye')");
    for _ in 0..50 {
        rt.wait_for_workers(std::time::Duration::from_millis(100));
        rt.run_workers();
        if rt.next_due().is_none() {
            break;
        }
    }
    assert!(rt.next_due().is_none());
}

/// A worker busy for a long time keeps its page free: the page is answered on a later beat,
/// and every beat in between returns at once.
#[test]
fn a_busy_worker_does_not_hold_its_page() {
    let mut rt = RuntimeX::new();
    js(
        &mut rt,
        r#"
        globalThis.heard = [];
        const code = `
            self.onmessage = (e) => {
                const until = Date.now() + e.data;
                while (Date.now() < until) {}
                postMessage({ done: e.data, cycle: (() => { const a = { name: 'a' }; a.self = a; return a; })() });
            };
        `;
        const url = URL.createObjectURL(new Blob([code]));
        globalThis.worker = new Worker(url);
        worker.onmessage = (e) => heard.push(e.data.done + ' ' + (e.data.cycle.self === e.data.cycle));
        worker.postMessage(300);
    "#,
    );
    let started = std::time::Instant::now();
    let mut beats = 0;
    while js(&mut rt, "heard.length") == "0" {
        let beat = std::time::Instant::now();
        rt.run_workers();
        assert!(
            beat.elapsed() < std::time::Duration::from_millis(50),
            "a beat waited for the worker"
        );
        // Looked in on again at once, until it answers.
        if js(&mut rt, "heard.length") == "0" {
            let due = rt.next_due();
            let now = rt.now();
            assert!(
                due.is_some_and(|due| due <= now),
                "a worker at work is worth looking in on: {due:?} at {now}"
            );
        }
        beats += 1;
        std::thread::sleep(std::time::Duration::from_millis(10));
        assert!(
            started.elapsed() < std::time::Duration::from_secs(10),
            "the worker never answered"
        );
    }
    assert!(
        beats > 5,
        "the page ran {beats} beats while the worker worked"
    );
    assert_eq!(js(&mut rt, "heard[0]"), "300 true");
}

/// A worker's own script is work its page waits on: what it posts as it starts is heard, though
/// the page has sent it nothing — and a worker whose script failed is waited on no longer.
#[test]
fn a_worker_is_heard_from_its_script_alone() {
    let mut rt = RuntimeX::new();
    js(
        &mut rt,
        r#"
        globalThis.heard = [];
        const url = URL.createObjectURL(new Blob(['postMessage("up")']));
        globalThis.worker = new Worker(url);
        worker.onmessage = (e) => heard.push(e.data);
        URL.revokeObjectURL(url);
        const broken = URL.createObjectURL(new Blob(['this is not javascript']));
        globalThis.brokenWorker = new Worker(broken);
    "#,
    );
    for _ in 0..50 {
        rt.wait_for_workers(std::time::Duration::from_millis(100));
        rt.run_workers();
        if js(&mut rt, "heard.length") != "0" {
            break;
        }
    }
    assert_eq!(js(&mut rt, "heard.join()"), "up");
    // The broken worker's script is done with, failed: nothing of it is waited for.
    let started = std::time::Instant::now();
    rt.wait_for_workers(std::time::Duration::from_secs(5));
    assert!(started.elapsed() < std::time::Duration::from_secs(1));
    js(&mut rt, "worker.terminate(); brokenWorker.terminate()");
}

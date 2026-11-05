//! What IPC costs between the main process and a page, which share a process and differ by a
//! thread: the round trip of an `invoke`, a megabyte sent one way, and a burst of small messages.
//! Slow and sensitive to what else the machine is doing, so ignored by default:
//!
//! ```text
//! cargo test -p verve-electron --release --test ipc_bench -- --ignored --nocapture
//! ```

mod support;

use serde_json::Value;
use support::{App, TIMEOUT};

/// An app whose page can `invoke` a handler that returns what it was sent, `send` to a sink that
/// counts what it hears, and ask how that went.
const MAIN: &str = r"
const { app, BrowserWindow, ipcMain } = require('electron');
globalThis.win = new BrowserWindow({ show: false, webPreferences: { nodeIntegration: true, contextIsolation: false } });
win.loadURL('data:text/html,<p>bench</p>');
ipcMain.handle('echo', (event, value) => value);
globalThis.heard = { count: 0, inOrder: true, bytes: 0 };
ipcMain.on('sink', (event, value) => {
  if (typeof value === 'number') { if (value !== heard.count) heard.inOrder = false; heard.count++; }
  else heard.bytes += value.length;
});
ipcMain.handle('heard', () => heard);
";

fn app() -> (App, u32) {
    let app = App::launch_script(MAIN, &[]);
    assert!(app.run_until(|app| !app.shell.windows().is_empty(), TIMEOUT));
    let page = app.shell.windows()[0].contents;
    (app, page)
}

/// Runs the body of an async function in the page and answers what it returns.
fn run(app: &App, page: u32, body: &str) -> Value {
    let body = format!("globalThis.ipc = require('electron').ipcRenderer; {body}");
    let out = app.run_page(page, &body, std::time::Duration::from_secs(120));
    out.get("value")
        .cloned()
        .expect("the page's script returned")
}

#[test]
#[ignore = "benchmark: see the module comment"]
fn invoke_round_trip() {
    let (app, page) = app();
    let times = run(
        &app,
        page,
        r"
for (let i = 0; i < 1000; i++) await ipc.invoke('echo', { a: 1 });
const times = [];
for (let i = 0; i < 10000; i++) {
  const began = performance.now();
  await ipc.invoke('echo', { a: 1 });
  times.push(performance.now() - began);
}
times.sort((a, b) => a - b);
return { p50: times[5000], p90: times[9000], p99: times[9900], max: times[9999] };",
    );
    println!("invoke x10000, small payload (ms): {times}");
    assert!(times["p50"].as_f64().unwrap() <= 0.2, "p50 {times}");
    assert!(times["p99"].as_f64().unwrap() <= 1.0, "p99 {times}");
}

#[test]
#[ignore = "benchmark: see the module comment"]
fn a_megabyte_sent_one_way() {
    let (app, page) = app();
    // One way is the time until the main process has it; the answer to a message sent after it
    // says it did, so what is measured is that and the way back of one small message.
    let each = run(
        &app,
        page,
        r"
const bytes = new Uint8Array(1 << 20);
await ipc.invoke('echo', 1);
const began = performance.now();
for (let i = 0; i < 50; i++) { ipc.send('sink', bytes); await ipc.invoke('echo', 1); }
return (performance.now() - began) / 50;",
    );
    println!("1 MiB Uint8Array send + one small round trip (ms, mean of 50): {each}");
    assert!(each.as_f64().unwrap() <= 2.0, "{each}");
}

#[test]
#[ignore = "benchmark: see the module comment"]
fn a_hundred_thousand_sends_wake_the_main_process_rarely_and_arrive_in_order() {
    let (app, page) = app();
    let before = app.hub.main_beats();
    let heard = run(
        &app,
        page,
        "for (let i = 0; i < 100000; i++) ipc.send('sink', i); return await ipc.invoke('heard');",
    );
    let beats = app.hub.main_beats() - before;
    println!("100000 sends: heard {heard}, main process beats {beats}");
    assert_eq!(heard["count"], 100_000);
    assert_eq!(heard["inOrder"], true);
    assert!(beats <= 10_000, "{beats} beats");
}

/// What a forced collection costs the main process when `sendSync` events are watched: a full
/// mark-sweep of a main heap of some tens of megabytes, the thing the sweep is rate-limited for.
#[test]
#[ignore = "benchmark: see the module comment"]
fn a_collection_of_a_large_main_heap() {
    let mut runtime = verve::Runtime::new();
    runtime
        .eval("globalThis.big = Array.from({ length: 400000 }, (_, i) => ({ i, text: 'item ' + i, list: [i, i + 1] })); 1")
        .expect("a large heap is built");
    boa_engine::gc::force_collect();
    let began = std::time::Instant::now();
    for _ in 0..5 {
        boa_engine::gc::force_collect();
    }
    println!(
        "one collection of a heap of 400000 objects with children (ms, mean of 5): {:.1}",
        began.elapsed().as_secs_f64() * 1000. / 5.
    );
}

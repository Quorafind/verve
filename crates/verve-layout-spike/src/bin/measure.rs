//! Q1: measure on the main thread and on a worker, and compare.
//!   measure            : no App, no window; TextSystem built from Platform::text_system()
//!   measure app        : inside a running GPUI app; the worker gets cx.text_system()
use gpui::{TextSystem, WindowTextSystem};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use verve_layout_spike::report;

fn assert_send_sync<T: Send + Sync>() {}

fn compare(label: &str, main: &str, worker: &str) -> bool {
    println!("===== {label}: main thread =====\n{main}");
    println!("===== {label}: worker thread =====\n{worker}");
    let same = main == worker;
    println!("===== {label}: MATCH = {same} =====\n");
    same
}

fn on_worker(ts: &Arc<TextSystem>, ws: &Arc<WindowTextSystem>) -> String {
    let (ts, ws) = (ts.clone(), ws.clone());
    std::thread::spawn(move || report(&ts, &ws))
        .join()
        .unwrap_or_default()
}

fn standalone() -> bool {
    // No Application is run: only the platform object is made, to get its text system.
    let platform = gpui_platform::current_platform(false);
    let ts = Arc::new(TextSystem::new(platform.text_system()));
    let ws = Arc::new(WindowTextSystem::new(ts.clone()));
    let main = report(&ts, &ws);
    let worker = on_worker(&ts, &ws);
    let same = compare("standalone", &main, &worker);
    // Eight threads at once, a hundred rounds each, all sharing one TextSystem.
    let handles: Vec<_> = (0..8)
        .map(|_| {
            let (ts, ws) = (ts.clone(), ws.clone());
            std::thread::spawn(move || (0..100).map(|_| report(&ts, &ws)).collect::<Vec<_>>())
        })
        .collect();
    let all_equal = handles
        .into_iter()
        .all(|h| h.join().is_ok_and(|v| v.iter().all(|r| *r == main)));
    println!(
        "===== standalone: 8 threads x 100 rounds, all equal to main = {all_equal} =====
"
    );
    same && all_equal
}

fn in_app() -> bool {
    let ok = Arc::new(AtomicBool::new(false));
    let ok2 = ok.clone();
    gpui_platform::application().run(move |cx| {
        let ts = cx.text_system().clone();
        let ws = Arc::new(WindowTextSystem::new(ts.clone()));
        let main = report(&ts, &ws);
        let worker = on_worker(&ts, &ws);
        ok2.store(compare("in-app", &main, &worker), Ordering::SeqCst);
        cx.quit();
    });
    ok.load(Ordering::SeqCst)
}

fn main() {
    assert_send_sync::<TextSystem>();
    assert_send_sync::<WindowTextSystem>();
    println!("TextSystem and WindowTextSystem are Send + Sync (compile-time check passed)\n");
    let ok = match std::env::args().nth(1).as_deref() {
        Some("app") => in_app(),
        _ => standalone(),
    };
    std::process::exit(if ok { 0 } else { 1 });
}

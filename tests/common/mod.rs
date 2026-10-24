//! What the stream and fetch tests share: running an async body to its end on a test's clock.

use std::cell::Cell;
use std::rc::Rc;

use runtime_x::RuntimeX;

/// Runs `body` — an async function's body, which sets `result` to an array — on a clock the
/// test moves, until it has set it; answers the array's items as strings.
pub fn run_async(body: &str) -> Vec<String> {
    let time = Rc::new(Cell::new(0.));
    let read = Rc::clone(&time);
    let mut rt = RuntimeX::with_clock(move || read.get());
    let source = format!(
        "globalThis.result = undefined; (async () => {{ {body}\n}})().catch(e => {{ globalThis.result = ['threw ' + (e && e.stack || e)]; }});"
    );
    let dir = std::env::temp_dir();
    rt.load_module(&dir.join("runtime-x-streams-test.js"), &source)
        .unwrap_or_else(|e| panic!("{e}"));
    for _ in 0..200 {
        if rt
            .eval("globalThis.result !== undefined")
            .map(|v| v.to_boolean())
            .unwrap_or(false)
        {
            break;
        }
        time.set(time.get() + 10.);
        rt.run_due();
    }
    let text = rt
        .eval("JSON.stringify(globalThis.result)")
        .unwrap_or_else(|e| panic!("{e}"));
    let parsed: serde_json::Value = serde_json::from_str(&runtime_x::print(&text))
        .unwrap_or_else(|_| panic!("no result: {}", runtime_x::print(&text)));
    parsed
        .as_array()
        .unwrap_or_else(|| panic!("result is not an array: {parsed}"))
        .iter()
        .map(|v| {
            v.as_str()
                .map(str::to_owned)
                .unwrap_or_else(|| v.to_string())
        })
        .collect()
}

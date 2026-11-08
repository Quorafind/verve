//! `require('electron')`'s first class, tested where it is made: no shell, no window on screen.
//! What it checks is the machinery every other package of the API is declared with — ES class
//! inheritance, a class that refuses a call without `new`, and Electron's argument errors.
//!
//! The messages are what Electron 39.8.10 answered to the same calls, observed with a probe
//! script run in `electron.exe` (`require('electron').BrowserWindow` in the main process).

use std::path::PathBuf;

use verve::Runtime;
use verve_electron::ElectronOptions;
use verve_electron::hub::Hub;

fn runtime() -> Runtime {
    let hub = Hub::new(ElectronOptions {
        app_path: PathBuf::from("."),
        dark: false,
    });
    let mut runtime = Runtime::new();
    verve_electron::api::install(&mut runtime, hub);
    // `eval` runs in the global scope, which has no module's `require`; the app's script gets one
    // from its own module, as `electron main.js` gives it (`tests/pipeline.rs`).
    let require = runtime.global_require();
    let context = runtime.context();
    context
        .register_global_property(
            verve::boa_engine::js_string!("require"),
            require,
            verve::boa_engine::property::Attribute::WRITABLE,
        )
        .expect("require is a global");
    runtime
}

/// What `source` — an expression — comes to, written out.
fn said(source: &str) -> String {
    let mut runtime = runtime();
    let value = runtime
        .eval(&format!(
            r#"(() => {{
                const {{ BrowserWindow }} = require('electron');
                const said = (f) => {{
                    try {{ return String(f()); }} catch (e) {{ return e.name + ': ' + e.message; }}
                }};
                return {source};
            }})()"#
        ))
        .unwrap_or_else(|error| panic!("{error}"));
    verve::print(&value)
}

#[test]
fn a_class_a_script_extends_is_a_browser_window() {
    // Electron 39: `class M extends BrowserWindow {}` makes windows that are both.
    assert_eq!(
        said(
            r#"(() => {
                class Mine extends BrowserWindow {}
                const win = new Mine({ title: 'mine' });
                return [
                    win instanceof Mine, win instanceof BrowserWindow,
                    win.id, win.webContents.id,
                    typeof win.loadURL, typeof win.on,
                ].join();
            })()"#
        ),
        "true,true,1,1,function,function"
    );
}

#[test]
fn a_window_is_an_event_emitter_whose_constructor_ran() {
    assert_eq!(
        said(
            r#"(() => {
                const win = new BrowserWindow();
                const seen = [];
                win.once('hello', (x) => seen.push(x));
                const emitted = [win.emit('hello', 1), win.emit('hello', 2)];
                return [...emitted, ...seen, win.listenerCount('hello')].join();
            })()"#
        ),
        "true,false,1,0"
    );
}

#[test]
fn a_call_without_new_throws_electrons_error() {
    // Electron 39.8.10: `BrowserWindow()`, `BrowserWindow.call({}, {})` and the
    // `util.inherits` subclass that calls it that way all throw `Error: Requires constructor
    // call` — an `Error`, not V8's `TypeError`, and no function-style subclass is possible.
    for call in [
        "BrowserWindow({ show: false })",
        "BrowserWindow.call({}, { show: false })",
        r#"(() => {
            const util = require('util');
            function Legacy(options) { BrowserWindow.call(this, options); }
            util.inherits(Legacy, BrowserWindow);
            return new Legacy({ show: false });
        })()"#,
    ] {
        assert_eq!(
            said(&format!("said(() => {call})")),
            "Error: Requires constructor call",
            "{call}"
        );
    }
}

#[test]
fn options_that_are_not_an_object_are_no_options() {
    // Electron 39.8.10: `new BrowserWindow(5)`, `('x')` and `(null)` make a default window.
    assert_eq!(
        said("[5, 'x', null, undefined].map((o) => new BrowserWindow(o).id).join()"),
        "1,2,3,4"
    );
}

#[test]
fn a_method_called_on_something_else_is_no_window() {
    // Electron 39.8.10: a method of `BrowserWindow` called on an object that is no window
    // answers `TypeError: Object has been destroyed`.
    assert_eq!(
        said("said(() => BrowserWindow.prototype.close.call({}))"),
        "TypeError: Object has been destroyed"
    );
}

#[test]
fn a_window_carries_nothing_a_script_can_copy() {
    assert_eq!(
        said(
            r#"(() => {
                const win = new BrowserWindow({ title: 'real' });
                const fake = Object.create(BrowserWindow.prototype);
                const proxy = new Proxy(win, {});
                return [
                    Object.getOwnPropertySymbols(win).length,
                    said(() => fake.loadURL('data:text/html,x')),
                    said(() => Object.assign(Object.create(BrowserWindow.prototype), win).close()),
                    said(() => proxy.close()),
                ].join('|');
            })()"#
        ),
        "0|TypeError: Object has been destroyed|TypeError: Object has been destroyed|\
         TypeError: Object has been destroyed"
    );
}

#[test]
fn a_url_that_is_not_a_string_is_a_conversion_failure() {
    // Electron 39.8.10, `win.loadURL(x)`: the text after `from` is the value when it is a
    // string, `null` or `undefined`, and empty for anything else.
    let said_for = |arg: &str| said(&format!("said(() => new BrowserWindow().loadURL({arg}))"));
    let failure = |from: &str| {
        format!("TypeError: Error processing argument at index 0, conversion failure from {from}")
    };
    assert_eq!(said_for("5"), failure(""));
    assert_eq!(said_for("undefined"), failure("undefined"));
    assert_eq!(said_for(""), failure("undefined"));
    assert_eq!(said_for("null"), failure("null"));
    assert_eq!(said_for("{}"), failure(""));
    assert_eq!(said_for("true"), failure(""));
}

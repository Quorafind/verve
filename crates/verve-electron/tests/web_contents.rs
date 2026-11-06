//! `webContents`, on the headless shell: loading and its events, the history, `will-navigate`,
//! scripts and sheets run in the page, `window.open`, the input events and the statics. The
//! expected values were observed on Electron 39.8.10 (Windows) by
//! `tests/electron/web-contents/observe` and are frozen here; a test says so when its number comes
//! from Verve's own rules instead.
//!
//! The headless shell lays a page out as a stack of blocks, 16 px a line and 8 px a character
//! (`FakeLayout::BlockStack`), so `display: block` elements are where a pointer can find them.
#![allow(clippy::unwrap_used)]

mod support;

use support::{App, TIMEOUT, run, run_main, run_with};
use verve_dom::Key;
use verve_electron::msg::FromShell;

/// The pages the `window.open` tests open from and to: a window opens only what its opener may.
const FILES: &[(&str, &str)] = &[("o.html", "<p>o</p>"), ("c.html", "<p>child</p>")];

/// The events a first load says, in the order Electron 39.8.10 says them.
const LOAD_EVENTS: &str = "['frame-created', 'did-start-loading', 'did-start-navigation', 'will-navigate', 'will-frame-navigate', 'did-frame-navigate', 'did-navigate', 'page-title-updated', 'dom-ready', 'did-frame-finish-load', 'did-finish-load', 'did-fail-provisional-load', 'did-fail-load', 'did-stop-loading']";

#[test]
fn a_data_url_loads_in_electrons_event_order() {
    // Electron 39.8.10: frame-created first, then did-start-loading, did-start-navigation,
    // did-frame-navigate before did-navigate, the title, dom-ready, the finish events.
    let lines = run(&format!(
        r#"
    const win = new BrowserWindow({{ show: false }});
    const wc = win.webContents;
    watch('wc', wc, {LOAD_EVENTS});
    log(await wc.loadURL('data:text/html,' + encodeURIComponent('<title>T1</title><p>one</p>')));
    await sleep(100);
    log(wc.getTitle(), wc.isLoading(), wc.getURL().startsWith('data:text/html'), wc.navigationHistory.length(), wc.navigationHistory.getActiveIndex());
    "#
    ));
    let events: Vec<&str> = lines
        .iter()
        .take_while(|l| l.starts_with("wc "))
        .map(|l| &l[3..])
        .collect();
    assert_eq!(
        events,
        [
            "frame-created",
            "did-start-loading",
            "did-start-navigation",
            "did-frame-navigate",
            "did-navigate",
            "page-title-updated",
            "dom-ready",
            "did-frame-finish-load",
            "did-finish-load",
            "did-stop-loading"
        ]
    );
    assert_eq!(lines[events.len()], "undefined");
    assert_eq!(lines[events.len() + 1], "T1 false true 1 0");
}

#[test]
fn a_file_that_is_not_there_rejects_and_says_it_failed() {
    // Electron 39.8.10: the promise rejects with `ERR_FILE_NOT_FOUND (-6) loading '<url>'` and
    // errno, code and url; did-fail-provisional-load and did-fail-load say (-6, name, url, true);
    // an error page commits, so the finish events follow; the failed address stays the page's.
    let lines = run(&format!(
        r#"
    const win = new BrowserWindow({{ show: false }});
    const wc = win.webContents;
    watch('wc', wc, {LOAD_EVENTS});
    wc.on('did-fail-load', (e, code, name, url, main) => log('fail', code, name, url, main));
    const error = await wc.loadURL('file:///definitely/not/here.html').catch((e) => e);
    log(error.message, error.errno, error.code, error.url, error instanceof Error);
    await sleep(100);
    log(wc.getURL(), wc.navigationHistory.length());
    "#
    ));
    let events: Vec<&str> = lines
        .iter()
        .filter(|l| l.starts_with("wc "))
        .map(|l| &l[3..])
        .collect();
    assert_eq!(
        events,
        [
            "frame-created",
            "did-start-loading",
            "did-start-navigation",
            "did-fail-provisional-load",
            "did-fail-load",
            "dom-ready",
            "did-frame-finish-load",
            "did-finish-load",
            "did-stop-loading"
        ]
    );
    assert!(
        lines.contains(
            &"fail -6 ERR_FILE_NOT_FOUND file:///definitely/not/here.html true".to_owned()
        ),
        "{lines:?}"
    );
    assert!(lines.contains(&"ERR_FILE_NOT_FOUND (-6) loading 'file:///definitely/not/here.html' -6 ERR_FILE_NOT_FOUND file:///definitely/not/here.html true".to_owned()), "{lines:?}");
    assert_eq!(lines.last().unwrap(), "file:///definitely/not/here.html 1");
}

#[test]
fn an_address_that_is_no_url_fails_at_once() {
    // Electron 39.8.10: did-fail-load (-300, ERR_INVALID_URL, '', true) before anything else, with
    // no frame ids, and the rejection `ERR_INVALID_URL (-300) loading ''`.
    let lines = run(r#"
    const win = new BrowserWindow({ show: false });
    const wc = win.webContents;
    wc.on('did-start-loading', () => log('started'));
    wc.on('did-fail-load', (...args) => log('fail', args.length, args.slice(1)));
    const error = await wc.loadURL('not a url').catch((e) => e);
    log(error.message, error.errno, error.code, JSON.stringify(error.url));
    "#);
    assert_eq!(lines[0], "fail 5 [-300,\"ERR_INVALID_URL\",\"\",true]");
    assert_eq!(
        lines[1],
        "ERR_INVALID_URL (-300) loading '' -300 ERR_INVALID_URL \"\""
    );
    assert_eq!(lines.len(), 2);
}

#[test]
fn a_load_that_another_load_replaces_is_aborted() {
    // Electron 39.8.10: the first promise rejects with `ERR_ABORTED (-3)` naming the address of
    // the second load; the second resolves.
    let lines = run(r#"
    const win = new BrowserWindow({ show: false });
    const first = win.loadURL('data:text/html,first').catch((e) => e);
    const second = win.loadURL('data:text/html,second');
    const error = await first;
    log(error.message, error.errno, error.code);
    log(await second);
    "#);
    assert_eq!(
        lines[0],
        "ERR_ABORTED (-3) loading 'data:text/html,second' -3 ERR_ABORTED"
    );
    assert_eq!(lines[1], "undefined");
}

#[test]
fn the_history_moves_with_loads_and_goes_back_and_forward() {
    // Electron 39.8.10: three loads are three entries; goBack/goToIndex/goForward load the entry
    // they name (did-start-navigation, did-navigate, did-finish-load); a load from the middle cuts
    // off what was ahead; reload adds nothing; the current entry cannot be removed and clear keeps
    // it; an entry out of range is null.
    let lines = run(r#"
    const win = new BrowserWindow({ show: false });
    const wc = win.webContents;
    const nav = wc.navigationHistory;
    const page = (n) => 'data:text/html,' + encodeURIComponent('<title>' + n + '</title>' + n);
    const moved = (go) => new Promise((resolve) => { wc.once('did-finish-load', resolve); go(); });
    for (const n of ['1', '2', '3']) await win.loadURL(page(n));
    log('start', [nav.length(), nav.getActiveIndex(), nav.canGoBack(), nav.canGoForward()]);
    const seen = [];
    for (const name of ['did-start-navigation', 'did-navigate', 'did-finish-load']) wc.on(name, () => seen.push(name));
    await moved(() => nav.goBack());
    log('back', [nav.length(), nav.getActiveIndex(), nav.canGoBack(), nav.canGoForward(), wc.getTitle()], seen);
    await moved(() => nav.goToIndex(0));
    log('zero', [nav.getActiveIndex(), wc.getTitle()], [nav.canGoToOffset(2), nav.canGoToOffset(3), nav.canGoToOffset(-1)]);
    await moved(() => nav.goForward());
    log('forward', [nav.getActiveIndex(), wc.getTitle()]);
    await win.loadURL(page('4'));
    log('branch', [nav.length(), nav.getActiveIndex(), nav.canGoForward()]);
    await moved(() => wc.reload());
    log('reload', [nav.length(), nav.getActiveIndex()]);
    log('entry', Object.keys(nav.getEntryAtIndex(1)).sort(), nav.getEntryAtIndex(1).title, nav.getEntryAtIndex(99));
    log('removed', nav.removeEntryAtIndex(2), nav.removeEntryAtIndex(0), [nav.length(), nav.getActiveIndex()]);
    nav.clear();
    log('cleared', [nav.length(), nav.getActiveIndex()]);
    "#);
    assert_eq!(lines[0], "start [3,2,true,false]");
    assert_eq!(
        lines[1],
        "back [3,1,true,true,\"2\"] [\"did-start-navigation\",\"did-navigate\",\"did-finish-load\"]"
    );
    assert_eq!(lines[2], "zero [0,\"1\"] [true,false,false]");
    assert_eq!(lines[3], "forward [1,\"2\"]");
    assert_eq!(lines[4], "branch [3,2,false]");
    assert_eq!(lines[5], "reload [3,2]");
    assert_eq!(lines[6], "entry [\"pageState\",\"title\",\"url\"] 2 null");
    assert_eq!(lines[7], "removed false true [2,1]");
    assert_eq!(lines[8], "cleared [1,0]");
}

#[test]
fn a_hash_change_is_a_navigation_within_the_page() {
    // Electron 39.8.10: did-start-loading, did-start-navigation (isSameDocument), then
    // did-navigate-in-page (url, true), did-stop-loading; the history gains an entry.
    let lines = run_with(
        r#"
    const win = new BrowserWindow({ show: false });
    const wc = win.webContents;
    await win.loadFile('a.html');
    const names = ['did-start-loading', 'did-start-navigation', 'did-navigate', 'did-navigate-in-page', 'did-stop-loading'];
    wc.on('did-navigate-in-page', (e, url, main) => log('in-page', url.split('/').pop(), main));
    wc.on('did-start-navigation', (e, url, inPlace) => log('start', url.split('/').pop(), inPlace, e.isSameDocument));
    watch('wc', wc, names.filter((n) => n !== 'did-navigate-in-page' && n !== 'did-start-navigation'));
    await wc.executeJavaScript("location.hash = '#frag'");
    await sleep(200);
    log(wc.navigationHistory.length(), wc.getURL().split('/').pop());
    "#,
        &[("a.html", "<title>A</title><p>a</p>")],
    );
    assert_eq!(lines[0], "wc did-start-loading");
    assert_eq!(lines[1], "start a.html#frag true true");
    assert_eq!(lines[2], "in-page a.html#frag true");
    assert_eq!(lines[3], "wc did-stop-loading");
    assert_eq!(lines[4], "2 a.html#frag");
}

#[test]
fn will_navigate_hears_a_link_and_can_stop_it_but_load_url_does_not() {
    // Electron 39.8.10: a click says did-start-navigation, will-frame-navigate, will-navigate (the
    // event holds url, isSameDocument, isMainFrame, frame, initiator, processId, routingId; the
    // arguments are url, isInPlace, isMainFrame, processId, routingId); a listener that prevents
    // it keeps the page where it was; loadFile says neither will-event.
    let lines = run_with(
        r#"
    const win = new BrowserWindow({ show: false });
    const wc = win.webContents;
    await win.loadFile('a.html');
    const names = ['did-start-navigation', 'will-frame-navigate', 'will-navigate', 'did-navigate'];
    watch('wc', wc, names);
    wc.on('will-navigate', (event, url, inPlace, main) => {
      log('details', Object.keys(event).sort(), url.split('/').pop(), inPlace, main, event.isSameDocument);
      event.preventDefault();
    });
    await wc.executeJavaScript("document.getElementById('l').click()");
    await sleep(300);
    log('stayed', wc.getURL().split('/').pop());
    wc.removeAllListeners('will-navigate');
    await wc.executeJavaScript("document.getElementById('l').click()");
    await sleep(300);
    log('went', wc.getURL().split('/').pop());
    await win.loadFile('a.html');
    "#,
        &[
            ("a.html", "<a id=\"l\" href=\"b.html\">go</a>"),
            ("b.html", "<title>B</title>b"),
        ],
    );
    let expected = [
        "wc did-start-navigation",
        "wc will-frame-navigate",
        "wc will-navigate",
        "details [\"frame\",\"initiator\",\"isMainFrame\",\"isSameDocument\",\"processId\",\"routingId\",\"url\"] b.html false true false",
        "stayed a.html",
        "wc did-start-navigation",
        "wc will-frame-navigate",
        "wc did-navigate",
        "went b.html",
        "wc did-start-navigation",
        "wc did-navigate",
    ];
    assert_eq!(lines, expected, "the order of will-navigate: {lines:?}");
}

#[test]
fn execute_javascript_answers_values_promises_and_failures() {
    // Electron 39.8.10: a value, an object and a settled promise come back; a throw (an Error, a
    // string, a syntax error) rejects with `Script failed to execute…`; a rejected promise rejects
    // with its reason; a function is `An object could not be cloned.`; a world of its own sees the
    // document and not the page's globals.
    let lines = run_main(
        r#"
    const win = new BrowserWindow({ show: false });
    const wc = win.webContents;
    await load(win, '<p id="p">hi</p>');
    const result = (promise) => promise.then((v) => JSON.stringify(v) ?? 'undefined', (e) => 'rejected: ' + e.message);
    log(await result(wc.executeJavaScript('1 + 2')));
    log(await result(wc.executeJavaScript('({ a: [1, 2], b: "x" })')));
    log(await result(wc.executeJavaScript('new Promise((r) => setTimeout(() => r("later"), 20))')));
    log(await result(wc.executeJavaScript('undefined')));
    log(await result(wc.executeJavaScript('Promise.reject(new Error("no"))')));
    log(await result(wc.executeJavaScript('throw new Error("boom")')));
    log(await result(wc.executeJavaScript('throw "str"')));
    log(await result(wc.executeJavaScript('var =')));
    log(await result(wc.executeJavaScript('(function(){})')));
    log(await result(wc.executeJavaScript(5)));
    log(await result(wc.executeJavaScriptInIsolatedWorld(7, [{ code: 'window.mark = 1; typeof document' }])));
    log(await result(wc.executeJavaScript('typeof window.mark')));
    "#,
    );
    let failed = "rejected: Script failed to execute, this normally means an error was thrown. Check the renderer console for the error.";
    assert_eq!(lines[0], "3");
    assert_eq!(lines[1], "{\"a\":[1,2],\"b\":\"x\"}");
    assert_eq!(lines[2], "\"later\"");
    assert_eq!(lines[3], "undefined");
    assert_eq!(lines[4], "rejected: no");
    assert_eq!(&lines[5..8], [failed, failed, failed]);
    assert_eq!(lines[8], "rejected: An object could not be cloned.");
    assert_eq!(lines[9], "5");
    assert_eq!(lines[10], "\"object\"");
    assert_eq!(lines[11], "\"undefined\"");
}

#[test]
fn inserted_css_changes_the_computed_style_until_it_is_removed() {
    // Electron 39.8.10: insertCSS answers a string key; the style is red, then black again.
    let lines = run(r#"
    const win = new BrowserWindow({ show: false });
    const wc = win.webContents;
    await load(win, '<p id="p">hi</p>');
    const color = () => wc.executeJavaScript('getComputedStyle(document.getElementById("p")).color');
    const key = await wc.insertCSS('#p { color: red }');
    log(typeof key, await color());
    await wc.removeInsertedCSS(key);
    log(await color());
    log(typeof await wc.insertCSS('#p { color: blue }', { cssOrigin: 'user' }));
    "#);
    assert_eq!(lines, ["string rgb(255, 0, 0)", "rgb(0, 0, 0)", "string"]);
}

#[test]
fn input_events_click_and_type_in_the_page() {
    // sendInputEvent reaches the page as a person's input would: a press and a release click the
    // element under the pointer, a right button asks for a context menu (with the link, the
    // editable field and the selected text), insertText types into the focused field.
    let lines = run(r#"
    const win = new BrowserWindow({ show: false });
    const wc = win.webContents;
    await load(win, '<button id="b" style="display:block">press</button><a id="a" style="display:block" href="https://example.com/x">link text</a><div id="e" contenteditable style="display:block">edit</div><input id="i">');
    await wc.executeJavaScript("window.clicks = 0; document.getElementById('b').addEventListener('click', () => window.clicks++)");
    wc.sendInputEvent({ type: 'mouseDown', x: 5, y: 5, button: 'left', clickCount: 1 });
    wc.sendInputEvent({ type: 'mouseUp', x: 5, y: 5, button: 'left', clickCount: 1 });
    log('clicks', await wc.executeJavaScript('window.clicks'));
    wc.on('context-menu', (event, params) => log('menu', params.linkURL, params.linkText, params.isEditable, params.mediaType, params.pageURL.startsWith('data:'), params.x, params.y));
    wc.sendInputEvent({ type: 'contextMenu', x: 5, y: 21 });
    await sleep(100);
    wc.sendInputEvent({ type: 'contextMenu', x: 5, y: 37 });
    await sleep(100);
    await wc.executeJavaScript('document.getElementById("i").focus()');
    await wc.insertText('abc');
    log('typed', await wc.executeJavaScript('document.getElementById("i").value'));
    "#);
    assert_eq!(lines[0], "clicks 1");
    assert_eq!(
        lines[1],
        "menu https://example.com/x link text false none true 5 21"
    );
    assert_eq!(lines[2], "menu   true none true 5 37");
    assert_eq!(lines[3], "typed abc");
}

#[test]
fn window_open_follows_the_handler_and_makes_a_child_window() {
    // Electron 39.8.10: with no handler the window opens and the opener says did-create-window; a
    // handler is given { url, frameName, features, disposition, referrer }; `deny` leaves the page
    // `null` and no window; `allow` with `overrideBrowserWindowOptions` wins over the features
    // (width 321 over 150, height 250 from the features); the new page's `opener` is the opener's
    // frame; a link with a target is `foreground-tab`, a call with features `new-window`.
    let lines = run_with(
        r#"
    const win = new BrowserWindow({ show: false });
    const wc = win.webContents;
    await win.loadFile('o.html');
    const url = new URL('c.html', wc.getURL()).href;
    wc.on('did-create-window', (w, details) => log('created', w.constructor.name, Object.keys(details).sort(), details.frameName, details.disposition));
    await wc.executeJavaScript(`window.open('${url}', 'named', 'width=300,height=200'), 0`);
    log('plain', BrowserWindow.getAllWindows().length);
    for (const w of BrowserWindow.getAllWindows()) if (w !== win) w.destroy();
    const seen = [];
    wc.setWindowOpenHandler((d) => { seen.push(d); return { action: 'deny' }; });
    log('deny', await wc.executeJavaScript(`window.open('${url}', 'nm', 'width=111,popup=1') === null`));
    log('details', Object.keys(seen[0]).sort(), seen[0].frameName, seen[0].features, seen[0].disposition, seen[0].referrer);
    log('windows', BrowserWindow.getAllWindows().length);
    wc.setWindowOpenHandler((d) => { seen.push(d.disposition); return { action: 'allow', overrideBrowserWindowOptions: { width: 321, show: false } }; });
    await wc.executeJavaScript(`window.open('${url}', 'nm2', 'width=150,height=250,left=10,top=20'), 0`);
    const child = BrowserWindow.getAllWindows().find((w) => w !== win);
    log('child', child.getSize(), child.webContents.getType(), !!child.webContents.opener, child.getParentWindow());
    await wc.executeJavaScript(`document.body.innerHTML = '<a id="b" target="_blank" href="${url}">x</a>'; document.getElementById('b').click()`);
    log('link', seen[seen.length - 1], BrowserWindow.getAllWindows().length);
    wc.setWindowOpenHandler(() => 'nonsense');
    log('bad', await wc.executeJavaScript(`window.open('${url}') === null`));
    win.destroy();
    for (let i = 0; i < 100 && BrowserWindow.getAllWindows().length > 0; i++) await sleep(20);
    log('after', BrowserWindow.getAllWindows().length);
    "#,
        FILES,
    );
    // The opener hears did-create-window before the script that opened the window has its result,
    // and a handler answer that is neither action is said once on the main process's console.
    let created = "created BrowserWindow [\"disposition\",\"frameName\",\"options\",\"postData\",\"referrer\",\"url\"]";
    assert!(
        lines[0].starts_with(&format!("{created} named new-window")),
        "{lines:?}"
    );
    assert_eq!(lines[1], "plain 2");
    assert_eq!(lines[2], "deny true");
    assert_eq!(
        lines[3],
        "details [\"disposition\",\"features\",\"frameName\",\"referrer\",\"url\"] nm width=111,popup=1 new-window {\"url\":\"\",\"policy\":\"strict-origin-when-cross-origin\"}"
    );
    assert_eq!(lines[4], "windows 1");
    assert!(
        lines[5].starts_with(&format!("{created} nm2 new-window")),
        "{lines:?}"
    );
    assert_eq!(lines[6], "child [321,250] window true null");
    assert!(
        lines[7].starts_with(&format!("{created}  foreground-tab")),
        "{lines:?}"
    );
    assert_eq!(lines[8], "link foreground-tab 3");
    assert_eq!(
        lines[9],
        "The window open handler response must be an object with an 'action' property of 'allow' or 'deny'."
    );
    assert_eq!(lines[10], "bad true");
    assert_eq!(lines[11], "after 0");
    assert_eq!(lines.len(), 12, "{lines:?}");
}

#[test]
fn before_input_event_can_keep_a_key_from_the_page() {
    // While a `before-input-event` listener exists the Hub says the shell is to hand the page's keys
    // to the main process (`intercepts_keys`); the listener gets { type, key, code, … }, and a key it
    // prevents never reaches the page.
    let app = App::launch_script(
        r#"
    const { app, BrowserWindow } = require('electron');
    global.seen = [];
    app.whenReady().then(async () => {
      const win = new BrowserWindow({ show: false });
      win.webContents.on('before-input-event', (event, input) => {
        global.seen.push([input.type, input.key, input.code, input.shift]);
        if (input.key === 'a') event.preventDefault();
      });
      await win.loadURL('data:text/html,' + encodeURIComponent('<script>window.keys = []; addEventListener("keydown", (e) => window.keys.push(e.key))</script>'));
      console.log('READY');
    });"#,
        &[],
    );
    assert!(
        app.run_until(|app| app.console().iter().any(|l| l == "READY"), TIMEOUT),
        "{:?}",
        app.console()
    );
    assert!(app.hub.intercepts_keys(1));
    for key in ['a', 'b'] {
        app.hub.from_shell(FromShell::KeyForContents {
            contents: 1,
            down: true,
            key: Key::char(key),
        });
    }
    assert!(app.run_until(
        |app| app.eval_renderer(1, "window.keys") == "[\"b\"]",
        TIMEOUT
    ));
    assert_eq!(
        app.eval_main("global.seen"),
        "[[\"keyDown\",\"a\",\"KeyA\",false],[\"keyDown\",\"b\",\"KeyB\",false]]"
    );
}

#[test]
fn a_window_does_not_inherit_permissions_its_opener_has() {
    // Electron's makeWebPreferences copies a security setting from the opener only when it has its
    // secure value: an opener with Node and no isolation opens a window with neither, and the
    // opener's preload does not follow.
    let lines = run_with(
        r#"
    const win = new BrowserWindow({ show: false, webPreferences: { nodeIntegration: true, contextIsolation: false, sandbox: false } });
    const wc = win.webContents;
    await win.loadFile('o.html');
    log('opener', await wc.executeJavaScript('typeof require'));
    wc.setWindowOpenHandler(() => ({ action: 'allow', overrideBrowserWindowOptions: { show: false } }));
    const url = new URL('c.html', wc.getURL()).href;
    await wc.executeJavaScript(`window.open('${url}'), 0`);
    await sleep(300);
    const child = BrowserWindow.getAllWindows().find((w) => w !== win);
    log('child', await child.webContents.executeJavaScript('typeof require'), await child.webContents.executeJavaScript('typeof process'));
    "#,
        FILES,
    );
    assert_eq!(lines, ["opener function", "child undefined undefined"]);
}

#[test]
fn promises_settle_when_the_page_or_the_window_goes_away() {
    // A script asked of a page whose document is replaced fails, so does one asked of a page that
    // is gone, and a loadURL still waiting when its window is destroyed fails as ERR_FAILED (-2).
    let lines = run(r#"
    const win = new BrowserWindow({ show: false });
    const wc = win.webContents;
    await load(win, '<p>a</p>');
    const stuck = wc.executeJavaScript('new Promise(() => {})').catch((e) => e.message.slice(0, 20));
    await win.loadURL('data:text/html,second');
    log('replaced', await stuck);
    const other = new BrowserWindow({ show: false });
    const pending = other.loadURL('data:text/html,never').catch((e) => e.code);
    const script = other.webContents.executeJavaScript('1').catch((e) => e.message.slice(0, 20));
    other.destroy();
    log('destroyed', await pending, await script);
    "#);
    assert_eq!(lines[0], "replaced Script failed to exe");
    assert_eq!(lines[1], "destroyed ERR_FAILED Script failed to exe");
}

#[test]
fn history_entries_outside_the_list_are_none() {
    // Electron answers null and false for an index below 0 or not a number, never entry 0.
    let lines = run(r#"
    const win = new BrowserWindow({ show: false });
    const nav = win.webContents.navigationHistory;
    await load(win, '<p>a</p>');
    await win.loadURL('data:text/html,b');
    log(nav.getEntryAtIndex(-1), nav.getEntryAtIndex(NaN), nav.removeEntryAtIndex(-1), nav.removeEntryAtIndex(NaN), nav.length());
    "#);
    assert_eq!(lines, ["null null false false 2"]);
}

#[test]
fn a_page_cannot_open_a_window_on_what_it_may_not_load() {
    // Chromium calls the handler and then refuses: a remote (here: data:) page gets no `file:`
    // window, and no page gets a top-level `data:` window.
    let lines = run_with(
        r#"
    const win = new BrowserWindow({ show: false });
    const wc = win.webContents;
    let asked = 0;
    wc.setWindowOpenHandler(() => { asked++; return { action: 'allow' }; });
    await load(win, '<p>x</p>');
    await wc.executeJavaScript("window.open('file:///C:/Windows/win.ini'), 0");

    log('from data', asked, BrowserWindow.getAllWindows().length);
    await win.loadFile('o.html');
    await wc.executeJavaScript("window.open('data:text/html,x'), 0");

    log('from file', asked, BrowserWindow.getAllWindows().length);
    "#,
        FILES,
    );
    let lines: Vec<_> = lines
        .into_iter()
        .filter(|l| !l.starts_with("Not allowed"))
        .collect();
    assert_eq!(lines, ["from data 1 1", "from file 2 1"]);
}

#[test]
fn a_script_asked_of_a_load_that_is_replaced_before_it_commits_fails() {
    // loadURL(a); executeJavaScript(x); loadURL(b): the page's isolate is replaced, whether or not
    // `a` has committed, and what x was asked fails rather than waiting for ever.
    let lines = run(r#"
    const win = new BrowserWindow({ show: false });
    const wc = win.webContents;
    win.loadURL('data:text/html,a').catch(() => {});
    const script = wc.executeJavaScript('1').catch((e) => e.message.slice(0, 20));
    win.loadURL('data:text/html,b').catch(() => {});
    log('script', await script);
    "#);
    assert_eq!(lines, ["script Script failed to exe"]);
}

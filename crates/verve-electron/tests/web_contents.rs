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

use support::run;

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

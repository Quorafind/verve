//! `BrowserWindow`, on the headless shell: its options and defaults, its getters and setters,
//! its events and the round trip that closes it. The expected values were observed on Electron
//! 39.8.10 (Windows) by `tests/electron/browser-window/observe`, and are frozen here; a test says
//! so when its number comes from Verve's own rules instead.

mod support;

use support::{App, TIMEOUT, run};

#[test]
fn a_window_has_electrons_defaults() {
    // Electron 39.8.10: `getSize()` [800, 600], `getContentSize()` [784, 561] with a frame.
    let lines = run(r#"
    const win = new BrowserWindow({ show: false });
    log(win.getSize(), win.getContentSize(), win.getMinimumSize(), win.getMaximumSize());
    log(win.getTitle(), win.isVisible(), win.isFocused(), win.isNormal(), win.getOpacity(), win.getBackgroundColor());
    log([win.isResizable(), win.isMovable(), win.isMinimizable(), win.isMaximizable(), win.isFullScreenable(), win.isClosable(), win.isFocusable(), win.isEnabled(), win.isMenuBarVisible()]);
    log([win.isAlwaysOnTop(), win.isFullScreen(), win.isKiosk(), win.isModal(), win.isSnapped(), win.isTabletMode(), win.isDocumentEdited(), win.isMenuBarAutoHide(), win.isContentProtected(), win.isVisibleOnAllWorkspaces()]);
    log(win.getParentWindow(), win.getChildWindows(), win.getRepresentedFilename(), win.getNativeWindowHandle().length, win.id, win.webContents.id);
    "#);
    assert_eq!(lines[0], "[800,600] [784,561] [0,0] [0,0]");
    assert_eq!(lines[1], "Electron false false true 1 #FFFFFF");
    assert_eq!(lines[2], "[true,true,true,true,true,true,true,true,true]");
    assert_eq!(
        lines[3],
        "[false,false,false,false,false,false,false,false,false,false]"
    );
    assert_eq!(lines[4], "null []  8 1 1");
}

#[test]
fn sizes_and_places_are_whole_numbers_and_never_below_the_systems_smallest() {
    // Electron 39.8.10: a string, a fraction and `null` are no width; -5 and 0 are the smallest
    // a window is (136 wide, 39 high); `x` without `y` is no position; `center` yields to both.
    let lines = run(r#"
    const size = (o) => { const w = new BrowserWindow({ show: false, ...o }); const s = w.getSize(); w.destroy(); return s; };
    log(size({ width: '300' }), size({ width: 'abc' }), size({ width: 300.7 }), size({ width: null }));
    log(size({ width: -5 }), size({ width: 0 }), size({ width: 300, height: 0 }));
    const at = (o) => { const w = new BrowserWindow({ show: false, ...o }); const p = w.getPosition(); w.destroy(); return p; };
    log(at({ x: 10, y: 20 }), at({ center: true, x: 5, y: 5 }), at({ x: '10', y: '20' }), at({ x: 10.5, y: 20.5 }));
    const centred = at({});
    log(at({ x: 7 }).join() === centred.join());
    "#);
    assert_eq!(lines[0], "[800,600] [800,600] [800,600] [800,600]");
    assert_eq!(lines[1], "[136,600] [136,600] [300,39]");
    assert_eq!(lines[2], "[10,20] [5,5] [560,240] [560,240]");
    assert_eq!(lines[3], "true");
}

#[test]
fn the_content_size_option_and_a_missing_frame_change_what_the_size_means() {
    // Electron 39.8.10: a frameless window's content is its bounds; `useContentSize` makes
    // width and height the content's (Verve's frame is 16 x 39 around it).
    let lines = run(r#"
    const info = (o) => { const w = new BrowserWindow({ show: false, ...o }); const r = [w.getSize(), w.getContentSize()]; w.destroy(); return r; };
    log(info({ frame: false, width: 300, height: 200 }));
    log(info({ useContentSize: true, width: 300, height: 200 }));
    "#);
    assert_eq!(lines[0], "[[300,200],[300,200]]");
    assert_eq!(lines[1], "[[316,239],[300,200]]");
}

#[test]
fn flags_given_as_options_are_what_the_getters_answer() {
    // Electron 39.8.10: a transparent window cannot be resized; `fullscreen` and `kiosk` are the
    // same thing on Windows; `minWidth` is a limit that does not resize the window.
    let lines = run(r#"
    const flags = (o) => { const w = new BrowserWindow({ show: false, ...o }); const r = [w.isResizable(), w.isFullScreen(), w.isKiosk(), w.isAlwaysOnTop(), w.isClosable(), w.isFocusable(), w.getOpacity(), w.getSize()]; w.destroy(); return r; };
    log(flags({ transparent: true }));
    log(flags({ fullscreen: true }));
    log(flags({ kiosk: true }));
    log(flags({ alwaysOnTop: true, closable: false, focusable: false, opacity: 0.5, resizable: false }));
    const limits = new BrowserWindow({ show: false, width: 200, height: 100, minWidth: 300, minHeight: 150, maxWidth: 400, maxHeight: 100 });
    log(limits.getSize(), limits.getMinimumSize(), limits.getMaximumSize());
    "#);
    assert_eq!(lines[0], "[false,false,false,false,true,true,1,[800,600]]");
    assert_eq!(lines[1], "[true,true,true,false,true,true,1,[1920,1080]]");
    assert_eq!(lines[2], "[true,true,true,false,true,true,1,[1920,1080]]");
    assert_eq!(
        lines[3],
        "[false,false,false,true,false,false,0.5,[800,600]]"
    );
    assert_eq!(lines[4], "[200,100] [300,150] [400,100]");
}

#[test]
fn options_of_the_wrong_type_are_left_at_their_defaults_and_throw_nothing() {
    // Electron 39.8.10: none of these throws; `show: 'no'` is a truthy string.
    let lines = run(r#"
    const tries = [
      () => new BrowserWindow(5), () => new BrowserWindow('x'), () => new BrowserWindow({ show: false, backgroundColor: 'notacolor' }),
      () => new BrowserWindow({ show: false, backgroundColor: 5 }), () => new BrowserWindow({ show: false, webPreferences: 'x' }),
      () => new BrowserWindow({ show: false, parent: {} }), () => new BrowserWindow({ show: false, title: 5 }),
    ];
    for (const make of tries) { try { make().destroy(); log('ok'); } catch (e) { log('threw', e.message); } }
    const modal = new BrowserWindow({ show: false, modal: true });
    log(modal.isModal());
    try { BrowserWindow(); } catch (e) { log(e.name, e.message); }
    "#);
    assert_eq!(&lines[..7], ["ok"; 7]);
    assert_eq!(lines[7], "false");
    assert_eq!(lines[8], "Error Requires constructor call");
}

#[test]
fn background_colours_are_read_as_electron_reads_them() {
    // Electron 39.8.10: `getBackgroundColor()` is `#RRGGBB`, upper case, alpha dropped, and
    // `#ARGB`/`#AARRGGBB` put the alpha first; an unreadable colour is white, silently.
    let lines = run(r#"
    const colours = [undefined, '#FFF', '#abc', '#aabbcc', '#80aabbcc', '#8abc', 'rgb(1,2,3)', 'rgba(1,2,3,0.5)', 'hsl(120,100%,50%)', 'hsla(120,100%,50%,0.5)', 'red', 'RED', 'transparent'];
    const out = [];
    for (const c of colours) { const w = new BrowserWindow({ show: false, backgroundColor: c }); out.push(w.getBackgroundColor()); w.destroy(); }
    log(out);
    const w = new BrowserWindow({ show: false });
    const set = [];
    for (const c of ['#fff', 'rgba(0,0,0,0.5)', 'blue', '#12345678', 'bad']) { w.setBackgroundColor(c); set.push(w.getBackgroundColor()); }
    log(set);
    "#);
    assert_eq!(
        lines[0],
        r##"["#FFFFFF","#FFFFFF","#AABBCC","#AABBCC","#AABBCC","#AABBCC","#010203","#010203","#00FF00","#00FF00","#FF0000","#FFFFFF","#FFFFFF"]"##
    );
    assert_eq!(
        lines[1],
        r##"["#FFFFFF","#000000","#0000FF","#345678","#FFFFFF"]"##
    );
}

#[test]
fn window_and_contents_ids_count_apart_from_one_and_are_not_reused() {
    let lines = run(r#"
    const a = new BrowserWindow({ show: false });
    const b = new BrowserWindow({ show: false });
    log(a.id, a.webContents.id, b.id, b.webContents.id);
    b.destroy();
    const c = new BrowserWindow({ show: false });
    log(c.id, c.webContents.id);
    log(BrowserWindow.getAllWindows().map((w) => w.id));
    log(BrowserWindow.fromId(a.id) === a, BrowserWindow.fromId(b.id), BrowserWindow.fromId(99));
    log(BrowserWindow.fromWebContents(a.webContents) === a, BrowserWindow.fromBrowserView({}));
    log(BrowserWindow.getFocusedWindow());
    "#);
    assert_eq!(lines[0], "1 1 2 2");
    assert_eq!(lines[1], "3 3");
    // Electron 39.8.10 lists the newest window first.
    assert_eq!(lines[2], "[3,1]");
    assert_eq!(lines[3], "true null null");
    assert_eq!(lines[4], "true null");
    assert_eq!(lines[5], "null");
}

#[test]
fn a_destroyed_window_throws_object_has_been_destroyed_but_for_a_few_members() {
    // Electron 39.8.10: `id` and `isDestroyed()` keep answering, `destroy()` again is quiet, the
    // window is `closed` and then its contents `destroyed`, and no `close` is heard.
    let lines = run(r#"
    const win = new BrowserWindow({ show: false });
    const contents = win.webContents;
    watch('win', win, ['close', 'closed']);
    watch('contents', contents, ['destroyed']);
    log('alive', win.isDestroyed(), contents.isDestroyed());
    win.destroy();
    log('isDestroyed', win.isDestroyed(), BrowserWindow.getAllWindows().length, BrowserWindow.fromId(win.id), contents.isDestroyed());
    await sleep(100);
    const tried = {};
    for (const name of ['getTitle', 'getSize', 'show', 'close', 'getParentWindow']) { try { win[name](); tried[name] = 'ok'; } catch (e) { tried[name] = e.name + ': ' + e.message; } }
    try { win.webContents; } catch (e) { tried.webContents = e.message; }
    try { win.setTitle('x'); } catch (e) { tried.setTitle = e.message; }
    log(tried);
    log(win.id, win.isDestroyed(), win.destroy());
    try { BrowserWindow.fromWebContents(contents); } catch (e) { log('fromWebContents', e.message); }
    "#);
    assert_eq!(lines[0], "alive false false");
    assert_eq!(lines[1], "isDestroyed true 0 null true");
    assert_eq!(&lines[2..4], ["win closed", "contents destroyed"]);
    let destroyed = "TypeError: Object has been destroyed";
    assert_eq!(
        lines[4],
        format!(
            r#"{{"getTitle":"{destroyed}","getSize":"{destroyed}","show":"{destroyed}","close":"{destroyed}","getParentWindow":"{destroyed}","webContents":"Object has been destroyed","setTitle":"Object has been destroyed"}}"#
        )
    );
    assert_eq!(lines[5], "1 true undefined");
    assert_eq!(lines[6], "fromWebContents Object has been destroyed");
}

#[test]
fn native_methods_read_their_arguments_as_electrons_do() {
    // Electron 39.8.10's messages (gin): the last bad argument is the one named.
    let lines = run(r#"
    const win = new BrowserWindow({ show: false });
    const tries = {
      opacity: () => win.setOpacity('a'), title: () => win.setTitle(5), size: () => win.setSize('a', 'b'),
      one: () => win.setSize(300), position: () => win.setPosition('a', 1), url: () => win.loadURL(),
      opaque: () => { win.setOpacity(2); return win.getOpacity(); }, bounds: () => win.setBounds('x'), none: () => win.setBounds(),
      icon: () => win.setIcon('nonexistent.png'),
    };
    for (const [name, run] of Object.entries(tries)) { try { log(name, 'ok', run()); } catch (e) { log(name, e.name, e.message); } }
    "#);
    assert_eq!(
        lines[0],
        "opacity TypeError Error processing argument at index 0, conversion failure from a"
    );
    assert_eq!(
        lines[1],
        "title TypeError Error processing argument at index 0, conversion failure from "
    );
    assert_eq!(
        lines[2],
        "size TypeError Error processing argument at index 1, conversion failure from b"
    );
    assert_eq!(lines[3], "one TypeError Insufficient number of arguments.");
    assert_eq!(
        lines[4],
        "position TypeError Error processing argument at index 0, conversion failure from a"
    );
    assert_eq!(
        lines[5],
        "url TypeError Error processing argument at index 0, conversion failure from undefined"
    );
    assert_eq!(lines[6], "opaque ok 1");
    assert_eq!(lines[7], "bounds ok undefined");
    assert_eq!(lines[8], "none ok undefined");
    assert_eq!(
        lines[9],
        "icon Error Failed to load image from path 'nonexistent.png'"
    );
}

#[test]
fn a_setter_changes_what_the_getter_answers_at_once() {
    // Electron 39.8.10: `getBounds()` after `setBounds()`, `isMaximized()` after `maximize()`,
    // a minimized window is not visible, `setMinimumSize` does not resize the window.
    let lines = run(r#"
    const win = new BrowserWindow({ show: false });
    win.setBounds({ x: 50, y: 60, width: 500, height: 400 });
    log(win.getBounds(), win.getSize(), win.getPosition());
    win.setSize(480, 360);
    log(win.getSize());
    win.setBounds({ width: 450 });
    log(win.getBounds());
    win.setContentSize(300, 200);
    log(win.getSize(), win.getContentSize());
    win.setMinimumSize(600, 500);
    log(win.getSize(), win.getMinimumSize());
    win.setMinimumSize(0, 0);
    win.maximize();
    log(win.isMaximized(), win.isNormal(), win.getNormalBounds());
    win.unmaximize();
    log(win.isMaximized(), win.isNormal());
    win.show(); win.minimize();
    log(win.isMinimized(), win.isVisible(), win.isFocused());
    win.restore();
    log(win.isMinimized(), win.isVisible());
    win.showInactive(); win.hide();
    log(win.isVisible(), win.isFocused());
    win.setFullScreen(true);
    log(win.isFullScreen(), win.fullScreen);
    win.setFullScreen(false);
    log(win.isFullScreen());
    win.setOpacity(0.5); win.setTitle('T');
    log(win.getOpacity(), win.getTitle(), win.title);
    "#);
    assert_eq!(
        lines[0],
        r#"{"x":50,"y":60,"width":500,"height":400} [500,400] [50,60]"#
    );
    assert_eq!(lines[1], "[480,360]");
    assert_eq!(lines[2], r#"{"x":50,"y":60,"width":450,"height":360}"#);
    assert_eq!(lines[3], "[316,239] [300,200]");
    assert_eq!(lines[4], "[316,239] [600,500]");
    assert!(lines[5].starts_with("true false {\"x\":"), "{}", lines[5]);
    assert_eq!(lines[6], "false true");
    assert_eq!(lines[7], "true false false");
    assert_eq!(lines[8], "false true");
    assert_eq!(lines[9], "false false");
    assert_eq!(lines[10], "true true");
    assert_eq!(lines[11], "false");
    assert_eq!(lines[12], "0.5 T T");
}

#[test]
fn flags_have_a_setter_a_getter_and_a_property() {
    let lines = run(r#"
    const win = new BrowserWindow({ show: false });
    win.setResizable(false); win.setMovable(false); win.setMinimizable(false); win.setMaximizable(false);
    win.setFullScreenable(false); win.setClosable(false); win.setFocusable(false); win.setAlwaysOnTop(true, 'floating');
    log([win.isResizable(), win.isMovable(), win.isMinimizable(), win.isMaximizable(), win.isFullScreenable(), win.isClosable(), win.isFocusable(), win.isAlwaysOnTop()]);
    log([win.resizable, win.movable, win.minimizable, win.maximizable, win.fullScreenable, win.closable, win.focusable]);
    win.resizable = true; win.closable = true;
    log(win.isResizable(), win.isClosable());
    win.setContentProtection(true); win.setMenuBarVisibility(false); win.setAutoHideMenuBar(true); win.setHasShadow(true);
    log(win.isContentProtected(), win.isMenuBarVisible(), win.menuBarVisible, win.isMenuBarAutoHide(), win.hasShadow(), win.shadow);
    win.setResizable('yes');
    log(win.isResizable());
    win.setFullScreenable(false);
    win.setFullScreen(true);
    log(win.isFullScreen());
    "#);
    assert_eq!(lines[0], "[false,false,false,false,false,false,false,true]");
    assert_eq!(lines[1], "[false,false,false,false,false,false,false]");
    assert_eq!(lines[2], "true true");
    assert_eq!(lines[3], "true false false true true true");
    assert_eq!(lines[4], "true");
    // Electron 39.8.10: a window that is not full-screenable does not go full screen.
    assert_eq!(lines[5], "false");
}

#[test]
fn macos_members_do_nothing_on_other_systems() {
    // Electron 39.8.10 on Windows: the document-edited mark and the represented file are not kept.
    let lines = run(r#"
    const win = new BrowserWindow({ show: false });
    win.setDocumentEdited(true); win.setRepresentedFilename('/x');
    log(win.isDocumentEdited(), win.getRepresentedFilename(), win.documentEdited, win.representedFilename, win.snapped, win.isSnapped());
    log(win.setThumbarButtons([]));
    "#);
    if cfg!(target_os = "macos") {
        return;
    }
    assert_eq!(lines[0], "false  false  false false");
    assert_eq!(lines[1], "true");
}

#[test]
fn class_extends_and_util_inherits_both_make_a_subclass() {
    // Electron 39.8.10: `class extends` works; a function-style subclass inherits but cannot be
    // constructed, because the native class refuses `BrowserWindow.call(this)`.
    let lines = run(r#"
    class Mine extends BrowserWindow { constructor() { super({ show: false, title: 'mine' }); } }
    const mine = new Mine();
    log(mine instanceof BrowserWindow, mine.getTitle(), mine.id);
    mine.on('hide', () => log('hide heard'));
    mine.show(); mine.hide();
    await sleep(100);
    function Old() { BrowserWindow.call(this, { show: false }); }
    require('util').inherits(Old, BrowserWindow);
    log(Old.prototype instanceof BrowserWindow);
    try { new Old(); } catch (e) { log(e.name, e.message); }
    "#);
    assert_eq!(lines[0], "true mine 1");
    assert_eq!(lines[1], "hide heard");
    assert_eq!(lines[2], "true");
    assert_eq!(lines[3], "Error Requires constructor call");
}

#[test]
fn creating_a_window_is_heard_first_as_its_contents_then_as_the_window() {
    // Electron 39.8.10: `web-contents-created` then `browser-window-created`, each with an
    // event that can be prevented.
    let lines = run(r#"
    app.on('web-contents-created', (event, contents) => log('contents', typeof event.preventDefault, contents.id));
    app.on('browser-window-created', (event, win) => log('window', typeof event.preventDefault, win instanceof BrowserWindow, !!win.webContents));
    new BrowserWindow({ show: false });
    "#);
    assert_eq!(lines, ["contents function 1", "window function true true"]);
}

#[test]
fn events_carry_an_event_except_ready_to_show() {
    // Electron 39.8.10: every listener of a window, and of its contents' `destroyed`, is given an
    // event; `ready-to-show` is given nothing.
    let lines = run(r#"
    const win = new BrowserWindow({ show: false });
    for (const name of ['show', 'closed', 'ready-to-show']) win.on(name, (...args) => log(name, args.length, args[0] && typeof args[0].preventDefault));
    win.webContents.on('destroyed', (...args) => log('destroyed', args.length));
    await load(win, '<p>x</p>');
    win.show();
    await sleep(100);
    win.destroy();
    await sleep(100);
    "#);
    assert_eq!(
        lines,
        [
            "ready-to-show 0 undefined",
            "show 1 function",
            "closed 1 function",
            "destroyed 1"
        ]
    );
}

#[test]
fn reload_loads_the_committed_address_again_and_capture_page_is_not_supported() {
    let lines = run(r#"
    const win = new BrowserWindow({ show: false });
    let loads = 0;
    win.webContents.on('did-finish-load', () => loads++);
    await load(win, '<p>a</p>');
    win.reload();
    await sleep(300);
    log('loads', loads);
    await win.capturePage().catch((e) => log(e.code, e.message));
    "#);
    assert_eq!(lines[0], "loads 2");
    assert_eq!(
        lines[1],
        "ERR_NOT_SUPPORTED capturePage is not supported by Verve"
    );
}

#[test]
fn the_shells_events_become_the_windows_in_electrons_order() {
    // Electron 39.8.10: setBounds is `resize`, `move`; `always-on-top-changed` only on a change
    // and with `(event, flag)`; showing is `show`, the app's `browser-window-focus`, `focus`.
    let lines = run(r#"
    const win = new BrowserWindow({ show: false });
    app.on('browser-window-focus', (event, who) => log('app browser-window-focus', typeof event.preventDefault, who === win));
    app.on('browser-window-blur', (event, who) => log('app browser-window-blur', who === win));
    watch('win', win, ['resize', 'move', 'maximize', 'unmaximize', 'show', 'hide', 'focus', 'blur', 'minimize', 'restore', 'enter-full-screen', 'leave-full-screen', 'resized', 'moved']);
    win.on('always-on-top-changed', (event, flag) => log('always-on-top-changed', typeof event.preventDefault, flag));
    win.setBounds({ x: 50, y: 60, width: 500, height: 400 });
    await sleep(100); log('--');
    win.setSize(450, 350);
    await sleep(100); log('--');
    win.setAlwaysOnTop(true); win.setAlwaysOnTop(true); win.setAlwaysOnTop(false);
    await sleep(100); log('--');
    win.show();
    await sleep(100); log('--', BrowserWindow.getFocusedWindow() === win);
    win.hide();
    await sleep(100); log('--', BrowserWindow.getFocusedWindow());
    "#);
    assert_eq!(
        lines,
        [
            "win resize",
            "win move",
            "--",
            "win resize",
            "--",
            "always-on-top-changed function true",
            "always-on-top-changed function false",
            "--",
            "win show",
            "app browser-window-focus function true",
            "win focus",
            "-- true",
            "win hide",
            "app browser-window-blur true",
            "win blur",
            "-- null",
        ]
    );
}

#[test]
fn the_pages_title_is_the_windows_unless_the_windows_listener_prevents_it() {
    // Electron 39.8.10: the window hears `page-title-updated` before the contents does, whichever
    // listener was added first; preventing it on the contents does not keep the title.
    let lines = run(r#"
    for (const mode of ['plain', 'prevent-on-contents', 'prevent-on-window']) {
      const win = new BrowserWindow({ show: false, title: 'initial' });
      win.webContents.on('page-title-updated', (event, title, explicit) => { log(mode, 'contents', title, explicit, typeof event.preventDefault); if (mode === 'prevent-on-contents') event.preventDefault(); });
      win.on('page-title-updated', (event, title, explicit) => { log(mode, 'win', title, explicit); if (mode === 'prevent-on-window') event.preventDefault(); });
      await load(win, '<title>from page</title><p>x</p>');
      await sleep(50);
      log(mode, win.getTitle());
      win.destroy();
    }
    const win = new BrowserWindow({ show: false, title: 'initial' });
    await load(win, '<p>no title</p>');
    log('no title', win.getTitle());
    "#);
    assert_eq!(
        lines,
        [
            "plain win from page true",
            "plain contents from page true function",
            "plain from page",
            "prevent-on-contents win from page true",
            "prevent-on-contents contents from page true function",
            "prevent-on-contents from page",
            "prevent-on-window win from page true",
            "prevent-on-window contents from page true function",
            "prevent-on-window initial",
            "no title initial",
        ]
    );
}

#[test]
fn ready_to_show_is_said_once_after_the_page_stopped_loading() {
    // Electron 39.8.10: a hidden window says it after `did-stop-loading`; a shown one too; with
    // `paintWhenInitiallyHidden: false` it waits for the window to be shown.
    let lines = run(r#"
    for (const [name, opts] of [['hidden', { show: false }], ['shown', { show: true, width: 200, height: 150 }], ['no-paint', { show: false, paintWhenInitiallyHidden: false }]]) {
      const win = new BrowserWindow(opts);
      watch(name, win, ['ready-to-show', 'show', 'focus']);
      watch(name, win.webContents, ['dom-ready', 'did-finish-load', 'did-stop-loading']);
      await load(win, '<p>x</p>');
      await sleep(200);
      if (!opts.show) { log(name, '-- shown'); win.show(); await sleep(200); }
      win.destroy();
    }
    const never = new BrowserWindow({ show: false });
    never.on('ready-to-show', () => log('never loaded'));
    await sleep(200);
    "#);
    assert_eq!(
        lines,
        [
            "hidden dom-ready",
            "hidden did-finish-load",
            "hidden did-stop-loading",
            "hidden ready-to-show",
            "hidden -- shown",
            "hidden show",
            "hidden focus",
            "shown dom-ready",
            "shown did-finish-load",
            "shown did-stop-loading",
            "shown ready-to-show",
            "no-paint dom-ready",
            "no-paint did-finish-load",
            "no-paint did-stop-loading",
            "no-paint -- shown",
            "no-paint show",
            "no-paint focus",
            "no-paint ready-to-show",
        ]
    );
}

/// A page that logs `beforeunload`, `pagehide`, `visibilitychange` and `unload`, and keeps its
/// `beforeunload` when `block` is set.
fn closing_page(block: bool) -> String {
    let keep = if block {
        "e.preventDefault(); e.returnValue = 'stay';"
    } else {
        ""
    };
    format!(
        "<script>addEventListener('beforeunload', (e) => {{ console.log('page beforeunload'); {keep} }});\
         addEventListener('pagehide', () => console.log('page pagehide'));\
         addEventListener('visibilitychange', () => console.log('page visibilitychange ' + document.visibilityState));\
         addEventListener('unload', () => console.log('page unload'));</script><p>x</p>"
    )
}

/// Makes a window on the page above and answers its closing, as lines of what was heard.
fn closing(setup: &str, block: bool, act: &str) -> Vec<String> {
    let page = closing_page(block)
        .replace('\\', "\\\\")
        .replace('`', "\\`");
    run(&format!(
        r#"
    const win = new BrowserWindow({{ show: false }});
    watch('win', win, ['close', 'closed']);
    watch('contents', win.webContents, ['destroyed']);
    await load(win, `{page}`);
    {setup}
    {act}
    await sleep(400);
    log('destroyed', win.isDestroyed());
    "#
    ))
}

#[test]
fn closing_asks_the_page_then_unloads_it_then_destroys_contents_before_the_window_is_closed() {
    // Electron 39.8.10: close, beforeunload, pagehide, visibilitychange, unload, destroyed, closed.
    let lines = closing("", false, "win.close();");
    assert_eq!(
        lines,
        [
            "win close",
            "page beforeunload",
            "page pagehide",
            "page visibilitychange hidden",
            "page unload",
            "contents destroyed",
            "win closed",
            "destroyed true",
        ]
    );
}

#[test]
fn a_close_listener_that_prevents_it_keeps_the_window_and_asks_nothing_of_the_page() {
    let lines = closing(
        "win.on('close', (e) => e.preventDefault());",
        false,
        "win.close();",
    );
    assert_eq!(lines, ["win close", "destroyed false"]);
}

#[test]
fn destroy_says_closed_then_destroyed_and_asks_nothing() {
    let lines = closing("", false, "win.destroy();");
    assert_eq!(
        lines,
        ["win closed", "contents destroyed", "destroyed true"]
    );
}

#[test]
fn a_page_that_keeps_its_beforeunload_keeps_the_window() {
    // Electron 39.8.10: without a `will-prevent-unload` listener, or with one that does not
    // prevent, the window stays.
    let lines = closing("", true, "win.close();");
    assert_eq!(lines, ["win close", "page beforeunload", "destroyed false"]);
    let lines = closing(
        "win.webContents.on('will-prevent-unload', (e) => log('will-prevent-unload', typeof e.preventDefault));",
        true,
        "win.close();",
    );
    assert_eq!(
        lines,
        [
            "win close",
            "page beforeunload",
            "will-prevent-unload function",
            "destroyed false"
        ]
    );
}

#[test]
fn a_will_prevent_unload_listener_that_prevents_overrules_the_page() {
    // Electron 39.8.10: the page's refusal is ignored and the window closes as usual.
    let lines = closing(
        "win.webContents.on('will-prevent-unload', (e) => { log('will-prevent-unload'); e.preventDefault(); });",
        true,
        "win.close();",
    );
    assert_eq!(
        lines,
        [
            "win close",
            "page beforeunload",
            "will-prevent-unload",
            "page pagehide",
            "page visibilitychange hidden",
            "page unload",
            "contents destroyed",
            "win closed",
            "destroyed true",
        ]
    );
}

#[test]
fn closing_twice_in_a_row_says_close_twice_and_closes_once() {
    // Electron 39.8.10: `close`, `close`, then one round trip.
    let lines = closing("", false, "win.close(); win.close();");
    assert_eq!(&lines[..3], ["win close", "win close", "page beforeunload"]);
    assert_eq!(lines.iter().filter(|line| *line == "win closed").count(), 1);
}

#[test]
fn a_listener_cannot_cancel_a_close_that_is_already_under_way() {
    // Verve's rule (Electron's second `close()` of a closing window is not observed to be
    // cancellable): the second `'close'` is heard, its `preventDefault()` is not, and the round
    // trip the first one began finishes.
    let lines = closing(
        "let count = 0; win.on('close', (e) => { if (++count === 2) e.preventDefault(); });",
        false,
        "win.close(); win.close();",
    );
    assert_eq!(
        lines,
        [
            "win close",
            "win close",
            "page beforeunload",
            "page pagehide",
            "page visibilitychange hidden",
            "page unload",
            "contents destroyed",
            "win closed",
            "destroyed true",
        ]
    );
}

#[test]
fn a_prevented_close_during_a_round_trip_still_calls_off_a_quit() {
    // Verve's reading (not observed on Electron): the round trip the first `close()` began
    // finishes, and the `app.quit()` that asked the window again is cancelled by its listener.
    let lines = run(r#"
    const win = new BrowserWindow({ show: false });
    await load(win, '<p>x</p>');
    let count = 0;
    win.on('close', (e) => { if (++count === 2) e.preventDefault(); });
    win.close();
    app.quit();
    await sleep(400);
    log(count, win.isDestroyed());
    "#);
    assert_eq!(lines, ["2 true"]);
}

#[test]
fn a_destroyed_window_is_no_parent() {
    // Verve's rule, as `setParentWindow` throws for a destroyed one and the constructor does not:
    // the window has no parent, and is not modal.
    let lines = run(r#"
    const parent = new BrowserWindow({ show: false });
    parent.destroy();
    const win = new BrowserWindow({ show: false, parent, modal: true });
    log(win.getParentWindow(), win.isModal());
    "#);
    assert_eq!(lines, ["null false"]);
}

#[test]
fn a_page_that_hangs_is_closed_after_the_wait_and_what_its_listeners_queued_runs() {
    // `closed` is heard when the wait for the page runs out (3 s); a promise its listener made
    // has run by the time the next timer fires, not only after it.
    let lines = run(r#"
    const win = new BrowserWindow({ show: false });
    await load(win, '<script>addEventListener("beforeunload", () => { while (true) {} });</script>');
    let ran = false;
    win.on('closed', () => { Promise.resolve().then(() => { ran = true; }); });
    setTimeout(() => log('queued ran', ran), 3600);
    win.close();
    await sleep(4200);
    log('destroyed', win.isDestroyed());
    "#);
    assert_eq!(lines, ["queued ran true", "destroyed true"]);
}

#[test]
fn children_close_with_their_parent_without_being_asked() {
    // Electron 39.8.10: `parent close`, then each child `closed` newest first, then the parent's;
    // a child's `close` listener is not asked and cannot keep it.
    let lines = run(r#"
    const parent = new BrowserWindow({ show: false, title: 'parent' });
    const a = new BrowserWindow({ show: false, parent, title: 'a' });
    const b = new BrowserWindow({ show: false, parent, title: 'b' });
    log(parent.getChildWindows().map((w) => w.getTitle()), a.getParentWindow() === parent);
    log(BrowserWindow.getAllWindows().map((w) => w.getTitle()));
    b.setParentWindow(null);
    log(parent.getChildWindows().map((w) => w.getTitle()), b.getParentWindow());
    b.setParentWindow(parent);
    log(parent.getChildWindows().map((w) => w.getTitle()));
    watch('parent', parent, ['close', 'closed']);
    watch('a', a, ['close', 'closed']);
    watch('b', b, ['close', 'closed']);
    a.on('close', (e) => e.preventDefault());
    parent.close();
    await sleep(400);
    log(a.isDestroyed(), b.isDestroyed(), parent.isDestroyed(), BrowserWindow.getAllWindows().length);
    "#);
    assert_eq!(lines[0], r#"["b","a"] true"#);
    assert_eq!(lines[1], r#"["b","a","parent"]"#);
    assert_eq!(lines[2], r#"["a"] null"#);
    assert_eq!(lines[3], r#"["b","a"]"#);
    assert_eq!(
        &lines[4..8],
        ["parent close", "b closed", "a closed", "parent closed"]
    );
    assert_eq!(lines[8], "true true true 0");
}

#[test]
fn destroying_a_parent_destroys_its_children_first() {
    let lines = run(r#"
    const parent = new BrowserWindow({ show: false });
    const child = new BrowserWindow({ show: false, parent });
    watch('parent', parent, ['close', 'closed']);
    watch('child', child, ['close', 'closed']);
    parent.destroy();
    log(child.isDestroyed());
    await sleep(200);
    "#);
    assert_eq!(lines, ["true", "child closed", "parent closed"]);
}

#[test]
fn set_parent_window_refuses_what_electron_refuses() {
    // Electron 39.8.10: a value that is no window is `Error: Must pass BaseWindow instance or
    // null`, no argument at all is the gin `TypeError`, and `undefined` is `null`.
    let lines = run(r#"
    const win = new BrowserWindow({ show: false });
    const parent = new BrowserWindow({ show: false });
    for (const bad of [{}, 'x', 5]) {
      try { win.setParentWindow(bad); log('set'); } catch (e) { log(e.name, e.message); }
    }
    try { win.setParentWindow(); log('set'); } catch (e) { log(e.name, e.message); }
    win.setParentWindow(parent);
    win.setParentWindow(undefined);
    log(win.getParentWindow(), parent.getChildWindows().length);
    "#);
    assert_eq!(
        lines,
        [
            "Error Must pass BaseWindow instance or null",
            "Error Must pass BaseWindow instance or null",
            "Error Must pass BaseWindow instance or null",
            "TypeError Insufficient number of arguments.",
            "null 0",
        ]
    );
}

#[test]
fn a_modal_window_keeps_its_parent_and_the_parent_stays_disabled() {
    // Electron 39.8.10: `setParentWindow` of a modal window throws `Error: Can not be called for
    // modal window`, for another window and for `null`; the parent stays disabled meanwhile.
    let lines = run(r#"
    const one = new BrowserWindow({ show: false });
    const two = new BrowserWindow({ show: false });
    const modal = new BrowserWindow({ show: false, parent: one, modal: true });
    modal.show();
    for (const parent of [two, null]) {
      try { modal.setParentWindow(parent); log('set'); } catch (e) { log(e.name, e.message); }
    }
    log(modal.getParentWindow() === one, one.isEnabled(), two.isEnabled());
    "#);
    assert_eq!(
        lines,
        [
            "Error Can not be called for modal window",
            "Error Can not be called for modal window",
            "true false true",
        ]
    );
}

#[test]
fn a_window_cannot_be_its_own_ancestor_and_closing_a_chain_ends() {
    // Verve's rule: Electron has no check. Refused, `a` stays the root, and closing it closes the
    // chain, deepest first, each window once.
    let lines = run(r#"
    const a = new BrowserWindow({ show: false });
    const b = new BrowserWindow({ show: false, parent: a });
    const c = new BrowserWindow({ show: false, parent: b });
    for (const parent of [a, b, c]) {
      try { a.setParentWindow(parent); log('set'); } catch (e) { log(e.name, e.message); }
    }
    log(a.getParentWindow(), b.getParentWindow() === a, c.getParentWindow() === b);
    watch('a', a, ['closed']);
    watch('b', b, ['closed']);
    watch('c', c, ['closed']);
    a.close();
    await sleep(300);
    log(a.isDestroyed(), b.isDestroyed(), c.isDestroyed());
    "#);
    assert_eq!(
        lines,
        [
            "Error A window cannot be its own ancestor",
            "Error A window cannot be its own ancestor",
            "Error A window cannot be its own ancestor",
            "null true true",
            "c closed",
            "b closed",
            "a closed",
            "true true true",
        ]
    );
}

#[test]
fn a_window_closed_while_its_parent_closes_says_closed_once() {
    // A `closed` listener that ends the app while the parent is still closing its children must
    // not make the child say `closed` again.
    let app = App::launch_script(
        r#"
const { app, BrowserWindow } = require('electron');
app.on('window-all-closed', () => {});
app.whenReady().then(() => {
  const parent = new BrowserWindow({ show: false });
  const child = new BrowserWindow({ show: false, parent });
  parent.on('closed', () => console.log('parent closed'));
  child.on('closed', () => { console.log('child closed'); app.exit(0); });
  parent.close();
});"#,
        &[],
    );
    assert_eq!(app.exit_code(TIMEOUT), Some(0), "{:?}", app.console());
    let lines = app.console();
    let said = |text: &str| lines.iter().filter(|line| *line == text).count();
    assert_eq!(said("child closed"), 1, "{lines:?}");
    assert_eq!(said("parent closed"), 1, "{lines:?}");
}

#[test]
fn a_shown_modal_window_disables_its_parent() {
    // Electron 39.8.10: the parent is disabled while the modal window is visible, and enabled
    // again when it is hidden or closed.
    let lines = run(r#"
    const parent = new BrowserWindow({ show: false });
    const modal = new BrowserWindow({ show: false, parent, modal: true });
    log(modal.isModal(), parent.isEnabled(), modal.isEnabled());
    modal.show();
    log(parent.isEnabled());
    modal.hide();
    log(parent.isEnabled());
    modal.show();
    modal.close();
    await sleep(300);
    log(parent.isEnabled());
    parent.setEnabled(false);
    log(parent.isEnabled());
    "#);
    assert_eq!(lines, ["true true true", "false", "true", "true", "false"]);
}

#[test]
fn windows_closing_by_a_persons_click_run_the_same_round_trip() {
    let app = App::launch_script(
        r#"
const { app, BrowserWindow } = require('electron');
app.on('window-all-closed', () => console.log('window-all-closed'));
app.whenReady().then(() => {
  const win = new BrowserWindow({ show: false, title: 'clicked' });
  win.on('close', () => console.log('close'));
  win.on('closed', () => console.log('closed'));
  win.webContents.on('destroyed', () => console.log('destroyed'));
});
"#,
        &[],
    );
    let window = app.window("clicked").expect("the window");
    app.shell.user_close(window.id);
    assert!(
        app.run_until(|app| app.console().len() >= 4, TIMEOUT),
        "{:?}",
        app.console()
    );
    assert_eq!(
        app.console(),
        ["close", "destroyed", "closed", "window-all-closed"]
    );
}

#[test]
fn a_page_that_keeps_its_beforeunload_cancels_a_quit() {
    let app = App::launch_script(
        &format!(
            r#"
const {{ app, BrowserWindow }} = require('electron');
app.whenReady().then(async () => {{
  const win = new BrowserWindow({{ show: false }});
  await new Promise((resolve) => {{ win.webContents.once('did-finish-load', resolve); win.loadURL('data:text/html,' + encodeURIComponent(`{}`)); }});
  app.on('before-quit', () => console.log('before-quit'));
  app.on('will-quit', () => console.log('will-quit'));
  app.quit();
  setTimeout(() => console.log('still', BrowserWindow.getAllWindows().length), 400);
}});
"#,
            closing_page(true)
        ),
        &[],
    );
    assert!(app.run_until(
        |app| app.console().iter().any(|l| l.starts_with("still")),
        TIMEOUT
    ));
    let lines = app.console();
    assert!(lines.contains(&"before-quit".to_owned()), "{lines:?}");
    assert!(!lines.contains(&"will-quit".to_owned()), "{lines:?}");
    assert!(lines.contains(&"still 1".to_owned()), "{lines:?}");
    assert_eq!(app.shell.exit_code(), None);
}

#[test]
fn a_window_and_its_web_contents_belong_together() {
    // Electron 39.8.10: `webContents` is the same object each time, an `EventEmitter` of the class
    // `WebContents`, and `BrowserWindow.fromWebContents` finds the window again.
    let lines = run(r#"
    const win = new BrowserWindow({ show: false });
    const wc = win.webContents;
    log(wc === win.webContents, wc.constructor.name, wc instanceof require('events'), BrowserWindow.fromWebContents(wc) === win, wc.isDestroyed());
    win.destroy();
    log(wc.isDestroyed());
    "#);
    assert_eq!(lines, ["true WebContents true true false", "true"]);
}

#[test]
fn the_window_loads_and_reloads_through_its_web_contents() {
    // `win.loadURL` and `win.loadFile` are `webContents`'; a reload loads the same address as the
    // same entry (Electron 39.8.10: no new entry, did-start-navigation, did-navigate, then
    // did-finish-load).
    let lines = run(r#"
    const win = new BrowserWindow({ show: false });
    const wc = win.webContents;
    await load(win, '<title>R</title>r');
    watch('wc', wc, ['did-navigate', 'did-finish-load']);
    await new Promise((resolve) => { wc.once('did-finish-load', resolve); win.reload(); });
    log(wc.navigationHistory.length(), wc.getTitle());
    const error = await win.loadURL('file:///definitely/not/here.html').catch((e) => e.code);
    log(error);
    "#);
    assert_eq!(lines[0], "wc did-navigate");
    assert_eq!(lines[1], "wc did-finish-load");
    assert_eq!(lines[2], "1 R");
    assert_eq!(lines.last().map(String::as_str), Some("ERR_FILE_NOT_FOUND"));
}

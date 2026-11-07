//! `Menu`, `MenuItem`, `Tray` and `globalShortcut` on a headless shell, held to what Electron
//! 39.8.10 answered on Windows (`tests/electron/menu/expected/probe.txt`, made by
//! `tests/electron/menu/observe`).
//!
//! Nothing here shows a menu, a tray icon or a hotkey: the headless shell keeps what it is sent,
//! and a global shortcut is a record in it, never a registration with the system.
#![allow(clippy::unwrap_used)]

mod support;

use std::time::Duration;

use support::{App, TIMEOUT};
use verve_dom::{Key, Modifiers};
use verve_electron::msg::{FromShell, MenuOp, ToShell, TrayEvent};

const PACKAGE: (&str, &str) = (
    "package.json",
    r#"{"name":"p15probe","version":"1.0.0","main":"main.js"}"#,
);

/// An app made of `main_js`, up when it has logged `READY`.
fn launch(main_js: &str) -> App {
    let app = App::launch_script(main_js, &[PACKAGE]);
    assert!(
        app.run_until(
            |app| app.console().iter().any(|line| line == "READY"),
            TIMEOUT
        ),
        "{:?}",
        app.console()
    );
    app
}

/// The numbers of the `Upsert`s of menu `id` the shell was sent.
fn upserts(app: &App, id: u32) -> usize {
    (app.shell.received().iter())
        .filter(|msg| matches!(msg, ToShell::Menu(MenuOp::Upsert(spec)) if spec.id == id))
        .count()
}

fn ctrl(code: &str, key: &str) -> Key {
    Key {
        key: key.to_owned(),
        code: code.to_owned(),
        modifiers: Modifiers {
            ctrl: true,
            ..Modifiers::default()
        },
        repeat: false,
    }
}

/// What the script's `global.log` holds, once it holds `count` entries.
fn log_of(app: &App, count: usize) -> String {
    let long_enough = |app: &App| {
        let length = app.eval_main("global.log.length");
        length.parse::<usize>().is_ok_and(|have| have >= count)
    };
    assert!(app.run_until(long_enough, TIMEOUT), "{:?}", app.console());
    app.eval_main("global.log")
}

// ---------- what Electron answered ----------

/// The labels of `expected/probe.txt` that Verve is held to, longest first: the lines of the other
/// labels are about Electron's own internals (the names of its prototype's methods, the number
/// its `commandId`s start at, a menu's `_events`) or about window flags that are P13's.
#[cfg(windows)]
const HELD_TO: &[&str] = &[
    "before-ready register",
    "before-ready isRegistered",
    "before-ready tray 123",
    "before-ready tray",
    "before-ready buildFromTemplate",
    "before-ready getApplicationMenu",
    "default app menu",
    "default help sub",
    "popup no window",
    "register invalid2",
    "register invalid",
    "register nofn",
    "registerAll invalid",
    "before-ready unregisterAll",
    "before-ready registerAll",
    "before-ready unregister",
    "before-ready register invalid",
    "registerAll empty",
    "isRegistered bogus",
    "isRegistered",
    "unregister bogus",
    "unregister none",
    "unregisterAll",
    "tray bad 123",
    "tray bad",
    "tray {}",
    "role lowercased",
    "role",
    "submenu of",
    "menu inst",
    "menu props",
    "keys2",
    "keys",
    "desc type",
    "desc",
    "type bogus",
    "type submenu none",
    "type inferred",
    "submenu bad",
    "submenu template",
    "no args",
    "tmpl not array",
    "tmpl bad item",
    "tmpl null item",
    "tmpl sep ok",
    "insert bad",
    "insert neg",
    "insert big",
    "append str",
    "setApplicationMenu bad2",
    "setApplicationMenu bad",
    "popup bad opts",
    "sort sep",
    "sort invisible sep",
    "sort before/after",
    "sort groups",
    "sort submenu",
    "sort cross-group",
    "sort cycle",
    "radio initial",
    "radio after r2",
    "radio after r3",
    "radio set false",
    "radio set true",
    "radio groupId",
    "checkbox",
    "getMenuItemById nested",
    "getMenuItemById missing",
    "click role+click",
    "click plain",
    "app.applicationMenu",
    "set null",
    "setMenu bad",
    "setMenu null",
    "removeMenu",
    "setMenu ok",
];

#[cfg(windows)]
fn held_to(line: &str) -> bool {
    HELD_TO.iter().any(|label| {
        line.strip_prefix(label)
            .is_some_and(|rest| rest.starts_with(' '))
    })
}

#[cfg(windows)]
#[test]
fn menus_trays_and_shortcuts_answer_as_electron_39_does() {
    let observe = support::fixture("../menu/observe");
    // `App::launch` reads the manifest, which names the app `p15probe` as it was in Electron.
    let dir = App::scratch_dir("menu-probe");
    for name in ["main.js", "package.json"] {
        std::fs::copy(observe.join(name), dir.join(name)).unwrap();
    }
    let app = App::launch(&dir);
    let expected = std::fs::read_to_string(observe.join("../expected/probe.txt")).unwrap();
    // The script ends the app when it has printed what it saw.
    assert_eq!(app.exit_code(TIMEOUT), Some(0), "{:?}", app.console());
    let got: Vec<String> = (app.console().iter())
        .filter_map(|line| line.strip_prefix("P15 ").map(str::to_owned))
        .collect();
    let mut differences = Vec::new();
    for line in expected.lines().filter(|line| held_to(line)) {
        if !got.iter().any(|have| have == line) {
            let label = line.split(" ok ").next().unwrap_or(line);
            let near = got.iter().find(|have| have.starts_with(label));
            differences.push(format!("want: {line}\n got: {near:?}"));
        }
    }
    assert!(differences.is_empty(), "{}", differences.join("\n"));
}

// ---------- clicks ----------

#[test]
fn a_click_flips_a_checkbox_first_then_the_radio_group_then_calls_click_with_the_event() {
    let app = launch(
        r#"
const { app, Menu } = require('electron');
global.log = [];
app.whenReady().then(() => {
  const menu = Menu.buildFromTemplate([{ label: 'Tools', submenu: [
    { id: 'plain', label: 'Plain', click: (item, win, event) => log.push(['plain', item.label, win, event.shiftKey, event.ctrlKey, event.altKey, event.metaKey, event.triggeredByAccelerator]) },
    { id: 'box', label: 'Box', type: 'checkbox', click: (item) => log.push(['box', item.checked]) },
    { type: 'separator' },
    { id: 'r1', label: 'R1', type: 'radio', checked: true, click: (item) => log.push(['r1', item.checked]) },
    { id: 'r2', label: 'R2', type: 'radio', click: (item) => log.push(['r2', item.checked, menu.getMenuItemById('r1').checked]) },
  ] }]);
  Menu.setApplicationMenu(menu);
  global.ids = {};
  for (const id of ['plain', 'box', 'r1', 'r2']) ids[id] = menu.getMenuItemById(id).commandId;
  console.log('READY');
});"#,
    );
    let ids: serde_json::Value = serde_json::from_str(&app.eval_main("global.ids")).unwrap();
    let item = |name: &str| u32::try_from(ids[name].as_u64().unwrap()).unwrap();
    app.hub.from_shell(FromShell::MenuClicked {
        menu_item: item("plain"),
        window: None,
        triggered_by_accelerator: true,
        modifiers: Modifiers {
            shift: true,
            ..Modifiers::default()
        },
    });
    for name in ["box", "box", "r2", "r1"] {
        app.shell.menu_click(item(name));
    }
    assert_eq!(
        log_of(&app, 5),
        r#"[["plain","Plain",null,true,false,false,false,true],["box",true],["box",false],["r2",true,false],["r1",true]]"#
    );
    // The script set its own menu, so Electron's default one was not made.
    assert_eq!(app.shell.application_menu().unwrap().items.len(), 1);
}

#[test]
fn a_role_does_its_work_on_the_focused_window_and_page_and_not_the_scripts_click() {
    let app = launch(
        r#"
const { app, Menu, BrowserWindow } = require('electron');
global.log = [];
app.whenReady().then(async () => {
  const menu = Menu.buildFromTemplate([{ label: 'Edit', submenu: [
    { role: 'selectAll', click: () => log.push('click of selectAll') },
    { role: 'close' },
    { role: 'quit' },
  ] }]);
  Menu.setApplicationMenu(menu);
  const win = new BrowserWindow({ show: true });
  win.webContents.selectAll = () => log.push('selectAll');
  win.close = () => log.push('close');
  win.focus();
  for (let i = 0; i < 200 && !win.isFocused(); i++) await new Promise((resolve) => setTimeout(resolve, 10));
  global.roles = menu.items[0].submenu.items.map((item) => item.commandId);
  console.log('READY');
});"#,
    );
    let roles: Vec<u32> = serde_json::from_str(&app.eval_main("global.roles")).unwrap();
    app.shell.menu_click(roles[0]);
    app.shell.menu_click(roles[1]);
    assert_eq!(log_of(&app, 2), r#"["selectAll","close"]"#);
    app.shell.menu_click(roles[2]);
    assert_eq!(app.exit_code(TIMEOUT), Some(0));
}

// ---------- what the shell is sent ----------

#[test]
fn changes_made_in_one_turn_reach_the_shell_as_one_upsert() {
    let app = launch(
        r#"
const { app, Menu } = require('electron');
app.whenReady().then(() => {
  global.MenuItem = require('electron').MenuItem;
  global.menu = Menu.buildFromTemplate([{ id: 'a', label: 'A' }, { id: 'b', label: 'B' }]);
  Menu.setApplicationMenu(menu);
  console.log('READY');
});"#,
    );
    let id = app.shell.application_menu().unwrap().id;
    let before = upserts(&app, id);
    app.eval_main(
        "(() => { const a = menu.getMenuItemById('a'); a.enabled = false; a.label = 'A2'; menu.append(new MenuItem({ label: 'C' })); a.enabled = false; return 1; })()",
    );
    assert!(app.run_until(|app| upserts(app, id) > before, TIMEOUT));
    std::thread::sleep(Duration::from_millis(200));
    assert_eq!(upserts(&app, id), before + 1);
    let menu = app.shell.application_menu().unwrap();
    let shown: Vec<_> = (menu.items.iter())
        .map(|item| (item.label.as_str(), item.enabled))
        .collect();
    assert_eq!(shown, [("A2", false), ("B", true), ("C", true)]);
}

#[test]
fn an_app_that_sets_no_menu_gets_electrons_default_one_and_its_windows_show_it() {
    let app = launch(
        r#"
const { app, BrowserWindow } = require('electron');
app.whenReady().then(() => {
  new BrowserWindow({ show: false, title: 'w' });
  global.labels = require('electron').Menu.getApplicationMenu().items.map((item) => item.label);
  console.log('READY');
});"#,
    );
    let labels = app.eval_main("global.labels");
    if cfg!(target_os = "macos") {
        assert!(
            labels.ends_with(r#""File","Edit","View","Window","Help"]"#),
            "{labels}"
        );
    } else {
        assert_eq!(labels, r#"["File","Edit","View","Window","Help"]"#);
    }
    let window = app.window("w").unwrap();
    assert!(app.run_until(|app| app.shell.menu_of(window.id).is_some(), TIMEOUT));
}

#[test]
fn set_menu_and_remove_menu_give_a_window_a_menu_of_its_own_or_none() {
    let app = launch(
        r#"
const { app, Menu, BrowserWindow } = require('electron');
app.whenReady().then(() => {
  const win = new BrowserWindow({ show: false, title: 'w' });
  global.own = Menu.buildFromTemplate([{ label: 'Own' }]);
  win.setMenu(global.own);
  global.win = win;
  console.log('READY');
});"#,
    );
    let window = app.window("w").unwrap();
    let own =
        |app: &App| (app.shell.menu_of(window.id)).is_some_and(|menu| menu.items[0].label == "Own");
    assert!(app.run_until(own, TIMEOUT));
    if !cfg!(target_os = "macos") {
        app.eval_main("global.win.removeMenu()");
        assert!(app.run_until(|app| app.shell.menu_of(window.id).is_none(), TIMEOUT));
    }
}

#[test]
fn a_popup_is_opened_over_the_window_and_calls_back_when_it_closes() {
    let app = launch(
        r#"
const { app, Menu, BrowserWindow } = require('electron');
app.whenReady().then(() => {
  const win = new BrowserWindow({ show: false });
  const menu = Menu.buildFromTemplate([{ label: 'Inspect' }]);
  menu.on('menu-will-show', () => console.log('will-show'));
  menu.on('menu-will-close', () => console.log('will-close'));
  const shown = menu.popup({ window: win, x: 10, y: 20, callback: () => console.log('callback') });
  console.log('shown', JSON.stringify([shown.x, shown.y, shown.browserWindow === win]));
  setTimeout(() => console.log('READY'), 100);
});"#,
    );
    let lines: Vec<_> = app.console().into_iter().filter(|l| l != "READY").collect();
    assert_eq!(
        lines,
        ["shown [10,20,true]", "will-show", "will-close", "callback"]
    );
    let popups = app.shell.popups();
    assert_eq!(popups.len(), 1);
    assert_eq!(popups[0].at, Some((10., 20.)));
}

// ---------- keys ----------

const WITH_ACCELERATOR: &str = r#"
const { app, Menu, BrowserWindow } = require('electron');
app.whenReady().then(async () => {
  Menu.setApplicationMenu(Menu.buildFromTemplate([{ label: 'File', submenu: [
    { label: 'Open', accelerator: 'Ctrl+O', click: (item, win, event) => console.log('opened', event.triggeredByAccelerator, event.ctrlKey) },
  ] }]));
  const win = new BrowserWindow({ show: false, title: 'w' });
  global.win = win;
  await win.loadURL('data:text/html,' + encodeURIComponent('<script>window.keys = []; addEventListener("keydown", (e) => window.keys.push(e.key))</script>'));
  BEFORE_INPUT
  console.log('READY');
});"#;

#[test]
fn the_page_hears_an_accelerators_key_first_and_the_item_is_clicked_after() {
    // Electron 39.8.10 (a visible window, the coordinator's menu-order probes): the page gets
    // keydown and keyup, and the item is clicked because the page left the keydown alone.
    let app = launch(&WITH_ACCELERATOR.replace("BEFORE_INPUT", ""));
    let window = app.window("w").unwrap();
    app.shell.press_key(window.id, ctrl("KeyO", "o"));
    assert!(app.run_until(
        |app| app.console().iter().any(|line| line == "opened true true"),
        TIMEOUT
    ));
    assert!(app.run_until(
        |app| app.eval_renderer(window.contents, "window.keys") == r#"["o"]"#,
        TIMEOUT
    ));
}

#[test]
fn a_page_that_prevents_the_keydown_keeps_the_item_from_being_clicked() {
    let script = WITH_ACCELERATOR.replace("BEFORE_INPUT", "").replace(
        "window.keys.push(e.key))",
        "{ window.keys.push(e.key); if (e.key === \"p\") e.preventDefault(); })",
    );
    let script = script
        .replace("Ctrl+O", "Ctrl+P")
        .replace("'opened'", "'opened-p'");
    let app = launch(&script);
    let window = app.window("w").unwrap();
    app.shell.press_key(window.id, ctrl("KeyP", "p"));
    // The page heard it, so the main process has had the chance to click; a later key proves it
    // has gone by.
    assert!(app.run_until(
        |app| app.eval_renderer(window.contents, "window.keys") == r#"["p"]"#,
        TIMEOUT
    ));
    app.shell.press_key(window.id, ctrl("KeyQ", "q"));
    assert!(app.run_until(
        |app| app.eval_renderer(window.contents, "window.keys") == r#"["p","q"]"#,
        TIMEOUT
    ));
    app.eval_main("0");
    assert!(
        !app.console().iter().any(|line| line.starts_with("opened")),
        "{:?}",
        app.console()
    );
}

#[test]
fn before_input_event_is_heard_before_the_page_and_can_keep_the_key_from_both() {
    let app = launch(&WITH_ACCELERATOR.replace(
        "BEFORE_INPUT",
        "global.block = true; win.webContents.on('before-input-event', (event) => { if (global.block) event.preventDefault(); });",
    ));
    let window = app.window("w").unwrap();
    app.shell.press_key(window.id, ctrl("KeyO", "o"));
    // The main process does its jobs in order: this one runs after the key was dealt with.
    app.eval_main("global.block = false");
    assert!(
        !app.console().iter().any(|line| line.starts_with("opened")),
        "{:?}",
        app.console()
    );
    assert_eq!(app.eval_renderer(window.contents, "window.keys"), "[]");
    app.shell.press_key(window.id, ctrl("KeyO", "o"));
    assert!(app.run_until(
        |app| app.console().iter().any(|line| line == "opened true true"),
        TIMEOUT
    ));
}

#[test]
fn the_key_up_of_a_key_before_input_event_prevented_reaches_neither_it_nor_the_page() {
    // Electron 39.8.10: a prevented key down has no key up for the listener or the page.
    let page = r#"window.keys = []; window.ups = []; addEventListener("keyup", (e) => window.ups.push(e.key));"#;
    let script = WITH_ACCELERATOR
        .replace(
            "BEFORE_INPUT",
            "global.heard = []; win.webContents.on('before-input-event', (event, input) => { global.heard.push(input.type + ':' + input.key); if (input.key === 'o') event.preventDefault(); });",
        )
        .replace("window.keys = [];", page);
    let app = launch(&script);
    let window = app.window("w").unwrap();
    app.shell.press_key(window.id, ctrl("KeyO", "o"));
    app.shell.press_key(window.id, ctrl("KeyP", "p"));
    assert!(app.run_until(
        |app| app.eval_renderer(window.contents, "window.ups") == r#"["p"]"#,
        TIMEOUT
    ));
    assert_eq!(
        app.eval_main("global.heard.join(' ')"),
        r#""keyDown:o keyDown:p keyUp:p""#
    );
}

#[test]
fn a_held_accelerator_is_heard_by_the_page_each_repeat_and_its_key_up_is_not_swallowed() {
    // Nothing is swallowed but the key up of a key `before-input-event` prevented.
    for listener in ["", "win.webContents.on('before-input-event', () => {});"] {
        let page = r#"window.keys = []; window.ups = []; addEventListener("keyup", (e) => window.ups.push(e.key));"#;
        let script = WITH_ACCELERATOR
            .replace("BEFORE_INPUT", listener)
            .replace("window.keys = [];", page);
        let app = launch(&script);
        let window = app.window("w").unwrap();
        app.shell.hold_key(window.id, ctrl("KeyO", "o"), 3);
        assert!(
            app.run_until(
                |app| app.eval_renderer(window.contents, "window.ups") == r#"["o"]"#,
                TIMEOUT
            ),
            "{listener:?}: {}",
            app.eval_renderer(window.contents, "window.ups")
        );
        assert_eq!(
            app.eval_renderer(window.contents, "window.keys"),
            r#"["o","o","o","o"]"#
        );
    }
}

// ---------- global shortcuts ----------

#[test]
fn register_all_takes_every_key_or_none_and_the_arguments_are_read_before_the_app_is_ready() {
    let app = launch(
        r#"
const { app, globalShortcut } = require('electron');
global.gs = globalShortcut;
const message = (f) => { try { return f(); } catch (error) { return error.message; } };
global.early = [
  message(() => globalShortcut.register('NotAKey', () => {})),
  message(() => globalShortcut.unregister('A')),
  message(() => globalShortcut.unregisterAll()),
  message(() => globalShortcut.registerAll([], () => {})),
  globalShortcut.isRegistered('A'),
];
global.log = [];
app.whenReady().then(() => {
  const f = () => {};
  log.push(globalShortcut.register('CommandOrControl+Shift+X', f));
  log.push(globalShortcut.registerAll(['CommandOrControl+Shift+Y', 'CommandOrControl+Shift+X'], f));
  log.push(globalShortcut.isRegistered('CommandOrControl+Shift+Y'));
  log.push(globalShortcut.registerAll([], f));
  log.push(globalShortcut.registerAll(['F13', 'F14'], f));
  log.push(message(() => globalShortcut.registerAll(['F15', 'NotAKey'], f)));
  console.log('READY');
});"#,
    );
    assert_eq!(
        app.eval_main("global.early"),
        r#"["Error processing argument at index 0, conversion failure from NotAKey","globalShortcut cannot be used before the app is ready","globalShortcut cannot be used before the app is ready","globalShortcut cannot be used before the app is ready",false]"#
    );
    assert_eq!(
        app.eval_main("global.log"),
        r#"[true,false,false,true,true,"Error processing argument at index 0, conversion failure from "]"#
    );
    assert_eq!(app.shell.shortcuts().len(), 3);
}

#[test]
fn a_global_shortcut_is_registered_through_the_shell_and_its_callback_runs_when_it_is_pressed() {
    let app = launch(
        r#"
const { app, globalShortcut } = require('electron');
global.gs = globalShortcut;
global.log = [];
global.early = (() => { try { globalShortcut.register('A', () => {}); } catch (error) { return error.message; } })();
app.whenReady().then(() => {
  log.push(['register', globalShortcut.register('CommandOrControl+Shift+X', () => log.push('pressed'))]);
  log.push(['again', globalShortcut.register('CommandOrControl+Shift+X', () => {})]);
  log.push(['is', globalShortcut.isRegistered('CommandOrControl+Shift+X'), globalShortcut.isRegistered('F13')]);
  console.log('READY');
});"#,
    );
    assert_eq!(
        app.eval_main("global.early"),
        r#""globalShortcut cannot be used before the app is ready""#
    );
    assert_eq!(
        app.eval_main("global.log"),
        r#"[["register",true],["again",false],["is",true,false]]"#
    );
    let taken = app.shell.shortcuts();
    assert_eq!(taken.len(), 1);
    let id = *taken.keys().next().unwrap();
    app.shell.press_shortcut(id);
    assert_eq!(
        log_of(&app, 4),
        r#"[["register",true],["again",false],["is",true,false],"pressed"]"#
    );
    // A system that refuses the key makes `register` answer false.
    app.shell.accept_shortcuts(false);
    assert_eq!(
        app.eval_main("global.gs.register('CommandOrControl+Shift+Y', () => {})"),
        "false"
    );
    app.shell.accept_shortcuts(true);
    app.eval_main("global.gs.unregister('CommandOrControl+Shift+X')");
    assert!(app.run_until(|app| app.shell.shortcuts().is_empty(), TIMEOUT));
}

// ---------- the tray ----------

#[test]
fn a_tray_is_an_icon_the_shell_keeps_and_tells_the_script_what_happened_to() {
    let app = launch(
        r#"
const { app, Tray, Menu, nativeImage } = require('electron');
app.whenReady().then(() => {
  global.tray = new Tray(nativeImage.createEmpty());
  tray.setToolTip('hint');
  tray.setTitle('title');
  tray.setContextMenu(Menu.buildFromTemplate([{ label: 'Show' }]));
  tray.on('click', (event, bounds, position) => console.log('click', JSON.stringify([event.shiftKey, bounds, position])));
  tray.on('right-click', (event, bounds) => console.log('right-click', bounds.width));
  console.log('READY');
});"#,
    );
    let icon = app.shell.trays()[&1].clone();
    assert_eq!(icon.tool_tip, "hint");
    assert_eq!(icon.title, "title");
    assert!(icon.context_menu.is_some());
    app.shell.tray_event(1, TrayEvent::Click);
    app.shell.tray_event(1, TrayEvent::RightClick);
    assert!(app.run_until(|app| app.console().len() >= 3, TIMEOUT));
    let lines = app.console();
    assert_eq!(
        &lines[1..],
        [
            r#"click [false,{"x":1800,"y":1040,"width":16,"height":16},{"x":1808,"y":1048}]"#,
            "right-click 16"
        ]
    );
    assert_eq!(
        app.eval_main("tray.getBounds()"),
        r#"{"x":1800,"y":1040,"width":16,"height":16}"#
    );
    app.eval_main("tray.destroy()");
    assert_eq!(app.eval_main("tray.isDestroyed()"), "true");
    assert_eq!(
        app.eval_main(
            "(() => { try { tray.setToolTip('x'); } catch (error) { return error.message; } })()"
        ),
        r#""Tray is destroyed""#
    );
    assert!(app.run_until(|app| app.shell.trays()[&1].destroyed, TIMEOUT));
}

/// Collects garbage on the main process's thread until `done`, which the shell's state decides.
fn collect_until(app: &App, done: impl Fn(&App) -> bool) -> bool {
    app.run_until(
        |app| {
            app.hub.to_main(|_| boa_engine::gc::force_collect());
            done(app)
        },
        TIMEOUT,
    )
}

#[test]
fn a_tray_nothing_refers_to_goes_when_it_is_collected() {
    let app = launch(
        r#"
const { app, Tray, nativeImage } = require('electron');
app.whenReady().then(() => {
  new Tray(nativeImage.createEmpty());
  console.log('READY');
});"#,
    );
    assert!(!app.shell.trays()[&1].destroyed);
    assert!(collect_until(&app, |app| app.shell.trays()[&1].destroyed));
}

#[test]
fn a_menu_popped_up_and_let_go_of_is_dropped_by_the_shell_when_it_is_collected() {
    let app = launch(
        r#"
const { app, Menu, BrowserWindow } = require('electron');
app.whenReady().then(() => {
  const win = new BrowserWindow({ show: false });
  Menu.buildFromTemplate([{ label: 'Inspect' }]).popup({ window: win });
  setTimeout(() => console.log('READY'), 100);
});"#,
    );
    let dropped = |app: &App| {
        (app.shell.received().iter()).any(|msg| matches!(msg, ToShell::Menu(MenuOp::Drop(_))))
    };
    assert!(!dropped(&app));
    assert!(collect_until(&app, dropped));
}

#[test]
fn a_change_a_timer_makes_reaches_the_shell_in_the_same_turn() {
    let app = launch(
        r#"
const { app, Menu } = require('electron');
app.whenReady().then(() => {
  const menu = Menu.buildFromTemplate([{ id: 'a', label: 'A' }]);
  Menu.setApplicationMenu(menu);
  setTimeout(() => { menu.getMenuItemById('a').label = 'late'; console.log('changed'); }, 50);
  console.log('READY');
});"#,
    );
    let changed = |app: &App| app.console().iter().any(|line| line == "changed");
    assert!(app.run_until(changed, TIMEOUT));
    // The loop's idle wait is up to half a second: the menu must not have waited for it.
    let sent = |app: &App| {
        app.shell
            .application_menu()
            .is_some_and(|m| m.items[0].label == "late")
    };
    assert!(app.run_until(sent, Duration::from_millis(250)));
}

//! The headless shell as a test sees it: the boxes a page lays out for itself, what a person's
//! click and window resize do to a page, and how the app's console and realms are read.

mod support;

use support::{App, TIMEOUT};

const PAGE: &str = r#"
const { BrowserWindow } = require('electron');
console.log('main says hi');
const win = new BrowserWindow({ title: 'page', width: 500, height: 300 });
win.loadURL('data:text/html,<style>html,body{margin:0}div,p,button{display:block;box-sizing:border-box;height:16px;margin:0;padding:0;border:0}</style><div id="a">hello world</div><p id="p"><span id="s">ab</span> cd</p><button id="b">go</button>');
"#;

/// An app whose window has shown its page: the app, the window's id and its page's.
fn shown() -> (App, u32, u32) {
    let app = App::launch_script(PAGE, &[]);
    let window = app.window("page").expect("the window was made");
    let text = |app: &App| app.page_text(window.id);
    assert!(app.run_until(
        |app| text(app).as_deref() == Some("hello worldab cdgo"),
        TIMEOUT
    ));
    (app, window.id, window.contents)
}

fn rect(app: &App, contents: u32, id: &str) -> String {
    let js = format!(
        "(() => {{ const r = document.getElementById('{id}').getBoundingClientRect(); return [r.x, r.y, r.width, r.height]; }})()"
    );
    app.eval_renderer(contents, &js)
}

#[test]
fn blocks_stack_over_the_full_width_in_the_pages_own_layout() {
    let (app, _, contents) = shown();
    // The page lays itself out (`verve-layout`) when a script reads a box; the window's width
    // is the one thing it waits for, and the shell sends it once the window is up.
    assert!(app.run_until(|app| rect(app, contents, "a") == "[0,0,500,16]", TIMEOUT));
    assert_eq!(rect(&app, contents, "p"), "[0,16,500,16]");
    // A button laid out as a block takes the line under the paragraph. What text is as wide as
    // is P31's: until its inline layout lands, a box is only as tall as its style says.
    assert_eq!(rect(&app, contents, "b"), "[0,32,500,16]");
}

#[test]
fn resizing_a_window_tells_the_page_and_lays_it_out_again() {
    let (app, window, contents) = shown();
    assert!(app.run_until(|app| rect(app, contents, "a") == "[0,0,500,16]", TIMEOUT));
    app.shell.resize(window, 40., 300.);
    assert!(app.run_until(
        |app| app.eval_renderer(contents, "innerWidth") == "40",
        TIMEOUT
    ));
    // A block is as wide as the window it is in.
    assert!(app.run_until(|app| rect(app, contents, "a") == "[0,0,40,16]", TIMEOUT));
    assert_eq!(app.shell.windows()[0].bounds.width, 40.);
}

#[test]
fn a_click_reaches_the_pages_listener_and_the_console_keeps_both_processes_lines() {
    let (app, window, contents) = shown();
    app.eval_renderer(
        contents,
        "document.getElementById('b').addEventListener('click', () => console.log('clicked')); 0",
    );
    app.shell.click(window, "#b");
    assert!(app.run_until(|app| app.console().contains(&"clicked".to_owned()), TIMEOUT));
    assert_eq!(app.console(), ["main says hi", "clicked"]);
}

#[test]
fn the_main_process_answers_in_json() {
    // A window keeps the app up: with none, P02's rule ends it as soon as the script has run.
    let main = "globalThis.answer = { n: 6 * 7 }; new (require('electron').BrowserWindow)();";
    let app = App::launch_script(main, &[]);
    assert_eq!(app.eval_main("answer"), r#"{"n":42}"#);
    assert_eq!(app.eval_main("process.type"), r#""browser""#);
}

#[test]
fn the_app_exit_code_is_waited_for() {
    let app = App::launch_script("setTimeout(() => process.exit(3), 10)", &[]);
    assert_eq!(app.wait_exit(TIMEOUT), 3);
}

#[test]
fn a_shown_window_gets_its_first_paint_once_with_the_first_snapshot() {
    use std::sync::atomic::{AtomicU32, Ordering};
    let paints = std::sync::Arc::new(AtomicU32::new(0));
    let app = App::launch_script(
        "const { BrowserWindow } = require('electron');\n\
         setTimeout(() => new BrowserWindow().loadURL('data:text/html,<p>x</p>'), 300);",
        &[],
    );
    let counted = std::sync::Arc::clone(&paints);
    app.hub.observe_shell(move |msg| {
        let painted = matches!(
            msg,
            verve_electron::msg::FromShell::Window {
                event: verve_electron::msg::WindowEvent::FirstPaint,
                ..
            }
        );
        counted.fetch_add(u32::from(painted), Ordering::SeqCst);
    });
    assert!(app.run_until(|_| paints.load(Ordering::SeqCst) >= 1, TIMEOUT));
    std::thread::sleep(std::time::Duration::from_millis(200));
    assert_eq!(paints.load(Ordering::SeqCst), 1);
}

#[test]
fn a_hidden_window_whose_page_is_drawn_gets_its_first_paint_when_it_is_shown() {
    use std::sync::atomic::{AtomicU32, Ordering};
    use verve_electron::msg::{ToShell, WindowOp};
    let app = App::launch_script(
        "const { BrowserWindow } = require('electron');\n\
         new BrowserWindow({ show: false, title: 'later' }).loadURL('data:text/html,<p>x</p>');",
        &[],
    );
    let window = {
        assert!(app.run_until(|app| app.window("later").is_some(), TIMEOUT));
        app.window("later").expect("the window").id
    };
    assert!(app.run_until(|app| app.page_text(window).is_some(), TIMEOUT));
    let paints = std::sync::Arc::new(AtomicU32::new(0));
    let counted = std::sync::Arc::clone(&paints);
    app.hub.observe_shell(move |msg| {
        let painted = matches!(
            msg,
            verve_electron::msg::FromShell::Window {
                event: verve_electron::msg::WindowEvent::FirstPaint,
                ..
            }
        );
        counted.fetch_add(u32::from(painted), Ordering::SeqCst);
    });
    std::thread::sleep(std::time::Duration::from_millis(200));
    assert_eq!(
        paints.load(Ordering::SeqCst),
        0,
        "a hidden window has no frame"
    );
    let show = ToShell::Window {
        window,
        op: WindowOp::Show,
    };
    app.hub.to_shell(show);
    assert_eq!(paints.load(Ordering::SeqCst), 1);
}

#[test]
fn an_input_methods_composition_reaches_the_focused_field_as_chromes_events() {
    // Electron 39.8.10 (Input.imeSetComposition / Input.insertText, any text field): the first
    // update is compositionstart, compositionupdate, beforeinput, input; a commit is
    // compositionupdate, beforeinput, input, compositionend, and the inputs are
    // `insertCompositionText` with `isComposing`. The page hears the same through the shell.
    use verve_electron::msg::CompositionEvent;
    let page = r#"<input id="i"><div id="e" contenteditable></div>"#;
    let main = format!(
        "const {{ BrowserWindow }} = require('electron'); const win = new BrowserWindow({{ title: 'ime', show: false }}); win.loadURL('data:text/html,{page}');"
    );
    let app = App::launch_script(&main, &[]);
    let window = app.window("ime").expect("the window was made");
    let contents = window.contents;
    assert!(app.run_until(
        |app| app.eval_renderer(contents, "!!document.getElementById('i')") == "true",
        TIMEOUT
    ));
    let listen = "globalThis.heard = []; for (const kind of ['compositionstart', 'compositionupdate', 'compositionend', 'beforeinput', 'input']) { document.addEventListener(kind, (e) => heard.push(kind + (e.inputType ? ':' + e.inputType : '') + (e.data ? '=' + e.data : '') + (e.isComposing ? '*' : ''))); } document.getElementById('i').focus(); 0";
    app.eval_renderer(contents, listen);
    // `eval_renderer` answers JSON, so a string comes quoted.
    let heard = |app: &App| {
        app.eval_renderer(contents, "heard.join(' ')")
            .trim_matches('"')
            .to_owned()
    };
    app.shell.compose(
        window.id,
        CompositionEvent::Update {
            text: "ni".into(),
            selection: Some(2..2),
        },
    );
    assert!(app.run_until(
        |app| heard(app).contains("input:insertCompositionText=ni*"),
        TIMEOUT
    ));
    assert_eq!(
        heard(&app),
        "compositionstart compositionupdate=ni beforeinput:insertCompositionText=ni* input:insertCompositionText=ni*"
    );
    app.shell
        .compose(window.id, CompositionEvent::Commit("你".into()));
    assert!(app.run_until(
        |app| app.eval_renderer(contents, "document.getElementById('i').value") == "\"你\"",
        TIMEOUT
    ));
    assert!(heard(&app).ends_with(
        "compositionupdate=你 beforeinput:insertCompositionText=你* input:insertCompositionText=你* compositionend=你"
    ));
    // The same in an editable region, then cancelled.
    app.eval_renderer(contents, "document.getElementById('e').focus(); 0");
    app.shell.compose(
        window.id,
        CompositionEvent::Update {
            text: "hao".into(),
            selection: None,
        },
    );
    let text = |app: &App| app.eval_renderer(contents, "document.getElementById('e').textContent");
    assert!(app.run_until(|app| text(app) == "\"hao\"", TIMEOUT));
    app.shell.compose(window.id, CompositionEvent::Cancel);
    assert!(app.run_until(|app| text(app) == "\"\"", TIMEOUT));
}

#[test]
fn the_shell_holds_the_clipboard_a_page_copies_to_and_pastes_from() {
    // The clipboard is the shell's. The headless shell keeps one in memory, so this test never
    // reaches the user's. A page's copy arrives as `WriteClipboard`; a paste is the clipboard
    // read first and sent in place of Ctrl+V, or asked for by the page's own renderer when
    // `webContents.paste()` has no key to wait for.
    let page = r#"<input id="i" value="ab"><div id="e" contenteditable>x</div>"#;
    let main = format!(
        "const {{ BrowserWindow }} = require('electron'); globalThis.win = new BrowserWindow({{ title: 'clip', show: false }}); win.loadURL('data:text/html,{page}');"
    );
    let app = App::launch_script(&main, &[]);
    let window = app.window("clip").expect("the window was made");
    let contents = window.contents;
    assert!(app.run_until(
        |app| app.eval_renderer(contents, "!!document.getElementById('i')") == "true",
        TIMEOUT
    ));
    app.eval_renderer(
        contents,
        "navigator.clipboard.writeText('from the page'); 0",
    );
    assert!(app.run_until(
        |app| app.shell.clipboard().text.as_deref() == Some("from the page"),
        TIMEOUT
    ));
    // A person pastes into the field.
    app.eval_renderer(
        contents,
        "const i = document.getElementById('i'); i.focus(); i.setSelectionRange(2, 2); 0",
    );
    app.shell.set_clipboard(verve_dom::ClipboardData {
        text: Some("XY".into()),
        html: Some("<b>XY</b>".into()),
        ..Default::default()
    });
    // Ctrl+V reaches the page as any key does: it hears the keydown, and the paste is that key's
    // default action, made when the shell has answered the page's read of the clipboard.
    let mut ctrl_v = verve_dom::Key::char('v');
    ctrl_v.modifiers.ctrl = true;
    app.shell.press_key(window.id, ctrl_v);
    let value = |app: &App| app.eval_renderer(contents, "document.getElementById('i').value");
    assert!(app.run_until(|app| value(app) == "\"abXY\"", TIMEOUT));
    // `webContents.paste()` goes through the renderer asking the shell, and keeps the markup.
    app.eval_renderer(
        contents,
        "const e = document.getElementById('e'); e.focus(); getSelection().collapse(e.firstChild, 1); 0",
    );
    app.eval_main("win.webContents.paste(); 0");
    let html = |app: &App| app.eval_renderer(contents, "document.getElementById('e').innerHTML");
    assert!(app.run_until(|app| html(app) == "\"x<b>XY</b>\"", TIMEOUT));
    // A page reads the clipboard through the same shell once it is allowed to.
    app.eval_main(
        "win.webContents.session.setPermissionRequestHandler((wc, permission, callback) => callback(true)); 0",
    );
    app.eval_renderer(
        contents,
        "globalThis.read = 'pending'; navigator.clipboard.readText().then((t) => { read = t; }); 0",
    );
    assert!(app.run_until(
        |app| app.eval_renderer(contents, "read") == "\"XY\"",
        TIMEOUT
    ));
}

#[test]
fn two_files_dragged_in_from_the_system_reach_the_page_with_their_paths() {
    // Electron 39.8.10 (Input.dispatchDragEvent with two files): the page hears `dragenter` with
    // no files yet, `dragover`, and at the drop `files.length` is 2; `file.path` does not exist,
    // `webUtils.getPathForFile(file)` gives the path.
    let dir = App::scratch_dir("drag-two-files");
    std::fs::create_dir_all(&dir).expect("a folder");
    let (a, b) = (dir.join("a.txt"), dir.join("b.txt"));
    std::fs::write(&a, "hello\n").expect("a file");
    std::fs::write(&b, "world!\n").expect("a file");
    let page =
        r#"<style>html,body{margin:0}%23t{width:200px;height:100px}</style><div id="t">drop</div>"#;
    let main = format!(
        "const {{ BrowserWindow }} = require('electron'); new BrowserWindow({{ title: 'drop', show: false, webPreferences: {{ nodeIntegration: true, contextIsolation: false }} }}).loadURL('data:text/html,{page}');"
    );
    let app = App::launch_script(&main, &[]);
    let window = app.window("drop").expect("the window was made");
    let contents = window.contents;
    assert!(app.run_until(
        |app| app.eval_renderer(contents, "!!document.getElementById('t')") == "true",
        TIMEOUT
    ));
    let listen = "globalThis.heard = []; const t = document.getElementById('t'); \
        t.addEventListener('dragenter', (e) => { e.preventDefault(); heard.push('enter files=' + e.dataTransfer.files.length); }); \
        t.addEventListener('dragover', (e) => { e.preventDefault(); heard.push('over'); }); \
        t.addEventListener('drop', (e) => { e.preventDefault(); const { webUtils } = require('electron'); \
            const files = Array.from(e.dataTransfer.files); \
            heard.push('drop files=' + files.length + ' ' + files.map((f) => f.name + ':' + webUtils.getPathForFile(f) + ':' + ('path' in f)).join('|')); }); 0";
    app.eval_renderer(contents, listen);
    app.shell.drag_enter(
        window.id,
        verve_dom::ExternalDrag {
            files: vec![a.clone(), b.clone()],
            ..Default::default()
        },
        50.,
        50.,
    );
    app.shell.drag_over(window.id, 52., 51.);
    app.shell.drop_at(window.id, 52., 51.);
    let heard = |app: &App| {
        app.eval_renderer(contents, "heard.join(' ; ')")
            .trim_matches('"')
            .replace("\\\\", "/")
    };
    assert!(app.run_until(|app| heard(app).contains("drop files=2"), TIMEOUT));
    let path = |p: &std::path::Path| p.to_string_lossy().replace('\\', "/");
    assert_eq!(
        heard(&app),
        format!(
            "enter files=0 ; over ; over ; drop files=2 a.txt:{}:false|b.txt:{}:false",
            path(&a),
            path(&b)
        )
    );
    std::fs::remove_dir_all(&dir).ok();
}

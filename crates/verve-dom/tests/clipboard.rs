//! `copy`, `cut` and `paste` events, what the default actions do, and `navigator.clipboard`.
//!
//! The clipboard belongs to the shell, so a copy is a [`Request::WriteClipboard`] and a paste is
//! handed [`ClipboardData`]. The sequences come from the Clipboard API and HTML specifications and
//! Chromium's `Editor::DispatchClipboardEvent`; they were not observed in Electron, because copying
//! in a real Electron writes the user's system clipboard (even with `preventDefault` and
//! `setData`). What is taken from there:
//!
//! - `copy`, `cut` and `paste` are cancelable, bubble, and are dispatched at the focused element,
//!   else the element the selection starts in, else `<body>`;
//! - the default action of a copy writes the selection (`text/plain` and `text/html`) unless a
//!   listener canceled the event, in which case what the listener put in `clipboardData` is
//!   written; `setData` without `preventDefault` changes nothing;
//! - a cut is a copy followed by `deleteByCut` (`beforeinput` then `input`);
//! - a paste is `insertFromPaste`: for `<input>` and `<textarea>` the `beforeinput` carries the
//!   text in `data`; in an editable region `data` is `null` and `dataTransfer` has the
//!   clipboard's formats.

mod common;

use std::cell::RefCell;
use std::rc::Rc;

use common::{Page, on_page};
use verve_dom::{ClipboardData, Options, PermissionBackend, Request};

fn page(body: &str) -> Page {
    let mut page = Page::new(Options::default());
    page.eval(&format!(
        "globalThis.log = [];
         document.body.innerHTML = {body:?};
         for (const kind of ['copy', 'cut', 'paste', 'beforeinput', 'input']) {{
             document.addEventListener(kind, (e) => log.push(kind + (e.inputType ? ':' + e.inputType : '')), true);
         }}"
    ));
    page
}

fn writes(page: &mut Page) -> Vec<Request> {
    page.document
        .take_requests()
        .into_iter()
        .filter(|r| matches!(r, Request::WriteClipboard { .. }))
        .collect()
}

fn text(text: &str) -> ClipboardData {
    ClipboardData {
        text: Some(text.to_owned()),
        ..ClipboardData::default()
    }
}

fn written(text: &str, html: Option<&str>) -> Request {
    Request::WriteClipboard {
        text: Some(text.to_owned()),
        html: html.map(str::to_owned),
        custom: Vec::new(),
    }
}

#[test]
fn copying_a_selection_writes_its_text_and_markup() {
    on_page(|| {
        let mut page = page("<p id=p>Hello <b>wor</b>ld</p>");
        page.eval("getSelection().selectAllChildren(document.getElementById('p'))");
        assert!(page.document.copy(&mut page.runtime));
        assert_eq!(
            writes(&mut page),
            [written("Hello world", Some("Hello <b>wor</b>ld"))]
        );
        assert_eq!(page.eval("log.join(' ')"), "copy");
    });
}

#[test]
fn copying_nothing_writes_nothing() {
    on_page(|| {
        let mut page = page("<p id=p>Hello</p>");
        assert!(!page.document.copy(&mut page.runtime));
        assert!(writes(&mut page).is_empty());
    });
}

#[test]
fn a_copy_listener_that_cancels_and_sets_data_replaces_the_default() {
    on_page(|| {
        let mut page = page("<p id=p>Hello</p>");
        page.eval(
            "getSelection().selectAllChildren(document.getElementById('p'));
             document.addEventListener('copy', (e) => {
                 e.preventDefault();
                 e.clipboardData.setData('text/plain', 'X');
                 e.clipboardData.setData('text/html', '<i>X</i>');
                 e.clipboardData.setData('application/x-mine', 'm');
             });",
        );
        assert!(page.document.copy(&mut page.runtime));
        assert_eq!(
            writes(&mut page),
            [Request::WriteClipboard {
                text: Some("X".into()),
                html: Some("<i>X</i>".into()),
                custom: vec![("application/x-mine".into(), b"m".to_vec())],
            }]
        );
    });
}

#[test]
fn setting_data_without_canceling_changes_nothing() {
    on_page(|| {
        let mut page = page("<p id=p>Hello</p>");
        page.eval(
            "getSelection().selectAllChildren(document.getElementById('p'));
             document.addEventListener('copy', (e) => e.clipboardData.setData('text/plain', 'X'));",
        );
        page.document.copy(&mut page.runtime);
        assert_eq!(writes(&mut page), [written("Hello", Some("Hello"))]);
    });
}

#[test]
fn copying_and_cutting_from_a_text_field_use_its_selection() {
    on_page(|| {
        let mut page = page("<input id=i value='hello world'>");
        page.eval("const i = document.getElementById('i'); i.focus(); i.setSelectionRange(0, 5);");
        assert!(page.document.copy(&mut page.runtime));
        assert_eq!(writes(&mut page), [written("hello", None)]);
        page.eval("log.length = 0");
        assert!(page.document.cut(&mut page.runtime));
        assert_eq!(writes(&mut page), [written("hello", None)]);
        assert_eq!(page.eval("i.value"), " world");
        assert_eq!(
            page.eval("log.join(' ')"),
            "cut beforeinput:deleteByCut input:deleteByCut"
        );
    });
}

#[test]
fn cutting_in_an_editable_region_deletes_the_selection() {
    on_page(|| {
        let mut page = page("<div id=ed contenteditable><p id=p>abcdef</p></div>");
        page.eval(
            "document.getElementById('ed').focus(); const t = document.getElementById('p').firstChild;
             const r = document.createRange(); r.setStart(t, 0); r.setEnd(t, 3);
             getSelection().removeAllRanges(); getSelection().addRange(r);",
        );
        assert!(page.document.cut(&mut page.runtime));
        assert_eq!(writes(&mut page), [written("abc", Some("abc"))]);
        assert_eq!(
            page.eval("document.getElementById('ed').innerHTML"),
            "<p id=\"p\">def</p>"
        );
        assert_eq!(
            page.eval("log.join(' ')"),
            "cut beforeinput:deleteByCut input:deleteByCut"
        );
    });
}

#[test]
fn a_canceled_cut_leaves_the_text_to_the_listener() {
    on_page(|| {
        let mut page = page("<input id=i value='hello world'>");
        page.eval(
            "const i = document.getElementById('i'); i.focus(); i.setSelectionRange(0, 5);
             document.addEventListener('cut', (e) => { e.preventDefault(); e.clipboardData.setData('text/plain', 'mine'); });",
        );
        page.document.cut(&mut page.runtime);
        assert_eq!(writes(&mut page), [written("mine", None)]);
        assert_eq!(page.eval("i.value"), "hello world");
    });
}

#[test]
fn pasting_into_a_text_field_inserts_the_text() {
    on_page(|| {
        let mut page = page("<input id=i value=ab>");
        page.eval(
            "const i = document.getElementById('i'); i.focus(); i.setSelectionRange(2, 2);
             document.addEventListener('beforeinput', (e) => log.push('data=' + e.data));",
        );
        page.document.paste(&mut page.runtime, &text("pasted"));
        assert_eq!(page.eval("i.value"), "abpasted");
        assert_eq!(
            page.eval("log.join(' ')"),
            "paste beforeinput:insertFromPaste data=pasted input:insertFromPaste"
        );
    });
}

#[test]
fn a_canceled_paste_inserts_nothing_and_the_listener_reads_the_clipboard() {
    on_page(|| {
        let mut page = page("<input id=i value=ab>");
        page.eval(
            "const i = document.getElementById('i'); i.focus(); globalThis.seen = [];
             document.addEventListener('paste', (e) => {
                 e.preventDefault();
                 seen.push(e.clipboardData.getData('text/plain'), e.clipboardData.types.join());
                 e.clipboardData.setData('text/plain', 'ignored');
             });",
        );
        let data = ClipboardData {
            text: Some("pasted".into()),
            html: Some("<b>pasted</b>".into()),
            ..ClipboardData::default()
        };
        page.document.paste(&mut page.runtime, &data);
        assert_eq!(page.eval("i.value"), "ab");
        assert_eq!(page.eval("seen.join('|')"), "pasted|text/plain,text/html");
    });
}

#[test]
fn pasting_markup_into_an_editable_region_cleans_it() {
    on_page(|| {
        let mut page = page("<div id=ed contenteditable><p id=p>x</p></div>");
        page.eval(
            "document.getElementById('ed').focus(); const t = document.getElementById('p').firstChild;
             getSelection().collapse(t, 1);
             document.addEventListener('beforeinput', (e) => log.push('html=' + e.dataTransfer.getData('text/html').length + ' data=' + e.data));",
        );
        let data = ClipboardData {
            text: Some("plain".into()),
            html: Some("<b>bold</b><script>alert(1)</script><i onclick=\"x()\">it</i><a href=\"javascript:x()\">a</a><a href=\"java&#9;script:x()\">b</a><a href=\"data:text/html,x\">c</a><svg><animate attributeName=\"href\" to=\"javascript:x()\"></animate></svg>".into()),
            ..ClipboardData::default()
        };
        page.document.paste(&mut page.runtime, &data);
        assert_eq!(
            page.eval("document.getElementById('p').innerHTML"),
            "x<b>bold</b><i>it</i><a>a</a><a>b</a><a>c</a><svg></svg>"
        );
        assert!(
            page.eval("log.join(' ')")
                .contains("beforeinput:insertFromPaste")
        );
        assert!(page.eval("log.join(' ')").contains("data=null"));
    });
}

#[test]
fn pasting_and_matching_style_takes_the_text() {
    on_page(|| {
        let mut page = page("<div id=ed contenteditable><p id=p>x</p></div>");
        page.eval(
            "document.getElementById('ed').focus(); getSelection().collapse(document.getElementById('p').firstChild, 1);",
        );
        let data = ClipboardData {
            text: Some("a<b\nc".into()),
            html: Some("<b>bold</b>".into()),
            ..ClipboardData::default()
        };
        page.document.paste_plain(&mut page.runtime, &data);
        assert_eq!(
            page.eval("document.getElementById('ed').innerHTML"),
            "<p id=\"p\">xa&lt;b</p><p>c</p>"
        );
    });
}

#[test]
fn pasting_files_lists_them_in_the_transfer() {
    on_page(|| {
        let dir = std::env::temp_dir().join(format!("verve-paste-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("a folder");
        let file = dir.join("note.txt");
        std::fs::write(&file, "hello").expect("a file");
        let mut page = page("<div id=ed contenteditable><p>x</p></div>");
        page.eval(
            "document.getElementById('ed').focus(); globalThis.seen = [];
             document.addEventListener('paste', (e) => {
                 e.preventDefault();
                 seen.push(e.clipboardData.files.length, e.clipboardData.files[0].name,
                           e.clipboardData.files[0].size, e.clipboardData.types.join());
             });",
        );
        let data = ClipboardData {
            files: vec![file.clone()],
            ..ClipboardData::default()
        };
        page.document.paste(&mut page.runtime, &data);
        assert_eq!(page.eval("seen.join('|')"), "1|note.txt|5|Files");
        std::fs::remove_dir_all(&dir).ok();
    });
}

#[test]
fn execcommand_copy_and_cut_use_the_same_path() {
    on_page(|| {
        let mut page = page("<div id=ed contenteditable><p id=p>abcdef</p></div>");
        page.eval(
            "document.getElementById('ed').focus(); const t = document.getElementById('p').firstChild;
             const r = document.createRange(); r.setStart(t, 0); r.setEnd(t, 3);
             getSelection().removeAllRanges(); getSelection().addRange(r);",
        );
        // A script may copy and cut only for a few seconds after a person pressed something.
        page.document
            .key_down(&mut page.runtime, &verve_dom::Key::named("Shift"));
        assert_eq!(page.eval("document.execCommand('copy')"), "true");
        assert_eq!(writes(&mut page), [written("abc", Some("abc"))]);
        assert_eq!(page.eval("document.execCommand('cut')"), "true");
        assert_eq!(page.eval("document.getElementById('p').textContent"), "def");
        assert_eq!(page.eval("log.join(' ')"), "copy cut input:deleteByCut");
    });
}

#[test]
fn write_text_asks_the_shell_to_write() {
    on_page(|| {
        let mut page = Page::new(Options::default());
        page.eval("navigator.clipboard.writeText('hi')");
        page.settle();
        assert_eq!(writes(&mut page), [written("hi", None)]);
    });
}

#[test]
fn write_waits_for_the_blobs_and_sends_every_format() {
    on_page(|| {
        let mut page = Page::new(Options::default());
        page.eval(
            "var done = false;
             navigator.clipboard.write([new ClipboardItem({
                 'text/plain': new Blob(['hi'], { type: 'text/plain' }),
                 'text/html': Promise.resolve(new Blob(['<i>hi</i>'], { type: 'text/html' })),
                 'image/png': new Blob([new Uint8Array([1, 2, 3])], { type: 'image/png' }),
             })]).then(() => { done = true; });",
        );
        page.settle();
        assert_eq!(page.eval("done"), "true");
        assert_eq!(
            writes(&mut page),
            [Request::WriteClipboard {
                text: Some("hi".into()),
                html: Some("<i>hi</i>".into()),
                custom: vec![("image/png".into(), vec![1, 2, 3])],
            }]
        );
    });
}

/// Grants every permission.
struct Grants(Rc<RefCell<u32>>);

impl PermissionBackend for Grants {
    fn ask(&self, _: u64, _: &str, _: &str, _: bool) -> Option<bool> {
        *self.0.borrow_mut() += 1;
        Some(true)
    }

    fn known(&self, _: &str) -> Option<bool> {
        Some(true)
    }
}

fn granting_page() -> Page {
    Page::new(Options {
        url: "http://127.0.0.1:7/p".into(),
        permissions: Some(Box::new(Grants(Rc::default()))),
        ..Options::default()
    })
}

fn reads(page: &mut Page) -> Vec<u64> {
    page.document
        .take_requests()
        .into_iter()
        .filter_map(|r| match r {
            Request::ReadClipboard { call } => Some(call),
            _ => None,
        })
        .collect()
}

#[test]
fn read_text_resolves_with_what_the_shell_answers() {
    on_page(|| {
        let mut page = granting_page();
        page.eval("var out = 'pending'; navigator.clipboard.readText().then((t) => { out = t; })");
        page.settle();
        assert_eq!(page.eval("out"), "pending");
        let calls = reads(&mut page);
        assert_eq!(calls.len(), 1);
        page.document
            .settle_clipboard(&mut page.runtime, calls[0], &text("from the shell"));
        assert_eq!(page.eval("out"), "from the shell");
    });
}

#[test]
fn read_resolves_with_items_for_the_formats_held() {
    on_page(|| {
        let mut page = granting_page();
        page.eval(
            "var out = 'pending';
             navigator.clipboard.read().then(async (items) => {
                 const item = items[0];
                 const html = await (await item.getType('text/html')).text();
                 out = items.length + ' ' + item.types.join() + ' ' + html;
             });",
        );
        page.settle();
        let calls = reads(&mut page);
        let data = ClipboardData {
            text: Some("t".into()),
            html: Some("<b>t</b>".into()),
            image_png: Some(vec![137, 80]),
            ..ClipboardData::default()
        };
        page.document
            .settle_clipboard(&mut page.runtime, calls[0], &data);
        page.settle();
        assert_eq!(
            page.eval("out"),
            "1 text/plain,text/html,image/png <b>t</b>"
        );
    });
}

#[test]
fn an_empty_clipboard_reads_as_no_items() {
    on_page(|| {
        let mut page = granting_page();
        page.eval("var out; navigator.clipboard.read().then((items) => { out = items.length; })");
        page.settle();
        let calls = reads(&mut page);
        page.document
            .settle_clipboard(&mut page.runtime, calls[0], &ClipboardData::default());
        page.settle();
        assert_eq!(page.eval("out"), "0");
    });
}

#[test]
fn a_page_with_no_permission_backend_may_not_read() {
    on_page(|| {
        let mut page = Page::new(Options::default());
        page.eval(
            "var out; navigator.clipboard.read().catch((e) => { out = e.name; });
             var text; navigator.clipboard.readText().catch((e) => { text = e.name; });",
        );
        page.settle();
        assert_eq!(
            page.eval("out + ' ' + text"),
            "NotAllowedError NotAllowedError"
        );
        assert!(reads(&mut page).is_empty());
    });
}

#[test]
fn a_data_transfer_keeps_the_rules_of_its_mode() {
    on_page(|| {
        let mut page = Page::new(Options::default());
        page.eval(
            "const dt = new DataTransfer();
             dt.setData('Text', 'a'); dt.setData('text/html', '<b>a</b>'); dt.setData('URL', 'http://x/\\n#c');
             globalThis.out = [dt.types.join(), dt.getData('text/plain'), dt.getData('url'), dt.items.length,
                 dt.items[0].kind + ':' + dt.items[0].type, dt.getData('missing') === ''].join('|');
             dt.setData('text', 'b'); dt.clearData('text/html');
             out += '#' + dt.types.join() + '#' + dt.getData('text/plain');
             dt.items.add('c', 'text/x-c'); dt.items.add(new File(['xyz'], 'f.txt', { type: 'text/plain' }));
             out += '#' + dt.types.join() + '#' + dt.files.length + dt.items.length;
             dt.items.remove(0); dt.items.clear(); out += '#' + dt.items.length + dt.files.length;",
        );
        assert_eq!(
            page.eval("out"),
            "text/plain,text/html,text/uri-list|a|http://x/|3|string:text/plain|true#text/uri-list,text/plain#b#text/uri-list,text/plain,text/x-c,Files#14#00"
        );
    });
}

#[test]
fn the_copy_and_cut_shortcuts_do_what_the_menu_does() {
    on_page(|| {
        let mut page = page("<input id=i value='hello world'>");
        page.eval("const i = document.getElementById('i'); i.focus(); i.setSelectionRange(6, 11);");
        let mut key = verve_dom::Key::char('c');
        key.modifiers.ctrl = true;
        page.document.key_down(&mut page.runtime, &key);
        assert_eq!(writes(&mut page), [written("world", None)]);
        key = verve_dom::Key::char('x');
        key.modifiers.ctrl = true;
        page.document.key_down(&mut page.runtime, &key);
        assert_eq!(writes(&mut page), [written("world", None)]);
        assert_eq!(page.eval("i.value"), "hello ");
    });
}

#[test]
fn a_script_needs_a_recent_press_to_copy_or_cut() {
    on_page(|| {
        let mut page = page("<div id=ed contenteditable><p id=p>abcdef</p></div>");
        page.eval(
            "document.getElementById('ed').focus(); const t = document.getElementById('p').firstChild;
             const r = document.createRange(); r.setStart(t, 0); r.setEnd(t, 3);
             getSelection().removeAllRanges(); getSelection().addRange(r);",
        );
        // Electron 39.8.10 (p24-review3/out-activation.txt): `queryCommandEnabled` of copy and cut
        // is false until a trusted press or key, and `execCommand` then fires no event.
        assert_eq!(
            page.eval("[document.queryCommandEnabled('copy'), document.queryCommandEnabled('cut'), document.queryCommandSupported('copy')].join()"),
            "false,false,true"
        );
        assert_eq!(page.eval("document.execCommand('copy')"), "false");
        assert_eq!(page.eval("document.execCommand('cut')"), "false");
        assert!(writes(&mut page).is_empty());
        assert_eq!(page.eval("log.join(' ')"), "");
        assert_eq!(
            page.eval("document.getElementById('p').textContent"),
            "abcdef"
        );
        // A person's own copy from the menu needs none.
        assert!(page.document.copy(&mut page.runtime));
        // After a press the script may.
        page.document
            .key_down(&mut page.runtime, &verve_dom::Key::named("Shift"));
        assert_eq!(
            page.eval(
                "[document.queryCommandEnabled('copy'), document.queryCommandEnabled('cut')].join()"
            ),
            "true,true"
        );
        assert_eq!(page.eval("document.execCommand('copy')"), "true");
    });
}

fn ctrl(c: char, shift: bool) -> verve_dom::Key {
    let mut key = verve_dom::Key::char(c);
    key.modifiers.ctrl = true;
    key.modifiers.shift = shift;
    key
}

#[test]
fn ctrl_v_is_a_keydown_whose_default_asks_the_shell_for_the_clipboard() {
    on_page(|| {
        let mut page = page("<input id=i value=ab>");
        page.eval(
            "const i = document.getElementById('i'); i.focus(); i.setSelectionRange(2, 2);
             document.addEventListener('keydown', (e) => log.push('keydown:' + e.key));",
        );
        page.document.key_down(&mut page.runtime, &ctrl('v', false));
        // The page has heard the key; nothing is pasted until the shell answers.
        assert_eq!(page.eval("log.join(' ')"), "keydown:v");
        assert_eq!(page.eval("i.value"), "ab");
        let calls = reads(&mut page);
        assert_eq!(calls.len(), 1);
        page.document
            .settle_clipboard(&mut page.runtime, calls[0], &text("XY"));
        assert_eq!(page.eval("i.value"), "abXY");
        assert_eq!(
            page.eval("log.join(' ')"),
            "keydown:v paste beforeinput:insertFromPaste input:insertFromPaste"
        );
    });
}

#[test]
fn a_canceled_ctrl_v_asks_for_nothing() {
    on_page(|| {
        let mut page = page("<input id=i value=ab>");
        page.eval(
            "document.getElementById('i').focus();
             document.addEventListener('keydown', (e) => e.preventDefault());",
        );
        page.document.key_down(&mut page.runtime, &ctrl('v', false));
        assert!(reads(&mut page).is_empty());
    });
}

#[test]
fn ctrl_shift_v_and_the_menu_paste_the_text_alone() {
    on_page(|| {
        let mut page = page("<div id=ed contenteditable><p id=p>x</p></div>");
        page.eval(
            "document.getElementById('ed').focus(); getSelection().collapse(document.getElementById('p').firstChild, 1);",
        );
        let data = ClipboardData {
            text: Some("t".into()),
            html: Some("<b>bold</b>".into()),
            ..ClipboardData::default()
        };
        page.document.key_down(&mut page.runtime, &ctrl('v', true));
        let calls = reads(&mut page);
        page.document
            .settle_clipboard(&mut page.runtime, calls[0], &data);
        assert_eq!(page.eval("document.getElementById('p').innerHTML"), "xt");
        page.document.request_paste(&mut page.runtime, false);
        let calls = reads(&mut page);
        page.document
            .settle_clipboard(&mut page.runtime, calls[0], &data);
        assert_eq!(
            page.eval("document.getElementById('p').innerHTML"),
            "xt<b>bold</b>"
        );
    });
}

#[test]
fn a_paste_into_a_plaintext_only_host_has_no_transfer() {
    on_page(|| {
        let mut page = page("<div id=ed contenteditable=plaintext-only>x</div>");
        page.eval(
            "document.getElementById('ed').focus(); getSelection().collapse(document.getElementById('ed').firstChild, 1);
             globalThis.seen = [];
             document.addEventListener('beforeinput', (e) => seen.push('before=' + e.data + ' ' + e.dataTransfer));
             document.addEventListener('input', (e) => seen.push('input ' + e.dataTransfer));",
        );
        page.document.paste(&mut page.runtime, &text("yz"));
        assert_eq!(page.eval("seen.join('|')"), "before=yz null|input null");
    });
}

#[test]
fn files_keep_their_paths_even_when_they_are_folders() {
    on_page(|| {
        let dir = std::env::temp_dir().join(format!("verve-paste-dir-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("sub")).expect("a folder");
        let mut page = page("<div id=ed contenteditable><p>x</p></div>");
        page.eval(
            "document.getElementById('ed').focus(); globalThis.seen = [];
             document.addEventListener('paste', (e) => {
                 e.preventDefault();
                 seen.push(e.clipboardData.files.length, e.clipboardData.files[0].name);
             });",
        );
        let data = ClipboardData {
            files: vec![dir.join("sub")],
            ..ClipboardData::default()
        };
        page.document.paste(&mut page.runtime, &data);
        assert_eq!(page.eval("seen.join('|')"), "1|sub");
        std::fs::remove_dir_all(&dir).ok();
    });
}

#[test]
fn write_refuses_more_than_one_item_and_what_it_cannot_write() {
    on_page(|| {
        let mut page = Page::new(Options::default());
        page.eval(
            "var out = [];
             const blob = (t) => new Blob(['x'], { type: t });
             navigator.clipboard.write([new ClipboardItem({ 'text/plain': blob('text/plain') }), new ClipboardItem({ 'text/plain': blob('text/plain') })])
                 .catch((e) => out.push(e.name + ': ' + e.message));
             navigator.clipboard.write([new ClipboardItem({ 'text/x-nothing': blob('text/x-nothing') })])
                 .catch((e) => out.push(e.name));
             try { new ClipboardItem({ 'text/plain': 'a string' }); } catch (e) { out.push(e.name); }",
        );
        page.settle();
        assert_eq!(
            page.eval("out.join('|')"),
            "TypeError|NotAllowedError: Support for multiple ClipboardItems is not implemented.|NotAllowedError"
        );
        assert!(writes(&mut page).is_empty());
    });
}

#[test]
fn an_unfocused_document_may_not_use_the_clipboard() {
    on_page(|| {
        let mut page = Page::new(Options::default());
        page.document.set_window_focus(&mut page.runtime, false);
        page.eval(
            "var out; navigator.clipboard.writeText('x').catch((e) => { out = e.name + ': ' + e.message; });",
        );
        page.settle();
        assert_eq!(
            page.eval("out"),
            "NotAllowedError: Document is not focused."
        );
        assert!(writes(&mut page).is_empty());
    });
}

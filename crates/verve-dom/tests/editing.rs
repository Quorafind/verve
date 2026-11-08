//! Typing and deleting in `contenteditable`, the undo history, and `execCommand`.
//!
//! The event orders and the results were observed in Electron 39.8.10 (Windows 10, a hidden
//! window, keys through `webContents.debugger` and `Input.dispatchKeyEvent`; probe
//! `p24/probe1`):
//!
//! - A typed character is `keydown`, `keypress`, `beforeinput` (cancelable, `inputType`
//!   `insertText`, `getTargetRanges()` the caret), `textInput`, `input`, `keyup`, and then
//!   `selectionchange` as a task of its own. Verve has no `textInput` event.
//! - Enter on `<p>abc|</p>` gives `<p>abc</p><p><br></p>` with the caret at `(P, 0)`; Shift+Enter
//!   gives `<p>abc<br><br></p>` with the caret between the breaks; Backspace at the start of an
//!   empty paragraph gives its target range from the end of the text before it to `(P, 0)`.
//! - `plaintext-only`: Enter inserts a `\n` as `insertLineBreak`, and `getTargetRanges()` is `[]`.
//! - A space at the end of a line (or doubled) is written as `&nbsp;`: Ctrl+Backspace on
//!   `hello world|` leaves `hello&nbsp;`.
//! - `document.execCommand` from a script fires `input` only, with `formatBold`, `formatItalic`,
//!   `formatUnderline`, `formatStrikeThrough` (an `<strike>`), `insertLink` (`data` the address),
//!   `insertText` (`data` the text), `formatRemove`, `historyUndo`/`historyRedo`; `insertHTML`,
//!   `unlink` and `formatBlock` have an empty `inputType`.
//! - `queryCommandSupported`: true for `bold`, `insertText`, `undo`, `copy`, `selectAll`…, false
//!   for `paste` and unknown names; `queryCommandValue('defaultParagraphSeparator')` is `div`.

mod common;

use common::{Page, on_page};
use verve_dom::{Key, Modifiers, Options};

const SHAPE: &str = "(el) => el.innerHTML.replace(/\\u00a0/g, '~').replace(/&nbsp;/g, '~')";

fn page(html: &str) -> Page {
    let mut page = Page::new(Options::default());
    page.eval(&format!(
        "globalThis.shape = {SHAPE}; globalThis.log = [];
         document.body.innerHTML = {html:?};
         globalThis.ed = document.getElementById('ed');
         for (const kind of ['keydown', 'keypress', 'beforeinput', 'input', 'keyup']) {{
             document.addEventListener(kind, (e) => log.push(kind + (e.inputType ? ':' + e.inputType : '') + (e.data ? '=' + e.data : '')), true);
         }}
         ed.focus();"
    ));
    page
}

fn press(page: &mut Page, key: Key) {
    page.document.key_down(&mut page.runtime, &key);
    page.document.key_up(&mut page.runtime, &key);
}

fn chord(name: &str, ctrl: bool, shift: bool) -> Key {
    let mut key = Key::named(name);
    key.modifiers = Modifiers {
        ctrl,
        shift,
        ..Modifiers::default()
    };
    key
}

fn caret(page: &mut Page, script: &str) {
    page.eval(script);
    page.eval("log.length = 0");
}

fn shape(page: &mut Page) -> String {
    page.eval("shape(ed)")
}

fn log(page: &mut Page) -> String {
    let text = page.eval("log.join(' ')");
    page.eval("log.length = 0");
    text
}

#[test]
fn typing_is_keydown_keypress_beforeinput_input_keyup() {
    on_page(|| {
        let mut page = page("<div id=ed contenteditable><p>ab</p><p>cd</p></div>");
        caret(
            &mut page,
            "getSelection().collapse(ed.firstChild.firstChild, 2)",
        );
        press(&mut page, Key::char('c'));
        assert_eq!(
            log(&mut page),
            "keydown keypress beforeinput:insertText=c input:insertText=c keyup"
        );
        assert_eq!(shape(&mut page), "<p>abc</p><p>cd</p>");
        assert_eq!(
            page.eval("var s = getSelection(); s.anchorNode.data + s.anchorOffset"),
            "abc3"
        );
        // beforeinput names the place the text goes, and can be cancelled.
        page.eval(
            "var seen; ed.addEventListener('beforeinput', (e) => { seen = e.getTargetRanges().map(r => r.startOffset + '-' + r.endOffset).join(); e.preventDefault(); });",
        );
        press(&mut page, Key::char('x'));
        assert_eq!(page.eval("seen"), "3-3");
        assert_eq!(shape(&mut page), "<p>abc</p><p>cd</p>");
        assert!(
            !log(&mut page)
                .split(' ')
                .any(|event| event.starts_with("input:"))
        );
    });
}

#[test]
fn text_with_chinese_characters_goes_in_the_region() {
    on_page(|| {
        let mut page = page("<div id=ed contenteditable></div>");
        page.document.type_text(&mut page.runtime, "你好");
        assert_eq!(shape(&mut page), "你好");
        assert_eq!(
            page.eval("ed.firstChild.data + getSelection().anchorOffset"),
            "你好2"
        );
        page.document.insert_text(&mut page.runtime, "!");
        assert_eq!(shape(&mut page), "你好!");
    });
}

#[test]
fn enter_splits_a_block_and_shift_enter_breaks_a_line() {
    on_page(|| {
        let mut page = page("<div id=ed contenteditable><p>abc</p></div>");
        caret(
            &mut page,
            "getSelection().collapse(ed.firstChild.firstChild, 3)",
        );
        press(&mut page, Key::named("Enter"));
        assert_eq!(shape(&mut page), "<p>abc</p><p><br></p>");
        assert_eq!(
            page.eval("var s = getSelection(); s.anchorNode.nodeName + s.anchorOffset"),
            "P0"
        );
        assert!(log(&mut page).contains("beforeinput:insertParagraph input:insertParagraph"));
        // In the middle of a line the rest moves to the new block.
        page.eval(
            "ed.innerHTML = '<p>abcd</p>'; getSelection().collapse(ed.firstChild.firstChild, 2)",
        );
        press(&mut page, Key::named("Enter"));
        assert_eq!(shape(&mut page), "<p>ab</p><p>cd</p>");
        page.eval(
            "ed.innerHTML = '<p>abc</p>'; getSelection().collapse(ed.firstChild.firstChild, 3)",
        );
        press(&mut page, chord("Enter", false, true));
        assert_eq!(shape(&mut page), "<p>abc<br><br></p>");
        assert_eq!(
            page.eval("var s = getSelection(); s.anchorNode.nodeName + s.anchorOffset"),
            "P2"
        );
        assert!(log(&mut page).contains("beforeinput:insertLineBreak"));
    });
}

#[test]
fn text_directly_in_the_host_stays_bare_and_the_new_line_is_a_div() {
    on_page(|| {
        let mut page = page("<div id=ed contenteditable></div>");
        page.document.type_text(&mut page.runtime, "x");
        press(&mut page, Key::named("Enter"));
        assert_eq!(shape(&mut page), "x<div><br></div>");
        press(&mut page, Key::named("Enter"));
        assert_eq!(shape(&mut page), "x<div><br></div><div><br></div>");
    });
}

#[test]
fn backspace_joins_blocks_and_tells_what_it_removes() {
    on_page(|| {
        let mut page = page("<div id=ed contenteditable><p>abc</p><p><br></p><p>cd</p></div>");
        caret(&mut page, "getSelection().collapse(ed.children[1], 0); var seen;
            ed.addEventListener('beforeinput', (e) => { seen = e.getTargetRanges().map(r => [r.startContainer.nodeName, r.startOffset, r.endContainer.nodeName, r.endOffset].join()).join(';'); });");
        press(&mut page, Key::named("Backspace"));
        assert_eq!(shape(&mut page), "<p>abc</p><p>cd</p>");
        // From the end of the text before to the start of the empty paragraph.
        assert_eq!(page.eval("seen"), "#text,3,P,0");
        assert_eq!(
            page.eval("var s = getSelection(); s.anchorNode.data + s.anchorOffset"),
            "abc3"
        );
        assert!(
            log(&mut page)
                .contains("beforeinput:deleteContentBackward input:deleteContentBackward")
        );
        // At the start of a block with text, the two blocks become one.
        caret(
            &mut page,
            "getSelection().collapse(ed.children[1].firstChild, 0)",
        );
        press(&mut page, Key::named("Backspace"));
        assert_eq!(shape(&mut page), "<p>abccd</p>");
        // Inside a text node a grapheme goes (an emoji is one, two UTF-16 units).
        page.eval(
            "ed.innerHTML = '<p>a😀</p>'; getSelection().collapse(ed.firstChild.firstChild, 3)",
        );
        press(&mut page, Key::named("Backspace"));
        assert_eq!(shape(&mut page), "<p>a</p>");
    });
}

#[test]
fn delete_and_word_deletes_follow_the_caret() {
    on_page(|| {
        let mut page = page("<div id=ed contenteditable><p>hello world</p></div>");
        caret(
            &mut page,
            "getSelection().collapse(ed.firstChild.firstChild, 11)",
        );
        press(&mut page, chord("Backspace", true, false));
        // The space left at the end of the line is a no-break space: Electron 39.8.10.
        assert_eq!(shape(&mut page), "<p>hello~</p>");
        assert!(log(&mut page).contains("beforeinput:deleteWordBackward"));
        page.eval("ed.innerHTML = '<p>hello world</p>'; getSelection().collapse(ed.firstChild.firstChild, 0)");
        press(&mut page, Key::named("Delete"));
        assert_eq!(shape(&mut page), "<p>ello world</p>");
        // A selection is deleted whole by either key.
        page.eval("getSelection().setBaseAndExtent(ed.firstChild.firstChild, 1, ed.firstChild.firstChild, 5)");
        press(&mut page, Key::named("Backspace"));
        assert_eq!(shape(&mut page), "<p>eworld</p>");
    });
}

#[test]
fn spaces_that_would_collapse_are_no_break_spaces() {
    on_page(|| {
        let mut page = page("<div id=ed contenteditable></div>");
        page.document.type_text(&mut page.runtime, "a ");
        assert_eq!(shape(&mut page), "a~");
        page.document.type_text(&mut page.runtime, "b");
        assert_eq!(shape(&mut page), "a b");
        page.document.type_text(&mut page.runtime, "  c");
        assert_eq!(shape(&mut page), "a b ~c");
    });
}

#[test]
fn plaintext_only_takes_text_and_newlines() {
    on_page(|| {
        let mut page = page("<div id=ed contenteditable=plaintext-only>xy</div>");
        caret(
            &mut page,
            "getSelection().collapse(ed.firstChild, 1); var seen = []; ed.addEventListener('beforeinput', (e) => seen.push(e.getTargetRanges().length));",
        );
        press(&mut page, Key::named("Enter"));
        assert_eq!(page.eval("ed.textContent"), "x\ny");
        assert!(log(&mut page).contains("beforeinput:insertLineBreak"));
        press(&mut page, Key::char('q'));
        assert_eq!(page.eval("ed.textContent"), "x\nqy");
        // Electron 39.8.10: the target ranges of a plaintext-only host are empty.
        assert_eq!(page.eval("seen.join()"), "0,0");
    });
}

#[test]
fn moving_selecting_and_select_all() {
    on_page(|| {
        let mut page = page("<div id=ed contenteditable><p>one two</p><p>three</p></div>");
        caret(
            &mut page,
            "getSelection().collapse(ed.firstChild.firstChild, 3)",
        );
        press(&mut page, Key::named("ArrowRight"));
        assert_eq!(page.eval("getSelection().focusOffset"), "4");
        press(&mut page, chord("ArrowRight", true, false));
        assert_eq!(page.eval("getSelection().focusOffset"), "7");
        press(&mut page, chord("ArrowLeft", false, true));
        assert_eq!(page.eval("getSelection().toString()"), "o");
        press(&mut page, chord("a", true, false));
        assert_eq!(page.eval("getSelection().toString()"), "one twothree");
        press(&mut page, Key::named("ArrowLeft"));
        assert_eq!(page.eval("getSelection().isCollapsed"), "true");
        // Typing replaces a selection.
        page.eval("getSelection().setBaseAndExtent(ed.firstChild.firstChild, 0, ed.firstChild.firstChild, 3)");
        press(&mut page, Key::char('1'));
        assert_eq!(shape(&mut page), "<p>1 two</p><p>three</p>");
    });
}

#[test]
fn undo_and_redo_walk_the_hosts_history_and_typing_is_one_step() {
    on_page(|| {
        let mut page = page("<div id=ed contenteditable><p>ab</p></div>");
        caret(
            &mut page,
            "getSelection().collapse(ed.firstChild.firstChild, 2)",
        );
        page.document.type_text(&mut page.runtime, "cde");
        press(&mut page, Key::named("Enter"));
        assert_eq!(shape(&mut page), "<p>abcde</p><p><br></p>");
        page.eval("log.length = 0");
        // Electron 39.8.10: typing and the Enter that follows it are one step.
        press(&mut page, chord("z", true, false));
        assert_eq!(shape(&mut page), "<p>ab</p>");
        assert!(log(&mut page).contains("beforeinput:historyUndo input:historyUndo"));
        assert_eq!(page.eval("document.queryCommandEnabled('undo')"), "false");
        press(&mut page, chord("z", true, true));
        assert_eq!(shape(&mut page), "<p>abcde</p><p><br></p>");
        press(&mut page, chord("z", true, false));
        press(&mut page, chord("y", true, false));
        assert_eq!(shape(&mut page), "<p>abcde</p><p><br></p>");
        // Typing after an undo clears the redo.
        press(&mut page, chord("z", true, false));
        page.eval("log.length = 0");
        press(&mut page, Key::char('x'));
        assert_eq!(page.eval("document.queryCommandEnabled('redo')"), "false");
    });
}

#[test]
fn typing_joins_the_open_step_until_the_selection_moves() {
    // Electron 39.8.10, on `<p>ab|</p>`, one Ctrl+Z after each sequence:
    // c, Enter, d gives `ab`; "cd", Backspace, "e" gives `abcd` and a second undo `ab`;
    // c, ArrowLeft, ArrowRight, d gives `abc`; c, a script's collapse(), d gives `abcd`.
    on_page(|| {
        let mut page = page("<div id=ed contenteditable><p>ab</p></div>");
        let reset =
            "ed.innerHTML = '<p>ab</p>'; getSelection().collapse(ed.firstChild.firstChild, 2);";
        let undo = |page: &mut Page| press(page, chord("z", true, false));
        page.eval(reset);
        for key in [Key::char('c'), Key::named("Enter"), Key::char('d')] {
            press(&mut page, key);
        }
        undo(&mut page);
        assert_eq!(shape(&mut page), "<p>ab</p>");
        page.eval(reset);
        for key in [
            Key::char('c'),
            Key::char('d'),
            Key::named("Backspace"),
            Key::char('e'),
        ] {
            press(&mut page, key);
        }
        undo(&mut page);
        assert_eq!(shape(&mut page), "<p>abcd</p>");
        undo(&mut page);
        assert_eq!(shape(&mut page), "<p>ab</p>");
        page.eval(reset);
        for key in [
            Key::char('c'),
            Key::named("ArrowLeft"),
            Key::named("ArrowRight"),
            Key::char('d'),
        ] {
            press(&mut page, key);
        }
        undo(&mut page);
        assert_eq!(shape(&mut page), "<p>abc</p>");
        // A script that moves the selection away and back closes the step too: Electron 39.8.10
        // undoes only the `d` typed after it (the re-review's probe, `undo_script_selection_between_typing`).
        page.eval(reset);
        press(&mut page, Key::char('c'));
        page.eval(
            "var c = getSelection().getRangeAt(0), n = c.startContainer, o = c.startOffset;
             getSelection().collapse(n, 0); getSelection().collapse(n, o)",
        );
        press(&mut page, Key::char('d'));
        undo(&mut page);
        assert_eq!(shape(&mut page), "<p>abc</p>");
    });
}

#[test]
fn a_history_keeps_a_thousand_steps() {
    // Electron 39.8.10: after 1010 steps that do not join (bold toggles), exactly 1000 undos.
    on_page(|| {
        let mut page = page("<div id=ed contenteditable><p>hello</p></div>");
        page.eval("getSelection().selectAllChildren(ed.firstChild)");
        for _ in 0..1010 {
            page.eval("document.execCommand('bold')");
        }
        let undone = page.eval(
            "var n = 0; while (document.queryCommandEnabled('undo') && n < 2000) { document.execCommand('undo'); n++ } n",
        );
        assert_eq!(undone, "1000");
    });
}

#[test]
fn a_beforeinput_handler_that_moves_the_caret_makes_a_delete_follow_it() {
    // Electron 39.8.10: on `<p>abcd|ef</p>` a handler doing collapse(t, 2), then Backspace,
    // gives `<p>acdef</p>` (though getTargetRanges said 3-4).
    on_page(|| {
        let mut page = page("<div id=ed contenteditable><p>abcdef</p></div>");
        page.eval(
            "var t = ed.firstChild.firstChild; getSelection().collapse(t, 4);
             ed.addEventListener('beforeinput', () => getSelection().collapse(t, 2));",
        );
        press(&mut page, Key::named("Backspace"));
        assert_eq!(shape(&mut page), "<p>acdef</p>");
    });
}

#[test]
fn deleting_across_blocks_joins_the_first_line_and_prunes_what_is_left_empty() {
    // Electron 39.8.10 (Backspace unless noted).
    on_page(|| {
        let mut page = page("<div id=ed contenteditable></div>");
        let cases = [
            // `abc<div><div>|def</div></div>`
            (
                "abc<div><div>def</div></div>",
                "ed.lastChild.firstChild.firstChild",
                0,
                "abcdef",
            ),
            // `<div>abc</div><div><p>|def</p></div>`
            (
                "<div>abc</div><div><p>def</p></div>",
                "ed.lastChild.firstChild.firstChild",
                0,
                "<div>abcdef</div>",
            ),
        ];
        for (html, node, offset, shaped) in cases {
            page.eval(&format!(
                "ed.innerHTML = {html:?}; getSelection().collapse({node}, {offset})"
            ));
            press(&mut page, Key::named("Backspace"));
            assert_eq!(shape(&mut page), shaped, "{html}");
        }
        // `abc<div>def</div>` from (ed, 1) to (def, 1).
        page.eval("ed.innerHTML = 'abc<div>def</div>'; var d = ed.lastChild.firstChild; getSelection().setBaseAndExtent(ed, 1, d, 1)");
        press(&mut page, Key::named("Backspace"));
        assert_eq!(shape(&mut page), "abcef");
        // Everything selected leaves a bare line break.
        page.eval("ed.innerHTML = 'abc<div>def</div>'; var d = ed.lastChild.firstChild; getSelection().setBaseAndExtent(ed, 0, d, 3)");
        press(&mut page, Key::named("Backspace"));
        assert_eq!(shape(&mut page), "<br>");
        // `<div><p>abc|</p>def</div>` with Delete, and `<div><p>abc</p>|def</div>` with Backspace.
        page.eval("ed.innerHTML = '<div><p>abc</p>def</div>'; getSelection().collapse(ed.firstChild.firstChild.firstChild, 3)");
        press(&mut page, Key::named("Delete"));
        assert_eq!(shape(&mut page), "<p>abcdef</p>");
        page.eval("ed.innerHTML = '<div><p>abc</p>def</div>'; getSelection().collapse(ed.firstChild.lastChild, 0)");
        press(&mut page, Key::named("Backspace"));
        assert_eq!(shape(&mut page), "<p>abcdef</p>");
        // The break that ends the joined line goes with it.
        page.eval("ed.innerHTML = 'abc<div>def<br>ghi</div>'; getSelection().collapse(ed.lastChild.firstChild, 0)");
        press(&mut page, Key::named("Backspace"));
        assert_eq!(shape(&mut page), "abcdef<div>ghi</div>");
    });
}

#[test]
fn exec_command_formats_and_fires_only_input() {
    on_page(|| {
        let setup = "ed.innerHTML = '<p>hello world</p>'; ed.focus(); var t = ed.firstChild.firstChild; getSelection().setBaseAndExtent(t, 0, t, 5); log.length = 0;";
        let mut page = page("<div id=ed contenteditable></div>");
        for (command, value, html, event) in [
            ("bold", "", "<p><b>hello</b> world</p>", "input:formatBold"),
            (
                "italic",
                "",
                "<p><i>hello</i> world</p>",
                "input:formatItalic",
            ),
            (
                "underline",
                "",
                "<p><u>hello</u> world</p>",
                "input:formatUnderline",
            ),
            (
                "strikeThrough",
                "",
                "<p><strike>hello</strike> world</p>",
                "input:formatStrikeThrough",
            ),
            (
                "createLink",
                "http://x/",
                "<p><a href=\"http://x/\">hello</a> world</p>",
                "input:insertLink=http://x/",
            ),
            ("insertText", "ZZ", "<p>ZZ world</p>", "input:insertText=ZZ"),
            ("formatBlock", "h1", "<h1>hello world</h1>", "input"),
        ] {
            page.eval(setup);
            assert_eq!(
                page.eval(&format!(
                    "document.execCommand({command:?}, false, {value:?})"
                )),
                "true",
                "{command}"
            );
            assert_eq!(shape(&mut page), html, "{command}");
            assert_eq!(log(&mut page), event, "{command}");
        }
        // Bold again takes it off.
        page.eval(setup);
        page.eval("document.execCommand('bold')");
        page.eval("var b = ed.querySelector('b'); getSelection().selectAllChildren(b); log.length = 0; document.execCommand('bold')");
        assert_eq!(shape(&mut page), "<p>hello world</p>");
        assert_eq!(log(&mut page), "input:formatBold");
        // Without an editable selection the editing commands refuse.
        page.eval("getSelection().removeAllRanges()");
        assert_eq!(page.eval("document.execCommand('bold')"), "false");
    });
}

#[test]
fn exec_command_inserts_deletes_and_lists() {
    on_page(|| {
        let setup = "ed.innerHTML = '<p>hello world</p>'; ed.focus(); var t = ed.firstChild.firstChild; getSelection().setBaseAndExtent(t, 0, t, 5);";
        let mut page = page("<div id=ed contenteditable></div>");
        page.eval(setup);
        page.eval("document.execCommand('insertHTML', false, '<b>B</b>t')");
        // The space after the typed text is written as a no-break space (`~`), as Chromium does.
        assert_eq!(shape(&mut page), "<p><b>B</b>t~world</p>");
        page.eval(setup);
        page.eval("document.execCommand('delete')");
        assert_eq!(shape(&mut page), "<p>~world</p>");
        page.eval(setup);
        page.eval("document.execCommand('insertParagraph')");
        assert_eq!(shape(&mut page), "<p><br></p><p>~world</p>");
        page.eval(setup);
        page.eval("document.execCommand('insertOrderedList')");
        assert_eq!(shape(&mut page), "<p><ol><li>hello world</li></ol></p>");
        page.eval(setup);
        page.eval("document.execCommand('bold'); document.execCommand('removeFormat'); log.length = 0; document.execCommand('undo')");
        assert_eq!(shape(&mut page), "<p><b>hello</b> world</p>");
        assert_eq!(log(&mut page), "input:historyUndo");
        page.eval("log.length = 0; document.execCommand('redo')");
        assert_eq!(shape(&mut page), "<p>hello world</p>");
    });
}

#[test]
fn query_command_answers_as_electron_does() {
    on_page(|| {
        let mut page = page("<div id=ed contenteditable><p>hello world</p></div>");
        page.eval("var t = ed.firstChild.firstChild; getSelection().setBaseAndExtent(t, 0, t, 5);");
        assert_eq!(
            page.eval("['bold', 'insertText', 'undo', 'copy', 'selectAll', 'paste', 'foo'].map(c => document.queryCommandSupported(c)).join()"),
            "true,true,true,true,true,false,false"
        );
        assert_eq!(
            page.eval(
                "document.queryCommandEnabled('bold') + ',' + document.queryCommandEnabled('redo')"
            ),
            "true,false"
        );
        assert_eq!(page.eval("document.queryCommandState('bold')"), "false");
        page.eval("document.execCommand('bold')");
        assert_eq!(page.eval("document.queryCommandState('bold')"), "true");
        assert_eq!(
            page.eval("document.queryCommandValue('defaultParagraphSeparator')"),
            "div"
        );
        assert_eq!(page.eval("document.queryCommandValue('formatBlock')"), "p");
        page.eval("getSelection().removeAllRanges()");
        assert_eq!(page.eval("document.queryCommandEnabled('bold')"), "false");
    });
}

#[test]
fn design_mode_makes_the_document_editable() {
    on_page(|| {
        let mut page = Page::new(Options::default());
        page.eval("document.body.innerHTML = '<p>hi</p>'; getSelection().collapse(document.body.firstChild.firstChild, 2);");
        assert_eq!(page.eval("document.designMode"), "off");
        page.document.type_text(&mut page.runtime, "!");
        assert_eq!(page.eval("document.body.innerHTML"), "<p>hi</p>");
        page.eval("document.designMode = 'on'");
        page.document.type_text(&mut page.runtime, "!");
        assert_eq!(page.eval("document.body.innerHTML"), "<p>hi!</p>");
    });
}

#[test]
fn undo_puts_back_only_what_the_step_changed() {
    on_page(|| {
        let mut page = page("<div id=ed contenteditable><p>abc</p></div>");
        caret(
            &mut page,
            "var t = ed.firstChild.firstChild; getSelection().collapse(t, 3)",
        );
        press(&mut page, Key::char('d'));
        // A script changes the same text node and the element around it; the history has none
        // of this.
        page.eval("t.appendData('Z'); ed.firstChild.setAttribute('data-s', '1')");
        assert_eq!(shape(&mut page), "<p data-s=\"1\">abcdZ</p>");
        press(&mut page, chord("z", true, false));
        assert_eq!(shape(&mut page), "<p data-s=\"1\">abcZ</p>");
    });
}

#[test]
fn backspace_and_delete_join_a_block_inside_the_host_with_the_text_before_it() {
    // Electron 39.8.10: `abc<div>def</div>` gives `abcdef` for Backspace at the start of "def"
    // and for Delete at the end of "abc".
    on_page(|| {
        let mut page = page("<div id=ed contenteditable>abc<div>def</div></div>");
        page.eval("getSelection().collapse(ed.lastChild.firstChild, 0)");
        press(&mut page, Key::named("Backspace"));
        assert_eq!(shape(&mut page), "abcdef");
        page.eval("ed.innerHTML = 'abc<div>def</div>'; getSelection().collapse(ed.firstChild, 3)");
        press(&mut page, Key::named("Delete"));
        assert_eq!(shape(&mut page), "abcdef");
    });
}

#[test]
fn enter_in_a_heading_keeps_the_tag_when_text_follows_and_uses_the_separator_at_its_end() {
    // Electron 39.8.10: `<h1>ab|cd</h1>` gives `<h1>ab</h1><h1>cd</h1>`; `<h1>abcd|</h1>` gives
    // `<h1>abcd</h1><div><br></div>`.
    on_page(|| {
        let mut page = page("<div id=ed contenteditable><h1>abcd</h1></div>");
        page.eval("getSelection().collapse(ed.firstChild.firstChild, 2)");
        press(&mut page, Key::named("Enter"));
        assert_eq!(shape(&mut page), "<h1>ab</h1><h1>cd</h1>");
        page.eval(
            "ed.innerHTML = '<h1>abcd</h1>'; getSelection().collapse(ed.firstChild.firstChild, 4)",
        );
        press(&mut page, Key::named("Enter"));
        assert_eq!(shape(&mut page), "<h1>abcd</h1><div><br></div>");
    });
}

#[test]
fn a_cancelled_selectstart_leaves_the_selection_to_select_all() {
    // Electron 39.8.10: Ctrl+A fires a trusted `selectstart`, and cancelling it keeps the
    // selection as it was.
    on_page(|| {
        let mut page = page("<div id=ed contenteditable><p>abc</p></div>");
        page.eval(
            "var trusted = null;
             ed.addEventListener('selectstart', (e) => { trusted = e.isTrusted; e.preventDefault(); });
             getSelection().collapse(ed.firstChild.firstChild, 1);",
        );
        press(&mut page, chord("a", true, false));
        assert_eq!(page.eval("trusted"), "true");
        assert_eq!(
            page.eval("getSelection().isCollapsed + ',' + getSelection().focusOffset"),
            "true,1"
        );
    });
}

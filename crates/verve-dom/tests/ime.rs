//! Input-method composition in text fields and editable regions.
//!
//! The order and the fields were observed in Electron 39.8.10 (Windows 10, a hidden window), with
//! the compositions driven through `webContents.debugger`: `Input.imeSetComposition` for each
//! update, `Input.insertText` to commit, `Input.imeSetComposition` with no text to cancel (probe
//! `p24/probe2`). The same in `<input>`, `<textarea>` and `contenteditable`:
//!
//! - The first update is `compositionstart` (data `""`), `compositionupdate`, `beforeinput`,
//!   `input`; later ones have no `compositionstart`. Both input events are `insertCompositionText`
//!   with `isComposing: true`, and `beforeinput` is not cancelable.
//! - A commit is `compositionupdate` (the committed text), `beforeinput`, `textInput`, `input`
//!   (all `insertCompositionText`, `isComposing: true`), `compositionend`; there is no
//!   `insertText` input. Verve has no `textInput` event.
//! - An empty update (or a cancel) ends the composition: `compositionupdate` with data `""`,
//!   `beforeinput` with data `""`, `input` with data `null`, `compositionend` with data `""`.
//! - `keydown` and `keyup` while composing have `isComposing: true`. That is all Chromium
//!   supplied in the probe: it sent `key: "Process"` and `windowsVirtualKeyCode: 229` itself and
//!   Electron echoed them. Verve answers `key: "Process"` and `keyCode: 229` for a keydown
//!   because UI Events defines `Process` as the key value of a key an input method handles and
//!   Chromium's key code for it is `VKEY_PROCESSKEY` (229); this was not observed.
//! - In `contenteditable` the composed text is a text node in the DOM that scripts see; the first
//!   `getTargetRanges()` is the caret, later ones the composed text. A second composition next to
//!   committed text joins the same text node, and a cancel leaves what was committed.

mod common;

use common::{Page, on_page};
use verve_dom::{Key, Modifiers, Options, Rect};

fn page(body: &str) -> Page {
    let mut page = Page::new(Options::default());
    page.eval(&format!(
        "globalThis.log = []; globalThis.ranges = [];
         document.body.innerHTML = {body:?};
         var el = document.getElementById('el');
         for (const kind of ['keydown', 'keyup', 'compositionstart', 'compositionupdate', 'compositionend', 'beforeinput', 'input']) {{
             document.addEventListener(kind, (e) => {{
                 let line = kind + (e.inputType ? ':' + e.inputType : '') + (kind === 'keydown' || kind === 'keyup' ? ':' + e.key + ':' + e.keyCode : '');
                 if (kind !== 'keydown' && kind !== 'keyup' && e.data) line += '=' + e.data;
                 if (e.isComposing) line += '*';
                 if (kind === 'beforeinput') {{
                     line += e.cancelable ? '(cancelable)' : '';
                     ranges.push(e.getTargetRanges().map(r => r.startContainer.nodeName + r.startOffset + '-' + r.endContainer.nodeName + r.endOffset).join());
                 }}
                 log.push(line);
             }}, true);
         }}
         el.focus();"
    ));
    page
}

fn take(page: &mut Page) -> String {
    let text = page.eval("log.join(' ')");
    page.eval("log.length = 0");
    text
}

const START: &str = "compositionstart compositionupdate=";

#[test]
fn a_text_field_composes_commits_and_cancels() {
    for tag in ["<input id=el>", "<textarea id=el></textarea>"] {
        on_page(|| {
            let mut page = page(tag);
            page.document.compose(&mut page.runtime, "n", Some(1..1));
            assert_eq!(
                take(&mut page),
                format!(
                    "{START}n beforeinput:insertCompositionText=n* input:insertCompositionText=n*"
                )
            );
            assert_eq!(page.eval("el.value + el.selectionStart"), "n1");
            assert!(page.document.is_composing());
            page.document.compose(&mut page.runtime, "ni", Some(2..2));
            assert_eq!(
                take(&mut page),
                "compositionupdate=ni beforeinput:insertCompositionText=ni* input:insertCompositionText=ni*"
            );
            page.document.compose(&mut page.runtime, "你", Some(1..1));
            take(&mut page);
            assert_eq!(page.eval("el.value"), "你");
            // The commit: the text in place of the composition, and the composition over.
            page.document.commit_composition(&mut page.runtime, "你");
            assert_eq!(
                take(&mut page),
                "compositionupdate=你 beforeinput:insertCompositionText=你* input:insertCompositionText=你* compositionend=你"
            );
            assert_eq!(
                page.eval("el.value + el.selectionStart + el.selectionEnd"),
                "你11"
            );
            assert!(!page.document.is_composing());
            // A second composition is cancelled: what it wrote goes, and it ends with an empty
            // `compositionupdate` and an `input` with null data.
            page.document.compose(&mut page.runtime, "hao", Some(3..3));
            assert_eq!(page.eval("el.value + el.selectionStart"), "你hao4");
            take(&mut page);
            page.document.cancel_composition(&mut page.runtime);
            assert_eq!(
                take(&mut page),
                "compositionupdate beforeinput:insertCompositionText* input:insertCompositionText* compositionend"
            );
            assert_eq!(page.eval("el.value + el.selectionStart"), "你1");
            // A selection inside the composition is where the caret sits.
            page.document.compose(&mut page.runtime, "hao", Some(1..3));
            assert_eq!(
                page.eval("el.value + el.selectionStart + el.selectionEnd"),
                "你hao24"
            );
            page.document.commit_composition(&mut page.runtime, "好");
            assert_eq!(page.eval("el.value"), "你好");
        });
    }
}

#[test]
fn keys_during_a_composition_are_process_keys() {
    on_page(|| {
        let mut page = page("<input id=el>");
        page.document.compose(&mut page.runtime, "x", None);
        take(&mut page);
        page.document.key_down(&mut page.runtime, &Key::char('n'));
        page.document.key_up(&mut page.runtime, &Key::char('n'));
        // The keydown is `Process` and 229 (UI Events; see the top of this file), the keyup keeps
        // its key; both are `isComposing` (observed in Electron 39.8.10).
        assert_eq!(take(&mut page), "keydown:Process:229* keyup:n:78*");
        // The keys did not type anything.
        assert_eq!(page.eval("el.value"), "x");
    });
}

#[test]
fn an_editable_region_composes_in_a_text_node_of_the_page() {
    on_page(|| {
        let mut page = page("<div id=el contenteditable></div>");
        page.document.compose(&mut page.runtime, "n", Some(1..1));
        assert_eq!(
            take(&mut page),
            "compositionstart compositionupdate=n beforeinput:insertCompositionText=n* input:insertCompositionText=n*"
        );
        // The caret is the first target range, the composed text the next ones.
        assert_eq!(page.eval("ranges.join('|')"), "DIV0-DIV0");
        assert_eq!(page.eval("el.innerHTML"), "n");
        assert_eq!(
            page.eval("var s = getSelection(); s.anchorNode.data + s.anchorOffset"),
            "n1"
        );
        page.document.compose(&mut page.runtime, "ni", Some(2..2));
        page.document.compose(&mut page.runtime, "你", Some(1..1));
        assert_eq!(
            page.eval("ranges.join('|')"),
            "DIV0-DIV0|#text0-#text1|#text0-#text2"
        );
        assert_eq!(page.eval("el.childNodes.length + el.textContent"), "1你");
        take(&mut page);
        page.document.commit_composition(&mut page.runtime, "你");
        assert_eq!(
            take(&mut page),
            "compositionupdate=你 beforeinput:insertCompositionText=你* input:insertCompositionText=你* compositionend=你"
        );
        assert_eq!(
            page.eval("el.innerHTML + getSelection().anchorOffset"),
            "你1"
        );
        // The next composition joins the text node; cancelling it leaves what was committed.
        page.document.compose(&mut page.runtime, "hao", Some(1..3));
        assert_eq!(page.eval("el.childNodes.length + el.textContent"), "1你hao");
        assert_eq!(
            page.eval("var s = getSelection(); s.anchorOffset + ',' + s.focusOffset"),
            "2,4"
        );
        page.document.cancel_composition(&mut page.runtime);
        assert_eq!(
            page.eval("el.textContent + getSelection().anchorOffset"),
            "你1"
        );
        assert!(!page.document.is_composing());
        // A commit with no composition is plain text.
        page.document.commit_composition(&mut page.runtime, "!");
        assert_eq!(page.eval("el.textContent"), "你!");
    });
}

#[test]
fn the_candidate_window_follows_the_caret() {
    on_page(|| {
        let mut page = page("<input id=el><div id=ce contenteditable></div>");
        let (field, region) = (page.find("#el"), page.find("#ce"));
        page.document.set_layout(
            &mut page.runtime,
            [
                (field, Rect::new(10., 20., 100., 18.)),
                (region, Rect::new(10., 60., 200., 40.)),
            ],
        );
        assert_eq!(
            page.document.caret_rect(),
            Some(Rect::new(10., 20., 100., 18.))
        );
        page.eval("document.getElementById('ce').focus()");
        page.document.compose(&mut page.runtime, "n", None);
        assert_eq!(
            page.document.caret_rect(),
            Some(Rect::new(10., 60., 200., 40.))
        );
        // Boxes that moved (the page scrolled) are not answered from the old place.
        page.document.update_layout(
            &mut page.runtime,
            [(region, Rect::new(10., 10., 200., 40.))],
        );
        assert_eq!(
            page.document.caret_rect(),
            Some(Rect::new(10., 10., 200., 40.))
        );
    });
}

#[test]
fn a_commit_joins_the_typing_before_it_in_the_undo_history() {
    // Electron 39.8.10: "ab" typed, an input method's commit of "你", then one Ctrl+Z gives "".
    on_page(|| {
        let mut page = page("<div id=el contenteditable></div>");
        page.document.type_text(&mut page.runtime, "ab");
        page.document.compose(&mut page.runtime, "n", Some(1..1));
        page.document.commit_composition(&mut page.runtime, "你");
        assert_eq!(page.eval("el.innerHTML"), "ab你");
        let mut undo = Key::named("z");
        undo.modifiers = Modifiers {
            ctrl: true,
            ..Modifiers::default()
        };
        page.document.key_down(&mut page.runtime, &undo);
        page.document.key_up(&mut page.runtime, &undo);
        assert_eq!(page.eval("el.innerHTML"), "");
    });
}

#[test]
fn a_field_hears_selectionchange_after_a_composition_step() {
    // Electron 39.8.10: the event goes to the `<input>` itself, bubbles, is trusted and not
    // composed, and comes after the microtasks the step's `input` listener queued.
    on_page(|| {
        let mut page = page("<input id=el>");
        page.eval(
            "globalThis.heard = [];
             document.addEventListener('selectionchange', (e) => heard.push(e.target.id + ':' + e.bubbles + ':' + e.isTrusted));",
        );
        page.eval(
            "document.addEventListener('input', () => Promise.resolve().then(() => heard.push('microtask')));
             document.addEventListener('selectionchange', (e) => heard.push('composed=' + e.composed));",
        );
        page.document.compose(&mut page.runtime, "n", Some(1..1));
        // After the input listener's microtask, and not composed (Electron 39.8.10).
        assert_eq!(
            page.eval("heard.join()"),
            "microtask,el:true:true,composed=false"
        );
    });
}

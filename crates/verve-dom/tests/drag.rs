//! HTML5 drag and drop: inside the page, and files dragged in from the system.
//!
//! The orders and fields below were observed in Electron 39.8.10 (Windows 10, a hidden window; the
//! drag driven by `webContents.debugger` with `Input.setInterceptDrags`, `Input.dispatchMouseEvent`
//! and `Input.dispatchDragEvent`; probe `p24/probe4`, a draggable 100x60 box at (10, 10) and a drop
//! target at (300, 10)):
//!
//! - Press at (20, 20), move to (22, 30): `pointerdown mousedown pointermove mousemove` then
//!   `dragstart` (target the box, `clientX/Y` 20/20, `buttons` 1, `effectAllowed` `uninitialized`,
//!   `types` empty, cancelable, bubbling) then `pointercancel` (not cancelable). Three 1-pixel moves
//!   from the press are no drag: `pointermove`/`mousemove` each, `pointerup`, `mouseup`, `click`.
//!   Presses on a button, a link, a draggable element or `user-select: none` text leave an existing
//!   selection alone; a press on text is `selectstart` (at the text, cancelable: the old selection
//!   is then kept) and a caret (the review's probe, `p24-review3/out-select.txt`).
//! - After that the page hears no `pointermove`. A move to the target is `drag` (at the box),
//!   `dragenter`, `dragover` (at the target); a second move there is `drag`, `dragover`.
//! - Letting go over an element whose `dragover` was canceled is `drag`, `dragover`, `drop`, then
//!   `dragend` (at the box, `dropEffect` the effect it ended with, `copy` there). The probe also
//!   heard `pointerup` and `mouseup` after the drop, but only because its release was dispatched
//!   by hand after the intercepted drag; a native drag's loop swallows the release (from source,
//!   not observed), so none is sent, and there is no `click` (none was heard).
//! - `dragenter` and `dragover` list `types` (`text/plain,text/x-custom`) and `items`
//!   (`string:text/plain`…); `dropEffect` is `none` in every event but `dragend`; `dragend` and
//!   `dragleave` are not cancelable.
//! - Moving onto the document element: `dragenter` there (with `relatedTarget` the old target),
//!   then `dragleave` on the old one (with `relatedTarget` the new), and no `dragover`. A drag
//!   let go over a target that had not accepted it is `drag`, `dragleave`, `dragend` with
//!   `dropEffect` `none` (the review's probe, `p24-review3/out-effects.txt`); `dragCancel` alone
//!   gave only `dragend`, so Escape's `drag`, `dragleave`, `dragend` is from the source.
//! - From the system (`Input.dispatchDragEvent` with two files and a string): `dragenter` alone,
//!   then one `dragover` for each further move; `types` is `text/plain,Files`; `items` is `string:text/plain` then one
//!   `file:text/plain` per file; `files.length` is 0 until the drop; at the drop `dragover` is heard
//!   again first, then `drop` with 2 files named `a.txt` and `b.txt` of 6 and 7 bytes and type
//!   `text/plain`, and `'path' in file` is false. Unaccepted, the drop is `dragover`, `dragover`,
//!   `dragleave`, and the page does not navigate: with `navigateOnDragDrop` at its default of
//!   `false` Electron 39.8.10 fires no `will-navigate` and opens no window
//!   (`p24-review3/out-navdrop.txt`; the review's probe).

mod common;

use common::{Page, on_page};
use verve_dom::{DropEffect, ExternalDrag, Options, Pointer, Rect, Request};

fn at(x: f32, y: f32) -> Pointer {
    Pointer {
        x,
        y,
        count: 1,
        ..Pointer::default()
    }
}

const WATCH: &str = "
    globalThis.log = [];
    const kinds = ['pointerdown', 'mousedown', 'pointermove', 'mousemove', 'pointerup', 'mouseup',
        'pointercancel', 'click', 'dragstart', 'drag', 'dragenter', 'dragover', 'dragleave', 'drop',
        'dragend'];
    for (const kind of kinds) {
        document.addEventListener(kind, (e) => {
            const name = e.target === document.documentElement || e.target === document.body ? 'BODY' : e.target.id;
            log.push(kind + ':' + name);
            if (globalThis.detail) detail(kind, e);
        }, true);
    }";

fn page(extra: &str) -> Page {
    let mut page = Page::new(Options::default());
    page.eval(&format!(
        "document.body.innerHTML = '<div id=\"src\" draggable=\"true\">drag me</div><div id=\"dst\">drop here</div>';
         {WATCH} {extra}"
    ));
    let dom = page.document.dom();
    let layout = [
        (
            dom.query(dom.document, "#src").unwrap(),
            Rect::new(10., 10., 100., 60.),
        ),
        (
            dom.query(dom.document, "#dst").unwrap(),
            Rect::new(300., 10., 200., 100.),
        ),
    ];
    drop(dom);
    page.document.set_layout(&mut page.runtime, layout);
    page
}

fn heard(page: &mut Page) -> String {
    let said = page.eval("log.join(' ')");
    page.eval("log.length = 0");
    said
}

/// Listeners that accept every drag over `#dst` and record the transfer's fields.
const ACCEPTING: &str = "
    globalThis.fields = [];
    document.addEventListener('dragstart', (e) => {
        fields.push('start types=' + e.dataTransfer.types.join() + ' allowed=' + e.dataTransfer.effectAllowed + ' effect=' + e.dataTransfer.dropEffect
            + ' x=' + e.clientX + ',' + e.clientY + ' buttons=' + e.buttons + ' cancelable=' + e.cancelable);
        e.dataTransfer.setData('text/plain', 'payload');
        e.dataTransfer.setData('text/x-custom', 'c');
        e.dataTransfer.effectAllowed = 'copyMove';
        e.dataTransfer.setDragImage(e.target, 3, 4);
        fields.push('set types=' + e.dataTransfer.types.join() + ' items=' + Array.from(e.dataTransfer.items).map((i) => i.kind + ':' + i.type).join());
    });
    document.getElementById('dst').addEventListener('dragenter', (e) => e.preventDefault());
    document.getElementById('dst').addEventListener('dragover', (e) => {
        e.preventDefault();
        e.dataTransfer.dropEffect = 'copy';
    });
    globalThis.detail = (kind, e) => {
        if (kind === 'dragenter' || kind === 'drop' || kind === 'dragend') {
            fields.push(kind + ' types=' + e.dataTransfer.types.join() + ' effect=' + e.dataTransfer.dropEffect
                + ' files=' + e.dataTransfer.files.length + ' data=' + e.dataTransfer.getData('text/plain')
                + ' cancelable=' + e.cancelable);
        }
    };";

#[test]
fn a_drag_inside_the_page_runs_from_dragstart_to_dragend() {
    on_page(|| {
        let mut page = page(ACCEPTING);
        let doc = page.document.clone();
        doc.pointer_down(&mut page.runtime, &at(20., 20.));
        assert_eq!(heard(&mut page), "pointerdown:src mousedown:src");
        // The move that crosses the threshold is still heard; then the drag starts.
        doc.pointer_move(&mut page.runtime, &at(22., 30.));
        assert_eq!(
            heard(&mut page),
            "pointermove:src mousemove:src dragstart:src pointercancel:src"
        );
        assert!(doc.dragging());
        assert_eq!(doc.drag_image_hot_spot(), Some((3, 4)));
        // From here on the page hears no pointer moves.
        doc.pointer_move(&mut page.runtime, &at(120., 30.));
        assert_eq!(heard(&mut page), "drag:src dragenter:BODY dragover:BODY");
        doc.pointer_move(&mut page.runtime, &at(350., 30.));
        assert_eq!(heard(&mut page), "drag:src dragenter:dst dragleave:BODY");
        doc.pointer_move(&mut page.runtime, &at(352., 31.));
        assert_eq!(heard(&mut page), "drag:src dragover:dst");
        doc.pointer_up(&mut page.runtime, &at(352., 31.));
        assert_eq!(
            heard(&mut page),
            "drag:src dragover:dst drop:dst dragend:src"
        );
        assert!(!doc.dragging());
        assert_eq!(
            page.eval("fields.join('\\n')"),
            "start types= allowed=uninitialized effect=none x=20,20 buttons=1 cancelable=true\n\
             set types=text/plain,text/x-custom items=string:text/plain,string:text/x-custom\n\
             dragenter types=text/plain,text/x-custom effect=none files=0 data= cancelable=true\n\
             dragenter types=text/plain,text/x-custom effect=none files=0 data= cancelable=true\n\
             drop types=text/plain,text/x-custom effect=none files=0 data=payload cancelable=true\n\
             dragend types=text/plain,text/x-custom effect=copy files=0 data= cancelable=false"
        );
    });
}

#[test]
fn a_press_that_hardly_moves_is_a_click() {
    on_page(|| {
        let mut page = page("");
        let doc = page.document.clone();
        doc.pointer_down(&mut page.runtime, &at(20., 20.));
        heard(&mut page);
        for x in [21., 22., 23.] {
            doc.pointer_move(&mut page.runtime, &at(x, 20.));
        }
        doc.pointer_up(&mut page.runtime, &at(23., 20.));
        assert_eq!(
            heard(&mut page),
            "pointermove:src mousemove:src pointermove:src mousemove:src pointermove:src mousemove:src pointerup:src mouseup:src click:src"
        );
        assert!(!doc.dragging());
    });
}

#[test]
fn a_canceled_dragstart_leaves_the_pointer_to_the_page() {
    on_page(|| {
        let mut page = page("document.addEventListener('dragstart', (e) => e.preventDefault());");
        let doc = page.document.clone();
        doc.pointer_down(&mut page.runtime, &at(20., 20.));
        doc.pointer_move(&mut page.runtime, &at(30., 30.));
        doc.pointer_move(&mut page.runtime, &at(40., 30.));
        assert!(!doc.dragging());
        assert_eq!(
            heard(&mut page),
            "pointerdown:src mousedown:src pointermove:src mousemove:src dragstart:src pointermove:src mousemove:src"
        );
    });
}

#[test]
fn a_drag_nobody_accepts_leaves_and_ends_with_no_effect() {
    on_page(|| {
        let mut page = page(
            "globalThis.fields = [];
             globalThis.detail = (kind, e) => { if (kind === 'dragend') fields.push(e.dataTransfer.dropEffect); };",
        );
        let doc = page.document.clone();
        doc.pointer_down(&mut page.runtime, &at(20., 20.));
        doc.pointer_move(&mut page.runtime, &at(22., 30.));
        doc.pointer_move(&mut page.runtime, &at(350., 30.));
        heard(&mut page);
        doc.pointer_move(&mut page.runtime, &at(352., 31.));
        doc.pointer_up(&mut page.runtime, &at(352., 31.));
        assert_eq!(
            heard(&mut page),
            "drag:src dragover:dst drag:src dragleave:dst dragend:src"
        );
        assert_eq!(page.eval("fields.join()"), "none");
    });
}

#[test]
fn escape_ends_the_drag_where_it_is() {
    on_page(|| {
        let mut page = page(ACCEPTING);
        let doc = page.document.clone();
        doc.pointer_down(&mut page.runtime, &at(20., 20.));
        doc.pointer_move(&mut page.runtime, &at(22., 30.));
        doc.pointer_move(&mut page.runtime, &at(350., 30.));
        heard(&mut page);
        doc.key_down(&mut page.runtime, &verve_dom::Key::named("Escape"));
        assert_eq!(heard(&mut page), "drag:src dragleave:dst dragend:src");
        assert!(!doc.dragging());
    });
}

#[test]
fn a_drop_effect_the_source_does_not_allow_is_not_a_drop() {
    on_page(|| {
        let mut page = page(
            "document.addEventListener('dragstart', (e) => { e.dataTransfer.effectAllowed = 'move'; });
             document.getElementById('dst').addEventListener('dragover', (e) => { e.preventDefault(); e.dataTransfer.dropEffect = 'copy'; });",
        );
        let doc = page.document.clone();
        doc.pointer_down(&mut page.runtime, &at(20., 20.));
        doc.pointer_move(&mut page.runtime, &at(22., 30.));
        doc.pointer_move(&mut page.runtime, &at(350., 30.));
        doc.pointer_up(&mut page.runtime, &at(350., 30.));
        assert!(!page.eval("log.join(' ')").contains("drop:dst"));
    });
}

#[test]
fn a_link_is_dragged_with_its_address() {
    on_page(|| {
        let mut page = page(
            "document.body.insertAdjacentHTML('beforeend', '<a id=\"link\" href=\"http://x/y\">y</a>');
             globalThis.seen = [];
             document.addEventListener('dragstart', (e) => seen.push(e.dataTransfer.getData('text/uri-list') + ' ' + e.dataTransfer.types.join()));",
        );
        let link = page.find("#link");
        page.document
            .set_layout(&mut page.runtime, [(link, Rect::new(10., 200., 40., 16.))]);
        let doc = page.document.clone();
        doc.pointer_down(&mut page.runtime, &at(20., 205.));
        doc.pointer_move(&mut page.runtime, &at(30., 215.));
        assert!(doc.dragging());
        assert_eq!(
            page.eval("seen.join()"),
            "http://x/y text/uri-list,text/plain"
        );
    });
}

fn files(page_dir: &std::path::Path) -> Vec<std::path::PathBuf> {
    std::fs::create_dir_all(page_dir).expect("a folder");
    let (a, b) = (page_dir.join("a.txt"), page_dir.join("b.txt"));
    std::fs::write(&a, "hello\n").expect("a file");
    std::fs::write(&b, "world!\n").expect("a file");
    vec![a, b]
}

const EXTERNAL: &str = "
    globalThis.fields = [];
    const dst = document.getElementById('dst');
    dst.addEventListener('dragenter', (e) => e.preventDefault());
    dst.addEventListener('dragover', (e) => e.preventDefault());
    globalThis.detail = (kind, e) => {
        if (['dragenter', 'dragover', 'drop'].includes(kind)) {
            const files = Array.from(e.dataTransfer.files);
            fields.push(kind + ' types=' + e.dataTransfer.types.join() + ' items=' + Array.from(e.dataTransfer.items).map((i) => i.kind + ':' + i.type).join()
                + ' files=' + files.length + ' effect=' + e.dataTransfer.dropEffect + ' data=' + e.dataTransfer.getData('text/plain')
                + ' ' + files.map((f) => f.name + ':' + f.size + ':' + f.type + ':' + ('path' in f)).join());
        }
    };";

#[test]
fn files_dragged_in_from_the_system_are_listed_and_then_readable() {
    on_page(|| {
        let dir = std::env::temp_dir().join(format!("verve-drag-{}", std::process::id()));
        let paths = files(&dir);
        let mut page = page(EXTERNAL);
        let doc = page.document.clone();
        let data = ExternalDrag {
            files: paths,
            text: Some("dragged text".into()),
            ..ExternalDrag::default()
        };
        let effect = doc.drag_enter(&mut page.runtime, data, &at(350., 30.));
        assert_eq!(effect, DropEffect::Copy);
        assert_eq!(heard(&mut page), "dragenter:dst");
        assert_eq!(
            doc.drag_over(&mut page.runtime, &at(352., 31.)),
            DropEffect::Copy
        );
        assert_eq!(heard(&mut page), "dragover:dst");
        assert_eq!(
            doc.drop(&mut page.runtime, &at(352., 31.)),
            DropEffect::Copy
        );
        assert_eq!(heard(&mut page), "dragover:dst drop:dst");
        assert_eq!(
            page.eval("fields.join('\\n')"),
            "dragenter types=text/plain,Files items=string:text/plain,file:text/plain,file:text/plain files=0 effect=none data= \n\
             dragover types=text/plain,Files items=string:text/plain,file:text/plain,file:text/plain files=0 effect=none data= \n\
             dragover types=text/plain,Files items=string:text/plain,file:text/plain,file:text/plain files=0 effect=none data= \n\
             drop types=text/plain,Files items=string:text/plain,file:text/plain,file:text/plain files=2 effect=none data=dragged text a.txt:6:text/plain:false,b.txt:7:text/plain:false"
        );
        assert!(!doc.dragging());
        std::fs::remove_dir_all(&dir).ok();
    });
}

#[test]
fn a_drop_nobody_accepts_does_not_navigate() {
    on_page(|| {
        let dir = std::env::temp_dir().join(format!("verve-drag-nav-{}", std::process::id()));
        let paths = files(&dir);
        let mut page = page("");
        let doc = page.document.clone();
        let data = ExternalDrag {
            files: vec![paths[0].clone()],
            ..ExternalDrag::default()
        };
        assert_eq!(
            doc.drag_enter(&mut page.runtime, data, &at(650., 400.)),
            DropEffect::None
        );
        assert_eq!(heard(&mut page), "dragenter:BODY");
        assert_eq!(
            doc.drop(&mut page.runtime, &at(650., 400.)),
            DropEffect::None
        );
        assert_eq!(heard(&mut page), "dragover:BODY dragleave:BODY");
        // Electron 39.8.10 with `navigateOnDragDrop` at its default: no navigation.
        assert!(
            !page
                .document
                .take_requests()
                .iter()
                .any(|r| matches!(r, Request::Navigate { .. } | Request::Open { .. }))
        );
        std::fs::remove_dir_all(&dir).ok();
    });
}

#[test]
fn a_page_that_handles_the_drop_does_not_navigate() {
    on_page(|| {
        let dir = std::env::temp_dir().join(format!("verve-drag-handled-{}", std::process::id()));
        let paths = files(&dir);
        let mut page = page(EXTERNAL);
        let doc = page.document.clone();
        let data = ExternalDrag {
            files: paths,
            ..ExternalDrag::default()
        };
        doc.drag_enter(&mut page.runtime, data, &at(350., 30.));
        doc.drop(&mut page.runtime, &at(350., 30.));
        assert!(
            !page
                .document
                .take_requests()
                .iter()
                .any(|r| matches!(r, Request::Navigate { .. }))
        );
        std::fs::remove_dir_all(&dir).ok();
    });
}

#[test]
fn leaving_the_window_is_a_dragleave() {
    on_page(|| {
        let mut page = page(EXTERNAL);
        let doc = page.document.clone();
        let data = ExternalDrag {
            text: Some("t".into()),
            ..ExternalDrag::default()
        };
        doc.drag_enter(&mut page.runtime, data, &at(350., 30.));
        heard(&mut page);
        doc.drag_leave(&mut page.runtime);
        assert_eq!(heard(&mut page), "dragleave:dst");
        assert!(!doc.dragging());
    });
}

#[test]
fn a_listener_chooses_the_drop_effect() {
    on_page(|| {
        let mut page = page(
            "const dst = document.getElementById('dst');
             dst.addEventListener('dragenter', (e) => e.preventDefault());
             dst.addEventListener('dragover', (e) => { e.preventDefault(); e.dataTransfer.dropEffect = 'link'; });",
        );
        let doc = page.document.clone();
        let data = ExternalDrag {
            uri_list: vec!["http://x/".into()],
            ..ExternalDrag::default()
        };
        // `dragenter` alone: the effect is the default one, copy for an unrestricted source.
        assert_eq!(
            doc.drag_enter(&mut page.runtime, data, &at(350., 30.)),
            DropEffect::Copy
        );
        // The `dragover` is where the listener chooses.
        assert_eq!(
            doc.drag_over(&mut page.runtime, &at(352., 31.)),
            DropEffect::Link
        );
    });
}

/// The effect `dragend` reports for a drag whose target cancels `dragover` and sets nothing, for
/// each `effectAllowed` (Electron 39.8.10, `p24-review3/out-effects.txt`).
#[test]
fn the_default_effect_follows_the_effect_allowed() {
    on_page(|| {
        for (allowed, expected) in [
            ("copyMove", "move"),
            ("all", "copy"),
            ("uninitialized", "copy"),
            ("linkMove", "move"),
            ("copyLink", "copy"),
            ("link", "link"),
        ] {
            let mut page = page(&format!(
                "globalThis.ended = [];
                 document.addEventListener('dragstart', (e) => {{ if ('{allowed}' !== 'uninitialized') e.dataTransfer.effectAllowed = '{allowed}'; }});
                 const dst = document.getElementById('dst');
                 dst.addEventListener('dragenter', (e) => e.preventDefault());
                 dst.addEventListener('dragover', (e) => e.preventDefault());
                 document.addEventListener('dragend', (e) => ended.push(e.dataTransfer.dropEffect));"
            ));
            let doc = page.document.clone();
            doc.pointer_down(&mut page.runtime, &at(20., 20.));
            doc.pointer_move(&mut page.runtime, &at(22., 30.));
            doc.pointer_move(&mut page.runtime, &at(350., 30.));
            doc.pointer_move(&mut page.runtime, &at(352., 31.));
            doc.pointer_up(&mut page.runtime, &at(352., 31.));
            assert_eq!(page.eval("ended.join()"), expected, "{allowed}");
        }
    });
}

#[test]
fn an_explicit_none_is_no_drop() {
    on_page(|| {
        let mut page = page(
            "globalThis.ended = [];
             const dst = document.getElementById('dst');
             dst.addEventListener('dragenter', (e) => e.preventDefault());
             dst.addEventListener('dragover', (e) => { e.preventDefault(); e.dataTransfer.dropEffect = 'none'; });
             document.addEventListener('dragend', (e) => ended.push(e.dataTransfer.dropEffect));",
        );
        let doc = page.document.clone();
        doc.pointer_down(&mut page.runtime, &at(20., 20.));
        doc.pointer_move(&mut page.runtime, &at(22., 30.));
        doc.pointer_move(&mut page.runtime, &at(350., 30.));
        doc.pointer_up(&mut page.runtime, &at(350., 30.));
        assert!(!page.eval("log.join(' ')").contains("drop:dst"));
        assert_eq!(page.eval("ended.join()"), "none");
    });
}

#[test]
fn a_press_on_text_selects_as_the_pointer_moves() {
    on_page(|| {
        let mut page = page("");
        page.eval("document.body.insertAdjacentHTML('beforeend', '<p id=\"p\">hello world</p>');");
        let (p, text) = {
            let dom = page.document.dom();
            let p = dom.query(dom.document, "#p").unwrap();
            (p, dom.children(p)[0])
        };
        page.document
            .set_layout(&mut page.runtime, [(p, Rect::new(0., 100., 200., 20.))]);
        // Ten pixels to a character.
        page.document
            .set_position_provider(Some(Box::new(move |x, _| Some((text, (x / 10.) as usize)))));
        let doc = page.document.clone();
        doc.pointer_down(&mut page.runtime, &at(15., 105.));
        assert_eq!(
            page.eval("getSelection().isCollapsed + ' ' + getSelection().anchorOffset"),
            "true 1"
        );
        doc.pointer_move(&mut page.runtime, &at(55., 105.));
        assert_eq!(page.eval("getSelection().toString()"), "ello");
        doc.pointer_move(&mut page.runtime, &at(25., 105.));
        assert_eq!(page.eval("getSelection().toString()"), "e");
        doc.pointer_move(&mut page.runtime, &at(5., 105.));
        assert_eq!(page.eval("getSelection().toString()"), "h");
        doc.pointer_move(&mut page.runtime, &at(55., 105.));
        doc.pointer_up(&mut page.runtime, &at(55., 105.));
        // Let go: it stays where it was.
        doc.pointer_move(&mut page.runtime, &at(95., 105.));
        assert_eq!(page.eval("getSelection().toString()"), "ello");
        // Shift reaches from the old anchor.
        let mut shifted = at(85., 105.);
        shifted.modifiers.shift = true;
        doc.pointer_down(&mut page.runtime, &shifted);
        doc.pointer_up(&mut page.runtime, &shifted);
        assert_eq!(page.eval("getSelection().toString()"), "ello wo");
    });
}

#[test]
fn a_press_that_drags_an_element_does_not_select() {
    on_page(|| {
        let mut page = page("");
        page.document
            .set_position_provider(Some(Box::new(|_, _| None)));
        let doc = page.document.clone();
        page.eval("getSelection().selectAllChildren(document.getElementById('dst'))");
        doc.pointer_down(&mut page.runtime, &at(20., 20.));
        doc.pointer_move(&mut page.runtime, &at(22., 30.));
        assert!(doc.dragging());
        assert_eq!(page.eval("getSelection().toString()"), "drop here");
    });
}

/// A page with a paragraph of ten-pixel characters and a button, and a selection to keep.
fn selecting_page() -> Page {
    let mut page = page("");
    page.eval(
        "document.body.insertAdjacentHTML('beforeend', '<p id=\"p\">hello world</p><button id=\"b\">go</button><div id=\"empty\"></div>');
         globalThis.heard = [];
         document.addEventListener('selectstart', (e) => heard.push('selectstart:' + e.target.nodeName + ':' + e.cancelable));
         document.addEventListener('selectionchange', () => heard.push('selectionchange'));",
    );
    let (p, text, b, empty) = {
        let dom = page.document.dom();
        let p = dom.query(dom.document, "#p").unwrap();
        let (b, empty) = (
            dom.query(dom.document, "#b").unwrap(),
            dom.query(dom.document, "#empty").unwrap(),
        );
        (p, dom.children(p)[0], b, empty)
    };
    page.document.set_layout(
        &mut page.runtime,
        [
            (p, Rect::new(0., 100., 200., 20.)),
            (b, Rect::new(0., 200., 50., 20.)),
            (empty, Rect::new(0., 300., 50., 20.)),
        ],
    );
    page.document
        .set_position_provider(Some(Box::new(move |x, y| {
            (y < 150.).then(|| (text, (x / 10.) as usize))
        })));
    page
}

#[test]
fn a_press_on_a_button_leaves_the_selection() {
    on_page(|| {
        let mut page = selecting_page();
        page.eval("getSelection().selectAllChildren(document.getElementById('p'))");
        let doc = page.document.clone();
        doc.pointer_down(&mut page.runtime, &at(10., 205.));
        doc.pointer_up(&mut page.runtime, &at(10., 205.));
        assert_eq!(page.eval("getSelection().toString()"), "hello world");
    });
}

#[test]
fn a_press_on_text_fires_a_cancelable_selectstart_first() {
    on_page(|| {
        let mut page = selecting_page();
        let doc = page.document.clone();
        doc.pointer_down(&mut page.runtime, &at(15., 105.));
        page.settle();
        assert_eq!(
            page.eval("heard.join(' ')"),
            "selectstart:#text:true selectionchange"
        );
    });
}

#[test]
fn a_canceled_selectstart_keeps_the_old_selection_through_the_drag() {
    on_page(|| {
        let mut page = selecting_page();
        page.eval(
            "getSelection().selectAllChildren(document.getElementById('b'));
             document.addEventListener('selectstart', (e) => e.preventDefault());",
        );
        let doc = page.document.clone();
        doc.pointer_down(&mut page.runtime, &at(15., 105.));
        doc.pointer_move(&mut page.runtime, &at(55., 105.));
        doc.pointer_up(&mut page.runtime, &at(55., 105.));
        assert_eq!(page.eval("getSelection().toString()"), "go");
    });
}

#[test]
fn a_press_where_there_is_no_text_puts_the_caret_in_the_element() {
    on_page(|| {
        let mut page = selecting_page();
        page.eval("getSelection().selectAllChildren(document.getElementById('p'))");
        let doc = page.document.clone();
        doc.pointer_down(&mut page.runtime, &at(10., 305.));
        assert_eq!(
            page.eval("getSelection().isCollapsed + ' ' + getSelection().anchorNode.id"),
            "true empty"
        );
    });
}

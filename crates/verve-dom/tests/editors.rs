//! Real editors on the document: ProseMirror, Lexical and CodeMirror 6, typed into with the keys
//! and the input method a person has, and read back through their own state.
//!
//! Ignored by default: the packages are installed with `verve::npm` the first time, into
//! `VERVE_DOM_EDITORS` or, without it, the test target's own scratch directory, so these tests
//! reach the npm registry. Run with `cargo test -p verve-dom --test editors -- --ignored`.
//!
//! CodeMirror measures its text with the browser's layout. These tests have none, so
//! [`fixed_layout`] reports one of fixed metrics: every line is 20 pixels tall and every
//! character 8 pixels wide, in a column 800 pixels wide. It is laid out again after each key,
//! since CodeMirror reads the boxes it needs after each update.

mod common;

use std::path::{Path, PathBuf};

use common::{Page, on_page};
use verve_dom::{Key, Modifiers, NodeId, Options, Rect};

const LINE: f32 = 20.;
const CHAR: f32 = 8.;

/// The project the packages are installed in, installed if it is not yet.
fn prepared(name: &str, dependencies: serde_json::Value, fixture: &str) -> PathBuf {
    let dir = std::env::var_os("VERVE_DOM_EDITORS")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("editors"))
        .join(name);
    let package = serde_json::json!({
        "name": format!("verve-dom-editors-{name}"),
        "private": true,
        "dependencies": dependencies,
    });
    let manifest = serde_json::to_string_pretty(&package).unwrap();
    let installed = std::fs::read_to_string(dir.join("package.json")).ok();
    if !dir.join("node_modules").exists() || installed.as_deref() != Some(manifest.as_str()) {
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("package.json"), &manifest).unwrap();
        let options = verve::npm::InstallOptions {
            cache_dir: Some(dir.join(".tarballs")),
            ..verve::npm::InstallOptions::default()
        };
        verve::npm::install(&dir, &options).unwrap_or_else(|e| panic!("npm install failed: {e}"));
    }
    std::fs::copy(
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures")
            .join(fixture),
        dir.join(fixture),
    )
    .unwrap();
    dir
}

/// A page that has run the fixture `file` of `dir`.
fn page_with(dir: &Path, file: &str) -> Page {
    let mut page = Page::new(Options::default());
    let promise = page
        .runtime
        .import_module(&format!("./{file}"), Some(&dir.join("page.js")))
        .unwrap_or_else(|e| panic!("the page failed: {e}"));
    page.settle();
    if let verve::boa_engine::builtins::promise::PromiseState::Rejected(error) = promise.state() {
        panic!(
            "the page failed: {}\n{}",
            verve::print(&error),
            page.console()
        );
    }
    page
}

fn key(name: &str, ctrl: bool) -> Key {
    let mut key = if name.chars().count() == 1 {
        Key::char(name.chars().next().unwrap())
    } else {
        Key::named(name)
    };
    key.modifiers = Modifiers {
        ctrl,
        ..Modifiers::default()
    };
    key
}

fn press(page: &mut Page, name: &str, ctrl: bool) {
    let key = key(name, ctrl);
    page.document.key_down(&mut page.runtime, &key);
    page.document.key_up(&mut page.runtime, &key);
    page.settle();
}

fn type_text(page: &mut Page, text: &str) {
    for c in text.chars() {
        press(page, &c.to_string(), false);
    }
}

/// The boxes, the line fragments and the `.cm-line` elements of one layout.
type Measured = (Vec<(NodeId, Rect)>, Vec<(NodeId, Vec<Rect>)>, Vec<NodeId>);

/// Reports the layout CodeMirror measures: the editor and its scroller and content, each line a
/// row of fixed height, and each line's text the width its characters come to. A text position
/// asked for is the character the pointer is over.
fn fixed_layout(page: &mut Page) {
    let (rects, fragments, lines): Measured = {
        let dom = page.document.dom();
        let lines = dom.query_all(dom.document, ".cm-line");
        let height = LINE * lines.len().max(1) as f32;
        let mut rects = Vec::new();
        for selector in [".cm-editor", ".cm-scroller", ".cm-content"] {
            if let Some(node) = dom.query(dom.document, selector) {
                rects.push((node, Rect::new(0., 0., 800., height)));
            }
        }
        let mut fragments = Vec::new();
        for (row, line) in lines.iter().enumerate() {
            let width = dom.text_content(*line).chars().count() as f32 * CHAR;
            let rect = Rect::new(0., row as f32 * LINE, 800., LINE);
            rects.push((*line, rect));
            for child in dom.children(*line) {
                let kid = Rect::new(0., rect.y, width, LINE);
                fragments.push((*child, vec![kid]));
            }
        }
        (rects, fragments, lines)
    };
    page.document.set_layout(&mut page.runtime, rects);
    page.document.set_fragments(fragments);
    let owned = lines.clone();
    let texts: Vec<(NodeId, usize)> = {
        let dom = page.document.dom();
        owned
            .iter()
            .map(|line| {
                let first = dom.descendants(*line).into_iter().find(|n| {
                    dom.get(*n)
                        .is_some_and(|n| matches!(n.kind, verve_dom::NodeKind::Text(_)))
                });
                (
                    first.unwrap_or(*line),
                    dom.text_content(*line).chars().count(),
                )
            })
            .collect()
    };
    page.document
        .set_position_provider(Some(Box::new(move |x, y| {
            let row = (y / LINE).floor().max(0.) as usize;
            let (node, length) = texts.get(row.min(texts.len().saturating_sub(1)))?;
            Some((*node, ((x / CHAR).round().max(0.) as usize).min(*length)))
        })));
}

#[test]
#[ignore = "fetches packages from npm"]
fn prosemirror_takes_typing_enter_backspace_and_undo() {
    on_page(|| {
        let dir = prepared(
            "prosemirror",
            serde_json::json!({
                "prosemirror-state": "1.4.3",
                "prosemirror-view": "1.37.1",
                "prosemirror-model": "1.24.1",
                "prosemirror-schema-basic": "1.2.3",
                "prosemirror-example-setup": "1.2.3",
            }),
            "editors-prosemirror.mjs",
        );
        let mut page = page_with(&dir, "editors-prosemirror.mjs");
        page.eval(
            "document.body.innerHTML = '<div id=\"editor\"></div>';
             const { EditorState, EditorView, schema, exampleSetup } = pm;
             globalThis.view = new EditorView(document.getElementById('editor'), {
                 state: EditorState.create({ schema, plugins: exampleSetup({ schema }) }),
             });
             view.focus();",
        );
        page.settle();
        type_text(&mut page, "hello");
        press(&mut page, "Enter", false);
        type_text(&mut page, "world");
        assert_eq!(page.eval("view.state.doc.textContent"), "helloworld");
        assert_eq!(page.eval("view.state.doc.childCount"), "2");
        for _ in 0..5 {
            press(&mut page, "Backspace", false);
        }
        assert_eq!(page.eval("view.state.doc.textContent"), "hello");
        assert_eq!(page.eval("view.state.doc.childCount"), "2");
        // Backspace in the empty paragraph joins it to the one before.
        press(&mut page, "Backspace", false);
        assert_eq!(page.eval("view.state.doc.childCount"), "1");
        assert_eq!(page.eval("view.state.doc.textContent"), "hello");
        // History groups edits made within 500 ms, so how much one undo takes back depends on the
        // timing; what must hold is that it takes something back and that redo restores the doc.
        let joined = page.eval("JSON.stringify(view.state.doc.toJSON())");
        press(&mut page, "z", true);
        assert_ne!(page.eval("JSON.stringify(view.state.doc.toJSON())"), joined);
        press(&mut page, "y", true);
        assert_eq!(page.eval("JSON.stringify(view.state.doc.toJSON())"), joined);
    });
}

#[test]
#[ignore = "fetches packages from npm"]
fn lexical_takes_typing_enter_and_an_input_methods_composition() {
    on_page(|| {
        let dir = prepared(
            "lexical",
            serde_json::json!({
                "lexical": "0.20.0",
                "@lexical/rich-text": "0.20.0",
                "@lexical/history": "0.20.0",
            }),
            "editors-lexical.mjs",
        );
        let mut page = page_with(&dir, "editors-lexical.mjs");
        page.eval(
            "document.body.innerHTML = '<div id=\"editor\" contenteditable=\"true\"></div>';
             const { createEditor, $getRoot, $createParagraphNode, registerRichText, createEmptyHistoryState, registerHistory } = lx;
             globalThis.editor = createEditor({ namespace: 'verve', onError: (e) => { throw e; } });
             editor.setRootElement(document.getElementById('editor'));
             registerRichText(editor);
             registerHistory(editor, createEmptyHistoryState(), 300);
             editor.update(() => {
                 const root = $getRoot();
                 root.clear();
                 const paragraph = $createParagraphNode();
                 root.append(paragraph);
                 paragraph.select();
             }, { discrete: true });
             globalThis.text = () => editor.getEditorState().read(() => $getRoot().getTextContent());
             document.getElementById('editor').focus();",
        );
        page.settle();
        type_text(&mut page, "hello");
        press(&mut page, "Enter", false);
        type_text(&mut page, "world");
        assert_eq!(page.eval("text()"), "hello\n\nworld");
        // An input method composes `ni` and commits `你`.
        page.document.compose(&mut page.runtime, "ni", Some(2..2));
        page.settle();
        page.document.commit_composition(&mut page.runtime, "你");
        page.settle();
        assert_eq!(page.eval("text()"), "hello\n\nworld你");
        press(&mut page, "z", true);
        assert_ne!(page.eval("text()"), "hello\n\nworld你");
    });
}

#[test]
#[ignore = "fetches packages from npm"]
fn codemirror_takes_typing_and_undo_on_a_fixed_layout() {
    on_page(|| {
        let dir = prepared(
            "codemirror",
            serde_json::json!({
                "@codemirror/state": "6.5.0",
                "@codemirror/view": "6.36.1",
                "@codemirror/commands": "6.7.1",
            }),
            "editors-codemirror.mjs",
        );
        let mut page = page_with(&dir, "editors-codemirror.mjs");
        page.eval(
            "const { EditorState, EditorView, keymap, defaultKeymap, history, historyKeymap } = cm;
             globalThis.view = new EditorView({
                 state: EditorState.create({ doc: '', extensions: [history(), keymap.of([...defaultKeymap, ...historyKeymap])] }),
                 parent: document.body,
             });
             view.focus();",
        );
        fixed_layout(&mut page);
        page.settle();
        for text in ["let a", "= 1;"] {
            type_text(&mut page, text);
            fixed_layout(&mut page);
        }
        press(&mut page, "Enter", false);
        fixed_layout(&mut page);
        type_text(&mut page, "b");
        fixed_layout(&mut page);
        assert_eq!(page.eval("view.state.doc.toString()"), "let a= 1;\nb");
        press(&mut page, "Backspace", false);
        assert_eq!(page.eval("view.state.doc.toString()"), "let a= 1;\n");
        press(&mut page, "z", true);
        fixed_layout(&mut page);
        assert_ne!(page.eval("view.state.doc.toString()"), "let a= 1;\n");
        // Redo puts back exactly what the undo took, however it grouped the edits.
        press(&mut page, "y", true);
        fixed_layout(&mut page);
        assert_eq!(page.eval("view.state.doc.toString()"), "let a= 1;\n");
    });
}

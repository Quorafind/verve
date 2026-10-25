//! The DOM arena under load: what scripts detach and let go of is reclaimed, what is in the
//! document — and what a script still holds — is not, and listeners and expandos live and die
//! with the nodes they are on. Ported from su-editor's Obsidian host's `tests/arena.rs`.

mod common;

use common::{Page, on_page};
use verve_dom::Options;

#[test]
fn detached_trees_nothing_holds_are_reclaimed_and_the_rest_survive() {
    on_page(|| {
        let mut page = Page::new(Options::default());
        let before = page.document.dom().len();
        page.eval(
            r#"
            globalThis.clicks = 0;
            // Ten thousand rows, each with a listener, put in and taken out again.
            for (let i = 0; i < 10000; i++) {
                const row = document.createElement('div');
                row.className = 'row';
                row.textContent = 'row ' + i;
                document.body.appendChild(row);
                row.addEventListener('click', () => clicks++);
                row.remove();
            }
            // One kept in the document, one kept by the script.
            const kept = document.createElement('div');
            kept.className = 'kept';
            kept.textContent = 'kept';
            document.body.appendChild(kept);
            kept.addEventListener('click', () => clicks++);
            globalThis.held = document.createElement('div');
            held.textContent = 'held';
            held.addEventListener('click', () => clicks++);
            "ok"
            "#,
        );
        assert!(page.document.dom().len() > before + 20000);
        let freed = page.document.sweep(&mut page.runtime);
        assert!(freed >= 20000, "freed {freed}");
        // Only the two survivors and their text remain.
        assert_eq!(page.document.dom().len(), before + 4);
        // Both still answer, after a collection: the document holds one, the script the other.
        page.eval("document.querySelector('.kept').click(); held.click();");
        assert_eq!(page.eval("clicks"), "2");
        // The kept one, found again through the document, is the same object.
        page.eval("globalThis.again = document.querySelector('.kept'); again.mark = 1;");
        page.document.sweep(&mut page.runtime);
        assert_eq!(page.eval("document.querySelector('.kept').mark"), "1");
        // Let go of `held`: now it goes too.
        page.eval("held = null;");
        assert_eq!(page.document.sweep(&mut page.runtime), 2);
        assert_eq!(page.document.dom().len(), before + 2);
        assert_eq!(page.document.update().changes.removed.len() >= 20002, true);
    });
}

#[test]
fn a_node_left_in_the_document_keeps_its_listeners_without_a_reference() {
    on_page(|| {
        let mut page = Page::new(Options::default());
        page.eval(
            r#"
            globalThis.heard = 0;
            (function () {
                const button = document.createElement('button');
                button.className = 'go';
                document.body.appendChild(button);
                button.addEventListener('click', () => heard++);
                button.expando = 'still';
            })();
            "#,
        );
        page.document.sweep(&mut page.runtime);
        page.eval("document.querySelector('.go').click()");
        assert_eq!(page.eval("heard + ' ' + document.querySelector('.go').expando"), "1 still");
    });
}

#[test]
fn a_tree_a_script_holds_any_part_of_stays_whole() {
    on_page(|| {
        let mut page = Page::new(Options::default());
        page.eval(
            r#"
            globalThis.heard = 0;
            // Built off the document, the root let go of, a leaf held.
            (function () {
                const root = document.createElement('section');
                root.innerHTML = '<ul><li><button>press</button></li></ul>';
                const button = root.querySelector('button');
                button.addEventListener('click', () => heard++);
                button.mark = 'leaf';
                root.querySelector('ul').mark = 'list';
                globalThis.leaf = button;
            })();
            // And the other way: the root held, the leaf with a listener let go of.
            globalThis.top = document.createElement('div');
            (function () {
                top.innerHTML = '<p><i>x</i></p>';
                top.querySelector('i').addEventListener('click', () => heard += 10);
            })();
            "#,
        );
        page.document.sweep(&mut page.runtime);
        // The leaf's tree is all there, what was put on its nodes with it.
        assert_eq!(
            page.eval("[leaf.closest('section').nodeName, leaf.parentNode.parentNode.mark, leaf.mark].join(' ')"),
            "SECTION list leaf"
        );
        page.eval("leaf.click(); top.querySelector('i').click(); document.body.appendChild(top)");
        assert_eq!(page.eval("heard"), "11");
    });
}

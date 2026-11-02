//! Live collections: what a script holds follows the tree, and is one object.

mod common;

use common::{Page, on_page};
use verve_dom::Options;

#[test]
fn a_collection_follows_the_tree() {
    on_page(|| {
        let mut page = Page::new(Options::default());
        assert_eq!(
            page.eval(
                r#"
                const el = document.createElement('div');
                el.innerHTML = '<p class=a>1</p>text<p class=a>2</p>';
                const kids = el.children, nodes = el.childNodes;
                const byTag = el.getElementsByTagName('p'), byClass = el.getElementsByClassName('a');
                const before = [kids.length, nodes.length, byTag.length, byClass.length];
                el.append(document.createElement('p'));
                el.firstChild.classList.remove('a');
                const after = [kids.length, nodes.length, byTag.length, byClass.length];
                el.firstChild.remove();
                const iter = [...kids].map(k => k.tagName).join();
                [before, after, kids.length, Array.from(nodes).length, iter, 1 in kids, 9 in kids].join(' | ')
                "#
            ),
            "2,3,2,2 | 3,4,3,1 | 2 | 3 | P,P | true | false"
        );
    });
}

#[test]
fn the_same_collection_is_handed_out_each_time() {
    on_page(|| {
        let mut page = Page::new(Options::default());
        assert_eq!(
            page.eval(
                r#"
                const el = document.createElement('div');
                el.append(document.createElement('i'));
                [el.childNodes === el.childNodes, el.children === el.children,
                 el.querySelectorAll('i') === el.querySelectorAll('i'),
                 el.childNodes.item(0) === el.firstChild, String(el.children.item(3))].join(' ')
                "#
            ),
            "true true false true null"
        );
    });
}

#[test]
fn form_elements_and_select_options_are_live_and_named() {
    on_page(|| {
        let mut page = Page::new(Options::default());
        assert_eq!(
            page.eval(
                r#"
                document.body.innerHTML = '<form id=f><input name=user><select id=s><option>a</option></select></form>';
                const f = document.getElementById('f'), s = document.getElementById('s');
                const elements = f.elements, options = s.options;
                const input = elements.user === f.firstChild, byId = elements.s === s;
                f.append(Object.assign(document.createElement('input'), { name: 'late' }));
                s.append(new Option('b'));
                [input, byId, elements.length, elements.late.name, options.length,
                 String(elements.namedItem('nope')), document.forms.length, document.forms[0] === f].join(' ')
                "#
            ),
            "true true 3 late 2 null 1 true"
        );
    });
}

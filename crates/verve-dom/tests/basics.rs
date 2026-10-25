//! The document a script builds: nodes, queries, markup, events through the tree.

mod common;

use common::{Page, on_page};
use verve_dom::Options;

#[test]
fn a_script_builds_queries_and_writes_out_the_document() {
    on_page(|| {
        let mut page = Page::new(Options::default());
        let out = page.eval(
            r#"
            const list = document.createElement('ul');
            list.id = 'items';
            for (const word of ['one', 'two', 'three']) {
                const item = document.createElement('li');
                item.className = 'item ' + word;
                item.textContent = word;
                list.appendChild(item);
            }
            document.body.append(list, 'tail');
            [
                document.querySelectorAll('#items > li.item').length,
                document.querySelector('li:nth-child(2)').textContent,
                list.firstChild.nextSibling.classList.contains('two'),
                list.lastElementChild.previousElementSibling === list.children[1],
                document.body.innerHTML,
                list instanceof HTMLUListElement && list instanceof Element && list instanceof Node && list instanceof EventTarget,
                document.body.lastChild.nodeType,
                typeof window.addEventListener, window === globalThis, document.defaultView === window,
            ].join('|')
            "#,
        );
        assert_eq!(
            out,
            "3|two|true|true|<ul id=\"items\"><li class=\"item one\">one</li><li class=\"item two\">two</li><li class=\"item three\">three</li></ul>tail|true|3|function|true|true"
        );
    });
}

#[test]
fn events_are_captured_down_the_tree_and_bubble_back_up() {
    on_page(|| {
        let mut page = Page::new(Options::default());
        let out = page.eval(
            r#"
            document.body.innerHTML = '<div id="outer"><p id="inner"><b id="leaf">x</b></p></div>';
            const outer = document.getElementById('outer');
            const inner = document.getElementById('inner');
            const leaf = document.getElementById('leaf');
            const heard = [];
            const note = (name) => (e) => heard.push(name + ':' + e.eventPhase + ':' + e.currentTarget.id);
            window.addEventListener('ping', (e) => heard.push('window:' + e.eventPhase + ':' + (e.currentTarget === window)), true);
            document.addEventListener('ping', note('document'));
            outer.addEventListener('ping', note('outer-capture'), { capture: true });
            outer.addEventListener('ping', note('outer'));
            inner.addEventListener('ping', (e) => { heard.push('inner'); });
            leaf.addEventListener('ping', note('leaf'));
            leaf.dispatchEvent(new Event('ping', { bubbles: true }));
            const stopped = [];
            inner.addEventListener('pong', (e) => { stopped.push('inner'); e.stopPropagation(); });
            outer.addEventListener('pong', () => stopped.push('outer'));
            leaf.dispatchEvent(new CustomEvent('pong', { bubbles: true, detail: 7 }));
            const click = new MouseEvent('click', { bubbles: true, clientX: 3 });
            let target;
            document.addEventListener('click', (e) => { target = e.target.id + ':' + e.clientX + ':' + (e instanceof UIEvent); });
            leaf.dispatchEvent(click);
            [heard.join(','), stopped.join(','), target, leaf.onclick === null, 'oninput' in document].join(' | ')
            "#,
        );
        assert_eq!(
            out,
            "window:1:true,outer-capture:1:outer,leaf:2:leaf,inner,outer:3:outer,document:3:undefined | inner | leaf:3:true | true | true"
        );
    });
}

#[test]
fn markup_is_read_into_the_document_and_dom_parser_makes_documents() {
    on_page(|| {
        let mut page = Page::new(Options::default());
        let out = page.eval(
            r#"
            const doc = new DOMParser().parseFromString('<!doctype html><title>T</title><p>Hello <em>there</em></p>', 'text/html');
            const made = document.implementation.createHTMLDocument('x');
            made.body.innerHTML = '<span>y</span>';
            [doc.title, doc.body.innerHTML, doc.querySelector('em').textContent, doc.documentElement.nodeName,
             made.body.firstChild.tagName, doc.doctype.name, new XMLSerializer().serializeToString(doc.body)].join('|')
            "#,
        );
        assert_eq!(out, "T|<p>Hello <em>there</em></p>|there|HTML|SPAN|html|<body><p>Hello <em>there</em></p></body>");
    });
}

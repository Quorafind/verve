//! The document a script builds: nodes, queries, markup, events through the tree.

mod common;

use common::{Page, on_page};
use verve_dom::Options;

#[test]
fn review_entities_and_childnode_moves_do_not_lose_data() {
    on_page(|| {
        let mut page = Page::new(Options::default());
        assert_eq!(page.eval(r#"
            (() => {
                const text = '&' + 'a'.repeat(38) + 'é中文';
                const box = document.createElement('div');
                box.innerHTML = '<i title="' + text + '">' + text + '</i>';
                const parsed = new DOMParser().parseFromString('<p>' + text, 'text/html');
                if (box.firstChild.title !== text || box.textContent !== text || parsed.body.textContent !== text) throw Error('实体边界');
                const results = [];
                for (const move of [s => s.before(s, s.nextSibling), s => s.after(s.previousSibling, s),
                                    s => s.replaceWith(s, s.nextSibling), s => s.replaceWith(s.nextSibling, s)]) {
                    box.innerHTML = '<i></i><span></span><b></b>';
                    move(box.children[1]);
                    results.push([...box.children].map(n => n.localName).join(','));
                }
                box.innerHTML = '<i></i><span></span><b></b>';
                const s = box.children[1], b = box.lastChild;
                for (const method of ['adoptNode', 'importNode']) {
                    let refused = false;
                    try { document[method](document); } catch (e) { refused = e.name === 'NotSupportedError'; }
                    if (!refused) throw Error('文档节点不应被转移或导入');
                }
                let refused = false;
                try { document.createTextNode('x').deleteData(2, 1); } catch (e) { refused = e.name === 'IndexSizeError'; }
                if (!refused) throw Error('越界字符删除');
                try { s.replaceWith(box); } catch (e) { if (e.name !== 'HierarchyRequestError') throw e; }
                if (s.parentNode !== box) throw Error('替换失败丢节点');
                try { box.append(b, box); } catch (e) { if (e.name !== 'HierarchyRequestError') throw e; }
                if (b.parentNode !== box) throw Error('插入失败丢节点');
                return results.join('|');
            })()
        "#), "i,span,b|i,span,b|i,span,b|i,b,span");
    });
}

#[test]
fn review_hierarchy_errors_keep_original_children() {
    on_page(|| {
        let mut page = Page::new(Options::default());
        assert_eq!(page.eval(r#"
            (() => {
                const fragment = document.createDocumentFragment();
                const child = document.createElement('i');
                fragment.append(child);
                let refused = false;
                try { child.replaceWith(fragment); } catch (e) { refused = e.name === 'HierarchyRequestError'; }
                if (!refused || child.parentNode !== fragment) throw Error('片段祖先替换应保持原树');
                const box = document.createElement('div');
                box.append(child);
                refused = false;
                try { box.replaceChild(box, child); } catch (e) { refused = e.name === 'HierarchyRequestError'; }
                if (!refused || child.parentNode !== box) throw Error('replaceChild 应先检查层级');
                const doc = document.implementation.createHTMLDocument('x');
                const root = doc.documentElement;
                const next = doc.createElement('html');
                refused = false;
                try { doc.appendChild(next); } catch (e) { refused = e.name === 'HierarchyRequestError'; }
                if (!refused || next.parentNode) throw Error('文档只能有一个根元素');
                doc.replaceChild(next, root);
                const doctype = doc.doctype;
                refused = false;
                try { doc.replaceChildren(child, doctype); } catch (e) { refused = e.name === 'HierarchyRequestError'; }
                return refused && doc.documentElement === next && doctype.parentNode === doc && child.parentNode === box;
            })()
        "#), "true");
    });
}

#[test]
fn review_deep_clone_import_and_serialization_are_iterative() {
    on_page(|| {
        let mut page = Page::new(Options::default());
        assert_eq!(
            page.eval(
                r#"
            (() => {
                let root = document.createElement('i');
                for (let n = 0; n < 100000; n++) {
                    const parent = document.createElement('i');
                    parent.appendChild(root);
                    root = parent;
                }
                const size = 7 * 100001;
                return [root.outerHTML.length === size, root.innerHTML.length === size - 7,
                    root.cloneNode(true).outerHTML.length === size,
                    document.importNode(root, true).outerHTML.length === size,
                    new XMLSerializer().serializeToString(root).length === size].join('|');
            })()
        "#
            ),
            "true|true|true|true|true"
        );
    });
}

#[test]
fn review_wide_fragment_and_text_conversion_preserve_order() {
    on_page(|| {
        let mut page = Page::new(Options::default());
        assert_eq!(page.eval(r#"
            (() => {
                const p = document.createElement('div');
                p.innerHTML = 'a<!---->'.repeat(20000);
                if (p.childNodes.length !== 40000 || p.textContent.length !== 20000) throw Error('批量插入');
                p.replaceChildren(...p.childNodes);
                if (p.childNodes.length !== 40000) throw Error('批量转换');
                p.innerHTML = '';
                p.innerHTML = '<textarea></textarea>'.repeat(8000) + 'x'.repeat(200000);
                if (p.children.length !== 8000 || p.textContent.length !== 200000) throw Error('原始文本扫描');
                p.innerHTML = '<'.repeat(80000);
                return p.textContent.length === 80000;
            })()
        "#), "true");
    });
}

#[test]
fn review_deep_parsing_and_unwrapped_ancestor_links() {
    on_page(|| {
        let mut page = Page::new(Options::default());
        assert_eq!(page.eval(r#"
            (() => {
                const root = document.createElement('section');
                root.innerHTML = '<div>'.repeat(20000) + '<b id=leaf>x</b>';
                const leaf = root.querySelector('#leaf');
                return leaf.textContent + '|' + (leaf.getRootNode() === root) + '|' + root.textContent;
            })()
        "#), "x|true|x");
    });
}

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
        assert_eq!(
            out,
            "T|<p>Hello <em>there</em></p>|there|HTML|SPAN|html|<body><p>Hello <em>there</em></p></body>"
        );
    });
}

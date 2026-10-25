//! `MutationObserver` hearing what a script changed, as the DOM records it, in a microtask;
//! `ResizeObserver` and `IntersectionObserver` driven by the boxes the drawer reports.

mod common;

use common::{Page, on_page};
use verve_dom::{Options, Rect};

#[test]
fn a_mutation_observer_hears_child_lists_attributes_and_text() {
    on_page(|| {
        let mut page = Page::new(Options::default());
        page.eval(
            r#"
            document.body.innerHTML = '<div id="box"><p id="a">one</p></div><div id="elsewhere"></div>';
            globalThis.heard = [];
            const describe = (r) => {
                const nodes = (list) => [...list].map(n => n.nodeName).join('+');
                switch (r.type) {
                    case 'childList': return 'list ' + r.target.id + ' +' + nodes(r.addedNodes) + ' -' + nodes(r.removedNodes) + ' after ' + (r.previousSibling && r.previousSibling.id);
                    case 'attributes': return 'attr ' + r.target.id + ' ' + r.attributeName + ' was ' + r.oldValue;
                    default: return 'text ' + r.target.parentNode.id + ' was ' + r.oldValue;
                }
            };
            globalThis.observer = new MutationObserver((records, self) => {
                heard.push(records.map(describe).join('; ') + (self === observer ? '' : ' ?'));
            });
            observer.observe(document.getElementById('box'), { childList: true, subtree: true, attributes: true, attributeOldValue: true, characterData: true, characterDataOldValue: true, attributeFilter: ['class', 'title'] });
            const box = document.getElementById('box');
            const b = document.createElement('p');
            b.id = 'b';
            box.appendChild(b);
            document.getElementById('a').className = 'x';
            document.getElementById('a').setAttribute('data-ignored', '1');
            document.getElementById('a').firstChild.data = 'uno';
            document.getElementById('elsewhere').title = 'not watched';
            heard.push('sync ' + heard.length);
            "#,
        );
        page.runtime.run_microtasks();
        assert_eq!(
            page.eval("heard.join(' | ')"),
            "sync 0 | list box +P - after a; attr a class was null; text a was one"
        );
        // What is recorded and not yet heard is `takeRecords`'s; a disconnected observer hears
        // nothing more.
        page.eval("heard.length = 0; document.getElementById('b').remove(); globalThis.taken = observer.takeRecords().length; observer.disconnect(); document.getElementById('a').className = 'y'");
        page.runtime.run_microtasks();
        assert_eq!(page.eval("taken + ' ' + heard.length"), "1 0");
        assert_eq!(page.console(), "");
    });
}

#[test]
fn resize_and_intersection_observers_follow_the_painted_boxes() {
    on_page(|| {
        let mut page = Page::new(Options {
            viewport: (800., 600.),
            ..Options::default()
        });
        page.eval(
            r#"
            document.body.innerHTML = '<div id="panel"></div><div id="below"></div>';
            globalThis.sizes = [];
            globalThis.sights = [];
            new ResizeObserver((entries) => {
                for (const e of entries) sizes.push(e.target.id + ' ' + e.contentRect.width + 'x' + e.contentRect.height + ' ' + e.borderBoxSize[0].inlineSize);
            }).observe(document.getElementById('panel'));
            const io = new IntersectionObserver((entries) => {
                for (const e of entries) sights.push(e.target.id + ' ' + e.isIntersecting + ' ' + e.intersectionRatio);
            });
            io.observe(document.getElementById('panel'));
            io.observe(document.getElementById('below'));
            "#,
        );
        let panel = page.find("#panel");
        let below = page.find("#below");
        // Painted: each heard.
        page.document.set_layout(
            &mut page.runtime,
            [(panel, Rect::new(0., 0., 200., 100.)), (below, Rect::new(0., 700., 100., 100.))],
        );
        assert_eq!(page.eval("sizes.join(',')"), "panel 200x100 200");
        assert_eq!(page.eval("sights.join(',')"), "panel true 1,below false 0");
        // Painted the same: nothing new. Then bigger, and scrolled half into sight.
        page.document.set_layout(
            &mut page.runtime,
            [(panel, Rect::new(0., 0., 200., 100.)), (below, Rect::new(0., 700., 100., 100.))],
        );
        page.document.set_layout(
            &mut page.runtime,
            [(panel, Rect::new(0., 0., 300., 120.)), (below, Rect::new(0., 550., 100., 100.))],
        );
        assert_eq!(page.eval("sizes.join(',')"), "panel 200x100 200,panel 300x120 300");
        assert_eq!(page.eval("sights.join(',')"), "panel true 1,below false 0,below true 0.5");
        // getBoundingClientRect reads the same boxes.
        assert_eq!(page.eval("document.getElementById('below').getBoundingClientRect().top"), "550");
        assert_eq!(page.console(), "");
    });
}

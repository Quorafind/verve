//! Custom elements: upgraded when the parser makes them or `define` finds them, and told what
//! happens to them before the DOM method that did it returns.

mod common;

use common::{Page, on_page};
use verve_dom::Options;

const CLASS: &str = r#"
    window.log = [];
    class Probe extends HTMLElement {
        static observedAttributes = ['mode'];
        constructor() { super(); log.push('new'); }
        connectedCallback() { log.push('in'); }
        disconnectedCallback() { log.push('out'); }
        attributeChangedCallback(name, old, now) { log.push(`${name}:${old}>${now}`); }
    }
"#;

#[test]
fn innerhtml_makes_upgraded_elements() {
    on_page(|| {
        let mut page = Page::new(Options::default());
        page.eval(CLASS);
        page.eval("customElements.define('x-probe', Probe)");
        assert_eq!(
            page.eval(
                r#"
                document.body.innerHTML = '<x-probe mode="a" other="b"></x-probe>';
                [document.body.firstChild instanceof Probe, log.join(' ')].join(' | ')
                "#
            ),
            "true | new mode:null>a in"
        );
    });
}

#[test]
fn define_upgrades_what_is_there_in_document_order() {
    on_page(|| {
        let mut page = Page::new(Options::default());
        page.eval(CLASS);
        page.eval(
            r#"document.body.innerHTML =
                '<x-probe id=a mode=1></x-probe><div><x-probe id=b></x-probe></div>';
               window.seen = [];
               customElements.whenDefined('x-probe').then(() => seen.push('defined'));"#,
        );
        assert_eq!(page.eval("log.length"), "0");
        page.eval("customElements.define('x-probe', Probe)");
        page.runtime.run_microtasks();
        assert_eq!(
            page.eval("log.join(' ') + ' | ' + seen"),
            "new mode:null>1 in new in | defined"
        );
    });
}

#[test]
fn callbacks_run_before_the_method_returns_and_for_observed_attributes_only() {
    on_page(|| {
        let mut page = Page::new(Options::default());
        page.eval(CLASS);
        assert_eq!(
            page.eval(
                r#"
                customElements.define('x-probe', Probe);
                const el = document.createElement('x-probe');
                el.setAttribute('mode', 'a');
                el.setAttribute('other', 'b');
                const after = log.join(' ');
                el.setAttribute('mode', 'c');
                el.removeAttribute('mode');
                document.body.append(el);
                el.remove();
                after + ' | ' + log.slice(2).join(' ')
                "#
            ),
            "new mode:null>a | mode:a>c mode:c>null in out"
        );
    });
}

#[test]
fn upgrade_takes_a_detached_tree_and_a_constructor_may_not_add_children() {
    on_page(|| {
        let mut page = Page::new(Options::default());
        page.eval(CLASS);
        assert_eq!(
            page.eval(
                r#"
                const tpl = document.createElement('div');
                tpl.innerHTML = '<x-probe mode=z></x-probe>';
                customElements.define('x-probe', Probe);
                const before = log.length;
                customElements.upgrade(tpl);
                class Bad extends HTMLElement { constructor() { super(); this.append('x'); } }
                customElements.define('x-bad', Bad);
                let name = '';
                try { document.createElement('x-bad'); } catch (e) { name = e.name; }
                [before, log.join(' '), name, customElements.getName(Probe)].join(' | ')
                "#
            ),
            "0 | new mode:null>z | NotSupportedError | x-probe"
        );
    });
}

#[test]
fn define_refuses_reserved_names_and_a_constructor_used_twice() {
    on_page(|| {
        let mut page = Page::new(Options::default());
        assert_eq!(
            page.eval(
                r#"
                const outcome = (name, ctor) => {
                    try { customElements.define(name, ctor); return 'ok'; } catch (e) { return e.name; }
                };
                class A extends HTMLElement {}
                class B extends HTMLElement {}
                [outcome('font-face', A), outcome('annotation-xml', A), outcome('Bad-Name', A),
                 outcome('x-a', A), outcome('x-b', A), outcome('x-a', B), outcome('x-b2', B)].join(' ')
                "#
            ),
            "SyntaxError SyntaxError SyntaxError ok NotSupportedError NotSupportedError ok"
        );
    });
}

//! Shadow trees: what is connected in one, the flat tree a drawer walks, slots and their
//! `slotchange`, styles scoped to them, and events that cross them.

mod common;

use common::{Page, on_page};
use verve_dom::Options;

/// A page with `hosted(light, shadow, init)`: a `<div>` in the body holding `light`, with an
/// open shadow root (unless `init` says otherwise) holding `shadow`.
fn shadow_page() -> Page {
    let mut page = Page::new(Options::default());
    page.eval(
        r#"
        function hosted(light, shadow, init) {
            const host = document.createElement('div');
            host.innerHTML = light;
            document.body.append(host);
            const root = host.attachShadow({ mode: 'open', ...init });
            root.innerHTML = shadow;
            return { host, root };
        }
        "#,
    );
    page
}

#[test]
fn a_shadow_tree_is_connected_and_rooted_at_its_host() {
    on_page(|| {
        let mut page = shadow_page();
        assert_eq!(
            page.eval(
                r#"
                (() => {
                    const { host, root } = hosted('', '<span id=in>in</span>');
                    const inside = root.getElementById('in');
                    return [
                        host.shadowRoot === root,
                        root.host === host,
                        root.mode,
                        root.delegatesFocus,
                        inside.getRootNode() === root,
                        inside.getRootNode({ composed: true }) === document,
                        inside.isConnected,
                        root.isConnected,
                        document.body.contains(inside),
                        host.querySelector('span') === null,
                        root.querySelectorAll('span').length,
                        inside.assignedSlot === null,
                    ].join(' ');
                })()
                "#
            ),
            "true true open false true true true true true true 1 true"
        );
    });
}

#[test]
fn a_closed_shadow_tree_is_not_handed_out() {
    on_page(|| {
        let mut page = shadow_page();
        assert_eq!(
            page.eval(
                r#"
                (() => {
                    const { host, root } = hosted('', '<b>x</b>', { mode: 'closed' });
                    let refused = false;
                    try { host.attachShadow({ mode: 'open' }); } catch (e) { refused = e.name === 'NotSupportedError'; }
                    let badMode = false;
                    try { document.createElement('div').attachShadow({}); } catch (e) { badMode = e instanceof TypeError; }
                    return [host.shadowRoot === null, root.host === host, root.mode,
                            host.childNodes.length, refused, badMode].join(' ');
                })()
                "#
            ),
            "true true closed 0 true true"
        );
    });
}

#[test]
fn the_flat_tree_puts_slotted_nodes_under_their_slot() {
    on_page(|| {
        let mut page = shadow_page();
        page.eval(
            r#"
            const { host, root } = hosted('<b slot="a">A</b><i>B</i><u slot="gone">C</u>', '<slot name="a"></slot><slot></slot>');
            "#,
        );
        let dom = page.document.dom();
        let host = dom.children(dom.body)[0];
        let root = dom.shadow_root(host).expect("a shadow root");
        let slots = dom.children(root).to_vec();
        let light = dom.children(host).to_vec();
        assert_eq!(dom.flat_children(host), slots);
        assert_eq!(dom.flat_children(slots[0]), vec![light[0]]);
        assert_eq!(dom.flat_children(slots[1]), vec![light[1]]);
        assert_eq!(dom.flat_parent(light[0]), Some(slots[0]));
        assert_eq!(dom.flat_parent(light[1]), Some(slots[1]));
        assert_eq!(dom.flat_parent(light[2]), None);
        assert_eq!(dom.flat_parent(slots[0]), Some(host));
        assert_eq!(dom.flat_parent(slots[1]), Some(host));
    });
}

#[test]
fn slots_answer_what_they_show_and_say_when_it_changes() {
    on_page(|| {
        let mut page = shadow_page();
        assert_eq!(
            page.eval(
                r#"
                (() => {
                    const { host, root } = hosted('<b slot="a">A</b><i>B</i>', '<slot name="a"></slot><slot><em>fallback</em></slot>');
                    const [named, plain] = root.querySelectorAll('slot');
                    window.seen = [];
                    for (const slot of [named, plain]) {
                        slot.addEventListener('slotchange', e => seen.push([slot === named ? 'named' : 'plain',
                            e.target === slot, e.bubbles, e.composed]));
                    }
                    window.host = host;
                    const before = [named.assignedNodes().length, plain.assignedNodes().length,
                                    host.children[0].assignedSlot === named, host.children[1].assignedSlot === plain];
                    return [before.join(','), named.assignedElements()[0].tagName].join(' ');
                })()
                "#
            ),
            "1,1,true,true B"
        );
        // What a slot holds when it comes into the tree is told once; then only what changes.
        page.runtime.run_microtasks();
        page.eval("seen.length = 0");
        assert_eq!(
            page.eval("host.append(document.createElement('s')); 'ok'"),
            "ok"
        );
        page.runtime.run_microtasks();
        assert_eq!(
            page.eval("seen.map(s => s.join(':')).join(' ')"),
            "plain:true:false:false"
        );
    });
}

#[test]
fn an_empty_slot_shows_its_own_children() {
    on_page(|| {
        let mut page = shadow_page();
        page.eval(
            r#"
            const { host, root } = hosted('', '<slot><em>fallback</em></slot>');
            "#,
        );
        let dom = page.document.dom();
        let host = dom.children(dom.body)[0];
        let slot = dom.children(dom.shadow_root(host).expect("a root"))[0];
        let fallback = dom.children(slot)[0];
        assert_eq!(dom.flat_children(slot), vec![fallback]);
        assert_eq!(dom.flat_parent(fallback), Some(slot));
    });
}

#[test]
fn a_manual_assignment_takes_only_what_assign_names() {
    on_page(|| {
        let mut page = shadow_page();
        assert_eq!(
            page.eval(
                r#"
                (() => {
                    const { host, root } = hosted('<b>A</b><i slot="x">B</i>', '<slot></slot>', { slotAssignment: 'manual' });
                    const slot = root.firstChild;
                    const before = slot.assignedNodes().length;
                    slot.assign(host.children[1], host.children[0]);
                    const after = [...slot.assignedNodes()].map(n => n.tagName).join(',');
                    return [root.slotAssignment, before, after, host.children[1].assignedSlot === slot].join(' ');
                })()
                "#
            ),
            "manual 0 I,B true"
        );
    });
}

#[test]
fn a_shadow_trees_styles_are_its_own() {
    on_page(|| {
        let mut page = shadow_page();
        assert_eq!(
            page.eval(
                r#"
                (() => {
                    const outside = document.createElement('style');
                    outside.textContent = '.x { color: rgb(1, 2, 3); }';
                    document.head.append(outside);
                    const host = document.createElement('div');
                    host.innerHTML = '<span slot="a" class="x">slotted</span>';
                    document.body.append(host);
                    const root = host.attachShadow({ mode: 'open' });
                    const shadow = document.createElement('style');
                    shadow.textContent = [
                        '.x { color: rgb(9, 9, 9); }',
                        ':host { border-top-width: 3px; border-top-style: solid; }',
                        ':host(.big) { border-bottom-width: 4px; border-bottom-style: solid; }',
                        '::slotted(.x) { font-size: 21px; }',
                    ].join('\n');
                    const inner = document.createElement('div');
                    inner.className = 'x';
                    const slot = document.createElement('slot');
                    slot.setAttribute('name', 'a');
                    root.append(shadow, inner, slot);
                    const loose = document.createElement('div');
                    loose.className = 'x';
                    document.body.append(loose);
                    const style = name => getComputedStyle(name);
                    const colour = n => style(n).color;
                    const before = [colour(inner), colour(loose),
                                    style(host).borderTopWidth, style(host).borderBottomWidth,
                                    style(host.children[0]).fontSize];
                    host.classList.add('big');
                    return [...before, style(host).borderBottomWidth,
                            style(host.children[0]).fontSize].join(' ');
                })()
                "#
            ),
            "rgb(9, 9, 9) rgb(1, 2, 3) 3px 0px 21px 4px 21px"
        );
    });
}

#[test]
fn an_outer_tree_styles_a_part_and_wins_normal_cascades() {
    on_page(|| {
        let mut page = shadow_page();
        assert_eq!(
            page.eval(
                r#"
                (() => {
                    const style = document.createElement('style');
                    style.textContent = [
                        'my-el::part(label) { color: rgb(5, 5, 5); }',
                        'my-el { color: rgb(2, 2, 2); border-left-width: 7px; border-left-style: solid; }',
                    ].join('\n');
                    document.head.append(style);
                    const host = document.createElement('my-el');
                    document.body.append(host);
                    const root = host.attachShadow({ mode: 'open' });
                    root.innerHTML = '<span part="label">x</span><style>:host { color: rgb(3, 3, 3); border-left-width: 9px; border-left-style: solid; }</style>';
                    const part = root.querySelector('span');
                    const normal = [getComputedStyle(part).color, getComputedStyle(host).color,
                                    getComputedStyle(host).borderLeftWidth];
                    // `!important` in the shadow tree beats the outer tree's.
                    root.lastChild.textContent = ':host { color: rgb(4, 4, 4) !important; }';
                    return [...normal, getComputedStyle(host).color].join(' ');
                })()
                "#
            ),
            "rgb(5, 5, 5) rgb(2, 2, 2) 7px rgb(4, 4, 4)"
        );
    });
}

#[test]
fn what_is_slotted_inherits_from_its_slot() {
    on_page(|| {
        let mut page = shadow_page();
        assert_eq!(
            page.eval(
                r#"
                (() => {
                    const { host, root } = hosted('<b>light</b>', '<style>slot { color: rgb(6, 6, 6); }</style><slot></slot>');
                    return getComputedStyle(host.firstChild).color;
                })()
                "#
            ),
            "rgb(6, 6, 6)"
        );
    });
}

#[test]
fn outside_a_shadow_tree_the_target_is_its_host() {
    on_page(|| {
        let mut page = shadow_page();
        assert_eq!(
            page.eval(
                r#"
                (() => {
                    const { host, root } = hosted('', '<button id=b>go</button>');
                    host.id = 'host';
                    const button = root.getElementById('b');
                    const seen = [];
                    root.addEventListener('click', e => seen.push('root:' + e.target.id));
                    document.body.addEventListener('click', e => seen.push('body:' + e.target.id));
                    button.dispatchEvent(new Event('click', { bubbles: true, composed: true }));
                    button.dispatchEvent(new Event('click', { bubbles: true }));
                    return seen.join(' ');
                })()
                "#
            ),
            "root:b body:host root:b"
        );
    });
}

#[test]
fn a_closed_tree_is_left_out_of_the_path_seen_from_outside() {
    on_page(|| {
        let mut page = shadow_page();
        assert_eq!(
            page.eval(
                r#"
                (() => {
                    const { host, root } = hosted('', '<p><slot></slot></p>', { mode: 'closed' });
                    const p = root.firstChild;
                    host.innerHTML = '<i>slotted</i>';
                    const names = path => path.map(n => n.nodeName || 'window').join('>');
                    let outer, inner;
                    document.body.addEventListener('x', e => { outer = names(e.composedPath()); });
                    p.addEventListener('x', e => { inner = names(e.composedPath()); });
                    p.firstChild.dispatchEvent(new Event('x', { bubbles: true, composed: true }));
                    host.firstChild.dispatchEvent(new Event('x', { bubbles: true, composed: true }));
                    return [outer, inner].join(' | ');
                })()
                "#
            ),
            // The slotted node goes through its slot, which body cannot see inside a closed tree.
            "I>DIV>BODY>HTML>#document>window | I>SLOT>P>#document-fragment>DIV>BODY>HTML>#document>window"
        );
    });
}

#[test]
fn the_focus_is_the_host_from_outside() {
    on_page(|| {
        let mut page = shadow_page();
        assert_eq!(
            page.eval(
                r#"
                (() => {
                    const { host, root } = hosted('', '<input id=i>', { delegatesFocus: true });
                    host.focus();
                    return [document.activeElement === host, root.activeElement.id,
                            host.matches(':focus')].join(' ');
                })()
                "#
            ),
            "true i true"
        );
    });
}

#[test]
fn a_template_with_shadowrootmode_in_a_page_is_its_parents_shadow_root() {
    on_page(|| {
        let mut page = shadow_page();
        assert_eq!(
            page.eval(
                r#"
                (() => {
                    const page = new DOMParser().parseFromString(
                        '<div id=h><template shadowrootmode=open shadowrootdelegatesfocus><b>x</b><slot></slot></template><i>light</i></div>' +
                        '<p id=c><template shadowrootmode=closed><u></u></template></p>', 'text/html');
                    const host = page.getElementById('h');
                    return [host.shadowRoot.mode, host.shadowRoot.delegatesFocus, host.shadowRoot.innerHTML,
                            host.children.length, String(host.querySelector('template')),
                            String(page.getElementById('c').shadowRoot)].join(' ');
                })()
                "#
            ),
            "open true <b>x</b><slot></slot> 1 null null"
        );
    });
}

#[test]
fn only_the_elements_the_specification_lists_take_a_shadow_root() {
    on_page(|| {
        let mut page = shadow_page();
        assert_eq!(
            page.eval(
                r#"
                (() => {
                    const names = [];
                    for (const tag of ['button', 'input', 'a', 'x', 'x-y', 'section', 'h3', 'span']) {
                        try { document.createElement(tag).attachShadow({ mode: 'open' }); names.push(tag + ':ok'); }
                        catch (e) { names.push(tag + ':' + e.name); }
                    }
                    return names.join(' ');
                })()
                "#
            ),
            concat!(
                "button:NotSupportedError input:NotSupportedError a:NotSupportedError ",
                "x:NotSupportedError x-y:ok section:ok h3:ok span:ok"
            )
        );
    });
}

#[test]
fn flattened_assigned_nodes_pass_through_nested_slots_and_fallback() {
    on_page(|| {
        let mut page = shadow_page();
        assert_eq!(
            page.eval(
                r#"
                (() => {
                    // inner's host sits in outer's shadow tree, and is given to outer's slot.
                    const { host: outer, root } = hosted('<b slot="s">x</b>', '<div id=in><slot name="s"></slot></div>');
                    const inner = root.getElementById('in');
                    const innerRoot = inner.attachShadow({ mode: 'open' });
                    innerRoot.innerHTML = '<slot id=pass></slot><slot id=empty name=none><i>fallback</i></slot>';
                    const tags = list => [...list].map(n => n.nodeName).join(',');
                    const pass = innerRoot.getElementById('pass'), empty = innerRoot.getElementById('empty');
                    const names = {
                        plain: tags(empty.assignedNodes()),
                        flat: tags(empty.assignedNodes({ flatten: true })),
                        elements: tags(empty.assignedElements({ flatten: true })),
                        passPlain: tags(pass.assignedNodes()),
                        passFlat: tags(pass.assignedNodes({ flatten: true })),
                    };
                    return JSON.stringify(names);
                })()
                "#
            ),
            r#"{"plain":"","flat":"I","elements":"I","passPlain":"SLOT","passFlat":"B"}"#
        );
    });
}

#[test]
fn an_unknown_mode_is_a_type_error() {
    on_page(|| {
        let mut page = shadow_page();
        assert_eq!(
            page.eval(
                r#"
                (() => {
                    try { document.createElement('div').attachShadow({ mode: 'bogus' }); return 'no error'; }
                    catch (e) { return [e instanceof TypeError, e.name].join(' '); }
                })()
                "#
            ),
            "true TypeError"
        );
    });
}

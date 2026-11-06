//! A document in more than one JavaScript world: each node has an object of its own in each
//! world, the listeners of a node are heard in the order they were added whichever world added
//! them, and a node an isolated world holds is not freed under it. (The spike of P21.)

mod common;

use common::{Page, on_page};
use verve::WorldId;
use verve_dom::Options;

fn in_world(page: &mut Page, world: WorldId, source: &str) -> String {
    let previous = page.runtime.enter_world(world);
    let answer = page.eval(source);
    page.runtime.enter_world(previous);
    answer
}

fn two_worlds() -> (Page, WorldId) {
    let mut page = Page::new(Options::default());
    let world = page
        .document
        .create_world(&mut page.runtime)
        .expect("a world");
    (page, world)
}

#[test]
fn a_node_has_an_object_in_each_world_and_expandos_are_not_shared() {
    on_page(|| {
        let (mut page, world) = two_worlds();
        in_world(
            &mut page,
            world,
            "globalThis.made = document.body.appendChild(document.createElement('div')); made.id = 'made'; made.expando = 'isolated'; made.nodeName",
        );
        // The main world sees the node the isolated world made, and an object of its own for it.
        assert_eq!(page.eval("document.getElementById('made').nodeName"), "DIV");
        assert_eq!(
            page.eval("globalThis.found = document.getElementById('made'); typeof found.expando"),
            "undefined"
        );
        assert_eq!(in_world(&mut page, world, "made.expando"), "isolated");
        page.eval("found.expando = 'main'");
        assert_eq!(in_world(&mut page, world, "made.expando"), "isolated");
        assert_eq!(page.eval("found.expando"), "main");
        // Each world has its own `window`, `document` and classes.
        assert_eq!(
            in_world(
                &mut page,
                world,
                "[made instanceof HTMLDivElement, made instanceof Node, document === window.document, window === globalThis].join()"
            ),
            "true,true,true,true"
        );
        assert_eq!(page.eval("typeof made"), "undefined");
        let same = in_world(
            &mut page,
            world,
            "globalThis.again = document.getElementById('made'); again === made",
        );
        assert_eq!(same, "true");
        // The same node, not the same object.
        let main = page.runtime.world_global(verve::MAIN_WORLD).expect("main");
        let other = page.runtime.world_global(world).expect("isolated");
        let context = page.runtime.context();
        let found = main.get(boa_engine::js_string!("found"), context).unwrap();
        let made = other.get(boa_engine::js_string!("made"), context).unwrap();
        assert!(found.as_object() != made.as_object());
        assert_eq!(page.eval("found.id"), "made");
    });
}

#[test]
fn a_built_in_of_one_world_is_not_the_others() {
    on_page(|| {
        let (mut page, world) = two_worlds();
        page.eval("Array.prototype.push = null; Node.prototype.marker = 'page'; 0");
        assert_eq!(in_world(&mut page, world, "[1].push(2)"), "2");
        assert_eq!(
            in_world(&mut page, world, "typeof document.body.marker"),
            "undefined"
        );
        assert_eq!(page.eval("document.body.marker"), "page");
    });
}

#[test]
fn listeners_of_both_worlds_are_heard_in_the_order_they_were_added() {
    on_page(|| {
        let (mut page, world) = two_worlds();
        page.eval("document.body.innerHTML = '<button id=go>go</button>'; 0");
        let add = |page: &mut Page, world: WorldId, name: &str| {
            in_world(
                page,
                world,
                &format!(
                    "document.getElementById('go').addEventListener('click', (e) => console.log('{name}', e.target.id, e.currentTarget === document.getElementById('go'))); 0"
                ),
            );
        };
        add(&mut page, verve::MAIN_WORLD, "page-1");
        add(&mut page, world, "isolated-1");
        add(&mut page, verve::MAIN_WORLD, "page-2");
        add(&mut page, world, "isolated-2");
        let go = page.find("#go");
        page.document.press(&mut page.runtime, go);
        let console = page.console();
        let lines: Vec<&str> = console
            .lines()
            .map(|l| l.split_once(": ").unwrap().1)
            .collect();
        assert_eq!(
            lines,
            [
                "page-1 go true",
                "isolated-1 go true",
                "page-2 go true",
                "isolated-2 go true"
            ]
        );
        // Dispatched from the isolated world, the page's listeners are heard too, in the same order.
        in_world(
            &mut page,
            world,
            "document.getElementById('go').dispatchEvent(new MouseEvent('click', { bubbles: true }))",
        );
        let console = page.console();
        assert_eq!(console.lines().count(), 4, "{console}");
        assert!(console.lines().next().unwrap().contains("page-1"));
    });
}

#[test]
fn window_listeners_and_handlers_of_both_worlds_hear_an_event() {
    on_page(|| {
        let (mut page, world) = two_worlds();
        page.eval("addEventListener('ping', () => console.log('page window')); onresize = () => console.log('page onresize'); 0");
        in_world(
            &mut page,
            world,
            "addEventListener('ping', () => console.log('isolated window')); onresize = () => console.log('isolated onresize'); 0",
        );
        page.eval("dispatchEvent(new Event('ping')); dispatchEvent(new Event('resize'))");
        let console = page.console();
        let lines: Vec<&str> = console
            .lines()
            .map(|l| l.split_once(": ").unwrap().1)
            .collect();
        assert_eq!(
            lines,
            [
                "page window",
                "isolated window",
                "page onresize",
                "isolated onresize"
            ]
        );
    });
}

#[test]
fn a_listener_of_the_other_world_acts_on_the_event() {
    on_page(|| {
        let (mut page, world) = two_worlds();
        page.eval("document.body.innerHTML = '<a id=link href=#x>x</a>'; 0");
        in_world(
            &mut page,
            world,
            "document.getElementById('link').addEventListener('click', (e) => { e.preventDefault(); e.stopPropagation(); })",
        );
        page.eval("globalThis.reached = false; document.body.addEventListener('click', () => { reached = true; })");
        assert_eq!(
            page.eval("const e = new MouseEvent('click', { bubbles: true, cancelable: true }); document.getElementById('link').dispatchEvent(e); [e.defaultPrevented, reached].join()"),
            "true,false"
        );
    });
}

#[test]
fn a_node_an_isolated_world_holds_stays_after_the_page_removes_it() {
    on_page(|| {
        let (mut page, world) = two_worlds();
        in_world(
            &mut page,
            world,
            "globalThis.held = document.body.appendChild(document.createElement('section')); held.id = 'held'; held.append(document.createElement('p')); 0",
        );
        page.eval("document.getElementById('held').remove(); 0");
        // Neither the page nor anything else holds an object of it; the isolated world does.
        page.document.sweep(&mut page.runtime);
        page.document.sweep(&mut page.runtime);
        assert_eq!(
            in_world(
                &mut page,
                world,
                "[held.isConnected, held.id, held.firstChild.nodeName].join()"
            ),
            "false,held,P"
        );
        // Let go of it there, and the tree is freed.
        in_world(&mut page, world, "globalThis.held = null; 0");
        let freed: usize = (0..3).map(|_| page.document.sweep(&mut page.runtime)).sum();
        assert!(freed >= 2, "freed {freed}");
    });
}

#[test]
fn a_second_world_gets_its_own_objects_for_a_subtree_it_never_saw() {
    on_page(|| {
        let (mut page, world) = two_worlds();
        page.eval("document.body.innerHTML = '<ul id=list><li>a</li><li>b</li></ul>'; 0");
        assert_eq!(
            in_world(
                &mut page,
                world,
                "document.querySelectorAll('li').length + ':' + document.getElementById('list').lastChild.textContent"
            ),
            "2:b"
        );
        // Moving a node keeps each world's objects in step.
        page.eval("const list = document.getElementById('list'); list.insertBefore(list.lastChild, list.firstChild); 0");
        assert_eq!(
            in_world(
                &mut page,
                world,
                "document.getElementById('list').firstChild.textContent"
            ),
            "b"
        );
    });
}

#[test]
fn an_observer_made_in_a_world_is_told_in_that_world() {
    on_page(|| {
        let (mut page, world) = two_worlds();
        in_world(
            &mut page,
            world,
            "globalThis.seen = []; \
             new MutationObserver((records) => { \
               const r = records[0]; \
               seen.push(r.target === document.body, \
                 r.addedNodes[0] === document.body.lastChild, \
                 r.constructor.constructor('return this')() === globalThis); \
             }).observe(document.body, { childList: true }); 0",
        );
        page.eval("document.body.appendChild(document.createElement('p')); 0");
        page.settle();
        assert_eq!(in_world(&mut page, world, "seen.join()"), "true,true,true");
    });
}

#[test]
fn a_world_s_navigator_is_the_page_s() {
    on_page(|| {
        let mut page = Page::new(Options {
            user_agent: Some("Verve Test/1".into()),
            language: Some("de-DE".into()),
            ..Options::default()
        });
        let world = page
            .document
            .create_world(&mut page.runtime)
            .expect("a world");
        assert_eq!(
            in_world(
                &mut page,
                world,
                "navigator.userAgent + '|' + navigator.language"
            ),
            "Verve Test/1|de-DE"
        );
    });
}

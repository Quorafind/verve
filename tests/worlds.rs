//! Worlds: a realm of the runtime's own context with web globals of its own, over the one event
//! loop; and the listeners of one node in two worlds, heard in the order they were added.

use boa_engine::property::Attribute;
use boa_engine::{JsObject, JsValue, js_string};
use verve::dom_events::{self, host_target, host_target_beside, set_world_counterpart};
use verve::{MAIN_WORLD, Runtime, WorldId};

fn run(rt: &mut Runtime, world: WorldId, source: &str) -> String {
    let previous = rt.enter_world(world);
    let value = rt.eval(source).unwrap_or_else(|e| panic!("{source}: {e}"));
    rt.enter_world(previous);
    verve::print(&value)
}

fn two() -> (Runtime, WorldId) {
    let mut rt = Runtime::new();
    let world = rt.create_world().expect("a world");
    (rt, world)
}

#[test]
fn a_world_has_built_ins_of_its_own() {
    let (mut rt, world) = two();
    assert_eq!(world, 1);
    run(&mut rt, MAIN_WORLD, "Array.prototype.push = null; 0");
    assert_eq!(run(&mut rt, world, "[1].push(2)"), "2");
    assert_eq!(
        run(&mut rt, world, "typeof Array.prototype.push"),
        "function"
    );
    let page = run(&mut rt, MAIN_WORLD, "globalThis.marker = 1; typeof marker");
    assert_eq!(page, "number");
    assert_eq!(run(&mut rt, world, "typeof marker"), "undefined");
    assert_eq!(run(&mut rt, world, "globalThis === global"), "true");
}

#[test]
fn a_world_has_the_web_globals_over_one_event_loop() {
    let (mut rt, world) = two();
    let names = "[typeof console.log, typeof setTimeout, typeof fetch, typeof URL, typeof TextEncoder, \
                 typeof Event, typeof EventTarget, typeof Blob, typeof structuredClone, typeof Headers].join()";
    let expected =
        "function,function,function,function,function,function,function,function,function,function";
    assert_eq!(run(&mut rt, world, names), expected);
    run(
        &mut rt,
        world,
        "globalThis.seen = []; setTimeout(() => seen.push('timer'), 0); Promise.resolve().then(() => seen.push('job'));",
    );
    run(
        &mut rt,
        MAIN_WORLD,
        "globalThis.main = []; setTimeout(() => main.push('timer'), 0)",
    );
    rt.run_due();
    assert_eq!(run(&mut rt, world, "seen.join()"), "job,timer");
    assert_eq!(run(&mut rt, MAIN_WORLD, "main.join()"), "timer");
    // The classes are the world's own.
    let own = "const a = new URL('http://x/'); const e = new Event('t'); \
               [a instanceof URL, e instanceof Event, e instanceof EventTarget].join()";
    assert_eq!(run(&mut rt, world, own), "true,true,false");
    let first = run(&mut rt, MAIN_WORLD, "typeof URL");
    assert_eq!(first, "function");
}

#[test]
fn the_world_that_runs_is_the_one_of_the_function() {
    let (mut rt, world) = two();
    let global = rt.world_global(world).expect("its global");
    let main = rt.world_global(MAIN_WORLD).expect("the first global");
    assert!(!JsObject::equals(&global, &main));
    run(&mut rt, world, "globalThis.which = () => 'world'");
    let which = global
        .get(js_string!("which"), rt.context())
        .expect("a function");
    // Called from the first world, a function runs in the world it was made in.
    let answer = rt
        .call(&which, &JsValue::undefined(), &[])
        .expect("it runs");
    assert_eq!(verve::print(&answer), "world");
    assert_eq!(verve::current_world(rt.context()), MAIN_WORLD);
    let inside = run(&mut rt, world, "[].constructor === Array");
    assert_eq!(inside, "true");
}

/// A host target in each world over the one node, as a document's wrappers are.
fn node_in_both(rt: &mut Runtime, world: WorldId) {
    set_world_counterpart(rt.context(), |target, cx| {
        let Some(object) = target.as_object() else {
            return Ok(None);
        };
        let id = dom_events::host_id(&object).expect("a host target");
        let _ = id;
        let here = cx
            .global_object()
            .get(js_string!("node"), cx)
            .expect("the node of this world");
        Ok(Some(here))
    });
    let proto = |rt: &mut Runtime, world: WorldId| {
        let previous = rt.enter_world(world);
        let proto = dom_events::classes(rt.context())
            .expect("classes")
            .event_target
            .1;
        rt.enter_world(previous);
        proto
    };
    let (first, second) = (proto(rt, MAIN_WORLD), proto(rt, world));
    let main = host_target(first, 7);
    let other = host_target_beside(second, 7, &main);
    for (world, node) in [(MAIN_WORLD, main), (world, other)] {
        let previous = rt.enter_world(world);
        rt.context()
            .register_global_property(js_string!("node"), node, Attribute::all())
            .expect("a global");
        rt.enter_world(previous);
    }
}

#[test]
fn the_listeners_of_a_node_in_two_worlds_are_heard_in_the_order_they_were_added() {
    let (mut rt, world) = two();
    node_in_both(&mut rt, world);
    run(
        &mut rt,
        MAIN_WORLD,
        "globalThis.log = []; node.addEventListener('click', () => log.push('page 1'))",
    );
    run(
        &mut rt,
        world,
        "globalThis.log = []; node.addEventListener('click', () => log.push('isolated 1'))",
    );
    run(
        &mut rt,
        MAIN_WORLD,
        "node.addEventListener('click', () => log.push('page 2'))",
    );
    run(
        &mut rt,
        world,
        "node.addEventListener('click', () => log.push('isolated 2'))",
    );
    run(
        &mut rt,
        MAIN_WORLD,
        "node.dispatchEvent(new Event('click'))",
    );
    assert_eq!(run(&mut rt, MAIN_WORLD, "log.join()"), "page 1,page 2");
    assert_eq!(run(&mut rt, world, "log.join()"), "isolated 1,isolated 2");
    run(
        &mut rt,
        world,
        "log.length = 0; node.dispatchEvent(new Event('click'))",
    );
    assert_eq!(run(&mut rt, world, "log.join()"), "isolated 1,isolated 2");
}

#[test]
fn a_listener_gets_its_own_worlds_target_and_event() {
    let (mut rt, world) = two();
    node_in_both(&mut rt, world);
    run(
        &mut rt,
        world,
        "globalThis.got = []; node.addEventListener('ping', (e) => { \
         got.push(e.target === node, e.currentTarget === node, e instanceof Event); \
         e.preventDefault(); e.stopPropagation(); })",
    );
    run(
        &mut rt,
        MAIN_WORLD,
        "globalThis.after = []; node.addEventListener('ping', () => after.push('late'))",
    );
    let result = run(
        &mut rt,
        MAIN_WORLD,
        "const e = new Event('ping', { cancelable: true }); const r = node.dispatchEvent(e); \
         [r, e.defaultPrevented, after.length].join()",
    );
    assert_eq!(result, "false,true,1");
    assert_eq!(run(&mut rt, world, "got.join()"), "true,true,true");
}

/// A value the collector can be seen to free.
#[derive(boa_engine::Trace, boa_engine::Finalize, boa_engine::JsData)]
#[allow(dead_code)]
struct Probe(#[unsafe_ignore_trace] Freed);

struct Freed(std::sync::Arc<std::sync::atomic::AtomicBool>);

impl Drop for Freed {
    fn drop(&mut self) {
        self.0.store(true, std::sync::atomic::Ordering::SeqCst);
    }
}

#[test]
fn a_dropped_runtime_frees_its_realms() {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};
    let freed = [
        Arc::new(AtomicBool::new(false)),
        Arc::new(AtomicBool::new(false)),
    ];
    {
        let (mut rt, world) = two();
        for (at, id) in [MAIN_WORLD, world].into_iter().enumerate() {
            // Touch the classes a realm keeps for the host, so that they are made.
            run(
                &mut rt,
                id,
                "new Event('x'); new Headers(); new ReadableStream(); new MessageChannel(); 0",
            );
            let global = rt.world_global(id).expect("a global");
            let probe = JsObject::from_proto_and_data(None, Probe(Freed(freed[at].clone())));
            global
                .set(
                    js_string!("probe"),
                    JsValue::from(probe),
                    false,
                    rt.context(),
                )
                .expect("set");
        }
    }
    boa_gc::force_collect();
    assert!(freed[0].load(Ordering::SeqCst), "the first realm is freed");
    assert!(freed[1].load(Ordering::SeqCst), "the second realm is freed");
}

#[test]
fn what_an_event_carries_into_a_world_is_a_copy_of_that_world() {
    let (mut rt, world) = two();
    node_in_both(&mut rt, world);
    run(
        &mut rt,
        world,
        "globalThis.got = []; node.addEventListener('ping', (e) => { \
         got.push(Object.getPrototypeOf(e.detail) === Object.prototype, e.detail.n, \
         e.detail.constructor.constructor('return this')() === globalThis); })",
    );
    run(
        &mut rt,
        MAIN_WORLD,
        "node.dispatchEvent(new CustomEvent('ping', { detail: { n: 7 } }))",
    );
    assert_eq!(run(&mut rt, world, "got.join()"), "true,7,true");
}

#[test]
fn the_class_of_a_mirrored_event_is_not_the_page_s_to_name() {
    let (mut rt, world) = two();
    node_in_both(&mut rt, world);
    run(
        &mut rt,
        world,
        "globalThis.got = []; globalThis.Evil = function () {}; \
         node.addEventListener('ping', (e) => got.push(e instanceof CustomEvent, e instanceof Evil));",
    );
    run(
        &mut rt,
        MAIN_WORLD,
        "const e = new CustomEvent('ping'); \
         Object.defineProperty(e, 'constructor', { value: { name: 'Evil' } }); \
         node.dispatchEvent(e)",
    );
    assert_eq!(run(&mut rt, world, "got.join()"), "true,false");
}

#[test]
fn a_target_of_the_listeners_own_world_is_heard_when_another_world_dispatches() {
    let (mut rt, world) = two();
    run(
        &mut rt,
        world,
        "globalThis.t = new EventTarget(); globalThis.got = []; \
         t.addEventListener('change', (e) => got.push(e.target === t, e instanceof Event));",
    );
    let target = rt.world_global(world).expect("global");
    let target = target
        .get(js_string!("t"), rt.context())
        .expect("the target");
    let main = rt.world_global(MAIN_WORLD).expect("global");
    main.set(js_string!("t"), target, false, rt.context())
        .expect("set");
    // The page's own `dispatchEvent`, as the host's dispatch from Rust runs it.
    run(
        &mut rt,
        MAIN_WORLD,
        "EventTarget.prototype.dispatchEvent.call(t, new Event('change'))",
    );
    assert_eq!(run(&mut rt, world, "got.join()"), "true,true");
}

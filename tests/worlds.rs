//! Worlds: a realm of the runtime's own context with web globals of its own, over the one event
//! loop; and the listeners of one node in two worlds, heard in the order they were added.

use boa_engine::{JsObject, JsValue, js_string};
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

use crate::builtins::map::ordered_map::OrderedMap;
use crate::{JsNativeErrorKind, JsValue, TestAction, run_test_actions};
use boa_macros::js_str;
use indoc::indoc;

#[test]
fn construct() {
    run_test_actions([
        TestAction::assert_eq("(new Map()).size", 0),
        TestAction::assert_eq("(new Map([['1', 'one'], ['2', 'two']])).size", 2),
    ]);
}

#[test]
fn clone() {
    run_test_actions([
        TestAction::run(indoc! {r#"
                let original = new Map([["1", "one"], ["2", "two"]]);
                let clone = new Map(original);
            "#}),
        TestAction::assert_eq("clone.size", 2),
        TestAction::assert_eq("original.set('3', 'three'); original.size", 3),
        TestAction::assert_eq("clone.size", 2),
    ]);
}

#[test]
fn symbol_iterator() {
    run_test_actions([
        TestAction::run_harness(),
        TestAction::run(indoc! {r#"
                const map1 = new Map();
                map1.set('0', 'foo');
                map1.set(1, 'bar');
                const iterator = map1[Symbol.iterator]();
                let item1 = iterator.next();
                let item2 = iterator.next();
                let item3 = iterator.next();
            "#}),
        TestAction::assert("arrayEquals(item1.value, ['0', 'foo'])"),
        TestAction::assert("arrayEquals(item2.value, [1, 'bar'])"),
        TestAction::assert_eq("item3.value", JsValue::undefined()),
        TestAction::assert("item3.done"),
    ]);
}

// Should behave the same as symbol_iterator
#[test]
fn entries() {
    run_test_actions([
        TestAction::run_harness(),
        TestAction::run(indoc! {r#"
                const map1 = new Map();
                map1.set('0', 'foo');
                map1.set(1, 'bar');
                const iterator = map1.entries();
                let item1 = iterator.next();
                let item2 = iterator.next();
                let item3 = iterator.next();
            "#}),
        TestAction::assert("arrayEquals(item1.value, ['0', 'foo'])"),
        TestAction::assert("arrayEquals(item2.value, [1, 'bar'])"),
        TestAction::assert_eq("item3.value", JsValue::undefined()),
        TestAction::assert("item3.done"),
    ]);
}

#[test]
fn merge() {
    run_test_actions([
        TestAction::run(indoc! {r#"
                let first = new Map([["1", "one"], ["2", "two"]]);
                let second = new Map([["2", "second two"], ["3", "three"]]);
                let third = new Map([["4", "four"], ["5", "five"]]);
                let merged1 = new Map([...first, ...second]);
                let merged2 = new Map([...second, ...third]);
            "#}),
        TestAction::assert_eq("merged1.size", 3),
        TestAction::assert_eq("merged1.get('2')", js_str!("second two")),
        TestAction::assert_eq("merged2.size", 4),
    ]);
}

#[test]
fn get() {
    run_test_actions([
        TestAction::run(indoc! {r#"
                let map = new Map([["1", "one"], ["2", "two"]]);
            "#}),
        TestAction::assert_eq("map.get('1')", js_str!("one")),
        TestAction::assert_eq("map.get('2')", js_str!("two")),
        TestAction::assert_eq("map.get('3')", JsValue::undefined()),
        TestAction::assert_eq("map.get()", JsValue::undefined()),
    ]);
}

#[test]
fn set() {
    run_test_actions([
        TestAction::run("let map = new Map();"),
        TestAction::assert("map.set(); map.has(undefined)"),
        TestAction::assert_eq("map.get()", JsValue::undefined()),
        TestAction::assert_eq("map.set('1', 'one'); map.get('1')", js_str!("one")),
        TestAction::assert("map.set('2'); map.has('2')"),
        TestAction::assert_eq("map.get('2')", JsValue::undefined()),
    ]);
}

#[test]
fn clear() {
    run_test_actions([
        TestAction::run(indoc! {r#"
                let map = new Map([["1", "one"], ["2", "two"]]);
                map.clear();
            "#}),
        TestAction::assert_eq("map.size", 0),
    ]);
}

#[test]
fn delete() {
    run_test_actions([
        TestAction::run("let map = new Map([['1', 'one'], ['2', 'two']]);"),
        TestAction::assert_eq("map.size", 2),
        TestAction::assert("map.delete('1')"),
        TestAction::assert("!map.has('1')"),
        TestAction::assert("!map.delete('1')"),
    ]);
}

#[test]
fn has() {
    run_test_actions([
        TestAction::run("let map = new Map([['1', 'one']]);"),
        TestAction::assert("!map.has()"),
        TestAction::assert("map.has('1')"),
        TestAction::assert("!map.has('2')"),
    ]);
}

#[test]
fn keys() {
    run_test_actions([
        TestAction::run(indoc! {r#"
                const map1 = new Map();
                map1.set('0', 'foo');
                map1.set(1, 'bar');
                const keysIterator = map1.keys();
                let item1 = keysIterator.next();
                let item2 = keysIterator.next();
                let item3 = keysIterator.next();
            "#}),
        TestAction::assert_eq("item1.value", js_str!("0")),
        TestAction::assert_eq("item2.value", 1),
        TestAction::assert("item3.done"),
    ]);
}

#[test]
fn for_each() {
    run_test_actions([
        TestAction::run(indoc! {r#"
                let map = new Map([[1, 5], [2, 10], [3, 15]]);
                let valueSum = 0;
                let keySum = 0;
                let sizeSum = 0;
                function callingCallback(value, key, map) {
                    valueSum += value;
                    keySum += key;
                    sizeSum += map.size;
                }
                map.forEach(callingCallback);
            "#}),
        TestAction::assert_eq("valueSum", 30),
        TestAction::assert_eq("keySum", 6),
        TestAction::assert_eq("sizeSum", 9),
    ]);
}

#[test]
fn values() {
    run_test_actions([
        TestAction::run(indoc! {r#"
                const map1 = new Map();
                map1.set('0', 'foo');
                map1.set(1, 'bar');
                const valuesIterator = map1.values();
                let item1 = valuesIterator.next();
                let item2 = valuesIterator.next();
                let item3 = valuesIterator.next();
            "#}),
        TestAction::assert_eq("item1.value", js_str!("foo")),
        TestAction::assert_eq("item2.value", js_str!("bar")),
        TestAction::assert("item3.done"),
    ]);
}

#[test]
fn modify_key() {
    run_test_actions([
        TestAction::run(indoc! {r#"
                let obj = new Object();
                let map = new Map([[obj, "one"]]);
                obj.field = "Value";
            "#}),
        TestAction::assert_eq("map.get(obj)", js_str!("one")),
    ]);
}

#[test]
fn order() {
    run_test_actions([
        TestAction::run_harness(),
        TestAction::run("let map = new Map([[1, 'one'], [2, 'two']]);"),
        TestAction::assert("arrayEquals(Array.from(map.keys()), [1, 2])"),
        TestAction::run("map.set(1, 'five')"),
        TestAction::assert("arrayEquals(Array.from(map.keys()), [1, 2])"),
        TestAction::run("map.set()"),
        TestAction::assert("arrayEquals(Array.from(map.keys()), [1, 2, undefined])"),
        TestAction::run("map.delete(2)"),
        TestAction::assert("arrayEquals(Array.from(map.keys()), [1, undefined])"),
        TestAction::run("map.set(2, 'two')"),
        TestAction::assert("arrayEquals(Array.from(map.keys()), [1, undefined, 2])"),
    ]);
}

#[test]
fn recursive_display() {
    run_test_actions([
        TestAction::run(indoc! {r#"
                let map = new Map();
                let array = new Array([map]);
            "#}),
        TestAction::assert_with_op("map.set('y', map)", |v, _| {
            v.display().to_string() == r#"Map { "y" → Map(1) }"#
        }),
        TestAction::assert_with_op("map.set('z', array)", |v, _| {
            v.display().to_string() == r#"Map { "y" → Map(2), "z" → Array(1) }"#
        }),
    ]);
}

#[test]
fn not_a_function() {
    run_test_actions([TestAction::assert_native_error(
        "let map = Map()",
        JsNativeErrorKind::Type,
        "cannot call `Map` constructor without new",
    )]);
}

#[test]
fn for_each_delete() {
    run_test_actions([
        TestAction::run_harness(),
        TestAction::run(indoc! {r#"
                let map = new Map([[0, "a"], [1, "b"], [2, "c"]]);
                let result = [];
                map.forEach(function(value, key) {
                    if (key === 0) {
                        map.delete(0);
                        map.set(3, "d");
                    }
                    result.push([key, value]);
                })
            "#}),
        TestAction::assert(indoc! {r#"
                arrayEquals(
                    result,
                    [
                        [0, "a"],
                        [1, "b"],
                        [2, "c"],
                        [3, "d"]
                    ]
                )
            "#}),
    ]);
}

#[test]
fn for_of_delete() {
    run_test_actions([
        TestAction::run_harness(),
        TestAction::run(indoc! {r#"
                let map = new Map([[0, "a"], [1, "b"], [2, "c"]]);
                let result = [];
                for (a of map) {
                    if (a[0] === 0) {
                        map.delete(0);
                        map.set(3, "d");
                    }
                    result.push([a[0], a[1]]);
                }
            "#}),
        TestAction::assert(indoc! {r#"
                arrayEquals(
                    result,
                    [
                        [0, "a"],
                        [1, "b"],
                        [2, "c"],
                        [3, "d"]
                    ]
                )
            "#}),
    ]);
}

#[test]
fn get_or_insert_inserts_on_miss() {
    run_test_actions([
        TestAction::run("let map = new Map();"),
        TestAction::assert_eq("map.getOrInsert('x', 42)", 42),
        TestAction::assert("map.has('x')"),
        TestAction::assert_eq("map.get('x')", 42),
    ]);
}

#[test]
fn get_or_insert_returns_existing_on_hit() {
    run_test_actions([
        TestAction::run("let map = new Map([['y', 99]]);"),
        TestAction::assert_eq("map.getOrInsert('y', 123)", 99),
        TestAction::assert_eq("map.get('y')", 99), // unchanged
    ]);
}

#[test]
fn get_or_insert_canonicalizes_key() {
    run_test_actions([
        TestAction::run("let map = new Map();"),
        // -0 and +0 should canonicalize to +0
        TestAction::assert_eq("map.getOrInsert(-0, 'minus zero')", js_str!("minus zero")),
        TestAction::assert("map.has(0)"),
        TestAction::assert_eq("map.get(0)", js_str!("minus zero")),
    ]);
}

#[test]
fn get_or_insert_with_undefined_value() {
    run_test_actions([
        TestAction::run("let map = new Map();"),
        TestAction::assert_eq("map.getOrInsert('z', undefined)", JsValue::undefined()),
        TestAction::assert("map.has('z')"),
        TestAction::assert_eq("map.get('z')", JsValue::undefined()),
    ]);
}

#[test]
fn get_or_insert_computed_this_not_map() {
    run_test_actions([TestAction::assert_native_error(
        "Map.prototype.getOrInsertComputed.call({}, 'k', x => x)",
        JsNativeErrorKind::Type,
        "method `Map.prototype.getOrInsertComputed` called on incompatible receiver",
    )]);
}

#[test]
fn get_or_insert_computed_requires_callable() {
    run_test_actions([TestAction::assert_native_error(
        "new Map().getOrInsertComputed('k', undefined)",
        JsNativeErrorKind::Type,
        "method `Map.prototype.getOrInsertComputed` called with non-callable callback function",
    )]);
}

#[test]
fn get_or_insert_computed_not_called_on_hit() {
    run_test_actions([
        TestAction::run("const m = new Map([['k', 7]]); let calls = 0;"),
        TestAction::assert_eq(
            "m.getOrInsertComputed('k', (key) => { calls++; return 1; })",
            7,
        ),
        TestAction::assert_eq("calls", 0),
        TestAction::assert_eq("m.get('k')", 7),
    ]);
}

#[test]
fn get_or_insert_computed_this_is_undefined_and_key_canonicalized() {
    run_test_actions([
        TestAction::run(
            r#"
            const m = new Map();
            let seenThis, seenKey;
            const v = m.getOrInsertComputed(-0, function(k) { 'use strict'; seenThis = this; seenKey = k; return 'ok'; });
        "#,
        ),
        // `this` inside callback is undefined
        TestAction::assert("seenThis === undefined"),
        // key argument is canonicalized (-0 → +0)
        TestAction::assert("Object.is(seenKey, 0)"),
        TestAction::assert_eq("v", js_str!("ok")),
        TestAction::assert("m.has(0)"),
        TestAction::assert_eq("m.get(0)", js_str!("ok")),
    ]);
}

#[test]
fn delete_keeps_the_order_of_the_remaining_keys() {
    run_test_actions([
        TestAction::run_harness(),
        TestAction::run(indoc! {r#"
                const map = new Map();
                for (let i = 0; i < 6; i++) map.set(i, i * 10);
                map.delete(1);
                map.delete(4);
                map.delete(1);
            "#}),
        TestAction::assert("arrayEquals(Array.from(map.keys()), [0, 2, 3, 5])"),
        TestAction::assert("arrayEquals(Array.from(map.values()), [0, 20, 30, 50])"),
        TestAction::assert_eq("map.size", 4),
        // A key added after a deletion goes last, as in Node 22.
        TestAction::assert("map.set(1, 10); arrayEquals(Array.from(map.keys()), [0, 2, 3, 5, 1])"),
    ]);
}

#[test]
fn delete_compacts_once_tombstones_outnumber_the_entries() {
    let mut map = OrderedMap::new();
    for i in 0..100 {
        map.insert(JsValue::new(i), i);
    }
    for i in 0..60 {
        map.remove(&JsValue::new(i));
    }

    // `full_len` counts the slots of the index map, tombstones included. The 51st deletion left the
    // tombstones outnumbering the entries that were left, so that one dropped them; the nine after
    // it left nine tombstones, in 49 slots for the 40 entries that remain.
    assert_eq!(map.len(), 40);
    assert_eq!(map.full_len(), 49);
    assert_eq!(map.get(&JsValue::new(60)), Some(&60));
}

/// An iterator that runs to its end has to give the lock back: it takes the lock out of itself and
/// drops it as a plain value, which never calls `Finalize`. While a lock was held, nothing
/// compacted, so a map that had been through a `for…of` or an `Array.from` grew without bound.
#[test]
fn delete_compacts_after_an_iteration_has_run_to_its_end() {
    run_test_actions([
        TestAction::run(indoc! {r#"
                const map = new Map();
                for (let i = 0; i < 100; i++) map.set(i, i);
                const spread = [...map];
                const from = Array.from(map.entries());
                for (let i = 0; i < 60; i++) map.delete(i);
                globalThis.iterated = map;
            "#}),
        TestAction::assert_eq("spread.length", 100),
        TestAction::assert_eq("from.length", 100),
        TestAction::inspect_context(|context| {
            let map = context
                .global_object()
                .get(js_str!("iterated"), context)
                .expect("`iterated` is a global")
                .as_object()
                .expect("a Map is an object")
                .clone()
                .downcast::<OrderedMap<JsValue>>()
                .expect("the global holds a Map");

            let map = map.borrow();
            assert_eq!(map.data().len(), 40);
            assert_eq!(map.data().full_len(), 49);
        }),
    ]);
}

/// A copy of a map is nobody's iteration: it starts unlocked, so it can compact.
#[test]
fn a_copy_of_a_locked_map_can_compact() {
    let mut map = OrderedMap::new();
    for i in 0..100 {
        map.insert(JsValue::new(i), i);
    }
    map.lock();

    let mut copy = map.clone();
    for i in 0..60 {
        copy.remove(&JsValue::new(i));
    }

    assert_eq!(copy.len(), 40);
    assert_eq!(copy.full_len(), 49);
    map.unlock();
}

#[test]
fn for_of_delete_and_add_during_iteration() {
    run_test_actions([
        TestAction::run_harness(),
        TestAction::run(indoc! {r#"
                const map = new Map([[1, "a"], [2, "b"], [3, "c"]]);
                const seen = [];
                for (const [key] of map) {
                    seen.push(key);
                    if (key === 1) {
                        map.delete(2);
                        map.set(4, "d");
                    }
                }
            "#}),
        // Node 22.21.1 prints `[1,3,4] [1,3,4]` for this program.
        TestAction::assert("arrayEquals(seen, [1, 3, 4])"),
        TestAction::assert("arrayEquals(Array.from(map.keys()), [1, 3, 4])"),
    ]);
}

#[test]
fn delete_inside_for_each_skips_the_deleted_key() {
    run_test_actions([
        TestAction::run_harness(),
        TestAction::run(indoc! {r#"
                const map = new Map([[1, "a"], [2, "b"], [3, "c"], [4, "d"]]);
                const seen = [];
                map.forEach((value, key) => {
                    seen.push(key);
                    if (key === 1) map.delete(2);
                    if (key === 3) map.delete(3);
                });
            "#}),
        TestAction::assert("arrayEquals(seen, [1, 3, 4])"),
        TestAction::assert("arrayEquals(Array.from(map.keys()), [1, 4])"),
    ]);
}

/// `JsMap::for_each_native` — the Rust-side walk behind `TryFromJs` — has to step over a
/// tombstone left by an ordinary `delete`, as the JS iterators do.
#[test]
fn for_each_native_steps_over_a_deleted_entry() {
    use crate::object::builtins::JsMap;

    run_test_actions([
        TestAction::run(indoc! {r#"
                const map = new Map([["a", 1], ["b", 2], ["c", 3]]);
                map.delete("b");
                globalThis.converted = map;
            "#}),
        TestAction::inspect_context(|context| {
            let value = context
                .global_object()
                .get(js_str!("converted"), context)
                .expect("`converted` is a global");
            let js_map = JsMap::from_object(value.as_object().expect("a Map is an object"))
                .expect("the global holds a Map");

            let mut pairs = Vec::new();
            js_map
                .for_each_native(|key, value| {
                    pairs.push((
                        key.to_string(context)?.to_std_string_escaped(),
                        value.to_number(context)?,
                    ));
                    Ok(())
                })
                .expect("walking the map cannot fail here");

            assert_eq!(pairs, vec![("a".to_string(), 1.0), ("c".to_string(), 3.0)]);
        }),
    ]);
}

#[test]
fn clear_keeps_a_live_iterator_running() {
    // `Map.prototype.clear` empties each entry instead of removing it, so an iterator that is
    // part-way through keeps its position and goes on to see entries added afterwards.
    run_test_actions([
        TestAction::run(
            "const m = new Map([['a', 1], ['b', 2], ['c', 3]]);
             const seen = [];
             const it = m.entries();
             seen.push(it.next().value[0]);
             m.clear();
             m.set('z', 9);
             const afterClear = it.next();
             seen.push(afterClear.done ? 'done' : afterClear.value[0]);
             for (const entry of m) seen.push(entry[0]);",
        ),
        TestAction::assert("seen.join() === 'a,z,z'"),
        TestAction::assert_eq("m.size", 1),
        // A second `clear()` on an already empty map is a no-op.
        TestAction::run("m.clear();"),
        TestAction::assert_eq("m.size", 0),
        // Clearing before the iterator starts still yields only what is inserted afterwards.
        TestAction::run(
            "const fresh = new Map([['a', 1]]);
             const freshIt = fresh.entries();
             fresh.clear();
             fresh.set('q', 7);
             const firstAfter = freshIt.next();",
        ),
        TestAction::assert("firstAfter.done === false && firstAfter.value[0] === 'q'"),
        // A `for…of` that clears the map while walking it terminates.
        TestAction::run(
            "const selfClearing = new Map([['a', 1], ['b', 2]]);
             let rounds = 0;
             for (const entry of selfClearing) {
                 rounds++;
                 selfClearing.clear();
                 if (rounds > 3) break;
             }",
        ),
        TestAction::assert("rounds <= 3"),
    ]);
}

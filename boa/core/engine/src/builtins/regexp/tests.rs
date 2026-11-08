use crate::{
    JsNativeErrorKind, JsValue, TestAction, js_string, native_function::NativeFunctionObject,
    run_test_actions,
};
use boa_macros::js_str;
use indoc::indoc;

#[test]
fn constructors() {
    run_test_actions([
        TestAction::run(indoc! {r#"
                var constructed = new RegExp("[0-9]+(\\.[0-9]+)?");
                var literal = /[0-9]+(\.[0-9]+)?/;
                var ctor_literal = new RegExp(/[0-9]+(\.[0-9]+)?/);
            "#}),
        TestAction::assert("constructed.test('1.0')"),
        TestAction::assert("literal.test('1.0')"),
        TestAction::assert("ctor_literal.test('1.0')"),
    ]);
}

#[test]
fn species() {
    run_test_actions([
        TestAction::run(indoc! {r#"
        var descriptor = Object.getOwnPropertyDescriptor(RegExp, Symbol.species);
        var accessor = descriptor.get;
        var name = Object.getOwnPropertyDescriptor(accessor, "name");
        var length = Object.getOwnPropertyDescriptor(accessor, "length");
        var thisVal = {};
        "#}),
        // length
        TestAction::assert_eq("length.value", 0),
        TestAction::assert("!length.enumerable"),
        TestAction::assert("!length.writable"),
        TestAction::assert("length.configurable"),
        // return-value
        TestAction::assert("Object.is(accessor.call(thisVal), thisVal)"),
        // symbol-species-name
        TestAction::assert_eq("name.value", js_str!("get [Symbol.species]")),
        TestAction::assert("!name.enumerable"),
        TestAction::assert("!name.writable"),
        TestAction::assert("name.configurable"),
        // symbol-species
        TestAction::assert_eq("descriptor.set", JsValue::undefined()),
        TestAction::assert_with_op("accessor", |v, _| {
            v.as_object()
                .is_some_and(|o| o.is::<NativeFunctionObject>())
        }),
        TestAction::assert("!descriptor.enumerable"),
        TestAction::assert("descriptor.configurable"),
    ]);
}

#[test]
fn flags() {
    run_test_actions([
        TestAction::run(indoc! {r#"
                var re_gi = /test/gi;
                var re_sm = /test/sm;
                var re_u = /test/u;
            "#}),
        TestAction::assert("re_gi.global"),
        TestAction::assert("re_gi.ignoreCase"),
        TestAction::assert("!re_gi.multiline"),
        TestAction::assert("!re_gi.dotAll"),
        TestAction::assert("!re_gi.unicode"),
        TestAction::assert("!re_gi.sticky"),
        TestAction::assert_eq("re_gi.flags", js_str!("gi")),
        //
        TestAction::assert("!re_sm.global"),
        TestAction::assert("!re_sm.ignoreCase"),
        TestAction::assert("re_sm.multiline"),
        TestAction::assert("re_sm.dotAll"),
        TestAction::assert("!re_sm.unicode"),
        TestAction::assert("!re_sm.sticky"),
        TestAction::assert_eq("re_sm.flags", js_str!("ms")),
        //
        TestAction::assert("!re_u.global"),
        TestAction::assert("!re_u.ignoreCase"),
        TestAction::assert("!re_u.multiline"),
        TestAction::assert("!re_u.dotAll"),
        TestAction::assert("re_u.unicode"),
        TestAction::assert("!re_u.sticky"),
        TestAction::assert_eq("re_u.flags", js_str!("u")),
    ]);
}

#[test]
fn escape() {
    run_test_actions([
        TestAction::assert_eq(
            r"RegExp.escape('The Quick Brown Fox')",
            js_str!("\\x54he\\x20Quick\\x20Brown\\x20Fox"),
        ),
        TestAction::assert_eq(
            r"RegExp.escape('Buy it. use it. break it. fix it.')",
            js_str!("\\x42uy\\x20it\\.\\x20use\\x20it\\.\\x20break\\x20it\\.\\x20fix\\x20it\\."),
        ),
        TestAction::assert_eq(r"RegExp.escape('(*.*)')", js_str!("\\(\\*\\.\\*\\)")),
        TestAction::assert_eq(r"RegExp.escape('｡^･ｪ･^｡')", js_str!("｡\\^･ｪ･\\^｡")),
        TestAction::assert_eq(
            r"RegExp.escape('😊 *_* +_+ ... 👍')",
            js_str!("😊\\x20\\*_\\*\\x20\\+_\\+\\x20\\.\\.\\.\\x20👍"),
        ),
        TestAction::assert_eq(
            r"RegExp.escape('\\d \\D (?:)')",
            js_str!("\\\\d\\x20\\\\D\\x20\\(\\?\\x3a\\)"),
        ),
        TestAction::assert_native_error(
            "RegExp.escape(1)",
            JsNativeErrorKind::Type,
            "RegExp.escape requires a string argument",
        ),
    ]);
}

#[test]
fn last_index() {
    run_test_actions([
        TestAction::run(r"var regex = /[0-9]+(\.[0-9]+)?/g;"),
        TestAction::assert_eq("regex.lastIndex", 0),
        TestAction::assert("regex.test('1.0foo')"),
        TestAction::assert_eq("regex.lastIndex", 3),
        TestAction::assert("!regex.test('1.0foo')"),
        TestAction::assert_eq("regex.lastIndex", 0),
    ]);
}

#[test]
fn exec() {
    run_test_actions([
        TestAction::run_harness(),
        TestAction::run(indoc! {r"
                var re = /quick\s(brown).+?(jumps)/ig;
                var result = re.exec('The Quick Brown Fox Jumps Over The Lazy Dog');
            "}),
        TestAction::assert(indoc! {r#"
            arrayEquals(
                result,
                ["Quick Brown Fox Jumps", "Brown", "Jumps"]
            )
        "#}),
        TestAction::assert_eq("result.index", 4),
        TestAction::assert_eq(
            "result.input",
            js_str!("The Quick Brown Fox Jumps Over The Lazy Dog"),
        ),
    ]);
}

#[test]
fn no_panic_on_parse_fail() {
    run_test_actions([
        TestAction::assert_native_error(
            r"var re = /]/u;",
            JsNativeErrorKind::Syntax,
            "Invalid regular expression literal: Invalid atom character at line 1, col 10",
        ),
        TestAction::assert_native_error(
            r"var re = /a{/u;",
            JsNativeErrorKind::Syntax,
            "Invalid regular expression literal: Invalid quantifier at line 1, col 10",
        ),
    ]);
}

#[test]
fn to_string() {
    run_test_actions([
        TestAction::assert_eq("(new RegExp('a+b+c')).toString()", js_str!("/a+b+c/")),
        TestAction::assert_eq("(new RegExp('bar', 'g')).toString()", js_str!("/bar/g")),
        TestAction::assert_eq(r"(new RegExp('\\n', 'g')).toString()", js_string!(r"/\n/g")),
        TestAction::assert_eq(r"/\n/g.toString()", js_string!(r"/\n/g")),
        TestAction::assert_eq(r"/,\;/.toString()", js_string!(r"/,\;/")),
    ]);
}
#[test]
fn search() {
    const ERROR: &str = "RegExp.prototype[Symbol.search] method called on incompatible value";
    run_test_actions([
        TestAction::run(indoc! {r#"
                var search = Object.getOwnPropertyDescriptor(RegExp.prototype, Symbol.search);
                var length = Object.getOwnPropertyDescriptor(search.value, 'length');
                var name = Object.getOwnPropertyDescriptor(search.value, 'name');
            "#}),
        // prop-desc
        TestAction::assert("!search.enumerable"),
        TestAction::assert("search.writable"),
        TestAction::assert("search.configurable"),
        // length
        TestAction::assert_eq("length.value", 1),
        TestAction::assert("!length.enumerable"),
        TestAction::assert("!length.writable"),
        TestAction::assert("length.configurable"),
        // name
        TestAction::assert_eq("name.value", js_str!("[Symbol.search]")),
        TestAction::assert("!name.enumerable"),
        TestAction::assert("!name.writable"),
        TestAction::assert("name.configurable"),
        // success-return-val
        TestAction::assert_eq("/a/[Symbol.search]('abc')", 0),
        TestAction::assert_eq("/b/[Symbol.search]('abc')", 1),
        TestAction::assert_eq("/c/[Symbol.search]('abc')", 2),
        // failure-return-val
        TestAction::assert_eq("/z/[Symbol.search]('a')", -1),
        // coerce-string
        TestAction::assert_eq(
            indoc! {r#"
                /ring/[Symbol.search]({
                    toString: function() {
                        return 'toString value';
                    }
                });
            "#},
            4,
        ),
        // this-val-non-obj
        TestAction::assert_native_error("search.value.call()", JsNativeErrorKind::Type, ERROR),
        TestAction::assert_native_error(
            "search.value.call(undefined)",
            JsNativeErrorKind::Type,
            ERROR,
        ),
        TestAction::assert_native_error("search.value.call(null)", JsNativeErrorKind::Type, ERROR),
        TestAction::assert_native_error("search.value.call(true)", JsNativeErrorKind::Type, ERROR),
        TestAction::assert_native_error(
            "search.value.call('string')",
            JsNativeErrorKind::Type,
            ERROR,
        ),
        TestAction::assert_native_error(
            "search.value.call(Symbol.search)",
            JsNativeErrorKind::Type,
            ERROR,
        ),
        TestAction::assert_native_error("search.value.call(86)", JsNativeErrorKind::Type, ERROR),
        // u-lastindex-advance
        TestAction::assert_eq(r"/\udf06/u[Symbol.search]('\ud834\udf06')", -1),
        TestAction::assert_eq("/a/[Symbol.search](\"a\")", 0),
        TestAction::assert_eq("/a/[Symbol.search](\"ba\")", 1),
        TestAction::assert_eq("/a/[Symbol.search](\"bb\")", -1),
        TestAction::assert_eq("/u/[Symbol.search](null)", 1),
        TestAction::assert_eq("/d/[Symbol.search](undefined)", 2),
    ]);
}

#[test]
fn regular_expression_construction_independent_of_global_reg_exp() {
    let regex = "/abc/";
    run_test_actions([
        TestAction::run(regex),
        TestAction::run("RegExp = null"),
        TestAction::run(regex),
    ]);
}

#[test]
fn regexp_no_panic_on_empty_class_quantifier() {
    // Regression test for https://github.com/boa-dev/boa/issues/2718
    // `/[]*1/u.exec()` previously caused a panic in the regress crate.
    // It should return null without panicking.
    run_test_actions([TestAction::assert_eq("/[]*1/u.exec()", JsValue::null())]);
}

#[test]
fn match_all_over_a_latin1_subject_keeps_the_last_index_and_input() {
    run_test_actions([
        TestAction::run_harness(),
        TestAction::run(indoc! {r#"
                const subject = "2026-11-03 INFO a\n2026-11-04 WARN b\n";
                const re = /(\d{4})-(\d\d)-(\d\d) (\w+) (\w)/g;
                const matches = [];
                for (const m of subject.matchAll(re)) {
                    matches.push([m[0], m[1], m[4], m.index, m.input === subject]);
                }
            "#}),
        // Node 22.21.1 prints `[["2026-11-03 INFO a","2026","INFO",0,true],["2026-11-04 WARN b","2026","WARN",18,true]]`.
        TestAction::assert(indoc! {r#"
                matches.length === 2 &&
                matches[0][0] === "2026-11-03 INFO a" &&
                matches[0][1] === "2026" &&
                matches[0][2] === "INFO" &&
                matches[0][3] === 0 &&
                matches[0][4] === true &&
                matches[1][0] === "2026-11-04 WARN b" &&
                matches[1][3] === 18 &&
                matches[1][4] === true
            "#}),
    ]);
}

#[test]
fn replace_over_a_latin1_subject_replaces_every_match() {
    run_test_actions([
        TestAction::run_harness(),
        TestAction::run(indoc! {r#"
                const subject = "2026-11-03 INFO a\n2026-11-04 WARN b\n";
                const re = /(\d{4})-(\d\d)-(\d\d) (\w+) (\w)/g;
                const replaced = subject.replace(re, "$4-$5");
                const wide = "2026-11-03 信息 行 7\n2026-11-04 警告 行 8\n";
                const wideRe = /(\d{4})-(\d\d)-(\d\d) (\S+) \S+ (\d)/g;
                const wideReplaced = wide.replace(wideRe, "$4-$5");
            "#}),
        // Both strings come from Node 22.21.1.
        TestAction::assert_eq("replaced", js_str!("INFO-a\nWARN-b\n")),
        TestAction::assert_eq("wideReplaced", js_str!("信息-7\n警告-8\n")),
    ]);
}

#[test]
fn exec_loop_over_a_latin1_subject_advances_once_per_match() {
    run_test_actions([
        TestAction::run(indoc! {r#"
                const subject = "a1 b2 c3";
                const re = /[a-z](\d)/g;
                const seen = [];
                let m;
                while ((m = re.exec(subject)) !== null) {
                    seen.push([m[0], m[1], m.index, re.lastIndex]);
                }
                const after = re.lastIndex;
                const again = re.exec(subject);
            "#}),
        // Node 22.21.1 prints `[["a1","1",0,2],["b2","2",3,5],["c3","3",6,8]] 0 a1`.
        TestAction::assert(indoc! {r#"
                seen.length === 3 &&
                seen[0][0] === "a1" && seen[0][1] === "1" && seen[0][2] === 0 && seen[0][3] === 2 &&
                seen[1][0] === "b2" && seen[1][2] === 3 && seen[1][3] === 5 &&
                seen[2][0] === "c3" && seen[2][2] === 6 && seen[2][3] === 8 &&
                after === 0 && again !== null && again[0] === "a1"
            "#}),
    ]);
}

#[test]
fn split_on_a_latin1_subject_captures_groups() {
    run_test_actions([
        TestAction::run_harness(),
        TestAction::assert(
            "arrayEquals('a1b2c3'.split(/(\\d)/), ['a', '1', 'b', '2', 'c', '3', ''])",
        ),
        TestAction::assert("arrayEquals('a1b2c3'.split(/(\\d)/, 3), ['a', '1', 'b'])"),
    ]);
}

/// One pattern scanning two different Latin-1 subjects in turn: the widened copy belongs to the
/// subject it was made from, so neither scan sees the other's code units.
#[test]
fn one_pattern_scans_two_latin1_subjects_in_turn() {
    run_test_actions([
        TestAction::run_harness(),
        TestAction::run(indoc! {r#"
                const re = /\d/g;
                const first = "a1 b2 c3";
                const second = "d4 e5";
                const indexes = (subject) => Array.from(subject.matchAll(re), (m) => m.index);
                const firstAgain = indexes(first);
                const secondIndexes = indexes(second);
                const firstThird = indexes(first);
            "#}),
        // Node 22.21.1 prints `[1,4,7] [1,4] [1,4,7]` for this program.
        TestAction::assert("arrayEquals(firstAgain, [1, 4, 7])"),
        TestAction::assert("arrayEquals(secondIndexes, [1, 4])"),
        TestAction::assert("arrayEquals(firstThird, [1, 4, 7])"),
    ]);
}

/// The widened Latin-1 subject is kept for the matches that follow within one scan and dropped at
/// its end, so a single `test` on a big file does not leave a copy of it resident.
#[test]
fn a_scan_keeps_the_widened_subject_only_while_it_runs() {
    fn widened(_: &mut crate::Context) -> bool {
        super::WIDENED.with_borrow(Option::is_some)
    }

    run_test_actions([
        TestAction::run("const re = /a/g; const subject = 'aXaY';"),
        // A global search that matched comes back for the next match, so it keeps the widening.
        TestAction::assert("re.exec(subject) !== null && re.lastIndex === 1"),
        TestAction::inspect_context(|context| assert!(widened(context), "a running scan keeps it")),
        // The failure that ends the scan drops it.
        TestAction::assert("re.exec(subject) !== null && re.lastIndex === 3"),
        TestAction::assert("re.exec(subject) === null"),
        TestAction::inspect_context(|context| {
            assert!(!widened(context), "a finished scan drops it");
        }),
        // A search that is neither global nor sticky has one match, and drops it there.
        TestAction::assert("/a/.test('aXaY')"),
        TestAction::inspect_context(|context| {
            assert!(!widened(context), "a single search drops it");
        }),
    ]);
}

/// `split` probes the subject with a sticky splitter and keeps the widening between probes, so
/// the split itself has to drop it — including on the `limit` returns and the empty-subject path.
#[test]
fn a_split_drops_the_widened_subject_at_every_exit() {
    fn widened(_: &mut crate::Context) -> bool {
        super::WIDENED.with_borrow(Option::is_some)
    }

    run_test_actions([
        TestAction::run_harness(),
        TestAction::run("const subject = 'a,b,c';"),
        TestAction::assert("arrayEquals(subject.split(/,/), ['a', 'b', 'c'])"),
        TestAction::inspect_context(|context| {
            assert!(!widened(context), "a finished split drops it");
        }),
        // Node 22.21.1 prints `[a,b]` for a limit of 2.
        TestAction::assert("arrayEquals('a,b,c'.split(/,/, 2), ['a', 'b'])"),
        TestAction::inspect_context(|context| {
            assert!(!widened(context), "a split stopped by its limit drops it");
        }),
        TestAction::assert("arrayEquals(''.split(/,/), [''])"),
        TestAction::inspect_context(|context| {
            assert!(!widened(context), "a split of an empty subject drops it");
        }),
        // A search runs one `exec`, which keeps the widening for a next match that never comes.
        TestAction::assert("/b/g.test(subject) && 'a,b'.search(/b/g) === 2"),
        TestAction::inspect_context(|context| {
            assert!(!widened(context), "a search drops it");
        }),
        // A getter that throws leaves the same way.
        TestAction::run(indoc! {r#"
                const bad = /,/;
                Object.defineProperty(bad, "flags", {
                    get() { throw new Error("nope"); },
                });
            "#}),
        TestAction::assert(
            "(function () { try { 'a,b'.split(bad); return false; } catch (e) { return e.message === 'nope'; } })()",
        ),
        TestAction::inspect_context(|context| {
            assert!(
                !widened(context),
                "a split that threw on the way out drops it"
            );
        }),
    ]);
}

#[test]
fn match_result_carries_groups_and_indices_as_node_does() {
    // Expected values are Node 22.21.1's own output, frozen here.
    run_test_actions([
        // Without the `d` flag the result has no `indices`, and its `groups` is undefined
        // when the pattern has no named group.
        TestAction::assert("!('indices' in /a/.exec('a'))"),
        TestAction::assert("/a/.exec('a').groups === undefined"),
        TestAction::assert(
            "Reflect.ownKeys(/a/.exec('a')).join() === '0,length,index,input,groups'",
        ),
        // With `d` the indices array exists, with the whole match at [0] and one entry per group.
        TestAction::assert("'indices' in /a/d.exec('a')"),
        TestAction::assert("/a/d.exec('a').indices[0].join() === '0,1'"),
        // A group that did not participate is undefined in both arrays, not a pair.
        TestAction::assert("/a(b)?c/d.exec('ac')[1] === undefined"),
        TestAction::assert("/a(b)?c/d.exec('ac').indices[1] === undefined"),
        // A named group is reachable by name in `groups` and in `indices.groups`, and the
        // unmatched one stays undefined in both.
        TestAction::assert("/(?<x>a)/d.exec('a').groups.x === 'a'"),
        TestAction::assert("/(?<x>a)/d.exec('a').indices.groups.x.join() === '0,1'"),
        TestAction::assert("/(?<x>a)?b/.exec('b').groups.x === undefined"),
        TestAction::assert(
            "Object.keys(/(?<x>a)(?<y>b)?/d.exec('a').indices.groups).join() === 'x,y'",
        ),
        TestAction::assert("/(?<x>a)(?<y>b)?/d.exec('a').indices.groups.y === undefined"),
        TestAction::assert("/(?<x>a)(?<y>b)?/d.exec('a').indices.length === 3"),
        // Without `d` a named-group match still has `groups` but no indices object.
        TestAction::assert("/(?<x>a)/.exec('a').groups.x === 'a'"),
        TestAction::assert("!('indices' in /(?<x>a)/.exec('a'))"),
    ]);
}

#[test]
fn regexp_string_iterator_may_be_stepped_again_from_exec() {
    // `exec` calls `next` on the iterator that is running it.
    run_test_actions([TestAction::assert_eq(
        indoc! {r#"
            const log = [];
            const orig = RegExp.prototype.exec;
            let it, hit = false;
            RegExp.prototype.exec = function (s) {
              if (!hit) { hit = true; try { log.push(JSON.stringify(it.next())); } catch (e) { log.push(e.constructor.name + ': ' + e.message); } }
              return orig.call(this, s);
            };
            it = 'aaa'.matchAll(/a/g);
            try { log.push(JSON.stringify(it.next())); } catch (e) { log.push('outer ' + e.constructor.name); }
            RegExp.prototype.exec = orig;
            log.join(' | ');
        "#},
        js_str!("{\"value\":[\"a\"],\"done\":false} | {\"value\":[\"a\"],\"done\":false}"),
    )]);
}

#[test]
fn regexp_string_iterator_steps_in_order_when_exec_steps_it() {
    run_test_actions([TestAction::assert_eq(
        indoc! {r#"
            const log = [];
            const orig = RegExp.prototype.exec;
            let it, hit = false;
            RegExp.prototype.exec = function (s) {
              const m = orig.call(this, s);
              if (m && !hit) { hit = true; const inner = it.next(); log.push('inner ' + inner.value.index + ' ' + inner.done); }
              return m;
            };
            it = 'aaa'.matchAll(/a/g);
            const outer = it.next();
            log.push('outer ' + outer.value.index);
            log.push(JSON.stringify(it.next().value.index));
            RegExp.prototype.exec = orig;
            log.join(' | ');
        "#},
        js_str!("inner 1 false | outer 0 | 2"),
    )]);
}

#[test]
fn regexp_string_iterator_completed_by_a_nested_step() {
    run_test_actions([TestAction::assert_eq(
        indoc! {r#"
            const log = [];
            const orig = RegExp.prototype.exec;
            let it, hit = false;
            RegExp.prototype.exec = function (s) {
              if (!hit) { hit = true; log.push(JSON.stringify(it.next().done)); }
              return orig.call(this, s);
            };
            it = 'a'.matchAll(/a/g);
            log.push(JSON.stringify(it.next().done), JSON.stringify(it.next().done));
            RegExp.prototype.exec = orig;
            log.join(' ');
        "#},
        js_str!("false true true"),
    )]);
}

#[test]
fn regexp_string_iterator_is_completed_when_exec_throws() {
    // The specification's closure ends at the throw, so every later `next` is done. Node 22
    // keeps stepping after the throw instead.
    run_test_actions([TestAction::assert_eq(
        indoc! {r#"
            class R extends RegExp { n = 0; exec(s) { if (this.n++ === 1) throw new Error('E'); return super.exec(s); } }
            const it = 'aaa'.matchAll(new R('a', 'g'));
            const log = [];
            for (let i = 0; i < 4; i++) {
              try { log.push(JSON.stringify(it.next())); } catch (e) { log.push('threw'); }
            }
            log.join(' | ');
        "#},
        js_str!("{\"value\":[\"a\"],\"done\":false} | threw | {\"done\":true} | {\"done\":true}"),
    )]);
}

#[test]
fn regexp_string_iterator_is_completed_when_to_string_throws() {
    // The specification's closure ends at the throw, so every later `next` is done. Node 22
    // keeps stepping after the throw instead.
    run_test_actions([TestAction::assert_eq(
        indoc! {r#"
            class R extends RegExp { n = 0; exec(s) { const m = super.exec(s); if (this.n++ === 1) return { 0: { toString() { throw new Error('E'); } }, index: 0 }; return m; } }
            const it = 'aaa'.matchAll(new R('a', 'g'));
            const log = [];
            for (let i = 0; i < 4; i++) {
              try { log.push(JSON.stringify(it.next())); } catch (e) { log.push('threw'); }
            }
            log.join(' | ');
        "#},
        js_str!("{\"value\":[\"a\"],\"done\":false} | threw | {\"done\":true} | {\"done\":true}"),
    )]);
}

#[test]
fn regexp_string_iterator_is_completed_when_last_index_read_throws() {
    // The specification's closure ends at the throw, so every later `next` is done. Node 22
    // keeps stepping after the throw instead.
    run_test_actions([TestAction::assert_eq(
        indoc! {r#"
            class R extends RegExp { n = 0; exec(s) { const m = super.exec(s); if (this.n++ === 0) this.lastIndex = { valueOf() { throw new Error('E'); } }; return m; } }
            const it = 'abc'.matchAll(new R('', 'g'));
            const log = [];
            for (let i = 0; i < 4; i++) {
              try { log.push(JSON.stringify(it.next())); } catch (e) { log.push('threw'); }
            }
            log.join(' | ');
        "#},
        js_str!("threw | {\"done\":true} | {\"done\":true} | {\"done\":true}"),
    )]);
}

#[test]
fn regexp_string_iterator_is_completed_when_last_index_write_throws() {
    // The specification's closure ends at the throw, so every later `next` is done. Node 22
    // keeps stepping after the throw instead.
    run_test_actions([TestAction::assert_eq(
        indoc! {r#"
            class R extends RegExp { n = 0; exec(s) { const m = super.exec(s); if (this.n++ === 0) Object.defineProperty(this, 'lastIndex', { writable: false }); return m; } }
            const it = 'abc'.matchAll(new R('', 'g'));
            const log = [];
            for (let i = 0; i < 4; i++) {
              try { log.push(JSON.stringify(it.next())); } catch (e) { log.push('threw'); }
            }
            log.join(' | ');
        "#},
        js_str!("threw | {\"done\":true} | {\"done\":true} | {\"done\":true}"),
    )]);
}

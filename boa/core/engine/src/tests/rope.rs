//! Strings built by `+=` are ropes until read; they must behave as the flat string does.
use crate::{TestAction, run_test_actions};
use boa_macros::js_str;
use indoc::indoc;

/// Defines `rope()`, a fresh long string built by appends, and `flat`, the same text built by `repeat`.
const SETUP: &str = indoc! {r#"
    function rope() {
        let s = '';
        for (let i = 0; i < 100; i++) s += 'abcdefgh';
        return s;
    }
    const flat = 'abcdefgh'.repeat(100);
"#};

fn check(source: &'static str) {
    run_test_actions([TestAction::run(SETUP), TestAction::assert(source)]);
}

#[test]
fn append_loop_200k() {
    run_test_actions([
        TestAction::run("let s = ''; for (let i = 0; i < 200000; i++) s += 'ab' + (i % 10);"),
        TestAction::assert_eq("s.length", 600_000),
        TestAction::assert_eq("s.slice(0, 6)", js_str!("ab0ab1")),
        TestAction::assert_eq("s.charCodeAt(599999)", 57),
    ]);
}

#[test]
fn length_and_equality() {
    check("rope().length === 800 && rope() === flat && flat === rope() && rope() == flat");
    check("!(rope() === flat + 'x') && rope() !== flat + 'x'");
}

#[test]
fn ordering_and_locale_compare() {
    check("rope() <= flat && rope() >= flat && !(rope() < flat) && !(rope() > flat)");
    check("rope() < rope() + 'a' && (rope() + 'a') > rope()");
    check("rope().localeCompare(flat) === 0 && rope().localeCompare(flat + 'z') < 0");
}

#[test]
fn property_keys_and_map_keys() {
    check("const o = {[flat]: 1}; o[rope()] === 1 && (o[rope()] = 2, o[flat] === 2)");
    check("const o = {}; o[rope()] = 3; Object.keys(o)[0] === flat && flat in o");
    check("const m = new Map([[flat, 'v']]); m.get(rope()) === 'v' && m.has(rope())");
    check("const m = new Map(); m.set(rope(), 1); m.get(flat) === 1");
    check("new Set([rope(), flat, rope()]).size === 1");
}

#[test]
fn indexing_and_iteration() {
    check("rope()[799] === 'h' && rope()[0] === 'a' && rope().at(-1) === 'h'");
    check("[...rope()].length === 800 && Array.from(rope()).join('') === flat");
    check(
        "let n = 0; for (const c of rope()) n += c.charCodeAt(0); n === 100 * 'abcdefgh'.split('').reduce((a, c) => a + c.charCodeAt(0), 0)",
    );
}

#[test]
fn string_prototype_methods() {
    check(indoc! {r#"
        const r = rope();
        [
            r.at(1) === 'b', r.charAt(2) === 'c', r.charCodeAt(3) === 100, r.codePointAt(4) === 101,
            r.concat('!') === flat + '!', r.endsWith('gh'), r.startsWith('abc'), r.includes('hab'),
            r.indexOf('h') === 7, r.lastIndexOf('a') === 792, r.localeCompare(flat) === 0,
            r.match(/cd/).index === 2, [...r.matchAll(/a/g)].length === 100,
            r.normalize() === flat, r.padEnd(802, '-').endsWith('h--'), r.padStart(802, '-').startsWith('--a'),
            r.repeat(2) === flat + flat, r.replace('abc', 'X').startsWith('Xdef'),
            r.replaceAll('abcdefgh', '').length === 0, r.search(/h/) === 7,
            r.slice(-8) === 'abcdefgh', r.split('h').length === 101, r.substring(8, 16) === 'abcdefgh',
            r.toLowerCase() === flat, r.toUpperCase() === flat.toUpperCase(),
            r.toLocaleLowerCase() === flat, r.toLocaleUpperCase() === flat.toUpperCase(),
            r.toString() === flat, r.trim() === flat, r.trimStart() === flat, r.trimEnd() === flat,
            r.valueOf() === flat, r.isWellFormed(), r.toWellFormed() === flat,
            String(r) === flat, `${r}` === flat
        ].every(Boolean)
    "#});
}

#[test]
fn json_and_regexp() {
    check(
        "JSON.stringify(rope()) === JSON.stringify(flat) && JSON.parse(JSON.stringify(rope())) === flat",
    );
    check("JSON.stringify({[rope()]: rope()}) === JSON.stringify({[flat]: flat})");
    check("JSON.parse('[\"' + rope() + '\"]')[0] === flat");
    check("/(def)gh/.exec(rope())[1] === 'def' && /^(?:abcdefgh)+$/.test(rope())");
    check("rope().replace(/b/g, 'B').indexOf('B') === 1 && rope().split(/h/).length === 101");
    check("new RegExp(rope()).test(flat) && /a/g[Symbol.replace](rope(), 'x').length === 800");
}

#[test]
fn source_text() {
    check("eval(rope().slice(0, 0) + '1 + ' + ' '.repeat(300) + '2') === 3");
    check(
        "(function () { let s = 'return 1;'; while (s.length < 300) s += ' '; s += '/*x*/'; return new Function(s)(); })() === 1",
    );
    check("(0, eval)('var x = \"' + rope() + '\"; x') === flat");
}

#[test]
fn append_after_read_and_sharing() {
    check("let s = rope(); const t = s; s += 'x'; t === flat && s === flat + 'x'");
    check("let s = rope(); s.charCodeAt(0); s += 'y'; s.length === 801 && s.endsWith('hy')");
    check("let a = rope(); const b = a + a; a += 'q'; b === flat + flat && a === flat + 'q'");
    check(
        "const parts = []; let s = flat; for (let i = 0; i < 5; i++) { s += i; parts.push(s); } parts[2] === flat + '012'",
    );
}

#[test]
fn mixed_widths_and_surrogates() {
    check(
        "let s = rope(); s += '\\u20ac'; s += 'b'; s.length === 802 && s.charCodeAt(800) === 0x20ac && s.endsWith('\\u20acb')",
    );
    check(
        "let s = rope() + '\\ud83d'; s += '\\ude00'; [...s].length === 801 && s.codePointAt(800) === 0x1f600",
    );
    check(
        "let s = '\\u20ac'.repeat(200); s += rope(); s.length === 1000 && s[0] === '\\u20ac' && s[999] === 'h'",
    );
}

#[test]
fn non_string_operands() {
    check("rope() + 1 === flat + '1' && 1 + rope() === '1' + flat");
    check("rope() + null + undefined + true === flat + 'nullundefinedtrue'");
    check("rope() + {toString() { return 'o'; }} === flat + 'o'");
    check("typeof (rope() + [1, 2]) === 'string' && (rope() + [1, 2]).endsWith('h1,2')");
}

#[test]
fn numbers_join_strings_as_their_decimal_text() {
    check(indoc! {r#"
        const n = [0, 7, -1, 2147483647, -2147483648, 1.5, -0, NaN, 1e21];
        n.map((x) => 'ab' + x).join() === 'ab0,ab7,ab-1,ab2147483647,ab-2147483648,ab1.5,ab0,abNaN,ab1e+21'
            && n.map((x) => x + 'ab').join() === '0ab,7ab,-1ab,2147483647ab,-2147483648ab,1.5ab,0ab,NaNab,1e+21ab'
    "#});
    check(
        "rope() + 12 === flat + '12' && 12 + rope() === '12' + flat && ('' + 5) === '5' && (5 + '') === '5'",
    );
    check(
        "let s = 'x'.repeat(254); s + 12 === 'x'.repeat(254) + '12' && 12 + s === '12' + 'x'.repeat(254)",
    );
    check(
        "let s = 'x'.repeat(250); (s + 12).length === 252 && (s + 123456).length === 256 && (123456 + s).length === 256",
    );
    check(
        "'\\u20ac' + 42 === '\\u20ac42' && 42 + '\\u20ac' === '42\\u20ac' && ('\\u20ac'.repeat(300) + 42).endsWith('\\u20ac42')",
    );
}

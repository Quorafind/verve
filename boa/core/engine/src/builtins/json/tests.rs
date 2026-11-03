use boa_macros::js_str;
use indoc::indoc;

use crate::{JsNativeErrorKind, JsValue, JsVariant, TestAction, js_string, run_test_actions};

#[test]
fn json_sanity() {
    run_test_actions([
        TestAction::assert_eq(r#"JSON.parse('{"aaa":"bbb"}').aaa"#, js_str!("bbb")),
        TestAction::assert_eq(
            r#"JSON.stringify({aaa: 'bbb'})"#,
            js_string!(r#"{"aaa":"bbb"}"#),
        ),
    ]);
}

#[test]
fn json_stringify_remove_undefined_values_from_objects() {
    run_test_actions([TestAction::assert_eq(
        r#"JSON.stringify({ aaa: undefined, bbb: 'ccc' })"#,
        js_string!(r#"{"bbb":"ccc"}"#),
    )]);
}

#[test]
fn json_stringify_remove_function_values_from_objects() {
    run_test_actions([TestAction::assert_eq(
        r#"JSON.stringify({ aaa: () => {}, bbb: 'ccc' })"#,
        js_string!(r#"{"bbb":"ccc"}"#),
    )]);
}

#[test]
fn json_stringify_remove_symbols_from_objects() {
    run_test_actions([TestAction::assert_eq(
        r#"JSON.stringify({ aaa: Symbol(), bbb: 'ccc' })"#,
        js_string!(r#"{"bbb":"ccc"}"#),
    )]);
}

#[test]
fn json_stringify_replacer_array_strings() {
    run_test_actions([TestAction::assert_eq(
        r#"JSON.stringify({aaa: 'bbb', bbb: 'ccc', ccc: 'ddd'}, ['aaa', 'bbb'])"#,
        js_string!(r#"{"aaa":"bbb","bbb":"ccc"}"#),
    )]);
}

#[test]
fn json_stringify_replacer_array_numbers() {
    run_test_actions([TestAction::assert_eq(
        r#"JSON.stringify({ 0: 'aaa', 1: 'bbb', 2: 'ccc'}, [1, 2])"#,
        js_string!(r#"{"1":"bbb","2":"ccc"}"#),
    )]);
}

#[test]
fn json_stringify_replacer_function() {
    run_test_actions([TestAction::assert_eq(
        indoc! {r#"
            JSON.stringify({ aaa: 1, bbb: 2}, (key, value) => {
                if (key === 'aaa') {
                    return undefined;
                }

                return value;
            })
        "#},
        js_string!(r#"{"bbb":2}"#),
    )]);
}

#[test]
fn json_stringify_arrays() {
    run_test_actions([TestAction::assert_eq(
        "JSON.stringify(['a', 'b'])",
        js_string!(r#"["a","b"]"#),
    )]);
}

#[test]
fn json_stringify_object_array() {
    run_test_actions([TestAction::assert_eq(
        "JSON.stringify([{a: 'b'}, {b: 'c'}])",
        js_string!(r#"[{"a":"b"},{"b":"c"}]"#),
    )]);
}

#[test]
fn json_stringify_array_converts_undefined_to_null() {
    run_test_actions([TestAction::assert_eq(
        "JSON.stringify([undefined])",
        js_str!("[null]"),
    )]);
}

#[test]
fn json_stringify_array_converts_function_to_null() {
    run_test_actions([TestAction::assert_eq(
        "JSON.stringify([() => {}])",
        js_str!("[null]"),
    )]);
}

#[test]
fn json_stringify_array_converts_symbol_to_null() {
    run_test_actions([TestAction::assert_eq(
        "JSON.stringify([Symbol()])",
        js_str!("[null]"),
    )]);
}
#[test]
fn json_stringify_function_replacer_propagate_error() {
    run_test_actions([TestAction::assert_opaque_error(
        "JSON.stringify({x: 1}, (key, value) => { throw 1 })",
        1,
    )]);
}

#[test]
fn json_stringify_function() {
    run_test_actions([TestAction::assert_eq(
        "JSON.stringify(() => {})",
        JsValue::undefined(),
    )]);
}

#[test]
fn json_stringify_undefined() {
    run_test_actions([TestAction::assert_eq(
        "JSON.stringify(undefined)",
        JsValue::undefined(),
    )]);
}

#[test]
fn json_stringify_symbol() {
    run_test_actions([TestAction::assert_eq(
        "JSON.stringify(Symbol())",
        JsValue::undefined(),
    )]);
}

#[test]
fn json_stringify_no_args() {
    run_test_actions([TestAction::assert_eq(
        "JSON.stringify()",
        JsValue::undefined(),
    )]);
}

#[test]
fn json_stringify_fractional_numbers() {
    run_test_actions([TestAction::assert_eq("JSON.stringify(1.2)", js_str!("1.2"))]);
}

#[test]
fn json_stringify_pretty_print() {
    run_test_actions([TestAction::assert_eq(
        r#"JSON.stringify({a: "b", b: "c"}, undefined, 4)"#,
        js_string!(indoc! {r#"
            {
                "a": "b",
                "b": "c"
            }"#
        }),
    )]);
}

#[test]
fn json_stringify_pretty_print_four_spaces() {
    run_test_actions([TestAction::assert_eq(
        r#"JSON.stringify({a: "b", b: "c"}, undefined, 4.3)"#,
        js_string!(indoc! {r#"
            {
                "a": "b",
                "b": "c"
            }"#
        }),
    )]);
}

#[test]
fn json_stringify_pretty_print_twenty_spaces() {
    run_test_actions([TestAction::assert_eq(
        r#"JSON.stringify({a: "b", b: "c"}, undefined, 20)"#,
        js_string!(indoc! {r#"
            {
                      "a": "b",
                      "b": "c"
            }"#
        }),
    )]);
}

#[test]
fn json_stringify_pretty_print_with_number_object() {
    run_test_actions([TestAction::assert_eq(
        r#"JSON.stringify({a: "b", b: "c"}, undefined, new Number(10))"#,
        js_string!(indoc! {r#"
            {
                      "a": "b",
                      "b": "c"
            }"#
        }),
    )]);
}

#[test]
fn json_stringify_pretty_print_bad_space_argument() {
    run_test_actions([TestAction::assert_eq(
        r#"JSON.stringify({a: "b", b: "c"}, undefined, [])"#,
        js_string!(r#"{"a":"b","b":"c"}"#),
    )]);
}

#[test]
fn json_stringify_pretty_print_with_too_long_string() {
    run_test_actions([TestAction::assert_eq(
        r#"JSON.stringify({a: "b", b: "c"}, undefined, "abcdefghijklmn")"#,
        js_string!(indoc! {r#"
            {
            abcdefghij"a": "b",
            abcdefghij"b": "c"
            }"#
        }),
    )]);
}

#[test]
fn json_stringify_pretty_print_with_string_object() {
    run_test_actions([TestAction::assert_eq(
        r#"JSON.stringify({a: "b", b: "c"}, undefined, new String("abcd"))"#,
        js_string!(indoc! {r#"
            {
            abcd"a": "b",
            abcd"b": "c"
            }"#
        }),
    )]);
}

#[test]
fn json_parse_array_with_reviver() {
    run_test_actions([
        TestAction::run_harness(),
        TestAction::run(indoc! {r#"
                function reviver(k, v){
                    if (typeof v == 'number') {
                        return v * 2;
                    } else {
                        return v;
                    }
                }
            "#}),
        TestAction::assert("arrayEquals(JSON.parse('[1,2,3,4]', reviver), [2,4,6,8])"),
    ]);
}

#[test]
fn json_parse_object_with_reviver() {
    run_test_actions([
        TestAction::run(indoc! {r#"
                var jsonString = JSON.stringify({
                    firstname: "boa",
                    lastname: "snake"
                });

                function dataReviver(key, value) {
                    if (key == 'lastname') {
                        return 'interpreter';
                    } else {
                        return value;
                    }
                }

                var jsonObj = JSON.parse(jsonString, dataReviver);
            "#}),
        TestAction::assert_eq("jsonObj.firstname", js_str!("boa")),
        TestAction::assert_eq("jsonObj.lastname", js_str!("interpreter")),
    ]);
}

#[test]
fn json_parse_sets_prototypes() {
    run_test_actions([
        TestAction::run(indoc! {r#"
                const jsonString = "{\"ob\":{\"ject\":1},\"arr\": [0,1]}";
                const jsonObj = JSON.parse(jsonString);
            "#}),
        TestAction::assert("Object.getPrototypeOf(jsonObj.ob) === Object.prototype"),
        TestAction::assert("Object.getPrototypeOf(jsonObj.arr) === Array.prototype"),
    ]);
}

#[test]
fn json_fields_should_be_enumerable() {
    run_test_actions([
        TestAction::assert(indoc! {r#"
                var a = JSON.parse('{"x":0}');
                a.propertyIsEnumerable('x')
            "#}),
        TestAction::assert(indoc! {r#"
                var b = JSON.parse('[0, 1]');
                b.propertyIsEnumerable('0');
            "#}),
    ]);
}

#[test]
fn json_parse_with_no_args_throws_syntax_error() {
    // `JSON.parse()` is `JSON.parse(undefined)`, which Node answers with this message.
    run_test_actions([TestAction::assert_native_error(
        "JSON.parse();",
        JsNativeErrorKind::Syntax,
        "\"undefined\" is not valid JSON",
    )]);
}

#[test]
fn json_stringify_cyclic_object_throws_type_error() {
    run_test_actions([TestAction::assert_native_error(
        "var a = {}; a.a = a; JSON.stringify(a);",
        JsNativeErrorKind::Type,
        "cyclic object value",
    )]);
}

#[test]
fn json_stringify_cyclic_array_throws_type_error() {
    run_test_actions([TestAction::assert_native_error(
        "var a = []; a[0] = a; JSON.stringify(a);",
        JsNativeErrorKind::Type,
        "cyclic object value",
    )]);
}

#[test]
fn json_stringify_cyclic_nested_object_throws_type_error() {
    run_test_actions([TestAction::assert_native_error(
        "var a = {}; var b = { a }; a.b = b; JSON.stringify(a);",
        JsNativeErrorKind::Type,
        "cyclic object value",
    )]);
}

#[test]
fn json_stringify_builds_the_same_text_the_spec_path_did() {
    // Expected values are Node 22.21.1's own output, frozen here. Every case is one the
    // serializer has to fall back for, or one whose text the single-allocation assembly must
    // keep exactly.
    run_test_actions([
        // Gaps: the separator, the indent and the closing line feed are assembled separately.
        TestAction::assert(
            r#"JSON.stringify({a:1,b:[1,2]}, null, 2) === '{\n  "a": 1,\n  "b": [\n    1,\n    2\n  ]\n}'"#,
        ),
        TestAction::assert(
            r#"JSON.stringify({a:{b:{c:1}}}, null, 1) === '{\n "a": {\n  "b": {\n   "c": 1\n  }\n }\n}'"#,
        ),
        TestAction::assert(r#"JSON.stringify({a:1}, null, '\t') === '{\n\t"a": 1\n}'"#),
        // A `toJSON` method, an accessor and a proxy all go through the ordinary path.
        TestAction::assert(r#"JSON.stringify({a:{toJSON(){return 'T';}}}) === '{"a":"T"}'"#),
        TestAction::assert(r#"JSON.stringify({get a(){return 1;}}) === '{"a":1}'"#),
        TestAction::assert(
            r#"JSON.stringify(new Proxy({a:1},{ownKeys(){return ['a'];},getOwnPropertyDescriptor(){return {value:1,enumerable:true,configurable:true};},get(t,k){return k==='a'?1:undefined;}})) === '{"a":1}'"#,
        ),
        // A replacer function and a replacer array.
        TestAction::assert(
            r#"JSON.stringify({a:1,b:2}, (k,v) => typeof v === 'number' ? v*2 : v) === '{"a":2,"b":4}'"#,
        ),
        TestAction::assert(r#"JSON.stringify({a:1,b:2,c:3}, ['a','c']) === '{"a":1,"c":3}'"#),
        // A skipped member must not leave its separator behind.
        TestAction::assert(r#"JSON.stringify({a:undefined,b:1}) === '{"b":1}'"#),
        TestAction::assert(r#"JSON.stringify({a:function(){},b:1}) === '{"b":1}'"#),
        TestAction::assert(
            r#"JSON.stringify(Object.defineProperty({a:1},'b',{value:2,enumerable:false})) === '{"a":1}'"#,
        ),
        TestAction::assert(r#"JSON.stringify([1,,3]) === '[1,null,3]'"#),
        TestAction::assert(r#"JSON.stringify({}) === '{}'"#),
        TestAction::assert(r#"JSON.stringify([]) === '[]'"#),
        TestAction::assert(r#"JSON.stringify(Object.create(null)) === '{}'"#),
        // The three branches of QuoteJSONString: a control character, a quote and a backslash,
        // and a character outside Latin-1.
        TestAction::assert(r#"JSON.stringify('a\u0001b') === '"a\\u0001b"'"#),
        TestAction::assert(r#"JSON.stringify('a"b\\c') === '"a\\"b\\\\c"'"#),
        TestAction::assert(r#"JSON.stringify('日') === '"日"'"#),
    ]);
}

#[test]
fn json_parse_reads_a_lone_surrogate_as_node_does() {
    run_test_actions([
        // An escaped lone surrogate is the single code unit it names, not an error: it used to
        // reach the value through `String::from_utf16`, which rejects one.
        TestAction::assert(r#"JSON.parse('"\\ud800"').charCodeAt(0) === 55296"#),
        TestAction::assert(r#"JSON.parse('"\\ud800"').length === 1"#),
        // A lone surrogate that is not escaped is kept as the code unit it is.
        TestAction::assert(r#"JSON.parse('"\ud800"').charCodeAt(0) === 55296"#),
        // A pair stays a pair: two code units, one code point.
        TestAction::assert(r#"JSON.parse('"\\ud83d\\ude00"').length === 2"#),
        TestAction::assert(r#"JSON.parse('"\\ud83d\\ude00"').codePointAt(0) === 128512"#),
        // `\u0000` and `\u0001` are code units like any other.
        TestAction::assert(r#"JSON.parse('"\\u0000"').length === 1"#),
        TestAction::assert(r#"JSON.parse('"a\\u0001b"').length === 3"#),
    ]);
}

#[test]
fn json_parse_reads_nesting_deeper_than_the_script_parser_allows() {
    // The reader keeps its containers on a heap stack, so nesting costs memory rather than Rust
    // stack: the script parser this used to go through gave up at 128 levels.
    run_test_actions([
        TestAction::assert(r#"JSON.parse('['.repeat(10000) + ']'.repeat(10000)).length === 1"#),
        TestAction::assert(
            r#"(function () {
                   let v = JSON.parse('{"a":'.repeat(10000) + '1' + '}'.repeat(10000));
                   for (let i = 0; i < 10000; i++) {
                       v = v.a;
                   }
                   return v === 1;
               })()"#,
        ),
        // A number, a string and a literal close their container on their own.
        TestAction::assert(r#"JSON.parse('[[[1]]]')[0][0][0] === 1"#),
        TestAction::assert(r#"JSON.parse('{"a":{"b":{"c":true}}}').a.b.c === true"#),
        TestAction::assert(r#"JSON.parse('[{"a":[]},[],{}]').length === 3"#),
    ]);
}

#[test]
fn json_parse_reports_errors_the_way_node_does() {
    // Every message here is Node 22.21.1's own, and the position and line it names are too.
    run_test_actions([
        TestAction::assert_native_error(
            "JSON.parse('');",
            JsNativeErrorKind::Syntax,
            "Unexpected end of JSON input",
        ),
        TestAction::assert_native_error(
            "JSON.parse('   ');",
            JsNativeErrorKind::Syntax,
            "Unexpected end of JSON input",
        ),
        TestAction::assert_native_error(
            r#"JSON.parse("'a'");"#,
            JsNativeErrorKind::Syntax,
            "Unexpected token ''', \"'a'\" is not valid JSON",
        ),
        TestAction::assert_native_error(
            "JSON.parse('{\"a\":1,}');",
            JsNativeErrorKind::Syntax,
            "Expected double-quoted property name in JSON at position 7 (line 1 column 8)",
        ),
        TestAction::assert_native_error(
            "JSON.parse('[1,]');",
            JsNativeErrorKind::Syntax,
            "Unexpected token ']', \"[1,]\" is not valid JSON",
        ),
        TestAction::assert_native_error(
            "JSON.parse('[1,2');",
            JsNativeErrorKind::Syntax,
            "Expected ',' or ']' after array element in JSON at position 4 (line 1 column 5)",
        ),
        TestAction::assert_native_error(
            "JSON.parse('{\"a\":1 \"b\":2}');",
            JsNativeErrorKind::Syntax,
            "Expected ',' or '}' after property value in JSON at position 7 (line 1 column 8)",
        ),
        TestAction::assert_native_error(
            "JSON.parse('[1] x');",
            JsNativeErrorKind::Syntax,
            "Unexpected non-whitespace character after JSON at position 4 (line 1 column 5)",
        ),
        TestAction::assert_native_error(
            r#"JSON.parse('"abc');"#,
            JsNativeErrorKind::Syntax,
            "Unterminated string in JSON at position 4 (line 1 column 5)",
        ),
        TestAction::assert_native_error(
            r#"JSON.parse('"a\\qb"');"#,
            JsNativeErrorKind::Syntax,
            "Bad escaped character in JSON at position 3 (line 1 column 4)",
        ),
        TestAction::assert_native_error(
            r#"JSON.parse('"\\u00zz"');"#,
            JsNativeErrorKind::Syntax,
            "Bad Unicode escape in JSON at position 5 (line 1 column 6)",
        ),
        TestAction::assert_native_error(
            r#"JSON.parse('"a\tb"');"#,
            JsNativeErrorKind::Syntax,
            "Bad control character in string literal in JSON at position 2 (line 1 column 3)",
        ),
        TestAction::assert_native_error(
            "JSON.parse('-');",
            JsNativeErrorKind::Syntax,
            "No number after minus sign in JSON at position 1 (line 1 column 2)",
        ),
        TestAction::assert_native_error(
            "JSON.parse('01');",
            JsNativeErrorKind::Syntax,
            "Unexpected number in JSON at position 1 (line 1 column 2)",
        ),
        TestAction::assert_native_error(
            "JSON.parse('1.');",
            JsNativeErrorKind::Syntax,
            "Unterminated fractional number in JSON at position 2 (line 1 column 3)",
        ),
        TestAction::assert_native_error(
            "JSON.parse('1e');",
            JsNativeErrorKind::Syntax,
            "Exponent part is missing a number in JSON at position 2 (line 1 column 3)",
        ),
        // A text that is only one of the names JSON has no value for is quoted by itself.
        TestAction::assert_native_error(
            "JSON.parse('NaN');",
            JsNativeErrorKind::Syntax,
            "\"NaN\" is not valid JSON",
        ),
        TestAction::assert_native_error(
            "JSON.parse('Infinity');",
            JsNativeErrorKind::Syntax,
            "\"Infinity\" is not valid JSON",
        ),
        // A short text is quoted whole, a long one only up to the token.
        TestAction::assert(
            r#"(function () { try { JSON.parse("x".repeat(20)); } catch (e) { return e.message; } })() === 'Unexpected token ' + String.fromCharCode(39) + 'x' + String.fromCharCode(39) + ', "xxxxxxxxxxxxxxxxxxxx" is not valid JSON'"#,
        ),
        TestAction::assert(
            r#"(function () { try { JSON.parse("x".repeat(21)); } catch (e) { return e.message; } })() === 'Unexpected token ' + String.fromCharCode(39) + 'x' + String.fromCharCode(39) + ', "xxxxxxxxxx"... is not valid JSON'"#,
        ),
    ]);
}

#[test]
fn json_parse_answers_the_numbers_and_keys_node_does() {
    run_test_actions([
        // Numbers are read by `str::parse`, so they round the way every other double does.
        TestAction::assert("JSON.parse('-0') === 0"),
        TestAction::assert("Object.is(JSON.parse('-0'), -0)"),
        TestAction::assert("JSON.parse('1e400') === Infinity"),
        TestAction::assert("JSON.parse('12345678901234567890') === 12345678901234567890"),
        TestAction::assert("JSON.parse(' 1.5e-3 ') === 0.0015"),
        // A repeated key keeps the first position and the last value.
        TestAction::assert(
            r#"JSON.stringify(JSON.parse('{"a":1,"b":2,"a":3}')) === '{"a":3,"b":2}'"#,
        ),
        // `__proto__` is an own property of the object, not its prototype.
        TestAction::assert(
            r#"Object.getPrototypeOf(JSON.parse('{"__proto__":{"x":1}}')) === Object.prototype"#,
        ),
        TestAction::assert(
            r#"Object.prototype.hasOwnProperty.call(JSON.parse('{"__proto__":{"x":1}}'), '__proto__')"#,
        ),
        // A text that is one value inside a container nests as deep as it says.
        TestAction::assert(r#"JSON.parse('{"a":[{"b":[1]}]}').a[0].b[0] === 1"#),
    ]);
}

#[test]
fn json_parse_reads_a_whole_number_as_an_integer_value() {
    run_test_actions([
        TestAction::run("var parsed = JSON.parse('[0,123,-7,2147483647,2147483648,1.0,1e2,-0]');"),
        TestAction::inspect_context(|context| {
            let parsed = context
                .global_object()
                .get(js_string!("parsed"), context)
                .unwrap();
            let parsed = parsed.as_object().unwrap();
            let kinds: Vec<_> = (0..8_u32)
                .map(|index| {
                    matches!(
                        parsed.get(index, context).unwrap().variant(),
                        JsVariant::Integer32(_)
                    )
                })
                .collect();
            assert_eq!(kinds, [true, true, true, true, false, false, false, false]);
        }),
    ]);
}

#[test]
fn json_parse_with_a_reviver_throws_the_same_messages() {
    run_test_actions([TestAction::assert(
        r#"(function () {
                for (const text of ['{', '[1,]', '', '{"a" 1}', 'nul']) {
                    let plain, revived;
                    try { JSON.parse(text); } catch (e) { plain = e.message; }
                    try { JSON.parse(text, (k, v) => v); } catch (e) { revived = e.message; }
                    if (plain === undefined || plain !== revived) return false;
                }
                return true;
            })()"#,
    )]);
}

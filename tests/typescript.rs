//! `module.stripTypeScriptTypes`, as Node has it: types erased in place so every position
//! stays, what erasing cannot express refused with Node's codes, the `transform` mode that
//! compiles it instead, and a load hook importing TypeScript through it.

use boa_engine::{JsString, JsValue, builtins::promise::PromiseState};
use serde_json::{Value, json};
use verve::Runtime;

/// A runtime with `stripTypeScriptTypes` at hand.
struct Ts {
    rt: Runtime,
    strip: JsValue,
    run: JsValue,
}

impl Ts {
    fn new() -> Self {
        let mut rt = Runtime::new();
        let exports = rt
            .load_module(
                &std::env::temp_dir().join("verve-typescript-test.js"),
                r#"
                const { stripTypeScriptTypes } = require('module');
                globalThis.stripTypeScriptTypes = stripTypeScriptTypes;
                const settle = (f) => {
                    try { return JSON.stringify({ ok: f() }); }
                    catch (e) {
                        return JSON.stringify({ name: e.name, code: e.code, message: e.message });
                    }
                };
                exports.strip = (code, options) => settle(() => stripTypeScriptTypes(code, options));
                exports.run = (code, options) =>
                    settle(() => (0, eval)(stripTypeScriptTypes(code, options)));
                "#,
            )
            .unwrap();
        let pick = rt.eval("(e, name) => e[name]").unwrap();
        let mut member = |name: &str| {
            rt.call(
                &pick,
                &JsValue::undefined(),
                &[exports.clone(), JsString::from(name).into()],
            )
            .unwrap()
        };
        let (strip, run) = (member("strip"), member("run"));
        Self { rt, strip, run }
    }

    fn call(&mut self, run: bool, code: &str, options: &str) -> Value {
        let f = if run {
            self.run.clone()
        } else {
            self.strip.clone()
        };
        let options = self.rt.eval(&format!("({options})")).unwrap();
        let out = self
            .rt
            .call(
                &f,
                &JsValue::undefined(),
                &[JsString::from(code).into(), options],
            )
            .unwrap();
        serde_json::from_str(&verve::print(&out)).unwrap()
    }

    /// What `code` compiles to with `options`; panics if it was refused.
    fn with(&mut self, code: &str, options: &str) -> String {
        let out = self.call(false, code, options);
        match out["ok"].as_str() {
            Some(js) => js.to_owned(),
            None => panic!("refused: {out}"),
        }
    }

    /// What strip mode makes of `code`.
    fn strip(&mut self, code: &str) -> String {
        self.with(code, "undefined")
    }

    /// What `code` with `options` is refused with: `[name, code]`, and the message.
    fn refused(&mut self, code: &str, options: &str) -> ([String; 2], String) {
        let out = self.call(false, code, options);
        assert!(out["ok"].is_null(), "not refused: {out}");
        let text = |key: &str| out[key].as_str().unwrap_or_default().to_owned();
        ([text("name"), text("code")], text("message"))
    }

    /// The completion value of the script `code` compiles to with `options`, as JSON.
    fn run(&mut self, code: &str, options: &str) -> Value {
        let out = self.call(true, code, options);
        match out.get("ok") {
            Some(value) => value.clone(),
            None => panic!("running threw: {out}"),
        }
    }
}

fn utf16_column(line: &str, token: &str) -> Option<usize> {
    line.find(token).map(|i| line[..i].encode_utf16().count())
}

#[test]
fn strip_mode_keeps_every_line_and_column() {
    let mut ts = Ts::new();
    let source = "const total: Map<string, number> = new Map<string, number>(); // 合計\n\
        function add<T extends object>(this: Window, key: string, n?: number): void {\n\
        \x20 total.set(key, (total.get(key)! + (n as number)) satisfies number);\n\
        }\n\
        let 名前: Ünïcode<'é', '\u{1F600}'> = add; const after = 名前;\n";
    let js = ts.strip(source);
    assert_eq!(js.encode_utf16().count(), source.encode_utf16().count());
    assert_eq!(js.len(), source.len());
    assert_eq!(js.lines().count(), source.lines().count());
    for (kept, original) in js.lines().zip(source.lines()) {
        for token in [
            "total",
            "add",
            "key",
            "after",
            "名前;",
            "= add",
            "// 合計",
            "(n",
            "}",
        ] {
            assert_eq!(
                utf16_column(kept, token),
                utf16_column(original, token),
                "{token} moved in {kept:?}"
            );
        }
    }
    assert_eq!(
        js.lines().take(4).collect::<Vec<_>>(),
        [
            "const total                      = new Map                (); // 合計",
            "function add                  (              key        , n         )       {",
            "  total.set(key, (total.get(key)  + (n          ))                 );",
            "}",
        ]
    );
    // Outside ASCII, the same width in both encodings.
    assert!(js.contains("let 名前  \u{a0} \u{a0}    "), "{js:?}");
    assert!(js.contains("\u{a0}\u{a0}   = add"), "{js:?}");
    assert!(!js.chars().any(|c| "<>:'é😀Üï".contains(c)), "{js}");
}

#[test]
fn declarations_of_types_only_erase_entirely() {
    let mut ts = Ts::new();
    let source = "\
interface Point { x: number; y: number }
type Pair<T> = [T, T];
declare const env: Record<string, string>;
declare function hello(name: string): void;
declare class Outside {}
declare enum Ambient { A }
declare module 'virtual' { export const x: number }
declare global { interface Window { extra: 1 } }
declare namespace Lib { const v: number }
namespace Types { export type A = 1; export interface B {} }
function over(a: string): string;
function over(a: number): number;
export interface Exported {}
export type { Point };
export type * from './types';
export default interface Shape {}
export declare const also: number;
export as namespace UMD;
import type Default from 'a';
import type { Named } from 'b';
import type * as Everything from 'c';
import type Required = require('d');";
    let js = ts.strip(source);
    assert!(js.trim().is_empty(), "{js:?}");
    assert_eq!(js.lines().count(), source.lines().count());
    assert_eq!(js.len(), source.len());
}

#[test]
fn type_only_specifiers_leave_the_rest_of_the_import() {
    let mut ts = Ts::new();
    assert_eq!(
        ts.strip("import a, { type B, c, type D } from 'm';\nexport { type B as E, c as f };\nexport { type G } from 'n';"),
        "import a, {         c,        } from 'm';\nexport {              c as f };\nexport {        } from 'n';"
    );
}

#[test]
fn assertions_generics_and_non_null_erase() {
    let mut ts = Ts::new();
    let source = "\
const box = <T,>(value: T): { value: T } => ({ value });
function first<T>(items: T[]): T | undefined { return items[0]; }
const n = first<number>([1, 2])!;
const s = (n as unknown as number).toFixed(1) satisfies string;
const make = Array<number>;
let late!: number;
late = make(2).length;
const tagged = String.raw<string>`a${n}`;
try { throw 1 } catch (e: unknown) {}
[box<string>('x').value, n, s, late, tagged, new Map<string, number>().size]";
    let js = ts.strip(source);
    assert!(js.contains("const n = first        ([1, 2]) ;"), "{js}");
    assert!(js.contains("let late         ;"), "{js}");
    assert!(
        js.contains("const box =     (value   )               => ({ value });"),
        "{js}"
    );
    assert!(
        js.contains("const s = (n                     ).toFixed(1)                 ;"),
        "{js}"
    );
    assert_eq!(
        ts.run(source, "undefined"),
        json!(["x", 1, "1.0", 2, "a1", 0])
    );
}

#[test]
fn class_members_lose_their_typescript_modifiers() {
    let mut ts = Ts::new();
    let source = "\
interface Named { name: string }
abstract class Base<T> { abstract kind(): string; protected base: T | undefined; }
class Thing extends Base<number> implements Named, Iterable<number> {
  public readonly name: string = 'thing';
  private count?: number;
  declare ghost: string;
  protected static override made: number = 0;
  #secret!: number;
  [key: string]: unknown;
  override kind(): string { return 'thing'; }
  public greet?(this: Thing, to?: string): string { return 'hi ' + (to ?? this.name); }
  constructor();
  constructor(count?: number) { super(); this.count = count; Thing.made++; }
  *[Symbol.iterator](): Iterator<number> { yield this.count!; }
  public get size(): number { return this.count ?? 0; }
}
const t = new Thing(3);
['ghost' in t, t.greet!(), t.kind(), [...t], t.size, Thing.made, Object.keys(t).sort()]";
    let js = ts.strip(source);
    let lines: Vec<&str> = js.lines().collect();
    assert_eq!(
        lines[1..16],
        [
            "         class Base    {                                    base               ; }",
            "class Thing extends Base                                            {",
            "                  name         = 'thing';",
            "          count         ;",
            "                        ",
            "            static          made         = 0;",
            "  #secret         ;",
            "                         ",
            "           kind()         { return 'thing'; }",
            "         greet (             to         )         { return 'hi ' + (to ?? this.name); }",
            "                ",
            "  constructor(count         ) { super(); this.count = count; Thing.made++; }",
            "  *[Symbol.iterator]()                   { yield this.count ; }",
            "         get size()         { return this.count ?? 0; }",
            "}",
        ]
    );
    assert_eq!(
        ts.run(source, "undefined"),
        json!([
            false,
            "hi thing",
            "thing",
            [3],
            3,
            1,
            ["base", "count", "name"]
        ])
    );
}

#[test]
fn enums_are_refused_in_strip_mode_and_compiled_in_transform() {
    let mut ts = Ts::new();
    let source = "enum Color { Red, Green = 'g', Blue = 4 }\nconst enum Fixed { One = 1 }\n[Color.Red, Color[0], Color.Green, Color.Blue, Fixed.One]";
    let (kind, message) = ts.refused(source, "undefined");
    assert_eq!(kind, ["SyntaxError", "ERR_UNSUPPORTED_TYPESCRIPT_SYNTAX"]);
    assert!(
        message.contains("enum") && message.contains("1:1"),
        "{message}"
    );
    assert_eq!(
        ts.run(source, "{ mode: 'transform' }"),
        json!([0, "Red", "g", 4, 1])
    );
}

#[test]
fn namespaces_and_other_compiled_syntax_need_transform() {
    let mut ts = Ts::new();
    let unsupported = [
        "namespace Values { export const x = 1 }",
        "module Old { export type A = 1 }",
        "class P { constructor(private x: number) {} }",
        "class P { constructor(readonly x: number) {} }",
        "import fs = require('fs');",
        "namespace A { export const b = 1 }\nimport B = A.b;",
        "export = {};",
        "const n = <number>value;",
    ];
    for source in unsupported {
        let (kind, _) = ts.refused(source, "undefined");
        assert_eq!(
            kind,
            ["SyntaxError", "ERR_UNSUPPORTED_TYPESCRIPT_SYNTAX"],
            "{source}"
        );
    }
    let source = "\
namespace Shapes { export const sides = 4; export namespace Inner { export const deep = true } }
class Point { constructor(public x: number, protected readonly y = 2) {} sum() { return this.x + this.y } }
const p = <Point>new Point(1);
[Shapes.sides, Shapes.Inner.deep, p.x, p.sum()]";
    assert_eq!(
        ts.run(source, "{ mode: 'transform' }"),
        json!([4, true, 1, 3])
    );
}

#[test]
fn what_is_not_typescript_is_refused_as_invalid() {
    let mut ts = Ts::new();
    for source in [
        "let x: = 1;",
        "function (",
        "function f(a: number = ) {}",
        "a as;",
    ] {
        let (kind, message) = ts.refused(source, "undefined");
        assert_eq!(
            kind,
            ["SyntaxError", "ERR_INVALID_TYPESCRIPT_SYNTAX"],
            "{source}"
        );
        assert!(!message.is_empty());
    }
    let (_, message) = ts.refused(
        "let ok = 1;\nlet x: = 1;",
        "{ sourceUrl: 'file:///bad.ts' }",
    );
    assert!(message.contains("file:///bad.ts:2:"), "{message}");
}

#[test]
fn options_are_checked_as_node_checks_them() {
    let mut ts = Ts::new();
    let code = |ts: &mut Ts, call: &str| -> [String; 2] {
        let source = format!(
            "(() => {{ try {{ stripTypeScriptTypes({call}); return null; }} catch (e) {{ return [e.name, e.code]; }} }})()"
        );
        serde_json::from_value(ts.rt_eval_json(&source)).unwrap()
    };
    let type_error = ["TypeError".to_owned(), "ERR_INVALID_ARG_TYPE".to_owned()];
    let value_error = ["TypeError".to_owned(), "ERR_INVALID_ARG_VALUE".to_owned()];
    assert_eq!(code(&mut ts, "42"), type_error);
    assert_eq!(code(&mut ts, "undefined"), type_error);
    assert_eq!(code(&mut ts, "'x', 'strip'"), type_error);
    assert_eq!(
        code(&mut ts, "'x', { sourceMap: 'yes', mode: 'transform' }"),
        type_error
    );
    assert_eq!(code(&mut ts, "'x', { sourceUrl: 1 }"), type_error);
    assert_eq!(code(&mut ts, "'x', { mode: 'compile' }"), value_error);
    assert_eq!(code(&mut ts, "'x', { sourceMap: true }"), value_error);
}

impl Ts {
    fn rt_eval_json(&mut self, source: &str) -> Value {
        let text = self.rt.eval(&format!("JSON.stringify({source})")).unwrap();
        serde_json::from_str(&verve::print(&text)).unwrap()
    }
}

#[test]
fn source_url_and_source_map_are_appended() {
    let mut ts = Ts::new();
    assert_eq!(
        ts.with("let x: number = 1", "{ sourceUrl: 'file:///a.ts' }"),
        "let x         = 1\n\n//# sourceURL=file:///a.ts"
    );
    assert_eq!(
        ts.with(
            "let x: number = 1",
            "{ mode: 'transform', sourceUrl: 'file:///a.ts' }"
        ),
        "let x = 1;\n\n\n//# sourceURL=file:///a.ts"
    );
    let mapped = ts.with(
        "enum E { A }\nlet x: number = E.A",
        "{ mode: 'transform', sourceMap: true, sourceUrl: 'file:///a.ts' }",
    );
    let (code, url) = mapped
        .split_once("\n\n//# sourceMappingURL=data:application/json;base64,")
        .unwrap();
    assert!(code.contains("let x = E.A;"), "{code}");
    assert!(!mapped.contains("sourceURL="));
    let map = ts.rt_eval_json(&format!(
        "JSON.parse(Buffer.from({url:?}, 'base64').toString())"
    ));
    assert_eq!(map["version"], 3);
    assert_eq!(map["sources"], json!(["file:///a.ts"]));
    assert_eq!(
        map["sourcesContent"],
        json!(["enum E { A }\nlet x: number = E.A"])
    );
    assert!(map["mappings"].as_str().is_some_and(|m| !m.is_empty()));
}

#[test]
fn jsx_in_tsx_is_left_as_written() {
    let mut ts = Ts::new();
    let source = "const view = (props: { name: string }) => <Card title={props.name as string} {...(rest as object)}>\n  <Text<string> bold>hi &amp; {props.name!}</Text>\n</Card>;";
    assert_eq!(
        ts.strip(source),
        "const view = (props                  ) => <Card title={props.name          } {...(rest          )}>\n  <Text         bold>hi &amp; {props.name }</Text>\n</Card>;"
    );
    let transformed = ts.with(source, "{ mode: 'transform' }");
    assert!(
        transformed.contains("<Card title={props.name}"),
        "{transformed}"
    );
}

#[test]
fn erasing_never_joins_two_statements_into_one() {
    let mut ts = Ts::new();
    // Each line break TypeScript ended a statement at still ends one.
    let source = "\
let calls = []
const f = (x) => { calls.push(x); return f }
let a = f
type T = string
(1)
let b = a as any
[2].forEach(f)
interface I {}
`3`
class C {
  x = 1
  declare y: number
  [Symbol.iterator] = 4
  z = 5
  private ['w'] = 6
}
const c = new C()
let d = c!
declare const e: number
;-7
calls.push(c[Symbol.iterator], c.w)
calls";
    let js = ts.strip(source);
    assert!(js.contains("let a = f\n;              \n(1)"), "{js}");
    assert!(js.contains("let b = a;      \n[2]"), "{js}");
    assert!(
        js.contains("  x = 1\n  ;                \n  [Symbol"),
        "{js}"
    );
    assert!(js.contains("  z = 5\n  ;       ['w']"), "{js}");
    assert_eq!(js.len(), source.len());
    // Joined, `f\n(1)` would call `f`, and `[2].forEach(f)` would tag the template `3`.
    assert_eq!(ts.run(source, "undefined"), json!([2, 4, 6]));
}

#[test]
fn an_arrow_return_type_over_lines_keeps_the_arrow_legal() {
    let mut ts = Ts::new();
    let source = "const f = (a: number): {\n  doubled: number\n} => ({ doubled: a * 2 });\nconst g = async (): Promise<\n  number\n> => 1;\n[f(2).doubled, typeof g]";
    let js = ts.strip(source);
    assert_eq!(
        js,
        "const f = (a            \n                 \n) => ({ doubled: a * 2 });\nconst g = async (           \n        \n) => 1;\n[f(2).doubled, typeof g]"
    );
    assert_eq!(ts.run(source, "undefined"), json!([4, "function"]));
}

#[test]
fn a_load_hook_imports_typescript_through_it() {
    let dir = std::env::temp_dir().join(format!("verve-typescript-{}-hook", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("src")).unwrap();
    std::fs::write(
        dir.join("src/shape.ts"),
        "import type { Unit } from './units.ts';\nexport interface Shape { area(): number }\nexport class Square implements Shape {\n  constructor(private side: number) {}\n  area(): number { return this.side ** 2; }\n}\n",
    )
    .unwrap();
    std::fs::write(
        dir.join("src/main.ts"),
        "import { Square, type Shape } from './shape.ts';\nconst shapes: Shape[] = [new Square(3)];\nexport const total: number = shapes.reduce((sum, s) => sum + s.area(), 0);\n",
    )
    .unwrap();
    let main = r#"
        import { registerHooks, stripTypeScriptTypes } from 'node:module';
        import { readFileSync } from 'node:fs';
        import { fileURLToPath } from 'node:url';
        const modes = [];
        registerHooks({
            load(url, context, next) {
                if (!url.endsWith('.ts')) return next(url, context);
                const source = readFileSync(fileURLToPath(url), 'utf8');
                // Parameter properties need the transform; the rest strips in place.
                const mode = source.includes('constructor(private') ? 'transform' : 'strip';
                modes.push(url.split('/').pop() + ' ' + mode);
                const code = stripTypeScriptTypes(source, { mode, sourceUrl: url });
                return { format: 'module', source: code, shortCircuit: true };
            },
        });
        const { total } = await import('./src/main.ts');
        globalThis.result = { total, modes };
    "#;
    let mut runtime = Runtime::new();
    let promise = runtime.load_esm(&dir.join("main.mjs"), main).unwrap();
    for _ in 0..20 {
        runtime.run_microtasks();
    }
    match promise.state() {
        PromiseState::Fulfilled(_) => {}
        other => panic!("{other:?}"),
    }
    let result = runtime.eval("JSON.stringify(globalThis.result)").unwrap();
    assert_eq!(
        verve::print(&result),
        r#"{"total":9,"modes":["main.ts strip","shape.ts transform"]}"#
    );
}

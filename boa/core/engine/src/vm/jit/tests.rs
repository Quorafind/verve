//! The JIT's tests: compiled code must do exactly what the interpreter does.
//!
//! Most of them are differential: a program runs once interpreted and once with every function
//! compiled before it first runs (and once more compiled only when hot, entering in the middle
//! of loops), and the results must be identical — values shown so that `-0`, `NaN`, types and
//! exceptions all count.

use std::sync::atomic::{AtomicUsize, Ordering};

use boa_parser::Source;

use super::{JitOptions, JitStats, compiler::bits};
use crate::{Context, JsValue, NativeFunction, js_string};

/// Shows a value so that what `String()` hides is visible: `-0`, the type of a string or a
/// bigint, the elements of arrays and the properties of objects.
const SHOW: &str = r#"
function show(v) {
  switch (typeof v) {
    case 'number': return Object.is(v, -0) ? '-0' : String(v);
    case 'bigint': return v + 'n';
    case 'string': return JSON.stringify(v);
    case 'symbol': return v.toString();
    case 'function': return 'fn ' + v.name;
    case 'object':
      if (v === null) return 'null';
      if (Array.isArray(v)) return '[' + v.map(show).join(',') + ']';
      return '{' + Object.keys(v).map(k => k + ':' + show(v[k])).join(',') + '}';
    default: return String(v);
  }
}
"#;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Mode {
    Interpreted,
    /// Every function compiled before it first runs.
    Eager,
    /// Compiled once a little hot: entered in the middle of a run.
    Hot,
}

fn context(mode: Mode) -> Context {
    let mut context = Context::default();
    *context.jit_options_mut() = match mode {
        Mode::Interpreted => JitOptions {
            enabled: false,
            ..JitOptions::default()
        },
        Mode::Eager => JitOptions {
            enabled: true,
            threshold: 0,
            ..JitOptions::default()
        },
        Mode::Hot => JitOptions {
            enabled: true,
            threshold: 7,
            ..JitOptions::default()
        },
    };
    context
}

fn run(source: &str, mode: Mode) -> (String, JitStats) {
    let mut context = context(mode);
    let result = match context.eval(Source::from_bytes(&format!("{SHOW}\n{source}"))) {
        Ok(value) => match value.as_string() {
            Some(string) => string.to_std_string_escaped(),
            None => value.display().to_string(),
        },
        Err(error) => format!("uncaught {error}"),
    };
    (result, context.jit_stats())
}

/// Runs `source` in every mode, asserts the results are the same, and returns them.
#[track_caller]
fn same(source: &str) -> String {
    let (interpreted, stats) = run(source, Mode::Interpreted);
    assert_eq!(stats.compiled, 0, "nothing compiles with the JIT off");
    for mode in [Mode::Eager, Mode::Hot] {
        let (compiled, _) = run(source, mode);
        assert_eq!(
            interpreted, compiled,
            "{mode:?} differs from the interpreter for:\n{source}"
        );
    }
    interpreted
}

#[test]
fn tag_bits_match_the_engine() {
    assert_eq!(JsValue::from(5).raw_bits(), (bits::INT32_TAG as u64) | 5);
    assert_eq!(JsValue::from(-1).raw_bits() >> 32, bits::INT32_HIGH as u64);
    assert_eq!(JsValue::from(true).raw_bits(), bits::TRUE as u64);
    assert_eq!(JsValue::from(false).raw_bits(), bits::FALSE as u64);
    assert_eq!(JsValue::null().raw_bits(), bits::OTHER as u64);
    assert_eq!(JsValue::undefined().raw_bits(), bits::UNDEFINED as u64);
    assert_eq!(JsValue::nan().raw_bits(), bits::CANONICAL_NAN as u64);
    let object = JsValue::from(crate::JsObject::with_null_proto());
    assert_eq!(object.raw_bits() >> 48, bits::TOP16_OBJECT as u64);
    assert_eq!(object.raw_bits() >> 50, bits::POINTER_TOP14 as u64);
    let string = JsValue::from(js_string!("s"));
    assert_eq!(string.raw_bits() >> 50, bits::POINTER_TOP14 as u64);
    let symbol = JsValue::from(crate::JsSymbol::new(None).expect("a symbol"));
    assert_eq!(symbol.raw_bits() >> 48, bits::TOP16_SYMBOL as u64);
    let bigint = JsValue::from(crate::JsBigInt::from(7));
    assert_eq!(bigint.raw_bits() >> 50, bits::POINTER_TOP14 as u64);
    for float in [0.5, -0.0, f64::INFINITY, f64::NEG_INFINITY, 1e300, -3.25] {
        let bits = JsValue::from(float).raw_bits();
        assert_eq!(bits, float.to_bits());
        assert_ne!(bits >> 50, bits::POINTER_TOP14 as u64);
    }
}

#[test]
fn hot_functions_are_compiled_and_entered() {
    let mut context = Context::default();
    *context.jit_options_mut() = JitOptions {
        enabled: true,
        threshold: 100,
        ..JitOptions::default()
    };
    let value = context
        .eval(Source::from_bytes(
            "function sum(n) { let s = 0; for (let i = 0; i < n; i++) s += i; return s; }
             let total = 0;
             for (let k = 0; k < 50; k++) total += sum(1000);
             total",
        ))
        .expect("runs");
    assert_eq!(value.as_number(), Some(50.0 * 499_500.0));
    let stats = context.jit_stats();
    assert!(stats.compiled >= 1, "{stats:?}");
    assert!(stats.entries >= 50, "{stats:?}");
    assert!(stats.code_bytes > 0, "{stats:?}");
}

#[test]
fn cold_functions_are_not_compiled() {
    let mut context = Context::default();
    *context.jit_options_mut() = JitOptions {
        enabled: true,
        threshold: 1000,
        ..JitOptions::default()
    };
    context
        .eval(Source::from_bytes("function f(x) { return x + 1 } f(1); f(2);"))
        .expect("runs");
    assert_eq!(context.jit_stats().compiled, 0);
}

#[test]
fn disabled_jit_compiles_nothing() {
    let (_, stats) = run(
        "function f(n) { let s = 0; for (let i = 0; i < n; i++) s += i; return s } f(10000)",
        Mode::Interpreted,
    );
    assert_eq!(stats, JitStats::default());
}

#[test]
fn generators_and_async_functions_stay_interpreted() {
    let out = same(
        "function* g(n) { for (let i = 0; i < n; i++) yield i * 2; }
         async function a(x) { let y = await x; return y + 1; }
         let s = 0; for (const v of g(100)) s += v;
         let p; a(41).then(v => { p = v; });
         [s, typeof p].map(show).join(' ')",
    );
    assert_eq!(out, "9900 \"undefined\"");
    let (_, stats) = run("function* g() { yield 1 } [...g()]; 1", Mode::Eager);
    assert!(stats.rejected >= 1, "{stats:?}");
}

#[test]
fn arithmetic_edge_cases() {
    same(
        "const vals = [0, -0, 1, -1, 2, 3, 7, -7, 0.5, -2.5, 2147483647, -2147483648, 2147483648,
                       4294967295, 1e21, NaN, Infinity, -Infinity, '3', 'x', '', true, false, null,
                       undefined, {}, [], [5]];
         const out = [];
         for (const a of vals) for (const b of vals) {
           out.push(show(a + b), show(a - b), show(a * b), show(a / b), show(a % b));
           out.push(show(a & b), show(a | b), show(a ^ b), show(a << b), show(a >> b), show(a >>> b));
           out.push(show(a < b), show(a <= b), show(a > b), show(a >= b));
           out.push(show(a == b), show(a != b), show(a === b), show(a !== b));
         }
         for (const a of vals) { let x = a, y = a; x++; y--; out.push(show(x), show(y), show(!a), show(-a), show(~a)); }
         out.join(' ')",
    );
}

#[test]
fn bigint_arithmetic_and_mixing() {
    same(
        "const out = [];
         const vals = [0n, 1n, -5n, 2n ** 64n, 3, 'a', null];
         for (const a of vals) for (const b of vals) {
           for (const f of [(x, y) => x + y, (x, y) => x * y, (x, y) => x < y, (x, y) => x === y, (x, y) => x == y]) {
             try { out.push(show(f(a, b))); } catch (e) { out.push(e.name); }
           }
         }
         out.join(' ')",
    );
}

#[test]
fn control_flow() {
    same(
        "function classify(n) {
           switch (n % 5) { case 0: return 'zero'; case 1: case 2: return 'small'; default: break; }
           if (n > 50 && n < 60 || n === 99) return 'band';
           return n ?? 'none';
         }
         const out = [];
         outer: for (let i = 0; i < 100; i++) {
           for (let j = 0; j < 10; j++) {
             if (j === 3) continue;
             if (i * j > 300) break outer;
             if ((i + j) % 7 === 0) continue outer;
           }
           out.push(classify(i));
         }
         let k = 0; do { k += 3; } while (k < 100);
         let w = 0; while (true) { if (++w > 20) break; }
         for (const key in { a: 1, b: 2 }) out.push(key);
         out.push(k, w, null ?? 'n', 0 || 'or', 1 && 'and', undefined?.x);
         out.map(show).join(' ')",
    );
}

#[test]
fn exceptions_cross_compiled_frames() {
    let out = same(
        "function thrower(n) { if (n > 3) throw new RangeError('big ' + n); return n; }
         function middle(n) { let s = 0; for (let i = 0; i <= n; i++) s += thrower(i); return s; }
         function outer() {
           const out = [];
           for (let n = 0; n < 8; n++) {
             try { out.push(middle(n)); }
             catch (e) { out.push(e.name + ':' + e.message); }
             finally { out.push('f' + n); }
           }
           try { null.x; } catch (e) { out.push(e.message); }
           try { undefinedName; } catch (e) { out.push(e.name); }
           return out;
         }
         outer().map(show).join(' ')",
    );
    assert!(out.contains("RangeError:big 4"), "{out}");
}

#[test]
fn error_stacks_are_the_same() {
    same(
        "function a(n) { if (n === 0) throw new Error('deep'); return a(n - 1) + 1; }
         let stack;
         for (let i = 0; i < 3; i++) { try { a(5); } catch (e) { stack = e.stack; } }
         stack",
    );
}

#[test]
fn closures_objects_and_calls() {
    same(
        "function counter() { let c = 0; return { inc() { return ++c; }, get value() { return c; } }; }
         class Point { constructor(x, y) { this.x = x; this.y = y; } len() { return Math.hypot(this.x, this.y); } }
         const k = counter();
         const pts = [];
         for (let i = 0; i < 50; i++) { k.inc(); pts.push(new Point(i, i * 2).len()); }
         const o = { a: 1, b: 'two', c: [1, 2, 3] };
         let s = 0; for (let i = 0; i < 100; i++) { o.a += i; s += o.c[i % 3]; }
         [k.value, pts[49], o.a, s, [3, 1, 2].sort((x, y) => x - y), Object.keys(o)].map(show).join(' ')",
    );
}

#[test]
fn deep_recursion_reports_the_same_error() {
    same(
        "function r(n) { return n === 0 ? 0 : 1 + r(n - 1); }
         let out = [];
         out.push(r(1000));
         try { r(1e6); } catch (e) { out.push(e.name, e.message); }
         out.map(show).join(' ')",
    );
}

#[test]
fn osr_enters_long_loops() {
    let mut context = context(Mode::Hot);
    let value = context
        .eval(Source::from_bytes(
            "let s = 0; for (let i = 0; i < 100000; i++) { s = (s + i * 7) | 0; } s",
        ))
        .expect("runs");
    let expected = (0..100_000i64).fold(0i32, |s, i| s.wrapping_add((i * 7) as i32));
    assert_eq!(value.as_number(), Some(f64::from(expected)));
    let stats = context.jit_stats();
    assert!(stats.compiled >= 1 && stats.entries >= 1, "{stats:?}");
}

#[test]
fn garbage_collection_during_compiled_code() {
    static COLLECTIONS: AtomicUsize = AtomicUsize::new(0);
    for mode in [Mode::Interpreted, Mode::Eager, Mode::Hot] {
        let mut context = context(mode);
        context
            .register_global_callable(
                js_string!("gc"),
                0,
                NativeFunction::from_fn_ptr(|_, _, _| {
                    boa_gc::force_collect();
                    COLLECTIONS.fetch_add(1, Ordering::Relaxed);
                    Ok(JsValue::undefined())
                }),
            )
            .expect("registers");
        let value = context
            .eval(Source::from_bytes(
                "function build(n) {
                   let keep = { list: [] };
                   for (let i = 0; i < n; i++) {
                     let tmp = { i, s: 'x' + i, next: keep.list[i - 1] };
                     keep.list.push(tmp);
                     if (i % 50 === 0) gc();
                     tmp = i;      // the register drops the object for an int
                   }
                   gc();
                   let sum = 0;
                   for (const o of keep.list) sum += o.i + o.s.length + (o.next ? o.next.i : 0);
                   return sum;
                 }
                 let r = 0; for (let k = 0; k < 5; k++) r = build(400); r",
            ))
            .expect("runs");
        let expected: usize = (0..400usize)
            .map(|i| i + format!("x{i}").len() + i.saturating_sub(1))
            .sum();
        assert_eq!(value.as_number(), Some(expected as f64), "{mode:?}");
    }
    assert!(COLLECTIONS.load(Ordering::Relaxed) > 0);
}

#[test]
fn interrupts_stop_compiled_loops() {
    let mut context = context(Mode::Eager);
    let handle = context.interrupt_handle();
    let stopper = std::thread::spawn(move || {
        std::thread::sleep(std::time::Duration::from_millis(100));
        handle.interrupt();
    });
    let result = context.eval(Source::from_bytes(
        "function spin() { let i = 0; while (true) { i = (i + 1) | 0; } } spin()",
    ));
    stopper.join().expect("the thread ends");
    assert!(result.is_err(), "the loop stops");
    assert!(context.jit_stats().compiled >= 1);
    // The context runs again after an interrupt.
    let value = context
        .eval(Source::from_bytes("let t = 0; for (let i = 0; i < 10; i++) t += i; t"))
        .expect("runs");
    assert_eq!(value.as_number(), Some(45.0));
}

#[test]
fn loop_iteration_limit_holds_in_compiled_code() {
    let mut context = context(Mode::Eager);
    context.runtime_limits_mut().set_loop_iteration_limit(1000);
    let result = context.eval(Source::from_bytes(
        "function f() { let i = 0; while (true) { i++; } } f()",
    ));
    assert!(result.is_err());
}

#[test]
fn panics_in_helpers_unwind_to_the_caller() {
    let mut context = context(Mode::Eager);
    context
        .register_global_callable(
            js_string!("boom"),
            0,
            NativeFunction::from_fn_ptr(|_, _, _| panic!("boom from a native")),
        )
        .expect("registers");
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        context.eval(Source::from_bytes(
            "function f() { let s = 0; for (let i = 0; i < 10; i++) { s += i; if (i === 5) boom(); } return s } f()",
        ))
    }));
    let payload = result.expect_err("the panic reaches the caller");
    assert_eq!(payload.downcast_ref::<&str>(), Some(&"boom from a native"));
}

#[test]
fn turning_the_jit_off_takes_effect() {
    let mut context = context(Mode::Eager);
    let source = "function f(n) { let s = 0; for (let i = 0; i < n; i++) s += i; return s } f(100)";
    context.eval(Source::from_bytes(source)).expect("runs");
    let entries = context.jit_stats().entries;
    assert!(entries > 0);
    context.jit_options_mut().enabled = false;
    context.eval(Source::from_bytes("f(100)")).expect("runs");
    assert_eq!(context.jit_stats().entries, entries);
}

/// A deterministic random program generator: expressions over every type, in loops, with
/// exceptions caught, run interpreted and compiled.
struct Generator(u64);

impl Generator {
    fn next(&mut self) -> u64 {
        // xorshift64*
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    fn pick<'a>(&mut self, items: &[&'a str]) -> &'a str {
        items[(self.next() % items.len() as u64) as usize]
    }

    fn expression(&mut self, depth: u32) -> String {
        const LEAVES: &[&str] = &[
            "x", "y", "z", "a", "b", "i", "0", "-0", "1", "-1", "2", "31", "0.5", "-1.5",
            "2147483647", "-2147483648", "4294967296", "NaN", "Infinity", "null", "undefined",
            "true", "false", "'7'", "'q'", "''", "o.p", "arr[i & 3]",
        ];
        const BINARY: &[&str] = &[
            "+", "-", "*", "/", "%", "&", "|", "^", "<<", ">>", ">>>", "<", "<=", ">", ">=",
            "==", "!=", "===", "!==", "&&", "||", "??",
        ];
        const UNARY: &[&str] = &["-", "!", "~", "+", "typeof "];
        if depth == 0 || self.next() % 4 == 0 {
            return self.pick(LEAVES).to_owned();
        }
        match self.next() % 6 {
            0 => format!("({}{})", self.pick(UNARY), self.expression(depth - 1)),
            1 => format!(
                "({} ? {} : {})",
                self.expression(depth - 1),
                self.expression(depth - 1),
                self.expression(depth - 1)
            ),
            _ => format!(
                "({} {} {})",
                self.expression(depth - 1),
                self.pick(BINARY),
                self.expression(depth - 1)
            ),
        }
    }

    fn statement(&mut self) -> String {
        let target = self.pick(&["x", "y", "z"]);
        match self.next() % 7 {
            0 => format!("if ({}) {{ {target} = {}; }} else {{ {target}++; }}", self.expression(2), self.expression(2)),
            1 => format!("{target}--;"),
            2 => format!("o.p = {};", self.expression(2)),
            3 => format!("if ({}) continue;", self.expression(1)),
            _ => format!("{target} = {};", self.expression(3)),
        }
    }

    fn program(&mut self) -> String {
        let body: Vec<String> = (0..6).map(|_| self.statement()).collect();
        format!(
            "function f(a, b) {{
               let x = a, y = b, z = 0; const o = {{ p: 1 }}; const arr = [1, 'two', 3.5, null];
               const out = [];
               for (let i = 0; i < 12; i++) {{
                 try {{ {} }} catch (e) {{ out.push(e.name); }}
                 out.push(show(x), show(y), show(z));
               }}
               return out.join(',');
             }}
             const inputs = [[0, 1], [-0, -1], [3, 0.5], [2147483647, 1], ['5', 2], [null, undefined],
                             [true, NaN], [{{}}, 'x'], [10n, 3], [-2147483648, -1]];
             inputs.map(([a, b]) => {{ try {{ return f(a, b); }} catch (e) {{ return 'E:' + e.name; }} }}).join('\\n')",
            body.join("\n                       ")
        )
    }
}

#[test]
fn random_programs_match_the_interpreter() {
    let count = std::env::var("BOA_JIT_FUZZ_COUNT")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(150);
    let mut generator = Generator(0x9E37_79B9_7F4A_7C15);
    for _ in 0..count {
        let program = generator.program();
        same(&program);
    }
}

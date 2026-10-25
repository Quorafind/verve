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
        .eval(Source::from_bytes(
            "function f(x) { return x + 1 } f(1); f(2);",
        ))
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
        .eval(Source::from_bytes(
            "let t = 0; for (let i = 0; i < 10; i++) t += i; t",
        ))
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
            "x",
            "y",
            "z",
            "a",
            "b",
            "i",
            "0",
            "-0",
            "1",
            "-1",
            "2",
            "31",
            "0.5",
            "-1.5",
            "2147483647",
            "-2147483648",
            "4294967296",
            "NaN",
            "Infinity",
            "null",
            "undefined",
            "true",
            "false",
            "'7'",
            "'q'",
            "''",
            "o.p",
            "arr[i & 3]",
        ];
        const BINARY: &[&str] = &[
            "+", "-", "*", "/", "%", "&", "|", "^", "<<", ">>", ">>>", "<", "<=", ">", ">=", "==",
            "!=", "===", "!==", "&&", "||", "??",
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
            0 => format!(
                "if ({}) {{ {target} = {}; }} else {{ {target}++; }}",
                self.expression(2),
                self.expression(2)
            ),
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

/// A context in `mode`, for tests that already have a `context` in scope.
fn fresh(mode: Mode) -> Context {
    context(mode)
}

/// The code generator panicking leaves the function interpreted and the JIT off for the process
/// (for the thread, in tests); it never unwinds into the script's caller.
#[test]
fn a_panicking_code_generator_switches_the_jit_off() {
    super::latch::reset();
    super::compiler::PANIC_IN_CODEGEN.with(|panic| panic.set(true));
    let mut context = fresh(Mode::Eager);
    let value = context
        .eval(Source::from_bytes(
            "function f(n) { let s = 0; for (let i = 0; i < n; i++) s += i; return s } f(100)",
        ))
        .expect("runs interpreted");
    super::compiler::PANIC_IN_CODEGEN.with(|panic| panic.set(false));
    assert_eq!(value.as_number(), Some(4950.0));
    let stats = context.jit_stats();
    assert_eq!(stats.compiled, 0, "{stats:?}");
    assert!(stats.rejected >= 1, "{stats:?}");
    assert_eq!(
        super::jit_unavailable(),
        Some(super::JitUnavailable::CompilerPanicked)
    );
    // Nothing more is compiled, even with the generator fixed.
    let mut context = fresh(Mode::Eager);
    context
        .eval(Source::from_bytes(
            "function g(n) { let s = 0; for (let i = 0; i < n; i++) s += i; return s } g(100)",
        ))
        .expect("runs interpreted");
    assert_eq!(context.jit_stats().compiled, 0);
    super::latch::reset();
}

/// A host that refuses executable memory switches the JIT off after the first refusal, so each
/// hot function does not pay a whole compile to be refused again.
#[test]
fn refused_executable_memory_switches_the_jit_off() {
    super::latch::reset();
    super::memory::FAIL_TO_MAP.with(|fail| fail.set(true));
    let mut context = fresh(Mode::Eager);
    let value = context
        .eval(Source::from_bytes(
            "function f(n) { let s = 0; for (let i = 0; i < n; i++) s += i; return s }
             function g(n) { let s = 1; for (let i = 0; i < n; i++) s *= 2; return s }
             f(10) + g(10)",
        ))
        .expect("runs interpreted");
    super::memory::FAIL_TO_MAP.with(|fail| fail.set(false));
    assert_eq!(value.as_number(), Some(45.0 + 1024.0));
    assert_eq!(
        super::jit_unavailable(),
        Some(super::JitUnavailable::NoExecutableMemory)
    );
    let stats = context.jit_stats();
    assert_eq!(stats.compiled, 0);
    assert_eq!(stats.live_functions, 0);
    super::latch::reset();
}

fn code_block(
    emit: impl FnOnce(&mut crate::vm::opcode::BytecodeEmitter),
    registers: u32,
) -> crate::vm::CodeBlock {
    let mut emitter = crate::vm::opcode::BytecodeEmitter::new();
    emit(&mut emitter);
    let mut code_block = crate::vm::CodeBlock::new(js_string!("t"), 0, true);
    code_block.bytecode = emitter.into_bytecode();
    code_block.register_count = registers;
    code_block
}

#[test]
fn malformed_bytecode_is_refused() {
    use crate::vm::opcode::{Address, RegisterOperand};
    let options = JitOptions {
        threshold: 0,
        ..JitOptions::default()
    };
    let live = std::rc::Rc::default();
    let compiles = |code_block: &crate::vm::CodeBlock| {
        super::compiler::compile(code_block, &options, &live).is_ok()
    };
    // Well-formed: stores, a jump to an instruction, a return.
    assert!(compiles(&code_block(
        |e| {
            e.emit_store_zero(RegisterOperand::new(0));
            e.emit_jump(Address::new(10));
            e.emit_return();
        },
        1,
    )));
    // A jump into the middle of an instruction.
    assert!(!compiles(&code_block(
        |e| {
            e.emit_store_zero(RegisterOperand::new(0));
            e.emit_jump(Address::new(2));
            e.emit_return();
        },
        1,
    )));
    // A jump past the end.
    assert!(!compiles(&code_block(
        |e| {
            e.emit_jump(Address::new(1000));
            e.emit_return();
        },
        1,
    )));
    // A register beyond the frame's.
    assert!(!compiles(&code_block(
        |e| {
            e.emit_store_zero(RegisterOperand::new(1));
            e.emit_return();
        },
        1,
    )));
    assert!(!compiles(&code_block(
        |e| {
            e.emit_move(RegisterOperand::new(0), RegisterOperand::new(u32::MAX));
            e.emit_return();
        },
        1,
    )));
    // Inline code at the end, with nowhere to go on to.
    assert!(!compiles(&code_block(
        |e| e.emit_store_zero(RegisterOperand::new(0)),
        1,
    )));
}

#[test]
fn compile_limits_hold_and_code_is_given_back() {
    let source = "function f(n) { let s = 0; for (let i = 0; i < n; i++) s += i; return s }
                  function g(n) { let s = 0; for (let i = 0; i < n; i++) s -= i; return s }
                  f(10) + g(10)";
    let mut context = fresh(Mode::Eager);
    context.jit_options_mut().max_compiled_functions = 1;
    context.eval(Source::from_bytes(source)).expect("runs");
    assert_eq!(context.jit_stats().compiled, 1);

    let mut context = fresh(Mode::Eager);
    context.jit_options_mut().max_live_code_bytes = 1;
    context.eval(Source::from_bytes(source)).expect("runs");
    let stats = context.jit_stats();
    assert_eq!(stats.compiled, 1, "{stats:?}");
    assert!(stats.live_code_bytes >= 4096 && stats.code_bytes == stats.live_code_bytes);

    // Code is counted in pages, and given back when its function is collected.
    let mut context = fresh(Mode::Eager);
    context
        .eval(Source::from_bytes(
            "(function () { const f = function (n) { let s = 0; for (let i = 0; i < n; i++) s += i; return s }; return f(10) })()",
        ))
        .expect("runs");
    let before = context.jit_stats();
    assert!(before.live_functions >= 1, "{before:?}");
    assert_eq!(before.code_bytes % 4096, 0);
    boa_gc::force_collect();
    let after = context.jit_stats();
    assert!(
        after.live_functions < before.live_functions,
        "{before:?} {after:?}"
    );
    assert!(after.live_code_bytes < before.live_code_bytes);
    assert_eq!(after.code_bytes, before.code_bytes);
}

/// After the slow path of an inline instruction (here `+` on strings), compiled code goes on
/// rather than returning to the interpreter at every turn of the loop.
#[test]
fn slow_paths_stay_in_compiled_code() {
    let mut context = fresh(Mode::Eager);
    let value = context
        .eval(Source::from_bytes(
            "function f(n) { let s = ''; for (let i = 0; i < n; i++) s = s + 'x'; return s.length } f(1000)",
        ))
        .expect("runs");
    assert_eq!(value.as_number(), Some(1000.0));
    let stats = context.jit_stats();
    assert!(stats.entries < 50, "{stats:?}");
}

// ---- Phase 2: property caches, element access, nested calls ----

/// Property reads and writes through the inline caches, with everything that must miss them:
/// new shapes, prototypes, accessors, deletes, frozen and non-writable properties, and objects
/// in dictionary mode.
#[test]
fn property_caches_match_the_interpreter() {
    same(
        "function get(o) { return o.x; }
         function set(o, v) { o.x = v; return o.x; }
         const out = [];
         // Monomorphic, then a new shape at the same site, then many (megamorphic).
         const a = { x: 1 }, b = { y: 0, x: 2 }, shapes = [];
         for (let k = 0; k < 8; k++) { const o = {}; o['p' + k] = k; o.x = k * 10; shapes.push(o); }
         for (let i = 0; i < 60; i++) {
           out.push(get(a), get(b), get(shapes[i % 8]));
           out.push(set(a, i), set(shapes[i % 8], -i));
         }
         // A property added in the loop changes the shape under the cache.
         const grow = { x: 0 };
         for (let i = 0; i < 40; i++) { if (i === 20) grow.z = 1; out.push(get(grow)); set(grow, i); }
         // Deleted mid-loop: the site must see the miss (and undefined).
         const del = { x: 5, w: 6 };
         for (let i = 0; i < 40; i++) { if (i === 25) delete del.x; out.push(show(get(del))); }
         // Prototype data and methods, then shadowed by an own property.
         const proto = { x: 'proto', m() { return 'm' + this.x; } };
         const child = Object.create(proto);
         for (let i = 0; i < 40; i++) {
           if (i === 30) child.x = 'own';
           out.push(get(child), child.m());
           if (i === 35) proto.x = 'changed';
         }
         // Accessors, own and inherited; a setter that counts.
         let sets = 0;
         const acc = { get x() { return 'getter'; }, set x(v) { sets += v; } };
         const inh = Object.create(acc);
         for (let i = 0; i < 30; i++) { out.push(get(acc), get(inh)); set(acc, 1); set(inh, 2); }
         out.push(sets);
         // Frozen and non-writable: sloppy writes are ignored, strict ones throw.
         const frozen = Object.freeze({ x: 'f' });
         const fixed = { x: 'n' }; Object.defineProperty(fixed, 'x', { writable: false });
         const later = { x: 0 };
         for (let i = 0; i < 30; i++) {
           out.push(set(frozen, i), set(fixed, i));
           if (i === 15) Object.freeze(later);
           out.push(set(later, i));
         }
         function strictSet(o, v) { 'use strict'; o.x = v; return o.x; }
         for (let i = 0; i < 20; i++) {
           try { out.push(strictSet(i < 10 ? a : frozen, i)); } catch (e) { out.push(e.name); }
         }
         // Dictionary mode (many deletes), primitives, null and undefined.
         const dict = {}; for (let k = 0; k < 40; k++) dict['k' + k] = k;
         for (let k = 0; k < 30; k++) delete dict['k' + k];
         dict.x = 'dict';
         for (let i = 0; i < 30; i++) {
           out.push(get(dict), set(dict, 'd' + i), get('str'), get(5), get(true));
           try { get(i % 2 ? null : undefined); } catch (e) { out.push(e.name); }
           try { set(null, 1); } catch (e) { out.push(e.name); }
         }
         // Proxies and lengths.
         const proxy = new Proxy({ x: 'target' }, { get(t, k) { return 'trap ' + String(k); } });
         const lens = [[1, 2, 3], 'four', { length: 'own' }, function f(p, q) {}, new Uint8Array(5)];
         for (let i = 0; i < 30; i++) {
           out.push(get(proxy));
           for (const v of lens) out.push(v.length);
         }
         out.map(show).join(' ')",
    );
}

/// Elements of arrays by `int32` index: dense arrays of each kind, holes, out of range,
/// negative and non-integer indexes, non-extensible arrays, setters on the prototype, typed
/// arrays and strings.
#[test]
fn element_access_matches_the_interpreter() {
    same(
        "const out = [];
         function read(a, i) { return a[i]; }
         function write(a, i, v) { a[i] = v; return a[i]; }
         const ints = [1, 2, 3, 4], floats = [0.5, 1.5, 2.5], mixed = [1, 'two', null, {}];
         const holey = [1, , 3]; holey[10] = 'far';
         const sealed = Object.preventExtensions([7, 8, 9]);
         const typed = new Int16Array([5, 6, 7]);
         for (let i = -2; i < 14; i++) {
           for (const a of [ints, floats, mixed, holey, sealed, typed, 'text']) out.push(show(read(a, i)));
           out.push(show(read(ints, i + 0.5)), show(read(ints, String(i))));
         }
         for (let i = 0; i < 12; i++) {
           out.push(show(write(ints, i % 6, i * 3)), show(write(floats, i % 3, i % 2 ? 'str' : i / 4)));
           out.push(show(write(sealed, i % 5, i)), show(write(typed, i % 4, i * 1000)));
           out.push(show(write(mixed, -1, i)));
         }
         Object.defineProperty(Array.prototype, '20', { set(v) { out.push('setter ' + v); }, configurable: true });
         const short = [];
         for (let i = 0; i < 25; i++) write(short, i, i);
         delete Array.prototype[20];
         out.push(show(short), show(ints), show(floats), show(mixed), mixed[-1], show(sealed), show([...typed]));
         let s = 0; const big = new Array(1000).fill(1);
         for (let i = 0; i < big.length; i++) { big[i] = big[i] + i; s += big[i]; }
         out.push(s);
         out.join(' ')",
    );
}

/// Compiled functions call compiled functions nested on the native stack: deep recursion past
/// the nesting cap, exceptions thrown through compiled callees and caught at every depth,
/// `finally`, the recursion limit, and calls through interpreted frames (generators, natives
/// calling back) in between.
#[test]
fn nested_compiled_calls_match_the_interpreter() {
    same(
        "function fib(n) { return n < 2 ? n : fib(n - 1) + fib(n - 2); }
         function depth(n) { return n === 0 ? 0 : 1 + depth(n - 1); }
         function thrower(n) { if (n % 7 === 3) throw new Error('at ' + n); return n; }
         function middle(n) { try { return thrower(n) * 2; } finally { log.push('f' + n); } }
         function outer(n) { let s = 0; for (let i = 0; i < n; i++) { try { s += middle(i); } catch (e) { s -= 1; log.push(e.message); } } return s; }
         function* gen(n) { for (let i = 0; i < n; i++) yield depth(i); }
         function viaNative(n) { return [1, 2, 3].map(x => depth(x + n)).reduce((a, b) => a + b); }
         function rec(n) { if (n === 0) throw new RangeError('bottom'); return rec(n - 1); }
         const log = [];
         const out = [fib(18), depth(200), depth(3000), outer(40), [...gen(30)].join(','), viaNative(50)];
         for (let d of [10, 100, 1000]) { try { rec(d); } catch (e) { out.push(e.name + ':' + e.message); } }
         try { depth(1e6); } catch (e) { out.push(e.name, e.message); }
         out.push(depth(100));
         out.concat(log).map(show).join(' ')",
    );
}

/// Errors thrown deep in nested compiled calls carry the same stack as interpreted.
#[test]
fn nested_calls_keep_error_stacks() {
    same(
        "function a(n) { if (n === 0) throw new Error('deep'); return b(n - 1) + 1; }
         function b(n) { return a(n) + 0; }
         let stack;
         for (let i = 0; i < 30; i++) { try { a(8); } catch (e) { stack = e.stack; } }
         stack",
    );
}

/// A loop calling a compiled function stays in compiled code: the callee is entered nested,
/// and the caller is not entered again after each call.
#[test]
fn calls_between_compiled_functions_do_not_return_to_the_interpreter() {
    let mut context = fresh(Mode::Eager);
    let value = context
        .eval(Source::from_bytes(
            "function add(a, b) { return a + b; }
             function loop(n) { let s = 0; for (let i = 0; i < n; i++) s = add(s, i) | 0; return s; }
             loop(1000)",
        ))
        .expect("runs");
    assert_eq!(value.as_number(), Some(499_500.0));
    let stats = context.jit_stats();
    // About one entry per call of `add`; returning to the interpreter would be two.
    assert!(stats.entries < 1100, "{stats:?}");
}

/// The garbage collector runs inside nested compiled calls, with objects in the registers and
/// on the stack of every frame.
#[test]
fn garbage_collection_during_nested_calls() {
    static COLLECTIONS: AtomicUsize = AtomicUsize::new(0);
    let mut results = Vec::new();
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
                "function leaf(o, i) { if (i % 16 === 0) gc(); return { v: o.v + i, s: o.s + i }; }
                 function mid(o, n) { let r = o; for (let i = 0; i < n; i++) r = leaf(r, i); return r; }
                 function top(n) { let acc = { v: 0, s: '' }; for (let k = 0; k < n; k++) acc = mid(acc, 20); return acc; }
                 const r = top(40); r.v + ':' + r.s.length",
            ))
            .expect("runs");
        results.push(value.display().to_string());
    }
    assert!(results.windows(2).all(|w| w[0] == w[1]), "{results:?}");
    assert!(COLLECTIONS.load(Ordering::Relaxed) > 0);
}

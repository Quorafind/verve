//! The JIT (`--features jit`): hot code runs compiled, gives what the interpreter gives, and can
//! be switched off — per runtime, and for `verve run` by `VERVE_JIT=0`.
#![cfg(feature = "jit")]

use std::process::Command;

use verve::Runtime;

const VERVE: &str = env!("CARGO_BIN_EXE_verve");

/// A script that does some of everything: numbers, strings, objects, closures, exceptions,
/// generators, promises and timers.
const SCRIPT: &str = r#"
function mix(n) {
  let s = 0, f = 0.5, text = '';
  for (let i = 0; i < n; i++) {
    s = (s + i * 31) | 0;
    f = f * 1.0001 + (i % 7) / 3;
    if (i % 1000 === 0) text += String.fromCharCode(97 + (i / 1000) % 26);
  }
  return [s, f.toFixed(6), text];
}
function* gen(n) { for (let i = 0; i < n; i++) yield i * i; }
class Acc { constructor() { this.total = 0; } add(x) { this.total += x; return this; } }
const acc = new Acc();
for (const v of gen(2000)) acc.add(v);
let caught = [];
for (let i = 0; i < 300; i++) {
  try { if (i % 100 === 99) throw new TypeError('t' + i); } catch (e) { caught.push(e.message); }
}
const result = { mix: mix(200000), total: acc.total, caught };
JSON.stringify(result)
"#;

fn eval(runtime: &mut Runtime) -> String {
    let value = runtime.eval(SCRIPT).expect("the script runs");
    verve::print(&value)
}

#[test]
fn compiled_and_interpreted_runs_agree() {
    let mut interpreted = Runtime::new();
    interpreted.set_jit_enabled(false);
    assert!(!interpreted.jit_enabled());
    // Making a runtime runs some JavaScript, which `VERVE_JIT_THRESHOLD=0` compiles.
    let before = interpreted.jit_stats();
    let expected = eval(&mut interpreted);
    let after = interpreted.jit_stats();
    assert_eq!((after.compiled, after.entries), (before.compiled, before.entries));

    for threshold in [0, 50, 1000] {
        let mut compiled = Runtime::new();
        compiled.set_jit_enabled(true);
        compiled.jit_options_mut().threshold = threshold;
        assert_eq!(eval(&mut compiled), expected, "threshold {threshold}");
        let stats = compiled.jit_stats();
        assert!(stats.compiled >= 1 && stats.entries >= 1, "threshold {threshold}: {stats:?}");
    }
}

#[test]
fn switching_off_stops_compiled_code() {
    let mut runtime = Runtime::new();
    runtime.set_jit_enabled(true);
    runtime.jit_options_mut().threshold = 0;
    runtime
        .eval("function f(n) { let s = 0; for (let i = 0; i < n; i++) s += i; return s } f(10)")
        .expect("runs");
    let entries = runtime.jit_stats().entries;
    assert!(entries > 0);
    runtime.set_jit_enabled(false);
    let value = runtime.eval("f(100)").expect("runs");
    assert_eq!(verve::print(&value), "4950");
    assert_eq!(runtime.jit_stats().entries, entries);
}

#[test]
fn verve_run_obeys_the_environment() {
    let dir = std::env::temp_dir().join(format!("verve-jit-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("a temporary directory");
    let script = dir.join("jit.js");
    let source = format!("{SCRIPT};\nconsole.log(JSON.stringify(result))");
    std::fs::write(&script, source).expect("the script is written");
    let run = |vars: &[(&str, &str)]| {
        let mut command = Command::new(VERVE);
        command.arg("run").arg(&script);
        command.env_remove("VERVE_JIT").env_remove("VERVE_JIT_THRESHOLD");
        command.env_remove("BOA_JIT").env_remove("BOA_JIT_THRESHOLD");
        for (name, value) in vars {
            command.env(name, value);
        }
        let output = command.output().expect("verve runs");
        assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
        String::from_utf8(output.stdout).expect("UTF-8")
    };
    let off = run(&[("VERVE_JIT", "0")]);
    assert_eq!(run(&[]), off);
    assert_eq!(run(&[("VERVE_JIT_THRESHOLD", "0")]), off);
    let _ = std::fs::remove_dir_all(&dir);
}

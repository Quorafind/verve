//! WebAssembly as wasm-bindgen's glue and hand-written loaders use it: a module compiled from
//! bytes, its functions called both ways, its memory read and written from JavaScript and
//! grown, an error thrown through it, a trap, and its globals and tables.

use verve::Runtime;

fn js(rt: &mut Runtime, source: &str) -> String {
    let value = rt.eval(source).unwrap_or_else(|e| panic!("{source}: {e}"));
    verve::print(&value)
}

fn settle(rt: &mut Runtime) {
    for _ in 0..50 {
        rt.run_microtasks();
        if rt.run_due() == 0 && rt.next_due().is_none() {
            break;
        }
    }
}

/// The module, as bytes in a global `bytes`.
fn load(rt: &mut Runtime, wat: &str) {
    let bytes = wat::parse_str(wat).unwrap();
    let list: Vec<String> = bytes.iter().map(|b| b.to_string()).collect();
    js(
        rt,
        &format!("globalThis.bytes = new Uint8Array([{}])", list.join(",")),
    );
}

const MODULE: &str = r#"
(module
  (import "env" "log" (func $log (param i32 i32)))
  (import "env" "twice" (func $twice (param i32) (result i32)))
  (memory (export "memory") 1 4)
  (global $count (export "count") (mut i32) (i32.const 7))
  (table (export "table") 2 funcref)
  (elem (i32.const 0) $add $big)
  (data (i32.const 16) "hello from wasm")
  (func $add (export "add") (param i32 i32) (result i32) local.get 0 local.get 1 i32.add)
  (func $big (export "big") (param i64) (result i64) local.get 0 i64.const 2 i64.mul)
  (func (export "half") (param f64) (result f64) local.get 0 f64.const 2 f64.div)
  (func (export "greet") (call $log (i32.const 16) (i32.const 15)))
  ;; The sum of `len` bytes at `ptr` — of what JavaScript wrote there.
  (func (export "sum") (param $ptr i32) (param $len i32) (result i32) (local $acc i32)
    (block $done (loop $next
      (br_if $done (i32.eqz (local.get $len)))
      (local.set $acc (i32.add (local.get $acc) (i32.load8_u (local.get $ptr))))
      (local.set $ptr (i32.add (local.get $ptr) (i32.const 1)))
      (local.set $len (i32.sub (local.get $len) (i32.const 1)))
      (br $next)))
    local.get $acc)
  (func (export "callOut") (param i32) (result i32) local.get 0 call $twice i32.const 1 i32.add)
  (func (export "grow") (result i32) i32.const 1 memory.grow)
  (func (export "boom") unreachable)
  (func (export "bump") global.get $count i32.const 1 i32.add global.set $count))
"#;

#[test]
fn a_module_called_both_ways() {
    let mut rt = Runtime::with_clock(|| 0.);
    load(&mut rt, MODULE);
    js(
        &mut rt,
        r#"
        globalThis.out = [];
        const decoder = new TextDecoder();
        let wasm;
        const imports = { env: {
            log: (ptr, len) => out.push(decoder.decode(new Uint8Array(wasm.memory.buffer, ptr, len))),
            twice: (n) => { if (n < 0) throw new Error('negative'); return n * 2; },
        } };
        WebAssembly.instantiate(bytes, imports).then(({ module, instance }) => {
            wasm = instance.exports;
            out.push(module instanceof WebAssembly.Module, instance instanceof WebAssembly.Instance);
            out.push(WebAssembly.Module.exports(module).length, WebAssembly.Module.imports(module).map(i => i.name).join());
            out.push(wasm.add(2, 40), String(wasm.big(21n)), wasm.half(5));
            wasm.greet();
            // Written from JavaScript, read by the module.
            const view = new Uint8Array(wasm.memory.buffer);
            view.set([1, 2, 3, 4], 100);
            out.push(wasm.sum(100, 4));
            out.push(wasm.callOut(20));
            try { wasm.callOut(-1); } catch (e) { out.push('thrown ' + e.message); }
            try { wasm.boom(); } catch (e) { out.push(e instanceof WebAssembly.RuntimeError); }
            // Grown: the old buffer detached, a bigger one in its place.
            const before = wasm.memory.buffer;
            out.push(wasm.grow(), before.byteLength, wasm.memory.buffer.byteLength / 65536);
            out.push(wasm.memory.grow(1), wasm.memory.buffer.byteLength / 65536);
            out.push(wasm.count.value); wasm.bump(); out.push(wasm.count.value);
            out.push(wasm.table.length, wasm.table.get(0)(1, 2));
            out.push(WebAssembly.validate(bytes), WebAssembly.validate(new Uint8Array([0, 1, 2])));
        }).catch(e => out.push('failed ' + e + ' ' + e.stack));
        WebAssembly.compile(new Uint8Array([0, 97, 115, 109, 9, 9, 9, 9])).catch(e => out.push(e instanceof WebAssembly.CompileError));
    "#,
    );
    settle(&mut rt);
    assert_eq!(
        js(&mut rt, "JSON.stringify(out)"),
        r#"[true,true,12,"log,twice",42,"42",2.5,"hello from wasm",10,41,"thrown negative",true,1,0,2,2,3,7,8,2,3,true,false,true]"#
    );
}

#[test]
fn a_memory_made_by_javascript_is_imported() {
    let mut rt = Runtime::with_clock(|| 0.);
    load(
        &mut rt,
        r#"
        (module
          (import "js" "mem" (memory 1))
          (func (export "peek") (param i32) (result i32) local.get 0 i32.load8_u)
          (func (export "poke") (param i32 i32) local.get 0 local.get 1 i32.store8))
    "#,
    );
    let out = js(
        &mut rt,
        r#"
        const mem = new WebAssembly.Memory({ initial: 1, maximum: 2 });
        const early = new Uint8Array(mem.buffer);
        early[5] = 99;
        const instance = new WebAssembly.Instance(new WebAssembly.Module(bytes), { js: { mem } });
        instance.exports.poke(6, 42);
        [instance.exports.peek(5), new Uint8Array(mem.buffer)[6], early[6], Object.isFrozen(instance.exports)].join()
    "#,
    );
    assert_eq!(out, "99,42,42,true");
}

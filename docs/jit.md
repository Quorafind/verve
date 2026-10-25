# A JIT for Verve

Verve runs JavaScript on Boa, a bytecode interpreter. This is the plan for compiling hot code to
native code, and the record of its first phase, which is built: a baseline JIT on Cranelift in
`boa/core/engine/src/vm/jit/`, behind the `jit` feature of `boa_engine` and of `verve`.

## What exists, and what was chosen

- **Boa upstream** has no JIT, no JIT branch and no open design for one; the maintainers' stated
  focus is the interpreter. Nothing to reuse.
- **Other JavaScript engines in Rust** — Nova, Brimstone, Starlight (abandoned), and Kiesel (in
  Zig) — are interpreters. None has a JIT that could be lifted out.
- **Code generators**: Cranelift (`cranelift-codegen` + `cranelift-frontend`, the backend of
  Wasmtime and of `rustc_codegen_cranelift`) produces decent code fast, for x86-64, AArch64,
  RISC-V and s390x, with a register allocator, SSA construction and a verifier.
  dynasm-rs is an assembler: every operation written by hand, per architecture, with no register
  allocation. LLVM (inkwell) compiles too slowly for a first tier and is a large C++ dependency.

**Cranelift**, as a **baseline (method) JIT** that compiles hot bytecode functions and calls back
into the engine for everything complicated. Version 0.135 (pinned `~0.135.7`) is the newest whose
MSRV is the 1.95 the tree builds with; 0.136 asks for 1.96.

The shape of the JIT comes from what Boa's VM is:

- **Register-based bytecode** (`vm/opcode/mod.rs`, ~200 opcodes), with fused compare-and-jumps and
  an `IncrementLoopIteration` in every loop.
- **NaN-boxed values** (`value/inner/nan_boxed.rs`): a `JsValue` is one 64-bit word. `int32`s,
  `f64`s, booleans, `null` and `undefined` are plain bits; objects, strings, symbols and bigints are
  tagged pointers that own a reference count.
- **Registers live in the VM stack** (`Vm::stack`, a `Vec<JsValue>`), from each frame's `rp`.
- **Calls do not recurse natively**: `Call` pushes a `CallFrame` and the interpreter loop carries
  on in the callee; `Return` pops it.
- **A reference-counting, non-moving collector** (`boa_gc`): a value is a root while anything
  outside the GC heap holds a reference, and the VM stack is outside it.

So compiled code works **on the interpreter's own frame**, in the manner of V8's Sparkplug: the
same `CallFrame`, the same registers in the VM stack, the same `pc` whenever anything can observe
it. That one decision answers most of the hard questions below.

## Where the time goes

Measured on the release `verve` (Windows, x86-64):

| What | Cost |
|---|---|
| `verve run -e 0` | ~105 ms (process start and globals) |
| Parse + bytecompile, Rebon's plugin host and its vendored libraries (460 KB of JS) | 118 ms — **3.7 MB/s** |
| A plugin bundle of 1 MB | ~0.3 s before any of it runs |

Startup is parse and bytecompile time, and Boa compiles every function body eagerly, including
the many a bundle never calls. **A JIT does nothing for that.** Two separate items would:

1. **Lazy function compilation**: compile a function's body on its first call. Most of a bundle
   never runs.
2. **A bytecode cache**: serialise `CodeBlock`s keyed by source hash and Boa version, and load them
   on the next start.

Both are on the roadmap below as their own track. They are likely the bigger win for Rebon's
startup. The JIT is for what runs after: Code Mode programs, plugin work over large notes and
data, parsers, diff and search loops.

## How it works

### Compilation

`compiler.rs` turns one `CodeBlock` into one native function, `fn(state, block) -> u32`.

- **Leaders**: `pc` 0, every branch target and exception handler, and the instruction after
  every branch and after every instruction handed to the interpreter. Each leader is a Cranelift
  block and an entry point; a `br_table` on the `block` argument starts there.
- **Inline instructions**, each with a fast path for the common types and, for any other types, a
  call that runs the instruction exactly as the interpreter does:
  - register `Move` and constant stores;
  - `+ - *` on `int32` (with overflow, and `-0` for `*`) and on `f64`;
  - `& | ^ << >> >>>` with `ToInt32` of `int32` and `f64`;
  - `< <= > >=`, `== !=` on numbers;
  - `=== !==`: numbers by value, and booleans, `null`, `undefined`, objects and symbols by
    identity;
  - `++ --` and `!`;
  - every jump, including the fused `JumpIfNot{Less,Greater}Than[OrEqual]`, `&&`, `||` and `??`;
  - `IncrementLoopIteration`: the interrupt flag and the loop limit are checked inline.

  Each fast path computes what the interpreter's fast path computes, **to the bit**
  (`value/operations.rs`: an `int32` stays an `int32`, a `NaN` is the canonical one, and `>>>`
  gives a `u32` that is an `int32` only when it fits).
- **Everything else** — property access, calls, environments, iterators, classes, `typeof`,
  strings — goes through `jit_step(state, pc)`. It runs the interpreter's own handler for the
  instruction at `pc` (`OPCODE_HANDLERS`) and answers the `pc` to continue at. Compiled code
  continues at the next instruction, a branch target, or any other leader (through `dispatch`). If
  the frame changed, or there is no block at that `pc`, it returns to the interpreter.
- Helpers are called through absolute addresses, and Cranelift keeps constants and jump tables
  inside the function, so **there is nothing to relocate**. Code that would need relocation is
  refused.

Cranelift settings: `opt_level=speed`, the verifier in debug builds, inline stack probes, and frame
pointers kept so profilers and debuggers can walk compiled frames.

### Entering and leaving

The interpreter loop (`Context::run_loop`) checks one flag, `vm.jit.pending`, before each
instruction. The flag is set when a frame is pushed, when a frame is popped (the caller is back on
top), when a handler catches, and when a loop that may yet be compiled turns over.

When the flag is set, `jit::try_enter` counts the frame's code block hotter. If the block is hot
enough it compiles it, and then it runs the compiled code if a block starts at the frame's `pc`.
So compiled code is entered:

- at a function's start;
- after a call it made returns;
- at an exception handler;
- in the middle of a hot loop (OSR), at the instruction after `IncrementLoopIteration`.

Entering anywhere costs nothing extra, because the frame *is* the state.

### Calls

A call never nests native code. A `Call` in compiled code runs through `jit_step`, which pushes the
callee's frame. Compiled code sees the frame change and returns. The interpreter loop then enters
the callee's compiled code, or interprets it. When the callee returns, the loop re-enters the
caller's compiled code after the call.

So native stack depth does not grow with JavaScript recursion. Recursion limits,
`Maximum call stack size exceeded`, the shadow stack behind `error.stack` and
`Error.captureStackTrace`, and every frame-walking feature behave exactly as they do interpreted.

The price is an exit and an entry per call, about 15 ns together. A call-heavy function therefore
gains nothing from compiling (see the profitability check below), which is why a JIT-to-JIT call
path is the first item of phase 2.

### Deoptimisation and fallback

There is nothing to deoptimise. At every instruction boundary the frame is exactly what the
interpreter would have, so leaving compiled code is just returning to the interpreter loop, which
continues at `frame.pc`. Fallback happens at three levels:

1. **Per operation**: unexpected types take the slow path, which is the interpreter's handler for
   that instruction.
2. **Per instruction**: instructions without an inline form always run through `jit_step`.
3. **Per function**: functions are not compiled when they are generators or async functions, when
   they have more than `max_bytecode_len` bytes of bytecode (32 KB), when compiling them would not
   pay (below), when the context's code budget (64 MB) is spent, or when the host cannot make
   executable memory. Such a function is marked `Rejected` and stays interpreted for good.

### Tier-up

Each `CodeBlock` carries a `JitSlot`: a hotness counter, a tier (`Interpreted`, `Compiled` or
`Rejected`), and its code.

- Hotness is incremented on every entry, every return into the function, and every loop
  iteration. A function is compiled when hotness passes `JitOptions::threshold` (1000 by
  default).
- **Profitability**: a call to a JavaScript function costs an exit from compiled code and an entry
  back, about 15 ns together. That is roughly what eight instructions done inline save. A
  function is compiled only if
  - its loop bodies (the spans of backward branches) do at least eight inline instructions per
    call in them, or
  - at least half its instructions are inline.

  Straight-line glue code (`return a.b(c)`) and loops that mostly call out
  (`s = add(s, id(i))`) are left to the interpreter, where they are no slower.
- `threshold = 0` compiles every function the first time it runs, profitable or not. It is a
  testing mode.

### The garbage collector

There is no new GC interaction to get right.

- Values stay in the VM stack, which the collector already treats as roots.
- Compiled code reads values as bits, only ever stores values that hold no pointer (numbers,
  booleans, `null`, `undefined`), and never keeps a pointer in a machine register across a helper
  call.
- Overwriting a register that holds a pointer goes through `jit_store`, which drops the old value,
  decrementing its reference count, as the interpreter would.
- Copying a pointer value (`Move` of an object) goes through `jit_step`, which clones it.
- `boa_gc` never moves objects.

Collections happen inside helpers (allocation), when compiled code holds nothing.

The test `garbage_collection_during_compiled_code` forces collections from inside compiled loops
while registers alternate between objects and integers.

### Exceptions

An instruction that throws inside `jit_step` goes through the interpreter's own `handle_error`:

- If a handler in the same frame catches it, `jit_step` answers the handler's `pc`, which is a
  leader, and compiled code carries on there.
- If the frame unwinds, compiled code returns, and the interpreter continues in the frame that
  catches it, entering that frame's compiled code if it has some.

Uncatchable errors (interrupts, runtime limits) take the same path out. Backtraces are identical,
because `frame.pc` is always current when anything can throw.

**Panics**: there is no unwind information for compiled code, so no panic may unwind through it.
Every helper runs under `catch_unwind`; compiled code returns, and `jit::runtime::execute` resumes
the panic in Rust, where an isolate's own panic handling sees it as before.

### Generators and async functions

They are not compiled in phase 1: code blocks flagged generator or async are rejected, as is any
code with `Await`, the yield opcodes or `CreatePromiseCapability`.

The design leaves them close, though. A suspended generator's state is its `CallFrame` and its
registers, which compiled code shares. Running `Yield` and `Await` through `jit_step` (which pops
the frame and leaves compiled code), and making the instruction after them a leader, would let a
resumed generator re-enter compiled code like any returning call. That is in phase 2, behind the
same differential tests.

### Executable memory and untrusted code

Code Mode runs model-written programs, so the JIT's memory handling has to hold against hostile
JavaScript:

- **W^X**: each function's code goes into fresh pages mapped read-write. The code is copied in, the
  pages become read-execute (`region`: `mprotect`, or `VirtualProtect` on Windows), and they are
  never writable again. No page is ever writable and executable at once. On AArch64 the
  instruction cache is flushed (`wasmtime-internal-jit-icache-coherence`). The pages are unmapped
  when the code block is dropped.
- **What JavaScript controls**: only which bytecode runs. Compiled code is generated by the
  compiler from Boa's own bytecode, never from bytes the program supplies, and all memory access
  from compiled code is to the frame's registers (indices fixed by the bytecode compiler) and to
  `JitState`.
- **Limits**: hostile code can force compilations by making many hot functions. A context
  compiles at most `code_budget` bytes (64 MB by default; Code Mode should set less), and never a
  function above 32 KB of bytecode.
- **Not yet done**: constant blinding. Integer constants from the program (`StoreInt32`) are
  embedded in machine code as immediates — the classic JIT-spray surface. With W^X and ASLR the
  exposure is small, but blinding untrusted constants is on the roadmap. Until then the
  conservative choice for Code Mode is `set_jit_enabled(false)`, or a low `code_budget`.
- **Platforms**:
  - Windows and Linux allow RW→RX remapping by default.
  - Windows processes with Arbitrary Code Guard cannot use the JIT.
  - macOS under the hardened runtime needs the
    `com.apple.security.cs.allow-unsigned-executable-memory` entitlement, since the JIT maps pages
    without `MAP_JIT`.

  In each case where the JIT cannot make executable memory, compilation fails, the function is
  rejected, and everything runs interpreted. Nothing crashes.

## Switching it on and off

| Where | What |
|---|---|
| `boa_engine` feature `jit` | Builds the JIT (Cranelift, `region`). Off by default. Needs 64-bit and NaN-boxing (not `jsvalue-enum`). |
| `verve` feature `jit` | Turns on `boa_engine/jit`. Off by default. |
| `Context::jit_options_mut()` | `JitOptions { enabled, threshold, max_bytecode_len, code_budget }`. |
| `Context::jit_stats()` | `JitStats { compiled, rejected, entries, code_bytes, compile_time }`. |
| `Runtime::set_jit_enabled(bool)`, `Runtime::jit_enabled()` | Always present; without the feature, off and a no-op. |
| `Runtime::jit_options_mut()`, `Runtime::jit_stats()` | With the feature. |
| `BOA_JIT=0`, `BOA_JIT_THRESHOLD=<n>` | Read when a `Context` is made. |
| `VERVE_JIT=0`, `VERVE_JIT_THRESHOLD=<n>` | Read when a `Runtime` is made, after Boa's. `verve run` and every isolate obey them. |

With the feature built, the JIT is on unless switched off.

## Testing

- **Differential unit tests** (`vm/jit/tests.rs`): every program runs interpreted, then compiled
  eagerly (threshold 0), then compiled when hot (entered mid-run, through OSR). The results must be
  equal, shown so that `-0`, `NaN`, types and thrown errors all count. The programs cover:
  - an operator matrix over 28 values of every type;
  - bigints;
  - `switch`, labelled `break`/`continue`, `for-in`, `do-while`, `??`;
  - exceptions through compiled frames, with `finally`;
  - `error.stack`;
  - closures, classes and accessors;
  - recursion to the limit;
  - generators and async functions, which must stay interpreted.
- **A generated-program fuzzer**: random expressions over every type, in loops, with exceptions
  caught, compared the same way. It runs 150 programs by default; `BOA_JIT_FUZZ_COUNT=3000` runs
  more. It found a real interpreter bug on its first long run: `i32::MIN % -1` panicked in builds
  with overflow checks, fixed in `value/operations.rs`.
- **Behaviour tests**:
  - tier-up happens, and cold code is not compiled;
  - OSR enters a running loop;
  - the GC runs during compiled code;
  - an interrupt from another thread stops a compiled infinite loop;
  - the loop-iteration limit holds;
  - a panic in a native called from compiled code reaches the caller;
  - turning the JIT off takes effect.
- **Whole suites in every configuration**:
  - `boa_engine`'s suite with `BOA_JIT_THRESHOLD=0`, so everything it runs is compiled first;
  - Verve's whole suite, without the feature, with it, forced (`VERVE_JIT_THRESHOLD=0`, which
    reaches the `verve run` children too), and with the feature built but switched off;
  - Verve's `tests/jit.rs`: API and CLI agreement.
- **test262**: every directory except `staging`, run with `BOA_JIT=0` and with
  `BOA_JIT_THRESHOLD=0`, then compared test by test; see the results below.

## Results (phase 1)

Release `verve`, Windows x86-64 (32 threads); milliseconds.

Each benchmark below ran 7 times, with the three configurations interleaved, at high priority;
the table shows the best time. The machine was shared with other work, so differences under
about 10% are noise. The call- and property-heavy rows were measured again on their own, best of
15: callLoop +2%, fib27 +5%, propLoop +2%, push −4% against the build without the feature.

| benchmark | without the feature | feature, `VERVE_JIT=0` | JIT on | speedup |
|---|---|---|---|---|
| `loop`: `s = (s + i * 3) \| 0`, 3M iterations | 190 | 176 | 27 | **7.0×** |
| `bitops`: an FNV-style hash, 2M iterations | 279 | 303 | 33 | **8.5×** |
| `mandel`: Mandelbrot 200×200, `f64` | 156 | 143 | 39 | **4.0×** |
| `sieve`: array reads and writes | 163 | 160 | 166 | 1.0× |
| `objects`: `p.x = p.x + i` | 207 | 201 | 212 | 1.0× |
| `strings`: `+=` and `String.fromCharCode` | 155 | 154 | 165 | 0.9× |
| `fib(27)`, recursive | 133 | 136 | 146 | 0.9× |
| `callLoop`: `s = add(s, id(i)) \| 0` | 798 | 823 | 868 | 0.9× |
| `propLoop`: `s = (s + o.a) \| 0` | 269 | 264 | 305 | 0.9× |
| `push`: `arr.push(i & 7)` | 450 | 463 | 406 | 1.1× |

Numeric loops run 4–9× faster. Code dominated by calls, property access and strings runs as fast
as before, within noise, because it still goes through the interpreter's handlers; that is what
phase 2 is for. The `jit` feature adds 5.3 MB to the release binary (30.3 to 35.9 MB).
Compiling a function takes about 4 ms in a release build, for about 5 KB of machine code (the
first compile on a thread includes building Cranelift's target description).

Correctness at the end of phase 1:

| Suite | Result |
|---|---|
| `boa_engine` (1341 tests) | Passes with the JIT forced (`BOA_JIT_THRESHOLD=0`), off, and at defaults. |
| Generated programs | 3000 match the interpreter. |
| Verve (320 tests) | Passes at defaults, forced (`VERVE_JIT_THRESHOLD=0`, Rebon's plugin host and Code Mode runner included) and off; 317 pass without the feature. |
| test262 | 52,095 results compared one by one between interpreted and JIT-forced runs: **0 differ**. 49,898 pass in both. Covers `language`, `annexB`, `harness`, `intl402` and every `built-ins` directory. |

`test262/test/staging` is left out of the test262 comparison: one test there allocates 64 GB and
aborts the tester process, interpreted or not.

## Roadmap

**Phase 1 (done)**: the baseline described above.

**Phase 2: make calls and objects fast.** This is where plugin code spends its time.

1. **Property inline caches in compiled code.** Boa already has per-instruction inline caches
   (`vm/inline_cache`: shape and slot). For `GetPropertyByName`, `SetPropertyByName` and
   `GetLengthProperty`, emit the shape check and the slot load or store inline, with `jit_step` on
   a miss. This needs a stable view of `JsObject`'s layout (shape pointer and property storage)
   exported from `object/`.
2. **JIT-to-JIT calls.** When the callee is compiled, push its frame through a helper and call its
   code directly, nesting native frames under a depth cap (counted into `run_depth` and checked
   against `native_stack_limit`). Exceeding the cap falls back to the trampoline. The aim is to
   keep the recursion error semantics within the interpreter's limits.
3. **Specialised helpers** with pre-decoded operands for the most common stepped instructions
   (`GetNameGlobal`, `PushFromRegister`, `Call`, `Return`, `SetAccumulator`), skipping the
   decode and the handler table.
4. **Generators and async functions**, as above.
5. **Constant blinding** for untrusted code, and an `untrusted` profile of `JitOptions` (blinding
   on, a smaller budget) for Code Mode.

**Phase 3: type feedback.** Record operand types per instruction in the interpreter and in
baseline code. Then compile a second tier that keeps `int32` and `f64` values unboxed in machine
registers, removes redundant type checks, and keeps loop variables in registers. This tier needs
real deoptimisation (materialising boxed values back into the frame), which phase 1 avoids
entirely. Background compilation on another thread fits here too, since `CodeBlock`s are not
`Send` and would need a snapshot.

**Startup track (separate from the JIT, probably worth more to Rebon):**

1. Lazy function bytecompilation.
2. A bytecode cache keyed by source hash and engine version, for plugin bundles and the plugin
   host.
3. Measure parse vs. compile split first: if the parser dominates, the cache matters more than
   laziness.

## Risks and open problems

- **Exactness depends on the fast paths mirroring `value/operations.rs`.** A change to how the
  interpreter boxes a result (for example `div_fast`, which returns `int32` `0` for `0 / -5`, where
  JavaScript gives `-0` — a Boa bug the JIT deliberately does not inline) must be mirrored. The
  differential tests and the fuzzer are the guard; run them on any change to `value/`.
- **Compile time** is about 4 ms a function, on the JavaScript thread. The type checks of each
  inline instruction expand into many small blocks. A threshold of 1000 and the profitability
  check keep compiles rare, but a program that makes many hot functions pays 4 ms for each.
  Phase 2 should try `opt_level=none`, share the slow paths, and consider compiling on another
  thread.
- **Entry and exit cost** makes call-heavy code no faster in phase 1. The profitability check
  keeps it from being slower.
- **Memory**: compiled code is a page or more per function, freed with its code block. Compiled
  code is not shared between contexts.
- **Cranelift MSRV**: Cranelift's newest releases follow stable Rust closely. The pin must move
  with the toolchain.
- **Hosts without executable memory** (Windows ACG, macOS hardened runtime without the
  entitlement, iOS) run interpreted. This is correct, just not faster.

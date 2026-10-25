# A JIT for Verve

Verve runs JavaScript on Boa, a bytecode interpreter. This is the plan for compiling hot code to
native code, and the record of its first two phases, which are built: a baseline JIT on Cranelift
in `boa/core/engine/src/vm/jit/`, behind the `jit` feature of `boa_engine` and of `verve`. Phase 1
compiled arithmetic, comparisons and branches; phase 2 added property and element access, fast
helpers for the frequent instructions, calls between compiled functions, and generators and
async functions.

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
  every branch, after every instruction handed to the interpreter, and after every inline
  instruction that has a slow path (so that after the slow path compiled code goes on, rather than
  returning to the interpreter at every turn of a loop that adds strings). Each leader is a
  Cranelift block and an entry point; a `br_table` on the `block` argument starts there.
- **Validation**: the compiler does not trust the bytecode it is given. It refuses a code block
  if:
  - a branch, jump-table entry or exception handler points anywhere but the start of an
    instruction;
  - the last instruction could fall off the end of the code;
  - a register that compiled code reads or writes itself is not below the code block's register
    count, or the register file would not fit `i32` offsets.

  Register offsets are computed with checked arithmetic.
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
  - `IncrementLoopIteration`: the interrupt flag and the loop limit are checked inline;
  - `%` of two `int32`s (phase 2), with `rem_fast`'s `-0`;
  - `Move` of an object, and a store over one: the reference count is the first word of the
    object's allocation, counted up and down inline (phase 2);
  - property and element access, through caches (phase 2, below).

  Each fast path computes what the interpreter's fast path computes, **to the bit**
  (`value/operations.rs`: an `int32` stays an `int32`, a `NaN` is the canonical one, and `>>>`
  gives a `u32` that is an `int32` only when it fits).
- **Fast helpers** (phase 2, `helpers.rs`): Rust functions with the operands decoded at compile
  time, for the frequent instructions whose common case needs the engine's data structures (below).
- **Everything else** — calls, environments, iterators, classes, `typeof`, strings — goes through
  `jit_step::<OP>(state, pc)`. It runs the interpreter's own handler for the instruction at `pc`
  and answers the `pc` to continue at. There is one instance per opcode (`STEPS`), so the handler
  is a direct call and the opcode is not decoded again. Compiled code continues at the next
  instruction, a branch target, or any other leader (through `dispatch`). If the frame changed,
  or there is no block at that `pc`, it returns to the interpreter.
- Helpers are called through absolute addresses, and Cranelift keeps constants and jump tables
  inside the function, so **there is nothing to relocate**. Code that would need relocation is
  refused.

Cranelift settings: `opt_level=speed`, the verifier in debug builds, inline stack probes, and frame
pointers kept so profilers and debuggers can walk compiled frames.

### Fast helpers

Each fast helper does exactly what the interpreter's handler does in the common case and answers
*done*, or changes nothing and answers *slow*, after which compiled code runs the instruction with
`jit_step`. So a fast path only has to be right about when it applies. None of them throws,
calls JavaScript or reads `frame.pc`. A panic in one is caught, kept, and turned into *slow*;
`jit_step` then returns at once and `execute` resumes the panic.

| Instruction | Fast path |
|---|---|
| `PushFromRegister` (up to three in a row, one call) | push a clone; the registers' address is reloaded, the stack may have moved |
| `PopIntoRegister`, `SetAccumulator`, `GetArgument`, `CheckReturn` | as the handler, when there is a value to pop / the frame is not a constructor's |
| `GetName`, `GetNameGlobal` | when `find_runtime_binding` would leave the binding where the bytecode compiler put it (declarative current environment, no `with`, not poisoned by `eval`, not deleted): read it as `get_binding` does; a binding on the global object through the instruction's inline cache |
| `GetPropertyByName`, `SetPropertyByName` | a hit of the instruction's inline cache on a data property (own or, for a read, on the prototype); fills the access's site cache |
| `GetLengthProperty` | an array's `length`, a string's, or as `GetPropertyByName` |
| `GetPropertyByValue`, `SetPropertyByValue` | a dense array by an `int32` index, as `get_dense_property` and `set_dense_property` |

### Property caches and elements

Boa's interpreter keeps an inline cache per property access (`vm/inline_cache`: up to four shapes,
each with a slot, held weakly). Compiled code keeps its own, simpler cache per access, the *site
cache* (`layout.rs`): the shape of the last object whose own data property it was (two words),
the property's slot, and the count of garbage collections at the time. A site is filled by the
helper, only from a hit of the interpreter's cache, and only for a **shared** shape, whose
properties never change; a dictionary-mode (unique) shape is left to the helper.

`GetPropertyByName` and `SetPropertyByName` check in machine code: the value is an object, its
`GcRefCell` is not borrowed mutably (not borrowed at all, for a write), its shape's two words are
the site's, no collection has run since the site was filled, and the slot is in the storage. Then
they read or write the slot, counting references inline when the old or new value is an object
(any other pointer — a string, a symbol, a bigint — takes the helper). On a miss the helper runs,
and fills the site.

**Why the count of collections**: a shape is freed only by a collection, and its address may then
be taken by a new shape. `boa_gc` counts collections (`boa_gc::raw::collections`), before anything
is freed; a site is trusted only while the count is the one it was filled at. After each
collection every site misses once. The test `collections_invalidate_property_caches` checks that
the check is there (it fails without it).

`GetLengthProperty` of an array is inline (its vtable is the array's; `length` is its first
slot). `GetPropertyByValue` and `SetPropertyByValue` of a dense array by a non-negative `int32`
index are inline: the vtable, the borrow and `extensible` flags, the kind of the elements (`i32`,
`f64` or values) and their length, then the element. A write that would change the kind of the
elements (a float into `i32`s) takes the helper, which does what the interpreter does.

**Layout**: the offsets come from `offset_of!` and from `boa_gc::raw` (the value in a `GcBox`, a
`GcRefCell`'s flag and value). `Shape`'s representation is `repr(u64)` (a tag word and a pointer,
no padding), and so is `IndexedProperties`'s. Where a `Vec` keeps its pointer and length, and a
`ThinVec` its length and elements, Rust does not say: they are found by looking at real ones.
Everything is checked on each thread, the first time it compiles, against a real object (the
borrow flag in each state, the shape's words, the storage, the vtable, `extensible`, each kind of
elements). If anything does not match, property and element accesses are compiled as helper calls
only.

Two builtins got the same kind of shortcut, in the interpreter as much as in compiled code:
`Array.prototype.push` and `Array.prototype.fill` on a *plain dense array* — the realm's array
shape (a writable `length`, nothing else), extensible, dense elements, and `Array.prototype` and
`Object.prototype` as its prototypes with no elements (and a null prototype after them) —
append or write the elements directly, which is all their specification steps would do there.

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

A `Call` in compiled code runs through `jit_step`, whose handler pushes the callee's frame as the
interpreter does (`function_call`, the shadow stack, `this`, environments). Then, in phase 2:

- if the callee's code is compiled (or gets hot enough now), `jit_step` runs it **nested**, on
  the native stack under the caller's helper (`run_callees`), and when the caller's frame is back
  on top it carries on after the call, in the caller's compiled code;
- otherwise compiled code returns, the interpreter's loop runs the callee, and re-enters the
  caller's compiled code after the call, as in phase 1.

Nesting stops at 64 compiled frames (`MAX_NESTED`) and when the native stack used since the
outermost run passes a quarter of `RuntimeLimits::native_stack_limit`; past either, calls go back
to the loop. The frames are the interpreter's own in both cases, so recursion limits,
`Maximum call stack size exceeded`, `error.stack` and every frame-walking feature behave exactly
as interpreted. An exception thrown in a nested callee unwinds the frames as the interpreter
does: if a compiled caller catches it, its helper carries on at the handler; if it unwinds past
the caller, the caller returns too.

A nested call saves the return to the loop and the two entries of phase 1. What is left of a call
is Boa's own: pushing and popping the frame, `function_call` and `handle_return`, over 100 ns,
which compiled code does not change (see the roadmap).

### Deoptimisation and fallback

There is nothing to deoptimise. At every instruction boundary the frame is exactly what the
interpreter would have, so leaving compiled code is just returning to the interpreter loop, which
continues at `frame.pc`. Fallback happens at three levels:

1. **Per operation**: unexpected types take the slow path, which is the interpreter's handler for
   that instruction.
2. **Per instruction**: instructions without an inline form always run through `jit_step`.
3. **Per function**: a function stays interpreted for good (it is marked `Rejected`) when:
   - it has more than `max_bytecode_len` bytes of bytecode (32 KB);
   - its bytecode fails validation (above);
   - compiling it would not pay (below);
   - one of the context's limits is reached (below).
4. **Per process**: if the code generator panics, or the host refuses executable memory (or
   Cranelift has no backend for it), the function stays interpreted and the JIT switches itself
   off for the rest of the process, so no function pays for a compile that would fail the same
   way. The panic is caught and does not unwind into the host. `boa_engine::vm::jit_unavailable()`
   tells which of the two happened.
5. **Per thread**: if the object layout does not check out (above), property and element
   accesses compile to helper calls only.

### Tier-up

Each `CodeBlock` carries a `JitSlot`: a hotness counter, a tier (`Interpreted`, `Compiled` or
`Rejected`), and its code.

- Hotness is incremented on every entry, every return into the function, and every loop
  iteration. A function is compiled when hotness passes `JitOptions::threshold` (1000 by
  default).
- **Profitability**: a call to a JavaScript function that is not compiled costs an exit from
  compiled code and an entry back, about 15 ns together (a compiled callee runs nested and costs
  neither). That is roughly what eight instructions done inline save. A function is compiled
  only if
  - its loop bodies (the spans of backward branches) do at least eight inline instructions per
    call in them, or
  - at least half its instructions are inline.

  Since phase 2 an instruction done by a fast helper counts as inline, so most code passes;
  straight-line glue made of what only `jit_step` does is still left to the interpreter.
- `threshold = 0` compiles every function the first time it runs, profitable or not. It is a
  testing mode.
- **Limits**, each settable through `jit_options_mut()`. Bytes are counted as mapped, rounded up to
  pages:
  - `max_live_code_bytes` (64 MB): the code mapped at once. The bytes are given back when a
    function's code is dropped with its code block.
  - `max_total_code_bytes` (256 MB): the code compiled in the context's lifetime, given back or
    not. This bounds the time and memory compiling can take.
  - `max_compiled_functions` (10 000): the functions compiled in the context's lifetime.

  `jit_stats()` reports `code_bytes` (lifetime), `live_code_bytes` and `live_functions`.

### The garbage collector

There is no new GC interaction to get right.

- Values stay in the VM stack, which the collector already treats as roots.
- Compiled code reads values as bits and never keeps a pointer in a machine register across a
  helper call.
- It stores objects (a `Move`, a property or element read) only after counting the new reference
  inline, as `clone` does; other pointers (strings, symbols, bigints) are cloned by helpers.
- Overwriting a register or a property slot that holds an object counts the reference down
  inline, which is all dropping an object does (`boa_gc` frees only in collections, and compiled
  code never runs while one is dropping). Other pointers are dropped by `jit_store` or a helper.
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
the panic in Rust, where an isolate's own panic handling sees it as before. A panic while
*compiling* is caught the same way and switches the JIT off (above).

**Unwind information**: compiled functions are not registered with the platform unwinder
(`RtlAddFunctionTable` on Windows, `__register_frame` elsewhere).

- Nothing needs it for correctness: no unwind passes through compiled code.
- It does show in native backtraces. A crash dump, a debugger or a sampling profiler that stops
  inside compiled code, or inside a helper it called, may be unable to step past the compiled
  frame. Frame pointers are kept, so frame-pointer walkers (`perf --call-graph fp`, ETW with frame
  pointers) still get through, but `RtlVirtualUnwind`-based tools such as Windows crash dumps and
  WinDbg's `k` stop there.
- Rust's own `std::backtrace` from a panic inside a helper shows the frames above compiled code
  only up to it.

Registering unwind info (Cranelift can emit it) is on the roadmap.

### Generators and async functions

They are compiled since phase 2. A suspended generator's state is its `CallFrame` and its
registers, which compiled code shares. `Generator`, `GeneratorYield`, `Await` and the async
generator opcodes run through `jit_step`, take the frame off the stack and so make compiled code
return. A resumed frame comes back with its registers on the stack (wherever they now are; `execute`
reads `rp` from the frame), and the interpreter's loop enters it at the instruction after the
suspend point, which starts a block as the instruction after every stepped one does.

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
- **Limits**: hostile code can force compilations by making many hot functions. Per context:
  - `max_live_code_bytes`, `max_total_code_bytes` and `max_compiled_functions` bound the memory
    and the compile time (above);
  - no function above 32 KB of bytecode is ever compiled.

  Code Mode should set all three far lower than the defaults.
- **Not yet done: constant blinding.** Every `Store*` constant from the program is embedded in
  the machine code as an immediate, the classic JIT-spray surface:
  - `StoreDouble` embeds a full 8-byte immediate the program chooses freely;
  - `StoreFloat`, `StoreInt32`, `StoreInt16` and `StoreInt8` embed 4, 4, 2 and 1 bytes;
  - the other constant stores embed fixed values the program cannot choose.

  With W^X and ASLR the exposure is small, but it is not none. Blinding would emit `k ^ c` and
  `xor` with `c` (a random key per function) instead of `k` for every program-chosen immediate.
  It is on the roadmap. Until it lands, the conservative choice for Code Mode is
  `set_jit_enabled(false)`, or low limits.
- **Platforms**:
  - Windows and Linux allow RW→RX remapping by default.
  - Windows processes with Arbitrary Code Guard (ACG) cannot use the JIT at all.
  - macOS: code is mapped with `MAP_JIT`, so a process under the hardened runtime needs only
    `com.apple.security.cs.allow-jit`. On Apple silicon the pages are switched writable for the
    compiling thread alone with `pthread_jit_write_protect_np`, written, and switched back
    before anything runs, which is what keeps W^X there; on Intel they are made read-execute
    with `mprotect`. The instruction cache is cleared as on other non-x86 targets. Where
    `MAP_JIT` is refused the ordinary mapping is tried (a process that is not hardened, or holds
    `allow-unsigned-executable-memory`). A hardened process with neither entitlement runs
    interpreted. `.github/workflows/macos-jit.yml` checks all three on both architectures, with
    `examples/jit_probe.rs`.
  - AArch64 Linux: after writing code, `pipeline_flush_mt` issues a `membarrier(2)`
    (`MEMBARRIER_CMD_PRIVATE_EXPEDITED_SYNC_CORE`) so that every thread's instruction stream sees
    it.
    - The process registers itself for that command the first time it is refused.
    - Kernels before 4.16, which lack the command, get the much slower `MEMBARRIER_CMD_GLOBAL`.
    - A seccomp filter that denies `membarrier` (some container profiles do) makes the flush
      fail. Compiling then fails, and the JIT switches itself off (below) rather than run
      possibly stale instructions.

    x86-64 needs no such flush.

  In each case where the JIT cannot make or publish executable memory, the first compile fails,
  that function is rejected, and the JIT switches itself off for the process, so later hot
  functions do not each pay a full compile to be refused. Everything runs interpreted. Nothing
  crashes.

## Switching it on and off

| Where | What |
|---|---|
| `boa_engine` feature `jit` | Builds the JIT (Cranelift, `region`). Off by default. Needs 64-bit and NaN-boxing (not `jsvalue-enum`). |
| `verve` feature `jit` | Turns on `boa_engine/jit`. Off by default. |
| `Context::jit_options_mut()` | `JitOptions { enabled, threshold, max_bytecode_len, max_live_code_bytes, max_total_code_bytes, max_compiled_functions }`. |
| `Context::jit_stats()` | `JitStats { compiled, rejected, entries, code_bytes, live_code_bytes, live_functions, compile_time }`. |
| `boa_engine::vm::jit_unavailable()` | `Some(CompilerPanicked \| NoExecutableMemory)` once the JIT has switched itself off for the process. |
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
  - property caches: hits, misses, new shapes at a site, megamorphic sites, prototypes, accessors
    (own and inherited), deletes, frozen and non-writable properties (sloppy and strict),
    dictionary-mode objects, primitives, `null`, proxies, lengths;
  - elements of each kind, holes, out of range, negative and non-integer indexes, non-extensible
    arrays, setters on `Array.prototype`, typed arrays, strings;
  - nested calls: past the nesting cap, exceptions through compiled callees caught at every
    depth, `finally`, the recursion error, calls through generators and natives, `error.stack`;
  - the GC during nested calls, and property caches across collections (shapes freed and new
    ones made);
  - generators (`yield*`, `return()`, `throw()`) and async functions and generators, with their
    promise jobs run;
  - `push` and `fill`, against Node's output.
- **A generated-program fuzzer**: random expressions over every type, in loops, with exceptions
  caught, compared the same way. It runs 150 programs by default; `BOA_JIT_FUZZ_COUNT=3000` runs
  more. It found a real interpreter bug on its first long run: `i32::MIN % -1` panicked in builds
  with overflow checks, fixed in `value/operations.rs`.
- **Hardening tests**:
  - a panicking code generator leaves the function interpreted and switches the JIT off;
  - refused executable memory switches it off after one try;
  - malformed bytecode (targets inside instructions or past the end, registers out of range,
    falling off the end) is refused;
  - each limit holds, and code is given back when its function is collected;
  - the slow path of string `+` stays in compiled code.
- **Behaviour tests**:
  - the object layout checks out on the test thread (else accesses would silently be helpers);
  - a collection makes the next access through a site miss, and only then;
  - a loop calling a compiled function does not return to the interpreter per call;
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

## Results (phase 2)

Release `verve`, Windows x86-64 (32 threads), the same benchmarks; milliseconds, best of 7 to 15
runs with the configurations interleaved at high priority. The machine was shared with other
builds and test runs (50–85% busy), so the absolute numbers are higher than phase 1's table and
differences under about 15% are noise; the columns were measured together and compare.

| benchmark | Node 22 | phase 1 JIT | phase 2 JIT | phase 2, `VERVE_JIT=0` | phase 2 vs phase 1 |
|---|---|---|---|---|---|
| `loop` | 3 | 17 | 17 | 164 | 1.0× |
| `bitops` | 9 | 27 | 24 | 237 | 1.1× |
| `mandel` | 6 | 40 | 36 | 122 | 1.1× |
| `fib(27)` | 3 | 140 | 94 | 113 | **1.5×** |
| `objects`: `p.x = p.x + i` | 5 | 230 | 20 | 166 | **11.5×** |
| `strings`: `+=` and `String.fromCharCode` | 5 | 159 | 109 | 135 | **1.5×** |
| `sieve`: `fill` and element writes | 5 | 126 | 14 | 80 | **9.0×** |
| `callLoop`: `s = add(s, id(i)) \| 0` | 9 | 851 | 522 | 657 | **1.6×** |
| `propLoop`: `s = (s + o.a) \| 0` | 2 | 153 | 23 | 224 | **6.7×** |
| `push`: `arr.push(i & 7)` | 18 | 355 | 116 | 163 | **3.1×** |

- Property and element access, the rows that phase 1 left at interpreter speed (and `objects`
  slower than interpreted), are now 3–12× faster than phase 1, within 3–11× of Node.
- Calls are about 1.5× faster than phase 1 and 15–25% faster than the interpreter, and still
  30–60× slower than Node, which inlines them: what is left is Boa's frame machinery (roadmap).
- Some gains are the interpreter's too (`VERVE_JIT=0` column against phase 1's interpreter,
  about 175–200 ms for `objects` and 150–175 for `sieve`): `push` and `fill` on plain dense arrays,
  native calls popping their arguments without allocating, cheaper `Gc` drops
  (`finalizer_safe`), and registers made without cloning.
- Compiling: about 5 ms a function in a release build (the first on a thread includes the ISA
  and the layout check), 57 KB mapped for the 5 functions of the numeric benchmarks. The release
  binary is 0.6 MB larger than phase 1's (36.6 MB), mostly the per-opcode `jit_step`.

Why `objects` was slower compiled than interpreted in phase 1: each of its four property accesses
and two `Move`s of the object went through `jit_step`, which adds a call, a decode of the opcode
and a check of the frame to the interpreter's own handler, about 40 cycles each, while the five
instructions it did inline saved less than that.

Correctness at the end of phase 2:

| Suite | Result |
|---|---|
| `boa_engine` (1358 tests, 3 ignored) | Passes with the JIT forced (`BOA_JIT_THRESHOLD=0`), off, and at defaults. |
| Generated programs | 3000 match the interpreter. |
| Verve (322 tests, 10 ignored) | Passes at defaults, forced (`VERVE_JIT_THRESHOLD=0`) and off, with the `jit` feature. The `fetch_network` tests time out on a heavily loaded machine in every mode, JIT or not, and pass otherwise. |
| test262 | 52,095 results compared one by one between interpreted and JIT-forced runs: **0 differ** but for two timing-dependent `Atomics.notify` tests (different ones on each run) that time out under load and pass in both modes when rerun alone. 49,895 pass in both. |

## Roadmap

**Phase 1 (done)**: the baseline described above.

**Phase 2 (done): calls and objects.**

1. Property caches in compiled code: own data properties of shared shapes inline, everything the
   interpreter's cache holds through a fast helper.
2. Calls between compiled functions, nested under a depth cap.
3. Fast helpers for the frequent instructions, and `jit_step` instanced per opcode.
4. Elements of dense arrays inline; `push` and `fill` shortcuts in the builtins.
5. Generators and async functions compiled.

**Phase 2, left over**, in the order the profiles suggest:

1. **The cost of a call itself.** With the rest fast, a call is Boa's frame machinery: in the
   `callLoop` profile, `handle_return` (dropping the frame's stack values and the `CallFrame`'s
   handles) 10%, `push_frame` 8%, `function_call` 8%, `CallValue::resolve` 6%, the pushes of
   `this`, the function and the arguments 7%. Options: a leaner `CallFrame` (fewer `Gc` handles
   cloned and dropped per call: realm, active runnable, environments), a direct call path for
   plain functions (no environment, no `arguments`) from compiled code, and keeping `this`/the
   function out of the value stack.
2. **Method calls**: `o.m()` reads `m` from the prototype through the helper and the
   interpreter's cache; inline prototype reads need a check of the prototype's shape too (the
   object's shape does not change when its prototype's properties do).
3. **Dictionary-mode objects and the global object** (unique shapes) inline: a unique shape keeps
   its identity when a property is added or its attributes change, so a cache needs a version
   (`unique_shape.rs` has a `TODO: invalidate the pointer`).
4. **Strings and native calls**: `+` of strings, `length` and indexing of strings, and the cost of
   calling a native function (`native_function_call`: shadow stack, realm swap).
5. **Constant blinding** of every program-chosen `Store*` immediate (`StoreDouble`, `StoreFloat`,
   `StoreInt32`, `StoreInt16`, `StoreInt8`), and an `untrusted` profile of `JitOptions` (blinding
   on, low limits) for Code Mode, if Code Mode is ever to run with the JIT.
6. **Unwind information** registered for compiled functions, so native debuggers, crash dumps and
   profilers walk through them.
7. **Compile time**: phase 2 compiles more code inline per function (see Results).

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
- **Compile time** is about 5 ms a function, on the JavaScript thread. The type checks of each
  inline instruction expand into many small blocks, and phase 2 added the property and element
  accesses. A threshold of 1000 and the profitability check keep compiles rare, but a program
  that makes many hot functions pays 5 ms for each. Next: try `opt_level=none`, share the slow
  paths, and consider compiling on another thread.
- **Layout knowledge.** Compiled code reads objects, shapes, borrow flags, property storage and
  dense elements directly. The offsets come from the types (`offset_of!`, `repr`) or are found
  by looking, and are all checked against a real object before use, so a change to those types
  either keeps working or turns inline accesses off. What the checks cannot see is a change of
  *meaning* — a new borrow state, a new kind of elements, a shape that changes its properties in
  place — which needs the JIT changed with it. `layout.rs` lists what is relied on.
- **The interpreter's caches decide.** Fast helpers hit only where the interpreter's inline cache
  hits, and site caches are filled only from such hits; if the interpreter's cache is wrong (for
  example a prototype property removed under a cached `PROTOTYPE` slot), compiled code is wrong
  the same way. That is by design — the differential tests compare the two — but it means a fix
  to the caches must be made in the interpreter.
- **Builtin shortcuts** (`push`, `fill`) are exact only while their preconditions mirror the
  specification's steps; a change to how arrays store `length` or elements must keep them in
  step. Their tests compare against Node's output, not only the interpreter.
- **Nested calls use native stack**: up to 64 compiled frames, within a quarter of
  `native_stack_limit`. A host that runs scripts on a very small stack should lower the limit,
  as it already must for nested runs.
- **Call-heavy code** gains about 15–20% over the interpreter: the rest is Boa's frame machinery
  (roadmap).
- **Memory**: compiled code is a page or more per function, counted in pages and freed with its
  code block. Compiled code is not shared between contexts.
- **Cranelift MSRV**: Cranelift's newest releases follow stable Rust closely. The pin must move
  with the toolchain.
- **Hosts without executable memory** (Windows ACG, macOS hardened runtime without the
  entitlement, iOS, AArch64 Linux with `membarrier` denied) run interpreted after one failed
  compile. This is correct, just not faster.

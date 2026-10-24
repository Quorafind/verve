# Boa for Su

This branch (`su/0.22`) is Boa 0.22.0 (`v0.22`) with the fixes Su needs to run Obsidian community
plugins unchanged. Su takes every Boa crate from here through `[patch.crates-io]`, so the crates
always agree with one another.

## What it carries

- `boa_ast`, `scope_analyzer.rs`: a class method's computed name (`class C { [key]() {} }`) is
  visited by the scope analysis, as a field's and an object method's are. Without it a binding the
  name used from an enclosing function was read from the wrong environment and the class threw
  `ReferenceError: access of uninitialized binding` (bundles built with Effect, Obsidian Day
  Planner among them). `ClassMethodDefinition::name` is `pub(crate)` for it.
- `boa_ast`, `scope.rs`: `this` in an arrow function inside another arrow function
  (`method() { return () => { f(x => this.y) } }`) escapes past the arrows to the function that
  owns `this` (`Scope::mark_arrow`, set in `visit_function_like`). Before, the method's `this` was
  not kept and the inner arrow read `undefined` (Admonition's and Day Planner's settings).

- `boa_parser`, `left_hand_side/mod.rs`: `super(...)` is a whole CallExpression, so what follows
  a call follows it too — `super(x).then(f)`, `super()[k]`, `super()()`, `super()?.x`, a tagged
  template. Before, the parser stopped at the call and `return super(x).then(f)` was a
  `SyntaxError: expected token ';', got '.'` (Terminal's bundle).

- `boa_engine`, `builtins/regexp`: a compiled pattern is kept per thread by its source and flags
  (`COMPILED`, up to 1024 of them), so a regular expression literal evaluated again — once per
  line of a note, in Dataview's parser — no longer compiles its pattern every time.

- `boa_engine`, `builtins/array`: `sort`, `toSorted` and the typed arrays' `sort`/`toSorted` sort
  with their own stable merge sort (`stable_sort_by`), not `slice::sort_by`. Since Rust 1.81
  `sort_by` may panic when the comparison is not a total order, and a JavaScript comparator often
  is not one (`(a, b) => a.compareDocumentPosition(b) === 4 ? -1 : 1`, a random one, one that
  answers `NaN`) — the panic took the whole app down. The merge sort only reads in bounds and
  always ends; such a comparator gets an unspecified order, as the specification allows. An
  exception from the comparator still stops the sort at once.

- `boa_engine`, `bytecompiler`: the callee of a method call (`a.f()`, `a[k]()`), an item of an
  optional chain (`a?.b.c`) and a name looked up in an environment (`x is not defined`) carry
  their own source position. They had none, so an error there was reported at the start of the
  enclosing function — for a plugin, whose bundle Su wraps in a CommonJS function, at the first
  line of `main.js` (`var Kp = window.i18next, za = Kp.createInstance()` in Latex Suite). And
  reading or setting a property of `null`/`undefined` throws V8's message, `Cannot read
  properties of undefined (reading 'createInstance')` / `Cannot set properties of null (setting
  'x')`, not `cannot convert 'null' or 'undefined' to object` (`nullish_base_error` in
  `vm/opcode`). The bytecode snapshots in `tests/insta-bytecode` changed only in their source maps.

- `boa_engine`, `builtins/error/stack.rs`: V8's stack trace API, which packages bundled from npm
  use (`depd` in express and body-parser, `source-map-support`, custom error classes):
  - `error.stack` is V8's format: `${error}` (name and message, read when the stack is read),
    then a `    at f (file:line:col)` line per frame, newest first — `    at file:line:col` for
    top-level code, `(native)` for a built-in. Before, it was the frames alone, so the first line
    was a frame, and a native error carried a line for the Rust location it was made at.
  - `Error.captureStackTrace(target, constructorOpt)` gives `target` an own `stack` accessor with
    the current stack, without `constructorOpt`'s newest call and what is above it (without
    every frame if it is not on the stack). The function is found by its code
    (`ShadowStack::capture`, `SourceInfo::ptr_eq`). The stack is formatted on first read, as in
    V8, so a message set after the capture is in it; the setter replaces it with a value.
  - `Error.stackTraceLimit` (10, writable): how many frames an error or a capture keeps, up to
    `RuntimeLimits::backtrace_limit`; none when it is not a number. Only the frames in `stack`
    — the backtrace a `JsError` carries to the embedder still follows `backtrace_limit`.
  - `Error.prepareStackTrace(error, callSites)`, when set, makes the `stack` instead, with
    `CallSite` objects (`getFunctionName`, `getFileName`, `getLineNumber`, `getColumnNumber`,
    `isNative`, `toString`, …; `getThis`/`getFunction` are `undefined`, `getTypeName` and
    `getMethodName` `null`, `isToplevel` always true). A `stack` read inside it is formatted
    plainly rather than calling it again.
  - `bytecompiler/class.rs`: a class method with a literal name has that name in its code, so its
    frames show it (`at method (…)`) rather than looking like top-level code.

- `boa_engine`, `vm/runtime_limits.rs`, `vm/mod.rs`: the recursion limits are V8's, and hitting
  one throws V8's `RangeError: Maximum call stack size exceeded`, which JavaScript can catch
  (code probes how deep it may go with `try { recurse() } catch {}`); before, it was an
  uncatchable `RuntimeLimitError` after only 512 calls, and the value stack's 10 240 slots held
  even fewer frames — or one function with that many bindings. Measured: a call from JavaScript
  to JavaScript takes no native stack (its frame is on the heap); a call from native code back
  into JavaScript nests the interpreter, ~1.6–1.7 KiB a level in a release build (a getter, a
  `map` callback, `call`/`apply`, a template's `toString`), ~3 KiB for `eval`, ~4.5 KiB for a
  `JSON.parse` reviver, and 6–7.5 / 10 / 16.6 KiB in a debug build. So:
  - `recursion` is 10 000 frames (was 512) and `stack_size` 1 Mi values (was 10 Ki).
  - What bounds the native stack is a new `native_stack` limit, in bytes, counted from where the
    outermost run of the interpreter started (`Vm::native_stack_base`, `run_depth`) and checked
    at every call and `eval`: 1 MiB by default, which fits Rust's default 2 MiB thread.
    **Su should set it to about half the stack of the thread the plugins run on** —
    `context.runtime_limits_mut().set_native_stack_limit(4 << 20)` on an 8 MiB thread, `16 << 20`
    on the plugin host's 32 MiB one. With 4 MiB on 8 MiB, a `map`/getter/`apply` chain goes
    ~2 400 levels deep in release (~550 in debug), a `JSON.parse` reviver ~900 (~250); every
    kind ended in the `RangeError`, never an overflow, in both profiles.
  - `RuntimeLimitError::Recursion` and `StackSize` are no longer thrown; the interrupt and the
    loop limit are still uncatchable.
  The tests that pinned the old uncatchable error (`recursion_in_async_gen…`,
  `recursion_in_setter…`) now expect the `RangeError`.

- `boa_engine`: `Array.fromAsync`, `ArrayBuffer.prototype.transfer`/`transferToFixedLength`/
  `detached`, `Error.isError` and `Atomics.pause` are there without the `experimental` feature —
  finished proposals V8 ships, which Su (built without `experimental`) lacked. The proposals V8
  does not ship (`Promise.allKeyed`, `Iterator.prototype.includes`, `import.defer`) stay behind
  it.

- `boa_engine`, `builtins/date/utils.rs`: `Date.parse` and `new Date(string)` read the formats
  V8 reads beyond the specified ones (`parse_legacy_date`): `2020/01/31`, `2020-01-31 10:05`,
  `2020-1-5`, `01/31/2020`, `12-25-2020`, `January 31, 2020`, `31 Jan 2020`,
  `Jan 31, 2020 3:04 PM`, `Tue Jan 01 2020 10:00:00 GMT+0800 (China Standard Time)`,
  `…T10:00:00+0800`; a two-digit year is 19xx from 50, else 20xx. Local time without a zone, as
  in V8. Before, all of them were `Invalid Date` — moment falls back to `new Date(string)` for
  them, and notes are full of such dates. An unknown word still makes a string not a date.

- `boa_engine`, `builtins/date`, `builtins/number`: without the `intl` feature (Su builds
  without it), `Date.prototype.toLocaleString`/`toLocaleDateString`/`toLocaleTimeString` format
  as V8 does for `en-US` by default — `1/15/2020, 8:05:09 PM` — instead of returning `toString`'s
  `Wed Jan 15 2020 20:05:09 GMT+0800`; and `Number.prototype.toLocaleString` groups thousands and
  keeps at most three fraction digits (`1,234.568`) instead of returning `toString`'s. The
  locales and options are ignored, as before.

- Parsing and compiling a function with thousands of bindings — a bundle's top level, which Su
  wraps in a CommonJS function — was quadratic in them, three ways: `boa_ast`'s `Scope` searched
  its bindings list by name for every identifier (now also a name → position map,
  `Scope::positions`); `FunctionDeclarationInstantiation` (`bytecompiler/declarations.rs`) kept
  its name lists as `Vec`s searched with `contains` (now `FxHashSet`s); and the register
  allocator scanned all registers — one persistent register per local binding — for a free one
  on every allocation (now a min-heap of free registers, `RegisterAllocator::free`, which hands
  out the same lowest-free register, so the bytecode is unchanged). Sheet Plus (19 MB): parse
  19.4 s → 8.0 s, compile 17.9 s → 2.2 s; make.md (5.6 MB): 3.3 + 2.6 s → 2.2 + 0.9 s; a
  synthetic 1 MB top level: compile 1.85 s → 0.31 s.

- `boa_gc`, `lib.rs`: a collection no longer frees what it found dead before it returns. The
  dead nodes leave the heap, and their memory is returned a few at a time by later allocations
  (`BoaGc::unfreed`, `FREE_PER_ALLOCATION`), the rest before the next collection — and all of
  them at once in `force_collect`. Safe because by then nothing can reach them: they were
  unreachable, their finalizers have run, and every weak reference to them was cleared. And the
  second marking after finalizers is skipped when no dead node became a root (only a resurrection
  makes one). On Dataview's heap in Su, a collection's pause fell from ~30–45 ms to ~21–35 ms;
  the sweep part of it from 10–25 ms to ~1 ms.

- `boa_engine`: `Context::interrupt_handle()` hands out an `InterruptHandle` any thread can use
  to stop what the context is running — V8's `TerminateExecution`. The next loop iteration or call
  throws `RuntimeLimitError::Interrupted`, an engine error JavaScript cannot catch, and the
  request is used up, so what runs next runs. For an embedder whose watchdog finds a script that
  will not return.

## Keeping it

Rebase onto the next release tag when Su moves to it, and drop what upstream has fixed.

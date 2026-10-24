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

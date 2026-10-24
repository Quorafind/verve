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

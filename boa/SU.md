# Boa for Su

This branch (`su/0.22`) is Boa 0.22.0 (`v0.22`) with the fixes Su needs to run Obsidian community
plugins unchanged. Su takes every Boa crate from here through `[patch.crates-io]`, so the crates
always agree with one another.

## What it carries

- `boa_engine`, `module`, `vm/opcode/call`: `ModuleLoader::prepare_import` runs before a
  dynamic import enters the job queue, so hosts can capture a blob URL before JavaScript revokes
  it. A preparation error rejects the import promise. `Module::load_link_evaluate_sync` loads
  synchronous dependency graphs without draining unrelated jobs, rejects top-level await before
  evaluation and detects re-entrant evaluation; `Module::set_synthetic_export` updates live
  bindings in synthetic module namespaces. `Module::namespace_with_marker` supplies an additional
  immutable marker without snapshotting live bindings or changing the ordinary namespace.
  Node-specific errors and resolution remain the embedder's responsibility.

- `boa_parser`, `expression`: `async(...)` uses the ordinary argument grammar until a following
  `=>` identifies an async arrow head. A call to a function named `async`, including the nested
  `iterator(item, async(callback))` used by asynckit, remains a call and keeps its following
  operators and call chain. Async arrows retain their binding, rest, `await`, strict-mode and
  duplicate-parameter checks; regex and template arguments use the normal lexer goals.

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

- `boa_engine`, `builtins/symbol`: `Symbol.for` finds or makes its symbol under the registry
  key's entry, not by a lookup followed by an insert. Two threads asking for a key the registry
  lacked each made a symbol, the registry kept one, and a realm that had the other never matched
  `Symbol.for(key)` again (React's `react.element`, so `Minified React error #31`).

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

- `Function.prototype.toString` of a class constructor returns the class's source text, from
  `class` to its closing `}` (`[[SourceText]]`), not `function A() { [native code] }`. Dependency
  injection libraries tell a class from a function with `/^class[\s{]/` on it (the Journals
  plugin: "Expected a Token, MultiToken, or class constructor; received function"). The parser
  sets the span on `ClassDeclaration`/`ClassExpression` (`with_linear_span`; the class tail now
  returns its closing brace's linear position) and `compile_class` gives it to the constructor's
  code. Methods, accessors, arrows and the rest already returned their source. And a function
  made by `new Function(…)` returns the source text the specification builds for it,
  `function anonymous(a,b\n) {\nreturn a\n}`, rather than `[native code]`.

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

- `boa_engine`, `builtins/array_buffer`: a data block the allocator refuses (`new
  Uint8Array(16 * 1024 * 1024)` under a job object's memory limit) throws V8's `RangeError:
  Array buffer allocation failed`, not `invalid layout Layout { size: …, align: 64 } while
  allocating data block` — the engine's layout is no business of the script's, and an
  embedder tells an exhausted memory limit by V8's words. A fixed-length buffer past
  `HostHooks::max_buffer_size` fails with the same words, as V8's does past its own limit.

- `boa_parser`, `parser/mod.rs`: `parse_function_body` and `parse_formal_parameters` — what
  `Function(...)` parses its parts with — fail on input left after what they parsed. Before,
  `new Function("}); f(); ({")` made a function whose body stopped at the first `}` and
  `new Function("a) { f() }; (function (", "")` one with that text as its parameters,
  where the specification parses each part alone so neither can close the function early.
  `vm.compileFunction` relies on it to wrap a body in its `contextExtensions`.

- `boa_engine`, `object/builtins/jsproxy.rs`: `JsProxy::from_target_and_handler` makes a proxy
  without a `Context`. `HostHooks::create_global_object` has the new realm's intrinsics but no
  context, and Verve's `node:vm` makes a context's global object a proxy there — its handler
  empty while the realm installs its builtins, given its traps once the realm exists — so the
  contextified object's properties are the context's globals, as Node's interceptors make them.

- `boa_engine`, `value/operations.rs`: the `int32` fast paths of `%` use `wrapping_rem`, so
  `(-2147483648 | 0) % -1` is `-0` instead of a panic in builds with overflow checks.

- `boa_engine`, `vm/jit/` (feature `jit`, off by default): a baseline JIT. Hot functions are
  compiled with Cranelift to native code that keeps the interpreter's frame exactly (the same
  `CallFrame`, registers in the VM stack), does numbers, moves, comparisons and branches inline,
  and runs everything else with the interpreter's own opcode handlers. It touches the VM in a
  few places: `JitSlot` in `CodeBlock`, `JitVm` in `Vm`, a pending flag set by
  `push_frame`/`pop_frame`/`handle_exception_at`/`IncrementLoopIteration` and polled by
  `run_loop`, and `JsValue::raw_bits`/`from_raw_bits` (with `JsValue` and the NaN-boxed inner
  value `repr(transparent)` on 64-bit). `Context::jit_options_mut`, `Context::jit_stats`,
  `BOA_JIT=0`, `BOA_JIT_THRESHOLD=<n>`. `vm::object_layout_checked()` answers whether property
  and element accesses compile inline on this thread — the layout they read is checked once per
  thread against a real object, at that thread's first compile or the first call here, whichever
  comes first, and the answer is kept — so an embedder can tell a JIT that runs its hot loops as
  compiled code with only helper property accesses from a fully inline one; without it the check
  is visible only inside this crate's own tests. Design and results: Verve's `docs/jit.md`.

- `boa_engine`, `object/jsobject.rs`: `JsObject::downgrade` and `WeakJsObject`, a reference to
  an object that does not keep it alive and is read without a call into JavaScript (unlike a
  `WeakRef`, it keeps nothing until the end of the job). A host that hands out one object per
  native thing — `verve-dom`'s one object per DOM node — keeps its objects this way, so a node a
  script let go of is collected and then freed.

- `boa_string`, `str.rs`: `JsStr::index_of` starts at `from_index` instead of skipping that many
  windows from the front. `Windows` implements `next` only, so `skip(n)` walked n windows one at a
  time and every search from a position cost O(position) — 40k-part `split` 2,297 ms, 200k parts
  still running after 120 s. A Latin-1 pair now compares byte slices and a UTF-16 pair `u16`
  slices, as subslices of the haystack; a needle and a haystack in different encodings still
  compare code unit by code unit. `split`, `indexOf`, `includes`, `replaceAll` and string-pattern
  `replace` are linear in the string now.

- `boa_engine`, `builtins/map/ordered_map.rs`, `builtins/set/ordered_set.rs`: `Map.prototype.delete`
  and `Set.prototype.delete` leave a tombstone instead of calling `shift_remove`, which moved
  every entry after the deleted one: deleting half the keys of 40k took 878 ms, and 300k did not
  finish. A tombstone is O(1) and the iterators skip it, so the order they see is unchanged; once
  the tombstones outnumber the entries that are left and no iterator holds the lock, they are
  dropped in one O(n) pass, amortized O(1) per deletion. `Set::delete_in_place` leaves the
  tombstone without compacting, for `Set.prototype.difference`, which walks the set by index as
  the specification writes it and would otherwise see the entries move underneath it.

  The set operations that walk `this` by index while they call the other set's `has` — the set-like
  `has` may delete from `this` — hold the same lock, otherwise a compaction in the middle of the
  walk skipped the value at the next index. `Set.prototype.isDisjointFrom`, `isSubsetOf` and
  `intersection` take it with `SetLock::new`, as `Set.prototype.forEach` does.

  The lock is what holds compaction back, so an iterator has to give it back. `next` took the lock
  out of the iterator at the end of its walk and dropped it as a plain value, which never
  finalizes, so every map or set that had been through a `for…of`, a spread, an `Array.from` or
  `entries()` stayed locked and never compacted; a pattern like `delete` then `set` with an
  occasional iteration grew `full_len` without bound. Both iterators now unlock where the walk
  ends, `Finalize` stays for one abandoned half-way, and a clone starts unlocked — the set
  operations copy a set that may be being iterated, and the copies carried its lock over.

- `boa_engine`, `builtins/map/mod.rs`, `builtins/set/mod.rs`: `Map::for_each_native` and
  `Set::for_each_native` — the Rust-side walks behind `JsMap`/`JsSet` and the `TryFromJs`
  conversions of `HashMap`/`BTreeMap` — step over a tombstone. Before the tombstones above they
  could not see one after the lock was released, and their index loops did not advance when
  `get_index` answered `None`, so the first tombstone spun forever. `OrderedSet::is_empty` asks
  `len()`, as `OrderedMap::is_empty` does, rather than counting tombstones as elements.

- `boa_string`, `lib.rs`: `JsString::ptr_eq` says whether two strings are one allocation rather
  than merely equal, which the regexp widening below uses: a string's contents never change, so the
  units derived from one allocation stay right for the next call on it.

- `boa_engine`, `builtins/regexp/mod.rs`: a Latin-1 subject reaches `regress` as its UTF-16 code
  units, widened once per scan instead of once per match. Widening the whole subject in every
  `exec` made a global `exec`, `matchAll`, `replace` or `split` quadratic in its length: 8k lines
  took 724 ms where 2k took 54 ms. The units of one allocation stay right for every later match on
  it, so `WIDENED` keeps the last subject and its units for the next `exec`, matched by
  `JsString::ptr_eq`. What a script can observe does not change: `lastIndex` is still read and
  written once per match, and an overridden `exec` is still called.

  The entry holds the subject — a slice of it pins the whole string it was cut from — and its copy
  at two bytes a unit, about three times the size of the last subject, so it is released when the
  scan that needed it ends. `ReleaseWidened` is a guard dropped on every way out of `exec`,
  `@@search` and `@@split`, `?` errors included, so each of these ends with nothing kept:

  - the failed match that ends a global scan, and the out-of-range `lastIndex` that ends one;
  - the one match of a search that is neither global nor sticky;
  - every `@@search` and `@@split`, whose result no next `exec` will want.

  Two cases still keep it until the next Latin-1 `exec`: a `test`, `exec` or `search` on a global
  or sticky pattern that matched — the scan has not ended, its next step is one `exec` away — and a
  `matchAll` iterator abandoned half-way, which has no point to release at. A sticky match that
  fails also keeps it: `@@split` runs the sticky splitter once per position of the subject.

- `boa_engine`, `builtins/regexp/mod.rs`: a match result's `indices` array is built only for a
  `d`-flagged pattern. `RegExpBuiltinExec` made it unconditionally — the array, its whole-match
  pair and that pair's own array — and dropped all three unless step 34 read them, so every match
  of every other pattern paid for a result nobody could observe. `MakeMatchIndicesIndexPairArray`
  is now step 34's own work (`make_match_indices`), and step 25's "List" stays a list of nothing.

  The result array is also made in one allocation: the whole match at 0 and the captures at
  `1..n` are a single `Array::create_array_from_list`, where `ArrayCreate` plus one
  `CreateDataPropertyOrThrow` per element made the same array in `n + 2` steps. Elements still
  sort before `index`, `input` and `groups` in `Reflect.ownKeys`, and `length` still comes from
  the number of elements, so what a script observes is unchanged; `groups` and `indices` keep
  being defined afterwards, in the order steps 32 and 34 name. `groups` is `undefined` when the
  pattern has no named group, and the `groupNames` object that only `indices` reads is no longer
  built for a match that has no `indices` — nor is `named_groups` collected into a `Vec` just to
  ask whether it is empty.

  `tokenize` 200 KB of source with a sticky tokenizer: 262 ms to 190 ms (median, 5 runs, a shared
  machine). A bare `exec` loop over one 8-code-unit subject, 100k times: 221–228 ms to
  142–145 ms; the same loop over `/(a)(b)/g`, which has two captures: 257–265 ms to 162–175 ms.
  A `d`-flagged pattern, which still builds `indices`, reads 229 ms either way — the saving is
  the array that no longer exists, not the matcher.

- `boa_engine`, `builtins/json/mod.rs`: `JSON.stringify` copies each member once. `SerializeJSONObject`
  and `SerializeJSONArray` collect their members as a `Vec<u16>` each — every quoted key, colon,
  separator and serialized value flattened into its own buffer — and then flatten all of them
  again into the result. A member's bytes therefore moved twice, and the first buffer was
  reallocated as it grew. `partial` now holds the pieces as `JsString`s and `concat_members`
  copies them, with the opening and closing text, into one allocation at the end; the separator
  is made once per container instead of once per member, and a member the serializer skips no
  longer has a comma reserved for it.

  `stringify` of 70,000 records (7,032,596 code units): 707–746 ms to 408–470 ms, five interleaved
  runs each, load 22–32%. `JSON.parse` of the same text, which this does not touch, read
  1865–2033 ms before and 1960–2034 ms after. The suite's `json` test, which parses as well as
  stringifies, went from 3444–3591 ms to 2849–3019 ms (JIT column, 5 runs, load 21–36%).

- `boa_engine`, `code_cache.rs`, `script.rs`: a classic script can be written down and read back, as
  a module could already be. `Script::code_cache` compiles the script with every function it
  declares and answers its bytes; `Script::from_code_cache` reads them into a script that runs as
  the parsed one, its own code block made now and the rest when their functions are first called.
  The bytes say which of the two they hold (format 3), so a module's are not read as a script's.
  A script is written down only if it declares nothing in the global scope: parsing a script is
  what makes its global declarations, and code read back in another run would have to make them
  again. A block-level function declaration in a non-strict script makes an unconfigurable global
  var binding the same way, while the script compiles (Annex B.3.2.2), so such a script is not
  written down either — `Script::code_cache` answers `None` for it. Node's CommonJS wrapper
  declares nothing, so a CommonJS module is written down, which is what Verve does with it.
  Reading checks the same things as for a module (`read_header`, `read_scopes`, `read_codes` are
  shared), and `BOA_CODE_CACHE_ROUNDTRIP=1` sends scripts parsed from a file through it too: one
  evaluated from a string, which no cache keeps, is used as parsed — writing it would compile it,
  and every function it declares, at the parse rather than at the first call.

- `boa_engine`, `builtins/typed_array/object.rs`: `CanonicalNumericIndexString` rejects a key by its
  first code unit before parsing it. Reading or writing any named property of a typed array —
  `ta.length`, `ta.byteLength`, `ta.byteOffset`, `ta.buffer`, and every `ta.foo` — reached the
  integer-indexed exotic `[[Get]]`, which ran the whole conversion for a key that could never be an
  index: `ToNumber(key)` and then a `JsString::from(n)` allocation to compare back. A key that is
  not a canonical numeric index string starts with `0`–`9`, `-`, `I` or `N` and with nothing else,
  so the other keys are refused without parsing or allocating (`may_be_canonical_numeric_index`).
  The check only widens what is let through to the parser: `-0`, `0`, `-1`, `1.5`, `1e+21`,
  `Infinity`, `-Infinity` and `NaN` still pass and still resolve exactly as before. Measured on the
  same round (interpreter, 1M iterations, median of five processes): `ta.length` 386.9 → 201.5 ms,
  `ta.byteOffset` 374.3 → 212.7 ms, a missing string key 389.5 → 211.6 ms, and the suite's `typed`
  (six 1M loops reading `f.length` per iteration) 3589.5 → 2350.1 ms.

- `boa_engine`, `vm/opcode/get/property.rs`: `GetLengthProperty` reads a typed array's `length` from
  its internal slots, the way it already did for arrays and strings. The opcode's array fast path
  checked `is_array()`, which a typed array is not, so `f.length` fell through to the inline cache,
  the integer-indexed exotic `[[Get]]`, `CanonicalNumericIndexString`, the prototype walk to
  `%TypedArray%.prototype`, and an accessor call that itself had to re-validate the receiver. The
  shortcut is taken only when the read cannot be observed: the receiver is a typed array, it has no
  own `length`, every prototype between it and `%TypedArray%.prototype` is an ordinary object
  without `length`, and the accessor that remains has the intrinsic getter (compared as a function
  pointer, `NativeFunction::as_fn_ptr`, the first use of that new accessor). The getter only reads
  internal slots, so skipping it is unobservable; a detached buffer and a resized length-tracking
  view are still computed from the current buffer length, exactly as the getter does. A subclass
  getter, a shadowing data property on `Uint8Array.prototype`, an own `length`, a rewired prototype
  and a replaced accessor all fall back to the spec path. Measured on the same round (median of five
  interleaved processes, load 19–38%): `ta.length` over 1M iterations 178.7 → 73.7 ms compiled
  (175.3 → 72.0 best) and 238.6 → 102.8 ms in the interpreter (205.5 → 95.0 best); the suite's
  `typed` 2131.8 → 1391.2 ms compiled, 2907.7 → 1845.1 ms interpreted.

- `boa_engine`, `vm/opcode/{get,set}/property.rs`: reading and writing a typed array element goes
  to the buffer. `GetPropertyByValue`/`SetPropertyByValue` had a dense-element fast path for arrays
  only, so `f[i]` built a `PropertyKey`, ran the integer-indexed exotic `[[Get]]`/`[[Set]]` with its
  `CanonicalNumericIndexString` check, and went through `validate_index` per element. A key that
  already parsed as an index needs no canonicalisation, and no receiver can observe an
  integer-indexed read, so the read takes the element straight from the buffer; the write does the
  same only when the receiver is the array itself (any other receiver, as in `super[0] = v`, gets
  `OrdinarySet`, which defines on the receiver) and the value is already a `Number`, or a `BigInt` for the BigInt arrays — anything
  else runs `ToNumber`/`ToBigInt`, which can call a user `valueOf` that detaches the buffer, and the
  spec converts before it checks, so those keep the spec path (which also throws for a `Number`
  written into a BigInt array and the other way around). A detached buffer, an out-of-range index, a
  detached-in-the-middle length-tracking view and a `SharedArrayBuffer` view all keep their existing
  behaviour: the reads and writes go through the same `BufferRef` slice, so a shared view stays on
  its atomic path. Measured on the same round (median of five interleaved processes): one element
  read 1M times 53.7 → 39.8 ms compiled (load 38–48%), 80.5 → 65.6 ms interpreted (load 13–18%);
  the suite's `typed` 1359.1 → 1060.2 ms compiled, 1559.8 → 1309.5 ms interpreted.

- `boa_engine`, `vm/opcode/define/own_property.rs`: defining a named property of an object literal
  inserts it directly. `DefineOwnPropertyByName`, which `{ i, s: 'k' + i }` emits once per property,
  built a four-field descriptor and called `[[DefineOwnProperty]]`, which fetched the current
  descriptor, asked for extensibility, then inserted. For an ordinary extensible object that lacks
  the key, `ValidateAndApplyPropertyDescriptor` inserts the descriptor unchanged and returns `true`
  without running any user code, so the descriptor and the two lookups can be skipped. The shortcut
  applies only to an ordinary object whose shape lacks the key; an array, an arguments object, a
  module namespace, a proxy and a typed array have their own `[[DefineOwnProperty]]`, and a
  non-extensible object or an existing key still needs the full comparison. Measured on the same
  round (median of five interleaved processes): the suite's `alloc` 295.0 → 278.9 ms compiled and
  344.5 → 335.0 ms interpreted (load 22–46%); `vdom` 82.2 → 79.6 ms compiled, 97.8 → 88.7 ms
  interpreted; `oo` does not touch this path, and its spread stayed inside the round's noise.

- `boa_engine`, `vm/opcode/set/property.rs`: assigning a new property inserts it directly.
  `this.x = x` in a constructor ran the full `OrdinarySet`: `[[GetOwnProperty]]` on the receiver,
  then a walk up the prototype chain looking for a setter or a non-writable data property, then
  `CreateDataProperty`. When the object is ordinary and extensible, the key is absent on the whole
  chain, every prototype is ordinary, and the receiver is the object itself, that walk can only end
  at `CreateDataProperty` with the default attributes, so the walk is replaced by one insert. A
  non-writable data property or a setter on the chain still fails or calls, a proxy still
  intercepts, a non-extensible receiver still fails, and `Reflect.set` with a separate receiver
  still defines on the receiver. Ordinary objects are recognised by the internal methods the
  shortcut depends on being the ordinary ones (`std::ptr::fn_addr_eq` on the four method pointers,
  `[[Set]]` included, since an override of it on the object or a prototype changes the outcome),
  not on the address of a `const`, which the compiler may materialise twice). Measured on the same
  round (median of five interleaved processes, load 27–37%): 200,000 constructor calls storing three
  fields 304.1 → 232.4 ms compiled, 320.0 → 253.6 ms interpreted; storing one field on a fresh
  object 68.4 → 46.7 ms compiled, 73.7 → 60.3 ms interpreted.

- `boa_engine`, `builtins/{map,set}/ordered_{map,set}.rs`: `clear()` no longer drops the entries out
  from under a live iterator. Both `Map.prototype.clear` and `Set.prototype.clear` emptied the
  backing store and freed its capacity, which also reset the length an iterator counts against, so
  an iterator that was part-way through ended at once instead of going on to the entries added
  after the call; `new Map([['a',1]])` with an iterator, `clear()`, `set('z',9)` then `next()`
  reported `done: true` where the specification yields `['z', 9]`. The specification only empties
  each entry's key and value, so a locked store now keeps its length and replaces every entry with
  a tombstone; an unlocked one still clears outright, which keeps `clear()` on a map nobody
  iterates O(1) and frees the capacity as before.

- `boa_engine`, `builtins/typed_array/builtin.rs`: `%TypedArray%.prototype.sort` reads a comparator's
  result by its sign, and `CompareTypedArrayElements` returned `Greater` for any positive number and
  `Less` for everything else, `+0` and `-0` included. A comparator that says "equal" therefore said
  "greater" on every pair, so an equal group came back reversed and a pre-sorted input was reordered:
  `new Uint8Array([0,1,2,3]).sort(() => 0)` gave `3,2,1,0` where the specification keeps the input,
  and test262's `builtins/TypedArray/prototype/sort/stability` failed. Both zeroes now mean
  `Equal`; only a number above or below zero orders the pair. NaN was already `Equal`.

- `boa_engine`, `builtins/array/mod.rs`, `vm/jit/helpers.rs`: `Array::plain_dense_array` is
  `pub(crate)`, and the JIT's `SetPropertyByValue` helper uses it to append: `a[i] = x` with `i`
  the count of a plain dense array's elements (and `length` at least that) pushes the element
  and grows `length` to cover it, as `[[Set]]` and `Array`'s `[[DefineOwnProperty]]` do there.
  Any other array, index or `length` takes the interpreter's `[[Set]]`.

- `boa_engine`, `vm/jit/compiler/arith.rs` (feature `jit`): `/` is compiled inline as `div_fast`
  computes it: an exact `int32` quotient is an `int32`, every other quotient of numbers an `f64`.
  A dividend of `0` takes the interpreter's handler, whose `0 / -5` is `0` where JavaScript's is
  `-0`.

- `boa_engine`, `builtins/function/mod.rs`, `vm/mod.rs`, `vm/jit/call.rs`: `function_call` is split
  in two, the checks and the clones of the function's parts, and `enter_ordinary_function`, which
  pushes the frame and sets `this` and the environments; the interpreter's behaviour is
  unchanged. `Context::check_runtime_limits` is split into the interrupt check and
  `runtime_limits_exceeded`, which only reads. Compiled code calls a compiled ordinary function
  through `enter_ordinary_function` directly (`jit_call`), when no limit is about to be passed and
  no interrupt is pending; every other callee goes through `jit_step`.

- `boa_engine`, `vm/jit/` (feature `jit`): `PushFromRegister`, `PopIntoRegister`, `GetArgument` and
  `CheckReturn` are compiled inline, reading and writing the VM stack's `Vec` (pointer, length and
  capacity found by looking at a real one, as the object layout is) and the frame's `fp`,
  `argument_count` and `CONSTRUCT` flag; a full stack, a pointer that is not an object and a
  constructor's frame take the fast helper or the interpreter's handler. `JitState` carries the
  stack's address. No interpreter code changes.

- `boa_engine`, `vm/jit/helpers.rs` (feature `jit`): the fast helpers of `GetLengthProperty`,
  `GetPropertyByValue` and `SetPropertyByValue` handle typed arrays by calling the interpreter's
  own shortcuts (`typed_array_length_shortcut`, `typed_array_element_get_shortcut`,
  `typed_array_element_set_shortcut`), so the guards live in one place. A store is the
  helper's only when its receiver is the array itself, which compiled code decides at compile
  time (the instruction's receiver register is the object's).

## Keeping it

Rebase onto the next release tag when Su moves to it, and drop what upstream has fixed.

- `boa_engine`, `builtins/json/mod.rs`, `builtins/json/parse.rs`: `JSON.parse` reads its text once. It
  used to be validated by `serde_json`, then parsed again as a script by `boa_parser`, compiled to
  bytecode and run on the VM — three readings of the same bytes, and a `String` copy of them, to
  build the values the text already spells out. Without a reviver the reader in `parse.rs` now
  builds them directly from the code units: an object is an `JsObject` with each member created as
  it is read, an array is one `Array::create_array_from_list`, and a string without an escape is a
  slice of the text rather than a fresh buffer. Containers are kept on an explicit `Vec`, so a
  deeply nested text costs heap instead of Rust stack — the script parser gave up at 128 levels,
  where Node answers as deep as memory allows. A reviver still builds its values through the script parser, since `context.source`
  needs the text each primitive came from, but the text is validated by the same reader first, so
  both paths throw the same messages.

  Because the reader is now the one validating, its `SyntaxError` messages are V8's, with the
  position and the line and column V8 names: `Unexpected token '}', "[1,]" is not valid JSON`,
  `Expected ',' or ']' after array element in JSON at position 4 (line 1 column 5)`,
  `Unexpected end of JSON input`, and a text that is only `undefined`, `NaN` or `Infinity` is quoted
  by itself. A whole number that fits an `i32` is an integer value, as a script's number literal
  is, and `-0` stays a float. Lone surrogates, both escaped and raw, are code units like any other
  rather than an error — `String::from_utf16` rejected them before.

  `JSON.parse` of 70,000 records (7,032,596 code units): 1536–2034 ms to 453–530 ms, five interleaved
  runs each, load 19–70%. The suite's `json` test, whose parse phase that is, went from 2926–3519 ms
  to 1315–1428 ms (JIT column, 5 runs, load 23–41%), which is 8.0× Node rather than 21×.

# Starting fast: lazy compilation and the code cache

Every plugin host and contained-plugin process Rebon starts loads the same modules again: for the
DSH composition, compose-runtime's resolver and 29 payload modules, about 430 KB of JavaScript with
a 49 KB bundled Cordis. Node loaded them in 46 ms; Verve took 120. This note is what was measured,
what was built (lazy compilation of function bodies, smaller syntax trees, and an on-disk cache of
compiled modules), and how to use it.

## Where the time went

Release `verve`, Windows x86-64, `startup.mjs` (load the resolver, then import every payload module;
the time printed is from the script's start to the last import). Counters were added for the
measurement only (they are not in the tree):

| Part | Time |
|---|---|
| Parsing (lexer about 20 ms of it) | 46–50 ms |
| Scope analysis | 7–10 ms |
| Bytecompiling (modules and every function in them) | 15–18 ms |
| Running the modules' top-level code | 7–9 ms |
| Load hooks (`module.registerHooks`), reading 35 files, converting sources | 17–25 ms |
| Resolution | 4–8 ms |

So the cost was the front end: parse, analysis and compile together were two thirds of the time.
Of the about 1,400 functions and module bodies in the composition, **76 functions run while it
loads**: Boa compiled them all.
Opening a file costs about half a millisecond on this machine (a scanner looks at each), which
matters for anything that reads one file per module.

The parser is the largest single cost and is not changed here beyond its tree's size (below):
lazy parsing would need a pre-parser that finds the free variables of the functions it skips, so
that scope analysis can still decide what escapes — a rewrite of Boa's parser and analyser. The
code cache removes parsing from every start but the first, which is what a plugin host needs.

## Lazy compilation

`Context::set_lazy_compile(true)` (Boa; off by default, `BOA_LAZY_COMPILE=1`) compiles a function's
body when it is first called or constructed. Verve turns it on for every runtime
(`Runtime::set_lazy_compile`, `VERVE_LAZY_COMPILE=0` to turn it off).

- **The stub.** `FunctionCompiler::compile` gives a lazy function a `CodeBlock` with no bytecode but
  with everything read of a function before it runs: name, `length`, the flags function objects are
  made with (strict, async, generator, has-prototype, and the class flags), this mode, parameter
  count and source text. `CodeBlock::lazy` holds a `LazyCode`: the pending function (its
  `FunctionCompiler`, parameters, body, the scopes it was declared in, whether it contains a direct
  `eval`) until it is compiled, then the compiled `CodeBlock`. In debug builds the compiled code is
  checked to agree with what the stub said.
- **The first call.** `function_call` and `function_construct` see `is_lazy()`, compile (or take
  what another closure of the same function compiled), and store the compiled code in the function
  object, so its later calls go straight to it; `create_function_object*` give new closures the
  compiled code once there is one. Frames, generators, the shadow stack and the JIT only ever see
  compiled code; `Error.captureStackTrace` compares the compiled code's identity.
- **The syntax tree.** `FunctionBody` holds its statements behind an `Rc`, so a stub keeps its body
  without copying it out of the tree; the rest of the module's tree is dropped after linking, as
  before. To keep that memory small, the large variants of `Expression` and `Statement` are boxed:
  an `Expression` went from 168 to 56 bytes and a `Statement` from 416 to 80, which also made
  parsing about 13% faster.
- **Exact semantics.** The body is compiled from the same tree with the same scopes, so the code is
  the code an eager compile makes. Early errors are still reported at parse time. Two things could
  differ, and are handled:
  - a sloppy direct `eval` can add `var` bindings to the scopes the nested functions resolve
    against, and a function compiled after that `eval` ran would see them in every call. So inside
    a function that contains a direct `eval`, nested functions are compiled at once, and any code
    that emits a direct `eval` compiles its stubs eagerly before it can run (`ByteCompiler::finish`);
  - a function compiled later finds a global `let` that a script declared since, as the
    specification has it; one compiled with its script looks the name up on the global object,
    as eager Boa always has. So **eager and lazy differ** for a function that reads a global
    `let` declared by a later script, and the code cache, which compiles every function when it
    writes a module down, sides with eager: such a function in a cached module misses the later
    `let`. The global declarative environment is resized before lazily compiled code runs, so
    the index it was compiled with exists.
  - a script's global lexical declarations are only found once its GlobalDeclarationInstantiation
    has run. Parsing a script makes them (its own code is compiled against them), but they are
    hidden from every other lookup (`Scope::withdraw_bindings`) until the script runs, and hidden
    again if instantiating them throws; a script parsed and never run, or one whose
    `let undefined` fails, declares nothing. Before, they were found from the moment the script
    was parsed, by any code compiled afterwards (eager Boa had that bug for later scripts too);
    two scripts parsed with the same `let` now fail when the second runs, not when it parses.
- **Interners.** A `Sym` only means something to the interner that made it, and a function's tree
  is its context's. `Interner` now has an identity; a stub records its own, and a first call from
  another context throws a `TypeError` rather than compile with the wrong interner. This is why
  Boa's default stays eager. Verve has one context per runtime.

## The code cache

### Boa: writing a module down and reading it back

`Module::code_cache(&self, context) -> JsResult<Option<Vec<u8>>>` compiles an unlinked module with
every function it declares and writes down what linking it needs: import and export entries, its
scopes (`Scope::parts`/`Scope::from_parts`), its code blocks (bytecode, constants, bindings,
handlers, inline-cache names, source maps, spans), and its function declarations.
`Module::from_code_cache(bytes, source, path, realm, context)` reads that back into a module that
links and runs as the parsed one, with the same source positions, `toString` and stack traces.
It checks every code block at once but makes only the module's own: each function gets a stub,
and its code block is made from its bytes when it is first called (`LazyCode` holds either a
syntax tree or a cached module's record). Of the composition's 1,433 code blocks, 107 are made
while it loads.
Compiling a module was split out of `InitializeEnvironment` for this; import resolution is
unchanged.

Reading never trusts the bytes to be in bounds and never panics:

- the header: format, Boa's `code_cache::fingerprint()` (version, opcode table, features), the
  length and digest of the source text, and a digest of the realm's global scope — code finds a
  name declared there by index, so it is only read into a realm whose global scope binds the same
  names (and each such binding is checked again by name and index);
- every count, index, tag, string, span and scope (binding flags, names bound once, indices within
  the scope), with code blocks nested in exactly one parent;
- every module import and re-export, against the modules the module requests;
- every code block's bytecode, through `Bytecode::check_instruction_in` (generated with the
  opcode table): each instruction whole and none of the reserved opcodes, each register below the
  frame's register count (the interpreter reads registers unchecked) including those held as
  plain numbers (`JumpTable`, `TemplateCreate`), each jump and handler at the start of an
  instruction, the code ending with a return; each constant, binding and inline-cache index an
  instruction holds within its table and of the kind its handler reads; the registers async
  frames use implicitly; the global tables pointing at constants of the right kind; source maps
  in order.

What is not proved is what running the instructions does with the stack and the environments: code
that pops more than it pushed, or names a binding its environments lack, is stopped by Boa's own
assertions — a panic, not undefined behaviour. Well-formed code is run as written. So the bytes
must come from a writer the embedder trusts — as it trusts the source text — and an embedder that
keeps them on disk must authenticate them.

`BOA_CODE_CACHE_ROUNDTRIP=1` sends every parsed module through the cache before it is used: a whole
suite then runs code read back from it.

### Verve: the pack on disk

`Runtime::set_code_cache_dir_with_secret(dir, secret)` keeps the compiled code of the ES modules a
runtime loads, signed with the embedder's secret; `set_code_cache_dir_with_user_secret(dir)` signs
with the user's own (`verve::code_cache_user_secret`), which is what `verve run --code-cache=<dir>`
and `VERVE_CODE_CACHE=<dir>` do. `disable_code_cache` stops. Off by default.

- **One pack per build**, `<dir>/<build>.pack`: a header, an index (key, offset, length, tag per
  entry), then the code. A run opens it once, reads the index, and reads an entry when its module
  loads; nothing else stays in memory. A file larger than a pack can be (64 MB), or an index
  claiming more entries than fit, is not read. Writing merges what this run compiled with what is
  on disk (another process may have written since), newest first, then what this run read (from
  the pack it opened and checked), then the rest, keeping only entries whose tags verify, capped at
  64 MB; it goes to a temporary file renamed into place, when a burst of loading settles (100 ms
  after the first new entry, at the next round of jobs), at exit, when the runtime is dropped, or
  on `Runtime::flush_code_cache`.
- **The key** of an entry is a SHA-256 of the build's fingerprint — Boa's fingerprint, Verve's
  version, the executable's path, size and modification time — of the realm's global scope
  (`boa_engine::code_cache::global_scope_fingerprint`), and of the whole source text the loader
  produced (after any load hooks). Any rebuild starts a new pack; old ones are left for the
  embedder to delete. Hosts with different global scopes keep entries of their own.
- **The tag** is an HMAC-SHA256 of the key and the code under a 32-byte secret that is never in
  the directory: the embedder's, or the user's own — 32 random bytes in
  `%LOCALAPPDATA%\Verve\code-cache-secret` (Windows) or `$XDG_CONFIG_HOME/verve/code-cache-secret`
  (else `~/.config/verve/…`), made the first time, written and synced beside the final name and
  linked into place only if none is there, so processes starting together agree on one. A secret
  file that is not 32 bytes leaves the run without a cache, with a warning. The tag is checked in
  constant time before Boa reads anything.
- **The directory** (and the user's secret folder) is made private when it is made: a protected
  DACL granting its owner alone on Windows, `0700` elsewhere. On Unix a directory or secret file
  not owned by the effective user, writable by its group or others, or a link, is refused, and the
  pack and secret are opened with `O_NOFOLLOW`. **JavaScript the runtime runs can neither read nor
  write the cache directory or the user's secret**, whatever `--allow-read`/`--allow-write` grant
  (`ERR_ACCESS_DENIED`).
- Anything missing, not authentic, or refused by Boa is parsed and compiled as without the cache,
  and its fresh entry replaces the old one. `Runtime::code_cache_stats` counts hits, misses,
  refusals and stores.

### Trust model

A checksum would only catch accidents; the HMAC means an entry is only read if it was written by
someone holding the secret, and the secret is never kept where the entries are. So:

- **whoever can only write the cache directory can make modules compile again, nothing more**:
  the key of an entry is made of public data, but its tag is not, and a key file planted in the
  directory is not read (an earlier version read `<dir>/key`, which let write access alone forge
  entries; that is gone);
- **whoever can read the secret can make every module the runtime loads run code of their
  choosing**, with that runtime's permissions. The user's secret is in a folder only they can
  reach; an embedder's must be kept as carefully. Anyone who can already change the user's files
  can change the plugins themselves, so the user's own account is the boundary;
- code less trusted than the modules being cached must not reach the secret or the directory. The
  permission model denies JavaScript both; native code in the same process is not confined. Rebon's
  Code Mode runs untrusted programs: it must not use a cache;
- what a forged entry could do is bounded by the secret, not by Boa's checks: those make damaged or
  foreign bytes fail safely, not malicious ones written with the key.

## Results

Release `verve` (`--features jit`), Windows 10, x86-64, Node 22; `startup.mjs` on Rebon's real
payload; best of 9, with the median peak working set. The machine was shared with other builds, so
differences under about 10% are noise.

| Configuration | Process | Peak memory | Loading (in-script) |
|---|---|---|---|
| Node 22 | 152 ms | 38.0 MB | 44 ms |
| Verve before (`main` at a95e3784, eager) | 198 ms | 22.3 MB | 138 ms |
| Verve now, eager (`VERVE_LAZY_COMPILE=0`) | 186 ms | 20.8 MB | 115 ms |
| Verve now, lazy (the default) | 167 ms | 21.9 MB | 105 ms |
| Verve, code cache cold (compiles everything, writes the pack) | 212 ms | 22.7 MB | 123 ms |
| Verve, code cache warm | **105 ms** | **18.0 MB** | **48 ms** |

On a quiet machine, with counters, the warm load took 25–29 ms in-script: reading the pack's
entries about 2 ms, reading the code back about 4 ms (107 of 1,433 code blocks made), running the
modules about 10 ms, and the load hooks and reading the 29 source files most of the rest. A cold
lazy load is still about two and a half times Node's: parsing (about 40 ms) is most of it.

The warm cache brings loading to Node's level, and the whole process (Verve starts in about 47 ms,
Node in about 106) well under it, at less memory than eager compilation.

## Tests

All on the final tree, rebased on `main` at 65fdd840 (JIT phase 2 and the inline-cache fix):

| Suite | Modes | Result |
|---|---|---|
| Boa (`boa_engine`, `boa_parser`, `boa_ast`, `boa_interner`, insta bytecode) | eager, lazy, lazy with every module through the cache (`BOA_CODE_CACHE_ROUNDTRIP=1`); the same three with the JIT forced (`BOA_JIT_THRESHOLD=0`); `annex-b` eager and lazy | all pass (1,739 tests; 1,776 with the JIT; 1,740 with `annex-b`) |
| Verve (all 328 tests) | lazy, eager, cache cold, cache warm (`code-cache-env`, `VERVE_CODE_CACHE`), JIT forced lazy and eager (`VERVE_JIT_THRESHOLD=0 BOA_JIT_THRESHOLD=0`) | all pass |
| test262 (`language`, `built-ins`, `annexB`, `intl402`, `harness`; 52,095 results) | eager, lazy, lazy through the cache, JIT forced eager, JIT forced lazy through the cache, compared test by test with eager | 0 differences but `Atomics.notify` (`notify-two`, `notify-all-on-loc`, bigint `notify-all-on-loc`), which flip both ways between runs of the same configuration under load and pass in every mode run alone |

Focused tests: `boa/core/engine/src/tests/lazy.rs` (early errors in uncalled functions,
`toString`/`name`/`length`/`prototype`, closures and TDZ, default parameters and `arguments`,
classes with private members and `super`, generators and async functions, direct `eval` in
functions, arrows and parameters, `with`, annex B, stack traces and `captureStackTrace`, template
objects and regexps, closures made before the first call, recursion, `this`, a first call from
another context, the JIT); `boa/core/engine/src/tests/code_cache.rs` (round trips eager and lazy
with imports, private fields and stack traces; refusal for another source and another global
scope; every byte flipped three ways and every truncation, never a panic; direct `eval`; bodies
read back when first called); `tests/code_cache.rs` (round trips, a changed source, damaged and
cut packs, another secret, a forged entry, `verve run --code-cache` and `VERVE_CODE_CACHE`).

## Using it from Rebon

- A plugin host's guest: `runtime.set_code_cache_dir_with_secret(&dir, secret)?` before loading
  anything, with `dir` a per-user directory under Rebon's data directory (for example
  `%LOCALAPPDATA%\Rebon\code-cache` or `~/.cache/rebon/code-cache`; Verve makes it private) and
  `secret` 32 random bytes Rebon keeps in its own private storage, not in `dir`. Or
  `set_code_cache_dir_with_user_secret(&dir)` to share the user's Verve secret. An error is a
  reason to run without the cache, not to fail. Call `runtime.flush_code_cache()` before a host
  is shut down without the runtime being dropped.
- Contained-plugin processes started as `verve run`: pass `--code-cache=<dir>`; the user's secret
  is used.
- Code Mode: no cache; lazy compilation is on by default and needs nothing.
- `code_cache_stats()` is there to log how well the cache does.

## Notes for other work on the engine

- `CodeBlock` has one more field, `lazy: Option<Box<LazyCode>>`. A function object's code may be a
  stub with no bytecode: code that reads a function's `CodeBlock` to run it (JIT-to-JIT calls, for
  one) must call through `function_call`, or take `CodeBlock::compiled`, when `is_lazy()`.
- `Bytecode::check_instruction_in` is generated by `generate_opcodes!` from each operand's
  `Argument` type, which gained a `check` method; a new operand type needs one. The verifier
  classifies index operands (and plain `u32`s) by instruction and field name
  (`code_cache::operand_use`), and refuses code with one it does not know: an instruction with a
  new index or `u32` field needs an entry there, or modules using it are compiled rather than
  read back.

## What is left

- **Lazy parsing**, as above: the largest cost of a cold start, and a parser rewrite.
- **CommonJS scripts** are not cached: a script's global declarations are made when it is
  compiled, which a cached one would have to replay.
- **Faster reading back**: checking every record still takes about 2 ms of the 4 ms reading
  the composition back costs; scopes could be made on demand too.

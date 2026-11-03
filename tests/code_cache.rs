//! The code cache: modules read back from it run as parsed ones, and a pack that is damaged,
//! forged, signed with another secret or written for another source is never read.

use std::path::{Path, PathBuf};
use std::process::Command;

use boa_engine::builtins::promise::PromiseState;
use verve::{CodeCacheStats, Runtime};

const VERVE: &str = env!("CARGO_BIN_EXE_verve");

/// The secret these tests sign entries with: an embedder's, kept outside the directory.
const SECRET: [u8; 32] = [0x5e; 32];

const MAIN: &str = r#"
import { Point, add, label } from './lib.mjs';
const log = [];
function* gen() { yield* [1, 2, 3]; }
async function twice(x) { await null; return x * 2; }
log.push([...gen()].join(''), add(40, 2), new Point(3, 4).norm(), label`a${1}b`);
log.push(add.toString(), Point.name, typeof Point.prototype.norm);
try { null.x; } catch (e) { log.push(e.message); }
function thrower() { throw new Error('boom'); }
try { thrower(); } catch (e) { log.push(e.stack.split('\n')[1].trim().replace(/\(.*[\\/]/, '(')); }
log.push(await twice(21), 2n ** 70n, /b+/.exec('abbbc')[0]);
globalThis.result = log.join(' | ');
"#;

const LIB: &str = r#"
export function add(a, b = 0) { return a + b; }
export class Point {
    #x; #y;
    constructor(x, y) { this.#x = x; this.#y = y; }
    norm() { return Math.hypot(this.#x, this.#y); }
}
export const label = (strings, ...values) => strings.raw.join('_') + values.join('');
"#;

/// A folder of its own for one test: the modules, and a cache directory in it.
struct Fixture {
    root: PathBuf,
}

impl Fixture {
    fn new(name: &str) -> Self {
        let root =
            std::env::temp_dir().join(format!("verve-code-cache-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("main.mjs"), MAIN).unwrap();
        std::fs::write(root.join("lib.mjs"), LIB).unwrap();
        Self { root }
    }

    fn cache(&self) -> PathBuf {
        self.root.join("cache")
    }

    fn pack(&self) -> PathBuf {
        std::fs::read_dir(self.cache())
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .find(|path| path.extension().is_some_and(|ext| ext == "pack"))
            .expect("a pack was written")
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

/// Runs `main.mjs` in a fresh runtime set up by `setup`; answers its result and what the
/// cache did.
fn run(fixture: &Fixture, setup: impl FnOnce(&mut Runtime)) -> (String, Option<CodeCacheStats>) {
    let mut runtime = Runtime::new();
    setup(&mut runtime);
    let main = fixture.root.join("main.mjs");
    let promise = runtime.load_esm(&main, MAIN).unwrap();
    for _ in 0..100 {
        if !matches!(promise.state(), PromiseState::Pending) {
            break;
        }
        runtime.run_microtasks();
        runtime.run_due();
    }
    assert!(
        matches!(promise.state(), PromiseState::Fulfilled(_)),
        "{:?} {:?}",
        promise.state(),
        runtime.take_console()
    );
    let result = verve::print(&runtime.eval("globalThis.result").unwrap());
    let stats = runtime.code_cache_stats();
    runtime.flush_code_cache();
    (result, stats)
}

fn uncached(fixture: &Fixture) -> String {
    run(fixture, |_| {}).0
}

fn cached(fixture: &Fixture) -> (String, CodeCacheStats) {
    let dir = fixture.cache();
    let (result, stats) = run(fixture, |runtime| {
        runtime
            .set_code_cache_dir_with_secret(&dir, SECRET)
            .unwrap();
    });
    (result, stats.unwrap())
}

#[test]
fn modules_read_back_run_as_parsed_ones() {
    let fixture = Fixture::new("roundtrip");
    let expected = uncached(&fixture);
    assert!(expected.contains("123 | 42 | 5 | a_b1"), "{expected}");

    let (cold, stats) = cached(&fixture);
    assert_eq!(cold, expected);
    assert_eq!(
        (stats.hits, stats.misses, stats.stored),
        (0, 2, 2),
        "{stats:?}"
    );

    let (warm, stats) = cached(&fixture);
    assert_eq!(warm, expected);
    assert_eq!(
        (stats.hits, stats.misses, stats.refused),
        (2, 0, 0),
        "{stats:?}"
    );

    // Eager compilation reads back the same entries and runs the same.
    let dir = fixture.cache();
    let (eager, stats) = run(&fixture, |runtime| {
        runtime.set_lazy_compile(false);
        runtime
            .set_code_cache_dir_with_secret(&dir, SECRET)
            .unwrap();
    });
    assert_eq!(eager, expected);
    assert_eq!(stats.unwrap().hits, 2);
}

#[test]
fn a_changed_source_is_compiled_again() {
    let fixture = Fixture::new("changed");
    let (_, stats) = cached(&fixture);
    assert_eq!(stats.stored, 2);
    std::fs::write(
        fixture.root.join("lib.mjs"),
        LIB.replace("return a + b;", "return a + b + 0;"),
    )
    .unwrap();
    let (result, stats) = cached(&fixture);
    assert!(result.contains(" | 42 | "), "{result}");
    assert_eq!(
        (stats.hits, stats.misses, stats.stored),
        (1, 1, 1),
        "{stats:?}"
    );
    // The new entry is read back next time, and the old one is still there for the old source.
    let (_, stats) = cached(&fixture);
    assert_eq!(stats.hits, 2, "{stats:?}");
}

#[test]
fn damaged_packs_are_never_read() {
    let fixture = Fixture::new("damaged");
    let expected = uncached(&fixture);
    cached(&fixture);
    let pack = fixture.pack();
    let good = std::fs::read(&pack).unwrap();
    // Flipped bits anywhere, and the pack cut short: every module still runs, from its source
    // wherever its entry was touched, and the pack is whole again after.
    for at in [
        8,
        40,
        60,
        good.len() / 3,
        good.len() / 2,
        good.len() - 40,
        good.len() - 1,
    ] {
        let mut damaged = good.clone();
        damaged[at] ^= 0x5a;
        std::fs::write(&pack, &damaged).unwrap();
        let (result, stats) = cached(&fixture);
        assert_eq!(result, expected, "damage at {at}");
        assert_eq!(stats.hits + stats.misses + stats.refused, 2, "{stats:?}");
        assert!(stats.hits < 2, "damage at {at} is noticed: {stats:?}");
        let (_, stats) = cached(&fixture);
        assert_eq!(
            stats.hits, 2,
            "the pack is mended after damage at {at}: {stats:?}"
        );
    }
    for len in [0, 7, 8, 50, good.len() / 2, good.len() - 1] {
        std::fs::write(&pack, &good[..len]).unwrap();
        let (result, stats) = cached(&fixture);
        assert_eq!(result, expected, "cut to {len}");
        assert!(stats.hits < 2, "{stats:?}");
    }
}

#[test]
fn entries_signed_with_another_secret_are_refused() {
    let fixture = Fixture::new("secret");
    let expected = uncached(&fixture);
    let dir = fixture.cache();
    let with_secret = |secret: [u8; 32]| {
        run(&fixture, |runtime| {
            runtime
                .set_code_cache_dir_with_secret(&dir, secret)
                .unwrap();
        })
    };
    let (_, stats) = with_secret([7; 32]);
    assert_eq!(stats.unwrap().stored, 2);
    // To another secret, those entries are as an entry written by someone without it would
    // be: not authentic. They are compiled again, and kept signed with that secret.
    let (result, stats) = with_secret([9; 32]);
    assert_eq!(result, expected);
    let stats = stats.unwrap();
    assert_eq!(
        (stats.hits, stats.refused, stats.stored),
        (0, 2, 2),
        "{stats:?}"
    );
    let (_, stats) = with_secret([9; 32]);
    assert_eq!(stats.unwrap().hits, 2);
}

/// The pack's layout: a header, an index entry per module (key, offset, length, tag), then
/// the code. Answers each entry's key, code and tag.
fn entries(bytes: &[u8]) -> Vec<([u8; 32], Vec<u8>, [u8; 32])> {
    let count = u32::from_le_bytes(bytes[8..12].try_into().unwrap()) as usize;
    let code = 12 + count * 80;
    (0..count)
        .map(|i| {
            let at = 12 + i * 80;
            let word = |o: usize| u64::from_le_bytes(bytes[at + o..at + o + 8].try_into().unwrap());
            let (offset, len) = (word(32) as usize, word(40) as usize);
            (
                bytes[at..at + 32].try_into().unwrap(),
                bytes[code + offset..code + offset + len].to_vec(),
                bytes[at + 48..at + 80].try_into().unwrap(),
            )
        })
        .collect()
}

fn pack_of(entries: &[([u8; 32], Vec<u8>, [u8; 32])]) -> Vec<u8> {
    let mut bytes = b"VERVEPK\x02".to_vec();
    bytes.extend_from_slice(&(entries.len() as u32).to_le_bytes());
    let mut offset = 0u64;
    for (key, code, tag) in entries {
        bytes.extend_from_slice(key);
        bytes.extend_from_slice(&offset.to_le_bytes());
        bytes.extend_from_slice(&(code.len() as u64).to_le_bytes());
        bytes.extend_from_slice(tag);
        offset += code.len() as u64;
    }
    for (_, code, _) in entries {
        bytes.extend_from_slice(code);
    }
    bytes
}

/// The tag Verve signs an entry with, under `secret`.
fn tag(secret: &[u8; 32], key: &[u8; 32], code: &[u8]) -> [u8; 32] {
    use hmac::{Hmac, Mac};
    let mut mac = Hmac::<sha2::Sha256>::new_from_slice(secret).unwrap();
    mac.update(b"verve code cache entry v1");
    mac.update(key);
    mac.update(&(code.len() as u64).to_le_bytes());
    mac.update(code);
    mac.finalize().into_bytes().into()
}

#[test]
fn a_forged_entry_is_refused() {
    let fixture = Fixture::new("forged");
    let expected = uncached(&fixture);
    cached(&fixture);
    // Someone who can write the pack but not read the secret can change an entry's code and
    // keep its key, but cannot make its tag.
    let pack = fixture.pack();
    let mut forged = entries(&std::fs::read(&pack).unwrap());
    let code = &mut forged[0].1;
    let middle = code.len() / 2;
    code[middle] ^= 0x01;
    std::fs::write(&pack, pack_of(&forged)).unwrap();
    let (result, stats) = cached(&fixture);
    assert_eq!(result, expected);
    assert_eq!(stats.refused, 1, "{stats:?}");
}

/// The attack the directory's own key file allowed: with write access to the directory alone,
/// an attacker puts in a secret of their own and signs code of their choosing with it, under
/// the victim's key (made of public data). No secret is read from the directory, so it fails.
#[test]
fn a_secret_planted_in_the_directory_signs_nothing() {
    let fixture = Fixture::new("planted");
    let expected = uncached(&fixture);
    cached(&fixture);
    let pack = fixture.pack();
    let mut forged = entries(&std::fs::read(&pack).unwrap());
    // Code taken from another module's entry, re-signed with the planted secret.
    let planted = [b'A'; 32];
    std::fs::write(fixture.cache().join("key"), planted).unwrap();
    let evil = forged[1].1.clone();
    forged[0].1 = evil;
    forged[0].2 = tag(&planted, &forged[0].0, &forged[0].1);
    std::fs::write(&pack, pack_of(&forged)).unwrap();
    let (result, stats) = cached(&fixture);
    assert_eq!(result, expected);
    assert_eq!(stats.refused, 1, "{stats:?}");
}

#[test]
fn javascript_cannot_reach_the_cache_directory() {
    let fixture = Fixture::new("reach");
    cached(&fixture);
    let pack = fixture.pack();
    let dir = fixture.cache();
    let mut runtime = Runtime::new();
    runtime
        .set_code_cache_dir_with_secret(&dir, SECRET)
        .unwrap();
    let require = runtime.global_require();
    let global = runtime.context().global_object();
    global
        .set(
            boa_engine::js_string!("require"),
            require,
            false,
            runtime.context(),
        )
        .unwrap();
    let path = |p: &Path| serde_json::to_string(&p.to_string_lossy()).unwrap();
    let probe = format!(
        r#"
        const fs = require('fs');
        const tries = [
            () => fs.readFileSync({pack}),
            () => fs.writeFileSync({pack}, 'x'),
            () => fs.writeFileSync({new}, 'x'),
            () => fs.readdirSync({dir}),
            () => fs.unlinkSync({pack}),
        ];
        tries.map(t => {{ try {{ t(); return 'allowed'; }} catch (e) {{ return e.code; }} }}).join(',')
        "#,
        pack = path(&pack),
        new = path(&dir.join("new.pack")),
        dir = path(&dir),
    );
    let result = verve::print(&runtime.eval(&probe).unwrap());
    assert_eq!(
        result,
        "ERR_ACCESS_DENIED,ERR_ACCESS_DENIED,ERR_ACCESS_DENIED,ERR_ACCESS_DENIED,ERR_ACCESS_DENIED"
    );
    assert!(pack.exists());
    // Outside it, everything is granted as before.
    let outside = fixture.root.join("lib.mjs");
    let read = runtime
        .eval(&format!(
            "require('fs').readFileSync({}, 'utf8').length > 0",
            path(&outside)
        ))
        .unwrap();
    assert_eq!(verve::print(&read), "true");
}

/// A pack is read no further than a pack this build could have written: a file that claims
/// more entries than fit, or is larger than a pack can be, costs a start nothing.
#[test]
fn an_oversized_pack_is_not_read() {
    let fixture = Fixture::new("oversized");
    let expected = uncached(&fixture);
    cached(&fixture);
    let pack = fixture.pack();
    let good = std::fs::read(&pack).unwrap();
    let mut claims = good.clone();
    claims[8..12].copy_from_slice(&u32::MAX.to_le_bytes());
    std::fs::write(&pack, &claims).unwrap();
    let (result, stats) = cached(&fixture);
    assert_eq!(result, expected);
    assert_eq!(stats.hits, 0, "{stats:?}");
    let (_, stats) = cached(&fixture);
    assert_eq!(stats.hits, 2, "the pack is written afresh: {stats:?}");
    // Larger than any pack: not read, and not merged into the next one.
    let file = std::fs::OpenOptions::new().write(true).open(&pack).unwrap();
    file.set_len(65 << 20).unwrap();
    drop(file);
    let (result, stats) = cached(&fixture);
    assert_eq!(result, expected);
    assert_eq!(stats.hits, 0, "{stats:?}");
    assert!(std::fs::metadata(&pack).unwrap().len() < 1 << 20);
}

/// A fixture with `files` written in it: the CommonJS modules one test loads.
fn cjs(name: &str, files: &[(&str, &str)]) -> Fixture {
    let fixture = Fixture::new(name);
    for (file, text) in files {
        let path = fixture.root.join(file);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }
    fixture
}

/// Runs the CommonJS module `entry` in a fresh runtime set up by `setup`; answers what it left
/// in `globalThis.result` and what the cache did.
fn run_cjs(
    fixture: &Fixture,
    entry: &str,
    setup: impl FnOnce(&mut Runtime),
) -> (String, Option<CodeCacheStats>) {
    let mut runtime = Runtime::new();
    setup(&mut runtime);
    let path = fixture.root.join(entry);
    let source = std::fs::read_to_string(&path).unwrap();
    runtime
        .load_module(&path, &source)
        .unwrap_or_else(|error| panic!("{error}"));
    let result = verve::print(&runtime.eval("globalThis.result").unwrap());
    let stats = runtime.code_cache_stats();
    runtime.flush_code_cache();
    (result, stats)
}

fn cjs_cached(fixture: &Fixture, entry: &str) -> (String, CodeCacheStats) {
    let dir = fixture.cache();
    let (result, stats) = run_cjs(fixture, entry, |runtime| {
        runtime
            .set_code_cache_dir_with_secret(&dir, SECRET)
            .unwrap();
    });
    (result, stats.unwrap())
}

/// Runs the module `entry` as a runtime's own ESM entry, in a fresh runtime set up by `setup`.
fn run_mjs(
    fixture: &Fixture,
    entry: &str,
    setup: impl FnOnce(&mut Runtime),
) -> (String, Option<CodeCacheStats>) {
    let mut runtime = Runtime::new();
    setup(&mut runtime);
    let path = fixture.root.join(entry);
    let source = std::fs::read_to_string(&path).unwrap();
    let promise = runtime.load_esm(&path, &source).unwrap();
    for _ in 0..100 {
        if !matches!(promise.state(), PromiseState::Pending) {
            break;
        }
        runtime.run_microtasks();
        runtime.run_due();
    }
    assert!(
        matches!(promise.state(), PromiseState::Fulfilled(_)),
        "{:?} {:?}",
        promise.state(),
        runtime.take_console()
    );
    let result = verve::print(&runtime.eval("globalThis.result").unwrap());
    let stats = runtime.code_cache_stats();
    runtime.flush_code_cache();
    (result, stats)
}

fn mjs_cached(fixture: &Fixture, entry: &str) -> (String, CodeCacheStats) {
    let dir = fixture.cache();
    let (result, stats) = run_mjs(fixture, entry, |runtime| {
        runtime
            .set_code_cache_dir_with_secret(&dir, SECRET)
            .unwrap();
    });
    (result, stats.unwrap())
}

/// The two modules most of the CommonJS tests here load: one with a class, a generator and a
/// default parameter, and one that re-exports from the other.
const CJS_MAIN: &str = r#"
const { Point, add } = require('./lib.cjs');
class Local { #x = 1; n() { return this.#x; } }
function* gen() { yield* [1, 2, 3]; }
globalThis.result = [
    [...gen()].join(''), add(40, 2), new Point(3, 4).norm(), new Local().n(),
    typeof require('module'), add.toString().includes('b = 0'),
].join(' | ');
"#;

const CJS_LIB: &str = r#"
exports.add = (a, b = 0) => a + b;
exports.Point = class {
    #x; #y;
    constructor(x, y) { this.#x = x; this.#y = y; }
    norm() { return Math.hypot(this.#x, this.#y); }
};
"#;

#[test]
fn commonjs_modules_read_back_run_as_parsed_ones() {
    let fixture = cjs(
        "cjs-roundtrip",
        &[("main.cjs", CJS_MAIN), ("lib.cjs", CJS_LIB)],
    );
    let expected = run_cjs(&fixture, "main.cjs", |_| {}).0;
    assert_eq!(expected, "123 | 42 | 5 | 1 | function | true");

    let (cold, stats) = cjs_cached(&fixture, "main.cjs");
    assert_eq!(cold, expected);
    assert_eq!(
        (stats.cjs_hits, stats.cjs_misses, stats.cjs_stored),
        (0, 2, 2),
        "{stats:?}"
    );

    let (warm, stats) = cjs_cached(&fixture, "main.cjs");
    assert_eq!(warm, expected);
    assert_eq!(
        (stats.cjs_hits, stats.cjs_misses, stats.cjs_refused),
        (2, 0, 0),
        "{stats:?}"
    );

    // Eager compilation reads back the same entries and runs the same.
    let dir = fixture.cache();
    let (eager, stats) = run_cjs(&fixture, "main.cjs", |runtime| {
        runtime.set_lazy_compile(false);
        runtime
            .set_code_cache_dir_with_secret(&dir, SECRET)
            .unwrap();
    });
    assert_eq!(eager, expected);
    assert_eq!(stats.unwrap().cjs_hits, 2);
}

#[test]
fn a_changed_commonjs_source_is_compiled_again() {
    let fixture = cjs(
        "cjs-changed",
        &[("main.cjs", CJS_MAIN), ("lib.cjs", CJS_LIB)],
    );
    let (_, stats) = cjs_cached(&fixture, "main.cjs");
    assert_eq!(stats.cjs_stored, 2);
    std::fs::write(
        fixture.root.join("lib.cjs"),
        CJS_LIB.replace("=> a + b;", "=> a + b + 0;"),
    )
    .unwrap();
    let (result, stats) = cjs_cached(&fixture, "main.cjs");
    assert_eq!(result, "123 | 42 | 5 | 1 | function | true");
    assert_eq!(
        (stats.cjs_hits, stats.cjs_misses, stats.cjs_stored),
        (1, 1, 1),
        "{stats:?}"
    );
    // The new entry is read back next time, and the old one is still there for the old source.
    let (_, stats) = cjs_cached(&fixture, "main.cjs");
    assert_eq!(stats.cjs_hits, 2, "{stats:?}");
}

#[test]
fn a_damaged_pack_still_runs_commonjs_from_its_source() {
    let fixture = cjs(
        "cjs-damaged",
        &[("main.cjs", CJS_MAIN), ("lib.cjs", CJS_LIB)],
    );
    let expected = run_cjs(&fixture, "main.cjs", |_| {}).0;
    cjs_cached(&fixture, "main.cjs");
    let pack = fixture.pack();
    let good = std::fs::read(&pack).unwrap();
    for at in [
        8,
        40,
        60,
        good.len() / 3,
        good.len() / 2,
        good.len() - 40,
        good.len() - 1,
    ] {
        let mut damaged = good.clone();
        damaged[at] ^= 0x5a;
        std::fs::write(&pack, &damaged).unwrap();
        let (result, stats) = cjs_cached(&fixture, "main.cjs");
        assert_eq!(result, expected, "damage at {at}");
        assert!(stats.cjs_hits < 2, "damage at {at} is noticed: {stats:?}");
        let (result, stats) = cjs_cached(&fixture, "main.cjs");
        assert_eq!(result, expected, "damage at {at}");
        assert_eq!(
            stats.cjs_hits, 2,
            "the pack is mended after damage at {at}: {stats:?}"
        );
    }
    // Cut short: refused, and compiled again.
    for len in [0, 7, 8, 50, good.len() / 2, good.len() - 1] {
        std::fs::write(&pack, &good[..len]).unwrap();
        let (result, stats) = cjs_cached(&fixture, "main.cjs");
        assert_eq!(result, expected, "cut to {len}");
        assert!(stats.cjs_hits < 2, "{stats:?}");
    }
}

/// A wrapper that declares something in the global scope is not cached: parsing a script is
/// what makes its global declarations, and code read back in another run would have to make
/// them again. A `});` in the source closes Node's wrapper early, which puts what follows in
/// the script's own scope.
#[test]
fn a_wrapper_that_declares_a_global_is_not_cached() {
    const LEAK: &str = r#"
});
globalThis.leaked = (globalThis.leaked || 0) + 1;
globalThis.result = 'ran ' + globalThis.leaked;
var declared = 1;
(function () {
"#;
    let fixture = cjs("cjs-global", &[("leak.cjs", LEAK)]);
    let (cold, stats) = cjs_cached(&fixture, "leak.cjs");
    assert_eq!(cold, "ran 1");
    assert_eq!((stats.cjs_stored, stats.cjs_skipped), (0, 1), "{stats:?}");
    // Nothing was kept, so the second run compiles it again rather than reading it back.
    let (warm, stats) = cjs_cached(&fixture, "leak.cjs");
    assert_eq!(warm, "ran 1");
    assert_eq!(
        (stats.cjs_hits, stats.cjs_stored, stats.cjs_skipped),
        (0, 0, 1),
        "{stats:?}"
    );
}

/// Reading a CommonJS module back changes nothing about what it exports: a cycle still sees
/// the partial `exports` of the module it came from, and a `module.exports` that is replaced
/// is what `require` answers.
#[test]
fn a_cached_commonjs_module_keeps_its_exports_and_cycles() {
    let fixture = cjs(
        "cjs-cycle",
        &[
            (
                "main.cjs",
                "const a = require('./a.cjs');\nglobalThis.result = [a.seesB, require('./fn.cjs')(2), typeof require('./fn.cjs')].join(' | ');\n",
            ),
            (
                "a.cjs",
                "exports.early = 1;\nconst b = require('./b.cjs');\nexports.seesB = b.saw;\n",
            ),
            ("b.cjs", "exports.saw = require('./a.cjs').early + 1;\n"),
            (
                "fn.cjs",
                "module.exports = function (n) { return n * 21; };\n",
            ),
        ],
    );
    let expected = run_cjs(&fixture, "main.cjs", |_| {}).0;
    assert_eq!(expected, "2 | 42 | function");
    let (cold, stats) = cjs_cached(&fixture, "main.cjs");
    assert_eq!(cold, expected);
    assert_eq!((stats.cjs_misses, stats.cjs_stored), (4, 4), "{stats:?}");
    let (warm, stats) = cjs_cached(&fixture, "main.cjs");
    assert_eq!(warm, expected);
    assert_eq!((stats.cjs_hits, stats.cjs_misses), (4, 0), "{stats:?}");
}

/// A `.json` module and a native addon are not code the cache keeps: the one is parsed by the
/// JavaScript parser, the other refused. Neither is counted, and both answer as they do without
/// a cache.
#[test]
fn json_and_native_modules_are_not_cached() {
    let fixture = cjs(
        "cjs-json",
        &[
            (
                "main.cjs",
                "const data = require('./data.json');\n\
                 let addon = 'none';\n\
                 try { require('./addon.node'); } catch (e) { addon = e.code; }\n\
                 globalThis.result = [data.n, addon].join(' | ');\n",
            ),
            ("data.json", "{\"n\": 7}\n"),
            ("addon.node", "\u{7f}ELF not really\n"),
        ],
    );
    let expected = run_cjs(&fixture, "main.cjs", |_| {}).0;
    assert_eq!(expected, "7 | ERR_DLOPEN_DISABLED");
    let (cold, stats) = cjs_cached(&fixture, "main.cjs");
    assert_eq!(cold, expected);
    assert_eq!(
        (stats.cjs_misses, stats.cjs_stored),
        (1, 1),
        "only the module itself is counted: {stats:?}"
    );
    let (warm, stats) = cjs_cached(&fixture, "main.cjs");
    assert_eq!(warm, expected);
    assert_eq!((stats.cjs_hits, stats.cjs_misses), (1, 0), "{stats:?}");
}

/// A source that is neither a CommonJS wrapper nor a module — nothing names its format — is
/// refused with the error parsing it as a script gave, with a cache as without one, and nothing
/// is kept for it.
#[test]
fn a_source_that_is_neither_is_refused_as_before() {
    let fixture = cjs("cjs-broken", &[("broken.js", "const = ;\n")]);
    let failure = |setup: &dyn Fn(&mut Runtime)| {
        let mut runtime = Runtime::new();
        setup(&mut runtime);
        let path = fixture.root.join("broken.js");
        let source = std::fs::read_to_string(&path).unwrap();
        let error = runtime.load_module(&path, &source).unwrap_err();
        (error.to_string(), runtime.code_cache_stats())
    };
    // Without a cache: a run of the whole suite may have one from its environment.
    let (plain, stats) = failure(&|runtime| runtime.disable_code_cache());
    assert!(plain.contains("SyntaxError"), "{plain}");
    assert_eq!(stats, None);
    let dir = fixture.cache();
    let (cached, stats) = failure(&|runtime| {
        runtime
            .set_code_cache_dir_with_secret(&dir, SECRET)
            .unwrap();
    });
    assert_eq!(cached, plain);
    let stats = stats.unwrap();
    assert_eq!((stats.cjs_hits, stats.cjs_stored), (0, 0), "{stats:?}");
    let (again, stats) = failure(&|runtime| {
        runtime
            .set_code_cache_dir_with_secret(&dir, SECRET)
            .unwrap();
    });
    assert_eq!(again, plain);
    assert_eq!(stats.unwrap().cjs_hits, 0);
}

/// A `.js` file no extension or package names the format of is required as Node's CommonJS
/// wrapper, and kept as one: read back, it runs as it did.
#[test]
fn an_unnamed_js_file_is_cached_as_commonjs() {
    let fixture = cjs(
        "js-cjs",
        &[
            (
                "main.cjs",
                "const lib = require('./lib.js');\nglobalThis.result = lib.v;\n",
            ),
            ("lib.js", "exports.v = 'lib ' + (1 + 1);\n"),
        ],
    );
    let expected = run_cjs(&fixture, "main.cjs", |_| {}).0;
    assert_eq!(expected, "lib 2");
    let (cold, stats) = cjs_cached(&fixture, "main.cjs");
    assert_eq!(cold, expected);
    assert_eq!((stats.cjs_misses, stats.cjs_stored), (2, 2), "{stats:?}");
    let (warm, stats) = cjs_cached(&fixture, "main.cjs");
    assert_eq!(warm, expected);
    assert_eq!((stats.cjs_hits, stats.cjs_misses), (2, 0), "{stats:?}");
}

/// A `.js` file that is a module — required, it falls back to the module loader. Its code is a
/// module's, not a script's: the CommonJS counts do not move for it, and no script entry is kept.
#[test]
fn an_unnamed_js_file_that_is_a_module_falls_back() {
    let fixture = cjs(
        "js-esm",
        &[
            (
                "main.cjs",
                "const mod = require('./mod.js');\nglobalThis.result = mod.v;\n",
            ),
            ("mod.js", "export const v = 'esm';\n"),
        ],
    );
    let expected = run_cjs(&fixture, "main.cjs", |_| {}).0;
    assert_eq!(expected, "esm");
    let (cold, stats) = cjs_cached(&fixture, "main.cjs");
    assert_eq!(cold, expected);
    assert_eq!(
        (
            stats.cjs_misses,
            stats.cjs_stored,
            stats.misses,
            stats.stored
        ),
        (1, 1, 1, 1),
        "the module is counted as one: {stats:?}"
    );
    let (warm, stats) = cjs_cached(&fixture, "main.cjs");
    assert_eq!(warm, expected);
    assert_eq!(
        (stats.cjs_hits, stats.cjs_misses, stats.hits),
        (1, 0, 1),
        "{stats:?}"
    );
}

/// One source text, run as a CommonJS wrapper and as a module: a script's entry and a module's
/// are told apart, so neither is read as the other.
#[test]
fn a_source_run_both_ways_keeps_an_entry_of_each_kind() {
    const SHARED: &str = "globalThis.shared = 'shared' + 1;\n";
    let fixture = cjs(
        "js-both",
        &[
            (
                "use-cjs.cjs",
                "require('./plain/lib.js');\nglobalThis.result = String(globalThis.shared);\n",
            ),
            (
                "use-esm.mjs",
                "import './pkg/lib.js';\nglobalThis.result = String(globalThis.shared);\n",
            ),
            ("plain/lib.js", SHARED),
            ("pkg/package.json", "{\"type\": \"module\"}\n"),
            ("pkg/lib.js", SHARED),
        ],
    );
    assert_eq!(run_cjs(&fixture, "use-cjs.cjs", |_| {}).0, "shared1");
    assert_eq!(run_cjs(&fixture, "use-esm.mjs", |_| {}).0, "shared1");

    let (cold, stats) = cjs_cached(&fixture, "use-cjs.cjs");
    assert_eq!(cold, "shared1");
    assert_eq!(
        (stats.cjs_misses, stats.cjs_stored, stats.misses),
        (2, 2, 0),
        "{stats:?}"
    );
    // The module compiles from its own source: the script's entry is not read as one, and does
    // not count as a hit.
    let (esm, stats) = cjs_cached(&fixture, "use-esm.mjs");
    assert_eq!(esm, "shared1");
    assert_eq!(
        (stats.misses, stats.stored, stats.cjs_hits),
        (2, 2, 0),
        "{stats:?}"
    );
    let (esm, stats) = cjs_cached(&fixture, "use-esm.mjs");
    assert_eq!(esm, "shared1");
    assert_eq!((stats.hits, stats.cjs_hits), (2, 0), "{stats:?}");
    let (cjs, stats) = cjs_cached(&fixture, "use-cjs.cjs");
    assert_eq!(cjs, "shared1");
    assert_eq!((stats.cjs_hits, stats.hits), (2, 0), "{stats:?}");
}

/// A byte order mark and a `#!` line are left out of the wrapper, so both are written down and
/// read back as the same script they run as.
#[test]
fn a_byte_order_mark_and_a_hashbang_are_cached() {
    let fixture = cjs(
        "js-bom-shebang",
        &[
            (
                "main.cjs",
                "const a = require('./bom.js');\nconst b = require('./shebang.js');\n\
                 globalThis.result = [a.v, b.v].join(' | ');\n",
            ),
            ("bom.js", "\u{feff}exports.v = 'bom';\n"),
            (
                "shebang.js",
                "#!/usr/bin/env node\nexports.v = 'shebang';\n",
            ),
        ],
    );
    let expected = run_cjs(&fixture, "main.cjs", |_| {}).0;
    assert_eq!(expected, "bom | shebang");
    let (cold, stats) = cjs_cached(&fixture, "main.cjs");
    assert_eq!(cold, expected);
    assert_eq!((stats.cjs_misses, stats.cjs_stored), (3, 3), "{stats:?}");
    let (warm, stats) = cjs_cached(&fixture, "main.cjs");
    assert_eq!(warm, expected);
    assert_eq!((stats.cjs_hits, stats.cjs_misses), (3, 0), "{stats:?}");
}

/// An `import` of a `.js` file no extension or package names the format of: the decision parses
/// its wrapper once, and the CommonJS load that follows runs that parse rather than reading the
/// entry again or parsing the source again.
#[test]
fn an_import_of_an_unnamed_commonjs_file_parses_it_once() {
    let fixture = cjs(
        "import-js-cjs",
        &[
            (
                "main.mjs",
                "import mod from './lib.js';\nglobalThis.result = mod.v;\n",
            ),
            ("lib.js", "module.exports = { v: 'lib ' + (1 + 1) };\n"),
        ],
    );
    assert_eq!(run_mjs(&fixture, "main.mjs", |_| {}).0, "lib 2");

    let (cold, stats) = mjs_cached(&fixture, "main.mjs");
    assert_eq!(cold, "lib 2");
    assert_eq!(
        (stats.cjs_misses, stats.cjs_stored, stats.misses),
        (1, 1, 1),
        "the wrapper is parsed and compiled once: {stats:?}"
    );

    let (warm, stats) = mjs_cached(&fixture, "main.mjs");
    assert_eq!(warm, "lib 2");
    assert_eq!(
        (stats.cjs_hits, stats.cjs_misses, stats.hits),
        (1, 0, 1),
        "the entry is read once: {stats:?}"
    );
}

/// A `.js` entry no package names the format of runs from the wrapper the decision compiled:
/// the run's entry takes the fast path the imports take, so nothing is parsed as a module.
#[test]
fn an_unnamed_js_entry_is_run_from_the_wrapper_it_was_decided_with() {
    let fixture = cjs(
        "js-entry",
        &[
            (
                "main.js",
                "const lib = require('./lib.cjs');\nglobalThis.result = lib.v;\n",
            ),
            ("lib.cjs", "exports.v = 'entry ' + (40 + 2);\n"),
        ],
    );
    assert_eq!(run_cjs(&fixture, "main.js", |_| {}).0, "entry 42");

    let (cold, stats) = cjs_cached(&fixture, "main.js");
    assert_eq!(cold, "entry 42");
    assert_eq!(
        (
            stats.cjs_misses,
            stats.cjs_stored,
            stats.misses,
            stats.stored
        ),
        (2, 2, 0, 0),
        "neither file is parsed as a module: {stats:?}"
    );

    let (warm, stats) = cjs_cached(&fixture, "main.js");
    assert_eq!(warm, "entry 42");
    assert_eq!(
        (stats.cjs_hits, stats.cjs_misses, stats.misses),
        (2, 0, 0),
        "{stats:?}"
    );
}

/// One `.js` file, CommonJS for one run and a module for the next: the entry's key holds the
/// source text, so the code kept for the first is not run as the second.
#[test]
fn a_file_that_becomes_a_module_is_not_run_from_its_commonjs_code() {
    let fixture = cjs(
        "js-becomes-esm",
        &[
            (
                "main.mjs",
                "const mod = await import('./lib.js');\nglobalThis.result = mod.default.v;\n",
            ),
            ("lib.js", "module.exports = { v: 'commonjs' };\n"),
        ],
    );
    assert_eq!(run_mjs(&fixture, "main.mjs", |_| {}).0, "commonjs");
    let (cold, stats) = mjs_cached(&fixture, "main.mjs");
    assert_eq!(cold, "commonjs");
    assert_eq!((stats.cjs_misses, stats.cjs_stored), (1, 1), "{stats:?}");

    std::fs::write(
        fixture.root.join("lib.js"),
        "export default { v: 'module' };\n",
    )
    .unwrap();
    let (changed, stats) = mjs_cached(&fixture, "main.mjs");
    assert_eq!(changed, "module");
    assert_eq!(
        (stats.hits, stats.misses, stats.stored, stats.cjs_hits),
        (1, 1, 1, 0),
        "the changed file compiles as a module, and its script entry is not read: {stats:?}"
    );
}

#[test]
fn verve_run_keeps_a_cache_with_the_flag_or_the_variable() {
    let fixture = Fixture::new("cli");
    let script = fixture.root.join("print.mjs");
    std::fs::write(
        &script,
        "await import('./main.mjs'); console.log(globalThis.result);",
    )
    .unwrap();
    // The user's secret lives in their own folder: here, the fixture's.
    let home = fixture.root.join("home");
    let run = |flag: Option<&Path>, env: Option<&Path>| {
        let mut command = Command::new(VERVE);
        command
            .env("LOCALAPPDATA", &home)
            .env("XDG_CONFIG_HOME", &home)
            .arg("run")
            .arg("-A");
        if let Some(dir) = flag {
            command.arg(format!("--code-cache={}", dir.display()));
        }
        command.arg(&script);
        command.env_remove("VERVE_CODE_CACHE");
        if let Some(dir) = env {
            command.env("VERVE_CODE_CACHE", dir);
        }
        let output = command.output().unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8(output.stdout).unwrap()
    };
    let expected = run(None, None);
    assert!(expected.contains(" | 42 | "), "{expected}");
    let dir = fixture.cache();
    assert_eq!(run(Some(&dir), None), expected);
    assert!(fixture.pack().exists());
    assert_eq!(run(Some(&dir), None), expected);
    assert_eq!(run(None, Some(&dir)), expected);
    // The secret is the user's, not the directory's.
    assert!(!dir.join("key").exists());
    let secret = if cfg!(windows) {
        home.join("Verve").join("code-cache-secret")
    } else {
        home.join("verve").join("code-cache-secret")
    };
    assert_eq!(std::fs::read(&secret).unwrap().len(), 32);
    // A secret that is not one leaves the run without a cache, with a warning.
    std::fs::write(&secret, b"short").unwrap();
    let mut command = Command::new(VERVE);
    let output = command
        .env("LOCALAPPDATA", &home)
        .env("XDG_CONFIG_HOME", &home)
        .arg("run")
        .arg("-A")
        .arg(format!("--code-cache={}", dir.display()))
        .arg(&script)
        .output()
        .unwrap();
    assert!(output.status.success());
    assert_eq!(String::from_utf8(output.stdout).unwrap(), expected);
    assert!(String::from_utf8_lossy(&output.stderr).contains("not used"));
}

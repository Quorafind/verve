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

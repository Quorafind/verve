//! `fs`, `require` and `import` reading an archive in a runtime, each against Electron 39's
//! `lib/node/asar-fs-wrapper.ts`: what is inside answers as if it were a folder, `original-fs`
//! does not see it, and a write into it fails as a path through a file does.
#![allow(clippy::unwrap_used)]

use std::path::Path;
use std::sync::Arc;

use verve::Runtime;
use verve::vfs::Overlay;
use verve_asar::{AsarOptions, AsarOverlay};

mod support;

use support::{Dir, Pack};

/// A realm with `require` on its global object, as a renderer has `window.require`, reading
/// `pack` written as `app.asar` in `dir`.
fn realm(dir: &Dir, pack: &Pack) -> Runtime {
    realm_with(dir, pack, AsarOptions::default())
}

fn realm_with(dir: &Dir, pack: &Pack, options: AsarOptions) -> Runtime {
    pack.write(&dir.at("app.asar"));
    let mut rt = Runtime::new();
    let require = rt.global_require();
    rt.context()
        .register_global_property(
            boa_engine::js_string!("require"),
            require,
            boa_engine::property::Attribute::all(),
        )
        .unwrap();
    rt.set_vfs(Some(Arc::new(AsarOverlay::new(options))));
    rt
}

/// Runs `body` as `main.js` in `dir`, lets what it started settle, and reads the array it set
/// as `globalThis.result`, items as strings.
fn run(rt: &mut Runtime, dir: &Dir, body: &str) -> Vec<String> {
    let prelude = r#"
        const fs = require('fs');
        const path = require('path');
        const asar = path.join(__dirname, 'app.asar');
        const inside = (...names) => path.join(asar, ...names);
        const slashes = (text) => text.split(String.fromCharCode(92)).join('/');
        const same = (a, b) => slashes(a) === slashes(b);
        const fail = (f) => {
            try { f(); return 'no error'; }
            catch (e) { return [e.code, typeof e.errno, e.syscall, e.message].join('|'); }
        };
    "#;
    rt.load_module(&dir.at("main.js"), &format!("{prelude}\n{body}"))
        .unwrap_or_else(|e| panic!("{e}"));
    for _ in 0..10 {
        rt.run_microtasks();
        rt.run_due();
    }
    let json = rt
        .eval("JSON.stringify(globalThis.result)")
        .unwrap_or_else(|e| panic!("{e}"));
    let parsed: serde_json::Value = serde_json::from_str(&verve::print(&json))
        .unwrap_or_else(|_| panic!("no result: {}", verve::print(&json)));
    parsed
        .as_array()
        .unwrap_or_else(|| panic!("result is not an array: {parsed}"))
        .iter()
        .map(|v| v.as_str().map_or_else(|| v.to_string(), str::to_owned))
        .collect()
}

fn app() -> Pack {
    Pack::new()
        .file("package.json", br#"{"name":"app","main":"./lib/main.js"}"#)
        .file(
            "lib/main.js",
            b"module.exports = 'main:' + require('./util').name;",
        )
        .file("lib/util.js", b"exports.name = 'util';")
        .file("lib/data.json", br#"{"answer":42}"#)
        .file("lib/sub/index.js", b"module.exports = 'sub index';")
        .file(
            "lib/esm.mjs",
            b"import { name } from './dep.mjs'; export default 'esm:' + name;",
        )
        .file("lib/dep.mjs", b"export const name = 'dep';")
        .file("hello.txt", b"hello from the archive")
        .executable("bin/run.sh", b"#!/bin/sh\n")
        .link("latest", "lib/util.js")
        .link("libs", "lib")
        .dir("empty")
        .file(
            "node_modules/pkg/package.json",
            br#"{"name":"pkg","exports":{"./thing":"./thing.js"}}"#,
        )
        .file(
            "node_modules/pkg/thing.js",
            b"module.exports = 'thing of pkg';",
        )
}

#[test]
fn files_are_read_and_stat_like_a_folder_would_be() {
    let dir = Dir::new("sync");
    let mut rt = realm(&dir, &app());
    let result = run(
        &mut rt,
        &dir,
        r#"
        const file = inside('hello.txt');
        const st = fs.statSync(file);
        const dirStat = fs.statSync(inside('lib'));
        const root = fs.statSync(asar);
        const link = fs.lstatSync(inside('latest'));
        const run = fs.statSync(inside('bin', 'run.sh'));
        globalThis.result = [
            fs.readFileSync(file, 'utf8'),
            fs.readFileSync(file).length,
            fs.readFileSync(inside('lib', 'data.json'), 'utf8'),
            [st.isFile(), st.isDirectory(), st.size, (st.mode & 0o170000) === 0o100000].join(),
            [dirStat.isDirectory(), dirStat.size, (dirStat.mode & 0o170000) === 0o040000].join(),
            'root:' + [root.isDirectory(), root.isFile()].join(),
            'link:' + [link.isSymbolicLink(), fs.statSync(inside('latest')).isFile()].join(),
            'exec:' + ((run.mode & 0o111) === 0o111),
            'big:' + fs.statSync(file, { bigint: true }).size,
            'times:' + (st.mtimeMs > 0 && st.mtimeMs === st.birthtimeMs),
            'exists:' + [fs.existsSync(file), fs.existsSync(inside('nope')), fs.existsSync(asar)].join(),
            'readdir:' + fs.readdirSync(inside('lib')).join(),
            'root:' + fs.readdirSync(asar).join(),
            'typed:' + fs.readdirSync(asar, { withFileTypes: true })
                .map((d) => d.name + (d.isDirectory() ? '/' : d.isSymbolicLink() ? '@' : '')).join(),
            'recursive:' + fs.readdirSync(inside('lib'), { recursive: true }).sort().join(),
            'access:' + [fail(() => fs.accessSync(file, fs.constants.R_OK)),
                         fs.constants.W_OK && fail(() => fs.accessSync(file, fs.constants.W_OK)).split('|')[0]].join(),
            'real:' + same(fs.realpathSync(inside('latest')), inside('lib', 'util.js')),
            'real2:' + same(fs.realpathSync(inside('libs', 'util.js')), inside('lib', 'util.js')),
            'native:' + same(fs.realpathSync.native(inside('latest')), inside('lib', 'util.js')),
        ];
        "#,
    );
    assert_eq!(
        result,
        [
            "hello from the archive",
            "22",
            r#"{"answer":42}"#,
            "true,false,22,true",
            "true,0,true",
            "root:true,false",
            "link:true,true",
            "exec:true",
            "big:22",
            "times:true",
            "exists:true,false,true",
            "readdir:data.json,dep.mjs,esm.mjs,main.js,sub,util.js",
            "root:bin,empty,hello.txt,latest,lib,libs,node_modules,package.json",
            "typed:bin/,empty/,hello.txt,latest@,lib/,libs@,node_modules/,package.json",
            "recursive:data.json,dep.mjs,esm.mjs,main.js,sub,sub/index.js,util.js",
            "access:no error,EACCES",
            "real:true",
            "real2:true",
            "native:true",
        ]
    );
}

#[test]
fn a_missing_file_or_a_file_read_as_a_folder_fails_in_electrons_words() {
    let dir = Dir::new("errors");
    let mut rt = realm(&dir, &app());
    let result = run(
        &mut rt,
        &dir,
        r#"
        const e = fail;
        globalThis.result = [
            e(() => fs.readFileSync(inside('lib', 'missing.js'))),
            e(() => fs.statSync(inside('nope'))),
            e(() => fs.readdirSync(inside('hello.txt'))),
            e(() => fs.readdirSync(inside('nope'))),
            e(() => fs.accessSync(inside('nope'), fs.constants.W_OK)),
            e(() => fs.realpathSync(inside('nope'))),
            e(() => fs.readFileSync(inside('lib'))),
        ];
        "#,
    );
    let archive = dir.at("app.asar").display().to_string();
    let result: Vec<String> = result
        .iter()
        .map(|line| line.replace(&archive, "<asar>"))
        .collect();
    assert_eq!(
        result[..6],
        [
            "ENOENT|number|open|ENOENT, lib/missing.js not found in <asar>",
            "ENOENT|number|stat|ENOENT, nope not found in <asar>",
            "ENOTDIR|number|scandir|ENOTDIR, not a directory",
            "ENOENT|number|scandir|ENOENT, nope not found in <asar>",
            "ENOENT|number|access|ENOENT, nope not found in <asar>",
            "ENOENT|number|realpath|ENOENT, nope not found in <asar>",
        ]
    );
    assert!(result[6].starts_with("EISDIR|"), "{}", result[6]);
}

#[test]
fn callbacks_promises_streams_and_descriptors_read_the_archive() {
    let dir = Dir::new("async");
    let mut rt = realm(&dir, &app());
    let result = run(
        &mut rt,
        &dir,
        r#"
        const file = inside('hello.txt');
        const log = [];
        globalThis.result = log;
        fs.readFile(file, 'utf8', (err, text) => log.push('cb:' + text));
        fs.stat(file, (err, st) => log.push('stat:' + st.size));
        fs.readdir(inside('lib', 'sub'), (err, names) => log.push('readdir:' + names));
        fs.exists(file, (yes) => log.push('exists:' + yes));
        fs.readFile(inside('nope'), (err) => log.push('cb-error:' + err.code));
        fs.promises.readFile(file, 'utf8').then((text) => log.push('promise:' + text));
        fs.promises.stat(inside('lib')).then((st) => log.push('pstat:' + st.isDirectory()));
        fs.promises.readdir(inside('lib', 'sub')).then((n) => log.push('preaddir:' + n));
        fs.promises.access(file).then(() => log.push('paccess'));
        fs.promises.realpath(inside('latest')).then((p) => log.push('prealpath:' + same(p, inside('lib', 'util.js'))));
        fs.promises.readFile(inside('nope')).catch((e) => log.push('perror:' + e.code));
        let streamed = '';
        fs.createReadStream(file, { encoding: 'utf8', highWaterMark: 5 })
            .on('data', (chunk) => { streamed += chunk; })
            .on('end', () => log.push('stream:' + streamed));
        const fd = fs.openSync(file, 'r');
        const buffer = Buffer.alloc(5);
        fs.readSync(fd, buffer, 0, 5, 6);
        log.push('fd:' + buffer + ':' + fs.fstatSync(fd).size);
        fs.closeSync(fd);
        fs.promises.open(file).then(async (handle) => {
            log.push('handle:' + (await handle.readFile('utf8')).length);
            await handle.close();
        });
        const opened = fs.opendirSync(inside('lib', 'sub'));
        log.push('opendir:' + opened.readSync().name);
        opened.closeSync();
        "#,
    );
    let mut sorted = result.clone();
    sorted.sort();
    let mut expected = [
        "cb:hello from the archive",
        "stat:22",
        "readdir:index.js",
        "exists:true",
        "cb-error:ENOENT",
        "promise:hello from the archive",
        "pstat:true",
        "preaddir:index.js",
        "paccess",
        "prealpath:true",
        "perror:ENOENT",
        "stream:hello from the archive",
        "fd:from :22",
        "handle:22",
        "opendir:index.js",
    ]
    .map(str::to_owned);
    expected.sort();
    assert_eq!(sorted, expected);
}

#[test]
fn require_and_import_load_from_the_archive() {
    let dir = Dir::new("modules2");
    let mut rt = realm(&dir, &app());
    let result = run(
        &mut rt,
        &dir,
        r#"
        const log = [];
        globalThis.result = log;
        log.push(require('./app.asar'));
        log.push(require('./app.asar/lib/main'));
        log.push(require('./app.asar/lib/data.json').answer);
        log.push(require('./app.asar/lib/sub'));
        log.push(require('./app.asar/node_modules/pkg/thing'));
        log.push(require('./app.asar/latest').name);
        log.push(same(require.resolve('./app.asar/lib/util'), inside('lib', 'util.js')));
        log.push(same(require.resolve('./app.asar'), inside('lib', 'main.js')));
        log.push(fail(() => require('./app.asar/nope.js')).split('|')[0]);
        import('./app.asar/lib/esm.mjs').then(
            (m) => log.push(m.default),
            (e) => log.push('import failed: ' + e.message),
        );
        "#,
    );
    assert_eq!(
        result,
        [
            "main:util",
            "main:util",
            "42",
            "sub index",
            "thing of pkg",
            "util",
            "true",
            "true",
            "MODULE_NOT_FOUND",
            "esm:dep",
        ]
    );
}

#[test]
fn original_fs_and_no_asar_see_the_archive_as_the_file_it_is() {
    let dir = Dir::new("original");
    let mut rt = realm(&dir, &app());
    let result = run(
        &mut rt,
        &dir,
        r#"
        const original = require('original-fs');
        const log = [];
        globalThis.result = log;
        log.push('same:' + (original === fs));
        log.push('archive:' + [original.statSync(asar).isFile(), original.statSync(asar).isDirectory()].join());
        log.push('inside:' + fail(() => original.readFileSync(inside('hello.txt'))).split('|')[0]);
        log.push('fs:' + fs.statSync(asar).isDirectory());
        process.noAsar = true;
        log.push('noAsar:' + [process.noAsar, fs.statSync(asar).isFile()].join());
        log.push('noAsar inside:' + fail(() => fs.readFileSync(inside('hello.txt'))).split('|')[0]);
        process.noAsar = false;
        log.push('back:' + fs.readFileSync(inside('hello.txt'), 'utf8').length);
        "#,
    );
    assert_eq!(
        result,
        [
            "same:false",
            "archive:true,false",
            "inside:ENOENT",
            "fs:true",
            "noAsar:true,true",
            "noAsar inside:ENOENT",
            "back:22",
        ]
    );
}

#[test]
fn writes_into_the_archive_fail_as_a_path_through_a_file_does() {
    let dir = Dir::new("writes");
    let mut rt = realm(&dir, &app());
    std::fs::write(dir.at("out.txt"), "outside").unwrap();
    let result = run(
        &mut rt,
        &dir,
        r#"
        const code = (f) => fail(f).split('|').slice(0, 3).join('|');
        const target = inside('hello.txt');
        globalThis.result = [
            code(() => fs.writeFileSync(target, 'x')),
            code(() => fs.appendFileSync(inside('new.txt'), 'x')),
            code(() => fs.mkdirSync(inside('fresh'))),
            code(() => fs.mkdirSync(inside('fresh', 'deep'), { recursive: true })),
            code(() => fs.unlinkSync(target)),
            code(() => fs.rmSync(inside('lib'), { recursive: true })),
            code(() => fs.renameSync(target, path.join(__dirname, 'moved.txt'))),
            code(() => fs.copyFileSync(path.join(__dirname, 'out.txt'), inside('copy.txt'))),
            code(() => fs.chmodSync(target, 0o777)),
            code(() => fs.openSync(target, 'w')),
            code(() => fs.truncateSync(target)),
            code(() => fs.symlinkSync('x', inside('sym'))),
            fs.existsSync(inside('new.txt')) + ':' + fs.readFileSync(target, 'utf8').length,
        ];
        "#,
    );
    let denied = |syscall: &str| format!("ENOTDIR|number|{syscall}");
    assert_eq!(&result[0], &denied("open"));
    assert_eq!(&result[1], &denied("open"));
    assert_eq!(&result[2], &denied("mkdir"));
    assert_eq!(&result[3], &denied("mkdir"));
    assert_eq!(&result[4], &denied("unlink"));
    assert!(result[5].starts_with("ENOTDIR|number|"), "{}", result[5]);
    assert!(result[6].starts_with("ENOTDIR|number|"), "{}", result[6]);
    assert_eq!(&result[7], &denied("copyfile"));
    assert!(result[8].starts_with("ENOTDIR|number|"), "{}", result[8]);
    assert_eq!(&result[9], &denied("open"));
    assert!(result[10].starts_with("ENOTDIR|number|"), "{}", result[10]);
    assert!(result[11].starts_with("ENOTDIR|number|"), "{}", result[11]);
    assert_eq!(&result[12], "false:22");
}

#[test]
fn copying_out_of_the_archive_reads_it() {
    let dir = Dir::new("copy");
    let mut rt = realm(&dir, &app());
    let result = run(
        &mut rt,
        &dir,
        r#"
        fs.copyFileSync(inside('hello.txt'), path.join(__dirname, 'copy.txt'));
        fs.cpSync(inside('lib', 'sub'), path.join(__dirname, 'cp'), { recursive: true });
        globalThis.result = [
            fs.readFileSync(path.join(__dirname, 'copy.txt'), 'utf8'),
            fs.readFileSync(path.join(__dirname, 'cp', 'index.js'), 'utf8'),
        ];
        "#,
    );
    assert_eq!(
        result,
        ["hello from the archive", "module.exports = 'sub index';"]
    );
}

#[test]
fn unpacked_files_are_real_files_and_packed_ones_are_extracted_on_demand() {
    let dir = Dir::new("unpacked");
    let pack = Pack::new()
        .file("a.txt", b"packed")
        .unpacked("native/addon.node", b"unpacked bytes")
        .executable("bin/tool", b"#!tool");
    let mut rt = realm(&dir, &pack);
    let result = run(
        &mut rt,
        &dir,
        r#"
        const addon = inside('native', 'addon.node');
        globalThis.result = [
            fs.readFileSync(addon, 'utf8'),
            String(fs.statSync(addon).size),
            fs.readdirSync(inside('native')).join(),
        ];
        "#,
    );
    assert_eq!(result, ["unpacked bytes", "14", "addon.node"]);

    let archive = dir.at("app.asar");
    let unpacked = rt.materialize(&archive.join("native").join("addon.node"));
    assert_eq!(
        unpacked.unwrap().unwrap(),
        dir.at("app.asar.unpacked")
            .join("native")
            .join("addon.node")
    );
    let tool = rt
        .materialize(&archive.join("bin").join("tool"))
        .unwrap()
        .unwrap();
    assert_eq!(std::fs::read(&tool).unwrap(), b"#!tool");
    assert_eq!(tool.file_name().unwrap(), "tool");
    assert!(!tool.starts_with(&archive));
    // Once for each file.
    let again = rt
        .materialize(&archive.join("bin").join("tool"))
        .unwrap()
        .unwrap();
    assert_eq!(again, tool);
    // A folder with no unpacked twin is not a place a program can run in; one with a twin is.
    let missing = rt.materialize(&archive.join("bin")).unwrap().unwrap_err();
    assert_eq!(missing.kind(), std::io::ErrorKind::NotADirectory);
    std::fs::create_dir_all(dir.at("app.asar.unpacked/bin")).unwrap();
    assert_eq!(
        rt.materialize(&archive.join("bin")).unwrap().unwrap(),
        dir.at("app.asar.unpacked").join("bin")
    );
    assert_eq!(
        rt.materialize(&archive).unwrap().unwrap(),
        dir.at("app.asar.unpacked")
    );
    // What the archive does not hold is the disk's.
    assert!(rt.materialize(&dir.at("elsewhere.txt")).is_none());
}

#[test]
fn what_is_not_an_archive_is_left_to_the_disk() {
    let dir = Dir::new("plain");
    let mut rt = realm(&dir, &app());
    std::fs::create_dir_all(dir.at("plain.asar")).unwrap();
    std::fs::write(dir.at("plain.asar/file.txt"), "on disk").unwrap();
    std::fs::write(dir.at("broken.asar"), "not an archive").unwrap();
    let result = run(
        &mut rt,
        &dir,
        r#"
        const code = (f) => fail(f).split('|').filter((_, i) => i !== 1 && i !== 2).join('|');
        globalThis.result = [
            fs.readFileSync(path.join(__dirname, 'plain.asar', 'file.txt'), 'utf8'),
            String(fs.statSync(path.join(__dirname, 'plain.asar')).isDirectory()),
            fs.readFileSync(path.join(__dirname, 'broken.asar'), 'utf8'),
            code(() => fs.readFileSync(path.join(__dirname, 'broken.asar', 'x'))).replace(__dirname, '<dir>'),
            code(() => fs.readFileSync(path.join(__dirname, 'absent.asar', 'x'))).split('|')[0],
        ];
        "#,
    );
    assert_eq!(result[0], "on disk");
    assert_eq!(result[1], "true");
    assert_eq!(result[2], "not an archive");
    assert!(result[3].contains("Invalid package"), "{}", result[3]);
    assert_eq!(result[4], "ENOENT");
}

#[test]
fn relative_paths_are_the_realms_own() {
    let dir = Dir::new("relative");
    let mut rt = realm(&dir, &app());
    let result = run(
        &mut rt,
        &dir,
        r#"
        process.chdir(__dirname);
        globalThis.result = [
            fs.readFileSync('app.asar/hello.txt', 'utf8'),
            fs.readFileSync('./app.asar/lib/../hello.txt', 'utf8'),
            String(fs.existsSync('app.asar/nope')),
        ];
        "#,
    );
    assert_eq!(
        result,
        ["hello from the archive", "hello from the archive", "false"]
    );
}

#[test]
fn a_module_run_from_the_archive_can_be_forgotten_and_run_again() {
    let dir = Dir::new("forget");
    let pack = Pack::new().file(
        "count.js",
        b"globalThis.runs = (globalThis.runs || 0) + 1; module.exports = 1;",
    );
    let mut rt = realm(&dir, &pack);
    let result = run(
        &mut rt,
        &dir,
        "require('./app.asar/count'); require('./app.asar/count'); globalThis.result = [String(globalThis.runs)];",
    );
    assert_eq!(result, ["1"]);
    rt.forget_module(&dir.at("app.asar").join("count.js"));
    rt.forget_module(&dir.at("main.js"));
    let again = run(
        &mut rt,
        &dir,
        "require('./app.asar/count'); globalThis.result = [String(globalThis.runs)];",
    );
    assert_eq!(again, ["2"]);
}

#[test]
fn a_validated_archive_reads_the_files_whose_digests_match() {
    let dir = Dir::new("validated");
    let pack = Pack::new().with_integrity(4).file("a.txt", b"0123456789");
    let mut rt = realm_with(
        &dir,
        &pack,
        AsarOptions {
            validate_integrity: true,
            ..AsarOptions::default()
        },
    );
    let result = run(
        &mut rt,
        &dir,
        "globalThis.result = [fs.readFileSync(inside('a.txt'), 'utf8')];",
    );
    assert_eq!(result, ["0123456789"]);
}

#[test]
fn the_cache_key_names_a_packed_file_by_where_it_lies_in_its_archive() {
    let dir = Dir::new("cache-key");
    let pack = Pack::new()
        .file("a.js", b"1")
        .file("b.js", b"2")
        .unpacked("c.js", b"3");
    pack.write(&dir.at("app.asar"));
    let overlay = AsarOverlay::new(AsarOptions::default());
    let key = |name: &str| overlay.cache_key(&dir.at("app.asar").join(name));
    assert!(key("a.js").is_some());
    assert_eq!(key("a.js"), key("a.js"));
    assert_ne!(key("a.js"), key("b.js"));
    assert_eq!(key("c.js"), None);
    assert_eq!(key("missing.js"), None);
    assert_eq!(overlay.cache_key(Path::new("elsewhere.js")), None);
}

#[test]
fn modes_are_the_ones_electron_gives_an_entry() {
    let dir = Dir::new("modes");
    let mut rt = realm(&dir, &app());
    let result = run(
        &mut rt,
        &dir,
        r#"
        const mode = (...names) => (fs.lstatSync(inside(...names)).mode).toString(8);
        globalThis.result = [mode('hello.txt'), mode('lib'), mode('bin', 'run.sh'), mode('latest'), mode()];
        "#,
    );
    assert_eq!(result, ["100644", "40755", "100755", "120777", "40755"]);
}

#[test]
fn write_access_is_refused_for_packed_files_in_electrons_words_and_real_for_unpacked() {
    let dir = Dir::new("access");
    let pack = Pack::new().file("a.txt", b"a").unpacked("u.txt", b"u");
    let mut rt = realm(&dir, &pack);
    let result = run(
        &mut rt,
        &dir,
        r#"
        globalThis.result = [
            fail(() => fs.accessSync(inside('a.txt'), fs.constants.W_OK)),
            fail(() => fs.accessSync(inside('u.txt'), fs.constants.W_OK)),
            fail(() => fs.accessSync(inside('a.txt'), fs.constants.R_OK)),
        ];
        "#,
    );
    assert_eq!(
        result,
        [
            "EACCES|number|access|EACCES: permission denied, access 'a.txt'",
            "no error",
            "no error"
        ]
    );
}

#[test]
fn a_failed_integrity_check_is_reported_to_the_host_and_is_an_error() {
    let dir = Dir::new("integrity-callback");
    let at = dir.at("app.asar");
    Pack::new()
        .with_integrity(4)
        .file("a.txt", b"0123456789")
        .write(&at);
    let mut bytes = std::fs::read(&at).unwrap();
    let last = bytes.len() - 1;
    bytes[last] ^= 1;
    std::fs::write(&at, bytes).unwrap();
    let told = Arc::new(std::sync::Mutex::new(Vec::new()));
    let sink = Arc::clone(&told);
    let overlay = AsarOverlay::new(AsarOptions {
        validate_integrity: true,
        on_integrity_failure: Some(Arc::new(move |error| {
            sink.lock().unwrap().push(error.message().to_owned());
        })),
        ..AsarOptions::default()
    });
    let error = overlay.read(&at.join("a.txt")).unwrap().unwrap_err();
    assert!(verve_asar::AsarError::of(&error).unwrap().is_integrity());
    assert_eq!(told.lock().unwrap().len(), 1);
    // A file that is whole raises nothing.
    assert!(overlay.read(&at.join("missing")).unwrap().is_err());
    assert_eq!(told.lock().unwrap().len(), 1);
}

#[test]
fn a_disabled_overlay_claims_nothing() {
    let dir = Dir::new("disabled");
    Pack::new().file("a.txt", b"a").write(&dir.at("app.asar"));
    let overlay = AsarOverlay::new(AsarOptions {
        disabled: true,
        ..AsarOptions::default()
    });
    assert!(overlay.stat(&dir.at("app.asar").join("a.txt")).is_none());
}

#[test]
fn extracted_files_are_private_to_the_overlay_and_removed_with_it() {
    let dir = Dir::new("extracted");
    Pack::new()
        .executable("tool", b"#!tool")
        .write(&dir.at("app.asar"));
    let overlay = AsarOverlay::new(AsarOptions::default());
    let path = overlay
        .materialize(&dir.at("app.asar").join("tool"))
        .unwrap()
        .unwrap();
    assert_eq!(std::fs::read(&path).unwrap(), b"#!tool");
    let root = path.parent().unwrap().parent().unwrap().to_path_buf();
    assert!(
        root.file_name()
            .unwrap()
            .to_string_lossy()
            .starts_with("verve-electron-asar-")
    );
    let other = AsarOverlay::new(AsarOptions::default());
    let again = other
        .materialize(&dir.at("app.asar").join("tool"))
        .unwrap()
        .unwrap();
    assert_ne!(
        again.parent().unwrap().parent(),
        path.parent().unwrap().parent()
    );
    drop(overlay);
    assert!(!root.exists());
}

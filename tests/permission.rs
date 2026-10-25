//! The permission model's filesystem and worker half: every `fs` family (sync, callback,
//! promises, streams, `Dir`, `FileHandle`), module loading, `..` and link escapes,
//! `process.permission.has`, workers — and an embedder that grants nothing keeps the disk and
//! workers it always had.

use std::path::{Path, PathBuf};

use verve::{FsAccess, Permissions, Runtime};

/// A folder with what each test reads and writes: `open` (read and write granted), `ro` (read
/// granted), `secret` (nothing granted).
struct Tree {
    root: PathBuf,
}

impl Tree {
    fn new(test: &str) -> Self {
        let root = std::env::temp_dir().join(format!("verve-perm-{}-{test}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        for dir in ["open", "ro", "secret"] {
            std::fs::create_dir_all(root.join(dir)).unwrap();
        }
        std::fs::write(root.join("open/a.txt"), "open").unwrap();
        std::fs::write(root.join("ro/r.txt"), "read only").unwrap();
        std::fs::write(root.join("secret/s.txt"), "secret").unwrap();
        std::fs::write(
            root.join("secret/s.js"),
            "module.exports = 'secret module';",
        )
        .unwrap();
        std::fs::write(root.join("secret/s.mjs"), "export default 'secret esm';").unwrap();
        std::fs::write(root.join("open/m.js"), "module.exports = 'open module';").unwrap();
        std::fs::write(
            root.join("open/m.mjs"),
            "import s from '../secret/s.mjs'; export default s;",
        )
        .unwrap();
        std::fs::write(root.join("open/ok.mjs"), "export default 'open esm';").unwrap();
        Self { root }
    }

    fn path(&self, rest: &str) -> PathBuf {
        self.root.join(rest)
    }

    fn permissions(&self) -> Permissions {
        Permissions {
            fs_read: FsAccess::Only(vec![self.path("open"), self.path("ro")]),
            fs_write: FsAccess::Only(vec![self.path("open")]),
            worker: false,
            ..Permissions::default()
        }
    }

    fn runtime(&self) -> Runtime {
        let mut rt = Runtime::new();
        rt.set_permissions(self.permissions());
        rt
    }
}

impl Drop for Tree {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

/// Runs `body` (an async function's) as a module in `open`, with `root` its folder's parent,
/// until it sets `result`; answers `result`'s items as text.
fn run(rt: &mut Runtime, tree: &Tree, body: &str) -> Vec<String> {
    let root = tree.root.to_string_lossy().replace('\\', "\\\\");
    let source = format!(
        "const root = '{root}'; const path = require('path'); const fs = require('fs');
         const at = (p) => path.join(root, p);
         const slash = (p) => p.split(String.fromCharCode(92)).join('/');
         const code = (f) => {{ try {{ f(); return 'ok'; }} catch (e) {{ return e.code + ':' + (e.permission || ''); }} }};
         globalThis.result = undefined;
         (async () => {{ {body}\n }})().catch(e => {{ globalThis.result = ['threw ' + (e && e.stack || e)]; }});"
    );
    rt.load_module(&tree.path("open/main.js"), &source)
        .unwrap_or_else(|e| panic!("{e}"));
    for _ in 0..100 {
        rt.run_microtasks();
        if rt
            .eval("globalThis.result !== undefined")
            .is_ok_and(|v| v.to_boolean())
        {
            break;
        }
        rt.run_due();
        std::thread::sleep(std::time::Duration::from_millis(1));
    }
    let text = rt
        .eval("JSON.stringify(globalThis.result)")
        .unwrap_or_else(|e| panic!("{e}"));
    let parsed: serde_json::Value = serde_json::from_str(&verve::print(&text))
        .unwrap_or_else(|_| panic!("no result: {}", verve::print(&text)));
    parsed
        .as_array()
        .unwrap_or_else(|| panic!("not an array: {parsed}"))
        .iter()
        .map(|v| {
            v.as_str()
                .map(str::to_owned)
                .unwrap_or_else(|| v.to_string())
        })
        .collect()
}

#[test]
fn sync_calls_are_checked_against_the_grants() {
    let tree = Tree::new("sync");
    let mut rt = tree.runtime();
    let result = run(
        &mut rt,
        &tree,
        r#"
        let denied;
        try { fs.readFileSync(at('secret/s.txt')) } catch (e) { denied = e }
        globalThis.result = [
            fs.readFileSync(at('open/a.txt'), 'utf8'),
            fs.readFileSync(at('ro/r.txt'), 'utf8'),
            denied.code, denied.permission,
            slash(denied.resource) === slash(at('secret/s.txt')),
            denied.message,
            code(() => fs.writeFileSync(at('ro/r.txt'), 'x')),
            code(() => fs.writeFileSync(at('open/new.txt'), 'x')),
            code(() => fs.appendFileSync(at('secret/s.txt'), 'x')),
            code(() => fs.statSync(at('secret/s.txt'))),
            code(() => fs.lstatSync(at('secret'))),
            code(() => fs.readdirSync(at('secret'))),
            code(() => fs.readdirSync(at('ro'))),
            String(fs.existsSync(at('secret/s.txt'))),
            String(fs.existsSync(at('open/a.txt'))),
            code(() => fs.accessSync(at('secret/s.txt'))),
            code(() => fs.accessSync(at('ro/r.txt'), fs.constants.W_OK)),
            code(() => fs.realpathSync(at('secret'))),
            code(() => fs.mkdirSync(at('ro/sub'))),
            code(() => fs.mkdirSync(at('open/sub/deeper'), { recursive: true })),
            code(() => fs.rmSync(at('ro/r.txt'))),
            code(() => fs.unlinkSync(at('secret/s.txt'))),
            code(() => fs.renameSync(at('open/a.txt'), at('secret/a.txt'))),
            code(() => fs.copyFileSync(at('secret/s.txt'), at('open/s.txt'))),
            code(() => fs.cpSync(at('secret'), at('open/copy'), { recursive: true })),
            code(() => fs.openSync(at('secret/s.txt'), 'r')),
            code(() => fs.openSync(at('ro/r.txt'), 'r+')),
            code(() => fs.closeSync(fs.openSync(at('ro/r.txt'), 'r'))),
            code(() => fs.opendirSync(at('secret'))),
            code(() => fs.truncateSync(at('ro/r.txt'))),
            code(() => fs.utimesSync(at('ro/r.txt'), 1, 1)),
            code(() => fs.chmodSync(at('ro/r.txt'), 0o644)),
            code(() => fs.mkdtempSync(at('secret/t-'))),
            code(() => fs.linkSync(at('ro/r.txt'), at('open/hard'))),
            code(() => fs.watch(at('secret')).close()),
            fs.readFileSync(at('secret/s.txt').replace('s.txt', '../ro/r.txt'), 'utf8'),
        ];
        "#,
    );
    let expected = [
        "open",
        "read only",
        "ERR_ACCESS_DENIED",
        "FileSystemRead",
        "true",
        "Access to this API has been restricted",
        "ERR_ACCESS_DENIED:FileSystemWrite",
        "ok",
        "ERR_ACCESS_DENIED:FileSystemWrite",
        "ERR_ACCESS_DENIED:FileSystemRead",
        "ERR_ACCESS_DENIED:FileSystemRead",
        "ERR_ACCESS_DENIED:FileSystemRead",
        "ok",
        "false",
        "true",
        "ERR_ACCESS_DENIED:FileSystemRead",
        "ERR_ACCESS_DENIED:FileSystemWrite",
        "ERR_ACCESS_DENIED:FileSystemRead",
        "ERR_ACCESS_DENIED:FileSystemWrite",
        "ok",
        "ERR_ACCESS_DENIED:FileSystemWrite",
        "ERR_ACCESS_DENIED:FileSystemWrite",
        "ERR_ACCESS_DENIED:FileSystemWrite",
        "ERR_ACCESS_DENIED:FileSystemRead",
        "ERR_ACCESS_DENIED:FileSystemRead",
        "ERR_ACCESS_DENIED:FileSystemRead",
        "ERR_ACCESS_DENIED:FileSystemWrite",
        "ok",
        "ERR_ACCESS_DENIED:FileSystemRead",
        "ERR_ACCESS_DENIED:FileSystemWrite",
        "ERR_ACCESS_DENIED:FileSystemWrite",
        "ERR_ACCESS_DENIED:FileSystemWrite",
        "ERR_ACCESS_DENIED:FileSystemWrite",
        "ERR_ACCESS_DENIED:FileSystemWrite",
        "ERR_ACCESS_DENIED:FileSystemRead",
        "read only",
    ];
    assert_eq!(result, expected);
    assert_eq!(
        std::fs::read_to_string(tree.path("ro/r.txt")).unwrap(),
        "read only"
    );
    assert_eq!(
        std::fs::read_to_string(tree.path("secret/s.txt")).unwrap(),
        "secret"
    );
    assert!(tree.path("open/new.txt").is_file());
    assert!(!tree.path("open/copy").exists());
}

#[test]
fn callbacks_promises_handles_and_streams_are_checked_too() {
    let tree = Tree::new("async");
    let mut rt = tree.runtime();
    let result = run(
        &mut rt,
        &tree,
        r#"
        const viaCallback = (f) => new Promise((resolve) =>
            f((error, value) => resolve(error ? error.code + ':' + error.permission : 'ok')));
        const viaPromise = (p) => p.then(() => 'ok', (e) => e.code + ':' + e.permission);
        const viaStream = (s, event) => new Promise((resolve) => {
            s.on('error', (e) => resolve(e.code + ':' + e.permission));
            s.on(event, () => resolve('ok'));
        });
        const handle = await fs.promises.open(at('open/a.txt'), 'r');
        const opened = (await handle.readFile('utf8'));
        await handle.close();
        const dir = await fs.promises.opendir(at('ro'));
        const first = (await dir.read()).name;
        globalThis.result = [
            await viaCallback((cb) => fs.readFile(at('secret/s.txt'), cb)),
            await viaCallback((cb) => fs.readFile(at('ro/r.txt'), cb)),
            await viaCallback((cb) => fs.writeFile(at('ro/x.txt'), 'x', cb)),
            await viaCallback((cb) => fs.stat(at('secret'), cb)),
            await viaCallback((cb) => fs.readdir(at('secret'), cb)),
            await viaCallback((cb) => fs.mkdir(at('secret/d'), cb)),
            await new Promise((resolve) => fs.exists(at('secret/s.txt'), (e) => resolve(String(e)))),
            await viaPromise(fs.promises.readFile(at('secret/s.txt'))),
            await viaPromise(fs.promises.writeFile(at('ro/x.txt'), 'x')),
            await viaPromise(fs.promises.open(at('secret/s.txt'))),
            await viaPromise(fs.promises.open(at('ro/r.txt'), 'w')),
            await viaPromise(fs.promises.opendir(at('secret'))),
            await viaPromise(fs.promises.rm(at('secret'), { recursive: true })),
            await viaPromise(fs.promises.access(at('secret/s.txt'))),
            opened,
            first,
            await viaStream(fs.createReadStream(at('secret/s.txt')), 'end'),
            await viaStream(fs.createWriteStream(at('ro/w.txt')), 'ready'),
            await viaStream(fs.createReadStream(at('ro/r.txt')).resume(), 'end'),
            await viaStream(fs.createWriteStream(at('open/w.txt')), 'ready'),
        ];
        "#,
    );
    assert_eq!(
        result,
        [
            "ERR_ACCESS_DENIED:FileSystemRead",
            "ok",
            "ERR_ACCESS_DENIED:FileSystemWrite",
            "ERR_ACCESS_DENIED:FileSystemRead",
            "ERR_ACCESS_DENIED:FileSystemRead",
            "ERR_ACCESS_DENIED:FileSystemWrite",
            "false",
            "ERR_ACCESS_DENIED:FileSystemRead",
            "ERR_ACCESS_DENIED:FileSystemWrite",
            "ERR_ACCESS_DENIED:FileSystemRead",
            "ERR_ACCESS_DENIED:FileSystemWrite",
            "ERR_ACCESS_DENIED:FileSystemRead",
            "ERR_ACCESS_DENIED:FileSystemWrite",
            "ERR_ACCESS_DENIED:FileSystemRead",
            "open",
            "r.txt",
            "ERR_ACCESS_DENIED:FileSystemRead",
            "ERR_ACCESS_DENIED:FileSystemWrite",
            "ok",
            "ok",
        ]
    );
    assert!(!tree.path("ro/w.txt").exists());
    assert!(tree.path("secret/s.txt").exists());
}

#[test]
fn modules_are_loaded_only_from_what_may_be_read() {
    let tree = Tree::new("modules");
    let mut rt = tree.runtime();
    let result = run(
        &mut rt,
        &tree,
        r#"
        const denied = (f) => { try { f(); return 'loaded'; } catch (e) { return e.code + ':' + e.permission; } };
        const imported = (p) => p.then((m) => 'loaded ' + m.default, (e) => e.code + ':' + e.permission);
        const { pathToFileURL } = require('url');
        globalThis.result = [
            require('./m.js'),
            denied(() => require('../secret/s.js')),
            denied(() => require(at('secret/s.js'))),
            await imported(import(pathToFileURL(at('open/ok.mjs')).href)),
            await imported(import(pathToFileURL(at('secret/s.mjs')).href)),
            // A granted module importing one that is not.
            await imported(import(pathToFileURL(at('open/m.mjs')).href)),
        ];
        "#,
    );
    assert_eq!(
        result,
        [
            "open module",
            "ERR_ACCESS_DENIED:FileSystemRead",
            "ERR_ACCESS_DENIED:FileSystemRead",
            "loaded open esm",
            "ERR_ACCESS_DENIED:FileSystemRead",
            "ERR_ACCESS_DENIED:FileSystemRead",
        ]
    );
}

#[test]
fn a_package_in_a_folder_that_may_not_be_read_is_not_loaded() {
    let tree = Tree::new("packages");
    let package = tree.path("secret/node_modules/hidden");
    std::fs::create_dir_all(&package).unwrap();
    std::fs::write(
        package.join("package.json"),
        r#"{"name":"hidden","main":"main.js"}"#,
    )
    .unwrap();
    std::fs::write(package.join("main.js"), "module.exports = 'hidden';").unwrap();
    std::fs::write(
        tree.path("secret/app.js"),
        "module.exports = require('hidden');",
    )
    .unwrap();
    let mut rt = Runtime::new();
    rt.set_permissions(Permissions {
        fs_read: FsAccess::Only(vec![tree.path("open")]),
        ..Permissions::default()
    });
    let required = rt.require(&tree.path("secret/app.js").to_string_lossy());
    assert!(
        required
            .as_ref()
            .is_err_and(|e| e.contains("Access to this API has been restricted")),
        "{required:?}"
    );
}

#[test]
fn dot_dot_and_links_cannot_reach_out_of_a_grant() {
    let tree = Tree::new("escape");
    let linked = make_link(&tree.path("secret"), &tree.path("open/link"), true);
    let dangling = make_link(
        &tree.path("secret/new.txt"),
        &tree.path("open/dangling"),
        false,
    );
    let mut rt = tree.runtime();
    let result = run(
        &mut rt,
        &tree,
        r#"
        globalThis.result = [
            code(() => fs.readFileSync(at('open/../secret/s.txt'))),
            code(() => fs.readFileSync(root + '/open/sub/../../secret/s.txt')),
            code(() => fs.writeFileSync(at('open/../ro/escape.txt'), 'x')),
            code(() => fs.readFileSync(at('open/link/s.txt'))),
            code(() => fs.writeFileSync(at('open/link/new.txt'), 'x')),
            code(() => fs.writeFileSync(at('open/dangling'), 'x')),
            code(() => fs.lstatSync(at('open/link'))),
        ];
        "#,
    );
    assert_eq!(
        result[..3],
        [
            "ERR_ACCESS_DENIED:FileSystemRead",
            "ERR_ACCESS_DENIED:FileSystemRead",
            "ERR_ACCESS_DENIED:FileSystemWrite",
        ]
    );
    // Making links needs a privilege Windows does not always grant; when it did, neither the
    // link to a folder nor one that leads nowhere yet gets out.
    if linked {
        assert_eq!(result[3], "ERR_ACCESS_DENIED:FileSystemRead");
        assert_eq!(result[4], "ERR_ACCESS_DENIED:FileSystemWrite");
        // The link itself is in the granted folder.
        assert_eq!(result[6], "ok");
    }
    if dangling {
        assert_eq!(result[5], "ERR_ACCESS_DENIED:FileSystemWrite");
        assert!(!tree.path("secret/new.txt").exists());
    }
    assert!(!tree.path("ro/escape.txt").exists());
}

fn make_link(target: &Path, link: &Path, dir: bool) -> bool {
    #[cfg(unix)]
    {
        let _ = dir;
        std::os::unix::fs::symlink(target, link).is_ok()
    }
    #[cfg(windows)]
    {
        if dir {
            std::os::windows::fs::symlink_dir(target, link).is_ok()
        } else {
            std::os::windows::fs::symlink_file(target, link).is_ok()
        }
    }
}

#[test]
fn process_permission_has_answers_as_nodes_does() {
    let tree = Tree::new("has");
    let mut rt = tree.runtime();
    let result = run(
        &mut rt,
        &tree,
        r#"
        const has = (...a) => String(process.permission.has(...a));
        globalThis.result = [
            has('fs'), has('fs.read'), has('fs.write'),
            has('fs.read', at('ro/r.txt')), has('fs.read', at('ro')),
            has('fs.read', at('secret')), has('fs.write', at('ro/r.txt')),
            has('fs.write', at('open/x/y')), has('fs.read', at('open/../secret')),
            has('worker'), has('child'), has('addon'), has('net'),
        ];
        "#,
    );
    assert_eq!(
        result,
        [
            "false", "false", "false", "true", "true", "false", "false", "true", "false", "false",
            "false", "false", "false"
        ]
    );
}

#[test]
fn workers_are_refused_without_the_grant() {
    let tree = Tree::new("workers");
    let mut rt = tree.runtime();
    let result = run(
        &mut rt,
        &tree,
        r#"
        let web, threads;
        try { new Worker('data:text/javascript,postMessage(1)') } catch (e) { web = e }
        try { new (require('worker_threads').Worker)('x.js') } catch (e) { threads = e }
        globalThis.result = [web.code, web.permission, threads.code, threads.permission];
        "#,
    );
    assert_eq!(
        result,
        [
            "ERR_ACCESS_DENIED",
            "WorkerThreads",
            "ERR_ACCESS_DENIED",
            "WorkerThreads"
        ]
    );
    let mut granted = Runtime::new();
    granted.set_permissions(Permissions {
        worker: true,
        ..tree.permissions()
    });
    let result = run(
        &mut granted,
        &tree,
        r#"
        const w = new Worker('data:text/javascript,postMessage(1)');
        w.terminate();
        globalThis.result = ['started'];
        "#,
    );
    assert_eq!(result, ["started"]);
}

#[test]
fn a_worker_reads_no_file_its_page_cannot() {
    let tree = Tree::new("worker-fs");
    let mut rt = Runtime::new();
    rt.set_permissions(Permissions {
        worker: true,
        ..tree.permissions()
    });
    let secret = tree
        .path("secret/s.txt")
        .to_string_lossy()
        .replace('\\', "\\\\");
    rt.eval(&format!(
        "globalThis.answer = undefined;
         const w = new Worker('data:text/javascript,' + encodeURIComponent(`
            const fs = process.getBuiltinModule('fs');
            try {{ fs.readFileSync('{secret}'); postMessage('read it') }}
            catch (e) {{ postMessage(e.code + ':' + e.permission) }}`));
         w.onmessage = (e) => {{ globalThis.answer = e.data; w.terminate(); }};"
    ))
    .unwrap();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
    while std::time::Instant::now() < deadline {
        rt.wait_for_workers(std::time::Duration::from_millis(50));
        rt.run_workers();
        if rt
            .eval("globalThis.answer !== undefined")
            .unwrap()
            .to_boolean()
        {
            break;
        }
    }
    assert_eq!(
        verve::print(&rt.eval("globalThis.answer").unwrap()),
        "ERR_ACCESS_DENIED:FileSystemRead"
    );
}

#[test]
fn an_embedder_that_grants_nothing_keeps_the_disk_and_workers_it_had() {
    let defaults = Permissions::default();
    assert_eq!(defaults.fs_read, FsAccess::All);
    assert_eq!(defaults.fs_write, FsAccess::All);
    assert!(defaults.worker);
    assert!(defaults.outbound.is_empty());
    assert!(!defaults.dns);
    let tree = Tree::new("defaults");
    let mut rt = Runtime::new();
    assert_eq!(rt.permissions(), defaults);
    let result = run(
        &mut rt,
        &tree,
        r#"
        globalThis.result = [
            fs.readFileSync(at('secret/s.txt'), 'utf8'),
            code(() => fs.writeFileSync(at('secret/w.txt'), 'x')),
            require('../secret/s.js'),
            String(process.permission.has('fs')),
            String(process.permission.has('fs.write', at('secret'))),
            String(process.permission.has('worker')),
            String(process.permission.has('net')),
        ];
        "#,
    );
    assert_eq!(
        result,
        [
            "secret",
            "ok",
            "secret module",
            "true",
            "true",
            "true",
            "false"
        ]
    );
    // Changing the filesystem grants leaves the network as it was, and back again.
    rt.set_permissions(Permissions::none());
    assert_eq!(rt.permissions().fs_read, FsAccess::Only(Vec::new()));
    let denied = rt.eval(&format!(
        "process.getBuiltinModule('fs').readFileSync({:?})",
        tree.path("secret/s.txt").to_string_lossy()
    ));
    assert!(denied.is_err_and(|e| e.contains("restricted")));
}

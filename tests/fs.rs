//! `fs` as Node's: reading and writing in every form, `Stats` and `Dirent`, folders made and
//! removed recursively, descriptors, errors with Node's codes, callbacks and promises.

use std::path::{Path, PathBuf};

use verve::Runtime;

fn folder(test: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("verve-fs-{}-{test}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// Runs `body` as `main.js` in `dir` (whose path it has as `__dirname`), lets its promises and
/// callbacks settle, then reads `exports.result`.
fn run_in(rt: &mut Runtime, dir: &Path, body: &str) -> Vec<String> {
    let exports = rt
        .load_module(&dir.join("main.js"), body)
        .unwrap_or_else(|e| panic!("{e}"));
    rt.run_microtasks();
    let json = rt.eval("JSON.stringify").unwrap();
    let text = rt
        .call(&json, &boa_engine::JsValue::undefined(), &[exports])
        .unwrap_or_else(|e| panic!("{e}"));
    let parsed: serde_json::Value = serde_json::from_str(&verve::print(&text)).unwrap();
    parsed["result"]
        .as_array()
        .unwrap_or_else(|| panic!("no result in {parsed}"))
        .iter()
        .map(|v| {
            v.as_str()
                .map(str::to_owned)
                .unwrap_or_else(|| v.to_string())
        })
        .collect()
}

#[test]
fn reads_writes_and_stats() {
    let dir = folder("rw");
    let mut rt = Runtime::new();
    let result = run_in(
        &mut rt,
        &dir,
        r#"
        const fs = require('fs');
        const path = require('path');
        const file = path.join(__dirname, 'a.txt');
        fs.writeFileSync(file, 'héllo');
        fs.appendFileSync(file, Buffer.from(' world'));
        const st = fs.statSync(file);
        const dirSt = fs.statSync(__dirname);
        const missing = fs.statSync(path.join(__dirname, 'nope'), { throwIfNoEntry: false });
        let err;
        try { fs.readFileSync(path.join(__dirname, 'nope.md')) } catch (e) { err = e }
        fs.writeFileSync(path.join(__dirname, 'b.bin'), 'aGk=', 'base64');
        exports.result = [
            fs.readFileSync(file, 'utf8'),
            fs.readFileSync(file).length,
            fs.readFileSync(file, { encoding: 'hex' }).slice(0, 4),
            st.isFile(), st.isDirectory(), dirSt.isDirectory(), st.size,
            st.mtime instanceof Date, typeof st.mtimeMs, st instanceof fs.Stats,
            missing === undefined,
            fs.existsSync(file), fs.existsSync(path.join(__dirname, 'nope')), fs.existsSync(undefined),
            err.code, err.syscall, err.errno, err.path.endsWith('nope.md'), err.message.startsWith('ENOENT: no such file or directory, open'),
            fs.readFileSync(path.join(__dirname, 'b.bin'), 'utf8'),
            typeof fs.statSync(file, { bigint: true }).size,
        ];
    "#,
    );
    assert_eq!(
        result,
        [
            "héllo world",
            "12",
            "68c3",
            "true",
            "false",
            "true",
            "12",
            "true",
            "number",
            "true",
            "true",
            "true",
            "false",
            "false",
            "ENOENT",
            "open",
            "-2",
            "true",
            "true",
            "hi",
            "bigint",
        ]
    );
}

#[test]
fn folders_made_listed_copied_and_removed() {
    let dir = folder("dirs");
    let mut rt = Runtime::new();
    let result = run_in(
        &mut rt,
        &dir,
        r#"
        const fs = require('fs');
        const path = require('path');
        const deep = path.join(__dirname, 'x', 'y', 'z');
        const first = fs.mkdirSync(deep, { recursive: true });
        const again = fs.mkdirSync(deep, { recursive: true });
        fs.writeFileSync(path.join(deep, 'f.md'), '1');
        fs.writeFileSync(path.join(__dirname, 'x', 'top.md'), '2');
        let exists;
        try { fs.mkdirSync(path.join(__dirname, 'x')) } catch (e) { exists = e.code }
        const entries = fs.readdirSync(path.join(__dirname, 'x'), { withFileTypes: true });
        const all = fs.readdirSync(path.join(__dirname, 'x'), { recursive: true });
        fs.cpSync(path.join(__dirname, 'x'), path.join(__dirname, 'copy'), { recursive: true });
        let isDir;
        try { fs.rmSync(path.join(__dirname, 'copy')) } catch (e) { isDir = e.code }
        let notEmpty;
        try { fs.rmdirSync(path.join(__dirname, 'copy')) } catch (e) { notEmpty = e.code }
        const copied = fs.readFileSync(path.join(__dirname, 'copy', 'y', 'z', 'f.md'), 'utf8');
        fs.rmSync(path.join(__dirname, 'copy'), { recursive: true });
        fs.rmSync(path.join(__dirname, 'never'), { force: true });
        const temp = fs.mkdtempSync(path.join(__dirname, 'tmp-'));
        fs.renameSync(path.join(__dirname, 'x', 'top.md'), path.join(__dirname, 'moved.md'));
        let excl;
        fs.copyFileSync(path.join(__dirname, 'moved.md'), path.join(__dirname, 'c.md'));
        try { fs.copyFileSync(path.join(__dirname, 'moved.md'), path.join(__dirname, 'c.md'), fs.constants.COPYFILE_EXCL) } catch (e) { excl = e.code }
        exports.result = [
            path.basename(first), again === undefined, exists,
            entries.map(e => e.name + ':' + e.isDirectory() + ':' + e.isFile()).join(','),
            entries[0] instanceof fs.Dirent, path.basename(entries[0].parentPath),
            all.join(','),
            isDir, notEmpty, copied,
            fs.existsSync(path.join(__dirname, 'copy')),
            path.basename(temp).startsWith('tmp-') && path.basename(temp).length === 10 && fs.statSync(temp).isDirectory(),
            fs.existsSync(path.join(__dirname, 'moved.md')), excl,
        ];
    "#,
    );
    assert_eq!(
        result,
        [
            "x",
            "true",
            "EEXIST",
            "top.md:false:true,y:true:false",
            "true",
            "x",
            "top.md,y,y/z,y/z/f.md",
            "ERR_FS_EISDIR",
            "ENOTEMPTY",
            "1",
            "false",
            "true",
            "true",
            "EEXIST",
        ]
    );
}

#[test]
fn descriptors_read_and_write_at_positions() {
    let dir = folder("fds");
    let mut rt = Runtime::new();
    let result = run_in(
        &mut rt,
        &dir,
        r#"
        const fs = require('fs');
        const path = require('path');
        const file = path.join(__dirname, 'd.txt');
        const fd = fs.openSync(file, 'w+');
        fs.writeSync(fd, 'abcdef');
        fs.writeSync(fd, Buffer.from('XY'), 0, 2, 1);
        const buf = Buffer.alloc(4);
        const n = fs.readSync(fd, buf, 0, 4, 0);
        const size = fs.fstatSync(fd).size;
        fs.ftruncateSync(fd, 3);
        fs.closeSync(fd);
        let bad;
        try { fs.closeSync(fd) } catch (e) { bad = e.code }
        fs.writeSync(1, 'to the console\n');
        exports.result = [n, buf.toString(), size, fs.readFileSync(file, 'utf8'), bad];
    "#,
    );
    assert_eq!(result, ["4", "aXYd", "6", "aXY", "EBADF"]);
    assert!(rt.take_console().iter().any(|l| l.text == "to the console"));
}

#[test]
fn callbacks_and_promises() {
    let dir = folder("async");
    let mut rt = Runtime::new();
    rt.load_module(
        &dir.join("main.js"),
        r#"
        const fs = require('fs');
        const fsp = require('node:fs/promises');
        const path = require('path');
        const file = path.join(__dirname, 'p.txt');
        globalThis.log = [];
        fs.writeFile(file, 'one', err => {
            log.push('written ' + err);
            fs.readFile(file, 'utf8', (err, text) => log.push('read ' + text));
            fs.exists(file, yes => log.push('exists ' + yes));
            fs.stat(path.join(__dirname, 'none'), err => log.push('stat ' + err.code));
        });
        (async () => {
            await fsp.writeFile(path.join(__dirname, 'q.txt'), 'two');
            log.push('promise ' + await fsp.readFile(path.join(__dirname, 'q.txt'), 'utf8'));
            try { await fsp.readFile(path.join(__dirname, 'none')) } catch (e) { log.push('rejected ' + e.code) }
            const handle = await fsp.open(path.join(__dirname, 'q.txt'), 'r+');
            const { bytesRead, buffer } = await handle.read(Buffer.alloc(3), 0, 3, 0);
            await handle.write('W', 0);
            log.push('handle ' + bytesRead + buffer.toString() + ' ' + (await handle.stat()).size);
            await handle.close();
            log.push('after ' + await fsp.readFile(path.join(__dirname, 'q.txt'), 'utf8'));
            log.push('list ' + (await fsp.readdir(__dirname)).join(','));
            let thrown = 'none';
            try { fs.readFile(file) } catch (e) { thrown = e.code }
            log.push('no callback ' + thrown);
            const w = fs.watch(__dirname, () => {});
            w.on('close', () => log.push('watcher closed'));
            w.close();
        })();
    "#,
    )
    .unwrap_or_else(|e| panic!("{e}"));
    rt.run_microtasks();
    let log = verve::print(&rt.eval("log.join('|')").unwrap());
    let parts: Vec<&str> = log.split('|').collect();
    for expected in [
        "written null",
        "read one",
        "exists true",
        "stat ENOENT",
        "promise two",
        "rejected ENOENT",
        "handle 3two 3",
        "after Wwo",
        "list p.txt,q.txt",
        "no callback ERR_INVALID_ARG_TYPE",
        "watcher closed",
    ] {
        assert!(parts.contains(&expected), "{expected} not in {log}");
    }
}

#[test]
fn times_modes_and_links() {
    let dir = folder("meta");
    let mut rt = Runtime::new();
    let result = run_in(
        &mut rt,
        &dir,
        r#"
        const fs = require('fs');
        const path = require('path');
        const file = path.join(__dirname, 't.txt');
        fs.writeFileSync(file, 'x');
        fs.utimesSync(file, 1000000, new Date(2000000000));
        const m = fs.statSync(file).mtimeMs;
        fs.chmodSync(file, 0o444);
        let denied;
        try { fs.accessSync(file, fs.constants.W_OK) } catch (e) { denied = e.code }
        fs.chmodSync(file, 0o644);
        fs.accessSync(file, fs.constants.W_OK);
        fs.truncateSync(file, 0);
        exports.result = [m, denied, fs.statSync(file).size, typeof fs.realpathSync.native(file), fs.realpathSync(__dirname).includes('?')];
    "#,
    );
    assert_eq!(result, ["2000000000", "EACCES", "0", "string", "false"]);
}

#[test]
fn file_streams_pipe_through_zlib_and_back() {
    let dir = folder("streams");
    let mut rt = Runtime::new();
    let result = run_in(
        &mut rt,
        &dir,
        r#"
        const fs = require('fs');
        const path = require('path');
        const zlib = require('zlib');
        const { pipeline } = require('stream');
        const source = path.join(__dirname, 'source.txt');
        const packed = path.join(__dirname, 'source.txt.gz');
        const text = 'a line of a note\n'.repeat(5000);
        fs.writeFileSync(source, text);
        exports.result = ['pending'];
        pipeline(fs.createReadStream(source, { highWaterMark: 1024 }), zlib.createGzip(), fs.createWriteStream(packed), (error) => {
            const back = zlib.gunzipSync(fs.readFileSync(packed)).toString();
            const partial = [];
            fs.createReadStream(source, { start: 2, end: 5, encoding: 'utf8' })
                .on('data', (chunk) => partial.push(chunk))
                .on('end', () => {
                    exports.result = [String(error), back === text, partial.join(''),
                        fs.createReadStream(source) instanceof fs.ReadStream,
                        fs.createWriteStream(path.join(__dirname, 'w.txt')) instanceof require('stream').Writable];
                });
        });
    "#,
    );
    assert_eq!(result, ["undefined", "true", "line", "true", "true"]);
}

#[cfg(unix)]
#[test]
fn posix_directory_descriptors_support_sync_without_reading_contents() {
    let dir = folder("directory-sync");
    let mut rt = Runtime::new();
    let result = run_in(
        &mut rt,
        &dir,
        r#"
        const fs = require('node:fs');
        const fd = fs.openSync(__dirname, 'r');
        const directory = fs.fstatSync(fd).isDirectory();
        fs.fsyncSync(fd);
        fs.closeSync(fd);
        let closed, read, write;
        try { fs.fstatSync(fd) } catch (error) { closed = error.code }
        try { fs.readFileSync(__dirname) } catch (error) { read = error.code }
        try { fs.openSync(__dirname, 'w') } catch (error) { write = error.code }
        exports.result = [directory, closed, read, write];
        "#,
    );
    assert_eq!(result, ["true", "EBADF", "EISDIR", "EISDIR"]);
}

#[cfg(unix)]
#[test]
fn posix_directory_descriptors_support_callback_sync_and_close() {
    let dir = folder("directory-callback");
    let mut rt = Runtime::new();
    let result = run_in(
        &mut rt,
        &dir,
        r#"
        const fs = require('node:fs');
        exports.result = ['pending'];
        fs.open(__dirname, 'r', (error, fd) => {
            if (error) { exports.result = ['open', error.code]; return }
            fs.fsync(fd, error => {
                if (error) { exports.result = ['sync', error.code]; return }
                fs.close(fd, error => {
                    exports.result = [error === null];
                    try { fs.fstatSync(fd) } catch (error) { exports.result.push(error.code) }
                });
            });
        });
        "#,
    );
    assert_eq!(result, ["true", "EBADF"]);
}

#[cfg(unix)]
#[test]
fn posix_directory_file_handles_support_the_atomic_publish_protocol() {
    let dir = folder("directory-promise");
    let mut rt = Runtime::new();
    let result = run_in(
        &mut rt,
        &dir,
        r#"
        const fs = require('node:fs/promises');
        const path = require('node:path');
        exports.result = ['pending'];
        (async () => {
            const temporary = path.join(__dirname, 'temporary.json');
            const destination = path.join(__dirname, 'unit.json');
            const file = await fs.open(temporary, 'wx');
            try { await file.writeFile('{"n":42}'); await file.sync() }
            finally { await file.close() }
            await fs.rename(temporary, destination);
            const directory = await fs.open(__dirname, 'r');
            try { await directory.sync() } finally { await directory.close() }
            exports.result = [await fs.readFile(destination, 'utf8')];
        })().catch(error => { exports.result = ['error', error.code] });
        "#,
    );
    assert_eq!(result, [r#"{"n":42}"#]);
}

#[cfg(windows)]
#[test]
fn windows_directory_opens_keep_the_node_error() {
    let dir = folder("directory-windows");
    let mut rt = Runtime::new();
    let result = run_in(
        &mut rt,
        &dir,
        r#"
        const fs = require('node:fs');
        exports.result = [];
        try { fs.openSync(__dirname, 'r') } catch (error) { exports.result.push(error.code) }
        fs.open(__dirname, 'r', error => exports.result.push(error.code));
        fs.promises.open(__dirname, 'r').catch(error => exports.result.push(error.code));
        "#,
    );
    assert_eq!(result, ["EISDIR", "EISDIR", "EISDIR"]);
}

#[test]
fn directory_open_still_requires_read_permission() {
    let dir = folder("directory-permission");
    let mut rt = Runtime::new();
    rt.set_permissions(verve::Permissions::none());
    let result = run_in(
        &mut rt,
        &dir,
        r#"
        exports.result = [];
        try { require('node:fs').openSync(__dirname, 'r') }
        catch (error) { exports.result.push(error.code) }
        "#,
    );
    assert_eq!(result, ["ERR_ACCESS_DENIED"]);
}
